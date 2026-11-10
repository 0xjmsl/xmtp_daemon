use anyhow::{anyhow, Result};
use xmtp_mls::groups::UpdateAdminListType;
use xmtp_mls::mls_common::group::GroupMetadataOptions;

use super::conversations::ConversationInfo;
use super::helpers::{get_client_arc, group_id_to_topic, ns_to_ms, topic_to_group_id};
use super::messaging::MemberInfo;

// ---------------------------------------------------------------------------
// Bridge structs — FRB generates Dart classes from these
// ---------------------------------------------------------------------------

/// Role information for a group member.
pub struct MemberRoleInfo {
    pub is_admin: bool,
    pub is_super_admin: bool,
}

/// An identity (Ethereum address or passkey) associated with an inbox.
pub struct IdentityInfo {
    pub identifier: String,
    pub kind: String,
}

/// An MLS installation associated with an inbox.
pub struct InstallationInfo {
    pub id: String,
    pub created_at: Option<i64>,
}

/// Full inbox state for a given inbox ID.
pub struct InboxStateInfo {
    pub inbox_id: String,
    pub identities: Vec<IdentityInfo>,
    pub installations: Vec<InstallationInfo>,
    pub recovery_identity: IdentityInfo,
}

// ---------------------------------------------------------------------------
// Group creation
// ---------------------------------------------------------------------------

/// Create a new group with the given inbox IDs and optional metadata.
/// Returns the conversation info for the new group.
pub async fn new_group(
    inbox_ids: Vec<String>,
    name: Option<String>,
    image_url_square: Option<String>,
    description: Option<String>,
) -> Result<ConversationInfo> {
    let client = get_client_arc()?;

    let inbox_id_refs: Vec<&str> = inbox_ids.iter().map(|s| s.as_str()).collect();

    let opts = GroupMetadataOptions {
        name: name.clone(),
        image_url_square: image_url_square.clone(),
        description: description.clone(),
        ..Default::default()
    };

    let group = client
        .create_group_with_inbox_ids(&inbox_id_refs, None, Some(opts))
        .await
        .map_err(|e| anyhow!("Failed to create group: {e}"))?;

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
    let created_at_ms = ns_to_ms(group.created_at_ns);

    Ok(ConversationInfo {
        id: topic.clone(),
        topic,
        created_at_ms,
        conversation_type: "group".to_string(),
        peer_inbox_id: None,
        name,
        image_url_square,
        description,
        members: member_infos,
    })
}

// ---------------------------------------------------------------------------
// Member listing
// ---------------------------------------------------------------------------

/// List all members of a group.
pub async fn list_group_members(topic: String) -> Result<Vec<MemberInfo>> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    let members = group
        .members()
        .await
        .map_err(|e| anyhow!("Failed to list group members: {e}"))?;

    Ok(members
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
        .collect())
}

/// List admin inbox IDs for a group.
pub fn list_group_admins(topic: String) -> Result<Vec<String>> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    group
        .admin_list()
        .map_err(|e| anyhow!("Failed to list group admins: {e}"))
}

/// List super admin inbox IDs for a group.
pub fn list_group_super_admins(topic: String) -> Result<Vec<String>> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    group
        .super_admin_list()
        .map_err(|e| anyhow!("Failed to list group super admins: {e}"))
}

// ---------------------------------------------------------------------------
// Member management
// ---------------------------------------------------------------------------

/// Add members to a group by their inbox IDs.
pub async fn add_group_members(topic: String, inbox_ids: Vec<String>) -> Result<bool> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    let inbox_id_refs: Vec<&str> = inbox_ids.iter().map(|s| s.as_str()).collect();
    group
        .add_members_by_inbox_id(&inbox_id_refs)
        .await
        .map_err(|e| anyhow!("Failed to add group members: {e}"))?;

    Ok(true)
}

/// Remove members from a group by their inbox IDs.
pub async fn remove_group_members(topic: String, inbox_ids: Vec<String>) -> Result<bool> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    let inbox_id_refs: Vec<&str> = inbox_ids.iter().map(|s| s.as_str()).collect();
    group
        .remove_members_by_inbox_id(&inbox_id_refs)
        .await
        .map_err(|e| anyhow!("Failed to remove group members: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Admin management
// ---------------------------------------------------------------------------

/// Add an inbox ID to the admin list.
pub async fn add_group_admin(topic: String, inbox_id: String) -> Result<bool> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    group
        .update_admin_list(UpdateAdminListType::Add, inbox_id)
        .await
        .map_err(|e| anyhow!("Failed to add group admin: {e}"))?;

    Ok(true)
}

/// Remove an inbox ID from the admin list.
pub async fn remove_group_admin(topic: String, inbox_id: String) -> Result<bool> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    group
        .update_admin_list(UpdateAdminListType::Remove, inbox_id)
        .await
        .map_err(|e| anyhow!("Failed to remove group admin: {e}"))?;

    Ok(true)
}

