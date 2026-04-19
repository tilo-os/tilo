use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::{Widget, Element};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppKind { Terminal, FileExplorer, Browser, Settings, Other(String) }

impl AppKind {
    pub fn as_str(&self) -> &str { match self { AppKind::Terminal => "Terminal", AppKind::FileExplorer => "Files", AppKind::Browser => "Browser", AppKind::Settings => "Settings", AppKind::Other(s) => s.as_str() } }
}

/// Content for a single tile (single tab) hosted by a `TileHost`.
///
/// **Interim:** During Plan 1, `App` carries a `Box<dyn Widget>` so concrete widgets
/// need not implement the full `AppWidget` trait yet. Plan 3 redefines the variant as
/// `Box<dyn AppWidget>` (which extends `Widget`) once the app contract lands. The
/// `Wayland(SurfaceId)` variant is reserved for future compositor integration.
pub enum TileContent {
    App(Element),
    Stub { kind: AppKind, label: String },
    // reserved: Wayland(u64),
}

pub struct TileHost { content: TileContent, rect: Rect }

impl TileHost {
    pub fn new(content: TileContent) -> Self { Self { content, rect: Rect::ZERO } }
    pub fn kind_string(&self) -> String {
        match &self.content {
            TileContent::App(_) => "App".into(),
            TileContent::Stub { kind, .. } => kind.as_str().into(),
        }
    }
    pub fn content_mut(&mut self) -> &mut TileContent { &mut self.content }
}

impl Widget for TileHost {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) { if let TileContent::App(w) = &mut self.content { w.sync(s, p); } }
    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool { if let TileContent::App(w) = &mut self.content { w.update(s, p) } else { false } }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = match &mut self.content {
            TileContent::App(w) => {
                let child = w.build_layout(taffy, theme, node_map);
                taffy.new_with_children(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, ..Default::default() }, &[child]).unwrap()
            }
            TileContent::Stub { .. } => {
                taffy.new_leaf(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, ..Default::default() }).unwrap()
            }
        };
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        if let TileContent::App(w) = &mut self.content {
            let child_node = taffy.children(node).unwrap()[0];
            w.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.lg as f64);
        scene.fill(vello::peniko::Fill::NonZero, transform, theme.tokens.colors.surfaces.raised, None, &shape);
        match &mut self.content {
            TileContent::App(w) => w.draw(scene, transform, tr, theme, presence),
            TileContent::Stub { kind, label } => {
                let text_size = theme.tokens.typography.scale.xl;
                let (tw, _) = tr.measure(label, text_size, None);
                let x = self.rect.center().x - tw as f64 / 2.0;
                let y = self.rect.center().y - text_size as f64 / 2.0;
                tr.draw_text(scene, label, text_size, theme.tokens.colors.text_primary, transform * Affine::translate((x, y)), None);
                let sm = theme.tokens.typography.scale.sm;
                let s = format!("{} coming soon", kind.as_str());
                let (sw, _) = tr.measure(&s, sm, None);
                let sx = self.rect.center().x - sw as f64 / 2.0;
                tr.draw_text(scene, &s, sm, theme.tokens.colors.text_dim, transform * Affine::translate((sx, y + text_size as f64 + 8.0)), None);
            }
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        if let TileContent::App(w) = &mut self.content {
            let child_node = taffy.children(node_id).unwrap()[0];
            w.handle_event(event, scale_factor, m, s, p, handled, focus_target, child_node, taffy);
        }
    }
    fn name(&self) -> &str { "TileHost" }
    fn get_rect(&self) -> Rect { self.rect }
}
