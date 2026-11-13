//! The Music page: the whole UI drawn into the Vello scene, so the app owns its
//! layout and hit-testing; playback and downloads stay on this device.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::Receiver;
use ui::time::{Duration, Instant};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash, ObjId};
use base64::Engine as _;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use std::cell::Cell;
use std::rc::Rc;
use ui::ImageStore;
use ui::portal::{FileFilter, PickRequest, PortalChannel};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{Notice, ToastKind};
use ui::widget::{ModalDialog, NavFeed, PromptBody, TransportFeed, Widget, WindowBarLink};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::audio::AudioEngine;
use crate::backend::{
    AlbumPage, ArtistHit, ArtistPage, HomeContent, HomeItem, HomeItemKind, HomeSection,
    PlaylistPage, SearchBundle, SearchTop, TrackHit,
};
use crate::lyrics::{self, LyricsLine, LyricsWorker, TrackMeta};
use crate::model::{
    Album, Artist, CacheState, CatalogueTrack, Library, Playback, RepeatMode, Track,
};
use crate::mpris::{MprisCommand, MprisHandle, PlaybackStatus, PlayerSnapshot};
#[cfg(test)]
use crate::seed;
use crate::stream::{StreamRequest, StreamUpdate, StreamWorker};

/// A host-visible stream start settles within this, even when a resolver or backend wedges.
const STREAM_START_TIMEOUT: Duration = Duration::from_secs(30);
const PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(100);

const OFFLINE_FAILED: &str = "Couldn\u{2019}t download for offline listening";

fn cover_request() -> PickRequest {
    PickRequest::open("Choose a playlist cover", "Set cover")
        .max_contents_bytes(usize::try_from(MAX_COVER_SOURCE_BYTES).unwrap_or(usize::MAX))
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Home,
    Recommendations,
    Songs,
    Albums,
    Artists,
    Playlists,
    Downloads,
    Liked,
    Browse,
    Queue,
    Lyrics,
    NowPlaying,
    Album(String),
    Artist(String),
    Playlist(String),
    Search,
    Kept(String),
}

/// Whether this build can reach an online catalogue; the browser build cannot.
const ONLINE_CATALOGUE: bool = cfg!(feature = "live");

const HOME_EMPTY_TITLE: &str = "music-home-empty-title";
const HOME_EMPTY_MESSAGE: &str = "music-home-empty-message";
const HOME_EMPTY_ACTION: &str = "music-home-empty-action";

fn first_run_copy(online: bool) -> (&'static str, bool) {
    if online {
        ("music-first-run-online", true)
    } else {
        ("music-first-run-offline", false)
    }
}

/// A failed Home fetch's note, only when an online catalogue was expected.
fn home_failure_note(online: bool, error: String) -> Option<String> {
    online.then_some(error)
}

impl View {
    /// A destination: reaching one starts a fresh trail, so there is no back button.
    fn is_section(&self) -> bool {
        matches!(
            self,
            Self::Home
                | Self::Songs
                | Self::Albums
                | Self::Artists
                | Self::Playlists
                | Self::Downloads
                | Self::Liked
                | Self::Browse
                | Self::Queue
                | Self::Kept(_)
        )
    }
}

/// A press captured during draw, run on the next mouse press. Owned data only:
/// the hit list is rebuilt every frame.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Nav(View),
    Recommendation(ui::tilo_resource::Edit<crate::model::Library>),
    Back,
    Play {
        /// Shared across every visible row hit so drawing a long list does not
        /// clone the full queue once per row.
        queue: Arc<Vec<String>>,
        index: usize,
    },
    ShufflePlay(Arc<Vec<String>>),
    OpenSearch,
    TogglePlay,
    Next,
    Prev,
    ToggleShuffle,
    CycleRepeat,
    SetRepeat(RepeatMode),
    OpenPlayerMenu(Rect),
    SeekFrac(f64),
    VolumeFrac(f64),
    ToggleMute,
    PickPlaylistCover(String),
    AddToQueue(String),
    PlayNext(String),
    RemoveFromQueue(usize),
    MoveQueueItem {
        from: usize,
        to: usize,
    },
    ToggleLike(String),
    ToggleSaveAlbum(String),
    SetOffline {
        track: String,
        keep: bool,
    },
    ToggleFollow(String),
    AddToPlaylist {
        playlist: String,
        track: String,
    },
    RemoveFromPlaylist {
        playlist: String,
        track: String,
    },
    MovePlaylistItem {
        playlist: String,
        from: usize,
        to: usize,
    },
    CreatePlaylist,
    BeginNewPlaylist,
    BeginNewSpotifyPlaylist,
    BeginNewPlaylistFor(String),
    BeginRenamePlaylist(String),
    CommitRename(String),
    DeletePlaylist(String),
    /// Open the track menu at the cursor, remembering which list the row came from.
    OpenTrackMenu {
        track: String,
        in_playlist: Option<String>,
        queue_index: Option<usize>,
    },
    OpenAddToPlaylist(String),
    SpotifyLibrary {
        track: String,
        save: bool,
    },
    SpotifyEffect(crate::actions::MusicEffect),
    OpenPlaylistMenu(String),
    OpenSpotifyPlaylistMenu(String),
    CloseNamePrompt,
    /// Toggle the queue view from the bar, returning to the view it was opened from.
    ToggleQueue,
    SetQuery(ui::framework::views::QuerySpec),
    OpenFacet(String),
    KeepQuery,
    ForgetKept(String),
    ShowControl {
        home: String,
        action: Box<Action>,
    },
    RetryRemote(String),
    /// Swallows clicks on a modal surface so they do not reach the scrim.
    None,
}

#[derive(Clone)]
struct Hit {
    rect: Rect,
    action: Action,
}

struct TrackTableSlot {
    table: track_table::TrackTableHost,
    queue: Arc<Vec<String>>,
    playlist: Option<String>,
    queue_offset: Option<usize>,
}

#[derive(Default)]
pub(crate) struct Page {
    track_tables: Vec<TrackTableSlot>,
    media_grids: Vec<media_grid::MediaGridHost>,
    parts: Vec<page_part::PagePart>,
    source_buttons: Vec<ui::widget::Button>,
}

/// An audio-file chooser in flight: a path, or the chosen file's contents.
enum LocalFilePick {
    Path(PortalChannel),
    Contents(ui::portal::ContentsChannel),
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
pub(crate) struct Music {
    lib: Library,
    opened: Rc<std::cell::RefCell<Vec<String>>>,
    library_heads: Option<(Vec<ChangeHash>, usize)>,
    /// Downloads and cache for this device, overlaid on the shared library after each
    /// reload: never in the document, never an action.
    device_cache: BTreeMap<String, CacheState>,
    media_cache: crate::storage::MediaCache,
    media_cache_root: PathBuf,
    /// Tests and captures never read or write the person's profile; the live app does.
    persist_device_profile: bool,
    pb: Playback,
    view: View,
    nav: ui::framework::views::ViewStack<View>,
    search: render::SearchView,
    /// The query views the person kept, a personal setting that follows their devices;
    /// `None` before the surface is attached.
    kept: Option<ui::framework::Setting<ui::framework::views::KeptViews>>,
    kept_here: ui::framework::views::KeptViews,
    library_revision: u64,
    restore_selection: Option<Vec<String>>,
    /// The remote query the platform's search provider asked for; the app draws no field.
    search_query: String,
    pending_catalogue_query: Option<String>,
    catalogue_invocation: Option<tilo_actions::InvocationId>,
    spotify_catalogue: bool,
    /// Whether this device connected an online catalogue; Home fetches nothing until it has.
    catalogue_connected: bool,
    source_invocation: Option<tilo_actions::InvocationId>,
    source_disconnect_pending: bool,
    spotify_jobs: HashMap<u64, spotify::PendingJob>,
    spotify_devices: Vec<crate::backend::SpotifyDevice>,
    spotify_device_buttons: Vec<ui::widget::Button>,
    spotify_remote_state: serde_json::Value,
    spotify_remote_queue: Option<(serde_json::Value, ui::time::Instant)>,
    spotify_library_offset: u32,
    spotify_library_more: bool,
    spotify_menu_controls: Vec<ui::widget::ActionEmitter<()>>,
    control_domain: Option<ui::ControlDomain>,
    source_error: Option<String>,
    open_file_layout_visible: Option<u8>,
    back_requested: std::rc::Rc<std::cell::Cell<bool>>,
    bar_controls: std::collections::VecDeque<tilo_actions::ControlInvocation>,
    bar_entries: ui::framework::BarCommands<window_bar::BarCommand>,
    search_opener: ui::search::SearchOpener,
    audio: AudioEngine,

    /// The streaming worker, spawned lazily on the first play or search and never
    /// during `--shots`, so a capture opens no socket.
    worker: Option<StreamWorker>,
    backend_name: String,
    backend_live: bool,
    /// Search hits from the catalogue, re-applied each frame so a `sync` that reloads
    /// the library keeps them.
    live_hits: Vec<CatalogueTrack>,
    stored_library: crate::model::Library,
    /// A search awaiting the catalogue; `None` when idle.
    live_search_pending: Option<String>,
    live_results: Option<(String, Vec<String>)>,
    live_bundle: Option<(String, SearchBundle)>,
    /// In-flight stream resolutions: they keep redraws flowing until the URL lands.
    resolve_in_flight: usize,
    /// Monotonic request id: a reply that is not the latest one changes nothing.
    next_request_id: u64,
    active_resolve_request: Option<u64>,
    /// The lifecycle invocation waiting for one stream-resolution id; never an in-window play.
    resource_stream_invocation: Option<(tilo_actions::InvocationId, u64, u64, Instant)>,
    resource_stream_resume: Option<String>,
    /// Terminal updates stay per invocation until the host consumes them; a newer one
    /// must not overwrite an unconsumed result.
    resource_stream_results: Vec<tilo_actions::InvocationUpdate>,
    active_search_request: Option<u64>,
    /// `(query, reason)` of the last search failure, shown with a retry control and announced.
    search_error: Option<(String, String)>,
    /// The last source failure the person can see, until another source succeeds.
    playback_error: Option<String>,

    /// MPRIS media keys; an inert stub until `enable_mpris`, so nothing branches on it.
    mpris: MprisHandle,
    mpris_bus_name: String,
    /// Playback identity shown to the shell: a stable track resource, never the signed URL.
    handoff: media::handoff::AppPlaybackHandle,
    pending_media_handoff: Option<PendingMediaHandoff>,
    local_file: Option<crate::local_file::LocalFile>,
    local_file_pick: Option<(tilo_actions::InvocationId, LocalFilePick)>,

    /// The cover being chosen: a browser page hands over the file's contents, not a path.
    cover_pick: Option<ui::portal::ContentsChannel>,
    cover_pick_for: Option<String>,
    /// The off-thread PNG conversion after a portal pick: decoding never runs per frame.
    cover_embed: Option<Receiver<Result<String, String>>>,
    cover_embed_for: Option<String>,

    /// Local playback starts from this device's state; it is never read from or written
    /// to the document.
    playback_loaded: bool,
    bound_track: Option<String>,
    playhead_ms: f64,
    stream_recovery: Option<(String, u64)>,
    last_tick: Instant,

    /// A permutation of `pb.queue` while shuffle is on, rebuilt on demand: the shuffle
    /// flag is synced, the order is not.
    shuffle_order: Vec<usize>,
    /// The xorshift state that builds the shuffle order; tests set it for determinism.
    shuffle_rng: u64,
    premute_volume: Option<u8>,
    new_playlist_track: Option<String>,

    rect: Rect,
    body_rect: Rect,
    body_scroll: ui::widget::ScrollRegion,
    /// An offset asked for before the content height is known, applied after layout.
    scroll_target: Option<f64>,
    frame_theme: Option<Theme>,
    content_height: f64,

    places: NavFeed,
    transport: TransportFeed,
    now_playing: render::NowPlaying,
    playing_now: std::sync::Arc<shell::Playing>,
    bar: Option<WindowBarLink>,
    pane: Option<ui::LocalState<ui::widget::NavPane>>,
    shown_view: Option<View>,

    mouse: Point,
    hits: Vec<Hit>,
    /// How many hits are the body's; the chrome painted over it takes the pointer first.
    body_hits: usize,
    page: Page,
    sources_anchor: Option<Rect>,
    track_tables_drawn: usize,
    media_grids_drawn: usize,
    page_parts_drawn: usize,
    table_tooltip: Option<String>,
    /// Where the queue was toggled from, so closing it returns there.
    dirty: bool,
    toast: Notice,

    /// The context menu: the `ui` primitive, opened at the cursor and drawn last.
    context: ContextMenu,
    /// The system theme's scale from the last frame, so a menu's first event uses the
    /// same geometry the next frame paints.
    context_text_scale: f32,
    /// The action behind each open menu row, parallel to `context.items()`.
    menu_actions: Vec<Action>,
    /// The chosen row index the menu callbacks write (`-1` for none); `Send + Sync`,
    /// which the menu's callback requires.
    pending: Arc<AtomicI64>,

    menu_anchor_row: Option<Rect>,
    ctx_track: Option<String>,
    /// The playlist the menu's row belongs to, so "Remove from playlist" can target it.
    ctx_in_playlist: Option<String>,
    /// The queue position of the menu's row, so "Remove from queue" can target it.
    ctx_queue_index: Option<usize>,
    /// A membership picked from search, kept until its row is visible so the view can
    /// scroll to it and highlight that row.
    revealed_membership: Option<(String, String)>,

    /// The name-input overlay up for a new or renamed playlist.
    name_prompt: Option<NamePrompt>,
    name_input: String,
    name_dialog: Option<NameDlg>,

    lyrics: Vec<LyricsLine>,
    lyrics_for: Option<String>,
    lyrics_synced: bool,

    shot_stage: Option<ShotStage>,
    staged_menu_track: Option<String>,
    staged_row_pointer: Option<usize>,
    staged_facet: Option<String>,

