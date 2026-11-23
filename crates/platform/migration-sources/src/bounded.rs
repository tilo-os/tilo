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
    tilo_files::read_from(reader, limit).map_err(|error| {
        if error.kind() == io::ErrorKind::FileTooLarge {
            exceeded()
        } else {
            error
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_bounded_enforces_limit_and_reports_caller_error() {
        let too_large = || io::Error::new(io::ErrorKind::InvalidData, "too large");

        assert_eq!(
            read_bounded(&b"abcd"[..], 4, too_large).ok().as_deref(),
            Some(&b"abcd"[..])
        );

        let over = read_bounded(&b"abcde"[..], 4, too_large).err();
        assert_eq!(
            over.as_ref().map(io::Error::kind),
            Some(io::ErrorKind::InvalidData)
        );
        assert_eq!(
            over.as_ref().map(ToString::to_string).as_deref(),
            Some("too large")
        );

        let infinite = read_bounded(io::repeat(b'x'), 16, too_large).err();
        assert_eq!(
            infinite.as_ref().map(io::Error::kind),
            Some(io::ErrorKind::InvalidData)
        );
    }
}
