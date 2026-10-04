//! Root widget for the Music app.
//!
//! The whole UI is drawn directly into the Vello scene (nav rail, library
//! grids, track lists, detail pages, search and the now-playing transport
//! bar), so the app owns its layout math and hit-testing rather than
//! composing stock taffy widgets. User-authored library state lives in the
//! Automerge document via [`crate::doc`]; `sync` refreshes that render
//! snapshot each frame. Playback, downloads and other device effects stay
//! local so one device can never start audio or consume another device's disk.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::Receiver;
use ui::time::{Duration, Instant};

use automerge::{AutoCommit, ChangeHash, ObjId};
use base64::Engine as _;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use std::cell::Cell;
use std::rc::Rc;
use ui::ImageStore;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
#[cfg(test)]
use ui::widget::SplitLayout;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::ToastKind;
use ui::widget::{ModalDialog, PromptBody, SidebarDrawer, Widget};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::audio::AudioEngine;
use crate::backend::{
    AlbumPage, ArtistHit, ArtistPage, HomeContent, HomeItem, HomeItemKind, HomeSection,
    PlaylistPage, SearchBundle, SearchTop, TrackHit,
};
use crate::doc::{self, LiveTrack};
use crate::icon;
use crate::lyrics::{self, LyricsLine, LyricsWorker, TrackMeta};
use crate::model::{Album, Artist, CacheState, Library, Playback, RepeatMode, Track};
use crate::mpris::{MprisCommand, MprisHandle, PlaybackStatus, PlayerSnapshot};
use crate::seed;
use crate::stream::{StreamRequest, StreamUpdate, StreamWorker};

/// A host-visible stream-start operation must eventually settle even when a
/// resolver, staging download, or audio backend wedges. This bounds the
/// non-cancellable lifecycle contract without turning a slow normal seek into
/// a fabricated success.
const STREAM_START_TIMEOUT: Duration = Duration::from_secs(30);

/// What a failed offline download says in passing. The reason stays with the
/// song: its row in Downloads offers Retry, and that button names the cause.
const OFFLINE_FAILED: &str = "Couldn\u{2019}t download for offline listening";

/// A transient message over the content: what it says, what kind of result
/// it reports, and when it appeared.
struct Notice {
    text: String,
    kind: ToastKind,
    at: Instant,
}

fn cover_request() -> PickRequest {
    PickRequest::open("Choose a playlist cover", "Set cover")
        .thread_name("music-portal-cover")
        .filter(
            FileFilter::new("Images")
                .mimetype("image/png")
                .mimetype("image/jpeg")
                .mimetype("image/webp")
                .glob("*.png")
                .glob("*.jpg")
                .glob("*.jpeg")
                .glob("*.webp"),
        )
        .all_files()
}

/// Which screen is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Home,
    Songs,
    Albums,
    Artists,
    Playlists,
    Downloads,
    Liked,
    Browse,
    Queue,
    Lyrics,
    Album(String),
    Artist(String),
    Playlist(String),
}

/// A click action captured during draw and dispatched on the next mouse
/// press. Owned data only, since the hit list is rebuilt every frame.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Nav(View),
    Back,
    /// Open the platform search for this app, prefilled with a genre: the
    /// Browse tiles route here.
    BrowseGenre(String),
    /// Play `queue[index]` after replacing the queue.
    Play {
        /// Shared across every visible row hit so drawing a long list does not
        /// clone the full queue once per row.
        queue: Arc<Vec<String>>,
        index: usize,
    },
    /// Turn shuffle on and play the queue from a random entry.
    ShufflePlay(Arc<Vec<String>>),
    /// Open the platform search for Music.
    OpenSearch,
    TogglePlay,
    Next,
    Prev,
    ToggleShuffle,
    CycleRepeat,
    /// Repeat in this mode, or stop repeating when it already does.
    SetRepeat(RepeatMode),
    /// Open the narrow transport's More menu, anchored at this button: the
    /// playback controls the compact bar has no room for.
    OpenPlayerMenu(Rect),
    SeekFrac(f64),
    VolumeFrac(f64),
    /// Toggle mute: drop the volume to 0, or restore the pre-mute level.
    ToggleMute,
    /// Open / close the volume popover (shown when the inline slider collapses
    /// to a button on a narrower bar).
    ToggleVolumePopover,
    /// Open the native file portal to pick a cover image for this playlist.
    PickPlaylistCover(String),
    /// Append a track to the end of the play queue.
    AddToQueue(String),
    /// Insert a track right after the current track ("Play next").
    PlayNext(String),
    /// Remove the queue entry at this absolute index.
    RemoveFromQueue(usize),
    /// Move the queue entry at `from` to `to` (reorder).
    MoveQueueItem {
        from: usize,
        to: usize,
    },
    /// Toggle a track's "Liked Songs" membership.
    ToggleLike(String),
    /// Toggle an album's saved-to-library state.
    ToggleSaveAlbum(String),
    SetOffline {
        track: String,
        keep: bool,
    },
    /// Toggle whether the user follows an artist.
    ToggleFollow(String),
    /// Append a track to a named playlist (from the context menu).
    AddToPlaylist {
        playlist: String,
        track: String,
    },
    /// Remove a track from a playlist (playlist detail row affordance).
    RemoveFromPlaylist {
        playlist: String,
        track: String,
    },
    /// Reorder a track within a playlist.
    MovePlaylistItem {
        playlist: String,
        from: usize,
        to: usize,
    },
    /// Create a new (empty) playlist with the pending name and open it.
    CreatePlaylist,
    /// Begin editing a new-playlist name (opens the name input overlay).
    BeginNewPlaylist,
    /// Begin a new playlist that the given track will be added to on create
    /// (the "Add to playlist… → New playlist…" flow).
    BeginNewPlaylistFor(String),
    /// Begin renaming the playlist with this id.
    BeginRenamePlaylist(String),
    /// Commit the pending rename of the named playlist.
    CommitRename(String),
    /// Delete a playlist by id.
    DeletePlaylist(String),
    /// Open the track context menu at the cursor for this track, captured in
    /// the named originating list (so "remove from playlist" can target it).
    OpenTrackMenu {
        track: String,
        in_playlist: Option<String>,
        queue_index: Option<usize>,
    },
    /// Open the "Add to playlist…" picker for the menu's target track.
    OpenAddToPlaylist(String),
    /// Dismiss the inline name-input overlay without committing.
    CloseNamePrompt,
    /// Toggle the queue view from the now-playing bar: open it (remembering the
    /// current view), or, when already on it, hide it and return to where the
    /// user was before.
    ToggleQueue,
    /// Show or hide the adaptive navigation sidebar.
    ToggleSidebar,
    /// Dismiss the overlay sidebar (scrim tap / Esc).
    CloseSidebar,
    /// A no-op hit used to swallow clicks inside a modal surface so they don't
    /// fall through to the scrim behind it.
    None,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    Scrubber,
    Volume,
}

#[derive(Clone)]
struct Hit {
    rect: Rect,
    action: Action,
}

#[derive(Clone, Debug)]
struct PendingMediaHandoff {
    uri: String,
    position_ms: u64,
    resume_playing: bool,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool tracks an independent, unrelated concern (backend readiness, one-time \
              seeding, playback init, the volume popover, the redraw flag) scattered across a \
              large app-state struct; grouping them into a sub-struct would only rename the \
              ~50 call sites across app.rs and render.rs, not reduce them"
)]
pub struct MusicApp {
    lib: Library,
    /// The document heads that produced `lib`. A sync with unchanged heads
    /// reuses the projection and avoids even decoding the library document.
    library_heads: Option<Vec<ChangeHash>>,
    /// Shared, preindexed Ctrl+K catalog. Cloning the provider is O(1), so
    /// host/provider retrieval never walks the whole music corpus.
    library_search_provider: crate::search_type::LibrarySearchProvider,
    /// Monotonic diagnostic used by tests to prove provider retrieval does not
    /// trigger a catalog rebuild.
    library_search_builds: u64,
    /// Terminal download/cache choices belong to this device.  They are
    /// overlaid onto the shared library projection after every reload, never
    /// serialized into Automerge or offered as document actions.
    device_cache: BTreeMap<String, CacheState>,
    /// Content-addressed media objects + manifest, strictly device-local.
    media_cache: crate::storage::MediaCache,
    media_cache_root: PathBuf,
    /// Tests and hermetic screenshot fixtures never read or write a person's
    /// actual local profile. The live app enables it.
    persist_device_profile: bool,
    pb: Playback,
    view: View,
    nav_back: Vec<View>,
    /// The query the live catalogue is answering. Search belongs to the
    /// platform: the app draws no field, it only runs the remote query the
    /// platform's search provider asks for.
    search_query: String,
    /// The exchange with the platform search's live-catalogue provider.
    live_search: crate::live_search::LiveSearch,
    /// The header's two actions, real retained buttons: open an audio file,
    /// and open the platform search for this app.
    open_file_button: ui::widget::Button,
    search_button: ui::widget::Button,
    /// Search opened from the app's own painted controls (a Browse tile).
    search_opener: ui::search::SearchOpener,
    audio: AudioEngine,

    /// The non-blocking streaming worker (real YouTube-Music search + audio
    /// resolution). Spawned lazily on the first Play / search — never during
    /// `--shots` — so the screenshot path stays hermetic. `None` until then.
    worker: Option<StreamWorker>,
    /// Which data source the worker bound to ("offline (bundled library)" or
    /// "youtube-music (live)"), for the now-playing status line.
    backend_name: String,
    /// Whether the bound backend can actually stream from `YouTube`.
    backend_live: bool,
    /// Real search results merged in-memory (playable from the platform
    /// search, and re-injected into the library snapshot each frame so they
    /// stay playable across a `sync` that reloads the catalog from the
    /// document).
    live_hits: Vec<LiveTrack>,
    /// A live search query awaiting results (drives animation frames + a
    /// "searching…" affordance). `None` when idle.
    live_search_pending: Option<String>,
    /// The most recent live search's `(query, ordered track ids)`, in the
    /// catalogue's ranking order.
    live_results: Option<(String, Vec<String>)>,
    /// The full multi-type search response for the current query: songs,
    /// albums, artists and playlists. The platform search's live-catalogue
    /// rows are made from it.
    live_bundle: Option<(String, SearchBundle)>,
    /// Count of in-flight stream resolutions (keeps redraws flowing until the
    /// resolved URL lands and can be handed to the audio engine).
    resolve_in_flight: usize,
    /// Monotonic identity for worker-bound requests. Worker replies are allowed
    /// to arrive after newer user intent; each response must prove it still
    /// belongs to the latest request before it can change UI or audio state.
    next_request_id: u64,
    active_resolve_request: Option<u64>,
    /// The lifecycle invocation waiting for one exact stream-resolution id.
    /// This never represents ordinary in-window playback actions.
    resource_stream_invocation: Option<(tilo_actions::InvocationId, u64, u64, Instant)>,
    resource_stream_resume: Option<String>,
    /// Terminal updates are retained per invocation until the host consumes
    /// them. A new operation must never overwrite an earlier terminal result
    /// just because the host polls on a later frame.
    resource_stream_results: Vec<tilo_actions::InvocationUpdate>,
    active_search_request: Option<u64>,
    /// `(query, reason)` for the current live-search failure. Kept separate
    /// from a toast so it can be rendered with a retry control and announced
    /// through the app's accessibility tree.
    search_error: Option<(String, String)>,
    /// Most recent user-visible source failure. Unlike a transient log note,
    /// this is rendered and exposed through accessibility until another source
    /// is requested or succeeds.
    playback_error: Option<String>,

    /// MPRIS media-control service (media keys + the desktop media controls).
    /// A no-op stub until [`Self::enable_mpris`] brings it up on the live
    /// binary; inert in `--shots`/tests and without the `audio` feature.
    mpris: MprisHandle,
    /// App-native playback identity advertised privately to the shell. Music
    /// hands over a stable track resource, never its signed decoder URL.
    handoff: media::handoff::AppPlaybackHandle,
    pending_media_handoff: Option<PendingMediaHandoff>,
    local_file: Option<crate::local_file::LocalFile>,
    local_file_pick: Option<(tilo_actions::InvocationId, PortalChannel)>,

    /// A playlist-cover file-portal pick in flight: the receiver the frame loop
    /// drains, plus the playlist it targets. `None` when no picker is open.
    cover_pick: Option<PortalChannel>,
    /// Which playlist an in-flight cover pick belongs to. The portal only ever
    /// answers "which file", so the app remembers "for what".
    cover_pick_for: Option<String>,
    /// The off-thread portable PNG conversion started after a portal selection.
    /// Image decoding/resize/encoding never runs in the frame loop.
    cover_embed: Option<Receiver<Result<String, String>>>,
    cover_embed_for: Option<String>,

    seeded: bool,
    /// When `true`, a first `sync` onto an empty document writes the bundled
    /// sample catalog instead of an empty library. Only the `--shots` renderer
    /// and tests set this (see [`MusicApp::seed_sample_library`]); the live app
    /// starts with an empty library and fills it from real playback + search.
    seed_samples: bool,
    /// Whether the initial local playback state has been initialized. Playback
    /// is intentionally never adopted from, or written to, the shared doc.
    playback_loaded: bool,
    /// Track id the playhead is currently bound to (detects track changes
    /// across a doc reload so the live playhead isn't reset spuriously).
    bound_track: Option<String>,
    playhead_ms: f64,
    last_tick: Instant,

    /// Shuffle playback order: a permutation of `pb.queue` indices consulted by
    /// `advance`/`previous` while `pb.shuffle` is on (empty / stale ⇒ rebuilt
    /// on demand). Kept in-memory only — the shuffle *flag* round-trips through
    /// the CRDT, the concrete order is re-derived per session.
    shuffle_order: Vec<usize>,
    /// State for the small xorshift PRNG that builds the shuffle permutation.
    /// Seeded from wall-clock at construction; tests set it for determinism.
    shuffle_rng: u64,
    /// Volume level captured before muting, restored when unmuted.
    premute_volume: Option<u8>,
    /// The track to drop into a brand-new playlist once its name is committed
    /// (set by "Add to playlist… → New playlist…"); `None` for a plain create.
    new_playlist_track: Option<String>,

    rect: Rect,
    body_rect: Rect,
    scroll: f64,
    content_height: f64,

    /// Adaptive navigation sidebar: the shared [`ui::widget::sidebar::SidebarDrawer`]
    /// primitive. It owns the docked-vs-overlay decision, the eased reveal, the
    /// dimming scrim and the user-toggle memory, and hands back the resolved
    /// sidebar/content rects — so collapse clears cleanly (its own clip) and the
    /// panel never resizes against the content.
    drawer: SidebarDrawer,
    /// Hit rect of the sidebar toggle, captured during draw so the event
    /// handler can route a press to [`SidebarDrawer::toggle`].
    toggle_rect: Rect,
    /// Height of the now-playing bar in the last painted theme, so the event
    /// path hit-tests the bar that was drawn.
    bar_h: f64,

    mouse: Point,
    hits: Vec<Hit>,
    /// Per-row right-click targets, captured during draw and matched on a
    /// right mouse press (kept separate from `hits` so a left-click plays the
    /// row while a right-click opens its context menu).
    row_menu_hits: Vec<Hit>,
    drag: Option<Drag>,
    /// A horizontal swipe in progress on the now-playing bar: `(start point,
    /// tap action under the press)`. On release a large horizontal delta skips
    /// track (swipe left → next, right → previous); a small delta falls back to
    /// the captured tap. Enables touch-style prev/next on the bottom bar.
    bar_press: Option<(Point, Option<Action>)>,
    /// Interactive bar rects in the now-playing transport, captured during
    /// draw so the event handler can map drags to fractions.
    scrubber_rect: Rect,
    volume_rect: Rect,
    /// The volume button's hit rect (shown when the inline slider is collapsed)
    /// and the open state + slider rect of its popover.
    volume_btn_rect: Rect,
    volume_popover_open: bool,
    volume_popover_rect: Rect,
    volume_popover_slider: Rect,
    /// The now-playing bar's queue-button rect, captured during draw. Hovering
    /// it opens the queue preview overlay; clicking it toggles the queue view.
    queue_btn_rect: Rect,
    /// The view (+ scroll offset) to return to when the queue view is toggled
    /// back off, so "Queue" restores exactly where the user was rather than
    /// resetting. `None` when the queue view wasn't opened via the toggle.
    queue_return: Option<(View, f64)>,
    dirty: bool,
    toast: Option<Notice>,

    /// Self-routing right-click context menu (the `ui` primitive, driven
    /// imperatively: opened at the cursor, drawn last, polled via a sink).
    context: ContextMenu,
    /// The scale from the most recently rendered system theme. Context menus
    /// are rebuilt on demand, so retaining this lets their first pointer or
    /// keyboard event use the same enlarged geometry the next frame paints.
    context_text_scale: f32,
    /// The action each open menu row maps to. Parallel to `context.items()`;
    /// drained when a selection lands so the host performs the CRDT mutation
    /// the `Arc<Fn>` callback model can't express.
    menu_actions: Vec<Action>,
    /// Shared sink the menu callbacks write the chosen row index into; the
    /// host drains it after `click_item` and dispatches into the normal
    /// `Action` pipeline. `-1` means "no pending selection". `Arc<AtomicI64>`
    /// (not `Rc<Cell>`) because `ContextMenu`'s `MenuAction` callback is
    /// `Send + Sync`.
    pending: Arc<AtomicI64>,

    /// The row a context menu was opened on: it stays lifted while the menu
    /// is open.
    menu_anchor_row: Option<Rect>,
    /// The track the open context menu targets (for go-to-album/artist etc.).
    ctx_track: Option<String>,
    /// The playlist the context-menu target row belongs to, if it was opened
    /// from a playlist detail (so "Remove from playlist" can target it).
    ctx_in_playlist: Option<String>,
    /// The absolute queue index of the context-menu target, if opened from the
    /// queue view (so "Remove from queue" can target it).
    ctx_queue_index: Option<usize>,
    /// Membership selected from a Ctrl+K result. It is retained until the
    /// playlist row is visible, so the view can scroll and highlight the exact
    /// still-current row rather than merely opening the playlist at its top.
    revealed_membership: Option<(String, String)>,

    /// A name-input overlay (create / rename playlist). When `Some`, a shared
    /// [`ModalDialog`] hosting a real [`TextInput`] is up; `name_input` carries
    /// the committed value into the `CreatePlaylist` / `CommitRename` handlers.
    name_prompt: Option<NamePrompt>,
    name_input: String,
    /// The live name-prompt dialog, built lazily from `name_prompt` +
    /// `name_input` and torn down when it resolves.
    name_dialog: Option<NameDlg>,

    /// Cached lyrics for the current track + the id they were fetched for, so
    /// the lyrics view doesn't re-resolve every frame.
    lyrics: Vec<LyricsLine>,
    lyrics_for: Option<String>,
    /// Whether the current `lyrics` are synced (time-stamped).
    lyrics_synced: bool,

    /// A deferred `--shots` staging action, applied once on the first `sync`
    /// after the library has loaded (the catalog is empty at construction).
    shot_stage: Option<ShotStage>,
    /// A staged track whose context menu opens over its row once the row is
    /// laid out.
    staged_menu_track: Option<String>,
    /// Open the narrow transport's More menu once the bar is laid out.
    staged_player_menu: bool,
    /// A staged sidebar state, snapped once the drawer knows the window's
    /// size class (docked or floating each remember their own state).
    staged_sidebar: Option<bool>,

    // ---- real artwork + live content -------------------------------------
    /// Async remote-image cache: real album/artist/playlist covers fetched off
    /// the UI thread, decoded to GPU textures and painted. See [`ui::ImageStore`].
    images: ImageStore,
    /// Entity id (album / artist / playlist / track) → best cover URL, folded
    /// in from live search/home/artist results and re-applied each frame.
    art_urls: HashMap<String, String>,
    /// The real YouTube-Music Home page (charts / new releases / top artists),
    /// once fetched. `None` offline / before it lands → the sample home shows.
    home: Option<HomeContent>,
    home_requested: bool,
    /// Set once a Home fetch has come back empty-handed (offline build, network
    /// error). Stops the loading skeleton — and the animation loop — so the
    /// empty Home settles instead of shimmering forever.
    home_failed: bool,
    /// Detail pages (artist / album / playlist ids) whose live fetch failed, so
    /// their skeleton stops and an honest "couldn't load" state shows instead.
    failed_pages: HashSet<String>,
    /// Real artist pages keyed by channel id, and the set already requested.
    artist_pages: HashMap<String, ArtistPage>,
    artist_requested: HashSet<String>,
    /// Real album pages keyed by album id, and the set already requested.
    album_pages: HashMap<String, AlbumPage>,
    album_requested: HashSet<String>,
    /// Real (live YouTube-Music) playlist pages keyed by playlist id, and the
    /// set already requested.
    playlist_pages: HashMap<String, PlaylistPage>,
    playlist_requested: HashSet<String>,
    /// Whether this instance may touch the network (spawn the worker, fetch
    /// covers). False for the hermetic `--shots`/test path so a headless render
    /// never opens a socket; the live-shots pre-warm flips it on.
    allow_network: bool,
    /// Test override for the metered-connection check (bypasses `NetworkManager`
    /// so prefetch gating is deterministic in tests). `None` ⇒ query the real
    /// connection via [`crate::net`].
    force_metered: Option<bool>,

