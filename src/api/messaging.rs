use anyhow::{anyhow, Result};
use futures::StreamExt;
use xmtp_content_types::remote_attachment::{decrypt_attachment, RemoteAttachment};
use xmtp_db::encrypted_store::group_message::{GroupMessageKind, MsgQueryArgs};
use xmtp_mls::groups::send_message_opts::SendMessageOpts;

use super::helpers::{
    address_to_identifier, get_client_arc, group_id_to_topic, ms_to_ns, ns_to_ms,
    topic_to_group_id,
};
/// The plugin streamed into a flutter_rust_bridge `StreamSink`; the daemon
/// streams into a tokio channel instead (dropping the receiver ends the stream).
pub type StreamSink<T> = tokio::sync::mpsc::UnboundedSender<T>;

// ---------------------------------------------------------------------------
// Bridge structs — FRB generates Dart classes from these
// ---------------------------------------------------------------------------

/// A member of a conversation, exposed to Dart.
#[derive(Clone)]
pub struct MemberInfo {
    pub inbox_id: String,
    pub address: String,
    /// `member` | `admin` | `super_admin`, as XMTP
    /// reports it, forwarded to the bot with the group's member list.
    pub permission_level: String,
}

pub fn permission_str(p: &xmtp_mls::groups::members::PermissionLevel) -> String {
    use xmtp_mls::groups::members::PermissionLevel::*;
    match p {
        Member => "member",
        Admin => "admin",
        SuperAdmin => "super_admin",
    }
    .to_string()
}

/// A decrypted remote attachment, exposed to Dart.
pub struct AttachmentInfo {
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// A message, exposed to Dart.
pub struct MessageInfo {
    pub id: String,
    pub sent_at_ms: i64,
    pub sender_inbox_id: String,
    pub conversation_topic: String,
    pub encoded_content_bytes: Vec<u8>,
    pub members: Vec<MemberInfo>,
}

// ---------------------------------------------------------------------------
// Send methods
// ---------------------------------------------------------------------------

/// Send a DM to a peer by Ethereum address.
/// Finds or creates the DM conversation, then sends the message.
/// Returns the hex-encoded message ID.
pub async fn send_message(
    address: String,
    content_bytes: Vec<u8>,
) -> Result<String> {
    let client = get_client_arc()?;
    let identifier = address_to_identifier(&address)?;
    let dm = client
        .find_or_create_dm(identifier, None)
        .await
        .map_err(|e| anyhow!("Failed to find or create DM: {e}"))?;
    let msg_id = dm
        .send_message(&content_bytes, SendMessageOpts { should_push: true })
        .await
        .map_err(|e| anyhow!("Failed to send message: {e}"))?;
    Ok(hex::encode(&msg_id))
}

/// Send a DM to a peer by inbox ID.
/// Returns the hex-encoded message ID.
pub async fn send_message_by_inbox_id(
    inbox_id: String,
    content_bytes: Vec<u8>,
) -> Result<String> {
    let client = get_client_arc()?;
    let dm = client
        .find_or_create_dm_by_inbox_id(&inbox_id, None)
        .await
        .map_err(|e| anyhow!("Failed to find or create DM by inbox ID: {e}"))?;
    let msg_id = dm
        .send_message(&content_bytes, SendMessageOpts { should_push: true })
        .await
        .map_err(|e| anyhow!("Failed to send message: {e}"))?;
    Ok(hex::encode(&msg_id))
}

/// Send a message to a group/conversation by topic (hex-encoded group_id).
/// Returns the hex-encoded message ID.
pub async fn send_group_message(
    topic: String,
    content_bytes: Vec<u8>,
) -> Result<String> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;
    let msg_id = group
        .send_message(&content_bytes, SendMessageOpts { should_push: true })
        .await
        .map_err(|e| anyhow!("Failed to send group message: {e}"))?;
    Ok(hex::encode(&msg_id))
}

/// Send raw EncodedContent bytes to a conversation by topic (hex group id).
/// Works for DMs and groups alike: a DM is an MLS group too. `push = false`
/// for silent kinds (read receipts). Added for the daemon.
pub async fn send_to_topic(topic: String, content_bytes: Vec<u8>, push: bool) -> Result<String> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("No conversation for topic {topic}: {e}"))?;
    let msg_id = group
        .send_message(&content_bytes, SendMessageOpts { should_push: push })
        .await
        .map_err(|e| anyhow!("Failed to send: {e}"))?;
    Ok(hex::encode(&msg_id))
}

// ---------------------------------------------------------------------------
// Message retrieval
// ---------------------------------------------------------------------------

