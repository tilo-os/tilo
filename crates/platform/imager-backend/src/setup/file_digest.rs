//! Hash a protected file in bounded chunks, refusing when it changed while read.
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io,
    os::unix::fs::{FileExt as _, MetadataExt as _},
};

/// The file's hex SHA-256, or the caller's refusal when it grew past `limit`
/// or was replaced, rewritten or resized underneath the read.
pub(crate) fn stable(
    file: &File,
    limit: u64,
    refuse: impl Fn() -> io::Error,
) -> io::Result<String> {
    let before = file.metadata()?;
    let mut hash = Sha256::new();
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read_at(&mut buffer, offset)?;
        if count == 0 {
            break;
        }
        offset += count as u64;
        if offset > limit {
            return Err(refuse());
        }
        hash.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    if offset != before.len()
        || (
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
        )
    {
        return Err(refuse());
    }
    Ok(hex::encode(hash.finalize()))
}