    // ---- lyrics fetch ----------------------------------------------------
    /// Async lyrics worker (LRCLIB). Spawned lazily; results fold into `lyrics`.
    lyrics_worker: Option<LyricsWorker>,
    /// The track id a lyrics fetch is in flight for (dedupes requests).
    lyrics_pending_for: Option<String>,
    /// Resolved lyrics cached by track id (`lines`, `synced`), so revisiting a
    /// track — or prefetching the next queued one on a stable connection — is
    /// instant. Populated for every resolved track, not just the current one.
    lyrics_cache: HashMap<String, (Vec<LyricsLine>, bool)>,
    /// The current track id we've already run a prefetch pass for (dedupe), so
    /// prefetch fires once per track change, not every frame.
    prefetched_for: Option<String>,

    // ---- search performance ----------------------------------------------
    /// Wall-clock of the last search keystroke, for debouncing the live query.
    search_edit_at: Option<Instant>,
    /// The last query actually dispatched to the live worker (dedupe debounce).
    live_query_sent: Option<String>,

    // ---- offline downloads -----------------------------------------------
    /// Reserved for a future byte-backed downloader. The current product does
    /// not simulate downloads: a progress bar without durable playable bytes
    /// would be a false offline-completion claim.
    downloads: HashMap<String, crate::audio::OfflineDownload>,
    offline_resolves: HashMap<u64, String>,
    offline_invocation: Option<(
        tilo_actions::InvocationId,
        std::collections::BTreeSet<String>,
    )>,
    offline_results: Vec<tilo_actions::InvocationUpdate>,
    offline_errors: HashMap<String, String>,

    /// Framework-owned action dispatch: brackets the undo boundary (the same undo
    /// path the Ctrl+K palette takes), encodes and dispatches each on-screen
    /// affordance's [`crate::actions`] action, and remembers the last one so a
    /// test can prove a button and the Ctrl+K
    /// [`ActionSpec`](tilo_actions::ActionSpec) build the byte-identical action.
    actions: crate::resource::ActionSink,
    // Drop after the audio engine/downloads so test-owned files are no longer open.
    #[cfg(test)]
    _test_cache_dir: tempfile::TempDir,
}

/// A deferred `--shots` staging action (see [`MusicApp::app_root_for_shot`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShotStage {
    TrackMenu,
    NewPlaylistPrompt,
    /// The Home page in its shimmering skeleton loading state.
    Skeleton,
    /// A bundled track playing, so the now-playing bar shows cover art + a live
    /// equalizer on the current row.
    NowPlaying,
    /// A deterministic, local-only download state for an offscreen capture.
    Downloading,
    /// A deterministic durable-cache success state for an offscreen capture.
    OfflineCached,
    /// A deterministic terminal download error that exposes the Retry affordance.
    OfflineError,
    /// The synced-lyrics view with a highlighted current line at ~40s.
    Lyrics,
    /// A Ctrl+K playlist-membership result activated through its canonical View
    /// operation, leaving the precise row revealed/highlighted.
    MembershipOpen,
}

/// What the inline name-input overlay is collecting.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NamePrompt {
    /// Creating a brand-new playlist.
    NewPlaylist,
    /// Renaming an existing playlist (by id).
    Rename(String),
}

/// The live name-prompt dialog: the shared [`ModalDialog`] hosting a real
/// [`PromptBody`]/[`TextInput`] plus the submit / cancel cells its footer
/// buttons flip (drained each event by the host).
struct NameDlg {
    dialog: ModalDialog<PromptBody>,
    submit: Rc<Cell<bool>>,
    cancel: Rc<Cell<bool>>,
}

