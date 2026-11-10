use anyhow::{anyhow, Result};
use xmtp_db::encrypted_store::consent_record::ConsentState;
use xmtp_db::encrypted_store::group::{ConversationType, GroupQueryArgs};

use super::helpers::{
    address_to_identifier, extract_peer_inbox_id, get_client_arc, group_id_to_topic,
    parse_consent_state, topic_to_group_id,
};
use super::messaging::MemberInfo;

// ---------------------------------------------------------------------------
// Bridge structs
// ---------------------------------------------------------------------------

/// A conversation (DM or group), exposed to Dart.
pub struct ConversationInfo {
    pub id: String,
    pub topic: String,
    pub created_at_ms: i64,
    pub conversation_type: String,
    pub peer_inbox_id: Option<String>,
    pub name: Option<String>,
    pub image_url_square: Option<String>,
    pub description: Option<String>,
    pub members: Vec<MemberInfo>,
}

// ---------------------------------------------------------------------------
// Conversation listing
// ---------------------------------------------------------------------------

/// List all DM conversations, optionally filtered by consent state.
pub async fn list_dms(consent_state: Option<String>) -> Result<Vec<ConversationInfo>> {
    let client = get_client_arc()?;

    let consent_states = match consent_state {
        Some(s) => Some(vec![parse_consent_state(&s)?]),
        None => None,
    };

    let args = GroupQueryArgs {
        conversation_type: Some(ConversationType::Dm),
        consent_states,
        ..Default::default()
    };

    let my_inbox_id = client.inbox_id();

    let groups = client
        .find_groups(args)
        .map_err(|e| anyhow!("Failed to list DMs: {e}"))?;

    let mut result = Vec::with_capacity(groups.len());
    for group in groups {
        let members_result = group.members().await;
        let member_infos: Vec<MemberInfo> = members_result
            .unwrap_or_default()
            .iter()
            .map(|m| MemberInfo {
                inbox_id: m.inbox_id.clone(),
                permission_level: super::messaging::permission_str(&m.permission_level),
                address: m
                    .account_identifiers
                    .first()
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            })
            .collect();

        let topic = group_id_to_topic(&group.group_id);
        let created_at_ms = super::helpers::ns_to_ms(group.created_at_ns);

        let peer_inbox_id = group
            .dm_id
            .as_deref()
            .and_then(|dm_id| extract_peer_inbox_id(dm_id, my_inbox_id));

        result.push(ConversationInfo {
            id: topic.clone(),
            topic,
            created_at_ms,
            conversation_type: "dm".to_string(),
            peer_inbox_id,
            name: None,
            image_url_square: None,
            description: None,
            members: member_infos,
        });
    }

    Ok(result)
}

/// List all group conversations, optionally filtered by consent state.
pub async fn list_groups(consent_state: Option<String>) -> Result<Vec<ConversationInfo>> {
    let client = get_client_arc()?;

    let consent_states = match consent_state {
        Some(s) => Some(vec![parse_consent_state(&s)?]),
        None => None,
    };

    let args = GroupQueryArgs {
        conversation_type: Some(ConversationType::Group),
        consent_states,
        ..Default::default()
    };

    let groups = client
        .find_groups(args)
        .map_err(|e| anyhow!("Failed to list groups: {e}"))?;

    let mut result = Vec::with_capacity(groups.len());
    for group in groups {
        let members_result = group.members().await;
        let member_infos: Vec<MemberInfo> = members_result
            .unwrap_or_default()
            .iter()
            .map(|m| MemberInfo {
                inbox_id: m.inbox_id.clone(),
                permission_level: super::messaging::permission_str(&m.permission_level),
                address: m
                    .account_identifiers
                    .first()
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            })
            .collect();

        let topic = group_id_to_topic(&group.group_id);
        let created_at_ms = super::helpers::ns_to_ms(group.created_at_ns);

        let name = group.group_name().ok();
        let description = group.group_description().ok();
        let image_url_square = group.group_image_url_square().ok();

        result.push(ConversationInfo {
            id: topic.clone(),
            topic,
            created_at_ms,
            conversation_type: "group".to_string(),
            peer_inbox_id: None,
            name,
            image_url_square,
            description,
            members: member_infos,
        });
    }

    Ok(result)
}

/// List all conversations (DMs + groups), optionally filtered by consent state.
pub async fn list_conversations(
    consent_state: Option<String>,
) -> Result<Vec<ConversationInfo>> {
    let mut dms = list_dms(consent_state.clone()).await?;
    let groups = list_groups(consent_state).await?;
    dms.extend(groups);
    Ok(dms)
}

