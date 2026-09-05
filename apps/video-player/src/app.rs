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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager};
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{ActionEmitter, Button, ButtonFace, VideoPlayer, Widget};

use crate::collab::{CollabPlayback, Role};

/// Inset (logical px) of the floating top overlay (media title + Open) from the
/// video edges.
const OVERLAY_MARGIN: f64 = 14.0;
/// Height (logical px) of the floating Open pill.
const OPEN_BTN_H: f64 = 32.0;
/// Width (logical px) of the floating Open pill.
const OPEN_BTN_W: f64 = 84.0;

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
    dirty: bool,
}

impl PlayerApp {
    /// A player with a fresh engine and no media loaded.
    pub fn new() -> Self {
        let mut player = VideoPlayer::new()
            .with_engine(media::MediaEngine::new())
            .with_test_id("video_surface");
        player.enable_mpris("qos_video_player", "Video Player");
        let action_surface = crate::actions::resources("No media", false, true, &AutoCommit::new());
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
    ///   * `"settings"` — register app player-settings (priority icons + a
    ///     couple under the gear) and open the settings-gear menu.
    ///   * `"fullscreen"` — register the same settings and toggle fullscreen so
    ///     the control bar shows the immersive (exit-fullscreen) state.
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
                app.player.open(media::Source::Remote {
                    url: "https://demo.local/clip.mp4".into(),
                });
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
        if matches!(scenario, Some("settings" | "fullscreen")) {
            app.register_demo_settings();
        }
        match scenario {
            Some("settings") => app.player.open_settings_menu(None),
            Some("fullscreen") => app.player.toggle_fullscreen(),
            _ => {}
        }
        if !matches!(scenario, Some("empty")) {
            app.player.reveal_controls();
        }
        app
    }

    /// Register a representative set of app player-settings so a capture (or a
    /// demo) shows the control-bar priority icons and the gear menu together:
    /// three high-priority controls (resolution, captions, speed) as their own
    /// icons and two low-priority ones (autoplay, loop) tucked under the gear.
    pub fn register_demo_settings(&mut self) {
        use ui::icons::Icon;
        use ui::widget::video_player::PlayerSetting;
        self.player.set_setting(PlayerSetting::choice(
            "resolution",
            Icon::Monitor,
            "Resolution",
            3,
            vec!["1080p".into(), "720p".into(), "480p".into(), "Auto".into()],
            0,
        ));
        self.player.set_setting(PlayerSetting::toggle(
            "captions",
            Icon::FileText,
            "Captions",
            2,
            true,
        ));
        self.player.set_setting(PlayerSetting::choice(
            "speed",
            Icon::List,
            "Speed",
            1,
            vec!["0.5×".into(), "1×".into(), "1.5×".into(), "2×".into()],
            1,
        ));
        self.player.set_setting(PlayerSetting::toggle(
            "autoplay",
            Icon::Play,
            "Autoplay",
            0,
            true,
        ));
        self.player
            .set_setting(PlayerSetting::button("loop", Icon::Refresh, "Loop", 0));
    }

    /// Attach collaborative playback: a live collab socket plus the host/
    /// follower role from the environment. Only the live run path calls this —
    /// `--shots` and tests stay local so no thread/socket is created.
    #[must_use]
    pub fn with_collab(mut self) -> Self {
        let role = Role::from_env();
        self.player.set_transport_enabled(role == Role::Host);
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
        self.title = path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        self.player.open(source);
        self.player.play();
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

    fn close_media(&mut self) {
        let mut player = VideoPlayer::new()
            .with_engine(media::MediaEngine::new())
            .with_test_id("video_surface");
        player.set_transport_enabled(
            self.collab
                .as_ref()
                .is_none_or(|collab| collab.role() == Role::Host),
        );
        player.enable_mpris("qos_video_player", "Video Player");
        self.player = player;
        self.title = "No media".into();
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
            PlayerOperation::SeekSeconds(seconds) => {
                self.player.seek_to_ms((seconds.max(0.0) * 1000.0) as u64);
            }
            PlayerOperation::SetVolume(volume) => self.player.set_volume(volume as f32),
            PlayerOperation::ToggleFullscreen => self.player.toggle_fullscreen(),
        }
        self.dirty = true;
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
        crate::actions::resources(
            &self.title,
            self.player.has_media(),
            self.collab
                .as_ref()
                .is_none_or(|collab| collab.role() == Role::Host),
            doc,
        )
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
        if self
            .collab
            .as_ref()
            .is_some_and(|collab| collab.role() == Role::Follower)
            && matches!(
                &operation,
                crate::actions::PlayerOperation::TogglePlayback
                    | crate::actions::PlayerOperation::SeekSeconds(_)
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
        // Keep pumping while media plays, a portal open is pending, or a collab
        // session is live (so followers track the host / the host broadcasts).
        self.player.needs_animation_frame() || self.portal_rx.is_some() || self.collab.is_some()
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
        let mut app = PlayerApp::new();
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
    fn resources_nest_media_transport_audio_and_presentation() {
        let mut app = synth_app();
        app.open_path(Path::new("/tmp/Clip.mp4"));
        let resources = Widget::resources(&app, &AutoCommit::new());
        let app_resource = &resources[0];
        assert_eq!(app_resource.kind, "video-player.application");
        let media = &app_resource.children[0];
        assert_eq!(media.kind, "media.video");
        assert_eq!(media.children[0].kind, "media.transport");
        assert_eq!(media.children[1].kind, "media.audio");
        assert_eq!(media.children[2].kind, "media.presentation");
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
        let mut app = PlayerApp::new();
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
}