impl MusicApp {
    // A flat struct literal initializing every field of `MusicApp`'s ~90-field
    // state; splitting it into sub-builders would only relocate the field
    // assignments across more functions, not reduce them.
    #[allow(
        clippy::too_many_lines,
        reason = "flat struct-literal initializing every MusicApp field once; splitting it would only relocate field assignments, not reduce them"
    )]
    pub fn new() -> Self {
        Self::new_with_device_profile(true)
    }

    /// Construct the hermetic screenshot/test app without even reading a
    /// user-owned device profile.  Calling `set_offline_for_shot` after `new`
    /// is too late: it prevents writes, but a read could already have changed
    /// the captured state.
    pub(crate) fn new_for_shot() -> Self {
        Self::new_with_device_profile(false)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "flat struct-literal initializing every MusicApp field once; splitting it would only relocate the field assignments, not reduce them"
    )]
    fn new_with_device_profile(load_device_profile: bool) -> Self {
        // Optional initial-view override, used by `--shots` to capture
        // screens other than Home (e.g. `TILO_MUSIC_VIEW=album:al_glasslight`).
        let (view, query) = Self::parse_initial_view();
        // A preset search query (`TILO_MUSIC_VIEW=search:…`) arms the debounce so
        // the live-shot loop dispatches it after the usual idle beat.
        let preset_search_at = (!query.is_empty()).then(ui::time::now);
        let persist_device_profile = load_device_profile && !cfg!(test);
        let device_profile = persist_device_profile
            .then(crate::storage::load_device_profile)
            .unwrap_or_default();
        let media_cache = persist_device_profile
            .then(crate::storage::load_media_cache)
            .unwrap_or_default();
        #[cfg(test)]
        let test_cache_dir = tempfile::tempdir().expect("test-owned Music cache");
        #[cfg(test)]
        let media_cache_root = test_cache_dir.path().to_path_buf();
        #[cfg(not(test))]
        let media_cache_root = crate::storage::media_cache_dir();
        let device_cache = media_cache
            .entries
            .keys()
            .map(|track_id| (track_id.clone(), CacheState::Cached))
            .collect();
        Self {
            lib: Library::default(),
            library_heads: None,
            library_search_provider: crate::search_type::LibrarySearchProvider::new(
                &Library::default(),
            ),
            library_search_builds: 0,
            device_cache,
            media_cache,
            media_cache_root,
            persist_device_profile,
            pb: device_profile.playback,
            view,
            nav_back: Vec::new(),
            search_query: query,
            live_search: {
                let live = crate::live_search::LiveSearch::default();
                live.set_reachable(true);
                live
            },
            open_file_button: Self::make_open_file_button(),
            search_button: ui::search::search_button(ui::search::SearchRequest::this_app()),
            search_opener: ui::search::SearchOpener::new(),
            audio: AudioEngine::new(),
            worker: None,
            backend_name: "offline (bundled library)".to_string(),
            backend_live: false,
            live_hits: Vec::new(),
            live_search_pending: None,
            live_results: None,
            live_bundle: None,
            resolve_in_flight: 0,
            next_request_id: 0,
            active_resolve_request: None,
            resource_stream_invocation: None,
            resource_stream_resume: None,
            resource_stream_results: Vec::new(),
            active_search_request: None,
            search_error: None,
            playback_error: None,
            mpris: MprisHandle::disabled(),
            handoff: media::handoff::AppPlaybackHandle::new(),
            pending_media_handoff: None,
            local_file: device_profile.local_file,
            local_file_pick: None,
            cover_pick: None,
            cover_pick_for: None,
            cover_embed: None,
            cover_embed_for: None,
            seeded: false,
            seed_samples: false,
            playback_loaded: false,
            bound_track: None,
            playhead_ms: 0.0,
            last_tick: ui::time::now(),
            shuffle_order: Vec::new(),
            shuffle_rng: seed_rng(),
            premute_volume: None,
            new_playlist_track: None,
            rect: Rect::ZERO,
            body_rect: Rect::ZERO,
            scroll: 0.0,
            content_height: 0.0,
            drawer: SidebarDrawer::new().left(),
            toggle_rect: Rect::ZERO,
            bar_h: 0.0,
            mouse: Point::ZERO,
            hits: Vec::new(),
            row_menu_hits: Vec::new(),
            drag: None,
            bar_press: None,
            scrubber_rect: Rect::ZERO,
            volume_rect: Rect::ZERO,
            volume_btn_rect: Rect::ZERO,
            volume_popover_open: false,
            volume_popover_rect: Rect::ZERO,
            volume_popover_slider: Rect::ZERO,
            queue_btn_rect: Rect::ZERO,
            queue_return: None,
            dirty: true,
            toast: None,
            context: ContextMenu::new(),
            context_text_scale: 1.0,
            menu_actions: Vec::new(),
            pending: Arc::new(AtomicI64::new(-1)),
            menu_anchor_row: None,
            ctx_track: None,
            ctx_in_playlist: None,
            ctx_queue_index: None,
            revealed_membership: None,
            name_prompt: None,
            name_input: String::new(),
            name_dialog: None,
            lyrics: Vec::new(),
            lyrics_for: None,
            lyrics_synced: false,
            shot_stage: None,
            staged_menu_track: None,
            staged_player_menu: false,
            staged_sidebar: None,
            images: ImageStore::new(&image_fetcher()),
            art_urls: HashMap::new(),
            home: None,
            home_requested: false,
            home_failed: false,
            failed_pages: HashSet::new(),
            artist_pages: HashMap::new(),
            artist_requested: HashSet::new(),
            album_pages: HashMap::new(),
            album_requested: HashSet::new(),
            playlist_pages: HashMap::new(),
            playlist_requested: HashSet::new(),
            allow_network: true,
            force_metered: None,
            lyrics_worker: None,
            lyrics_pending_for: None,
            lyrics_cache: HashMap::new(),
            prefetched_for: None,
            search_edit_at: preset_search_at,
            live_query_sent: None,
            downloads: HashMap::new(),
            offline_resolves: HashMap::new(),
            offline_invocation: None,
            offline_results: Vec::new(),
            offline_errors: HashMap::new(),
            actions: crate::resource::ActionSink::default(),
            #[cfg(test)]
            _test_cache_dir: test_cache_dir,
        }
    }

    /// Disable all network use for a hermetic headless render (`--shots` /
    /// tests): no worker thread, no cover fetches. The offline sample + gradient
    /// placeholders render deterministically.
    pub fn set_offline_for_shot(&mut self) {
        self.allow_network = false;
        self.live_search.set_reachable(false);
        self.persist_device_profile = false;
    }

    /// Seed the bundled sample catalog on the first `sync` instead of starting
    /// with an empty library. Only the `--shots` renderer and tests call this —
    /// the live app never fabricates a library.
    pub fn seed_sample_library(&mut self) {
        self.seed_samples = true;
    }

    /// Whether every live fetch this frame needs has settled — used by the
    /// live-shots pre-warm to know when the real covers + content are in.
    pub fn is_settled_for_shot(&self) -> bool {
        self.images.is_idle()
            && self.live_search_pending.is_none()
            && (self.home.is_some() || !matches!(self.view, View::Home))
    }

    /// Whether the async image cache has no fetch/decode in flight.
    pub fn images_idle(&self) -> bool {
        self.images.is_idle()
    }

    /// Whether at least one cover URL has been discovered (so the pre-warm knows
    /// real artwork exists to wait for).
    pub fn any_art_requested(&self) -> bool {
        !self.art_urls.is_empty()
    }

    /// Whether the browse content for the current view has loaded (real Home /
    /// artist / album data), so a live shot doesn't capture a loading state.
    pub fn is_content_loaded_for_shot(&self) -> bool {
        match &self.view {
            View::Home => self.home.is_some(),
            View::Artist(id) => self.artist_pages.contains_key(id) || !is_live_id(id),
            View::Album(id) => self.album_pages.contains_key(id) || !is_live_album_id(id),
            View::Playlist(id) => self.playlist_pages.contains_key(id) || !is_live_playlist_id(id),
            View::Browse => self.live_bundle.is_some(),
            _ => true,
        }
    }

    /// Kick off a real search (keeping the current view), for staging a
    /// lyrics/now-playing live shot. Idempotent per query.
    pub fn shot_begin_search(&mut self, query: &str) {
        query.clone_into(&mut self.search_query);
        self.request_live_search();
    }

    /// Whether a live search has returned results.
    pub fn shot_has_results(&self) -> bool {
        self.live_results
            .as_ref()
            .is_some_and(|(_, ids)| !ids.is_empty())
    }

    /// Whether lyrics for the current track have settled (loaded or confirmed
    /// absent), for the live lyrics shot.
    pub fn shot_lyrics_ready(&self) -> bool {
        !self.lyrics.is_empty() || (self.lyrics_for.is_some() && self.lyrics_pending_for.is_none())
    }

    /// After results land, play the top live hit — for a live lyrics/now-playing
    /// shot. No-op if there are no results yet.
    pub fn shot_play_top_result(&mut self, state: &mut AutoCommit) -> bool {
        let Some((_, ids)) = self.live_results.clone() else {
            return false;
        };
        let Some(first) = ids.first().cloned() else {
            return false;
        };
        self.inject_live_hits();
        self.dispatch(
            Action::Play {
                queue: vec![first].into(),
                index: 0,
            },
            state,
        );
        true
    }

    /// Open a global-search hit's deep link onto the real navigation / play
    /// path. This is the app side of the [`crate::search_type`] seam: the OS
    /// search host round-trips `music://{track,album,artist,playlist}/<id>`
    /// back to Music, and this routes it exactly like an in-app click —
    /// playing the track, or navigating to the album / artist / playlist page
    /// (the view's own `sync` then loads a live page on demand). Returns
    /// whether the link was recognised and routed.
    pub fn open_deep_link(&mut self, link: &str, state: &mut AutoCommit) -> bool {
        let Some(dl) = crate::search_type::DeepLink::parse(link) else {
            return false;
        };
        match dl {
            crate::search_type::DeepLink::Track(id) => {
                self.dispatch(
                    Action::Play {
                        queue: vec![id].into(),
                        index: 0,
                    },
                    state,
                );
            }
            crate::search_type::DeepLink::Album(id) => {
                self.dispatch(Action::Nav(View::Album(id)), state);
            }
            crate::search_type::DeepLink::Artist(id) => {
                self.dispatch(Action::Nav(View::Artist(id)), state);
            }
            crate::search_type::DeepLink::Playlist(id) => {
                self.dispatch(Action::Nav(View::Playlist(id)), state);
            }
        }
        true
    }

    fn parse_initial_view() -> (View, String) {
        let Ok(raw) = std::env::var("TILO_MUSIC_VIEW") else {
            return (View::Home, String::new());
        };
        let v = match raw.as_str() {
            "home" => View::Home,
            "songs" => View::Songs,
            "albums" => View::Albums,
            "artists" => View::Artists,
            "playlists" => View::Playlists,
            "downloads" => View::Downloads,
            "liked" => View::Liked,
            "queue" => View::Queue,
            "lyrics" => View::Lyrics,
            "browse" => View::Browse,
            other => {
                if let Some(id) = other.strip_prefix("album:") {
                    View::Album(id.to_string())
                } else if let Some(id) = other.strip_prefix("artist:") {
                    View::Artist(id.to_string())
                } else if let Some(id) = other.strip_prefix("playlist:") {
                    View::Playlist(id.to_string())
                } else if let Some(q) = other.strip_prefix("search:") {
                    return (View::Browse, q.to_string());
                } else {
                    View::Home
                }
            }
        };
        (v, String::new())
    }

    /// Convert a published top-level destination id back to its concrete view.
    /// The resource layer only admits ids from `NAVIGATION_VIEWS`; this router
    /// keeps the final local navigation step equally explicit.
    fn top_level_view(id: &str) -> Option<View> {
        match id {
            "home" => Some(View::Home),
            "songs" => Some(View::Songs),
            "albums" => Some(View::Albums),
            "artists" => Some(View::Artists),
            "playlists" => Some(View::Playlists),
            "downloads" => Some(View::Downloads),
            "liked" => Some(View::Liked),
            "browse" => Some(View::Browse),
            "queue" => Some(View::Queue),
            "lyrics" => Some(View::Lyrics),
            _ => None,
        }
    }

    // ---- state plumbing ---------------------------------------------------

    fn ensure_seeded(&mut self, state: &mut AutoCommit) {
        if self.seeded {
            return;
        }
        if !doc::is_seeded(state) {
            if self.seed_samples {
                // `--shots` / tests: the bundled sample catalog so a headless
                // frame (and the deterministic tests) have real content.
                let lib = seed::sample_library();
                let pb = seed::sample_playback(&lib);
                doc::seed(state, &lib, &pb);
            } else {
                // Live default: an empty library. The document structure is
                // still initialised (so `is_seeded`, persistence and sync all
                // work) but carries no fabricated tracks/playlists — the real
                // library fills in from playback and search.
                // The shared canonical seed, so peers that open the same
                // unseeded record concurrently hold one set of containers.
                doc::seed_empty_shared(state);
            }
        }
        self.seeded = true;
    }

    /// Refresh just the catalog/playlists/cache from the document. Runs
    /// every `sync` so remote library edits (a playlist gaining a track on
    /// another device) appear without disturbing local playback.
    fn reload_library(&mut self, state: &mut AutoCommit) {
        let heads = state.get_heads();
        if self.library_heads.as_ref() == Some(&heads) {
            return;
        }
        let mut next_library = doc::load_library(state);
        // `Library` carries cache state only as a convenient render projection.
        // Its authoritative copy is local to this device, not the shared doc.
        next_library.cache = self.device_cache.clone();
        let searchable_catalog_changed = self.lib.tracks != next_library.tracks
            || self.lib.albums != next_library.albums
            || self.lib.artists != next_library.artists
            || self.lib.playlists != next_library.playlists;
        self.lib = next_library;
        self.library_heads = Some(heads);
        // Playback/cache/liked-state document updates need their render
        // projection refreshed, but they do not change a Ctrl+K row's title,
        // metadata, membership, or canonical target. Reindex only when those
        // searchable library entities actually changed.
        if searchable_catalog_changed {
            self.refresh_library_search_provider();
        }
        self.sync_playlist_covers();
    }

    /// Rebuild the owned catalog only after the library itself changed. The
    /// provider clones an Arc-backed [`ui::search::SearchCatalog`], so every
    /// later `search_providers()` call is O(1).
    fn refresh_library_search_provider(&mut self) {
        self.library_search_provider = crate::search_type::LibrarySearchProvider::new(&self.lib);
        self.library_search_builds = self.library_search_builds.wrapping_add(1);
    }

    /// Retire the old simulated-download seam. The render loop still invokes
    /// this, but it can never manufacture a false "cached" completion state.
    fn advance_downloads(&mut self, _state: &mut AutoCommit) {
        let mut complete = Vec::new();
        let mut progress = Vec::new();
        for (track_id, download) in &self.downloads {
            if let Some(fraction) = download.progress() {
                progress.push((track_id.clone(), fraction));
            }
            if let Some(result) = download.take_result() {
                complete.push((track_id.clone(), result));
            }
        }
        for (track_id, fraction) in progress {
            self.update_device_cache(&track_id, CacheState::Downloading(fraction));
        }
        for (track_id, result) in complete {
            self.downloads.remove(&track_id);
            match result {
                Ok(()) => {
                    self.media_cache =
                        crate::storage::load_media_cache_from(&self.media_cache_root);
                    if self
                        .media_cache
                        .object_path(&self.media_cache_root, &track_id)
                        .is_some()
                    {
                        self.update_device_cache(&track_id, CacheState::Cached);
                        self.settle_offline_track(&track_id, None);
                        self.notify(ToastKind::Success, "Saved for offline listening");
                    } else {
                        self.update_device_cache(&track_id, CacheState::Streamed);
                        self.settle_offline_track(
                            &track_id,
                            Some("download completed without a durable media object".into()),
                        );
                    }
                }
                Err(error) => {
                    self.update_device_cache(&track_id, CacheState::Streamed);
                    self.offline_errors.insert(track_id.clone(), error.clone());
                    self.settle_offline_track(&track_id, Some(error.clone()));
                    self.notify(ToastKind::Error, OFFLINE_FAILED);
                }
            }
        }
    }

    /// Mirror each playlist's persisted cover URL into the image cache so a
    /// cover set in a prior session (or by a peer) renders. Cheap: a handful of
    /// playlists, and the image store dedups fetches by URL.
    fn sync_playlist_covers(&mut self) {
        for pl in &self.lib.playlists {
            if !pl.cover.is_empty() {
                self.art_urls.insert(pl.id.clone(), pl.cover.clone());
            }
        }
    }

    /// Adopt the shared library snapshot. Playback intentionally starts from
    /// this device's local state rather than a peer's transport settings.
    fn reload(&mut self, state: &mut AutoCommit) {
        self.reload_library(state);
        self.bound_track = self.pb.current.clone();
        self.playhead_ms = self.pb.position_ms as f64;
    }

    fn persist_playback(&mut self, _state: &mut AutoCommit) {
        // Reducers such as a media-key play/pause can receive an already loaded
        // track before a queue has been materialised. Canonicalise that local
        // convenience state before it reaches the local audio boundary.
        if let Some(current) = self.pb.current.clone() {
            if let Some(index) = self.pb.queue.iter().position(|id| id == &current) {
                self.pb.queue_index = index;
            } else {
                self.pb.queue = vec![current];
                self.pb.queue_index = 0;
            }
            let duration = self.current_duration_ms();
            if duration > 0 {
                self.playhead_ms = self.playhead_ms.clamp(0.0, duration as f64);
            }
        } else {
            self.pb.playing = false;
            self.pb.queue_index = 0;
            self.playhead_ms = 0.0;
        }
        self.pb.position_ms = self.playhead_ms as u64;
        self.bound_track = self.pb.current.clone();
        self.report_device_profile_error();
    }

    fn persist_device_profile(&self) -> Result<(), String> {
        if !self.persist_device_profile {
            return Ok(());
        }
        crate::storage::save_device_profile(&crate::storage::DeviceProfile {
            // Do not persist a marker for media bytes we do not own. Playback
            // resume remains durable, device-local state.
            cache: BTreeMap::new(),
            local_file: self.local_file.clone(),
            playback: self.pb.clone(),
        })
        .map_err(|error| format!("could not save local Music state: {error}"))
    }

    fn report_device_profile_error(&mut self) {
        if let Err(error) = self.persist_device_profile() {
            self.playback_error = Some(error.clone());
            self.notify(ToastKind::Error, error);
        }
    }

    /// Show a transient message: a confirmation (`Success`), something under
    /// way (`Info`) or a failure (`Error`).
    fn notify(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toast = Some(Notice {
            text: text.into(),
            kind,
            at: ui::time::now(),
        });
    }

    fn update_device_cache(&mut self, id: &str, state: CacheState) {
        if matches!(state, CacheState::Streamed) {
            self.device_cache.remove(id);
        } else {
            self.device_cache.insert(id.to_string(), state);
        }
        self.lib.cache = self.device_cache.clone();
    }

    // ---- action dispatch --------------------------------------------------

    /// Dispatch one [`MusicAction`](crate::actions::MusicAction) — the single
    /// CRDT-mutation path shared with the Ctrl+K palette. The action is applied
    /// through the Tilo action executor (a pinned actor + committed change, so it
    /// replays byte-identically on peers), exactly as the palette does. The
    /// document heads captured just before the first action of the event are
    /// stashed so the host records **one** undo boundary bracketing it
    /// ([`CommittedActions::take_committed_action`](ui::undo::CommittedActions::take_committed_action)). Callers reload the
    /// in-memory library from the document afterward for immediate UI feedback;
    /// `sync` reconciles it again on the next frame (so a palette dispatch shows
    /// up the same way).
    fn perform_action(&mut self, state: &mut AutoCommit, action: &crate::actions::MusicAction) {
        self.actions.dispatch_with_context(
            state,
            crate::actions::MUSIC_ACTION_TYPE,
            action,
            crate::actions::history_context(action),
        );
    }

    /// Show the shared library, keeping this device's cache.
    /// An empty projection does not wipe a library that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::model::Library) {
        if state.tracks.is_empty()
            && state.albums.is_empty()
            && state.artists.is_empty()
            && state.playlists.is_empty()
            && (!self.lib.tracks.is_empty()
                || !self.lib.albums.is_empty()
                || !self.lib.artists.is_empty()
                || !self.lib.playlists.is_empty())
        {
            return;
        }
        let searchable = self.lib.tracks != state.tracks
            || self.lib.albums != state.albums
            || self.lib.artists != state.artists
            || self.lib.playlists != state.playlists;
        let cache = self.device_cache.clone();
        self.lib = state.clone();
        self.lib.cache = cache;
        if searchable {
            self.refresh_library_search_provider();
        }
        self.sync_playlist_covers();
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Music>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// The context Ctrl+K needs to build Music's resource tree: the user's
    /// playlists and their tracks, read from the in-memory library cache (itself
    /// re-projected from the document each `sync`). The virtual "Liked Songs"
    /// playlist is a projection of the like set, not a stored playlist, so it is
    /// not surfaced here — only real, mutable playlists are.
    pub(crate) fn music_resource_ctx(&self) -> crate::actions::MusicResourceCtx {
        let mut playlists = self.lib.playlists.iter().collect::<Vec<_>>();
        playlists.sort_by(|a, b| a.id.cmp(&b.id));
        let playlists = playlists
            .into_iter()
            .take(crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT)
            .map(|p| crate::actions::PlaylistResource {
                id: p.id.clone(),
                name: p.name.clone(),
                tracks: p
                    .track_ids
                    .iter()
                    .take(crate::actions::OPEN_PLAYLIST_TRACK_LIMIT)
                    .map(|tid| crate::actions::TrackRef {
                        id: tid.clone(),
                        label: self.track_title(tid),
                        artist: self.track_artist(tid),
                        liked: self.lib.is_liked(tid),
                        rank: p.track_ranks.get(tid).copied().unwrap_or_default(),
                    })
                    .collect(),
            })
            .collect();
        // HashMap iteration is deliberately unspecified. Sort before applying
        // the contextual page cap so the palette's initial resource set is
        // stable across process runs and peers.
        let mut tracks = self.lib.tracks.values().collect::<Vec<_>>();
        tracks.sort_by(|a, b| a.id.cmp(&b.id));
        let mut albums = self.lib.albums.values().collect::<Vec<_>>();
        albums.sort_by(|a, b| a.id.cmp(&b.id));
        let mut artists = self.lib.artists.values().collect::<Vec<_>>();
        artists.sort_by(|a, b| a.id.cmp(&b.id));
        crate::actions::MusicResourceCtx {
            playlists,
            tracks: tracks
                .into_iter()
                .take(crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT)
                .map(|track| crate::actions::CatalogTrackResource {
                    id: track.id.clone(),
                    label: self.track_title(&track.id),
                    artist: self.track_artist(&track.id),
                    album_id: track.album_id.clone(),
                    artist_id: track.artist_id.clone(),
                    offline: matches!(self.lib.cache_state(&track.id), CacheState::Cached),
                    liked: self.lib.is_liked(&track.id),
                })
                .collect(),
            albums: albums
                .into_iter()
                .take(crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT)
                .map(|album| crate::actions::CatalogAlbumResource {
                    id: album.id.clone(),
                    label: album.title.clone(),
                    saved: self.lib.is_album_saved(&album.id),
                })
                .collect(),
            artists: artists
                .into_iter()
                .take(crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT)
                .map(|artist| crate::actions::CatalogArtistResource {
                    id: artist.id.clone(),
                    label: artist.name.clone(),
                    followed: self.lib.is_artist_followed(&artist.id),
                })
                .collect(),
            membership: None,
            local_file: self.local_file_resource(),
            queue: self
                .pb
                .queue
                .iter()
                .take(crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT)
                .enumerate()
                .map(|(index, track)| crate::actions::QueueItemResource {
                    index,
                    track: track.clone(),
                    label: self.track_title(track),
                    artist: self.track_artist(track),
                    current: self.pb.queue_index == index
                        && self.pb.current.as_deref() == Some(track),
                })
                .collect(),
            playback: self.pb.clone(),
        }
    }

    fn membership_resource(
        &self,
        playlist_id: &str,
        track_id: &str,
    ) -> Option<crate::actions::PlaylistMembershipResource> {
        let playlist = self.lib.playlist(playlist_id)?;
        let index = playlist.track_ids.iter().position(|id| id == track_id)?;
        let track = self.lib.track(track_id)?;
        Some(crate::actions::PlaylistMembershipResource {
            playlist: playlist.id.clone(),
            track: crate::actions::TrackRef {
                id: track.id.clone(),
                label: self.track_title(&track.id),
                artist: self.track_artist(&track.id),
                liked: self.lib.is_liked(&track.id),
                rank: playlist
                    .track_ranks
                    .get(&track.id)
                    .copied()
                    .unwrap_or_default(),
            },
            index,
            total: playlist.track_ids.len(),
            up_rank: playlist
                .rank_for_move(&track.id, index.saturating_sub(1))
                .unwrap_or_default(),
            down_rank: playlist
                .rank_for_move(&track.id, index.saturating_add(1))
                .unwrap_or_default(),
        })
    }

    /// Build only the data needed to resolve one resource at the authority
    /// door. This is intentionally separate from [`Self::music_resource_ctx`]:
    /// opening Ctrl+K should stay O(page size), while activating a search hit
    /// can still re-resolve any stable resource against current library state.
    pub(crate) fn music_resource_ctx_for_id(
        &self,
        id: &str,
    ) -> Option<crate::actions::MusicResourceCtx> {
        if id == "music.library" {
            return Some(self.music_resource_ctx());
        }
        if crate::actions::parse_view_resource_id(id).is_some() {
            return Some(crate::actions::MusicResourceCtx {
                playback: self.pb.clone(),
                ..Default::default()
            });
        }
        let mut ctx = crate::actions::MusicResourceCtx {
            local_file: self.local_file_resource(),
            playback: self.pb.clone(),
            ..Default::default()
        };
        if id == crate::actions::LOCAL_FILE_RESOURCE_ID {
            return Some(ctx);
        }
        if id == "music.player:player" {
            ctx.tracks = self.catalog_track_resources();
            return Some(ctx);
        }
        if id == crate::actions::QUEUE_RESOURCE_ID {
            ctx.queue = self
                .pb
                .queue
                .iter()
                .take(crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT)
                .enumerate()
                .map(|(index, track)| crate::actions::QueueItemResource {
                    index,
                    track: track.clone(),
                    label: self.track_title(track),
                    artist: self.track_artist(track),
                    current: self.pb.queue_index == index
                        && self.pb.current.as_deref() == Some(track),
                })
                .collect();
            return Some(ctx);
        }
        if let Some((index, track_id)) = crate::actions::parse_queue_item_resource_id(id) {
            if self.pb.queue.get(index).map(String::as_str) != Some(track_id.as_str()) {
                return None;
            }
            ctx.queue.push(crate::actions::QueueItemResource {
                index,
                label: self.track_title(&track_id),
                artist: self.track_artist(&track_id),
                track: track_id,
                current: self.pb.queue_index == index
                    && self.pb.current.as_deref() == self.pb.queue.get(index).map(String::as_str),
            });
            return Some(ctx);
        }
        if let Some(track_id) = id.strip_prefix("music.track:").filter(|id| !id.is_empty()) {
            let track = self.lib.track(track_id)?;
            ctx.tracks.push(crate::actions::CatalogTrackResource {
                id: track.id.clone(),
                label: self.track_title(&track.id),
                artist: self.track_artist(&track.id),
                album_id: track.album_id.clone(),
                artist_id: track.artist_id.clone(),
                offline: matches!(self.lib.cache_state(&track.id), CacheState::Cached),
                liked: self.lib.is_liked(&track.id),
            });
            return Some(ctx);
        }
        if let Some(album_id) = id.strip_prefix("music.album:").filter(|id| !id.is_empty()) {
            let album = self.lib.album(album_id)?;
            ctx.albums.push(crate::actions::CatalogAlbumResource {
                id: album.id.clone(),
                label: album.title.clone(),
                saved: self.lib.is_album_saved(&album.id),
            });
            return Some(ctx);
        }
        if let Some(artist_id) = id.strip_prefix("music.artist:").filter(|id| !id.is_empty()) {
            let artist = self.lib.artist(artist_id)?;
            ctx.artists.push(crate::actions::CatalogArtistResource {
                id: artist.id.clone(),
                label: artist.name.clone(),
                followed: self.lib.is_artist_followed(&artist.id),
            });
            return Some(ctx);
        }
        if let Some((playlist_id, track_id)) = crate::actions::parse_playlist_track_resource_id(id)
        {
            ctx.membership = Some(self.membership_resource(&playlist_id, &track_id)?);
            return Some(ctx);
        }
        let playlist_id = id
            .strip_prefix("music.playlist:")
            .filter(|id| !id.is_empty())?;
        let playlist = self.lib.playlist(playlist_id)?;
        // `add-track` needs a current target set to refuse stale resource
        // arguments; retain only ids/labels once this playlist is directly
        // requested, never while the palette merely opens.
        ctx.tracks = self.catalog_track_resources();
        ctx.playlists.push(crate::actions::PlaylistResource {
            id: playlist.id.clone(),
            name: playlist.name.clone(),
            tracks: Vec::new(),
        });
        Some(ctx)
    }

    fn catalog_track_resources(&self) -> Vec<crate::actions::CatalogTrackResource> {
        self.lib
            .tracks
            .values()
            .map(|track| crate::actions::CatalogTrackResource {
                id: track.id.clone(),
                label: self.track_title(&track.id),
                artist: self.track_artist(&track.id),
                album_id: track.album_id.clone(),
                artist_id: track.artist_id.clone(),
                offline: matches!(self.lib.cache_state(&track.id), CacheState::Cached),
                liked: self.lib.is_liked(&track.id),
            })
            .collect()
    }

    /// A track's title for the palette (its id when the track is gone).
    fn track_title(&self, track_id: &str) -> String {
        self.playback_track(track_id)
            .map_or_else(|| track_id.to_string(), |t| t.title.clone())
    }

    /// Who performs a track, the palette row's secondary line.
    fn track_artist(&self, track_id: &str) -> String {
        self.playback_track(track_id)
            .map(|t| self.lib.artist_name(&t.artist_id).to_string())
            .unwrap_or_default()
    }

    fn current_duration_ms(&self) -> u64 {
        if self
            .local_file
            .as_ref()
            .is_some_and(|file| self.pb.current.as_deref() == Some(&file.track.id))
        {
            return self.audio.duration_ms().unwrap_or(0);
        }
        self.pb
            .current
            .as_ref()
            .and_then(|id| self.playback_track(id))
            .map_or(0, super::model::Track::duration_ms)
    }

    /// The next few queued track ids (after the current one), for the queue
    /// hover preview. Respects the linear queue order; empty when nothing is
    /// queued up next.
    fn queue_preview_tracks(&self) -> Vec<String> {
        self.pb
            .queue
            .iter()
            .skip(self.pb.queue_index + 1)
            .take(4)
            .cloned()
            .collect()
    }

    /// Whether the queue hover preview should be showing: the pointer is over
    /// the now-playing bar's queue button. Captured geometry, so this reflects
    /// the real, laid-out button rect.
    pub(crate) fn queue_preview_active(&self) -> bool {
        self.queue_btn_rect.width() > 0.0 && self.queue_btn_rect.contains(self.mouse)
    }

    /// The best cover-art id for a track: the track's own recorded cover when
    /// present, else its album's cover. Album-track hits frequently omit a
    /// per-track thumbnail (only the album carries one), so the now-playing bar
    /// must fall back to the album art rather than a procedural placeholder.
    fn best_cover_id(&self, track_id: &str, album_id: &str) -> String {
        if self.art_urls.contains_key(track_id) {
            track_id.to_string()
        } else if !album_id.is_empty() && self.art_urls.contains_key(album_id) {
            album_id.to_string()
        } else {
            track_id.to_string()
        }
    }

    // ---- MPRIS media control ---------------------------------------------

    /// Bring up the MPRIS D-Bus service (media keys + the desktop media
    /// controls). Called only from the live-binary entry (`app_root`). Safe
    /// everywhere: on unsupported hosts, without `audio`, or without a session bus
    /// it installs the
    /// inert stub, so no code path branches on whether MPRIS is present.
    pub fn enable_mpris(&mut self) {
        let snap = self.mpris_snapshot();
        self.mpris = MprisHandle::spawn(crate::mpris::config(), snap);
    }

    /// Stage an authenticated shell handoff. It is applied after the shared
    /// library has loaded, so the stable track id must resolve on this device
    /// before Music advertises acceptance back to the source.
    #[must_use]
    pub fn with_media_handoff(
        mut self,
        uri: String,
        position_ms: u64,
        resume_playing: bool,
    ) -> Self {
        self.pending_media_handoff = Some(PendingMediaHandoff {
            uri,
            position_ms,
            resume_playing,
        });
        self
    }

    /// Build the MPRIS snapshot from the current playback + library state.
    fn mpris_snapshot(&self) -> PlayerSnapshot {
        let status = match &self.pb.current {
            None => PlaybackStatus::Stopped,
            Some(_) if self.pb.playing => PlaybackStatus::Playing,
            Some(_) => PlaybackStatus::Paused,
        };
        let mut snap = PlayerSnapshot {
            status,
            volume: (f64::from(self.pb.volume) / 100.0).clamp(0.0, 1.0),
            position_us: (self.playhead_ms.max(0.0) * 1000.0) as i64,
            can_next: self.pb.queue_index + 1 < self.pb.queue.len(),
            // "Previous" restarts the track past a few seconds in, else steps back.
            can_prev: self.pb.queue_index > 0 || self.playhead_ms > 3000.0,
            ..PlayerSnapshot::default()
        };
        if let Some(id) = self.pb.current.clone() {
            if let Some(t) = self.playback_track(&id) {
                snap.track_id.clone_from(&id);
                snap.title.clone_from(&t.title);
                snap.artist = self.lib.artist_name(&t.artist_id).to_string();
                snap.album = self.lib.album_title(&t.album_id).to_string();
                snap.length_us = self.current_duration_ms() as i64 * 1000;
                let cover = self.best_cover_id(&id, &t.album_id);
                if let Some(url) = self.art_urls.get(&cover) {
                    snap.art_url.clone_from(url);
                }
            }
            snap.can_play = !self.pb.playing;
            snap.can_pause = self.pb.playing;
            snap.can_seek = snap.length_us > 0;
        }
        snap
    }

    /// Push the latest player state to MPRIS. Deduped inside the handle, so this
    /// is cheap to call every frame.
    fn publish_mpris(&mut self) {
        if !self.mpris.is_active() {
            return;
        }
        let snap = self.mpris_snapshot();
        self.mpris.publish(snap);
    }

    fn publish_handoff(&self) {
        let Some(track_id) = self.pb.current.as_deref() else {
            self.handoff.clear();
            return;
        };
        let Some(track) = self.lib.track(track_id) else {
            self.handoff.clear();
            return;
        };
        let uri = crate::search_type::SearchHit::track_link(track_id);
        self.handoff.publish(media::handoff::AppPlayback {
            resource_uri: &uri,
            title: &track.title,
            position_ms: self.playhead_ms.max(0.0) as u64,
            duration_ms: Some(track.duration_ms()),
            playing: self.pb.playing,
            has_video: false,
            // A handoff is acknowledged only after this device actually has
            // a decoder source. Merely resolving the synced id is not enough.
            audible: self.pb.volume > 0
                && self.audio.has_stream()
                && self.audio.frames_rendered() > 0
                && !self.audio.needs_poll(),
            controller_bus_name: Some(crate::mpris::BUS_NAME),
        });
    }

    fn apply_pending_media_handoff(&mut self, state: &mut AutoCommit) {
        let Some(pending) = self.pending_media_handoff.take() else {
            return;
        };
        let Some(crate::search_type::DeepLink::Track(track_id)) =
            crate::search_type::DeepLink::parse(&pending.uri)
        else {
            return;
        };
        if !self.lib.tracks.contains_key(&track_id) {
            // The room document may still be hydrating. Keep the exact request
            // pending; without a published session the source cannot receive a
            // false acknowledgement.
            self.pending_media_handoff = Some(pending);
            return;
        }
        self.dispatch(
            Action::Play {
                queue: vec![track_id].into(),
                index: 0,
            },
            state,
        );
        let duration = self.current_duration_ms();
        let position_ms = if duration == 0 {
            pending.position_ms
        } else {
            pending.position_ms.min(duration)
        };
        self.playhead_ms = position_ms as f64;
        self.pb.position_ms = position_ms;
        self.pb.playing = pending.resume_playing;
        if !pending.resume_playing {
            self.audio.pause();
        }
        self.persist_playback(state);
    }

    /// Drain external MPRIS control commands (media keys, the desktop media
    /// widget, `playerctl`) and dispatch each through the normal `Action`
    /// pipeline — external and in-app controls share one code path.
    fn drain_mpris(&mut self, state: &mut AutoCommit) {
        for cmd in self.mpris.drain_commands() {
            match cmd {
                MprisCommand::Play => {
                    if !self.pb.playing {
                        self.dispatch(Action::TogglePlay, state);
                    }
                }
                MprisCommand::Pause | MprisCommand::Stop => {
                    if self.pb.playing {
                        self.dispatch(Action::TogglePlay, state);
                    }
                }
                MprisCommand::PlayPause => self.dispatch(Action::TogglePlay, state),
                MprisCommand::Next => self.dispatch(Action::Next, state),
                MprisCommand::Previous => self.dispatch(Action::Prev, state),
                MprisCommand::Seek(us) => {
                    let dur = self.current_duration_ms() as f64;
                    if dur > 0.0 {
                        let target = (self.playhead_ms + us as f64 / 1000.0).clamp(0.0, dur);
                        self.dispatch(Action::SeekFrac(target / dur), state);
                    }
                }
                MprisCommand::SetPosition(us) => {
                    let dur = self.current_duration_ms() as f64;
                    if dur > 0.0 {
                        let target = (us as f64 / 1000.0).clamp(0.0, dur);
                        self.dispatch(Action::SeekFrac(target / dur), state);
                    }
                }
                MprisCommand::SetVolume(v) => {
                    self.dispatch(Action::VolumeFrac(v.clamp(0.0, 1.0)), state);
                }
                // No compositor surface to raise/quit from the app side yet.
                MprisCommand::Raise | MprisCommand::Quit => {}
            }
        }
    }

    /// Apply a completed playlist-cover pick from the file portal (if any
    /// arrived this frame). The selected local file is converted off-thread
    /// before a portable cover value is persisted onto the playlist.
    fn drain_cover_pick(&mut self, _state: &mut AutoCommit) {
        let Some(rx) = &self.cover_pick else { return };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.cover_pick = None;
                self.cover_pick_for = None;
                return;
            }
        };
        self.cover_pick = None;
        let playlist_id = self.cover_pick_for.take();
        match outcome {
            PortalOutcome::Opened(path) => {
                let Some(playlist_id) = playlist_id else {
                    return;
                };
                self.cover_embed_for = Some(playlist_id);
                self.cover_embed = Some(embed_cover_off_thread(path));
                self.notify(ToastKind::Info, "Preparing the playlist cover");
            }
            PortalOutcome::Cancelled => {}
            PortalOutcome::Failed(reason) => {
                self.notify(
                    ToastKind::Error,
                    format!("Couldn\u{2019}t set the cover: {reason}"),
                );
            }
        }
    }

    fn drain_cover_embed(&mut self, state: &mut AutoCommit) {
        let Some(receiver) = &self.cover_embed else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.cover_embed = None;
                self.cover_embed_for = None;
                self.notify(ToastKind::Error, "Couldn’t prepare the playlist cover");
                return;
            }
        };
        self.cover_embed = None;
        let playlist_id = self.cover_embed_for.take();
        match (playlist_id, result) {
            (Some(playlist_id), Ok(url)) => {
                self.perform_action(state, &crate::actions::set_cover_action(&playlist_id, &url));
                self.reload_library(state);
                self.art_urls.insert(playlist_id, url);
                self.notify(ToastKind::Success, "Playlist cover updated");
            }
            (_, Err(reason)) => {
                self.notify(
                    ToastKind::Error,
                    format!("Couldn’t set the cover: {reason}"),
                );
            }
            (None, Ok(_)) => {}
        }
    }

    /// Advance the playhead while playing and auto-advance at end of track.
    /// In-memory only (called from `draw`, which has no document handle); the
    /// updated position persists to the document on the next user event.
    fn tick(&mut self) {
        let now = ui::time::now();
        self.last_tick = now;
        self.expire_resource_stream_if_due(now);
        if (!self.pb.playing || self.pb.current.is_none()) && !self.audio.needs_poll() {
            return;
        }
        // Drive the playhead exclusively from a real pipeline. Before the
        // pipeline has prerolled (no position yet), it holds at its last known
        // position instead of running a decorative timer while a stream is
        // resolving/buffering. In particular, a build without an audio backend
        // must never claim a sample or unavailable source is progressing.
        if self.audio.has_stream() || self.audio.needs_poll() {
            // Service the pipeline bus so a failed fetch/decode is detected (and
            // the message backlog drained) before we trust the transport state.
            let errored = self.audio.pump();
            if errored {
                // The pipeline died (a dead proxy, an undecodable/forbidden
                // stream). Stop pretending to play and surface the real reason,
                // instead of freezing the transport at 0:00 while the UI still
                // shows "playing". Tearing the stream down keeps `has_stream`
                // false so we don't re-toast every frame.
                let msg = self
                    .audio
                    .take_error()
                    .unwrap_or_else(|| "this track could not be played".to_string());
                let message = format!("Can\u{2019}t play: {msg}");
                self.playback_error = Some(message.clone());
                self.notify(ToastKind::Error, message);
                self.pb.playing = false;
                self.audio.stop();
                self.fail_resource_stream(
                    "music.audio_start_failed",
                    "The audio pipeline failed after stream resolution.",
                    None,
                    true,
                );
                return;
            }
            if !self.pb.playing || self.pb.current.is_none() {
                return;
            }
            #[cfg(feature = "audio")]
            self.settle_resource_stream_after_rendered_frames(self.audio.frames_rendered());
            if self.audio.reached_eos() {
                // The staged stream ended. For a fully-streamed track this is a
                // normal end-of-song; for a SABR-truncated free URL it fires
                // ~1 minute in. Tear the ended pipeline down and move to the next
                // track (resolving + streaming it) so playback keeps going rather
                // than freezing at the truncation point.
                self.audio.stop();
                self.auto_advance();
                return;
            }
            if let Some(pos) = self.audio.position_ms() {
                self.playhead_ms = pos as f64;
            }
        }
        let dur = self.current_duration_ms();
        if dur > 0 && self.playhead_ms >= dur as f64 {
            self.auto_advance();
        }
    }

    // ---- transport --------------------------------------------------------

    /// Auto-advance to the next track *and* start streaming it. Used by the
    /// transport tick when the current stream ends — either at its real duration
    /// or early, because a free (no-PoToken) `YouTube` URL is SABR-gated to its
    /// first ~1-minute segment. Advancing without also resolving the next track
    /// would silently change the "now playing" track but leave it soundless, so
    /// this pairs the two. Carries no document handle (tick has none); the new
    /// position persists on the next real event.
    fn auto_advance(&mut self) {
        self.advance(true);
        if self.pb.playing
            && let Some(id) = self.pb.current.clone()
        {
            self.start_stream(&id);
        }
    }

    fn play_index(&mut self, index: usize) {
        if let Some(id) = self.pb.queue.get(index).cloned() {
            self.pb.queue_index = index;
            self.pb.current = Some(id);
            self.pb.playing = true;
            self.pb.position_ms = 0;
            self.playhead_ms = 0.0;
            self.bound_track = self.pb.current.clone();
        }
    }

    fn advance(&mut self, auto: bool) {
        if self.pb.queue.is_empty() {
            return;
        }
        if self.pb.repeat == RepeatMode::One && auto {
            self.pb.position_ms = 0;
            self.playhead_ms = 0.0;
            return;
        }
        // Shuffle: walk the shuffled permutation of queue positions instead of
        // the linear order, so playback genuinely follows a random sequence.
        if self.pb.shuffle {
            self.ensure_shuffle_order();
            let pos = self
                .shuffle_order
                .iter()
                .position(|&i| i == self.pb.queue_index)
                .unwrap_or(0);
            let last = self.shuffle_order.len().saturating_sub(1);
            if pos >= last {
                if self.pb.repeat == RepeatMode::All {
                    // Fresh permutation for the next lap.
                    self.reshuffle_all();
                    let first = self.shuffle_order.first().copied().unwrap_or(0);
                    self.play_index(first);
                } else {
                    self.playhead_ms = self.current_duration_ms() as f64;
                    self.pb.playing = false;
                }
            } else if let Some(&next_idx) = self.shuffle_order.get(pos + 1) {
                self.play_index(next_idx);
            }
            return;
        }
        let last = self.pb.queue.len() - 1;
        if self.pb.queue_index >= last {
            if self.pb.repeat == RepeatMode::All {
                self.play_index(0);
            } else {
                // Stop at the end of the queue.
                self.playhead_ms = self.current_duration_ms() as f64;
                self.pb.playing = false;
            }
        } else {
            let next = self.pb.queue_index + 1;
            self.play_index(next);
        }
    }

    fn previous(&mut self) {
        if self.playhead_ms > 3000.0 {
            self.pb.position_ms = 0;
            self.playhead_ms = 0.0;
            return;
        }
        // Shuffle: step back through the shuffled permutation.
        if self.pb.shuffle && !self.pb.queue.is_empty() {
            self.ensure_shuffle_order();
            let pos = self
                .shuffle_order
                .iter()
                .position(|&i| i == self.pb.queue_index)
                .unwrap_or(0);
            if pos == 0 {
                self.pb.position_ms = 0;
                self.playhead_ms = 0.0;
                return;
            }
            let Some(&prev_idx) = self.shuffle_order.get(pos - 1) else {
                return;
            };
            self.play_index(prev_idx);
            return;
        }
        if self.pb.queue_index == 0 {
            self.pb.position_ms = 0;
            self.playhead_ms = 0.0;
            return;
        }
        let prev = self.pb.queue_index - 1;
        self.play_index(prev);
    }

    // ---- shuffle order ----------------------------------------------------

    /// One step of an xorshift64* PRNG (self-contained; no `rand` dependency).
    fn next_rand(&mut self) -> u64 {
        let mut x = if self.shuffle_rng == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            self.shuffle_rng
        };
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.shuffle_rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Rebuild the shuffle order as a permutation of the queue indices, pinned
    /// so the *currently playing* track stays first (it's already playing —
    /// only what comes after it is randomised).
    fn rebuild_shuffle_order(&mut self) {
        let n = self.pb.queue.len();
        if n == 0 {
            self.shuffle_order.clear();
            return;
        }
        let cur = self.pb.queue_index.min(n - 1);
        let mut rest: Vec<usize> = (0..n).filter(|&i| i != cur).collect();
        self.fisher_yates(&mut rest);
        self.shuffle_order = std::iter::once(cur).chain(rest).collect();
    }

    /// Rebuild the shuffle order as a fresh permutation of *all* queue indices
    /// (used when a repeating queue laps, so the next lap has a new order).
    fn reshuffle_all(&mut self) {
        let n = self.pb.queue.len();
        let mut order: Vec<usize> = (0..n).collect();
        self.fisher_yates(&mut order);
        self.shuffle_order = order;
    }

    /// In-place Fisher–Yates using the app's PRNG.
    fn fisher_yates(&mut self, v: &mut [usize]) {
        if v.len() < 2 {
            return;
        }
        for i in (1..v.len()).rev() {
            let j = (self.next_rand() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }

    /// Ensure a shuffle order exists that matches the current queue length,
    /// rebuilding it (current-first) if it's empty or stale after a queue edit.
    fn ensure_shuffle_order(&mut self) {
        if self.shuffle_order.len() != self.pb.queue.len() {
            self.rebuild_shuffle_order();
        }
    }

    /// Insert a track immediately after the current one ("Play next"). If
    /// nothing is playing this seeds and starts the queue.
    fn queue_play_next(&mut self, id: String) {
        if self.pb.queue.is_empty() || self.pb.current.is_none() {
            self.pb.queue = vec![id];
            self.play_index(0);
            return;
        }
        let at = (self.pb.queue_index + 1).min(self.pb.queue.len());
        self.pb.queue.insert(at, id);
    }

    /// Remove the queue entry at an absolute index, keeping `queue_index`
    /// pointed at the same logical track (or the next one if the current was
    /// removed).
    fn queue_remove(&mut self, index: usize) {
        if index >= self.pb.queue.len() {
            return;
        }
        self.pb.queue.remove(index);
        if index < self.pb.queue_index {
            self.pb.queue_index = self.pb.queue_index.saturating_sub(1);
        } else if index == self.pb.queue_index {
            self.pb.position_ms = 0;
            self.playhead_ms = 0.0;
            // The current track was dropped: clamp and re-bind to whatever now
            // sits at this slot (or stop if the queue emptied).
            if self.pb.queue.is_empty() {
                self.pb.current = None;
                self.pb.playing = false;
                self.pb.queue_index = 0;
            } else {
                self.pb.queue_index = self.pb.queue_index.min(self.pb.queue.len() - 1);
                self.pb.current = self.pb.queue.get(self.pb.queue_index).cloned();
            }
        }
        self.bound_track = self.pb.current.clone();
    }

    /// Reorder a queue entry, keeping `queue_index` on the same track.
    fn queue_move(&mut self, from: usize, to: usize) {
        let n = self.pb.queue.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let current_id = self.pb.queue.get(self.pb.queue_index).cloned();
        let item = self.pb.queue.remove(from);
        self.pb.queue.insert(to, item);
        if let Some(cur) = current_id
            && let Some(pos) = self.pb.queue.iter().position(|t| *t == cur)
        {
            self.pb.queue_index = pos;
        }
    }

    // --- name-prompt dialog (shared ModalDialog + real TextInput) --------

    /// Build the name-prompt dialog from `name_prompt` + the staged
    /// `name_input` (which seeds the field for a rename or a `--shots` frame).
    fn ensure_name_dialog(&mut self) {
        if self.name_dialog.is_some() {
            return;
        }
        let Some(kind) = self.name_prompt.clone() else {
            return;
        };
        let (title, primary) = match kind {
            NamePrompt::NewPlaylist => ("New playlist", "Create"),
            NamePrompt::Rename(_) => ("Rename playlist", "Save"),
        };
        let body = PromptBody::new("", "Playlist name").with_initial(&self.name_input);
        let submit = Rc::new(Cell::new(false));
        let cancel = Rc::new(Cell::new(false));
        let dialog = ModalDialog::new(title, body)
            .open(true)
            .with_max_width(420.0)
            .primary_action(primary, {
                let s = submit.clone();
                move || s.set(true)
            })
            .secondary_action("Cancel", {
                let c = cancel.clone();
                move || c.set(true)
            })
            .on_dismiss({
                let c = cancel.clone();
                move || c.set(true)
            });
        self.name_dialog = Some(NameDlg {
            dialog,
            submit,
            cancel,
        });
    }

    /// Lay the name-prompt dialog out against the app rect so its box + footer
    /// buttons resolve for hit-testing and paint.
    fn layout_name_dialog(&mut self) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let rect = self.rect;
        let Some(nd) = self.name_dialog.as_mut() else {
            return;
        };
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = nd.dialog.build_layout(&mut taffy, &theme, &mut node_map);
        // `new_with_children` only fails when a child `NodeId` belongs to a
        // *different* tree, which never happens here — `node` was just built
        // in this exact `taffy` above.
        #[allow(
            clippy::expect_used,
            reason = "new_with_children only errors for a child NodeId from a different tree; `node` was just built in this exact taffy above"
        )]
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(rect.width() as f32),
                        height: Dimension::length(rect.height() as f32),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .expect("a single freshly-created taffy node always attaches to a fresh tree");
        let _ = taffy.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(rect.width() as f32),
                height: taffy::AvailableSpace::Definite(rect.height() as f32),
            },
        );
        nd.dialog
            .apply_layout(&taffy, node, Point::new(rect.x0, rect.y0));
    }

    /// Route an event into the open name-prompt dialog, then apply the flags its
    /// callbacks set (Enter is a submit convenience).
    fn route_name_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        let enter = matches!(
            event,
            WindowEvent::KeyboardInput { event: ke, .. }
                if ke.state == ElementState::Pressed
                    && matches!(ke.logical_key, Key::Named(NamedKey::Enter))
        );
        if enter {
            self.submit_name_prompt(state);
            *handled = true;
            self.dirty = true;
            return;
        }
        self.ensure_name_dialog();
        self.layout_name_dialog();
        let (submit, cancel) = {
            let Some(nd) = self.name_dialog.as_mut() else {
                return;
            };
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            // `new_leaf` on a fresh tree with a default `Style` cannot fail;
            // this throwaway node only satisfies `Widget::handle_event`'s
            // trait signature.
            #[allow(
                clippy::expect_used,
                reason = "new_leaf on a fresh tree with a default Style cannot fail; this throwaway node only satisfies Widget::handle_event's trait signature"
            )]
            let node = taffy
                .new_leaf(Style::default())
                .expect("a leaf on a fresh taffy tree");
            let mut ft = None;
            nd.dialog.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor,
                    modifiers: &modifiers,
                    state,
                    path: &automerge::ROOT,
                    handled,
                    focus_target: &mut ft,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            (nd.submit.replace(false), nd.cancel.replace(false))
        };
        if submit {
            self.submit_name_prompt(state);
        } else if cancel {
            self.dispatch(Action::CloseNamePrompt, state);
        }
        *handled = true;
        self.dirty = true;
    }

    /// Copy the field value into `name_input` and dispatch the commit action.
    fn submit_name_prompt(&mut self, state: &mut AutoCommit) {
        let value = self
            .name_dialog
            .as_ref()
            .map(|nd| nd.dialog.content().value().to_string())
            .unwrap_or_default();
        self.name_input = value;
        let commit = match self.name_prompt.clone() {
            Some(NamePrompt::NewPlaylist) => Action::CreatePlaylist,
            Some(NamePrompt::Rename(id)) => Action::CommitRename(id),
            None => Action::None,
        };
        self.dispatch(commit, state);
    }

    fn dispatch(&mut self, action: Action, state: &mut AutoCommit) {
        self.dirty = true;
        match action {
            Action::Nav(v) => self.on_nav(v),
            Action::Back => self.on_back(),
            Action::BrowseGenre(genre) => self.on_browse_genre(&genre),
            Action::Play { queue, index } => self.on_play(queue, index, state),
            Action::ShufflePlay(queue) => self.on_shuffle_play(queue, state),
            Action::OpenSearch => {
                self.search_opener
                    .open(ui::search::SearchRequest::this_app());
            }
            Action::TogglePlay => self.on_toggle_play(state),
            Action::Next => self.on_next(state),
            Action::Prev => self.on_prev(state),
            Action::ToggleShuffle => self.on_toggle_shuffle(state),
            Action::CycleRepeat => self.on_cycle_repeat(state),
            Action::SetRepeat(mode) => self.on_set_repeat(mode, state),
            Action::OpenPlayerMenu(anchor) => self.open_player_menu(anchor),
            Action::SeekFrac(f) => self.on_seek_frac(f, state),
            Action::VolumeFrac(f) => self.on_volume_frac(f, state),
            Action::ToggleVolumePopover => {
                self.volume_popover_open = !self.volume_popover_open;
            }
            Action::ToggleMute => self.on_toggle_mute(state),
            Action::PickPlaylistCover(playlist_id) => self.on_pick_playlist_cover(playlist_id),
            Action::AddToQueue(id) => self.on_add_to_queue(&id, state),
            Action::PlayNext(id) => self.on_play_next(&id, state),
            Action::RemoveFromQueue(index) => self.on_remove_from_queue(index, state),
            Action::MoveQueueItem { from, to } => self.on_move_queue_item(from, to, state),
            Action::ToggleLike(id) => self.on_toggle_like(&id, state),
            Action::ToggleSaveAlbum(id) => self.on_toggle_save_album(&id, state),
            Action::SetOffline { track, keep } => {
                if keep {
                    if let Err(error) = self.request_offline_download(&track) {
                        self.offline_errors.insert(track, error);
                        self.notify(ToastKind::Error, OFFLINE_FAILED);
                    }
                } else {
                    if let Some(download) = self.downloads.remove(&track) {
                        download.cancel();
                    }
                    self.offline_resolves.retain(|_, pending| pending != &track);
                    self.offline_errors.remove(&track);
                    if let Err(error) = self.media_cache.remove(&self.media_cache_root, &track) {
                        self.notify(
                            ToastKind::Error,
                            format!("Couldn’t remove the download: {error}"),
                        );
                    }
                    self.update_device_cache(&track, CacheState::Streamed);
                    self.settle_offline_track(&track, Some("offline download cancelled".into()));
                }
            }
            Action::ToggleFollow(id) => self.on_toggle_follow(&id, state),
            Action::AddToPlaylist { playlist, track } => {
                self.on_add_to_playlist(&playlist, &track, state);
            }
            Action::RemoveFromPlaylist { playlist, track } => {
                self.on_remove_from_playlist(&playlist, &track, state);
            }
            Action::MovePlaylistItem { playlist, from, to } => {
                self.on_move_playlist_item(&playlist, from, to, state);
            }
            Action::BeginNewPlaylist => self.on_begin_new_playlist(None),
            Action::BeginNewPlaylistFor(track) => self.on_begin_new_playlist(Some(track)),
            Action::CreatePlaylist => self.on_create_playlist(state),
            Action::BeginRenamePlaylist(id) => self.on_begin_rename_playlist(id),
            Action::CommitRename(id) => self.on_commit_rename(&id, state),
            Action::DeletePlaylist(id) => self.on_delete_playlist(id, state),
            Action::OpenTrackMenu {
                track,
                in_playlist,
                queue_index,
            } => self.open_track_menu(track, in_playlist.as_deref(), queue_index),
            Action::OpenAddToPlaylist(track) => self.open_add_to_playlist_menu(&track),
            Action::ToggleQueue => self.on_toggle_queue(),
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::CloseSidebar => self.drawer.close(),
            Action::CloseNamePrompt => self.on_close_name_prompt(),
            Action::None => {}
        }
    }

    fn on_cycle_repeat(&mut self, state: &mut AutoCommit) {
        self.pb.repeat = self.pb.repeat.next();
        self.persist_playback(state);
    }

    fn on_set_repeat(&mut self, mode: RepeatMode, state: &mut AutoCommit) {
        self.pb.repeat = if self.pb.repeat == mode {
            RepeatMode::Off
        } else {
            mode
        };
        self.persist_playback(state);
    }

    fn on_remove_from_queue(&mut self, index: usize, state: &mut AutoCommit) {
        let Some(track) = self.pb.queue.get(index).cloned() else {
            return;
        };
        if let Err(error) = self.apply_resource_remove_queue_item(index, &track) {
            self.notify(
                ToastKind::Error,
                format!("Couldn’t update the queue: {error}"),
            );
        }
        self.persist_playback(state);
    }

    fn on_move_queue_item(&mut self, from: usize, to: usize, state: &mut AutoCommit) {
        self.queue_move(from, to);
        self.persist_playback(state);
    }

    fn on_remove_from_playlist(&mut self, playlist: &str, track: &str, state: &mut AutoCommit) {
        self.perform_action(state, &crate::actions::remove_track_action(playlist, track));
        self.reload_library(state);
    }

    fn on_move_playlist_item(
        &mut self,
        playlist: &str,
        from: usize,
        to: usize,
        state: &mut AutoCommit,
    ) {
        let Some(track) = self
            .lib
            .playlist(playlist)
            .and_then(|playlist| playlist.track_ids.get(from))
            .cloned()
        else {
            return;
        };
        let Some(rank) = self
            .lib
            .playlist(playlist)
            .and_then(|playlist| playlist.rank_for_move(&track, to))
        else {
            return;
        };
        self.perform_action(
            state,
            &crate::actions::move_track_action(playlist, &track, rank),
        );
        self.reload_library(state);
    }

    fn on_close_name_prompt(&mut self) {
        self.name_prompt = None;
        self.name_input.clear();
        self.name_dialog = None;
        self.new_playlist_track = None;
    }

    fn on_nav(&mut self, v: View) {
        if v != self.view {
            self.nav_back.push(self.view.clone());
            self.view = v;
            self.scroll = 0.0;
        }
        // Picking a destination dismisses the overlay sidebar so the
        // chosen screen takes the full width (Adwaita flap behaviour).
        if self.drawer.is_overlay() {
            self.drawer.close();
        }
    }

    fn on_back(&mut self) {
        if let Some(v) = self.nav_back.pop() {
            self.view = v;
            self.scroll = 0.0;
        }
    }

    fn on_browse_genre(&mut self, genre: &str) {
        // A Browse tile opens the platform search for Music, prefilled with
        // the genre; the live catalogue answers through the search provider.
        self.search_opener
            .open(ui::search::SearchRequest::this_app().with_query(genre));
        if self.drawer.is_overlay() {
            self.drawer.close();
        }
    }

    fn on_play(&mut self, queue: Arc<Vec<String>>, index: usize, state: &mut AutoCommit) {
        self.pb.queue = Arc::unwrap_or_clone(queue);
        // A fresh queue invalidates any prior shuffle permutation; it's
        // rebuilt (current-first) on the next shuffled advance.
        self.shuffle_order.clear();
        self.play_index(index);
        if let Some(id) = self.pb.current.clone() {
            self.persist_live_track(&id, state);
            self.start_stream(&id);
        }
        self.persist_playback(state);
    }

    fn on_toggle_play(&mut self, state: &mut AutoCommit) {
        // Play/pause is a no-op with nothing loaded: never flip the
        // transport to a fake "playing" state when there's no current
        // track. The button simply does nothing until a song is queued.
        if self.pb.current.is_none() {
            return;
        }
        self.pb.playing = !self.pb.playing;
        if self.pb.playing {
            if self.audio.has_stream() {
                self.audio.play();
            } else if let Some(id) = self.pb.current.clone() {
                self.start_stream(&id);
            }
        } else {
            self.audio.pause();
        }
        self.persist_playback(state);
    }

    fn on_next(&mut self, state: &mut AutoCommit) {
        self.advance(false);
        if let Some(id) = self.pb.current.clone() {
            self.persist_live_track(&id, state);
            self.start_stream(&id);
        }
        self.persist_playback(state);
    }

    fn on_prev(&mut self, state: &mut AutoCommit) {
        self.previous();
        if let Some(id) = self.pb.current.clone() {
            self.persist_live_track(&id, state);
            self.start_stream(&id);
        }
        self.persist_playback(state);
    }

    fn on_toggle_shuffle(&mut self, state: &mut AutoCommit) {
        self.pb.shuffle = !self.pb.shuffle;
        if self.pb.shuffle {
            // Randomise the upcoming order now so the next/auto-advance
            // follows a shuffled sequence.
            self.rebuild_shuffle_order();
        } else {
            // Back to the queue's linear order.
            self.shuffle_order.clear();
        }
        self.persist_playback(state);
    }

    fn on_shuffle_play(&mut self, queue: Arc<Vec<String>>, state: &mut AutoCommit) {
        if queue.is_empty() {
            return;
        }
        let start = (self.next_rand() % queue.len() as u64) as usize;
        self.pb.shuffle = true;
        self.on_play(queue, start, state);
        self.rebuild_shuffle_order();
        self.persist_playback(state);
    }

    fn on_seek_frac(&mut self, f: f64, state: &mut AutoCommit) {
        let dur = self.current_duration_ms();
        self.playhead_ms = (f.clamp(0.0, 1.0) * dur as f64).max(0.0);
        self.audio.seek_ms(self.playhead_ms as u64);
        self.persist_playback(state);
    }

    fn on_volume_frac(&mut self, f: f64, state: &mut AutoCommit) {
        self.pb.volume = (f.clamp(0.0, 1.0) * 100.0).round() as u8;
        // Dragging the volume above zero clears a mute.
        if self.pb.volume > 0 {
            self.premute_volume = None;
        }
        self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
        self.persist_playback(state);
    }

    fn on_toggle_mute(&mut self, state: &mut AutoCommit) {
        if self.pb.volume > 0 {
            self.premute_volume = Some(self.pb.volume);
            self.pb.volume = 0;
        } else {
            // Restore the pre-mute level (default to a sensible level if
            // we were already at zero with nothing captured).
            self.pb.volume = self.premute_volume.take().filter(|&v| v > 0).unwrap_or(80);
        }
        self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
        self.persist_playback(state);
    }

    fn on_pick_playlist_cover(&mut self, playlist_id: String) {
        // Route through the toolkit's native host file chooser.
        // The pick is async; the frame loop drains the
        // outcome and applies the cover.
        self.cover_pick_for = Some(playlist_id);
        self.cover_pick = Some(cover_request().send());
        self.notify(ToastKind::Info, "Choose a cover image");
    }

    fn on_add_to_queue(&mut self, id: &str, state: &mut AutoCommit) {
        self.pb.queue.push(id.to_string());
        self.notify(ToastKind::Success, "Added to the queue");
        self.persist_playback(state);
    }

    fn on_play_next(&mut self, id: &str, state: &mut AutoCommit) {
        self.queue_play_next(id.to_string());
        self.notify(ToastKind::Success, "Playing next");
        if self.pb.current.as_deref() == Some(id) {
            self.start_stream(id);
        }
        self.persist_playback(state);
    }

    fn on_toggle_like(&mut self, id: &str, state: &mut AutoCommit) {
        let liked = !self.lib.is_liked(id);
        self.perform_action(state, &crate::actions::set_liked_action(id, liked));
        self.reload_library(state);
        let now = self.lib.is_liked(id);
        self.notify(
            ToastKind::Success,
            if now {
                "Added to liked songs"
            } else {
                "Removed from liked songs"
            },
        );
    }

    fn on_toggle_save_album(&mut self, id: &str, state: &mut AutoCommit) {
        let saved = !self.lib.is_album_saved(id);
        self.perform_action(state, &crate::actions::set_saved_album_action(id, saved));
        self.reload_library(state);
        let now = self.lib.is_album_saved(id);
        self.notify(
            ToastKind::Success,
            if now {
                "Saved to your library"
            } else {
                "Removed from your library"
            },
        );
    }

    fn on_toggle_follow(&mut self, id: &str, state: &mut AutoCommit) {
        let followed = !self.lib.is_artist_followed(id);
        self.perform_action(
            state,
            &crate::actions::set_followed_artist_action(id, followed),
        );
        self.reload_library(state);
        let now = self.lib.is_artist_followed(id);
        // Prefer a live artist-page name; fall back to the library.
        let label = self
            .artist_pages
            .get(id)
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| self.lib.artist_name(id).to_string());
        self.notify(
            ToastKind::Success,
            if now {
                format!("Following {label}")
            } else {
                format!("Unfollowed {label}")
            },
        );
    }

    fn on_add_to_playlist(&mut self, playlist: &str, track: &str, state: &mut AutoCommit) {
        let Some(rank) = self
            .lib
            .playlist(playlist)
            .map(crate::model::Playlist::append_rank)
        else {
            return;
        };
        self.perform_action(
            state,
            &crate::actions::add_track_action(playlist, track, rank),
        );
        self.reload_library(state);
        let pl = self
            .lib
            .playlist(playlist)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        self.notify(ToastKind::Success, format!("Added to {pl}"));
    }

    /// Begin the new-playlist name prompt; `track` is set when this came from
    /// "Add to playlist… → New playlist…" so the create also adds it.
    fn on_begin_new_playlist(&mut self, track: Option<String>) {
        self.name_prompt = Some(NamePrompt::NewPlaylist);
        self.name_input = String::new();
        self.name_dialog = None;
        self.new_playlist_track = track;
    }

    fn on_create_playlist(&mut self, state: &mut AutoCommit) {
        let name = if self.name_input.trim().is_empty() {
            "New playlist".to_string()
        } else {
            self.name_input.trim().to_string()
        };
        // Mint the id at authoring time and carry it in the action, so
        // the create replays deterministically (see `crate::actions`).
        let id = crate::actions::new_playlist_id();
        self.perform_action(state, &crate::actions::create_playlist_action(&id, &name));
        // If this create came from "Add to playlist… → New playlist…",
        // drop the target track into the freshly-created playlist — a
        // second action bracketed into the same undo boundary.
        let added_track = self.new_playlist_track.take();
        if let Some(track) = &added_track {
            self.perform_action(state, &crate::actions::add_track_action(&id, track, 0));
        }
        self.reload_library(state);
        self.name_prompt = None;
        self.name_input.clear();
        self.name_dialog = None;
        self.nav_back.push(self.view.clone());
        self.view = View::Playlist(id);
        self.scroll = 0.0;
        self.notify(
            ToastKind::Success,
            if added_track.is_some() {
                format!("Added to {name}")
            } else {
                format!("Created {name}")
            },
        );
    }

    fn on_begin_rename_playlist(&mut self, id: String) {
        self.name_input = self
            .lib
            .playlist(&id)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        self.name_prompt = Some(NamePrompt::Rename(id));
        self.name_dialog = None;
    }

    fn on_commit_rename(&mut self, id: &str, state: &mut AutoCommit) {
        let name = self.name_input.trim().to_string();
        if !name.is_empty() {
            self.perform_action(state, &crate::actions::rename_playlist_action(id, &name));
            self.reload_library(state);
        }
        self.name_prompt = None;
        self.name_input.clear();
        self.name_dialog = None;
    }

    fn on_delete_playlist(&mut self, id: String, state: &mut AutoCommit) {
        self.perform_action(state, &crate::actions::delete_playlist_action(&id));
        self.reload_library(state);
        // Leave the now-deleted detail page.
        if self.view == View::Playlist(id) {
            self.view = View::Playlists;
            self.scroll = 0.0;
        }
        self.notify(ToastKind::Success, "Playlist deleted");
    }

    fn on_toggle_queue(&mut self) {
        if self.view == View::Queue {
            // Toggle off: restore exactly where the user was (view +
            // scroll), not a reset. Fall back to nav-back if the queue
            // was reached some other way.
            if let Some((v, s)) = self.queue_return.take() {
                self.view = v;
                self.scroll = s;
            } else if let Some(v) = self.nav_back.pop() {
                self.view = v;
                self.scroll = 0.0;
            }
        } else {
            // Toggle on: remember the current view + scroll so the next
            // toggle returns here.
            self.queue_return = Some((self.view.clone(), self.scroll));
            self.view = View::Queue;
            self.scroll = 0.0;
        }
        if self.drawer.is_overlay() {
            self.drawer.close();
        }
    }

    /// Resolve a track's stream URL and hand it to the audio engine.
    ///
    /// Every source is resolved on the [`crate::stream`] worker thread — the UI
    /// never performs a synchronous network request while handling a click. The
    /// resolved URL is handed to the audio engine when it lands, in
    /// [`Self::drain_worker`]. If the configured backend cannot serve the
    /// selected id (for example an old bundled/demo entry), it reports a clear
    /// failure and stops transport rather than simulating playback.
    fn start_stream(&mut self, track_id: &str) -> Option<u64> {
        if self
            .local_file
            .as_ref()
            .is_some_and(|file| file.track.id == track_id)
        {
            return match self.start_local_file() {
                Ok(request) => Some(request),
                Err(error) => {
                    self.playback_error = Some(error.clone());
                    self.notify(ToastKind::Error, error);
                    self.pb.playing = false;
                    self.audio.stop();
                    None
                }
            };
        }
        if self.play_cached_track_if_available(track_id) {
            return None;
        }
        let t = self.lib.track(track_id)?;
        let yt_id = t.yt_id.clone();

        self.ensure_worker();
        let Some(request_id) = self.next_worker_request_id() else {
            self.active_resolve_request = None;
            self.resolve_in_flight = 0;
            let message = "Can’t start audio: request identity space is exhausted".to_string();
            self.playback_error = Some(message.clone());
            self.notify(ToastKind::Error, message);
            self.pb.playing = false;
            self.audio.stop();
            return None;
        };
        // A direct UI/media-key choice wins over a still-running host effect.
        // The old worker result has no authority after this point, but the host
        // must receive a terminal answer rather than polling Accepted forever.
        self.supersede_resource_stream_if_needed(request_id);
        let queued = if let Some(w) = &self.worker {
            // Stop the previous stream while the next one resolves so two
            // tracks never overlap on a fast skip.
            self.audio.pause();
            w.request(StreamRequest::Resolve {
                track_id: track_id.to_string(),
                video_id: yt_id,
                request_id,
            })
        } else {
            Err("music worker is unavailable".to_string())
        };
        match queued {
            Ok(()) => {
                self.active_resolve_request = Some(request_id);
                self.playback_error = None;
                // Only the newest intent keeps the UI awake. Older queued work may
                // still finish on the serial backend, but it cannot affect this
                // transport or create an idle redraw loop.
                self.resolve_in_flight = 1;
                Some(request_id)
            }
            Err(error) => {
                self.active_resolve_request = None;
                self.resolve_in_flight = 0;
                let message = format!("Can\u{2019}t start audio: {error}");
                self.playback_error = Some(message.clone());
                self.notify(ToastKind::Error, message);
                self.pb.playing = false;
                self.audio.stop();
                None
            }
        }
    }

    /// Prefer a verified local object before constructing a resolver request.
    /// The `file://` URI never crosses the resource/action boundary; it stays
    /// inside this device's audio engine and remains usable after restart.
    fn play_cached_track_if_available(&mut self, track_id: &str) -> bool {
        let Ok(Some(path)) = self.media_cache.touch(&self.media_cache_root, track_id) else {
            return false;
        };
        let uri = match media::source::path_to_file_uri(&path) {
            Ok(uri) => uri,
            Err(error) => {
                tracing::warn!(%error, "cached audio path cannot be represented as a file URI");
                return false;
            }
        };
        self.fail_resource_stream(
            "music.stream_start_superseded",
            "A cached source replaced the pending playback request.",
            None,
            true,
        );
        self.audio.load(&uri);
        if self.pb.playing {
            self.audio.play();
        }
        self.update_device_cache(track_id, CacheState::Cached);
        true
    }

    fn request_offline_download(&mut self, track_id: &str) -> Result<(), String> {
        if self.downloads.contains_key(track_id)
            || self.offline_resolves.values().any(|id| id == track_id)
        {
            return Ok(());
        }
        let video_id = self
            .lib
            .track(track_id)
            .ok_or_else(|| "offline-cache target is no longer in the library".to_string())?
            .yt_id
            .clone();
        let request_id = self
            .next_worker_request_id()
            .ok_or_else(|| "offline request identity space is exhausted".to_string())?;
        self.ensure_worker();
        let worker = self
            .worker
            .as_ref()
            .ok_or_else(|| "music worker is unavailable".to_string())?;
        worker.request(StreamRequest::Resolve {
            track_id: track_id.to_string(),
            video_id,
            request_id,
        })?;
        self.offline_resolves
            .insert(request_id, track_id.to_string());
        self.offline_errors.remove(track_id);
        self.update_device_cache(track_id, CacheState::Downloading(0.0));
        Ok(())
    }

    fn settle_offline_track(&mut self, track_id: &str, error: Option<String>) {
        let Some((invocation, empty)) =
            self.offline_invocation
                .as_mut()
                .and_then(|(invocation, pending)| {
                    pending
                        .remove(track_id)
                        .then_some((*invocation, pending.is_empty()))
                })
        else {
            return;
        };
        if error.is_some() || empty {
            self.offline_invocation = None;
            let update = match error {
                Some(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "music.offline_download_failed".into(),
                        message,
                        details: Some(serde_json::json!({"track_id": track_id})),
                        retryable: true,
                    },
                },
                None => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({"state":"cached"})),
                        resources: Vec::new(),
                    },
                },
            };
            self.offline_results.push(update);
        }
    }

    /// Register a host operation against one exact resolver request. Kept in
    /// the app rather than the render bridge so timeout/supersession logic sees
    /// every stream intent through the same state machine.
    pub(crate) fn begin_resource_stream_invocation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        request_id: u64,
    ) {
        self.resource_stream_resume = None;
        self.resource_stream_invocation = Some((
            invocation,
            request_id,
            if self
                .local_file
                .as_ref()
                .is_some_and(|file| self.pb.current.as_deref() == Some(&file.track.id))
            {
                0
            } else {
                self.audio.frames_rendered()
            },
            ui::time::now() + STREAM_START_TIMEOUT,
        ));
    }

    /// Give a pending host operation a terminal failure exactly once. Results
    /// remain queued until that invocation is polled, so later operations and
    /// late worker events cannot overwrite them.
    fn fail_resource_stream(
        &mut self,
        code: &str,
        message: &str,
        details: Option<serde_json::Value>,
        retryable: bool,
    ) {
        self.resource_stream_resume = None;
        if let Some((invocation, _, _, _)) = self.resource_stream_invocation.take() {
            self.resource_stream_results
                .push(tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: code.into(),
                        message: message.into(),
                        details,
                        retryable,
                    },
                });
        }
    }

    /// A later local stream choice supersedes a host lifecycle invocation. We
    /// deliberately keep the new local intent responsive instead of rejecting
    /// media keys or an on-screen skip while an external resolver is slow.
    fn supersede_resource_stream_if_needed(&mut self, replacement_request: u64) {
        if self
            .resource_stream_invocation
            .is_some_and(|(_, expected, _, _)| expected != replacement_request)
        {
            self.fail_resource_stream(
                "music.stream_start_superseded",
                "A newer local playback choice superseded this stream start.",
                Some(serde_json::json!({ "replacement_request": replacement_request })),
                true,
            );
        }
    }

    /// Time out a hosted stream start if neither the resolver nor the audio
    /// backend has produced the advertised terminal evidence by its deadline.
    /// Late events are ignored because the active request is cleared first.
    fn expire_resource_stream_if_due(&mut self, now: Instant) {
        let Some((_, request_id, _, deadline)) = self.resource_stream_invocation else {
            return;
        };
        if now < deadline {
            return;
        }
        if self.active_resolve_request == Some(request_id) {
            self.active_resolve_request = None;
            self.resolve_in_flight = 0;
        }
        // The resolver may already have delivered a URL while the staged
        // download or audio pipeline remains wedged. The lifecycle deadline
        // covers that whole start, so retire transport in either phase.
        self.pb.playing = false;
        self.audio.stop();
        let message = "Audio startup timed out before playback could begin.";
        self.playback_error = Some(message.into());
        self.toast = Some(Notice {
            text: message.into(),
            kind: ToastKind::Error,
            at: now,
        });
        self.fail_resource_stream(
            "music.stream_start_timed_out",
            message,
            Some(serde_json::json!({ "request_id": request_id })),
            true,
        );
    }

    /// Apply one canonical playback effect and return the exact resolver id
    /// when it needs asynchronous stream startup. Queue/seek/volume snapshots
    /// settle locally and return `None`.
    pub(crate) fn apply_resource_playback(
        &mut self,
        playback: Playback,
    ) -> Result<Option<u64>, String> {
        if playback.queue.len() > 10_000
            || playback.volume > 100
            || playback
                .queue
                .iter()
                .any(|id| self.playback_track(id).is_none())
        {
            return Err("invalid local playback request".into());
        }
        match playback.current.as_deref() {
            None if playback.playing || playback.position_ms != 0 || playback.queue_index != 0 => {
                return Err("an empty player must be stopped at position zero".into());
            }
            Some(current)
                if playback.queue.get(playback.queue_index).map(String::as_str)
                    != Some(current)
                    || self.playback_track(current).is_none() =>
            {
                return Err("current playback track does not match the queue".into());
            }
            Some(current)
                if {
                    let duration = if self.pb.current.as_deref() == Some(current) {
                        self.current_duration_ms()
                    } else {
                        self.playback_track(current)
                            .map_or(0, crate::model::Track::duration_ms)
                    };
                    let pending_local_duration = duration == 0
                        && self
                            .local_file
                            .as_ref()
                            .is_some_and(|file| file.track.id == current);
                    !pending_local_duration && playback.position_ms > duration
                } =>
            {
                return Err("playback position exceeds the selected track duration".into());
            }
            _ => {}
        }
        let requested_playing = playback.playing;
        let changed_track = self.pb.current != playback.current;
        let changed_position = self.pb.position_ms != playback.position_ms;
        self.pb = playback;
        self.playhead_ms = self.pb.position_ms as f64;
        self.bound_track.clone_from(&self.pb.current);
        self.shuffle_order.clear();
        self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
        if changed_track && !self.pb.playing {
            self.retire_replaced_playback();
        }
        let needs_source = (changed_track || !self.audio.has_stream()) && self.pb.playing;
        let request = if needs_source {
            self.pb
                .current
                .clone()
                .and_then(|id| self.start_stream(&id))
        } else {
            if self.pb.playing {
                self.audio.play();
            } else {
                self.audio.pause();
            }
            None
        };
        if requested_playing && request.is_none() && !self.audio.has_stream() {
            return Err(self
                .playback_error
                .clone()
                .unwrap_or_else(|| "Audio source could not start".into()));
        }
        if changed_position || (needs_source && self.pb.position_ms > 0) {
            self.audio.seek_ms(self.pb.position_ms);
        }
        if let Err(error) = self.persist_device_profile() {
            self.audio.stop();
            self.pb.playing = false;
            return Err(error);
        }
        Ok(request)
    }

    fn retire_replaced_playback(&mut self) {
        self.audio.stop();
        self.active_resolve_request = None;
        self.resolve_in_flight = 0;
        self.fail_resource_stream(
            "music.stream_start_superseded",
            "The selected audio source changed.",
            None,
            true,
        );
    }

    pub(crate) fn apply_resource_play_track(&mut self, id: &str) -> Result<Option<u64>, String> {
        if self.playback_track(id).is_none() {
            return Err("track is no longer in the library".into());
        }
        self.pb.queue = vec![id.to_string()];
        self.shuffle_order.clear();
        self.play_index(0);
        if self.play_cached_track_if_available(id) {
            return Ok(None);
        }
        self.start_stream(id)
            .map(Some)
            .ok_or_else(|| "stream resolver could not start".to_string())
    }

    /// Start one exact queue occurrence selected through the resource/action
    /// surface. A queue may contain duplicate tracks, so validating only a
    /// track id would let a stale result play the wrong occurrence after a
    /// reorder. This remains device-local by design: only the listener who
    /// invoked it changes playback.
    pub(crate) fn apply_resource_play_queue_item(
        &mut self,
        index: usize,
        track: &str,
    ) -> Result<Option<u64>, String> {
        if self.pb.queue.get(index).map(String::as_str) != Some(track) {
            return Err("queue item is no longer at that position".into());
        }
        self.pb.queue_index = index;
        self.pb.current = Some(track.to_string());
        self.pb.position_ms = 0;
        self.playhead_ms = 0.0;
        self.pb.playing = true;
        self.bound_track = Some(track.to_string());
        self.shuffle_order.clear();
        if self.play_cached_track_if_available(track) {
            self.persist_device_profile()?;
            return Ok(None);
        }
        let request = self
            .start_stream(track)
            .ok_or_else(|| "stream resolver could not start".to_string())?;
        self.persist_device_profile()?;
        Ok(Some(request))
    }

    /// Apply a stale-safe queue removal routed through a canonical queue-item
    /// resource. If deleting the current item promotes another row, the audio
    /// backend follows it immediately rather than continuing to play a row
    /// that is no longer in the queue.
    pub(crate) fn apply_resource_remove_queue_item(
        &mut self,
        index: usize,
        track: &str,
    ) -> Result<(), String> {
        if self.pb.queue.get(index).map(String::as_str) != Some(track) {
            return Err("queue item is no longer at that position".into());
        }
        let removed_current = index == self.pb.queue_index;
        let keep_playing = self.pb.playing;
        self.queue_remove(index);
        if removed_current {
            self.retire_replaced_playback();
            if keep_playing && let Some(current) = self.pb.current.clone() {
                let _ = self.start_stream(&current);
            }
        }
        self.persist_device_profile()
    }

    /// Apply a stale-safe queue reorder routed through a canonical queue-item
    /// resource. Absolute indices make a queued operation unambiguous even
    /// when the same track appears several times.
    pub(crate) fn apply_resource_move_queue_item(
        &mut self,
        index: usize,
        track: &str,
        to: usize,
    ) -> Result<(), String> {
        if self.pb.queue.get(index).map(String::as_str) != Some(track) {
            return Err("queue item is no longer at that position".into());
        }
        if to >= self.pb.queue.len() {
            return Err("queue destination is no longer valid".into());
        }
        self.queue_move(index, to);
        self.persist_device_profile()
    }

    fn next_worker_request_id(&mut self) -> Option<u64> {
        // Zero is the "not requested" sentinel. Wrapping is forbidden because
        // a wrapped id could alias an old queued response. At
        // exhaustion, callers fail closed and surface a retryable error rather
        // than granting an old response authority under a reused identity.
        let next = self.next_request_id.checked_add(1)?;
        self.next_request_id = next;
        Some(next)
    }

    /// Ensure the background streaming worker exists. Spawned lazily on the
    /// first Play / search (an input event) — never during `--shots`.
    fn ensure_worker(&mut self) {
        if self.worker.is_none() {
            self.worker = Some(StreamWorker::spawn());
        }
    }

    /// Install the worker this test will talk to, instead of letting
    /// [`Self::ensure_worker`] spawn one against whatever backend the build and
    /// the host's network happen to offer. The lazy spawn keeps whatever is
    /// already here, so this decides the data source for the whole session.
    #[cfg(test)]
    pub(crate) fn use_worker(&mut self, worker: StreamWorker) {
        self.worker = Some(worker);
    }

    /// Fire a real YouTube-Music search for the current query on the worker.
    /// No-op offline builds still spawn the worker (which reports the offline
    /// backend); the UI keeps showing the local library either way.
    fn request_live_search(&mut self) {
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.ensure_worker();
        let Some(request_id) = self.next_worker_request_id() else {
            self.live_search_pending = None;
            self.active_search_request = None;
            self.search_error = Some((
                q,
                "request identity space is exhausted; restart Music to search again".to_string(),
            ));
            return;
        };
        if let Some(w) = &self.worker {
            if let Err(error) = w.request(StreamRequest::Search {
                query: q.clone(),
                request_id,
            }) {
                self.live_search_pending = None;
                self.active_search_request = None;
                self.search_error = Some((q, error));
                return;
            }
            self.active_search_request = Some(request_id);
            self.search_error = None;
            self.live_query_sent = Some(q.clone());
            self.live_search_pending = Some(q);
            self.search_edit_at = None;
        } else {
            self.search_error = Some((q, "music worker is unavailable".to_string()));
        }
    }

    /// Persist a live track (and its artist/album) into the document when it is
    /// actually played, so it joins the user's library permanently and syncs
    /// across devices. No-op for sample tracks.
    fn persist_live_track(&self, track_id: &str, state: &mut AutoCommit) {
        if !is_live_track(track_id) {
            return;
        }
        if let Some(lt) = self.live_hits.iter().find(|h| h.track_id == track_id) {
            let lt = lt.clone();
            doc::upsert_live_track(state, &lt);
        }
    }

    /// Drain the worker's updates (once per frame). Applies resolved streams to
    /// the audio engine, folds real search hits into the in-memory library and
    /// records the bound backend. Needs no document handle — search results are
    /// merged into the document lazily when a track is *played*.
    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive worker protocol keeps each request and terminal state adjacent"
    )]
    fn drain_worker(&mut self) {
        let updates = match &self.worker {
            Some(w) => w.poll(),
            None => return,
        };
        for up in updates {
            match up {
                StreamUpdate::Backend { name, live } => {
                    self.backend_live = live;
                    self.backend_name = name;
                }
                StreamUpdate::SearchResults {
                    query,
                    request_id,
                    bundle,
                } => self.apply_search_results(&query, request_id, &bundle),
                StreamUpdate::SearchUnavailable {
                    query,
                    request_id,
                    error,
                } => self.apply_search_failure(&query, request_id, &error),
                StreamUpdate::Home { content } => {
                    self.absorb_home(&content);
                    self.home = Some(*content);
                    self.home_failed = false;
                }
                StreamUpdate::HomeUnavailable => {
                    // No live Home (offline build / network error): stop the
                    // skeleton and let the honest empty Home settle.
                    self.home_failed = true;
                }
                StreamUpdate::PageUnavailable { id } => {
                    // A live detail fetch failed: stop that page's skeleton so it
                    // shows an honest "couldn't load" state instead of shimmering.
                    self.failed_pages.insert(id);
                }
                StreamUpdate::Artist { page } => {
                    self.absorb_artist(&page);
                    self.artist_pages.insert(page.id.clone(), *page);
                }
                StreamUpdate::Album { page } => {
                    self.absorb_album(&page);
                    self.album_pages.insert(page.id.clone(), *page);
                }
                StreamUpdate::Playlist { page } => {
                    self.absorb_playlist(&page);
                    self.playlist_pages.insert(page.id.clone(), *page);
                }
                StreamUpdate::Resolved {
                    track_id,
                    request_id,
                    stream,
                } => {
                    if let Some(cache_track) = self.offline_resolves.remove(&request_id) {
                        match crate::audio::OfflineDownload::start(
                            cache_track.clone(),
                            stream.url.clone(),
                            stream.fallback_url.clone(),
                            self.media_cache_root.clone(),
                        ) {
                            Ok(download) => {
                                self.downloads.insert(cache_track, download);
                            }
                            Err(error) => {
                                self.update_device_cache(&cache_track, CacheState::Streamed);
                                self.offline_errors
                                    .insert(cache_track.clone(), error.clone());
                                self.settle_offline_track(&cache_track, Some(error.clone()));
                                self.notify(ToastKind::Error, OFFLINE_FAILED);
                            }
                        }
                    } else {
                        self.apply_resolved_stream(&track_id, request_id, &stream);
                    }
                }
                StreamUpdate::ResolveFailed {
                    track_id,
                    request_id,
                    error,
                } => {
                    if let Some(cache_track) = self.offline_resolves.remove(&request_id) {
                        self.update_device_cache(&cache_track, CacheState::Streamed);
                        self.offline_errors
                            .insert(cache_track.clone(), error.clone());
                        self.settle_offline_track(&cache_track, Some(error.clone()));
                        self.notify(ToastKind::Error, OFFLINE_FAILED);
                    } else {
                        self.apply_resolve_failure(&track_id, request_id, &error);
                    }
                }
                // Per-kind "more" pages are never requested (the platform
                // search pages the live catalogue itself); notes are logs.
                StreamUpdate::SearchMore { .. }
                | StreamUpdate::SearchMoreUnavailable { .. }
                | StreamUpdate::Note(_) => {}
            }
        }
    }

    fn apply_search_results(&mut self, query: &str, request_id: u64, bundle: &SearchBundle) {
        if self.active_search_request == Some(request_id) && self.search_query == query {
            self.active_search_request = None;
            self.search_error = None;
            self.live_search
                .answer(query, &crate::live_search::rows_of(bundle));
            self.ingest_search_results(query, bundle.clone());
        }
    }

    fn apply_search_failure(&mut self, query: &str, request_id: u64, error: &str) {
        if self.active_search_request == Some(request_id) && self.search_query == query {
            self.active_search_request = None;
            self.live_search_pending = None;
            self.search_error = Some((query.to_string(), error.to_string()));
            self.live_search.fail(query);
        }
    }

    /// Send the newest catalogue query the platform search asked for to the
    /// worker, or answer it at once from the last reply when it is the same.
    fn pump_live_search(&mut self) {
        let Some(query) = self.live_search.take_query() else {
            return;
        };
        if let Some((answered, bundle)) = &self.live_bundle
            && answered.trim() == query.trim()
        {
            let rows = crate::live_search::rows_of(bundle);
            self.live_search.answer(&query, &rows);
            return;
        }
        self.search_query = query;
        self.request_live_search();
        if self.active_search_request.is_none() {
            self.live_search.fail(&self.search_query);
        }
    }

    /// Adopt a resolved stream only if it answers the currently active intent.
    /// The id check is deliberately stronger than a track-id check: returning
    /// to the same track after visiting another one creates a new request, so
    /// an older A cannot overwrite the newer A source.
    fn apply_resolved_stream(
        &mut self,
        track_id: &str,
        request_id: u64,
        stream: &crate::backend::ResolvedStream,
    ) {
        if self.active_resolve_request != Some(request_id) {
            return;
        }
        self.resolve_in_flight = 0;
        self.active_resolve_request = None;
        if self.pb.current.as_deref() == Some(track_id) {
            self.playback_error = None;
            // Prefer the rangeable proxy URL; the engine falls back to the
            // direct googlevideo URL if the proxy yields no bytes.
            self.audio
                .load_with_fallback(&stream.url, stream.fallback_url.as_deref());
            if self.playhead_ms > 0.0 {
                self.audio.seek_ms(self.playhead_ms as u64);
            }
            if self.pb.playing {
                self.audio.play();
            }
        }
        #[cfg(not(feature = "audio"))]
        if self
            .resource_stream_invocation
            .is_some_and(|(_, expected, _, _)| expected == request_id)
        {
            let Some((invocation, _, _, _)) = self.resource_stream_invocation.take() else {
                return;
            };
            self.resource_stream_results
                .push(tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput {
                        // This build has no audio backend.  Resolution means a
                        // stream URL was obtained; it must not claim playback
                        // has started when it cannot render any audio.
                        value: Some(serde_json::json!({ "state": "resolved" })),
                        resources: Vec::new(),
                    },
                });
        }
    }

    /// Complete a background resource invocation only after decoded PCM has
    /// reached the real sink.  The resolver merely gives the pipeline a URL;
    /// it is not evidence that audio playback started.
    #[cfg(feature = "audio")]
    fn settle_resource_stream_after_rendered_frames(&mut self, rendered_frames: u64) {
        if let Some((invocation, _, frames_before, _)) = self.resource_stream_invocation
            && (rendered_frames > frames_before
                || (rendered_frames > 0
                    && self.resource_stream_resume.is_some()
                    && self.resource_stream_resume == self.pb.current
                    && self.audio.is_playing_confirmed()))
        {
            self.resource_stream_invocation = None;
            self.resource_stream_resume = None;
            self.resource_stream_results
                .push(tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({ "state": "playing" })),
                        resources: Vec::new(),
                    },
                });
        }
    }

    /// Settle the *current* resolution failure visibly. Stale failures are
    /// intentionally ignored — a failed earlier A must not stop a newer A.
    fn apply_resolve_failure(&mut self, track_id: &str, request_id: u64, error: &str) {
        if self.active_resolve_request != Some(request_id) {
            return;
        }
        self.resolve_in_flight = 0;
        self.active_resolve_request = None;
        if self.pb.current.as_deref() == Some(track_id) {
            let message = format!("Can\u{2019}t stream: {error}");
            self.playback_error = Some(message.clone());
            self.notify(ToastKind::Error, message);
            self.pb.playing = false;
            self.audio.stop();
        }
        if self
            .resource_stream_invocation
            .is_some_and(|(_, expected, _, _)| expected == request_id)
        {
            self.fail_resource_stream(
                "music.stream_resolution_failed",
                &format!("Could not start audio: {error}"),
                Some(serde_json::json!({ "track_id": track_id })),
                true,
            );
        }
    }

    /// Re-inject the accumulated live search hits into the in-memory library
    /// snapshot. Idempotent; runs each frame so live tracks survive a `sync`
    /// that reloads the catalog from the document.
    fn inject_live_hits(&mut self) {
        let mut changed = false;
        for lt in &self.live_hits {
            let had_artist = self.lib.artists.contains_key(&lt.artist_id);
            self.lib
                .artists
                .entry(lt.artist_id.clone())
                .or_insert_with(|| Artist {
                    id: lt.artist_id.clone(),
                    name: lt.artist_name.clone(),
                    genre: String::new(),
                    monthly_listeners: 0,
                });
            changed |= !had_artist;
            let had_album = self.lib.albums.contains_key(&lt.album_id);
            self.lib
                .albums
                .entry(lt.album_id.clone())
                .or_insert_with(|| Album {
                    id: lt.album_id.clone(),
                    title: lt.album_title.clone(),
                    artist_id: lt.artist_id.clone(),
                    year: 0,
                });
            changed |= !had_album;
            let had_track = self.lib.tracks.contains_key(&lt.track_id);
            self.lib
                .tracks
                .entry(lt.track_id.clone())
                .or_insert_with(|| Track {
                    id: lt.track_id.clone(),
                    title: lt.title.clone(),
                    album_id: lt.album_id.clone(),
                    artist_id: lt.artist_id.clone(),
                    track_no: 0,
                    duration_s: lt.duration_s,
                    yt_id: lt.yt_id.clone(),
                    plays: 0,
                });
            changed |= !had_track;
        }
        if changed {
            self.refresh_library_search_provider();
        }
    }

    /// Remember the best cover URL for an entity id (album / artist / track /
    /// playlist). Empty ids/urls are ignored.
    fn record_cover(&mut self, id: &str, url: &str) {
        if id.is_empty() || url.is_empty() {
            return;
        }
        self.art_urls
            .entry(id.to_string())
            .or_insert_with(|| url.to_string());
    }

    /// Fold a batch of real track hits into the playable library (as live
    /// tracks) and record their covers. Idempotent.
    fn absorb_tracks(&mut self, hits: &[TrackHit]) {
        for hit in hits {
            let lt = hit_to_live(hit);
            self.record_cover(&lt.album_id, &hit.cover_url);
            self.record_cover(&lt.track_id, &hit.cover_url);
            if !self.live_hits.iter().any(|h| h.track_id == lt.track_id) {
                self.live_hits.push(lt);
            }
        }
        self.inject_live_hits();
    }

    /// Fold real Home content into the app: chart tracks become playable live
    /// tracks; every tile's cover URL is recorded for the async image cache.
    fn absorb_home(&mut self, content: &HomeContent) {
        for section in &content.sections {
            for item in &section.items {
                self.record_cover(&item.id, &item.cover_url);
                if item.kind == crate::backend::HomeItemKind::Track {
                    let hit = home_item_to_hit(item);
                    self.absorb_tracks(&[hit]);
                }
            }
        }
    }

    /// Fold a real artist page: its top tracks become playable, and the covers
    /// of its tracks + albums are recorded.
    fn absorb_artist(&mut self, page: &ArtistPage) {
        self.absorb_tracks(&page.tracks);
        for al in &page.albums {
            self.record_cover(&al.id, &al.cover_url);
        }
        // The artist-page endpoint returns a wide banner, not a square avatar.
        // Use it (cover-cropped to the circle) as a fallback when we don't
        // already have a proper avatar recorded from a home/search tile.
        self.record_cover(&page.id, &page.header_url);
    }

    /// Fold a real album page: its tracks become playable + covered.
    fn absorb_album(&mut self, page: &AlbumPage) {
        self.absorb_tracks(&page.tracks);
        self.record_cover(&page.id, &page.cover_url);
    }

    /// Fold a real (live) playlist page: its tracks become playable + covered.
    fn absorb_playlist(&mut self, page: &PlaylistPage) {
        self.absorb_tracks(&page.tracks);
        self.record_cover(&page.id, &page.cover_url);
    }

    /// Record the covers of every album/artist/playlist in a search bundle so
    /// the async image cache can fetch their real artwork; the tracks are folded
    /// into the playable library as live tracks.
    fn absorb_bundle(&mut self, bundle: &SearchBundle) {
        self.absorb_tracks(&bundle.tracks);
        for a in &bundle.albums {
            self.record_cover(&a.id, &a.cover_url);
        }
        for a in &bundle.artists {
            self.record_cover(&a.id, &a.avatar_url);
        }
        for p in &bundle.playlists {
            self.record_cover(&p.id, &p.cover_url);
        }
        if let Some(top) = &bundle.top {
            match top {
                SearchTop::Album(a) => self.record_cover(&a.id, &a.cover_url),
                SearchTop::Artist(a) => self.record_cover(&a.id, &a.avatar_url),
                SearchTop::Playlist(p) => self.record_cover(&p.id, &p.cover_url),
                SearchTop::Track(_) => {}
            }
        }
    }

    /// Ingest a fresh multi-type search response for `query`: record covers,
    /// make tracks playable, and stash the bundle the platform search reads.
    fn ingest_search_results(&mut self, query: &str, bundle: SearchBundle) {
        if self.live_search_pending.as_deref() == Some(query) {
            self.live_search_pending = None;
        }
        self.absorb_bundle(&bundle);
        let ordered: Vec<String> = bundle
            .tracks
            .iter()
            .map(|h| format!("{LIVE_TRACK_PREFIX}{}", h.video_id))
            .collect();
        self.live_results = Some((query.to_string(), ordered));
        self.live_bundle = Some((query.to_string(), bundle));
    }

    /// The decoded cover image for an entity id, if one has loaded. Requests an
    /// async fetch+decode on first sight; returns `None` (placeholder) until it
    /// lands.
    fn art_image(&self, id: &str) -> Option<vello::peniko::ImageData> {
        let url = self.art_urls.get(id)?;
        self.images.get(url)
    }

    /// Whether any cover fetch/decode is still in flight (keeps frames flowing).
    fn images_pending(&self) -> bool {
        self.images.pending() > 0
    }

    // ---- live content requests -------------------------------------------

    /// Request the real Home page once, when networking is allowed.
    fn ensure_home(&mut self) {
        if self.home_requested || !self.allow_network {
            return;
        }
        self.ensure_worker();
        if let Some(w) = &self.worker {
            match w.request(StreamRequest::Home) {
                Ok(()) => self.home_requested = true,
                Err(error) => {
                    self.home_failed = true;
                    self.notify(
                        ToastKind::Error,
                        format!("Couldn\u{2019}t load Home: {error}"),
                    );
                }
            }
        }
    }

    /// Request a real artist page for a channel id (once each).
    fn ensure_artist(&mut self, id: &str) {
        if !self.allow_network || !is_live_id(id) || self.artist_requested.contains(id) {
            return;
        }
        self.ensure_worker();
        if let Some(w) = &self.worker {
            match w.request(StreamRequest::Artist(id.to_string())) {
                Ok(()) => {
                    self.artist_requested.insert(id.to_string());
                }
                Err(error) => {
                    self.failed_pages.insert(id.to_string());
                    self.notify(
                        ToastKind::Error,
                        format!("Couldn\u{2019}t load artist: {error}"),
                    );
                }
            }
        }
    }

    /// Request a real album page for an album id (once each).
    fn ensure_album(&mut self, id: &str) {
        if !self.allow_network || !is_live_album_id(id) || self.album_requested.contains(id) {
            return;
        }
        self.ensure_worker();
        if let Some(w) = &self.worker {
            match w.request(StreamRequest::Album(id.to_string())) {
                Ok(()) => {
                    self.album_requested.insert(id.to_string());
                }
                Err(error) => {
                    self.failed_pages.insert(id.to_string());
                    self.notify(
                        ToastKind::Error,
                        format!("Couldn\u{2019}t load album: {error}"),
                    );
                }
            }
        }
    }

    /// Request a real (live YouTube-Music) playlist page for an id (once each).
    fn ensure_playlist(&mut self, id: &str) {
        if !self.allow_network || !is_live_playlist_id(id) || self.playlist_requested.contains(id) {
            return;
        }
        self.ensure_worker();
        if let Some(w) = &self.worker {
            match w.request(StreamRequest::Playlist(id.to_string())) {
                Ok(()) => {
                    self.playlist_requested.insert(id.to_string());
                }
                Err(error) => {
                    self.failed_pages.insert(id.to_string());
                    self.notify(
                        ToastKind::Error,
                        format!("Couldn\u{2019}t load playlist: {error}"),
                    );
                }
            }
        }
    }

    fn push_hit(&mut self, rect: Rect, action: Action) -> bool {
        let hovered = rect.contains(self.mouse);
        self.hits.push(Hit { rect, action });
        hovered
    }

    /// Register a right-click context target for a track row. Matched only on
    /// a right mouse press (left-clicks still play the row).
    fn push_row_menu(&mut self, rect: Rect, action: Action) {
        self.row_menu_hits.push(Hit { rect, action });
    }

    /// Topmost right-click target containing `p` (later-drawn = on top), and
    /// the row it belongs to.
    fn row_menu_at(&self, p: Point) -> Option<(Rect, Action)> {
        self.row_menu_hits
            .iter()
            .rev()
            .find(|h| h.rect.contains(p))
            .map(|h| (h.rect, h.action.clone()))
    }

    // ---- adaptive sidebar geometry (delegated to `SidebarDrawer`) --------

    /// The width-driven sidebar layout (docked vs overlay). Computed straight
    /// from the allocated width so it's valid even before the first `resolve`.
    #[cfg(test)]
    fn sidebar_layout(&self) -> SplitLayout {
        self.drawer.layout_for(self.rect.width())
    }

    /// The width the content column is inset by on the left for the docked
    /// sidebar (0 while overlaying — the flap floats over the content). Read
    /// from the drawer's last-resolved geometry.
    #[cfg(test)]
    fn content_inset(&self) -> f64 {
        (self.drawer.content_rect().x0 - self.rect.x0).max(0.0)
    }

    /// The sidebar's on-screen rect (from the drawer's last `resolve`). Spans
    /// the full window height; the now-playing transport is inset to its right
    /// (see `nowplaying_rect`) so the two never overlap.
    fn sidebar_rect(&self) -> Rect {
        self.drawer.sidebar_rect()
    }

    /// The now-playing transport's on-screen rect: it starts at the content
    /// column's left edge (right of the docked sidebar; full-width in overlay,
    /// where the floating sidebar draws above it) and spans to the right edge.
    fn nowplaying_rect(&self) -> Rect {
        let r = self.rect;
        Rect::new(self.drawer.content_rect().x0, r.y1 - self.bar_h, r.x1, r.y1)
    }

    /// Whether the sidebar toggle sits at the content's leading edge rather
    /// than in the sidebar's own header: whenever the sidebar is hidden.
    fn toggle_in_content(&self) -> bool {
        !self.drawer.is_open()
    }

    /// Flip the sidebar open/closed for the current layout.
    fn toggle_sidebar(&mut self) {
        self.drawer.toggle();
        self.dirty = true;
    }

    /// Stage the sidebar for a deterministic `--shots` frame (no event loop to
    /// drive the reveal animation). `open` snaps it fully revealed.
    pub fn snap_sidebar(&mut self, open: bool) {
        self.staged_sidebar = Some(open);
    }

    /// Stage a track context menu open for a `--shots` frame. Deferred to the
    /// first `sync` (the catalog is empty at construction); see `apply_shot_stage`.
    pub fn snap_track_menu(&mut self) {
        self.shot_stage = Some(ShotStage::TrackMenu);
        // Default the shot to the Songs library so the menu has rows behind it.
        if matches!(self.view, View::Home) {
            self.view = View::Songs;
        }
    }

    /// Stage the inline create-playlist name prompt open for a `--shots` frame.
    pub fn snap_new_playlist_prompt(&mut self) {
        self.shot_stage = Some(ShotStage::NewPlaylistPrompt);
        if matches!(self.view, View::Home) {
            self.view = View::Playlists;
        }
    }

    /// Open `view` for a `--shots` frame.
    pub fn snap_view(&mut self, view: View) {
        self.view = view;
    }

    /// Stage the Home skeleton loading state for a `--shots` frame.
    pub fn snap_skeleton(&mut self) {
        self.shot_stage = Some(ShotStage::Skeleton);
        self.view = View::Home;
    }

    /// Stage a bundled track playing for a now-playing `--shots` frame (cover
    /// art in the transport bar + a live equalizer on the current row).
    pub fn snap_nowplaying(&mut self) {
        self.shot_stage = Some(ShotStage::NowPlaying);
        self.view = View::Songs;
    }

    /// Stage a playing track on a narrow window with the transport's More
    /// menu open (shuffle, repeat, like, lyrics, queue).
    pub fn snap_player_menu(&mut self) {
        self.snap_nowplaying();
        self.staged_player_menu = true;
    }

    /// Stage the Queue view with a real "Next up" list: a bundled track plays
    /// (so the transport bar is live) and the full sample queue trails it.
    pub fn snap_queue(&mut self) {
        self.shot_stage = Some(ShotStage::NowPlaying);
        self.view = View::Queue;
    }

    /// Stage the synced-lyrics view with a highlighted current line.
    pub fn snap_lyrics(&mut self) {
        self.shot_stage = Some(ShotStage::Lyrics);
        self.view = View::Lyrics;
    }

    /// Stage a track list with in-flight offline downloads for a `--shots`
    /// frame: a few rows show the animated download spinner at different
    /// progress, alongside the idle download affordance on the rest.
    pub fn snap_downloading(&mut self) {
        self.shot_stage = Some(ShotStage::Downloading);
        self.view = View::Downloads;
    }

    /// Stage a populated, durable cache without touching the real device cache.
    pub fn snap_offline_cached(&mut self) {
        self.shot_stage = Some(ShotStage::OfflineCached);
        self.view = View::Downloads;
    }

    /// Stage a failed local download and its explicit retry control.
    pub fn snap_offline_error(&mut self) {
        self.shot_stage = Some(ShotStage::OfflineError);
        self.view = View::Downloads;
    }

    /// Stage a full (live-style) artist page for a hermetic `--shots` frame:
    /// header + popular tracks + sectioned discography + related artists + bio.
    pub fn snap_artist_page(&mut self) {
        let page = ArtistPage {
            id: "ar_aurora".into(),
            name: "Aurora Skies".into(),
            subscriber_count: 2_140_553,
            description: "Aurora Skies is a dream-pop project known for washes of \
                reverbed guitar, glassy synth pads and hushed vocals. Across two \
                records the band has built a nocturnal, cinematic sound that pairs \
                slow-building crescendos with intimate, close-mic'd songwriting."
                .into(),
            wikipedia_url: "https://en.wikipedia.org/wiki/Aurora_Skies".into(),
            header_url: String::new(),
            tracks: Self::snap_artist_page_tracks(),
            albums: Self::snap_artist_page_albums(),
            similar: Self::snap_artist_page_similar(),
        };
        self.absorb_artist(&page);
        self.artist_pages.insert(page.id.clone(), page);
        self.view = View::Artist("ar_aurora".into());
    }

    fn snap_artist_page_tracks() -> Vec<TrackHit> {
        vec![
            sample_hit(
                "aur_g1",
                "Glasslight",
                "Aurora Skies",
                "ar_aurora",
                "al_glasslight",
                "Glasslight",
                215,
            ),
            sample_hit(
                "aur_p1",
                "Paper Boats",
                "Aurora Skies",
                "ar_aurora",
                "al_paperboats",
                "Paper Boats",
                221,
            ),
            sample_hit(
                "aur_g5",
                "Halcyon",
                "Aurora Skies",
                "ar_aurora",
                "al_glasslight",
                "Glasslight",
                263,
            ),
            sample_hit(
                "aur_g3",
                "Slow Tide",
                "Aurora Skies",
                "ar_aurora",
                "al_glasslight",
                "Glasslight",
                242,
            ),
            sample_hit(
                "aur_p3",
                "Lighthouse",
                "Aurora Skies",
                "ar_aurora",
                "al_paperboats",
                "Paper Boats",
                234,
            ),
        ]
    }

    fn snap_artist_page_albums() -> Vec<crate::backend::ArtistAlbum> {
        vec![
            crate::backend::ArtistAlbum {
                id: "al_glasslight".into(),
                title: "Glasslight".into(),
                year: 2023,
                kind: crate::backend::AlbumKind::Album,
                cover_url: String::new(),
            },
            crate::backend::ArtistAlbum {
                id: "al_paperboats".into(),
                title: "Paper Boats".into(),
                year: 2021,
                kind: crate::backend::AlbumKind::Album,
                cover_url: String::new(),
            },
            crate::backend::ArtistAlbum {
                id: "al_halcyon_single".into(),
                title: "Halcyon".into(),
                year: 2024,
                kind: crate::backend::AlbumKind::Single,
                cover_url: String::new(),
            },
            crate::backend::ArtistAlbum {
                id: "al_tides_ep".into(),
                title: "Tides EP".into(),
                year: 2022,
                kind: crate::backend::AlbumKind::Ep,
                cover_url: String::new(),
            },
        ]
    }

    fn snap_artist_page_similar() -> Vec<ArtistHit> {
        vec![
            ArtistHit {
                id: "ar_midnight".into(),
                name: "Midnight Transit".into(),
                subscriber_count: 884_201,
                avatar_url: String::new(),
            },
            ArtistHit {
                id: "ar_lumen".into(),
                name: "Lumen Theory".into(),
                subscriber_count: 3_771_440,
                avatar_url: String::new(),
            },
            ArtistHit {
                id: "ar_neon".into(),
                name: "Neon Harbor".into(),
                subscriber_count: 1_009_330,
                avatar_url: String::new(),
            },
            ArtistHit {
                id: "ar_kavi".into(),
                name: "Kavi & The Tide".into(),
                subscriber_count: 1_502_998,
                avatar_url: String::new(),
            },
        ]
    }

    /// Stage a full (live-style) album page for a hermetic `--shots` frame:
    /// cover + header (play/shuffle/save) + the ordered track list.
    pub fn snap_album_page(&mut self) {
        let page = AlbumPage {
            id: "al_glasslight".into(),
            title: "Glasslight".into(),
            artist_id: "ar_aurora".into(),
            artist_name: "Aurora Skies".into(),
            year: 2023,
            cover_url: String::new(),
            tracks: vec![
                sample_hit(
                    "aur_g1",
                    "Glasslight",
                    "Aurora Skies",
                    "ar_aurora",
                    "al_glasslight",
                    "Glasslight",
                    215,
                ),
                sample_hit(
                    "aur_g2",
                    "Paper Lanterns",
                    "Aurora Skies",
                    "ar_aurora",
                    "al_glasslight",
                    "Glasslight",
                    198,
                ),
                sample_hit(
                    "aur_g3",
                    "Slow Tide",
                    "Aurora Skies",
                    "ar_aurora",
                    "al_glasslight",
                    "Glasslight",
                    242,
                ),
                sample_hit(
                    "aur_g4",
                    "Ghost in the Frame",
                    "Aurora Skies",
                    "ar_aurora",
                    "al_glasslight",
                    "Glasslight",
                    187,
                ),
                sample_hit(
                    "aur_g5",
                    "Halcyon",
                    "Aurora Skies",
                    "ar_aurora",
                    "al_glasslight",
                    "Glasslight",
                    263,
                ),
            ],
        };
        self.absorb_album(&page);
        self.album_pages.insert(page.id.clone(), page);
        self.view = View::Album("al_glasslight".into());
    }

    /// Stage the failure state a real unavailable live artist detail uses. This
    /// keeps screenshot/a11y review honest: an error must settle with a clear
    /// recovery message rather than look like a loading frame forever.
    pub fn snap_error_page(&mut self) {
        let id = "UC_music_shot_unavailable".to_string();
        self.failed_pages.insert(id.clone());
        self.view = View::Artist(id);
    }

    /// Stage a real canonical membership-open operation for `--shots`. The
    /// post-activation frame shows the target row in its owning playlist.
    pub fn snap_membership_open(&mut self) {
        self.shot_stage = Some(ShotStage::MembershipOpen);
    }

    /// Start a bundled track playing at `pos_ms` for a deterministic shot.
    /// Prefers `tk_g1` (it carries bundled synced lyrics).
    fn stage_play_bundled(&mut self, pos_ms: f64) {
        let ids: Vec<String> = self
            .lib
            .tracks_sorted()
            .iter()
            .map(|t| t.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        let idx = ids.iter().position(|id| id == "tk_g1").unwrap_or(0);
        self.pb.queue = ids;
        self.pb.queue_index = idx;
        self.pb.current = self.pb.queue.get(idx).cloned();
        self.pb.playing = true;
        self.playhead_ms = pos_ms;
        self.bound_track = self.pb.current.clone();
    }

    /// Apply a deferred `--shots` staging action now that the library is
    /// loaded. Runs once; clears itself.
    fn apply_shot_stage(&mut self) {
        let Some(stage) = self.shot_stage.take() else {
            return;
        };
        match stage {
            ShotStage::TrackMenu => {
                // Target the 3rd sorted track; the menu opens over its row once
                // the row has been laid out (see `open_staged_track_menu`).
                let tracks = self.lib.tracks_sorted();
                self.staged_menu_track = tracks
                    .get(2)
                    .or_else(|| tracks.first())
                    .map(|t| t.id.clone());
            }
            ShotStage::NewPlaylistPrompt => {
                self.name_prompt = Some(NamePrompt::NewPlaylist);
                self.name_input = "Late Night Coding".to_string();
                self.name_dialog = None;
            }
            ShotStage::Skeleton => {
                // Force the loading state offline so the shimmer skeleton renders
                // hermetically (no network fetch actually runs).
                self.home_requested = true;
                self.home = None;
            }
            ShotStage::NowPlaying => {
                self.stage_play_bundled(48_000.0);
            }
            ShotStage::Downloading => {
                for (id, progress) in [("tk_g1", 0.32), ("tk_g2", 0.71)] {
                    self.update_device_cache(id, CacheState::Downloading(progress));
                }
            }
            ShotStage::OfflineCached => {
                for id in ["tk_g1", "tk_g2"] {
                    self.update_device_cache(id, CacheState::Cached);
                }
            }
            ShotStage::OfflineError => {
                self.offline_errors.insert(
                    "tk_g1".into(),
                    "Network connection ended before the download finished".into(),
                );
            }
            ShotStage::Lyrics => {
                // ~40s in lands mid-song so a synced line is highlighted.
                self.stage_play_bundled(40_000.0);
            }
            ShotStage::MembershipOpen => {
                let Some((playlist, track)) = self.lib.playlists.iter().find_map(|playlist| {
                    playlist
                        .track_ids
                        .last()
                        .map(|track| (playlist.id.clone(), track.clone()))
                }) else {
                    return;
                };
                let operation =
                    crate::actions::MusicOperation::OpenPlaylistMembership { playlist, track };
                let Ok(payload) = serde_json::to_vec(&operation) else {
                    return;
                };
                // This takes the identical operation-decoding/validation path
                // a Ctrl+K result takes after its canonical resource action
                // resolves at the authority door.
                let _ = self.answer_resource_operation(
                    tilo_actions::OperationKind::View,
                    crate::actions::MUSIC_OPERATION_TYPE,
                    &payload,
                    &AutoCommit::new(),
                    &automerge::ROOT,
                );
            }
        }
    }

    // ---- context menu -----------------------------------------------------

    /// Push an action row onto the open menu, recording its parallel `Action`.
    fn push_menu(&mut self, label: impl Into<String>, action: Action) {
        let idx = self.menu_actions.len() as i64;
        let pending = self.pending.clone();
        self.context.push(MenuItem::action(label, move || {
            pending.store(idx, Ordering::SeqCst);
        }));
        self.menu_actions.push(action);
    }

    /// Build + open the per-track context menu at the cursor.
    fn open_track_menu(
        &mut self,
        track: String,
        in_playlist: Option<&str>,
        queue_index: Option<usize>,
    ) {
        self.context = ContextMenu::new();
        self.menu_actions.clear();
        self.ctx_track = Some(track.clone());
        self.ctx_in_playlist = in_playlist.map(ToString::to_string);
        self.ctx_queue_index = queue_index;

        let liked = self.lib.is_liked(&track);
        let album_id = self.lib.track(&track).map(|t| t.album_id.clone());
        let artist_id = self.lib.track(&track).map(|t| t.artist_id.clone());

        self.push_menu(
            "Play",
            Action::Play {
                queue: vec![track.clone()].into(),
                index: 0,
            },
        );
        self.push_menu("Play next", Action::PlayNext(track.clone()));
        self.push_menu("Add to queue", Action::AddToQueue(track.clone()));
        if self.lib.tracks.contains_key(&track) {
            self.context.push(MenuItem::separator());
            self.menu_actions.push(Action::Back); // separator placeholder, never fired
            self.push_menu(
                "Add to playlist\u{2026}",
                Action::OpenAddToPlaylist(track.clone()),
            );
            if let Some(pl) = in_playlist {
                self.push_menu(
                    "Remove from this playlist",
                    Action::RemoveFromPlaylist {
                        playlist: pl.to_string(),
                        track: track.clone(),
                    },
                );
            }
            if let Some(qi) = queue_index {
                self.push_menu("Remove from queue", Action::RemoveFromQueue(qi));
            }
            self.context.push(MenuItem::separator());
            self.menu_actions.push(Action::Back);
            if let Some(al) = album_id {
                self.push_menu("Go to album", Action::Nav(View::Album(al)));
            }
            if let Some(ar) = artist_id {
                self.push_menu("Go to artist", Action::Nav(View::Artist(ar)));
            }
            self.push_menu(
                if liked { "Unlike" } else { "Like" },
                Action::ToggleLike(track),
            );
        } else if let Some(qi) = queue_index {
            self.push_menu("Remove from queue", Action::RemoveFromQueue(qi));
        }
        self.open_context_menu_at_pointer();
    }

    /// A menu row that shows whether its setting is on.
    fn push_checked_menu(&mut self, label: impl Into<String>, checked: bool, action: Action) {
        let idx = self.menu_actions.len() as i64;
        let pending = self.pending.clone();
        self.context.push(
            MenuItem::action(label, move || {
                pending.store(idx, Ordering::SeqCst);
            })
            .with_checked(checked),
        );
        self.menu_actions.push(action);
    }

    /// The narrow transport's More menu: shuffle, repeat, like, lyrics and
    /// the queue, each showing whether it is on. Opens beside `anchor`.
    fn open_player_menu(&mut self, anchor: Rect) {
        self.context = ContextMenu::new();
        self.menu_actions.clear();
        self.ctx_track = None;
        self.ctx_in_playlist = None;
        self.ctx_queue_index = None;
        self.menu_anchor_row = None;
        let idle = self.pb.current.is_none() && self.pb.queue.is_empty();
        self.push_checked_menu("Shuffle", self.pb.shuffle, Action::ToggleShuffle);
        self.push_checked_menu(
            "Repeat all",
            self.pb.repeat == RepeatMode::All,
            Action::SetRepeat(RepeatMode::All),
        );
        self.push_checked_menu(
            "Repeat one",
            self.pb.repeat == RepeatMode::One,
            Action::SetRepeat(RepeatMode::One),
        );
        if idle {
            for index in 0..3 {
                self.context.set_item_enabled(index, false);
            }
        }
        self.context.push(MenuItem::separator());
        self.menu_actions.push(Action::None);
        if let Some(track) = self.pb.current.clone()
            && !self.is_local_file(&track)
        {
            let liked = self.lib.is_liked(&track);
            self.push_menu(
                if liked { "Unlike" } else { "Like" },
                Action::ToggleLike(track),
            );
        }
        self.push_checked_menu(
            "Lyrics",
            self.view == View::Lyrics,
            Action::Nav(View::Lyrics),
        );
        self.push_checked_menu("Queue", self.view == View::Queue, Action::ToggleQueue);
        self.context.set_text_scale(self.context_text_scale);
        self.context.open_anchored(anchor, self.rect);
        self.dirty = true;
    }

    /// Build + open the "Add to playlist…" secondary picker.
    fn open_add_to_playlist_menu(&mut self, track: &str) {
        self.context = ContextMenu::new();
        self.menu_actions.clear();
        self.ctx_track = Some(track.to_string());

        self.push_menu(
            "New playlist\u{2026}",
            Action::BeginNewPlaylistFor(track.to_string()),
        );
        if !self.lib.playlists.is_empty() {
            self.context.push(MenuItem::separator());
            self.menu_actions.push(Action::Back);
        }
        let playlists: Vec<(String, String)> = self
            .lib
            .playlists
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        for (id, name) in playlists {
            self.push_menu(
                name,
                Action::AddToPlaylist {
                    playlist: id,
                    track: track.to_string(),
                },
            );
        }

        self.open_context_menu_at_pointer();
    }

    /// Open the populated context menu at the pointer, bounded by the app rect.
    ///
    /// `ContextMenu::open_anchored` owns the geometry: it measures its rows with
    /// the frame's renderer and theme when it paints, flips above the anchor
    /// when there is no room below, and clamps to the viewport. The previous
    /// hand-rolled nudge guessed `220 × (rows·28 + 24)` and laid the menu out
    /// against `Theme::dark()` regardless of the live theme, so its hit rows
    /// could disagree with the painted ones at any other density or scale.
    fn open_context_menu_at_pointer(&mut self) {
        let anchor = Rect::from_origin_size(self.mouse, (0.0, 0.0));
        if let Some(row) = self.menu_anchor_row {
            self.context.anchor_row(row);
        }
        self.context.set_text_scale(self.context_text_scale);
        self.context.open_anchored(anchor, self.rect);
        self.dirty = true;
    }

    /// Keep the imperatively owned shared menu aligned with the current
    /// system accessibility geometry even while it is closed.
    fn sync_context_menu_geometry(&mut self, theme: &Theme) {
        self.context_text_scale = theme.text_scale;
        self.context.set_text_scale(self.context_text_scale);
        self.context.set_viewport(Some(self.rect));
    }

    /// Drain a context-menu selection (set by a row callback) and dispatch the
    /// parallel [`Action`]. Returns whether something was handled.
    fn drain_menu_selection(&mut self, state: &mut AutoCommit) -> bool {
        let idx = self.pending.swap(-1, Ordering::SeqCst);
        if idx >= 0 {
            if let Some(action) = self.menu_actions.get(idx as usize).cloned() {
                self.dispatch(action, state);
            }
            return true;
        }
        false
    }

    // ---- lyrics -----------------------------------------------------------

    /// Resolve lyrics for the current track when the bound track changes, via
    /// the synchronous bundled/gated resolver. Cached by track id so the
    /// lyrics view doesn't re-resolve every frame. Live fetch runs on the
    /// async worker (gated behind the `lyrics` feature); the bundled path is
    /// always available for `--shots` + offline.
    fn ensure_lyrics(&mut self) {
        let cur = self.pb.current.clone();
        if cur == self.lyrics_for {
            return;
        }
        self.lyrics_for.clone_from(&cur);
        self.lyrics.clear();
        self.lyrics_synced = false;
        self.lyrics_pending_for = None;
        let Some(id) = cur else { return };
        // Already resolved (revisited track, or prefetched) — use the cache.
        if let Some((lines, synced)) = self.lyrics_cache.get(&id) {
            self.lyrics.clone_from(lines);
            self.lyrics_synced = *synced;
            return;
        }
        let Some(track) = self.lib.track(&id).cloned() else {
            return;
        };
        // Bundled sample lyrics (seed tracks) resolve instantly + offline.
        if let Some(l) = lyrics::bundled::for_track(&id) {
            self.lyrics_cache
                .insert(id.clone(), (l.lines.clone(), l.synced));
            self.lyrics = l.lines;
            self.lyrics_synced = l.synced;
            return;
        }
        // Otherwise fetch the real synced lyrics from LRCLIB off-thread.
        if !self.allow_network {
            return;
        }
        let meta = TrackMeta {
            track_id: id.clone(),
            title: track.title.clone(),
            artist: self.lib.artist_name(&track.artist_id).to_string(),
            album: self.lib.album_title(&track.album_id).to_string(),
            duration_s: track.duration_s,
        };
        let w = self.lyrics_worker.get_or_insert_with(LyricsWorker::new);
        w.request(meta);
        self.lyrics_pending_for = Some(id);
    }

    /// Adopt any lyrics the async LRCLIB worker resolved for the current track.
    fn poll_lyrics(&mut self) {
        let mut got = Vec::new();
        if let Some(w) = &self.lyrics_worker {
            while let Some(r) = w.poll() {
                got.push(r);
            }
        }
        for (id, l) in got {
            // Cache every resolved result (current OR prefetched next track).
            self.lyrics_cache
                .insert(id.clone(), (l.lines.clone(), l.synced));
            if self.pb.current.as_deref() == Some(id.as_str()) {
                self.lyrics = l.lines;
                self.lyrics_synced = l.synced;
                self.lyrics_for = Some(id.clone());
            }
            if self.lyrics_pending_for.as_deref() == Some(id.as_str()) {
                self.lyrics_pending_for = None;
            }
        }
    }

    /// The next track in the play order (respecting shuffle), if any — the
    /// prefetch target.
    fn next_queued_track(&self) -> Option<String> {
        if self.pb.queue.is_empty() {
            return None;
        }
        if self.pb.shuffle && !self.shuffle_order.is_empty() {
            let pos = self
                .shuffle_order
                .iter()
                .position(|&i| i == self.pb.queue_index)?;
            let ni = *self.shuffle_order.get(pos + 1)?;
            return self.pb.queue.get(ni).cloned();
        }
        self.pb.queue.get(self.pb.queue_index + 1).cloned()
    }

    /// Prefetch on a **stable (non-metered)** connection: warm the next queued
    /// song's lyrics + its album metadata so the transition is instant. Runs at
    /// most once per current-track change and never on a metered link (or with
    /// networking disabled for a headless render). Returns whether it ran.
    fn maybe_prefetch(&mut self) -> bool {
        let stable = self
            .force_metered
            .map_or_else(crate::net::connection_is_stable, |m| !m);
        if !self.allow_network || !stable {
            return false;
        }
        let Some(cur) = self.pb.current.clone() else {
            return false;
        };
        if self.prefetched_for.as_deref() == Some(cur.as_str()) {
            return false;
        }
        self.prefetched_for = Some(cur);

        let Some(next) = self.next_queued_track() else {
            return false;
        };
        // Prefetch lyrics for the next track (into the shared cache) unless we
        // already have them.
        if !self.lyrics_cache.contains_key(&next)
            && self.lyrics_pending_for.as_deref() != Some(next.as_str())
            && let Some(track) = self.lib.track(&next).cloned()
        {
            if let Some(l) = lyrics::bundled::for_track(&next) {
                self.lyrics_cache.insert(next.clone(), (l.lines, l.synced));
            } else {
                let meta = TrackMeta {
                    track_id: next.clone(),
                    title: track.title.clone(),
                    artist: self.lib.artist_name(&track.artist_id).to_string(),
                    album: self.lib.album_title(&track.album_id).to_string(),
                    duration_s: track.duration_s,
                };
                self.lyrics_worker
                    .get_or_insert_with(LyricsWorker::new)
                    .request(meta);
            }
            // Warm the next track's album metadata + cover.
            let album_id = track.album_id;
            self.ensure_album(&album_id);
        }
        true
    }

    /// Debounced live search: fire a real YouTube-Music query a short beat after
    /// the user stops typing, so keystrokes stay smooth and we don't spam the
    /// API. Enter still fires immediately (via [`Self::request_live_search`]).
    fn maybe_dispatch_live_search(&mut self) {
        const DEBOUNCE_MS: u128 = 350;
        let Some(edited) = self.search_edit_at else {
            return;
        };
        if edited.elapsed().as_millis() < DEBOUNCE_MS {
            return;
        }
        self.search_edit_at = None;
        let q = self.search_query.trim().to_string();
        if q.len() < 2 || self.live_query_sent.as_deref() == Some(q.as_str()) {
            return;
        }
        self.request_live_search();
    }
}

