use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{IdentityMetadata, NotAvailable, ProfilePhoto};

const MAX_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize)]
struct PublicRecord {
    #[serde(default)]
    profile_photo: Option<ProfilePhoto>,
    display_name: String,
    avatar_color: [u8; 3],
    threshold_identity: PublicIdentity,
    schema_version: u32,
}

#[derive(Deserialize)]
struct PublicIdentity {
    single_device: bool,
    group_public_key: [u8; 32],
    threshold: u16,
    total_shares: u16,
    share_index: u16,
}

/// Reads only public fields, without resolving a secret reference or decoding a share.
/// The caller must independently authenticate the returned public key before authorizing it.
pub fn load_metadata_from_bytes(bytes: &[u8]) -> Result<IdentityMetadata, NotAvailable> {
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid_record("identity record exceeds the size limit"));
    }
    let record: PublicRecord = serde_json::from_slice(bytes).map_err(|error| {
        NotAvailable::ConnectionFailed(io::Error::new(io::ErrorKind::InvalidData, error))
    })?;
    if !matches!(record.schema_version, 1 | 2) {
        return Err(invalid_record("unsupported identity record schema"));
    }
    let identity = record.threshold_identity;
    Ok(IdentityMetadata {
        profile_photo: record.profile_photo,
        display_name: record.display_name,
        avatar_color: record.avatar_color,
        group_public_key: identity.group_public_key,
        threshold: identity.threshold,
        total_shares: identity.total_shares,
        share_index: identity.share_index,
        single_device: identity.single_device,
    })
}

/// Reads an explicitly supplied, private user-owned regular file without following a symlink.
/// No environment, active selector, or secret provider is consulted.
pub fn load_metadata_from_path(path: &Path) -> Result<IdentityMetadata, NotAvailable> {
    let bytes = read_private(path, MAX_RECORD_BYTES).map_err(NotAvailable::ConnectionFailed)?;
    load_metadata_from_bytes(&bytes)
}

/// Resolves a public presentation record in an explicitly supplied directory.
/// An absent or invalid selector uses the legacy `primary.json`; selected symlinks are refused.
pub fn resolve_active_identity(dir: &Path) -> Option<PathBuf> {
    let selector = read_private(&dir.join("active"), 128).ok();
    let id = selector
        .as_ref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .map(str::trim)
        .filter(|id| valid_identity_id(id))
        .unwrap_or("primary");
    let path = dir.join(format!("{id}.json"));
    let metadata = fs::symlink_metadata(&path).ok()?;
    metadata.file_type().is_file().then_some(path)
}

fn valid_identity_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().enumerate().all(|(i, byte)| {
            byte.is_ascii_lowercase()
                || i > 0 && (byte.is_ascii_digit() || matches!(byte, b'_' | b'-'))
        })
}

fn invalid_record(message: &str) -> NotAvailable {
    NotAvailable::ConnectionFailed(io::Error::new(
        io::ErrorKind::InvalidData,
        message.to_owned(),
    ))
}

fn read_private(path: &Path, limit: u64) -> io::Result<Zeroizing<Vec<u8>>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity metadata must be a private user-owned regular file",
        ));
    }
    if metadata.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity metadata exceeds the size limit",
        ));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity metadata exceeds the size limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "public_record_tests.rs"]
mod tests;
