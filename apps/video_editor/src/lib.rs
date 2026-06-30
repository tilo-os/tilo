//! Video Editor — a CRDT-native non-linear editor for qOS, built on the
//! `ui` toolkit.
//!
//! Layers:
//!   * [`model`]   — pure data + editing math (split / trim / move), tested.
//!   * [`doc`]     — Automerge projection of the project (the collaborative,
//!                   undoable source of truth) + the demo project seed.
//!   * [`timecode`]— wall-clock ↔ frame conversions and formatting.
//!   * [`export`]  — export presets + the (GStreamer-gated) render pipeline.
//!   * [`app`]     — the root [`app::VideoEditorApp`] widget: media bin,
//!                   preview + transport, multi-track timeline, inspector,
//!                   export dialog — all drawn into one Vello scene.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

pub mod app;
pub mod doc;
pub mod export;
pub mod model;
pub mod timecode;

pub use app::VideoEditorApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(VideoEditorApp::new())
}
