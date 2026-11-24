//! A window shared with the workspace's other devices, from the owner's
//! side: the share session that stages captured frames and encodes them with
//! the codec its viewers negotiated ([`flatpak_share::SharedFlatpakSession`]);
//! the encoders behind it ([`encoder`]); and the settings it is encoded with
//! ([`config`], [`hdr_metadata`]). What it encodes is published on the
//! workspace's live session as the window's track (`tilo_live`).
//!
//! Capture and input injection stay in the compositor, which provides its
//! frame store ([`window_frames::provide`]). This crate sits
//! below it, so a change to the compositor does not rebuild the encode
//! path and its tests run on their own.

pub mod config;
pub mod encoder;
pub mod flatpak_share;
pub mod hdr_metadata;
pub mod window_frames;
