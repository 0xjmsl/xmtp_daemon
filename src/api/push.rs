use anyhow::{anyhow, Result};
use xmtp_db::encrypted_store::group::GroupQueryArgs;

use super::conversations::ConversationInfo;
use super::helpers::{
    extract_peer_inbox_id, get_client_arc, group_id_to_topic, ns_to_ms,
};
use super::messaging::{MemberInfo, MessageInfo};

// ---------------------------------------------------------------------------
// XMTP topic format helpers
// ---------------------------------------------------------------------------
//
// Push APIs deal in the FULL XMTP topic format (e.g. `/xmtp/mls/1/g-${hex}/proto`)
// because that is what the notification server's `subscribeWithMetadata` endpoint
// expects AND what the FCM/APNs payload's `topic` field carries when a push
// arrives. This differs from the rest of the plugin's Rust API which uses raw
// hex group_ids — push topics intentionally diverge to match the notif-server
// wire format end-to-end.

const GROUP_TOPIC_PREFIX: &str = "/xmtp/mls/1/g-";
const TOPIC_SUFFIX: &str = "/proto";

/// Format a raw group_id as a full XMTP group-message topic.
fn format_group_topic(group_id: &[u8]) -> String {
    format!("{}{}{}", GROUP_TOPIC_PREFIX, hex::encode(group_id), TOPIC_SUFFIX)
}

/// Parse a full XMTP group-message topic back to raw group_id bytes.
/// Errors if the topic isn't in the expected `/xmtp/mls/1/g-${hex}/proto` format.
fn parse_group_topic(topic: &str) -> Result<Vec<u8>> {
    let hex_part = topic
        .strip_prefix(GROUP_TOPIC_PREFIX)
        .and_then(|s| s.strip_suffix(TOPIC_SUFFIX))
        .ok_or_else(|| {
            anyhow!(
                "Topic {topic} is not a valid XMTP group-message topic (expected `{}HEX{}`)",
                GROUP_TOPIC_PREFIX,
                TOPIC_SUFFIX
            )
        })?;
    hex::decode(hex_part).map_err(|e| anyhow!("Topic hex segment is invalid: {e}"))
}

// ---------------------------------------------------------------------------
// Bridge structs
// ---------------------------------------------------------------------------

/// One HMAC key for one conversation topic, valid for a 30-day epoch window.
///
/// libxmtp rotates HMAC keys every 30 days. `get_all_hmac_keys` returns three
/// keys per conversation (prior epoch, current, next) so the notification
/// server can keep matching push deliveries across epoch boundaries without
/// the client re-subscribing.
pub struct HmacKeyEntry {
    /// Hex-encoded conversation topic (matches `MessageInfo.conversation_topic`).
    pub topic: String,
    /// Raw HMAC key bytes for this epoch.
    pub hmac_key: Vec<u8>,
    /// Which 30-day epoch this key belongs to. The current epoch is computed
    /// as `floor(unix_seconds / (30 * 86400))`.
    pub thirty_day_periods_since_epoch: i64,
}

// ---------------------------------------------------------------------------
// HMAC keys
// ---------------------------------------------------------------------------

