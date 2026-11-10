//! Native encrypted archive backups.
//!
//! Thin wrappers over libxmtp's `xmtp_archive` primitives (export/import into
//! the MLS store). These mirror the upstream FFI bodies verbatim
//! (`bindings_ffi/src/mls.rs::{create_archive, import_archive, archive_metadata}`
//! @ v1.9.0) — the only differences are that we operate over the global
//! `CLIENT` (free functions, same shape as `client.rs`) instead of `&self`, and
//! map errors into `anyhow`.
//!
//! The 32-byte encryption `key` is derived ONCE in the Dart facade
//! (`Argon2id(password, salt = inbox_id)` via pointycastle) and passed down, so
//! Windows/Android/iOS share a single KDF and archives are portable across every
//! platform/device of the inbox. This module never sees the password.

use anyhow::{anyhow, Result};

use xmtp_mls::groups::device_sync::archive::exporter::ArchiveExporter;
use xmtp_mls::groups::device_sync::archive::insert_importer;
use xmtp_mls::groups::device_sync::archive::{ArchiveImporter, BackupMetadata, ENC_KEY_SIZE};
use xmtp_proto::xmtp::device_sync::{BackupElementSelection, BackupOptions};

use super::helpers::get_client_arc;

/// Which application data an archive carries. An empty selection at the call
/// site means both (the upstream default). Mirrors
/// `xmtp_proto::xmtp::device_sync::BackupElementSelection`.
pub enum BackupElement {
    Messages,
    Consent,
}

impl From<BackupElement> for BackupElementSelection {
    fn from(value: BackupElement) -> Self {
        match value {
            BackupElement::Messages => BackupElementSelection::Messages,
            BackupElement::Consent => BackupElementSelection::Consent,
        }
    }
}

/// Archive header (scope/counts/time range), read without importing.
pub struct ArchiveMetadata {
    pub backup_version: u16,
    pub elements: Vec<BackupElement>,
    pub exported_at_ns: i64,
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
}

impl From<BackupMetadata> for ArchiveMetadata {
    fn from(value: BackupMetadata) -> Self {
        Self {
            backup_version: value.backup_version,
            elements: value
                .elements
                .into_iter()
                .filter_map(|s| match s {
                    BackupElementSelection::Messages => Some(BackupElement::Messages),
                    BackupElementSelection::Consent => Some(BackupElement::Consent),
                    _ => None,
                })
                .collect(),
            exported_at_ns: value.exported_at_ns,
            start_ns: value.start_ns,
            end_ns: value.end_ns,
        }
    }
}

/// libxmtp requires a key of at least `ENC_KEY_SIZE` (32) bytes and uses the
/// first 32. Re-implements the private `check_key` helper from
/// `bindings_ffi/src/mls.rs` (we can't import it — it's not `pub`). Our Dart
/// KDF already emits exactly 32 bytes, so this is effectively a pass-through.
fn check_key(mut key: Vec<u8>) -> Result<Vec<u8>> {
    if key.len() < ENC_KEY_SIZE {
        return Err(anyhow!(
            "The encryption key must be at least {ENC_KEY_SIZE} bytes long."
        ));
    }
    key.truncate(ENC_KEY_SIZE);
    Ok(key)
}

fn build_options(
    elements: Vec<BackupElement>,
    start_ns: Option<i64>,
    end_ns: Option<i64>,
    exclude_disappearing_messages: bool,
) -> BackupOptions {
    BackupOptions {
        start_ns,
        end_ns,
        // `BackupOptions.elements` is a repeated proto enum => `Vec<i32>`.
        elements: elements
            .into_iter()
            .map(|el| {
                let selection: BackupElementSelection = el.into();
                selection as i32
            })
            .collect(),
        exclude_disappearing_messages,
    }
}

/// Write an encrypted archive of the active client's MLS store to `path`.
/// `key` must be >= 32 bytes (first 32 used). Mirrors
/// `bindings_ffi/src/mls.rs::create_archive`.
///
/// NOTE: an EMPTY `elements` vec archives NOTHING (metadata frame only) —
/// upstream `xmtp_archive::BatchExportStream` builds one input stream per
/// selected element, so empty ⇒ zero streams. The "empty = both" default is
/// applied one layer up in the Dart facade `exportArchive`
/// (`lib/xmtp_plugin.dart`); never call this with an empty vec expecting both.
pub async fn create_archive(
    path: String,
    key: Vec<u8>,
    elements: Vec<BackupElement>,
    start_ns: Option<i64>,
    end_ns: Option<i64>,
    exclude_disappearing_messages: bool,
) -> Result<()> {
    let client = get_client_arc()?;
    let db = client.context.db();
    let options = build_options(elements, start_ns, end_ns, exclude_disappearing_messages);
    ArchiveExporter::export_to_file(options, db, path, &check_key(key)?)
        .await
        .map_err(|e| anyhow!("create archive failed: {e}"))?;
    Ok(())
}

/// Decrypt + import an archive at `path` into the active client's MLS store
/// (additive). A wrong key (bad password / different inbox) fails to decrypt in
/// `from_file` before any write. Mirrors `import_archive`.
pub async fn import_archive(path: String, key: Vec<u8>) -> Result<()> {
    let client = get_client_arc()?;
    let mut importer = ArchiveImporter::from_file(path, &check_key(key)?)
        .await
        .map_err(|e| anyhow!("could not open archive (wrong password / not your archive?): {e}"))?;
    insert_importer(&mut importer, &client.context)
        .await
        .map_err(|e| anyhow!("import archive failed: {e}"))?;
    Ok(())
}

/// Read the archive header without importing. Also verifies the key — a wrong
/// password makes `from_file` fail to decrypt. Mirrors `archive_metadata`.
pub async fn archive_metadata(path: String, key: Vec<u8>) -> Result<ArchiveMetadata> {
    let importer = ArchiveImporter::from_file(path, &check_key(key)?)
        .await
        .map_err(|e| anyhow!("could not open archive (wrong password / not your archive?): {e}"))?;
    Ok(importer.metadata.into())
}
