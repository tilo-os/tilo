//! The local video-player app: an edge-to-edge reusable
//! [`ui::widget::VideoPlayer`] driven by a shared [`media::MediaEngine`].
//!
//! `PlayerApp` is the root widget. The video fills the whole surface — there is
//! **no persistent app chrome bar** (the OS owns window chrome; HIG §1e wants
//! video immersive). The library's toggle, the media title and Open are slots
//! in the window's bar (`window_bar`), which steps aside with the rest of the
//! desktop's chrome while the video plays full screen, and the library
//! itself is a sheet over a scrim (`library`). With no picture the area says
//! why (`status`): nothing is open, a first frame is coming, or the open
//! failed. It opens files through the XDG
//! `FileChooser` portal (or `--open PATH`), and — when a collab session is
//! attached — keeps playback in sync across peers via the shared [`media`]
//! transport.

mod decoder;
mod effects;
mod history;
mod input;
mod library;
mod picker;
mod requests;
mod rows;
mod shots;
mod status;
mod window_bar;

use library::{
    local_library_accessibility_nodes, shared_accessibility_nodes, shared_accessibility_targets,
};

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use ui::widget::{AppModel, ModelPath};

use taffy::style_helpers::percent;
use taffy::{NodeId, Size, Style, TaffyTree};
use tilo_clock::unix_now_ms;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::icons::Icon;
use ui::paint::fill_rect;
use ui::portal::{ContentsOutcome, PickedContents, PortalOutcome};
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
use crate::preferences::{PLAYBACK_RATES, PlayerSettings, RecentMedia, ResumePoints};
use crate::shared::{Library as PlayerResource, library as verbs};

/// The playback preferences the player follows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PlaybackPreferences {
    autoplay: bool,
    loop_enabled: bool,
    /// The output volume, `0.0..=1.0`.
    volume: f32,
    /// The playback speed, one of [`PLAYBACK_RATES`].
    rate: f32,
}

impl Default for PlaybackPreferences {
    fn default() -> Self {
        let defaults = PlayerSettings::default();
        Self::from_settings(&defaults)
    }
}

impl PlaybackPreferences {
    /// What this device stored, as the player follows it.
    fn from_settings(stored: &PlayerSettings) -> Self {
        Self {
            autoplay: stored.autoplay,
            loop_enabled: stored.loop_enabled,
            volume: stored.volume,
            rate: stored.rate,
        }
    }
}

/// A decoder open is asynchronous. Do not turn portal/direct admission into a
/// false successful history entry until a first picture has actually arrived.
const OPEN_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
mod opening;

use opening::*;
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
fn configured_player(mode: RunMode, preferences: PlaybackPreferences) -> VideoPlayer {
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
    configure_player_settings(&mut player, preferences);
    player
}

/// How often a playing video's resume point is written while it plays.
const RESUME_WRITE_INTERVAL_MS: u64 = 3_000;
/// Within this of the end, a video counts as finished: replaying it starts at
/// the beginning rather than in its last moments.
const RESUME_END_MARGIN_MS: u64 = 2_000;
/// A volume change is kept once it settles: a slider drag writes one value
/// instead of one per step, and the value it comes to rest at is never
/// dropped.
const VOLUME_SETTLE_MS: u64 = 400;

/// Put this device's own volume and speed on `player`: what the person last
/// chose here, whatever video it plays next. The volume is only set when it
/// differs, so an unrelated preference change does not unmute a player the
/// person muted.
fn apply_playback_levels(player: &mut VideoPlayer, preferences: PlaybackPreferences) {
    if (player.volume() - preferences.volume).abs() > f32::EPSILON {
        player.set_volume(preferences.volume);
    }
    if let Some(engine) = player.engine_mut() {
        engine.set_rate(preferences.rate);
    }
}

