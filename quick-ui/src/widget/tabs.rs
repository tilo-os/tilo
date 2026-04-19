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
    close_rects: Vec<Rect>,
    hover_idx: Option<usize>,
    hover_close_idx: Option<usize>,
    press_origin: Option<(usize, Point)>,
    last_cursor: Point,
    pub pending_drag: Option<DragPayload>,
    pub pending_reorder: Option<(usize, usize)>,
    pub pending_close: Option<u64>,
}

impl TabBar {
    pub fn new(group_id_tag: &str, tabs: Vec<TabDescriptor>, active_id: u64) -> Self {
        Self {
            group_id_tag: group_id_tag.into(),
            tabs,
            active_id,
            rect: Rect::ZERO,
            tab_rects: Vec::new(),
            close_rects: Vec::new(),
            hover_idx: None,
            hover_close_idx: None,
            press_origin: None,
            last_cursor: Point::ZERO,
            pending_drag: None,
            pending_reorder: None,
            pending_close: None,
        }
    }
    pub fn tabs(&self) -> &[TabDescriptor] { &self.tabs }
    pub fn tabs_mut(&mut self) -> &mut Vec<TabDescriptor> { &mut self.tabs }
    pub fn active_id(&self) -> u64 { self.active_id }
    pub fn set_active(&mut self, id: u64) { if self.tabs.iter().any(|t| t.id == id) { self.active_id = id; } }
    pub fn remove_tab(&mut self, id: u64) -> Option<usize> {
        let idx = self.tabs.iter().position(|t| t.id == id)?;
        self.tabs.remove(idx);
        if self.active_id == id {
            self.active_id = self.tabs.get(idx).or_else(|| self.tabs.last()).map(|t| t.id).unwrap_or(0);
        }
        Some(idx)
    }
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to { return; }
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
    }
    pub fn take_drag(&mut self) -> Option<DragPayload> { self.pending_drag.take() }
    pub fn take_reorder(&mut self) -> Option<(usize, usize)> { self.pending_reorder.take() }
    pub fn take_close(&mut self) -> Option<u64> { self.pending_close.take() }
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
        // Content-sized pill tabs with a gap between them. The close button
        // ("×") sits inside the right end of each pill, 16 px wide.
        self.tab_rects.clear();
        self.close_rects.clear();
        let gap = 6.0_f64;
        let pad_x = 12.0_f64;
        let close_w = 18.0_f64;
        let close_inset = 4.0_f64;
        let mut x = self.rect.x0;
        for t in &self.tabs {
            let approx_char_w = 7.0_f64;
            let title_w = t.title.chars().count() as f64 * approx_char_w;
            let w = (title_w + pad_x * 2.0 + close_w).max(72.0);
            let r = Rect::new(x, self.rect.y0, x + w, self.rect.y1);
            let close_x0 = r.x1 - close_w - close_inset;
            let close = Rect::new(close_x0, r.y0 + 4.0, close_x0 + close_w, r.y1 - 4.0);
            self.tab_rects.push(r);
            self.close_rects.push(close);
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
            // Offset title left so the close button has room.
            let title_x = r.x0 + 12.0;
            let title_y = r.center().y - th as f64 / 2.0;
            // Clip the title to the area before the close button so long
            // titles truncate cleanly instead of overlapping the "×".
            let clip_r = Rect::new(r.x0, r.y0, self.close_rects[i].x0 - 2.0, r.y1);
            scene.push_layer(vello::peniko::BlendMode::default(), 1.0, transform, &clip_r);
            tr.draw_text(scene, &t.title, text_size, text_color, transform * Affine::translate((title_x, title_y)), None);
            let _ = tw;
            scene.pop_layer();

            // Close button: rendered always but emphasised on hover of the
            // tab or the close rect itself. Concept-aligned: a "×" glyph on
            // a subtle hover chip.
            let close_r = self.close_rects[i];
            let close_hover = Some(i) == self.hover_close_idx;
            let close_chip_alpha: u8 = if close_hover { 64 } else if is_hover { 28 } else { 0 };
            if close_chip_alpha > 0 {
                let chip = RoundedRect::from_rect(close_r, close_r.height() / 2.0);
                scene.fill(vello::peniko::Fill::NonZero, transform, vello::peniko::Color::rgba8(255, 255, 255, close_chip_alpha), None, &chip);
            }
            let glyph = "×";
            let glyph_size = theme.tokens.typography.scale.base;
            let (gw, gh) = tr.measure(glyph, glyph_size, None);
            let gx = close_r.center().x - gw as f64 / 2.0;
            let gy = close_r.center().y - gh as f64 / 2.0;
            let glyph_color = if close_hover { text_primary } else { text_dim };
            tr.draw_text(scene, glyph, glyph_size, glyph_color, transform * Affine::translate((gx, gy)), None);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, _m: &winit::event::Modifiers, _s: &mut AutoCommit, _p: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                self.hover_close_idx = self.close_rects.iter().position(|r| r.contains(p));
                // Only count a tab as "hovered for highlight" if we're inside
                // the pill AND not inside its close chip — keeps the close
                // button's own hover state dominant.
                self.hover_idx = self
                    .tab_rects
                    .iter()
                    .position(|r| r.contains(p));
                if let Some((idx, origin)) = self.press_origin {
                    if (p - origin).hypot() > DRAG_START_THRESHOLD_PX {
                        if let Some(t) = self.tabs.get(idx) {
                            let group_tag_hash = self.group_id_tag.bytes().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));
                            self.pending_drag = Some(DragPayload::Tab { group: group_tag_hash, tab: t.id });
                            *handled = true;
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // Close button takes priority over selection/drag.
                if let Some(ci) = self.hover_close_idx {
                    if let Some(t) = self.tabs.get(ci) {
                        self.pending_close = Some(t.id);
                        *handled = true;
                        return;
                    }
                }
                if let Some(idx) = self.hover_idx {
                    let origin = self.tab_rects[idx].center();
                    self.press_origin = Some((idx, origin));
                    self.active_id = self.tabs[idx].id;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // Finalise a drag-reorder: if a drag was in flight and the
                // release point lands over another tab, emit a reorder.
                if self.pending_drag.is_some() {
                    if let Some((from, _)) = self.press_origin {
                        let to = self.tab_rects.iter().position(|r| r.contains(self.last_cursor));
                        if let Some(to) = to {
                            if to != from {
                                self.pending_reorder = Some((from, to));
                                *handled = true;
                            }
                        }
                    }
                    self.pending_drag = None;
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bar() -> TabBar {
        TabBar::new(
            "g1",
            vec![
                TabDescriptor { id: 1, title: "A".into() },
                TabDescriptor { id: 2, title: "B".into() },
                TabDescriptor { id: 3, title: "C".into() },
            ],
            1,
        )
    }

    #[test]
    fn remove_tab_drops_it_and_picks_neighbour_when_active() {
        let mut b = bar();
        b.set_active(2);
        assert_eq!(b.remove_tab(2), Some(1));
        assert_eq!(b.tabs().len(), 2);
        // Active fell through to the tab that slid into position `1` (C).
        assert_eq!(b.active_id(), 3);
    }

    #[test]
    fn remove_tab_at_end_picks_last_remaining() {
        let mut b = bar();
        b.set_active(3);
        b.remove_tab(3);
        assert_eq!(b.active_id(), 2);
    }

    #[test]
    fn reorder_moves_tab_to_requested_index() {
        let mut b = bar();
        b.reorder(0, 2);
        let ids: Vec<u64> = b.tabs().iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
    }
}
