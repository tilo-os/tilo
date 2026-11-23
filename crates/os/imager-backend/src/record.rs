//! Small private records. A record larger than its bound is refused, never
//! truncated.
use serde::Serialize;
use std::{ffi::OsStr, io};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};

/// Read all of `reader`, refusing it with `exceeded` when it holds more than
/// `limit` bytes.
pub use migration_sources::bounded::{read_bounded, read_bounded_string};

/// Whether `value` spells a lowercase 64-character SHA-256 digest.
pub fn is_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Read an owner-only record of `directory` within `limit`; `None` when it
/// does not exist.
pub fn read_optional(
    directory: &StorageDirectory,
    name: &str,
    limit: u64,
    exceeded: impl FnOnce() -> io::Error,
) -> io::Result<Option<Vec<u8>>> {
    match directory.open_read(OsStr::new(name), Privacy::OwnerOnly) {
        Ok(file) => read_bounded(file, limit, exceeded).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Read an already opened owner-only record within `limit`, refusing it with
/// `refuse` when it fails `accepted`, crosses the bound, or changes while read.
#[cfg(unix)]
pub fn read_private_file(
    file: &std::fs::File,
    limit: u64,
    accepted: impl Fn(&std::fs::Metadata) -> bool,
    refuse: impl Fn() -> io::Error,
) -> io::Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt as _;
    let before = file.metadata()?;
    if !before.is_file() || !accepted(&before) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    if before.len() > limit {
        return Err(refuse());
    }
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let bytes = read_bounded(reader, limit, &refuse)?;
    let after = file.metadata()?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.modified().ok(),
        before.accessed().ok(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.modified().ok(),
        after.accessed().ok(),
    ) {
        return Err(refuse());
    }
    Ok(bytes)
}

/// Publish `bytes` owner-only at `name`, accepting only an already published
/// record that holds exactly them (and fails `accepted` or the bound otherwise).
#[cfg(unix)]
pub fn publish_or_match(
    directory: &StorageDirectory,
    name: &str,
    bytes: &[u8],
    limit: u64,
    accepted: impl Fn(&std::fs::Metadata) -> bool,
    conflict: impl Fn() -> io::Error,
) -> io::Result<()> {
    match directory.open_read(OsStr::new(name), Privacy::OwnerOnly) {
        Ok(file) => {
            if read_private_file(&file, limit, accepted, &conflict)? != bytes {
                return Err(conflict());
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => directory
            .publish(
                OsStr::new(name),
                bytes,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(|error| error.into_io_error()),
        Err(error) => Err(error),
    }
}

/// Publish `record` owner-only in `directory`, refusing it with `exceeded`
/// when its encoding is larger than `limit`.
pub fn publish<T: Serialize>(
    directory: &StorageDirectory,
    name: &str,
    record: &T,
    limit: u64,
    mode: PublishMode,
    exceeded: impl FnOnce() -> io::Error,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(record)?;
    if bytes.len() as u64 > limit {
        return Err(exceeded());
    }
    directory
        .publish(OsStr::new(name), &bytes, mode, Privacy::OwnerOnly)
        .map_err(|error| error.into_io_error())
}
