//! Music — qOS native streaming + library app, built on the CRDT-native
//! `ui` toolkit.
//!
//! Architecture:
//! - [`model`] — pure data: catalog, playlists, liked/saved state, playback.
//! - [`seed`] — the bundled offline sample library (`--shots`, tests).
//! - [`doc`] — the Automerge bridge: library + play-state replicate across a
//!   user's devices, with the mutation helpers apps/actions dispatch through.
//! - [`backend`] — the swappable YouTube-Music data source
//!   ([`backend::MusicBackend`]): an offline sample impl (default) and a real
//!   rustypipe impl (`live` feature). `PoToken` is a seam ([`backend::potoken`]).
//! - [`stream`] — the non-blocking worker driving [`backend`] off the UI thread.
//! - [`source`] — legacy Piped/Invidious endpoint (`streaming` feature),
//!   superseded by [`backend`]/`live`.
//! - [`audio`] — `GStreamer` `playbin` output + live position query (`audio`
//!   feature).
//! - [`lyrics`] — LRC parsing + a bundled sample library, plus a feature-gated
//!   async lrclib fetch worker (`lyrics` feature).
//! - [`sync`] — the CRDT P2P sync seam: a [`sync::LibrarySync`] transport
//!   contract, an in-process loopback, and a live iroh transport (`p2p`).
//! - [`storage`] — on-disk persistence of the Automerge document.
//! - [`relay`] — the headless library server (`music --relay`).
//! - [`art`] / [`icon`] — procedural cover art + vector chrome glyphs.
//! - [`app`] — the root [`MusicApp`] widget that draws the whole UI.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

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
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod art;
pub mod audio;
pub mod backend;
pub mod doc;
pub mod icon;
pub mod lyrics;
pub mod model;
pub mod mpris;
pub mod net;
pub mod relay;
pub mod search_type;
pub mod seed;
pub mod source;
pub mod storage;
pub mod stream;
pub mod sync;

pub use app::{MusicApp, View};
pub use search_type::{MusicSearchProvider, SearchHit, SearchableType};

use ui::widget::Widget;

/// The searchable result types Music contributes to qOS's global search. An OS
/// search host registers these descriptors (id, display name, whether hits
/// carry a cover) once, then drives one of the providers per query. Exposing
/// this list keeps the host's integration a one-liner
/// (`for t in music::searchable_types() { registry.register(t) }`) with no
/// reach into the app internals — the same seam [`apps/tube`] pins for videos.
pub fn searchable_types() -> Vec<SearchableType> {
    search_type::all_searchable_types()
}

/// Build and return the app's root widget tree.
///
/// This is the live-binary entry (not `--shots`/tests), so it also brings up
/// the MPRIS D-Bus service (media keys + the desktop media controls). MPRIS
/// degrades to a no-op stub without the `audio` feature or a session bus, so
/// this never fails.
pub fn app_root() -> Box<dyn Widget> {
    let mut app = MusicApp::new();
    app.enable_mpris();
    Box::new(app)
}

/// Like [`app_root`] but staged for a named `--shots` scenario. `--shots`
/// renders a single offscreen frame with no event loop, so a state that would
/// normally need live input (an opened overlay sidebar, an open context menu)
/// is pre-snapped here.
///
/// Recognised scenarios (combine with `QOS_MUSIC_VIEW` to pick the screen):
/// `sidebar`/`collapsed` — nav sidebar open/collapsed; `context` — track
/// context menu open; `new-playlist` — inline create-playlist prompt open;
/// `empty` — a genuinely empty library's Home state; `search-focus` — search
/// pill focused, empty query; `skeleton` — Home in its loading state;
/// `nowplaying` — a bundled track playing; `queue` — the Queue view with
/// sample tracks trailing it; `lyrics` — synced-lyrics view; `downloading` —
/// a few rows mid-download; `search-tabs`/`search-songs`/`search-artists` —
/// search results filtered to that tab; `artist-page`/`album-page` — the
/// detail page for a bundled item; `error` — a failed live detail request with
/// its honest recovery state; `membership-open` — a playlist-membership Ctrl+K
/// result after its safe canonical Open action is activated.
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    let mut app = MusicApp::new();
    // Hermetic: a headless shot never spawns a worker or fetches covers, so the
    // frame is deterministic and offline (bundled sample + placeholder tiles).
    app.set_offline_for_shot();
    // Most screenshots need real content, so they use the bundled catalog.
    // `empty` deliberately stays unseeded to capture the real first-run state
    // (no songs, albums, artists, or playlists) rather than an empty search
    // field over fabricated content.
    if scenario != "empty" {
        app.seed_sample_library();
    }
    match scenario {
        "sidebar" => app.snap_sidebar(true),
        "collapsed" => app.snap_sidebar(false),
        "context" => app.snap_track_menu(),
        "new-playlist" => app.snap_new_playlist_prompt(),
        "search-focus" => app.snap_search_focus(),
        "skeleton" => app.snap_skeleton(),
        "nowplaying" => app.snap_nowplaying(),
        "queue" => app.snap_queue(),
        "lyrics" => app.snap_lyrics(),
        "downloading" => app.snap_downloading(),
        "search-tabs" => app.snap_search_results(app::SearchTab::All),
        "search-songs" => app.snap_search_results(app::SearchTab::Songs),
        "search-artists" => app.snap_search_results(app::SearchTab::Artists),
        "artist-page" => app.snap_artist_page(),
        "album-page" => app.snap_album_page(),
        "error" => app.snap_error_page(),
        "membership-open" => app.snap_membership_open(),
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