/// Find or create a DM with the given inbox ID. Returns the conversation info.
pub async fn find_or_create_dm_with_inbox_id(
    inbox_id: String,
) -> Result<ConversationInfo> {
    let client = get_client_arc()?;
    let my_inbox_id = client.inbox_id();
    let dm = client
        .find_or_create_dm_by_inbox_id(&inbox_id, None)
        .await
        .map_err(|e| anyhow!("Failed to find or create DM: {e}"))?;

    let members_result = dm.members().await;
    let member_infos: Vec<MemberInfo> = members_result
        .unwrap_or_default()
        .iter()
        .map(|m| MemberInfo {
            inbox_id: m.inbox_id.clone(),
            permission_level: super::messaging::permission_str(&m.permission_level),
            address: m
                .account_identifiers
                .first()
                .map(|id| id.to_string())
                .unwrap_or_default(),
        })
        .collect();

    let topic = group_id_to_topic(&dm.group_id);
    let created_at_ms = super::helpers::ns_to_ms(dm.created_at_ns);

    let peer_inbox_id = dm
        .dm_id
        .as_deref()
        .and_then(|dm_id| extract_peer_inbox_id(dm_id, my_inbox_id));

    Ok(ConversationInfo {
        id: topic.clone(),
        topic,
        created_at_ms,
        conversation_type: "dm".to_string(),
        peer_inbox_id,
        name: None,
        image_url_square: None,
        description: None,
        members: member_infos,
    })
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Check if an address can receive XMTP messages.
pub async fn can_message(address: String) -> Result<bool> {
    let client = get_client_arc()?;
    let identifier = address_to_identifier(&address)?;
    let result = client
        .can_message(&[identifier.clone()])
        .await
        .map_err(|e| anyhow!("can_message failed: {e}"))?;
    Ok(*result.get(&identifier).unwrap_or(&false))
}

/// Look up the inbox ID for an Ethereum address from the XMTP network.
/// Returns empty string if the address has no inbox on the network.
pub async fn inbox_id_from_address(address: String) -> Result<String> {
    use xmtp_api::ApiClientWrapper;
    use xmtp_api_d14n::MessageBackendBuilder;

    let identifier = address_to_identifier(&address)?;
    let api_ident: xmtp_proto::types::ApiIdentifier = identifier.clone().into();

    // Use the environment from the active client, fall back to production
    let env_str = {
        let guard = super::client::CLIENT.lock().map_err(|_| anyhow!("Client mutex poisoned"))?;
        guard.as_ref().map(|s| s.environment.clone()).unwrap_or_else(|| "production".to_string())
    };
    let (grpc_host, _, is_secure) = super::client::resolve_environment(&env_str);

    let lookup_client = MessageBackendBuilder::default()
        .v3_host(grpc_host)
        .maybe_gateway_host(None::<String>)
        .is_secure(is_secure)
        .build()
        .map_err(|e| anyhow!("Failed to build lookup client: {e}"))?;
    let lookup_api = ApiClientWrapper::new(
        std::sync::Arc::new(lookup_client),
        xmtp_api::strategies::exponential_cooldown(),
    );

    let inbox_id_map = lookup_api
        .get_inbox_ids(vec![api_ident.clone()])
        .await
        .map_err(|e| anyhow!("Failed to look up inbox ID: {e}"))?;

    Ok(inbox_id_map.get(&api_ident).cloned().unwrap_or_default())
}

/// Get the conversation topic (hex-encoded group_id) for a DM with a given address.
/// This finds or creates the DM, so it may call the network.
pub async fn conversation_topic_from_address(address: String) -> Result<String> {
    let client = get_client_arc()?;
    let identifier = address_to_identifier(&address)?;
    let dm = client
        .find_or_create_dm(identifier, None)
        .await
        .map_err(|e| anyhow!("Failed to find or create DM: {e}"))?;
    Ok(group_id_to_topic(&dm.group_id))
}

// ---------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------

/// Sync all conversations (welcomes + groups).
/// Returns the number of groups synced.
pub async fn sync_all(consent_states: Option<Vec<String>>) -> Result<i64> {
    let client = get_client_arc()?;

    let states: Option<Vec<ConsentState>> = match consent_states {
        Some(strings) => {
            let mut parsed = Vec::with_capacity(strings.len());
            for s in &strings {
                parsed.push(parse_consent_state(s)?);
            }
            Some(parsed)
        }
        None => Some(vec![ConsentState::Allowed]),
    };

    let summary = client
        .sync_all_welcomes_and_groups(states)
        .await
        .map_err(|e| anyhow!("sync_all failed: {e}"))?;

    Ok(summary.num_synced as i64)
}

/// Sync a single conversation by topic.
pub async fn sync_conversation(topic: String) -> Result<()> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;
    group
        .sync()
        .await
        .map_err(|e| anyhow!("Failed to sync conversation: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

/// Accept a conversation (set consent state to Allowed).
pub fn accept_conversation(topic: String) -> Result<bool> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;
    group
        .update_consent_state(ConsentState::Allowed)
        .map_err(|e| anyhow!("Failed to accept conversation: {e}"))?;
    Ok(true)
}

/// Deny a conversation (set consent state to Denied).
pub fn deny_conversation(topic: String) -> Result<bool> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;
    group
        .update_consent_state(ConsentState::Denied)
        .map_err(|e| anyhow!("Failed to deny conversation: {e}"))?;
    Ok(true)
}