    images: ImageStore,
    art_urls: HashMap<String, String>,
    /// The small cover made for the song's activity once per song, with the cover it
    /// was cut from.
    activity_art: Option<(String, String)>,
    playlist_mosaics: std::cell::RefCell<HashMap<String, (Vec<u64>, vello::peniko::ImageData)>>,
    home: Option<HomeContent>,
    home_asked: Option<Instant>,
    /// A Home fetch that came back empty: the skeleton and the animation loop stop.
    home_failed: bool,
    failed_pages: HashSet<String>,
    pages_asked: HashMap<String, Instant>,
    artist_pages: HashMap<String, ArtistPage>,
    artist_requested: HashSet<String>,
    album_pages: HashMap<String, AlbumPage>,
    album_requested: HashSet<String>,
    playlist_pages: HashMap<String, PlaylistPage>,
    playlist_requested: HashSet<String>,
    /// Whether this instance may open a socket: false for captures and tests.
    allow_network: bool,
    /// Test override for the metered check; `None` asks the real connection.
    force_metered: Option<bool>,

    lyrics_worker: Option<LyricsWorker>,
    lyrics_pending_for: Option<String>,
    lyrics_cache: HashMap<String, (Vec<LyricsLine>, bool)>,
    prefetched_for: Option<String>,

    search_edit_at: Option<Instant>,
    live_query_sent: Option<String>,

    downloads: HashMap<String, crate::audio::OfflineDownload>,
    offline_resolves: HashMap<u64, String>,
    offline_invocation: Option<(
        tilo_actions::InvocationId,
        std::collections::BTreeSet<String>,
    )>,
    offline_results: Vec<tilo_actions::InvocationUpdate>,
    effect_replies: Vec<(tilo_actions::InvocationId, ui::framework::EffectReply)>,
    offline_errors: HashMap<String, String>,