fn configure_player_settings(player: &mut VideoPlayer, preferences: PlaybackPreferences) {
    apply_playback_levels(player, preferences);
    // These settings are implemented by this app, so the shared settings
    // popover is never decorative.
    player.set_setting(PlayerSetting::toggle(
        "autoplay",
        Icon::Play,
        "Autoplay",
        0,
        preferences.autoplay,
    ));
    player.set_setting(PlayerSetting::toggle(
        "loop",
        Icon::Repeat1,
        "Repeat current video",
        0,
        preferences.loop_enabled,
    ));
    let rate = player
        .engine()
        .map_or(preferences.rate, media::MediaEngine::rate);
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

pub(crate) mod source;

pub(crate) use source::demo_media_ref;
use source::*;

#[allow(
    clippy::unwrap_used,
    reason = "new_with_children cannot fail for children built in this same tree; see doc comment"
)]
fn parent_node(taffy: &mut TaffyTree<()>, style: Style, children: &[NodeId]) -> NodeId {
    taffy.new_with_children(style, children).unwrap()
}
#[allow(
    clippy::struct_excessive_bools,
    reason = "overlay, local library, portal, and frame-dirty flags are independent widget state"
)]
pub struct PlayerApp {
    player: VideoPlayer,
    title: String,
    rect: Rect,
    /// The action of the idle or error state over the picture.
    status_action_rect: Rect,
    /// A video dropped anywhere on the window opens like one chosen.
    file_open: ui::widget::FileOpenSurface,
    /// How far the docked library pushes the picture in from the leading
    /// edge, as the last layout laid the player out.
    content_inset: f64,
    /// The docked library's width changed since the player was laid out.
    content_inset_stale: bool,
    /// The top of the player's transport, shown or not: a floating library
    /// stops above it.
    transport_top: f64,
    /// The library sheet: a leading drawer over the video, on the side of
    /// the Library button that opens it.
    drawer: SidebarDrawer,
    /// Where the sheet's lists scroll to, and how far they can.
    library_scroll: f64,
    library_max_scroll: f64,
    /// The area of the sheet its rows live in; rows outside it do not act.
    library_list_rect: Rect,
    /// The way back to the picture while the library fills a narrow window's
    /// content area ([`Self::library_takes_the_window`]).
    library_back_rect: Rect,
    library_queue_tab_rect: Rect,
    library_recent_tab_rect: Rect,
    library_shared_tab_rect: Rect,
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
    library_focus_id: Option<String>,
    /// Playback watched together with everyone who has the same video open.
    together: Together,
    preferences: PlaybackPreferences,
    /// This device's settings, where a live window keeps its preferences
    /// and recent videos; `None` for a capture or a past version.
    setting: Option<Setting<PlayerSettings>>,
    /// Where each video left off on this device, by its opaque identity, so
    /// opening it again resumes: see [`ResumePoints`].
    resume: ResumePoints,
    /// The resume point last written for the open video, so a playing video
    /// writes every few seconds and a still one only when it moves.
    resume_write: Option<ResumeWrite>,
    /// Where the open video is to be moved once its length is known: the
    /// decoder answers that asynchronously, and a seek before then would be
    /// clamped to the unknown length.
    pending_resume: Option<PendingResume>,
    /// When the volume last changed, while it is still settling.
    volume_settling_since: Option<u64>,
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
    /// The shared library's verbs bound to what the player shows, published
    /// for the view that hosts it: the view reads it, so it does not hold
    /// the player.
    playhead: Option<crate::resource::Playhead>,
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
    /// The empty state's action holds the keyboard.
    status_focused: bool,
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

/// The resume point of the open video as it was last written.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ResumeWrite {
    media_ref: String,
    position_ms: u64,
    playing: bool,
    at_ms: u64,
}

