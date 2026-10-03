//! The local video-player app: an edge-to-edge reusable
//! [`ui::widget::VideoPlayer`] driven by a shared [`media::MediaEngine`].
//!
//! `PlayerApp` is the root widget. The video fills the whole surface — there is
//! **no persistent app chrome bar** (the OS owns window chrome; HIG §1e wants
//! video immersive). Library, the media title and Open float over the picture
//! in a strip that appears with the player's controls and auto-hides during
//! playback (`strip`), and the library itself is a sheet over a scrim
//! (`library`). With no picture the same area says why: nothing is open, a
//! first frame is coming, or the open failed. It opens files through the XDG
//! `FileChooser` portal (or `--open PATH`), and — when a collab session is
//! attached — keeps playback in sync across peers via the shared [`media`]
//! transport.

mod library;
mod strip;

use library::{
    local_library_accessibility_nodes, shared_accessibility_nodes, shared_accessibility_targets,
};

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Size, Style, TaffyTree};
use tilo_clock::unix_now_ms;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager};
use ui::icons::Icon;
use ui::paint::fill_rect;
use ui::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, PickRequest, PickedContents, PortalChannel,
    PortalOutcome,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::video_player::{PlayerSetting, PlayerSettingEvent};
use ui::widget::{
    ActionEmitter, Button, ButtonFace, FloatingSurfaceProvider, FullscreenProvider, SidebarDrawer,
    VideoPlayer, Widget,
};

use crate::actions::PlayerOperation;
use crate::collab::{CollabPlayback, Role};
use crate::preferences::{PlaybackPreferences, RecentMedia};

/// A decoder open is asynchronous. Do not turn portal/direct admission into a
/// false successful history entry until a first picture has actually arrived.
const OPEN_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
/// One file-picker request at a time.  A portal response is asynchronous and
/// cannot be cancelled at the protocol level, so the app owns the receiver and
/// drops it when a newer direct open supersedes the request.  `generation` is
/// deliberately separate from the media generation: it identifies *this
/// chooser* rather than the media that may eventually be selected.
struct PendingPortalOpen {
    channel: PickerChannel,
    generation: u64,
    invocation: Option<tilo_actions::InvocationId>,
}

/// A host with paths answers with the chosen path; a host without them (a
/// browser page) hands over the file's bytes.
enum PickerChannel {
    Path(PortalChannel),
    Contents(ContentsChannel),
}

/// What the picker chose, in the form the host could give it.
enum Picked {
    Path(PathBuf),
    Contents(PickedContents),
}

/// A terminal response paired with the request identity and optional action
/// caller that owns it. Keeping this named makes `drain_portal`'s stale-result
/// guard readable without a deeply nested tuple type.
type PortalOpenOutcome = (
    u64,
    Option<tilo_actions::InvocationId>,
    Result<Picked, Option<String>>,
);

/// A canonical open invocation awaiting its decoder's first picture. This is
/// separate from the file-picker request: the picker can succeed while decode
/// fails, stalls, or is superseded before a frame appears.
struct PendingMediaOpen {
    invocation: tilo_actions::InvocationId,
    source_id: String,
    deadline: ui::time::Instant,
}

/// A file-backed local record. The source is deliberately kept only in this
/// process; public resources expose the opaque `id` and friendly basename.
#[derive(Clone)]
struct LocalMediaEntry {
    id: String,
    title: String,
    source: media::Source,
}

#[derive(Clone)]
struct LibraryRowHit {
    id: String,
    row: Rect,
    play: Rect,
    remove: Option<Rect>,
}

#[derive(Clone)]
struct SharedRowHit {
    id: String,
    jump: Option<Rect>,
    remove: Rect,
    annotation: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryTab {
    Queue,
    Recent,
    Shared,
}

/// A capture scenario that plays a real file: `open:` and its path.
pub(crate) const OPEN_CAPTURE: &str = "open:";

/// Every named capture scenario.
pub(crate) const CAPTURE_SCENARIOS: &[&str] = &[
    "empty",
    "loading",
    "error",
    "native-error",
    "playing",
    "paused",
    "settings",
    "metadata",
    "library",
    "library-focus",
    "recent",
    "shared",
    "fullscreen",
];

/// The clip a capture plays, and what the library lists beside it.
const DEMO_TITLE: &str = "Harbour at dusk";
const DEMO_QUEUE: &[(&str, &str)] = &[
    ("https://demo.local/market.mp4", "Market morning"),
    ("https://demo.local/night-train.mp4", "Night train"),
];

fn demo_source() -> media::Source {
    media::Source::Remote {
        url: "https://demo.local/harbour.mp4".into(),
    }
}

/// The opaque reference a shared bookmark on the demo clip carries.
pub(crate) fn demo_media_ref() -> String {
    local_source_token(&demo_source(), 0)
}

fn configured_player(autoplay: bool, loop_enabled: bool) -> VideoPlayer {
    let mut player = VideoPlayer::new()
        .with_engine(media::MediaEngine::new())
        .with_test_id("video_surface");
    player.enable_mpris("tilo_video_player", "Video Player");
    configure_player_settings(&mut player, autoplay, loop_enabled);
    player
}

/// The playback speeds the settings menu offers.
const PLAYBACK_RATES: [f32; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0];

fn configure_player_settings(player: &mut VideoPlayer, autoplay: bool, loop_enabled: bool) {
    // These settings are implemented by this app, so the shared settings
    // popover is never decorative.
    player.set_setting(PlayerSetting::toggle(
        "autoplay",
        Icon::Play,
        "Autoplay",
        0,
        autoplay,
    ));
    player.set_setting(PlayerSetting::toggle(
        "loop",
        Icon::Refresh,
        "Repeat current video",
        0,
        loop_enabled,
    ));
    let rate = player.engine().map_or(1.0, media::MediaEngine::rate);
    let selected = PLAYBACK_RATES
        .iter()
        .position(|candidate| (*candidate - rate).abs() < f32::EPSILON)
        .unwrap_or(3);
    player.set_setting(PlayerSetting::choice(
        "playback-rate",
        Icon::Timer,
        "Playback speed",
        0,
        PLAYBACK_RATES
            .iter()
            .map(|rate| format!("{rate}×"))
            .collect(),
        selected,
    ));
    player.set_setting(PlayerSetting::button(
        "picture-in-picture",
        Icon::PictureInPicture2,
        "Picture in Picture",
        0,
    ));
}

/// Build a taffy node from `children` already built in the same tree.
/// `new_with_children` only fails when a child id does not resolve in this
/// tree, which cannot happen for children this same call just built.
#[allow(
    clippy::unwrap_used,
    reason = "new_with_children cannot fail for children built in this same tree; see doc comment"
)]
fn parent_node(taffy: &mut TaffyTree<()>, style: Style, children: &[NodeId]) -> NodeId {
    taffy.new_with_children(style, children).unwrap()
}

fn find_resource<'a>(
    resources: &'a [tilo_actions::Resource],
    id: &str,
) -> Option<&'a tilo_actions::Resource> {
    resources.iter().find_map(|resource| {
        if resource.id.0 == id {
            Some(resource)
        } else {
            find_resource(&resource.children, id)
        }
    })
}

fn video_open_request() -> PickRequest {
    PickRequest::open("Open a video", "Play")
        .thread_name("video-player-portal")
        .filter(
            FileFilter::new("Video")
                .mimetype("video/mp4")
                .mimetype("video/webm")
                .mimetype("video/x-matroska")
                .mimetype("video/quicktime")
                .glob("*.mp4")
                .glob("*.webm")
                .glob("*.mkv")
                .glob("*.mov")
                .glob("*.avi"),
        )
        .all_files()
}

/// Build a [`media::Source`] for a filesystem path: a path under the FUSE
/// workspace mount becomes a [`media::Source::Workspace`] (read-through,
/// hydrate-on-demand); anything else is a [`media::Source::LocalFile`].
pub fn source_for_path(path: &Path) -> media::Source {
    let root = media::Source::workspace_root();
    if path.starts_with(&root) {
        media::Source::Workspace(path.to_path_buf())
    } else {
        media::Source::LocalFile(path.to_path_buf())
    }
}

/// Opaque, deterministic per-process identity for a local source. Resource
/// IDs and action payload guards need a stable token, but publishing a URI
/// would expose an absolute path to search, history, or a collaborator.
fn local_source_token(source: &media::Source, generation: u64) -> String {
    // FNV-1a is used only as an opaque UI identifier, not as a security
    // boundary. The monotonic generation prevents a same-path reopen from
    // accepting stale dynamic stream/chapter actions.
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, media::session::session_key(source).as_bytes());
    let hash = std::hash::Hasher::finish(&hasher);
    format!("local-media-{hash:016x}-open-{generation}")
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "overlay, local library, portal, and frame-dirty flags are independent widget state"
)]
pub struct PlayerApp {
    player: VideoPlayer,
    title: String,
    rect: Rect,
    /// The Open button's hit rect (top-right of the video). Zero while there
    /// is nothing open: the empty state owns Open then.
    open_button_rect: Rect,
    library_button_rect: Rect,
    /// The action of the idle or error state over the picture.
    status_action_rect: Rect,
    /// The library sheet: a trailing drawer over the video.
    drawer: SidebarDrawer,
    /// Where the sheet's lists scroll to, and how far they can.
    library_scroll: f64,
    library_max_scroll: f64,
    /// The area of the sheet its rows live in; rows outside it do not act.
    library_list_rect: Rect,
    library_queue_tab_rect: Rect,
    library_recent_tab_rect: Rect,
    library_shared_tab_rect: Rect,
    library_close_rect: Rect,
    library_row_hits: Vec<LibraryRowHit>,
    library_clear_rect: Rect,
    shared_watch_later_rect: Rect,
    shared_bookmark_rect: Rect,
    shared_row_hits: Vec<SharedRowHit>,
    library_visible: bool,
    library_tab: LibraryTab,
    queue: Vec<LocalMediaEntry>,
    recent: VecDeque<LocalMediaEntry>,
    current_queue_id: Option<String>,
    last_cursor: Point,
    /// A file-open portal request in flight, drained each `sync`. Requests are
    /// never replaced: doing so would let an old chooser response unexpectedly
    /// open a file after the user chose a newer source.
    portal_open: Option<PendingPortalOpen>,
    media_open: Option<PendingMediaOpen>,
    portal_generation: u64,
    resource_results: VecDeque<tilo_actions::InvocationUpdate>,
    open_action: Option<ActionEmitter<()>>,
    library_control_invocations: VecDeque<tilo_actions::ControlInvocation>,
    open_focused: bool,
    library_focused: bool,
    library_focus_id: Option<String>,
    /// Collaborative playback state (host/follower). `None` for a local-only
    /// session or the headless `--shots` path.
    collab: Option<CollabPlayback>,
    /// The collab socket. `None` unless a collab session is attached.
    manager: Option<CollaborationManager>,
    /// App-owned options whose state is surfaced through the settings resource.
    preferences: PlaybackPreferences,
    /// Deterministic decoder metadata used only by the explicit headless
    /// `metadata` screenshot fixture. Live runs always read the active engine.
    shot_metadata: Option<DecoderMetadata>,
    /// Deterministically holds the production no-frame/loading presentation
    /// for visual regression capture while the synthesized engine stays
    /// hermetic. Live runs always derive loading from decoder frame state.
    shot_loading: bool,
    pending_pip: Option<ui::floating_surface::FloatingSurfaceRequest>,
    actions: crate::resource::ActionSink,
    /// Framework-hosted surfaces leave the palette tree to
    /// `tilo_resource::publish`; standalone keeps the contextual builder.
    framework_hosted: bool,
    shared_library: crate::shared::SharedLibrary,
    /// Opaque source identity for durable shared playlists and markers. It
    /// survives reopening the same local media, unlike the per-open source ID.
    current_media_ref: Option<String>,
    current_source_id: Option<String>,
    source_generation: u64,
    search_cache: RefCell<Option<SearchCache>>,
    search_catalog_builds: Cell<u64>,
    metadata_revision: u64,
    search_metadata: DecoderMetadata,
    /// A human-readable failure from the latest open request. It is only
    /// painted in the no-frame state, preserving a successfully playing video
    /// when a later picker request fails.
    open_error: Option<String>,
    /// Test-only observation of the authoritative retained portal failure.
    /// Production builds carry no observer or additional settlement path.
    #[cfg(test)]
    history_test_open_error_observer: Option<std::rc::Rc<std::cell::RefCell<Option<String>>>>,
    /// Where files have no path: the kept copy (`crate::kept`) of each
    /// queue or recent entry opened by contents, by entry id.
    kept: HashMap<String, String>,
    /// Kept copies still on disk, newest first.
    kept_retained: Vec<String>,
    /// A kept copy being read back, and the entry it reopens.
    kept_recall: Option<KeptRecall>,
    kept_wake: ui::wake::WakeHandle,
    dirty: bool,
}

struct KeptRecall {
    bytes: std::sync::mpsc::Receiver<Option<Vec<u8>>>,
    token: String,
    title: String,
    queue_id: Option<String>,
}

#[derive(Clone, Default, PartialEq)]
struct DecoderMetadata {
    audio_tracks: Vec<crate::actions::TrackState>,
    subtitle_tracks: Vec<crate::actions::TrackState>,
    chapters: Vec<crate::actions::ChapterState>,
}

struct SearchCache {
    source_id: Option<String>,
    revision: u64,
    provider: Arc<crate::search::VideoPlayerSearchProvider>,
}

