use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::widget::app_widget::AppWidget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AppKind { Terminal, Files, Editor, Browser, Settings }

impl AppKind {
    pub fn as_str(&self) -> &str {
        match self {
            AppKind::Terminal => "Terminal",
            AppKind::Files => "Files",
            AppKind::Editor => "Editor",
            AppKind::Browser => "Browser",
            AppKind::Settings => "Settings",
        }
    }
}

/// Content for a single tile (single tab) hosted by a `TileHost`.
///
/// As of Plan 3 Task 3, `App` carries a `Box<dyn AppWidget>` — `AppWidget: Widget`
/// so layout/draw/event calls still auto-upcast to the base `Widget` contract.
/// The `Wayland(SurfaceId)` variant is reserved for future compositor integration.
pub enum TileContent {
    App(Box<dyn AppWidget>),
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
        // Shadow-2xl approximation beneath the card (concept:
        // `shadow-2xl` ≈ `0 25px 50px -12px rgba(0, 0, 0, 0.25)`).
        // Vello has no native gaussian blur so we approximate with four
        // progressively larger, translated-down, alpha-diminishing
        // rounded rects stacked behind the card. Each ring contributes
        // ~6 % alpha so their union reads as a soft 30 % drop shadow.
        let radius = theme.tokens.radii.xl as f64;
        for (spread, dy, alpha) in [(2.0_f64, 6.0_f64, 24u8), (6.0, 12.0, 18), (14.0, 20.0, 10), (24.0, 28.0, 4)] {
            let shadow_rect = Rect::new(
                self.rect.x0 - spread,
                self.rect.y0 + dy - spread * 0.3,
                self.rect.x1 + spread,
                self.rect.y1 + dy + spread,
            );
            let shadow_shape = RoundedRect::from_rect(shadow_rect, radius + spread * 0.5);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                vello::peniko::Color::rgba8(0, 0, 0, alpha),
                None,
                &shadow_shape,
            );
        }
        // White rounded-3xl card (concept: `bg-white` `rounded-3xl`
        // `border-white/20` `shadow-2xl`).
        let shape = RoundedRect::from_rect(self.rect, radius);
        scene.fill(vello::peniko::Fill::NonZero, transform, theme.tokens.colors.surfaces.window_card, None, &shape);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            theme.tokens.colors.surfaces.window_card_border,
            None,
            &shape,
        );
        match &mut self.content {
            TileContent::App(w) => w.draw(scene, transform, tr, theme, presence),
            TileContent::Stub { kind, label } => {
                // Dark text on the white card.
                let on_card = theme.tokens.colors.neutral[1]; // slate-900 in dark theme
                let on_card_dim = theme.tokens.colors.neutral[4]; // slate-600
                let text_size = theme.tokens.typography.scale.xl;
                let (tw, _) = tr.measure(label, text_size, None);
                let x = self.rect.center().x - tw as f64 / 2.0;
                let y = self.rect.center().y - text_size as f64 / 2.0;
                tr.draw_text(scene, label, text_size, on_card, transform * Affine::translate((x, y)), None);
                let sm = theme.tokens.typography.scale.sm;
                let s = format!("{} coming soon", kind.as_str());
                let (sw, _) = tr.measure(&s, sm, None);
                let sx = self.rect.center().x - sw as f64 / 2.0;
                tr.draw_text(scene, &s, sm, on_card_dim, transform * Affine::translate((sx, y + text_size as f64 + 8.0)), None);
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
    fn take_structure_dirty(&mut self) -> bool {
        // A hosted app widget could itself mutate its subtree (e.g. a file
        // explorer that spawns a new pane). Forward through so App can
        // rebuild the taffy tree when the inner widget reports dirty.
        if let TileContent::App(w) = &mut self.content {
            w.take_structure_dirty()
        } else {
            false
        }
    }
}
