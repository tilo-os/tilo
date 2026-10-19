//! How a Tilo app hands its frames to the render service of its room, and
//! how the service makes sure a frame is safe to draw.
//!
//! An app does not rasterize. Its toolkit builds the widget tree, lays it out
//! and records the frame as vello's encoding, as before, then writes that
//! encoding into shared memory instead of onto a GPU. The room's render
//! service — the only toolkit-app process that opens the GPU — validates it,
//! rasterizes it on its warm device into dma-bufs and hands those back; the app
//! attaches them to its own `wl_surface`, so the compositor imports them like
//! any client buffer (zero copies, direct scanout kept).
//! `docs/architecture/app-rendering.md` has the whole design.
//!
//! This crate is the contract both sides compile:
//!
//! - [`scene`]: a vello [`Encoding`](vello_encoding::Encoding) as bytes, with
//!   fonts, images and external surfaces referenced by id instead of by
//!   pointer.
//! - [`frame`]: one frame of one surface: its size, damage, scroll copies,
//!   the layers it changes and the backdrops it blurs. Records are
//!   type-length-value sections, so later steps (scene annotations,
//!   service-run motion) add sections without breaking older peers.
//! - [`validate`]: the CPU pass every scene passes before a GPU sees it, and
//!   the per-app [`Limits`] it enforces. A scene is hostile until it passes.
//! - [`control`]: the messages on the control channel ("frame N is in the
//!   ring", "frame N is in buffer B", "frame N was presented"), and on Linux
//!   [`channel`] carries them with descriptors over a `SOCK_SEQPACKET` pair,
//!   and [`shm`] the sealed memfds: the ring frames are written to, and the
//!   images an app uploads once.

pub mod control;
pub mod frame;
pub mod limits;
pub mod scene;
pub mod validate;

#[cfg(target_os = "linux")]
pub mod broker;
#[cfg(target_os = "linux")]
pub mod channel;
#[cfg(target_os = "linux")]
pub mod shm;

mod bytes;

pub use limits::Limits;

/// The version both peers name in their hello. A peer that speaks a newer
/// version must still accept this one's sections; a section it does not
/// know is skipped only when its sender marked it optional.
pub const PROTOCOL_VERSION: u16 = 1;

/// A font an app registered, by the number it chose.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct FontId(pub u32);

/// An image an app uploaded once, by the number it chose.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct ImageId(pub u32);

/// A surface (a window, a popup) the app renders through the service.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct SurfaceId(pub u32);

/// One layer of a surface's scene. Layer 0 is the surface's content; others
/// are parts of it the app updates on their own (scrolling regions,
/// popovers) and, from step 2 on, parts the service animates.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct LayerId(pub u32);

impl LayerId {
    /// The layer a surface's whole content is drawn in.
    pub const ROOT: Self = Self(0);
}

/// Why bytes could not be read as a record, or a record was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("the record ends early")]
    Truncated,
    #[error("a count or offset overflows")]
    Overflow,
    #[error("malformed record: {0}")]
    Malformed(&'static str),
    #[error("unknown required section {0}")]
    UnknownSection(u16),
    #[error("the record names {0}, which this connection never registered")]
    UnknownResource(&'static str),
    #[error("the record is over its limit: {0}")]
    OverLimit(&'static str),
    #[error("invalid scene: {0}")]
    Invalid(&'static str),
}
