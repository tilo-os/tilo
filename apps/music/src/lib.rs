//! Music — qOS native streaming + library app, built on the CRDT-native
//! `ui` toolkit.
//!
//! Architecture:
//!   * [`model`]  — pure data: catalog (artists/albums/tracks), playlists,
//!                  liked songs, saved albums, cache state and live playback.
//!   * [`seed`]   — the bundled offline sample library (populates `--shots`).
//!   * [`doc`]    — the Automerge bridge: library + playlists + liked + saved
//!                  albums + play-state replicate across a user's devices
//!                  ("our own library layer"), with mutation helpers
//!                  (create/rename/delete playlist, reorder/remove tracks,
//!                  toggle like / saved album).
//!   * [`backend`]— the **swappable** YouTube-Music data source: a
//!                  [`backend::MusicBackend`] trait with an offline sample impl
//!                  (default) and a real rustypipe InnerTube impl (`live`
//!                  feature) that does real song search and resolves a
//!                  directly-playable audio-stream URL. PoToken is a seam
//!                  ([`backend::potoken`]).
//!   * [`stream`] — the non-blocking worker driving [`backend`] off the UI
//!                  thread (search + stream-resolution requests in, results
//!                  drained per-frame).
//!   * [`source`] — legacy Piped/Invidious audio endpoint (network gated behind
//!                  the `streaming` feature); superseded by [`backend`]/`live`.
//!   * [`audio`]  — GStreamer `playbin` output + live position query (gated
//!                  behind `audio`; the playhead tracks the real pipeline).
//!   * [`lyrics`] — synced-or-plain lyrics: an LRC parser + a bundled sample
//!                  library (always compiled) + a feature-gated async lrclib
//!                  fetch worker (`lyrics` feature).
//!   * [`sync`]   — the CRDT P2P sync seam: a [`sync::LibrarySync`] transport
//!                  contract, an always-compiled in-process loopback, and a
//!                  feature-gated live iroh transport (`p2p`).
//!   * [`storage`]— on-disk persistence of the Automerge document.
//!   * [`relay`]  — the **headless** library server (`music --relay`): a home
//!                  server runs the document + sync transport with no GUI so
//!                  clients sync the library P2P (the qOS self-hosting model).
//!   * [`art`] / [`icon`] — procedural cover art + vector chrome glyphs.
//!   * [`app`]    — the root [`MusicApp`] widget that draws the whole UI:
//!                  collapsible sidebar, right-click track [`ContextMenu`],
//!                  queue + playlist management, like/heart affordances, and
//!                  the lyrics view.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.
//!
//! [`ContextMenu`]: ui::widget::context_menu::ContextMenu

pub mod app;
pub mod art;
pub mod audio;
pub mod backend;
pub mod doc;
pub mod icon;
pub mod lyrics;
pub mod model;
pub mod relay;
pub mod seed;
pub mod source;
pub mod storage;
pub mod stream;
pub mod sync;

pub use app::{MusicApp, View};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(MusicApp::new())
}

/// Like [`app_root`] but staged for a named `--shots` scenario. `--shots`
/// renders a single offscreen frame with no event loop, so a state that would
/// normally need live input (an opened overlay sidebar, an open context menu)
/// is pre-snapped here.
///
/// Recognised scenarios (combine with `QOS_MUSIC_VIEW` to pick the screen):
/// * `"sidebar"` — the navigation sidebar snapped fully open. Paired with a
///   narrow `--viewport`, this captures the overlay flap + dimming scrim;
///   at a wide viewport it simply shows the docked sidebar.
/// * `"collapsed"` — the docked sidebar snapped collapsed (content full-width),
///   to capture the docked-collapse affordance.
/// * `"context"` — a track right-click context menu opened over the library.
/// * `"new-playlist"` — the inline create-playlist name prompt open.
/// * `"search-focus"` — the header search pill focused with an empty query, so
///   the caret sits at the field start (not at the end of the placeholder).
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    let mut app = MusicApp::new();
    // Hermetic: a headless shot never spawns a worker or fetches covers, so the
    // frame is deterministic and offline (bundled sample + placeholder tiles).
    app.set_offline_for_shot();
    match scenario {
        "sidebar" => app.snap_sidebar(true),
        "collapsed" => app.snap_sidebar(false),
        "context" => app.snap_track_menu(),
        "new-playlist" => app.snap_new_playlist_prompt(),
        "search-focus" => app.snap_search_focus(),
        _ => {}
    }
    Box::new(app)
}

/// Build a Music app for a **live** screenshot: networking stays enabled so the
/// warm-up loop in the binary can fetch real Home/artist/album content + real
/// cover art before capturing the frame. `QOS_MUSIC_VIEW` picks the screen.
pub fn live_shot_app() -> MusicApp {
    MusicApp::new()
}
