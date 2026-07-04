//! `SidebarFolderRow` — a file-explorer sidebar row that renders a label
//! pill plus up to 3 presence dots on the right when peers are attributed
//! to this folder.
//!
//! Ports the `activeCollaborators` dot strip from
//! `nirios-concept/src/components/apps/FilesApp.tsx` (lines 101–111): a
//! flex `-space-x-1` cluster of 2.5 × 2.5 colored circles, one per peer,
//! surfaced on the active favorites row.
//!
//! Presence is keyed on `PresenceKey::App { app: FileExplorer, sub: <path> }`
//! so a peer "being in a folder" attributes to that row independently of
//! which tab hosts the file-explorer — matching the concept's
//! `c.activeAppId === app.id` filter, which scopes to the app instance.

use ui::ColorCompat;
use ui::permissions::AppKind;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use vello::Scene;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::command::{push, CommandSink, FileCommand};

/// Presence key for "a peer is focused on this folder in FileExplorer".
pub fn folder_presence_key(path: &Path) -> PresenceKey {
    PresenceKey::App {
        app: AppKind::FileExplorer,
        sub: path.display().to_string(),
    }
}

/// Up to `PresenceCap` peer colours to paint for this row, resolved via
/// `ctx.color_for` so per-peer overrides are honoured.
pub const PRESENCE_CAP: usize = 3;

/// Returns the RGB colours (up to `PRESENCE_CAP`) that should render as
/// dots on the row for `path`. Pure helper, trivially testable.
pub fn peer_dot_colors(ctx: &PresenceCtx, path: &Path) -> Vec<(u8, u8, u8)> {
    let key = folder_presence_key(path);
    ctx.peers_on(&key)
        .iter()
        .take(PRESENCE_CAP)
        .map(|u| ctx.color_for(u))
        .collect()
}

/// Returns the total number of peers on this folder (uncapped). Used
/// downstream if the caller wants to render a `+N` overflow chip.
pub fn peer_count(ctx: &PresenceCtx, path: &Path) -> usize {
    ctx.peers_on(&folder_presence_key(path)).len()
}

pub struct SidebarFolderRow {
    /// Stable id used for hit-testing against `files.sidebar.<N>` —
    /// kept here so future navigation wiring can route clicks by id
    /// without rebuilding the composition.
    #[allow(dead_code)]
    id: String,
    label: String,
    path: PathBuf,
    active: bool,
    rect: Rect,
    /// Command sink — a click pushes `Navigate(path)`. `None` leaves the
    /// row inert (the pre-wiring behaviour, kept for tests).
    sink: Option<CommandSink>,
    last_cursor: Point,
}

impl SidebarFolderRow {
    pub fn new(id: &str, label: &str, path: PathBuf) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            path,
            active: false,
            rect: Rect::ZERO,
            sink: None,
            last_cursor: Point::ZERO,
        }
    }

    pub fn with_active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Wire clicks on this row to navigate to its folder.
    pub fn with_sink(mut self, sink: CommandSink) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// For tests: compute how many dots this row would paint if asked to
    /// draw now.
    pub fn dot_count(&self, ctx: &PresenceCtx) -> usize {
        peer_count(ctx, &self.path).min(PRESENCE_CAP)
    }
}

