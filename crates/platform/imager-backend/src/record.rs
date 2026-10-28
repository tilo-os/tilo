//! Small private records. A record larger than its bound is refused, never
//! truncated.
use std::io::{self, Read};

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
