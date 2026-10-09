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

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use ui::widget::{AppModel, ModelPath};

use taffy::style_helpers::percent;
use taffy::{NodeId, Size, Style, TaffyTree};
use tilo_clock::unix_now_ms;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

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
    Button, ButtonFace, FloatingSurfaceProvider, FullscreenProvider, SidebarDrawer, VideoPlayer,
    Widget,
};

use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectReply, EffectStatus, Effected, LaunchInput, RunMode,
    Setting, SurfaceCx,
};
use ui::tilo_resource::{ActionDecl, Id};

use crate::actions::VideoEffect;
use crate::preferences::{PlayerSettings, RecentMedia};
use crate::shared::{Library as PlayerResource, library as verbs};

/// The playback preferences the player follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlaybackPreferences {
    autoplay: bool,
    loop_enabled: bool,
}

impl Default for PlaybackPreferences {
    fn default() -> Self {
        let defaults = PlayerSettings::default();
        Self {
            autoplay: defaults.autoplay,
            loop_enabled: defaults.loop_enabled,
        }
    }
}

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
    /// The caller of the open effect, answered once a picture decodes.
    reply: Option<EffectReply>,
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

/// A picker's answer, with the request generation it answers.
type PortalOpenOutcome = (u64, Result<Picked, Option<String>>);

/// A canonical open invocation awaiting its decoder's first picture. This is
/// separate from the file-picker request: the picker can succeed while decode
/// fails, stalls, or is superseded before a frame appears.
struct PendingMediaOpen {
    reply: EffectReply,
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

/// What the picture area says when the decoder cannot play a video.
const DECODER_FAILURE: &str = "The decoder could not play this video.";

/// What the authored capture library lists beside its demo clip.
const DEMO_QUEUE: &[(&str, &str)] = &[
    ("https://demo.local/market.mp4", "Market morning"),
    ("https://demo.local/night-train.mp4", "Night train"),
];

impl LibraryTab {
    /// The tab's name in the show-library effect.
    fn name(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Recent => "recent",
            Self::Shared => "shared",
        }
    }

    fn named(name: &str) -> Option<Self> {
        [Self::Queue, Self::Recent, Self::Shared]
            .into_iter()
            .find(|tab| tab.name() == name)
    }
}

