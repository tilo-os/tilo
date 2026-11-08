//! One record per stream message, for every exchange the engine runs itself:
//! the record's byte count, little-endian, then the bytes.

use crate::store::Cursor;
use neighbour_wire::NeighbourMsg;
use tokio::io::{AsyncRead, AsyncWrite};
use wire_frame::Framing;

/// One frame of a direct link's control stream.
///
/// A dialling device names its workspace first, then asks the other device's
/// retained history to continue from its own cursors; the other device
/// answers with the frames its store holds. The workspace's live messages
/// travel on their own streams, one per direction.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum LinkFrame {
    /// The workspace the dialling device wants, named before anything else.
    Workspace(u64),
    /// The retained history the dialling device asks for, from its cursors.
    Cursors(Vec<Cursor>),
    /// Retained history after the asker's cursors, and whether more is left.
    Frames {
        /// The frames the other device holds after the asker's cursors.
        frames: Vec<NeighbourMsg>,
        /// Whether a further ask carries more.
        more: bool,
    },
}

/// Write `frame` as the stream's next record.
pub(crate) async fn write_link<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &LinkFrame,
) -> Result<(), String> {
    let bytes = postcard::to_allocvec(frame).map_err(|error| error.to_string())?;
    write_frame(writer, &bytes).await
}

/// The next frame, or `None` once the stream ended cleanly before one started.
pub(crate) async fn read_link<R: AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<LinkFrame>, String> {
    match read_frame(reader, max).await? {
        Some(bytes) => postcard::from_bytes(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        None => Ok(None),
    }
}

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
