//! Hash a protected file in bounded chunks, refusing when it changed while read.
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io,
    os::unix::fs::{FileExt as _, MetadataExt as _},
    sync::atomic::AtomicBool,
};

/// The SHA-256 of exactly `len` bytes from the start of `file`, or the
/// caller's refusal when it is shorter or longer. `cancel`, when given, stops
/// the read between chunks.
pub(crate) fn exact(
    file: &File,
    len: u64,
    cancel: Option<&AtomicBool>,
    refuse: impl Fn() -> io::Error,
) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < len {
        if let Some(cancel) = cancel {
            crate::migration::check_cancel(cancel)?;
        }
        let limit =
            usize::try_from((len - offset).min(buffer.len() as u64)).map_err(io::Error::other)?;
        let count = file.read_at(&mut buffer[..limit], offset)?;
        if count == 0 {
            return Err(refuse());
        }
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    if file.read_at(&mut buffer[..1], len)? != 0 {
        return Err(refuse());
    }
    Ok(hash.finalize().into())
}

/// The file's hex SHA-256, or the caller's refusal when it grew past `limit`
/// or was replaced, rewritten or resized underneath the read.
pub(crate) fn stable(
    file: &File,
    limit: u64,
    refuse: impl Fn() -> io::Error,
) -> io::Result<String> {
    let before = file.metadata()?;
    if before.len() > limit {
        return Err(refuse());
    }
    let digest = exact(file, before.len(), None, &refuse)?;
    let after = file.metadata()?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) {
        return Err(refuse());
    }
    Ok(hex::encode(digest))
}
