use anyhow::{anyhow, Result};

use super::groups::{IdentityInfo, InboxStateInfo, InstallationInfo};
use super::helpers::{get_client_arc, ns_to_ms};

// ---------------------------------------------------------------------------
// Installation info
// ---------------------------------------------------------------------------

/// Get the current client's installation ID (hex-encoded public key).
pub fn get_installation_id() -> Result<String> {
    let client = get_client_arc()?;
    let id = client.installation_public_key();
    Ok(hex::encode(id))
}

// ---------------------------------------------------------------------------
// Inbox state
// ---------------------------------------------------------------------------

/// Get the current client's inbox state.
/// Returns identity, installation, and recovery information.
pub async fn get_inbox_state(refresh_from_network: bool) -> Result<InboxStateInfo> {
    let client = get_client_arc()?;

    let state = client
        .inbox_state(refresh_from_network)
        .await
        .map_err(|e| anyhow!("Failed to get inbox state: {e}"))?;

    let identities: Vec<IdentityInfo> = state
        .identifiers()
        .into_iter()
        .map(|ident| {
            let (identifier, kind) = identifier_to_parts(&ident);
            IdentityInfo { identifier, kind }
        })
        .collect();

    let installations: Vec<InstallationInfo> = state
        .installations()
        .into_iter()
        .map(|inst| InstallationInfo {
            id: hex::encode(&inst.id),
            created_at: inst.client_timestamp_ns.map(|ns| ns_to_ms(ns as i64)),
        })
        .collect();

    let recovery = state.recovery_identifier();
    let (rec_identifier, rec_kind) = identifier_to_parts(recovery);

    Ok(InboxStateInfo {
        inbox_id: state.inbox_id().to_string(),
        identities,
        installations,
        recovery_identity: IdentityInfo {
            identifier: rec_identifier,
            kind: rec_kind,
        },
    })
}

// ---------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------

/// Send a sync request to other installations.
/// Triggers history sync from existing devices.
pub async fn send_sync_request() -> Result<bool> {
    let client = get_client_arc()?;

    client
        .device_sync_client()
        .send_sync_request()
        .await
        .map_err(|e| anyhow!("Failed to send sync request: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn identifier_to_parts(ident: &xmtp_id::associations::Identifier) -> (String, String) {
    match ident {
        xmtp_id::associations::Identifier::Ethereum(eth) => {
            (eth.to_string(), "ethereum".to_string())
        }
        xmtp_id::associations::Identifier::Passkey(pk) => {
            (pk.to_string(), "passkey".to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::client::CLIENT;

    #[test]
    fn test_get_installation_id_before_init() {
        let mut guard = CLIENT.lock().unwrap();
        *guard = None;
        drop(guard);

        let result = get_installation_id();
        assert!(result.is_err());
    }
}
