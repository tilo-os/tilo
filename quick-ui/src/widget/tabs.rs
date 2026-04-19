use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PresenceCtx, PresenceKey, user_color};
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
    /// Hover-only maximize-icon hit regions, aligned 1:1 with `tabs`.
    /// Sits just to the left of each pill's close button and only draws
    /// when the pill is hovered, mirroring the concept's hover-reveal
    /// action row.
    max_rects: Vec<Rect>,
    hover_idx: Option<usize>,
    hover_close_idx: Option<usize>,
    /// Tab whose maximize icon the pointer is currently over. Kept
    /// separate from `hover_close_idx` so their hover visuals don't bleed
    /// into each other.
    hover_max_idx: Option<usize>,
    press_origin: Option<(usize, Point)>,
    last_cursor: Point,
    pub pending_drag: Option<DragPayload>,
    pub pending_reorder: Option<(usize, usize)>,
    pub pending_close: Option<u64>,
    /// Populated when the user clicks the maximize icon on a tab. The
    /// outer `WindowGroup` drains this and toggles its own maximized
    /// state so the shell can paint concept-style floating controls over
    /// a full-bleed tile.
    pub pending_max: Option<u64>,
    /// Populated when a drag gesture is released while the cursor is
    /// *outside* this bar's own rect — the user intends to move the tab
    /// to a different window-group. Carries `(tab_id, release_point)` so
    /// a parent widget (e.g. `GroupRow`) can resolve the drop target by
    /// hit-testing sibling bars. The parent is expected to drain this
    /// field immediately; leaving it set across frames is harmless but
    /// wasteful.
    pub pending_detach: Option<(u64, Point)>,
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
            max_rects: Vec::new(),
            hover_idx: None,
            hover_close_idx: None,
            hover_max_idx: None,
            press_origin: None,
            last_cursor: Point::ZERO,
            pending_drag: None,
            pending_reorder: None,
            pending_close: None,
            pending_max: None,
            pending_detach: None,
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
    /// True while a drag gesture is in flight (press threshold exceeded,
    /// release not yet seen). The parent `WindowGroup` reads this to paint
    /// a drag-in-flight accent ring matching the concept's drag-over state.
    pub fn drag_in_flight(&self) -> bool { self.pending_drag.is_some() }
    pub fn take_reorder(&mut self) -> Option<(usize, usize)> { self.pending_reorder.take() }
    pub fn take_close(&mut self) -> Option<u64> { self.pending_close.take() }
    pub fn take_max(&mut self) -> Option<u64> { self.pending_max.take() }
    /// Drain a cross-group detach request if any. See [`Self::pending_detach`].
    pub fn take_detach(&mut self) -> Option<(u64, Point)> { self.pending_detach.take() }
    /// Expose the bar's own rect so a parent widget (e.g. `GroupRow`) can
    /// hit-test a release point against sibling bars without having to
    /// re-walk the layout tree.
    pub fn bar_rect(&self) -> Rect { self.rect }
    /// Per-pill rects in tab order, populated during `apply_layout`.
    /// Exposed so `GroupRow` can resolve an "insert before pill X" drop
    /// index when a cross-group drag releases over a sibling's bar.
    pub fn tab_rects(&self) -> &[Rect] { &self.tab_rects }
    /// Compute an insertion index for a drop at `release_point`. Returns
    /// an index in `[0, tab_rects.len()]` — `0` means "before the first
    /// pill", `len` means "after the last pill". Vertical position is
    /// ignored (only the X coordinate matters); this lets callers drop
    /// anywhere inside the bar's horizontal extent.
    ///
    /// Rule: for each pill, if the release x is left of the pill's
    /// horizontal midpoint, insert **before** that pill; otherwise keep
    /// walking. If the release is past every pill's midpoint, append.
    pub fn drop_index_for(&self, release_point: Point) -> usize {
        for (i, r) in self.tab_rects.iter().enumerate() {
            let mid_x = (r.x0 + r.x1) / 2.0;
            if release_point.x < mid_x {
                return i;
            }
        }
        self.tab_rects.len()
    }
    /// Insert a tab at `idx` and make it active. Used by the cross-group
    /// drag routing in `GroupRow::handle_event` to splice a detached tab
    /// into the target bar without forcing a full rebuild.
    pub fn insert_tab(&mut self, idx: usize, desc: TabDescriptor) {
        let clamped = idx.min(self.tabs.len());
        let id = desc.id;
        self.tabs.insert(clamped, desc);
        self.active_id = id;
    }
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
        self.max_rects.clear();
        let gap = 6.0_f64;
        let pad_x = 12.0_f64;
        let close_w = 18.0_f64;
        let max_w = 16.0_f64;
        let close_inset = 4.0_f64;
        let mut x = self.rect.x0;
        for t in &self.tabs {
            let approx_char_w = 7.0_f64;
            let title_w = t.title.chars().count() as f64 * approx_char_w;
            // Reserve room for both the close glyph and the hover-reveal
            // maximize icon so pills don't resize when you hover them.
            let w = (title_w + pad_x * 2.0 + close_w + max_w).max(84.0);
            let r = Rect::new(x, self.rect.y0, x + w, self.rect.y1);
            let close_x0 = r.x1 - close_w - close_inset;
            let close = Rect::new(close_x0, r.y0 + 4.0, close_x0 + close_w, r.y1 - 4.0);
            let max_x0 = close_x0 - max_w - 2.0;
            let max_rect = Rect::new(max_x0, r.y0 + 4.0, max_x0 + max_w, r.y1 - 4.0);
            self.tab_rects.push(r);
            self.close_rects.push(close);
            self.max_rects.push(max_rect);
            x += w + gap;
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
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

            // Per-tab collaborator presence indicators. Matches the concept's
            // foggy blur: each peer active on this tab paints a low-alpha
            // colored circle at the bottom of the pill, colored by a stable
            // hash of their user id so the same person keeps the same hue
            // everywhere the UI renders them.
            let tab_key = PresenceKey::Tab { id: format!("{:x}", t.id) };
            let peers = presence.peers_on(&tab_key);
            if !peers.is_empty() {
                // Clip to the pill so the colored glow doesn't bleed into
                // adjacent tabs or the dark shell background.
                scene.push_layer(vello::peniko::BlendMode::default(), 1.0, transform, &shape);
                let dot_r = 8.0_f64;
                let span = (peers.len().min(4) as f64 - 1.0) * 10.0;
                let base_x = r.center().x - span / 2.0;
                let bottom = r.y1 + 2.0;
                for (pi, peer) in peers.iter().take(4).enumerate() {
                    let (cr, cg, cb) = user_color(&peer.0);
                    let cx = base_x + pi as f64 * 10.0;
                    let c = vello::kurbo::Circle::new(Point::new(cx, bottom), dot_r);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        vello::peniko::Color::rgba8(cr, cg, cb, 140),
                        None,
                        &c,
                    );
                }
                scene.pop_layer();
            }

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

            // Maximize icon: only visible on pill hover (concept's
            // hover-reveal action row). Drawn as a 1 px outlined square so
            // it reads as a generic "grow window" glyph without needing a
            // bespoke icon font.
            if is_hover {
                let max_r = self.max_rects[i];
                let max_hover = Some(i) == self.hover_max_idx;
                if max_hover {
                    let chip = RoundedRect::from_rect(max_r, max_r.height() / 2.0);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        vello::peniko::Color::rgba8(255, 255, 255, 64),
                        None,
                        &chip,
                    );
                }
                // 10×10 centered square outline.
                let s = 10.0_f64;
                let cx = max_r.center().x;
                let cy = max_r.center().y;
                let square = Rect::new(cx - s / 2.0, cy - s / 2.0, cx + s / 2.0, cy + s / 2.0);
                let outline_color = if max_hover { text_primary } else { text_dim };
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.2),
                    transform,
                    outline_color,
                    None,
                    &square,
                );
            }
        }

        // Drop-zone indicator: while a tab drag is in flight and the cursor
        // hovers another tab, paint a 2 px indigo bar at the gap where a
        // release would land the tab. The vec-remove-then-insert semantics
        // mean dropping onto a tab-to-the-right lands *after* that tab and
        // dropping onto a tab-to-the-left lands *before* it, so the bar
        // position mirrors that rule — this keeps the visual aligned with
        // the actual post-release index.
        if self.pending_drag.is_some() {
            if let (Some(to), Some((from, _))) = (
                self.tab_rects.iter().position(|r| r.contains(self.last_cursor)),
                self.press_origin,
            ) {
                if from != to {
                    let target = self.tab_rects[to];
                    let bar_x = if from < to { target.x1 } else { target.x0 - 2.0 };
                    let bar = Rect::new(bar_x, target.y0 - 2.0, bar_x + 2.0, target.y1 + 2.0);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        theme.tokens.colors.accent[5],
                        None,
                        &bar,
                    );
                }
            }
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, _m: &winit::event::Modifiers, _s: &mut AutoCommit, _p: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                self.hover_close_idx = self.close_rects.iter().position(|r| r.contains(p));
                self.hover_max_idx = self.max_rects.iter().position(|r| r.contains(p));
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
                // Maximize icon — only reachable while the pill is hovered,
                // so it never steals clicks from idle tabs.
                if let Some(mi) = self.hover_max_idx {
                    if let Some(t) = self.tabs.get(mi) {
                        self.pending_max = Some(t.id);
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
                // Finalise a drag release. Three possible outcomes:
                //
                //   1. Released over another pill in this same bar →
                //      `pending_reorder` (within-group move).
                //   2. Released somewhere outside this bar's own rect →
                //      `pending_detach` (cross-group move — the parent
                //      `GroupRow` resolves the target bar by hit-testing
                //      sibling `bar_rect()`s against the release point).
                //   3. Released inside the bar but not on another pill →
                //      dropped in the gap, treated as "cancel".
                if self.pending_drag.is_some() {
                    if let Some((from, _)) = self.press_origin {
                        let in_bar = self.rect.contains(self.last_cursor);
                        if in_bar {
                            let to = self.tab_rects.iter().position(|r| r.contains(self.last_cursor));
                            if let Some(to) = to {
                                if to != from {
                                    self.pending_reorder = Some((from, to));
                                    *handled = true;
                                }
                            }
                        } else if let Some(t) = self.tabs.get(from) {
                            self.pending_detach = Some((t.id, self.last_cursor));
                            *handled = true;
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

    #[test]
    fn user_color_is_stable_per_id_and_differs_across_ids() {
        // The widget now forwards to `crate::presence::user_color`, which
        // pins `alice` → pink-500 and `bob` → orange-500. The guarantees
        // we care about at the widget level are unchanged: stable per id,
        // distinguishable across ids.
        let a1 = user_color("alice");
        let a2 = user_color("alice");
        let b1 = user_color("bob");
        assert_eq!(a1, a2, "same id must map to same color across calls");
        assert_ne!(a1, b1, "different ids should map to different colors");
    }

    #[test]
    fn drop_index_for_uses_pill_midpoints() {
        // Hand-craft a bar with three known pill rects so we don't have to
        // run `apply_layout` through taffy for a drop-index unit test.
        // Pill A spans [0, 80], B [80, 160], C [160, 240].
        let mut b = bar();
        b.rect = Rect::new(0.0, 0.0, 240.0, 28.0);
        b.tab_rects = vec![
            Rect::new(0.0, 0.0, 80.0, 28.0),
            Rect::new(80.0, 0.0, 160.0, 28.0),
            Rect::new(160.0, 0.0, 240.0, 28.0),
        ];
        // Left of A's midpoint (40) → insert before A (index 0).
        assert_eq!(b.drop_index_for(Point::new(10.0, 14.0)), 0);
        // Past A's mid but before B's mid (120) → insert at index 1.
        assert_eq!(b.drop_index_for(Point::new(90.0, 14.0)), 1);
        // Past B's mid but before C's mid (200) → insert at index 2.
        assert_eq!(b.drop_index_for(Point::new(170.0, 14.0)), 2);
        // Past every midpoint → append at len().
        assert_eq!(b.drop_index_for(Point::new(230.0, 14.0)), 3);
    }

    #[test]
    fn drop_index_for_empty_bar_is_zero() {
        // A bar with no pills (e.g. a target that was just emptied) should
        // accept any drop at index 0 so the spliced tab lands cleanly.
        let mut b = bar();
        b.tab_rects.clear();
        assert_eq!(b.drop_index_for(Point::new(0.0, 0.0)), 0);
        assert_eq!(b.drop_index_for(Point::new(1000.0, 0.0)), 0);
    }

    #[test]
    fn take_max_drains_pending_request_exactly_once() {
        // Fresh bar has nothing pending — drain is `None` and stays that
        // way until an event seeds the request.
        let mut b = bar();
        assert_eq!(b.take_max(), None);
        b.pending_max = Some(2);
        assert_eq!(b.take_max(), Some(2));
        // Second drain is empty — ensures WindowGroup doesn't double-toggle
        // `maximized` on redundant polls within the same frame.
        assert_eq!(b.take_max(), None);
    }
}
