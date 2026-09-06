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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
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
use crate::preferences::PlaybackPreferences;

/// Inset (logical px) of the floating top overlay (media title + Open) from the
/// video edges.
const OVERLAY_MARGIN: f64 = 14.0;
/// Height (logical px) of the floating Open pill.
const OPEN_BTN_H: f64 = 32.0;
/// Width (logical px) of the floating Open pill.
const OPEN_BTN_W: f64 = 84.0;

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

pub struct PlayerApp {
    player: VideoPlayer,
    title: String,
    rect: Rect,
    /// The floating Open pill's hit rect (top-right of the video). Only live
    /// while the overlay is shown.
    open_button_rect: Rect,
    last_cursor: Point,
    /// A file-open portal request in flight, drained each `sync`.
    portal_rx: Option<PortalChannel>,
    resource_invocation: Option<qos_actions::InvocationId>,
    resource_result: Option<qos_actions::InvocationUpdate>,
    open_action: Option<ActionEmitter<()>>,
    open_focused: bool,
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
        }
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
        self.metadata_revision = self.metadata_revision.wrapping_add(1);
        self.search_cache.get_mut().take();
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
            last_cursor: Point::ZERO,
            portal_rx: None,
            resource_invocation: None,
            resource_result: None,
            open_action,
            open_focused: false,
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
            dirty: true,
        }
    }

    /// Open a filesystem `path` immediately (used by `--open PATH` and the
    /// portal outcome). Autoplays.
    #[must_use]
    pub fn with_open_path(mut self, path: &Path) -> Self {
        self.open_path(path);
        self
    }

    /// Build a player staged for a headless `--shots` capture: a source opened
    /// and playing (so a frame is produced) with the controls revealed. Opens
    /// `open` when given, else a synthesized demo clip. Never attaches a collab
    /// socket, so no thread/portal is created.
    ///
    /// `scenario` selects an optional demo overlay for the capture:
    ///   * `"empty"` — the first-run state before a source is chosen.
    ///   * `"settings"` — open the real autoplay/repeat settings menu.
    ///   * `"fullscreen"` — toggle fullscreen so the control bar shows its
    ///     immersive (exit-fullscreen) state.
    ///   * `"metadata"` — open the controls menu with deterministic embedded
    ///     audio/subtitle/chapter metadata.
    ///   * anything else / `None` — a plain player.
    pub fn for_shot(open: Option<PathBuf>, scenario: Option<&str>) -> Self {
        let mut app = Self::new();
        if !matches!(scenario, Some("empty")) {
            if let Some(path) = open {
                app.open_path(&path);
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
                app.set_current_source(&source);
                app.player.open(source);
                app.title = "Demo clip".to_string();
                // Park at a fixed position and pause so the synthesized frame and
                // the transport read-out are byte-deterministic across runs: a
                // *playing* demo would capture a different synth frame each time
                // depending on warm-up timing. `open` already seeded a frame, so
                // the `--shots` warm-up still breaks immediately.
                app.player.seek_to_ms(8_000);
                app.player.pause();
            }
        }
        match scenario {
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
            Some("fullscreen") => app.player.toggle_fullscreen(),
            _ => {}
        }
        if !matches!(scenario, Some("empty")) {
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

    fn open_path(&mut self, path: &Path) {
        let source = source_for_path(path);
        self.set_current_source(&source);
        self.search_cache.get_mut().take();
        self.metadata_revision = self.metadata_revision.wrapping_add(1);
        self.title = path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        self.player.open(source);
        if self.preferences.autoplay {
            self.player.play();
        } else {
            self.player.pause();
        }
        self.player.reveal_controls();
        self.player
            .set_now_playing(ui::widget::video_player::NowPlaying {
                track_id: path.to_string_lossy().into_owned(),
                title: self.title.clone(),
                artist: String::new(),
                art_url: String::new(),
            });
        self.dirty = true;
    }

    fn set_current_source(&mut self, source: &media::Source) {
        self.source_generation = self.source_generation.wrapping_add(1);
        self.current_source_id = Some(format!(
            "{}#open-{}",
            source.to_uri(),
            self.source_generation
        ));
    }

    fn close_media(&mut self) {
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
        self.source_generation = self.source_generation.wrapping_add(1);
        self.search_cache.get_mut().take();
        self.metadata_revision = self.metadata_revision.wrapping_add(1);
        self.dirty = true;
    }

    fn request_open_action(&mut self) {
        if let Some(action) = &mut self.open_action {
            let _ = action.emit(());
        }
    }

    /// Kick off a portal file-open (or the immediate "no portal" outcome in the
    /// hermetic build).
    fn trigger_open(&mut self) {
        self.portal_rx = Some(video_open_request().send());
    }

    fn perform_resource_operation(&mut self, operation: crate::actions::PlayerOperation) {
        use crate::actions::PlayerOperation;
        match operation {
            PlayerOperation::Open { path: None } => self.trigger_open(),
            PlayerOperation::Open { path: Some(path) } => {
                self.open_path(Path::new(&path));
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
        let mut clear = false;
        let mut outcome: Option<Result<PathBuf, Option<String>>> = None;
        if let Some(rx) = self.portal_rx.as_ref() {
            match rx.try_recv() {
                Ok(PortalOutcome::Opened(p)) => {
                    outcome = Some(Ok(p));
                    clear = true;
                }
                Ok(PortalOutcome::Cancelled) => {
                    outcome = Some(Err(None));
                    clear = true;
                }
                Ok(PortalOutcome::Failed(error)) => {
                    outcome = Some(Err(Some(error)));
                    clear = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    outcome = Some(Err(Some("video picker disconnected".into())));
                    clear = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if clear {
            self.portal_rx = None;
        }
        if let Some(outcome) = outcome {
            let result = match outcome {
                Ok(path) => {
                    self.open_path(&path);
                    Ok(true)
                }
                Err(None) => Ok(false),
                Err(Some(e)) => Err(e),
            };
            if let Some(invocation) = self.resource_invocation.take() {
                self.resource_result = Some(match result {
                    Ok(true) => qos_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: qos_actions::ActionOutput::default(),
                    },
                    Ok(false) => qos_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("video selection was cancelled".into()),
                    },
                    Err(message) => qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
                            code: "video_player.open_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    },
                });
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
        let title_x = self.rect.x0 + OVERLAY_MARGIN;
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
        let (title, subtitle) = if self.player.has_media() {
            ("Loading video…", "Preparing the first frame")
        } else {
            ("Open a video to start", "MP4, WebM, MKV, MOV and AVI")
        };
        let (title_w, _) = text_renderer.measure(title, 20.0, None);
        let (subtitle_w, _) = text_renderer.measure(subtitle, 13.0, None);
        text_renderer.draw_text(
            scene,
            title,
            20.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((center.x - f64::from(title_w) * 0.5, center.y + 55.0)),
            None,
        );
        text_renderer.draw_text(
            scene,
            subtitle,
            13.0,
            theme.tokens.colors.text_dim,
            transform
                * Affine::translate((center.x - f64::from(subtitle_w) * 0.5, center.y + 85.0)),
            None,
        );
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
                return Err(format!("video does not exist: {path}"));
            }
            self.open_path(Path::new(path));
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
            self.resource_invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else if matches!(
            operation,
            Some(crate::actions::PlayerOperation::Open { path: Some(_) })
        ) {
            self.resource_result = Some(qos_actions::InvocationUpdate::Succeeded {
                invocation,
                output: qos_actions::ActionOutput::default(),
            });
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
        if self
            .resource_result
            .as_ref()
            .is_some_and(|u| u.invocation() == invocation)
        {
            self.resource_result.take()
        } else {
            None
        }
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.drain_portal();
        self.tick_collab();
        self.player.sync(state, path);
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
            // Only intercept the Open pill while the overlay is actually shown;
            // during immersive playback that corner is just video.
            if self.overlay_visible() && self.open_button_rect.contains(self.last_cursor) {
                self.request_open_action();
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
        self.open_action.as_mut().and_then(ActionEmitter::take)
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
        Some(
            AccessNode::new(Role::Group)
                .with_name(if self.player.has_media() {
                    self.title.clone()
                } else {
                    "Video Player".into()
                })
                .with_children(vec![
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
                ]),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        matches!(path, [0]).then_some(self.open_button_rect)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        if matches!(path, [0]) {
            self.open_focused = true;
            self.player.reveal_controls();
            self.dirty = true;
            true
        } else {
            false
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
        } else {
            false
        }
    }

    fn tooltip(&self) -> Option<&str> {
        (self.overlay_visible() && self.open_button_rect.contains(self.last_cursor))
            .then_some("Open video (Ctrl+O)")
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
        self.player.needs_animation_frame() || self.portal_rx.is_some()
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
        app.open_path(&PathBuf::from("/tmp/Holiday.mp4"));
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
        app.portal_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(
            PathBuf::from("/tmp/Chosen.webm"),
        )));
        app.drain_portal();
        assert_eq!(app.title, "Chosen.webm");
        assert!(app.portal_rx.is_none(), "channel cleared after the outcome");
    }

    #[test]
    fn open_affordance_emits_the_advertised_host_routable_effect() {
        let mut app = PlayerApp::new();
        app.request_open_action();
        let invocation = Widget::take_control_invocation(&mut app).expect("open intent");
        assert_eq!(invocation.address.resource, "video-player.application");
        assert_eq!(invocation.address.action, "open");
        assert!(
            app.portal_rx.is_none(),
            "the control did not bypass the host"
        );
    }

    #[test]
    fn resources_cover_transport_navigation_audio_metadata_and_settings() {
        let mut app = synth_app();
        app.open_path(Path::new("/tmp/Clip.mp4"));
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
        app.open_path(Path::new("/tmp/Clip.mp4"));
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
        app.open_path(&PathBuf::from("/tmp/Clip.mp4"));
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
    fn autoplay_and_repeat_are_real_settings_not_demo_affordances() {
        let mut app = synth_app();
        app.preferences.autoplay = false;
        configure_player_settings(
            &mut app.player,
            app.preferences.autoplay,
            app.preferences.loop_enabled,
        );
        app.open_path(Path::new("/tmp/Clip.mp4"));
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
        app.open_path(&PathBuf::from("/tmp/Clip.mp4"));
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
        app.open_path(&PathBuf::from("/tmp/Clip.mp4"));
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
        app.open_path(Path::new("/tmp/Clip.mp4"));
        app.perform_resource_operation(crate::actions::PlayerOperation::SetPlaybackRate(2.0));
        let engine = app.player.engine().expect("engine");
        assert!((engine.rate() - 2.0).abs() < f32::EPSILON);
        assert!((engine.transport_state(media::now_ms()).rate - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn space_key_routed_through_the_app_toggles_playback() {
        let mut app = synth_app();
        app.open_path(&PathBuf::from("/tmp/Clip.mp4"));

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
        app.set_current_source(&source);
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