/// Get messages from a DM conversation after a given date (milliseconds since epoch).
/// Syncs the conversation first, then queries locally.
pub async fn get_messages_after_date(
    peer_address: String,
    from_date_ms: i64,
) -> Result<Vec<MessageInfo>> {
    let client = get_client_arc()?;
    let identifier = address_to_identifier(&peer_address)?;
    let dm = client
        .find_or_create_dm(identifier, None)
        .await
        .map_err(|e| anyhow!("Failed to find or create DM: {e}"))?;

    // Sync the conversation to get latest messages
    dm.sync()
        .await
        .map_err(|e| anyhow!("Failed to sync DM: {e}"))?;

    let args = MsgQueryArgs {
        sent_after_ns: Some(ms_to_ns(from_date_ms)),
        kind: Some(GroupMessageKind::Application),
        ..Default::default()
    };

    let messages = dm
        .find_messages(&args)
        .map_err(|e| anyhow!("Failed to query messages: {e}"))?;

    let members = dm
        .members()
        .await
        .map_err(|e| anyhow!("Failed to get members: {e}"))?;

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

    let topic = group_id_to_topic(&dm.group_id);

    Ok(messages
        .into_iter()
        .map(|msg| MessageInfo {
            id: hex::encode(&msg.id),
            sent_at_ms: ns_to_ms(msg.sent_at_ns),
            sender_inbox_id: msg.sender_inbox_id.clone(),
            conversation_topic: topic.clone(),
            encoded_content_bytes: msg.decrypted_message_bytes.clone(),
            members: member_infos.clone(),
        })
        .collect())
}

/// Get messages from a conversation by topic after a given date (milliseconds since epoch).
pub async fn get_messages_after_date_by_topic(
    topic: String,
    from_date_ms: i64,
) -> Result<Vec<MessageInfo>> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    // Sync the conversation to get latest messages
    group
        .sync()
        .await
        .map_err(|e| anyhow!("Failed to sync conversation: {e}"))?;

    let args = MsgQueryArgs {
        sent_after_ns: Some(ms_to_ns(from_date_ms)),
        kind: Some(GroupMessageKind::Application),
        ..Default::default()
    };

    let messages = group
        .find_messages(&args)
        .map_err(|e| anyhow!("Failed to query messages: {e}"))?;

    let members = group
        .members()
        .await
        .map_err(|e| anyhow!("Failed to get members: {e}"))?;

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

    Ok(messages
        .into_iter()
        .map(|msg| MessageInfo {
            id: hex::encode(&msg.id),
            sent_at_ms: ns_to_ms(msg.sent_at_ns),
            sender_inbox_id: msg.sender_inbox_id.clone(),
            conversation_topic: topic.clone(),
            encoded_content_bytes: msg.decrypted_message_bytes.clone(),
            members: member_infos.clone(),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// Subscribe to all incoming messages across all conversations.
/// Messages are pushed to the StreamSink as they arrive.
/// The stream runs until the Dart side cancels the subscription.
pub async fn subscribe_to_all_messages(sink: StreamSink<MessageInfo>) -> Result<()> {
    let client = get_client_arc()?;

    let stream = client
        .stream_all_messages_owned(None, None)
        .await
        .map_err(|e| anyhow!("Failed to start message stream: {e}"))?;

    futures::pin_mut!(stream);

    while let Some(result) = stream.next().await {
        match result {
            Ok(msg) => {
                // Skip non-application messages (e.g., membership changes)
                if msg.kind != GroupMessageKind::Application {
                    continue;
                }
                let info = MessageInfo {
                    id: hex::encode(&msg.id),
                    sent_at_ms: ns_to_ms(msg.sent_at_ns),
                    sender_inbox_id: msg.sender_inbox_id.clone(),
                    conversation_topic: group_id_to_topic(&msg.group_id),
                    encoded_content_bytes: msg.decrypted_message_bytes.clone(),
                    members: vec![], // Members omitted in stream for performance
                };
                if sink.send(info).is_err() {
                    break; // Dart side cancelled the subscription
                }
            }
            Err(e) => {
                tracing::warn!("Stream message error: {e}");
                // Continue streaming — don't kill the stream on individual errors
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Remote attachments
// ---------------------------------------------------------------------------

/// Download and decrypt a remote attachment.
/// Fetches the encrypted payload from the URL, verifies the content digest,
/// decrypts with HKDF-SHA256 + AES-256-GCM, and returns the attachment.
pub async fn load_remote_attachment(
    url: String,
    content_digest: String,
    secret: Vec<u8>,
    salt: Vec<u8>,
    nonce: Vec<u8>,
    scheme: String,
    content_length: Option<i64>,
    filename: Option<String>,
) -> Result<AttachmentInfo> {
    // 1. Download encrypted payload
    let response = reqwest::get(&url)
        .await
        .map_err(|e| anyhow!("Failed to download remote attachment from {url}: {e}"))?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "HTTP {} downloading remote attachment from {url}",
            response.status()
        ));
    }

    let encrypted_bytes = response
        .bytes()
        .await
        .map_err(|e| anyhow!("Failed to read response body: {e}"))?;

    // 2. Construct RemoteAttachment metadata for decryption
    let remote_attachment = RemoteAttachment {
        filename,
        url,
        content_digest,
        secret,
        nonce,
        salt,
        scheme,
        content_length: content_length.unwrap_or(encrypted_bytes.len() as i64) as usize,
    };

    // 3. Verify digest + decrypt + parse protobuf (all handled by xmtp_content_types)
    let attachment = decrypt_attachment(&encrypted_bytes, &remote_attachment)
        .map_err(|e| anyhow!("Failed to decrypt remote attachment: {e}"))?;

    Ok(AttachmentInfo {
        filename: attachment.filename.unwrap_or_default(),
        mime_type: attachment.mime_type,
        data: attachment.content,
    })
}
