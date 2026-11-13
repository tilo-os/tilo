//! Video Editor — a record-native non-linear editor for Tilo, built on the `ui` toolkit.
//!
//! [`model`] holds the pure data and editing math, [`resource`] the project's verbs and the
//! app, and [`app`] the root widget: media bin, preview with transport, timeline, inspector
//! and export dialog, all drawn into one Vello scene. [`gpu`] is the compositor the preview
//! and the export both render through; [`preview`] decides what it composites.
//!
//! The app runs on the app framework ([`VideoEditor`]): the live window and every capture mode
//! host the same editor.

// An app runs as its own sandboxed process, so an integer that wraps here costs one app rather
// than the desktop. The lints that produce a *crash* stay denied — indexing_slicing,
// string_slice, unwrap_used, expect_used, panic — because a panicking app is a bad app whoever
// wrote it. `shell`, `ui` and everything under `crates/` keep the full set.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod agent_render;
pub mod agent_timeline;
pub mod angles;
pub mod app;
pub mod audio;
pub mod curve;
pub mod export;
mod files;
pub mod gpu;
pub(crate) mod layout;
pub mod media;
pub mod model;
#[cfg(not(any(feature = "gst", feature = "decode")))]
mod page_sound;
pub mod param;
pub mod portal;
pub mod presence;
pub mod preview;
pub mod probe;
pub mod project_file;
pub mod resource;
pub mod search;
#[cfg(feature = "samples")]
pub mod seed;
pub mod shot;
pub mod thumbnail;
pub mod timecode;
pub mod waveform;

pub use app::VideoEditorApp;
pub use model::Project;
pub use resource::VideoEditor;

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(VideoEditor)
}
