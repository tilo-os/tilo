//! Encoded size refusal caps, checked before anything is cloned or written.
use serde::Serialize;
use std::io::{self, Write};

/// The owner's whole record, pretty-printed.
pub const MAX_AUTHORITY_BYTES: usize = 32 * 1024 * 1024;
/// A host file that embeds the record, such as the desktop session's state file.
pub const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_PENDING: usize = 8;

pub struct LimitedWriter<W> {
    inner: W,
    remaining: usize,
}
impl<W> LimitedWriter<W> {
    pub fn new(inner: W, limit: usize) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
    pub fn into_inner(self) -> W {
        self.inner
    }
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other(
                "native encoded state refusal cap exceeded",
            ));
        }
        let n = self.inner.write(bytes)?;
        self.remaining = self.remaining.saturating_sub(n);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
/// # Errors
/// `value` pretty-prints to more than `limit` bytes.
pub fn preflight<T: Serialize>(value: &T, limit: usize) -> io::Result<()> {
    serde_json::to_writer_pretty(LimitedWriter::new(io::sink(), limit), value)
        .map_err(io::Error::other)
}