impl PlayerApp {
    fn action_context<'a>(
        &'a self,
        audio_tracks: &'a [crate::actions::TrackState],
        subtitle_tracks: &'a [crate::actions::TrackState],
        chapters: &'a [crate::actions::ChapterState],
    ) -> crate::actions::PlayerResourceCtx<'a> {
        crate::actions::PlayerResourceCtx {
            title: &self.title,
            media: crate::actions::MediaAvailability {
                has_media: self.player.has_media(),
                source_id: self.current_source_id.clone(),
                media_ref: self.current_media_ref.clone(),
                can_control_transport: self
                    .collab
                    .as_ref()
                    .is_none_or(|collab| collab.role() == Role::Host),
            },
            settings: crate::actions::PlaybackSettingsState {
                autoplay: self.preferences.autoplay,
                loop_enabled: self.preferences.loop_enabled,
                rate: self.player.engine().map_or(1.0, media::MediaEngine::rate),
            },
            audio_tracks,
            subtitle_tracks,
            chapters,
            library: crate::actions::LocalLibraryState {
                queue: self.library_items(self.queue.iter()),
                recent: self.library_items(self.recent.iter()),
            },
        }
    }

    fn library_items<'a>(
        &self,
        entries: impl Iterator<Item = &'a LocalMediaEntry>,
    ) -> Vec<crate::actions::LocalMediaItemState> {
        entries
            .map(|entry| crate::actions::LocalMediaItemState {
                id: entry.id.clone(),
                title: entry.title.clone(),
                current: self.current_queue_id.as_deref() == Some(entry.id.as_str()),
            })
            .collect()
    }

    fn decoder_metadata(
        &self,
    ) -> (
        Vec<crate::actions::TrackState>,
        Vec<crate::actions::TrackState>,
        Vec<crate::actions::ChapterState>,
    ) {
        if let Some(metadata) = &self.shot_metadata {
            return (
                metadata.audio_tracks.clone(),
                metadata.subtitle_tracks.clone(),
                metadata.chapters.clone(),
            );
        }
        let audio_tracks = self
            .player
            .engine()
            .map(media::MediaEngine::audio_tracks)
            .unwrap_or_default()
            .into_iter()
            .map(|track| crate::actions::TrackState {
                stream_id: track.id,
                index: track.index,
                label: track.label,
                selected: track.selected,
            })
            .collect();
        let subtitle_tracks = self
            .player
            .engine()
            .map(media::MediaEngine::subtitle_tracks)
            .unwrap_or_default()
            .into_iter()
            .map(|track| crate::actions::TrackState {
                stream_id: track.id,
                index: track.index,
                label: track.label,
                selected: track.selected,
            })
            .collect();
        let chapters = self
            .player
            .engine()
            .map(media::MediaEngine::chapters)
            .unwrap_or_default()
            .into_iter()
            .map(|chapter| crate::actions::ChapterState {
                id: chapter.id,
                label: chapter.label,
                seconds: chapter.start_ms as f64 / 1000.0,
            })
            .collect();
        (audio_tracks, subtitle_tracks, chapters)
    }

    fn sync_decoder_settings(&mut self) {
        let (audio_tracks, subtitle_tracks, chapters) = self.decoder_metadata();
        let metadata = DecoderMetadata {
            audio_tracks: audio_tracks.clone(),
            subtitle_tracks: subtitle_tracks.clone(),
            chapters: chapters.clone(),
        };
        if self.search_metadata == metadata {
            return;
        }
        self.search_metadata = metadata;
        self.invalidate_search_cache();
        let audio_limit = audio_tracks
            .len()
            .min(crate::actions::CONTEXTUAL_CHILD_LIMIT);
        let subtitle_limit = subtitle_tracks
            .len()
            .min(crate::actions::CONTEXTUAL_CHILD_LIMIT);
        let chapter_limit = chapters.len().min(crate::actions::CONTEXTUAL_CHILD_LIMIT);
        let audio_tracks = audio_tracks.get(..audio_limit).unwrap_or(&audio_tracks);
        let subtitle_tracks = subtitle_tracks
            .get(..subtitle_limit)
            .unwrap_or(&subtitle_tracks);
        let chapters = chapters.get(..chapter_limit).unwrap_or(&chapters);
        if audio_tracks.is_empty() {
            self.player.remove_setting("audio-track");
        } else {
            let selected = audio_tracks
                .iter()
                .position(|track| track.selected)
                .unwrap_or(0);
            self.player.set_setting(PlayerSetting::choice(
                "audio-track",
                Icon::Volume2,
                "Audio track",
                0,
                audio_tracks
                    .iter()
                    .map(|track| track.label.clone())
                    .collect(),
                selected,
            ));
        }
        if subtitle_tracks.is_empty() {
            self.player.remove_setting("subtitle-track");
        } else {
            let selected = subtitle_tracks
                .iter()
                .position(|track| track.selected)
                .map_or(0, |index| index.saturating_add(1));
            let mut options = vec!["Off".to_string()];
            options.extend(subtitle_tracks.iter().map(|track| track.label.clone()));
            self.player.set_setting(PlayerSetting::choice(
                "subtitle-track",
                Icon::FileText,
                "Subtitles",
                0,
                options,
                selected,
            ));
        }
        if chapters.is_empty() {
            self.player.remove_setting("chapters");
        } else {
            self.player.set_setting(PlayerSetting::choice(
                "chapters",
                Icon::ListVideo,
                "Chapters",
                0,
                chapters
                    .iter()
                    .map(|chapter| chapter.label.clone())
                    .collect(),
                0,
            ));
        }
    }

    /// A player with a fresh engine and no media loaded.
    pub fn new() -> Self {
        Self::new_with_preferences(PlaybackPreferences::load())
    }

    fn new_with_preferences(preferences: PlaybackPreferences) -> Self {
        // Tests exercise the real request and completion paths; a recording
        // host answers every picker so no desktop file chooser ever opens.
        #[cfg(test)]
        ui::host::install_default(ui::host::HostServices::recording().0);
        let player = configured_player(preferences.autoplay, preferences.loop_enabled);
        let recent: VecDeque<LocalMediaEntry> = crate::preferences::load_recent_history()
            .into_iter()
            .map(|entry| LocalMediaEntry {
                id: entry.id,
                title: entry.title,
                source: entry.source,
            })
            .collect();
        Self::from_prepared(player, preferences, recent)
    }

    fn from_prepared(
        player: VideoPlayer,
        preferences: PlaybackPreferences,
        recent: VecDeque<LocalMediaEntry>,
    ) -> Self {
        let kept_retained = recent
            .iter()
            .filter_map(|entry| crate::kept::token_of(&entry.source).map(str::to_owned))
            .collect();
        let action_ctx = crate::actions::PlayerResourceCtx {
            title: "No media",
            media: crate::actions::MediaAvailability {
                has_media: false,
                source_id: None,
                media_ref: None,
                can_control_transport: true,
            },
            settings: crate::actions::PlaybackSettingsState {
                autoplay: preferences.autoplay,
                loop_enabled: preferences.loop_enabled,
                rate: player.engine().map_or(1.0, media::MediaEngine::rate),
            },
            audio_tracks: &[],
            subtitle_tracks: &[],
            chapters: &[],
            library: crate::actions::LocalLibraryState::default(),
        };
        let action_surface = crate::actions::resources(&action_ctx, &AutoCommit::new());
        let open_action = action_surface
            .first()
            .and_then(|resource| tilo_actions::ActionBinding::unit_for(resource, "open").ok())
            .map(ActionEmitter::new);
        Self {
            player,
            title: "No media".to_string(),
            rect: Rect::ZERO,
            open_button_rect: Rect::ZERO,
            library_button_rect: Rect::ZERO,
            status_action_rect: Rect::ZERO,
            drawer: SidebarDrawer::new()
                .right()
                .closed()
                .breakpoint(f64::INFINITY),
            library_scroll: 0.0,
            library_max_scroll: 0.0,
            library_list_rect: Rect::ZERO,
            library_queue_tab_rect: Rect::ZERO,
            library_recent_tab_rect: Rect::ZERO,
            library_shared_tab_rect: Rect::ZERO,
            library_close_rect: Rect::ZERO,
            library_row_hits: Vec::new(),
            library_clear_rect: Rect::ZERO,
            shared_watch_later_rect: Rect::ZERO,
            shared_bookmark_rect: Rect::ZERO,
            shared_row_hits: Vec::new(),
            library_visible: false,
            library_tab: LibraryTab::Queue,
            queue: Vec::new(),
            recent,
            current_queue_id: None,
            last_cursor: Point::ZERO,
            portal_open: None,
            media_open: None,
            portal_generation: 0,
            resource_results: VecDeque::new(),
            open_action,
            library_control_invocations: VecDeque::new(),
            open_focused: false,
            library_focused: false,
            library_focus_id: None,
            collab: None,
            manager: None,
            preferences,
            shot_metadata: None,
            shot_loading: false,
            pending_pip: None,
            actions: crate::resource::ActionSink::default(),
            framework_hosted: false,
            shared_library: crate::shared::SharedLibrary::default(),
            current_media_ref: None,
            current_source_id: None,
            source_generation: 0,
            search_cache: RefCell::new(None),
            search_catalog_builds: Cell::new(0),
            metadata_revision: 0,
            search_metadata: DecoderMetadata::default(),
            open_error: None,
            #[cfg(test)]
            history_test_open_error_observer: None,
            kept: HashMap::new(),
            kept_retained,
            kept_recall: None,
            kept_wake: ui::wake::WakeHandle::new(),
            dirty: true,
        }
    }

    /// Construct the real root type from inert prepared inputs, so the
    /// binary's launch-composition tests can exercise the production widget
    /// without reading settings, registering MPRIS, opening a
    /// decoder/provider, or attaching collaboration. Not feature-gated: the
    /// binary's tests cannot switch on a library feature without a self
    /// dev-dependency, which would claim `--features video-player/thin`.
    #[doc(hidden)]
    pub fn history_test_root() -> Self {
        let preferences = PlaybackPreferences {
            autoplay: false,
            loop_enabled: true,
        };
        let mut player = VideoPlayer::new()
            .with_engine(media::MediaEngine::new_synthesized())
            .with_test_id("video_surface");
        configure_player_settings(&mut player, preferences.autoplay, preferences.loop_enabled);
        let current = LocalMediaEntry {
            id: "controlled-queue-current".into(),
            title: "Controlled history clip".into(),
            source: media::Source::Remote {
                url: "https://fixture.invalid/current.mp4".into(),
            },
        };
        let recent = VecDeque::from([
            LocalMediaEntry {
                id: "controlled-recent-autumn".into(),
                title: "Autumn reel".into(),
                source: media::Source::Remote {
                    url: "https://fixture.invalid/autumn.mp4".into(),
                },
            },
            LocalMediaEntry {
                id: "controlled-recent-city".into(),
                title: "City study".into(),
                source: media::Source::Remote {
                    url: "https://fixture.invalid/city.mp4".into(),
                },
            },
        ]);
        let mut app = Self::from_prepared(player, preferences, recent);
        app.queue = vec![
            current.clone(),
            LocalMediaEntry {
                id: "controlled-queue-next".into(),
                title: "Next controlled clip".into(),
                source: media::Source::Remote {
                    url: "https://fixture.invalid/next.mp4".into(),
                },
            },
        ];
        app.title = current.title;
        app.current_queue_id = Some(current.id);
        app.current_source_id = Some("source-7".into());
        app.source_generation = 7;
        app.library_visible = true;
        app.library_tab = LibraryTab::Recent;
        app.library_focus_id = Some("controlled-recent-city".into());
        app.shot_metadata = Some(DecoderMetadata {
            audio_tracks: vec![crate::actions::TrackState {
                stream_id: "audio-1".into(),
                index: 0,
                label: "Controlled stereo".into(),
                selected: true,
            }],
            ..DecoderMetadata::default()
        });
        app
    }

    #[cfg(test)]
    pub(crate) fn history_test_with_pending_portal() -> (
        Self,
        std::sync::mpsc::Sender<PortalOutcome>,
        std::rc::Rc<std::cell::RefCell<Option<String>>>,
    ) {
        let mut app = Self::history_test_root();
        let (sender, channel) = std::sync::mpsc::channel();
        let observed_error = std::rc::Rc::new(std::cell::RefCell::new(None));
        app.history_test_open_error_observer = Some(observed_error.clone());
        app.portal_generation = 11;
        app.portal_open = Some(PendingPortalOpen {
            channel: PickerChannel::Path(channel),
            generation: 11,
            invocation: None,
        });
        (app, sender, observed_error)
    }

    /// Open a filesystem `path` immediately (used by `--open PATH` and the
    /// portal outcome). Autoplays.
    #[must_use]
    pub fn with_open_path(mut self, path: &Path) -> Self {
        let _ = self.open_path(path);
        self
    }

    /// Open a broker-validated handoff source before the first frame.
    #[must_use]
    pub fn with_open_source(mut self, source: media::Source, title: String) -> Self {
        let _ = self.open_local_source(source, title, None);
        self
    }

    /// Land on the acknowledged source playhead and preserve whether the
    /// source was playing. This runs after open so local autoplay preferences
    /// cannot override the handoff transaction.
    #[must_use]
    pub fn with_handoff_transport(mut self, position_ms: u64, resume_playing: bool) -> Self {
        self.player.seek_to_ms(position_ms);
        if resume_playing {
            self.player.play();
        } else {
            self.player.pause();
        }
        self
    }

    /// Stage a capture: a plain player on the demo clip, or the named
    /// scenario on top of it. `scenario` is `None` for a plain `--shots`, one
    /// of [`CAPTURE_SCENARIOS`], or `open:<path>` to play a real file.
    ///
    /// Scenarios:
    ///   * `"empty"` and `"error"` leave nothing open; `"error"` shows a
    ///     failed open.
    ///   * `"loading"` holds a source whose first frame has not arrived.
    ///   * `"playing"` and `"paused"` are explicit transport states.
    ///   * `"settings"` opens the settings menu; `"metadata"` opens it with
    ///     embedded audio, subtitle and chapter tracks.
    ///   * `"fullscreen"` shows the immersive control bar.
    ///   * `"library"`, `"library-focus"`, `"recent"` and `"shared"` open the
    ///     library on a tab (its shared rows come from the resource).
    pub(crate) fn stage_capture(&mut self, scenario: Option<&str>) {
        // A capture is the same on every machine: default preferences and no
        // history of what this person watched.
        self.preferences = PlaybackPreferences::default();
        configure_player_settings(
            &mut self.player,
            self.preferences.autoplay,
            self.preferences.loop_enabled,
        );
        self.recent.clear();
        let open = scenario
            .and_then(|name| name.strip_prefix(OPEN_CAPTURE))
            .map(PathBuf::from);
        let scenario = scenario.filter(|name| !name.starts_with(OPEN_CAPTURE));
        self.stage_capture_source(open, scenario);
        self.stage_capture_scenario(scenario);
        if !matches!(scenario, Some("empty" | "error")) {
            self.player.reveal_controls();
        }
    }

    /// Build a player staged for a capture, without the framework host: the
    /// same staging a hosted capture runs. Opens `open` when given.
    pub fn for_shot(open: Option<PathBuf>, scenario: Option<&str>) -> Self {
        let mut app = Self::new();
        match open {
            Some(path) => app.stage_capture(Some(&format!("{OPEN_CAPTURE}{}", path.display()))),
            None => app.stage_capture(scenario),
        }
        app
    }

    fn stage_capture_source(&mut self, open: Option<PathBuf>, scenario: Option<&str>) {
        if matches!(scenario, Some("empty" | "error")) {
            return;
        }
        if let Some(path) = open {
            let _ = self.open_path(&path);
            // The first picture is the capture; keep the playhead on it.
            self.player.pause();
            return;
        }
        // A capture stays hermetic: the synthesized engine produces the
        // frames, so no decoder pipeline is built and the demo source is
        // never contacted, even though the real backend is compiled in.
        // `--shots --open <file>` is the real-decode path.
        self.player
            .set_engine(media::MediaEngine::new_synthesized());
        let source = demo_source();
        let queue_id = self.ensure_queued(source.clone(), DEMO_TITLE.into());
        let _ = self.set_current_source(&source);
        self.player.open(source);
        self.title = DEMO_TITLE.to_string();
        self.current_queue_id = Some(queue_id);
        // Parked at the start for a stable reference frame; the explicit
        // `playing` scenario starts the engine below.
        self.player.seek_to_ms(0);
        self.player.pause();
    }

    fn stage_capture_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("error") => {
                self.open_error = Some("The decoder could not read this video.".into());
            }
            Some("loading") => self.shot_loading = true,
            Some("playing") => self.player.play(),
            Some("paused") => self.player.pause(),
            Some("settings") => self.player.open_settings_menu(None),
            Some("metadata") => {
                self.shot_metadata = Some(DecoderMetadata {
                    audio_tracks: vec![
                        crate::actions::TrackState {
                            stream_id: "fixture-audio-en".into(),
                            index: 0,
                            label: "English stereo".into(),
                            selected: true,
                        },
                        crate::actions::TrackState {
                            stream_id: "fixture-audio-de".into(),
                            index: 1,
                            label: "Deutsch 5.1".into(),
                            selected: false,
                        },
                    ],
                    subtitle_tracks: vec![crate::actions::TrackState {
                        stream_id: "fixture-subtitles-en".into(),
                        index: 0,
                        label: "English captions".into(),
                        selected: true,
                    }],
                    chapters: vec![
                        crate::actions::ChapterState {
                            id: "intro".into(),
                            label: "Introduction".into(),
                            seconds: 0.0,
                        },
                        crate::actions::ChapterState {
                            id: "scene".into(),
                            label: "Second act".into(),
                            seconds: 45.0,
                        },
                    ],
                });
                self.sync_decoder_settings();
                self.player.open_settings_menu(None);
            }
            Some("library" | "recent" | "library-focus" | "shared") => {
                for (url, title) in DEMO_QUEUE {
                    self.ensure_queued(
                        media::Source::Remote { url: (*url).into() },
                        (*title).into(),
                    );
                }
                // The clip that is playing leads, then what came before it.
                self.recent = self
                    .queue
                    .iter()
                    .take(1)
                    .chain(self.queue.iter().skip(1).rev())
                    .cloned()
                    .chain(std::iter::once(LocalMediaEntry {
                        id: "recent-mountain-pass".into(),
                        title: "Mountain pass".into(),
                        source: media::Source::Remote {
                            url: "https://demo.local/mountain.mp4".into(),
                        },
                    }))
                    .collect();
                self.library_visible = true;
                self.library_tab = match scenario {
                    Some("recent") => LibraryTab::Recent,
                    Some("shared") => LibraryTab::Shared,
                    _ => LibraryTab::Queue,
                };
                if scenario == Some("library-focus") {
                    self.library_focus_id = self.queue.get(1).map(|entry| entry.id.clone());
                }
            }
            Some("fullscreen") => self.player.toggle_fullscreen(),
            _ => {}
        }
    }

    /// Attach collaborative playback: a live collab socket plus the host/
    /// follower role from the environment. Only the live run path calls this —
    /// `--shots` and tests stay local so no thread/socket is created.
    #[must_use]
    pub fn with_collab(mut self) -> Self {
        let role = Role::from_env();
        self.player.set_transport_enabled(role == Role::Host);
        if role == Role::Follower {
            // Repeat changes shared transport and therefore belongs to the
            // host. The resource tree retains an unavailable explanation.
            self.player.remove_setting("loop");
        }
        self.collab = Some(CollabPlayback::new(role));
        self.manager = Some(CollaborationManager::new());
        self
    }

    /// Mark this player as a framework-hosted surface so
    /// [`Widget::resources`] defers to `tilo_resource::publish`.
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_framework_hosted(&mut self, hosted: bool) {
        self.framework_hosted = hosted;
    }

    /// Whether [`PublishesResources`](ui::resources::PublishesResources) is
    /// publishing only live track extras beside the framework door.
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.framework_hosted
    }

    /// Test-only: drop staged decoder metadata so hosted extras go empty.
    #[cfg(test)]
    pub(crate) fn clear_shot_metadata_for_test(&mut self) {
        self.shot_metadata = Some(DecoderMetadata::default());
    }

    /// Test-only: seed a playlist that has not been published through the
    /// framework resource yet.
    #[cfg(test)]
    pub(crate) fn seed_unpublished_playlist_for_test(&mut self, id: &str, name: &str) {
        self.shared_library.playlists.push(crate::shared::Playlist {
            id: id.into(),
            name: name.into(),
        });
    }

    /// Test-only: shared-library playlist ids currently held on the surface.
    #[cfg(test)]
    pub(crate) fn shared_playlist_ids_for_test(&self) -> Vec<String> {
        self.shared_library
            .playlists
            .iter()
            .map(|playlist| playlist.id.clone())
            .collect()
    }

    /// Show the playlists and annotations the framework resource holds.
    /// An empty projection does not wipe a library that has not been published yet.
    pub(crate) fn adopt_shared_library(&mut self, state: &crate::shared::SharedLibrary) {
        let incoming_empty =
            state.playlists.is_empty() && state.items.is_empty() && state.annotations.is_empty();
        let local_has_rows = !self.shared_library.playlists.is_empty()
            || !self.shared_library.items.is_empty()
            || !self.shared_library.annotations.is_empty();
        if incoming_empty && local_has_rows {
            return;
        }
        if state != &self.shared_library {
            self.shared_library = state.clone();
            self.invalidate_search_cache();
            self.dirty = true;
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::VideoPlayer>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// Shared read access to the inner video surface (for the headless shot
    /// warm-up and integration tests that assert a decoded frame landed).
    pub fn player(&self) -> &VideoPlayer {
        &self.player
    }

    /// Whether a decoded picture is currently on the surface. The `--shots`
    /// path pumps `sync` until this is true so a real (asynchronously prerolled)
    /// decode is captured, not a blank frame.
    pub fn has_media_frame(&self) -> bool {
        self.player.latest_frame().is_some()
    }

    /// Whether a capture of an opened file can stop waiting: its first
    /// picture arrived or the decoder reported why it never will.
    pub(crate) fn capture_settled(&self) -> bool {
        self.has_media_frame() || self.open_error.is_some()
    }

    /// Whether the engine decodes the file with the real backend rather than
    /// synthesizing frames.
    pub(crate) fn decodes_for_real(&self) -> bool {
        self.player
            .engine()
            .is_some_and(media::MediaEngine::is_real_decode)
    }

    /// Drop collaborative playback: a capture is local, and opens no socket.
    pub(crate) fn detach_collab(&mut self) {
        self.collab = None;
        self.manager = None;
        self.player.set_transport_enabled(true);
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        let source = source_for_path(path);
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("selected video")
            .to_string();
        let queue_id = self.ensure_queued(source.clone(), title.clone());
        self.open_local_source(source, title, Some(queue_id))
    }

    /// Open a video the host handed over as bytes (a browser page has no
    /// paths to give).
    fn open_contents(&mut self, file: &PickedContents) -> Result<(), String> {
        let source = media::Source::from_contents(&file.name, &file.bytes)
            .map_err(|e| format!("could not stage {}: {e}", file.name))?;
        let title = if file.name.is_empty() {
            "selected video".to_string()
        } else {
            file.name.clone()
        };
        let queue_id = self.ensure_queued(source.clone(), title.clone());
        self.open_local_source(source, title, Some(queue_id.clone()))?;
        if !crate::kept::native_paths() {
            self.keep_contents(&queue_id, file);
        }
        Ok(())
    }

    /// Keep a video opened by contents for Recent, or say why it is not.
    fn keep_contents(&mut self, queue_id: &str, file: &PickedContents) {
        if file.bytes.len() > crate::kept::KEEP_LIMIT {
            if let Some(entry) = self.queue.iter_mut().find(|entry| entry.id == queue_id) {
                entry.title = format!(
                    "{} — too large to reopen after a reload (over {} MB)",
                    entry.title,
                    crate::kept::KEEP_LIMIT / (1024 * 1024)
                );
            }
            self.invalidate_search_cache();
            return;
        }
        let token = crate::kept::token(&file.name, file.bytes.len());
        self.kept_retained.insert(0, token.clone());
        self.kept_retained.truncate(crate::kept::KEPT_VIDEOS);
        self.kept.insert(queue_id.to_owned(), token.clone());
        drop(crate::kept::keep(
            token,
            file.bytes.clone(),
            self.kept_retained.clone(),
        ));
    }

    /// Start reading a kept copy back; the entry opens once it is here.
    fn recall_kept(&mut self, token: &str, title: String, queue_id: Option<String>) {
        let wake = self.kept_wake.clone();
        self.kept_recall = Some(KeptRecall {
            bytes: crate::kept::recall(token.to_owned(), move || wake.wake()),
            token: token.to_owned(),
            title,
            queue_id,
        });
        self.open_error = None;
        self.dirty = true;
    }

    fn poll_kept_recall(&mut self) {
        let Some(recall) = self.kept_recall.as_ref() else {
            return;
        };
        let bytes = match recall.bytes.try_recv() {
            Ok(bytes) => bytes,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        let Some(recall) = self.kept_recall.take() else {
            return;
        };
        let opened = bytes
            .ok_or_else(|| {
                format!(
                    "the kept copy of {} is gone; open the file again",
                    recall.title
                )
            })
            .and_then(|bytes| {
                media::Source::from_contents(&recall.title, &bytes)
                    .map_err(|e| format!("could not stage {}: {e}", recall.title))
            });
        match opened {
            Ok(source) => {
                if let Some(queue_id) = &recall.queue_id {
                    self.kept.insert(queue_id.clone(), recall.token);
                }
                let _ = self.open_local_source(source, recall.title, recall.queue_id);
            }
            Err(error) => {
                self.open_error = Some(error);
                self.dirty = true;
            }
        }
    }

    fn ensure_queued(&mut self, source: media::Source, title: String) -> String {
        if let Some(entry) = self.queue.iter().find(|entry| {
            media::session::session_key(&entry.source) == media::session::session_key(&source)
        }) {
            return entry.id.clone();
        }
        let id = format!("local-queue-{}", local_source_token(&source, 0));
        self.queue.push(LocalMediaEntry {
            id: id.clone(),
            title,
            source,
        });
        self.invalidate_search_cache();
        id
    }

    fn open_local_entry(&mut self, item_id: &str) -> Result<(), String> {
        let entry = self
            .queue
            .iter()
            .chain(self.recent.iter())
            .find(|entry| entry.id == item_id)
            .cloned()
            .ok_or_else(|| "that local queue item is no longer available".to_string())?;
        self.open_local_source(entry.source, entry.title, Some(entry.id))
    }

    fn open_local_source(
        &mut self,
        source: media::Source,
        title: String,
        queue_id: Option<String>,
    ) -> Result<(), String> {
        if let Some(token) = crate::kept::token_of(&source) {
            let token = token.to_owned();
            self.recall_kept(&token, title, queue_id);
            return Ok(());
        }
        if let Err(error) = self.set_current_source(&source) {
            self.open_error = Some(error.clone());
            self.dirty = true;
            return Err(error);
        }
        self.cancel_pending_portal("a direct video open replaced the file picker");
        if let Some(pending) = self.media_open.take() {
            self.queue_resource_result(tilo_actions::InvocationUpdate::Cancelled {
                invocation: pending.invocation,
                message: Some("a newer video open replaced this decoder request".into()),
            });
        }
        self.open_error = None;
        self.invalidate_search_cache();
        self.title = title;
        self.current_queue_id = queue_id;
        self.player.open(source);
        if self.preferences.autoplay {
            self.player.play();
        } else {
            self.player.pause();
        }
        self.player.reveal_controls();
        self.player
            .set_now_playing(ui::widget::video_player::NowPlaying {
                // MPRIS/session metadata must not disclose a portal-granted
                // absolute path. The title above is just the user-facing file
                // name; this stable opaque ID is the machine identity.
                track_id: self.current_source_id.clone().unwrap_or_default(),
                title: self.title.clone(),
                artist: String::new(),
                art_url: String::new(),
            });
        self.dirty = true;
        Ok(())
    }

    fn record_current_history(&mut self) {
        let Some(queue_id) = self.current_queue_id.as_deref() else {
            return;
        };
        let Some(entry) = self
            .queue
            .iter()
            .find(|entry| entry.id == queue_id)
            .cloned()
        else {
            return;
        };
        self.recent.retain(|item| item.id != entry.id);
        self.recent.push_front(entry);
        self.recent
            .truncate(crate::preferences::RECENT_HISTORY_LIMIT);
        if let Err(error) = self.persist_recent_history() {
            tracing::warn!(%error, "video player: could not persist recent history");
        }
        self.invalidate_search_cache();
        self.dirty = true;
    }

    fn persist_recent_history(&self) -> std::io::Result<()> {
        // A page's `blob:` source dies with the page: an entry is kept by its
        // kept copy, while that copy is retained, or not at all.
        let entries: Vec<RecentMedia> = self
            .recent
            .iter()
            .filter_map(|entry| {
                let source = match self.kept.get(&entry.id) {
                    Some(token) => crate::kept::source(token),
                    None => entry.source.clone(),
                };
                let retained = crate::kept::token_of(&source)
                    .is_none_or(|token| self.kept_retained.iter().any(|kept| kept == token));
                (retained && !crate::kept::page_local(&source)).then(|| RecentMedia {
                    id: entry.id.clone(),
                    title: entry.title.clone(),
                    source,
                })
            })
            .collect();
        crate::preferences::save_recent_history(&entries)
    }

    fn clear_recent_history(&mut self) -> Result<(), String> {
        let previous = self.recent.clone();
        self.recent.clear();
        if let Err(error) = self.persist_recent_history() {
            self.recent = previous;
            return Err(format!("could not clear local recent history: {error}"));
        }
        if !self.kept_retained.is_empty() {
            self.kept.clear();
            self.kept_retained.clear();
            crate::kept::forget_all();
        }
        self.invalidate_search_cache();
        self.dirty = true;
        Ok(())
    }

    fn remove_queue_entry(&mut self, item_id: &str) -> Result<(), String> {
        let Some(index) = self.queue.iter().position(|entry| entry.id == item_id) else {
            return Err("that local queue item is no longer available".into());
        };
        self.queue.remove(index);
        if self.current_queue_id.as_deref() == Some(item_id) {
            self.current_queue_id = None;
        }
        self.invalidate_search_cache();
        self.dirty = true;
        Ok(())
    }

    fn advance_after_engine_end(&mut self) {
        if self.preferences.loop_enabled {
            return;
        }
        let Some(current) = self.current_queue_id.as_deref() else {
            return;
        };
        let Some(index) = self.queue.iter().position(|entry| entry.id == current) else {
            return;
        };
        let Some(next) = self.queue.get(index.saturating_add(1)) else {
            return;
        };
        let next_id = next.id.clone();
        if let Err(error) = self.open_local_entry(&next_id) {
            self.open_error = Some(error);
            self.dirty = true;
        }
    }

    /// Assign the next never-reused source identity. Once the u64 namespace is
    /// exhausted the app refuses a new source rather than making an action
    /// address from the start of the process current again.
    fn set_current_source(&mut self, source: &media::Source) -> Result<(), String> {
        let generation = self.source_generation.checked_add(1).ok_or_else(|| {
            "video source identity generation exhausted; restart Video Player".to_string()
        })?;
        self.source_generation = generation;
        self.current_source_id = Some(local_source_token(source, generation));
        self.current_media_ref = Some(local_source_token(source, 0));
        Ok(())
    }

    /// Invalidate the bounded search catalog before advancing its revision.
    /// Resetting the revision at the numeric boundary is safe only because the
    /// old provider is explicitly discarded first, so its `0` can never be
    /// mistaken for the fresh post-boundary catalog's `0`.
    fn invalidate_search_cache(&mut self) {
        self.search_cache.get_mut().take();
        self.metadata_revision = self.metadata_revision.checked_add(1).unwrap_or_default();
    }

    fn close_media(&mut self) {
        if let Some(pending) = self.media_open.take() {
            self.queue_resource_result(tilo_actions::InvocationUpdate::Cancelled {
                invocation: pending.invocation,
                message: Some("video was closed before decoding a first frame".into()),
            });
        }
        let mut player =
            configured_player(self.preferences.autoplay, self.preferences.loop_enabled);
        player.set_transport_enabled(
            self.collab
                .as_ref()
                .is_none_or(|collab| collab.role() == Role::Host),
        );
        if self
            .collab
            .as_ref()
            .is_some_and(|collab| collab.role() == Role::Follower)
        {
            player.remove_setting("loop");
        }
        self.player = player;
        self.title = "No media".into();
        self.current_source_id = None;
        self.current_media_ref = None;
        self.current_queue_id = None;
        self.invalidate_search_cache();
        self.dirty = true;
    }

    fn request_picture_in_picture(&mut self) {
        if !self.player.has_media() || self.player.get_rect().width() < 1.0 {
            return;
        }
        let region = self.player.get_rect();
        let aspect = (region.width() / region.height().max(1.0)).clamp(0.4, 2.8);
        self.pending_pip = Some(ui::floating_surface::FloatingSurfaceRequest {
            id: "video-player.current-video".into(),
            title: self.title.clone(),
            role: ui::floating_surface::FloatingRole::PictureInPicture,
            source: ui::floating_surface::FloatingSource::AppElement {
                app_id: "so.tilo.video-player".into(),
                element_id: "video_surface".into(),
                region,
            },
            preferred_size: vello::kurbo::Size::new(360.0, 360.0 / aspect),
            aspect_ratio: Some(aspect),
            initial_rect: None,
            presentation_rect: None,
            tracking_source: None,
        });
        self.dirty = true;
    }

    fn request_open_action(&mut self) {
        if let Some(action) = &mut self.open_action {
            let _ = action.emit(());
        }
    }

    fn request_library_play(&mut self, item_id: &str) {
        let resource_id = if self.queue.iter().any(|entry| entry.id == item_id) {
            crate::actions::queue_item_resource_id(item_id)
        } else {
            crate::actions::recent_item_resource_id(item_id)
        };
        self.request_library_action(&resource_id, "play");
    }

    fn request_library_remove(&mut self, item_id: &str) {
        self.request_library_action(&crate::actions::queue_item_resource_id(item_id), "remove");
    }

    fn request_library_clear(&mut self) {
        self.request_library_action("video-player.history.local", "clear");
    }

    fn request_library_hide(&mut self) {
        self.request_library_action("video-player.application", "hide-library");
    }

    fn request_queue_library(&mut self) {
        self.request_library_action("video-player.queue.local", "show");
    }

    fn request_recent_library(&mut self) {
        self.request_library_action("video-player.history.local", "show");
    }

    fn request_shared_library(&mut self) {
        self.request_library_action("video-player.playlists.shared", "show");
    }

    /// The Library button and `L` shortcut choose a concrete, replay-safe
    /// action from the current presentation. We never publish a toggle: a
    /// replay or delayed agent invocation must not invert a newer user choice.
    fn request_library_visibility_change(&mut self) {
        if self.library_visible {
            self.request_library_hide();
        } else {
            match self.library_tab {
                LibraryTab::Queue => self.request_queue_library(),
                LibraryTab::Recent => self.request_recent_library(),
                LibraryTab::Shared => self.request_shared_library(),
            }
        }
    }

    /// Route marker navigation through the same canonical resource action used
    /// by global search and agents. Shared marker resources live in the
    /// document, so this resolves them against the active document rather than
    /// manufacturing a local playback closure.
    fn request_annotation_jump(&mut self, doc: &AutoCommit, annotation_id: &str) {
        let (audio, subtitles, chapters) = self.decoder_metadata();
        let context = self.action_context(&audio, &subtitles, &chapters);
        let resource_id = crate::actions::annotation_resource_id(annotation_id);
        let Some(resource) = crate::actions::resource(&context, doc, &resource_id) else {
            return;
        };
        let Ok(binding) = tilo_actions::ActionBinding::unit_for(&resource, "jump") else {
            return;
        };
        if let Ok(invocation) = binding.invoke(()) {
            self.library_control_invocations.push_back(invocation);
        }
    }

    fn dispatch_shared(&mut self, doc: &mut AutoCommit, action: &crate::shared::SharedAction) {
        let context = crate::shared::history_context(action);
        self.actions
            .dispatch_with_context(doc, crate::shared::ACTION_TYPE, action, context);
        self.shared_library = crate::shared::read(doc);
        self.invalidate_search_cache();
        self.dirty = true;
    }

    fn add_current_to_watch_later(&mut self, doc: &mut AutoCommit) {
        let Some(media_ref) = self.current_media_ref.clone() else {
            return;
        };
        let playlist_id = "watch-later".to_string();
        if !self
            .shared_library
            .playlists
            .iter()
            .any(|playlist| playlist.id == playlist_id)
        {
            self.dispatch_shared(
                doc,
                &crate::shared::SharedAction::CreatePlaylist {
                    id: playlist_id.clone(),
                    name: "Watch later".into(),
                },
            );
        }
        if self
            .shared_library
            .items
            .iter()
            .any(|item| item.playlist_id == playlist_id && item.media_ref == media_ref)
        {
            return;
        }
        let position = self
            .shared_library
            .items
            .iter()
            .filter(|item| item.playlist_id == playlist_id)
            .map(|item| item.position)
            .max()
            .unwrap_or(0)
            .saturating_add(1024);
        let id = format!(
            "item-{}-{}",
            crate::actions::identity_token_for_action(&media_ref),
            unix_now_ms()
        );
        self.dispatch_shared(
            doc,
            &crate::shared::SharedAction::AddPlaylistItem {
                id,
                playlist_id,
                media_ref,
                title: self.title.clone(),
                position,
            },
        );
    }

    fn bookmark_current_time(&mut self, doc: &mut AutoCommit) {
        let Some(media_ref) = self.current_media_ref.clone() else {
            return;
        };
        let time_ms = self.player.position_ms();
        let id = format!(
            "annotation-{}-{}-{time_ms}",
            crate::actions::identity_token_for_action(&media_ref),
            unix_now_ms()
        );
        self.dispatch_shared(
            doc,
            &crate::shared::SharedAction::CreateAnnotation {
                id,
                media_ref,
                time_ms,
                text: String::new(),
            },
        );
    }

    fn request_library_action(&mut self, resource_id: &str, action_id: &str) {
        let (audio, subtitles, chapters) = self.decoder_metadata();
        let resources = crate::actions::resources(
            &self.action_context(&audio, &subtitles, &chapters),
            &AutoCommit::new(),
        );
        let Some(resource) = find_resource(&resources, resource_id) else {
            return;
        };
        let Ok(binding) = tilo_actions::ActionBinding::unit_for(resource, action_id) else {
            return;
        };
        if let Ok(invocation) = binding.invoke(()) {
            self.library_control_invocations.push_back(invocation);
        }
    }

    /// Kick off a portal file-open (or the immediate "no portal" outcome in the
    /// hermetic build). A second request is rejected instead of replacing the
    /// receiver for the first request, which would make the result race-prone.
    fn trigger_open(&mut self) -> Result<(), String> {
        if self.portal_open.is_some() {
            return Err("a video picker is already open".into());
        }
        // Do not accept a new background operation while a prior accepted
        // operation still has a completion for the host. Keeping the result is
        // more important than accepting unbounded work: dropping it would
        // strand that invocation forever.
        if !self.resource_results.is_empty() {
            return Err(
                "a previous video-picker completion is awaiting host acknowledgement".into(),
            );
        }
        let generation = self.portal_generation.checked_add(1).ok_or_else(|| {
            "video-picker request generation exhausted; restart Video Player".to_string()
        })?;
        self.portal_generation = generation;
        self.open_error = None;
        self.portal_open = Some(PendingPortalOpen {
            channel: if tilo_paths::availability(tilo_paths::Feature::NativePaths)
                == tilo_paths::Availability::Available
            {
                PickerChannel::Path(video_open_request().send())
            } else {
                PickerChannel::Contents(video_open_request().send_contents())
            },
            generation,
            invocation: None,
        });
        self.dirty = true;
        Ok(())
    }

    /// Drop a portal receiver that no longer represents the user's active
    /// intent. The portal may still complete in the desktop service, but its
    /// response has no receiver and therefore cannot mutate this player.
    fn cancel_pending_portal(&mut self, message: &str) {
        let Some(pending) = self.portal_open.take() else {
            return;
        };
        if let Some(invocation) = pending.invocation {
            self.queue_resource_result(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(message.into()),
            });
        }
    }

    fn await_open_frame(&mut self, invocation: tilo_actions::InvocationId) -> Result<(), String> {
        if self.media_open.is_some() {
            return Err("another video open is still waiting for decoder output".into());
        }
        let source_id = self
            .current_source_id
            .clone()
            .ok_or_else(|| "video source did not receive an identity".to_string())?;
        self.media_open = Some(PendingMediaOpen {
            invocation,
            source_id,
            deadline: ui::time::now() + OPEN_FRAME_TIMEOUT,
        });
        Ok(())
    }

    fn poll_open_frame(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let pending = self.media_open.as_ref()?;
        if pending.invocation != invocation {
            return None;
        }
        if self.current_source_id.as_deref() != Some(pending.source_id.as_str()) {
            let pending = self.media_open.take()?;
            return Some(tilo_actions::InvocationUpdate::Failed {
                invocation: pending.invocation,
                error: tilo_actions::ActionFailure {
                    code: "video_player.open_superseded".into(),
                    message: "The requested video was superseded before decoding a frame.".into(),
                    details: None,
                    retryable: true,
                },
            });
        }
        if self.player.latest_frame().is_some() {
            let pending = self.media_open.take()?;
            self.record_current_history();
            return Some(tilo_actions::InvocationUpdate::Succeeded {
                invocation: pending.invocation,
                output: tilo_actions::ActionOutput::default(),
            });
        }
        if ui::time::now() >= pending.deadline {
            let pending = self.media_open.take()?;
            let message = "The decoder did not produce a first frame in time.".to_string();
            self.open_error = Some(message.clone());
            self.dirty = true;
            return Some(tilo_actions::InvocationUpdate::Failed {
                invocation: pending.invocation,
                error: tilo_actions::ActionFailure {
                    code: "video_player.open_timed_out".into(),
                    message,
                    details: Some(serde_json::json!({
                        "timeout_seconds": OPEN_FRAME_TIMEOUT.as_secs(),
                    })),
                    retryable: true,
                },
            });
        }
        None
    }

    /// Queue an action completion without letting a newer action overwrite an
    /// older invocation that the host has not polled yet. `trigger_open`
    /// applies backpressure before accepting another asynchronous request, so
    /// every accepted invocation keeps its result until it is collected.
    fn queue_resource_result(&mut self, update: tilo_actions::InvocationUpdate) {
        self.resource_results.push_back(update);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all operation variants remain together so their local semantics cannot drift"
    )]
    fn perform_resource_operation(&mut self, operation: crate::actions::PlayerOperation) {
        use crate::actions::PlayerOperation;
        match operation {
            PlayerOperation::Open { path: None } => {
                // `answer_resource_operation` starts portal opens so it can
                // report a refusal to agents; this branch is retained for the
                // in-app path and intentionally ignores an already-reported
                // failure rather than panicking.
                let _ = self.trigger_open();
            }
            PlayerOperation::Open { path: Some(path) } => {
                if let Err(error) = self.open_path(Path::new(&path)) {
                    tracing::warn!(%error, "video player: could not open direct path");
                }
            }
            PlayerOperation::OpenQueueItem { item_id } => {
                if let Err(error) = self.open_local_entry(&item_id) {
                    tracing::warn!(%error, "video player: could not open queued item");
                }
            }
            PlayerOperation::RemoveQueueItem { item_id } => {
                if let Err(error) = self.remove_queue_entry(&item_id) {
                    tracing::warn!(%error, "video player: could not remove queued item");
                }
            }
            PlayerOperation::ClearRecentHistory => {
                if let Err(error) = self.clear_recent_history() {
                    tracing::warn!(%error, "video player: could not clear recent history");
                }
            }
            PlayerOperation::RevealLibrary => {
                self.library_visible = true;
                self.library_scroll = 0.0;
                self.library_tab = LibraryTab::Queue;
                self.player.reveal_controls();
            }
            PlayerOperation::RevealRecentLibrary => {
                self.library_visible = true;
                self.library_scroll = 0.0;
                self.library_tab = LibraryTab::Recent;
                self.player.reveal_controls();
            }
            PlayerOperation::RevealSharedLibrary => {
                self.library_visible = true;
                self.library_scroll = 0.0;
                self.library_tab = LibraryTab::Shared;
                self.player.reveal_controls();
            }
            PlayerOperation::HideLibrary => {
                self.library_visible = false;
                self.library_focus_id = None;
            }
            PlayerOperation::Close => self.close_media(),
            PlayerOperation::TogglePlayback => self.player.toggle_play(),
            PlayerOperation::ToggleMute => self.player.toggle_mute(),
            PlayerOperation::Reveal => self.player.reveal_controls(),
            PlayerOperation::SeekSeconds(seconds)
            | PlayerOperation::JumpToChapter { seconds, .. } => {
                self.player.seek_to_ms((seconds.max(0.0) * 1000.0) as u64);
            }
            PlayerOperation::JumpToAnnotation { time_ms, .. } => {
                self.player.seek_to_ms(time_ms);
                self.player.reveal_controls();
            }
            PlayerOperation::SeekBySeconds(seconds) => {
                let current = self.player.position_ms() as f64;
                let duration = self.player.duration_ms() as f64;
                let target =
                    (current + seconds.clamp(-3600.0, 3600.0) * 1000.0).clamp(0.0, duration) as u64;
                self.player.seek_to_ms(target);
            }
            PlayerOperation::Restart => self.player.seek_to_ms(0),
            PlayerOperation::SeekToEnd => self.player.seek_to_ms(self.player.duration_ms()),
            PlayerOperation::SetVolume(volume) => self.player.set_volume(volume as f32),
            PlayerOperation::SetAutoplay(enabled) => {
                if self.preferences.autoplay != enabled {
                    self.preferences.autoplay = enabled;
                    configure_player_settings(
                        &mut self.player,
                        self.preferences.autoplay,
                        self.preferences.loop_enabled,
                    );
                    self.persist_preferences();
                }
            }
            PlayerOperation::SetLoop(enabled) => {
                if self.preferences.loop_enabled != enabled {
                    self.preferences.loop_enabled = enabled;
                    configure_player_settings(
                        &mut self.player,
                        self.preferences.autoplay,
                        self.preferences.loop_enabled,
                    );
                    self.persist_preferences();
                }
            }
            PlayerOperation::SetPlaybackRate(rate) => {
                if let Some(engine) = self.player.engine_mut() {
                    engine.set_rate(rate as f32);
                }
                let selected = PLAYBACK_RATES
                    .iter()
                    .position(|candidate| (f64::from(*candidate) - rate).abs() < f64::EPSILON);
                if let Some(selected) = selected {
                    self.player.set_choice_selection("playback-rate", selected);
                }
            }
            PlayerOperation::OpenPictureInPicture => self.request_picture_in_picture(),
            PlayerOperation::SelectAudioTrack { index, .. } => {
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_audio_track(index))
                {
                    tracing::warn!(index, "video player: refused stale audio-track selection");
                }
            }
            PlayerOperation::SelectSubtitleTrack { target_index, .. } => {
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_subtitle_track(target_index))
                {
                    tracing::warn!(
                        ?target_index,
                        "video player: refused stale subtitle-track selection"
                    );
                }
            }
            PlayerOperation::ToggleFullscreen => self.player.toggle_fullscreen(),
        }
        self.dirty = true;
    }

    fn validate_dynamic_target(&self, operation: &PlayerOperation) -> Result<(), String> {
        let (audio, subtitles, chapters) = self.decoder_metadata();
        match operation {
            PlayerOperation::OpenQueueItem { item_id } => {
                if !self.queue.iter().any(|entry| entry.id == *item_id)
                    && !self.recent.iter().any(|entry| entry.id == *item_id)
                {
                    return Err("that local queue item changed; refresh and try again".into());
                }
            }
            PlayerOperation::RemoveQueueItem { item_id } => {
                if !self.queue.iter().any(|entry| entry.id == *item_id) {
                    return Err("that local queue item changed; refresh and try again".into());
                }
            }
            PlayerOperation::SelectAudioTrack {
                source_id,
                stream_id,
                index,
            } => {
                if self.current_source_id.as_deref() != Some(source_id)
                    || !audio
                        .iter()
                        .any(|track| track.index == *index && track.stream_id == *stream_id)
                {
                    return Err("the audio track changed; refresh search and try again".into());
                }
            }
            PlayerOperation::SelectSubtitleTrack {
                source_id,
                target_stream_id,
                target_index,
                expected_stream_id,
                expected_index,
            } => {
                if target_index.is_some() != target_stream_id.is_some()
                    || expected_index.is_some() != expected_stream_id.is_some()
                {
                    return Err(
                        "subtitle target was malformed; refresh search and try again".into(),
                    );
                }
                let desired = target_index.zip(target_stream_id.as_deref());
                let expected = expected_index.zip(expected_stream_id.as_deref());
                let actual = subtitles
                    .iter()
                    .find(|track| track.selected)
                    .map(|track| (track.index, track.stream_id.as_str()));
                if self.current_source_id.as_deref() != Some(source_id)
                    || desired.is_some_and(|(index, stream_id)| {
                        !subtitles
                            .iter()
                            .any(|track| track.index == index && track.stream_id == stream_id)
                    })
                    || actual != expected
                {
                    return Err("the subtitle track changed; refresh search and try again".into());
                }
            }
            PlayerOperation::JumpToChapter {
                source_id,
                id,
                seconds,
            } if self.current_source_id.as_deref() != Some(source_id)
                || !chapters.iter().any(|chapter| {
                    chapter.id == *id && (chapter.seconds - *seconds).abs() < f64::EPSILON
                }) =>
            {
                return Err("the chapter changed; refresh search and try again".into());
            }
            PlayerOperation::JumpToAnnotation { media_ref, .. }
                if self.current_media_ref.as_deref() != Some(media_ref) =>
            {
                return Err(
                    "this review marker belongs to a different video on this device".into(),
                );
            }
            _ => {}
        }
        Ok(())
    }

    /// Drain a pending portal outcome, opening the chosen file.
    #[allow(
        clippy::too_many_lines,
        reason = "each answer a picker can give, handled in one place"
    )]
    fn drain_portal(&mut self) {
        let pending = self.portal_open.take();
        let mut outcome: Option<PortalOpenOutcome> = None;
        if let Some(request) = pending.as_ref() {
            let received = match &request.channel {
                PickerChannel::Path(channel) => channel.try_recv().map(|answer| match answer {
                    PortalOutcome::Opened(p) => Ok(Picked::Path(p)),
                    PortalOutcome::Cancelled => Err(None),
                    PortalOutcome::Failed(error) => Err(Some(error)),
                }),
                PickerChannel::Contents(channel) => channel.try_recv().map(|answer| match answer {
                    ContentsOutcome::Picked(files) => files
                        .into_iter()
                        .next()
                        .map(Picked::Contents)
                        .ok_or_else(|| Some("the picker returned no file".into())),
                    ContentsOutcome::Cancelled => Err(None),
                    ContentsOutcome::Failed(error) => Err(Some(error)),
                }),
            };
            match received {
                Ok(result) => {
                    outcome = Some((request.generation, request.invocation, result));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    outcome = Some((
                        request.generation,
                        request.invocation,
                        Err(Some("video picker disconnected".into())),
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if outcome.is_none() {
            self.portal_open = pending;
            return;
        }
        // The receiver is intentionally dropped before opening the selected
        // path. `open_path` then cannot cancel the just-completed invocation.
        if let Some((generation, invocation, outcome)) = outcome {
            if generation != self.portal_generation {
                tracing::warn!(
                    generation,
                    current_generation = self.portal_generation,
                    "video player: ignored a stale portal outcome"
                );
                if let Some(invocation) = invocation {
                    self.queue_resource_result(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "video_player.stale_open_response".into(),
                            message: "ignored a stale video picker response; open again".into(),
                            details: None,
                            retryable: true,
                        },
                    });
                }
                return;
            }
            let result = match outcome {
                Ok(Picked::Path(path)) => self.open_path(&path).map(|()| true),
                Ok(Picked::Contents(file)) => self.open_contents(&file).map(|()| true),
                Err(None) => Ok(false),
                Err(Some(e)) => {
                    self.open_error = Some(e.clone());
                    self.dirty = true;
                    Err(e)
                }
            };
            if let Some(invocation) = invocation {
                match result {
                    Ok(true) => {
                        // Picker acceptance is not decoder success. Retain the
                        // invocation until `poll_open_frame` observes a first
                        // decoded picture (or a truthful timeout/failure).
                        if let Err(message) = self.await_open_frame(invocation) {
                            self.queue_resource_result(tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "video_player.open_lifecycle_failed".into(),
                                    message,
                                    details: None,
                                    retryable: true,
                                },
                            });
                        }
                    }
                    Ok(false) => {
                        self.queue_resource_result(tilo_actions::InvocationUpdate::Cancelled {
                            invocation,
                            message: Some("video selection was cancelled".into()),
                        });
                    }
                    Err(message) => {
                        self.queue_resource_result(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "video_player.open_failed".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        });
                    }
                }
            }
        }
    }

    /// Pump the collab transport: apply inbound host state to a follower, or
    /// broadcast the host's changing playback.
    fn tick_collab(&mut self) {
        let (Some(mgr), Some(collab)) = (self.manager.as_mut(), self.collab.as_mut()) else {
            return;
        };
        for ev in mgr.poll() {
            if let CollabEvent::Document { .. } = &ev {
                match ev.document_mutation() {
                    Some(Ok(collab_types::DocumentMutation::Incremental(data))) => {
                        collab.ingest(&data);
                    }
                    Some(Ok(collab_types::DocumentMutation::Snapshot(data))) => {
                        collab.ingest_snapshot(&data);
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "video collab: invalid document mutation");
                    }
                    None => {}
                }
            }
        }
        let now = unix_now_ms();
        match collab.role() {
            Role::Host => {
                // The manager connects in the background; the host's state
                // is stepped once it can reach followers, so the first delta
                // after connecting carries everything since.
                if mgr.is_connected()
                    && let Some(engine) = self.player.engine()
                    && let Some(delta) = collab.host_step(engine, now)
                {
                    mgr.send(CollabRequest::document_incremental(delta));
                }
            }
            Role::Follower => {
                if let Some(engine) = self.player.engine_mut() {
                    collab.follow_step(engine, now);
                }
            }
        }
    }

    fn drain_player_settings(&mut self) {
        for event in self.player.take_setting_events() {
            match event {
                PlayerSettingEvent::Toggled { id, on } if id == "autoplay" => {
                    self.perform_resource_operation(PlayerOperation::SetAutoplay(on));
                }
                PlayerSettingEvent::Toggled { id, on } if id == "loop" => {
                    if self
                        .collab
                        .as_ref()
                        .is_none_or(|collab| collab.role() == Role::Host)
                    {
                        self.perform_resource_operation(PlayerOperation::SetLoop(on));
                    }
                }
                PlayerSettingEvent::Chose { id, index } if id == "playback-rate" => {
                    if self
                        .collab
                        .as_ref()
                        .is_none_or(|collab| collab.role() == Role::Host)
                        && let Some(rate) = PLAYBACK_RATES.get(index)
                    {
                        self.perform_resource_operation(PlayerOperation::SetPlaybackRate(
                            f64::from(*rate),
                        ));
                    }
                }
                PlayerSettingEvent::Chose { id, index } if id == "audio-track" => {
                    let (audio_tracks, _, _) = self.decoder_metadata();
                    if let Some(track) = audio_tracks.get(index)
                        && let Some(source_id) = self.current_source_id.clone()
                    {
                        self.perform_resource_operation(PlayerOperation::SelectAudioTrack {
                            source_id,
                            stream_id: track.stream_id.clone(),
                            index: track.index,
                        });
                    }
                }
                PlayerSettingEvent::Chose { id, index } if id == "subtitle-track" => {
                    let (_, subtitle_tracks, _) = self.decoder_metadata();
                    let target = index
                        .checked_sub(1)
                        .and_then(|index| subtitle_tracks.get(index));
                    if let Some(source_id) = self.current_source_id.clone() {
                        let selected = subtitle_tracks.iter().find(|track| track.selected);
                        self.perform_resource_operation(PlayerOperation::SelectSubtitleTrack {
                            source_id,
                            target_stream_id: target.map(|track| track.stream_id.clone()),
                            target_index: target.map(|track| track.index),
                            expected_stream_id: selected.map(|track| track.stream_id.clone()),
                            expected_index: selected.map(|track| track.index),
                        });
                    }
                }
                PlayerSettingEvent::Chose { id, index } if id == "chapters" => {
                    let (_, _, chapters) = self.decoder_metadata();
                    if let Some(chapter) = chapters.get(index) {
                        self.perform_resource_operation(PlayerOperation::JumpToChapter {
                            source_id: self.current_source_id.clone().unwrap_or_default(),
                            id: chapter.id.clone(),
                            seconds: chapter.seconds,
                        });
                    }
                }
                PlayerSettingEvent::Activated { id } if id == "picture-in-picture" => {
                    self.perform_resource_operation(PlayerOperation::OpenPictureInPicture);
                }
                PlayerSettingEvent::Toggled { .. }
                | PlayerSettingEvent::Chose { .. }
                | PlayerSettingEvent::Activated { .. } => {}
            }
        }
    }

    /// Translate decoder-owned lifecycle events into app-visible state. The
    /// shared widget remains the single engine pump; this layer owns only the
    /// file-open contract and the user-facing error copy.
    fn drain_player_media_events(&mut self) {
        for event in self.player.take_media_events() {
            match event {
                media::MediaEvent::Error(message) => {
                    // The decoder's own wording is for logs and agents; the
                    // person is told what it means for them.
                    tracing::warn!(error = %message, "video player: the decoder reported an error");
                    self.open_error = Some("The decoder could not play this video.".into());
                    self.dirty = true;
                    if let Some(pending) = self.media_open.take() {
                        self.queue_resource_result(tilo_actions::InvocationUpdate::Failed {
                            invocation: pending.invocation,
                            error: tilo_actions::ActionFailure {
                                code: "video_player.decoder_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                }
                media::MediaEvent::Ended => {
                    // Queue advancement belongs to the app, but remains gated
                    // on a real engine end event rather than the current
                    // playhead looking close to its duration.
                    self.advance_after_engine_end();
                }
                media::MediaEvent::Buffering { .. }
                | media::MediaEvent::Position { .. }
                | media::MediaEvent::DurationKnown { .. } => {}
            }
        }
    }

    fn persist_preferences(&self) {
        if let Err(error) = self.preferences.save() {
            // A device-local preference must never prevent playback. The next
            // successful setting change can retry the atomic merged write.
            tracing::warn!(%error, "video player: could not persist playback preferences");
        }
    }

    fn maybe_loop_current_media(&mut self) {
        if !self.preferences.loop_enabled
            || !self.player.has_media()
            || self
                .collab
                .as_ref()
                .is_some_and(|collab| collab.role() == Role::Follower)
        {
            return;
        }
        let duration = self.player.duration_ms();
        if duration > 0 && self.player.position_ms() >= duration {
            self.player.seek_to_ms(0);
            self.player.play();
            self.dirty = true;
        }
    }

    /// Where Open is: its button over a video, the empty state's action
    /// without one.
    fn open_target_rect(&self) -> Rect {
        if self.open_button_rect.width() > 0.0 {
            self.open_button_rect
        } else {
            self.status_action_rect
        }
    }

    fn handle_app_pointer(&mut self, state: &mut AutoCommit) -> bool {
        self.drawer.resolve(self.rect);
        if self.library_visible && self.drawer.scrim_contains(self.last_cursor) {
            // A press on the scrim dismisses the sheet.
            self.request_library_hide();
            self.dirty = true;
            return true;
        }
        let in_list = self.library_list_rect.contains(self.last_cursor);
        if self.library_visible && self.drawer.sidebar_contains(self.last_cursor) {
            if self.library_close_rect.contains(self.last_cursor) {
                self.request_library_hide();
            } else if self.library_queue_tab_rect.contains(self.last_cursor) {
                self.request_queue_library();
            } else if self.library_recent_tab_rect.contains(self.last_cursor) {
                self.request_recent_library();
            } else if self.library_shared_tab_rect.contains(self.last_cursor) {
                self.request_shared_library();
            } else if self.library_tab == LibraryTab::Shared
                && self.shared_watch_later_rect.contains(self.last_cursor)
            {
                self.add_current_to_watch_later(state);
            } else if self.library_tab == LibraryTab::Shared
                && self.shared_bookmark_rect.contains(self.last_cursor)
            {
                self.bookmark_current_time(state);
            } else if self.library_tab == LibraryTab::Shared
                && in_list
                && let Some(row) = self
                    .shared_row_hits
                    .iter()
                    .find(|row| row.jump.is_some_and(|jump| jump.contains(self.last_cursor)))
                    .cloned()
            {
                self.request_annotation_jump(state, &row.id);
            } else if self.library_tab == LibraryTab::Shared
                && in_list
                && let Some(row) = self
                    .shared_row_hits
                    .iter()
                    .find(|row| row.remove.contains(self.last_cursor))
                    .cloned()
            {
                let action = if row.annotation {
                    crate::shared::SharedAction::DeleteAnnotation { id: row.id }
                } else {
                    crate::shared::SharedAction::DeletePlaylist {
                        observed_item_ids: self
                            .shared_library
                            .items
                            .iter()
                            .filter(|item| item.playlist_id == row.id)
                            .map(|item| item.id.clone())
                            .collect(),
                        id: row.id,
                    }
                };
                self.dispatch_shared(state, &action);
            } else if self.library_tab == LibraryTab::Recent
                && self.library_clear_rect.contains(self.last_cursor)
            {
                self.request_library_clear();
            } else if in_list
                && let Some(row) = self
                    .library_row_hits
                    .iter()
                    .find(|row| row.row.contains(self.last_cursor))
                    .cloned()
            {
                if row
                    .remove
                    .is_some_and(|rect| rect.contains(self.last_cursor))
                {
                    self.request_library_remove(&row.id);
                } else {
                    self.request_library_play(&row.id);
                }
            }
            self.dirty = true;
            return true;
        }
        if self.status_action_rect.contains(self.last_cursor) {
            self.request_open_action();
            return true;
        }
        if self.overlay_visible() && self.open_button_rect.contains(self.last_cursor) {
            self.request_open_action();
            return true;
        }
        if self.overlay_visible() && self.library_button_rect.contains(self.last_cursor) {
            self.request_library_visibility_change();
            return true;
        }
        false
    }

    /// Handle the keyboard values this app owns. Factoring this from winit's
    /// event carrier keeps shortcut tests fully safe and avoids fabricating
    /// invalid platform event structs just to exercise a key binding.
    fn handle_app_shortcut_input(
        &mut self,
        logical_key: &winit::keyboard::Key,
        state: ElementState,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        if state != ElementState::Pressed {
            return false;
        }
        // The library is an app-owned side panel rather than a modal supplied
        // by the shared player surface. Give it the conventional close key so
        // keyboard and switch-control users never have to hunt for the small
        // header button. Do not consume Escape while the panel is absent: the
        // shell and the embedded player may have a more relevant use for it.
        if matches!(
            logical_key,
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
        ) && self.library_visible
        {
            self.request_library_hide();
            return true;
        }
        let winit::keyboard::Key::Character(key) = logical_key else {
            return false;
        };
        if modifiers.state().control_key() && key.eq_ignore_ascii_case("o") {
            self.request_open_action();
        } else if key.eq_ignore_ascii_case("l") {
            self.request_library_visibility_change();
        } else if key.eq_ignore_ascii_case("p") && self.player.has_media() {
            self.perform_resource_operation(PlayerOperation::OpenPictureInPicture);
        } else {
            return false;
        }
        true
    }

    /// Route a logical key through the app-owned shortcuts, then the focused
    /// reusable player surface. This mirrors the live event order while
    /// remaining safe to exercise without fabricating a platform `KeyEvent`.
    fn route_key_input(
        &mut self,
        logical_key: &winit::keyboard::Key,
        state: ElementState,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        self.handle_app_shortcut_input(logical_key, state, modifiers)
            || self.player.handle_key_input(logical_key, state)
    }
}

impl Default for PlayerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl FloatingSurfaceProvider for PlayerApp {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.pending_pip.take()
    }
}

impl FullscreenProvider for PlayerApp {
    fn fullscreen_intent(&self) -> Option<bool> {
        FullscreenProvider::fullscreen_intent(&self.player)
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        FullscreenProvider::fullscreen_params(&self.player)
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        FullscreenProvider::on_fullscreen_changed(&mut self.player, active);
    }
}

impl Widget for PlayerApp {
    ui::provides!(
        dyn ui::widget::FullscreenProvider,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::undo::CommittedActions,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.drain_portal();
        if let Some(update) = self.poll_open_frame(invocation) {
            return Some(update);
        }
        self.resource_results
            .iter()
            .position(|update| update.invocation() == invocation)
            .and_then(|index| self.resource_results.remove(index))
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        self.poll_kept_recall();
        let shared = crate::shared::read(state);
        if shared != self.shared_library {
            self.shared_library = shared;
            self.invalidate_search_cache();
            self.dirty = true;
        }
        self.drain_portal();
        #[cfg(test)]
        if let Some(observer) = &self.history_test_open_error_observer {
            observer.replace(self.open_error.clone());
        }
        self.tick_collab();
        self.player.sync(state, path);
        self.drain_player_media_events();
        self.sync_decoder_settings();
        self.drain_player_settings();
        self.maybe_loop_current_media();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.drawer.scale_for(theme);
        // Edge-to-edge: the player fills the whole surface. The app keeps itself
        // in the node map (so it can intercept the floating Open pill) by owning
        // a single-child root that stretches the player to 100%×100%.
        let player_node = self.player.build_layout(taffy, theme, node_map);
        let root = parent_node(
            taffy,
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                ..Default::default()
            },
            &[player_node],
        );
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        let Ok(children) = taffy.children(node) else {
            return;
        };
        // children[0] = player, filling the whole surface (edge-to-edge).
        let Some(&player_node) = children.first() else {
            return;
        };
        self.player.apply_layout(taffy, player_node, new_origin);
        self.drawer.resolve(self.rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.sync_library_sheet();
        self.drawer.tick(theme);
        if self.shot_loading {
            self.paint_letterbox(scene, transform, theme);
        } else {
            self.player
                .draw(scene, transform, text_renderer, theme, presence);
        }
        self.paint_media_status(scene, transform, text_renderer, theme);
        self.place_top_strip(text_renderer, theme);
        if self.overlay_visible() {
            self.paint_overlay(scene, transform, text_renderer, theme);
        }
        self.paint_library_panel(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.handle_app_pointer(ctx.state)
        {
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.library_visible
            && self.drawer.sidebar_contains(self.last_cursor)
        {
            let step = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, lines) => {
                    f64::from(*lines) * self.library_list_rect.width() / 8.0
                }
                winit::event::MouseScrollDelta::PixelDelta(pixels) => pixels.y / ctx.scale_factor,
            };
            self.library_scroll = (self.library_scroll - step).clamp(0.0, self.library_max_scroll);
            self.dirty = true;
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::KeyboardInput { event, .. } = event
            && self.route_key_input(&event.logical_key, event.state, *ctx.modifiers)
        {
            *ctx.handled = true;
            return;
        }
        // Forward everything else to the player (its controls + shortcuts).
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        if let Some(&player_node) = children.first() {
            self.player
                .handle_event(event, &mut ctx.reborrow(ctx.path, player_node));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.player as &dyn Widget]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.player as &mut (dyn Widget + '_)]
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name("Open video")
                .with_description("Ctrl+O")
                .with_action_target("video-player.open")
                .with_bounds(self.open_target_rect())
                .with_state(AccessState {
                    focusable: true,
                    focused: self.open_focused,
                    disabled: self
                        .open_action
                        .as_ref()
                        .is_none_or(|action| !action.is_available()),
                    ..Default::default()
                }),
            AccessNode::new(Role::Button)
                .with_name(if self.library_visible {
                    "Hide library"
                } else {
                    "Show queue and recent videos"
                })
                .with_description("L")
                .with_action_target("video-player.library")
                .with_bounds(self.library_button_rect)
                .with_state(AccessState {
                    focusable: true,
                    focused: self.library_focused,
                    ..Default::default()
                }),
        ];
        if self.library_visible {
            children.extend([
                AccessNode::new(Role::Button)
                    .with_name("Close library")
                    .with_description("Escape")
                    .with_action_target("video-player.library.close")
                    .with_bounds(self.library_close_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.library_focus_id.as_deref() == Some("library-close"),
                        ..Default::default()
                    }),
                AccessNode::new(Role::Tab)
                    .with_name("Queue")
                    .with_action_target("video-player.library.tab.queue")
                    .with_bounds(self.library_queue_tab_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.library_focus_id.as_deref() == Some("library-tab-queue"),
                        selected: self.library_tab == LibraryTab::Queue,
                        ..Default::default()
                    }),
                AccessNode::new(Role::Tab)
                    .with_name("Recent")
                    .with_action_target("video-player.library.tab.recent")
                    .with_bounds(self.library_recent_tab_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.library_focus_id.as_deref() == Some("library-tab-recent"),
                        selected: self.library_tab == LibraryTab::Recent,
                        ..Default::default()
                    }),
                AccessNode::new(Role::Tab)
                    .with_name("Shared")
                    .with_action_target("video-player.library.tab.shared")
                    .with_bounds(self.library_shared_tab_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.library_focus_id.as_deref() == Some("library-tab-shared"),
                        selected: self.library_tab == LibraryTab::Shared,
                        ..Default::default()
                    }),
            ]);
            if self.library_tab == LibraryTab::Shared {
                children.extend(shared_accessibility_nodes(self));
            }
            children.extend(local_library_accessibility_nodes(self));
        }
        Some(
            AccessNode::new(Role::Group)
                .with_name(if self.player.has_media() {
                    self.title.clone()
                } else {
                    "Video Player".into()
                })
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0] => Some(self.open_target_rect()),
            [1] => Some(self.library_button_rect),
            [2] if self.library_visible => Some(self.library_close_rect),
            [3] if self.library_visible => Some(self.library_queue_tab_rect),
            [4] if self.library_visible => Some(self.library_recent_tab_rect),
            [5] if self.library_visible => Some(self.library_shared_tab_rect),
            [6] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                Some(self.shared_watch_later_rect)
            }
            [7] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                Some(self.shared_bookmark_rect)
            }
            [index] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                shared_accessibility_targets(self)
                    .get(index.saturating_sub(8))
                    .map(|(_, _, rect)| *rect)
            }
            [index] => {
                let library_index = index.saturating_sub(6);
                let play_count = self.library_row_hits.len();
                if let Some(row) = self.library_row_hits.get(library_index) {
                    Some(row.play)
                } else if self.library_tab == LibraryTab::Recent
                    && !self.recent.is_empty()
                    && library_index == play_count
                {
                    Some(self.library_clear_rect)
                } else {
                    let remove_index = library_index.saturating_sub(
                        play_count
                            + usize::from(
                                self.library_tab == LibraryTab::Recent && !self.recent.is_empty(),
                            ),
                    );
                    self.library_row_hits
                        .iter()
                        .filter_map(|row| row.remove)
                        .nth(remove_index)
                }
            }
            _ => None,
        }
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        match path {
            [0] => {
                self.open_focused = true;
                self.library_focused = false;
                self.player.reveal_controls();
                self.dirty = true;
                true
            }
            [1] => {
                self.library_focused = true;
                self.open_focused = false;
                self.player.reveal_controls();
                self.dirty = true;
                true
            }
            [2] if self.library_visible => {
                self.library_focus_id = Some("library-close".into());
                self.open_focused = false;
                self.library_focused = false;
                self.dirty = true;
                true
            }
            [3] if self.library_visible => {
                self.library_focus_id = Some("library-tab-queue".into());
                self.open_focused = false;
                self.library_focused = false;
                self.dirty = true;
                true
            }
            [4] if self.library_visible => {
                self.library_focus_id = Some("library-tab-recent".into());
                self.open_focused = false;
                self.library_focused = false;
                self.dirty = true;
                true
            }
            [5] if self.library_visible => {
                self.library_focus_id = Some("library-tab-shared".into());
                self.open_focused = false;
                self.library_focused = false;
                self.dirty = true;
                true
            }
            [6] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                self.library_focus_id = Some("shared-watch-later".into());
                self.dirty = true;
                true
            }
            [7] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                self.library_focus_id = Some("shared-bookmark".into());
                self.dirty = true;
                true
            }
            [index] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                let Some((focus, _, _)) = shared_accessibility_targets(self)
                    .get(index.saturating_sub(8))
                    .cloned()
                else {
                    return false;
                };
                self.library_focus_id = Some(focus);
                self.dirty = true;
                true
            }
            [index] => {
                let library_index = index.saturating_sub(6);
                let play_count = self.library_row_hits.len();
                self.library_focus_id = if let Some(row) = self.library_row_hits.get(library_index)
                {
                    Some(row.id.clone())
                } else if self.library_tab == LibraryTab::Recent
                    && !self.recent.is_empty()
                    && library_index == play_count
                {
                    Some("library-clear".into())
                } else {
                    let remove_index = library_index.saturating_sub(
                        play_count
                            + usize::from(
                                self.library_tab == LibraryTab::Recent && !self.recent.is_empty(),
                            ),
                    );
                    self.library_row_hits
                        .iter()
                        .filter_map(|row| row.remove.as_ref().map(|_| row.id.as_str()))
                        .nth(remove_index)
                        .map(|id| format!("library-remove-{id}"))
                };
                if self.library_focus_id.is_none() {
                    return false;
                }
                self.open_focused = false;
                self.library_focused = false;
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if target == "video-player.open" && matches!(action, ui::a11y::Action::Activate) {
            self.request_open_action();
            true
        } else if target == "video-player.library" && matches!(action, ui::a11y::Action::Activate) {
            self.request_library_visibility_change();
            true
        } else if let Some(item_id) = target.strip_prefix("video-player.library.play.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_play(item_id);
            true
        } else if target == "video-player.library.close"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_hide();
            true
        } else if target == "video-player.library.tab.queue"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_queue_library();
            true
        } else if target == "video-player.library.tab.recent"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_recent_library();
            true
        } else if target == "video-player.library.tab.shared"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_shared_library();
            true
        } else if target == "video-player.shared.watch-later"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.add_current_to_watch_later(state);
            true
        } else if target == "video-player.shared.bookmark"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.bookmark_current_time(state);
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.jump.annotation.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_annotation_jump(state, rest);
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.delete.annotation.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.dispatch_shared(
                state,
                &crate::shared::SharedAction::DeleteAnnotation { id: rest.into() },
            );
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.delete.playlist.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.dispatch_shared(
                state,
                &crate::shared::SharedAction::DeletePlaylist {
                    observed_item_ids: self
                        .shared_library
                        .items
                        .iter()
                        .filter(|item| item.playlist_id == rest)
                        .map(|item| item.id.clone())
                        .collect(),
                    id: rest.into(),
                },
            );
            true
        } else if target == "video-player.library.clear"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_clear();
            true
        } else if let Some(item_id) = target.strip_prefix("video-player.library.remove.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_remove(item_id);
            true
        } else {
            false
        }
    }

    fn tooltip(&self) -> Option<&str> {
        if (self.overlay_visible() && self.open_button_rect.contains(self.last_cursor))
            || self.status_action_rect.contains(self.last_cursor)
        {
            Some("Open video (Ctrl+O)")
        } else if self.overlay_visible() && self.library_button_rect.contains(self.last_cursor) {
            Some(if self.library_visible {
                "Hide queue and recent videos (L)"
            } else {
                "Show queue and recent videos (L)"
            })
        } else if self.library_visible && self.library_close_rect.contains(self.last_cursor) {
            Some("Close library (Escape)")
        } else if self.library_visible && self.library_queue_tab_rect.contains(self.last_cursor) {
            Some("Show this device's queue")
        } else if self.library_visible && self.library_recent_tab_rect.contains(self.last_cursor) {
            Some("Show recently played videos on this device")
        } else if self.library_visible && self.library_shared_tab_rect.contains(self.last_cursor) {
            Some("Show shared playlists and timestamp notes")
        } else if self.library_visible
            && self.library_tab == LibraryTab::Shared
            && self.shared_watch_later_rect.contains(self.last_cursor)
        {
            Some("Add the current video to the shared Watch later playlist")
        } else if self.library_visible
            && self.library_tab == LibraryTab::Shared
            && self.shared_bookmark_rect.contains(self.last_cursor)
        {
            Some("Add a shared bookmark at the current playback time")
        } else if self.library_visible
            && self.library_tab == LibraryTab::Shared
            && let Some(row) = self
                .shared_row_hits
                .iter()
                .find(|row| row.remove.contains(self.last_cursor))
        {
            Some(if row.annotation {
                "Delete this shared timestamp note"
            } else {
                "Delete this shared playlist"
            })
        } else if self.library_visible
            && self.library_tab == LibraryTab::Recent
            && self.library_clear_rect.contains(self.last_cursor)
        {
            Some("Clear recent videos from this device")
        } else if self.library_visible
            && self.library_tab == LibraryTab::Shared
            && self
                .shared_row_hits
                .iter()
                .any(|row| row.jump.is_some_and(|jump| jump.contains(self.last_cursor)))
        {
            Some("Jump to this timestamp in the current video")
        } else if self.library_row_hits.iter().any(|row| {
            row.remove
                .is_some_and(|rect| rect.contains(self.last_cursor))
        }) {
            Some("Remove from this session queue")
        } else if self
            .library_row_hits
            .iter()
            .any(|row| row.play.contains(self.last_cursor))
        {
            Some("Play this local video")
        } else {
            None
        }
    }

    fn name(&self) -> &'static str {
        "PlayerApp"
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.player.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.player.mark_drawn();
    }

    fn is_layout_dirty(&self) -> bool {
        self.player.is_layout_dirty()
    }

    fn needs_animation_frame(&self) -> bool {
        // Collaboration is polled through `needs_idle_sync`; treating an idle,
        // paused collab session as animation would otherwise force a 60 Hz
        // redraw forever. Playback and the portal are the only frame-rate work.
        self.player.needs_animation_frame()
            || self.drawer.is_animating()
            || self.portal_open.is_some()
            || self.media_open.is_some()
    }

    fn needs_idle_sync(&self) -> bool {
        // A follower must keep syncing even with no local input so it applies
        // the host's incoming transport.
        self.collab.is_some()
    }
}

