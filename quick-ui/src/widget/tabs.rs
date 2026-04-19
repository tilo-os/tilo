use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;
use crate::dnd::{DragPayload, DRAG_START_THRESHOLD_PX};

#[derive(Clone, Debug)]
pub struct TabDescriptor { pub id: u64, pub title: String }

pub struct TabBar {
    group_id_tag: String,
    tabs: Vec<TabDescriptor>,
    active_id: u64,
    rect: Rect,
    tab_rects: Vec<Rect>,
    hover_idx: Option<usize>,
    press_origin: Option<(usize, Point)>,
    pub pending_drag: Option<DragPayload>,
    pub pending_reorder: Option<(usize, usize)>,
}

impl TabBar {
    pub fn new(group_id_tag: &str, tabs: Vec<TabDescriptor>, active_id: u64) -> Self {
        Self {
            group_id_tag: group_id_tag.into(),
            tabs,
            active_id,
            rect: Rect::ZERO,
            tab_rects: Vec::new(),
            hover_idx: None,
            press_origin: None,
            pending_drag: None,
            pending_reorder: None,
        }
    }
    pub fn tabs(&self) -> &[TabDescriptor] { &self.tabs }
    pub fn active_id(&self) -> u64 { self.active_id }
    pub fn set_active(&mut self, id: u64) { if self.tabs.iter().any(|t| t.id == id) { self.active_id = id; } }
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to { return; }
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
    }
    pub fn take_drag(&mut self) -> Option<DragPayload> { self.pending_drag.take() }
}

impl Widget for TabBar {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        // ~28 px pills on the dark shell bg, above the white-card frame.
        let node = taffy.new_leaf(Style { size: Size { width: Dimension::auto(), height: Dimension::length(28.0) }, flex_shrink: 0.0, ..Default::default() }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        // Content-sized pill tabs with a gap between them.
        self.tab_rects.clear();
        let gap = 6.0_f64;
        let pad_x = 12.0_f64;
        let mut x = self.rect.x0;
        for t in &self.tabs {
            let approx_char_w = 7.0_f64;
            let title_w = t.title.chars().count() as f64 * approx_char_w;
            let w = (title_w + pad_x * 2.0).max(60.0);
            self.tab_rects.push(Rect::new(x, self.rect.y0, x + w, self.rect.y1));
            x += w + gap;
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        // No full-bar fill — pills sit directly on the dark shell background.
        let radius = self.rect.height() / 2.0;
        let text_primary = theme.tokens.colors.text_primary;
        let text_dim = theme.tokens.colors.text_dim;
        for (i, t) in self.tabs.iter().enumerate() {
            let r = self.tab_rects[i];
            let is_active = t.id == self.active_id;
            let is_hover = Some(i) == self.hover_idx;
            // Active: `white/10` translucent; inactive: `white/5`; hover: bumped.
            let fill_alpha: u8 = if is_active { 38 } else if is_hover { 26 } else { 13 };
            let fill = vello::peniko::Color::rgba8(255, 255, 255, fill_alpha);
            let shape = RoundedRect::from_rect(r, radius);
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
            // Faint border to match concept (`white/20` / `white/10`).
            let border_alpha: u8 = if is_active { 51 } else { 26 };
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                vello::peniko::Color::rgba8(255, 255, 255, border_alpha),
                None,
                &shape,
            );
            let text_color = if is_active { text_primary } else { text_dim };
            let text_size = theme.tokens.typography.scale.sm;
            let (tw, th) = tr.measure(&t.title, text_size, None);
            let x = r.center().x - tw as f64 / 2.0;
            let y = r.center().y - th as f64 / 2.0;
            tr.draw_text(scene, &t.title, text_size, text_color, transform * Affine::translate((x, y)), None);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, _m: &winit::event::Modifiers, _s: &mut AutoCommit, _p: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.hover_idx = self.tab_rects.iter().position(|r| r.contains(p));
                if let Some((idx, origin)) = self.press_origin {
                    if (p - origin).hypot() > DRAG_START_THRESHOLD_PX {
                        if let Some(t) = self.tabs.get(idx) {
                            let group_tag_hash = self.group_id_tag.bytes().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));
                            self.pending_drag = Some(DragPayload::Tab { group: group_tag_hash, tab: t.id });
                            self.press_origin = None;
                            *handled = true;
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if let Some(idx) = self.hover_idx {
                    let origin = self.tab_rects[idx].center();
                    self.press_origin = Some((idx, origin));
                    self.active_id = self.tabs[idx].id;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.press_origin = None;
            }
            _ => {}
        }
    }
    fn name(&self) -> &str { "TabBar" }
    fn get_rect(&self) -> Rect { self.rect }
    fn wants_pointer_capture(&self) -> bool { self.press_origin.is_some() || self.pending_drag.is_some() }
    fn accepts_drop(&self, payload: &DragPayload) -> bool { matches!(payload, DragPayload::Tab { .. }) }
}
