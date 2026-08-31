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
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager};
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, VideoPlayer, Widget};

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
        let player = VideoPlayer::new()
            .with_engine(media::MediaEngine::new())
            .with_test_id("video_surface");
        Self {
            player,
            title: "No media".to_string(),
            rect: Rect::ZERO,
            open_button_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            portal_rx: None,
            collab: None,
            manager: None,
            dirty: true,
        }
    }

    /// Open a filesystem `path` immediately (used by `--open PATH` and the
    /// portal outcome). Autoplays.
    #[must_use]
    pub fn with_open_path(mut self, path: PathBuf) -> Self {
        self.open_path(&path);
        self
    }

    /// Build a player staged for a headless `--shots` capture: a source opened
    /// and playing (so a frame is produced) with the controls revealed. Opens
    /// `open` when given, else a synthesized demo clip. Never attaches a collab
    /// socket, so no thread/portal is created.
    ///
    /// `scenario` selects an optional demo overlay for the capture:
    ///   * `"settings"` — register app player-settings (priority icons + a
    ///     couple under the gear) and open the settings-gear menu.
    ///   * `"fullscreen"` — register the same settings and toggle fullscreen so
    ///     the control bar shows the immersive (exit-fullscreen) state.
    ///   * anything else / `None` — a plain player.
    pub fn for_shot(open: Option<PathBuf>, scenario: Option<&str>) -> Self {
        let mut app = Self::new();
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
        if matches!(scenario, Some("settings" | "fullscreen")) {
            app.register_demo_settings();
        }
        match scenario {
            Some("settings") => app.player.open_settings_menu(None),
            Some("fullscreen") => app.player.toggle_fullscreen(),
            _ => {}
        }
        app.player.reveal_controls();
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
        self.collab = Some(CollabPlayback::new(Role::from_env()));
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
        self.dirty = true;
    }

    /// Kick off a portal file-open (or the immediate "no portal" outcome in the
    /// hermetic build).
    fn trigger_open(&mut self) {
        self.portal_rx = Some(video_open_request().send());
    }

    /// Drain a pending portal outcome, opening the chosen file.
    fn drain_portal(&mut self) {
        let mut clear = false;
        let mut chosen: Option<PathBuf> = None;
        if let Some(rx) = self.portal_rx.as_ref() {
            match rx.try_recv() {
                Ok(PortalOutcome::Opened(p)) => {
                    chosen = Some(p);
                    clear = true;
                }
                Ok(PortalOutcome::Cancelled | PortalOutcome::Failed(_))
                | Err(std::sync::mpsc::TryRecvError::Disconnected) => clear = true,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if clear {
            self.portal_rx = None;
        }
        if let Some(p) = chosen {
            self.open_path(&p);
        }
    }

    /// Pump the collab transport: apply inbound host state to a follower, or
    /// broadcast the host's changing playback.
    fn tick_collab(&mut self) {
        let (Some(mgr), Some(collab)) = (self.manager.as_mut(), self.collab.as_mut()) else {
            return;
        };
        for ev in mgr.poll() {
            match ev {
                CollabEvent::CRDTIncremental { data } => collab.ingest(&data),
                CollabEvent::CRDTUpdate { data } => collab.ingest_snapshot(&data),
                CollabEvent::SignedCRDTIncremental { envelope, .. } => {
                    collab.ingest(&envelope.payload);
                }
                _ => {}
            }
        }
        let now = media::now_ms();
        match collab.role() {
            Role::Host => {
                if let Some(engine) = self.player.engine()
                    && let Some(delta) = collab.host_step(engine, now)
                {
                    mgr.send(CollabRequest::SyncIncremental { data: delta });
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
        text_renderer.draw_text(
            scene,
            &self.title,
            title_size,
            vello::peniko::Color::from_rgb8(245, 246, 250),
            transform * Affine::translate((self.rect.x0 + OVERLAY_MARGIN, ty)),
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
    }
}

impl Default for PlayerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for PlayerApp {
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
                self.trigger_open();
                self.dirty = true;
                *handled = true;
                return;
            }
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