impl ui::search::capability::SearchSource for PlayerApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        let source_id = self.current_source_id.clone();
        let revision = self.metadata_revision;
        let mut cache = self.search_cache.borrow_mut();
        let reusable = cache
            .as_ref()
            .is_some_and(|cached| cached.source_id == source_id && cached.revision == revision);
        if !reusable {
            let provider = crate::search::VideoPlayerSearchProvider::new(
                &self.action_context(
                    &self.search_metadata.audio_tracks,
                    &self.search_metadata.subtitle_tracks,
                    &self.search_metadata.chapters,
                ),
                &self.shared_library,
            );
            *cache = Some(SearchCache {
                source_id,
                revision,
                provider: Arc::new(provider),
            });
            self.search_catalog_builds
                .set(self.search_catalog_builds.get().saturating_add(1));
        }
        if let Some(cached) = cache.as_ref() {
            vec![Box::new((*cached.provider).clone())]
        } else {
            // The cache was initialized immediately above. Returning no stale
            // provider is safer than panicking if that invariant ever changes.
            Vec::new()
        }
    }
}

impl ui::resources::PublishesResources for PlayerApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let (audio_tracks, subtitle_tracks, chapters) = self.decoder_metadata();
        let ctx = self.action_context(&audio_tracks, &subtitle_tracks, &chapters);
        // Framework [`tilo_resource::publish`] owns the application root,
        // transport/library Views, and shared-library document children.
        // Audio/subtitle/chapter rows are live decoder presentation only —
        // keep that same standalone slice when hosted, never a second root.
        if self.framework_hosted {
            return crate::actions::live_track_resources(&ctx, doc);
        }
        crate::actions::resources(&ctx, doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.framework_hosted && id == "video-player.application" {
            return None;
        }
        let (audio_tracks, subtitle_tracks, chapters) = self.decoder_metadata();
        crate::actions::resource(
            &self.action_context(&audio_tracks, &subtitle_tracks, &chapters),
            doc,
            id,
        )
    }
}

