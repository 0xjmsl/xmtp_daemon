use anyhow::{anyhow, Result};
use k256::ecdsa::SigningKey;
use sha3::{Digest, Keccak256};
use xmtp_id::associations::unverified::UnverifiedSignature;
use xmtp_id::associations::Identifier;

use super::helpers::get_client_arc;

// ---------------------------------------------------------------------------
// EIP-191 signing helper
// ---------------------------------------------------------------------------

/// Sign a message using EIP-191 personal_sign with a raw secp256k1 private key.
/// Returns a 65-byte recoverable ECDSA signature: r(32) || s(32) || v(1).
fn eip191_sign(private_key: &[u8], message: &str) -> Result<Vec<u8>> {
    let key_bytes: [u8; 32] = private_key
        .try_into()
        .map_err(|_| anyhow!("Signer private key must be exactly 32 bytes"))?;
    let signing_key =
        SigningKey::from_bytes((&key_bytes).into()).map_err(|e| anyhow!("Invalid key: {e}"))?;

    // EIP-191 prefix
    let prefixed = format!(
        "\x19Ethereum Signed Message:\n{}{}",
        message.len(),
        message
    );
    let digest = Keccak256::digest(prefixed.as_bytes());

    // Sign with recoverable ECDSA
    let (sig, recid) = signing_key
        .sign_prehash_recoverable(&digest)
        .map_err(|e| anyhow!("Failed to sign: {e}"))?;

    // Build 65 bytes: r(32) || s(32) || v(1)
    let mut sig_bytes = Vec::with_capacity(65);
    sig_bytes.extend_from_slice(&sig.to_bytes());
    sig_bytes.push(recid.to_byte());

    Ok(sig_bytes)
}

// ---------------------------------------------------------------------------
// Revoke installations (with active client)
// ---------------------------------------------------------------------------

