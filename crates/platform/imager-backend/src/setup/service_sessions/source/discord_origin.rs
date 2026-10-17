//! Exact selected Discord DOM storage; no raw-log token scraping or browser-profile transfer.
//!
//! Chromium schema and encoded strings:
//! <https://github.com/chromium/chromium/blob/134.0.6998.35/components/services/storage/dom_storage/local_storage_impl.cc>
//! <https://github.com/chromium/chromium/blob/134.0.6998.35/third_party/blink/renderer/modules/storage/cached_storage_area.cc>
//! This proves source protocol extraction, not that Discord currently accepts the
//! opaque value. Provider client key semantics require independent qualification.
use super::{Capture, SourceError, Stamp, capture, private_write, unavailable};
use std::{collections::BTreeMap, ffi::OsStr, fs::File, os::unix::fs::MetadataExt, path::Path};
use tilo_files::native::atomic::StorageDirectory;
use zeroize::Zeroizing;

mod engine;
#[cfg(test)]
mod tests;

pub const ORIGIN: &str = "https://discord.com";
pub const KEY: &str = "token";
const MAX_FILES: usize = 256;
const MAX_STORE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_VALUE_BYTES: usize = 16 * 1024;

/// Separate native inclusion consent, not implied by a cookie or bookmark grant.
/// Grant the exact Chromium profile or native Discord user-data root selected by
/// its original owner. Only its fixed Local Storage/leveldb subtree is read.
pub struct DiscordOriginGrant {
    profile: File,
    identity: (u32, u64, u64),
}
impl DiscordOriginGrant {
    /// Call only after explicit consent to include the selected Discord session.
    ///
    /// # Errors
    /// Refuses foreign/elevated callers, unsafe directory custody or non-directories.
    pub fn new(profile: File) -> Result<Self, SourceError> {
        let profile = crate::setup::migration_durability::readable_directory(&profile)
            .map_err(unavailable)?;
        let facts = profile.metadata().map_err(unavailable)?;
        let owner = rustix::process::getuid().as_raw();
        if !facts.is_dir() || facts.uid() != owner || rustix::process::geteuid().as_raw() != owner {
            return Err(SourceError::Unavailable);
        }
        storage(&profile)?;
        Ok(Self {
            profile,
            identity: (owner, facts.dev(), facts.ino()),
        })
    }
    fn validate(&self) -> Result<(), SourceError> {
        let facts = self.profile.metadata().map_err(unavailable)?;
        if (facts.uid(), facts.dev(), facts.ino()) != self.identity
            || rustix::process::getuid().as_raw() != self.identity.0
            || rustix::process::geteuid().as_raw() != self.identity.0
        {
            return Err(SourceError::Changed);
        }
        storage(&self.profile).map(drop)
    }
}
/// Only the selected opaque DOM string leaves the private snapshot.
pub struct CapturedOrigin {
    pub value: Zeroizing<String>,
    pub source_snapshot_digest: [u8; 32],
}
impl std::fmt::Debug for CapturedOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CapturedOrigin { private selected Discord origin }")
    }
}
fn storage(directory: &File) -> Result<StorageDirectory, SourceError> {
    let storage = StorageDirectory::from_file(directory.try_clone().map_err(unavailable)?)
        .map_err(unavailable)?;
    storage.validate_namespace_control().map_err(unavailable)?;
    Ok(storage)
}
fn child(parent: &File, name: &str) -> Result<File, SourceError> {
    let metadata =
        crate::setup::nofollow::metadata_nofollow(parent, Path::new(name)).map_err(unavailable)?;
    let held = crate::setup::nofollow::open_directory_nofollow(parent, Path::new(name), &metadata)
        .map_err(unavailable)?;
    let held =
        crate::setup::migration_durability::readable_directory(&held).map_err(unavailable)?;
    storage(&held)?;
    Ok(held)
}
struct Directory {
    local: File,
    data: File,
}
impl Directory {
    fn open(grant: &DiscordOriginGrant) -> Result<Self, SourceError> {
        grant.validate()?;
        let local = child(&grant.profile, "Local Storage")?;
        let data = child(&local, "leveldb")?;
        Ok(Self { local, data })
    }
    fn validate(&self, grant: &DiscordOriginGrant) -> Result<(), SourceError> {
        grant.validate()?;
        for (parent, name, held) in [
            (&grant.profile, "Local Storage", &self.local),
            (&self.local, "leveldb", &self.data),
        ] {
            let expected = cap_primitives::fs::Metadata::from_file(held).map_err(unavailable)?;
            let actual = crate::setup::nofollow::metadata_nofollow(parent, Path::new(name))
                .map_err(unavailable)?;
            if actual.is_symlink()
                || !actual.is_dir()
                || !crate::setup::nofollow::same_directory_identity(&expected, &actual)
            {
                return Err(SourceError::Changed);
            }
            storage(held)?;
        }
        Ok(())
    }
}
fn numbered(name: &str) -> bool {
    (6..=20).contains(&name.len()) && name.bytes().all(|byte| byte.is_ascii_digit())
}
fn database_member(name: &str) -> bool {
    name == "CURRENT"
        || name.strip_prefix("MANIFEST-").is_some_and(numbered)
        || [".log", ".ldb", ".sst", ".dbtmp"]
            .into_iter()
            .any(|suffix| name.strip_suffix(suffix).is_some_and(numbered))
}
fn capture_store(directory: &File, owner: u32) -> Result<BTreeMap<String, Capture>, SourceError> {
    let storage = storage(directory)?;
    let entries = cap_std::fs::Dir::from_std_file(directory.try_clone().map_err(unavailable)?);
    let mut output = BTreeMap::new();
    let mut bytes = 0u64;
    for (count, entry) in entries.entries().map_err(unavailable)?.enumerate() {
        if count >= MAX_FILES {
            return Err(SourceError::LimitExceeded);
        }
        let entry = entry.map_err(unavailable)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| SourceError::UnsupportedSchema)?;
        // Logs and locks have no semantic database records. Never acquire the
        // original lock or read diagnostic logs containing unrelated app state.
        if matches!(name.as_str(), "LOCK" | "LOG" | "LOG.old") {
            continue;
        }
        if !database_member(&name) {
            return Err(SourceError::UnsupportedSchema);
        }
        let captured = capture(&storage, owner, &name)?;
        bytes = bytes
            .checked_add(captured.stamp.length)
            .ok_or(SourceError::LimitExceeded)?;
        if bytes > MAX_STORE_BYTES {
            return Err(SourceError::LimitExceeded);
        }
        if output.insert(name, captured).is_some() {
            return Err(SourceError::Changed);
        }
    }
    Ok(output)
}
fn snapshot(grant: &DiscordOriginGrant) -> Result<(tempfile::TempDir, [u8; 32]), SourceError> {
    snapshot_observed(grant, || {})
}
fn snapshot_observed(
    grant: &DiscordOriginGrant,
    after_first: impl FnOnce(),
) -> Result<(tempfile::TempDir, [u8; 32]), SourceError> {
    let directory = Directory::open(grant)?;
    let first = capture_store(&directory.data, grant.identity.0)?;
    after_first();
    directory.validate(grant)?;
    if first != capture_store(&directory.data, grant.identity.0)? {
        return Err(SourceError::Changed);
    }
    directory.validate(grant)?;
    let held = storage(&directory.data)?;
    for (name, expected) in &first {
        let file = held
            .open_read(
                OsStr::new(name),
                tilo_files::native::atomic::Privacy::Inherit,
            )
            .map_err(unavailable)?;
        if Stamp::of(&file)? != expected.stamp {
            return Err(SourceError::Changed);
        }
    }
    directory.validate(grant)?;
    // LevelDB uses CURRENT's content as a filename. Validate it before its native
    // engine could follow a malicious path outside our disposable quarantine.
    let current = first.get("CURRENT").ok_or(SourceError::UnsupportedSchema)?;
    let name = std::str::from_utf8(&current.bytes).map_err(|_| SourceError::UnsupportedSchema)?;
    let manifest = name
        .strip_suffix('\n')
        .ok_or(SourceError::UnsupportedSchema)?;
    if !manifest.strip_prefix("MANIFEST-").is_some_and(numbered) || !first.contains_key(manifest) {
        return Err(SourceError::UnsupportedSchema);
    }
    let temporary = tempfile::tempdir().map_err(unavailable)?;
    let mut digest = blake3::Hasher::new();
    digest.update(b"tilo-selected-discord-origin-source-v1\0");
    for (name, captured) in &first {
        digest.update(&(name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update(&(captured.bytes.len() as u64).to_le_bytes());
        digest.update(&captured.bytes);
        private_write(&temporary.path().join(name), &captured.bytes)?;
    }
    Ok((temporary, *digest.finalize().as_bytes()))
}
fn dom_string(encoded: &[u8]) -> Result<Zeroizing<String>, SourceError> {
    if encoded.len() > MAX_VALUE_BYTES {
        return Err(SourceError::LimitExceeded);
    }
    let (tag, bytes) = encoded.split_first().ok_or(SourceError::InvalidCookies)?;
    let value = match tag {
        1 => bytes.iter().map(|byte| char::from(*byte)).collect(),
        0 if cfg!(target_endian = "little") && bytes.len().is_multiple_of(2) => {
            let units = Zeroizing::new(
                bytes
                    .chunks_exact(2)
                    .map(|pair| {
                        <[u8; 2]>::try_from(pair)
                            .map(u16::from_le_bytes)
                            .map_err(|_| SourceError::InvalidCookies)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
            String::from_utf16(&units).map_err(|_| SourceError::InvalidCookies)?
        }
        _ => return Err(SourceError::UnsupportedSchema),
    };
    let value = Zeroizing::new(value);
    if value.is_empty() || value.len() > MAX_VALUE_BYTES || value.chars().any(char::is_control) {
        return Err(SourceError::InvalidCookies);
    }
    Ok(value)
}
/// Read exactly the unpartitioned Discord origin's `token` DOM entry. The native
/// engine resolves current manifest, WAL, updates and tombstones on a private copy;
/// no deleted record or differently partitioned origin is selected.
///
/// # Errors
/// Refuses changed/unsafe sources, missing/deleted entries, unsupported schema,
/// corruption, ambiguous key encoding and bounded data limits.
pub fn read_selected(grant: &DiscordOriginGrant) -> Result<CapturedOrigin, SourceError> {
    let (snapshot, source_snapshot_digest) = snapshot(grant)?;
    let database = engine::Database::open(snapshot.path(), false)?;
    let version = database.get(b"VERSION")?;
    if version.as_ref().map(|value| value.as_slice()) != Some(b"1".as_slice()) {
        return Err(SourceError::UnsupportedSchema);
    }
    let latin_key = b"_https://discord.com\0\x01token";
    let utf16_key = b"_https://discord.com\0\x00t\0o\0k\0e\0n\0";
    let latin = database.get(latin_key)?;
    let utf16 = database.get(utf16_key)?;
    let selected = match (latin, utf16) {
        (Some(value), None) | (None, Some(value)) => value,
        (None, None) => return Err(SourceError::Unavailable),
        (Some(_), Some(_)) => return Err(SourceError::InvalidCookies),
    };
    Ok(CapturedOrigin {
        value: dom_string(&selected)?,
        source_snapshot_digest,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub fn fixture_selected_origin(profile: &Path) {
    let directory = profile.join("Local Storage/leveldb");
    std::fs::create_dir_all(&directory).unwrap();
    let database = engine::Database::open(&directory, true).unwrap();
    database.put(b"VERSION", Some(b"1"));
    database.put(
        b"_https://discord.com\0\x01token",
        Some(b"\x01\"private-origin-sentinel\""),
    );
}
