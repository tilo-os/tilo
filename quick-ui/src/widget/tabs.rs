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
        let node = taffy.new_leaf(Style { size: Size { width: Dimension::auto(), height: Dimension::length(36.0) }, ..Default::default() }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        let n = self.tabs.len().max(1) as f64;
        let tab_w = (self.rect.width() / n).max(60.0);
        self.tab_rects.clear();
        for i in 0..self.tabs.len() {
            let x0 = self.rect.x0 + (i as f64) * tab_w;
            self.tab_rects.push(Rect::new(x0, self.rect.y0, x0 + tab_w, self.rect.y1));
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        let bg = theme.tokens.colors.surfaces.raised;
        scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &self.rect);
        for (i, t) in self.tabs.iter().enumerate() {
            let r = self.tab_rects[i];
            let is_active = t.id == self.active_id;
            let fill = if is_active { theme.tokens.colors.surfaces.base } else if Some(i) == self.hover_idx { theme.tokens.colors.neutral[4] } else { bg };
            let shape = RoundedRect::from_rect(r, theme.tokens.radii.sm as f64);
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
            let text_size = theme.tokens.typography.scale.sm;
            let (tw, _) = tr.measure(&t.title, text_size, None);
            let x = r.center().x - tw as f64 / 2.0;
            let y = r.y0 + (r.height() - text_size as f64) / 2.0;
            tr.draw_text(scene, &t.title, text_size, theme.tokens.colors.text_primary, transform * Affine::translate((x, y)), None);
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