/// Where a just-opened video is to be moved, for its identity: applied once
/// the decoder knows how long the video is.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingResume {
    media_ref: String,
    position_ms: u64,
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
                if !Self::current(shared, now, duration_ms) {
                    // A playback that already ran past the end (such as an
                    // older one the room put back when it undid this
                    // player's move) is over: following it would seek here
                    // to the end again and swallow every seek and replay.
                    self.over = true;
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
            Some(shared)
                if shared.playing && !self.leading && Self::current(shared, now, duration_ms) =>
            {
                TogetherStep::Follow(shared.state())
            }
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
    /// A player with a fresh engine and no media loaded, for a window built
    /// in `mode`, on the default preferences until a live window reads this
    /// device's settings when it is attached.
    pub fn new(mode: RunMode) -> Self {
        // Tests exercise the real request and completion paths; a recording
        // host answers every picker so no desktop file chooser ever opens.
        #[cfg(test)]
        ui::host::install_default(ui::host::HostServices::recording().0);
        let preferences = PlaybackPreferences::default();
        let player = configured_player(mode, preferences);
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
            status_action_rect: Rect::ZERO,
            file_open: ui::widget::FileOpenSurface::default(),
            drawer: SidebarDrawer::new()
                .left()
                .closed()
                .content_kind(ui::widget::ContentKind::Document)
                .with_push(),
            content_inset: 0.0,
            content_inset_stale: false,
            transport_top: 0.0,
            library_scroll: 0.0,
            library_max_scroll: 0.0,
            library_list_rect: Rect::ZERO,
            library_queue_tab_rect: Rect::ZERO,
            library_recent_tab_rect: Rect::ZERO,
            library_shared_tab_rect: Rect::ZERO,
            library_row_hits: Vec::new(),
            library_clear_rect: Rect::ZERO,
            library_back_rect: Rect::ZERO,
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
            library_focus_id: None,
            together: Together::default(),
            preferences,
            setting: None,
            resume: ResumePoints::default(),
            resume_write: None,
            pending_resume: None,
            volume_settling_since: None,
            shot_metadata: None,
            shot_loading: false,
            pending_pip: None,
            dispatcher: None,
            playhead: None,
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
            status_focused: false,
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
            // The handoff says where playback stood a moment ago, which is
            // newer than anything this device stored: it wins.
            self.pending_resume = None;
            self.player.seek_to_ms(handoff.position_ms);
            if handoff.resume_playing {
                self.player.play();
            } else {
                self.player.pause();
            }
        }
    }

    /// Publish what the playhead is on into `playhead`, which the view that
    /// hosts this player reads.
    pub(crate) fn share_playhead(&mut self, playhead: &crate::resource::Playhead) {
        self.playhead = Some(playhead.clone());
        self.publish_playhead();
    }

    /// Tell the view what the playhead is on now. Nothing happens before the
    /// view hands over where to say it.
    fn publish_playhead(&self) {
        let Some(playhead) = &self.playhead else {
            return;
        };
        *playhead.borrow_mut() = self.playhead_declarations();
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

    /// Follow this device's stored preferences, the levels it plays at, its
    /// recent videos and where each of them left off.
    fn adopt_settings(&mut self, stored: &PlayerSettings) {
        self.preferences = PlaybackPreferences::from_settings(stored);
        configure_player_settings(&mut self.player, self.preferences);
        self.recent = stored
            .recent_history
            .iter()
            .map(|entry| LocalMediaEntry {
                id: entry.id.clone(),
                title: entry.title.clone(),
                source: entry.source.clone(),
            })
            .collect();
        // Rows are newest first; the map is oldest first, so it drops the
        // same row a bounded recent history would.
        self.resume.clear();
        for entry in stored.recent_history.iter().rev() {
            // A row read back from a launch is a place to resume from, never
            // an instruction to start playing: a restart never autoplays.
            self.resume
                .record(&resume_key(&entry.source), entry.position_ms, false);
        }
        self.kept_retained = self
            .recent
            .iter()
            .filter_map(|entry| crate::kept::token_of(&entry.source).map(str::to_owned))
            .collect();
        self.dirty = true;
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        let source = source_for_path(path);
        let title = source_title(&source);
        let queue_id = self.ensure_queued(source.clone(), title.clone());
        self.open_local_source(source, title, Some(queue_id))
    }

    /// Open a video dropped on the window, saying over the picture why it
    /// cannot be when it cannot.
    fn open_dropped(&mut self, path: &Path) {
        self.open_error = None;
        self.missing_codec = None;
        if let Err(error) = self.open_path(path) {
            self.open_error = Some(error);
        }
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
                    "{} — too large to reopen after a reload (over {})",
                    entry.title,
                    ui::format::memory(crate::kept::KEEP_LIMIT as u64)
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
        // This open leaves the video that was open: keep where it stood
        // before its decoder is replaced.
        self.remember_position(true);
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
        // Every open plays at this device's own levels, loudly enough and at
        // the speed this person chose here.
        apply_playback_levels(&mut self.player, self.preferences);
        self.resume_write = None;
        // Where this video left off, if it did: the seek lands once the
        // decoder reports the length (see `apply_pending_resume`).
        let resume = self
            .current_media_ref
            .as_deref()
            .and_then(|media_ref| self.resume.point(media_ref));
        self.pending_resume = resume.map(|(position_ms, _)| PendingResume {
            media_ref: self.current_media_ref.clone().unwrap_or_default(),
            position_ms,
        });
        let playing = resume.map_or(self.preferences.autoplay, |(_, playing)| playing);
        if playing {
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
        dyn ui::widget::controls::ActionControls,
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        self.poll_kept_recall();
        self.drain_portal();
        self.tick_together();
        self.player.sync(state, path);
        self.apply_pending_resume();
        self.drain_player_media_events();
        self.sync_decoder_settings();
        self.drain_player_settings();
        self.maybe_loop_current_media();
        self.remember_position(false);
        self.settle_volume();
        self.poll_open_frame();
        self.publish_playhead();
    }

    fn settle(&mut self, _edits: &mut ui::widget::Edits<'_>, _path: &automerge::ObjId) {
        self.run_window_toolbar();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.drawer.scale_for(theme);
        self.content_inset_stale = false;
        // Edge-to-edge: the player fills the whole surface beside the docked
        // library. The app keeps itself in the node map (so it can intercept
        // the floating Open pill) by owning a single-child root that
        // stretches the player over what the library leaves.
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
                padding: taffy::Rect {
                    left: taffy::style_helpers::length(ui::num::f32_of_f64(self.content_inset)),
                    ..taffy::Rect::zero()
                },
                ..Default::default()
            },
            &[player_node],
        );
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    /// The docked library's width, as the leading padding the player is
    /// laid out beside, on a retained tree as on a rebuilt one.
    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        self.content_inset_stale = false;
        let Ok(style) = taffy.style(node) else {
            return false;
        };
        let inset = taffy::style_helpers::length(ui::num::f32_of_f64(self.content_inset));
        if style.padding.left == inset {
            return false;
        }
        let mut style = style.clone();
        style.padding.left = inset;
        taffy.set_style(node, style).is_ok()
    }

    /// The player's place follows the docked library, which follows the
    /// window's width.
    fn composition_follows_allocation(&self) -> bool {
        true
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        let Ok(children) = taffy.children(node) else {
            return;
        };
        // children[0] = player, filling the whole surface (edge-to-edge).
        let Some(&player_node) = children.first() else {
            return;
        };
        self.player.apply_layout(taffy, player_node, new_origin);
        self.resolve_library();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // A key (L, Ctrl+O) or an arriving picture changes what the bar
        // shows without a sync.
        ui::window_toolbar::declare(Some(self.window_toolbar()));
        self.sync_library_sheet();
        self.drawer.tick(theme);
        self.transport_top = self.player.transport_rect(theme).y0;
        self.resolve_library();
        if self.shot_loading {
            self.paint_letterbox(scene, transform, theme);
        } else {
            self.player
                .draw(scene, transform, text_renderer, theme, presence);
        }
        self.paint_media_status(scene, transform, text_renderer, theme);
        self.paint_library_panel(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let Some(file) = self.file_open.handle_event(event) {
            if let ui::widget::FileOpenEvent::Open(path) = file {
                self.open_dropped(&path);
            }
            self.dirty = true;
            *ctx.handled = true;
            return;
        }
        // The library drawer reads its own dismissals (a press on the scrim,
        // Escape); the library's hide action is what closes it, so the
        // drawer stays open until that action lands.
        self.resolve_library();
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

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.player as &dyn Widget]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.player as &mut (dyn Widget + '_)]
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children = Vec::new();
        if self.library_visible {
            if self.library_back_rect.width() > 0.0 {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name("Back to video")
                        .with_description("Escape")
                        .with_action_target("video-player.library.back")
                        .with_bounds(self.library_back_rect)
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.library_focus_id.as_deref() == Some("library-back"),
                            ..Default::default()
                        }),
                );
            }
            children.extend([
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
        if self.status_action_rect.width() > 0.0 {
            let action = if self.formats_visible() {
                AccessNode::new(Role::Button)
                    .with_name(self.formats_label.clone())
                    .with_action_target("video-player.formats")
            } else {
                AccessNode::new(Role::Button)
                    .with_name("Open video")
                    .with_description("Ctrl+O")
                    .with_action_target("video-player.open")
            };
            children.push(
                action
                    .with_bounds(self.status_action_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.status_focused,
                        disabled: self.portal_open.is_some() && !self.formats_visible(),
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
            && self.status_access_index() == Some(*index)
        {
            return Some(self.status_action_rect);
        }
        if !self.library_visible {
            return None;
        }
        let back = self.library_back_rect.width() > 0.0;
        if back && path == [0] {
            return Some(self.library_back_rect);
        }
        let [index] = path else {
            return None;
        };
        self.library_control_rect(index.saturating_sub(usize::from(back)))
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        if let [index] = path
            && self.status_access_index() == Some(*index)
        {
            self.status_focused = true;
            self.dirty = true;
            return true;
        }
        self.status_focused = false;
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
        } else if target == "video-player.open" && matches!(action, ui::a11y::Action::Activate) {
            self.request_open_action();
            true
        } else if let Some(item_id) = target.strip_prefix("video-player.library.play.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_play(item_id);
            true
        } else if target == "video-player.library.back"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_video_show();
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
        if self.status_action_rect.contains(self.last_cursor) {
            Some("Open video (Ctrl+O)")
        } else if self.library_visible && self.library_back_rect.contains(self.last_cursor) {
            Some("Go back to the video")
        } else if self.library_visible && self.library_queue_tab_rect.contains(self.last_cursor) {
            Some("Show this device's queue")
        } else if self.library_visible && self.library_recent_tab_rect.contains(self.last_cursor) {
            Some("Show recently played videos on this device")
        } else if self.library_visible && self.library_shared_tab_rect.contains(self.last_cursor) {
            Some("Show the playlists and bookmarks everyone in this workspace shares")
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
        self.player.is_layout_dirty() || self.content_inset_stale
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

impl ui::widget::controls::ActionControls for PlayerApp {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        ui::widget::semantic_action_bindings(self.accessibility())
    }
}

impl PlayerApp {}

#[cfg(test)]
mod tests;
