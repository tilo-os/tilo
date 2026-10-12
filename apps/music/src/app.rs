//! Root widget for the Music app.
//!
//! The whole UI is drawn directly into the Vello scene (nav rail, library
//! grids, track lists, detail pages, search and the now-playing transport
//! bar), so the app owns its layout math and hit-testing rather than
//! composing stock taffy widgets. User-authored library state is the
//! [`Library`](crate::model::Library) resource; `sync` refreshes that render
//! snapshot each frame. Playback, downloads and other device effects stay
//! local so one device can never start audio or consume another device's disk.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::Receiver;
use ui::time::{Duration, Instant};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash, ObjId};
use base64::Engine as _;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use std::cell::Cell;
use std::rc::Rc;
use ui::ImageStore;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
#[cfg(test)]
use ui::widget::SplitLayout;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{Notice, ToastKind};
use ui::widget::{ModalDialog, PromptBody, SidebarDrawer, Widget};
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

/// A host-visible stream-start operation must eventually settle even when a
/// resolver, staging download, or audio backend wedges. This bounds the
/// non-cancellable lifecycle contract without turning a slow normal seek into
/// a fabricated success.
const STREAM_START_TIMEOUT: Duration = Duration::from_secs(30);
const PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What a failed offline download says in passing. The reason stays with the
/// song: its row in Downloads offers Retry, and that button names the cause.
const OFFLINE_FAILED: &str = "Couldn\u{2019}t download for offline listening";

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
    Album(String),
    Artist(String),
    Playlist(String),
}

/// A click action captured during draw and dispatched on the next mouse
/// press. Owned data only, since the hit list is rebuilt every frame.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Nav(View),
    Recommendation(ui::tilo_resource::Edit<crate::model::Library>),
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
    BeginNewSpotifyPlaylist,
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
    SpotifyLibrary {
        track: String,
        save: bool,
    },
    SpotifyEffect(crate::actions::MusicEffect),
    OpenSpotifyPlaylistMenu(String),
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

/// One track table of the page and what its rows stand for: the queue a
/// press plays from, and the playlist or queue position the rows belong to.
struct TrackTableSlot {
    table: track_table::TrackTableHost,
    queue: Arc<Vec<String>>,
    playlist: Option<String>,
    queue_offset: Option<usize>,
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
    /// Anchors a search hit or an agent's `open` named, shown on the next
    /// sync as their rows' own effects.
    opened: Rc<std::cell::RefCell<Vec<String>>>,
    /// The committed heads and pending operation count that produced `lib`.
    /// Reuse the projection without committing a pending transaction to read it.
    library_heads: Option<(Vec<ChangeHash>, usize)>,
    /// Shared, preindexed Ctrl+K catalog. Cloning the provider is O(1), so
    /// host/provider retrieval never walks the whole music corpus.
    /// Monotonic diagnostic used by tests to prove provider retrieval does not
    /// trigger a catalog rebuild.
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
    pending_catalogue_query: Option<String>,
    catalogue_invocation: Option<tilo_actions::InvocationId>,
    /// The header's two actions, real retained buttons: open an audio file,
    /// and open the platform search for this app.
    open_file_button: ui::widget::Button,
    source_buttons: Vec<ui::widget::Button>,
    spotify_catalogue: bool,
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
    open_file_layout_visible: Option<(bool, bool, u8)>,
    back_button: ui::widget::Button,
    back_requested: std::rc::Rc<std::cell::Cell<bool>>,
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
    /// Real search results merged in-memory (rendered by the Search view and
    /// re-injected into the library snapshot each frame so they stay visible
    /// across a `sync` that reloads the catalog from the document).
    live_hits: Vec<CatalogueTrack>,
    /// The library as the document holds it, last presented: what the live
    /// hits add to it is searchable beside the library's own entries.
    stored_library: crate::model::Library,
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
    /// The body's scrolling: the toolkit's region, with its wheel, bar,
    /// keys and assistive scrolling.
    body_scroll: ui::widget::ScrollRegion,
    /// An offset asked for before this frame's content height is known
    /// (a view change, a restored position); applied after the body is
    /// laid out.
    scroll_target: Option<f64>,
    /// The theme of the last painted frame, for the event path.
    frame_theme: Option<Theme>,
    content_height: f64,

