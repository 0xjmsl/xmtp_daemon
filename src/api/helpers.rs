use std::sync::Arc;

use anyhow::{anyhow, Result};
use xmtp_db::encrypted_store::consent_record::ConsentState;
use xmtp_id::associations::Identifier;
use xmtp_mls::MlsContext;

use super::client::CLIENT;

/// The concrete XMTP client type used by libxmtp.
type RustXmtpClient = xmtp_mls::Client<MlsContext>;

/// Clone the Arc<Client> from the global mutex, dropping the lock immediately.
/// Safe for use across await points.
pub(crate) fn get_client_arc() -> Result<Arc<RustXmtpClient>> {
    let guard = CLIENT
        .lock()
        .map_err(|_| anyhow!("Client mutex poisoned"))?;
    let state = guard
        .as_ref()
        .ok_or_else(|| anyhow!("Client not initialized. Call initializeClient first."))?;
    Ok(Arc::clone(&state.inner_client))
}

/// Retrieve the stored signer private key from the global client state.
pub(crate) fn get_signer_key() -> Result<Vec<u8>> {
    let guard = CLIENT
        .lock()
        .map_err(|_| anyhow!("Client mutex poisoned"))?;
    let state = guard
        .as_ref()
        .ok_or_else(|| anyhow!("Client not initialized. Call initializeClient first."))?;
    Ok(state.signer_private_key.clone())
}

/// Convert a hex-encoded topic string to a raw group_id byte vector.
pub(crate) fn topic_to_group_id(topic: &str) -> Result<Vec<u8>> {
    hex::decode(topic).map_err(|e| anyhow!("Invalid topic (not valid hex): {e}"))
}

/// Convert a raw group_id to a hex-encoded topic string.
pub(crate) fn group_id_to_topic(group_id: &[u8]) -> String {
    hex::encode(group_id)
}

/// Convert an Ethereum address string to an xmtp_id Identifier.
pub(crate) fn address_to_identifier(address: &str) -> Result<Identifier> {
    Identifier::eth(address).map_err(|e| anyhow!("Invalid Ethereum address: {e}"))
}

/// Convert nanoseconds to milliseconds.
pub(crate) fn ns_to_ms(ns: i64) -> i64 {
    ns / 1_000_000
}

/// Convert milliseconds to nanoseconds.
pub(crate) fn ms_to_ns(ms: i64) -> i64 {
    ms * 1_000_000
}

/// Extract the peer inbox ID from a dm_id string.
/// dm_id format is "dm:{inbox_id_1}:{inbox_id_2}".
/// Returns the inbox ID that is NOT `my_inbox_id`.
pub(crate) fn extract_peer_inbox_id(dm_id: &str, my_inbox_id: &str) -> Option<String> {
    let without_prefix = dm_id.strip_prefix("dm:")?;
    // The two inbox IDs are separated by ':'
    // Since inbox IDs can vary in length, find the split point
    if let Some(rest) = without_prefix.strip_prefix(my_inbox_id) {
        // my_inbox_id is the first part, peer is after the ':'
        rest.strip_prefix(':').map(|s| s.to_string())
    } else if let Some(rest) = without_prefix.strip_suffix(my_inbox_id) {
        // my_inbox_id is the second part, peer is before the ':'
        rest.strip_suffix(':').map(|s| s.to_string())
    } else {
        // Neither part matches — return the whole thing without prefix as fallback
        Some(without_prefix.to_string())
    }
}

/// Parse a consent state string ("allowed", "denied", "unknown") to the enum.
pub(crate) fn parse_consent_state(s: &str) -> Result<ConsentState> {
    match s.to_lowercase().as_str() {
        "allowed" => Ok(ConsentState::Allowed),
        "denied" => Ok(ConsentState::Denied),
        "unknown" => Ok(ConsentState::Unknown),
        other => Err(anyhow!("Invalid consent state: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_group_id_roundtrip() {
        let original = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];
        let topic = group_id_to_topic(&original);
        let recovered = topic_to_group_id(&topic).unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn test_topic_to_group_id_invalid_hex() {
        let result = topic_to_group_id("not_hex_zzzz");
        assert!(result.is_err());
    }

    #[test]
    fn test_ns_to_ms() {
        assert_eq!(ns_to_ms(1_000_000_000), 1_000);
        assert_eq!(ns_to_ms(500_000), 0); // truncates
        assert_eq!(ns_to_ms(0), 0);
    }

    #[test]
    fn test_ms_to_ns() {
        assert_eq!(ms_to_ns(1_000), 1_000_000_000);
        assert_eq!(ms_to_ns(0), 0);
    }

    #[test]
    fn test_parse_consent_state() {
        assert_eq!(parse_consent_state("allowed").unwrap(), ConsentState::Allowed);
        assert_eq!(parse_consent_state("Denied").unwrap(), ConsentState::Denied);
        assert_eq!(parse_consent_state("UNKNOWN").unwrap(), ConsentState::Unknown);
        assert!(parse_consent_state("invalid").is_err());
    }

    #[test]
    fn test_extract_peer_inbox_id_first_is_mine() {
        let dm_id = "dm:aaa111:bbb222";
        let peer = extract_peer_inbox_id(dm_id, "aaa111");
        assert_eq!(peer, Some("bbb222".to_string()));
    }

    #[test]
    fn test_extract_peer_inbox_id_second_is_mine() {
        let dm_id = "dm:aaa111:bbb222";
        let peer = extract_peer_inbox_id(dm_id, "bbb222");
        assert_eq!(peer, Some("aaa111".to_string()));
    }

    #[test]
    fn test_extract_peer_inbox_id_no_prefix() {
        let peer = extract_peer_inbox_id("aaa111:bbb222", "aaa111");
        assert_eq!(peer, None);
    }

    #[test]
    fn test_extract_peer_inbox_id_real_length_ids() {
        let id1 = "1111aaaa2222bbbb3333cccc4444dddd5555ee";
        let id2 = "abc123def456789012345678901234567890abcd";
        let dm_id = format!("dm:{id1}:{id2}");
        let peer = extract_peer_inbox_id(&dm_id, id1);
        assert_eq!(peer, Some(id2.to_string()));
    }

    #[test]
    fn test_get_client_arc_before_init() {
        // Ensure CLIENT is None
        let mut guard = CLIENT.lock().unwrap();
        *guard = None;
        drop(guard);

        let result = get_client_arc();
        assert!(result.is_err());
    }
}
