//! The Video Editor's picture: the compositor one frame is drawn through, and the encode a cut
//! is written by.
//!
//! [`preview`] decides what stands on a frame, [`gpu`] draws it, and [`export`] writes a timeline
//! to a file; [`render`] is what a device answers when a package asks it to draw a still or
//! render a cut by name. The project model and its editing math are [`video_edit`]'s: this crate
//! links the renderer, so it carries the toolkit's GPU stack and no widget.

// Frame maths on sampled values: an integer that wraps here costs one render, not the desktop.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "media maths on sampled values"
)]

pub mod export;
pub mod gpu;
pub mod page_sound;
pub mod preview;
pub mod render;
pub mod thumbnail;

#[cfg(test)]
mod seed;

pub use video_edit::Project;
