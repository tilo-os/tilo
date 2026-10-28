//! A window shared with the workspace's other devices, from the owner's
//! side: the share session that stages captured frames, encodes them with
//! the codec its viewers negotiated and wraps them in the neighbour
//! protocol's stream envelopes ([`flatpak_share::SharedFlatpakSession`]);
//! the encoders behind it ([`encoder`]); the registry of who views a share
//! and may drive its input ([`streaming`]); the counters the OSD shows
//! ([`diagnostics`]); and the settings it is encoded with ([`config`],
//! [`hdr_metadata`]).
//!
//! Transport, capture and input injection stay in the compositor, which
//! provides its frame store ([`window_frames::provide`]). This crate sits
//! below it, so a change to the compositor does not rebuild the encode
//! path and its tests run on their own.

pub mod config;
pub mod diagnostics;
pub mod encoder;
pub mod flatpak_share;
pub mod hdr_metadata;
pub mod streaming;
pub mod window_frames;
