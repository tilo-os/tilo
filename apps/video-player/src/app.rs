//! The local video-player app: minimal chrome (title + Open) above a reusable
//! [`ui::widget::VideoPlayer`] driven by a shared [`media::MediaEngine`].
//!
//! `PlayerApp` is the root widget. It owns the player, paints a slim top chrome
//! bar, opens files through the XDG FileChooser portal (or `--open PATH`), and —
//! when a collab session is attached — keeps playback in sync across peers via
//! the shared [`media`] transport.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{length, percent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::collaboration::{CollabEvent, CollaborationManager, CollabRequest};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{VideoPlayer, Widget};

use crate::collab::{CollabPlayback, Role};
use crate::portal::{self, OpenChannel, OpenOutcome};

/// Height of the top chrome bar.
const CHROME_HEIGHT: f32 = 48.0;

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
    chrome_rect: Rect,
    open_button_rect: Rect,
    last_cursor: Point,
    /// A file-open portal request in flight, drained each `sync`.
    portal_rx: Option<OpenChannel>,
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
            chrome_rect: Rect::ZERO,
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
    pub fn with_open_path(mut self, path: PathBuf) -> Self {
        self.open_path(path);
        self
    }

    /// Build a player staged for a headless `--shots` capture: a source opened
    /// and playing (so a frame is produced) with the controls revealed. Opens
    /// `open` when given, else a synthesized demo clip. Never attaches a collab
    /// socket, so no thread/portal is created.
    pub fn for_shot(open: Option<PathBuf>) -> Self {
        let mut app = Self::new();
        match open {
            Some(path) => app.open_path(path),
            None => {
                app.player
                    .open(media::Source::Remote { url: "https://demo.local/clip.mp4".into() });
                app.title = "Demo clip".to_string();
                app.player.play();
            }
        }
        app.player.reveal_controls();
        app
    }

    /// Attach collaborative playback: a live collab socket plus the host/
    /// follower role from the environment. Only the live run path calls this —
    /// `--shots` and tests stay local so no thread/socket is created.
    pub fn with_collab(mut self) -> Self {
        self.collab = Some(CollabPlayback::new(Role::from_env()));
        self.manager = Some(CollaborationManager::new());
        self
    }

    fn open_path(&mut self, path: PathBuf) {
        let source = source_for_path(&path);
        self.title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        self.player.open(source);
        self.player.play();
        self.dirty = true;
    }

    /// Kick off a portal file-open (or the immediate "no portal" outcome in the
    /// hermetic build).
    fn trigger_open(&mut self) {
        self.portal_rx = Some(portal::request_open());
    }

    /// Drain a pending portal outcome, opening the chosen file.
    fn drain_portal(&mut self) {
        let mut clear = false;
        let mut chosen: Option<PathBuf> = None;
        if let Some(rx) = self.portal_rx.as_ref() {
            match rx.try_recv() {
                Ok(OpenOutcome::Chosen(p)) => {
                    chosen = Some(p);
                    clear = true;
                }
                Ok(OpenOutcome::Cancelled) | Ok(OpenOutcome::Failed(_)) => clear = true,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => clear = true,
            }
        }
        if clear {
            self.portal_rx = None;
        }
        if let Some(p) = chosen {
            self.open_path(p);
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
                    collab.ingest(&envelope.payload)
                }
                _ => {}
            }
        }
        let now = media::now_ms();
        match collab.role() {
            Role::Host => {
                if let Some(engine) = self.player.engine() {
                    if let Some(delta) = collab.host_step(engine, now) {
                        mgr.send(CollabRequest::SyncIncremental { data: delta });
                    }
                }
            }
            Role::Follower => {
                if let Some(engine) = self.player.engine_mut() {
                    collab.follow_step(engine, now);
                }
            }
        }
    }

    fn paint_chrome(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &self.chrome_rect,
        );
        let pad = theme.tokens.spacing.s3 as f64;
        // Title (left).
        let title_size = 15.0f32;
        let ty = self.chrome_rect.y0 + (self.chrome_rect.height() - title_size as f64) * 0.5;
        text_renderer.draw_text(
            scene,
            &self.title,
            title_size,
            colors.text_primary,
            transform * Affine::translate((self.chrome_rect.x0 + pad, ty)),
            None,
        );
        // Open button (right).
        if self.open_button_rect.width() > 0.0 {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.accent[ui::tokens::colors::ACCENT_PRIMARY_IDX],
                None,
                &RoundedRect::from_rect(self.open_button_rect, 6.0),
            );
            let label = "Open";
            let lsize = 14.0f32;
            let (lw, _lh) = text_renderer.measure(label, lsize, None);
            let lx = self.open_button_rect.center().x - lw as f64 * 0.5;
            let ly = self.open_button_rect.center().y - lsize as f64 * 0.5;
            text_renderer.draw_text(
                scene,
                label,
                lsize,
                colors.text_primary,
                transform * Affine::translate((lx, ly)),
                None,
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
        // Chrome bar: a fixed-height full-width leaf we paint ourselves.
        let chrome = taffy
            .new_leaf(Style {
                size: Size { width: percent(1.0), height: length(CHROME_HEIGHT) },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        let player_node = self.player.build_layout(taffy, theme, node_map);
        let root = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size { width: percent(1.0), height: percent(1.0) },
                    ..Default::default()
                },
                &[chrome, player_node],
            )
            .unwrap();
        node_map.insert(root, self as *mut dyn Widget);
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        let children = taffy.children(node).unwrap();
        // children[0] = chrome, children[1] = player.
        let chrome_layout = taffy.layout(children[0]).unwrap();
        self.chrome_rect = Rect::new(
            new_origin.x + chrome_layout.location.x as f64,
            new_origin.y + chrome_layout.location.y as f64,
            new_origin.x + chrome_layout.location.x as f64 + chrome_layout.size.width as f64,
            new_origin.y + chrome_layout.location.y as f64 + chrome_layout.size.height as f64,
        );
        // Open button pinned to the right of the chrome bar.
        let bw = 76.0;
        let bh = 30.0;
        let by = self.chrome_rect.y0 + (self.chrome_rect.height() - bh) * 0.5;
        let bx1 = self.chrome_rect.x1 - 12.0;
        self.open_button_rect = Rect::new((bx1 - bw).max(self.chrome_rect.x0), by, bx1, by + bh);
        self.player.apply_layout(taffy, children[1], new_origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.player.draw(scene, transform, text_renderer, theme, presence);
        self.paint_chrome(scene, transform, text_renderer, theme);
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
            if self.open_button_rect.contains(self.last_cursor) {
                self.trigger_open();
                self.dirty = true;
                *handled = true;
                return;
            }
        }
        // Forward everything else to the player (its controls + shortcuts).
        let children = taffy.children(node_id).unwrap();
        if let Some(&player_node) = children.get(1) {
            self.player.handle_event(
                event, scale_factor, modifiers, state, path, handled, focus_target, player_node,
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

    fn name(&self) -> &str {
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
        app.open_path(PathBuf::from("/tmp/Holiday.mp4"));
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
        app.portal_rx = Some(portal::inject_for_test(OpenOutcome::Chosen(PathBuf::from(
            "/tmp/Chosen.webm",
        ))));
        app.drain_portal();
        assert_eq!(app.title, "Chosen.webm");
        assert!(app.portal_rx.is_none(), "channel cleared after the outcome");
    }
}