impl Default for MusicApp {
    fn default() -> Self {
        Self::new()
    }
}

// ---- small drawing helpers ----------------------------------------------

/// Truncate `text` to fit `max_w` px in `style`, appending an ellipsis.
///
/// Only appends the ellipsis when at least one real glyph fits before it. When
/// the column is too narrow for even a single character, this renders **nothing**
/// (an empty string) rather than a lone `\u{2026}`: a stray ellipsis with no
/// preceding text conveys nothing and, in a too-narrow column, would spill past
/// its bounds into the neighbouring UI.
fn elide(tr: &mut TextRenderer, text: &str, style: &TextStyle, max_w: f64) -> String {
    let out = tr.elide_to_width_style(text, ui::num::f32_of_f64(max_w.max(0.0)), style);
    if out != text && out.starts_with(ui::text_render::ELLIPSIS) {
        // Not even one glyph + ellipsis fits — render nothing so the `\u{2026}`
        // never overflows into an adjacent column.
        String::new()
    } else {
        out
    }
}

/// Library-id prefix marking a track that came from a live YouTube-Music
/// search (its `yt_id` is a real video id we can resolve a stream from). Sample
/// tracks use `tk_…` ids and never match.
const LIVE_TRACK_PREFIX: &str = "yt:";

/// Whether a track id refers to a real (streamable) YouTube-Music track.
fn is_live_track(track_id: &str) -> bool {
    track_id.starts_with(LIVE_TRACK_PREFIX)
}