/// Add an inbox ID to the super admin list.
pub async fn add_group_super_admin(topic: String, inbox_id: String) -> Result<bool> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    group
        .update_admin_list(UpdateAdminListType::AddSuper, inbox_id)
        .await
        .map_err(|e| anyhow!("Failed to add group super admin: {e}"))?;

    Ok(true)
}

/// Remove an inbox ID from the super admin list.
pub async fn remove_group_super_admin(topic: String, inbox_id: String) -> Result<bool> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    group
        .update_admin_list(UpdateAdminListType::RemoveSuper, inbox_id)
        .await
        .map_err(|e| anyhow!("Failed to remove group super admin: {e}"))?;

    Ok(true)
}

// ---------------------------------------------------------------------------
// Metadata updates
// ---------------------------------------------------------------------------

/// Update group metadata (name, description, image URL).
/// Only non-None fields are updated.
pub async fn update_group(
    topic: String,
    name: Option<String>,
    description: Option<String>,
    image_url: Option<String>,
) -> Result<bool> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    if let Some(n) = name {
        group
            .update_group_name(n)
            .await
            .map_err(|e| anyhow!("Failed to update group name: {e}"))?;
    }

    if let Some(d) = description {
        group
            .update_group_description(d)
            .await
            .map_err(|e| anyhow!("Failed to update group description: {e}"))?;
    }

    if let Some(u) = image_url {
        group
            .update_group_image_url_square(u)
            .await
            .map_err(|e| anyhow!("Failed to update group image URL: {e}"))?;
    }

    Ok(true)
}

// ---------------------------------------------------------------------------
// Role queries
// ---------------------------------------------------------------------------

/// Get the admin/super-admin role for a member in a group.
pub fn get_group_member_role(topic: String, inbox_id: String) -> Result<MemberRoleInfo> {
    let client = get_client_arc()?;
    let gid = topic_to_group_id(&topic)?;
    let group = client
        .group(&gid)
        .map_err(|e| anyhow!("Group not found for topic {topic}: {e}"))?;

    let is_admin = group
        .is_admin(inbox_id.clone())
        .map_err(|e| anyhow!("Failed to check admin status: {e}"))?;

    let is_super_admin = group
        .is_super_admin(inbox_id)
        .map_err(|e| anyhow!("Failed to check super admin status: {e}"))?;

    Ok(MemberRoleInfo {
        is_admin,
        is_super_admin,
    })
}

// ---------------------------------------------------------------------------
// Inbox states
// ---------------------------------------------------------------------------

/// Get the association state for each inbox ID.
/// Returns identity, installation, and recovery information.
pub async fn inbox_states_for_inbox_ids(
    inbox_ids: Vec<String>,
    refresh_from_network: bool,
) -> Result<Vec<InboxStateInfo>> {
    let client = get_client_arc()?;

    let inbox_id_refs: Vec<&str> = inbox_ids.iter().map(|s| s.as_str()).collect();

    let states = client
        .inbox_addresses(refresh_from_network, inbox_id_refs)
        .await
        .map_err(|e| anyhow!("Failed to get inbox states: {e}"))?;

    Ok(states
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
                    created_at: inst
                        .client_timestamp_ns
                        .map(|ns| ns_to_ms(ns as i64)),
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
        .collect())
}

/// Convert an Identifier enum to (identifier_string, kind_string).
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
    fn test_member_role_info() {
        let role = MemberRoleInfo {
            is_admin: true,
            is_super_admin: false,
        };
        assert!(role.is_admin);
        assert!(!role.is_super_admin);
    }

    #[test]
    fn test_identity_info() {
        let info = IdentityInfo {
            identifier: "0x1234".to_string(),
            kind: "ethereum".to_string(),
        };
        assert_eq!(info.identifier, "0x1234");
        assert_eq!(info.kind, "ethereum");
    }

    #[test]
    fn test_installation_info() {
        let info = InstallationInfo {
            id: "abcdef".to_string(),
            created_at: Some(1234567890),
        };
        assert_eq!(info.id, "abcdef");
        assert_eq!(info.created_at, Some(1234567890));

        let info_no_ts = InstallationInfo {
            id: "abcdef".to_string(),
            created_at: None,
        };
        assert!(info_no_ts.created_at.is_none());
    }

    #[test]
    fn test_inbox_state_info() {
        let state = InboxStateInfo {
            inbox_id: "inbox123".to_string(),
            identities: vec![
                IdentityInfo {
                    identifier: "0xABC".to_string(),
                    kind: "ethereum".to_string(),
                },
            ],
            installations: vec![
                InstallationInfo {
                    id: "install1".to_string(),
                    created_at: Some(1000),
                },
            ],
            recovery_identity: IdentityInfo {
                identifier: "0xDEF".to_string(),
                kind: "ethereum".to_string(),
            },
        };
        assert_eq!(state.inbox_id, "inbox123");
        assert_eq!(state.identities.len(), 1);
        assert_eq!(state.installations.len(), 1);
        assert_eq!(state.recovery_identity.identifier, "0xDEF");
    }
}
