//! Small private records. A record larger than its bound is refused, never
//! truncated.
use serde::Serialize;
use std::{
    ffi::OsStr,
    io::{self, Read},
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};

/// Read all of `reader`, refusing it with `exceeded` when it holds more than
/// `limit` bytes.
pub fn read_bounded(
    reader: impl Read,
    limit: u64,
    exceeded: impl FnOnce() -> io::Error,
) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(exceeded());
    }
    Ok(bytes)
}

/// [`read_bounded`] for UTF-8 text.
pub fn read_bounded_string(
    reader: impl Read,
    limit: u64,
    exceeded: impl FnOnce() -> io::Error,
) -> io::Result<String> {
    String::from_utf8(read_bounded(reader, limit, exceeded)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
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