/// Whether an id looks like a real `YouTube` channel id (artist page fetchable).
fn is_live_id(id: &str) -> bool {
    id.starts_with("UC")
}

/// Whether an id looks like a real YouTube-Music album/playlist browse id.
fn is_live_album_id(id: &str) -> bool {
    id.starts_with("MPRE") || id.starts_with("OLAK")
}

/// Whether a playlist id refers to a real (live YouTube-Music) playlist rather
/// than one of the app's own playlists. Local playlists are minted with a `pl_`
/// prefix (incl. the virtual `pl_liked`); everything else is a live YTM id
/// (`PL…`, `RDCLAK…`, `VL…`).
fn is_live_playlist_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with("pl_")
}

/// Build a [`TrackHit`] for a hermetic shot from sample-library-shaped fields.
fn sample_hit(
    video_id: &str,
    title: &str,
    artist: &str,
    artist_id: &str,
    album_id: &str,
    album: &str,
    dur: u32,
) -> TrackHit {
    TrackHit {
        video_id: video_id.into(),
        title: title.into(),
        artist: artist.into(),
        artist_id: Some(artist_id.into()),
        album: Some(album.into()),
        album_id: Some(album_id.into()),
        duration_s: dur,
        cover_url: String::new(),
    }
}

/// Convert a Home track tile into a [`TrackHit`] so it flows through the same
/// live-track merge path as a search hit.
fn home_item_to_hit(item: &HomeItem) -> TrackHit {
    let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
    TrackHit {
        video_id: item.video_id.clone(),
        title: item.title.clone(),
        artist: item.artist_name.clone(),
        artist_id: opt(&item.artist_id),
        album: opt(&item.album_title),
        album_id: opt(&item.album_id),
        duration_s: item.duration_s,
        cover_url: item.cover_url.clone(),
    }
}