impl Widget for SidebarFolderRow {
    fn sync(&mut self, _state: &mut automerge::AutoCommit, _path: &automerge::ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(28.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
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
        // Pill chrome — indigo-500/10 when active, transparent otherwise.
        if self.active {
            let bg = Color::from_rgba8(0x63, 0x66, 0xf1, (0.10 * 255.0) as u8);
            let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.sm as f64);
            scene.fill(Fill::NonZero, transform, bg, None, &shape);
        }

        // Label — text_primary at 55% alpha for inactive rows, matching
        // the sidebar wheel inactive-card treatment.
        let text_size = 13.0_f32;
        let (tr, tg, tb) = {
            let c = theme.tokens.colors.text_primary;
            (c.r(), c.g(), c.b())
        };
        let alpha = if self.active { 0xf0 } else { 0x8c };
        let text_color = Color::from_rgba8(tr, tg, tb, alpha);
        let (_tw, th) = text_renderer.measure(&self.label, text_size, None);
        let text_x = self.rect.x0 + 8.0;
        let text_y = self.rect.y0 + (self.rect.height() - th as f64) / 2.0;
        text_renderer.draw_text(
            scene,
            &self.label,
            text_size,
            text_color,
            transform * Affine::translate((text_x, text_y)),
            None,
        );

        // Presence dots — up to PRESENCE_CAP 5 px circles on the right
        // with a 1 px white-ish ring, right-to-left with a 4 px overlap
        // (concept's `-space-x-1`).
        let colors = peer_dot_colors(presence, &self.path);
        let radius = 3.0;
        let overlap = 4.0;
        let right = self.rect.x1 - 6.0;
        let cy = self.rect.center().y;
        for (i, (r, g, b)) in colors.iter().enumerate() {
            let cx = right - (i as f64) * (radius * 2.0 - overlap) - radius;
            let circle = Circle::new((cx, cy), radius);
            let fill = Color::from_rgb8(*r, *g, *b);
            scene.fill(Fill::NonZero, transform, fill, None, &circle);
            // White ring so the dot reads against coloured pill backgrounds.
            let ring = Circle::new((cx, cy), radius + 0.5);
            scene.stroke(
                &vello::kurbo::Stroke::new(0.75),
                transform,
                Color::from_rgba8(0xff, 0xff, 0xff, 0xb0),
                None,
                &ring,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut automerge::AutoCommit,
        _path: &automerge::ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if self.rect.contains(self.last_cursor) {
                    if let Some(sink) = &self.sink {
                        push(sink, FileCommand::Navigate(self.path.clone()));
                        *handled = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "SidebarFolderRow"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::permissions::UserId;
    use ui::presence::PresenceCtx;
    use std::path::PathBuf;

    fn ctx_with(path: &Path, users: &[&str]) -> PresenceCtx {
        let mut ctx = PresenceCtx::empty();
        let key = folder_presence_key(path);
        for u in users {
            ctx.register_presence(&key, UserId((*u).to_string()));
        }
        ctx
    }

    #[test]
    fn folder_presence_key_scopes_under_file_explorer_app() {
        let key = folder_presence_key(Path::new("/home/alice/Documents"));
        match key {
            PresenceKey::App { app, sub } => {
                assert_eq!(app, AppKind::FileExplorer);
                assert_eq!(sub, "/home/alice/Documents");
            }
            _ => panic!("expected App key, got {:?}", key),
        }
    }

    #[test]
    fn peer_dot_colors_respects_cap_and_order() {
        let path = PathBuf::from("/tmp/ws");
        // alice, bob, charlie, dave — 4 peers, cap = 3.
        let ctx = ctx_with(&path, &["alice", "bob", "charlie", "dave"]);
        let dots = peer_dot_colors(&ctx, &path);
        assert_eq!(dots.len(), PRESENCE_CAP);
        // alice comes first by registration order.
        assert_eq!(dots[0], ui::presence::user_color("alice"));
        assert_eq!(dots[1], ui::presence::user_color("bob"));
        assert_eq!(dots[2], ui::presence::user_color("charlie"));
    }

    #[test]
    fn peer_count_returns_uncapped_total() {
        let path = PathBuf::from("/tmp/ws");
        let ctx = ctx_with(&path, &["alice", "bob", "charlie", "dave", "eve"]);
        assert_eq!(peer_count(&ctx, &path), 5);
    }

    #[test]
    fn peer_dot_colors_returns_empty_when_no_presence() {
        let ctx = PresenceCtx::empty();
        let dots = peer_dot_colors(&ctx, Path::new("/no/one/here"));
        assert!(dots.is_empty());
    }

    #[test]
    fn peer_dot_colors_honours_color_override() {
        let path = PathBuf::from("/tmp/ws");
        let mut ctx = ctx_with(&path, &["alice"]);
        ctx.set_color_override(UserId("alice".into()), "#112233");
        let dots = peer_dot_colors(&ctx, &path);
        assert_eq!(dots, vec![(0x11, 0x22, 0x33)]);
    }

    #[test]
    fn dot_count_on_row_matches_helper_capped() {
        let path = PathBuf::from("/tmp/ws");
        let ctx = ctx_with(&path, &["alice", "bob", "charlie", "dave"]);
        let row = SidebarFolderRow::new("files.sidebar.0", "Home", path);
        assert_eq!(row.dot_count(&ctx), PRESENCE_CAP);
    }

    #[test]
    fn dot_count_zero_when_no_peers() {
        let row = SidebarFolderRow::new("files.sidebar.0", "Home", PathBuf::from("/tmp/ws"));
        assert_eq!(row.dot_count(&PresenceCtx::empty()), 0);
    }

    #[test]
    fn row_keeps_id_for_hit_testing() {
        let row = SidebarFolderRow::new("files.sidebar.4", "Music", PathBuf::from("/tmp/music"));
        assert_eq!(row.id, "files.sidebar.4");
    }
}
