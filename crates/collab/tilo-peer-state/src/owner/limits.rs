//! Encoded size refusal caps, checked before anything is cloned or written.
use std::io::{self, Write};

/// One workspace's record in the owner's file, pretty-printed.
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
/// Check that a workspace's record pretty-prints to at most `limit` bytes.
/// Its replica is written as base64, whose length follows from the
/// replica's; encoding the replica only to count it made every write cost
/// the record's whole history.
///
/// # Errors
/// `stored` pretty-prints to more than `limit` bytes.
pub(crate) fn preflight_record(
    stored: &super::record::StoredWorkspace,
    limit: usize,
) -> io::Result<()> {
    let peer = stored.peer.len().div_ceil(3).saturating_mul(4);
    let rest = limit
        .checked_sub(peer)
        .ok_or_else(|| io::Error::other("native encoded state refusal cap exceeded"))?;
    let fields = super::record::StoredWorkspace {
        peer: super::record::DurablePeer::default(),
        ..stored.clone()
    };
    let mut writer = LimitedWriter::new(io::sink(), rest);
    serde_json::to_writer_pretty(&mut writer, &fields).map_err(io::Error::other)?;
    crate::write_work::read(rest.saturating_sub(writer.remaining));
    Ok(())
}
