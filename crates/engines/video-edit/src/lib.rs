//! The Video Editor's engine: the project model and every edit that can be made to it, what a
//! media file really is, the project file format, the audio mix, waveforms, thumbnails and
//! decoded frames.
//!
//! This crate has no UI: no widgets, no window, no toolkit. The app draws the bin, the preview,
//! the timeline and the inspector, and hands this crate the project; a render helper and a
//! workspace's host render a cut through the same code, so what a person sees and what a file
//! gets are one implementation.
//!
//! [`model`] is the data and the editing math; [`probe`] reads a file's real format;
//! [`audio`] mixes and [`waveform`] feeds the drawing, and [`video_frames`] decodes filmstrip
//! frames on the shared engine.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "media maths on sampled values; the app's own note says why the crate allows it"
)]

pub mod angles;
pub mod audio;
pub mod curve;
pub mod files;
pub mod media;
pub mod model;
pub mod param;
pub mod probe;
pub mod timecode;
#[cfg(feature = "decode")]
pub mod video_frames;
pub mod waveform;

pub use model::Project;