    /// Adaptive navigation sidebar: the shared [`ui::widget::sidebar::SidebarDrawer`]
    /// primitive. It owns the docked-vs-overlay decision, the eased reveal, the
    /// dimming scrim and the user-toggle memory, and hands back the resolved
    /// sidebar/content rects — so collapse clears cleanly (its own clip) and the
    /// panel never resizes against the content.
    drawer: SidebarDrawer,
    /// The window's History control, held in the content column's header
    /// row: the window draws no History bar above the app.
    history: ui::widget::HistoryOpener,
    /// Where the History control stands in the header row.
    history_rect: Rect,
    /// Height of the now-playing bar in the last painted theme, so the event
    /// path hit-tests the bar that was drawn.
    bar_h: f64,

    mouse: Point,
    hits: Vec<Hit>,
    /// How many of `hits` belong to the page body; the rest are the chrome
    /// painted over it, which takes the pointer first.
    body_hits: usize,
    /// The page's track tables, in the order this frame drew them, each
    /// with what its rows play and belong to.
    track_tables: Vec<TrackTableSlot>,
    /// How many of them this frame has drawn so far.
    track_tables_drawn: usize,
    /// The page's cover grids, in the order this frame drew them.
    media_grids: Vec<media_grid::MediaGridHost>,
    media_grids_drawn: usize,
    /// The page's other embedded parts (a detail page's actions, the genre
    /// cards), in the order this frame drew them.
    page_parts: Vec<page_part::PagePart>,
    page_parts_drawn: usize,
    /// The name of the row control under the pointer, for its tooltip.
    table_tooltip: Option<String>,
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
    /// Where a pointer takes the scrubber and the volume slider: their
    /// faces' hit rects, captured at paint.
    scrubber_hit: Rect,
    volume_hit: Rect,
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
    toast: Notice,

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
    /// A row of the page's first track table a capture shows under the
    /// pointer.
    staged_row_pointer: Option<usize>,
    /// Open the narrow transport's More menu once the bar is laid out.
    staged_player_menu: bool,

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
    /// Callers waiting for an effect's stream, download or picker to settle.
    effect_replies: Vec<(tilo_actions::InvocationId, ui::framework::EffectReply)>,
    offline_errors: HashMap<String, String>,

    /// Where library edits go: the framework's dispatcher, which applies
    /// each as the participant's attributed, undoable action. `None` until
    /// the surface is attached.
    dispatcher: Option<ui::framework::Dispatcher<crate::model::Library>>,
    // Drop after the audio engine/downloads so test-owned files are no longer open.
    #[cfg(test)]
    _test_cache_dir: tempfile::TempDir,
}

