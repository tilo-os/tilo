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

/// Like [`app_root`] but staged for a named `--shots` scenario. `--shots`
/// renders a single offscreen frame with no event loop, so a state that would
/// normally need live input (an opened overlay sidebar) is pre-snapped here.
///
/// Recognised scenarios:
/// * `"sidebar"` — the navigation sidebar snapped fully open. Paired with a
///   narrow `--viewport`, this captures the overlay flap + dimming scrim;
///   at a wide viewport it simply shows the docked sidebar.
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    let mut app = MusicApp::new();
    if scenario == "sidebar" {
        app.snap_sidebar(true);
    }
    Box::new(app)
}
