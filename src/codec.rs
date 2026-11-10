//! Our codecs: raw `EncodedContent` bytes → what the bot needs.
//!
//! The point of this daemon: we decode the raw
//! bytes ourselves, so our non-standard `description` caption parameter
//! survives on every attachment kind. Uploaded files (remote / multi-remote)
//! are downloaded and decrypted here; the bot only ever sees plaintext files,
//! never the keys.

use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use prost::Message as _;
use xmtp_proto::xmtp::message_contents::EncodedContent;
use xmtp_proto::xmtp::mls::message_contents::content_types::MultiRemoteAttachment;

use crate::api::messaging::load_remote_attachment;

/// A file ready for the bot: plaintext bytes plus what it is.
#[derive(Debug, Clone)]
pub struct FileItem {
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// A decoded message, by what the bot does with it.
#[derive(Debug, Clone)]
pub enum Content {
    Text(String),
    /// Inline photo, uploaded file, or gallery: always a list, with the
    /// caption (our `description` parameter) once for the whole message.
    Files {
        items: Vec<FileItem>,
        description: Option<String>,
    },
    ReadReceipt,
    /// Anything we don't handle yet (reply, reaction, …): type + fallback.
    Unsupported {
        type_id: String,
        fallback: Option<String>,
    },
}

/// Largest single download we accept (bytes). `XMTP_MAX_DOWNLOAD`, default 100 MB.
fn max_download() -> u64 {
    std::env::var("XMTP_MAX_DOWNLOAD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100 * 1024 * 1024)
}

fn caption(params: &HashMap<String, String>) -> Option<String> {
    params
        .get("description")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn hex_param(params: &HashMap<String, String>, key: &str) -> Result<Vec<u8>> {
    let v = params
        .get(key)
        .ok_or_else(|| anyhow!("remote attachment missing `{key}`"))?;
    hex::decode(v).with_context(|| format!("`{key}` is not hex"))
}

// ---------------------------------------------------------------------------
// Encoding (what the bot sends)
// ---------------------------------------------------------------------------

fn encoded(
    type_id: &str,
    content: Vec<u8>,
    parameters: HashMap<String, String>,
    fallback: Option<String>,
) -> Vec<u8> {
    use xmtp_proto::xmtp::message_contents::ContentTypeId;
    EncodedContent {
        r#type: Some(ContentTypeId {
            authority_id: "xmtp.org".into(),
            type_id: type_id.into(),
            version_major: 1,
            version_minor: 0,
        }),
        parameters,
        fallback,
        compression: None,
        content,
    }
    .encode_to_vec()
}

pub fn encode_text(text: &str) -> Vec<u8> {
    let mut p = HashMap::new();
    p.insert("encoding".into(), "UTF-8".into());
    encoded("text", text.as_bytes().to_vec(), p, None)
}

/// Inline attachment, with our `description` caption when given. It sets a
/// `fallback`, so other XMTP apps
/// show a line instead of a blank bubble.
pub fn encode_attachment(file: &FileItem, description: Option<&str>) -> Vec<u8> {
    let mut p = HashMap::new();
    p.insert("filename".into(), file.filename.clone());
    p.insert("mimeType".into(), file.mime_type.clone());
    let caption = description.map(str::trim).filter(|d| !d.is_empty());
    if let Some(d) = caption {
        p.insert("description".into(), d.to_string());
    }
    let fallback = match caption {
        Some(d) => format!("[Attachment: {}] {d}", file.filename),
        None => format!("[Attachment: {}]", file.filename),
    };
    encoded("attachment", file.data.clone(), p, Some(fallback))
}

pub fn encode_read_receipt() -> Vec<u8> {
    encoded("readReceipt", Vec::new(), HashMap::new(), None)
}

/// The content type id (`authority/type`) of raw bytes, without decoding further.
pub fn type_of(bytes: &[u8]) -> Option<String> {
    let ec = EncodedContent::decode(bytes).ok()?;
    ec.r#type.map(|t| format!("{}/{}", t.authority_id, t.type_id))
}

pub async fn decode(bytes: &[u8]) -> Result<Content> {
    let ec = EncodedContent::decode(bytes).context("not an EncodedContent")?;
    let type_id = ec
        .r#type
        .as_ref()
        .map(|t| t.type_id.clone())
        .unwrap_or_default();
    let params = &ec.parameters;

    match type_id.as_str() {
        "text" => Ok(Content::Text(String::from_utf8_lossy(&ec.content).into_owned())),

        "readReceipt" => Ok(Content::ReadReceipt),

        // Inline: the file bytes ARE the content.
        "attachment" => Ok(Content::Files {
            items: vec![FileItem {
                filename: params.get("filename").cloned().unwrap_or_default(),
                mime_type: params
                    .get("mimeType")
                    .cloned()
                    .unwrap_or_else(|| "application/octet-stream".into()),
                data: ec.content.clone(),
            }],
            description: caption(params),
        }),

        // Uploaded: the content is the URL; the decrypt keys ride in params.
        "remoteStaticAttachment" => {
            let url = String::from_utf8(ec.content.clone()).context("URL is not UTF-8")?;
            let length: Option<i64> = params.get("contentLength").and_then(|v| v.parse().ok());
            if let Some(n) = length {
                if n as u64 > max_download() {
                    return Err(anyhow!("remote attachment too large: {n} bytes"));
                }
            }
            let file = load_remote_attachment(
                url,
                params.get("contentDigest").cloned().unwrap_or_default(),
                hex_param(params, "secret")?,
                hex_param(params, "salt")?,
                hex_param(params, "nonce")?,
                params.get("scheme").cloned().unwrap_or_else(|| "https".into()),
                length,
                params.get("filename").cloned(),
            )
            .await?;
            Ok(Content::Files {
                items: vec![FileItem {
                    filename: file.filename,
                    mime_type: file.mime_type,
                    data: file.data,
                }],
                description: caption(params),
            })
        }

        // Gallery: a list of uploaded files, one caption for the message.
        "multiRemoteStaticAttachment" => {
            let multi = MultiRemoteAttachment::decode(ec.content.as_slice())
                .context("bad MultiRemoteAttachment")?;
            let mut items = Vec::with_capacity(multi.attachments.len());
            for a in multi.attachments {
                if let Some(n) = a.content_length {
                    if n as u64 > max_download() {
                        return Err(anyhow!("gallery item too large: {n} bytes"));
                    }
                }
                let file = load_remote_attachment(
                    a.url,
                    a.content_digest,
                    a.secret,
                    a.salt,
                    a.nonce,
                    a.scheme,
                    a.content_length.map(i64::from),
                    a.filename,
                )
                .await?;
                items.push(FileItem {
                    filename: file.filename,
                    mime_type: file.mime_type,
                    data: file.data,
                });
            }
            Ok(Content::Files {
                items,
                description: caption(params),
            })
        }

        _ => Ok(Content::Unsupported {
            type_id,
            fallback: ec.fallback.filter(|f| !f.is_empty()),
        }),
    }
}
