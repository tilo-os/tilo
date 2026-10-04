//! Music — Tilo native streaming + library app, built on the CRDT-native
//! `ui` toolkit.
//!
//! Architecture:
//! - [`model`] — pure data: collaborative catalog/playlist state plus local
//!   playback/cache projections.
//! - [`seed`] — the bundled offline sample library (`--shots`, tests).
//! - [`doc`] — the Automerge bridge: user-authored library state replicates
//!   across devices; audio transport/cache never do.
//! - [`backend`] — the swappable YouTube-Music data source
//!   ([`backend::MusicBackend`]): a hermetic offline sample impl and the
//!   shipping-default rustypipe impl (`live` feature). `PoToken` is a seam
//!   ([`backend::potoken`]).
//! - [`stream`] — the non-blocking worker driving [`backend`] off the UI thread.
//! - [`audio`] — `GStreamer` `playbin` output + live position query (`audio`
//!   feature).
//! - [`lyrics`] — LRC parsing + a bundled sample library, plus a feature-gated
//!   async lrclib fetch worker (`live` or `lyrics` feature).
//! - the platform app document + action host — canonical shared library state,
//!   collaboration, history, and peer replication. Audio transport/cache stay
//!   device-local.
//! - [`storage`] — device-local playback profile and content-addressed cache.
//! - [`art`] / [`icon`] — procedural cover art + vector chrome glyphs.
//! - [`app`] — the root [`MusicApp`] widget that draws the whole UI.
//!
//! Live launches use `app_root_with_args()`; hermetic screenshots use
//! `app_root_for_shot()` without loading a personal profile.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
pub mod audio;
pub mod backend;
pub mod doc;
mod files;
pub mod icon;
pub mod live_search;
pub mod local_file;
pub mod lyrics;
pub mod model;
pub mod mpris;
pub mod net;
pub mod resource;
pub mod search_type;
pub mod seed;
pub mod storage;
pub mod stream;
pub use app::{MusicApp, View};
pub use search_type::{MusicSearchProvider, SearchHit, SearchableType};

use ui::widget::Widget;

/// The searchable result types Music contributes to Tilo's global search. An OS
/// search host registers these descriptors (id, display name, whether hits
/// carry a cover) once, then drives one of the providers per query. Exposing
/// this list keeps the host's integration a one-liner
/// (`for t in music::searchable_types() { registry.register(t) }`) with no
/// reach into the app internals — the same seam `apps/tube` pins for videos.
pub fn searchable_types() -> Vec<SearchableType> {
    search_type::all_searchable_types()
}

/// Build and return the app's root widget tree.
///
/// This is the live-binary entry (not `--shots`/tests), so it also brings up
/// the MPRIS D-Bus service (media keys + the desktop media controls). MPRIS
/// degrades to a no-op stub on unsupported hosts, without the `audio` feature,
/// or without a session bus, so
/// this never fails.
pub fn app_root() -> Box<dyn Widget> {
    let mut app = MusicApp::new();
    app.enable_mpris();
    Box::new(app)
}

/// The app as every host mounts it: the live player and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.music", app_root)
        .component_actions(actions::register)
        .as_definition("Music")
}

/// Build the live root from a validated app-owned resource argument. The
/// shell adds transport state only for an authenticated media handoff; an
/// ordinary `music://` launch opens the track paused at the beginning.
///
/// # Errors
///
/// Returns an error for unsupported resource URLs, unreadable local files,
/// unavailable audio support, or failure to persist the private playback profile.
/// Positional local paths and file URLs open for playback; decoder errors arrive
/// asynchronously through the normal playback status.
pub fn app_root_with_args(
    args: &ui::launch::LaunchArgs,
) -> Result<Box<dyn Widget>, ui::launch::LaunchError> {
    Ok(Box::new(app_with_args(args)?))
}

pub(crate) fn app_with_args(
    args: &ui::launch::LaunchArgs,
) -> Result<MusicApp, ui::launch::LaunchError> {
    let position_ms = std::env::var("TILO_MEDIA_HANDOFF_POSITION_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let resume = std::env::var("TILO_MEDIA_HANDOFF_RESUME_PLAYING").is_ok_and(|value| value == "1");
    app_for_launch_source(
        args.positionals().first().map(String::as_str),
        position_ms,
        resume,
    )
}

pub(crate) fn app_for_launch_source(
    source: Option<&str>,
    position_ms: u64,
    resume: bool,
) -> Result<MusicApp, ui::launch::LaunchError> {
    let mut app = MusicApp::new();
    app.enable_mpris();
    if let Some(source) = source {
        if source.starts_with("music://track/")
            && crate::search_type::DeepLink::parse(source).is_some()
        {
            app = app.with_media_handoff(source.to_owned(), position_ms, resume);
        } else {
            app.open_local_file(source)
                .map_err(ui::launch::LaunchError::Operation)?;
        }
    }
    Ok(app)
}

/// Like [`app_root`] but staged for a named `--shots` scenario. `--shots`
/// renders a single offscreen frame with no event loop, so a state that would
/// normally need live input (an opened overlay sidebar, an open context menu)
/// is pre-snapped here.
///
/// Recognised scenarios: `sidebar`/`collapsed` — nav sidebar open/collapsed;
/// `context` — track context menu open; `new-playlist` — the create-playlist
/// prompt open; `empty` — a genuinely empty library's Home state; `skeleton`
/// — Home in its loading state; `nowplaying` — a bundled track playing;
/// `player-menu` — the same on a narrow window with the transport's More
/// menu open;
/// `queue` — the Queue view with sample tracks trailing it; `lyrics` —
/// synced-lyrics view; `downloading`, `offline-cached`, and `offline-error` —
/// active, successful, and retryable device-local download states;
/// `artist-page`/`album-page`/`playlist-page` — the detail page for a bundled
/// item; `error` — a failed live detail request with its honest recovery
/// state; `membership-open` — a playlist-membership search result after its
/// safe canonical Open action is activated; `songs`, `albums`, `artists`,
/// `playlists`, `liked` and `browse` — that destination.
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    let mut app = MusicApp::new_for_shot();
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
        "skeleton" => app.snap_skeleton(),
        "nowplaying" => app.snap_nowplaying(),
        "player-menu" => app.snap_player_menu(),
        "queue" => app.snap_queue(),
        "lyrics" => app.snap_lyrics(),
        "downloading" => app.snap_downloading(),
        "offline-cached" => app.snap_offline_cached(),
        "offline-error" => app.snap_offline_error(),
        "artist-page" => app.snap_artist_page(),
        "album-page" => app.snap_album_page(),
        "error" => app.snap_error_page(),
        "membership-open" => app.snap_membership_open(),
        "playlist-page" => app.snap_view(View::Playlist("pl_focus".into())),
        "songs" => app.snap_view(View::Songs),
        "albums" => app.snap_view(View::Albums),
        "artists" => app.snap_view(View::Artists),
        "playlists" => app.snap_view(View::Playlists),
        "liked" => app.snap_view(View::Liked),
        "browse" => app.snap_view(View::Browse),
        _ => {}
    }
    Box::new(app)
}

/// Build a Music app for a **live** screenshot: networking stays enabled so the
/// warm-up loop in the binary can fetch real Home/artist/album content + real
/// cover art before capturing the frame. `TILO_MUSIC_VIEW` picks the screen.
pub fn live_shot_app() -> MusicApp {
    MusicApp::new()
}
pub use resource::{Music, MusicShell, launch_spec, standalone_launch};

#[cfg(test)]
mod sync;