/// A deferred screenshot staging action (see [`MusicApp::stage_scenario`]).
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
    NewSpotifyPlaylist,
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

    /// The app for a window nobody plays in — a past version in History, a
    /// test: no network, and this device's playback profile is neither read
    /// nor written.
    pub(crate) fn offline() -> Self {
        let mut app = Self::new_with_device_profile(false);
        app.allow_network = false;
        app
    }

    /// A test's app: this device's playback profile is neither read nor
    /// written, the network is allowed.
    #[cfg(test)]
    pub(crate) fn without_profile() -> Self {
        Self::new_with_device_profile(false)
    }

    /// Keep a test's app off the network and away from the playback profile.
    #[cfg(test)]
    pub(crate) fn disconnect(&mut self) {
        self.allow_network = false;
        self.persist_device_profile = false;
    }

    fn new_with_device_profile(load_device_profile: bool) -> Self {
        // Optional initial-view override, used by `--shots` to capture
        // screens other than Home (e.g. `TILO_MUSIC_VIEW=album:al_glasslight`).
        let query = String::new();
        // A preset search query (`TILO_MUSIC_VIEW=search:…`) arms the debounce so
        // the live-shot loop dispatches it after the usual idle beat.
        let preset_search_at = (!query.is_empty()).then(ui::time::now);
        let persist_device_profile = load_device_profile && !cfg!(test);
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
        reason = "flat struct-literal initializing every MusicApp field once; splitting it would only relocate the field assignments, not reduce them"
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
            nav_back: Vec::new(),
            search_query: query,
            pending_catalogue_query: None,
            catalogue_invocation: None,
            open_file_button: Self::make_open_file_button(),
            source_buttons: Self::make_source_buttons(),
            spotify_catalogue: device_profile.spotify_catalogue,
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
            back_button: {
                let requested = std::rc::Rc::clone(&back_requested);
                ui::widget::Button::icon_only("music-back", ui::icons::Icon::ChevronLeft, "Back")
                    .on_view(move || requested.set(true))
            },
            back_requested,
            search_button: ui::search::search_button(ui::search::SearchRequest::this_app()),
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
            last_tick: ui::time::now(),
            shuffle_order: Vec::new(),
            shuffle_rng: seed_rng(),
            premute_volume: None,
            new_playlist_track: None,
            rect: Rect::ZERO,
            body_rect: Rect::ZERO,
            body_scroll: {
                let mut region = ui::widget::ScrollRegion::vertical();
                // The page is the window's content: its keys scroll it.
                region.set_focused(true);
                region
            },
            scroll_target: None,
            frame_theme: None,
            content_height: 0.0,
            drawer: SidebarDrawer::new().left(),
            history: ui::widget::HistoryOpener::new(),
            history_rect: Rect::ZERO,
            bar_h: 0.0,
            mouse: Point::ZERO,
            hits: Vec::new(),
            body_hits: 0,
            track_tables: Vec::new(),
            track_tables_drawn: 0,
            media_grids: Vec::new(),
            media_grids_drawn: 0,
            page_parts: Vec::new(),
            page_parts_drawn: 0,
            table_tooltip: None,
            drag: None,
            bar_press: None,
            scrubber_rect: Rect::ZERO,
            volume_rect: Rect::ZERO,
            scrubber_hit: Rect::ZERO,
            volume_hit: Rect::ZERO,
            volume_btn_rect: Rect::ZERO,
            volume_popover_open: false,
            volume_popover_rect: Rect::ZERO,
            volume_popover_slider: Rect::ZERO,
            queue_btn_rect: Rect::ZERO,
            queue_return: None,
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
            staged_player_menu: false,
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
            effect_replies: Vec::new(),
            offline_errors: HashMap::new(),
            // A test drives the surface on a document it builds; its edits
            // land there.
            #[cfg(test)]
            dispatcher: Some(ui::framework::testing::dispatcher(
                <crate::MusicShell as ui::framework::ResourceApp>::ID,
            )),
            #[cfg(not(test))]
            dispatcher: None,
            #[cfg(test)]
            _test_cache_dir: test_cache_dir,
        }
    }

    /// Open a global-search hit's deep link onto the real navigation / play
    /// path. This is the app side of the [`crate::search_type`] seam: the OS
    /// search host round-trips `music://{track,album,artist,playlist}/<id>`
    /// back to Music, and this routes it exactly like an in-app click —
    /// playing the track, or navigating to the album / artist / playlist page
    /// (the view's own `sync` then loads a live page on demand). Returns
    /// whether the link was recognised and routed.
    pub fn open_deep_link(&mut self, link: &str, state: &mut ui::widget::Edits<'_>) -> bool {
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

    /// Convert a published top-level destination id back to its concrete view.
    /// The resource layer only admits ids from `NAVIGATION_VIEWS`; this router
    /// keeps the final local navigation step equally explicit.
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

    // ---- state plumbing ---------------------------------------------------

    /// Refresh just the catalog/playlists/cache from the document. Runs
    /// every `sync` so remote library edits (a playlist gaining a track on
    /// another device) appear without disturbing local playback.
    fn reload_library(&mut self, state: &AutoCommit) {
        let heads = (
            state.base_heads(),
            automerge::transaction::Transactable::pending_ops(state),
        );
        if self.library_heads.as_ref() == Some(&heads) {
            return;
        }
        let next_library =
            <crate::model::Library as ui::tilo_resource::Resource>::read(state).unwrap_or_default();
        self.lib = next_library;
        self.inject_live_hits();
        self.library_heads = Some(heads);
        self.sync_playlist_covers();
    }

    /// Retire the old simulated-download seam. The render loop still invokes
    /// this, but it can never manufacture a false "cached" completion state.
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

    /// Mirror each playlist's persisted cover URL into the image cache so a
    /// cover set in a prior session (or by a peer) renders. Cheap: a handful of
    /// playlists, and the image store dedups fetches by URL.
    fn sync_playlist_covers(&mut self) {
        for pl in self.lib.playlists() {
            if !pl.cover.is_empty() {
                self.art_urls.insert(pl.id.clone(), pl.cover.clone());
            }
        }
    }

    /// Adopt the shared library snapshot. Playback intentionally starts from
    /// this device's local state rather than a peer's transport settings.
    fn reload(&mut self, state: &AutoCommit) {
        self.reload_library(state);
        self.bound_track = self.pb.current.clone();
        self.playhead_ms = self.pb.position_ms as f64;
    }

    fn persist_playback(&mut self, _state: &mut ui::widget::Edits<'_>) {
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
        self.prepare_spotify_queue();
        self.report_device_profile_error();
    }

    fn persist_device_profile(&self) -> Result<(), String> {
        if !self.persist_device_profile {
            return Ok(());
        }
        crate::storage::save_device_profile(&crate::storage::DeviceProfile {
            spotify_catalogue: self.spotify_catalogue,
            local_file: self.local_file.clone(),
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

    /// Show a transient message: a confirmation (`Success`), something under
    /// way (`Info`) or a failure (`Error`).
    fn notify(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toast.show(text, kind);
    }

    fn update_device_cache(&mut self, id: &str, state: CacheState) {
        if matches!(state, CacheState::Streamed) {
            self.device_cache.remove(id);
        } else {
            self.device_cache.insert(id.to_string(), state);
        }
    }

    /// Whether this device holds `id`'s media: a download in flight or a
    /// durable cached copy. Never shared with other devices.
    pub(crate) fn cache_state(&self, id: &str) -> CacheState {
        self.device_cache.get(id).copied().unwrap_or_default()
    }

    // ---- action dispatch --------------------------------------------------

    /// Submit one verb or field edit of the library through the framework's
    /// dispatcher — the one door the Ctrl+K palette, agents and every
    /// on-screen affordance share: checked against the participant's
    /// permissions, attributed, undoable and replayed on peers. Callers
    /// reload the in-memory library from the document afterward for
    /// immediate feedback.
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

    /// Show the shared library, keeping this device's cache.
    /// An empty projection does not wipe a library that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::model::Library) {
        self.stored_library.clone_from(state);
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

    /// Send library edits through the framework's dispatcher.
    pub(crate) fn attach_dispatcher(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::model::Library>,
    ) {
        self.dispatcher = Some(dispatcher);
    }

    /// The context Ctrl+K needs to build Music's resource tree: the user's
    /// playlists and their tracks, read from the in-memory library cache (itself
    /// re-projected from the document each `sync`). The virtual "Liked Songs"
    /// playlist is a projection of the like set, not a stored playlist, so it is
    /// not surfaced here — only real, mutable playlists are.
    pub(crate) fn music_resource_ctx(&self) -> crate::actions::MusicResourceCtx {
        let mut playlists = self.lib.playlists().collect::<Vec<_>>();
        playlists.sort_by(|a, b| a.id.cmp(&b.id));
        let playlists = playlists
            .into_iter()
            .take(crate::actions::OPEN_RESOURCE_CATEGORY_LIMIT)
            .map(|p| crate::actions::PlaylistResource {
                id: p.id.clone(),
                name: p.name.clone(),
                tracks: p
                    .tracks
                    .values()
                    .take(crate::actions::OPEN_PLAYLIST_TRACK_LIMIT)
                    .map(|tid| crate::actions::TrackRef {
                        id: tid.clone(),
                        label: self.track_title(tid),
                        artist: self.track_artist(tid),
                        liked: self.lib.is_liked(tid),
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
                    offline: matches!(self.cache_state(&track.id), CacheState::Cached),
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
                    followed: self.artist_followed(&artist.id),
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

    /// The authoritative search result's metadata, for an admitted effect
    /// importing a catalogue song into the shared library.
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

    /// The rows this device shows beside the library: its player, queue and
    /// open file, and the tracks it keeps offline, which offer their removal.
    pub(crate) fn device_rows(&self) -> Vec<ui::framework::LiveRow> {
        let ctx = self.music_resource_ctx();
        let mut rows: Vec<_> = crate::actions::library_rows(&ctx)
            .into_iter()
            .chain(crate::recommendations::resources(&self.lib))
            .chain(crate::actions::live_device_resources(&ctx))
            .map(live_row)
            .collect();
        rows.push(self.source_row());
        rows.extend(self.spotify_device_rows());
        rows.extend(self.spotify_remote_rows());
        // Catalogue tracks have not entered the shared document. Publish the
        // same admitted live row that resolves search hits, so albums and
        // remote searches offer playback to agents and assistive technology.
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

    /// The row `id` this device shows, beyond the published page: a queue
    /// entry, the player, the open file, a track it keeps offline.
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
        if let Some(resource) = self
            .catalogue_resources()
            .into_iter()
            .find(|row| row.id.as_str() == id)
        {
            return Some(live_row(resource));
        }
        if id.starts_with("music.recommendation") {
            return crate::recommendations::resource_by_id(&self.lib, id).map(live_row);
        }
        if let Some(track) = id.strip_prefix("music.track:")
            && !self.stored_library.tracks.contains_key(track)
        {
            // A catalogue song the library does not hold yet: its row's
            // like imports the real song first.
            return self
                .live_hits
                .iter()
                .any(|hit| hit.track.id == track)
                .then(|| self.publish_resource(id).map(live_row))
                .flatten();
        }
        // The player, the queue and its entries, the open file, and the
        // library's tracks, albums, artists, playlists, memberships and views.
        self.publish_resource(id).map(live_row)
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
                        |name| format!("{} {name}", crate::i18n::t("music-spotify-output")),
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

    /// A track this device keeps offline, offering to remove its copy.
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
        if id == crate::actions::LOCAL_FILE_RESOURCE_ID || id == "music.player:player" {
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
        let mut ctx = crate::actions::MusicResourceCtx::for_document_id(&self.lib, id)?;
        for track in &mut ctx.tracks {
            track.offline = matches!(self.cache_state(&track.id), CacheState::Cached);
        }
        for artist in &mut ctx.artists {
            artist.followed = self.artist_followed(&artist.id);
        }
        Some(ctx)
    }

    /// A one-line "Title — Artist" label for a track id, for the palette.
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

    /// Decoder teardown at local EOS releases the live position query. Retain
    /// only the end position whose duration the decoder actually observed;
    /// catalogue metadata and transport intent cannot establish completion.
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
            controller_bus_name: Some(crate::mpris::BUS_NAME),
        });
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
    fn drain_mpris(&mut self, state: &mut ui::widget::Edits<'_>) {
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
                self.notify(ToastKind::Success, "Playlist cover updated");
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
        // Catalogue duration is display metadata, not an output completion
        // receipt. Only the decoder's EOS (including governed output drain)
        // above may advance; crossing the duration can still leave PCM queued.
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
            Action::BeginNewSpotifyPlaylist => {
                self.on_begin_new_playlist(None);
                self.name_prompt = Some(NamePrompt::NewSpotifyPlaylist);
            }
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
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::CloseSidebar => self.drawer.close(),
            Action::CloseNamePrompt => self.on_close_name_prompt(),
            Action::None => {}
        }
    }

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
        if v != self.view {
            self.nav_back.push(self.view.clone());
            self.view = v;
            self.set_scroll_y(0.0);
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
            self.set_scroll_y(0.0);
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

    fn on_add_to_queue(&mut self, id: &str, state: &mut ui::widget::Edits<'_>) {
        self.pb.queue.push(id.to_string());
        self.notify(ToastKind::Success, "Added to the queue");
        self.persist_playback(state);
    }

    fn on_play_next(&mut self, id: &str, state: &mut ui::widget::Edits<'_>) {
        self.queue_play_next(id.to_string());
        self.notify(ToastKind::Success, "Playing next");
        if self.pb.current.as_deref() == Some(id) {
            self.start_stream(id);
        }
        self.persist_playback(state);
    }

    fn on_toggle_like(&mut self, id: &str, state: &mut ui::widget::Edits<'_>) {
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
        self.nav_back.push(self.view.clone());
        self.view = View::Playlist(id);
        self.set_scroll_y(0.0);
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
        // Leave the now-deleted detail page.
        if self.view == View::Playlist(id) {
            self.view = View::Playlists;
            self.set_scroll_y(0.0);
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
                self.set_scroll_y(s);
            } else if let Some(v) = self.nav_back.pop() {
                self.view = v;
                self.set_scroll_y(0.0);
            }
        } else {
            // Toggle on: remember the current view + scroll so the next
            // toggle returns here.
            self.queue_return = Some((self.view.clone(), self.scroll_y()));
            self.view = View::Queue;
            self.set_scroll_y(0.0);
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
        self.toast.show(message, ToastKind::Error);
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
                                self.home_requested = false;
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
                    self.source_error = Some(error);
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

    fn catalogue_resources(&self) -> Vec<tilo_actions::Resource> {
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
            .filter_map(|(id, label)| crate::actions::live_catalogue_resource(&id, label))
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
        if self.request_page(id, StreamRequest::Artist(id.to_string()), "artist") {
            self.artist_requested.insert(id.to_string());
        }
    }

    /// Request a real album page for an album id (once each).
    fn ensure_album(&mut self, id: &str) {
        if !self.allow_network || !is_live_album_id(id) || self.album_requested.contains(id) {
            return;
        }
        if self.request_page(id, StreamRequest::Album(id.to_string()), "album") {
            self.album_requested.insert(id.to_string());
        }
    }

    /// Request a real (live YouTube-Music) playlist page for an id (once each).
    fn ensure_playlist(&mut self, id: &str) {
        if !self.allow_network || !is_live_playlist_id(id) || self.playlist_requested.contains(id) {
            return;
        }
        if self.request_page(id, StreamRequest::Playlist(id.to_string()), "playlist") {
            self.playlist_requested.insert(id.to_string());
        }
    }

    /// Send the page `request` for `id` to the worker; whether it was sent.
    /// A refused request marks the page failed and says so.
    fn request_page(&mut self, id: &str, request: StreamRequest, what: &str) -> bool {
        self.ensure_worker();
        let Some(w) = &self.worker else {
            return false;
        };
        match w.request(request) {
            Ok(()) => true,
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

    /// How far the body is scrolled.
    fn scroll_y(&self) -> f64 {
        self.scroll_target
            .unwrap_or_else(|| self.body_scroll.offset().y)
    }

    /// Scroll the body to `y`; the next frame clamps it to its content.
    fn set_scroll_y(&mut self, y: f64) {
        self.scroll_target = Some(y.max(0.0));
        self.dirty = true;
    }

    fn push_hit(&mut self, rect: Rect, action: Action) -> bool {
        let hovered = rect.contains(self.mouse);
        self.hits.push(Hit { rect, action });
        hovered
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

    /// Whether the sidebar toggle sits in the window's header row rather
    /// than in the docked sidebar's own row: whenever the sidebar is hidden
    /// or floats.
    fn toggle_in_content(&self) -> bool {
        !self.drawer.is_open() || self.drawer.is_overlay()
    }

    /// Flip the sidebar open/closed for the current layout.
    fn toggle_sidebar(&mut self) {
        self.drawer.toggle();
        self.dirty = true;
    }

    /// Take up what the launch named: a `music://` link plays the track
    /// (continuing a handoff's position once the track is in the library)
    /// or opens the album, artist or playlist; anything else is a local
    /// audio file to play.
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

    /// Stage the view state of the screenshot scenario `scenario`, once its
    /// library is in place.
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
            _ => {}
        }
    }

    /// Stage the sidebar for a deterministic `--shots` frame (no event loop to
    /// drive the reveal animation). `open` snaps it fully revealed.
    /// Before the first layout the drawer keeps the snap until it knows the
    /// window's size class.
    pub fn snap_sidebar(&mut self, open: bool) {
        self.drawer.snap(open);
        self.dirty = true;
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

    /// Build + open the "Add to playlist…" secondary picker.
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
pub(crate) const LIVE_TRACK_PREFIX: &str = "yt:";

pub(crate) fn live_track_id(source_key: &str) -> String {
    if source_key.starts_with("spotify:track:") {
        source_key.to_owned()
    } else {
        format!("{LIVE_TRACK_PREFIX}{source_key}")
    }
}

/// Whether a track id refers to a real (streamable) YouTube-Music track.
fn is_live_track(track_id: &str) -> bool {
    track_id.starts_with(LIVE_TRACK_PREFIX) || track_id.starts_with("spotify:track:")
}

/// Whether an id looks like a real `YouTube` channel id (artist page fetchable).
fn is_live_id(id: &str) -> bool {
    id.starts_with("UC") || id.starts_with("spotify:artist:")
}

/// Whether an id looks like a real YouTube-Music album/playlist browse id.
fn is_live_album_id(id: &str) -> bool {
    id.starts_with("MPRE") || id.starts_with("OLAK") || id.starts_with("spotify:album:")
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
    let nanos = ui::time::try_unix_now_ns().unwrap_or(0x1234_5678);
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

/// Convert a real search hit into a library-importable [`CatalogueTrack`],
/// minting stable ids where `YouTube` didn't supply them.
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
pub(crate) fn image_fetcher() -> ui::Fetcher {
    // No HTTP stack, but a portal-picked portable PNG still renders offline.
    Arc::new(embedded_cover_bytes)
}

mod local_files;
mod media_grid;
mod page_part;
mod recommendations;
mod render;
mod spotify;
mod track_table;

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests;

/// `resource`, one of the rows this device shows, as the surface publishes it.
fn live_row(resource: tilo_actions::Resource) -> ui::framework::LiveRow {
    let mut row = ui::framework::LiveRow::new(resource.id.as_str(), resource.kind, resource.label);
    for action in resource.actions {
        row = row.operation(action);
    }
    for child in resource.children {
        row = row.row(live_row(child));
    }
    row
}

#[cfg(test)]
impl MusicApp {
    /// The page the window shows.
    pub(crate) fn view_for_test(&self) -> View {
        self.view.clone()
    }
}

impl MusicApp {
    /// What opens a row's anchor for [`ui::framework::ViewCx::on_open`]: a
    /// song plays, an album, artist, playlist, membership or place is shown.
    /// An anchor naming no row Music opens is refused.
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

    /// Open the anchors [`Self::opener`] took, as their rows' effects do.
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

impl MusicApp {
    /// The library's field edits bound to the row `row`, as the document
    /// holds it now: an album is saved or unsaved, an artist followed or
    /// unfollowed (a Spotify artist through its provider effect), a song
    /// taken out of a playlist or moved one place, a recommendation input
    /// switched, a suggestion hidden or shown again, an artist preferred.
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

/// Setting `playlist`'s cover and adding a song of the library at its end,
/// each asking for its value. Renaming is every labelled item's own.
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

/// Taking `track` out of `playlist` and moving it one place, while the
/// playlist holds it.
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

/// The recommendations' switch on an input row, hiding or showing again a
/// suggestion, and preferring an artist.
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
