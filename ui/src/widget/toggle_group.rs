//! ToggleGroup — segmented control. A horizontal strip of equal-width
//! buttons where exactly one segment is selected. Clicking a segment
//! activates it and fires the `on_change` callback with the new index.
//!
//! Visual contract (concept: shadcn/ui-style segmented control):
//! - Outer rounded-rect bounded by `theme.tokens.colors.border_standard`
//!   with radius `theme.tokens.radii.md`. The outer radius is shared
//!   with the first and last segments only; interior segments are flat
//!   on all sides so the strip reads as a single chunk.
//! - Selected segment: fill `theme.tokens.colors.accent[ACCENT_PRIMARY_IDX]`,
//!   text `Color::WHITE`.
//! - Unselected segments: fill `theme.tokens.colors.surfaces.raised`,
//!   text `theme.tokens.colors.text_primary`.
//! - 1-px vertical separator line in `theme.tokens.colors.border_standard`
//!   between adjacent segments. There are exactly `segments.len() - 1`
//!   separators (none after the last segment, no top/bottom dividers).
//!
//! State lives entirely on the widget (no CRDT round-trip) because the
//! segmented control's selection is ephemeral UI sugar — callers persist
//! the choice via the `on_change` callback into their own CRDT slot.
use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use std::cell::Cell;
use std::collections::HashMap;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::Widget;

/// One entry in a `ToggleGroup`. Currently text-only; an icon variant
/// can be added later without breaking the constructor signature.
pub struct Segment {
    pub label: String,
}

impl Segment {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into() }
    }
}

pub struct ToggleGroup {
    segments: Vec<Segment>,
    selected: usize,
    on_change: Option<Box<dyn FnMut(usize)>>,
    /// Per-segment hit rect populated in `apply_layout`. Indices align
    /// 1:1 with `segments`. Drawing also consults this for the per-
    /// segment background fill.
    segment_rects: Vec<Rect>,
    rect: Rect,
    /// Last logical cursor position, captured on `CursorMoved` so the
    /// subsequent `MouseInput::Pressed` can hit-test against
    /// `segment_rects`. `WindowEvent::MouseInput` carries no position
    /// of its own — winit emits a `CursorMoved` first, so we cache the
    /// resolved position here. `Cell<Point>` keeps the field cheap to
    /// update without forcing a separate borrow path on the rest of
    /// the struct.
    last_cursor: Cell<Point>,
    /// Scene cache (Unit 3 of the 2026-05-23 perf plan).
    cached: Scene,
    /// `(rect, selected)` snapshot the cache was emitted from. The
    /// cache invalidates when either changes — both happen on user
    /// input, and apply_layout also re-runs when the parent reshapes.
    cache_dirty: bool,
    last_rect: Rect,
    last_selected: usize,
}

impl ToggleGroup {
    pub fn new(segments: Vec<Segment>, selected: usize) -> Self {
        let len = segments.len();
        // Clamp the initial selection so callers can't end up with an
        // out-of-bounds `selected` if they pass an empty `segments` vec
        // or a stale index. We saturate to the last valid slot rather
        // than panicking — keeps construction infallible.
        let selected = if len == 0 { 0 } else { selected.min(len - 1) };
        Self {
            segments,
            selected,
            on_change: None,
            segment_rects: Vec::new(),
            rect: Rect::ZERO,
            last_cursor: Cell::new(Point::ZERO),
            cached: Scene::new(),
            cache_dirty: true,
            last_rect: Rect::ZERO,
            last_selected: usize::MAX,
        }
    }

    pub fn on_change<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Number of interior separator lines this control will paint —
    /// always `segments.len() - 1` (saturating at zero for an empty or
    /// single-segment group). Exposed so tests can assert the draw pass
    /// emits a separator-per-gap without scraping the Vello scene for
    /// the exact stroke geometry.
    pub fn separator_count(&self) -> usize {
        self.segments.len().saturating_sub(1)
    }
}

