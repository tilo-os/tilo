//! The paint stream as a `tilo-paint` surface stream.
//!
//! A session shares a window with web members the way it shares any legacy
//! window — `SurfaceStreamStart`, `SurfaceFrame`s, `SurfaceStreamEnd` — with
//! [`CODEC`] as the codec label and one [`Payload`] per frame. The relay keeps
//! the latest group (from a keyframe on) for members that join later, so a
//! [`Payload::Key`] starts a group and is followed by the blobs its frame
//! references, and later [`Payload::Patches`] and new blobs extend it.
//!
//! [`Payload::is_key`] reads only the first byte: postcard writes an enum's
//! variant index first, so a relay detects keyframes without decoding.

use serde::{Deserialize, Serialize};

use crate::{Frame, Patch};

/// `SurfaceStreamStart::codec` of a paint stream.
pub const CODEC: &str = "tilo-paint";

/// Largest encoded payload; the relay forwards no larger frame.
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Largest blob carried whole, leaving room for the payload's own framing.
pub const MAX_BLOB_BYTES: usize = MAX_PAYLOAD_BYTES - 1024;

/// One frame of a paint stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Payload {
    /// A whole frame: the start of a group a late member can begin from.
    Key(Frame),
    /// Changes to the frame the receiver holds, oldest first.
    Patches(Vec<Patch>),
    /// The encoded bytes an image item references by this blake3 hash.
    Blob { hash: [u8; 32], bytes: Vec<u8> },
}

/// Why bytes are not a payload.
#[derive(Debug)]
pub enum WireError {
    TooLarge(usize),
    Codec(postcard::Error),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(n) => {
                write!(f, "paint payload of {n} bytes exceeds {MAX_PAYLOAD_BYTES}")
            }
            Self::Codec(e) => write!(f, "paint payload: {e}"),
        }
    }
}

impl std::error::Error for WireError {}

impl Payload {
    /// Encode, refusing anything the relay would not forward.
    ///
    /// # Errors
    ///
    /// [`WireError::TooLarge`] past [`MAX_PAYLOAD_BYTES`]; a codec error.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        let bytes = crate::encode(self).map_err(WireError::Codec)?;
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(WireError::TooLarge(bytes.len()));
        }
        Ok(bytes)
    }

    /// Decode one frame's bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(WireError::TooLarge(bytes.len()));
        }
        crate::decode(bytes).map_err(WireError::Codec)
    }

    /// Whether `bytes` encode a [`Payload::Key`], from the first byte alone.
    #[must_use]
    pub fn is_key(bytes: &[u8]) -> bool {
        bytes.first() == Some(&0)
    }
}

/// Keeps a receiver's frame from a stream of payloads: what a viewer holds.
#[derive(Default)]
pub struct Receiver {
    frame: Option<Frame>,
    /// A patch did not apply: ignore patches until the next keyframe.
    lost: bool,
}

impl Receiver {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one payload. Returns a blob it carried, for the caller's store.
    pub fn apply(&mut self, payload: Payload) -> Option<([u8; 32], Vec<u8>)> {
        match payload {
            Payload::Key(frame) => {
                self.frame = Some(frame);
                self.lost = false;
                None
            }
            Payload::Patches(patches) => {
                if self.lost {
                    return None;
                }
                let Some(frame) = self.frame.as_mut() else {
                    self.lost = true;
                    return None;
                };
                for patch in &patches {
                    if frame.apply(patch).is_err() {
                        self.lost = true;
                        break;
                    }
                }
                None
            }
            Payload::Blob { hash, bytes } => Some((hash, bytes)),
        }
    }

    #[must_use]
    pub const fn frame(&self) -> Option<&Frame> {
        self.frame.as_ref()
    }

    /// The receiver missed a patch and waits for the next keyframe.
    #[must_use]
    pub const fn is_lost(&self) -> bool {
        self.lost
    }
}
