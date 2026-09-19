//! Video Editor — a CRDT-native non-linear editor for Tilo, built on the
//! `ui` toolkit.
//!
//! Layers:
//!   * [`model`]   — pure data + editing math (split / trim / move / ripple /
//!                   snap / paste, filters + fades), tested.
//!   * [`curve`]   — tone curves, baked to the 256-entry lookup table the
//!                   compositor reads; the fine half of the colour grade.
//!   * [`media`]   — the FUSE-media seam: clips reference workspace paths
//!                   (`/var/workspace/…`); bytes hydrate on demand.
//!   * [`doc`]     — Automerge projection of the project (the collaborative,
//!                   undoable source of truth) + the demo project seed.
//!   * [`project_file`] — the versioned `.qvp` file behind Save / Save As /
//!                   Open: the same Automerge document, framed and stamped,
//!                   so a project can be copied and handed to someone.
//!   * [`preview`] — the composited-frame producer: a representative frame
//!                   from clip metadata, or (behind the `decode` feature) a
//!                   real decoded frame via `ui::VideoPlayer` / `GStreamer`.
//!   * [`timecode`]— wall-clock ↔ frame conversions and formatting.
//!   * [`gpu`]     — the wgpu compositor both the preview canvas and the
//!                   export render through, so they cannot disagree.
//!   * [`audio`]   — which clips are audible at an instant, and the mix.
//!   * [`export`]  — export presets + the (GStreamer-gated) render pipeline.
//!   * [`shot`]    — composing a `--shots` frame, and the fingerprint that
//!                   proves two runs of a scenario describe one picture.
//!   * [`presence`] — who else is in the edit: the presence keys for the
//!                   timeline domain, what this editor publishes about
//!                   where its user is, and how it draws everyone else.
//!   * [`app`]     — the root [`app::VideoEditorApp`] widget: media bin,
//!                   preview + transport, multi-track timeline, inspector,
//!                   export dialog — all drawn into one Vello scene.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

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
pub mod doc;
pub mod export;
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
pub mod search_provider;
pub mod shot;
pub mod thumbnail;
pub mod timecode;
pub mod waveform;

pub use app::VideoEditorApp;

use ui::widget::Widget;

/// Build the live app: it opens on a blank project and never fabricates
/// media or clips.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new())
}

/// Build a fully-populated app for the hermetic `--shots`/demo path. This is
/// the sole source of the bundled sample project — the live app never seeds it.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new_demo())
}

/// Root tree configured for a deterministic narrow-window `--shots` capture:
/// the media bin is snapped to its open overlay state so a single offscreen
/// frame shows the collapsed sidebar revealed over the editor behind its
/// scrim (the animation never runs in a one-shot render).
pub fn app_root_narrow() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new_with_bin_open())
}

/// Root configured to capture the graded drone clip selected with its colour
/// filters + fades showing in the inspector (the "clip selected with filters"
/// shot). Deterministic: playhead parked inside the drone open.
pub fn app_root_filters() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new_for_shot(
        app::ShotScenario::SelectedFilters,
    ))
}

/// Root configured to capture the timeline immediately after a split at the
/// playhead — two contiguous clips where one used to be (the "split just
/// performed" shot).
pub fn app_root_split() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new_for_shot(app::ShotScenario::JustSplit))
}

/// Root configured to capture a bin item's context menu (Add / Rename / Delete).
pub fn app_root_menu() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new_for_shot(app::ShotScenario::BinMenu))
}

/// Root configured to capture a bin item being renamed in place.
pub fn app_root_rename() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new_for_shot(app::ShotScenario::BinRename))
}

/// Root configured to capture a bin clip mid-drag over a timeline lane.
pub fn app_root_drag() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new_for_shot(
        app::ShotScenario::DragToTimeline,
    ))
}

/// Root configured to capture the per-clip effects stack in the inspector.
pub fn app_root_effects() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new_for_shot(app::ShotScenario::Effects))
}
