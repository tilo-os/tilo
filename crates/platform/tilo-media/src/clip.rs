//! Video clip recording with one API on the desktop and the page.
//!
//! A desktop encodes Motion-JPEG into an AVI of its own (`avi`); a page hands
//! its pictures to the browser's `WebCodecs` encoder and muxes the result into
//! `WebM` (`web`). Both keep the pictures without a device driver, a Wayland
//! server or a codec stack the platform crate would have to link, and both
//! name the clip by what its bytes are once it is written.
//!
//! The app that records hands in the JPEG encoder its own build carries
//! ([`JpegFrame`]): the platform layer compiles no codec, and the page ignores
//! it because the browser encodes there.

#[cfg(target_arch = "wasm32")]
use std::path::Path;

/// Frames per second written into the container timing. A clip plays back
/// whatever rate frames actually arrive; 30 is the standard assumption used
/// for the containers' rate and duration maths.
pub const FPS: u32 = 30;

/// The container this build records into, for a clip whose bytes name none.
#[cfg(not(target_arch = "wasm32"))]
pub const CONTAINER: &str = "avi";
#[cfg(target_arch = "wasm32")]
pub const CONTAINER: &str = "webm";

/// One picture handed to a [`Recorder`]: its size and its RGBA bytes.
pub struct Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

/// The muxed clip with its duration in milliseconds, or why it could not be
/// muxed.
pub type ClipResult = Result<(Vec<u8>, i64), String>;

/// The JPEG an app's build encodes a desktop frame with: RGB bytes, the size,
/// and the quality. `None` when the frame could not be encoded.
pub type JpegFrame = fn(&[u8], u32, u32, u8) -> Option<Vec<u8>>;

#[cfg(not(target_arch = "wasm32"))]
mod avi;
#[cfg(not(target_arch = "wasm32"))]
pub use avi::{Recorder, probe_avi};

/// The `WebM` container a page's clip is muxed into. Native builds compile it
/// too, so its tests run everywhere; only the page's recorder muxes with it.
pub mod webm;

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::Recorder;

/// Read the `avih` header of an AVI clip: its size and duration, or `None`
/// when the file is not one.
#[cfg(target_arch = "wasm32")]
pub fn probe_avi(_path: &Path) -> Option<(u32, u32, i64)> {
    None
}