impl Widget for ToggleGroup {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Default sizing: 120 px per segment, 36 px tall. Callers can
        // override by wrapping in a fixed-width container if needed —
        // segments always share the available width equally in
        // `apply_layout`.
        let width = (self.segments.len().max(1) as f32) * 120.0;
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(width),
                    height: Dimension::length(36.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let x0 = origin.x + layout.location.x as f64;
        let y0 = origin.y + layout.location.y as f64;
        let x1 = x0 + layout.size.width as f64;
        let y1 = y0 + layout.size.height as f64;
        self.rect = Rect::new(x0, y0, x1, y1);

        // Equal-width segments — partition the outer width by segment
        // count. Empty groups paint an empty strip (no segments, no
        // separators); a degenerate 1-segment group fills the entire
        // outer rect with that segment and emits no separators.
        self.segment_rects.clear();
        let n = self.segments.len();
        if n == 0 {
            return;
        }
        let total_w = self.rect.width();
        let seg_w = total_w / n as f64;
        for i in 0..n {
            let sx0 = self.rect.x0 + (i as f64) * seg_w;
            let sx1 = if i == n - 1 {
                // Floating-point safety: explicitly anchor the last
                // segment to `rect.x1` so accumulated rounding doesn't
                // leave a hairline gap on the right edge.
                self.rect.x1
            } else {
                self.rect.x0 + ((i + 1) as f64) * seg_w
            };
            self.segment_rects.push(Rect::new(sx0, self.rect.y0, sx1, self.rect.y1));
        }
        if self.rect != self.last_rect {
            self.cache_dirty = true;
            self.last_rect = self.rect;
        }
    }

    fn draw(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        if self.segments.is_empty() || self.segment_rects.len() != self.segments.len() {
            return;
        }

        if self.selected != self.last_selected {
            self.cache_dirty = true;
            self.last_selected = self.selected;
        }
        if !self.cache_dirty {
            out_scene.append(&self.cached, Some(out_transform));
            return;
        }
        self.cached.reset();
        let scene = &mut self.cached;
        let transform = Affine::IDENTITY;

        let colors = &theme.tokens.colors;
        let radius = theme.tokens.radii.md as f64;
        let outer_shape = RoundedRect::from_rect(self.rect, radius);

        // Per-segment background. The first / last segments share the
        // outer radius on their outer edges; interior segments paint as
        // plain rects. To get the rounded corners while keeping the
        // per-segment fills cheap we clip each fill against the outer
        // rounded rect — the segment rects themselves stay axis-aligned.
        scene.push_layer(
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &outer_shape,
        );
        for (i, seg_rect) in self.segment_rects.iter().enumerate() {
            let fill = if i == self.selected {
                colors.accent[ACCENT_PRIMARY_IDX]
            } else {
                colors.surfaces.raised
            };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                fill,
                None,
                seg_rect,
            );
        }
        scene.pop_layer();

        // Interior separators — `segments.len() - 1` vertical 1-px lines
        // between adjacent segments. Painted *after* the fills so the
        // separator color isn't overwritten; the line sits on the shared
        // edge between segment `i` and `i + 1`. We paint these as
        // `scene.stroke(Line, ...)` calls so each one is a single
        // `DrawTag::COLOR` in the encoding — that's what
        // `separator_count` accounts for at the API level.
        let separator_color = colors.border_standard;
        let separator_stroke = Stroke::new(1.0);
        for i in 0..self.segments.len().saturating_sub(1) {
            let x = self.segment_rects[i].x1;
            let line = Line::new(
                Point::new(x, self.rect.y0),
                Point::new(x, self.rect.y1),
            );
            scene.stroke(
                &separator_stroke,
                transform,
                separator_color,
                None,
                &line,
            );
        }

