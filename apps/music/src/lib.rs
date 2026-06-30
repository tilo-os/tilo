//! Music — qOS native streaming + library app, built on the CRDT-native
//! `ui` toolkit.
//!
//! Architecture:
//!   * [`model`]  — pure data: catalog (artists/albums/tracks), playlists,
//!                  cache state and live playback.
//!   * [`seed`]   — the bundled offline sample library (populates `--shots`).
//!   * [`doc`]    — the Automerge bridge: library + play-state replicate
//!                  across a user's devices ("our own library layer").
//!   * [`source`] — YouTube-Music streaming via a Piped/Invidious audio
//!                  endpoint (network gated behind the `streaming` feature).
//!   * [`audio`]  — GStreamer `playbin` output (gated behind `audio`).
//!   * [`art`] / [`icon`] — procedural cover art + vector chrome glyphs.
//!   * [`app`]    — the root [`MusicApp`] widget that draws the whole UI.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

pub mod app;
pub mod art;
pub mod audio;
pub mod doc;
pub mod icon;
pub mod model;
pub mod seed;
pub mod source;

pub use app::{MusicApp, View};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(MusicApp::new())
}
