//! Video Editor — a CRDT-native non-linear editor for Tilo, built on the
//! `ui` toolkit.
//!
//! Layers:
//!   * [`model`]   — pure data + editing math (split / trim / move / ripple /
//!     snap / paste, filters + fades), tested.
//!   * [`curve`]   — tone curves, baked to the 256-entry lookup table the
//!     compositor reads; the fine half of the colour grade.
//!   * [`media`]   — the FUSE-media seam: clips reference workspace paths
//!     (`/var/workspace/…`); bytes hydrate on demand.
//!   * [`doc`]     — Automerge projection of the project (the collaborative,
//!     undoable source of truth) + the demo project seed.
//!   * [`project_file`] — the versioned `.qvp` file behind Save / Save As /
//!     Open: the same Automerge document, framed and stamped,
//!     so a project can be copied and handed to someone.
//!   * [`preview`] — the composited-frame producer: a representative frame
//!     from clip metadata, or (behind the `decode` feature) a
//!     real decoded frame via `ui::VideoPlayer` / `GStreamer`.
//!   * [`timecode`]— wall-clock ↔ frame conversions and formatting.
//!   * [`gpu`]     — the wgpu compositor both the preview canvas and the
//!     export render through, so they cannot disagree.
//!   * [`audio`]   — which clips are audible at an instant, and the mix.
//!   * [`export`]  — export presets + the (GStreamer-gated) render pipeline.
//!   * [`shot`]    — the fingerprint that proves two runs of a `--shots`
//!     scenario describe one picture.
//!   * [`presence`] — who else is in the edit: the presence keys for the
//!     timeline domain, what this editor publishes about
//!     where its user is, and how it draws everyone else.
//!   * [`app`]     — the root [`app::VideoEditorApp`] widget: media bin,
//!     preview + transport, multi-track timeline, inspector,
//!     export dialog — all drawn into one Vello scene.
//!
//! The app runs on the app framework ([`VideoEditor`]): the live window and
//! every capture mode host the same editor, and a `--shots` scenario stages
//! its project through the host and the editor's view state through
//! `ResourceSurface::stage`.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod app;
pub mod audio;
pub mod curve;
pub mod export;
mod files;
pub mod gpu;
pub(crate) mod layout;
pub mod media;
pub mod model;
pub mod param;
pub mod portal;
pub mod presence;
pub mod preview;
pub mod probe;
pub mod project_file;
pub mod resource;
pub mod search;
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
    ui::framework::definition::<VideoEditor>()
}
