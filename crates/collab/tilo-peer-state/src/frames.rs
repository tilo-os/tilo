//! The frames of a link between two devices: one record per message, its byte
//! count little-endian, then the bytes.
//!
//! Every link the engine runs itself writes the same records — a device's
//! replica, one person's own documents, a stream of record content — so the
//! framing, the author a message is filtered by, and the close code a refused
//! connection ends with are here, for every host that dials.

// Nothing compiled for the web reads a link's frames yet: the page's session
// dials and answers them with its own wiring.
#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's session wires these")
)]

use neighbour_wire::{NeighbourMsg, peer_handle};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;
use wire_frame::Framing;

/// The largest frame: a document checkpoint with space to spare.
pub(crate) const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Messages published while a link is writing; a link further behind than
/// this ends, and the next connection catches up from the checkpoint.
pub(crate) const OUTGOING: usize = 1024;

/// Application close code of a refused connection.
pub(crate) const REFUSED: u32 = 1;

/// Count one more change of what a link keeps in step.
pub(crate) fn bump(count: &mut u64) {
    *count = count.wrapping_add(1);
}

/// Whether `message` was written by the device at `device`, rather than
/// carried from another of them.
pub(crate) fn from_device(message: &NeighbourMsg, device: [u8; 32]) -> bool {
    match message {
        NeighbourMsg::AppDocument { peer, envelope, .. } => {
            envelope.author.as_bytes() == &device || *peer == peer_handle(&device)
        }
        NeighbourMsg::IdentityHello { cert, .. } => cert.device_public_key == device,
        // The frames carrying no signature of their own name their author in
        // the handle the workspace knows it by.
        NeighbourMsg::RecordContentWant { requester, .. } => *requester == peer_handle(&device),
        NeighbourMsg::RecordContentPiece { responder, .. }
        | NeighbourMsg::RecordContentLacks { responder, .. } => *responder == peer_handle(&device),
        NeighbourMsg::LivePresence { peer, .. } | NeighbourMsg::Heartbeat { peer, .. } => {
            *peer == peer_handle(&device)
        }
        _ => false,
    }
}

/// Write `catch_up`, then every message published live that `remote` did not
/// send and that `hands` says is `remote`'s to be sent, to `send`.
pub(crate) async fn send_all<W: AsyncWrite + Unpin>(
    send: W,
    remote: [u8; 32],
    catch_up: Vec<NeighbourMsg>,
    mut live: broadcast::Receiver<NeighbourMsg>,
    hands: impl Fn(&NeighbourMsg) -> bool,
) -> Result<(), String> {
    let mut send = send;
    for message in &catch_up {
        write_message(&mut send, message).await?;
    }
    loop {
        match live.recv().await {
            Ok(message) if from_device(&message, remote) || !hands(&message) => {}
            Ok(message) => write_message(&mut send, &message).await?,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return Err("the link fell behind the device's changes".into());
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

/// Write `message` as this link's next record.
pub(crate) async fn write_message<W: AsyncWrite + Unpin>(
    send: &mut W,
    message: &NeighbourMsg,
) -> Result<(), String> {
    let bytes = postcard::to_allocvec(message).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    write_frame(send, &bytes).await
}

/// The next message this link brought, or `None` once the other side finished
/// its stream.
pub(crate) async fn read_message<R: AsyncRead + Unpin>(
    recv: &mut R,
) -> Result<Option<NeighbourMsg>, String> {
    match read_frame(recv, MAX_FRAME_BYTES).await? {
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