/// A wall-clock-derived seed for the shuffle PRNG, so each session's shuffle
/// order differs. Never zero (the xorshift step guards against it anyway).
fn seed_rng() -> u64 {
    use ui::time::UNIX_EPOCH;
    let nanos = ui::time::wall()
        .duration_since(UNIX_EPOCH)
        .map_or(0x1234_5678, |d| d.as_nanos() as u64);
    nanos ^ 0x9E37_79B9_7F4A_7C15
}

/// Lowercase-alnum slug for synthesising a stable id when the source omits one.
fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// Convert a real search hit into a library-mergeable [`LiveTrack`], minting
/// stable ids where `YouTube` didn't supply them.
fn hit_to_live(hit: &TrackHit) -> LiveTrack {
    let artist_id = hit
        .artist_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("yta:{}", slug(&hit.artist)));
    let album_title = hit.album.clone().unwrap_or_else(|| "Single".to_string());
    let album_id = hit
        .album_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("ytal:{}_{}", slug(&hit.artist), slug(&album_title)));
    LiveTrack {
        track_id: format!("{LIVE_TRACK_PREFIX}{}", hit.video_id),
        title: hit.title.clone(),
        artist_id,
        artist_name: hit.artist.clone(),
        album_id,
        album_title,
        duration_s: hit.duration_s,
        yt_id: hit.video_id.clone(),
    }
}

