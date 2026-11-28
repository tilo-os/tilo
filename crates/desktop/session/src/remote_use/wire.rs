//! What one of the person's devices sends another for a remote use, as the live link between them
//! carries it ([`crate::live::OwnDevices`], [`crate::live::Lane::RemoteUse`]): pieces of what a
//! camera, microphone or scanner captured, at most [`MAX_CHUNK_BYTES`] each, and a game
//! controller's semantic events. Nothing names a device node, a descriptor or a serial.

use os_actions::ForwardedGamepadEvent;
use serde::{Deserialize, Serialize};
use tilo_workspace_service::workspaces::person_workspace::devices::RemoteKind;
use tilo_workspace_service::workspaces::person_workspace::remote_use::{
    AudioFormat, MAX_CHUNK_BYTES, MAX_DOCUMENT_BYTES, MAX_PAGES, Profile, VideoFormat,
};

/// One message of the remote use `id`, its `sequence`-th: each is newer than the last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Carried {
    pub(crate) id: [u8; 16],
    pub(crate) sequence: u64,
    pub(crate) what: What,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum What {
    /// A piece of one frame, one stretch of sound, or one scanned page.
    Piece(Piece),
    /// One game controller event.
    Pad(ForwardedGamepadEvent),
}

/// A piece of one unit of what was captured: the bytes at `offset` of a unit `total` bytes long.
/// A scanned page names its place in the document (`page`), and the piece that ends the document
/// says so (`last`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Piece {
    pub(crate) offset: u64,
    pub(crate) total: u64,
    pub(crate) page: Option<u32>,
    pub(crate) last: bool,
    pub(crate) payload: Vec<u8>,
}

impl Carried {
    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        postcard::to_allocvec(self)
            .inspect_err(|error| tracing::warn!(%error, "a remote use's message does not encode"))
            .ok()
    }

    /// The message `body` says, when it is a well-formed one.
    pub(crate) fn decode(body: &[u8]) -> Option<Self> {
        let carried: Self = postcard::from_bytes(body).ok()?;
        (carried.id != [0; 16] && carried.sequence != 0).then_some(carried)
    }
}

/// The largest unit `profile` sends: one frame, one stretch of sound, or one page.
pub(crate) fn unit_bound(profile: &Profile) -> u64 {
    match profile {
        Profile::Camera {
            format,
            width,
            height,
            ..
        } => {
            // At most 4096 × 2160 pixels of four bytes: far inside a u64.
            let pixels = u64::from(*width).saturating_mul(u64::from(*height));
            match format {
                VideoFormat::Nv12 | VideoFormat::Mjpeg => pixels.saturating_mul(3) / 2,
                VideoFormat::Rgba8 => pixels.saturating_mul(4),
            }
        }
        Profile::Microphone {
            format,
            sample_rate,
            channels,
            frame_millis,
        } => {
            let width = match format {
                AudioFormat::PcmS16Le => 2,
                AudioFormat::PcmF32Le => 4,
            };
            u64::from(*sample_rate)
                .saturating_mul(u64::from(*channels))
                .saturating_mul(width)
                .saturating_mul(u64::from(*frame_millis))
                / 1_000
        }
        Profile::Scanner {
            max_document_bytes, ..
        } => (*max_document_bytes).min(MAX_DOCUMENT_BYTES),
        Profile::Controller => 0,
    }
}

impl Piece {
    /// Whether this piece fits what `profile` carries: at most [`MAX_CHUNK_BYTES`], inside a unit
    /// no larger than the profile sends, and a page only of a scanned document, within its pages.
    pub(crate) fn fits(&self, profile: &Profile) -> bool {
        let len = u64::try_from(self.payload.len()).unwrap_or(u64::MAX);
        let inside = !self.payload.is_empty()
            && self.payload.len() <= MAX_CHUNK_BYTES
            && self
                .offset
                .checked_add(len)
                .is_some_and(|end| end <= self.total)
            && self.total <= unit_bound(profile);
        inside
            && match profile {
                Profile::Camera { .. } | Profile::Microphone { .. } => self.page.is_none(),
                Profile::Scanner { max_pages, .. } => self
                    .page
                    .is_some_and(|page| page < (*max_pages).min(MAX_PAGES)),
                Profile::Controller => false,
            }
    }
}

/// Split `unit`, the `page`-th page or a frame (`None`), into pieces of at most
/// [`MAX_CHUNK_BYTES`]; the last piece of the unit ends the document when `last`.
pub(crate) fn pieces(unit: &[u8], page: Option<u32>, last: bool) -> Vec<Piece> {
    let total = u64::try_from(unit.len()).unwrap_or(u64::MAX);
    let count = unit.len().div_ceil(MAX_CHUNK_BYTES);
    (1_usize..)
        .zip(unit.chunks(MAX_CHUNK_BYTES))
        .map(|(number, payload)| Piece {
            offset: u64::try_from(number.saturating_sub(1).saturating_mul(MAX_CHUNK_BYTES))
                .unwrap_or(u64::MAX),
            total,
            page,
            last: last && number == count,
            payload: payload.to_vec(),
        })
        .collect()
}

/// Whether `kind` is captured: a camera, microphone or scanner, not a controller.
pub(crate) const fn captured(kind: RemoteKind) -> bool {
    !matches!(kind, RemoteKind::Controller)
}
