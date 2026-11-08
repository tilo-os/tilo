//! One record per stream message, for every exchange the engine runs itself:
//! the record's byte count, little-endian, then the bytes.

use tokio::io::{AsyncRead, AsyncWrite};
use wire_frame::Framing;

/// Outgoing records: bounded only by what the `u32` prefix can express.
pub(crate) async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
) -> Result<(), String> {
    Framing::little_endian(u32::MAX as usize)
        .write_async(writer, bytes)
        .await
        .map_err(|error| error.to_string())
}

/// The next record, at most `max` bytes, or `None` once the stream ended
/// cleanly before one started. An empty record is a peer bug.
pub(crate) async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<Vec<u8>>, String> {
    Framing::little_endian(max)
        .reject_empty()
        .read_async(reader)
        .await
        .map_err(|error| error.to_string())
}
