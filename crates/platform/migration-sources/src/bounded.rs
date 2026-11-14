//! Reading a file or a record within a fixed byte bound.

use std::io::{self, Read};

/// Read `reader` into memory, refusing more than `limit` bytes instead of
/// truncating it. `exceeded` names the refusal in the caller's terms.
///
/// # Errors
/// The reader's own error, or `exceeded()` when the bound is crossed.
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
///
/// # Errors
/// The reader's own error, `exceeded()` when the bound is crossed, or the
/// text not being UTF-8.
pub fn read_bounded_string(
    reader: impl Read,
    limit: u64,
    exceeded: impl FnOnce() -> io::Error,
) -> io::Result<String> {
    String::from_utf8(read_bounded(reader, limit, exceeded)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