impl ui::widget::controls::ActionControls for PlayerApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.open_action
            .as_mut()
            .and_then(ActionEmitter::take)
            .or_else(|| self.library_control_invocations.pop_front())
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        if let Some(action) = &self.open_action
            && let Some(binding) = bindings
                .iter_mut()
                .find(|binding| binding.control_id == "video-player.open")
        {
            *binding = action.metadata("video-player.open");
        }
        bindings
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.open_action {
            action.refresh_availability(allowed);
        }
    }
}

impl ui::history_mode::capability::DocumentHistory for PlayerApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl ui::undo::CommittedActions for PlayerApp {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl PlayerApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode(type_id, payload)? else {
            return Ok(false);
        };
        if operation.requires_media() && !self.player.has_media() {
            return Err("open a video before using this playback control".into());
        }
        self.validate_dynamic_target(&operation)?;
        if let PlayerOperation::JumpToAnnotation {
            id,
            media_ref,
            time_ms,
        } = &operation
        {
            let annotation_is_current =
                crate::shared::read(doc)
                    .annotations
                    .iter()
                    .any(|annotation| {
                        annotation.id == *id
                            && annotation.media_ref == *media_ref
                            && annotation.time_ms == *time_ms
                    });
            if !annotation_is_current {
                return Err("the review marker changed; refresh search and try again".into());
            }
        }
        if self
            .collab
            .as_ref()
            .is_some_and(|collab| collab.role() == Role::Follower)
            && matches!(
                &operation,
                crate::actions::PlayerOperation::TogglePlayback
                    | crate::actions::PlayerOperation::SeekSeconds(_)
                    | crate::actions::PlayerOperation::SeekBySeconds(_)
                    | crate::actions::PlayerOperation::Restart
                    | crate::actions::PlayerOperation::SeekToEnd
                    | crate::actions::PlayerOperation::SetLoop(_)
                    | crate::actions::PlayerOperation::SetPlaybackRate(_)
                    | crate::actions::PlayerOperation::JumpToChapter { .. }
                    | crate::actions::PlayerOperation::JumpToAnnotation { .. }
            )
        {
            return Err("this player is following the collaborative host".into());
        }
        if let crate::actions::PlayerOperation::Open { path: Some(path) } = &operation {
            if !Path::new(path).is_file() {
                let label = Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .unwrap_or("selected video");
                let message = format!("Video does not exist: {label}");
                self.open_error = Some(message.clone());
                self.dirty = true;
                return Err(message);
            }
            self.open_path(Path::new(path))?;
        } else if matches!(
            operation,
            crate::actions::PlayerOperation::Open { path: None }
        ) {
            self.trigger_open()?;
        } else if let crate::actions::PlayerOperation::OpenQueueItem { item_id } = &operation {
            self.open_local_entry(item_id)?;
        } else if matches!(
            operation,
            crate::actions::PlayerOperation::ClearRecentHistory
        ) {
            self.clear_recent_history()?;
        } else if let crate::actions::PlayerOperation::RemoveQueueItem { item_id } = &operation {
            self.remove_queue_entry(item_id)?;
        } else {
            self.perform_resource_operation(operation);
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let operation = crate::actions::decode(type_id, payload)?;
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if matches!(
            operation,
            Some(crate::actions::PlayerOperation::Open { path: None })
        ) {
            let Some(pending) = self.portal_open.as_mut() else {
                return Err("video picker did not start".into());
            };
            pending.invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else if matches!(
            operation,
            Some(crate::actions::PlayerOperation::Open { path: Some(_) })
        ) {
            // A filesystem path exists before admission, but a real decoder
            // can still fail after `open`. Accepted therefore means only
            // "decoding has begun"; polling waits for an actual frame.
            self.await_open_frame(invocation)?;
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else if matches!(
            operation,
            Some(crate::actions::PlayerOperation::OpenQueueItem { .. })
        ) {
            self.await_open_frame(invocation)?;
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::strip::TopStrip;
    use super::*;
    use ui::headless::Framed;

    #[test]
    fn opening_sources_sets_identity_without_exposing_paths() {
        // Case `source_for_path_splits_local_vs_workspace`.
        {
            // Point the mount at a scratch root so the classification is testable.
            let root = std::env::temp_dir().join(format!("tilo-vp-app-{}", std::process::id()));
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ROOT", &root);
            let ws = source_for_path(&root.join("media/a.mp4"));
            assert!(matches!(ws, media::Source::Workspace(_)));
            let local = source_for_path(Path::new("/home/u/b.mp4"));
            assert!(matches!(local, media::Source::LocalFile(_)));
        }
        // Case `opening_a_path_sets_title_and_loads_the_engine`.
        {
            let mut app = PlayerApp::new_with_preferences(PlaybackPreferences::default());
            app.open_path(&PathBuf::from("/tmp/Holiday.mp4"))
                .expect("source identity available");
            assert_eq!(app.title, "Holiday.mp4");
            assert!(app.player.is_playing(), "autoplay on open");
            assert_eq!(
                app.player.engine().unwrap().source(),
                Some(&media::Source::LocalFile(PathBuf::from("/tmp/Holiday.mp4")))
            );
        }
        // Case `source_identity_generation_exhaustion_refuses_the_new_open`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Original.mp4"))
                .expect("initial source identity");
            let original_id = app.current_source_id.clone();
            let original_title = app.title.clone();
            let original_source = app
                .player
                .engine()
                .and_then(|engine| engine.source())
                .cloned();
            app.source_generation = u64::MAX;

            let error = app
                .open_path(Path::new("/tmp/New.mp4"))
                .expect_err("source generation must not wrap into an old resource identity");
            assert!(error.contains("identity generation exhausted"));
            assert_eq!(app.source_generation, u64::MAX);
            assert_eq!(app.current_source_id, original_id);
            assert_eq!(app.title, original_title);
            assert_eq!(
                app.player.engine().and_then(|engine| engine.source()),
                original_source.as_ref()
            );
        }
        // Case `local_source_identity_and_now_playing_never_expose_an_absolute_path`.
        {
            let mut app = synth_app();
            let path = Path::new("/home/alice/Private videos/launch-cut.mp4");
            app.open_path(path)
                .expect("local source opens in the synthesized engine");
            let source_id = app.current_source_id.as_deref().unwrap_or_default();
            assert!(source_id.starts_with("local-media-"));
            assert!(!source_id.contains("/home/alice"));
            assert!(!source_id.contains("launch-cut"));
        }
        // Case `reopening_the_same_uri_invalidates_old_dynamic_targets`.
        {
            let mut app = PlayerApp::for_shot(None, Some("metadata"));
            let old_source = app.current_source_id.clone().unwrap();
            let source = media::Source::Remote {
                url: "https://demo.local/clip.mp4".into(),
            };
            app.set_current_source(&source)
                .expect("source identity available");
            assert_ne!(app.current_source_id.as_deref(), Some(old_source.as_str()));
            let track = app.shot_metadata.as_ref().unwrap().audio_tracks[0].clone();
            assert!(
                app.validate_dynamic_target(&PlayerOperation::SelectAudioTrack {
                    source_id: old_source,
                    stream_id: track.stream_id,
                    index: track.index,
                })
                .is_err()
            );
        }
    }

    #[test]
    fn portal_open_is_single_flight_generation_fenced_and_retained() {
        // Case `portal_outcome_opens_the_chosen_file`.
        {
            let mut app = PlayerApp::new();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Chosen.webm")));
            ui::host::scoped(&services, || app.trigger_open()).expect("picker starts");
            assert_eq!(recorder.pick_requests().len(), 1);
            app.drain_portal();
            assert_eq!(app.title, "Chosen.webm");
            assert!(
                app.portal_open.is_none(),
                "channel cleared after the outcome"
            );
        }
        // Case `portal_open_is_single_flight_and_a_direct_open_supersedes_it`.
        {
            let mut app = PlayerApp::new();
            app.trigger_open().expect("first picker starts");
            let first_generation = app.portal_open.as_ref().map(|open| open.generation);
            assert!(
                app.trigger_open().is_err(),
                "a second request must not replace the first portal receiver"
            );
            assert_eq!(
                app.portal_open.as_ref().map(|open| open.generation),
                first_generation
            );

            app.open_path(Path::new("/tmp/Direct.mp4"))
                .expect("source identity available");
            assert!(
                app.portal_open.is_none(),
                "a direct open drops the stale picker receiver before it can win later"
            );
        }
        // Case `portal_generation_exhaustion_fails_closed_without_aliasing_old_requests`.
        {
            let mut app = PlayerApp::new();
            app.portal_generation = u64::MAX;
            let error = app.trigger_open().expect_err("generation must not wrap");
            assert!(error.contains("generation exhausted"));
            assert!(app.portal_open.is_none());
            assert_eq!(app.portal_generation, u64::MAX);
        }
        // Case `stale_portal_outcome_cannot_open_media_in_release_or_debug_builds`.
        {
            let mut app = PlayerApp::new();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Stale.webm")));
            ui::host::scoped(&services, || app.trigger_open()).expect("picker starts");
            // A newer request generation makes the delivered outcome stale.
            app.portal_generation += 1;
            app.drain_portal();
            assert_eq!(app.title, "No media");
            assert!(app.current_source_id.is_none());
            assert!(app.portal_open.is_none());
        }
        // Case `completed_portal_result_backpressures_new_async_requests_until_polled`.
        {
            let mut app = PlayerApp::new();
            app.queue_resource_result(tilo_actions::InvocationUpdate::Cancelled {
                invocation: tilo_actions::InvocationId::new(),
                message: Some("test completion".into()),
            });
            let error = app
                .trigger_open()
                .expect_err("unpolled completion applies backpressure");
            assert!(error.contains("awaiting host acknowledgement"));
            assert!(app.portal_open.is_none());
            assert_eq!(app.resource_results.len(), 1, "the completion is retained");
        }
        // Case `portal_failure_is_retained_for_the_empty_state`.
        {
            let mut app = PlayerApp::new();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Failed("permission denied".into()));
            ui::host::scoped(&services, || app.trigger_open()).expect("picker starts");
            app.drain_portal();
            assert_eq!(app.open_error.as_deref(), Some("permission denied"));
            assert!(app.player.latest_frame().is_none());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn markers_and_metadata_targets_stay_fresh_and_canonical() {
        // Case `durable_marker_media_reference_survives_a_reopen_but_source_actions_stay_fresh`.
        {
            let mut app = synth_app();
            let path = Path::new("/tmp/review-cut.mp4");
            app.open_path(path).expect("first source identity");
            let first_source_id = app.current_source_id.clone();
            let media_ref = app.current_media_ref.clone();
            app.open_path(path).expect("reopened source identity");
            assert_ne!(app.current_source_id, first_source_id);
            assert_eq!(app.current_media_ref, media_ref);
            assert!(
                !app.current_media_ref
                    .as_deref()
                    .unwrap_or_default()
                    .contains("review-cut"),
                "the durable reference remains opaque"
            );
        }
        // Case `marker_jump_control_uses_its_canonical_resource_action`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/review-cut.mp4"))
                .expect("source identity");
            let mut doc = AutoCommit::new();
            app.dispatch_shared(
                &mut doc,
                &crate::shared::SharedAction::CreateAnnotation {
                    id: "review-marker".into(),
                    media_ref: app.current_media_ref.clone().unwrap(),
                    time_ms: 12_500,
                    text: "Tighten this cut".into(),
                },
            );
            app.request_annotation_jump(&doc, "review-marker");
            let invocation = ui::widget::controls::take_control_invocation(&mut app)
                .expect("marker jump intent");
            assert_eq!(
                invocation.address.resource,
                crate::actions::annotation_resource_id("review-marker")
            );
            assert_eq!(invocation.address.action, "jump");
        }
        // Case `marker_jump_refuses_a_stale_document_target_before_seeking`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/review-cut.mp4"))
                .expect("source identity");
            let media_ref = app.current_media_ref.clone().expect("durable media ref");
            let mut doc = AutoCommit::new();
            app.dispatch_shared(
                &mut doc,
                &crate::shared::SharedAction::CreateAnnotation {
                    id: "review-marker".into(),
                    media_ref: media_ref.clone(),
                    time_ms: 12_500,
                    text: String::new(),
                },
            );
            let encoded = tilo_actions::EncodedAction::encode_operation(
                crate::actions::OPERATION_TYPE,
                &PlayerOperation::JumpToAnnotation {
                    id: "review-marker".into(),
                    media_ref,
                    time_ms: 14_000,
                },
            )
            .expect("operation encodes");
            let error = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut doc,
                &root(),
            )
            .expect_err("changed timestamp must be refused");
            assert!(error.contains("review marker changed"));
            assert_ne!(app.player.position_ms(), 14_000);
        }
        // Case `metadata_revision_boundary_discards_the_cache_before_resetting`.
        {
            let mut app = PlayerApp::for_shot(None, Some("metadata"));
            let _first = ui::search::capability::search_providers(&app);
            assert_eq!(app.search_catalog_builds.get(), 1);
            assert!(app.search_cache.borrow().is_some());

            app.metadata_revision = u64::MAX;
            app.search_metadata = DecoderMetadata::default();
            app.sync_decoder_settings();
            assert_eq!(app.metadata_revision, 0);
            assert!(
                app.search_cache.borrow().is_none(),
                "the old max-revision provider is discarded before reusing revision zero"
            );

            let _second = ui::search::capability::search_providers(&app);
            assert_eq!(
                app.search_catalog_builds.get(),
                2,
                "the fresh post-boundary provider is rebuilt instead of reusing stale metadata"
            );
        }
        // Case `stale_stream_and_chapter_targets_are_refused_before_engine_mutation`.
        {
            let app = PlayerApp::for_shot(None, Some("metadata"));
            let audio = app.shot_metadata.as_ref().unwrap().audio_tracks[0].clone();
            let chapter = app.shot_metadata.as_ref().unwrap().chapters[0].clone();
            let stale_source = "https://old.example/clip.mkv".to_string();
            assert!(
                app.validate_dynamic_target(&PlayerOperation::SelectAudioTrack {
                    source_id: stale_source.clone(),
                    stream_id: audio.stream_id,
                    index: audio.index,
                })
                .is_err()
            );
            assert!(
                app.validate_dynamic_target(&PlayerOperation::JumpToChapter {
                    source_id: stale_source,
                    id: chapter.id,
                    seconds: chapter.seconds,
                })
                .is_err()
            );
            // Reordered array positions are rejected even for the same source.
            let source_id = app.current_source_id.clone().unwrap();
            assert!(
                app.validate_dynamic_target(&PlayerOperation::SelectAudioTrack {
                    source_id,
                    stream_id: "different-stream".into(),
                    index: 0,
                })
                .is_err()
            );
            let source_id = app.current_source_id.clone().unwrap();
            let selected = app.shot_metadata.as_ref().unwrap().subtitle_tracks[0].clone();
            let off = PlayerOperation::SelectSubtitleTrack {
                source_id: source_id.clone(),
                target_stream_id: None,
                target_index: None,
                expected_stream_id: Some(selected.stream_id.clone()),
                expected_index: Some(selected.index),
            };
            assert!(app.validate_dynamic_target(&off).is_ok());
            assert!(
                app.validate_dynamic_target(&PlayerOperation::SelectSubtitleTrack {
                    source_id,
                    target_stream_id: Some(selected.stream_id),
                    target_index: None,
                    expected_stream_id: None,
                    expected_index: None,
                })
                .is_err(),
                "mismatched target pairs are rejected"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn library_controls_emit_canonical_host_actions_and_are_accessible() {
        // Case `open_affordance_emits_the_advertised_host_routable_effect`.
        {
            let mut app = PlayerApp::new();
            app.request_open_action();
            let invocation =
                ui::widget::controls::take_control_invocation(&mut app).expect("open intent");
            assert_eq!(invocation.address.resource, "video-player.application");
            assert_eq!(invocation.address.action, "open");
            assert!(
                app.portal_open.is_none(),
                "the control did not bypass the host"
            );
        }
        // Case `library_row_emits_its_canonical_play_action_without_opening_directly`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            let item_id = app.queue[1].id.clone();
            let source_before = app.current_source_id.clone();
            app.request_library_play(&item_id);
            let invocation =
                ui::widget::controls::take_control_invocation(&mut app).expect("queue play intent");
            assert_eq!(
                invocation.address.resource,
                crate::actions::queue_item_resource_id(&item_id)
            );
            assert_eq!(invocation.address.action, "play");
            assert_eq!(
                app.current_source_id, source_before,
                "click did not bypass host lifecycle"
            );
        }
        // Case `library_remove_and_clear_controls_emit_their_canonical_host_actions`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            let item_id = app.queue[1].id.clone();
            app.request_library_remove(&item_id);
            let remove =
                ui::widget::controls::take_control_invocation(&mut app).expect("remove intent");
            assert_eq!(
                remove.address.resource,
                crate::actions::queue_item_resource_id(&item_id)
            );
            assert_eq!(remove.address.action, "remove");
            app.request_library_clear();
            let clear =
                ui::widget::controls::take_control_invocation(&mut app).expect("clear intent");
            assert_eq!(clear.address.resource, "video-player.history.local");
            assert_eq!(clear.address.action, "clear");
        }
        // Case `library_play_focus_has_a_real_accessibility_target_and_visual_state`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            let focused_id = app.queue[1].id.clone();
            app.library_row_hits = vec![
                LibraryRowHit {
                    id: app.queue[0].id.clone(),
                    row: Rect::ZERO,
                    play: Rect::new(0.0, 0.0, 44.0, 44.0),
                    remove: Some(Rect::new(44.0, 0.0, 88.0, 44.0)),
                },
                LibraryRowHit {
                    id: focused_id.clone(),
                    row: Rect::ZERO,
                    play: Rect::new(0.0, 44.0, 44.0, 88.0),
                    remove: Some(Rect::new(44.0, 44.0, 88.0, 88.0)),
                },
            ];
            assert!(Widget::focus_synthetic_accessibility(&mut app, &[7]));
            assert_eq!(app.library_focus_id.as_deref(), Some(focused_id.as_str()));
            assert_eq!(
                Widget::synthetic_accessibility_rect(&app, &[7]),
                Some(Rect::new(0.0, 44.0, 44.0, 88.0))
            );
        }
        // Case `library_close_and_tabs_are_reachable_accessibility_controls`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            app.library_close_rect = Rect::new(0.0, 0.0, 44.0, 44.0);
            app.library_queue_tab_rect = Rect::new(44.0, 0.0, 132.0, 44.0);
            app.library_recent_tab_rect = Rect::new(132.0, 0.0, 220.0, 44.0);
            app.library_shared_tab_rect = Rect::new(220.0, 0.0, 308.0, 44.0);

            let tree = Widget::accessibility(&app).expect("application accessibility tree");
            assert_eq!(tree.children[2].name.as_deref(), Some("Close library"));
            assert_eq!(tree.children[3].name.as_deref(), Some("Queue"));
            assert!(tree.children[3].state.selected);
            assert_eq!(tree.children[4].name.as_deref(), Some("Recent"));
            assert!(!tree.children[4].state.selected);
            assert_eq!(tree.children[5].name.as_deref(), Some("Shared"));
            assert!(!tree.children[5].state.selected);

            assert!(Widget::focus_synthetic_accessibility(&mut app, &[4]));
            assert_eq!(app.library_focus_id.as_deref(), Some("library-tab-recent"));
            let mut doc = AutoCommit::new();
            let path = root();
            assert!(Widget::perform_semantic_action(
                &mut app,
                "video-player.library.tab.recent",
                &ui::a11y::Action::Activate,
                &mut doc,
                &path,
            ));
            let recent =
                ui::widget::controls::take_control_invocation(&mut app).expect("recent intent");
            assert_eq!(recent.address.resource, "video-player.history.local");
            assert_eq!(recent.address.action, "show");
            assert_eq!(
                app.library_tab,
                LibraryTab::Queue,
                "a11y did not bypass host routing"
            );
            assert!(Widget::perform_semantic_action(
                &mut app,
                "video-player.library.close",
                &ui::a11y::Action::Activate,
                &mut doc,
                &path,
            ));
            let close =
                ui::widget::controls::take_control_invocation(&mut app).expect("close intent");
            assert_eq!(close.address.resource, "video-player.application");
            assert_eq!(close.address.action, "hide-library");
            assert!(app.library_visible, "a11y did not bypass host routing");

            app.perform_resource_operation(PlayerOperation::RevealRecentLibrary);
            assert_eq!(app.library_tab, LibraryTab::Recent);
            app.perform_resource_operation(PlayerOperation::HideLibrary);
            assert!(!app.library_visible);
        }
        // Case `library_has_conventional_escape_close_and_complete_hover_help`.
        {
            let mut app = PlayerApp::for_shot(None, Some("shared"));
            app.library_close_rect = Rect::new(0.0, 0.0, 44.0, 44.0);
            app.last_cursor = Point::new(20.0, 20.0);
            assert_eq!(app.tooltip(), Some("Close library (Escape)"));

            assert!(app.handle_app_shortcut_input(
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                ElementState::Pressed,
                winit::event::Modifiers::default(),
            ));
            let close = ui::widget::controls::take_control_invocation(&mut app)
                .expect("escape close intent");
            assert_eq!(close.address.resource, "video-player.application");
            assert_eq!(close.address.action, "hide-library");
            assert!(
                app.library_visible,
                "Escape did not bypass the host action lifecycle"
            );
        }
        // Case `library_navigation_is_published_as_canonical_actions`.
        {
            let app = PlayerApp::for_shot(None, Some("library"));
            let resources = ui::resources::resources(&app, &AutoCommit::new());
            let queue =
                find_resource(&resources, "video-player.queue.local").expect("queue resource");
            let recent =
                find_resource(&resources, "video-player.history.local").expect("recent resource");
            let shared = find_resource(&resources, "video-player.playlists.shared")
                .expect("shared library resource");
            let root = &resources[0];

            assert!(
                root.actions
                    .iter()
                    .any(|action| action.id == "show-library")
            );
            assert!(
                root.actions
                    .iter()
                    .any(|action| action.id == "hide-library")
            );
            assert!(queue.actions.iter().any(|action| action.id == "show"));
            assert!(recent.actions.iter().any(|action| action.id == "show"));
            assert!(shared.actions.iter().any(|action| action.id == "show"));
            for action in root
                .actions
                .iter()
                .filter(|action| matches!(action.id.as_str(), "show-library" | "hide-library"))
                .chain(queue.actions.iter().filter(|action| action.id == "show"))
                .chain(recent.actions.iter().filter(|action| action.id == "show"))
                .chain(shared.actions.iter().filter(|action| action.id == "show"))
            {
                assert_eq!(action.contract.retry, tilo_actions::RetryPolicy::Idempotent);
                assert_eq!(
                    action.contract.execution,
                    tilo_actions::ExecutionMode::Immediate
                );
                assert_eq!(
                    action.contract.tags,
                    vec!["device-local", "media", "presentation"]
                );
            }
            let queued_item = queue.children.first().expect("queued item");
            assert_eq!(
                queued_item
                    .actions
                    .iter()
                    .find(|action| action.id == "remove")
                    .expect("remove action")
                    .contract
                    .retry,
                tilo_actions::RetryPolicy::AtMostOnce
            );
        }
        // Case `library_visibility_controls_restore_the_last_explicit_tab`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            let mut doc = AutoCommit::new();

            // Pointer activation reopens Recent, not the hard-coded Queue default.
            app.library_tab = LibraryTab::Recent;
            app.library_button_rect = Rect::new(0.0, 0.0, 100.0, 44.0);
            app.last_cursor = Point::new(20.0, 20.0);
            assert!(app.handle_app_pointer(&mut doc));
            let hide = ui::widget::controls::take_control_invocation(&mut app)
                .expect("pointer hide intent");
            assert_eq!(hide.address.action, "hide-library");
            app.perform_resource_operation(PlayerOperation::HideLibrary);
            assert!(app.handle_app_pointer(&mut doc));
            let recent = ui::widget::controls::take_control_invocation(&mut app)
                .expect("pointer reopen intent");
            assert_eq!(recent.address.resource, "video-player.history.local");
            assert_eq!(recent.address.action, "show");

            // `L` uses the same explicit intent and retains a Shared view.
            app.perform_resource_operation(PlayerOperation::RevealSharedLibrary);
            let shortcut = winit::keyboard::Key::Character("l".into());
            assert!(app.handle_app_shortcut_input(
                &shortcut,
                ElementState::Pressed,
                winit::event::Modifiers::default(),
            ));
            let hide = ui::widget::controls::take_control_invocation(&mut app)
                .expect("shortcut hide intent");
            assert_eq!(hide.address.action, "hide-library");
            app.perform_resource_operation(PlayerOperation::HideLibrary);
            assert!(app.handle_app_shortcut_input(
                &shortcut,
                ElementState::Pressed,
                winit::event::Modifiers::default(),
            ));
            let shared = ui::widget::controls::take_control_invocation(&mut app)
                .expect("shortcut reopen intent");
            assert_eq!(shared.address.resource, "video-player.playlists.shared");
            assert_eq!(shared.address.action, "show");

            // Assistive-tech activation is routed through the identical Queue
            // action rather than privately changing the current tab.
            app.perform_resource_operation(PlayerOperation::RevealLibrary);
            let path = root();
            assert!(Widget::perform_semantic_action(
                &mut app,
                "video-player.library",
                &ui::a11y::Action::Activate,
                &mut doc,
                &path,
            ));
            let hide =
                ui::widget::controls::take_control_invocation(&mut app).expect("a11y hide intent");
            assert_eq!(hide.address.action, "hide-library");
            app.perform_resource_operation(PlayerOperation::HideLibrary);
            assert!(Widget::perform_semantic_action(
                &mut app,
                "video-player.library",
                &ui::a11y::Action::Activate,
                &mut doc,
                &path,
            ));
            let queue = ui::widget::controls::take_control_invocation(&mut app)
                .expect("a11y reopen intent");
            assert_eq!(queue.address.resource, "video-player.queue.local");
            assert_eq!(queue.address.action, "show");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn player_resources_and_transport_guards_are_published_honestly() {
        // Case `picture_in_picture_uses_the_shell_owned_floating_surface_contract`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            let mut app = lay_out(app, (1280.0, 720.0), 1.0);
            app.perform_resource_operation(PlayerOperation::OpenPictureInPicture);
            let request =
                ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut *app)
                    .expect("PiP request");
            assert_eq!(request.id, "video-player.current-video");
            assert_eq!(
                request.role,
                ui::floating_surface::FloatingRole::PictureInPicture
            );
            assert_eq!(request.aspect_ratio, Some(16.0 / 9.0));
            assert!(
                matches!(request.source, ui::floating_surface::FloatingSource::AppElement { ref app_id, ref element_id, .. } if app_id == "so.tilo.video-player" && element_id == "video_surface")
            );
        }
        // Case `resources_cover_transport_navigation_audio_metadata_and_settings`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            let resources = ui::resources::resources(&app, &AutoCommit::new());
            let app_resource = &resources[0];
            assert_eq!(app_resource.kind, "video-player.application");
            let media = &app_resource.children[0];
            assert_eq!(media.kind, "media.video");
            let kinds: Vec<&str> = media
                .children
                .iter()
                .map(|child| child.kind.as_str())
                .collect();
            assert_eq!(
                kinds,
                vec![
                    "media.transport",
                    "media.navigation",
                    "media.audio",
                    "media.tracks",
                    "media.chapters",
                    "media.player-settings",
                    "media.presentation",
                ]
            );
        }
        // Case `follower_rejects_transport_operations_even_if_invoked_out_of_band`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            app.collab = Some(CollabPlayback::new(Role::Follower));
            app.player.set_transport_enabled(false);
            let encoded = tilo_actions::EncodedAction::encode_operation(
                crate::actions::OPERATION_TYPE,
                &crate::actions::PlayerOperation::TogglePlayback,
            )
            .unwrap();
            let result = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut AutoCommit::new(),
                &automerge::ROOT,
            );
            assert!(result.is_err());
        }
        // Case `unavailable_transport_is_rejected_even_if_a_caller_bypasses_discovery`.
        {
            let mut app = PlayerApp::new();
            let encoded = tilo_actions::EncodedAction::encode_operation(
                crate::actions::OPERATION_TYPE,
                &crate::actions::PlayerOperation::TogglePlayback,
            )
            .unwrap();
            let result = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut AutoCommit::new(),
                &automerge::ROOT,
            );
            assert!(result.is_err());
        }
        // Case `idle_collaboration_does_not_turn_a_paused_player_into_an_animation`.
        {
            let mut app = PlayerApp::new();
            app.collab = Some(CollabPlayback::new(Role::Host));
            assert!(Widget::needs_idle_sync(&app));
            assert!(
                !Widget::needs_animation_frame(&app),
                "collab receives idle sync without requesting a 60 Hz redraw"
            );
        }
    }

    fn root() -> ObjId {
        automerge::ROOT
    }

    /// Run the production layout passes (sync → build → compute → apply) for
    /// `app` at `viewport`, with the accessibility text scale applied the way
    /// the live host applies it.
    fn lay_out(app: PlayerApp, viewport: (f32, f32), text_scale: f32) -> Framed<PlayerApp> {
        Framed::new(
            ui::State::new(),
            app,
            ui::headless::viewport(viewport),
            Theme::dark().with_text_scale(text_scale),
        )
    }

    /// [`lay_out`] with reduced motion, so a sheet that opens is open in the
    /// first frame instead of sliding in.
    fn lay_out_still(app: PlayerApp, viewport: (f32, f32), text_scale: f32) -> Framed<PlayerApp> {
        Framed::new(
            ui::State::new(),
            app,
            ui::headless::viewport(viewport),
            Theme::dark()
                .with_text_scale(text_scale)
                .with_reduce_motion(true),
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn player_chrome_geometry_follows_the_theme_at_every_text_scale() {
        // Case `top_strip_is_one_centre_line_that_contains_its_buttons`: the
        // Library and Open buttons share the strip's centre line, sit inside
        // the fade and keep the strip margin at 100% and 200% text.
        for scale in [1.0_f32, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let app = lay_out(
                PlayerApp::for_shot(None, Some("playing")),
                (1200.0, 800.0),
                scale,
            );
            let strip = TopStrip::at(app.rect.y0, &theme);
            let margin = f64::from(theme.tokens.spacing.s4);
            let (open, library) = (app.open_button_rect, app.library_button_rect);
            assert!(
                library.width() > 0.0 && open.width() > 0.0,
                "{scale}×: both buttons are placed once painted"
            );
            assert!(
                (open.center().y - strip.center_y).abs() < 0.01
                    && (library.center().y - strip.center_y).abs() < 0.01,
                "{scale}×: Open {open:?} and Library {library:?} share the strip centre {}",
                strip.center_y
            );
            assert!(
                open.y1 <= app.rect.y0 + strip.height && library.y1 <= app.rect.y0 + strip.height,
                "{scale}×: the fade ({}) contains both buttons",
                strip.height
            );
            assert!(
                (app.rect.x1 - open.x1 - margin).abs() < 0.01
                    && (library.x0 - app.rect.x0 - margin).abs() < 0.01,
                "{scale}×: both buttons keep the strip margin"
            );
            assert!(open.x0 > library.x1, "{scale}×: the buttons do not meet");
        }
        // Case `library_sheet_stacks_header_tabs_and_rows`: the sheet is one
        // column: its header, then the tab strip across it, then the rows,
        // at 100% and 200% text, with the actions clear of one another.
        for scale in [1.0_f32, 2.0] {
            let app = lay_out_still(
                PlayerApp::for_shot(None, Some("library")),
                (1200.0, 800.0),
                scale,
            );
            let sheet = app.drawer.sidebar_rect();
            assert!(sheet.width() > 0.0, "{scale}×: the sheet is open");
            for rect in [
                app.library_close_rect,
                app.library_queue_tab_rect,
                app.library_recent_tab_rect,
                app.library_shared_tab_rect,
            ] {
                assert!(
                    sheet.contains_rect(rect) && rect.width() > 0.0,
                    "{scale}×: {rect:?} lies inside the sheet {sheet:?}"
                );
            }
            assert!(
                app.library_close_rect.y1 <= app.library_queue_tab_rect.y0,
                "{scale}×: tabs start under the header"
            );
            assert!(
                (app.library_queue_tab_rect.x1 - app.library_recent_tab_rect.x0).abs() < 0.01
                    && (app.library_recent_tab_rect.x1 - app.library_shared_tab_rect.x0).abs()
                        < 0.01,
                "{scale}×: tabs form one contiguous strip"
            );
            assert!(
                app.library_list_rect.y0 >= app.library_queue_tab_rect.y1,
                "{scale}×: rows start under the tabs"
            );
            assert_eq!(app.library_row_hits.len(), app.queue.len().min(32));
            let mut previous_bottom = app.library_list_rect.y0 - app.library_scroll;
            for row in &app.library_row_hits {
                assert!(
                    (row.row.y0 - previous_bottom).abs() < 0.01 && row.row.height() > 0.0,
                    "{scale}×: rows stack with no gap or overlap"
                );
                previous_bottom = row.row.y1;
                let remove = row.remove.expect("a queued row can be removed");
                assert!(
                    row.row.contains_rect(remove),
                    "{scale}×: the remove action sits inside its row"
                );
            }
        }
        // Case `library_sheet_follows_its_drawer`: closing the library moves
        // no hit rect into the video.
        {
            let mut app = lay_out_still(
                PlayerApp::for_shot(None, Some("library")),
                (1200.0, 800.0),
                1.0,
            );
            app.perform_resource_operation(PlayerOperation::HideLibrary);
            let theme = Theme::dark().with_reduce_motion(true);
            let mut text = TextRenderer::new();
            let mut scene = Scene::new();
            app.sync_library_sheet();
            app.drawer.tick(&theme);
            app.paint_library_panel(&mut scene, Affine::IDENTITY, &mut text, &theme);
            assert_eq!(app.library_close_rect, Rect::ZERO);
            assert!(app.library_row_hits.is_empty());
        }
        // Case `fresh_player_shows_a_clean_empty_state_with_no_frame`.
        {
            let mut app = PlayerApp::new();
            let mut doc = AutoCommit::new();
            // Pumping the app before any file is opened must not fabricate a frame.
            app.sync(&mut doc, &root());
            app.sync(&mut doc, &root());
            assert!(
                app.player.latest_frame().is_none(),
                "empty state: no picture until a file is opened"
            );
            assert_eq!(app.title, "No media");
            assert!(!app.player.is_playing());
        }
    }

    /// A player pinned to the deterministic synthesized backend, so the app's
    /// open/transport wiring is exercised hermetically regardless of build
    /// features (real `GStreamer` decode is proven separately by the
    /// `--features decode` integration test with a real generated file).
    fn synth_app() -> PlayerApp {
        let mut app = PlayerApp::new_with_preferences(PlaybackPreferences::default());
        app.player.set_engine(media::MediaEngine::new_synthesized());
        app
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn decoder_lifecycle_confirms_opens_only_on_a_real_frame() {
        // Case `opening_a_file_then_syncing_lands_a_frame_and_duration`.
        {
            let mut app = synth_app();
            app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
                .expect("source identity available");
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &root());
            assert!(
                app.player.latest_frame().is_some(),
                "a frame is produced once opened"
            );
            assert!(
                app.player.duration_ms() > 0,
                "duration mirrored from the engine"
            );
            assert!(app.player.is_playing(), "autoplay after open");
        }
        // Case `direct_open_is_accepted_until_a_decoder_frame_confirms_success`.
        {
            let mut app = synth_app();
            let path = std::env::temp_dir().join(format!(
                "tilo-video-player-open-lifecycle-{}-{}.mp4",
                std::process::id(),
                unix_now_ms()
            ));
            std::fs::write(&path, []).expect("create a portal-equivalent local file");
            let operation = PlayerOperation::Open {
                path: Some(path.to_string_lossy().into_owned()),
            };
            let payload = serde_json::to_vec(&operation).expect("serialize operation");
            let invocation = tilo_actions::InvocationId::new();
            let mut doc = AutoCommit::new();
            let started = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::OPERATION_TYPE,
                    &payload,
                ),
                &mut doc,
                &root(),
            )
            .expect("direct open admits");
            assert_eq!(
                started,
                Some(ui::widget::ResourceOperationStart::Accepted),
                "path validation is admission, not decoder completion"
            );
            assert!(
                Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root())
                    .is_none()
            );
            let mut mutable_doc = doc;
            app.sync(&mut mutable_doc, &root());
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, false, &mutable_doc, &root()),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            std::fs::remove_file(path).ok();
        }
        // Case `queued_item_uses_the_same_pending_decoder_lifecycle_as_direct_open`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            let item_id = app.queue[1].id.clone();
            let payload = serde_json::to_vec(&PlayerOperation::OpenQueueItem {
                item_id: item_id.clone(),
            })
            .expect("serialize queue operation");
            let invocation = tilo_actions::InvocationId::new();
            let mut doc = AutoCommit::new();
            assert_eq!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &root()
                )
                .expect("queue item admits"),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            assert_eq!(app.current_queue_id.as_deref(), Some(item_id.as_str()));
            assert!(
                Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root())
                    .is_none(),
                "admission is still not a terminal playback success"
            );
            app.sync(&mut doc, &root());
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root()),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            assert_eq!(
                app.recent.front().map(|entry| entry.id.as_str()),
                Some(item_id.as_str())
            );
        }
        // Case `admitted_direct_open_reports_a_decoder_timeout_instead_of_false_success`.
        {
            let mut app = synth_app();
            let path = std::env::temp_dir().join(format!(
                "tilo-video-player-open-timeout-{}-{}.mp4",
                std::process::id(),
                unix_now_ms()
            ));
            std::fs::write(&path, []).expect("create a portal-equivalent local file");
            let payload = serde_json::to_vec(&PlayerOperation::Open {
                path: Some(path.to_string_lossy().into_owned()),
            })
            .expect("serialize operation");
            let invocation = tilo_actions::InvocationId::new();
            let mut doc = AutoCommit::new();
            assert_eq!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &root()
                )
                .expect("direct open admits"),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            app.media_open
                .as_mut()
                .expect("pending decoder open")
                .deadline = ui::time::now();
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root()),
                Some(tilo_actions::InvocationUpdate::Failed { error, .. })
                    if error.code == "video_player.open_timed_out"
            ));
            assert_eq!(
                app.open_error.as_deref(),
                Some("The decoder did not produce a first frame in time.")
            );
            std::fs::remove_file(path).ok();
        }
    }

    #[test]
    fn transport_settings_reach_the_engine_and_snapshot() {
        // Case `autoplay_and_repeat_are_real_settings_not_demo_affordances`.
        {
            let mut app = synth_app();
            app.preferences.autoplay = false;
            configure_player_settings(
                &mut app.player,
                app.preferences.autoplay,
                app.preferences.loop_enabled,
            );
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            assert!(
                !app.player.is_playing(),
                "disabled autoplay is honoured on open"
            );

            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &root());
            app.preferences.loop_enabled = true;
            configure_player_settings(
                &mut app.player,
                app.preferences.autoplay,
                app.preferences.loop_enabled,
            );
            let end = app.player.duration_ms();
            app.player.seek_to_ms(end);
            app.player.pause();
            app.maybe_loop_current_media();
            assert_eq!(app.player.position_ms(), 0, "repeat restarts at the end");
            assert!(app.player.is_playing(), "repeat resumes playback");
        }
        // Case `playhead_advances_while_playing_and_a_mid_play_seek_holds`.
        {
            let mut app = synth_app();
            app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
                .expect("source identity available");
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &root()); // baseline the synth clock
            std::thread::sleep(std::time::Duration::from_millis(40));
            app.sync(&mut doc, &root());
            assert!(
                app.player.position_ms() > 0,
                "playhead advanced while playing"
            );

            // Seek mid-play: the target lands in both the widget cache and the engine.
            app.player.seek_to_ms(50_000);
            assert_eq!(app.player.position_ms(), 50_000);
            assert_eq!(app.player.engine().unwrap().position_ms(), 50_000);

            // Playback continues forward from the seek target, not back to the start.
            std::thread::sleep(std::time::Duration::from_millis(30));
            app.sync(&mut doc, &root());
            assert!(
                app.player.position_ms() >= 50_000,
                "seek stuck; playback rode on from it"
            );
        }
        // Case `volume_and_mute_propagate_to_the_engine`.
        {
            let mut app = synth_app();
            app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
                .expect("source identity available");
            app.player.set_volume(0.3);
            assert!((app.player.volume() - 0.3).abs() < 1e-6);
            assert!((app.player.engine().unwrap().volume() - 0.3).abs() < 1e-6);
            app.player.toggle_mute();
            assert!(
                (app.player.engine().unwrap().volume() - 0.0).abs() < 1e-6,
                "mute drops the engine volume to zero"
            );
        }
        // Case `playback_speed_action_changes_the_engine_and_collaborative_snapshot`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            app.perform_resource_operation(crate::actions::PlayerOperation::SetPlaybackRate(2.0));
            let engine = app.player.engine().expect("engine");
            assert!((engine.rate() - 2.0).abs() < f32::EPSILON);
            assert!((engine.transport_state(unix_now_ms()).rate - 2.0).abs() < f32::EPSILON);
        }
        // Case `space_key_routed_through_the_app_toggles_playback`.
        {
            let mut app = synth_app();
            app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
                .expect("source identity available");
            app.player.set_focus(true);
            let was_playing = app.player.is_playing();
            assert!(app.route_key_input(
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Space),
                ElementState::Pressed,
                winit::event::Modifiers::default(),
            ));
            assert_ne!(
                app.player.is_playing(),
                was_playing,
                "Space reached the focused player through the app"
            );
        }
    }

    #[test]
    fn search_provider_reads_metadata_without_reindex_or_transport() {
        // Case `repeated_search_provider_retrieval_does_not_reindex_metadata`.
        {
            let app = PlayerApp::for_shot(None, Some("metadata"));
            let first = ui::search::capability::search_providers(&app);
            assert_eq!(app.search_catalog_builds.get(), 1);
            let second = ui::search::capability::search_providers(&app);
            assert_eq!(
                app.search_catalog_builds.get(),
                1,
                "unchanged Ctrl+K opens reuse the Arc catalog"
            );
            assert_eq!(
                first[0].search("Deutsch").len(),
                second[0].search("Deutsch").len()
            );
        }
        // Case `search_reveal_operation_never_changes_transport`.
        {
            let mut app = PlayerApp::for_shot(None, None);
            app.player.pause();
            let position = app.player.position_ms();
            app.perform_resource_operation(PlayerOperation::Reveal);
            assert!(!app.player.is_playing());
            assert_eq!(app.player.position_ms(), position);
        }
    }
}
