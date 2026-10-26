//! The metadata both video profiles describe, and the bounds they refuse to
//! exceed.

use crate::codec::Codec;

/// Largest encoded payload the callstream profile carries, in bytes.
///
/// Deliberately far below the `u32` wire representation: one realtime picture
/// larger than this is not useful, and accepting it would let a peer reserve
/// an unbounded amount of receiver memory.
pub const MAX_CALLSTREAM_PAYLOAD: usize = 8 * 1024 * 1024;

/// Largest whole `tilo-video/1` object, header included, in bytes.
pub const MAX_MOQ_OBJECT: usize = 512 * 1024;

/// Largest picture side a profile that signals a size may claim, in pixels.
pub const MAX_PICTURE_SIDE: u16 = 4096;

/// The parts of an encoded picture both first-party video profiles describe.
///
/// A profile that omits a field carries it as `None` here; the bytes and the
/// presence flag stay the profile's own. `codec` is the semantic codec, not a
/// profile id: [`crate::Profile::codec_id`] translates it for the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameMeta {
    /// Whether this unit is a random-access point.
    pub keyframe: bool,
    /// The compressed format of the payload.
    pub codec: Codec,
    /// The sender's monotonic picture number on its track.
    pub sequence: u64,
    /// Capture time in microseconds on the sender's clock, when the profile
    /// carries one.
    pub timestamp_us: Option<u64>,
    /// The picture size to show, when the profile carries one. A profile may
    /// carry `(0, 0)` for "not signalled".
    pub display_size: Option<(u16, u16)>,
}

impl FrameMeta {
    /// A picture with neither time nor size signalled.
    #[must_use]
    pub const fn new(keyframe: bool, codec: Codec, sequence: u64) -> Self {
        Self {
            keyframe,
            codec,
            sequence,
            timestamp_us: None,
            display_size: None,
        }
    }

    /// The same picture with its capture time.
    #[must_use]
    pub const fn with_timestamp(self, timestamp_us: u64) -> Self {
        Self {
            timestamp_us: Some(timestamp_us),
            ..self
        }
    }

    /// The same picture with the size to show.
    #[must_use]
    pub const fn with_display_size(self, width: u16, height: u16) -> Self {
        Self {
            display_size: Some((width, height)),
            ..self
        }
    }

    /// The width to show, or `0` when the profile carries none.
    pub const fn width(&self) -> u16 {
        match self.display_size {
            Some((width, _)) => width,
            None => 0,
        }
    }

    /// The height to show, or `0` when the profile carries none.
    pub const fn height(&self) -> u16 {
        match self.display_size {
            Some((_, height)) => height,
            None => 0,
        }
    }
}

/// Why an encoded picture cannot be admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvalidFrame {
    /// A payload larger than the profile's bound.
    #[error("encoded payload of {len} bytes exceeds the {max}-byte limit")]
    PayloadTooLarge { len: usize, max: usize },
    /// A picture that claims no pixels.
    #[error("picture size {width}x{height} has a zero side")]
    EmptySide { width: u16, height: u16 },
    /// A picture larger than the profile's side bound.
    #[error("picture size {width}x{height} exceeds the {max}-pixel side limit")]
    SideTooLarge { width: u16, height: u16, max: u16 },
}

/// Refuse a payload larger than `max` bytes.
///
/// An empty payload is left to the profile: the callstream header permits
/// one, the `MoQ` object does not.
pub fn validate_payload(len: usize, max: usize) -> Result<(), InvalidFrame> {
    if len > max {
        return Err(InvalidFrame::PayloadTooLarge { len, max });
    }
    Ok(())
}

/// Refuse a picture with a zero side or one larger than `max` pixels.
pub fn validate_side(width: u16, height: u16, max: u16) -> Result<(), InvalidFrame> {
    if width == 0 || height == 0 {
        return Err(InvalidFrame::EmptySide { width, height });
    }
    if width > max || height > max {
        return Err(InvalidFrame::SideTooLarge { width, height, max });
    }
    Ok(())
}
