//! Video Editor — a CRDT-native non-linear editor for qOS, built on the
//! `ui` toolkit.
//!
//! Layers:
//!   * [`model`]   — pure data + editing math (split / trim / move / ripple /
//!                   snap / paste, filters + fades), tested.
//!   * [`media`]   — the FUSE-media seam: clips reference workspace paths
//!                   (`/var/workspace/…`); bytes hydrate on demand.
//!   * [`doc`]     — Automerge projection of the project (the collaborative,
//!                   undoable source of truth) + the demo project seed.
//!   * [`preview`] — the composited-frame producer: a representative frame
//!                   from clip metadata, or (behind the `decode` feature) a
//!                   real decoded frame via `ui::VideoPlayer` / GStreamer.
//!   * [`timecode`]— wall-clock ↔ frame conversions and formatting.
//!   * [`export`]  — export presets + the (GStreamer-gated) render pipeline.
//!   * [`app`]     — the root [`app::VideoEditorApp`] widget: media bin,
//!                   preview + transport, multi-track timeline, inspector,
//!                   export dialog — all drawn into one Vello scene.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

pub mod actions;
pub mod app;
pub mod doc;
pub mod export;
pub mod media;
pub mod model;
pub mod portal;
pub mod preview;
pub mod probe;
pub mod timecode;

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
    Box::new(VideoEditorApp::new_for_shot(app::ShotScenario::SelectedFilters))
}

/// Root configured to capture the timeline immediately after a split at the
/// playhead — two contiguous clips where one used to be (the "split just
/// performed" shot).
pub fn app_root_split() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new_for_shot(app::ShotScenario::JustSplit))
}