/// Keep source reads bounded before handing an image decoder potentially
/// untrusted portal data. The resulting shared payload has the tighter action
/// budget in [`crate::actions::MAX_PORTABLE_COVER_BYTES`].
const MAX_COVER_SOURCE_BYTES: u64 = 8 * 1024 * 1024;

/// Decode a portal-selected image and normalize it into a small PNG data URL.
/// This pure helper lets tests exercise the same path without a real portal.
fn portable_cover_data_url(source: &[u8]) -> Result<String, String> {
    if source.is_empty() || source.len() as u64 > MAX_COVER_SOURCE_BYTES {
        return Err("image is empty or larger than 8 MiB".to_string());
    }
    // The shared preview decoder refuses a header declaring an oversized image
    // before it allocates a pixel, and applies the EXIF orientation.
    let cover = thumbnail::image_thumb(source, crate::actions::MAX_PORTABLE_COVER_EDGE)
        .ok_or_else(|| "image could not be decoded".to_string())?;
    let png =
        thumbnail::encode_png(&cover).ok_or_else(|| "image could not be encoded".to_string())?;
    if png.len() > crate::actions::MAX_PORTABLE_COVER_BYTES {
        return Err("normalized image is larger than 256 KiB".to_string());
    }
    let value = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    );
    if !crate::actions::portable_cover_value(&value) {
        return Err("normalized image did not meet the portable cover policy".to_string());
    }
    Ok(value)
}