/// A player for a window built in `mode`: only a window someone uses
/// decodes with the system's engine and announces itself to the session's
/// media controls. A capture or a past version gets the deterministic
/// synthesized engine and reaches nothing outside the process.
fn configured_player(mode: RunMode, autoplay: bool, loop_enabled: bool) -> VideoPlayer {
    let engine = if mode.is_live() {
        media::MediaEngine::new()
    } else {
        media::MediaEngine::new_synthesized()
    };
    let mut player = VideoPlayer::new()
        .with_engine(engine)
        .with_test_id("video_surface");
    if mode.is_live() {
        player.enable_mpris("tilo_video_player", "Video Player");
    }
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
        Icon::Repeat1,
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
        Icon::Gauge,
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
        "Picture in picture",
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

/// The source a launch argument names: a stream's `http(s)` URL, else a
/// path.
pub fn source_for_argument(argument: &str) -> media::Source {
    if argument.starts_with("http://") || argument.starts_with("https://") {
        media::Source::Remote {
            url: argument.to_owned(),
        }
    } else {
        source_for_path(Path::new(argument))
    }
}

/// What a source is called before anyone names it: a file's name, or a
/// stream's.
fn source_title(source: &media::Source) -> String {
    match source {
        media::Source::LocalFile(path) | media::Source::Workspace(path) => path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("selected video")
            .to_string(),
        media::Source::Remote { .. } => "Streaming video".to_string(),
    }
}

/// The synthesized clip a capture plays when its launch named no video.
fn demo_source() -> media::Source {
    media::Source::Remote {
        url: "https://demo.local/clip.mp4".into(),
    }
}

/// The shareable identity of the capture's demo clip, which its scenarios'
/// shared annotations point at.
pub(crate) fn demo_media_ref() -> String {
    local_source_token(&demo_source(), 0)
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
    /// The window's History control, held in the top strip: the strip is this
    /// app's header row, so the window draws no bar above the picture.
    history: ui::widget::HistoryOpener,
    /// Where the History control sits in the strip.
    history_button_rect: Rect,
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
    open_focused: bool,
    library_focused: bool,
    library_focus_id: Option<String>,
    /// Playback watched together with everyone who has the same video open.
    together: Together,
    /// The playback preferences the player follows.
    preferences: PlaybackPreferences,
    /// This device's settings, where a live window keeps its preferences
    /// and recent videos; `None` for a capture or a past version.
    setting: Option<Setting<PlayerSettings>>,
    /// Deterministic decoder metadata used only by the explicit headless
    /// `metadata` screenshot fixture. Live runs always read the active engine.
    shot_metadata: Option<DecoderMetadata>,
    /// Deterministically holds the production no-frame/loading presentation
    /// for visual regression capture while the synthesized engine stays
    /// hermetic. Live runs always derive loading from decoder frame state.
    shot_loading: bool,
    pending_pip: Option<ui::floating_surface::FloatingSurfaceRequest>,
    /// Where the shared library's edits and the player's own controls go:
    /// the framework's door, handed over when the surface is attached.
    dispatcher: Option<Dispatcher<PlayerResource>>,
    /// The shared library as last presented.
    shared_library: crate::shared::Library,
    /// What the window is built for: only a live one starts a decoder
    /// service, media controls or collaboration.
    mode: RunMode,
    /// What a capture waits for before its frame is taken.
    capture_awaits: CaptureAwaits,
    /// The video a capture's launch named, played once the scenario stages.
    pub(crate) capture_launched: Option<String>,
    /// Opaque source identity for durable shared playlists and markers. It
    /// survives reopening the same local media, unlike the per-open source ID.
    current_media_ref: Option<String>,
    current_source_id: Option<String>,
    source_generation: u64,
    /// The decoder metadata the settings menu shows.
    shown_metadata: DecoderMetadata,
    /// A human-readable failure from the latest open request. It is only
    /// painted in the no-frame state, preserving a successfully playing video
    /// when a later picker request fails.
    open_error: Option<String>,
    missing_codec: Option<String>,
    formats_focused: bool,
    formats_label: String,
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

/// What a capture of the player waits for before it is taken: a decoded
/// picture of what it plays, or what became of opening it (a picture or the
/// decoder's error).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureAwaits {
    Nothing,
    Frame,
    Outcome,
}

/// How far the playhead here may stray from where it stands for everyone
/// before this player says it moved it: past the drift a follower corrects
/// on its own, so following never echoes back as a move.
const TOGETHER_MOVE_MS: u64 = 1_500;
/// How long after following a peer's move this player waits before it says
/// anything itself, while its engine catches up.
const TOGETHER_SETTLE_MS: u64 = 1_500;
/// How long a paused shared transport stays something to follow: a video
/// left paused long ago starts on its own when it is opened again.
const TOGETHER_PAUSE_LIVES_MS: u64 = 10 * 60 * 1000;

/// What watching together does this frame.
#[derive(Debug, PartialEq)]
enum TogetherStep {
    /// Bring the engine to where playback stands for everyone.
    Follow(media::TransportState),
    /// Say where playback stands here: this player moved it.
    Publish(media::TransportState),
    Stay,
}

/// What this player last knew of playback watched together.
#[derive(Default)]
struct Together {
    /// The video it concerns.
    media_ref: Option<String>,
    /// The shared transport last seen, to tell a new one from an old one.
    seen: Option<crate::shared::SharedTransport>,
    /// What this player itself said last, to know its own change when it
    /// comes back.
    said: Option<media::TransportState>,
    /// Until when this player only follows, after it followed a move.
    settling_until: u64,
    /// Whether what was seen is over (left paused long ago, run past the
    /// end): nothing to follow or to compare with.
    over: bool,
    /// Whether what was seen is this player's own: it leads, so there is
    /// nothing to ride along with. Correcting its engine toward its own
    /// announcement would seek it back every frame while it starts playing
    /// or resumes, before its playhead moves.
    leading: bool,
}

impl Together {
    /// Decide this frame's step for the video `media_ref`, given where
    /// playback stands for everyone (`shared`) and here (`local`), at `now`.
    fn step(
        &mut self,
        media_ref: &str,
        shared: Option<&crate::shared::SharedTransport>,
        local: media::TransportState,
        now: u64,
        duration_ms: u64,
    ) -> TogetherStep {
        if self.media_ref.as_deref() != Some(media_ref) {
            // A video just opened: what stood before is the past unless it
            // is still running, or was paused a moment ago.
            let over = shared.filter(|shared| !Self::current(shared, now, duration_ms));
            *self = Self {
                media_ref: Some(media_ref.to_owned()),
                seen: over.cloned(),
                over: over.is_some(),
                ..Self::default()
            };
        }
        if shared != self.seen.as_ref() {
            self.seen = shared.cloned();
            self.over = false;
            self.leading = false;
            if let Some(shared) = shared {
                let state = shared.state();
                if self.said.is_some_and(|said| same(said, state)) {
                    // This player's own change, come back.
                    self.leading = true;
                    return TogetherStep::Stay;
                }
                self.settling_until = now.saturating_add(TOGETHER_SETTLE_MS);
                return TogetherStep::Follow(state);
            }
        }
        if now < self.settling_until {
            return match self.seen.as_ref() {
                Some(shared) => TogetherStep::Follow(shared.state()),
                None => TogetherStep::Stay,
            };
        }
        let joined = self.seen.as_ref().filter(|_| !self.over);
        let moved = match joined.map(crate::shared::SharedTransport::state) {
            None => local.playing,
            Some(shared) => {
                shared.playing != local.playing
                    || (shared.rate - local.rate).abs() > f32::EPSILON
                    || shared.projected_position(now).abs_diff(local.position_ms) > TOGETHER_MOVE_MS
            }
        };
        if moved {
            self.said = Some(local);
            return TogetherStep::Publish(local);
        }
        match joined {
            // Ride along with a peer's playback: small drift rides, the
            // engine corrects larger drift.
            Some(shared) if shared.playing && !self.leading => TogetherStep::Follow(shared.state()),
            _ => TogetherStep::Stay,
        }
    }

    /// Whether `shared` is still something to join: running inside the
    /// video, or paused a moment ago.
    fn current(shared: &crate::shared::SharedTransport, now: u64, duration_ms: u64) -> bool {
        if shared.playing {
            duration_ms == 0 || shared.state().projected_position(now) < duration_ms
        } else {
            now.saturating_sub(shared.epoch_ms) < TOGETHER_PAUSE_LIVES_MS
        }
    }
}

/// Whether two transports say the same, as the document stores them.
fn same(a: media::TransportState, b: media::TransportState) -> bool {
    a.playing == b.playing
        && a.position_ms == b.position_ms
        && a.epoch_ms == b.epoch_ms
        && (a.rate - b.rate).abs() < f32::EPSILON
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

impl PlayerApp {
    /// What the player shows that the document does not hold, for its live
    /// rows.
    pub(crate) fn player_rows(&self) -> crate::actions::PlayerRows<'_> {
        crate::actions::PlayerRows {
            title: self.player.has_media().then_some(self.title.as_str()),
            source_id: self.current_source_id.as_deref(),
            audio_tracks: &self.shown_metadata.audio_tracks,
            subtitle_tracks: &self.shown_metadata.subtitle_tracks,
            chapters: &self.shown_metadata.chapters,
            queue: self.library_items(self.queue.iter()),
            recent: self.library_items(self.recent.iter()),
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
        if self.shown_metadata == metadata {
            return;
        }
        self.shown_metadata = metadata;
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
                Icon::Headphones,
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
                Icon::MessageSquare,
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

    /// A player with a fresh engine and no media loaded, for a window built
    /// in `mode`, on the default preferences until a live window reads this
    /// device's settings when it is attached.
    pub fn new(mode: RunMode) -> Self {
        // Tests exercise the real request and completion paths; a recording
        // host answers every picker so no desktop file chooser ever opens.
        #[cfg(test)]
        ui::host::install_default(ui::host::HostServices::recording().0);
        let preferences = PlaybackPreferences::default();
        let player = configured_player(mode, preferences.autoplay, preferences.loop_enabled);
        Self::from_prepared(player, preferences, VecDeque::new(), mode)
    }

    fn from_prepared(
        player: VideoPlayer,
        preferences: PlaybackPreferences,
        recent: VecDeque<LocalMediaEntry>,
        mode: RunMode,
    ) -> Self {
        let kept_retained = recent
            .iter()
            .filter_map(|entry| crate::kept::token_of(&entry.source).map(str::to_owned))
            .collect();
        Self {
            player,
            title: "No media".to_string(),
            rect: Rect::ZERO,
            open_button_rect: Rect::ZERO,
            library_button_rect: Rect::ZERO,
            history: ui::widget::HistoryOpener::new(),
            history_button_rect: Rect::ZERO,
            status_action_rect: Rect::ZERO,
            drawer: SidebarDrawer::new().right().closed().overlay_only(),
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
            open_focused: false,
            library_focused: false,
            library_focus_id: None,
            together: Together::default(),
            preferences,
            setting: None,
            shot_metadata: None,
            shot_loading: false,
            pending_pip: None,
            dispatcher: None,
            shared_library: crate::shared::Library::default(),
            mode,
            capture_awaits: CaptureAwaits::Nothing,
            capture_launched: None,
            current_media_ref: None,
            current_source_id: None,
            source_generation: 0,
            shown_metadata: DecoderMetadata::default(),
            open_error: None,
            missing_codec: None,
            formats_focused: false,
            formats_label: crate::i18n::t("video-formats-title"),
            kept: HashMap::new(),
            kept_retained,
            kept_recall: None,
            kept_wake: ui::wake::WakeHandle::new(),
            dirty: true,
        }
    }

    /// Open what the launch named: a path or a stream's URL, continuing the
    /// playback another device handed over when it says where that stood.
    /// The handoff's position and play state land after the open, so local
    /// autoplay preferences cannot override them.
    pub fn open_launch(&mut self, launch: &LaunchInput<'_>) {
        let Some(argument) = launch.argument() else {
            return;
        };
        let handoff = launch.handoff();
        let source = source_for_argument(argument);
        let title = handoff
            .and_then(|handoff| handoff.title.clone())
            .unwrap_or_else(|| source_title(&source));
        let queue_id = self.ensure_queued(source.clone(), title.clone());
        if let Err(error) = self.open_local_source(source, title, Some(queue_id)) {
            tracing::warn!(%error, "video player: could not open the launch's video");
            return;
        }
        if let Some(handoff) = handoff {
            self.player.seek_to_ms(handoff.position_ms);
            if handoff.resume_playing {
                self.player.play();
            } else {
                self.player.pause();
            }
        }
    }

    /// Stage a capture of `scenario` (`None` for a plain `--shots`): the
    /// video the launch named, else a synthesized demo clip, the scenario's
    /// view state, and what the capture waits for before it is taken.
    pub(crate) fn stage_capture(&mut self, launched: Option<&str>, scenario: Option<&str>) {
        self.stage_shot_source(launched, scenario);
        self.stage_shot_scenario(scenario);
        if !matches!(scenario, Some("empty" | "error" | "missing-codec")) {
            self.player.reveal_controls();
        }
    }

    /// A player staged as a capture of `scenario` over the demo clip.
    #[cfg(test)]
    pub(crate) fn for_shot_open(launch: Option<&str>, scenario: Option<&str>) -> Self {
        let mut app = Self::new(RunMode::Capture);
        app.stage_capture(launch, scenario);
        app
    }

    #[cfg(test)]
    pub(crate) fn for_shot(scenario: Option<&str>) -> Self {
        Self::for_shot_open(None, scenario)
    }

    fn stage_shot_source(&mut self, launched: Option<&str>, scenario: Option<&str>) {
        if matches!(scenario, Some("empty" | "error" | "missing-codec")) {
            return;
        }
        if let Some(argument) = launched {
            // A capture of a real file shows that file decoded, never the
            // synthesized stand-in: without the system decoder it waits for
            // a picture that cannot come and refuses to capture.
            let engine = media::MediaEngine::new();
            if !engine.is_real_decode() {
                self.open_error =
                    Some("A real video capture requires the GStreamer decoder and runtime".into());
                self.capture_awaits = CaptureAwaits::Frame;
                return;
            }
            self.player.set_engine(engine);
            let source = source_for_argument(argument);
            let title = source_title(&source);
            let queue_id = self.ensure_queued(source.clone(), title.clone());
            let _ = self.open_local_source(source, title, Some(queue_id));
            self.capture_awaits = match scenario {
                Some("native-error") => CaptureAwaits::Outcome,
                Some("loading") => CaptureAwaits::Nothing,
                _ => CaptureAwaits::Frame,
            };
            return;
        }
        // The demo stays hermetic: the synthesized engine draws a
        // deterministic frame and the placeholder host is never contacted.
        let source = demo_source();
        let queue_id = self.ensure_queued(source.clone(), "Demo clip".into());
        let _ = self.set_current_source(&source);
        self.player.open(source);
        self.title = "Demo clip".to_string();
        self.current_queue_id = Some(queue_id);
        // Park at a fixed position for a stable reference frame. The
        // explicit `playing` fixture starts the engine below; default and
        // `paused` deliberately stay deterministic.
        self.player.seek_to_ms(8_000);
        self.player.pause();
        if scenario != Some("loading") {
            self.capture_awaits = CaptureAwaits::Frame;
        }
    }

    /// Whether a capture still waits for what it shows.
    fn capture_pending(&self) -> bool {
        let pictured = self.player.latest_frame().is_some();
        match self.capture_awaits {
            CaptureAwaits::Nothing => false,
            CaptureAwaits::Frame => !pictured,
            CaptureAwaits::Outcome => !pictured && self.open_error.is_none(),
        }
    }

    fn stage_shot_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            // Nothing opened: the open itself failed.
            Some("error") => self.fail_playback(),
            Some("missing-codec") => {
                self.fail_playback();
                self.missing_codec = Some("HEVC".into());
            }
            // A video opened and the decoder then gave up on it: the title
            // and the strip stay, the picture is the failure.
            Some("native-error") => {
                self.shot_loading = true;
                self.fail_playback();
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
            Some("library" | "recent" | "library-focus") => {
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
            Some("shared") => {
                self.library_visible = true;
                self.library_tab = LibraryTab::Shared;
            }
            Some("fullscreen") => self.player.toggle_fullscreen(),
            _ => {}
        }
    }

    /// Show the shared library the document holds.
    pub(crate) fn present_library(&mut self, state: &crate::shared::Library) {
        if state != &self.shared_library {
            self.shared_library = state.clone();
            self.dirty = true;
        }
    }

    /// Take part in the framework: its door for edits and controls, and in
    /// a live window this device's settings and what the launch named. A
    /// capture keeps what its launch named for the scenario to stage.
    pub(crate) fn attach_framework(&mut self, cx: &SurfaceCx<PlayerResource>) {
        self.dispatcher = Some(cx.dispatcher());
        match cx.mode() {
            RunMode::Live => {
                let setting = cx.settings::<PlayerSettings>();
                self.adopt_settings(&setting.get());
                self.setting = Some(setting);
                self.open_launch(&cx.launch());
            }
            RunMode::Capture => {
                self.capture_launched = cx.launch().argument().map(str::to_owned);
            }
            RunMode::Past => {}
        }
    }

    /// Follow this device's stored preferences and recent videos.
    fn adopt_settings(&mut self, stored: &PlayerSettings) {
        self.preferences = PlaybackPreferences {
            autoplay: stored.autoplay,
            loop_enabled: stored.loop_enabled,
        };
        configure_player_settings(
            &mut self.player,
            self.preferences.autoplay,
            self.preferences.loop_enabled,
        );
        self.recent = stored
            .recent_history
            .iter()
            .map(|entry| LocalMediaEntry {
                id: entry.id.clone(),
                title: entry.title.clone(),
                source: entry.source.clone(),
            })
            .collect();
        self.kept_retained = self
            .recent
            .iter()
            .filter_map(|entry| crate::kept::token_of(&entry.source).map(str::to_owned))
            .collect();
        self.dirty = true;
    }

    /// The open video's shareable identity, which the shared library's
    /// notes and transport name it by; never a path.
    pub fn media_ref(&self) -> Option<&str> {
        self.current_media_ref.as_deref()
    }

    /// Whether the library panel shows.
    pub fn library_visible(&self) -> bool {
        self.library_visible
    }

    /// The library tab shown or last shown: `queue`, `recent` or `shared`.
    pub fn library_tab_name(&self) -> &'static str {
        self.library_tab.name()
    }

    /// Shared read access to the inner video surface, for tests that assert
    /// a decoded frame landed.
    pub fn player(&self) -> &VideoPlayer {
        &self.player
    }

    /// Whether a decoded picture is currently on the surface. The `--shots`
    /// path pumps `sync` until this is true so a real (asynchronously prerolled)
    /// decode is captured, not a blank frame.
    pub fn has_media_frame(&self) -> bool {
        self.player.latest_frame().is_some()
    }

    /// The picture area says the decoder could not play the video. The
    /// decoder's own wording is for logs and agents; the person is told what
    /// it means for them, the same way wherever a decode fails.
    fn fail_playback(&mut self) {
        self.open_error = Some(DECODER_FAILURE.into());
        self.dirty = true;
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        let source = source_for_path(path);
        let title = source_title(&source);
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
        self.missing_codec = None;
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
        self.fail_open("a newer video open replaced this decoder request".into());
        self.open_error = None;
        self.missing_codec = None;
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
        match &self.setting {
            Some(setting) => setting.update(|stored| stored.recent_history = entries),
            None => Ok(()),
        }
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

    fn close_media(&mut self) {
        self.missing_codec = None;
        self.fail_open("video was closed before decoding a first frame".into());
        let player = configured_player(
            self.mode,
            self.preferences.autoplay,
            self.preferences.loop_enabled,
        );
        self.player = player;
        self.title = "No media".into();
        self.current_source_id = None;
        self.current_media_ref = None;
        self.current_queue_id = None;
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

    /// Invoke `effect` through the framework's door, as the palette and
    /// agents do: its availability and permission are checked there.
    fn request(&self, effect: &VideoEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    fn formats_visible(&self) -> bool {
        self.missing_codec.is_some() && self.open_error.is_some()
    }

    fn formats_access_index(&self) -> Option<usize> {
        self.accessibility()?
            .children
            .iter()
            .position(|node| node.action_target.as_deref() == Some("video-player.formats"))
    }

    fn focus_player_control(&mut self, path: &[usize]) -> bool {
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

    fn request_status_action(&self) {
        if self.formats_visible() {
            self.request(&VideoEffect::OpenFormats);
        } else {
            self.request_open_action();
        }
    }

    fn request_open_action(&self) {
        self.request(&VideoEffect::OpenVideo { path: None });
    }

    fn request_library_play(&self, item_id: &str) {
        let resource = if self.queue.iter().any(|item| item.id == item_id) {
            crate::actions::queue_item_resource_id(item_id)
        } else {
            crate::actions::recent_item_resource_id(item_id)
        };
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect_on(
                &resource,
                &VideoEffect::PlayQueued {
                    item: item_id.to_owned(),
                },
            );
        }
    }

    fn request_library_remove(&self, item_id: &str) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect_on(
                &crate::actions::queue_item_resource_id(item_id),
                &VideoEffect::RemoveQueued {
                    item: item_id.to_owned(),
                },
            );
        }
    }

    fn request_library_clear(&self) {
        self.request(&VideoEffect::ClearRecent);
    }

    fn request_library_hide(&self) {
        self.request(&VideoEffect::HideLibrary);
    }

    fn request_library_tab(&self, tab: LibraryTab) {
        self.request(&VideoEffect::ShowLibrary {
            tab: tab.name().to_owned(),
        });
    }

    /// The Library button and `L` shortcut choose a concrete, replay-safe
    /// effect from the current presentation. Nothing is a toggle: a replay
    /// or delayed agent invocation must not invert a newer user choice.
    fn request_library_visibility_change(&self) {
        if self.library_visible {
            self.request_library_hide();
        } else {
            self.request_library_tab(self.library_tab);
        }
    }

    fn request_annotation_jump(&self, annotation_id: &str) {
        self.request(&VideoEffect::JumpToAnnotation {
            annotation: annotation_id.to_owned(),
        });
    }

    /// Submit a shared library verb or field edit through the framework's
    /// door. The library shown follows once the document presents the
    /// change.
    fn dispatch_shared(&self, submit: impl ui::framework::Submit<PlayerResource>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(submit);
        }
    }

    fn delete_annotation(&self, id: &str) {
        self.dispatch_shared(PlayerResource::DELETE_ANNOTATION.with(Id::from_raw(id)));
    }

    /// Delete playlist `id` with its videos.
    fn delete_playlist(&self, id: &str) {
        self.dispatch_shared(PlayerResource::DELETE_PLAYLIST.with(Id::from_raw(id)));
    }

    /// The open video, added to the shared Watch later playlist; `None`
    /// without an open video or when it is already there.
    fn watch_later(&self) -> Option<verbs::AddToWatchLater> {
        let media_ref = self.current_media_ref.clone()?;
        (!self.shared_library.on_watch_later(&media_ref)).then(|| verbs::AddToWatchLater {
            media_ref,
            title: self.title.clone(),
        })
    }

    fn add_current_to_watch_later(&self) {
        if let Some(add) = self.watch_later() {
            self.dispatch_shared(PlayerResource::ADD_TO_WATCH_LATER.with(add));
        }
    }

    /// A bookmark of the moment the player shows, on the open video.
    fn bookmark(&self) -> Option<verbs::AddAnnotation> {
        Some(verbs::AddAnnotation {
            media_ref: self.current_media_ref.clone()?,
            time_ms: self.player.position_ms(),
            text: None,
        })
    }

    fn bookmark_current_time(&self) {
        if let Some(bookmark) = self.bookmark() {
            self.dispatch_shared(PlayerResource::ADD_ANNOTATION.with(bookmark));
        }
    }

    /// The shared library verbs bound to the playhead, for the palette and
    /// agents: bookmark the moment shown, add the open video to Watch later.
    pub(crate) fn playhead_declarations(&self) -> Vec<ActionDecl<PlayerResource>> {
        let Some(bookmark) = self.bookmark() else {
            return Vec::new();
        };
        let paths = verbs::AddAnnotation::paths();
        let mut declarations: Vec<ActionDecl<PlayerResource>> = PlayerResource::ADD_ANNOTATION
            .bind(paths.media_ref, bookmark.media_ref)
            .and(paths.time_ms, bookmark.time_ms)
            .declaration("bookmark-current-time", "Bookmark this moment")
            .into_iter()
            .map(|decl| decl.describe("Add a shared bookmark at the playback time shown."))
            .collect();
        if let Some(add) = self.watch_later() {
            let paths = verbs::AddToWatchLater::paths();
            declarations.extend(
                PlayerResource::ADD_TO_WATCH_LATER
                    .bind(paths.media_ref, add.media_ref)
                    .and(paths.title, add.title)
                    .declaration(
                        "add-this-video-to-watch-later",
                        "Add this video to Watch later",
                    )
                    .map(|decl| {
                        decl.describe("Add the open video to the shared Watch later playlist.")
                    }),
            );
        }
        declarations
    }

    /// Kick off a file-picker open, answering `reply` once a picture of the
    /// chosen video decodes. A second request is refused rather than
    /// replacing the first, which would make its answer race.
    fn trigger_open(&mut self, reply: Option<EffectReply>) -> Result<(), String> {
        if self.portal_open.is_some() {
            return Err("a video picker is already open".into());
        }
        let generation = self.portal_generation.checked_add(1).ok_or_else(|| {
            "video-picker request generation exhausted; restart Video Player".to_string()
        })?;
        self.portal_generation = generation;
        self.open_error = None;
        self.missing_codec = None;
        self.portal_open = Some(PendingPortalOpen {
            channel: if tilo_paths::availability(tilo_paths::Feature::NativePaths)
                == tilo_paths::Availability::Available
            {
                PickerChannel::Path(video_open_request().send())
            } else {
                PickerChannel::Contents(video_open_request().send_contents())
            },
            generation,
            reply,
        });
        self.dirty = true;
        Ok(())
    }

    /// Drop a picker receiver that no longer represents the user's intent.
    /// The picker may still answer, but nothing receives it.
    fn cancel_pending_portal(&mut self, message: &str) {
        let Some(pending) = self.portal_open.take() else {
            return;
        };
        if let Some(reply) = pending.reply {
            reply.finish(Err(message.into()));
        }
    }

    /// Answer `reply` once the open video's first picture decodes, or with
    /// why it did not.
    fn await_open_frame(&mut self, reply: EffectReply) {
        if let Some(previous) = self.media_open.take() {
            previous
                .reply
                .finish(Err("a newer video open replaced this one".into()));
        }
        let Some(source_id) = self.current_source_id.clone() else {
            reply.finish(Err("the video did not open".into()));
            return;
        };
        self.media_open = Some(PendingMediaOpen {
            reply,
            source_id,
            deadline: ui::time::now() + OPEN_FRAME_TIMEOUT,
        });
    }

    /// Answer the pending open: a picture of its video decoded, another
    /// video replaced it, or the decoder ran out of time.
    fn poll_open_frame(&mut self) {
        let Some(pending) = self.media_open.as_ref() else {
            return;
        };
        let outcome = if self.current_source_id.as_deref() != Some(pending.source_id.as_str()) {
            Some(Err(
                "The requested video was superseded before decoding a frame.".to_string(),
            ))
        } else if self.player.latest_frame().is_some() {
            self.record_current_history();
            Some(Ok(()))
        } else if ui::time::now() >= pending.deadline {
            let message = "The decoder did not produce a first frame in time.".to_string();
            self.open_error = Some(message.clone());
            self.dirty = true;
            Some(Err(message))
        } else {
            None
        };
        if let Some(outcome) = outcome
            && let Some(pending) = self.media_open.take()
        {
            pending
                .reply
                .finish(outcome.map(|()| ui::tilo_actions::ActionOutput::default()));
        }
    }

    /// Fail the pending open with the decoder's `message`.
    fn fail_open(&mut self, message: String) {
        if let Some(pending) = self.media_open.take() {
            pending.reply.finish(Err(message));
        }
    }

    /// Answer `effect`, invoked by a control, the palette, an agent or a
    /// script. Opening answers once a picture decodes.
    pub(crate) fn answer_effect(
        &mut self,
        effect: VideoEffect,
        cx: &mut EffectCx<PlayerResource, Self>,
    ) -> EffectAnswer {
        if effect.requires_media() && !self.player.has_media() {
            return Err("open a video before using this playback control".into());
        }
        match effect {
            VideoEffect::OpenVideo { path: None } => {
                self.trigger_open(Some(cx.reply()))?;
                Ok(Effected::Pending)
            }
            VideoEffect::OpenVideo { path: Some(path) } => {
                self.open_named(&path)?;
                self.await_open_frame(cx.reply());
                Ok(Effected::Pending)
            }
            VideoEffect::PlayQueued { item } => {
                self.open_local_entry(&item)?;
                self.await_open_frame(cx.reply());
                Ok(Effected::Pending)
            }
            effect => self.perform(effect).map(|()| Effected::done()),
        }
    }

    /// Open the video `path` names on this device: a file, or a stream's URL.
    fn open_named(&mut self, path: &str) -> Result<(), String> {
        self.missing_codec = None;
        let source = source_for_argument(path);
        if let media::Source::LocalFile(file) | media::Source::Workspace(file) = &source
            && !file.is_file()
        {
            let message = format!("Video does not exist: {}", source_title(&source));
            self.open_error = Some(message.clone());
            self.dirty = true;
            return Err(message);
        }
        let title = source_title(&source);
        let queue_id = self.ensure_queued(source.clone(), title.clone());
        self.open_local_source(source, title, Some(queue_id))
    }

    /// What differs from the effects' declarations right now: playback
    /// controls need an open video, and clearing needs a recent one.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if !self.formats_visible() {
            statuses.push(EffectStatus::unavailable(
                VideoEffect::OPEN_FORMATS,
                "no_missing_decoder",
                crate::i18n::t("video-formats-unavailable"),
            ));
        }
        if !self.player.has_media() {
            for effect in [
                VideoEffect::CLOSE.id,
                VideoEffect::TOGGLE_PLAYBACK.id,
                VideoEffect::TOGGLE_MUTE.id,
                VideoEffect::REVEAL.id,
                VideoEffect::SEEK.id,
                VideoEffect::SEEK_BY.id,
                VideoEffect::RESTART.id,
                VideoEffect::JUMP_TO_END.id,
                VideoEffect::SET_VOLUME.id,
                VideoEffect::SET_PLAYBACK_RATE.id,
                VideoEffect::SUBTITLES_OFF.id,
                VideoEffect::PICTURE_IN_PICTURE.id,
            ] {
                statuses.push(EffectStatus::unavailable(
                    ui::tilo_resource::ActionRef::<VideoEffect, ()>::new(effect, &[]),
                    "no_media",
                    "Open a video first",
                ));
            }
        }
        if self.recent.is_empty() {
            statuses.push(EffectStatus::unavailable(
                VideoEffect::CLEAR_RECENT,
                "no_recent",
                "No video has been played on this device",
            ));
        }
        statuses
    }

    /// Do `effect` now; refused when its target changed since it was
    /// offered.
    #[allow(
        clippy::too_many_lines,
        reason = "all effects stay together so their local semantics cannot drift"
    )]
    fn perform(&mut self, effect: VideoEffect) -> Result<(), String> {
        match effect {
            VideoEffect::OpenFormats => {
                if !self.formats_visible() {
                    return Err(crate::i18n::t("video-formats-unavailable"));
                }
                ui::portal::open_uri_detached("settings://updates");
            }
            VideoEffect::OpenVideo { path: Some(path) } => self.open_named(&path)?,
            VideoEffect::OpenVideo { path: None } => self.trigger_open(None)?,
            VideoEffect::PlayQueued { item } => self.open_local_entry(&item)?,
            VideoEffect::RemoveQueued { item } => self.remove_queue_entry(&item)?,
            VideoEffect::ClearRecent => self.clear_recent_history()?,
            VideoEffect::ShowLibrary { tab } => {
                self.library_tab = LibraryTab::named(&tab)
                    .ok_or_else(|| format!("no library tab `{tab}`: queue, recent or shared"))?;
                self.library_visible = true;
                self.player.reveal_controls();
            }
            VideoEffect::HideLibrary => {
                self.library_visible = false;
                self.library_focus_id = None;
            }
            VideoEffect::Close => self.close_media(),
            VideoEffect::TogglePlayback => self.player.toggle_play(),
            VideoEffect::ToggleMute => self.player.toggle_mute(),
            VideoEffect::Reveal => self.player.reveal_controls(),
            VideoEffect::Seek { seconds } => {
                self.player.seek_to_ms((seconds.max(0.0) * 1000.0) as u64);
            }
            VideoEffect::SeekBy { seconds } => {
                let current = self.player.position_ms() as f64;
                let duration = self.player.duration_ms() as f64;
                let target =
                    (current + seconds.clamp(-3600.0, 3600.0) * 1000.0).clamp(0.0, duration) as u64;
                self.player.seek_to_ms(target);
            }
            VideoEffect::Restart => self.player.seek_to_ms(0),
            VideoEffect::JumpToEnd => self.player.seek_to_ms(self.player.duration_ms()),
            VideoEffect::SetVolume { volume } => {
                self.player.set_volume(volume.clamp(0.0, 1.0) as f32);
            }
            VideoEffect::SetAutoplay { enabled } => {
                if self.preferences.autoplay != enabled {
                    self.preferences.autoplay = enabled;
                    self.preferences_changed();
                }
            }
            VideoEffect::SetLoop { enabled } => {
                if self.preferences.loop_enabled != enabled {
                    self.preferences.loop_enabled = enabled;
                    self.preferences_changed();
                }
            }
            VideoEffect::SetPlaybackRate { rate } => {
                let rate = rate.clamp(0.25, 4.0);
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
            VideoEffect::SelectAudioTrack { track } => {
                let index = self
                    .track_named(&track, true)
                    .ok_or("the audio track changed; refresh and try again")?
                    .index;
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_audio_track(index))
                {
                    return Err("the decoder refused that audio track".into());
                }
            }
            VideoEffect::SelectSubtitles { track } => {
                let index = self
                    .track_named(&track, false)
                    .ok_or("the subtitle track changed; refresh and try again")?
                    .index;
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_subtitle_track(Some(index)))
                {
                    return Err("the decoder refused that subtitle track".into());
                }
            }
            VideoEffect::SubtitlesOff => {
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_subtitle_track(None))
                {
                    return Err("the decoder refused to turn subtitles off".into());
                }
            }
            VideoEffect::JumpToChapter { chapter } => {
                let source_id = self.current_source_id.clone().unwrap_or_default();
                let seconds = self
                    .shown_metadata
                    .chapters
                    .iter()
                    .find(|candidate| {
                        crate::actions::chapter_resource_id(&source_id, &candidate.id) == chapter
                    })
                    .ok_or("the chapter changed; refresh and try again")?
                    .seconds;
                self.player.seek_to_ms((seconds.max(0.0) * 1000.0) as u64);
            }
            VideoEffect::JumpToAnnotation { annotation } => {
                let time_ms = self
                    .shared_library
                    .annotations
                    .get(&Id::from_raw(annotation))
                    .ok_or("the review note is gone")?;
                if self.current_media_ref.as_deref() != Some(time_ms.media_ref.as_str()) {
                    return Err("this review note belongs to a different video".into());
                }
                let time_ms = time_ms.time_ms;
                self.player.seek_to_ms(time_ms);
                self.player.reveal_controls();
            }
            VideoEffect::PictureInPicture => self.request_picture_in_picture(),
            VideoEffect::ToggleFullscreen => self.player.toggle_fullscreen(),
        }
        self.dirty = true;
        Ok(())
    }

    /// The decoder's audio (`audio`) or subtitle track a row id names, for
    /// the open source.
    fn track_named(&self, row: &str, audio: bool) -> Option<&crate::actions::TrackState> {
        let source_id = self.current_source_id.as_deref()?;
        let tracks = if audio {
            &self.shown_metadata.audio_tracks
        } else {
            &self.shown_metadata.subtitle_tracks
        };
        tracks.iter().find(|track| {
            let id = if audio {
                crate::actions::audio_resource_id(source_id, &track.stream_id)
            } else {
                crate::actions::subtitle_resource_id(source_id, &track.stream_id)
            };
            id == row
        })
    }

    /// Show and keep the preferences just changed.
    fn preferences_changed(&mut self) {
        configure_player_settings(
            &mut self.player,
            self.preferences.autoplay,
            self.preferences.loop_enabled,
        );
        if let Some(setting) = &self.setting {
            let preferences = self.preferences;
            // A device-local preference must never prevent playback; the
            // next change retries the write.
            if let Err(error) = setting.update(|stored| {
                stored.autoplay = preferences.autoplay;
                stored.loop_enabled = preferences.loop_enabled;
            }) {
                tracing::warn!(%error, "video player: could not keep playback preferences");
            }
        }
    }

    /// Drain a pending picker answer, opening the chosen file.
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
                Ok(result) => outcome = Some((request.generation, result)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    outcome = Some((
                        request.generation,
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
        let reply = pending.and_then(|pending| pending.reply);
        // The receiver is dropped before opening the chosen file, so the
        // open cannot cancel the request it completes.
        let Some((generation, outcome)) = outcome else {
            return;
        };
        if generation != self.portal_generation {
            tracing::warn!(
                generation,
                current_generation = self.portal_generation,
                "video player: ignored a stale portal outcome"
            );
            if let Some(reply) = reply {
                reply.finish(Err(
                    "ignored a stale video picker response; open again".into()
                ));
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
        let Some(reply) = reply else {
            return;
        };
        match result {
            // Picking is not decoding: answer once a picture arrives.
            Ok(true) => self.await_open_frame(reply),
            Ok(false) => reply.finish(Err("video selection was cancelled".into())),
            Err(message) => reply.finish(Err(message)),
        }
    }

    /// Watch together: follow where playback of the open video stands for
    /// everyone, and say where it stands here when this player moved it
    /// (played, paused, sought, changed speed). Only a live window with the
    /// right to edit says so; a viewer follows.
    fn tick_together(&mut self) {
        if !self.mode.is_live() {
            return;
        }
        let (Some(media_ref), Some(dispatcher)) =
            (self.current_media_ref.clone(), self.dispatcher.clone())
        else {
            return;
        };
        let now = unix_now_ms();
        let shared = self.shared_library.transport_for(&media_ref).cloned();
        let Some(engine) = self.player.engine_mut() else {
            return;
        };
        let local = engine.transport_state(now);
        let step = self.together.step(
            &media_ref,
            shared.as_ref(),
            local,
            now,
            engine.duration_ms().unwrap_or(0),
        );
        match step {
            TogetherStep::Follow(state) => {
                engine.apply_transport(&state, now);
                self.dirty = true;
            }
            TogetherStep::Publish(state) if !dispatcher.read_only() => {
                dispatcher.submit(PlayerResource::SET_TRANSPORT.with(verbs::SetTransport {
                    media_ref,
                    playing: state.playing,
                    position_ms: state.position_ms,
                    rate: f64::from(state.rate),
                    epoch_ms: state.epoch_ms,
                }));
            }
            TogetherStep::Publish(_) | TogetherStep::Stay => {}
        }
    }

    /// Do what the player's own settings menu chose.
    fn drain_player_settings(&mut self) {
        const RATES: [f64; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0];
        for event in self.player.take_setting_events() {
            let source_id = self.current_source_id.clone().unwrap_or_default();
            let effect = match event {
                PlayerSettingEvent::Toggled { id, on } if id == "autoplay" => {
                    Some(VideoEffect::SetAutoplay { enabled: on })
                }
                PlayerSettingEvent::Toggled { id, on } if id == "loop" => {
                    Some(VideoEffect::SetLoop { enabled: on })
                }
                PlayerSettingEvent::Chose { id, index } if id == "playback-rate" => RATES
                    .get(index)
                    .map(|rate| VideoEffect::SetPlaybackRate { rate: *rate }),
                PlayerSettingEvent::Chose { id, index } if id == "audio-track" => {
                    self.shown_metadata.audio_tracks.get(index).map(|track| {
                        VideoEffect::SelectAudioTrack {
                            track: crate::actions::audio_resource_id(&source_id, &track.stream_id),
                        }
                    })
                }
                PlayerSettingEvent::Chose { id, index } if id == "subtitle-track" => {
                    Some(match index.checked_sub(1) {
                        None => VideoEffect::SubtitlesOff,
                        Some(index) => match self.shown_metadata.subtitle_tracks.get(index) {
                            Some(track) => VideoEffect::SelectSubtitles {
                                track: crate::actions::subtitle_resource_id(
                                    &source_id,
                                    &track.stream_id,
                                ),
                            },
                            None => continue,
                        },
                    })
                }
                PlayerSettingEvent::Chose { id, index } if id == "chapters" => {
                    self.shown_metadata.chapters.get(index).map(|chapter| {
                        VideoEffect::JumpToChapter {
                            chapter: crate::actions::chapter_resource_id(&source_id, &chapter.id),
                        }
                    })
                }
                PlayerSettingEvent::Activated { id } if id == "picture-in-picture" => {
                    Some(VideoEffect::PictureInPicture)
                }
                PlayerSettingEvent::Toggled { .. }
                | PlayerSettingEvent::Chose { .. }
                | PlayerSettingEvent::Activated { .. } => None,
            };
            if let Some(effect) = effect
                && let Err(error) = self.perform(effect)
            {
                tracing::warn!(%error, "video player: a settings choice was refused");
            }
        }
    }

    /// Translate decoder-owned lifecycle events into app-visible state. The
    /// shared widget remains the single engine pump; this layer owns only the
    /// file-open contract and the user-facing error copy.
    fn drain_player_media_events(&mut self) {
        for event in self.player.take_media_events() {
            self.handle_media_event(event);
        }
    }

    fn handle_media_event(&mut self, event: media::MediaEvent) {
        match event {
            media::MediaEvent::MissingDecoder { codec } => {
                self.missing_codec = Some(codec);
                self.dirty = true;
            }
            media::MediaEvent::Error(message) => {
                // The decoder's own wording is for logs and agents; the
                // person is told what it means for them.
                tracing::warn!(error = %message, "video player: the decoder reported an error");
                self.fail_playback();
                if let Some(pending) = self.media_open.take() {
                    pending.reply.finish(Err(message));
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

    fn maybe_loop_current_media(&mut self) {
        if !self.preferences.loop_enabled || !self.player.has_media() {
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

    fn handle_app_pointer(&mut self, _state: &mut ui::widget::Edits<'_>) -> bool {
        self.drawer.resolve(self.rect);
        let in_list = self.library_list_rect.contains(self.last_cursor);
        if self.library_visible && self.drawer.sidebar_contains(self.last_cursor) {
            if self.library_close_rect.contains(self.last_cursor) {
                self.request_library_hide();
            } else if self.library_queue_tab_rect.contains(self.last_cursor) {
                self.request_library_tab(LibraryTab::Queue);
            } else if self.library_recent_tab_rect.contains(self.last_cursor) {
                self.request_library_tab(LibraryTab::Recent);
            } else if self.library_shared_tab_rect.contains(self.last_cursor) {
                self.request_library_tab(LibraryTab::Shared);
            } else if self.library_tab == LibraryTab::Shared
                && self.shared_watch_later_rect.contains(self.last_cursor)
            {
                self.add_current_to_watch_later();
            } else if self.library_tab == LibraryTab::Shared
                && self.shared_bookmark_rect.contains(self.last_cursor)
            {
                self.bookmark_current_time();
            } else if self.library_tab == LibraryTab::Shared
                && in_list
                && let Some(row) = self
                    .shared_row_hits
                    .iter()
                    .find(|row| row.jump.is_some_and(|jump| jump.contains(self.last_cursor)))
                    .cloned()
            {
                self.request_annotation_jump(&row.id);
            } else if self.library_tab == LibraryTab::Shared
                && in_list
                && let Some(row) = self
                    .shared_row_hits
                    .iter()
                    .find(|row| row.remove.contains(self.last_cursor))
                    .cloned()
            {
                if row.annotation {
                    self.delete_annotation(&row.id);
                } else {
                    self.delete_playlist(&row.id);
                }
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
            self.request_status_action();
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
        &self,
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
            self.request(&VideoEffect::PictureInPicture);
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
        Self::new(RunMode::Live)
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
    fn has_pending_visual_work(&self) -> bool {
        self.capture_pending()
            || self
                .children()
                .iter()
                .any(|child| child.has_pending_visual_work())
    }

    ui::provides!(
        dyn ui::widget::FullscreenProvider,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::widget::WindowHeader,
        dyn ui::widget::controls::ActionControls,
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        self.poll_kept_recall();
        self.drain_portal();
        self.tick_together();
        self.player.sync(state, path);
        self.drain_player_media_events();
        self.sync_decoder_settings();
        self.drain_player_settings();
        self.maybe_loop_current_media();
        self.poll_open_frame();
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
        self.history.tree_rebuilt();
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = ui::widget::placed(layout, origin);
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
        let history_hovered = self.history.is_hovered();
        // The library paints above the strip and its scrim owns background
        // input. In particular its Close face overlaps the trailing History
        // face, so that covered control must not consume the library click.
        let history_rect = if self.overlay_visible() && !self.library_visible {
            self.history_button_rect
        } else {
            Rect::ZERO
        };
        if self
            .history
            .handle_event(event, history_rect, ctx.scale_factor)
        {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }
        self.dirty |= history_hovered != self.history.is_hovered();
        // The library drawer reads its own dismissals (a press on the scrim,
        // Escape); the library's hide action is what closes it, so the
        // drawer stays open until that action lands.
        self.drawer.resolve(self.rect);
        let drawer_open = self.drawer.is_open();
        if self.drawer.handle_event(event, ctx) {
            if drawer_open && !self.drawer.is_open() {
                self.drawer.open();
                self.request_library_hide();
            }
            self.dirty = true;
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.handle_app_pointer(&mut ctx.state)
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

    /// The player, and the window's History control at its place in the
    /// strip, so it is addressable like any control; the strip paints it
    /// and routes its pointer through the opener.
    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = vec![&self.player as &dyn Widget];
        if let Some(history) = self.history.button() {
            children.push(history);
        }
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children = vec![&mut self.player as &mut (dyn Widget + '_)];
        if let Some(history) = self.history.button_mut() {
            children.push(history);
        }
        children
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
                    disabled: self.portal_open.is_some(),
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
        children.extend(self.history.accessibility(self.history_button_rect));
        if self.formats_visible() && self.status_action_rect.width() > 0.0 {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(self.formats_label.clone())
                    .with_action_target("video-player.formats")
                    .with_bounds(self.status_action_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.formats_focused,
                        ..Default::default()
                    }),
            );
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
        if let [index] = path
            && self.formats_access_index() == Some(*index)
        {
            return Some(self.status_action_rect);
        }
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
        if let [index] = path
            && self.formats_access_index() == Some(*index)
        {
            self.formats_focused = true;
            self.open_focused = false;
            self.library_focused = false;
            self.dirty = true;
            return true;
        }
        self.formats_focused = false;
        self.focus_player_control(path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if target == "video-player.formats" && matches!(action, ui::a11y::Action::Activate) {
            self.request(&VideoEffect::OpenFormats);
            true
        } else if self.history.perform_semantic_action(target, action) {
            true
        } else if target == "video-player.open" && matches!(action, ui::a11y::Action::Activate) {
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
            self.request_library_tab(LibraryTab::Queue);
            true
        } else if target == "video-player.library.tab.recent"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_tab(LibraryTab::Recent);
            true
        } else if target == "video-player.library.tab.shared"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_tab(LibraryTab::Shared);
            true
        } else if target == "video-player.shared.watch-later"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.add_current_to_watch_later();
            true
        } else if target == "video-player.shared.bookmark"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.bookmark_current_time();
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.jump.annotation.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_annotation_jump(rest);
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.delete.annotation.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.delete_annotation(rest);
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.delete.playlist.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.delete_playlist(rest);
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
        if self.formats_visible() && self.status_action_rect.contains(self.last_cursor) {
            return Some(self.formats_label.as_str());
        }
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
        // Playback and the portal are the only frame-rate work; a paused
        // player wakes for a peer's change when the document presents it.
        self.player.needs_animation_frame()
            || self.drawer.is_animating()
            || self.portal_open.is_some()
            || self.media_open.is_some()
    }

    fn needs_idle_sync(&self) -> bool {
        self.mode == RunMode::Live && self.current_media_ref.is_some()
    }
}

/// The top strip is the window's header row: it holds the History control
/// the host offers, over the picture, hiding with the other controls.
impl ui::widget::controls::ActionControls for PlayerApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.history.take()
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        ui::widget::semantic_action_bindings(self.accessibility())
    }
}

impl ui::widget::WindowHeader for PlayerApp {
    fn set_history_control(&mut self, control: Option<ui::widget::HistoryControl>) {
        if self.history.control() != control {
            self.history.set_control(control);
            self.dirty = true;
        }
    }
}

impl PlayerApp {}

#[cfg(test)]
mod tests {
    use super::strip::TopStrip;
    use super::*;
    use ui::headless::Framed;

    #[test]
    fn a_missing_decoder_exposes_one_formats_action_and_clears_on_a_new_open() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let mut app = PlayerApp::new(RunMode::Capture);
            app.handle_media_event(media::MediaEvent::MissingDecoder {
                codec: "HEVC".into(),
            });
            assert!(
                !app.formats_visible(),
                "the decoder has not reported failure yet"
            );
            app.handle_media_event(media::MediaEvent::Error("decoder missing".into()));
            let mut framed = Framed::new(
                ui::State::new(),
                app,
                ui::headless::viewport((960.0, 640.0)),
                theme,
            );
            assert!(
                framed.open_button_rect.width() > 0.0,
                "another video stays reachable"
            );
            let open = framed.open_button_rect;
            let formats = framed.status_action_rect;
            assert!(
                open.x1 <= formats.x0
                    || formats.x1 <= open.x0
                    || open.y1 <= formats.y0
                    || formats.y1 <= open.y0,
                "Open and the formats remedy have distinct painted targets"
            );
            let root = framed.accessibility().expect("player accessibility");
            let offers: Vec<_> = root
                .children
                .iter()
                .filter(|node| node.action_target.as_deref() == Some("video-player.formats"))
                .collect();
            assert_eq!(offers.len(), 1, "one visible decoder remedy");
            assert_eq!(
                offers[0].name.as_deref(),
                Some("Play more video and audio formats")
            );
            assert!(
                framed.status_action_rect.width() > 0.0 && framed.status_action_rect.height() > 0.0
            );
            assert!(framed.effect_statuses().iter().all(|status| *status
                != EffectStatus::unavailable(
                    VideoEffect::OPEN_FORMATS,
                    "no_missing_decoder",
                    crate::i18n::t("video-formats-unavailable")
                )));
            let index = framed
                .formats_access_index()
                .expect("the formats control is addressable");
            assert!(framed.focus_synthetic_accessibility(&[index]));
            assert!(framed.formats_focused);
            assert_eq!(
                framed.synthetic_accessibility_rect(&[index]),
                Some(framed.status_action_rect)
            );
            framed
                .open_named("/tilo-test-absent-video.mp4")
                .expect_err("a new failed file open");
            assert!(
                !framed.formats_visible(),
                "the prior decoder does not describe the new file"
            );
            assert!(framed.effect_statuses().iter().any(|status| *status
                == EffectStatus::unavailable(
                    VideoEffect::OPEN_FORMATS,
                    "no_missing_decoder",
                    crate::i18n::t("video-formats-unavailable")
                )));
        }
    }

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
            let mut app = PlayerApp::new(RunMode::Capture);
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
            let mut app = PlayerApp::for_shot(Some("metadata"));
            let old_source = app.current_source_id.clone().unwrap();
            let source = media::Source::Remote {
                url: "https://demo.local/clip.mp4".into(),
            };
            app.set_current_source(&source)
                .expect("source identity available");
            assert_ne!(app.current_source_id.as_deref(), Some(old_source.as_str()));
            let track = app.shot_metadata.as_ref().unwrap().audio_tracks[0].clone();
            assert!(
                app.perform(VideoEffect::SelectAudioTrack {
                    track: crate::actions::audio_resource_id(&old_source, &track.stream_id),
                })
                .is_err(),
                "a row of the previous open names no stream of this one"
            );
        }
    }

    #[test]
    fn portal_open_is_single_flight_generation_fenced_and_retained() {
        // Case `portal_outcome_opens_the_chosen_file`.
        {
            let mut app = PlayerApp::new(RunMode::Capture);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Chosen.webm")));
            ui::host::scoped(&services, || app.trigger_open(None)).expect("picker starts");
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
            let mut app = PlayerApp::new(RunMode::Capture);
            app.trigger_open(None).expect("first picker starts");
            let first_generation = app.portal_open.as_ref().map(|open| open.generation);
            assert!(
                app.trigger_open(None).is_err(),
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
            let mut app = PlayerApp::new(RunMode::Capture);
            app.portal_generation = u64::MAX;
            let error = app
                .trigger_open(None)
                .expect_err("generation must not wrap");
            assert!(error.contains("generation exhausted"));
            assert!(app.portal_open.is_none());
            assert_eq!(app.portal_generation, u64::MAX);
        }
        // Case `stale_portal_outcome_cannot_open_media_in_release_or_debug_builds`.
        {
            let mut app = PlayerApp::new(RunMode::Capture);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Stale.webm")));
            ui::host::scoped(&services, || app.trigger_open(None)).expect("picker starts");
            // A newer request generation makes the delivered outcome stale.
            app.portal_generation += 1;
            app.drain_portal();
            assert_eq!(app.title, "No media");
            assert!(app.current_source_id.is_none());
            assert!(app.portal_open.is_none());
        }
        // Case `portal_failure_is_retained_for_the_empty_state`.
        {
            let mut app = PlayerApp::new(RunMode::Capture);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Failed("permission denied".into()));
            ui::host::scoped(&services, || app.trigger_open(None)).expect("picker starts");
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
        // Case `marker_jump_seeks_the_video_its_note_belongs_to`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/review-cut.mp4"))
                .expect("source identity");
            let media_ref = app.current_media_ref.clone().expect("durable media ref");
            let mut library = crate::shared::Library::default();
            library.annotations.push(
                Id::from_raw("review-marker"),
                crate::shared::TimestampAnnotation {
                    media_ref,
                    time_ms: 12_500,
                    text: "Tighten this cut".into(),
                },
            );
            library.annotations.push(
                Id::from_raw("elsewhere"),
                crate::shared::TimestampAnnotation {
                    media_ref: "another-video".into(),
                    time_ms: 30_000,
                    text: String::new(),
                },
            );
            app.present_library(&library);
            app.perform(VideoEffect::JumpToAnnotation {
                annotation: "review-marker".into(),
            })
            .expect("the note belongs to the open video");
            assert_eq!(app.player.position_ms(), 12_500);
            let error = app
                .perform(VideoEffect::JumpToAnnotation {
                    annotation: "elsewhere".into(),
                })
                .expect_err("a note on another video is refused");
            assert!(error.contains("different video"));
            assert!(
                app.perform(VideoEffect::JumpToAnnotation {
                    annotation: "gone".into(),
                })
                .is_err(),
                "a deleted note is refused"
            );
            assert_eq!(app.player.position_ms(), 12_500);
        }
        // Case `stale_stream_and_chapter_targets_are_refused_before_engine_mutation`.
        {
            let mut app = PlayerApp::for_shot(Some("metadata"));
            let audio = app.shot_metadata.as_ref().unwrap().audio_tracks[0].clone();
            let chapter = app.shot_metadata.as_ref().unwrap().chapters[0].clone();
            let stale_source = "https://old.example/clip.mkv";
            assert!(
                app.perform(VideoEffect::SelectAudioTrack {
                    track: crate::actions::audio_resource_id(stale_source, &audio.stream_id),
                })
                .is_err()
            );
            let position = app.player.position_ms();
            assert!(
                app.perform(VideoEffect::JumpToChapter {
                    chapter: crate::actions::chapter_resource_id(stale_source, &chapter.id),
                })
                .is_err()
            );
            assert_eq!(app.player.position_ms(), position, "nothing moved");
            let source_id = app.current_source_id.clone().unwrap();
            app.perform(VideoEffect::JumpToChapter {
                chapter: crate::actions::chapter_resource_id(&source_id, &chapter.id),
            })
            .expect("a chapter of the open source");
            assert_eq!(
                app.player.position_ms(),
                0,
                "the first chapter starts the clip"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn library_controls_emit_canonical_host_actions_and_are_accessible() {
        // Case `library_play_focus_has_a_real_accessibility_target_and_visual_state`.
        {
            let mut app = PlayerApp::for_shot(Some("library"));
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
            let mut app = PlayerApp::for_shot(Some("library"));
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

            app.perform(VideoEffect::ShowLibrary {
                tab: "recent".into(),
            })
            .unwrap();
            assert_eq!(app.library_tab, LibraryTab::Recent);
            assert!(
                app.perform(VideoEffect::ShowLibrary {
                    tab: "elsewhere".into()
                })
                .is_err(),
                "an unknown tab is refused"
            );
            app.perform(VideoEffect::HideLibrary).unwrap();
            assert!(!app.library_visible);
        }
        // Case `library_has_conventional_escape_close_and_complete_hover_help`.
        {
            let mut app = PlayerApp::for_shot(Some("shared"));
            app.library_close_rect = Rect::new(0.0, 0.0, 44.0, 44.0);
            app.last_cursor = Point::new(20.0, 20.0);
            assert_eq!(app.tooltip(), Some("Close library (Escape)"));

            assert!(
                app.handle_app_shortcut_input(
                    &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                    ElementState::Pressed,
                    winit::event::Modifiers::default(),
                ),
                "Escape is the library's close key"
            );
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
            app.perform(VideoEffect::PictureInPicture).unwrap();
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
        // Case `live_rows_cover_the_open_video_its_streams_and_the_library`.
        {
            let app = PlayerApp::for_shot(Some("metadata"));
            let rows: Vec<_> = crate::actions::live_rows(&app.player_rows(), 32)
                .into_iter()
                .map(ui::framework::LiveRow::into_resource)
                .collect();
            let kinds: Vec<&str> = rows.iter().map(|row| row.kind.as_str()).collect();
            assert_eq!(kinds, ["media.video", "media.queue", "media.history"]);
            let media = &rows[0];
            assert_eq!(media.label, "Demo clip");
            let tracks = &media.children[0];
            assert_eq!(tracks.kind, "media.tracks");
            assert_eq!(
                tracks
                    .children
                    .iter()
                    .map(|track| track.label.as_str())
                    .collect::<Vec<_>>(),
                [
                    "English stereo is selected",
                    "Deutsch 5.1",
                    "English captions is selected"
                ]
            );
            assert!(
                tracks.children.iter().all(|track| track
                    .actions
                    .iter()
                    .all(|action| action.kind != ui::tilo_actions::OperationKind::Document)),
                "a live row never changes the document"
            );
            let source_id = app.current_source_id.clone().unwrap();
            let chapter = crate::actions::chapter_resource_id(&source_id, "scene");
            assert!(
                crate::actions::live_row(&app.player_rows(), &chapter).is_some(),
                "a row resolves by id"
            );
        }
        // Case `missing_media_leaves_transport_unavailable`.
        {
            let empty = PlayerApp::new(RunMode::Capture);
            assert!(empty.effect_statuses().iter().any(|status| {
                format!("{status:?}").contains("toggle-playback")
                    && format!("{status:?}").contains("no_media")
            }));
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            assert!(
                !app.effect_statuses()
                    .iter()
                    .any(|status| { format!("{status:?}").contains("toggle-playback") })
            );
        }
    }

    #[test]
    fn library_sheet_rows_tabs_and_scrim_answer_the_pointer() {
        use ui::framework::testing::TestApp;
        fn surface<T>(
            app: &mut TestApp<crate::resource::VideoPlayerShell>,
            f: impl FnOnce(&mut PlayerApp) -> T,
        ) -> T {
            app.with_surface::<PlayerApp, T>(f).expect("mounted player")
        }
        let mut app = TestApp::<crate::resource::VideoPlayerShell>::new().unwrap();
        surface(&mut app, |player| {
            // Hermetic authored media, with live framework action admission.
            player
                .player
                .set_engine(media::MediaEngine::new_synthesized());
            player.stage_capture(None, Some("library"));
        });
        app.harness()
            .relayout_themed(
                ui::headless::viewport((1200.0, 800.0)),
                Theme::dark().with_reduce_motion(true),
            )
            .unwrap();
        app.settle();
        let next = surface(&mut app, |player| player.queue[1].id.clone());
        let row = surface(&mut app, |player| {
            player
                .library_row_hits
                .iter()
                .find(|hit| hit.id == next)
                .cloned()
                .expect("queued row placed")
        });
        app.harness()
            .click_at(row.play.center().x, row.play.center().y);
        app.settle();
        assert_eq!(
            surface(&mut app, |player| player.current_queue_id.clone()),
            Some(next.clone()),
            "row body plays its canonical queue entry"
        );
        let remove = surface(&mut app, |player| {
            player
                .library_row_hits
                .iter()
                .find(|hit| hit.id == next)
                .and_then(|hit| hit.remove)
                .expect("queued row removal control")
        });
        app.harness().click_at(remove.center().x, remove.center().y);
        app.settle();
        assert!(
            surface(&mut app, |player| player
                .queue
                .iter()
                .all(|entry| entry.id != next)),
            "remove changes the queue without replaying the row"
        );
        let recent = surface(&mut app, |player| player.library_recent_tab_rect.center());
        app.harness().click_at(recent.x, recent.y);
        app.settle();
        assert_eq!(
            surface(&mut app, |player| player.library_tab_name()),
            "recent"
        );
        let close = surface(&mut app, |player| player.library_close_rect.center());
        app.harness().click_at(close.x, close.y);
        app.settle();
        assert!(!surface(&mut app, |player| player.library_visible));
        app.effect(&VideoEffect::ShowLibrary {
            tab: "queue".into(),
        })
        .unwrap();
        assert!(surface(&mut app, |player| player.library_visible));
        app.harness().click_at(40.0, 400.0);
        app.settle();
        assert!(
            !surface(&mut app, |player| player.library_visible),
            "the scrim closes the actual hosted library"
        );
        assert!(
            !surface(&mut app, |player| {
                player.last_cursor = Point::new(40.0, 400.0);
                player.handle_app_pointer(&mut ui::widget::Edits::lend(
                    &mut automerge::AutoCommit::new(),
                ))
            }),
            "the closed library no longer captures the same press"
        );
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
                PlayerApp::for_shot_open(None, Some("playing")),
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
                PlayerApp::for_shot_open(None, Some("library")),
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
                PlayerApp::for_shot_open(None, Some("library")),
                (1200.0, 800.0),
                1.0,
            );
            app.perform(VideoEffect::HideLibrary).unwrap();
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
            let mut app = PlayerApp::new(RunMode::Capture);
            let doc = automerge::AutoCommit::new();
            // Pumping the app before any file is opened must not fabricate a frame.
            app.sync(&doc, &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
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
        let mut app = PlayerApp::new(RunMode::Capture);
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
            let doc = automerge::AutoCommit::new();
            app.sync(&doc, &automerge::ROOT);
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
    }

    #[test]
    fn capture_waits_for_the_requested_file_in_the_shared_visual_lifecycle() {
        let requested = "/a-missing-tilo-capture-fixture.mp4";
        let app = PlayerApp::for_shot_open(Some(requested), None);
        assert!(
            Widget::has_pending_visual_work(&app),
            "a real-file capture cannot settle without its actual decoded picture"
        );
        assert!(
            !app.has_media_frame(),
            "no synthesized demo substitutes for the file"
        );
        let loading = PlayerApp::for_shot_open(None, Some("loading"));
        assert!(
            !Widget::has_pending_visual_work(&loading),
            "an explicitly requested loading capture need not await decoding"
        );
    }

    #[test]
    fn capture_scenarios_stage_the_two_failure_states() {
        // A failed open leaves nothing open; a decoder that gives up on an
        // opened video keeps its title. Both tell the person the same thing.
        let failed_open = PlayerApp::for_shot_open(None, Some("error"));
        assert_eq!(failed_open.open_error.as_deref(), Some(DECODER_FAILURE));
        assert!(!failed_open.player.has_media());
        let gave_up = PlayerApp::for_shot_open(None, Some("native-error"));
        assert_eq!(gave_up.open_error.as_deref(), Some(DECODER_FAILURE));
        assert!(gave_up.player.has_media());
        assert_eq!(gave_up.title, "Demo clip");
        assert!(
            gave_up.shot_loading,
            "the picture is held back for the failure"
        );
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

            let doc = automerge::AutoCommit::new();
            app.sync(&doc, &automerge::ROOT);
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
            let doc = automerge::AutoCommit::new();
            app.sync(&doc, &automerge::ROOT); // baseline the synth clock
            std::thread::sleep(std::time::Duration::from_millis(40));
            app.sync(&doc, &automerge::ROOT);
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
            app.sync(&doc, &automerge::ROOT);
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
            app.perform(VideoEffect::SetPlaybackRate { rate: 2.0 })
                .unwrap();
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

    fn transport(playing: bool, position_ms: u64, epoch_ms: u64) -> media::TransportState {
        media::TransportState {
            playing,
            position_ms,
            rate: 1.0,
            epoch_ms,
        }
    }

    fn shared(state: media::TransportState) -> crate::shared::SharedTransport {
        crate::shared::SharedTransport {
            playing: state.playing,
            position_ms: state.position_ms,
            rate: f64::from(state.rate),
            epoch_ms: state.epoch_ms,
        }
    }

    #[test]
    fn watching_together_follows_moves_and_says_its_own_once() {
        let start = 1_000_000;
        // A player that starts playing says so; its own change coming back
        // is not followed.
        let mut here = Together::default();
        assert_eq!(
            here.step("clip", None, transport(false, 0, start), start, 60_000),
            TogetherStep::Stay,
            "a paused player that nobody else watches says nothing"
        );
        let playing = transport(true, 0, start + 10);
        assert_eq!(
            here.step("clip", None, playing, start + 10, 60_000),
            TogetherStep::Publish(playing)
        );
        assert_eq!(
            here.step("clip", Some(&shared(playing)), playing, start + 20, 60_000),
            TogetherStep::Stay,
            "its own change is not followed back"
        );
        // Its engine still starting: the playhead has not moved yet. A
        // leader never seeks itself toward its own announcement.
        assert_eq!(
            here.step(
                "clip",
                Some(&shared(playing)),
                transport(true, 0, start + 900),
                start + 900,
                60_000
            ),
            TogetherStep::Stay,
            "a leader does not ride along with itself"
        );

        // Another player joins the same video and follows; while its engine
        // catches up it says nothing of its own.
        let mut there = Together::default();
        let local = transport(false, 0, start + 500);
        assert_eq!(
            there.step("clip", Some(&shared(playing)), local, start + 500, 60_000),
            TogetherStep::Follow(playing)
        );
        assert_eq!(
            there.step("clip", Some(&shared(playing)), local, start + 900, 60_000),
            TogetherStep::Follow(playing),
            "settling: a lagging engine is not a move"
        );
        // Small drift rides; a seek far away is a move everyone follows.
        let riding = transport(true, 2_100, start + 2_000);
        assert_eq!(
            there.step(
                "clip",
                Some(&shared(playing)),
                riding,
                start + 2_000,
                60_000
            ),
            TogetherStep::Follow(playing)
        );
        let sought = transport(true, 30_000, start + 2_100);
        assert_eq!(
            there.step(
                "clip",
                Some(&shared(playing)),
                sought,
                start + 2_100,
                60_000
            ),
            TogetherStep::Publish(sought)
        );
        // Pausing is a move too.
        assert_eq!(
            here.step(
                "clip",
                Some(&shared(sought)),
                playing,
                start + 2_200,
                60_000
            ),
            TogetherStep::Follow(sought),
            "the first player follows the seek"
        );
    }

    #[test]
    fn a_video_watched_long_ago_starts_on_its_own() {
        let now = 50_000_000;
        let long_ago = shared(transport(false, 12_000, now - TOGETHER_PAUSE_LIVES_MS - 1));
        let mut here = Together::default();
        assert_eq!(
            here.step(
                "clip",
                Some(&long_ago),
                transport(false, 0, now),
                now,
                60_000
            ),
            TogetherStep::Stay,
            "an old pause is not joined"
        );
        let finished = shared(transport(true, 0, now - 120_000));
        let mut there = Together::default();
        assert_eq!(
            there.step(
                "clip",
                Some(&finished),
                transport(false, 0, now),
                now,
                60_000
            ),
            TogetherStep::Stay,
            "a playback that ran past the end is over"
        );
    }

    #[test]
    fn revealing_the_controls_never_changes_transport() {
        let mut app = PlayerApp::for_shot(None);
        app.player.pause();
        let position = app.player.position_ms();
        app.perform(VideoEffect::Reveal).unwrap();
        assert!(!app.player.is_playing());
        assert_eq!(app.player.position_ms(), position);
    }
}