    /// Where library edits go: the dispatcher applies each as the participant's
    /// attributed, undoable action; `None` until the surface is attached.
    dispatcher: Option<ui::framework::Dispatcher<crate::model::Library>>,
    actions: Option<ui::framework::Actions<Library>>,
    // Drop after the audio engine/downloads so test-owned files are no longer open.
    #[cfg(test)]
    _test_cache_dir: tempfile::TempDir,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShotStage {
    TrackMenu,
    NewPlaylistPrompt,
    Skeleton,
    NowPlaying,
    Downloading,
    OfflineCached,
    OfflineError,
    Lyrics,
    MembershipOpen,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NamePrompt {
    NewPlaylist,
    NewSpotifyPlaylist,
    Rename(String),
}

/// The name dialog built from `name_prompt`, torn down when it resolves.
struct NameDlg {
    dialog: ModalDialog<PromptBody>,
    submit: Rc<Cell<bool>>,
    cancel: Rc<Cell<bool>>,
}

impl Music {
    pub(crate) fn queue_catalogue_query(&mut self, query: &str) {
        let query = query.trim();
        if !query.is_empty() && query != self.search_query {
            self.pending_catalogue_query = Some(query.to_owned());
        }
    }
    fn complete_catalogue_search(&mut self, result: Result<(), String>) {
        let Some(invocation) = self.catalogue_invocation.take() else {
            return;
        };
        self.resource_stream_results.push(match result {
            Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            Err(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "catalogue_search_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        });
    }

    pub fn new() -> Self {
        Self::with_bundled_covers(Self::new_with_device_profile(true))
    }

    /// The app for a window nobody plays in: no network, and this device's profile is
    /// neither read nor written.
    pub(crate) fn offline() -> Self {
        let mut app = Self::with_bundled_covers(Self::new_with_device_profile(false));
        app.allow_network = false;
        app.search = render::SearchView::hermetic();
        app
    }

    /// Sample covers, so the grids and the now-playing card show real artwork.
    fn with_bundled_covers(mut app: Self) -> Self {
        for album in crate::seed::sample_library().albums.keys() {
            app.record_cover(album, &crate::seed::sample_cover(album));
        }
        app
    }

    /// A test's app: this device's playback profile is neither read nor
    /// written, the network is allowed.
    #[cfg(test)]
    pub(crate) fn without_profile() -> Self {
        Self::new_with_device_profile(false)
    }

    #[cfg(test)]
    pub(crate) fn disconnect(&mut self) {
        self.allow_network = false;
        self.persist_device_profile = false;
    }

    fn new_with_device_profile(load_device_profile: bool) -> Self {
        // `TILO_MUSIC_VIEW` picks the captured view, e.g. `album:al_glasslight`.
        let query = String::new();
        // A preset search query arms the debounce, so the capture dispatches it after one beat.
        let preset_search_at = (!query.is_empty()).then(ui::time::now);
        let persist_device_profile =
            load_device_profile && !cfg!(test) && crate::storage::device_profile_has_a_place();
        let device_profile = persist_device_profile
            .then(crate::storage::load_device_profile)
            .unwrap_or_default();
        Self::from_device_profile(
            device_profile,
            persist_device_profile,
            query,
            preset_search_at,
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "flat struct-literal initializing every Music field once; splitting it would only relocate the field assignments, not reduce them"
    )]
    fn from_device_profile(
        device_profile: crate::storage::DeviceProfile,
        persist_device_profile: bool,
        query: String,
        preset_search_at: Option<Instant>,
    ) -> Self {
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
        let back_requested = std::rc::Rc::new(std::cell::Cell::new(false));
        Self {
            lib: Library::default(),
            library_heads: None,
            opened: Rc::default(),
            device_cache,
            media_cache,
            media_cache_root,
            persist_device_profile,
            pb: device_profile.playback,
            view: View::Home,
            nav: ui::framework::views::ViewStack::new(View::Home),
            #[cfg(test)]
            search: render::SearchView::local_only(),
            #[cfg(not(test))]
            search: render::SearchView::live(),
            kept: None,
            kept_here: ui::framework::views::KeptViews::default(),
            library_revision: 0,
            restore_selection: None,
            search_query: query,
            pending_catalogue_query: None,
            catalogue_invocation: None,
            page: Page {
                source_buttons: Self::make_source_buttons(),
                ..Page::default()
            },
            spotify_catalogue: device_profile.spotify_catalogue,
            catalogue_connected: device_profile.catalogue_connected
                || device_profile.spotify_catalogue,
            source_invocation: None,
            source_disconnect_pending: false,
            spotify_jobs: HashMap::new(),
            spotify_devices: Vec::new(),
            spotify_device_buttons: Vec::new(),
            spotify_remote_state: serde_json::Value::Null,
            spotify_remote_queue: None,
            spotify_library_offset: 50,
            spotify_library_more: true,
            spotify_menu_controls: Vec::new(),
            control_domain: None,
            source_error: None,
            open_file_layout_visible: None,
            back_requested,
            bar_controls: std::collections::VecDeque::new(),
            bar_entries: ui::framework::BarCommands::new(),
            search_opener: ui::search::SearchOpener::new(),
            audio: AudioEngine::new(),
            worker: None,
            backend_name: "offline (bundled library)".to_string(),
            backend_live: false,
            live_hits: device_profile.queue_catalogue,
            stored_library: Library::default(),
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
            mpris_bus_name: crate::mpris::config().bus_name,
            handoff: media::handoff::AppPlaybackHandle::new(),
            pending_media_handoff: None,
            local_file: device_profile.local_file,
            local_file_pick: None,
            cover_pick: None,
            cover_pick_for: None,
            cover_embed: None,
            cover_embed_for: None,
            playback_loaded: false,
            bound_track: None,
            playhead_ms: 0.0,
            stream_recovery: None,
            last_tick: ui::time::now(),
            shuffle_order: Vec::new(),
            shuffle_rng: seed_rng(),
            premute_volume: None,
            new_playlist_track: None,
            rect: Rect::ZERO,
            body_rect: Rect::ZERO,
            body_scroll: ui::widget::ScrollRegion::vertical(),
            scroll_target: None,
            frame_theme: None,
            content_height: 0.0,
            places: NavFeed::new(),
            transport: TransportFeed::new(),
            now_playing: render::NowPlaying::default(),
            playing_now: std::sync::Arc::default(),
            bar: None,
            pane: None,
            shown_view: None,
            mouse: Point::ZERO,
            hits: Vec::new(),
            body_hits: 0,
            sources_anchor: None,
            track_tables_drawn: 0,
            media_grids_drawn: 0,
            page_parts_drawn: 0,
            table_tooltip: None,
            dirty: true,
            toast: Notice::default(),
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
            staged_row_pointer: None,
            staged_facet: None,
            images: ImageStore::new(&image_fetcher()),
            art_urls: HashMap::new(),
            activity_art: None,
            playlist_mosaics: std::cell::RefCell::default(),
            home: None,
            home_asked: None,
            home_failed: false,
            failed_pages: HashSet::new(),
            pages_asked: HashMap::new(),
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
            effect_replies: Vec::new(),
            offline_errors: HashMap::new(),
            // A test drives the surface on a document it builds; its edits land there.
            #[cfg(test)]
            dispatcher: Some(ui::framework::testing::dispatcher(
                <crate::MusicShell as ui::framework::ResourceApp>::ID,
            )),
            #[cfg(not(test))]
            dispatcher: None,
            actions: None,
            #[cfg(test)]
            _test_cache_dir: test_cache_dir,
        }
    }

    /// A published destination id back to its view; the resource layer admits only
    /// `NAVIGATION_VIEWS`.
    fn top_level_view(id: &str) -> Option<View> {
        match id {
            "home" => Some(View::Home),
            "recommendations" => Some(View::Recommendations),
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

    /// Refresh the catalogue, playlists and cache from the document every `sync`, so a
    /// peer's edit appears without disturbing local playback.
    fn reload_library(&mut self, state: &AutoCommit) {
        let heads = (
            state.base_heads(),
            automerge::transaction::Transactable::pending_ops(state),
        );
        if self.library_heads.as_ref() == Some(&heads) {
            return;
        }
        // A library that does not read is not an empty one: what last read whole stays.
        let next_library = match <crate::model::Library as ui::tilo_resource::Resource>::read(state)
        {
            Ok(library) => library,
            Err(error) => {
                tracing::warn!(%error, "music: the library does not read");
                self.library_heads = Some(heads);
                return;
            }
        };
        self.stored_library.clone_from(&next_library);
        self.library_revision = self.library_revision.wrapping_add(1);
        self.lib = next_library;
        self.inject_live_hits();
        self.library_heads = Some(heads);
        self.sync_playlist_covers();
    }

    /// Kept for the render loop: it can never manufacture a cached completion.
    fn advance_downloads(&mut self, _state: &mut ui::widget::Edits<'_>) {
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

    fn sync_playlist_covers(&mut self) {
        for pl in self.lib.playlists() {
            if !pl.cover.is_empty() {
                self.art_urls.insert(pl.id.clone(), pl.cover.clone());
            }
        }
    }

    /// Adopt the shared library; playback stays this device's.
    fn reload(&mut self, state: &AutoCommit) {
        self.reload_library(state);
        self.bound_track = self.pb.current.clone();
        self.playhead_ms = self.pb.position_ms as f64;
    }

    fn persist_playback(&mut self, _state: &mut ui::widget::Edits<'_>) {
        // A media key can name a track before a queue exists: settle that before audio.
        if let Some(current) = self.pb.current.clone() {
            if self.pb.queue.get(self.pb.queue_index) != Some(&current) {
                if let Some(index) = self.pb.queue.iter().position(|id| id == &current) {
                    self.pb.queue_index = index;
                } else {
                    self.pb.queue = vec![current];
                    self.pb.queue_index = 0;
                }
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
        self.prepare_spotify_queue();
        self.report_device_profile_error();
    }

    fn persist_device_profile(&self) -> Result<(), String> {
        if !self.persist_device_profile {
            return Ok(());
        }
        crate::storage::save_device_profile(&crate::storage::DeviceProfile {
            spotify_catalogue: self.spotify_catalogue,
            catalogue_connected: self.catalogue_connected,
            local_file: self.kept_local_file(),
            playback: self.pb.clone(),
            queue_catalogue: self.queue_catalogue(),
        })
        .map_err(|error| format!("could not save local Music state: {error}"))
    }

    fn report_device_profile_error(&mut self) {
        if let Err(error) = self.persist_device_profile() {
            self.playback_error = Some(error.clone());
            self.notify(ToastKind::Error, error);
        }
    }

    fn queue_catalogue(&self) -> Vec<CatalogueTrack> {
        self.pb
            .queue
            .iter()
            .filter_map(|id| {
                let track = self.lib.track(id)?;
                Some(CatalogueTrack {
                    track: track.clone(),
                    album: self.lib.albums.get(&track.album_id)?.clone(),
                    artist: self.lib.artists.get(&track.artist_id)?.clone(),
                })
            })
            .collect()
    }

    fn notify(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toast.show(text, kind);
        self.toast.publish();
    }

    fn update_device_cache(&mut self, id: &str, state: CacheState) {
        if matches!(state, CacheState::Streamed) {
            self.device_cache.remove(id);
        } else {
            self.device_cache.insert(id.to_string(), state);
        }
    }

    pub(crate) fn cache_state(&self, id: &str) -> CacheState {
        self.device_cache.get(id).copied().unwrap_or_default()
    }

    /// The one door for a library edit: checked against the participant's permissions,
    /// attributed, undoable and replayed on peers. Callers reload afterwards.
    fn perform_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::model::Library>,
    ) {
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        if let Err(error) = dispatcher.submit_now(state, submit) {
            self.notify(ToastKind::Error, error.to_string());
        }
    }

    /// Show the shared library, keeping this device's cache; an empty projection keeps
    /// what is there.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::model::Library) {
        self.stored_library.clone_from(state);
        self.library_revision = self.library_revision.wrapping_add(1);
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
        self.lib = state.clone();
        self.inject_live_hits();
        self.sync_playlist_covers();
    }

    pub(crate) fn attach_dispatcher(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::model::Library>,
    ) {
        self.dispatcher = Some(dispatcher);
    }

    pub(crate) fn attach_actions(&mut self, actions: ui::framework::Actions<Library>) {
        self.actions = Some(actions);
    }

    /// The rows this window shows besides the library's, from playback and this device.
    pub(crate) fn music_rows(&self) -> crate::actions::MusicRows<'_, Self> {
        crate::actions::MusicRows {
            lib: &self.lib,
            playback: &self.pb,
            device: self,
            local_file: self.local_file_resource(),
        }
    }

    /// The row `id` this surface shows now, or nothing when the address is stale.
    pub(crate) fn publish_resource(&self, id: &str) -> Option<tilo_actions::ResourceSpec> {
        self.music_rows()
            .row(id)
            .map(ui::framework::LiveRow::into_resource)
            .or_else(|| <Library as ui::tilo_resource::Resource>::child(&self.lib, id))
    }

    /// The catalogue result's metadata, for the effect that imports it.
    pub(crate) fn catalogue_import(
        &self,
        track: &str,
    ) -> Result<ui::tilo_resource::Invoke<crate::model::Library>, String> {
        let hit = self
            .live_hits
            .iter()
            .find(|hit| hit.track.id == track)
            .ok_or_else(|| "Catalogue song is no longer available".to_string())?;
        Ok(crate::model::Library::IMPORT_TRACK.with(hit.clone()))
    }

    /// This device's rows: the player, the queue, the open file and offline copies.
    pub(crate) fn device_rows(&self) -> Vec<ui::framework::LiveRow> {
        let mut rows = self.music_rows().all();
        rows.extend(
            crate::recommendations::resources(&self.lib)
                .into_iter()
                .map(ui::framework::LiveRow::from_resource),
        );
        rows.push(self.source_row());
        rows.extend(self.spotify_device_rows());
        rows.extend(self.spotify_remote_rows());
        // A catalogue track is not in the document: publish its admitted live row so it
        // plays like a library one.
        let mut tracks: Vec<_> = self
            .live_hits
            .iter()
            .filter(|hit| !self.stored_library.tracks.contains_key(&hit.track.id))
            .map(|hit| crate::actions::track_resource_id(&hit.track.id))
            .collect();
        tracks.sort();
        tracks.dedup();
        rows.extend(
            tracks
                .iter()
                .take(crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT)
                .filter_map(|id| self.device_row(id)),
        );
        rows.extend(
            self.catalogue_rows()
                .into_iter()
                .take(crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT),
        );
        let offline_rows = self
            .device_cache
            .iter()
            .filter(|(_, state)| matches!(state, CacheState::Cached))
            .take(crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT)
            .filter_map(|(id, _)| self.offline_row(id));
        for row in offline_rows {
            if !rows.iter().any(|existing| existing.id() == row.id()) {
                rows.push(row);
            }
        }
        rows
    }

    pub(crate) fn device_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        if id.starts_with("music.spotify-remote:") {
            return self
                .spotify_remote_rows()
                .into_iter()
                .find(|row| row.id() == id);
        }
        if id == "music.source:catalogue" {
            return Some(self.source_row());
        }
        if id.starts_with("music.spotify-device:") {
            return self
                .spotify_device_rows()
                .into_iter()
                .find(|row| row.id() == id);
        }
        if let Some(row) = self.catalogue_rows().into_iter().find(|row| row.id() == id) {
            return Some(row);
        }
        if let Some(row) = self
            .catalogue_resources()
            .into_iter()
            .find(|row| row.id() == id)
        {
            return Some(row);
        }
        if id.starts_with("music.recommendation") {
            return crate::recommendations::resource_by_id(&self.lib, id)
                .map(ui::framework::LiveRow::from_resource);
        }
        if let Some(track) = id.strip_prefix("music.track:")
            && !self.stored_library.tracks.contains_key(track)
        {
            // A song not in the library yet: liking it imports the song first.
            return self
                .live_hits
                .iter()
                .any(|hit| hit.track.id == track)
                .then(|| {
                    self.publish_resource(id)
                        .map(ui::framework::LiveRow::from_resource)
                })
                .flatten();
        }
        // The player, the queue, the open file and the library's rows; places and albums the
        // framework resolves itself.
        self.publish_resource(id)
            .map(ui::framework::LiveRow::from_resource)
    }

    fn source_status(&self) -> String {
        if self.source_invocation.is_some() {
            crate::i18n::t("music-source-connecting")
        } else if !self.spotify_jobs.is_empty() {
            crate::i18n::t("music-spotify-updating")
        } else {
            self.source_error.clone().unwrap_or_else(|| {
                self.spotify_remote_state
                    .get("device")
                    .and_then(|device| device.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(
                        || self.backend_name.clone(),
                        |name| {
                            crate::i18n::t_args("music-spotify-output", &[("device", name.into())])
                        },
                    )
            })
        }
    }

    fn source_row(&self) -> ui::framework::LiveRow {
        use crate::actions::MusicEffect;
        ui::framework::LiveRow::new(
            "music.source:catalogue",
            "music.source",
            self.source_status(),
        )
        .effect(&MusicEffect::Sources)
        .effect(&MusicEffect::ConnectSpotifyLibrary)
        .effect(&MusicEffect::ConnectSpotify)
        .effect(&MusicEffect::ConnectSpotifyPlayback)
        .effect(&MusicEffect::SpotifyDevices)
        .effect(&MusicEffect::SpotifyQueue)
        .effect(&MusicEffect::SpotifyLibraryMore)
        .effect(&MusicEffect::NewSpotifyPlaylist)
        .effect(&MusicEffect::DisconnectSpotify)
        .effect(&MusicEffect::BrowseSpotify)
        .effect(&MusicEffect::BrowseYouTube)
    }

    fn offline_row(&self, track: &str) -> Option<ui::framework::LiveRow> {
        self.lib.track(track)?;
        Some(
            ui::framework::LiveRow::new(
                crate::actions::track_resource_id(track),
                "music.track",
                self.track_label(track),
            )
            .effect(&crate::actions::MusicEffect::RemoveOffline {
                tracks: vec![track.to_owned()],
            }),
        )
    }

    fn track_label(&self, track_id: &str) -> String {
        match self.playback_track(track_id) {
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

    /// Only the position the decoder saw at the end counts as played; metadata cannot.
    fn observed_position_ms(&self) -> Option<u64> {
        self.audio.position_ms().or_else(|| {
            self.local_file
                .as_ref()
                .filter(|file| {
                    !self.pb.playing
                        && !self.audio.has_stream()
                        && self.pb.current.as_deref() == Some(&file.track.id)
                })
                .and_then(|file| file.observed_duration_ms)
                .filter(|duration| self.playhead_ms.max(0.0) as u64 == *duration)
        })
    }

    fn observed_duration_ms(&self) -> Option<u64> {
        self.audio.duration_ms().or_else(|| {
            self.local_file
                .as_ref()
                .filter(|file| self.pb.current.as_deref() == Some(&file.track.id))
                .and_then(|file| file.observed_duration_ms)
        })
    }

    fn current_duration_ms(&self) -> u64 {
        if self
            .pb
            .current
            .as_deref()
            .is_some_and(crate::stream::valid_native_track)
            && let Some(duration) = self.audio.duration_ms()
        {
            return duration;
        }
        if self
            .local_file
            .as_ref()
            .is_some_and(|file| self.pb.current.as_deref() == Some(&file.track.id))
        {
            return self.observed_duration_ms().unwrap_or(0);
        }
        self.pb
            .current
            .as_ref()
            .and_then(|id| self.playback_track(id))
            .map_or(0, super::model::Track::duration_ms)
    }

    /// The track's own cover, else its album's: album hits often carry only album art.
    fn best_cover_id(&self, track_id: &str, album_id: &str) -> String {
        if self.art_urls.contains_key(track_id) {
            track_id.to_string()
        } else if !album_id.is_empty() && self.art_urls.contains_key(album_id) {
            album_id.to_string()
        } else {
            track_id.to_string()
        }
    }

    /// Bring up MPRIS; where it is unsupported it installs the inert stub, so no code
    /// path branches on whether it is there.
    pub fn enable_mpris(&mut self) {
        let snap = self.mpris_snapshot();
        let config = crate::mpris::config();
        self.mpris_bus_name.clone_from(&config.bus_name);
        self.mpris = MprisHandle::spawn(config, snap);
    }

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

    /// Push the player state to MPRIS; the handle dedupes, so this is cheap each frame.
    fn publish_mpris(&mut self) {
        if self.pb.current.is_some() {
            let snap = self.mpris_snapshot();
            self.audio
                .set_now_playing(&crate::mpris::now_playing_of(&snap));
        }
        if !self.mpris.is_active() {
            return;
        }
        let snap = self.mpris_snapshot();
        self.mpris.publish(snap);
    }

    fn publish_handoff(&self) {
        if self
            .pb
            .current
            .as_deref()
            .is_some_and(|id| id.starts_with("spotify:"))
        {
            self.handoff.clear();
            return;
        }
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
            controller_bus_name: Some(&self.mpris_bus_name),
        });
    }

    /// This device's playing song as the app's activity: the bar's pill and its card.
    fn publish_activity(&mut self) {
        let Some(track_id) = self.pb.current.clone() else {
            crate::activity::publish(None);
            return;
        };
        let Some(track) = self.playback_track(&track_id) else {
            crate::activity::publish(None);
            return;
        };
        let (title, album_id, artist_id) = (
            track.title.clone(),
            track.album_id.clone(),
            track.artist_id.clone(),
        );
        let artwork = self.activity_artwork(&track_id, &album_id);
        let song = crate::activity::Playing {
            track: track_id,
            title,
            artist: self.lib.artist_name(&artist_id).to_string(),
            playing: self.pb.playing,
            position_ms: self.playhead_ms.max(0.0),
            length_ms: self.current_duration_ms() as f64,
            artwork,
        };
        crate::activity::publish(Some(&song));
    }

    /// The card's cover as a small `data:` picture, cut down from the decoded one.
    fn activity_artwork(&mut self, track_id: &str, album_id: &str) -> String {
        let cover = self.best_cover_id(track_id, album_id);
        let Some(url) = self.art_urls.get(&cover).cloned() else {
            return String::new();
        };
        if let Some((kept, art)) = &self.activity_art
            && *kept == url
        {
            return art.clone();
        }
        let art = cover_picture(&url, &self.images);
        if !art.is_empty() {
            self.activity_art = Some((url, art.clone()));
        }
        art
    }

    fn drain_activity(&mut self, state: &mut ui::widget::Edits<'_>) {
        for pressed in crate::activity::take_commands() {
            // A press names the song it was made on: once another plays, it is
            // not this song's.
            if pressed
                .activity_id
                .as_deref()
                .is_some_and(|id| self.pb.current.as_deref() != Some(id))
            {
                continue;
            }
            match pressed.command.as_str() {
                "play-pause" => self.dispatch(Action::TogglePlay, state),
                "previous" => self.dispatch(Action::Prev, state),
                "next" => self.dispatch(Action::Next, state),
                "seek" => {
                    if let Some(fraction) = pressed.fraction {
                        self.dispatch(Action::SeekFrac(fraction), state);
                    }
                }
                _ => {}
            }
        }
    }

    fn apply_pending_media_handoff(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(pending) = self.pending_media_handoff.take() else {
            return;
        };
        let Some(crate::search_type::DeepLink::Track(track_id)) =
            crate::search_type::DeepLink::parse(&pending.uri)
        else {
            return;
        };
        if !self.lib.tracks.contains_key(&track_id) {
            // The workspace document may still be hydrating. Keep the exact request
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
    fn drain_mpris(&mut self, state: &mut ui::widget::Edits<'_>) {
        // The platform player's own system controls (a page's Media
        // Session) ask as MPRIS does.
        let pressed = self.audio.take_now_playing_actions();
        let commands = self.mpris.drain_commands();
        let pressed = pressed.into_iter().map(crate::mpris::command_for);
        for cmd in pressed.chain(commands) {
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
    fn drain_cover_pick(&mut self, _state: &mut ui::widget::Edits<'_>) {
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
            ui::portal::ContentsOutcome::Picked(files) => {
                let (Some(playlist_id), Some(file)) = (playlist_id, files.into_iter().next())
                else {
                    return;
                };
                self.cover_embed_for = Some(playlist_id);
                self.cover_embed = Some(embed_cover_off_thread(file.bytes));
                self.notify(ToastKind::Info, "Preparing the playlist cover");
            }
            ui::portal::ContentsOutcome::Cancelled => {}
            ui::portal::ContentsOutcome::Failed(reason) => {
                tracing::warn!(%reason, "the cover picker failed");
                self.notify(
                    ToastKind::Error,
                    "Couldn\u{2019}t open the image. Try another one.",
                );
            }
        }
    }

    fn drain_cover_embed(&mut self, state: &mut ui::widget::Edits<'_>) {
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
                self.perform_action(
                    state,
                    crate::model::Library::playlist_paths(&playlist_id)
                        .cover
                        .replace(url.clone()),
                );
                self.reload_library(state);
                self.art_urls.insert(playlist_id, url);
            }
            (_, Err(reason)) => {
                tracing::warn!(%reason, "the playlist cover could not be prepared");
                self.notify(
                    ToastKind::Error,
                    "Couldn\u{2019}t use that image as a cover. Try another one.",
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
        self.expire_spotify_jobs();
        if let Some(track) = self.audio.take_spotify_transition() {
            self.advance(true);
            if self.pb.current.as_deref() != Some(track.as_str()) {
                self.audio.stop();
                self.pb.playing = false;
                self.playback_error = Some(crate::i18n::t("music-spotify-queue-changed"));
            }
        }
        self.prepare_spotify_queue();
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
            if errored && self.pb.playing && self.resume_cut_stream(stream_cut::StreamCut::Failed) {
                return;
            }
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
                self.notify(ToastKind::Error, message.clone());
                self.pb.playing = false;
                self.audio.stop();
                self.fail_resource_stream("music.audio_start_failed", &message, None);
                return;
            }
            if !self.pb.playing || self.pb.current.is_none() {
                return;
            }
            #[cfg(feature = "audio")]
            self.settle_resource_stream_after_rendered_frames(self.audio.frames_rendered());
            self.observe_local_duration();
            if self.audio.reached_eos() {
                // A stream that ends well short of the song was cut off by
                // its source: pick it up where it stopped. Otherwise the song
                // is over and the next one starts.
                if self.resume_cut_stream(stream_cut::StreamCut::Ended) {
                    return;
                }
                // The song is over; a repeat of it may resume its own cuts.
                self.stream_recovery = None;
                self.audio.stop();
                self.auto_advance();
                return;
            }
            if let Some(pos) = self.audio.position_ms() {
                self.playhead_ms = pos as f64;
            }
        }
        // Catalogue duration is display metadata, not an output completion
        // receipt. Only the decoder's EOS (including governed output drain)
        // above may advance; crossing the duration can still leave PCM queued.
    }

    /// Auto-advance to the next track *and* start streaming it. Used by the
    /// transport tick when the current stream ends. Advancing without also
    /// resolving the next track would silently change the "now playing" track
    /// but leave it soundless, so this pairs the two. Carries no document
    /// handle (tick has none); the new position persists on the next real
    /// event.
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
            self.stream_recovery = None;
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
        if self.pb.shuffle {
            self.ensure_shuffle_order();
        }
        let at = (self.pb.queue_index + 1).min(self.pb.queue.len());
        self.pb.queue.insert(at, id);
        shuffle_inserted(&mut self.shuffle_order, at);
        shuffle_put_next(&mut self.shuffle_order, at, self.pb.queue_index);
    }

    /// Remove the queue entry at an absolute index, keeping `queue_index`
    /// pointed at the same logical track (or the next one if the current was
    /// removed).
    fn queue_remove(&mut self, index: usize) {
        if index >= self.pb.queue.len() {
            return;
        }
        self.pb.queue.remove(index);
        let slot = shuffle_removed(&mut self.shuffle_order, index);
        if index < self.pb.queue_index {
            self.pb.queue_index = self.pb.queue_index.saturating_sub(1);
        } else if index == self.pb.queue_index {
            self.pb.position_ms = 0;
            self.playhead_ms = 0.0;
            // The current track was dropped: clamp and re-bind to whatever now
            // sits at this slot, or to the next shuffled song, which is what
            // would have played after it (or stop if the queue emptied).
            if self.pb.queue.is_empty() {
                self.pb.current = None;
                self.pb.playing = false;
                self.pb.queue_index = 0;
            } else {
                let shuffled = slot.and_then(|slot| self.shuffle_order.get(slot).copied());
                self.pb.queue_index = shuffled
                    .unwrap_or(self.pb.queue_index)
                    .min(self.pb.queue.len() - 1);
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
        let item = self.pb.queue.remove(from);
        self.pb.queue.insert(to, item);
        // Track the occurrence, not its song id: a queue can hold duplicates.
        // The shuffled order follows its songs to their new places too.
        self.pb.queue_index = moved_index(self.pb.queue_index, from, to);
        for index in &mut self.shuffle_order {
            *index = moved_index(*index, from, to);
        }
    }

    /// Build the name-prompt dialog from `name_prompt` + the staged
    /// `name_input` (which seeds the field for a rename or a `--shots` frame).
    fn ensure_name_dialog(&mut self) {
        if self.name_dialog.is_some() {
            return;
        }
        let Some(kind) = self.name_prompt.clone() else {
            return;
        };
        let (title, primary, guidance) = match kind {
            NamePrompt::NewPlaylist => ("New playlist".into(), "Create".into(), String::new()),
            NamePrompt::NewSpotifyPlaylist => (
                crate::i18n::t("music-spotify-create-playlist"),
                crate::i18n::t("music-spotify-create"),
                crate::i18n::t("music-spotify-private-help"),
            ),
            NamePrompt::Rename(ref id) if id.starts_with("spotify:") => (
                crate::i18n::t("music-spotify-rename-playlist"),
                crate::i18n::t("music-spotify-save-name"),
                crate::i18n::t("music-spotify-ownership-help"),
            ),
            NamePrompt::Rename(_) => ("Rename playlist".into(), "Save".into(), String::new()),
        };
        let body = PromptBody::new(&guidance, "Playlist name").with_initial(&self.name_input);
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
                    size: Size::from_lengths(rect.width() as f32, rect.height() as f32),
                    ..Default::default()
                },
                &[node],
            )
            .expect("a single freshly-created taffy node always attaches to a fresh tree");
        let _ = ui::widget::solve_layout(
            &mut taffy,
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
        state: &mut ui::widget::Edits<'_>,
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
                    state: state.reborrow(),
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
    fn submit_name_prompt(&mut self, state: &mut ui::widget::Edits<'_>) {
        let value = self
            .name_dialog
            .as_ref()
            .map(|nd| nd.dialog.content().value().to_string())
            .unwrap_or_default();
        self.name_input = value;
        let commit = match self.name_prompt.clone() {
            Some(NamePrompt::NewPlaylist) => Action::CreatePlaylist,
            Some(NamePrompt::NewSpotifyPlaylist) => {
                Action::SpotifyEffect(crate::actions::MusicEffect::SpotifyCreatePlaylist {
                    name: self.name_input.trim().into(),
                })
            }
            Some(NamePrompt::Rename(id)) => Action::CommitRename(id),
            None => Action::None,
        };
        self.dispatch(commit, state);
    }

    fn recommendation_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        edit: ui::tilo_resource::Edit<crate::model::Library>,
    ) {
        self.perform_action(state, edit);
        self.reload_library(state);
    }

    fn dispatch(&mut self, action: Action, state: &mut ui::widget::Edits<'_>) {
        self.dirty = true;
        match action {
            Action::Recommendation(edit) => self.recommendation_action(state, edit),
            Action::Nav(v) => self.on_nav(v),
            Action::Back => self.on_back(),
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
            Action::ToggleMute => self.on_toggle_mute(state),
            Action::PickPlaylistCover(playlist_id) => self.on_pick_playlist_cover(playlist_id),
            Action::AddToQueue(id) => self.on_add_to_queue(&id, state),
            Action::PlayNext(id) => self.on_play_next(&id, state),
            Action::RemoveFromQueue(index) => self.on_remove_from_queue(index, state),
            Action::MoveQueueItem { from, to } => self.on_move_queue_item(from, to, state),
            Action::ToggleLike(id) => self.on_toggle_like(&id, state),
            Action::ToggleSaveAlbum(id) => self.on_toggle_save_album(&id, state),
            Action::SetOffline { track, keep } => self.on_set_offline(track, keep),
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
            Action::BeginNewSpotifyPlaylist => {
                self.on_begin_new_playlist(None);
                self.name_prompt = Some(NamePrompt::NewSpotifyPlaylist);
            }
            Action::BeginNewPlaylistFor(track) => self.on_begin_new_playlist(Some(track)),
            Action::CreatePlaylist => self.on_create_playlist(state),
            Action::OpenPlaylistMenu(id) => self.open_playlist_menu(&id),
            Action::BeginRenamePlaylist(id) => self.on_begin_rename_playlist(id),
            Action::CommitRename(id) => self.on_commit_rename(&id, state),
            Action::DeletePlaylist(id) => self.on_delete_playlist(id, state),
            Action::OpenTrackMenu {
                track,
                in_playlist,
                queue_index,
            } => self.open_track_menu(track, in_playlist.as_deref(), queue_index),
            Action::OpenAddToPlaylist(track) => self.open_add_to_playlist_menu(&track),
            Action::SpotifyLibrary { track, save } => {
                self.queue_spotify_library_control(&track, save);
            }
            Action::SpotifyEffect(effect) => {
                self.queue_spotify_effect_control(&effect);
                if matches!(
                    effect,
                    crate::actions::MusicEffect::SpotifyCreatePlaylist { .. }
                ) {
                    self.on_close_name_prompt();
                }
            }
            Action::OpenSpotifyPlaylistMenu(playlist) => self.open_spotify_playlist_menu(&playlist),
            Action::ToggleQueue => self.on_toggle_queue(),
            Action::CloseNamePrompt => self.on_close_name_prompt(),
            Action::SetQuery(query) => self.on_set_query(query),
            Action::OpenFacet(key) => self.open_facet(&key),
            Action::KeepQuery => self.on_keep_query(),
            Action::ForgetKept(id) => self.on_forget_kept(&id),
            Action::ShowControl { home, action } => self.on_show_control(&home, *action),
            Action::RetryRemote(source) => {
                if self.search.engine.retry(&source) {
                    self.dirty = true;
                }
            }
            Action::None => {}
        }
    }

    /// Keep `track` on this device, or let it go back to streaming.
    fn on_set_offline(&mut self, track: String, keep: bool) {
        if keep {
            if let Err(error) = self.request_offline_download(&track) {
                self.offline_errors.insert(track, error);
                self.notify(ToastKind::Error, OFFLINE_FAILED);
            }
            return;
        }
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

    fn track_title(&self, track_id: &str) -> String {
        self.playback_track(track_id)
            .map_or_else(|| track_id.to_string(), |t| t.title.clone())
    }

    fn on_shuffle_play(&mut self, queue: Arc<Vec<String>>, state: &mut ui::widget::Edits<'_>) {
        if queue.is_empty() {
            return;
        }
        let start = (self.next_rand() % queue.len() as u64) as usize;
        self.pb.shuffle = true;
        self.on_play(queue, start, state);
        self.rebuild_shuffle_order();
        self.persist_playback(state);
    }

    fn on_set_repeat(&mut self, mode: RepeatMode, state: &mut ui::widget::Edits<'_>) {
        self.pb.repeat = mode;
        self.persist_playback(state);
    }

    fn on_cycle_repeat(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.pb.repeat = self.pb.repeat.next();
        self.persist_playback(state);
    }

    fn on_remove_from_queue(&mut self, index: usize, state: &mut ui::widget::Edits<'_>) {
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

    fn on_move_queue_item(&mut self, from: usize, to: usize, state: &mut ui::widget::Edits<'_>) {
        self.queue_move(from, to);
        self.persist_playback(state);
    }

    fn on_remove_from_playlist(
        &mut self,
        playlist: &str,
        track: &str,
        state: &mut ui::widget::Edits<'_>,
    ) {
        self.perform_action(
            state,
            crate::model::Library::remove_from_playlist(playlist, track),
        );
        self.reload_library(state);
    }

    fn on_move_playlist_item(
        &mut self,
        playlist: &str,
        from: usize,
        to: usize,
        state: &mut ui::widget::Edits<'_>,
    ) {
        let Some(track) = self
            .lib
            .playlist(playlist)
            .and_then(|playlist| playlist.tracks.at(from))
            .map(|(_, track)| track.clone())
        else {
            return;
        };
        self.perform_action(
            state,
            crate::model::Library::move_in_playlist(playlist, &track, to),
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
        if v.is_section() {
            if v != self.view {
                self.open_section(v);
            }
        } else {
            self.open_view(v);
        }
    }

    fn on_back(&mut self) {
        self.go_back();
    }

    fn on_browse_genre(&mut self, genre: &str) {
        // A Browse tile opens the platform search for Music, prefilled with
        // the genre; the live catalogue answers through the search provider.
        self.search_opener
            .open(ui::search::SearchRequest::this_app().with_query(genre));
    }

    fn on_play(
        &mut self,
        queue: Arc<Vec<String>>,
        index: usize,
        state: &mut ui::widget::Edits<'_>,
    ) {
        self.pb.queue = Arc::unwrap_or_clone(queue);
        // A fresh queue invalidates any prior shuffle permutation; it's
        // rebuilt (current-first) on the next shuffled advance.
        self.shuffle_order.clear();
        self.play_index(index);
        if let Some(id) = self.pb.current.clone() {
            self.import_live_track(&id, state);
            self.start_stream(&id);
        }
        self.persist_playback(state);
    }

    fn on_toggle_play(&mut self, state: &mut ui::widget::Edits<'_>) {
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

    fn on_next(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.advance(false);
        if let Some(id) = self.pb.current.clone() {
            self.import_live_track(&id, state);
            self.start_stream(&id);
        }
        self.persist_playback(state);
    }

    fn on_prev(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.previous();
        if let Some(id) = self.pb.current.clone() {
            self.import_live_track(&id, state);
            self.start_stream(&id);
        }
        self.persist_playback(state);
    }

    fn on_toggle_shuffle(&mut self, state: &mut ui::widget::Edits<'_>) {
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

    fn on_seek_frac(&mut self, f: f64, state: &mut ui::widget::Edits<'_>) {
        let dur = self.current_duration_ms();
        self.playhead_ms = (f.clamp(0.0, 1.0) * dur as f64).max(0.0);
        self.audio.seek_ms(self.playhead_ms as u64);
        self.persist_playback(state);
    }

    fn on_volume_frac(&mut self, f: f64, state: &mut ui::widget::Edits<'_>) {
        self.pb.volume = (f.clamp(0.0, 1.0) * 100.0).round() as u8;
        // Dragging the volume above zero clears a mute.
        if self.pb.volume > 0 {
            self.premute_volume = None;
        }
        self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
        self.persist_playback(state);
    }

    fn on_toggle_mute(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.pb.volume = self.toggled_mute();
        self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
        self.persist_playback(state);
    }

    /// The song that plays on this device now, if any.
    pub(crate) fn playing_track(&self) -> Option<String> {
        self.pb.current.clone()
    }

    /// The volume a mute toggle leaves: zero, keeping the level to come
    /// back to, or that level again (a sensible one if there is none).
    fn toggled_mute(&mut self) -> u8 {
        if self.pb.volume > 0 {
            self.premute_volume = Some(self.pb.volume);
            0
        } else {
            self.premute_volume.take().filter(|&v| v > 0).unwrap_or(80)
        }
    }

    /// The playback `effect` (next, previous or mute) steps to, by the
    /// steps the person's own controls take: next and previous follow this
    /// device's shuffle order, previous starts a song that has played a
    /// while over, and mute keeps the level to come back to. The playback
    /// itself is left for the effect to apply.
    pub(crate) fn stepped_playback(
        &mut self,
        effect: &crate::actions::MusicEffect,
    ) -> crate::model::Playback {
        use crate::actions::MusicEffect;
        let before = self.pb.clone();
        let (playhead, bound) = (self.playhead_ms, self.bound_track.clone());
        match effect {
            MusicEffect::Next => self.advance(false),
            MusicEffect::Previous => self.previous(),
            MusicEffect::ToggleMute => self.pb.volume = self.toggled_mute(),
            _ => {}
        }
        let mut next = std::mem::replace(&mut self.pb, before);
        if matches!(effect, MusicEffect::Next | MusicEffect::Previous) {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a position within the song, in whole milliseconds"
            )]
            let position_ms = self.playhead_ms.max(0.0) as u64;
            next.position_ms = position_ms;
        }
        self.playhead_ms = playhead;
        self.bound_track = bound;
        next
    }

    fn on_pick_playlist_cover(&mut self, playlist_id: String) {
        // Route through the toolkit's native host file chooser.
        // The pick is async; the frame loop drains the
        // outcome and applies the cover.
        self.cover_pick_for = Some(playlist_id);
        self.cover_pick = Some(cover_request().send_contents());
        self.notify(ToastKind::Info, "Choose a cover image");
    }

    fn on_add_to_queue(&mut self, id: &str, state: &mut ui::widget::Edits<'_>) {
        self.pb.queue.push(id.to_string());
        shuffle_inserted(&mut self.shuffle_order, self.pb.queue.len() - 1);
        self.notify(ToastKind::Success, "Added to the queue");
        self.persist_playback(state);
    }

    fn on_play_next(&mut self, id: &str, state: &mut ui::widget::Edits<'_>) {
        let starts_queue = self.pb.queue.is_empty() || self.pb.current.is_none();
        self.queue_play_next(id.to_string());
        self.notify(ToastKind::Success, "Playing next");
        if starts_queue {
            self.start_stream(id);
        }
        self.persist_playback(state);
    }

    fn on_toggle_like(&mut self, id: &str, state: &mut ui::widget::Edits<'_>) {
        // A catalogue song (a search result) joins the library with its
        // album and artist first: it is liked as the same song everywhere.
        self.import_live_track(id, state);
        let liked = !self.lib.is_liked(id);
        self.perform_action(state, crate::model::Library::like(id, liked));
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

    fn on_toggle_save_album(&mut self, id: &str, state: &mut ui::widget::Edits<'_>) {
        let saved = !self.lib.is_album_saved(id);
        self.perform_action(state, crate::model::Library::save_album(id, saved));
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

    fn on_toggle_follow(&mut self, id: &str, state: &mut ui::widget::Edits<'_>) {
        if id.starts_with("spotify:artist:") {
            let effect = if self.artist_followed(id) {
                crate::actions::MusicEffect::SpotifyUnsave { uri: id.into() }
            } else {
                crate::actions::MusicEffect::SpotifySave { uri: id.into() }
            };
            self.queue_spotify_effect_control(&effect);
            self.dirty = true;
            return;
        }
        let followed = !self.lib.is_artist_followed(id);
        self.perform_action(state, crate::model::Library::follow_artist(id, followed));
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

    fn on_add_to_playlist(
        &mut self,
        playlist: &str,
        track: &str,
        state: &mut ui::widget::Edits<'_>,
    ) {
        let Some(edit) = self.lib.add_to_playlist(playlist, track) else {
            return;
        };
        self.perform_action(state, edit);
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

    fn on_create_playlist(&mut self, state: &mut ui::widget::Edits<'_>) {
        let name = if self.name_input.trim().is_empty() {
            "New playlist".to_string()
        } else {
            self.name_input.trim().to_string()
        };
        // If this create came from "Add to playlist… → New playlist…", the
        // new playlist holds the target track from the start: one action.
        let added_track = self.new_playlist_track.take();
        let before: HashSet<String> = self.lib.playlists().map(|p| p.id.clone()).collect();
        self.perform_action(
            state,
            crate::model::Library::CREATE_PLAYLIST.with(crate::resource::library::CreatePlaylist {
                name: name.clone(),
                track: added_track.clone(),
            }),
        );
        self.reload_library(state);
        let Some(id) = self
            .lib
            .playlists()
            .map(|p| p.id.clone())
            .find(|id| !before.contains(id))
        else {
            self.name_prompt = None;
            return;
        };
        self.name_prompt = None;
        self.name_input.clear();
        self.name_dialog = None;
        self.open_view(View::Playlist(id));
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
            .or_else(|| self.spotify_playlist_name(&id))
            .unwrap_or_default();
        self.name_prompt = Some(NamePrompt::Rename(id));
        self.name_dialog = None;
    }

    fn on_commit_rename(&mut self, id: &str, state: &mut ui::widget::Edits<'_>) {
        let name = self.name_input.trim().to_string();
        if !name.is_empty() {
            if id.starts_with("spotify:playlist:") {
                self.queue_spotify_effect_control(
                    &crate::actions::MusicEffect::SpotifyRenamePlaylist {
                        playlist: id.into(),
                        name,
                    },
                );
            } else {
                self.perform_action(
                    state,
                    crate::model::Library::playlist_paths(id).name.replace(name),
                );
                self.reload_library(state);
            }
        }
        self.name_prompt = None;
        self.name_input.clear();
        self.name_dialog = None;
    }

    fn on_delete_playlist(&mut self, id: String, state: &mut ui::widget::Edits<'_>) {
        self.perform_action(
            state,
            crate::model::Library::DELETE_PLAYLIST
                .with(ui::tilo_resource::Id::from_raw(id.as_str())),
        );
        self.reload_library(state);
        // Leave the now-deleted detail page, and forget it in the stack.
        let gone = View::Playlist(id);
        self.nav.forget(|view| *view == gone);
        if self.view == gone {
            self.view = View::Playlists;
            self.nav.replace(View::Playlists);
            self.set_scroll_y(0.0);
        }
    }

    fn on_toggle_queue(&mut self) {
        if self.view == View::Queue {
            // Toggle off: back to exactly where the user was (view, scroll
            // and selection), not a reset.
            self.go_back();
        } else {
            self.open_view(View::Queue);
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
        let yt_id = self
            .lib
            .track(track_id)
            .map(|track| track.yt_id.clone())
            .or_else(|| crate::stream::valid_native_track(track_id).then(|| track_id.to_owned()))?;

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
            w.request(if yt_id.starts_with("spotify:track:") {
                StreamRequest::NativeSpotify {
                    track_id: track_id.to_string(),
                    source_id: yt_id,
                    request_id,
                }
            } else {
                StreamRequest::Resolve {
                    track_id: track_id.to_string(),
                    video_id: yt_id,
                    request_id,
                }
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
                        retryable: false,
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
                        // These effects are published at most once. A new user
                        // invocation may retry, but this invocation must not.
                        retryable: false,
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
            );
        }
    }

    /// A newer playback choice (Next pressed again, another song chosen)
    /// settles the stream start still resolving as superseded: the newest
    /// choice is the one that plays.
    pub(super) fn supersede_pending_stream_start(&mut self) {
        self.fail_resource_stream(
            "music.stream_start_superseded",
            "A newer playback choice superseded this stream start.",
            None,
        );
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
        self.notify(ToastKind::Error, message);
        self.fail_resource_stream(
            "music.stream_start_timed_out",
            message,
            Some(serde_json::json!({ "request_id": request_id })),
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
            || playback.queue.iter().any(|id| {
                self.playback_track(id).is_none() && !crate::stream::valid_native_track(id)
            })
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
                    || (self.playback_track(current).is_none()
                        && !crate::stream::valid_native_track(current)) =>
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
                        && (crate::stream::valid_native_track(current)
                            || self
                                .local_file
                                .as_ref()
                                .is_some_and(|file| file.track.id == current));
                    !pending_local_duration && playback.position_ms > duration
                } =>
            {
                return Err("playback position exceeds the selected track duration".into());
            }
            _ => {}
        }
        // Transport preferences must not discard which shuffled tracks have
        // already played. Only a different shuffle mode, or a different
        // queue the order was not carried over to (`follow_shuffled_edit`),
        // invalidates it.
        if self.pb.shuffle != playback.shuffle
            || (self.pb.queue != playback.queue && self.shuffle_order.len() != playback.queue.len())
        {
            self.shuffle_order.clear();
        }
        let requested_playing = playback.playing;
        let changed_track = self.pb.current != playback.current;
        let changed_position = self.pb.position_ms != playback.position_ms;
        self.pb = playback;
        self.playhead_ms = self.pb.position_ms as f64;
        self.bound_track.clone_from(&self.pb.current);
        self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
        if changed_track && !self.pb.playing {
            self.retire_replaced_playback();
        }
        // A source already resolving for this track is not started again: a
        // queue or volume change while it resolves keeps the start it awaits.
        let source_pending = !changed_track && self.active_resolve_request.is_some();
        let needs_source =
            (changed_track || (!self.audio.has_stream() && !source_pending)) && self.pb.playing;
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
        if requested_playing && request.is_none() && !self.audio.has_stream() && !source_pending {
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

    /// Carry the shuffled order over to the queue `edit` turns this one
    /// into, `next`'s, as the person's own queue edits do; a removed playing
    /// song hands over to the next shuffled one in `next`. Answers the order
    /// as it was, or `None` when nothing is shuffled.
    pub(super) fn follow_shuffled_edit(
        &mut self,
        edit: crate::actions::QueueEdit,
        next: &mut Playback,
    ) -> Option<Vec<usize>> {
        use crate::actions::QueueEdit;
        if !self.pb.shuffle || next.shuffle != self.pb.shuffle {
            return None;
        }
        if matches!(edit, QueueEdit::PutNext(_)) {
            self.ensure_shuffle_order();
        }
        if self.shuffle_order.len() != self.pb.queue.len() {
            // No order drawn for this queue yet: one is drawn when needed.
            self.shuffle_order.clear();
            return None;
        }
        let before = self.shuffle_order.clone();
        match edit {
            QueueEdit::Added(at) => shuffle_inserted(&mut self.shuffle_order, at),
            QueueEdit::PutNext(at) => {
                shuffle_inserted(&mut self.shuffle_order, at);
                shuffle_put_next(&mut self.shuffle_order, at, self.pb.queue_index);
            }
            QueueEdit::Removed(index) => {
                let slot = shuffle_removed(&mut self.shuffle_order, index);
                let shuffled = slot.and_then(|slot| self.shuffle_order.get(slot).copied());
                if index == self.pb.queue_index
                    && next.current.is_some()
                    && let Some(following) = shuffled
                    && let Some(song) = next.queue.get(following)
                {
                    next.queue_index = following;
                    next.current = Some(song.clone());
                }
            }
        }
        Some(before)
    }

    fn retire_replaced_playback(&mut self) {
        self.audio.stop();
        self.active_resolve_request = None;
        self.resolve_in_flight = 0;
        self.fail_resource_stream(
            "music.stream_start_superseded",
            "The selected audio source changed.",
            None,
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

    /// Import a streamed track (and its artist/album) into the library when it
    /// is actually played, so it joins the user's library permanently and
    /// syncs across devices. No-op for tracks already in the library.
    fn import_live_track(&mut self, track_id: &str, state: &mut ui::widget::Edits<'_>) {
        if !is_live_track(track_id) || self.stored_library.tracks.contains_key(track_id) {
            return;
        }
        let Ok(import) = self.catalogue_import(track_id) else {
            return;
        };
        self.perform_action(state, import);
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
                StreamUpdate::Spotify { request_id, result } => {
                    self.finish_spotify_operation(request_id, result);
                }
                StreamUpdate::SourceSelected { result } => {
                    // The search asks the catalogue the person chose.
                    self.search.source_changed();
                    if let Some(invocation) = self.source_invocation.take() {
                        let update = match result {
                            Ok((name, live)) => {
                                if self.source_disconnect_pending {
                                    self.spotify_devices.clear();
                                    self.spotify_device_buttons.clear();
                                    self.open_file_layout_visible = None;
                                    self.spotify_remote_state = serde_json::Value::Null;
                                    self.spotify_remote_queue = None;
                                }
                                if self.source_disconnect_pending
                                    && self
                                        .pb
                                        .current
                                        .as_ref()
                                        .is_some_and(|id| id.starts_with("spotify:"))
                                {
                                    self.audio.stop();
                                    self.pb.playing = false;
                                }
                                // The person chose a catalogue, or left the one
                                // they had: Home follows what they chose.
                                self.catalogue_connected = !self.source_disconnect_pending;
                                self.source_disconnect_pending = false;
                                self.source_error = None;
                                self.spotify_catalogue = name.starts_with("Spotify");
                                self.backend_name = name;
                                self.backend_live = live;
                                self.report_device_profile_error();
                                self.playlist_pages
                                    .retain(|id, _| !id.starts_with("spotify:"));
                                self.playlist_requested
                                    .retain(|id| !id.starts_with("spotify:"));
                                self.artist_pages
                                    .retain(|id, _| !id.starts_with("spotify:"));
                                self.artist_requested
                                    .retain(|id| !id.starts_with("spotify:"));
                                self.album_pages.retain(|id, _| !id.starts_with("spotify:"));
                                self.album_requested
                                    .retain(|id| !id.starts_with("spotify:"));
                                self.failed_pages.retain(|id| !id.starts_with("spotify:"));
                                self.home = None;
                                self.spotify_library_offset = 50;
                                self.spotify_library_more = true;
                                self.home_asked = None;
                                self.home_failed = false;
                                self.live_results = None;
                                self.live_bundle = None;
                                self.live_search_pending = None;
                                self.active_search_request = None;
                                self.live_query_sent = None;
                                self.search_error = None;
                                self.view = View::Home;
                                self.set_scroll_y(0.0);
                                tilo_actions::InvocationUpdate::Succeeded {
                                    invocation,
                                    output: tilo_actions::ActionOutput::default(),
                                }
                            }
                            Err(message) => {
                                self.source_error = Some(message.clone());
                                self.notify(ToastKind::Error, message.clone());
                                tilo_actions::InvocationUpdate::Failed {
                                    invocation,
                                    error: tilo_actions::ActionFailure {
                                        code: "music.source_failed".into(),
                                        message,
                                        details: None,
                                        retryable: false,
                                    },
                                }
                            }
                        };
                        self.resource_stream_results.push(update);
                    }
                }
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
                    self.spotify_library_offset = 50;
                    self.spotify_library_more = content
                        .sections
                        .iter()
                        .any(|section| section.items.len() == 50);
                    self.absorb_home(&content);
                    self.home = Some(*content);
                    self.home_failed = false;
                }
                StreamUpdate::HomeUnavailable { error } => {
                    // No live Home (offline build / network error): stop the
                    // skeleton and let the honest empty Home settle.
                    self.home_failed = true;
                    self.source_error = home_failure_note(ONLINE_CATALOGUE, error);
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
                StreamUpdate::NativeSpotify {
                    track_id,
                    source_id,
                    account,
                    request_id,
                } => {
                    if self.active_resolve_request != Some(request_id)
                        || self.pb.current.as_deref() != Some(track_id.as_str())
                    {
                        continue;
                    }
                    self.active_resolve_request = None;
                    self.resolve_in_flight = 0;
                    match self.audio.load_spotify(&source_id, &account) {
                        Ok(()) => {
                            self.playback_error = None;
                            self.audio.seek_ms(self.pb.position_ms);
                            if self.pb.playing {
                                self.audio.play();
                            }
                        }
                        Err(error) => {
                            self.playback_error = Some(error.clone());
                            self.pb.playing = false;
                            self.fail_resource_stream(
                                "music.spotify_audio_unavailable",
                                &error,
                                None,
                            );
                        }
                    }
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
                // Notes are logs.
                StreamUpdate::Note(_) => {}
            }
        }
    }

    fn apply_search_results(&mut self, query: &str, request_id: u64, bundle: &SearchBundle) {
        if self.active_search_request == Some(request_id) && self.search_query == query {
            self.active_search_request = None;
            self.search_error = None;
            self.complete_catalogue_search(Ok(()));
            self.ingest_search_results(query, bundle.clone());
        }
    }

    fn apply_search_failure(&mut self, query: &str, request_id: u64, error: &str) {
        if self.active_search_request == Some(request_id) && self.search_query == query {
            self.active_search_request = None;
            self.live_search_pending = None;
            self.search_error = Some((query.to_string(), error.to_string()));
            self.complete_catalogue_search(Err(error.to_owned()));
        }
    }

    /// Send the newest catalogue query the platform search asked for to the
    /// worker, or answer it at once from the last reply when it is the same.
    fn pump_live_search(&mut self) {
        let Some(query) = self.pending_catalogue_query.take() else {
            return;
        };
        if self
            .live_bundle
            .as_ref()
            .is_some_and(|(answered, _)| answered.trim() == query.trim())
        {
            return;
        }
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(&crate::actions::MusicEffect::Search { query });
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
            self.audio.load(&stream.url);
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
    /// reached the real sink and the decoder confirms Playing. A sink callback
    /// can precede `GStreamer`'s asynchronous state transition; neither URL
    /// resolution nor those first frames alone confirm settled playback.
    #[cfg(feature = "audio")]
    fn settle_resource_stream_after_rendered_frames(&mut self, rendered_frames: u64) {
        if let Some((invocation, _, frames_before, _)) = self.resource_stream_invocation
            && self.audio.is_playing_confirmed()
            && (rendered_frames > frames_before
                || (rendered_frames > 0
                    && self.resource_stream_resume.is_some()
                    && self.resource_stream_resume == self.pb.current))
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
            );
        }
    }

    /// Re-inject the accumulated live search hits into the in-memory library
    /// snapshot. Idempotent; runs each frame so live tracks survive a `sync`
    /// that reloads the catalog from the document.
    fn inject_live_hits(&mut self) {
        for hit in &self.live_hits {
            self.lib
                .artists
                .entry(hit.artist.id.clone())
                .or_insert_with(|| hit.artist.clone());
            self.lib
                .albums
                .entry(hit.album.id.clone())
                .or_insert_with(|| hit.album.clone());
            self.lib
                .tracks
                .entry(hit.track.id.clone())
                .or_insert_with(|| hit.track.clone());
        }
    }

    /// Bounded live targets from the latest admitted catalogue answer.
    fn catalogue_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        use ui::tilo_resource::{SearchEntry, SearchWeight};
        let Some((_, bundle)) = &self.live_bundle else {
            return Vec::new();
        };
        let limit = crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT;
        let albums = bundle.albums.iter().take(limit).map(|album| {
            SearchEntry::new(album.title.clone(), String::new())
                .field(album.artist.clone(), SearchWeight::Subtitle)
                .anchored(crate::actions::album_resource_id(&album.id))
                .grouped("Albums")
                .subtitled(album.artist.clone())
        });
        let artists = bundle.artists.iter().take(limit).map(|artist| {
            SearchEntry::new(artist.name.clone(), String::new())
                .anchored(crate::actions::artist_resource_id(&artist.id))
                .grouped("Artists")
                .subtitled("Artist")
        });
        let playlists = bundle.playlists.iter().take(limit).map(|playlist| {
            SearchEntry::new(playlist.title.clone(), String::new())
                .field(playlist.subtitle.clone(), SearchWeight::Subtitle)
                .anchored(crate::actions::catalogue_playlist_resource_id(&playlist.id))
                .grouped("Playlists")
                .subtitled(playlist.subtitle.clone())
        });
        albums.chain(artists).chain(playlists).collect()
    }

    fn catalogue_resources(&self) -> Vec<ui::framework::LiveRow> {
        let Some((_, bundle)) = &self.live_bundle else {
            return Vec::new();
        };
        let limit = crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT;
        bundle
            .albums
            .iter()
            .take(limit)
            .map(|a| (crate::actions::album_resource_id(&a.id), a.title.as_str()))
            .chain(
                bundle
                    .artists
                    .iter()
                    .take(limit)
                    .map(|a| (crate::actions::artist_resource_id(&a.id), a.name.as_str())),
            )
            .chain(bundle.playlists.iter().take(limit).map(|a| {
                (
                    crate::actions::catalogue_playlist_resource_id(&a.id),
                    a.title.as_str(),
                )
            }))
            .filter_map(|(id, label)| crate::actions::live_catalogue_row(&id, label))
            .collect()
    }

    /// The songs the live search found that the library does not hold yet,
    /// as search entries beside the library's own: shown here, so findable
    /// here, without listing a song the library already has twice.
    pub(crate) fn live_search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        let mut found = crate::model::Library::default();
        for hit in &self.live_hits {
            if !self.stored_library.tracks.contains_key(&hit.track.id) {
                found.tracks.insert(hit.track.id.clone(), hit.track.clone());
            }
            if !self.stored_library.albums.contains_key(&hit.album.id) {
                found.albums.insert(hit.album.id.clone(), hit.album.clone());
            }
            if !self.stored_library.artists.contains_key(&hit.artist.id) {
                found
                    .artists
                    .insert(hit.artist.id.clone(), hit.artist.clone());
            }
        }
        let mut entries = crate::search_type::library_entries(&found);
        for entry in self.catalogue_entries() {
            if !entries
                .iter()
                .any(|existing| existing.action == entry.action)
            {
                entries.push(entry);
            }
        }
        entries
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
            let live = hit_to_live(hit);
            self.record_cover(&live.album.id, &hit.cover_url);
            self.record_cover(&live.track.id, &hit.cover_url);
            if !self.live_hits.iter().any(|h| h.track.id == live.track.id) {
                self.live_hits.push(live);
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
            .map(|h| live_track_id(&h.video_id))
            .collect();
        self.live_results = Some((query.to_string(), ordered));
        self.live_bundle = Some((query.to_string(), bundle));
    }

    /// The decoded cover image for an entity id, if one has loaded. Requests an
    /// async fetch+decode on first sight; returns `None` (placeholder) until it
    /// lands.
    fn art_image(&self, id: &str) -> Option<vello::peniko::ImageData> {
        if let Some(image) = self.art_urls.get(id).and_then(|url| self.images.get(url)) {
            return Some(image);
        }
        // A playlist without a cover of its own shows its tracks' artwork.
        let playlist = self.lib.playlist(id)?;
        self.playlist_mosaic(playlist)
    }

    /// A playlist's cover made from the artwork of its first tracks with
    /// distinct covers: one cover whole, two to four as a 2×2 mosaic. `None`
    /// when none of its tracks has artwork, where the card shows the
    /// playlist's initials. Composed once per set of covers and cached.
    fn playlist_mosaic(
        &self,
        playlist: &crate::model::Playlist,
    ) -> Option<vello::peniko::ImageData> {
        let mut covers = mosaic_covers(playlist.tracks.values(), |track_id| {
            self.lib.track(track_id).and_then(|track| {
                self.art_urls
                    .get(&track.album_id)
                    .or_else(|| self.art_urls.get(track_id))
                    .and_then(|url| self.images.get(url))
            })
        });
        if covers.len() < 2 {
            return covers.pop();
        }
        let ids: Vec<u64> = covers.iter().map(|cover| cover.data.id()).collect();
        let mut mosaics = self.playlist_mosaics.borrow_mut();
        if let Some((seen, mosaic)) = mosaics.get(&playlist.id)
            && *seen == ids
        {
            return Some(mosaic.clone());
        }
        let mosaic =
            ui::held_images::mosaic_of(&covers, ui::held_images::PORTRAIT_SIDE.saturating_mul(2))?;
        mosaics.insert(playlist.id.clone(), (ids, mosaic.clone()));
        Some(mosaic)
    }

    fn images_pending(&self) -> bool {
        self.images.pending() > 0
    }

    /// Fetch Home from the connected catalogue, once; with none connected nothing is fetched.
    fn ensure_home(&mut self) {
        if self.home_asked.is_some() || !self.allow_network || !self.catalogue_connected {
            return;
        }
        self.ensure_worker();
        if let Some(w) = &self.worker {
            match w.request(StreamRequest::Home) {
                Ok(()) => self.home_asked = Some(ui::time::now()),
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

    /// A real artist page, asked once.
    fn ensure_artist(&mut self, id: &str) {
        if !self.allow_network || !is_live_id(id) || self.artist_requested.contains(id) {
            return;
        }
        if self.request_page(id, StreamRequest::Artist(id.to_string()), "artist") {
            self.artist_requested.insert(id.to_string());
        }
    }

    /// A real album page, asked once.
    fn ensure_album(&mut self, id: &str) {
        if !self.allow_network || !is_live_album_id(id) || self.album_requested.contains(id) {
            return;
        }
        if self.request_page(id, StreamRequest::Album(id.to_string()), "album") {
            self.album_requested.insert(id.to_string());
        }
    }

    /// A real catalogue playlist page, asked once.
    fn ensure_playlist(&mut self, id: &str) {
        if !self.allow_network || !is_live_playlist_id(id) || self.playlist_requested.contains(id) {
            return;
        }
        if self.request_page(id, StreamRequest::Playlist(id.to_string()), "playlist") {
            self.playlist_requested.insert(id.to_string());
        }
    }

    /// Send the page request to the worker; a refused request marks the page failed.
    fn request_page(&mut self, id: &str, request: StreamRequest, what: &str) -> bool {
        self.ensure_worker();
        let Some(w) = &self.worker else {
            return false;
        };
        match w.request(request) {
            Ok(()) => {
                self.pages_asked.insert(id.to_string(), ui::time::now());
                true
            }
            Err(error) => {
                self.failed_pages.insert(id.to_string());
                self.notify(
                    ToastKind::Error,
                    format!("Couldn\u{2019}t load {what}: {error}"),
                );
                false
            }
        }
    }

    fn scroll_y(&self) -> f64 {
        self.scroll_target
            .unwrap_or_else(|| self.body_scroll.offset().y)
    }

    fn set_scroll_y(&mut self, y: f64) {
        self.scroll_target = Some(y.max(0.0));
    }

    fn push_hit(&mut self, rect: Rect, action: Action) -> bool {
        let hovered = rect.contains(self.mouse);
        self.hits.push(Hit { rect, action });
        hovered
    }

    /// What the launch named: a `music://` track (continuing a handoff's position), an
    /// album, artist or playlist, or a local audio file.
    pub(crate) fn open_launch(&mut self, launch: &ui::framework::LaunchInput<'_>) {
        let Some(source) = launch.argument() else {
            return;
        };
        if let Some(link) = crate::search_type::DeepLink::parse(source) {
            match link {
                crate::search_type::DeepLink::Track(_) => {
                    let handoff = launch.handoff().cloned().unwrap_or_default();
                    self.pending_media_handoff = Some(PendingMediaHandoff {
                        uri: source.to_owned(),
                        position_ms: handoff.position_ms,
                        resume_playing: handoff.resume_playing,
                    });
                }
                crate::search_type::DeepLink::Album(id) => self.on_nav(View::Album(id)),
                crate::search_type::DeepLink::Artist(id) => self.on_nav(View::Artist(id)),
                crate::search_type::DeepLink::Playlist(id) => self.on_nav(View::Playlist(id)),
            }
            return;
        }
        if let Err(error) = self.open_local_file(source) {
            self.playback_error = Some(error.clone());
            self.notify(ToastKind::Error, error);
        }
    }

    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "recommendations" | "recommendations-feedback" | "recommendations-empty" => {
                self.view = View::Recommendations;
            }
            "recommendations-home" => self.view = View::Home,
            "sidebar" => self.snap_sidebar(true),
            "collapsed" => self.snap_sidebar(false),
            "context" => self.snap_track_menu(),
            "new-playlist" => self.snap_new_playlist_prompt(),
            "search-focus" => self
                .search_opener
                .open(ui::search::SearchRequest::this_app()),
            "skeleton" => self.snap_skeleton(),
            "nowplaying" => self.snap_nowplaying(),
            "nowplaying-page" => {
                self.shot_stage = Some(ShotStage::NowPlaying);
                self.view = View::NowPlaying;
            }
            "queue" => self.snap_queue(),
            "lyrics" => self.snap_lyrics(),
            "downloading" => self.snap_downloading(),
            "offline-cached" => self.snap_offline_cached(),
            "offline-error" => self.snap_offline_error(),
            "search-tabs" | "search-songs" | "search-artists" => self
                .search_opener
                .open(ui::search::SearchRequest::this_app().with_query("Daft Punk")),
            "artist-page" => self.snap_artist_page(),
            "album-page" => self.snap_album_page(),
            "error" => self.snap_error_page(),
            "membership-open" => self.snap_membership_open(),
            "spotify-home" | "spotify-devices" | "spotify-error" | "spotify-queue"
            | "spotify-create" => self.snap_spotify(scenario),
            "library" => {
                self.view = View::Songs;
                self.restore_selection = Some(vec!["tk_g2".into(), "tk_g3".into()]);
            }
            "search" => self.stage_query("light", &[], None),
            "search-chips" => self.stage_query(
                "light",
                &[ui::framework::views::Chip::value(
                    crate::views::ARTIST,
                    "Aurora Skies",
                )],
                None,
            ),
            "search-remote-loading" => self.stage_query_waiting("light"),
            "search-actions" => self.stage_query("shuffle", &[], None),
            "search-facet" => {
                self.stage_query(
                    "light",
                    &[ui::framework::views::Chip::value(
                        crate::views::ARTIST,
                        "Aurora Skies",
                    )],
                    None,
                );
                self.staged_facet = Some(crate::views::ARTIST.to_owned());
            }
            "kept-view" => self.stage_kept_view("light"),
            _ => {}
        }
    }

    pub fn snap_sidebar(&mut self, open: bool) {
        if let Some(bar) = &self.bar {
            bar.stage_sidebar(open);
        }
        self.dirty = true;
    }

    pub fn snap_track_menu(&mut self) {
        self.shot_stage = Some(ShotStage::TrackMenu);
        if matches!(self.view, View::Home) {
            self.view = View::Songs;
        }
    }

    pub fn snap_new_playlist_prompt(&mut self) {
        self.shot_stage = Some(ShotStage::NewPlaylistPrompt);
        if matches!(self.view, View::Home) {
            self.view = View::Playlists;
        }
    }

    pub fn snap_skeleton(&mut self) {
        self.shot_stage = Some(ShotStage::Skeleton);
        self.view = View::Home;
    }

    pub fn snap_nowplaying(&mut self) {
        self.shot_stage = Some(ShotStage::NowPlaying);
        self.view = View::Songs;
    }

    pub fn snap_queue(&mut self) {
        self.shot_stage = Some(ShotStage::NowPlaying);
        self.view = View::Queue;
    }

    pub fn snap_lyrics(&mut self) {
        self.shot_stage = Some(ShotStage::Lyrics);
        self.view = View::Lyrics;
    }

    pub fn snap_downloading(&mut self) {
        self.shot_stage = Some(ShotStage::Downloading);
        self.view = View::Downloads;
    }

    pub fn snap_offline_cached(&mut self) {
        self.shot_stage = Some(ShotStage::OfflineCached);
        self.view = View::Downloads;
    }

    pub fn snap_offline_error(&mut self) {
        self.shot_stage = Some(ShotStage::OfflineError);
        self.view = View::Downloads;
    }

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
            provider_followed: None,
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

    pub fn snap_error_page(&mut self) {
        let id = "UC_music_shot_unavailable".to_string();
        self.failed_pages.insert(id.clone());
        self.view = View::Artist(id);
    }

    pub fn snap_membership_open(&mut self) {
        self.shot_stage = Some(ShotStage::MembershipOpen);
    }

    /// Start a bundled track at `pos_ms` for a capture; `tk_g1` carries lyrics.
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

    fn apply_shot_stage(&mut self) {
        let Some(stage) = self.shot_stage.take() else {
            return;
        };
        match stage {
            ShotStage::TrackMenu => {
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
                // Home was asked a moment ago, so its skeleton shows without a fetch.
                self.home_asked = ui::time::now().checked_sub(Duration::from_millis(500));
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
                self.stage_play_bundled(40_000.0);
            }
            ShotStage::MembershipOpen => {
                let Some((playlist, track)) = self.lib.playlists().find_map(|playlist| {
                    playlist
                        .tracks
                        .values()
                        .last()
                        .map(|track| (playlist.id.clone(), track.clone()))
                }) else {
                    return;
                };
                let _ = self.show(crate::actions::Place::Membership { playlist, track });
            }
        }
    }

    fn push_menu(&mut self, label: impl Into<String>, action: Action) {
        self.push_menu_with_detail(label, None, action);
    }

    fn push_menu_with_detail(
        &mut self,
        label: impl Into<String>,
        detail: Option<String>,
        action: Action,
    ) {
        let idx = self.menu_actions.len() as i64;
        let pending = self.pending.clone();
        let select = move || pending.store(idx, Ordering::SeqCst);
        let item = if matches!(action, Action::DeletePlaylist(_)) {
            MenuItem::destructive_action(label, select)
        } else {
            MenuItem::action(label, select)
        };
        let item = match detail {
            Some(detail) => item.with_detail(detail),
            None => item,
        };
        self.context.push(item);
        self.menu_actions.push(action);
    }

    fn open_playlist_menu(&mut self, id: &str) {
        self.context = ContextMenu::new();
        self.menu_actions.clear();
        self.ctx_track = None;
        self.ctx_in_playlist = None;
        self.ctx_queue_index = None;
        self.menu_anchor_row = None;
        self.push_menu("Rename", Action::BeginRenamePlaylist(id.to_owned()));
        self.push_menu("Set cover", Action::PickPlaylistCover(id.to_owned()));
        self.push_menu("Delete playlist", Action::DeletePlaylist(id.to_owned()));
        self.open_context_menu_at_pointer();
    }

    fn push_track_management(
        &mut self,
        track: &str,
        playlist: Option<&str>,
        queue_index: Option<usize>,
    ) {
        let position = if let Some(id) = playlist {
            self.lib.playlist(id).and_then(|list| {
                let ids = list.track_ids();
                ids.iter()
                    .position(|id| id == track)
                    .map(|i| (i, ids.len()))
            })
        } else {
            queue_index.map(|i| (i, self.pb.queue.len()))
        };
        if let Some((from, len)) = position {
            for (label, to) in [
                ("Move up", from.checked_sub(1)),
                ("Move down", (from + 1 < len).then_some(from + 1)),
            ] {
                if let Some(to) = to {
                    let action = playlist.map_or(Action::MoveQueueItem { from, to }, |id| {
                        Action::MovePlaylistItem {
                            playlist: id.to_owned(),
                            from,
                            to,
                        }
                    });
                    self.push_menu(label, action);
                }
            }
        }
        let (label, keep) = if self.offline_errors.contains_key(track) {
            ("Retry download", true)
        } else {
            match self.cache_state(track) {
                CacheState::Streamed => ("Keep offline", true),
                CacheState::Cached => ("Remove download", false),
                CacheState::Downloading(_) => ("Cancel download", false),
            }
        };
        if !self.is_local_file(track) {
            self.push_menu(
                label,
                Action::SetOffline {
                    track: track.to_owned(),
                    keep,
                },
            );
        }
    }

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

        self.push_track_management(&track, in_playlist, queue_index);
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
        if track.starts_with("spotify:track:") {
            self.push_menu(
                crate::i18n::t("music-spotify-save"),
                Action::SpotifyLibrary {
                    track: track.clone(),
                    save: true,
                },
            );
            self.push_menu(
                crate::i18n::t("music-spotify-unsave"),
                Action::SpotifyLibrary {
                    track: track.clone(),
                    save: false,
                },
            );
            if let View::Playlist(playlist) = &self.view
                && playlist.starts_with("spotify:playlist:")
            {
                let effect = crate::actions::MusicEffect::SpotifyPlaylistRemove {
                    playlist: playlist.clone(),
                    track: track.clone(),
                };
                self.push_menu(
                    crate::i18n::t("music-spotify-remove-playlist"),
                    Action::SpotifyEffect(effect),
                );
            }
        }
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

    /// The narrow transport's More menu: the controls the compact bar has no room for.
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
        if self.spotify_catalogue {
            self.push_menu(
                crate::i18n::t("music-spotify-devices"),
                Action::SpotifyEffect(crate::actions::MusicEffect::SpotifyDevices),
            );
            let devices = self.spotify_devices.clone();
            for device in devices.iter().filter(|device| !device.restricted).take(12) {
                let label = crate::i18n::t_args(
                    "music-spotify-select-device",
                    &[("device", (&device.name).into())],
                );
                self.push_menu(
                    label,
                    Action::SpotifyEffect(crate::actions::MusicEffect::SpotifyTransfer {
                        device: device.id.clone(),
                    }),
                );
            }
        }

        self.context.set_text_scale(self.context_text_scale);
        self.context.open_anchored(anchor, self.rect);
        self.dirty = true;
    }

    fn open_add_to_playlist_menu(&mut self, track: &str) {
        self.context = ContextMenu::new();
        self.menu_actions.clear();
        self.ctx_track = Some(track.to_string());

        self.push_menu(
            "New playlist\u{2026}",
            Action::BeginNewPlaylistFor(track.to_string()),
        );
        if track.starts_with("spotify:track:") {
            self.push_menu(
                crate::i18n::t("music-spotify-create-playlist"),
                Action::BeginNewSpotifyPlaylist,
            );
        }
        if !self.lib.playlists.is_empty() {
            self.context.push(MenuItem::separator());
            self.menu_actions.push(Action::Back);
        }
        let playlists: Vec<(String, String)> = self
            .lib
            .playlists()
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

        self.append_spotify_playlist_choices(track);

        self.open_context_menu_at_pointer();
    }

    /// Open the menu at the pointer; `ContextMenu::open_anchored` measures its rows and
    /// keeps them inside the viewport.
    fn open_context_menu_at_pointer(&mut self) {
        let anchor = Rect::from_origin_size(self.mouse, (0.0, 0.0));
        if let Some(row) = self.menu_anchor_row {
            self.context.anchor_row(row);
        }
        self.context.set_text_scale(self.context_text_scale);
        self.context.open_anchored(anchor, self.rect);
        self.dirty = true;
    }

    /// Keep the menu's geometry in step with the system's accessibility scale.
    fn sync_context_menu_geometry(&mut self, theme: &Theme) {
        self.context_text_scale = theme.text_scale;
        self.context.set_text_scale(self.context_text_scale);
        self.context.set_viewport(Some(self.rect));
    }

    /// Run the row the menu reported; whether there was one.
    fn drain_menu_selection(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        let idx = self.pending.swap(-1, Ordering::SeqCst);
        if idx >= 0 {
            if let Some(action) = self.menu_actions.get(idx as usize).cloned() {
                self.dispatch(action, state);
            }
            return true;
        }
        false
    }

    /// Resolve lyrics when the playing track changes, from the bundled library or the
    /// async worker, cached by track id.
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
        if let Some((lines, synced)) = self.lyrics_cache.get(&id) {
            self.lyrics.clone_from(lines);
            self.lyrics_synced = *synced;
            return;
        }
        let Some(track) = self.lib.track(&id).cloned() else {
            return;
        };
        if let Some(l) = lyrics::bundled::for_track(&id) {
            self.lyrics_cache
                .insert(id.clone(), (l.lines.clone(), l.synced));
            self.lyrics = l.lines;
            self.lyrics_synced = l.synced;
            return;
        }
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

    fn poll_lyrics(&mut self) {
        let mut got = Vec::new();
        if let Some(w) = &self.lyrics_worker {
            while let Some(r) = w.poll() {
                got.push(r);
            }
        }
        for (id, l) in got {
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

    /// Warm the next queued song's lyrics and cover on a stable connection, once per
    /// track change.
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
            let album_id = track.album_id;
            self.ensure_album(&album_id);
        }
        true
    }
}

impl Default for Music {
    fn default() -> Self {
        Self::new()
    }
}

/// Fit `text` into `max_w`: an empty string when not even one glyph fits, so no
/// lone ellipsis spills into the next column.
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

/// The id prefix of a track from a catalogue search; sample tracks use `tk_…`.
pub(crate) const LIVE_TRACK_PREFIX: &str = "yt:";

pub(crate) fn live_track_id(source_key: &str) -> String {
    if source_key.starts_with("spotify:track:") {
        source_key.to_owned()
    } else {
        format!("{LIVE_TRACK_PREFIX}{source_key}")
    }
}

/// Whether the id names a real streamable track.
fn is_live_track(track_id: &str) -> bool {
    track_id.starts_with(LIVE_TRACK_PREFIX) || track_id.starts_with("spotify:track:")
}

/// Whether the id names a real artist channel.
fn is_live_id(id: &str) -> bool {
    id.starts_with("UC") || id.starts_with("spotify:artist:")
}

fn is_live_album_id(id: &str) -> bool {
    id.starts_with("MPRE") || id.starts_with("OLAK") || id.starts_with("spotify:album:")
}

/// Whether the id is a catalogue playlist: the app's own are minted `pl_…`.
fn is_live_playlist_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with("pl_")
}

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

/// A wall-clock seed, so each session shuffles differently; never zero.
fn seed_rng() -> u64 {
    let nanos = ui::time::try_unix_now_ns().unwrap_or(0x1234_5678);
    nanos ^ 0x9E37_79B9_7F4A_7C15
}

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

fn hit_to_live(hit: &TrackHit) -> CatalogueTrack {
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
    CatalogueTrack {
        track: Track {
            id: live_track_id(&hit.video_id),
            title: hit.title.clone(),
            album_id: album_id.clone(),
            artist_id: artist_id.clone(),
            track_no: 0,
            duration_s: hit.duration_s,
            yt_id: hit.video_id.clone(),
            plays: 0,
        },
        album: Album {
            id: album_id,
            title: album_title,
            artist_id: artist_id.clone(),
            year: 0,
        },
        artist: Artist {
            id: artist_id,
            name: hit.artist.clone(),
            genre: String::new(),
            monthly_listeners: 0,
        },
    }
}

/// Bound what a decoder is handed from a portal; the shared payload has the tighter budget.
const MAX_COVER_SOURCE_BYTES: u64 = 8 * 1024 * 1024;

fn portable_cover_data_url(source: &[u8]) -> Result<String, String> {
    if source.is_empty() || source.len() as u64 > MAX_COVER_SOURCE_BYTES {
        return Err("image is empty or larger than 8 MiB".to_string());
    }
    // The shared decoder refuses an oversized header before allocating, and applies EXIF.
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

/// The activity's cover edge: a card draws it at 56 points, within the activity's
/// 16 KiB wire budget.
const ACTIVITY_ART_EDGE: u32 = 112;

/// The activity's `data:` picture from a cover this window decoded; empty until then.
fn cover_picture(url: &str, images: &ImageStore) -> String {
    images.get(url).map_or_else(String::new, |cover| {
        ui::held_images::cover(&cover, ACTIVITY_ART_EDGE)
    })
}

/// Encode the chosen cover off the UI thread, as background work every host runs.
fn embed_cover_off_thread(source: Vec<u8>) -> Receiver<Result<String, String>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    if let Err(error) =
        ui::background::Background::current().spawn("music-cover-embed", move || {
            let _ = sender.send(portable_cover_data_url(&source));
        })
    {
        let (fallback_sender, fallback_receiver) = std::sync::mpsc::channel();
        let _ = fallback_sender.send(Err(error.to_string()));
        return fallback_receiver;
    }
    receiver
}

/// Only embedded PNG values: actions reject paths and `file:` URLs, so the renderer
/// never opens a peer's file.
fn embedded_cover_bytes(url: &str) -> Option<Vec<u8>> {
    if !crate::actions::portable_cover_value(url) {
        return None;
    }
    let encoded = url.strip_prefix("data:image/png;base64,")?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}

/// Cover bytes over the shared `tilo_net` client under `live`; embedded PNGs render
/// everywhere and a capture opens no socket.
#[cfg(feature = "live")]
pub(crate) fn image_fetcher() -> ui::Fetcher {
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
        // The body is read through a hard cap, so a wrong Content-Length cannot allocate
        // without bound.
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
pub(crate) fn image_fetcher() -> ui::Fetcher {
    Arc::new(embedded_cover_bytes)
}

fn moved_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < index && index <= to {
        index - 1
    } else if to <= index && index < from {
        index + 1
    } else {
        index
    }
}

/// A song joined at `at`: it is heard after every song still to come.
fn shuffle_inserted(order: &mut Vec<usize>, at: usize) {
    if order.is_empty() {
        return;
    }
    for index in order.iter_mut() {
        if *index >= at {
            *index += 1;
        }
    }
    order.push(at);
}

/// "Play next" under shuffle: next in the order heard, not only in the queue's.
fn shuffle_put_next(order: &mut Vec<usize>, index: usize, current: usize) {
    let Some(from) = order.iter().position(|&i| i == index) else {
        return;
    };
    order.remove(from);
    let after = order
        .iter()
        .position(|&i| i == current)
        .map_or(order.len(), |at| at + 1);
    order.insert(after, index);
}

/// The song at `index` left: where in the order it was.
fn shuffle_removed(order: &mut Vec<usize>, index: usize) -> Option<usize> {
    let slot = order.iter().position(|&i| i == index);
    if let Some(slot) = slot {
        order.remove(slot);
    }
    for i in order.iter_mut() {
        if *i > index {
            *i -= 1;
        }
    }
    slot
}

mod local_files;
mod media_grid;
mod page_part;
mod published;
mod recommendations;
mod render;
mod shell;
mod stream_cut;
#[cfg(test)]
use shell::place_id as shell_place;
mod spotify;
mod track_table;
mod window;
mod window_bar;

pub use window::MusicApp;
pub(crate) use window::window;

#[cfg(test)]
#[path = "app/home_arrival_tests.rs"]
mod home_arrival_tests;
#[cfg(test)]
#[path = "app/stream_cut_tests.rs"]
mod stream_cut_tests;
#[cfg(test)]
#[path = "app/stream_start_tests.rs"]
mod stream_start_tests;
#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests;

#[cfg(test)]
impl Music {
    pub(crate) fn view_for_test(&self) -> View {
        self.view.clone()
    }
}

impl Music {
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&str) -> Result<(), ui::tilo_resource::Refusal> + 'static {
        let opened = Rc::clone(&self.opened);
        move |anchor| {
            if crate::actions::anchor_effect(anchor).is_none() {
                return Err(ui::tilo_resource::Refusal::new(format!(
                    "Music opens no `{anchor}`"
                )));
            }
            opened.borrow_mut().push(anchor.to_owned());
            Ok(())
        }
    }

    pub(crate) fn open_anchors(&mut self) {
        let anchors = std::mem::take(&mut *self.opened.borrow_mut());
        for anchor in anchors {
            let Some(effect) = crate::actions::anchor_effect(&anchor) else {
                continue;
            };
            if let Err(error) = self.begin_effect(tilo_actions::InvocationId::new(), effect) {
                self.notify(ToastKind::Error, error);
            }
        }
    }
}

impl Music {
    pub(crate) fn row_edits(&self, row: &str) -> Vec<ui::tilo_resource::ActionDecl<Library>> {
        use ui::tilo_resource::ActionDecl;
        let lib = &self.stored_library;
        if let Some(album) = row.strip_prefix("music.album:") {
            if !lib.albums.contains_key(album) {
                return Vec::new();
            }
            let saved = lib.is_album_saved(album);
            return vec![ActionDecl::edit(
                "toggle-save",
                if saved {
                    "Remove album from library"
                } else {
                    "Save album to library"
                },
                Library::save_album(album, !saved),
            )];
        }
        if let Some(artist) = row.strip_prefix("music.artist:") {
            if !lib.artists.contains_key(artist) || artist.starts_with("spotify:artist:") {
                return Vec::new();
            }
            let followed = lib.is_artist_followed(artist);
            return vec![ActionDecl::edit(
                "toggle-follow",
                if followed {
                    "Unfollow artist"
                } else {
                    "Follow artist"
                },
                Library::follow_artist(artist, !followed),
            )];
        }
        if let Some((playlist, track)) = crate::actions::parse_playlist_track_resource_id(row) {
            return membership_edits(lib, &playlist, &track);
        }
        if let Some(playlist) = crate::actions::parse_playlist_resource_id(row) {
            return playlist_edits(lib, &playlist);
        }
        recommendation_edits(lib, row)
    }
}

fn playlist_edits(lib: &Library, playlist: &str) -> Vec<ui::tilo_resource::ActionDecl<Library>> {
    use ui::tilo_resource::{ActionDecl, Id};
    let Some(list) = lib.playlist(playlist) else {
        return Vec::new();
    };
    let paths = Library::playlist_paths(playlist);
    let cover = paths.cover.clone();
    let tracks = paths.tracks;
    let end = list.len();
    vec![
        ActionDecl::edit_asking(
            "set-cover-url",
            "Set playlist cover URL",
            "cover",
            "An https:// image address; blank clears the cover",
            move |value: String| cover.replace(value.trim()),
        ),
        ActionDecl::edit_asking(
            "add-track",
            "Add track to playlist",
            "track",
            "The song of the library to add, by id or music.track: address",
            move |value: String| {
                let track = value
                    .strip_prefix("music.track:")
                    .unwrap_or(&value)
                    .to_owned();
                tracks.insert_as(&Id::from_raw(track.as_str()), end, &track)
            },
        ),
    ]
}

fn membership_edits(
    lib: &Library,
    playlist: &str,
    track: &str,
) -> Vec<ui::tilo_resource::ActionDecl<Library>> {
    use ui::tilo_resource::ActionDecl;
    let Some(list) = lib.playlist(playlist) else {
        return Vec::new();
    };
    let Some(index) = list
        .tracks
        .position(&ui::tilo_resource::Id::from_raw(track))
    else {
        return Vec::new();
    };
    let mut edits = vec![ActionDecl::edit(
        "remove",
        "Remove from playlist",
        Library::remove_from_playlist(playlist, track),
    )];
    if index > 0 {
        edits.push(ActionDecl::edit(
            "move-up",
            "Move track up",
            Library::move_in_playlist(playlist, track, index - 1),
        ));
    }
    if index + 1 < list.len() {
        edits.push(ActionDecl::edit(
            "move-down",
            "Move track down",
            Library::move_in_playlist(playlist, track, index + 1),
        ));
    }
    edits
}

fn recommendation_edits(lib: &Library, row: &str) -> Vec<ui::tilo_resource::ActionDecl<Library>> {
    use ui::tilo_resource::ActionDecl;
    let recommendations = Library::paths().recommendations;
    if let Some(key) = row.strip_prefix("music.recommendation-input:") {
        let Some(input) = crate::recommendations::Input::ALL
            .into_iter()
            .find(|input| input.key() == key)
        else {
            return Vec::new();
        };
        let enabled = lib.recommendations.enabled(input);
        return vec![ActionDecl::edit(
            "set-input",
            crate::i18n::t(if enabled {
                "music-recommendations-disable"
            } else {
                "music-recommendations-enable"
            }),
            crate::recommendations::Preferences::input_path(input).set(!enabled),
        )];
    }
    if let Some(artist) = row.strip_prefix("music.recommendation-artist:") {
        let preferred = lib.recommendations.prefers(artist);
        if !preferred && !lib.artists.contains_key(artist) {
            return Vec::new();
        }
        let artist = artist.to_owned();
        return vec![ActionDecl::edit(
            "set-preferred",
            crate::i18n::t(if preferred {
                "music-recommendations-unprefer"
            } else {
                "music-recommendations-prefer"
            }),
            if preferred {
                recommendations.preferred_artists.remove(&artist)
            } else {
                recommendations.preferred_artists.insert(&artist)
            },
        )];
    }
    if let Some(track) = row.strip_prefix("music.recommendation:") {
        let dismissed = lib.recommendations.is_dismissed(track);
        if !dismissed && !lib.tracks.contains_key(track) {
            return Vec::new();
        }
        let track = track.to_owned();
        return vec![if dismissed {
            ActionDecl::edit(
                "restore",
                crate::i18n::t("music-recommendations-restore"),
                recommendations.dismissed.remove(&track),
            )
        } else {
            ActionDecl::edit(
                "dismiss",
                crate::i18n::t("music-recommendations-dismiss"),
                recommendations.dismissed.insert(&track),
            )
        }];
    }
    Vec::new()
}

fn mosaic_covers<'a>(
    tracks: impl IntoIterator<Item = &'a String>,
    cover_of: impl Fn(&str) -> Option<vello::peniko::ImageData>,
) -> Vec<vello::peniko::ImageData> {
    let mut covers: Vec<vello::peniko::ImageData> = Vec::new();
    for track in tracks {
        let Some(cover) = cover_of(track) else {
            continue;
        };
        if covers.iter().any(|seen| seen.data.id() == cover.data.id()) {
            continue;
        }
        covers.push(cover);
        if covers.len() == 4 {
            break;
        }
    }
    covers
}

impl crate::actions::DeviceFacts for Music {
    fn offline(&self, track: &str) -> bool {
        matches!(self.cache_state(track), CacheState::Cached)
    }

    fn followed(&self, artist: &str) -> bool {
        self.artist_followed(artist)
    }
}

#[cfg(test)]
mod mosaic_tests {
    use super::mosaic_covers;
    use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

    fn cover(shade: u8) -> ImageData {
        ImageData {
            data: Blob::new(std::sync::Arc::new([shade, shade, shade, 255].repeat(4))),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 2,
            height: 2,
        }
    }

    #[test]
    fn a_playlist_cover_takes_the_first_four_distinct_track_covers() {
        let covers = [cover(10), cover(20), cover(30), cover(40), cover(50)];
        let tracks: Vec<String> = ["a", "a2", "none", "b", "c", "d", "e"]
            .map(str::to_owned)
            .to_vec();
        let cover_of = |track: &str| match track {
            "a" | "a2" => Some(covers[0].clone()),
            "b" => Some(covers[1].clone()),
            "c" => Some(covers[2].clone()),
            "d" => Some(covers[3].clone()),
            "e" => Some(covers[4].clone()),
            _ => None,
        };
        let chosen = mosaic_covers(&tracks, cover_of);
        let ids: Vec<u64> = chosen.iter().map(|c| c.data.id()).collect();
        let want: Vec<u64> = covers[..4].iter().map(|c| c.data.id()).collect();
        assert_eq!(ids, want);
        let mosaic = ui::held_images::mosaic_of(&chosen, 64).unwrap();
        assert_eq!((mosaic.width, mosaic.height), (64, 64));

        assert!(
            mosaic_covers(&tracks, |_| None).is_empty(),
            "no artwork: initials"
        );
    }
}
