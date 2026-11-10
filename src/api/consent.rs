use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use xmtp_db::encrypted_store::consent_record::{ConsentState, ConsentType, StoredConsentRecord};

use super::helpers::{get_client_arc, parse_consent_state, topic_to_group_id};

// ---------------------------------------------------------------------------
// Conversation-level consent
// ---------------------------------------------------------------------------

/// Get the consent state for a conversation by topic.
/// Returns "allowed", "denied", or "unknown".
pub fn get_conversation_consent_state(topic: String) -> Result<String> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    let state = group
        .consent_state()
        .map_err(|e| anyhow!("Failed to get consent state: {e}"))?;

    Ok(consent_state_to_string(state))
}

/// Set the consent state for a conversation by topic.
/// State must be "allowed", "denied", or "unknown".
pub fn set_conversation_consent_state(topic: String, state: String) -> Result<bool> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    let consent = parse_consent_state(&state)?;
    group
        .update_consent_state(consent)
        .map_err(|e| anyhow!("Failed to set consent state: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Inbox-level consent
// ---------------------------------------------------------------------------

/// Get the consent state for an inbox ID.
/// Returns "allowed", "denied", or "unknown".
pub async fn get_inbox_consent_state(inbox_id: String) -> Result<String> {
    let client = get_client_arc()?;

    let state = client
        .get_consent_state(ConsentType::InboxId, inbox_id)
        .await
        .map_err(|e| anyhow!("Failed to get inbox consent state: {e}"))?;

    Ok(consent_state_to_string(state))
}

/// Set the consent state for an inbox ID.
/// State must be "allowed", "denied", or "unknown".
pub async fn set_inbox_consent_state(inbox_id: String, state: String) -> Result<bool> {
    let client = get_client_arc()?;

    let consent = parse_consent_state(&state)?;
    let record = StoredConsentRecord {
        entity_type: ConsentType::InboxId,
        state: consent,
        entity: inbox_id,
        consented_at_ns: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0),
    };

    client
        .set_consent_states(&[record])
        .await
        .map_err(|e| anyhow!("Failed to set inbox consent state: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Consent sync
// ---------------------------------------------------------------------------

/// Sync consent preferences across devices.
/// This syncs welcomes and history sync groups to propagate consent state.
pub async fn sync_consent_preferences() -> Result<bool> {
    let client = get_client_arc()?;

    client
        .sync_all_welcomes_and_history_sync_groups()
        .await
        .map_err(|e| anyhow!("Failed to sync consent preferences: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn consent_state_to_string(state: ConsentState) -> String {
    match state {
        ConsentState::Allowed => "allowed".to_string(),
        ConsentState::Denied => "denied".to_string(),
        ConsentState::Unknown => "unknown".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consent_state_to_string() {
        assert_eq!(consent_state_to_string(ConsentState::Allowed), "allowed");
        assert_eq!(consent_state_to_string(ConsentState::Denied), "denied");
        assert_eq!(consent_state_to_string(ConsentState::Unknown), "unknown");
    }
}
