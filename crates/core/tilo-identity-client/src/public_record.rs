use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{IdentityMetadata, NotAvailable, ProfilePhoto};

const MAX_DOCUMENT_BYTES: u64 = 1024 * 1024;

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

/// Read only public fields: no secret reference is resolved and no share
/// decoded. The caller authenticates the public key on its own before trusting
/// it.
pub fn load_metadata_from_bytes(bytes: &[u8]) -> Result<IdentityMetadata, NotAvailable> {
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid_record("identity record exceeds the size limit"));
    }
    let record: PublicRecord = serde_json::from_slice(bytes).map_err(|error| {
        NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("identity JSON malformed: {error}"),
        ))
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
        // A record read off disk names no device.
        device_public_key: None,
    })
}

pub fn load_metadata_from_path(path: &Path) -> Result<IdentityMetadata, NotAvailable> {
    let bytes = read_private(path, MAX_DOCUMENT_BYTES).map_err(NotAvailable::ConnectionFailed)?;
    load_metadata_from_bytes(&bytes)
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
