//! Root widget for the Music app.
//!
//! The whole UI is drawn directly into the Vello scene (nav rail, library
//! grids, track lists, detail pages, search and the now-playing transport
//! bar), so the app owns its layout math and hit-testing rather than
//! composing stock taffy widgets. Authoritative state lives in the Automerge
//! document via [`crate::doc`]; `sync` refreshes the in-memory render
//! snapshot each frame and every mutation writes back to the document so the
//! library + play-state replicate across devices.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use std::cell::Cell;
use std::rc::Rc;
use ui::ImageStore;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{ModalDialog, PromptBody, SidebarDrawer, SplitLayout, TextInput, Widget};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::art;
use crate::audio::AudioEngine;
use crate::backend::{
    AlbumPage, ArtistHit, ArtistPage, HomeContent, HomeItem, HomeItemKind, HomeSection,
    PlaylistHit, PlaylistPage, SearchBundle, SearchKind, SearchTop, TrackHit,
};
use crate::doc::{self, LiveTrack};
use crate::icon::{self, Icon};
use crate::lyrics::{self, LyricsLine, LyricsWorker, TrackMeta};
use crate::model::{
    Album, Artist, CacheState, Library, Playback, RepeatMode, SearchResults, Track,
};
use crate::mpris::{MprisCommand, MprisHandle, PlaybackStatus, PlayerSnapshot};
use crate::seed;
use crate::source::Source;
use crate::stream::{StreamRequest, StreamUpdate, StreamWorker};

const SIDEBAR_W: f64 = 236.0;
const NOWPLAYING_H: f64 = 92.0;
const HEADER_H: f64 = 72.0;

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

/// Below this allocated width (logical px) the navigation sidebar stops
/// sitting permanently beside the content and becomes an overlay flap that
/// slides in over it behind a dimming scrim. Chosen so the sidebar plus a
/// readable two-column content area (album/track grids) stop fitting side by
/// side.
const SIDEBAR_BREAKPOINT: f64 = 760.0;

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
    Search,
    Queue,
    Lyrics,
    Album(String),
    Artist(String),
    Playlist(String),
}

/// The Spotify-style filter tabs across the top of the search results. `All`
/// shows a curated mix (top-result hero + a few of each type); the others show
/// just that type in a fuller layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchTab {
    All,
    Songs,
    Albums,
    Artists,
    Playlists,
}

impl SearchTab {
    pub const ALL: [SearchTab; 5] = [
        SearchTab::All,
        SearchTab::Songs,
        SearchTab::Albums,
        SearchTab::Artists,
        SearchTab::Playlists,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SearchTab::All => "All",
            SearchTab::Songs => "Songs",
            SearchTab::Albums => "Albums",
            SearchTab::Artists => "Artists",
            SearchTab::Playlists => "Playlists",
        }
    }

    /// The deep per-type fetch a tab drives (none for the mixed `All` tab).
    pub fn kind(self) -> Option<SearchKind> {
        match self {
            SearchTab::All => None,
            SearchTab::Songs => Some(SearchKind::Songs),
            SearchTab::Albums => Some(SearchKind::Albums),
            SearchTab::Artists => Some(SearchKind::Artists),
            SearchTab::Playlists => Some(SearchKind::Playlists),
        }
    }
}

/// A click action captured during draw and dispatched on the next mouse
/// press. Owned data only, since the hit list is rebuilt every frame.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Nav(View),
    Back,
    /// Open the Search screen filtered to a genre — the "Browse all" category
    /// chips route here so a tap actually navigates + filters.
    BrowseGenre(String),
    /// Play `queue[index]` after replacing the queue.
    Play {
        queue: Vec<String>,
        index: usize,
    },
    TogglePlay,
    Next,
    Prev,
    ToggleShuffle,
    CycleRepeat,
    SeekFrac(f64),
    VolumeFrac(f64),
    /// Toggle mute: drop the volume to 0, or restore the pre-mute level.
    ToggleMute,
    /// Open / close the volume popover (shown when the inline slider collapses
    /// to a button on a narrower bar).
    ToggleVolumePopover,
    ToggleDownload(String),
    /// Keep every track in the given list offline ("Download all" on a playlist
    /// or Liked Songs). Already-cached tracks are left as-is.
    DownloadAll(Vec<String>),
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
    /// Toggle whether the user follows an artist.
    ToggleFollow(String),
    /// Switch the active search filter tab (All / Songs / Albums / …).
    SelectSearchTab(SearchTab),
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
    FocusSearch,
    /// Toggle the queue view from the now-playing bar: open it (remembering the
    /// current view), or, when already on it, hide it and return to where the
    /// user was before.
    ToggleQueue,
    /// Toggle the adaptive navigation sidebar (hamburger / collapse chevron).
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool tracks an independent, unrelated concern (backend readiness, one-time \
              seeding, playback init, the volume popover, the redraw flag) scattered across a \
              large app-state struct; grouping them into a sub-struct would only rename the \
              ~50 call sites across app.rs and render.rs, not reduce them"
)]
pub struct MusicApp {
    lib: Library,
    pb: Playback,
    view: View,
    nav_back: Vec<View>,
    /// The header search field — a real `ui::TextInput` (local mode), so it
    /// gets full editing affordances for free: Ctrl+A/C/X/V, drag-to-select,
    /// double-click word, Shift+Arrow selection, word navigation. The app reads
    /// the live query via `search_input.value()`.
    search_input: TextInput,
    /// The search pill's full rect, captured during draw so a press inside it
    /// routes to `search_input` (caret placement / drag-select).
    search_pill_rect: Rect,
    source: Source,
    #[allow(dead_code)]
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
    /// Real search results merged in-memory (rendered by the Search view and
    /// re-injected into the library snapshot each frame so they stay visible
    /// across a `sync` that reloads the catalog from the document).
    live_hits: Vec<LiveTrack>,
    /// A live search query awaiting results (drives animation frames + a
    /// "searching…" affordance). `None` when idle.
    live_search_pending: Option<String>,
    /// The most recent live search's `(query, ordered track ids)`, so the
    /// Search view can render real YouTube-Music hits in ranking order even
    /// when a multi-word query wouldn't substring-match the local index.
    live_results: Option<(String, Vec<String>)>,
    /// The full multi-type search response for the current query: songs, albums,
    /// artists, playlists + a top-result hero. Powers the Spotify-style filter
    /// tabs; per-type "see all" deep fetches merge into the matching bucket.
    live_bundle: Option<(String, SearchBundle)>,
    /// The active search filter tab (All / Songs / Albums / Artists / Playlists).
    search_tab: SearchTab,
    /// Deep per-type "see all" fetches already dispatched, keyed `"query|kind"`,
    /// so switching to a tab enriches it exactly once.
    search_more_sent: HashSet<String>,
    /// Count of in-flight stream resolutions (keeps redraws flowing until the
    /// resolved URL lands and can be handed to the audio engine).
    resolve_in_flight: usize,

    /// MPRIS media-control service (media keys + the desktop media controls).
    /// A no-op stub until [`Self::enable_mpris`] brings it up on the live
    /// binary; inert in `--shots`/tests and without the `audio` feature.
    mpris: MprisHandle,
    /// Last player state published to MPRIS, so `publish_mpris` only pushes real
    /// changes (the handle itself also dedups).
    mpris_last: Option<PlayerSnapshot>,

    /// A playlist-cover file-portal pick in flight: the receiver the frame loop
    /// drains, plus the playlist it targets. `None` when no picker is open.
    cover_pick: Option<PortalChannel>,
    /// Which playlist an in-flight cover pick belongs to. The portal only ever
    /// answers "which file", so the app remembers "for what".
    cover_pick_for: Option<String>,

    seeded: bool,
    /// When `true`, a first `sync` onto an empty document writes the bundled
    /// sample catalog instead of an empty library. Only the `--shots` renderer
    /// and tests set this (see [`MusicApp::seed_sample_library`]); the live app
    /// starts with an empty library and fills it from real playback + search.
    seed_samples: bool,
    /// Whether the initial playback snapshot has been adopted from the
    /// document. After the first load, playback is authoritative in-memory
    /// (persisted out to the doc on each event) so the smoothly-advancing
    /// playhead isn't clobbered by re-reading the doc each frame.
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
    /// Hit rect of the hamburger toggle, captured during draw so the event
    /// handler can route a press to [`SidebarDrawer::toggle`].
    hamburger_rect: Rect,

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
    toast: Option<(String, Instant)>,

    /// Self-routing right-click context menu (the `ui` primitive, driven
    /// imperatively: opened at the cursor, drawn last, polled via a sink).
    context: ContextMenu,
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

    /// The track the open context menu targets (for go-to-album/artist etc.).
    ctx_track: Option<String>,
    /// The playlist the context-menu target row belongs to, if it was opened
    /// from a playlist detail (so "Remove from playlist" can target it).
    ctx_in_playlist: Option<String>,
    /// The absolute queue index of the context-menu target, if opened from the
    /// queue view (so "Remove from queue" can target it).
    ctx_queue_index: Option<usize>,

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
    /// Memoised local search results for the last query, so the Search view
    /// doesn't re-scan the whole library every frame while typing.
    search_cache: Option<(String, SearchResults)>,
    /// Wall-clock of the last search keystroke, for debouncing the live query.
    search_edit_at: Option<Instant>,
    /// The last query actually dispatched to the live worker (dedupe debounce).
    live_query_sent: Option<String>,

    // ---- offline downloads -----------------------------------------------
    /// In-flight offline downloads: track id → when the download started. Each
    /// frame the progress fraction is derived from the elapsed time and shown as
    /// an animated spinner on the row; at completion the track flips to
    /// [`CacheState::Cached`] (persisted). Held in memory only — the document
    /// stores just the *terminal* cache states, so an advancing download never
    /// churns the CRDT frame-by-frame.
    downloads: HashMap<String, Instant>,

    /// Framework-owned action dispatch: brackets the undo boundary (the same undo
    /// path the Ctrl+K palette takes), encodes and dispatches each on-screen
    /// affordance's [`crate::actions`] action, and remembers the last one so a
    /// test can prove a button and the Ctrl+K
    /// [`ActionSpec`](qos_actions::ActionSpec) build the byte-identical action.
    actions: ui::ActionHost,
}

