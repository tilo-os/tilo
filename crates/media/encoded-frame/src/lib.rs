//! The semantic contract of a first-party encoded video frame.
//!
//! Two wire profiles carry Tilo's own encoded pictures: the compositor's
//! per-share 13-byte frame header (`tilo-callstream`) and the `MoQ`
//! `tilo-video/1` 32-byte object header (`tilo-moq`). Their bytes differ —
//! one is a little-endian object with a version byte, the other a big-endian
//! access unit with a flags byte — so each stays a profile of its own. What
//! they must not disagree about is the meaning of a picture:
//!
//! * which codec an id names ([`Codec`], [`Profile`]);
//! * the metadata every picture carries ([`FrameMeta`]);
//! * the bounds a receiver enforces before allocating ([`MAX_CALLSTREAM_PAYLOAD`],
//!   [`MAX_MOQ_OBJECT`], [`MAX_PICTURE_SIDE`]);
//! * the validation of a payload and a picture size ([`validate_payload`],
//!   [`validate_side`]); and
//! * when a delta may reach a decoder: never after a missing reference, and
//!   never before a keyframe ([`UnitChain`], [`Sequence`]).
//!
//! The profiles keep their own byte layouts here: this crate owns the
//! contract, not the carriage. A profile that changes its bytes versions
//! them in the crate that writes them; the definitions they share are
//! compiled from this one place.
//!
//! Nothing here depends on a UI, a server protocol or a media backend, so
//! both a browser and a relay can compile the same contract. In particular
//! the keyframe-gap authority used to live in the peer-sync protocol; it is
//! media semantics, and now lives beside the frames it admits.

pub mod codec;
pub mod frame;
pub mod gap;

pub use codec::{Codec, Profile};
pub use frame::{
    FrameMeta, InvalidFrame, MAX_CALLSTREAM_PAYLOAD, MAX_MOQ_OBJECT, MAX_PICTURE_SIDE,
    validate_payload, validate_side,
};
pub use gap::{Admission, Sequence, SequenceError, SequenceEvent, UnitChain};

#[cfg(test)]
mod tests;