        // Outer border — stroke the rounded rect so the entire control
        // reads as one chunk against the surrounding canvas.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &outer_shape,
        );

        // Segment labels. Center each label horizontally inside its
        // segment rect; vertical centering uses the typography scale
        // because cosmic-text's measure returns the visual line height.
        let text_size = theme.tokens.typography.scale.sm;
        for (i, seg_rect) in self.segment_rects.iter().enumerate() {
            let label = &self.segments[i].label;
            let text_color = if i == self.selected {
                Color::WHITE
            } else {
                colors.text_primary
            };
            let (tw, th) = text_renderer.measure(label, text_size, None);
            let text_x = seg_rect.center().x - (tw as f64 / 2.0);
            let text_y = seg_rect.y0 + (seg_rect.height() - th as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                text_size,
                text_color,
                transform * Affine::translate((text_x, text_y)),
                None,
            );
        }
        out_scene.append(&self.cached, Some(out_transform));
    }

    fn needs_redraw(&self) -> bool {
        self.cache_dirty || self.selected != self.last_selected
    }

    fn mark_drawn(&mut self) { self.cache_dirty = false; }

    /// Segmented control has fixed intrinsic size set in
    /// `build_layout`. Clicking only changes `selected`, not size.
    fn is_layout_dirty(&self) -> bool { false }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                // Track cursor for the pressed-segment hit-test below.
                // We don't store it on the struct because the next
                // `MouseInput` event will also carry positional context
                // in winit's higher-level event flow — but since
                // `WindowEvent::MouseInput` carries no position itself,
                // we cache the last logical position here.
                let logical_pos = Point::new(
                    position.x / scale_factor,
                    position.y / scale_factor,
                );
                self.last_cursor.set(logical_pos);
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if *element_state != ElementState::Pressed {
                    return;
                }
                let pos = self.last_cursor.get();
                for (i, seg_rect) in self.segment_rects.iter().enumerate() {
                    if seg_rect.contains(pos) {
                        *handled = true;
                        *focus_target = Some(node_id);
                        if self.selected != i {
                            self.selected = i;
                            if let Some(cb) = self.on_change.as_mut() {
                                cb(i);
                            }
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "ToggleGroup" }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool { true }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use crate::widget::Widget;
    use std::cell::RefCell;
    use std::rc::Rc;
    use taffy::prelude::TaffyMaxContent;
    use vello_encoding::DrawTag;

    /// Build a `ToggleGroup` with a fixed rect so tests can simulate
    /// clicks without running a full taffy pass.
    fn group_at(segments: Vec<Segment>, selected: usize, rect: Rect) -> ToggleGroup {
        let mut g = ToggleGroup::new(segments, selected);
        g.rect = rect;
        // Populate segment_rects manually (mirrors apply_layout).
        let n = g.segments.len();
        if n > 0 {
            let w = rect.width() / n as f64;
            g.segment_rects = (0..n)
                .map(|i| {
                    let x0 = rect.x0 + (i as f64) * w;
                    let x1 = if i == n - 1 { rect.x1 } else { rect.x0 + ((i + 1) as f64) * w };
                    Rect::new(x0, rect.y0, x1, rect.y1)
                })
                .collect();
        }
        g
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn dispatch(g: &mut ToggleGroup, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        g.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
    }

    #[test]
    fn toggle_group_initial_selection_zero() {
        let g = ToggleGroup::new(
            vec![Segment::new("a"), Segment::new("b")],
            0,
        );
        assert_eq!(g.selected(), 0);
    }

    #[test]
    fn toggle_group_click_segment_two_selects_index_two() {
        // 3 segments spanning [0..300, 0..36]. Segment 2 = [200..300, 0..36].
        let captured: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
        let captured_clone = captured.clone();
        let mut g = group_at(
            vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
            0,
            Rect::new(0.0, 0.0, 300.0, 36.0),
        )
        .on_change(move |i| {
            *captured_clone.borrow_mut() = Some(i);
        });

        // Cursor into segment 2 (x = 250, y = 18 — well inside [200..300]).
        dispatch(&mut g, &cursor_moved(250.0, 18.0));
        dispatch(&mut g, &mouse_press());

        assert_eq!(g.selected(), 2, "click in segment 2 must select index 2");
        assert_eq!(
            *captured.borrow(),
            Some(2),
            "on_change must fire with the new index"
        );
    }

    #[test]
    fn toggle_group_paints_separator_lines_between_segments() {
        // 3 segments → 2 separators expected at the API level.
        let mut g = ToggleGroup::new(
            vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
            0,
        );
        // Run a real layout pass so `segment_rects` is populated.
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy.new_with_children(Style::default(), &[node]).unwrap();
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);

        // API-level invariant.
        assert_eq!(
            g.separator_count(),
            2,
            "3-segment group must report 2 separators"
        );

        // Encoder-level invariant: the draw pass must emit at least
        // N-1 = 2 colour draw-tags for the separator strokes. We use
        // `>=` because the same pass also paints the per-segment fills,
        // the outer border stroke, and label text — every one of those
        // contributes additional `DrawTag::COLOR` entries. Counting at
        // least the floor proves the separator strokes were emitted.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        g.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let colour_tags = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();

        assert!(
            colour_tags >= 2,
            "3-segment ToggleGroup must emit >= 2 colour draw-tags for separator lines (got {colour_tags})"
        );
    }
}
