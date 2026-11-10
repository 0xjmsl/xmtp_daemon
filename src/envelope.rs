//! The `incoming_message` envelope POSTed to the bot's webhook.
//!
//! Shape: `{id, senderInboxId, timestamp, conversationId, role, content,
//! contentType, kind, members, filename?, mimeType?, description?, attachments?}`.
//! `attachments` lists every file of a gallery; the top-level file fields
//! carry the FIRST file, so a bot that only reads one file keeps working.

use std::collections::HashMap;

use base64::Engine as _;
use serde::Serialize;

use crate::api::conversations::{self, ConversationInfo};
use crate::api::messaging::MessageInfo;
use crate::codec::{Content, FileItem};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub inbox_id: String,
    pub permission_level: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOut {
    /// base64 of the decrypted file
    pub content: String,
    pub filename: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Incoming {
    pub id: String,
    pub sender_inbox_id: String,
    /// ms since epoch
    pub timestamp: i64,
    pub conversation_id: String,
    pub role: &'static str,
    pub content: String,
    pub content_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<Member>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// All files of the message when there is more than one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<FileOut>>,
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn file_out(f: &FileItem) -> FileOut {
    FileOut {
        content: b64(&f.data),
        filename: f.filename.clone(),
        mime_type: f.mime_type.clone(),
    }
}

/// `kind` + `members` per conversation
/// step. Refreshed from libxmtp on a miss; a conversation we still can't
/// resolve means the message is dropped.
#[derive(Default)]
pub struct Conversations {
    by_topic: HashMap<String, (String, Vec<Member>)>,
}

impl Conversations {
    fn classify(c: &ConversationInfo) -> (String, Vec<Member>) {
        if c.conversation_type == "group" {
            let members = c
                .members
                .iter()
                .map(|m| Member {
                    inbox_id: m.inbox_id.clone(),
                    permission_level: m.permission_level.clone(),
                })
                .collect();
            ("group".into(), members)
        } else {
            // For a DM, `members` holds the PEER only.
            let peer = c.peer_inbox_id.clone().unwrap_or_default();
            (
                "dm".into(),
                vec![Member { inbox_id: peer, permission_level: "member".into() }],
            )
        }
    }

    pub async fn lookup(&mut self, topic: &str) -> Option<(String, Vec<Member>)> {
        if let Some(hit) = self.by_topic.get(topic) {
            return Some(hit.clone());
        }
        match conversations::list_conversations(None).await {
            Ok(all) => {
                for c in &all {
                    self.by_topic.insert(c.topic.clone(), Self::classify(c));
                }
            }
            Err(e) => tracing::warn!("conversation refresh failed: {e:#}"),
        }
        self.by_topic.get(topic).cloned()
    }

    /// Forget a conversation (e.g. after a membership change) so the next
    /// message re-reads it.
    #[allow(dead_code)]
    pub fn invalidate(&mut self, topic: &str) {
        self.by_topic.remove(topic);
    }

    /// For a GROUP message: read the member list fresh from libxmtp (like
    /// Group member list, sent once per inbound group
    /// message), update the cache, and return the `group_members` body.
    /// `None` for a DM or when the read fails (the cached list stays).
    pub async fn refresh_group(&mut self, topic: &str) -> Option<serde_json::Value> {
        let (kind, _) = self.lookup(topic).await?;
        if kind != "group" {
            return None;
        }
        let fresh = match crate::api::groups::list_group_members(topic.to_string()).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(%topic, "group member refresh failed: {e:#}");
                return None;
            }
        };
        let members: Vec<Member> = fresh
            .iter()
            .map(|m| Member {
                inbox_id: m.inbox_id.clone(),
                permission_level: m.permission_level.clone(),
            })
            .collect();
        self.by_topic.insert(topic.to_string(), ("group".into(), members));
        // Shape: {conversationId, members:[{inboxId, permissionLevel,
        // identifiers:[{identifier, identifierKind}]}]}.
        Some(serde_json::json!({
            "conversationId": topic,
            "members": fresh.iter().map(|m| serde_json::json!({
                "inboxId": m.inbox_id,
                "permissionLevel": m.permission_level,
                "identifiers": if m.address.is_empty() { vec![] } else {
                    vec![serde_json::json!({"identifier": m.address, "identifierKind": "Ethereum"})]
                },
            })).collect::<Vec<_>>(),
        }))
    }
}

/// Build the envelope for one decoded message. `None` = nothing to send to
/// the bot (unsupported type, or an unresolvable conversation).
pub async fn build(
    m: &MessageInfo,
    content: &Content,
    convos: &mut Conversations,
) -> Option<Incoming> {
    let mut env = Incoming {
        id: m.id.clone(),
        sender_inbox_id: m.sender_inbox_id.clone(),
        timestamp: m.sent_at_ms,
        conversation_id: m.conversation_topic.clone(),
        role: "user",
        content: String::new(),
        content_type: "text",
        kind: None,
        members: None,
        filename: None,
        mime_type: None,
        description: None,
        attachments: None,
    };

    match content {
        // Read receipts go out early, without kind/members.
        Content::ReadReceipt => {
            env.content_type = "readReceipt";
            return Some(env);
        }
        Content::Text(t) => env.content = t.clone(),
        Content::Files { items, description } => {
            let first = items.first()?;
            env.content_type = "attachment";
            env.content = b64(&first.data);
            env.filename = Some(first.filename.clone());
            env.mime_type = Some(first.mime_type.clone());
            env.description = description.clone();
            if items.len() > 1 {
                env.attachments = Some(items.iter().map(file_out).collect());
            }
        }
        Content::Unsupported { type_id, .. } => {
            tracing::info!(id = %m.id, %type_id, "unsupported type, not forwarded");
            return None;
        }
    }

    let Some((kind, members)) = convos.lookup(&m.conversation_topic).await else {
        tracing::warn!(id = %m.id, topic = %m.conversation_topic, "unresolvable conversation, dropped");
        return None;
    };
    env.kind = Some(kind);
    env.members = Some(members);
    Some(env)
}
