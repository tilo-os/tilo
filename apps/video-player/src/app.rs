//! The local video-player app: an edge-to-edge reusable
//! [`ui::widget::VideoPlayer`] driven by a shared [`media::MediaEngine`].
//!
//! `PlayerApp` is the root widget. The video fills the whole surface — there is
//! **no persistent app chrome bar** (the OS owns window chrome; HIG §1e wants
//! video immersive). The media title and the Open action float in a top overlay
//! that appears with the player's controls and auto-hides during playback, so
//! immersive video is truly edge-to-edge. It opens files through the XDG
//! `FileChooser` portal (or `--open PATH`), and — when a collab session is
//! attached — keeps playback in sync across peers via the shared [`media`]
//! transport.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager};
use ui::icons::Icon;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::video_player::{PlayerSetting, PlayerSettingEvent};
use ui::widget::{ActionEmitter, Button, ButtonFace, VideoPlayer, Widget};

use crate::actions::PlayerOperation;
use crate::collab::{CollabPlayback, Role};
use crate::preferences::{PlaybackPreferences, RecentMedia};

/// Inset (logical px) of the floating top overlay (media title + Open) from the
/// video edges.
const OVERLAY_MARGIN: f64 = 14.0;
/// Height (logical px) of the floating Open pill.
const OPEN_BTN_H: f64 = 32.0;
/// Width (logical px) of the floating Open pill.
const OPEN_BTN_W: f64 = 84.0;
/// A decoder open is asynchronous. Do not turn portal/direct admission into a
/// false successful history entry until a first picture has actually arrived.
const OPEN_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
/// One file-picker request at a time.  A portal response is asynchronous and
/// cannot be cancelled at the protocol level, so the app owns the receiver and
/// drops it when a newer direct open supersedes the request.  `generation` is
/// deliberately separate from the media generation: it identifies *this
/// chooser* rather than the media that may eventually be selected.
struct PendingPortalOpen {
    channel: PortalChannel,
    generation: u64,
    invocation: Option<qos_actions::InvocationId>,
}

/// A terminal response paired with the request identity and optional action
/// caller that owns it. Keeping this named makes `drain_portal`'s stale-result
/// guard readable without a deeply nested tuple type.
type PortalOpenOutcome = (
    u64,
    Option<qos_actions::InvocationId>,
    Result<PathBuf, Option<String>>,
);