/// How long a simulated offline download takes to complete (seconds). Long
/// enough that the working spinner is visibly distinct from the finished state.
const DOWNLOAD_SECS: f64 = 2.4;

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
    /// The synced-lyrics view with a highlighted current line at ~40s.
    Lyrics,
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
    // assignments across more functions, not reduce them (the same reasoning
    // as the file-level `too_many_arguments` allow above).
    #[allow(
        clippy::too_many_lines,
        reason = "flat struct-literal initializing every MusicApp field once; splitting it would only relocate field assignments, not reduce them"
    )]
    pub fn new() -> Self {
        // Optional initial-view override, used by `--shots` to capture
        // screens other than Home (e.g. `QOS_MUSIC_VIEW=album:al_glasslight`).
        let (view, query) = Self::parse_initial_view();
        // A preset search query (`QOS_MUSIC_VIEW=search:…`) arms the debounce so
        // the live-shot loop dispatches it after the usual idle beat.
        let preset_search_at = (!query.is_empty()).then(Instant::now);
        let mut search_input = TextInput::local()
            .with_plain_chrome()
            .with_font_size(14.0)
            .with_placeholder("Search songs, albums, artists")
            .with_id("music_search");
        if !query.is_empty() {
            search_input.set_text(&query);
        }
        Self {
            lib: Library::default(),
            pb: Playback::default(),
            view,
            nav_back: Vec::new(),
            search_input,
            search_pill_rect: Rect::ZERO,
            source: Source::default(),
            audio: AudioEngine::new(),
            worker: None,
            backend_name: "offline (bundled library)".to_string(),
            backend_live: false,
            live_hits: Vec::new(),
            live_search_pending: None,
            live_results: None,
            live_bundle: None,
            search_tab: SearchTab::All,
            search_more_sent: HashSet::new(),
            resolve_in_flight: 0,
            mpris: MprisHandle::disabled(),
            mpris_last: None,
            cover_pick: None,
            cover_pick_for: None,
            seeded: false,
            seed_samples: false,
            playback_loaded: false,
            bound_track: None,
            playhead_ms: 0.0,
            last_tick: Instant::now(),
            shuffle_order: Vec::new(),
            shuffle_rng: seed_rng(),
            premute_volume: None,
            new_playlist_track: None,
            rect: Rect::ZERO,
            body_rect: Rect::ZERO,
            scroll: 0.0,
            content_height: 0.0,
            drawer: SidebarDrawer::new()
                .left()
                .docked_width(SIDEBAR_W)
                .breakpoint(SIDEBAR_BREAKPOINT),
            hamburger_rect: Rect::ZERO,
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
            menu_actions: Vec::new(),
            pending: Arc::new(AtomicI64::new(-1)),
            ctx_track: None,
            ctx_in_playlist: None,
            ctx_queue_index: None,
            name_prompt: None,
            name_input: String::new(),
            name_dialog: None,
            lyrics: Vec::new(),
            lyrics_for: None,
            lyrics_synced: false,
            shot_stage: None,
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
            search_cache: None,
            search_edit_at: preset_search_at,
            live_query_sent: None,
            downloads: HashMap::new(),
            actions: ui::ActionHost::new(),
        }
    }

    /// Disable all network use for a hermetic headless render (`--shots` /
    /// tests): no worker thread, no cover fetches. The offline sample + gradient
    /// placeholders render deterministically.
    pub fn set_offline_for_shot(&mut self) {
        self.allow_network = false;
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
            View::Search => self.live_bundle.is_some(),
            _ => true,
        }
    }

    /// Kick off a real search (keeping the current view), for staging a
    /// lyrics/now-playing live shot. Idempotent per query.
    pub fn shot_begin_search(&mut self, query: &str) {
        self.search_input.set_text(query);
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
                queue: vec![first],
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
                        queue: vec![id],
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
        let Ok(raw) = std::env::var("QOS_MUSIC_VIEW") else {
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
            "search" => View::Search,
            other => {
                if let Some(id) = other.strip_prefix("album:") {
                    View::Album(id.to_string())
                } else if let Some(id) = other.strip_prefix("artist:") {
                    View::Artist(id.to_string())
                } else if let Some(id) = other.strip_prefix("playlist:") {
                    View::Playlist(id.to_string())
                } else if let Some(q) = other.strip_prefix("search:") {
                    return (View::Search, q.to_string());
                } else {
                    View::Home
                }
            }
        };
        (v, String::new())
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
                doc::seed(state, &Library::default(), &Playback::default());
            }
        }
        self.seeded = true;
    }

    /// Refresh just the catalog/playlists/cache from the document. Runs
    /// every `sync` so remote library edits (a playlist gaining a track on
    /// another device) appear without disturbing local playback.
    fn reload_library(&mut self, state: &AutoCommit) {
        self.lib = doc::load_library(state);
        self.sync_playlist_covers();
    }

    /// Advance every in-flight offline download by the wall clock. Called once
    /// per frame from `sync` (after the library reload) so the freshly-loaded
    /// terminal cache states get the in-flight progress overlaid back on top,
    /// and a finished download is persisted to the document exactly once.
    fn advance_downloads(&mut self, state: &mut AutoCommit) {
        if self.downloads.is_empty() {
            return;
        }
        let mut done: Vec<String> = Vec::new();
        for (id, started) in &self.downloads {
            let frac = (started.elapsed().as_secs_f64() / DOWNLOAD_SECS).min(1.0) as f32;
            if frac >= 1.0 {
                done.push(id.clone());
            } else {
                // Overlay the live progress on the reloaded library (the doc
                // holds only Streamed/Cached, never a per-frame Downloading).
                self.lib
                    .cache
                    .insert(id.clone(), CacheState::Downloading(frac));
            }
        }
        for id in done {
            self.downloads.remove(&id);
            doc::set_cache(state, &id, CacheState::Cached);
            self.lib.cache.insert(id.clone(), CacheState::Cached);
            let label = self
                .lib
                .track(&id)
                .map(|t| t.title.clone())
                .unwrap_or_default();
            self.toast = Some((
                format!("\u{201c}{label}\u{201d} saved for offline"),
                Instant::now(),
            ));
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

    /// Adopt the full snapshot (library + playback) from the document. Used
    /// for the initial load and by tests.
    fn reload(&mut self, state: &AutoCommit) {
        self.lib = doc::load_library(state);
        self.sync_playlist_covers();
        self.pb = doc::load_playback(state);
        self.bound_track = self.pb.current.clone();
        self.playhead_ms = self.pb.position_ms as f64;
    }

    fn persist_playback(&mut self, state: &mut AutoCommit) {
        self.pb.position_ms = self.playhead_ms as u64;
        doc::write_playback(state, &self.pb);
        self.bound_track = self.pb.current.clone();
    }

    // ---- action dispatch --------------------------------------------------

    /// Dispatch one [`MusicAction`](crate::actions::MusicAction) — the single
    /// CRDT-mutation path shared with the Ctrl+K palette. The action is applied
    /// through the qOS action executor (a pinned actor + committed change, so it
    /// replays byte-identically on peers), exactly as the palette does. The
    /// document heads captured just before the first action of the event are
    /// stashed so the host records **one** undo boundary bracketing it
    /// ([`ui::widget::Widget::take_committed_action`]). Callers reload the
    /// in-memory library from the document afterward for immediate UI feedback;
    /// `sync` reconciles it again on the next frame (so a palette dispatch shows
    /// up the same way).
    fn perform_action(&mut self, state: &mut AutoCommit, action: &crate::actions::MusicAction) {
        self.actions
            .dispatch(state, crate::actions::MUSIC_ACTION_TYPE, action);
    }

    /// The context Ctrl+K needs to build Music's resource tree: the user's
    /// playlists and their tracks, read from the in-memory library cache (itself
    /// re-projected from the document each `sync`). The virtual "Liked Songs"
    /// playlist is a projection of the like set, not a stored playlist, so it is
    /// not surfaced here — only real, mutable playlists are.
    pub(crate) fn music_resource_ctx(&self) -> crate::actions::MusicResourceCtx {
        let playlists = self
            .lib
            .playlists
            .iter()
            .map(|p| crate::actions::PlaylistResource {
                id: p.id.clone(),
                name: p.name.clone(),
                tracks: p
                    .track_ids
                    .iter()
                    .map(|tid| crate::actions::TrackRef {
                        id: tid.clone(),
                        label: self.track_label(tid),
                    })
                    .collect(),
            })
            .collect();
        crate::actions::MusicResourceCtx { playlists }
    }

    /// A one-line "Title — Artist" label for a track id, for the palette.
    fn track_label(&self, track_id: &str) -> String {
        match self.lib.track(track_id) {
            Some(t) => {
                let artist = self.lib.artist_name(&t.artist_id);
                if artist.is_empty() {
                    t.title.clone()
                } else {
                    format!("{} — {artist}", t.title)
                }
            }
            None => track_id.to_string(),
        }
    }

    fn current_duration_ms(&self) -> u64 {
        self.pb
            .current
            .as_ref()
            .and_then(|id| self.lib.track(id))
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
    /// everywhere: without the `audio` feature or a session bus it installs the
    /// inert stub, so no code path branches on whether MPRIS is present.
    pub fn enable_mpris(&mut self) {
        let snap = self.mpris_snapshot();
        self.mpris = MprisHandle::spawn(snap.clone());
        self.mpris_last = Some(snap);
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
            if let Some(t) = self.lib.track(&id) {
                snap.track_id.clone_from(&id);
                snap.title.clone_from(&t.title);
                snap.artist = self.lib.artist_name(&t.artist_id).to_string();
                snap.album = self.lib.album_title(&t.album_id).to_string();
                snap.length_us = t.duration_ms() as i64 * 1000;
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
    /// arrived this frame): persist the chosen image path onto the playlist and
    /// record it for the cover image cache, or toast a failure.
    fn drain_cover_pick(&mut self, state: &mut AutoCommit) {
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
                // Store as a `file://` URL so the image cache (shared with http
                // covers) can fetch it off disk.
                let url = format!("file://{}", path.display());
                // The pick (the file portal) is app-side, non-deterministic work;
                // the resulting write of the chosen URL is a replayable action so
                // the cover replicates to peers like any other playlist edit.
                self.perform_action(state, &crate::actions::set_cover_action(&playlist_id, &url));
                self.reload_library(state);
                // Overwrite (not `or_insert`) so re-picking a cover replaces the
                // old one in the image cache.
                self.art_urls.insert(playlist_id, url);
                self.toast = Some(("Playlist cover updated".to_string(), Instant::now()));
            }
            PortalOutcome::Cancelled => {}
            PortalOutcome::Failed(reason) => {
                self.toast = Some((
                    format!("Couldn\u{2019}t set cover: {reason}"),
                    Instant::now(),
                ));
            }
        }
    }

    /// Advance the playhead while playing and auto-advance at end of track.
    /// In-memory only (called from `draw`, which has no document handle); the
    /// updated position persists to the document on the next user event.
    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        if !self.pb.playing || self.pb.current.is_none() {
            return;
        }
        // When the live GStreamer backend is actually decoding a stream, drive
        // the playhead off the *real* pipeline position rather than simulating
        // it, so the counter reflects genuine playback. Before the pipeline has
        // prerolled (no position yet) we only advance if it has really reached
        // PLAYING — otherwise the counter holds instead of running a fake timer
        // while the stream is still resolving/buffering. Offline / no-audio
        // builds fall back to the local clock so the transport still advances.
        if self.audio.has_stream() {
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
                self.toast = Some((format!("Can\u{2019}t play: {msg}"), Instant::now()));
                self.pb.playing = false;
                self.audio.stop();
                return;
            }
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
            } else if self.audio.is_playing_real() {
                self.playhead_ms += dt * 1000.0;
            }
        } else {
            self.playhead_ms += dt * 1000.0;
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
            self.playhead_ms = 0.0;
            self.bound_track = self.pb.current.clone();
        }
    }

    fn advance(&mut self, auto: bool) {
        if self.pb.queue.is_empty() {
            return;
        }
        if self.pb.repeat == RepeatMode::One && auto {
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
            // The current track was dropped: clamp and re-bind to whatever now
            // sits at this slot (or stop if the queue emptied).
            if self.pb.queue.is_empty() {
                self.pb.current = None;
                self.pb.playing = false;
                self.pb.queue_index = 0;
            } else {
                self.pb.queue_index = self.pb.queue_index.min(self.pb.queue.len() - 1);
                self.pb.current = self.pb.queue.get(self.pb.queue_index).cloned();
                self.playhead_ms = 0.0;
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
            .with_max_size(420.0, 190.0)
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
                scale_factor,
                &modifiers,
                state,
                &automerge::ROOT,
                handled,
                &mut ft,
                node,
                &taffy,
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
            Action::TogglePlay => self.on_toggle_play(state),
            Action::Next => self.on_next(state),
            Action::Prev => self.on_prev(state),
            Action::ToggleShuffle => self.on_toggle_shuffle(state),
            Action::CycleRepeat => self.on_cycle_repeat(state),
            Action::SeekFrac(f) => self.on_seek_frac(f, state),
            Action::VolumeFrac(f) => self.on_volume_frac(f, state),
            Action::ToggleVolumePopover => {
                self.volume_popover_open = !self.volume_popover_open;
            }
            Action::ToggleMute => self.on_toggle_mute(state),
            Action::ToggleDownload(id) => self.on_toggle_download(&id, state),
            Action::DownloadAll(ids) => self.on_download_all(&ids, state),
            Action::PickPlaylistCover(playlist_id) => self.on_pick_playlist_cover(playlist_id),
            Action::AddToQueue(id) => self.on_add_to_queue(&id, state),
            Action::PlayNext(id) => self.on_play_next(&id, state),
            Action::RemoveFromQueue(index) => self.on_remove_from_queue(index, state),
            Action::MoveQueueItem { from, to } => self.on_move_queue_item(from, to, state),
            Action::ToggleLike(id) => self.on_toggle_like(&id, state),
            Action::ToggleSaveAlbum(id) => self.on_toggle_save_album(&id, state),
            Action::ToggleFollow(id) => self.on_toggle_follow(&id, state),
            Action::SelectSearchTab(tab) => self.on_select_search_tab(tab),
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
            Action::FocusSearch => self.on_focus_search(),
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

    fn on_remove_from_queue(&mut self, index: usize, state: &mut AutoCommit) {
        self.queue_remove(index);
        self.persist_playback(state);
    }

    fn on_move_queue_item(&mut self, from: usize, to: usize, state: &mut AutoCommit) {
        self.queue_move(from, to);
        self.persist_playback(state);
    }

    fn on_select_search_tab(&mut self, tab: SearchTab) {
        self.search_tab = tab;
        self.scroll = 0.0;
        // Switching to a specific type deep-fetches a fuller list for it
        // (once per query) so the tab is genuinely richer than the mix.
        self.request_search_more(tab);
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
        self.perform_action(
            state,
            &crate::actions::move_track_action(playlist, from, to),
        );
        self.reload_library(state);
    }

    fn on_focus_search(&mut self) {
        if self.view != View::Search {
            self.nav_back.push(self.view.clone());
        }
        self.view = View::Search;
        self.search_input.set_focus(true);
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
        if matches!(self.view, View::Search) {
            self.search_input.set_focus(true);
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
        // A "Browse all" category chip: drop the genre into the search
        // field and stay on Search so the (genre-aware) results render.
        if self.view != View::Search {
            self.nav_back.push(self.view.clone());
            self.view = View::Search;
        }
        self.search_input.set_text(genre);
        self.search_input.set_focus(true);
        // Invalidate the memoised query so the new results recompute, and
        // arm the debounced live search (live build surfaces real hits).
        self.search_cache = None;
        self.search_edit_at = Some(Instant::now());
        self.scroll = 0.0;
        if self.drawer.is_overlay() {
            self.drawer.close();
        }
    }

    fn on_play(&mut self, queue: Vec<String>, index: usize, state: &mut AutoCommit) {
        self.pb.queue = queue;
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
            self.audio.play();
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

    fn on_toggle_download(&mut self, id: &str, state: &mut AutoCommit) {
        let label = self
            .lib
            .track(id)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        let toast = match self.lib.cache_state(id) {
            CacheState::Cached => {
                // Already offline → drop it.
                self.downloads.remove(id);
                doc::set_cache(state, id, CacheState::Streamed);
                self.lib = doc::load_library(state);
                format!("Removed \u{201c}{label}\u{201d} from offline")
            }
            CacheState::Downloading(_) => {
                // Tapping an in-flight download cancels it.
                self.downloads.remove(id);
                doc::set_cache(state, id, CacheState::Streamed);
                self.lib = doc::load_library(state);
                format!("Stopped downloading \u{201c}{label}\u{201d}")
            }
            CacheState::Streamed => {
                // Start a real, time-progressing download. It shows the
                // working spinner and flips to Cached when it finishes.
                self.downloads.insert(id.to_string(), Instant::now());
                // Reflect the in-flight state immediately (the doc keeps
                // only terminal states; see `advance_downloads`).
                self.lib
                    .cache
                    .insert(id.to_string(), CacheState::Downloading(0.0));
                format!("Downloading \u{201c}{label}\u{201d} for offline")
            }
        };
        self.toast = Some((toast, Instant::now()));
    }

    fn on_download_all(&mut self, ids: &[String], state: &mut AutoCommit) {
        // Keep every not-yet-cached track offline in one gesture. Tracks
        // already cached stay as-is (idempotent).
        let mut added = 0usize;
        for id in ids {
            if self.lib.cache_state(id) != CacheState::Cached {
                doc::set_cache(state, id, CacheState::Cached);
                added += 1;
            }
        }
        self.lib = doc::load_library(state);
        self.toast = Some((
            if added == 0 {
                "Already saved for offline".to_string()
            } else {
                format!("Saving {added} songs for offline")
            },
            Instant::now(),
        ));
    }

    fn on_pick_playlist_cover(&mut self, playlist_id: String) {
        // Route through the host's XDG file-chooser portal (never an
        // in-app picker). The pick is async; the frame loop drains the
        // outcome and applies the cover.
        self.cover_pick_for = Some(playlist_id);
        self.cover_pick = Some(cover_request().send());
        self.toast = Some(("Choose a cover image\u{2026}".to_string(), Instant::now()));
    }

    fn on_add_to_queue(&mut self, id: &str, state: &mut AutoCommit) {
        self.pb.queue.push(id.to_string());
        let label = self
            .lib
            .track(id)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        self.toast = Some((
            format!("Added \u{201c}{label}\u{201d} to queue"),
            Instant::now(),
        ));
        self.persist_playback(state);
    }

    fn on_play_next(&mut self, id: &str, state: &mut AutoCommit) {
        self.queue_play_next(id.to_string());
        let label = self
            .lib
            .track(id)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        self.toast = Some((
            format!("Playing \u{201c}{label}\u{201d} next"),
            Instant::now(),
        ));
        if self.pb.current.as_deref() == Some(id) {
            self.start_stream(id);
        }
        self.persist_playback(state);
    }

    fn on_toggle_like(&mut self, id: &str, state: &mut AutoCommit) {
        self.perform_action(state, &crate::actions::toggle_liked_action(id));
        self.reload_library(state);
        let now = self.lib.is_liked(id);
        let label = self
            .lib
            .track(id)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        self.toast = Some((
            if now {
                format!("Liked \u{201c}{label}\u{201d}")
            } else {
                format!("Removed \u{201c}{label}\u{201d} from Liked Songs")
            },
            Instant::now(),
        ));
    }

    fn on_toggle_save_album(&mut self, id: &str, state: &mut AutoCommit) {
        self.perform_action(state, &crate::actions::toggle_saved_album_action(id));
        self.reload_library(state);
        let now = self.lib.is_album_saved(id);
        let label = self.lib.album_title(id).to_string();
        self.toast = Some((
            if now {
                format!("Added \u{201c}{label}\u{201d} to your library")
            } else {
                format!("Removed \u{201c}{label}\u{201d} from your library")
            },
            Instant::now(),
        ));
    }

    fn on_toggle_follow(&mut self, id: &str, state: &mut AutoCommit) {
        self.perform_action(state, &crate::actions::toggle_followed_artist_action(id));
        self.reload_library(state);
        let now = self.lib.is_artist_followed(id);
        // Prefer a live artist-page name; fall back to the library.
        let label = self
            .artist_pages
            .get(id)
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| self.lib.artist_name(id).to_string());
        self.toast = Some((
            if now {
                format!("Following {label}")
            } else {
                format!("Unfollowed {label}")
            },
            Instant::now(),
        ));
    }

    fn on_add_to_playlist(&mut self, playlist: &str, track: &str, state: &mut AutoCommit) {
        self.perform_action(state, &crate::actions::add_track_action(playlist, track));
        self.reload_library(state);
        let pl = self
            .lib
            .playlist(playlist)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        self.toast = Some((format!("Added to \u{201c}{pl}\u{201d}"), Instant::now()));
    }

    /// Begin the new-playlist name prompt; `track` is set when this came from
    /// "Add to playlist… → New playlist…" so the create also adds it.
    fn on_begin_new_playlist(&mut self, track: Option<String>) {
        self.name_prompt = Some(NamePrompt::NewPlaylist);
        self.name_input = String::new();
        self.name_dialog = None;
        self.new_playlist_track = track;
        self.search_input.set_focus(false);
    }

    fn on_create_playlist(&mut self, state: &mut AutoCommit) {
        let name = if self.name_input.trim().is_empty() {
            "New Playlist".to_string()
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
            self.perform_action(state, &crate::actions::add_track_action(&id, track));
        }
        self.reload_library(state);
        self.name_prompt = None;
        self.name_input.clear();
        self.name_dialog = None;
        self.nav_back.push(self.view.clone());
        self.view = View::Playlist(id);
        self.scroll = 0.0;
        self.toast = Some((
            if added_track.is_some() {
                format!("Added to new playlist \u{201c}{name}\u{201d}")
            } else {
                format!("Created \u{201c}{name}\u{201d}")
            },
            Instant::now(),
        ));
    }

    fn on_begin_rename_playlist(&mut self, id: String) {
        self.name_input = self
            .lib
            .playlist(&id)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        self.name_prompt = Some(NamePrompt::Rename(id));
        self.name_dialog = None;
        self.search_input.set_focus(false);
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
        self.toast = Some(("Playlist deleted".into(), Instant::now()));
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
    /// **Live tracks** (real YouTube-Music results, keyed `yt:<videoId>`) are
    /// resolved on the [`crate::stream`] worker thread — the UI never blocks on
    /// the network. The resolved googlevideo URL is handed to the audio engine
    /// when it lands, in [`Self::drain_worker`]. Non-live sample tracks (and
    /// builds without the `live` feature) fall back to the legacy Piped
    /// resolver, or stay silent with the local playhead advancing offline.
    fn start_stream(&mut self, track_id: &str) {
        let Some(t) = self.lib.track(track_id) else {
            return;
        };
        let yt_id = t.yt_id.clone();

        if is_live_track(track_id) {
            self.ensure_worker();
            if let Some(w) = &self.worker {
                // Stop the previous stream while the next one resolves so two
                // tracks never overlap on a fast skip.
                self.audio.pause();
                w.request(StreamRequest::Resolve {
                    track_id: track_id.to_string(),
                    video_id: yt_id,
                });
                self.resolve_in_flight += 1;
                return;
            }
        }

        // Fallback path (legacy Piped/Invidious `streaming` feature; offline it
        // returns an error and we stay silent, advancing the local playhead).
        match self.source.resolve_best(&yt_id) {
            Ok(stream) => {
                self.audio.load(&stream.url);
                if self.pb.playing {
                    self.audio.play();
                }
            }
            Err(_offline) => {}
        }
    }

    /// Ensure the background streaming worker exists. Spawned lazily on the
    /// first Play / search (an input event) — never during `--shots`.
    fn ensure_worker(&mut self) {
        if self.worker.is_none() {
            self.worker = Some(StreamWorker::spawn());
        }
    }

    /// Fire a real YouTube-Music search for the current query on the worker.
    /// No-op offline builds still spawn the worker (which reports the offline
    /// backend); the UI keeps showing the local library either way.
    fn request_live_search(&mut self) {
        let q = self.search_input.value().trim().to_string();
        if q.is_empty() {
            return;
        }
        self.ensure_worker();
        if let Some(w) = &self.worker {
            w.request(StreamRequest::Search(q.clone()));
            self.live_query_sent = Some(q.clone());
            self.live_search_pending = Some(q);
            self.search_edit_at = None;
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
                StreamUpdate::SearchResults { query, bundle } => {
                    self.ingest_search_results(&query, *bundle);
                }
                StreamUpdate::SearchMore {
                    query,
                    kind,
                    bundle,
                } => {
                    self.merge_search_more(&query, kind, *bundle);
                }
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
                StreamUpdate::Resolved { track_id, stream } => {
                    self.resolve_in_flight = self.resolve_in_flight.saturating_sub(1);
                    // Only adopt it if it's still the current track (the user
                    // may have skipped while it resolved).
                    if self.pb.current.as_deref() == Some(track_id.as_str()) {
                        // Prefer the rangeable proxy URL; the engine falls back
                        // to the direct googlevideo URL if the proxy yields no
                        // bytes, so playback always starts.
                        self.audio
                            .load_with_fallback(&stream.url, stream.fallback_url.as_deref());
                        if self.pb.playing {
                            self.audio.play();
                        }
                    }
                }
                StreamUpdate::ResolveFailed { track_id, error } => {
                    self.resolve_in_flight = self.resolve_in_flight.saturating_sub(1);
                    if self.pb.current.as_deref() == Some(track_id.as_str()) {
                        // Resolution failed outright (offline, geo-blocked, no
                        // playable stream). Don't leave the transport stuck on a
                        // frozen "playing" state — stop and tell the user why.
                        self.toast =
                            Some((format!("Can\u{2019}t stream: {error}"), Instant::now()));
                        self.pb.playing = false;
                        self.audio.stop();
                    }
                }
                StreamUpdate::Note(_) => {}
            }
        }
    }

    /// Re-inject the accumulated live search hits into the in-memory library
    /// snapshot. Idempotent; runs each frame so live tracks survive a `sync`
    /// that reloads the catalog from the document.
    fn inject_live_hits(&mut self) {
        for lt in &self.live_hits {
            self.lib
                .artists
                .entry(lt.artist_id.clone())
                .or_insert_with(|| Artist {
                    id: lt.artist_id.clone(),
                    name: lt.artist_name.clone(),
                    genre: String::new(),
                    monthly_listeners: 0,
                });
            self.lib
                .albums
                .entry(lt.album_id.clone())
                .or_insert_with(|| Album {
                    id: lt.album_id.clone(),
                    title: lt.album_title.clone(),
                    artist_id: lt.artist_id.clone(),
                    year: 0,
                });
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
    /// make tracks playable, and stash the bundle so the filter tabs render.
    /// Resets the active tab to `All` so a new query lands on the mix.
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
        let n_types = [
            !bundle.tracks.is_empty(),
            !bundle.albums.is_empty(),
            !bundle.artists.is_empty(),
            !bundle.playlists.is_empty(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        self.live_results = Some((query.to_string(), ordered));
        // A brand-new query resets the tab to the curated mix; re-running the
        // same query (debounce/Enter) keeps the user's chosen tab.
        let same_query = self.live_bundle.as_ref().is_some_and(|(q, _)| q == query);
        if !same_query {
            self.search_tab = SearchTab::All;
        }
        self.live_bundle = Some((query.to_string(), bundle));
        self.toast = Some((
            format!(
                "Results for \u{201c}{query}\u{201d}{}",
                if n_types > 1 {
                    " across songs, albums, artists & playlists"
                } else {
                    ""
                }
            ),
            Instant::now(),
        ));
    }

    /// Merge a deep per-type "see all" result into the current bundle's matching
    /// bucket (only if it's still the same query).
    fn merge_search_more(&mut self, query: &str, kind: SearchKind, more: SearchBundle) {
        self.absorb_bundle(&more);
        let Some((q, bundle)) = self.live_bundle.as_mut() else {
            return;
        };
        if q != query {
            return;
        }
        match kind {
            SearchKind::Songs => {
                // Extend the ordered live-results list with any new track ids too.
                let mut ids = self
                    .live_results
                    .as_ref()
                    .filter(|(rq, _)| rq == query)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                for h in &more.tracks {
                    let id = format!("{LIVE_TRACK_PREFIX}{}", h.video_id);
                    if !bundle.tracks.iter().any(|t| t.video_id == h.video_id) {
                        bundle.tracks.push(h.clone());
                    }
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                self.live_results = Some((query.to_string(), ids));
            }
            SearchKind::Albums => {
                for a in more.albums {
                    if !bundle.albums.iter().any(|x| x.id == a.id) {
                        bundle.albums.push(a);
                    }
                }
            }
            SearchKind::Artists => {
                for a in more.artists {
                    if !bundle.artists.iter().any(|x| x.id == a.id) {
                        bundle.artists.push(a);
                    }
                }
            }
            SearchKind::Playlists => {
                for p in more.playlists {
                    if !bundle.playlists.iter().any(|x| x.id == p.id) {
                        bundle.playlists.push(p);
                    }
                }
            }
        }
    }

    /// Fire a deep per-type search for the given filter tab, once per
    /// `(query, kind)`. No-op for the `All` tab and offline / no-query states.
    fn request_search_more(&mut self, tab: SearchTab) {
        let Some(kind) = tab.kind() else { return };
        if !self.allow_network {
            return;
        }
        let Some((query, _)) = self.live_bundle.clone() else {
            return;
        };
        let key = format!("{query}|{kind:?}");
        if self.search_more_sent.contains(&key) {
            return;
        }
        self.ensure_worker();
        if let Some(w) = &self.worker {
            w.request(StreamRequest::SearchMore { query, kind });
            self.search_more_sent.insert(key);
        }
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
            w.request(StreamRequest::Home);
            self.home_requested = true;
        }
    }

    /// Request a real artist page for a channel id (once each).
    fn ensure_artist(&mut self, id: &str) {
        if !self.allow_network || !is_live_id(id) || self.artist_requested.contains(id) {
            return;
        }
        self.ensure_worker();
        if let Some(w) = &self.worker {
            w.request(StreamRequest::Artist(id.to_string()));
            self.artist_requested.insert(id.to_string());
        }
    }

    /// Request a real album page for an album id (once each).
    fn ensure_album(&mut self, id: &str) {
        if !self.allow_network || !is_live_album_id(id) || self.album_requested.contains(id) {
            return;
        }
        self.ensure_worker();
        if let Some(w) = &self.worker {
            w.request(StreamRequest::Album(id.to_string()));
            self.album_requested.insert(id.to_string());
        }
    }

    /// Request a real (live YouTube-Music) playlist page for an id (once each).
    fn ensure_playlist(&mut self, id: &str) {
        if !self.allow_network || !is_live_playlist_id(id) || self.playlist_requested.contains(id) {
            return;
        }
        self.ensure_worker();
        if let Some(w) = &self.worker {
            w.request(StreamRequest::Playlist(id.to_string()));
            self.playlist_requested.insert(id.to_string());
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

    /// Topmost right-click target containing `p` (later-drawn = on top).
    fn row_menu_at(&self, p: Point) -> Option<Action> {
        self.row_menu_hits
            .iter()
            .rev()
            .find(|h| h.rect.contains(p))
            .map(|h| h.action.clone())
    }

    // ---- adaptive sidebar geometry (delegated to `SidebarDrawer`) --------

    /// The width-driven sidebar layout (docked vs overlay). Computed straight
    /// from the allocated width so it's valid even before the first `resolve`.
    #[allow(dead_code)]
    fn sidebar_layout(&self) -> SplitLayout {
        self.drawer.layout_for(self.rect.width())
    }

    /// The width the content column is inset by on the left for the docked
    /// sidebar (0 while overlaying — the flap floats over the content). Read
    /// from the drawer's last-resolved geometry.
    #[allow(dead_code)]
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
        Rect::new(
            self.drawer.content_rect().x0,
            r.y1 - NOWPLAYING_H,
            r.x1,
            r.y1,
        )
    }

    /// Whether the hamburger toggle is currently shown — whenever the sidebar
    /// is not fully docked-open, so the user can summon / re-expand it (also
    /// while it's mid-collapse animation).
    fn show_hamburger(&self) -> bool {
        self.drawer.is_overlay() || !self.drawer.is_open() || self.drawer.reveal() < 0.999
    }

    /// Flip the sidebar open/closed for the current layout.
    fn toggle_sidebar(&mut self) {
        self.drawer.toggle();
        self.dirty = true;
    }

    /// Stage the sidebar for a deterministic `--shots` frame (no event loop to
    /// drive the reveal animation). `open` snaps it fully revealed.
    pub fn snap_sidebar(&mut self, open: bool) {
        self.drawer.snap(open);
    }

    /// Stage a track context menu open for a `--shots` frame. Deferred to the
    /// first `sync` (the catalog is empty at construction); see [`apply_shot_stage`].
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

    /// Stage the header search pill focused with an empty query for a `--shots`
    /// frame — captures the caret at the field start (it must not sit at the end
    /// of the placeholder hint). `search_focused` is plain widget state, so no
    /// deferral is needed.
    pub fn snap_search_focus(&mut self) {
        self.search_input.set_focus(true);
        self.search_input.set_text("");
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
        self.view = View::Songs;
        let ids: Vec<String> = self
            .lib
            .tracks_sorted()
            .iter()
            .map(|t| t.id.clone())
            .collect();
        // Backdate each start so this single frame captures a mid-progress
        // spinner (the per-frame `advance_downloads` recomputes the same frac).
        for (i, frac) in [0.25_f64, 0.55, 0.85].iter().enumerate() {
            let Some(id) = ids.get(i) else { continue };
            let Some(start) = Instant::now()
                .checked_sub(std::time::Duration::from_secs_f64(DOWNLOAD_SECS * frac))
            else {
                continue;
            };
            self.downloads.insert(id.clone(), start);
            self.lib
                .cache
                .insert(id.clone(), CacheState::Downloading(*frac as f32));
        }
    }

    /// Stage the Search screen showing a multi-type result set + the Spotify
    /// filter tabs, for a hermetic `--shots` frame. Builds a bundle from
    /// sample-library entities so the render exercises the real search path
    /// (identical to a live query) without any network.
    pub fn snap_search_results(&mut self, tab: SearchTab) {
        let bundle = SearchBundle {
            top: Some(SearchTop::Artist(ArtistHit {
                id: "ar_aurora".into(),
                name: "Aurora Skies".into(),
                subscriber_count: 2_140_553,
                avatar_url: String::new(),
            })),
            tracks: Self::snap_search_tracks(),
            albums: Self::snap_search_albums(),
            artists: Self::snap_search_artists(),
            playlists: Self::snap_search_playlists(),
        };
        self.ingest_search_results("aurora", bundle);
        self.search_tab = tab;
        self.search_input.set_text("aurora");
        self.view = View::Search;
    }

    fn snap_search_tracks() -> Vec<TrackHit> {
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
        ]
    }

    fn snap_search_albums() -> Vec<crate::backend::AlbumHit> {
        vec![
            sample_album_hit(
                "al_glasslight",
                "Glasslight",
                "Aurora Skies",
                "ar_aurora",
                2023,
            ),
            sample_album_hit(
                "al_paperboats",
                "Paper Boats",
                "Aurora Skies",
                "ar_aurora",
                2021,
            ),
            sample_album_hit(
                "al_afterhours",
                "After Hours Drive",
                "Midnight Transit",
                "ar_midnight",
                2022,
            ),
        ]
    }

    fn snap_search_artists() -> Vec<ArtistHit> {
        vec![
            ArtistHit {
                id: "ar_aurora".into(),
                name: "Aurora Skies".into(),
                subscriber_count: 2_140_553,
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
        ]
    }

    fn snap_search_playlists() -> Vec<PlaylistHit> {
        vec![
            PlaylistHit {
                id: "PLaurora_essentials".into(),
                title: "Aurora Skies Essentials".into(),
                subtitle: "YouTube Music".into(),
                track_count: 40,
                cover_url: String::new(),
            },
            PlaylistHit {
                id: "PLdreampop".into(),
                title: "Dream Pop Dreaming".into(),
                subtitle: "YouTube Music".into(),
                track_count: 80,
                cover_url: String::new(),
            },
        ]
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
                // Target the 3rd sorted track and anchor the cursor on its row
                // (rows start at HEADER_H + ~9, 52px tall) so the highlighted
                // row and the menu's target track line up in the frame.
                let tracks = self.lib.tracks_sorted();
                let track = tracks
                    .get(2)
                    .or_else(|| tracks.first())
                    .map(|t| t.id.clone())
                    .unwrap_or_default();
                if !track.is_empty() {
                    let row_y = self.rect.y0 + HEADER_H + 2.0 * 52.0 + 26.0;
                    self.mouse = Point::new(self.rect.x0 + 360.0, row_y);
                    self.open_track_menu(track, None, None);
                }
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
            ShotStage::Lyrics => {
                // ~40s in lands mid-song so a synced line is highlighted.
                self.stage_play_bundled(40_000.0);
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
                queue: vec![track.clone()],
                index: 0,
            },
        );
        self.push_menu("Play next", Action::PlayNext(track.clone()));
        self.push_menu("Add to queue", Action::AddToQueue(track.clone()));
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

        let p = self.clamp_menu_origin(self.mouse);
        self.context.open_at([p.x as f32, p.y as f32]);
        self.layout_context_menu();
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

        let p = self.clamp_menu_origin(self.mouse);
        self.context.open_at([p.x as f32, p.y as f32]);
        self.layout_context_menu();
        self.dirty = true;
    }

    /// Nudge a menu anchor so the menu stays within the app rect. The menu is
    /// `MENU_WIDTH` (220) wide; height grows with the item count.
    fn clamp_menu_origin(&self, p: Point) -> Point {
        let menu_w = 220.0;
        let rows = self.menu_actions.len() as f64;
        let menu_h = rows * 28.0 + 24.0;
        let x = p.x.min(self.rect.x1 - menu_w - 8.0).max(self.rect.x0 + 8.0);
        let y = p.y.min(self.rect.y1 - menu_h - 8.0).max(self.rect.y0 + 8.0);
        Point::new(x, y)
    }

    /// Run the context menu's own layout pass against a throwaway taffy tree
    /// so its `rect` (and row geometry) are populated for hit-testing + draw.
    fn layout_context_menu(&mut self) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = self.context.build_layout(&mut taffy, &theme, &mut node_map);
        let _ = taffy.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(240.0),
                height: taffy::AvailableSpace::MaxContent,
            },
        );
        self.context.apply_layout(&taffy, node, Point::ZERO);
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

    // ---- search (debounced + memoised) -----------------------------------

    /// Memoised local-library search: recomputes only when the query changes,
    /// so typing doesn't re-scan the whole catalog every frame.
    fn local_search_cached(&mut self, q: &str) -> SearchResults {
        // `QOS_MUSIC_NOCACHE=1` forces the pre-memoisation behaviour (re-scan
        // every frame) so the search-cache win can be measured head-to-head.
        // Read once (not per frame).
        use std::sync::OnceLock;
        static NOCACHE: OnceLock<bool> = OnceLock::new();
        let nocache = *NOCACHE
            .get_or_init(|| std::env::var("QOS_MUSIC_NOCACHE").is_ok_and(|v| !v.is_empty()));
        if !nocache
            && let Some((cached_q, res)) = &self.search_cache
            && cached_q == q
        {
            return res.clone();
        }
        let res = self.lib.search(q);
        self.search_cache = Some((q.to_string(), res.clone()));
        res
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
        let q = self.search_input.value().trim().to_string();
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

fn rrect(r: Rect, radius: f64) -> RoundedRect {
    RoundedRect::from_rect(r, radius)
}

fn fill_rrect(scene: &mut Scene, t: Affine, r: Rect, radius: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rrect(r, radius));
}

// The params are independent geometry/style values a caller picks per call
// (position, size, color, weight, wrap width); a struct would just relocate
// them across 90+ call sites in `app/render.rs` without reducing complexity.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    text: &str,
    x: f64,
    y: f64,
    size: f32,
    color: Color,
    weight: TextWeight,
    max_w: Option<f32>,
) {
    tr.draw_text_weighted(
        scene,
        text,
        size,
        color,
        t * Affine::translate((x, y)),
        max_w,
        weight,
    );
}

/// Truncate `text` to fit `max_w` px at `size`, appending an ellipsis.
///
/// Only appends the ellipsis when at least one real glyph fits before it. When
/// the column is too narrow for even a single character, this renders **nothing**
/// (an empty string) rather than a lone `\u{2026}`: a stray ellipsis with no
/// preceding text conveys nothing and, in a too-narrow column, would spill past
/// its bounds into the neighbouring UI.
fn elide(tr: &mut TextRenderer, text: &str, size: f32, weight: TextWeight, max_w: f64) -> String {
    if f64::from(tr.measure_weighted(text, size, None, weight).0) <= max_w {
        return text.to_string();
    }
    let mut s = String::new();
    for ch in text.chars() {
        let cand = format!("{s}{ch}\u{2026}");
        if f64::from(tr.measure_weighted(&cand, size, None, weight).0) > max_w {
            break;
        }
        s.push(ch);
    }
    if s.is_empty() {
        // Not even one glyph + ellipsis fits — render nothing so the `\u{2026}`
        // never overflows into an adjacent column.
        String::new()
    } else {
        format!("{s}\u{2026}")
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

/// Build an [`AlbumHit`] for a hermetic shot from sample-library-shaped fields.
fn sample_album_hit(
    id: &str,
    title: &str,
    artist: &str,
    artist_id: &str,
    year: u32,
) -> crate::backend::AlbumHit {
    crate::backend::AlbumHit {
        id: id.into(),
        title: title.into(),
        artist: artist.into(),
        artist_id: Some(artist_id.into()),
        year,
        kind: crate::backend::AlbumKind::Album,
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
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
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

/// Read a **local** cover image (a `file://…` URL or an absolute path) off disk,
/// so a playlist cover the user picked via the file portal renders in every
/// build — including the hermetic offline / `--shots` path — without any
/// network. `None` for non-local URLs (handled by the HTTP fetcher) or an
/// unreadable / absurdly-large file.
fn read_local_image(url: &str) -> Option<Vec<u8>> {
    let path = if let Some(rest) = url.strip_prefix("file://") {
        // `file:///abs/path` → `/abs/path`; tolerate a missing host segment.
        std::path::PathBuf::from(rest)
    } else if url.starts_with('/') {
        std::path::PathBuf::from(url)
    } else {
        return None;
    };
    let bytes = std::fs::read(&path).ok()?;
    if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
        return None;
    }
    Some(bytes)
}

/// The transport the [`ImageStore`] uses to fetch cover bytes. Real HTTP under
/// the `live` build (a shared blocking `reqwest` client with a connection
/// pool); a local-file loader otherwise, so a picked playlist cover still
/// renders and the hermetic `--shots` path never opens a socket.
#[cfg(feature = "live")]
fn image_fetcher() -> ui::Fetcher {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    Arc::new(|url: &str| {
        // Local covers (the portal-picked playlist image) load straight off disk.
        if let Some(bytes) = read_local_image(url) {
            return Some(bytes);
        }
        let client = CLIENT.get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(12))
                .user_agent("Mozilla/5.0 (qOS Music)")
                .build()
                .unwrap_or_default()
        });
        let resp = client.get(url).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let bytes = resp.bytes().ok()?;
        // Covers are small; reject empty or absurd payloads.
        if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
            return None;
        }
        Some(bytes.to_vec())
    })
}

#[cfg(not(feature = "live"))]
fn image_fetcher() -> ui::Fetcher {
    // No HTTP stack, but a locally-picked playlist cover still renders offline.
    Arc::new(|url: &str| read_local_image(url))
}

mod render;

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_doc() -> (MusicApp, AutoCommit) {
        let mut app = MusicApp::new();
        // The behaviour tests exercise playback, queueing and playlists against
        // the bundled catalog, so — like `--shots` — they opt into the sample
        // library. The live default (no opt-in) starts empty; see
        // `default_startup_has_empty_library`.
        app.seed_sample_library();
        let mut doc = AutoCommit::new();
        app.ensure_seeded(&mut doc);
        app.reload(&doc);
        (app, doc)
    }

    // ---- synthetic winit events (drive `handle_event` like the real loop) --
    use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
    use winit::keyboard::NamedKey;

    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn press(button: MouseButton) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button,
        }
    }
    /// Drive one event through the production `handle_event` path.
    fn send(app: &mut MusicApp, ev: &WindowEvent, doc: &mut AutoCommit) -> bool {
        send_with(app, ev, winit::event::Modifiers::default(), doc)
    }

    /// Drive one event through `handle_event` with explicit modifiers held.
    fn send_with(
        app: &mut MusicApp,
        ev: &WindowEvent,
        mods: winit::event::Modifiers,
        doc: &mut AutoCommit,
    ) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        let root = automerge::ROOT;
        app.handle_event(
            ev,
            1.0,
            &mods,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }

    fn released(button: MouseButton) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Released,
            button,
        }
    }

    fn wheel(lines_y: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: synth_device(),
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, lines_y),
            phase: winit::event::TouchPhase::Moved,
        }
    }

    /// A character keypress event (logical key + committed text). `winit`'s
    /// per-platform `KeyEvent` field is `pub(crate)`, so — like the other app
    /// event-path tests — we zero the struct and set the public fields (the
    /// zeroed discriminants land on non-heap enum variants, so this is sound).
    fn char_key(c: &str) -> WindowEvent {
        use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey};
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(c.into());
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(c.into());
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    /// A named keypress event (Enter, Escape, Space, arrows …).
    fn named_key(named: winit::keyboard::NamedKey) -> WindowEvent {
        use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey};
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn shift_mods() -> winit::event::Modifiers {
        winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT)
    }

    fn ctrl_mods() -> winit::event::Modifiers {
        winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL)
    }

    /// The captured click rect (center) for the first hit matching `action`,
    /// so tests can click a control the real draw pass registered.
    fn hit_center(app: &MusicApp, action: &Action) -> Option<Point> {
        app.hits
            .iter()
            .find(|h| &h.action == action)
            .map(|h| h.rect.center())
    }

    #[test]
    fn seeds_and_loads_library() {
        let (app, _doc) = app_with_doc();
        assert!(app.lib.total_tracks() >= 20);
        // A fresh launch is idle: nothing is playing until the user starts it.
        assert!(
            app.pb.current.is_none(),
            "no fabricated now-playing track on startup"
        );
        assert!(!app.pb.playing, "transport is not playing on startup");
    }

    #[test]
    fn default_startup_has_empty_library() {
        // The live app (no `seed_sample_library` opt-in) must NOT fabricate a
        // catalog: a fresh document seeds an empty library + playlists.
        let mut app = MusicApp::new();
        let mut doc = AutoCommit::new();
        app.ensure_seeded(&mut doc);
        app.reload(&doc);
        assert_eq!(
            app.lib.total_tracks(),
            0,
            "no fabricated tracks on a fresh launch"
        );
        assert!(
            app.lib.playlists.is_empty(),
            "no fabricated playlists on a fresh launch"
        );
        assert!(
            app.pb.current.is_none(),
            "nothing playing on a fresh launch"
        );
        // The document is still initialised (so persistence + sync work), just
        // empty — a second launch adopts it without re-seeding.
        assert!(
            doc::is_seeded(&doc),
            "the empty library still initialises the doc structure"
        );
    }

    #[test]
    fn toggle_play_round_trips_through_doc() {
        let (mut app, mut doc) = app_with_doc();
        // Play/pause needs a loaded track (see `play_with_nothing_loaded_is_noop`).
        app.pb.current = Some("tk_g1".into());
        let was = app.pb.playing;
        app.dispatch(Action::TogglePlay, &mut doc);
        assert_eq!(app.pb.playing, !was);
        // Persisted to the document.
        assert_eq!(doc::load_playback(&doc).playing, !was);
    }

    #[test]
    fn play_with_nothing_loaded_is_noop() {
        let (mut app, mut doc) = app_with_doc();
        // Nothing queued, nothing current, stopped.
        assert!(app.pb.current.is_none());
        assert!(app.pb.queue.is_empty());
        assert!(!app.pb.playing);
        // Hitting play with an empty queue / no current track must do nothing —
        // never flip to a fabricated "playing" state.
        app.dispatch(Action::TogglePlay, &mut doc);
        assert!(!app.pb.playing, "play with nothing loaded stays stopped");
        assert!(app.pb.current.is_none());
        // And it stayed stopped in the persisted playback too.
        assert!(!doc::load_playback(&doc).playing);
    }

    #[test]
    fn play_action_sets_queue_and_current() {
        let (mut app, mut doc) = app_with_doc();
        let queue: Vec<String> = vec!["tk_g1".into(), "tk_g2".into(), "tk_g3".into()];
        app.dispatch(
            Action::Play {
                queue: queue.clone(),
                index: 1,
            },
            &mut doc,
        );
        assert_eq!(app.pb.current.as_deref(), Some("tk_g2"));
        assert_eq!(app.pb.queue, queue);
        assert!(app.pb.playing);
    }

    #[test]
    fn next_advances_and_wraps_with_repeat_all() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into()],
                index: 1,
            },
            &mut doc,
        );
        app.pb.repeat = RepeatMode::All;
        app.dispatch(Action::Next, &mut doc);
        assert_eq!(app.pb.queue_index, 0);
    }

    #[test]
    fn repeat_one_restarts_on_auto_advance() {
        let (mut app, _doc) = app_with_doc();
        app.pb.queue = vec!["tk_g1".into(), "tk_g2".into()];
        app.pb.queue_index = 0;
        app.pb.current = Some("tk_g1".into());
        app.pb.repeat = RepeatMode::One;
        app.playhead_ms = 5000.0;
        app.advance(true);
        assert_eq!(app.pb.queue_index, 0);
        assert_eq!(app.playhead_ms, 0.0);
    }

    #[test]
    fn seek_sets_playhead() {
        let (mut app, mut doc) = app_with_doc();
        app.pb.current = Some("tk_g1".into()); // 215s
        app.dispatch(Action::SeekFrac(0.5), &mut doc);
        assert!((app.playhead_ms - 215_000.0 * 0.5).abs() < 1.0);
    }

    #[test]
    fn download_toggle_persists() {
        use std::time::Duration;
        let (mut app, mut doc) = app_with_doc();
        // tk_g2 starts streamed.
        assert_eq!(app.lib.cache_state("tk_g2"), CacheState::Streamed);
        // Clicking download starts a real, in-flight download — a visible
        // working state, not an instant jump to "cached".
        app.dispatch(Action::ToggleDownload("tk_g2".into()), &mut doc);
        assert!(
            matches!(app.lib.cache_state("tk_g2"), CacheState::Downloading(_)),
            "the click enters the downloading (spinner) state",
        );
        assert!(app.downloads.contains_key("tk_g2"));
        // Let it finish (backdate the start past the download duration) and run
        // one real frame's sync — the per-frame progression completes it.
        app.downloads.insert(
            "tk_g2".into(),
            Instant::now().checked_sub(Duration::from_secs(30)).unwrap(),
        );
        app.sync(&mut doc, &automerge::ROOT);
        // It flipped to the done/offline state and persisted to the document.
        assert_eq!(app.lib.cache_state("tk_g2"), CacheState::Cached);
        assert_eq!(
            doc::load_library(&doc).cache_state("tk_g2"),
            CacheState::Cached
        );
        assert!(
            !app.downloads.contains_key("tk_g2"),
            "the finished download is retired from the in-flight set",
        );
    }

    #[test]
    fn download_click_animates_then_completes_and_indicator_survives_reload() {
        use std::time::Duration;
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot(); // hermetic
        app.playback_loaded = true;
        // tk_g1 seeds as already-cached; put it in a streamable state first so
        // the click genuinely starts a download (not a "remove offline" toggle).
        doc::set_cache(&mut doc, "tk_g1", CacheState::Streamed);
        app.lib = doc::load_library(&doc);
        assert_eq!(app.lib.cache_state("tk_g1"), CacheState::Streamed);
        // Start the download through the real action path.
        app.dispatch(Action::ToggleDownload("tk_g1".into()), &mut doc);
        assert!(matches!(
            app.lib.cache_state("tk_g1"),
            CacheState::Downloading(_)
        ));
        // A working download keeps frames flowing so the spinner spins — prove
        // it's the download driving it (not a lingering toast).
        app.toast = None;
        assert!(
            app.needs_animation_frame(),
            "an in-flight download alone keeps the frame loop animating",
        );
        // The Downloads/Songs list renders the spinner branch without panic, and
        // the in-flight progress survives the per-frame library reload (the
        // overlay is re-applied in `sync`/`advance_downloads`).
        app.view = View::Songs;
        draw_once(&mut app, &mut doc);
        assert!(
            matches!(app.lib.cache_state("tk_g1"), CacheState::Downloading(_)),
            "download progress survives the per-frame reload",
        );
        // Finish it and run a frame: the indicator flips to the done state.
        app.downloads.insert(
            "tk_g1".into(),
            Instant::now().checked_sub(Duration::from_secs(30)).unwrap(),
        );
        draw_once(&mut app, &mut doc);
        assert_eq!(app.lib.cache_state("tk_g1"), CacheState::Cached);
        assert!(
            !app.downloads.contains_key("tk_g1"),
            "a completed download stops animating (retired from the in-flight set)",
        );
    }

    #[test]
    fn nav_and_back() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(Action::Nav(View::Albums), &mut doc);
        assert_eq!(app.view, View::Albums);
        app.dispatch(Action::Nav(View::Album("al_glasslight".into())), &mut doc);
        assert_eq!(app.view, View::Album("al_glasslight".into()));
        app.dispatch(Action::Back, &mut doc);
        assert_eq!(app.view, View::Albums);
    }

    // ---- text truncation --------------------------------------------------

    #[test]
    fn elide_renders_nothing_when_no_glyph_fits() {
        let mut tr = TextRenderer::new();
        // A width far too narrow for any character + ellipsis: render nothing,
        // never a stray "…" that would overflow into the next column.
        let out = elide(
            &mut tr,
            "Some Long Album Title",
            13.0,
            TextWeight::Regular,
            2.0,
        );
        assert!(
            out.is_empty(),
            "too-narrow column renders no text, got {out:?}"
        );
        assert!(
            !out.contains('\u{2026}'),
            "no lone ellipsis when nothing fits"
        );

        // A genuinely truncating (but non-degenerate) width keeps some glyphs
        // and appends the ellipsis, and the result stays within the budget.
        let mid_w = 90.0;
        let mid = elide(
            &mut tr,
            "Some Long Album Title",
            13.0,
            TextWeight::Regular,
            mid_w,
        );
        assert!(
            mid.ends_with('\u{2026}'),
            "a truncation still shows the ellipsis: {mid:?}"
        );
        assert!(
            mid.chars().count() > 1,
            "at least one real glyph precedes it: {mid:?}"
        );
        assert!(
            f64::from(tr.measure_weighted(&mid, 13.0, None, TextWeight::Regular).0) <= mid_w,
            "the elided string fits inside the column budget",
        );

        // Fits outright → returned unchanged, no ellipsis.
        let whole = elide(&mut tr, "OK", 13.0, TextWeight::Regular, 500.0);
        assert_eq!(whole, "OK");
    }

    // ---- search categories ------------------------------------------------

    #[test]
    fn search_genre_chips_are_clickable_and_filter() {
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot(); // hermetic
        app.playback_loaded = true;
        // The empty Search screen shows the "Browse all" genre chips.
        app.view = View::Search;
        app.search_input.set_text("");
        draw_once(&mut app, &mut doc);
        // A genre chip is a real, registered click target (not inert decoration).
        let chip = app
            .hits
            .iter()
            .find_map(|h| match &h.action {
                Action::BrowseGenre(g) => Some((h.rect, g.clone())),
                _ => None,
            })
            .expect("a Browse-all genre chip registered a click target");
        let (rect, genre) = chip;
        // Clicking it (real hit-test path) activates the filter.
        app.mouse = rect.center();
        assert_eq!(
            app.hit_test(app.mouse),
            Some(Action::BrowseGenre(genre.clone()))
        );
        app.dispatch(Action::BrowseGenre(genre.clone()), &mut doc);
        // The query is now that genre and the results filter to it (non-empty:
        // at least the genre's own artist matches).
        assert_eq!(app.search_input.value(), genre);
        assert_eq!(app.view, View::Search);
        let res = app.lib.search(&genre);
        assert!(
            !res.is_empty(),
            "clicking the {genre:?} chip surfaces that genre's content",
        );
    }

    // ---- responsive sidebar -----------------------------------------------

    /// Set the app's allocated width directly (normally done by `apply_layout`)
    /// and resolve the sidebar drawer against it, as a real frame would.
    fn sized(app: &mut MusicApp, w: f64) {
        app.rect = Rect::new(0.0, 0.0, w, 800.0);
        app.drawer.resolve(app.rect);
    }

    #[test]
    fn layout_docks_when_wide_overlays_when_narrow() {
        let mut app = MusicApp::new();
        sized(&mut app, 1280.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
        sized(&mut app, 480.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
        // Boundary is inclusive at the breakpoint → docked.
        sized(&mut app, SIDEBAR_BREAKPOINT);
        assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
        sized(&mut app, SIDEBAR_BREAKPOINT - 1.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
    }

    #[test]
    fn sidebar_open_docked_closed_overlay_by_default() {
        let mut app = MusicApp::new();
        sized(&mut app, 1280.0);
        assert!(app.drawer.is_open(), "wide: docked + open");
        assert!(
            app.content_inset() > 0.0,
            "wide content is inset by the sidebar"
        );

        sized(&mut app, 480.0);
        assert!(!app.drawer.is_open(), "narrow: overlay + closed");
        assert_eq!(app.content_inset(), 0.0, "narrow content is full-width");
    }

    #[test]
    fn hamburger_toggles_overlay_sidebar() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 480.0); // overlay
        assert!(!app.drawer.is_open());
        app.dispatch(Action::ToggleSidebar, &mut doc);
        assert!(app.drawer.is_open(), "hamburger opens the flap");
        app.dispatch(Action::ToggleSidebar, &mut doc);
        assert!(!app.drawer.is_open(), "toggling again closes it");
    }

    #[test]
    fn navigating_dismisses_overlay_sidebar() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 480.0); // overlay
        app.drawer.open();
        assert!(app.drawer.is_open());
        // Selecting a destination from the open flap closes it.
        app.dispatch(Action::Nav(View::Albums), &mut doc);
        assert_eq!(app.view, View::Albums);
        assert!(!app.drawer.is_open(), "selection dismisses the flap");
    }

    #[test]
    fn navigating_keeps_docked_sidebar() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 1280.0); // docked
        app.dispatch(Action::Nav(View::Albums), &mut doc);
        assert!(app.drawer.is_open(), "docked sidebar persists across nav");
    }

    #[test]
    fn close_action_dismisses_sidebar() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 480.0);
        app.drawer.open();
        app.dispatch(Action::CloseSidebar, &mut doc);
        assert!(!app.drawer.is_open());
    }

    // ---- 1. sidebar collapse ----------------------------------------------

    #[test]
    fn docked_collapse_toggles_and_reexpands() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 1280.0); // docked, open by default
        assert!(app.drawer.is_open());
        // Collapse via the (docked) toggle.
        app.dispatch(Action::ToggleSidebar, &mut doc);
        assert!(!app.drawer.is_open(), "docked sidebar collapses");
        // Once collapsed, the hamburger reappears so it can be re-expanded.
        assert!(
            app.show_hamburger(),
            "collapsed docked rail offers a re-expand affordance"
        );
        app.dispatch(Action::ToggleSidebar, &mut doc);
        assert!(app.drawer.is_open(), "toggling again re-expands it");
    }

    /// Collapsing the docked sidebar clears it: after a settled collapse the
    /// resolved sidebar rect has zero width (nothing left to paint), so no
    /// stale sidebar pixels remain under the content.
    #[test]
    fn collapsed_docked_sidebar_clears_to_zero_width() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 1280.0);
        assert!(
            app.sidebar_rect().width() > 100.0,
            "open docked sidebar has width"
        );
        app.dispatch(Action::ToggleSidebar, &mut doc);
        // Settle the collapse animation.
        app.drawer.snap(false);
        app.drawer.resolve(app.rect);
        assert!(
            app.sidebar_rect().width() < 0.5,
            "collapsed sidebar clears to zero width"
        );
        assert_eq!(
            app.content_inset(),
            0.0,
            "collapsed content reclaims the full width"
        );
    }

    // ---- 2. context menu --------------------------------------------------

    /// Open a track's context menu, returning the action index for a label.
    fn menu_index(app: &MusicApp, label: &str) -> Option<usize> {
        app.context.items().iter().enumerate().find_map(|(i, it)| {
            if it.label() == Some(label) {
                Some(i)
            } else {
                None
            }
        })
    }

    #[test]
    fn open_track_menu_lists_expected_actions() {
        let (mut app, _doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        // tk_g2 is not liked in the seed, so the toggle row reads "Like".
        app.open_track_menu("tk_g2".into(), None, None);
        assert!(app.context.is_open());
        for label in [
            "Play",
            "Play next",
            "Add to queue",
            "Add to playlist\u{2026}",
            "Go to album",
            "Go to artist",
            "Like",
        ] {
            assert!(menu_index(&app, label).is_some(), "menu has {label}");
        }
        assert!(
            menu_index(&app, "Add to library").is_none(),
            "no duplicate library row"
        );
    }

    #[test]
    fn menu_add_to_queue_grows_the_queue() {
        let (mut app, mut doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let before = app.pb.queue.len();
        app.open_track_menu("tk_s2".into(), None, None);
        let idx = menu_index(&app, "Add to queue").unwrap() as i64;
        // Simulate the menu callback firing for that row.
        app.pending.store(idx, Ordering::SeqCst);
        app.drain_menu_selection(&mut doc);
        assert_eq!(
            app.pb.queue.len(),
            before + 1,
            "Add to queue grows the queue"
        );
        assert_eq!(
            app.pb.queue.last().map(std::string::String::as_str),
            Some("tk_s2")
        );
    }

    #[test]
    fn right_click_opens_menu_then_selection_dispatches() {
        // The full production path: draw to register row-menu hits, right-click
        // to open, then a left-click on the "Play next" row dispatches it.
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        // Render one frame so row-menu hits + geometry exist.
        draw_once(&mut app, &mut doc);
        // Find a track row's right-click target.
        let target = app.row_menu_hits.first().cloned();
        let target = target.expect("a track row registered a right-click target");
        let p = target.rect.center();
        send(&mut app, &cursor_at(p), &mut doc);
        let handled = send(&mut app, &press(MouseButton::Right), &mut doc);
        assert!(handled, "right-click on a row is consumed");
        assert!(app.context.is_open(), "right-click opens the context menu");
    }

    // ---- 3. add album to library ------------------------------------------

    #[test]
    fn toggle_save_album_round_trips() {
        let (mut app, mut doc) = app_with_doc();
        assert!(!app.lib.is_album_saved("al_strata"));
        app.dispatch(Action::ToggleSaveAlbum("al_strata".into()), &mut doc);
        assert!(app.lib.is_album_saved("al_strata"));
        assert!(doc::load_library(&doc).is_album_saved("al_strata"));
        app.dispatch(Action::ToggleSaveAlbum("al_strata".into()), &mut doc);
        assert!(!app.lib.is_album_saved("al_strata"));
    }

    // ---- 4. like toggle ---------------------------------------------------

    #[test]
    fn toggle_like_round_trips_and_persists() {
        let (mut app, mut doc) = app_with_doc();
        assert!(!app.lib.is_liked("tk_g2"));
        app.dispatch(Action::ToggleLike("tk_g2".into()), &mut doc);
        assert!(
            app.lib.is_liked("tk_g2"),
            "like is reflected in the snapshot"
        );
        assert!(
            doc::load_library(&doc).is_liked("tk_g2"),
            "like persists to the doc"
        );
        // Liked view surfaces it.
        assert!(app.lib.liked_ids().contains(&"tk_g2".to_string()));
        app.dispatch(Action::ToggleLike("tk_g2".into()), &mut doc);
        assert!(!app.lib.is_liked("tk_g2"));
    }

    // ---- 5. queue ops -----------------------------------------------------

    #[test]
    fn play_next_inserts_after_current() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into(), "c".into()],
                index: 0,
            },
            &mut doc,
        );
        assert_eq!(app.pb.queue_index, 0);
        app.dispatch(Action::PlayNext("x".into()), &mut doc);
        assert_eq!(
            app.pb.queue,
            vec!["a", "x", "b", "c"],
            "play-next sits right after current"
        );
    }

    #[test]
    fn remove_from_queue_drops_entry() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into(), "c".into()],
                index: 1,
            },
            &mut doc,
        );
        // Remove the entry before current → index shifts down, current stays "b".
        app.dispatch(Action::RemoveFromQueue(0), &mut doc);
        assert_eq!(app.pb.queue, vec!["b", "c"]);
        assert_eq!(app.pb.current.as_deref(), Some("b"));
        assert_eq!(app.pb.queue_index, 0);
    }

    #[test]
    fn move_queue_item_reorders_and_keeps_current() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into(), "c".into()],
                index: 0,
            },
            &mut doc,
        );
        app.dispatch(Action::MoveQueueItem { from: 2, to: 1 }, &mut doc);
        assert_eq!(app.pb.queue, vec!["a", "c", "b"]);
        assert_eq!(
            app.pb.current.as_deref(),
            Some("a"),
            "current track unchanged by a reorder below it"
        );
    }

    // ---- 6. playlist management -------------------------------------------

    #[test]
    fn create_playlist_navigates_to_it() {
        let (mut app, mut doc) = app_with_doc();
        app.name_input = "Roadtrip".into();
        let before = app.lib.playlists.len();
        app.dispatch(Action::CreatePlaylist, &mut doc);
        assert_eq!(app.lib.playlists.len(), before + 1);
        assert!(
            matches!(app.view, View::Playlist(_)),
            "creating opens the new playlist"
        );
        let created = app.lib.playlists.iter().any(|p| p.name == "Roadtrip");
        assert!(created);
    }

    #[test]
    fn a_home_fetch_always_settles_and_stops_the_skeleton() {
        // The guarantee is that Home settles — loaded or unavailable — and stops
        // animating. Which of the two happens depends on whether the machine
        // running the test can reach the network, and `live` is on by default,
        // so asserting specifically on the failure path made this pass only on
        // an offline machine.
        let mut app = MusicApp::new();
        let mut doc = AutoCommit::new();
        app.ensure_seeded(&mut doc);
        app.reload(&doc);
        assert!(
            app.lib.tracks.is_empty(),
            "default startup has an empty library"
        );

        // Kick the real Home fetch through the worker + backend.
        app.ensure_home();
        assert!(app.home_requested);
        assert!(
            app.home_is_loading(),
            "a Home request in flight reads as loading"
        );

        // Pump the worker until it reports Home unavailable (the offline backend
        // errors on `home()`), exercising stream.rs → drain end to end.
        let mut settled = false;
        for _ in 0..300 {
            app.drain_worker();
            // Either outcome ends the skeleton; only "still in flight" does not.
            if !app.home_is_loading() {
                settled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            settled,
            "Home settled within 3s — no endless skeleton. loaded={} failed={}",
            app.home.is_some(),
            app.home_failed
        );
        assert!(
            !app.needs_animation_frame(),
            "an unavailable, idle Home requests no more frames"
        );
    }

    #[test]
    fn global_search_deep_links_route_to_the_real_view_and_play_path() {
        use crate::search_type::SearchHit;
        let (mut app, mut doc) = app_with_doc();

        // Real ids straight from the seeded library.
        let album = app.lib.albums.keys().next().unwrap().clone();
        let artist = app.lib.artists.keys().next().unwrap().clone();
        let playlist = app.lib.playlists.first().unwrap().id.clone();
        let track = app.lib.tracks.keys().next().unwrap().clone();

        // An album deep-link opens the album page.
        assert!(app.open_deep_link(&SearchHit::album_link(&album), &mut doc));
        assert_eq!(app.view, View::Album(album));

        // An artist deep-link opens the artist page.
        assert!(app.open_deep_link(&SearchHit::artist_link(&artist), &mut doc));
        assert_eq!(app.view, View::Artist(artist));

        // A playlist deep-link opens the playlist page.
        assert!(app.open_deep_link(&SearchHit::playlist_link(&playlist), &mut doc));
        assert_eq!(app.view, View::Playlist(playlist));

        // A track deep-link actually plays that track through the real play path.
        assert!(app.open_deep_link(&SearchHit::track_link(&track), &mut doc));
        assert_eq!(app.pb.current.as_deref(), Some(track.as_str()));
        assert!(app.pb.queue.contains(&track));

        // A hostile / unknown link is rejected and changes nothing.
        let view_before = app.view.clone();
        assert!(!app.open_deep_link("https://evil/track/x", &mut doc));
        assert_eq!(app.view, view_before);
    }

    #[test]
    fn ctrl_k_search_provider_is_registered_and_activation_plays_and_navigates() {
        // The end-to-end seam the global Ctrl+K uses: the host collects the
        // app's `Widget::search_providers()`, registers them into the shared
        // `ui::search::SearchRegistry`, and on a chosen row calls
        // `Widget::activate_search_result` — the exact flow in
        // `ui::App::open_action_palette` / `action_palette_handle_key`.
        use crate::search_type::SearchHit;
        use ui::search::SearchRegistry;
        let (mut app, mut doc) = app_with_doc();

        let mut registry = SearchRegistry::new();
        for p in Widget::search_providers(&app) {
            registry.register(p);
        }
        assert_eq!(
            registry.provider_count(),
            1,
            "music contributes its library provider"
        );

        // A real song row activated through the Widget method plays the track.
        let track = app.lib.tracks.values().next().unwrap().clone();
        let song = registry
            .query(&track.title)
            .into_iter()
            .find(|r| r.action_id == SearchHit::track_link(&track.id))
            .expect("the song is findable via the registered provider");
        assert_eq!(song.kind, "Songs");
        Widget::activate_search_result(&mut app, &song.action_id, &mut doc, &automerge::ROOT);
        assert_eq!(
            app.pb.current.as_deref(),
            Some(track.id.as_str()),
            "activating the song row plays the real track through the play path"
        );
        assert!(app.pb.queue.contains(&track.id));

        // An artist row activated through the same method opens the artist page.
        let artist = app.lib.artists.values().next().unwrap().clone();
        let arow = registry
            .query(&artist.name)
            .into_iter()
            .find(|r| r.action_id == SearchHit::artist_link(&artist.id))
            .expect("the artist is findable via the registered provider");
        assert_eq!(arow.kind, "Artists");
        Widget::activate_search_result(&mut app, &arow.action_id, &mut doc, &automerge::ROOT);
        assert_eq!(
            app.view,
            View::Artist(artist.id),
            "activation opens the artist page"
        );
    }

    #[test]
    fn add_remove_reorder_playlist_tracks() {
        let (mut app, mut doc) = app_with_doc();
        // pl_morning seeds [tk_p1, tk_r1, tk_g2, tk_r3].
        app.dispatch(
            Action::AddToPlaylist {
                playlist: "pl_morning".into(),
                track: "tk_h2".into(),
            },
            &mut doc,
        );
        let pl = app.lib.playlist("pl_morning").unwrap().clone();
        assert_eq!(
            pl.track_ids.last().map(std::string::String::as_str),
            Some("tk_h2")
        );
        app.dispatch(
            Action::RemoveFromPlaylist {
                playlist: "pl_morning".into(),
                track: "tk_g2".into(),
            },
            &mut doc,
        );
        assert!(
            !app.lib
                .playlist("pl_morning")
                .unwrap()
                .track_ids
                .iter()
                .any(|t| t == "tk_g2")
        );
        app.dispatch(
            Action::MovePlaylistItem {
                playlist: "pl_morning".into(),
                from: 0,
                to: 1,
            },
            &mut doc,
        );
        assert_eq!(
            app.lib.playlist("pl_morning").unwrap().track_ids[1],
            "tk_p1"
        );
    }

    #[test]
    fn rename_and_delete_playlist_actions() {
        let (mut app, mut doc) = app_with_doc();
        app.name_input = "Concentration".into();
        app.dispatch(Action::CommitRename("pl_focus".into()), &mut doc);
        assert_eq!(app.lib.playlist("pl_focus").unwrap().name, "Concentration");
        app.dispatch(Action::DeletePlaylist("pl_focus".into()), &mut doc);
        assert!(app.lib.playlist("pl_focus").is_none());
    }

    // ---- 7. real audio path -----------------------------------------------

    #[test]
    fn play_drives_the_audio_pipeline_path() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["tk_g1".into()],
                index: 0,
            },
            &mut doc,
        );
        // The transport started the stream: the engine recorded a load (the
        // stub records the resolved/intended URI; the live backend set the
        // playbin uri). Offline the source resolver returns an error so no URI
        // is loaded — but with the `streaming`/`audio` features the path runs.
        // Either way `start_stream` was invoked and play state is set.
        assert!(app.pb.playing);
        // Volume changes hit the engine.
        app.dispatch(Action::VolumeFrac(0.5), &mut doc);
        assert!(
            (app.audio.volume() - 0.5).abs() < 1e-6,
            "volume reaches the audio engine"
        );
    }

    // ---- 8. real live streaming (network; `--features live` only) ---------

    /// End-to-end through the *real app widget*: submit a YouTube-Music search,
    /// let the worker return real hits, then play the top hit and confirm the
    /// app resolved + loaded a real `googlevideo.com` audio URL into the engine.
    /// Network-dependent, so gated behind the `live` feature and exercised by
    /// the nightly live-integration step rather than every deterministic shard.
    #[cfg(feature = "live")]
    #[test]
    #[ignore = "queries third-party music services; nightly runs it explicitly"]
    fn live_search_and_stream_resolve_through_the_app() {
        let (mut app, mut doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

        let first = live_search_top_result(&mut app);
        let _url = live_play_and_resolve_stream(&mut app, &mut doc, &first);

        // …and, when this build can actually reach a sound sink, prove the real
        // stream *renders* — decoded PCM buffers reach a real audio sink and the
        // transport advances off the stream clock. This is the end-to-end "a real
        // YouTube track plays sound" proof: the engine stages the resolved
        // (rangeable) source to a temp file with bounded reqwest ranges and plays
        // that.
        #[cfg(feature = "audio")]
        live_audio_render_proof(&mut app);
    }

    /// Submit a real YouTube-Music search, pump the worker until real hits
    /// land, and confirm the top hit is a real, injected, streamable library
    /// track. Returns its id. Part of
    /// [`live_search_and_stream_resolve_through_the_app`].
    #[cfg(feature = "live")]
    fn live_search_top_result(app: &mut MusicApp) -> String {
        use std::time::Duration;

        // Submit a real search (spawns the worker + sends the request).
        app.search_input.set_text("aurora runaway");
        app.request_live_search();
        assert!(app.worker.is_some(), "search spawned the streaming worker");

        // Pump until real results land (bounded ~8s).
        let mut ids = Vec::new();
        for _ in 0..800 {
            app.drain_worker();
            if let Some((q, r)) = &app.live_results
                && q == "aurora runaway"
                && !r.is_empty()
            {
                ids = r.clone();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !ids.is_empty(),
            "live search returned real tracks into the app"
        );

        // The top result is a real, injected, streamable library track.
        let first = ids[0].clone();
        app.inject_live_hits();
        assert!(
            super::is_live_track(&first),
            "live track keyed `yt:<videoId>`"
        );
        assert!(
            app.lib.track(&first).is_some(),
            "live track merged into the library"
        );
        first
    }

    /// Play `first` and pump the worker until the app resolves + loads a
    /// real rangeable audio URL into the engine. Returns the loaded URL.
    /// Part of [`live_search_and_stream_resolve_through_the_app`].
    #[cfg(feature = "live")]
    fn live_play_and_resolve_stream(
        app: &mut MusicApp,
        doc: &mut AutoCommit,
        first: &str,
    ) -> String {
        use std::time::Duration;

        // Play it → the app requests a stream resolution on the worker.
        app.dispatch(
            Action::Play {
                queue: vec![first.to_string()],
                index: 0,
            },
            doc,
        );
        assert_eq!(app.pb.current.as_deref(), Some(first));
        assert!(
            app.resolve_in_flight >= 1,
            "play requested a stream resolve"
        );

        // Pump until the resolved URL is loaded into the audio engine.
        let mut loaded_url = None;
        // The resolved audio URL is a fully-rangeable source: an Invidious
        // `local` proxy (`…/latest_version?…&itag=…&local=true`) that serves the
        // whole track over arbitrary byte ranges, or the direct googlevideo URL
        // as a fallback. Either carries an `itag`.
        let is_streamable = |u: &str| {
            u.contains("itag=") && (u.contains("local=true") || u.contains("googlevideo.com"))
        };
        for _ in 0..2000 {
            app.drain_worker();
            if let Some(uri) = app.audio.current_uri()
                && is_streamable(uri)
            {
                loaded_url = Some(uri.to_string());
                break;
            }
            if app.resolve_in_flight == 0 && app.audio.current_uri().is_none() {
                break; // resolve finished without a URL — fail below
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let url = loaded_url.expect("app resolved + loaded a rangeable audio URL");
        assert!(
            is_streamable(&url),
            "resolved a rangeable, itag-bearing audio URL"
        );
        url
    }

    /// When this build can reach a real sound sink, prove the resolved
    /// stream actually renders: decoded PCM buffers reach a real audio sink
    /// and the transport advances off the stream clock. Part of
    /// [`live_search_and_stream_resolve_through_the_app`].
    #[cfg(all(feature = "live", feature = "audio"))]
    fn live_audio_render_proof(app: &mut MusicApp) {
        use std::time::Duration;

        if audio_server_present() && decode_plugins_present() {
            // Drive the real frame loop until decoded PCM reaches the sink *and*
            // the transport clock has advanced off it (both prove genuine, not
            // faked, playback of the real stream).
            let mut frames = 0u64;
            for _ in 0..2000 {
                app.drain_worker();
                app.tick();
                frames = app.audio.frames_rendered();
                if frames > 0 && app.playhead_ms > 0.0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                frames > 0,
                "real resolved stream rendered PCM buffers to the sink"
            );
            assert_eq!(
                app.audio.audio_sink_is_real(),
                Some(true),
                "the rendered stream reached a real audio sink, not a fakesink"
            );
            assert!(
                app.playhead_ms > 0.0,
                "the playhead advanced with real playback"
            );
            eprintln!(
                "[live audio] PLAYING: sink={:?} frames={frames} playhead={:.0}ms",
                app.audio.actual_audio_sink_name(),
                app.playhead_ms
            );

            // …and it keeps *advancing*: sample the transport clock over the next
            // few seconds and require it to have moved meaningfully forward. This
            // is the "position advances over time" proof — a running playhead
            // driven by the real pipeline PTS, not a single one-shot nudge or a
            // frozen 0:00. The pipeline position and the frame counter must both
            // grow. Each sample is printed so the advance is visible in the log.
            let p0 = app.playhead_ms;
            let f0 = app.audio.frames_rendered();
            let mut advanced = false;
            let mut last_print = std::time::Instant::now();
            for _ in 0..400 {
                app.tick();
                if last_print.elapsed() >= Duration::from_millis(500) {
                    eprintln!(
                        "[live audio] tick: playhead={:.0}ms frames={}",
                        app.playhead_ms,
                        app.audio.frames_rendered()
                    );
                    last_print = std::time::Instant::now();
                }
                if app.playhead_ms > p0 + 250.0 && app.audio.frames_rendered() > f0 {
                    advanced = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            eprintln!(
                "[live audio] ADVANCED: {:.0}ms -> {:.0}ms (frames {} -> {})",
                p0,
                app.playhead_ms,
                f0,
                app.audio.frames_rendered()
            );
            assert!(
                advanced,
                "the real playhead kept advancing (from {p0:.0}ms) while frames kept flowing"
            );
        } else {
            eprintln!(
                "[live audio test] no audio server / decode plugins — skipped the render-proof"
            );
        }
    }

    /// Whether a running PipeWire/Pulse server is reachable (mirrors the audio
    /// module's check) so the render-proof above only runs where a sink exists.
    #[cfg(all(feature = "live", feature = "audio"))]
    fn audio_server_present() -> bool {
        let run = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
        run.is_some_and(|d| d.join("pipewire-0").exists() || d.join("pulse/native").exists())
            || std::env::var_os("PULSE_SERVER").is_some()
    }

    /// Whether the `GStreamer` decode chain for a staged `YouTube` audio stream (an
    /// m4a/AAC file: `filesrc ! qtdemux ! avdec_aac/faad ! audioconvert !
    /// audioresample ! autoaudiosink`) is installed. The render-proof skips
    /// honestly when it isn't — a stock build without `gstreamer1-libav` can't
    /// decode AAC, so there'd be no frames through no fault of the code.
    #[cfg(all(feature = "live", feature = "audio"))]
    fn decode_plugins_present() -> bool {
        let _ = gstreamer::init();
        let base = [
            "filesrc",
            "qtdemux",
            "audioconvert",
            "audioresample",
            "autoaudiosink",
        ];
        let have_base = base
            .iter()
            .all(|n| gstreamer::ElementFactory::find(n).is_some());
        let have_aac = gstreamer::ElementFactory::find("avdec_aac").is_some()
            || gstreamer::ElementFactory::find("faad").is_some();
        have_base && have_aac
    }

    /// Whether the `yt-dlp` full-stream resolver is available (mirrors
    /// [`crate::backend::ytdlp`]'s binary selection). The full-track proof skips
    /// honestly when it isn't — without yt-dlp the resolver can only fall back to
    /// the SABR-truncated direct URL, which genuinely cannot play past ~1 minute.
    #[cfg(all(feature = "live", feature = "audio"))]
    fn ytdlp_present() -> bool {
        let bin = std::env::var("QOS_MUSIC_YTDLP")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "yt-dlp".to_string());
        std::process::Command::new(bin)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// **Full-track proof** (`--features live,audio`; needs `yt-dlp` on the host):
    /// a real `YouTube` track must play *past the ~1-minute SABR-truncation point*.
    /// rustypipe's iOS/TV direct URL only serves its first ~1 minute; the app
    /// resolves a fully-rangeable URL via yt-dlp (the `android_vr` client) so the
    /// whole song streams. We stage the full file, seek to ~88 s, and require the
    /// transport to advance **past 90 s** while decoded PCM keeps reaching the
    /// sink — which a stream truncated at ~1 minute cannot do. Skips honestly
    /// when yt-dlp / an audio server / decode plugins are absent (never a
    /// fakesink, never a weakened assertion).
    #[cfg(all(feature = "live", feature = "audio"))]
    #[test]
    // Streams a real track end to end through yt-dlp and a live audio server, so
    // it depends on a third party being reachable and on that video still
    // existing. It self-skips when the tools are missing, which made it pass on
    // CI and fail on a developer machine that has them but cannot reach the
    // network — the environment decided the result, not the code.
    //
    // Opt in with `cargo nextest run -p music --run-ignored all -E
    // 'test(full_track_plays_past_one_minute)'` when changing the stream
    // resolver; that is when it has something to say.
    #[ignore = "streams a real track over the network; opt in with --run-ignored"]
    fn full_track_plays_past_one_minute() {
        use std::time::Duration;
        if !ytdlp_present() {
            eprintln!("[full-track] skipped — yt-dlp not installed (no full-stream resolver)");
            return;
        }
        if !audio_server_present() || !decode_plugins_present() {
            eprintln!("[full-track] skipped — no audio server / decode plugins");
            return;
        }

        let (mut app, mut doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.search_input
            .set_text("rick astley never gonna give you up");
        app.request_live_search();
        let mut ids = Vec::new();
        for _ in 0..1000 {
            app.drain_worker();
            if let Some((_, r)) = &app.live_results
                && !r.is_empty()
            {
                ids = r.clone();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!ids.is_empty(), "live search returned real tracks");
        let first = ids[0].clone();
        app.inject_live_hits();
        app.dispatch(
            Action::Play {
                queue: vec![first],
                index: 0,
            },
            &mut doc,
        );

        // Wait until the resolved stream renders AND the *whole* file has staged
        // (a fully-rangeable yt-dlp URL fetches the entire song in a few seconds).
        let mut frames = 0u64;
        for _ in 0..6000 {
            app.drain_worker();
            app.tick();
            frames = app.audio.frames_rendered();
            if frames > 0 && app.audio.staging_complete() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(frames > 0, "the resolved stream rendered PCM to the sink");
        assert!(
            app.audio.staging_complete(),
            "the whole track staged (fully-rangeable source, not a truncated segment)"
        );

        // The staged full track must report a multi-minute duration — a positive
        // discriminator that this is NOT the ~1-minute truncated segment.
        let dur = app.audio.duration_ms().unwrap_or(0);
        eprintln!("[full-track] staged whole file: duration={dur}ms frames={frames}");
        assert!(
            dur > 120_000,
            "staged source is a full track ({dur}ms > 2min), not a 1-minute stub"
        );

        // Seek to ~88 s. Then let the flushing seek land: drive ticks until the
        // *real* pipeline position reflects the seek (~88 s). Reaching 88 s is
        // itself decisive — a stream truncated at ~1 minute has no bytes there and
        // could never position at 88 s.
        app.audio.seek_ms(88_000);
        let mut landed_ms = 0u64;
        for _ in 0..1500 {
            app.tick();
            landed_ms = app.audio.position_ms().unwrap_or(0);
            if landed_ms >= 86_000 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("[full-track] seek to 88s landed at position={landed_ms}ms");
        assert!(
            landed_ms >= 86_000,
            "seek to 88s landed in the late part of the full track (position {landed_ms}ms) — \
             a 1-minute-truncated stream could never reach here"
        );

        // …and playback *continues* past 90 s with decoded PCM still flowing.
        let f0 = app.audio.frames_rendered();
        let mut past = false;
        for _ in 0..3000 {
            app.tick();
            if app.playhead_ms > 90_000.0 && app.audio.frames_rendered() > f0 {
                past = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!(
            "[full-track] advanced past cutoff: playhead={:.0}ms frames {}->{}",
            app.playhead_ms,
            f0,
            app.audio.frames_rendered()
        );
        assert!(
            past,
            "playback advanced past 90s (playhead {:.0}ms) while decoding — the full track \
             streams past the 1-minute cutoff",
            app.playhead_ms
        );
    }

    // ---- 9. lyrics --------------------------------------------------------

    #[test]
    fn ensure_lyrics_loads_bundled_for_current() {
        let (mut app, _doc) = app_with_doc();
        app.pb.current = Some("tk_g1".into());
        app.lyrics_for = None;
        app.ensure_lyrics();
        assert!(
            !app.lyrics.is_empty(),
            "bundled lyrics load for the current track"
        );
        assert!(app.lyrics_synced, "tk_g1 has synced lyrics");
    }

    // ---- shared draw helper -----------------------------------------------

    /// Render one offscreen frame through the production draw path so hit
    /// lists + geometry are populated (no GPU; just the Vello scene encode).
    fn draw_once(app: &mut MusicApp, doc: &mut AutoCommit) {
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;
        // Lay the root out to the test viewport first.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sync(doc, &automerge::ROOT);
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let mut scene = Scene::new();
        app.render_frame(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    }

    #[test]
    fn draw_registers_row_menu_hits() {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        draw_once(&mut app, &mut doc);
        assert!(
            !app.row_menu_hits.is_empty(),
            "the songs table registers per-row right-click targets"
        );
    }

    // ---- 10. shuffle actually reorders playback ----------------------------

    /// Build a longer play queue of real seeded tracks (index 0 = current).
    fn shuffle_queue() -> Vec<String> {
        [
            "tk_g1", "tk_g2", "tk_g3", "tk_g4", "tk_g5", "tk_p1", "tk_p2", "tk_p3",
        ]
        .iter()
        .map(std::string::ToString::to_string)
        .collect()
    }

    #[test]
    fn enabling_shuffle_builds_a_permutation_current_first() {
        let (mut app, mut doc) = app_with_doc();
        app.shuffle_rng = 0xD1CE_5EED_1234_5678; // deterministic order for the assert
        let q = shuffle_queue();
        app.dispatch(
            Action::Play {
                queue: q.clone(),
                index: 0,
            },
            &mut doc,
        );
        app.dispatch(Action::ToggleShuffle, &mut doc);
        assert!(app.pb.shuffle);
        assert!(
            doc::load_playback(&doc).shuffle,
            "shuffle flag persists to the doc"
        );
        assert_eq!(app.shuffle_order.len(), q.len());
        let mut sorted = app.shuffle_order.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            (0..q.len()).collect::<Vec<usize>>(),
            "shuffle order is a permutation of the queue"
        );
        assert_eq!(
            app.shuffle_order[0], 0,
            "the currently-playing track stays first"
        );
        assert_ne!(
            app.shuffle_order,
            (0..q.len()).collect::<Vec<usize>>(),
            "the upcoming order is genuinely reshuffled, not the identity"
        );
    }

    #[test]
    fn shuffled_advance_visits_every_track_then_stops() {
        let (mut app, mut doc) = app_with_doc();
        app.shuffle_rng = 0xABCD_1234_9876_5432;
        let q: Vec<String> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        app.dispatch(
            Action::Play {
                queue: q.clone(),
                index: 0,
            },
            &mut doc,
        );
        app.dispatch(Action::ToggleShuffle, &mut doc);
        app.pb.repeat = RepeatMode::Off;
        let order = app.shuffle_order.clone();
        let mut visited = vec![app.pb.queue_index];
        for _ in 0..(q.len() - 1) {
            app.dispatch(Action::Next, &mut doc);
            visited.push(app.pb.queue_index);
        }
        // The production Next path (advance) consults the shuffled order, not the
        // linear queue: proof the dead shuffle button now drives playback.
        assert_eq!(visited, order, "Next follows the shuffled permutation");
        let mut vs = visited.clone();
        vs.sort_unstable();
        assert_eq!(
            vs,
            (0..q.len()).collect::<Vec<usize>>(),
            "every queued track is visited exactly once"
        );
        let last = app.pb.current.clone();
        app.dispatch(Action::Next, &mut doc);
        assert!(
            !app.pb.playing,
            "advancing past the final shuffled track stops (repeat off)"
        );
        assert_eq!(app.pb.current, last, "playback stays on the final track");
    }

    #[test]
    fn shuffled_repeat_all_keeps_playing_after_the_last_track() {
        let (mut app, mut doc) = app_with_doc();
        app.shuffle_rng = 0x0F0F_1234_5678_9ABC;
        let q: Vec<String> = ["a", "b", "c", "d"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        app.dispatch(
            Action::Play {
                queue: q.clone(),
                index: 0,
            },
            &mut doc,
        );
        app.dispatch(Action::ToggleShuffle, &mut doc);
        app.pb.repeat = RepeatMode::All;
        for _ in 0..q.len() {
            app.dispatch(Action::Next, &mut doc);
        }
        assert!(
            app.pb.playing,
            "a repeating shuffled queue laps instead of stopping"
        );
    }

    #[test]
    fn disabling_shuffle_returns_to_linear_order() {
        let (mut app, mut doc) = app_with_doc();
        app.shuffle_rng = 0x5;
        let q: Vec<String> = ["a", "b", "c", "d"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        app.dispatch(Action::Play { queue: q, index: 0 }, &mut doc);
        app.dispatch(Action::ToggleShuffle, &mut doc); // on
        assert!(!app.shuffle_order.is_empty());
        app.dispatch(Action::ToggleShuffle, &mut doc); // off
        assert!(!app.pb.shuffle);
        assert!(
            app.shuffle_order.is_empty(),
            "disabling shuffle drops the shuffled order"
        );
        app.dispatch(Action::Next, &mut doc);
        assert_eq!(
            app.pb.queue_index, 1,
            "linear advance resumes after disabling shuffle"
        );
        assert_eq!(app.pb.current.as_deref(), Some("b"));
    }

    // ---- 11. keyboard transport shortcuts (via the real event path) --------

    #[test]
    fn s_key_toggles_shuffle_via_event_path() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into(), "c".into()],
                index: 0,
            },
            &mut doc,
        );
        assert!(!app.pb.shuffle);
        assert!(send(&mut app, &char_key("s"), &mut doc), "'s' is consumed");
        assert!(app.pb.shuffle, "'s' enables shuffle");
        assert_eq!(app.shuffle_order.len(), 3, "'s' builds the shuffled order");
        send(&mut app, &char_key("s"), &mut doc);
        assert!(!app.pb.shuffle, "'s' again disables it");
    }

    #[test]
    fn space_key_toggles_play_via_event_path() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into()],
                index: 0,
            },
            &mut doc,
        );
        assert!(app.pb.playing);
        send(&mut app, &named_key(NamedKey::Space), &mut doc);
        assert!(!app.pb.playing, "space pauses");
        send(&mut app, &named_key(NamedKey::Space), &mut doc);
        assert!(app.pb.playing, "space resumes");
    }

    #[test]
    fn n_and_p_keys_change_track() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into(), "c".into()],
                index: 0,
            },
            &mut doc,
        );
        send(&mut app, &char_key("n"), &mut doc);
        assert_eq!(app.pb.queue_index, 1, "'n' plays the next track");
        send(&mut app, &char_key("p"), &mut doc);
        assert_eq!(app.pb.queue_index, 0, "'p' plays the previous track");
    }

    #[test]
    fn arrow_keys_seek_and_shift_skips() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["tk_g1".into(), "tk_g2".into()],
                index: 0,
            },
            &mut doc,
        );
        app.playhead_ms = 10_000.0; // tk_g1 is 215s long
        send(&mut app, &named_key(NamedKey::ArrowRight), &mut doc);
        assert!(
            (app.playhead_ms - 15_000.0).abs() < 1.0,
            "right arrow seeks +5s"
        );
        send(&mut app, &named_key(NamedKey::ArrowLeft), &mut doc);
        assert!(
            (app.playhead_ms - 10_000.0).abs() < 1.0,
            "left arrow seeks -5s"
        );
        send_with(
            &mut app,
            &named_key(NamedKey::ArrowRight),
            shift_mods(),
            &mut doc,
        );
        assert_eq!(app.pb.queue_index, 1, "shift+right skips to the next track");
        send_with(
            &mut app,
            &named_key(NamedKey::ArrowLeft),
            shift_mods(),
            &mut doc,
        );
        assert_eq!(
            app.pb.queue_index, 0,
            "shift+left skips to the previous track"
        );
    }

    #[test]
    fn slash_key_focuses_search() {
        let (mut app, mut doc) = app_with_doc();
        assert_ne!(app.view, View::Search);
        send(&mut app, &char_key("/"), &mut doc);
        assert_eq!(app.view, View::Search);
        assert!(app.search_input.is_focused());
    }

    // ---- 12. drag-to-seek / drag-volume gestures ---------------------------

    #[test]
    fn dragging_the_scrubber_seeks() {
        let (mut app, mut doc) = app_with_doc();
        app.playback_loaded = true;
        app.pb.queue = vec!["tk_g1".into()];
        app.pb.current = Some("tk_g1".into());
        app.pb.queue_index = 0;
        draw_once(&mut app, &mut doc); // registers the scrubber rect at real geometry
        let sr = app.scrubber_rect;
        assert!(sr.width() > 10.0, "the full transport drew a scrubber bar");
        send(
            &mut app,
            &cursor_at(Point::new(sr.x0 + 4.0, sr.center().y)),
            &mut doc,
        );
        send(&mut app, &press(MouseButton::Left), &mut doc);
        assert_eq!(
            app.drag,
            Some(Drag::Scrubber),
            "pressing the bar begins a scrub drag"
        );
        let near_start = app.playhead_ms;
        send(
            &mut app,
            &cursor_at(Point::new(sr.x1 - 4.0, sr.center().y)),
            &mut doc,
        );
        let near_end = app.playhead_ms;
        send(&mut app, &released(MouseButton::Left), &mut doc);
        assert!(app.drag.is_none(), "releasing ends the drag");
        assert!(
            near_end > near_start + 100_000.0,
            "dragging right seeks forward ({near_start} -> {near_end})"
        );
        assert_eq!(
            doc::load_playback(&doc).position_ms,
            app.playhead_ms as u64,
            "the sought position persists"
        );
    }

    #[test]
    fn dragging_the_volume_bar_sets_volume() {
        let (mut app, mut doc) = app_with_doc();
        app.playback_loaded = true;
        draw_once(&mut app, &mut doc);
        let vr = app.volume_rect;
        assert!(vr.width() > 10.0, "the full transport drew a volume bar");
        send(
            &mut app,
            &cursor_at(Point::new(vr.x0 + 2.0, vr.center().y)),
            &mut doc,
        );
        send(&mut app, &press(MouseButton::Left), &mut doc);
        assert_eq!(app.drag, Some(Drag::Volume));
        let low = app.pb.volume;
        send(
            &mut app,
            &cursor_at(Point::new(vr.x1 - 2.0, vr.center().y)),
            &mut doc,
        );
        let high = app.pb.volume;
        send(&mut app, &released(MouseButton::Left), &mut doc);
        assert!(
            high > low,
            "dragging the volume bar right raises volume ({low} -> {high})"
        );
        assert!(high >= 95, "the far right is near max volume");
        assert!(
            (app.audio.volume() - f32::from(app.pb.volume) / 100.0).abs() < 1e-6,
            "the audio engine tracks the dragged volume"
        );
    }

    // ---- 13. mouse-wheel scroll + clamp ------------------------------------

    #[test]
    fn wheel_scrolls_and_clamps_to_content() {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        draw_once(&mut app, &mut doc);
        let max = (app.content_height - app.body_rect.height()).max(0.0);
        assert!(
            max > 0.0,
            "the songs list overflows, so there is room to scroll"
        );
        send(&mut app, &wheel(-3.0), &mut doc);
        assert!(app.scroll > 0.0, "wheel down scrolls the content");
        assert!(app.scroll <= max);
        send(&mut app, &wheel(-1000.0), &mut doc);
        assert_eq!(app.scroll, max, "a big scroll clamps at the content bottom");
        send(&mut app, &wheel(1000.0), &mut doc);
        assert_eq!(app.scroll, 0.0, "scrolling back up clamps at the top");
    }

    // ---- 14. mute toggle (speaker icon) ------------------------------------

    #[test]
    fn toggle_mute_silences_and_restores_volume() {
        let (mut app, mut doc) = app_with_doc();
        app.pb.volume = 76;
        app.audio.set_volume(0.76);
        app.dispatch(Action::ToggleMute, &mut doc);
        assert_eq!(app.pb.volume, 0, "mute drops the volume to zero");
        assert!(
            app.audio.volume().abs() < 1e-6,
            "the audio engine is silenced"
        );
        assert_eq!(
            doc::load_playback(&doc).volume,
            0,
            "mute persists to the doc"
        );
        app.dispatch(Action::ToggleMute, &mut doc);
        assert_eq!(app.pb.volume, 76, "unmute restores the pre-mute level");
        assert!((app.audio.volume() - 0.76).abs() < 1e-6);
    }

    #[test]
    fn speaker_icon_click_toggles_mute() {
        let (mut app, mut doc) = app_with_doc();
        draw_once(&mut app, &mut doc);
        assert!(app.pb.volume > 0);
        let c =
            hit_center(&app, &Action::ToggleMute).expect("the speaker icon is a wired mute toggle");
        send(&mut app, &cursor_at(c), &mut doc);
        send(&mut app, &press(MouseButton::Left), &mut doc);
        assert_eq!(app.pb.volume, 0, "clicking the speaker mutes");
    }

    // ---- 15. search typing + name-prompt keyboard flow ---------------------

    #[test]
    fn typing_in_search_builds_query_and_local_results() {
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot(); // no live worker thread in tests
        app.dispatch(Action::FocusSearch, &mut doc);
        assert!(app.search_input.is_focused());
        for ch in ["g", "l", "a", "s", "s"] {
            send(&mut app, &char_key(ch), &mut doc);
        }
        assert_eq!(
            app.search_input.value(),
            "glass",
            "typing builds the query through the event path"
        );
        // The Search view renders local hits from this exact cache.
        let res = app.local_search_cached("glass");
        assert!(
            res.tracks.contains(&"tk_g1".to_string()),
            "local search surfaces the matching track (Glasslight)"
        );
        send(&mut app, &named_key(NamedKey::Backspace), &mut doc);
        assert_eq!(
            app.search_input.value(),
            "glas",
            "backspace edits the query"
        );
    }

    /// The header search field is a real `ui::TextInput`, so Ctrl+A selects the
    /// whole query — driven through the production `handle_event` routing, not
    /// by poking the widget.
    #[test]
    fn search_ctrl_a_selects_whole_query_via_event_path() {
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot();
        app.dispatch(Action::FocusSearch, &mut doc);
        app.search_input.set_text("hello world");
        // Ctrl+A on the focused field.
        assert!(
            send_with(&mut app, &char_key("a"), ctrl_mods(), &mut doc),
            "Ctrl+A is consumed by the focused search field"
        );
        assert_eq!(
            app.search_input.selection_range_public(),
            Some((0, "hello world".len())),
            "Ctrl+A selected the entire query on the real handle_event path"
        );
    }

    /// A press-drag across the search field selects a range — proving the real
    /// mouse routing reaches the `TextInput`'s drag-select on the event path.
    #[test]
    fn search_drag_selects_range_via_event_path() {
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot();
        app.search_input.set_text("hello world");
        // Draw once so the field is laid out + its per-char metrics populated
        // (unfocused, so scroll stays at 0 and the click math is simple).
        draw_once(&mut app, &mut doc);
        let rect = app.search_input.get_rect();
        assert!(rect.width() > 20.0, "the search field laid out with width");
        let cxp = app.search_input.char_x_positions().to_vec();
        assert!(cxp.len() >= 10, "per-char metrics populated by the draw");
        // The widget hit-tests with a 10px inner padding.
        let x_for = |b: usize| rect.x0 + 10.0 + cxp[b];
        let y = rect.center().y;

        send(&mut app, &cursor_at(Point::new(x_for(2), y)), &mut doc); // hover → is_hovered
        send(&mut app, &press(MouseButton::Left), &mut doc); // focus + start drag
        assert!(
            app.search_input.is_dragging(),
            "the press began a selection drag"
        );
        send(&mut app, &cursor_at(Point::new(x_for(9), y)), &mut doc); // extend
        let sel = app
            .search_input
            .selection_range_public()
            .expect("the drag produced a selection");
        assert!(sel.1 > sel.0, "drag selected a non-empty range ({sel:?})");
        send(&mut app, &released(MouseButton::Left), &mut doc);
        assert!(!app.search_input.is_dragging(), "release ended the drag");
    }

    /// Toggling the queue view off returns the user to exactly where they were
    /// (view + scroll), not a reset.
    #[test]
    fn queue_toggle_opens_then_restores_previous_view() {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Albums;
        app.scroll = 137.0;

        app.dispatch(Action::ToggleQueue, &mut doc);
        assert_eq!(app.view, View::Queue, "toggling opens the queue view");
        assert_eq!(app.scroll, 0.0, "the queue view starts at the top");

        app.dispatch(Action::ToggleQueue, &mut doc);
        assert_eq!(
            app.view,
            View::Albums,
            "toggling off returns to the prior view"
        );
        assert_eq!(app.scroll, 137.0, "and restores the prior scroll position");
    }

    /// Hovering the now-playing bar's queue button opens the up-next preview,
    /// which lists the next few queued songs.
    #[test]
    fn queue_button_hover_opens_preview() {
        let (mut app, mut doc) = app_with_doc();
        // Keep the in-memory queue authoritative across the draw's `sync` (which
        // would otherwise re-adopt the seeded playback snapshot).
        app.playback_loaded = true;
        app.pb.queue = vec![
            "tk_g1".into(),
            "tk_a1".into(),
            "tk_h1".into(),
            "tk_p1".into(),
        ];
        app.pb.current = Some("tk_g1".into());
        app.pb.queue_index = 0;
        draw_once(&mut app, &mut doc); // registers the queue button rect
        assert!(
            app.queue_btn_rect.width() > 0.0,
            "the wide transport drew a queue button"
        );

        // The preview lists the upcoming (not-current) tracks.
        assert_eq!(
            app.queue_preview_tracks(),
            vec![
                "tk_a1".to_string(),
                "tk_h1".to_string(),
                "tk_p1".to_string()
            ],
        );

        // Not hovering yet → closed; hovering the button → open.
        app.mouse = Point::new(-10.0, -10.0);
        assert!(!app.queue_preview_active(), "no preview when not hovering");
        app.mouse = app.queue_btn_rect.center();
        assert!(
            app.queue_preview_active(),
            "hovering the queue button opens the preview"
        );
    }

    #[test]
    fn name_prompt_typing_and_enter_creates_playlist() {
        let (mut app, mut doc) = app_with_doc();
        // A real rect so the shared dialog lays out and its embedded field can
        // take keystrokes (keyboard is forwarded only after `place_content`).
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.dispatch(Action::BeginNewPlaylist, &mut doc);
        assert!(matches!(app.name_prompt, Some(NamePrompt::NewPlaylist)));
        // Typing routes through the shared ModalDialog into the real TextInput.
        for ch in ["J", "a", "z", "z"] {
            send(&mut app, &char_key(ch), &mut doc);
        }
        assert_eq!(
            app.name_dialog.as_ref().unwrap().dialog.content().value(),
            "Jazz",
            "keystrokes land in the dialog's embedded TextInput",
        );
        send(&mut app, &named_key(NamedKey::Enter), &mut doc);
        assert!(
            app.name_prompt.is_none(),
            "Enter commits and closes the prompt"
        );
        assert!(
            app.name_dialog.is_none(),
            "the dialog is torn down after commit"
        );
        let pl = app
            .lib
            .playlists
            .iter()
            .find(|p| p.name == "Jazz")
            .expect("Enter on the name prompt created the playlist");
        assert!(
            matches!(&app.view, View::Playlist(id) if id == &pl.id),
            "opens the freshly-created playlist"
        );
    }

    // ---- 16. "Add to playlist… → New playlist…" keeps the target track -----

    #[test]
    fn add_to_playlist_new_playlist_includes_target_track() {
        let (mut app, mut doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.open_add_to_playlist_menu("tk_s2");
        let idx = menu_index(&app, "New playlist\u{2026}")
            .expect("picker offers a New playlist row") as i64;
        app.pending.store(idx, Ordering::SeqCst);
        app.drain_menu_selection(&mut doc);
        assert!(
            matches!(app.name_prompt, Some(NamePrompt::NewPlaylist)),
            "opens the name prompt"
        );
        assert_eq!(
            app.new_playlist_track.as_deref(),
            Some("tk_s2"),
            "remembers the track being added"
        );
        app.name_input = "Late Night".into();
        app.dispatch(Action::CreatePlaylist, &mut doc);
        let pl = app
            .lib
            .playlists
            .iter()
            .find(|p| p.name == "Late Night")
            .expect("the playlist was created");
        assert!(
            pl.track_ids.contains(&"tk_s2".to_string()),
            "the target track was added to the new playlist"
        );
        // And it round-trips through the document, not just the snapshot.
        let saved = doc::load_library(&doc);
        let spl = saved
            .playlists
            .iter()
            .find(|p| p.name == "Late Night")
            .unwrap();
        assert!(
            spl.track_ids.contains(&"tk_s2".to_string()),
            "the add persists to the doc"
        );
    }

    #[test]
    fn plain_new_playlist_is_empty() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(Action::BeginNewPlaylist, &mut doc);
        app.name_input = "Blank".into();
        app.dispatch(Action::CreatePlaylist, &mut doc);
        let pl = app
            .lib
            .playlists
            .iter()
            .find(|p| p.name == "Blank")
            .unwrap();
        assert!(
            pl.track_ids.is_empty(),
            "a plain create (no target track) makes an empty playlist"
        );
    }

    // ---- 17. downloads are honest (no fabricated data) ---------------------

    #[test]
    fn download_toggle_toast_is_honest_about_offline() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(Action::ToggleDownload("tk_g2".into()), &mut doc);
        let (msg, _) = app.toast.clone().expect("toggling download shows a toast");
        assert!(
            msg.contains("offline"),
            "the toast describes the real keep-offline action: {msg}"
        );
        assert!(
            !msg.contains("Downloaded"),
            "no fabricated completed-download claim: {msg}"
        );
    }

    #[test]
    fn seed_has_no_fabricated_in_progress_download() {
        let lib = seed::sample_library();
        assert!(
            lib.cache
                .values()
                .all(|s| !matches!(s, CacheState::Downloading(_))),
            "the seed no longer parks a track at a perpetual fake download progress",
        );
    }

    // ---- 18. per-view render smoke test ------------------------------------

    #[test]
    fn every_view_renders_without_panic() {
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot(); // hermetic: no worker / cover fetches
        app.playback_loaded = true;
        app.pb.queue = vec!["tk_g1".into(), "tk_g2".into()];
        app.pb.current = Some("tk_g1".into());
        app.pb.queue_index = 0;
        let views = [
            View::Home,
            View::Songs,
            View::Albums,
            View::Artists,
            View::Playlists,
            View::Downloads,
            View::Liked,
            View::Search,
            View::Queue,
            View::Lyrics,
            View::Album("al_glasslight".into()),
            View::Artist("ar_aurora".into()),
            View::Playlist("pl_focus".into()),
        ];
        for v in views {
            app.view = v.clone();
            draw_once(&mut app, &mut doc);
            assert!(app.content_height > 0.0, "{v:?} produced a frame");
            assert!(!app.hits.is_empty(), "{v:?} registered interactive hits");
        }
    }

    // ---- 19. real audio / playback state ----------------------------------

    /// Render one full frame at a specific viewport size (drives the same
    /// passes the live app + `--shots` do), so responsive layout + captured
    /// interaction rects can be asserted.
    fn draw_at(app: &mut MusicApp, doc: &mut AutoCommit, w: f64, h: f64) {
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.sync(doc, &automerge::ROOT);
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let mut scene = Scene::new();
        app.render_frame(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    }

    #[test]
    fn now_playing_uses_album_cover_when_track_art_missing() {
        let (mut app, _doc) = app_with_doc();
        // An album-track hit often omits per-track art; only the album cover is
        // known — the now-playing bar must use it, not a procedural placeholder.
        app.art_urls
            .insert("al_x".into(), "http://cover/al_x.jpg".into());
        assert_eq!(
            app.best_cover_id("tk_x", "al_x"),
            "al_x",
            "falls back to album art"
        );
        // Track art wins when present.
        app.art_urls
            .insert("tk_x".into(), "http://cover/tk_x.jpg".into());
        assert_eq!(app.best_cover_id("tk_x", "al_x"), "tk_x");
        // With neither, the (procedural) track id is used.
        assert_eq!(app.best_cover_id("tk_none", "al_none"), "tk_none");
    }

    // ---- 20. lyrics: synced load + moving highlight ------------------------

    #[test]
    fn lyrics_view_loads_synced_and_highlight_moves_with_playhead() {
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot();
        app.playback_loaded = true; // don't let sync clobber the staged track
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        app.view = View::Lyrics;
        app.playhead_ms = 40_000.0;
        draw_at(&mut app, &mut doc, 1000.0, 800.0);
        assert!(
            !app.lyrics.is_empty(),
            "synced lyrics loaded for the current track"
        );
        assert!(app.lyrics_synced, "tk_g1 lyrics are time-synced");
        // The highlighted line (last timestamp <= playhead) advances with time.
        let l = crate::lyrics::Lyrics {
            lines: app.lyrics.clone(),
            synced: true,
        };
        let early = l.current_line(10_000).expect("a line at 10s");
        let late = l.current_line(50_000).expect("a line at 50s");
        assert!(
            late > early,
            "the highlighted current line moves forward in time"
        );
    }

    // ---- 21. favourites sync via the CRDT document ------------------------

    #[test]
    fn favourite_like_writes_to_synced_doc_and_converges() {
        let (mut app, mut doc) = app_with_doc();
        assert!(!doc::load_library(&doc).is_liked("tk_g2"));
        // Like via the normal action pipeline (what a click dispatches).
        app.dispatch(Action::ToggleLike("tk_g2".into()), &mut doc);
        assert!(
            doc::load_library(&doc).is_liked("tk_g2"),
            "the like is written to the app's synced Automerge document"
        );
        // It converges to another device's replica over the standard sync path.
        let mut peer = AutoCommit::new();
        crate::sync::converge(&mut doc, &mut peer);
        assert!(
            doc::load_library(&peer).is_liked("tk_g2"),
            "the favourite syncs to another device via the CRDT"
        );
    }

    /// Single source of truth on the real click path: clicking a song row's like
    /// heart dispatches the byte-identical `EncodedAction` the Ctrl+K "Toggle
    /// like" spec builds for that track — the on-screen affordance and the
    /// palette are one action, not two code paths. Proven end-to-end through the
    /// production `handle_event` path.
    #[test]
    fn like_heart_click_dispatches_the_action_the_palette_builds() {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        draw_once(&mut app, &mut doc);
        // Find a real like-heart hit the draw pass registered.
        let (id, center) = app
            .hits
            .iter()
            .find_map(|h| match &h.action {
                Action::ToggleLike(id) => Some((id.clone(), h.rect.center())),
                _ => None,
            })
            .expect("a song row registers a like-heart hit");
        let expected = crate::actions::encode(&crate::actions::toggle_liked_action(&id));
        // The sample library seeds some tracks as already-liked, so assert the
        // membership *flips* rather than assuming a fixed direction.
        let before = doc::load_library(&doc).is_liked(&id);

        send(&mut app, &cursor_at(center), &mut doc);
        send(&mut app, &press(MouseButton::Left), &mut doc);

        assert_eq!(
            app.actions.last_dispatched(),
            Some(&expected),
            "the like heart and the palette build the identical EncodedAction"
        );
        assert_eq!(
            doc::load_library(&doc).is_liked(&id),
            !before,
            "and it actually toggled the track's liked state in the doc"
        );
        // The heads captured for the host's undo bracket are armed (the host
        // drains this via `take_committed_action` to record one undo boundary).
        assert!(
            app.actions.take_committed().is_some(),
            "the click armed exactly one undo boundary for the host to record"
        );
    }

    #[test]
    fn saved_album_syncs_via_doc() {
        let (mut app, mut doc) = app_with_doc();
        let before = doc::load_library(&doc).is_album_saved("al_glasslight");
        app.dispatch(Action::ToggleSaveAlbum("al_glasslight".into()), &mut doc);
        let after = doc::load_library(&doc).is_album_saved("al_glasslight");
        assert_ne!(
            before, after,
            "toggling flips the saved-album state in the synced doc"
        );
        let mut peer = AutoCommit::new();
        crate::sync::converge(&mut doc, &mut peer);
        assert_eq!(
            doc::load_library(&peer).is_album_saved("al_glasslight"),
            after,
            "the saved-album state syncs to another device via the CRDT"
        );
    }

    // ---- 22. responsive bottom bar: volume + swipe ------------------------

    #[test]
    fn volume_control_is_responsive_by_bar_width() {
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot();
        app.playback_loaded = true;
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        // Wide bar (docked sidebar, ~1044px content): inline slider.
        draw_at(&mut app, &mut doc, 1280.0, 800.0);
        assert!(
            app.volume_rect.width() > 0.0,
            "wide bar shows an inline volume slider"
        );
        assert_eq!(
            app.volume_btn_rect,
            Rect::ZERO,
            "wide bar has no collapsed button"
        );
        // Mid bar (~764px content): slider collapses to a button.
        draw_at(&mut app, &mut doc, 1000.0, 800.0);
        assert_eq!(
            app.volume_rect,
            Rect::ZERO,
            "mid bar drops the inline slider"
        );
        assert!(
            app.volume_btn_rect.width() > 0.0,
            "mid bar shows a volume button"
        );
        // Very small (compact bar): volume hidden entirely.
        draw_at(&mut app, &mut doc, 520.0, 800.0);
        assert_eq!(app.volume_rect, Rect::ZERO);
        assert_eq!(app.volume_btn_rect, Rect::ZERO, "compact bar hides volume");
    }

    #[test]
    fn volume_button_opens_a_slider_popover() {
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot();
        app.playback_loaded = true;
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        draw_at(&mut app, &mut doc, 1000.0, 800.0); // collapsed-to-button width
        assert!(app.volume_btn_rect.width() > 0.0);
        app.dispatch(Action::ToggleVolumePopover, &mut doc);
        draw_at(&mut app, &mut doc, 1000.0, 800.0);
        assert!(
            app.volume_popover_open,
            "the volume button opens the popover"
        );
        assert!(
            app.volume_popover_slider.width() > 0.0,
            "the popover hosts a volume slider"
        );
        assert_eq!(
            app.volume_rect, app.volume_popover_slider,
            "the popover slider is the active volume drag target"
        );
    }

    #[test]
    fn compact_bar_renders_progress_below_the_status_bar() {
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot();
        app.playback_loaded = true;
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        draw_at(&mut app, &mut doc, 520.0, 800.0); // compact
        assert!(
            app.scrubber_rect.y1 >= app.rect.y1 - 1.0,
            "the progress bar sits below the whole status bar (its bottom edge)"
        );
        assert!(
            app.scrubber_rect.width() > 300.0,
            "the progress spans the bar width"
        );
    }

    #[test]
    fn compact_scrubber_is_grabbable_not_a_hairline() {
        // At narrow width the scrub bar must be a real, grabbable track — not a
        // 1-2px hairline glued to the window edge.
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot();
        app.playback_loaded = true;
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        draw_at(&mut app, &mut doc, 520.0, 800.0); // compact bar
        let sr = app.scrubber_rect;
        assert!(
            sr.height() >= 16.0,
            "the narrow scrub bar has a grabbable hit height (was {})",
            sr.height()
        );
        assert!(sr.width() > 300.0, "the scrub bar spans the bar width");
        // The hit strip still reaches the window's bottom edge (edge press seeks)…
        assert!(
            sr.y1 >= app.rect.y1 - 1.0,
            "the scrub hit strip reaches the bottom edge"
        );
        // …but its top clears the transport row, so a play/pause press never seeks.
        assert!(
            sr.y0 > app.rect.y1 - NOWPLAYING_H / 2.0,
            "the scrub strip stays below the transport buttons"
        );
    }

    #[test]
    fn songs_row_columns_are_balanced_not_cavernous() {
        // A wide row: the album should sit near the row's horizontal middle
        // (not bunched against the right-hand control cluster) and the title
        // column must be bounded — so a row reads as one connected line instead
        // of "title-left / everything-right" with a cavern between.
        let left = 260.0;
        let right = 1372.0;
        let cols = render::track_row_columns(left, right, true);
        let mid = f64::midpoint(left, right);
        assert!(
            (cols.album - mid).abs() < 130.0,
            "album sits near the row middle (album {} vs mid {mid})",
            cols.album
        );
        assert!(
            cols.album + 150.0 < cols.like,
            "album column leaves room before the control cluster (album {} / like {})",
            cols.album,
            cols.like
        );
        let title_col = cols.album - cols.text;
        assert!(
            (200.0..=520.0).contains(&title_col),
            "title column is bounded, not stretched across the row (was {title_col})"
        );
        assert!(
            cols.album < right - 380.0,
            "album is not bunched against the right edge (album {})",
            cols.album
        );
        // Mid-width rows keep the album ahead of the controls too.
        let m = render::track_row_columns(60.0, 720.0, true);
        assert!(m.album >= m.text + 190.0 && m.album <= m.like - 150.0);
    }

    #[test]
    fn lyrics_wide_layout_is_balanced_with_left_art() {
        // Wide: cover art fills the left column and the lyric band shifts right
        // to balance it — not a centred column with an empty left half.
        let body = Rect::new(232.0, 72.0, 1280.0, 708.0);
        let band_top = body.y0 + 92.0;
        let band_bot = body.y1 - 24.0;
        let layout = render::lyrics_layout(body, band_top, band_bot);
        let art = layout.art.expect("wide layout shows cover art on the left");
        let body_mid = f64::midpoint(body.x0, body.x1);
        assert!(art.center().x < body_mid, "cover art sits in the left half");
        assert!(
            art.width() >= 180.0,
            "the cover art is a substantial square"
        );
        assert!(
            layout.lyrics.center().x > body_mid,
            "the lyrics column shifts right to balance the art"
        );
        assert!(
            layout.lyrics.x1 <= body.x1,
            "the lyrics column stays within the body"
        );
        assert!(
            art.x1 < layout.lyrics.x0,
            "art and lyrics columns don't overlap"
        );

        // Narrow: a single full-width centred column, no side art.
        let narrow = Rect::new(0.0, 72.0, 700.0, 708.0);
        let nl = render::lyrics_layout(narrow, 164.0, 684.0);
        assert!(nl.art.is_none(), "narrow layout has no side art");
        assert!(
            (nl.lyrics.x0 - narrow.x0).abs() < 1.0 && (nl.lyrics.x1 - narrow.x1).abs() < 1.0,
            "narrow lyrics span the full width"
        );
    }

    #[test]
    fn swiping_the_bar_left_plays_next() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["tk_g1".into(), "tk_g2".into(), "tk_g3".into()],
                index: 0,
            },
            &mut doc,
        );
        app.set_offline_for_shot();
        draw_at(&mut app, &mut doc, 520.0, 800.0); // compact bar
        let bar = app.nowplaying_rect();
        // Start on the bar's empty title area (left of the transport buttons).
        let start = Point::new(bar.x0 + bar.width() * 0.35, bar.center().y);
        let endp = Point::new(start.x - 130.0, start.y);
        send(&mut app, &cursor_at(start), &mut doc);
        send(&mut app, &press(MouseButton::Left), &mut doc);
        send(&mut app, &cursor_at(endp), &mut doc);
        send(&mut app, &released(MouseButton::Left), &mut doc);
        assert_eq!(
            app.pb.queue_index, 1,
            "a leftward swipe skips to the next track"
        );
        assert_eq!(app.pb.current.as_deref(), Some("tk_g2"));
    }

    // ---- 23. prefetch on a stable connection ------------------------------

    #[test]
    fn prefetch_only_runs_on_a_stable_connection() {
        let (mut app, _doc) = app_with_doc();
        app.playback_loaded = true;
        app.pb.queue = vec!["tk_g1".into(), "tk_a1".into()]; // tk_a1 has bundled lyrics
        app.pb.queue_index = 0;
        app.pb.current = Some("tk_g1".into());
        // Metered → suppressed.
        app.force_metered = Some(true);
        assert!(
            !app.maybe_prefetch(),
            "prefetch is suppressed on a metered link"
        );
        assert!(app.prefetched_for.is_none());
        assert!(!app.lyrics_cache.contains_key("tk_a1"));
        // Stable → runs, warming the next queued track's lyrics into the cache.
        app.force_metered = Some(false);
        assert!(app.maybe_prefetch(), "prefetch runs on a stable connection");
        assert_eq!(app.prefetched_for.as_deref(), Some("tk_g1"));
        assert!(
            app.lyrics_cache.contains_key("tk_a1"),
            "the next queued track's lyrics are prefetched into the shared cache"
        );
        // Idempotent within the same current track.
        assert!(
            !app.maybe_prefetch(),
            "prefetch fires once per track change"
        );
    }

    // ---- 24. multi-type search, filter tabs, artist + album pages ---------

    use crate::backend::{AlbumHit, AlbumKind, ArtistAlbum};

    fn th(video: &str, title: &str) -> TrackHit {
        TrackHit {
            video_id: video.into(),
            title: title.into(),
            artist: "Daft Punk".into(),
            artist_id: Some("UCdaftpunk".into()),
            album: Some("Discovery".into()),
            album_id: Some("MPREdiscovery".into()),
            duration_s: 300,
            cover_url: String::new(),
        }
    }

    /// A realistic multi-type search response: a top-result artist plus songs,
    /// an album, an artist and a playlist — exactly what one live search yields.
    fn sample_bundle() -> SearchBundle {
        SearchBundle {
            top: Some(SearchTop::Artist(ArtistHit {
                id: "UCdaftpunk".into(),
                name: "Daft Punk".into(),
                subscriber_count: 5_000_000,
                avatar_url: String::new(),
            })),
            tracks: vec![
                th("vid_one", "One More Time"),
                th("vid_two", "Digital Love"),
            ],
            albums: vec![AlbumHit {
                id: "MPREdiscovery".into(),
                title: "Discovery".into(),
                artist: "Daft Punk".into(),
                artist_id: Some("UCdaftpunk".into()),
                year: 2001,
                kind: AlbumKind::Album,
                cover_url: String::new(),
            }],
            artists: vec![ArtistHit {
                id: "UCdaftpunk".into(),
                name: "Daft Punk".into(),
                subscriber_count: 5_000_000,
                avatar_url: String::new(),
            }],
            playlists: vec![PlaylistHit {
                id: "PLdaftmix".into(),
                title: "This Is Daft Punk".into(),
                subtitle: "YouTube Music".into(),
                track_count: 50,
                cover_url: String::new(),
            }],
        }
    }

    /// Put the app on the Search screen showing a fed multi-type bundle, offline
    /// (no worker, no network) so the render path is exercised hermetically.
    fn search_showing_bundle() -> (MusicApp, AutoCommit) {
        let (mut app, doc) = app_with_doc();
        app.set_offline_for_shot();
        app.playback_loaded = true;
        app.ingest_search_results("daft punk", sample_bundle());
        app.view = View::Search;
        app.search_input.set_text("daft punk");
        (app, doc)
    }

    fn has_play_of(app: &MusicApp, id: &str) -> bool {
        app.hits.iter().any(
            |h| matches!(&h.action, Action::Play { queue, .. } if queue.iter().any(|q| q == id)),
        )
    }
    fn has_nav(app: &MusicApp, view: View) -> bool {
        let want = Action::Nav(view);
        app.hits.iter().any(|h| h.action == want)
    }

    #[test]
    fn search_returns_songs_albums_artists_and_playlists() {
        let (mut app, mut doc) = search_showing_bundle();
        // One search populates every result type.
        let (_, b) = app
            .live_bundle
            .as_ref()
            .expect("bundle stored for the query");
        assert!(
            !b.tracks.is_empty()
                && !b.albums.is_empty()
                && !b.artists.is_empty()
                && !b.playlists.is_empty(),
            "a single search returns songs, albums, artists AND playlists",
        );
        // On the All tab the real render surfaces each type as a click target.
        app.search_tab = SearchTab::All;
        draw_once(&mut app, &mut doc);
        assert!(has_play_of(&app, "yt:vid_one"), "a song is a playable row");
        assert!(
            has_nav(&app, View::Album("MPREdiscovery".into())),
            "an album is navigable"
        );
        assert!(
            has_nav(&app, View::Artist("UCdaftpunk".into())),
            "an artist is navigable"
        );
        assert!(
            has_nav(&app, View::Playlist("PLdaftmix".into())),
            "a playlist is navigable"
        );
    }

    #[test]
    fn filter_tabs_are_clickable_and_switch_instantly() {
        let (mut app, mut doc) = search_showing_bundle();
        app.search_tab = SearchTab::All;
        draw_once(&mut app, &mut doc);
        // All five Spotify-style filter tabs are real, registered click targets.
        for tab in SearchTab::ALL {
            assert!(
                app.hits
                    .iter()
                    .any(|h| h.action == Action::SelectSearchTab(tab)),
                "the {tab:?} filter tab is a clickable target",
            );
        }
        // Clicking Albums switches instantly (synchronously): the view now shows
        // albums and drops the song rows — no async wait on the network.
        app.dispatch(Action::SelectSearchTab(SearchTab::Albums), &mut doc);
        assert_eq!(
            app.search_tab,
            SearchTab::Albums,
            "the tab switches immediately"
        );
        draw_once(&mut app, &mut doc);
        assert!(
            has_nav(&app, View::Album("MPREdiscovery".into())),
            "Albums tab shows the albums"
        );
        assert!(
            !has_play_of(&app, "yt:vid_one"),
            "Albums tab does not render the song list"
        );
        // Songs tab is a clean, flat track list (the album-less listening flow).
        app.dispatch(Action::SelectSearchTab(SearchTab::Songs), &mut doc);
        assert_eq!(app.search_tab, SearchTab::Songs);
        draw_once(&mut app, &mut doc);
        assert!(
            has_play_of(&app, "yt:vid_one") && has_play_of(&app, "yt:vid_two"),
            "Songs tab lists every matching song as a playable row",
        );
    }

    #[test]
    fn artist_page_from_search_result_navigates_and_plays_a_top_track() {
        let (mut app, mut doc) = search_showing_bundle();
        app.search_tab = SearchTab::All;
        draw_once(&mut app, &mut doc);
        // Click the artist result (real hit-test path) → the artist page opens.
        let nav = Action::Nav(View::Artist("UCdaftpunk".into()));
        let p = hit_center(&app, &nav).expect("artist result is a click target");
        assert_eq!(app.hit_test(p), Some(nav.clone()));
        app.dispatch(nav, &mut doc);
        assert_eq!(app.view, View::Artist("UCdaftpunk".into()));
        // Supply the real artist page (as the worker would) and render it.
        let page = ArtistPage {
            id: "UCdaftpunk".into(),
            name: "Daft Punk".into(),
            subscriber_count: 5_000_000,
            description: "French electronic music duo formed in Paris in 1993.".into(),
            wikipedia_url: "https://en.wikipedia.org/wiki/Daft_Punk".into(),
            header_url: String::new(),
            tracks: vec![th("top_track", "Harder Better Faster Stronger")],
            albums: vec![
                ArtistAlbum {
                    id: "MPREdiscovery".into(),
                    title: "Discovery".into(),
                    year: 2001,
                    kind: AlbumKind::Album,
                    cover_url: String::new(),
                },
                ArtistAlbum {
                    id: "MPREsingle".into(),
                    title: "Da Funk".into(),
                    year: 1995,
                    kind: AlbumKind::Single,
                    cover_url: String::new(),
                },
            ],
            similar: vec![ArtistHit {
                id: "UCjustice".into(),
                name: "Justice".into(),
                subscriber_count: 1_000_000,
                avatar_url: String::new(),
            }],
        };
        app.absorb_artist(&page);
        app.artist_pages.insert("UCdaftpunk".into(), page);
        draw_once(&mut app, &mut doc);
        // The full artist page surfaces a Follow control, related artists, and the
        // album + single as separate discography entries.
        assert!(
            app.hits
                .iter()
                .any(|h| h.action == Action::ToggleFollow("UCdaftpunk".into())),
            "the artist page has a Follow control",
        );
        assert!(
            has_nav(&app, View::Artist("UCjustice".into())),
            "related artists are navigable"
        );
        assert!(
            has_nav(&app, View::Album("MPREsingle".into())),
            "the single appears in the discography"
        );
        // Click a top track → it plays.
        let top_id = "yt:top_track".to_string();
        let play = app
            .hits
            .iter()
            .find_map(|h| match &h.action {
                Action::Play { queue, .. } if queue.iter().any(|q| q == &top_id) => {
                    Some(h.action.clone())
                }
                _ => None,
            })
            .expect("a top track is a playable row on the artist page");
        app.dispatch(play, &mut doc);
        assert_eq!(
            app.pb.current.as_deref(),
            Some("yt:top_track"),
            "clicking a top track plays it"
        );
        assert!(app.pb.queue.contains(&top_id));
    }

    #[test]
    fn album_page_lists_ordered_tracks_and_plays_the_album() {
        let (mut app, mut doc) = app_with_doc();
        app.set_offline_for_shot();
        app.playback_loaded = true;
        let page = AlbumPage {
            id: "MPREdiscovery".into(),
            title: "Discovery".into(),
            artist_id: "UCdaftpunk".into(),
            artist_name: "Daft Punk".into(),
            year: 2001,
            cover_url: String::new(),
            tracks: vec![
                th("al_one", "One More Time"),
                th("al_two", "Aerodynamic"),
                th("al_three", "Digital Love"),
            ],
        };
        app.absorb_album(&page);
        app.album_pages.insert("MPREdiscovery".into(), page);
        app.view = View::Album("MPREdiscovery".into());
        draw_once(&mut app, &mut doc);
        // The album's full ordered track list renders — every track is playable.
        for vid in ["al_one", "al_two", "al_three"] {
            assert!(
                has_play_of(&app, &format!("yt:{vid}")),
                "album track {vid} is a playable row"
            );
        }
        // The album header plays the whole album in track order.
        let header = app.hits.iter().any(|h| {
            matches!(
                &h.action,
                Action::Play { queue, index: 0 }
                    if queue.len() == 3 && queue[0] == "yt:al_one" && queue[2] == "yt:al_three"
            )
        });
        assert!(header, "the album header plays the whole album in order");
    }

    #[test]
    fn following_an_artist_toggles_and_persists() {
        let (mut app, mut doc) = app_with_doc();
        assert!(!app.lib.is_artist_followed("UCdaftpunk"));
        app.dispatch(Action::ToggleFollow("UCdaftpunk".into()), &mut doc);
        assert!(
            app.lib.is_artist_followed("UCdaftpunk"),
            "Follow adds the artist"
        );
        // Persisted to the CRDT — a fresh load from the document sees it.
        assert!(
            doc::load_library(&doc)
                .followed_artists
                .contains("UCdaftpunk"),
            "follow round-trips through the document",
        );
        app.dispatch(Action::ToggleFollow("UCdaftpunk".into()), &mut doc);
        assert!(
            !app.lib.is_artist_followed("UCdaftpunk"),
            "toggling again unfollows"
        );
    }
}