/// Revoke specific installations by their hex-encoded IDs.
/// Requires the recovery wallet's private key to sign the revocation.
pub async fn revoke_installations(
    signer_private_key: Vec<u8>,
    installation_ids: Vec<String>,
) -> Result<bool> {
    let client = get_client_arc()?;

    // Convert hex installation IDs to bytes
    let ids: Vec<Vec<u8>> = installation_ids
        .iter()
        .map(|id| hex::decode(id).map_err(|e| anyhow!("Invalid installation ID hex: {e}")))
        .collect::<Result<Vec<_>>>()?;

    let mut sig_request = client
        .identity_updates()
        .revoke_installations(ids)
        .await
        .map_err(|e| anyhow!("Failed to create revocation request: {e}"))?;

    // Sign with EIP-191 and add to the request
    let text = sig_request.signature_text();
    let sig_bytes = eip191_sign(&signer_private_key, &text)?;
    let unverified = UnverifiedSignature::new_recoverable_ecdsa(sig_bytes);
    let verifier = client.scw_verifier();
    sig_request
        .add_signature(unverified, &*verifier)
        .await
        .map_err(|e| anyhow!("Failed to add signature: {e}"))?;

    client
        .identity_updates()
        .apply_signature_request(sig_request)
        .await
        .map_err(|e| anyhow!("Failed to apply revocation: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Revoke all other installations (with active client)
// ---------------------------------------------------------------------------

/// Revoke all installations except the current one.
/// Useful for "log out all other devices" functionality.
pub async fn revoke_all_other_installations(signer_private_key: Vec<u8>) -> Result<bool> {
    let client = get_client_arc()?;

    // Get current installation ID
    let own_installation_id = client.installation_public_key().to_vec();

    // Get all installations from inbox state
    let inbox_state = client
        .inbox_state(true)
        .await
        .map_err(|e| anyhow!("Failed to get inbox state: {e}"))?;

    let other_ids: Vec<Vec<u8>> = inbox_state
        .installations()
        .into_iter()
        .filter(|inst| inst.id != own_installation_id)
        .map(|inst| inst.id)
        .collect();

    if other_ids.is_empty() {
        return Ok(true); // No other installations to revoke
    }

    let mut sig_request = client
        .identity_updates()
        .revoke_installations(other_ids)
        .await
        .map_err(|e| anyhow!("Failed to create revocation request: {e}"))?;

    let text = sig_request.signature_text();
    let sig_bytes = eip191_sign(&signer_private_key, &text)?;
    let unverified = UnverifiedSignature::new_recoverable_ecdsa(sig_bytes);
    let verifier = client.scw_verifier();
    sig_request
        .add_signature(unverified, &*verifier)
        .await
        .map_err(|e| anyhow!("Failed to add signature: {e}"))?;

    client
        .identity_updates()
        .apply_signature_request(sig_request)
        .await
        .map_err(|e| anyhow!("Failed to apply revocation: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Remove account / identity (with active client)
// ---------------------------------------------------------------------------

/// Remove an identity (wallet address) from the current inbox.
/// Requires the recovery wallet's private key to sign.
/// The identity being removed cannot be the recovery identity.
pub async fn remove_account(
    recovery_private_key: Vec<u8>,
    identifier_to_remove: String,
) -> Result<bool> {
    let client = get_client_arc()?;

    let identifier = Identifier::eth(&identifier_to_remove)
        .map_err(|e| anyhow!("Invalid identifier: {e}"))?;

    let mut sig_request = client
        .identity_updates()
        .revoke_identities(vec![identifier])
        .await
        .map_err(|e| anyhow!("Failed to create identity removal request: {e}"))?;

    let text = sig_request.signature_text();
    let sig_bytes = eip191_sign(&recovery_private_key, &text)?;
    let unverified = UnverifiedSignature::new_recoverable_ecdsa(sig_bytes);
    let verifier = client.scw_verifier();
    sig_request
        .add_signature(unverified, &*verifier)
        .await
        .map_err(|e| anyhow!("Failed to add signature: {e}"))?;

    client
        .identity_updates()
        .apply_signature_request(sig_request)
        .await
        .map_err(|e| anyhow!("Failed to apply identity removal: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Add account / identity (with active client)
// ---------------------------------------------------------------------------

/// Add a new identity (wallet address) to the current inbox.
/// `associate_identity` internally signs with the installation key as the
/// existing member, so we only need to provide the new member's signature.
pub async fn add_account(
    new_account_private_key: Vec<u8>,
) -> Result<bool> {
    let client = get_client_arc()?;

    // Derive the new wallet address from the new account key
    let new_address = super::client::address_from_private_key(new_account_private_key.clone())?;
    let new_identifier = Identifier::eth(&new_address)
        .map_err(|e| anyhow!("Invalid new account address: {e}"))?;

    let mut sig_request = client
        .identity_updates()
        .associate_identity(new_identifier)
        .await
        .map_err(|e| anyhow!("Failed to create association request: {e}"))?;

    let text = sig_request.signature_text();
    let verifier = client.scw_verifier();

    // Only the new member needs to sign — the existing member (installation key)
    // was already signed internally by associate_identity.
    let new_sig_bytes = eip191_sign(&new_account_private_key, &text)?;
    let new_unverified = UnverifiedSignature::new_recoverable_ecdsa(new_sig_bytes);
    sig_request
        .add_signature(new_unverified, &*verifier)
        .await
        .map_err(|e| anyhow!("Failed to add new member signature: {e}"))?;

    // Apply to network
    client
        .identity_updates()
        .apply_signature_request(sig_request)
        .await
        .map_err(|e| anyhow!("Failed to apply association: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Static: revoke installations without active client
// ---------------------------------------------------------------------------

/// Revoke installations without an active client.
/// Creates a temporary API connection to perform the revocation.
pub async fn static_revoke_installations(
    signer_private_key: Vec<u8>,
    inbox_id: String,
    installation_ids: Vec<String>,
) -> Result<bool> {
    use xmtp_api::ApiClientWrapper;
    use xmtp_api_d14n::MessageBackendBuilder;
    use xmtp_mls::identity_updates::revoke_installations_with_verifier;

    // Derive the recovery address from the signer key
    let recovery_address = super::client::address_from_private_key(signer_private_key.clone())?;
    let recovery_identifier = Identifier::eth(&recovery_address)
        .map_err(|e| anyhow!("Invalid recovery address: {e}"))?;

    // Convert hex installation IDs to bytes
    let ids: Vec<Vec<u8>> = installation_ids
        .iter()
        .map(|id| hex::decode(id).map_err(|e| anyhow!("Invalid installation ID hex: {e}")))
        .collect::<Result<Vec<_>>>()?;

    // Create signature request (pure function, no client needed)
    let mut sig_request = revoke_installations_with_verifier(
        &recovery_identifier,
        &inbox_id,
        ids,
    )
    .map_err(|e| anyhow!("Failed to create static revocation request: {e}"))?;

    // Use the environment from the active client, fall back to production
    let env_str = {
        let guard = super::client::CLIENT.lock().map_err(|_| anyhow!("Client mutex poisoned"))?;
        guard.as_ref().map(|s| s.environment.clone()).unwrap_or_else(|| "production".to_string())
    };
    let (grpc_host, _, is_secure) = super::client::resolve_environment(&env_str);

    // Create lightweight API connection
    let api_client = MessageBackendBuilder::default()
        .v3_host(grpc_host)
        .maybe_gateway_host(None::<String>) // d14n gateway breaks v3 identity reads — pure v3 mode (same as initialize_client)
        .is_secure(is_secure)
        .build()
        .map_err(|e| anyhow!("Failed to build API client: {e}"))?;

    let api = ApiClientWrapper::new(
        std::sync::Arc::new(api_client),
        xmtp_api::strategies::exponential_cooldown(),
    );

    // Sign the request
    let text = sig_request.signature_text();
    let sig_bytes = eip191_sign(&signer_private_key, &text)?;
    let unverified = UnverifiedSignature::new_recoverable_ecdsa(sig_bytes);
    sig_request
        .add_signature(unverified, &api)
        .await
        .map_err(|e| anyhow!("Failed to add signature: {e}"))?;

    // Apply
    xmtp_mls::identity_updates::apply_signature_request_with_verifier(
        &api,
        sig_request,
        &api,
    )
    .await
    .map_err(|e| anyhow!("Failed to apply static revocation: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Static: inbox states without active client
// ---------------------------------------------------------------------------

/// Get inbox states without an active client.
/// Creates a temporary API connection and ephemeral DB.
pub async fn static_inbox_states_for_inbox_ids(
    inbox_ids: Vec<String>,
) -> Result<Vec<super::groups::InboxStateInfo>> {
    use super::groups::{IdentityInfo, InstallationInfo, InboxStateInfo};
    use super::helpers::ns_to_ms;
    use xmtp_api::ApiClientWrapper;
    use xmtp_api_d14n::MessageBackendBuilder;
    use xmtp_db::{EncryptedMessageStore, NativeDb, StorageOption};
    use xmtp_id::scw_verifier::SmartContractSignatureVerifier;
    use xmtp_mls::client::inbox_addresses_with_verifier;

    // Use the environment from the active client, fall back to production
    let env_str = {
        let guard = super::client::CLIENT.lock().map_err(|_| anyhow!("Client mutex poisoned"))?;
        guard.as_ref().map(|s| s.environment.clone()).unwrap_or_else(|| "production".to_string())
    };
    let (grpc_host, _, is_secure) = super::client::resolve_environment(&env_str);

    // Create lightweight API connection
    let api_client = MessageBackendBuilder::default()
        .v3_host(grpc_host)
        .maybe_gateway_host(None::<String>) // d14n gateway breaks v3 identity reads — pure v3 mode (same as initialize_client)
        .is_secure(is_secure)
        .build()
        .map_err(|e| anyhow!("Failed to build API client: {e}"))?;

    let api = ApiClientWrapper::new(
        std::sync::Arc::new(api_client),
        xmtp_api::strategies::exponential_cooldown(),
    );

    let scw_verifier =
        std::sync::Arc::new(Box::new(api.clone()) as Box<dyn SmartContractSignatureVerifier>);

    // Create ephemeral DB for inbox state resolution
    let db = NativeDb::new_unencrypted(&StorageOption::Ephemeral)
        .map_err(|e| anyhow!("Failed to create ephemeral DB: {e}"))?;
    let store = EncryptedMessageStore::new(db)
        .map_err(|e| anyhow!("Failed to create message store: {e}"))?;

    // Query inbox states
    let inbox_id_refs: Vec<&str> = inbox_ids.iter().map(|s| s.as_str()).collect();
    let states = inbox_addresses_with_verifier(
        &api,
        &store.db(),
        inbox_id_refs,
        &*scw_verifier,
    )
    .await
    .map_err(|e| anyhow!("Failed to get inbox states: {e}"))?;

    // Convert to our bridge structs
    let results: Vec<InboxStateInfo> = states
        .into_iter()
        .map(|state| {
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

            InboxStateInfo {
                inbox_id: state.inbox_id().to_string(),
                identities,
                installations,
                recovery_identity: IdentityInfo {
                    identifier: rec_identifier,
                    kind: rec_kind,
                },
            }
        })
        .collect();

    Ok(results)
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

    #[test]
    fn test_eip191_sign_deterministic() {
        // Known private key (key = 1)
        let key = vec![
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x01,
        ];
        let sig1 = eip191_sign(&key, "test message").unwrap();
        let sig2 = eip191_sign(&key, "test message").unwrap();
        assert_eq!(sig1, sig2, "Same key + message must produce same signature");
        assert_eq!(sig1.len(), 65, "Signature must be 65 bytes");
    }

    #[test]
    fn test_eip191_sign_different_messages() {
        let key = vec![
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x01,
        ];
        let sig1 = eip191_sign(&key, "message A").unwrap();
        let sig2 = eip191_sign(&key, "message B").unwrap();
        assert_ne!(sig1, sig2, "Different messages must produce different signatures");
    }

    #[test]
    fn test_eip191_sign_invalid_key() {
        let result = eip191_sign(&[0u8; 16], "test");
        assert!(result.is_err(), "Should reject invalid key length");
    }
}
