//! An outgoing message POSTed to `/send` → an XMTP send.
//!
//! Body shapes:
//! - text:        `{conversationId, content: "...", tool_calls?}` (no contentType)
//! - attachment:  `{conversationId, contentType: "attachment", filename, mimeType, content}`
//!   where content is base64, a `data:` URL, or an array of byte values
//! - read receipt `{conversationId, contentType: "readReceipt", content: ""}`
//! - menu:        `{conversationId, contentType: "actions",
//!                  actions: {id, description, actions: [{id, label, style?}]}}`
//!
//! `description` on an attachment is sent as the file's caption.

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use serde_json::Value;

use crate::api::messaging::send_to_topic;
use crate::codec::{self, FileItem};

fn bytes_of(v: &Value) -> Result<Vec<u8>> {
    match v {
        Value::String(s) => {
            let b64 = match s.strip_prefix("data:") {
                Some(rest) => rest
                    .split_once(',')
                    .map(|(_, data)| data)
                    .ok_or_else(|| anyhow!("malformed data: URL"))?,
                None => s.as_str(),
            };
            base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .context("attachment content is not base64")
        }
        Value::Array(items) => items
            .iter()
            .map(|n| {
                n.as_u64()
                    .filter(|n| *n <= 255)
                    .map(|n| n as u8)
                    .ok_or_else(|| anyhow!("byte array holds a non-byte"))
            })
            .collect(),
        _ => Err(anyhow!("unsupported attachment content")),
    }
}

/// Encode one outgoing message. Returns (conversation topic, bytes, push?).
pub fn encode(msg: &Value) -> Result<(String, Vec<u8>, bool)> {
    let topic = msg
        .get("conversationId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("outgoing_message without conversationId"))?
        .to_string();
    let kind = msg.get("contentType").and_then(Value::as_str).unwrap_or("text");
    match kind {
        // A receipt must not ring the other side's phone.
        "readReceipt" => Ok((topic, codec::encode_read_receipt(), false)),
        // A menu of buttons: `actions` is `{id, description, actions:[...]}`.
        "actions" => {
            let menu = msg
                .get("actions")
                .ok_or_else(|| anyhow!("actions message without `actions`"))?;
            Ok((topic, codec::encode_actions(menu)?, true))
        }
        "attachment" => {
            let file = FileItem {
                filename: msg
                    .get("filename")
                    .and_then(Value::as_str)
                    .unwrap_or("file")
                    .to_string(),
                mime_type: msg
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream")
                    .to_string(),
                data: bytes_of(msg.get("content").unwrap_or(&Value::Null))?,
            };
            let desc = msg.get("description").and_then(Value::as_str);
            Ok((topic, codec::encode_attachment(&file, desc), true))
        }
        _ => {
            let text = msg
                .get("content")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| anyhow!("text message without content"))?;
            Ok((topic, codec::encode_text(text), true))
        }
    }
}

/// Encode and send. Returns the new message id (hex).
pub async fn send(msg: &Value) -> Result<String> {
    let (topic, bytes, push) = encode(msg)?;
    send_to_topic(topic, bytes, push).await
}