fn portable_cover_path(path: &Path) -> Result<String, String> {
    let metadata = crate::files::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len > MAX_COVER_SOURCE_BYTES {
        return Err("image is larger than 8 MiB".to_string());
    }
    let source = crate::files::read(path).map_err(|error| error.to_string())?;
    portable_cover_data_url(&source)
}

/// Run local disk I/O and decode/resize/encode work away from the UI thread.
fn embed_cover_off_thread(path: PathBuf) -> Receiver<Result<String, String>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    if let Err(error) = ui::rt::spawn_thread("music-cover-embed", move || {
        let _ = sender.send(portable_cover_path(&path));
    }) {
        let (fallback_sender, fallback_receiver) = std::sync::mpsc::channel();
        let _ = fallback_sender.send(Err(error.to_string()));
        return fallback_receiver;
    }
    receiver
}

/// Decode only canonical embedded cover values. Document actions reject local
/// paths and `file:` URLs, so the renderer never dereferences peer-local files.
fn embedded_cover_bytes(url: &str) -> Option<Vec<u8>> {
    if !crate::actions::portable_cover_value(url) {
        return None;
    }
    let encoded = url.strip_prefix("data:image/png;base64,")?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}

/// The transport the [`ImageStore`] uses to fetch cover bytes. Real HTTP under
/// the `live` build: a shared `tilo_net` client on the app runtime, which the
/// store's loader waits on from its own worker; embedded portable PNG values
/// render in every build, and the hermetic `--shots` path never opens a socket.
#[cfg(feature = "live")]
fn image_fetcher() -> ui::Fetcher {
    use std::sync::OnceLock;
    const MAX_COVER_BYTES: usize = 8 * 1024 * 1024;
    static CLIENT: OnceLock<Option<tilo_net::Client>> = OnceLock::new();
    Arc::new(|url: &str| {
        if let Some(bytes) = embedded_cover_bytes(url) {
            return Some(bytes);
        }
        if !url.starts_with("https://") {
            return None;
        }
        let client = CLIENT
            .get_or_init(|| {
                tilo_net::Client::builder()
                    .timeout(std::time::Duration::from_secs(12))
                    .user_agent("Mozilla/5.0 (Tilo Music)")
                    .build()
                    .ok()
            })
            .clone()?;
        let url = url.to_owned();
        // The body is read through a hard cap, so a lying or absent
        // Content-Length cannot turn cover art into an unbounded allocation.
        let bytes = ui::rt::app()
            .block_on(async move {
                let resp = client.get(url).send().await.ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                resp.bytes_limited(MAX_COVER_BYTES).await.ok()
            })
            .ok()??;
        (!bytes.is_empty()).then(|| bytes.to_vec())
    })
}

#[cfg(not(feature = "live"))]
fn image_fetcher() -> ui::Fetcher {
    // No HTTP stack, but a portal-picked portable PNG still renders offline.
    Arc::new(embedded_cover_bytes)
}

mod local_files;
mod render;

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests;