/// Get all HMAC keys for every conversation the client knows about, including
/// stitched duplicate DMs. Flattened to `Vec<HmacKeyEntry>` for easier Dart
/// consumption (libxmtp's native shape is `HashMap<group_id, Vec<HmacKey>>`).
///
/// The notification server's `subscribeWithMetadata` endpoint takes these
/// `(topic, hmac_key, epoch)` triples to filter messages on its XMTP listener
/// side while preserving end-to-end encryption.
pub async fn get_all_hmac_keys() -> Result<Vec<HmacKeyEntry>> {
    let client = get_client_arc()?;

    let groups = client
        .find_groups(GroupQueryArgs {
            include_duplicate_dms: true,
            ..Default::default()
        })
        .map_err(|e| anyhow!("Failed to list groups for HMAC keys: {e}"))?;

    let mut result = Vec::new();
    for group in groups {
        let topic = format_group_topic(&group.group_id);
        let keys = group
            .hmac_keys(-1..=1)
            .map_err(|e| anyhow!("Failed to read HMAC keys for {topic}: {e}"))?;
        for hmac in keys {
            result.push(HmacKeyEntry {
                topic: topic.clone(),
                hmac_key: hmac.key.to_vec(),
                thirty_day_periods_since_epoch: hmac.epoch,
            });
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Push-arrival decryption
// ---------------------------------------------------------------------------

/// Decrypt an FCM/APNs push payload for an existing conversation.
///
/// Returns `Vec<MessageInfo>` because libxmtp may surface multiple decoded
/// messages from a single envelope (rare but possible). Caller should handle
/// the multi-message case; empty Vec is valid if the payload contained no
/// application content (commit-only, etc.).
pub async fn process_push_message(
    topic: String,
    encrypted_bytes: Vec<u8>,
) -> Result<Vec<MessageInfo>> {
    let client = get_client_arc()?;
    let gid = parse_group_topic(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    let messages = group
        .process_streamed_group_message(encrypted_bytes)
        .await
        .map_err(|e| anyhow!("Failed to process push message for {topic}: {e}"))?;

    let members = group
        .members()
        .await
        .map_err(|e| anyhow!("Failed to get members for {topic}: {e}"))?;

    let member_infos: Vec<MemberInfo> = members
        .iter()
        .map(|m| MemberInfo {
            inbox_id: m.inbox_id.clone(),
            permission_level: crate::api::messaging::permission_str(&m.permission_level),
            address: m
                .account_identifiers
                .first()
                .map(|id| id.to_string())
                .unwrap_or_default(),
        })
        .collect();

    // MessageInfo.conversation_topic stays raw hex for consistency with the rest
    // of the plugin (subscribeToAllMessages, getMessagesAfterDateByTopic, etc.).
    // Only the input `topic` parameter speaks the full XMTP format to match the
    // push-wire shape.
    let raw_topic = group_id_to_topic(&gid);

    Ok(messages
        .into_iter()
        .map(|msg| MessageInfo {
            id: hex::encode(&msg.id),
            sent_at_ms: ns_to_ms(msg.sent_at_ns),
            sender_inbox_id: msg.sender_inbox_id.clone(),
            conversation_topic: raw_topic.clone(),
            encoded_content_bytes: msg.decrypted_message_bytes.clone(),
            members: member_infos.clone(),
        })
        .collect())
}

/// Decrypt an FCM/APNs push payload that arrived on the welcome topic
/// (`/xmtp/mls/1/w-${installation_id}/proto`).
///
/// Returns `Vec<ConversationInfo>` because a single welcome envelope may
/// produce multiple conversations when DM stitching applies.
pub async fn process_welcome(encrypted_bytes: Vec<u8>) -> Result<Vec<ConversationInfo>> {
    let client = get_client_arc()?;
    let my_inbox_id = client.inbox_id();

    let groups = client
        .process_streamed_welcome_message(encrypted_bytes)
        .await
        .map_err(|e| anyhow!("Failed to process welcome: {e}"))?;

    let mut result = Vec::with_capacity(groups.len());
    for group in groups {
        let members_result = group.members().await;
        let member_infos: Vec<MemberInfo> = members_result
            .unwrap_or_default()
            .iter()
            .map(|m| MemberInfo {
                inbox_id: m.inbox_id.clone(),
                permission_level: crate::api::messaging::permission_str(&m.permission_level),
                address: m
                    .account_identifiers
                    .first()
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            })
            .collect();

        let topic = group_id_to_topic(&group.group_id);
        let created_at_ms = ns_to_ms(group.created_at_ns);

        let (conversation_type, peer_inbox_id, name, description, image_url_square) =
            if let Some(dm_id) = group.dm_id.as_deref() {
                let peer = extract_peer_inbox_id(dm_id, my_inbox_id);
                ("dm".to_string(), peer, None, None, None)
            } else {
                let name = group.group_name().ok();
                let description = group.group_description().ok();
                let image_url_square = group.group_image_url_square().ok();
                ("group".to_string(), None, name, description, image_url_square)
            };

        result.push(ConversationInfo {
            id: topic.clone(),
            topic,
            created_at_ms,
            conversation_type,
            peer_inbox_id,
            name,
            image_url_square,
            description,
            members: member_infos,
        });
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::client::CLIENT;
    use super::*;

    fn clear_client() {
        let mut guard = CLIENT.lock().unwrap();
        *guard = None;
    }

    #[tokio::test]
    async fn test_get_all_hmac_keys_before_init() {
        clear_client();
        let result = get_all_hmac_keys().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_process_push_message_before_init() {
        clear_client();
        let result = process_push_message("deadbeef".to_string(), vec![1, 2, 3]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_process_welcome_before_init() {
        clear_client();
        let result = process_welcome(vec![1, 2, 3]).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_format_group_topic_roundtrip() {
        let raw = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];
        let topic = format_group_topic(&raw);
        assert_eq!(topic, "/xmtp/mls/1/g-deadbeef01020304/proto");
        let recovered = parse_group_topic(&topic).unwrap();
        assert_eq!(raw, recovered);
    }

    #[test]
    fn test_parse_group_topic_rejects_raw_hex() {
        let result = parse_group_topic("deadbeef01020304");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_group_topic_rejects_welcome_topic() {
        let result = parse_group_topic("/xmtp/mls/1/w-abc123/proto");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_group_topic_rejects_bad_hex() {
        let result = parse_group_topic("/xmtp/mls/1/g-zzzz/proto");
        assert!(result.is_err());
    }
}