/// A canonical open invocation awaiting its decoder's first picture. This is
/// separate from the file-picker request: the picker can succeed while decode
/// fails, stalls, or is superseded before a frame appears.
struct PendingMediaOpen {
    invocation: qos_actions::InvocationId,
    source_id: String,
    deadline: std::time::Instant,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum LibraryTab {
    Queue,
    Recent,
}

fn configured_player(autoplay: bool, loop_enabled: bool) -> VideoPlayer {
    let mut player = VideoPlayer::new()
        .with_engine(media::MediaEngine::new())
        .with_test_id("video_surface");
    player.enable_mpris("qos_video_player", "Video Player");
    configure_player_settings(&mut player, autoplay, loop_enabled);
    player
}

fn configure_player_settings(player: &mut VideoPlayer, autoplay: bool, loop_enabled: bool) {
    const RATES: [&str; 8] = ["0.25×", "0.5×", "0.75×", "1×", "1.25×", "1.5×", "2×", "4×"];
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
    let selected = [0.25_f32, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0]
        .iter()
        .position(|candidate| (*candidate - rate).abs() < f32::EPSILON)
        .unwrap_or(3);
    player.set_setting(PlayerSetting::choice(
        "playback-rate",
        Icon::Refresh,
        "Playback speed",
        0,
        RATES.into_iter().map(str::to_string).collect(),
        selected,
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

fn fill_rect(scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, transform, color, None, &rect);
}

#[allow(
    clippy::too_many_arguments,
    reason = "immediate-mode panel text needs its render context and explicit style"
)]
fn panel_text(
    scene: &mut Scene,
    text_renderer: &mut TextRenderer,
    transform: Affine,
    rect: Rect,
    text: &str,
    size: f32,
    color: Color,
    weight: TextWeight,
) {
    let _ = weight;
    text_renderer.draw_text(
        scene,
        text,
        size,
        color,
        transform * Affine::translate((rect.x0, rect.y0 + f64::from(size))),
        None,
    );
}

fn find_resource<'a>(
    resources: &'a [qos_actions::Resource],
    id: &str,
) -> Option<&'a qos_actions::Resource> {
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
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in source.to_uri().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
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
    /// The floating Open pill's hit rect (top-right of the video). Only live
    /// while the overlay is shown.
    open_button_rect: Rect,
    library_button_rect: Rect,
    library_panel_rect: Rect,
    library_queue_tab_rect: Rect,
    library_recent_tab_rect: Rect,
    library_close_rect: Rect,
    library_row_hits: Vec<LibraryRowHit>,
    library_clear_rect: Rect,
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
    resource_results: VecDeque<qos_actions::InvocationUpdate>,
    open_action: Option<ActionEmitter<()>>,
    library_control_invocations: VecDeque<qos_actions::ControlInvocation>,
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
    dirty: bool,
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
                Icon::Settings,
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
                Icon::Settings,
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
        let player = configured_player(preferences.autoplay, preferences.loop_enabled);
        let recent = crate::preferences::load_recent_history()
            .into_iter()
            .map(|entry| LocalMediaEntry {
                id: entry.id,
                title: entry.title,
                source: entry.source,
            })
            .collect();
        let action_ctx = crate::actions::PlayerResourceCtx {
            title: "No media",
            media: crate::actions::MediaAvailability {
                has_media: false,
                source_id: None,
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
            .and_then(|resource| qos_actions::ActionBinding::unit_for(resource, "open").ok())
            .map(ActionEmitter::new);
        Self {
            player,
            title: "No media".to_string(),
            rect: Rect::ZERO,
            open_button_rect: Rect::ZERO,
            library_button_rect: Rect::ZERO,
            library_panel_rect: Rect::ZERO,
            library_queue_tab_rect: Rect::ZERO,
            library_recent_tab_rect: Rect::ZERO,
            library_close_rect: Rect::ZERO,
            library_row_hits: Vec::new(),
            library_clear_rect: Rect::ZERO,
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
            current_source_id: None,
            source_generation: 0,
            search_cache: RefCell::new(None),
            search_catalog_builds: Cell::new(0),
            metadata_revision: 0,
            search_metadata: DecoderMetadata::default(),
            open_error: None,
            dirty: true,
        }
    }

    /// Open a filesystem `path` immediately (used by `--open PATH` and the
    /// portal outcome). Autoplays.
    #[must_use]
    pub fn with_open_path(mut self, path: &Path) -> Self {
        let _ = self.open_path(path);
        self
    }

    /// Build a player staged for a headless `--shots` capture: a source opened
    /// with its real engine state and the controls revealed. Opens
    /// `open` when given, else a synthesized demo clip. Never attaches a collab
    /// socket, so no thread/portal is created.
    ///
    /// `scenario` selects an optional demo overlay for the capture:
    ///   * `"empty"` — the first-run state before a source is chosen.
    ///   * `"error"` — a decoder/open failure before a picture arrives.
    ///   * `"native-error"` — use `--open` with a bad source to capture the
    ///     decoder's actual error event rather than fixture copy.
    ///   * `"playing"` / `"paused"` — explicit transport states (the
    ///     default remains paused for byte-stable review).
    ///   * `"settings"` — open the real autoplay/repeat settings menu.
    ///   * `"fullscreen"` — toggle fullscreen so the control bar shows its
    ///     immersive (exit-fullscreen) state.
    ///   * `"metadata"` — open the controls menu with deterministic embedded
    ///     audio/subtitle/chapter metadata.
    ///   * `"library"` / `"recent"` — populated device-local queue/history.
    ///   * `"library-focus"` — the same queue with a keyboard-visible Play
    ///     focus ring, for accessibility visual review.
    ///   * anything else / `None` — a plain player.
    pub fn for_shot(open: Option<PathBuf>, scenario: Option<&str>) -> Self {
        let mut app = Self::new();
        if !matches!(scenario, Some("empty" | "error")) {
            if let Some(path) = open {
                let _ = app.open_path(&path);
            } else {
                // The demo shot must stay hermetic: pin the deterministic
                // synthesized-frame engine so no real GStreamer pipeline is
                // built and the placeholder `demo.local` host is never
                // contacted — even though `decode` compiles the real backend in
                // by default. `--shots --open <file>` is the real-decode path.
                app.player.set_engine(media::MediaEngine::new_synthesized());
                let source = media::Source::Remote {
                    url: "https://demo.local/clip.mp4".into(),
                };
                let queue_id = app.ensure_queued(source.clone(), "Demo clip".into());
                let _ = app.set_current_source(&source);
                app.player.open(source);
                app.title = "Demo clip".to_string();
                app.current_queue_id = Some(queue_id);
                // Park at a fixed position for a stable reference frame. The
                // explicit `playing` fixture starts the real engine below;
                // default/`paused` deliberately stay deterministic.
                app.player.seek_to_ms(8_000);
                app.player.pause();
            }
        }
        match scenario {
            Some("error") => {
                app.open_error = Some(
                    "The local decoder could not read this video. Check the file and try again."
                        .into(),
                );
            }
            Some("playing") => app.player.play(),
            Some("paused") => app.player.pause(),
            Some("settings") => app.player.open_settings_menu(None),
            Some("metadata") => {
                app.shot_metadata = Some(DecoderMetadata {
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
                            label: "Introduction · 0:00".into(),
                            seconds: 0.0,
                        },
                        crate::actions::ChapterState {
                            id: "scene".into(),
                            label: "Second act · 0:45".into(),
                            seconds: 45.0,
                        },
                    ],
                });
                app.sync_decoder_settings();
                app.player.open_settings_menu(None);
            }
            Some("library" | "recent" | "library-focus") => {
                for (url, title) in [
                    ("https://demo.local/next.mp4", "Second act"),
                    ("https://demo.local/finale.mp4", "Closing scene"),
                ] {
                    app.ensure_queued(media::Source::Remote { url: url.into() }, title.into());
                }
                app.recent = app.queue.iter().rev().cloned().collect();
                app.library_visible = true;
                app.library_tab = if scenario == Some("recent") {
                    LibraryTab::Recent
                } else {
                    LibraryTab::Queue
                };
                if scenario == Some("library-focus") {
                    app.library_focus_id = app.queue.get(1).map(|entry| entry.id.clone());
                }
            }
            Some("fullscreen") => app.player.toggle_fullscreen(),
            _ => {}
        }
        if !matches!(scenario, Some("empty" | "error")) {
            app.player.reveal_controls();
        }
        app
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

    fn ensure_queued(&mut self, source: media::Source, title: String) -> String {
        if let Some(entry) = self
            .queue
            .iter()
            .find(|entry| entry.source.to_uri() == source.to_uri())
        {
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
        if let Err(error) = self.set_current_source(&source) {
            self.open_error = Some(error.clone());
            self.dirty = true;
            return Err(error);
        }
        self.cancel_pending_portal("a direct video open replaced the file picker");
        if let Some(pending) = self.media_open.take() {
            self.queue_resource_result(qos_actions::InvocationUpdate::Cancelled {
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
        let entries: Vec<RecentMedia> = self
            .recent
            .iter()
            .map(|entry| RecentMedia {
                id: entry.id.clone(),
                title: entry.title.clone(),
                source: entry.source.clone(),
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
            self.queue_resource_result(qos_actions::InvocationUpdate::Cancelled {
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
        self.current_queue_id = None;
        self.invalidate_search_cache();
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

    fn request_library_action(&mut self, resource_id: &str, action_id: &str) {
        let (audio, subtitles, chapters) = self.decoder_metadata();
        let resources = crate::actions::resources(
            &self.action_context(&audio, &subtitles, &chapters),
            &AutoCommit::new(),
        );
        let Some(resource) = find_resource(&resources, resource_id) else {
            return;
        };
        let Ok(binding) = qos_actions::ActionBinding::unit_for(resource, action_id) else {
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
            channel: video_open_request().send(),
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
            self.queue_resource_result(qos_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(message.into()),
            });
        }
    }

    fn await_open_frame(&mut self, invocation: qos_actions::InvocationId) -> Result<(), String> {
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
            deadline: std::time::Instant::now() + OPEN_FRAME_TIMEOUT,
        });
        Ok(())
    }

    fn poll_open_frame(
        &mut self,
        invocation: qos_actions::InvocationId,
    ) -> Option<qos_actions::InvocationUpdate> {
        let pending = self.media_open.as_ref()?;
        if pending.invocation != invocation {
            return None;
        }
        if self.current_source_id.as_deref() != Some(pending.source_id.as_str()) {
            let pending = self.media_open.take()?;
            return Some(qos_actions::InvocationUpdate::Failed {
                invocation: pending.invocation,
                error: qos_actions::ActionFailure {
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
            return Some(qos_actions::InvocationUpdate::Succeeded {
                invocation: pending.invocation,
                output: qos_actions::ActionOutput::default(),
            });
        }
        if std::time::Instant::now() >= pending.deadline {
            let pending = self.media_open.take()?;
            let message = "The decoder did not produce a first frame in time.".to_string();
            self.open_error = Some(message.clone());
            self.dirty = true;
            return Some(qos_actions::InvocationUpdate::Failed {
                invocation: pending.invocation,
                error: qos_actions::ActionFailure {
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
    fn queue_resource_result(&mut self, update: qos_actions::InvocationUpdate) {
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
                // `invoke_resource_operation` starts portal opens so it can
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
                self.player.reveal_controls();
            }
            PlayerOperation::Close => self.close_media(),
            PlayerOperation::TogglePlayback => self.player.toggle_play(),
            PlayerOperation::ToggleMute => self.player.toggle_mute(),
            PlayerOperation::Reveal => self.player.reveal_controls(),
            PlayerOperation::SeekSeconds(seconds)
            | PlayerOperation::JumpToChapter { seconds, .. } => {
                self.player.seek_to_ms((seconds.max(0.0) * 1000.0) as u64);
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
                let selected = [0.25_f64, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0]
                    .iter()
                    .position(|candidate| (*candidate - rate).abs() < f64::EPSILON);
                if let Some(selected) = selected {
                    self.player.set_choice_selection("playback-rate", selected);
                }
            }
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
            _ => {}
        }
        Ok(())
    }

    /// Drain a pending portal outcome, opening the chosen file.
    fn drain_portal(&mut self) {
        let pending = self.portal_open.take();
        let mut outcome: Option<PortalOpenOutcome> = None;
        if let Some(request) = pending.as_ref() {
            match request.channel.try_recv() {
                Ok(PortalOutcome::Opened(p)) => {
                    outcome = Some((request.generation, request.invocation, Ok(p)));
                }
                Ok(PortalOutcome::Cancelled) => {
                    outcome = Some((request.generation, request.invocation, Err(None)));
                }
                Ok(PortalOutcome::Failed(error)) => {
                    outcome = Some((request.generation, request.invocation, Err(Some(error))));
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
                    self.queue_resource_result(qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
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
                Ok(path) => self.open_path(&path).map(|()| true),
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
                            self.queue_resource_result(qos_actions::InvocationUpdate::Failed {
                                invocation,
                                error: qos_actions::ActionFailure {
                                    code: "video_player.open_lifecycle_failed".into(),
                                    message,
                                    details: None,
                                    retryable: true,
                                },
                            });
                        }
                    }
                    Ok(false) => {
                        self.queue_resource_result(qos_actions::InvocationUpdate::Cancelled {
                            invocation,
                            message: Some("video selection was cancelled".into()),
                        });
                    }
                    Err(message) => {
                        self.queue_resource_result(qos_actions::InvocationUpdate::Failed {
                            invocation,
                            error: qos_actions::ActionFailure {
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
        let now = media::now_ms();
        match collab.role() {
            Role::Host => {
                if let Some(engine) = self.player.engine()
                    && let Some(delta) = collab.host_step(engine, now)
                {
                    mgr.send(CollabRequest::DocumentIncremental { data: delta });
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
                    const RATES: [f32; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0];
                    if self
                        .collab
                        .as_ref()
                        .is_none_or(|collab| collab.role() == Role::Host)
                        && let Some(rate) = RATES.get(index)
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
                    self.open_error = Some(message.clone());
                    self.dirty = true;
                    if let Some(pending) = self.media_open.take() {
                        self.queue_resource_result(qos_actions::InvocationUpdate::Failed {
                            invocation: pending.invocation,
                            error: qos_actions::ActionFailure {
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

    /// Whether the floating overlay (media title + Open pill) should show this
    /// frame. It rides the player's own controls visibility, so it auto-hides
    /// during immersive playback — and it stays up in the empty state (no frame
    /// decoded yet) so there is always a way to open a file.
    fn overlay_visible(&self) -> bool {
        self.player.controls_visible() || self.player.latest_frame().is_none()
    }

    /// Paint the floating top overlay: the media title (left) over a soft scrim
    /// and the Open pill (right). Drawn on top of the video, only when
    /// [`Self::overlay_visible`]. There is no persistent chrome bar — the video
    /// is edge-to-edge (HIG §1e).
    fn paint_overlay(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let opacity = if self.player.has_media() {
            self.player.controls_opacity()
        } else {
            1.0
        };
        if opacity <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            opacity,
            transform,
            &self.rect,
        );
        // A soft translucent scrim behind the title so it stays legible over
        // bright frames, without a hard opaque bar.
        let scrim_h = 44.0;
        let scrim = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + scrim_h,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            vello::peniko::Color::from_rgba8(0, 0, 0, 90),
            None,
            &scrim,
        );
        // Title (left), vertically centred in the scrim. The scrim is always
        // dark (over-media chrome), so the label is a fixed near-white in every
        // theme — `text_primary` would flip to near-black on light and vanish
        // against the dark scrim.
        let title_size = 15.0f32;
        let ty = self.rect.y0 + (scrim_h - f64::from(title_size)) * 0.5;
        let display_title = if self.player.has_media() {
            self.title.as_str()
        } else {
            "Video Player"
        };
        let title_x = self.library_button_rect.x1 + 12.0;
        let title_budget = (self.open_button_rect.x0 - title_x - OVERLAY_MARGIN)
            .max(0.0)
            .min(f64::from(f32::MAX)) as f32;
        let display_title = text_renderer.elide_to_width(display_title, title_size, title_budget);
        text_renderer.draw_text(
            scene,
            &display_title,
            title_size,
            vello::peniko::Color::from_rgb8(245, 246, 250),
            transform * Affine::translate((title_x, ty)),
            None,
        );
        if self.library_button_rect.width() > 0.0 {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.library_button_rect, "Library")
                    .ghost()
                    .text_size(14.0)
                    .focused(self.library_focused)
                    .hovered(self.library_button_rect.contains(self.last_cursor)),
            );
        }
        // Open pill (right), a floating rounded accent button — the shared
        // primary Button face, so it gains hover/press feedback over the video.
        if self.open_button_rect.width() > 0.0 {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.open_button_rect, "Open")
                    .radius(OPEN_BTN_H / 2.0)
                    .text_size(14.0)
                    .weight(TextWeight::Regular)
                    .primary()
                    .focused(self.open_focused)
                    .hovered(self.open_button_rect.contains(self.last_cursor)),
            );
        }
        scene.pop_layer();
    }

    fn paint_media_status_copy(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.player.latest_frame().is_some() || self.rect.height() < 180.0 {
            return;
        }
        let center = self.rect.center();
        let (title, subtitle) = if let Some(error) = &self.open_error {
            ("Couldn't open video", error.as_str())
        } else if self.player.has_media() {
            ("Loading video…", "Preparing the first frame")
        } else {
            ("Open a video to start", "MP4, WebM, MKV, MOV and AVI")
        };
        let copy_width = (self.rect.width() - 32.0).max(0.0) as f32;
        let title = text_renderer.elide_to_width(title, 20.0, copy_width);
        let subtitle = text_renderer.elide_to_width(subtitle, 13.0, copy_width);
        let (title_w, _) = text_renderer.measure(&title, 20.0, None);
        let (subtitle_w, _) = text_renderer.measure(&subtitle, 13.0, None);
        text_renderer.draw_text(
            scene,
            &title,
            20.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((center.x - f64::from(title_w) * 0.5, center.y + 55.0)),
            None,
        );
        text_renderer.draw_text(
            scene,
            &subtitle,
            13.0,
            theme.tokens.colors.text_dim,
            transform
                * Affine::translate((center.x - f64::from(subtitle_w) * 0.5, center.y + 85.0)),
            None,
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the compact panel paints and records one matching set of interaction geometry"
    )]
    fn paint_library_panel(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.library_visible || self.library_panel_rect.width() <= 0.0 {
            return;
        }
        let colors = &theme.tokens.colors;
        let panel = self.library_panel_rect;
        fill_rect(scene, transform, panel, colors.surfaces.raised);
        fill_rect(
            scene,
            transform,
            Rect::new(panel.x0, panel.y0, panel.x0 + 1.0, panel.y1),
            colors.border_standard,
        );
        panel_text(
            scene,
            text_renderer,
            transform,
            Rect::new(
                panel.x0 + 16.0,
                panel.y0 + 10.0,
                panel.x1 - 48.0,
                panel.y0 + 34.0,
            ),
            "Library",
            18.0,
            colors.text_primary,
            TextWeight::Semibold,
        );
        panel_text(
            scene,
            text_renderer,
            transform,
            Rect::new(
                panel.x0 + 16.0,
                panel.y0 + 34.0,
                panel.x1 - 12.0,
                panel.y0 + 54.0,
            ),
            "Local to this device · paths never shared",
            12.0,
            colors.text_dim,
            TextWeight::Regular,
        );
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(self.library_close_rect, "Close")
                .ghost()
                .text_size(12.0)
                .hovered(self.library_close_rect.contains(self.last_cursor)),
        );
        self.library_clear_rect = Rect::new(
            panel.x1 - 124.0,
            panel.y0 + 10.0,
            panel.x1 - 74.0,
            panel.y0 + 38.0,
        );
        if self.library_tab == LibraryTab::Recent && !self.recent.is_empty() {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.library_clear_rect, "Clear")
                    .ghost()
                    .text_size(12.0)
                    .focused(self.library_focus_id.as_deref() == Some("library-clear"))
                    .hovered(self.library_clear_rect.contains(self.last_cursor)),
            );
        }
        let queue_active = self.library_tab == LibraryTab::Queue;
        for (label, rect, active) in [
            ("Queue", self.library_queue_tab_rect, queue_active),
            ("Recent", self.library_recent_tab_rect, !queue_active),
        ] {
            let mut face = ButtonFace::new(rect, label).ghost().text_size(13.0);
            if active {
                face = face.primary();
            }
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &face.hovered(rect.contains(self.last_cursor)),
            );
        }
        self.library_row_hits.clear();
        let entries: Vec<LocalMediaEntry> = match self.library_tab {
            LibraryTab::Queue => self.queue.clone(),
            LibraryTab::Recent => self.recent.iter().cloned().collect(),
        };
        let mut y = panel.y0 + 108.0;
        let row_h = 62.0;
        if entries.is_empty() {
            panel_text(
                scene,
                text_renderer,
                transform,
                Rect::new(panel.x0 + 20.0, y, panel.x1 - 20.0, y + 44.0),
                if self.library_tab == LibraryTab::Queue {
                    "Your queue is empty. Open a video to add it here."
                } else {
                    "Recently played videos will appear here after a frame is decoded."
                },
                13.0,
                colors.text_dim,
                TextWeight::Regular,
            );
            return;
        }
        for entry in entries
            .into_iter()
            .take(crate::actions::CONTEXTUAL_CHILD_LIMIT)
        {
            if y + row_h > panel.y1 - 14.0 {
                break;
            }
            let rect = Rect::new(panel.x0 + 12.0, y, panel.x1 - 12.0, y + row_h - 4.0);
            let remove = (self.library_tab == LibraryTab::Queue).then_some(Rect::new(
                rect.x1 - 106.0,
                rect.y0 + 7.0,
                rect.x1 - 56.0,
                rect.y0 + 51.0,
            ));
            let play = Rect::new(rect.x1 - 50.0, rect.y0 + 7.0, rect.x1 - 2.0, rect.y0 + 51.0);
            if rect.contains(self.last_cursor) {
                fill_rect(scene, transform, rect, colors.neutral[2]);
            }
            let active = self.current_queue_id.as_deref() == Some(entry.id.as_str());
            let label =
                text_renderer.elide_to_width(&entry.title, 14.0, (rect.width() - 120.0) as f32);
            panel_text(
                scene,
                text_renderer,
                transform,
                Rect::new(
                    rect.x0 + 12.0,
                    rect.y0 + 8.0,
                    remove.map_or(play.x0, |remove| remove.x0) - 8.0,
                    rect.y0 + 29.0,
                ),
                &label,
                14.0,
                if active {
                    colors.accent[6]
                } else {
                    colors.text_primary
                },
                TextWeight::Medium,
            );
            panel_text(
                scene,
                text_renderer,
                transform,
                Rect::new(
                    rect.x0 + 12.0,
                    rect.y0 + 29.0,
                    remove.map_or(play.x0, |remove| remove.x0) - 8.0,
                    rect.y1 - 4.0,
                ),
                if active {
                    "Playing on this device"
                } else {
                    "Local video"
                },
                12.0,
                colors.text_dim,
                TextWeight::Regular,
            );
            if let Some(remove) = remove {
                Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ButtonFace::new(remove, "Remove")
                        .ghost()
                        .text_size(11.0)
                        .focused(
                            self.library_focus_id.as_deref()
                                == Some(format!("library-remove-{}", entry.id).as_str()),
                        )
                        .hovered(remove.contains(self.last_cursor)),
                );
            }
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(
                    play,
                    if self.library_tab == LibraryTab::Recent {
                        "Replay"
                    } else {
                        "Play"
                    },
                )
                .primary()
                .text_size(11.0)
                .focused(self.library_focus_id.as_deref() == Some(entry.id.as_str()))
                .hovered(play.contains(self.last_cursor)),
            );
            self.library_row_hits.push(LibraryRowHit {
                id: entry.id,
                row: rect,
                play,
                remove,
            });
            y += row_h;
        }
    }
}

impl Default for PlayerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for PlayerApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::OPERATION_TYPE]
    }

    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        let (audio_tracks, subtitle_tracks, chapters) = self.decoder_metadata();
        crate::actions::resources(
            &self.action_context(&audio_tracks, &subtitle_tracks, &chapters),
            doc,
        )
    }

    fn resource(&self, _doc: &AutoCommit, id: &str) -> Option<qos_actions::Resource> {
        let (audio_tracks, subtitle_tracks, chapters) = self.decoder_metadata();
        crate::actions::resource(
            &self.action_context(&audio_tracks, &subtitle_tracks, &chapters),
            id,
        )
    }

    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        let source_id = self.current_source_id.clone();
        let revision = self.metadata_revision;
        let mut cache = self.search_cache.borrow_mut();
        let reusable = cache
            .as_ref()
            .is_some_and(|cached| cached.source_id == source_id && cached.revision == revision);
        if !reusable {
            let provider = crate::search::VideoPlayerSearchProvider::new(&self.action_context(
                &self.search_metadata.audio_tracks,
                &self.search_metadata.subtitle_tracks,
                &self.search_metadata.chapters,
            ));
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

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == qos_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode(type_id, payload)? else {
            return Ok(false);
        };
        if operation.requires_media() && !self.player.has_media() {
            return Err("open a video before using this playback control".into());
        }
        self.validate_dynamic_target(&operation)?;
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

    fn start_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let operation = crate::actions::decode(type_id, payload)?;
        if !self.invoke_resource_operation(kind, type_id, payload, doc, path)? {
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
                qos_actions::ActionOutput::default(),
            )))
        }
    }

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
        self.drain_portal();
        if let Some(update) = self.poll_open_frame(invocation) {
            return Some(update);
        }
        self.resource_results
            .iter()
            .position(|update| update.invocation() == invocation)
            .and_then(|index| self.resource_results.remove(index))
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.drain_portal();
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
        // Floating Open pill: top-right corner, over the video.
        let by0 = self.rect.y0 + OVERLAY_MARGIN;
        let bx1 = self.rect.x1 - OVERLAY_MARGIN;
        let bx0 = (bx1 - OPEN_BTN_W).max(self.rect.x0);
        self.open_button_rect = Rect::new(bx0, by0, bx1, by0 + OPEN_BTN_H);
        self.library_button_rect = Rect::new(
            self.rect.x0 + OVERLAY_MARGIN,
            by0,
            (self.rect.x0 + OVERLAY_MARGIN + 88.0).min(bx0 - OVERLAY_MARGIN),
            by0 + OPEN_BTN_H,
        );
        let panel_width = if self.rect.width() < 640.0 {
            self.rect.width()
        } else {
            360.0_f64.min(self.rect.width())
        };
        self.library_panel_rect = Rect::new(
            self.rect.x1 - panel_width,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        );
        self.library_close_rect = Rect::new(
            self.library_panel_rect.x1 - 68.0,
            self.library_panel_rect.y0 + 10.0,
            self.library_panel_rect.x1 - 12.0,
            self.library_panel_rect.y0 + 38.0,
        );
        self.library_queue_tab_rect = Rect::new(
            self.library_panel_rect.x0 + 12.0,
            self.library_panel_rect.y0 + 66.0,
            self.library_panel_rect.x0 + 98.0,
            self.library_panel_rect.y0 + 96.0,
        );
        self.library_recent_tab_rect = Rect::new(
            self.library_panel_rect.x0 + 104.0,
            self.library_panel_rect.y0 + 66.0,
            self.library_panel_rect.x0 + 196.0,
            self.library_panel_rect.y0 + 96.0,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.player
            .draw(scene, transform, text_renderer, theme, presence);
        self.paint_media_status_copy(scene, transform, text_renderer, theme);
        if self.overlay_visible() {
            self.paint_overlay(scene, transform, text_renderer, theme);
        }
        self.paint_library_panel(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if self.library_visible && self.library_panel_rect.contains(self.last_cursor) {
                if self.library_close_rect.contains(self.last_cursor) {
                    self.library_visible = false;
                } else if self.library_queue_tab_rect.contains(self.last_cursor) {
                    self.library_tab = LibraryTab::Queue;
                } else if self.library_recent_tab_rect.contains(self.last_cursor) {
                    self.library_tab = LibraryTab::Recent;
                } else if self.library_tab == LibraryTab::Recent
                    && self.library_clear_rect.contains(self.last_cursor)
                {
                    self.request_library_clear();
                } else if let Some(row) = self
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
                *handled = true;
                return;
            }
            // Only intercept the Open pill while the overlay is actually shown;
            // during immersive playback that corner is just video.
            if self.overlay_visible() && self.open_button_rect.contains(self.last_cursor) {
                self.request_open_action();
                *handled = true;
                return;
            }
            if self.overlay_visible() && self.library_button_rect.contains(self.last_cursor) {
                self.library_visible = !self.library_visible;
                self.player.reveal_controls();
                self.dirty = true;
                *handled = true;
                return;
            }
        }
        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == ElementState::Pressed
            && modifiers.state().control_key()
            && matches!(&event.logical_key, winit::keyboard::Key::Character(c) if c.eq_ignore_ascii_case("o"))
        {
            self.request_open_action();
            *handled = true;
            return;
        }
        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == ElementState::Pressed
            && matches!(&event.logical_key, winit::keyboard::Key::Character(c) if c.eq_ignore_ascii_case("l"))
        {
            self.library_visible = !self.library_visible;
            self.player.reveal_controls();
            self.dirty = true;
            *handled = true;
            return;
        }
        // Forward everything else to the player (its controls + shortcuts).
        let Ok(children) = taffy.children(node_id) else {
            return;
        };
        if let Some(&player_node) = children.first() {
            self.player.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                player_node,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.player as &dyn Widget]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.player as &mut (dyn Widget + '_)]
    }

    fn take_control_invocation(&mut self) -> Option<qos_actions::ControlInvocation> {
        self.open_action
            .as_mut()
            .and_then(ActionEmitter::take)
            .or_else(|| self.library_control_invocations.pop_front())
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<qos_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.open_action {
            action.refresh_availability(allowed);
        }
    }

    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        self.open_action.as_ref().map_or_else(Vec::new, |action| {
            vec![action.metadata("video-player.open")]
        })
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name("Open video")
                .with_description("Ctrl+O")
                .with_action_target("video-player.open")
                .with_bounds(self.open_button_rect)
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
            children.extend(self.library_row_hits.iter().map(|row| {
                let id = &row.id;
                let title = self
                    .queue
                    .iter()
                    .chain(self.recent.iter())
                    .find(|entry| entry.id == *id)
                    .map_or("Local video", |entry| entry.title.as_str());
                AccessNode::new(Role::Button)
                    .with_name(format!("Play {title}"))
                    .with_description("Opens this device-local video")
                    .with_action_target(format!("video-player.library.play.{id}"))
                    .with_bounds(row.play)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.library_focus_id.as_deref() == Some(id.as_str()),
                        ..Default::default()
                    })
            }));
            if self.library_tab == LibraryTab::Recent && !self.recent.is_empty() {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name("Clear recent videos")
                        .with_description("Removes recent-history entries from this device")
                        .with_action_target("video-player.library.clear")
                        .with_bounds(self.library_clear_rect)
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.library_focus_id.as_deref() == Some("library-clear"),
                            ..Default::default()
                        }),
                );
            }
            children.extend(self.library_row_hits.iter().filter_map(|row| {
                row.remove.map(|remove| {
                    AccessNode::new(Role::Button)
                        .with_name("Remove from queue")
                        .with_description("Removes this item from the session-only queue")
                        .with_action_target(format!("video-player.library.remove.{}", row.id))
                        .with_bounds(remove)
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.library_focus_id.as_deref()
                                == Some(format!("library-remove-{}", row.id).as_str()),
                            ..Default::default()
                        })
                })
            }));
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
            [0] => Some(self.open_button_rect),
            [1] => Some(self.library_button_rect),
            [index] => {
                let library_index = index.saturating_sub(2);
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
            [index] => {
                let library_index = index.saturating_sub(2);
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
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if target == "video-player.open" && matches!(action, ui::a11y::Action::Activate) {
            self.request_open_action();
            true
        } else if target == "video-player.library" && matches!(action, ui::a11y::Action::Activate) {
            self.library_visible = !self.library_visible;
            self.player.reveal_controls();
            self.dirty = true;
            true
        } else if let Some(item_id) = target.strip_prefix("video-player.library.play.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_play(item_id);
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
        if self.overlay_visible() && self.open_button_rect.contains(self.last_cursor) {
            Some("Open video (Ctrl+O)")
        } else if self.overlay_visible() && self.library_button_rect.contains(self.last_cursor) {
            Some("Show queue and recent videos (L)")
        } else if self.library_visible
            && self.library_tab == LibraryTab::Recent
            && self.library_clear_rect.contains(self.last_cursor)
        {
            Some("Clear recent videos from this device")
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
            || self.portal_open.is_some()
            || self.media_open.is_some()
    }

    fn needs_idle_sync(&self) -> bool {
        // A follower must keep syncing even with no local input so it applies
        // the host's incoming transport.
        self.collab.is_some()
    }

    fn fullscreen_intent(&self) -> Option<bool> {
        self.player.fullscreen_intent()
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        self.player.fullscreen_params()
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        self.player.on_fullscreen_changed(active);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_for_path_splits_local_vs_workspace() {
        // Point the mount at a scratch root so the classification is testable.
        let root = std::env::temp_dir().join(format!("qos-vp-app-{}", std::process::id()));
        unsafe { std::env::set_var("QOS_WORKSPACE_ROOT", &root) };
        let ws = source_for_path(&root.join("media/a.mp4"));
        assert!(matches!(ws, media::Source::Workspace(_)));
        let local = source_for_path(Path::new("/home/u/b.mp4"));
        assert!(matches!(local, media::Source::LocalFile(_)));
        unsafe { std::env::remove_var("QOS_WORKSPACE_ROOT") };
    }

    #[test]
    fn opening_a_path_sets_title_and_loads_the_engine() {
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

    #[test]
    fn portal_outcome_opens_the_chosen_file() {
        let mut app = PlayerApp::new();
        app.portal_generation = 1;
        app.portal_open = Some(PendingPortalOpen {
            channel: ui::portal::inject_for_test(PortalOutcome::Opened(PathBuf::from(
                "/tmp/Chosen.webm",
            ))),
            generation: 1,
            invocation: None,
        });
        app.drain_portal();
        assert_eq!(app.title, "Chosen.webm");
        assert!(
            app.portal_open.is_none(),
            "channel cleared after the outcome"
        );
    }

    #[test]
    fn portal_open_is_single_flight_and_a_direct_open_supersedes_it() {
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

    #[test]
    fn portal_generation_exhaustion_fails_closed_without_aliasing_old_requests() {
        let mut app = PlayerApp::new();
        app.portal_generation = u64::MAX;
        let error = app.trigger_open().expect_err("generation must not wrap");
        assert!(error.contains("generation exhausted"));
        assert!(app.portal_open.is_none());
        assert_eq!(app.portal_generation, u64::MAX);
    }

    #[test]
    fn source_identity_generation_exhaustion_refuses_the_new_open() {
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

    #[test]
    fn local_source_identity_and_now_playing_never_expose_an_absolute_path() {
        let mut app = synth_app();
        let path = Path::new("/home/alice/Private videos/launch-cut.mp4");
        app.open_path(path)
            .expect("local source opens in the synthesized engine");
        let source_id = app.current_source_id.as_deref().unwrap_or_default();
        assert!(source_id.starts_with("local-media-"));
        assert!(!source_id.contains("/home/alice"));
        assert!(!source_id.contains("launch-cut"));
    }

    #[test]
    fn metadata_revision_boundary_discards_the_cache_before_resetting() {
        let mut app = PlayerApp::for_shot(None, Some("metadata"));
        let _first = app.search_providers();
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

        let _second = app.search_providers();
        assert_eq!(
            app.search_catalog_builds.get(),
            2,
            "the fresh post-boundary provider is rebuilt instead of reusing stale metadata"
        );
    }

    #[test]
    fn stale_portal_outcome_cannot_open_media_in_release_or_debug_builds() {
        let mut app = PlayerApp::new();
        app.portal_generation = 2;
        app.portal_open = Some(PendingPortalOpen {
            channel: ui::portal::inject_for_test(PortalOutcome::Opened(PathBuf::from(
                "/tmp/Stale.webm",
            ))),
            generation: 1,
            invocation: None,
        });
        app.drain_portal();
        assert_eq!(app.title, "No media");
        assert!(app.current_source_id.is_none());
        assert!(app.portal_open.is_none());
    }

    #[test]
    fn completed_portal_result_backpressures_new_async_requests_until_polled() {
        let mut app = PlayerApp::new();
        app.queue_resource_result(qos_actions::InvocationUpdate::Cancelled {
            invocation: qos_actions::InvocationId::new(),
            message: Some("test completion".into()),
        });
        let error = app
            .trigger_open()
            .expect_err("unpolled completion applies backpressure");
        assert!(error.contains("awaiting host acknowledgement"));
        assert!(app.portal_open.is_none());
        assert_eq!(app.resource_results.len(), 1, "the completion is retained");
    }

    #[test]
    fn portal_failure_is_retained_for_the_empty_state() {
        let mut app = PlayerApp::new();
        app.portal_generation = 1;
        app.portal_open = Some(PendingPortalOpen {
            channel: ui::portal::inject_for_test(PortalOutcome::Failed("permission denied".into())),
            generation: 1,
            invocation: None,
        });
        app.drain_portal();
        assert_eq!(app.open_error.as_deref(), Some("permission denied"));
        assert!(app.player.latest_frame().is_none());
    }

    #[test]
    fn open_affordance_emits_the_advertised_host_routable_effect() {
        let mut app = PlayerApp::new();
        app.request_open_action();
        let invocation = Widget::take_control_invocation(&mut app).expect("open intent");
        assert_eq!(invocation.address.resource, "video-player.application");
        assert_eq!(invocation.address.action, "open");
        assert!(
            app.portal_open.is_none(),
            "the control did not bypass the host"
        );
    }

    #[test]
    fn library_row_emits_its_canonical_play_action_without_opening_directly() {
        let mut app = PlayerApp::for_shot(None, Some("library"));
        let item_id = app.queue[1].id.clone();
        let source_before = app.current_source_id.clone();
        app.request_library_play(&item_id);
        let invocation = Widget::take_control_invocation(&mut app).expect("queue play intent");
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

    #[test]
    fn library_remove_and_clear_controls_emit_their_canonical_host_actions() {
        let mut app = PlayerApp::for_shot(None, Some("library"));
        let item_id = app.queue[1].id.clone();
        app.request_library_remove(&item_id);
        let remove = Widget::take_control_invocation(&mut app).expect("remove intent");
        assert_eq!(
            remove.address.resource,
            crate::actions::queue_item_resource_id(&item_id)
        );
        assert_eq!(remove.address.action, "remove");
        app.request_library_clear();
        let clear = Widget::take_control_invocation(&mut app).expect("clear intent");
        assert_eq!(clear.address.resource, "video-player.history.local");
        assert_eq!(clear.address.action, "clear");
    }

    #[test]
    fn library_play_focus_has_a_real_accessibility_target_and_visual_state() {
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
        assert!(Widget::focus_synthetic_accessibility(&mut app, &[3]));
        assert_eq!(app.library_focus_id.as_deref(), Some(focused_id.as_str()));
        assert_eq!(
            Widget::synthetic_accessibility_rect(&app, &[3]),
            Some(Rect::new(0.0, 44.0, 44.0, 88.0))
        );
    }

    #[test]
    fn resources_cover_transport_navigation_audio_metadata_and_settings() {
        let mut app = synth_app();
        app.open_path(Path::new("/tmp/Clip.mp4"))
            .expect("source identity available");
        let resources = Widget::resources(&app, &AutoCommit::new());
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

    #[test]
    fn follower_rejects_transport_operations_even_if_invoked_out_of_band() {
        let mut app = synth_app();
        app.open_path(Path::new("/tmp/Clip.mp4"))
            .expect("source identity available");
        app.collab = Some(CollabPlayback::new(Role::Follower));
        app.player.set_transport_enabled(false);
        let encoded = qos_actions::EncodedAction::encode_operation(
            crate::actions::OPERATION_TYPE,
            &crate::actions::PlayerOperation::TogglePlayback,
        )
        .unwrap();
        let result = Widget::invoke_resource_operation(
            &mut app,
            qos_actions::OperationKind::View,
            &encoded.type_id,
            &encoded.payload,
            &AutoCommit::new(),
            &automerge::ROOT,
        );
        assert!(result.is_err());
    }

    #[test]
    fn unavailable_transport_is_rejected_even_if_a_caller_bypasses_discovery() {
        let mut app = PlayerApp::new();
        let encoded = qos_actions::EncodedAction::encode_operation(
            crate::actions::OPERATION_TYPE,
            &crate::actions::PlayerOperation::TogglePlayback,
        )
        .unwrap();
        let result = Widget::invoke_resource_operation(
            &mut app,
            qos_actions::OperationKind::View,
            &encoded.type_id,
            &encoded.payload,
            &AutoCommit::new(),
            &automerge::ROOT,
        );
        assert!(result.is_err());
    }

    #[test]
    fn idle_collaboration_does_not_turn_a_paused_player_into_an_animation() {
        let mut app = PlayerApp::new();
        app.collab = Some(CollabPlayback::new(Role::Host));
        assert!(Widget::needs_idle_sync(&app));
        assert!(
            !Widget::needs_animation_frame(&app),
            "collab receives idle sync without requesting a 60 Hz redraw"
        );
    }

    fn root() -> ObjId {
        automerge::ROOT
    }

    #[test]
    fn fresh_player_shows_a_clean_empty_state_with_no_frame() {
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
    fn opening_a_file_then_syncing_lands_a_frame_and_duration() {
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

    #[test]
    fn direct_open_is_accepted_until_a_decoder_frame_confirms_success() {
        let mut app = synth_app();
        let path = std::env::temp_dir().join(format!(
            "qos-video-player-open-lifecycle-{}-{}.mp4",
            std::process::id(),
            media::now_ms()
        ));
        std::fs::write(&path, []).expect("create a portal-equivalent local file");
        let operation = PlayerOperation::Open {
            path: Some(path.to_string_lossy().into_owned()),
        };
        let payload = serde_json::to_vec(&operation).expect("serialize operation");
        let invocation = qos_actions::InvocationId::new();
        let doc = AutoCommit::new();
        let started = Widget::start_resource_operation(
            &mut app,
            invocation,
            qos_actions::OperationKind::Effect,
            crate::actions::OPERATION_TYPE,
            &payload,
            &doc,
            &root(),
        )
        .expect("direct open admits");
        assert_eq!(
            started,
            Some(ui::widget::ResourceOperationStart::Accepted),
            "path validation is admission, not decoder completion"
        );
        assert!(
            Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root()).is_none()
        );
        let mut mutable_doc = doc;
        app.sync(&mut mutable_doc, &root());
        assert!(matches!(
            Widget::poll_resource_operation(&mut app, invocation, false, &mutable_doc, &root()),
            Some(qos_actions::InvocationUpdate::Succeeded { .. })
        ));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn queued_item_uses_the_same_pending_decoder_lifecycle_as_direct_open() {
        let mut app = PlayerApp::for_shot(None, Some("library"));
        let item_id = app.queue[1].id.clone();
        let payload = serde_json::to_vec(&PlayerOperation::OpenQueueItem {
            item_id: item_id.clone(),
        })
        .expect("serialize queue operation");
        let invocation = qos_actions::InvocationId::new();
        let mut doc = AutoCommit::new();
        assert_eq!(
            Widget::start_resource_operation(
                &mut app,
                invocation,
                qos_actions::OperationKind::Effect,
                crate::actions::OPERATION_TYPE,
                &payload,
                &doc,
                &root(),
            )
            .expect("queue item admits"),
            Some(ui::widget::ResourceOperationStart::Accepted)
        );
        assert_eq!(app.current_queue_id.as_deref(), Some(item_id.as_str()));
        assert!(
            Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root()).is_none(),
            "admission is still not a terminal playback success"
        );
        app.sync(&mut doc, &root());
        assert!(matches!(
            Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root()),
            Some(qos_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert_eq!(
            app.recent.front().map(|entry| entry.id.as_str()),
            Some(item_id.as_str())
        );
    }

    #[test]
    fn admitted_direct_open_reports_a_decoder_timeout_instead_of_false_success() {
        let mut app = synth_app();
        let path = std::env::temp_dir().join(format!(
            "qos-video-player-open-timeout-{}-{}.mp4",
            std::process::id(),
            media::now_ms()
        ));
        std::fs::write(&path, []).expect("create a portal-equivalent local file");
        let payload = serde_json::to_vec(&PlayerOperation::Open {
            path: Some(path.to_string_lossy().into_owned()),
        })
        .expect("serialize operation");
        let invocation = qos_actions::InvocationId::new();
        let doc = AutoCommit::new();
        assert_eq!(
            Widget::start_resource_operation(
                &mut app,
                invocation,
                qos_actions::OperationKind::Effect,
                crate::actions::OPERATION_TYPE,
                &payload,
                &doc,
                &root(),
            )
            .expect("direct open admits"),
            Some(ui::widget::ResourceOperationStart::Accepted)
        );
        app.media_open
            .as_mut()
            .expect("pending decoder open")
            .deadline = std::time::Instant::now();
        assert!(matches!(
            Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root()),
            Some(qos_actions::InvocationUpdate::Failed { error, .. })
                if error.code == "video_player.open_timed_out"
        ));
        assert_eq!(
            app.open_error.as_deref(),
            Some("The decoder did not produce a first frame in time.")
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn autoplay_and_repeat_are_real_settings_not_demo_affordances() {
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

    #[test]
    fn playhead_advances_while_playing_and_a_mid_play_seek_holds() {
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

    #[test]
    fn volume_and_mute_propagate_to_the_engine() {
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

    #[test]
    fn playback_speed_action_changes_the_engine_and_collaborative_snapshot() {
        let mut app = synth_app();
        app.open_path(Path::new("/tmp/Clip.mp4"))
            .expect("source identity available");
        app.perform_resource_operation(crate::actions::PlayerOperation::SetPlaybackRate(2.0));
        let engine = app.player.engine().expect("engine");
        assert!((engine.rate() - 2.0).abs() < f32::EPSILON);
        assert!((engine.transport_state(media::now_ms()).rate - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn space_key_routed_through_the_app_toggles_playback() {
        let mut app = synth_app();
        app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
            .expect("source identity available");

        // Lay the app out so `handle_event` can find the player child node.
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(640.0),
                    height: taffy::AvailableSpace::Definite(480.0),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, Point::ZERO);
        app.player.set_focus(true);

        let was_playing = app.player.is_playing();
        let mut doc = AutoCommit::new();
        let r = root();
        let mods = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus_target = None;
        let space = {
            let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = winit::keyboard::Key::Named(winit::keyboard::NamedKey::Space);
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = ElementState::Pressed;
            ev.repeat = false;
            ev.text = None;
            WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            }
        };
        app.handle_event(
            &space,
            1.0,
            &mods,
            &mut doc,
            &r,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        assert!(handled, "the app forwarded the key to the player");
        assert_ne!(
            app.player.is_playing(),
            was_playing,
            "Space toggled playback via the app"
        );
    }

    #[test]
    fn repeated_search_provider_retrieval_does_not_reindex_metadata() {
        let app = PlayerApp::for_shot(None, Some("metadata"));
        let first = app.search_providers();
        assert_eq!(app.search_catalog_builds.get(), 1);
        let second = app.search_providers();
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

    #[test]
    fn search_reveal_operation_never_changes_transport() {
        let mut app = PlayerApp::for_shot(None, None);
        app.player.pause();
        let position = app.player.position_ms();
        app.perform_resource_operation(PlayerOperation::Reveal);
        assert!(!app.player.is_playing());
        assert_eq!(app.player.position_ms(), position);
    }

    #[test]
    fn stale_stream_and_chapter_targets_are_refused_before_engine_mutation() {
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

    #[test]
    fn reopening_the_same_uri_invalidates_old_dynamic_targets() {
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
