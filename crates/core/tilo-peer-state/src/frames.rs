//! The frames of a link between two devices: one record per message, its byte
//! count little-endian, then the bytes.
//!
//! Every link the engine runs itself writes the same records — a device's
//! replica, a stream of record content — so the framing, the author a message
//! is filtered by, and the close code a refused connection ends with are here,
//! for every host that dials. Beside them: where a device stands in a workspace's
//! retained history ([`Cursor`]), what one replay round may carry
//! ([`Budget`]), and what it answers ([`Replay`]), which a page that keeps no
//! store speaks as a device that does.

// Nothing compiled for the web reads a link's frames yet: the page's session
// dials and answers them with its own wiring.
#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's session wires these")
)]

use neighbour_wire::{NeighbourMsg, peer_handle};
use tokio::io::{AsyncRead, AsyncWrite};
use wire_frame::Framing;

/// The largest frame: a document checkpoint with space to spare.
pub(crate) const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Messages published while a link is writing; a link further behind than
/// this ends, and the next connection catches up from the checkpoint.
pub(crate) const OUTGOING: usize = 1024;

/// Endings a link may fall behind before it reads the devices whose links
/// ended from the state that keeps them, rather than from the channel.
pub(crate) const ENDINGS: usize = 64;

/// Application close code of a refused connection.
pub(crate) const REFUSED: u32 = 1;

/// Where a device stands on one author's line of a record: it has every frame
/// up to `after_seq` and asks for the ones after it. A device that has none of
/// a line names no cursor for it, and is sent it from its first frame, a
/// checkpoint at sequence number 0 included.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct Cursor {
    /// The line it resumes.
    pub lineage: crate::retained::Lineage,
    /// The last sequence number it holds.
    pub after_seq: u64,
}

/// Where a store has nothing: the earliest frame it still keeps on a line.
/// A device whose cursor is further back has a gap there and asks another
/// device of the workspace for what the store retired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Floor {
    /// The line the store is short on.
    pub lineage: crate::retained::Lineage,
    /// The earliest frame it holds there.
    pub earliest_seq: u64,
}

/// What one replay round carries.
#[derive(Clone, Debug, Default)]
pub struct Replay {
    /// The frames after the subscriber's cursors, in storage order.
    pub frames: Vec<NeighbourMsg>,
    /// Lineages the store cannot continue from the cursors it was given.
    pub floors: Vec<Floor>,
    /// Whether the round stopped at its budget with frames left; ask again
    /// from the cursors these frames advance.
    pub more: bool,
}

/// How much one replay round may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// The most frames in one round.
    pub max_frames: usize,
    /// The most frame bytes in one round.
    pub max_bytes: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_frames: 256,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Where `peer`'s replica stands, one cursor per author line of its sealed
/// history: what it asks a store to continue from.
#[must_use]
pub fn cursors(peer: &crate::StatePeer) -> Vec<Cursor> {
    peer.replay_floors()
        .into_iter()
        .map(|(app, record, author, seq)| Cursor {
            lineage: crate::retained::Lineage::of(&app, &record, author),
            after_seq: seq,
        })
        .collect()
}

/// Where `cursors` stand after `frames`: one cursor per line they belong to,
/// at the last sequence number it carried.
pub fn advance(cursors: &mut Vec<Cursor>, frames: &[NeighbourMsg]) {
    for frame in frames {
        let Ok(frame) = crate::retained::Frame::of(frame) else {
            continue;
        };
        match cursors
            .iter_mut()
            .find(|cursor| cursor.lineage == frame.lineage)
        {
            Some(cursor) => cursor.after_seq = cursor.after_seq.max(frame.seq),
            None => cursors.push(Cursor {
                lineage: frame.lineage,
                after_seq: frame.seq,
            }),
        }
    }
}

/// Whether `message` is a sealed document frame: the only history a store
/// keeps, since it never holds a workspace's keys.
#[must_use]
pub fn sealed(message: &NeighbourMsg) -> bool {
    match message {
        NeighbourMsg::AppDocument { envelope, .. } => envelope.encryption.is_some(),
        _ => false,
    }
}

/// The device `message` names as its own, when it names one: a frame a
/// workspace's host forwards says whose it is by its own signature, whatever
/// link carried it.
#[must_use]
pub fn author_of(message: &NeighbourMsg) -> Option<[u8; 32]> {
    match message {
        NeighbourMsg::AppDocument { envelope, .. } => Some(*envelope.author.as_bytes()),
        NeighbourMsg::IdentityHello { cert, .. } => Some(cert.device_public_key),
        _ => None,
    }
}

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
        NeighbourMsg::DocumentContentWants { requester, .. }
        | NeighbourMsg::DocumentContentReleased { requester, .. } => {
            *requester == peer_handle(&device)
        }
        NeighbourMsg::DocumentContentPieces { responder, .. }
        | NeighbourMsg::DocumentContentLacks { responder, .. }
        | NeighbourMsg::DocumentContentRefused { responder, .. }
        | NeighbourMsg::DocumentContentKept { responder, .. } => *responder == peer_handle(&device),
        NeighbourMsg::LivePresence { peer, .. } | NeighbourMsg::Heartbeat { peer, .. } => {
            *peer == peer_handle(&device)
        }
        _ => false,
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
