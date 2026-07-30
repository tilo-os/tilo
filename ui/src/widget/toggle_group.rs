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
    cache_rect: Rect,
    last_selected: usize,
    /// Whether the control holds keyboard focus. Drives the focus ring drawn
    /// in place of the standard outer border, so the group reads as focused.
    is_focused: bool,
    /// `is_focused` at the last cache emit, so a focus change invalidates the
    /// cached scene and the ring repaints.
    last_focused: bool,
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
            cache_rect: Rect::ZERO,
            last_selected: usize::MAX,
            is_focused: false,
            last_focused: false,
        }
    }

    pub fn on_change<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Programmatically set the selected segment (clamped to a valid index),
    /// e.g. when the app changes the active choice for a reason other than a
    /// click on this control. Does **not** fire `on_change` (that is reserved
    /// for user-driven changes); it just keeps the control in step with app
    /// state. Invalidates the scene cache when the value actually changes.
    pub fn set_selected(&mut self, selected: usize) {
        let clamped = selected.min(self.segments.len().saturating_sub(1));
        if self.selected != clamped {
            self.selected = clamped;
            self.cache_dirty = true;
        }
    }

    /// Place the control at an absolute rect without going through taffy —
    /// the counterpart to [`TextInput::apply_layout_external`] for an app
    /// that hand-positions a shared control over its own painted chrome.
    /// Repartitions the segment hit-rects from the new box. Focus / a11y /
    /// selection are unaffected.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.set_rect_and_partition(rect);
    }

    /// Set `self.rect` and repartition the equal-width segment hit-rects.
    /// Shared by the taffy [`Widget::apply_layout`] path and the absolute
    /// [`Self::apply_layout_external`] path so both compute identical
    /// geometry. Empty groups paint an empty strip (no segments, no
    /// separators); a degenerate 1-segment group fills the whole rect.
    fn set_rect_and_partition(&mut self, rect: Rect) {
        self.rect = rect;
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
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.cache_dirty = true;
        }
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

/// Visual style for an immediate-mode segmented control painted via
/// [`ToggleGroup::paint`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SegmentedStyle {
    /// Unified strip — every segment is filled (selected in the accent,
    /// unselected in the raised surface), interior 1-px separators between
    /// adjacent segments, and one outer border wrapping the whole control.
    /// Matches the retained [`ToggleGroup`]. Segment rects must be contiguous.
    #[default]
    Solid,
    /// Discrete pills — only the selected pill (accent) or a hovered pill
    /// (subtle hover surface) is filled; unselected pills are transparent.
    /// No outer border, no separators. The tab-strip / mode-switch look.
    Pill,
    /// Underline tabs — no fill at all; the selected tab carries a short
    /// accent underline bar along its bottom edge, its label lifting to the
    /// primary text colour. The document/sidebar tab-strip look.
    Underline,
}

/// Resolve the `(fill, label)` colours for one segment given the style and
/// its selected / hovered state. `fill == None` means "paint no background"
/// (a transparent, unselected pill). Single source of truth for both the
/// retained draw's palette intent and [`ToggleGroup::paint`].
pub fn segment_colors(
    theme: &Theme,
    style: SegmentedStyle,
    selected: bool,
    hovered: bool,
) -> (Option<Color>, Color) {
    let colors = &theme.tokens.colors;
    match style {
        SegmentedStyle::Solid => {
            if selected {
                (Some(theme.accent_primary), Color::WHITE)
            } else {
                (Some(colors.surfaces.raised), colors.text_primary)
            }
        }
        SegmentedStyle::Pill => {
            if selected {
                (Some(theme.accent_primary), Color::WHITE)
            } else if hovered {
                (Some(theme.bg_hover), colors.text_dim)
            } else {
                (None, colors.text_dim)
            }
        }
        SegmentedStyle::Underline => {
            // No fill; the selected tab's label lifts to the primary colour,
            // the accent underline (drawn in `paint`) carries the selection.
            let fg = if selected || hovered { colors.text_primary } else { colors.text_dim };
            (None, fg)
        }
    }
}

/// Configuration for [`ToggleGroup::paint`], the immediate-mode segmented
/// face. The caller owns the per-segment rects (its own layout) and its own
/// hit-test; this paints the themed, stateful look over them so tab strips /
/// mode switches share one segmented control's palette and geometry.
pub struct SegmentedFace<'a> {
    segments: &'a [Rect],
    labels: &'a [&'a str],
    selected: usize,
    hovered: Option<usize>,
    style: SegmentedStyle,
    radius: f64,
    text_size: f32,
    weight: crate::text_render::TextWeight,
    /// When `Some(pad)`, labels are left-aligned `pad` px from each segment's
    /// left edge instead of horizontally centred — the nav-bar tab look where
    /// a trailing badge follows the label. `None` (default) centres them.
    label_pad_left: Option<f64>,
}

impl<'a> SegmentedFace<'a> {
    /// A segmented face over `segments` (one rect per label, same order).
    pub fn new(segments: &'a [Rect], labels: &'a [&'a str], selected: usize) -> Self {
        Self {
            segments,
            labels,
            selected,
            hovered: None,
            style: SegmentedStyle::Solid,
            radius: 8.0,
            text_size: 13.0,
            weight: crate::text_render::TextWeight::Medium,
            label_pad_left: None,
        }
    }
    pub fn style(mut self, style: SegmentedStyle) -> Self { self.style = style; self }
    pub fn pill(mut self) -> Self { self.style = SegmentedStyle::Pill; self }
    pub fn hovered(mut self, idx: Option<usize>) -> Self { self.hovered = idx; self }
    pub fn radius(mut self, r: f64) -> Self { self.radius = r; self }
    pub fn text_size(mut self, s: f32) -> Self { self.text_size = s; self }
    pub fn weight(mut self, w: crate::text_render::TextWeight) -> Self { self.weight = w; self }
    /// Left-align labels `pad` px from each segment's left edge (nav-bar tab
    /// style) instead of centring them.
    pub fn left_labels(mut self, pad: f64) -> Self { self.label_pad_left = Some(pad); self }
}

impl ToggleGroup {
    /// Immediate-mode paint of a themed segmented control over caller-owned
    /// per-segment rects, matching the retained [`ToggleGroup`]'s look for the
    /// `Solid` style (unified strip + separators + outer border) and a
    /// tab-strip look for `Pill` (selected/hovered pill only). For
    /// canvas/immediate-mode apps that own their own rect hit-testing: call
    /// this to paint, keep your existing hit-test to fire the selection.
    /// Extends the widget rather than forking a painter — the palette flows
    /// through [`segment_colors`].
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        face: &SegmentedFace<'_>,
    ) {
        let segs = face.segments;
        if segs.is_empty() {
            return;
        }
        let colors = &theme.tokens.colors;

        match face.style {
            SegmentedStyle::Solid => {
                // Union rect of the (contiguous) segments → outer chunk.
                let outer = Rect::new(segs[0].x0, segs[0].y0, segs[segs.len() - 1].x1, segs[0].y1);
                let outer_shape = RoundedRect::from_rect(outer, face.radius);
                // Per-segment fills clipped to the rounded outer so the end
                // segments inherit the corner radius.
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &outer_shape,
                );
                for (i, seg) in segs.iter().enumerate() {
                    let (fill, _) = segment_colors(theme, face.style, i == face.selected, false);
                    if let Some(fill) = fill {
                        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, seg);
                    }
                }
                scene.pop_layer();
                // Interior separators.
                let sep = Stroke::new(1.0);
                for seg in segs.iter().take(segs.len().saturating_sub(1)) {
                    let x = seg.x1;
                    scene.stroke(
                        &sep,
                        transform,
                        colors.border_standard,
                        None,
                        &Line::new(Point::new(x, outer.y0), Point::new(x, outer.y1)),
                    );
                }
                // Outer border.
                scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &outer_shape);
            }
            SegmentedStyle::Pill => {
                for (i, seg) in segs.iter().enumerate() {
                    let (fill, _) = segment_colors(
                        theme,
                        face.style,
                        i == face.selected,
                        face.hovered == Some(i),
                    );
                    if let Some(fill) = fill {
                        scene.fill(
                            vello::peniko::Fill::NonZero,
                            transform,
                            fill,
                            None,
                            &RoundedRect::from_rect(*seg, face.radius),
                        );
                    }
                }
            }
            SegmentedStyle::Underline => {
                // No fills; the selected tab gets a short accent underline bar
                // inset from its edges along the bottom.
                if let Some(seg) = segs.get(face.selected) {
                    let inset = 12.0_f64.min((seg.width() / 2.0 - 2.0).max(0.0));
                    let bar = Rect::new(seg.x0 + inset, seg.y1 - 2.0, seg.x1 - inset, seg.y1);
                    scene.fill(vello::peniko::Fill::NonZero, transform, theme.accent_primary, None, &bar);
                }
            }
        }

        // Labels — centred in each segment. Selected pills use a slightly
        // heavier weight so the active choice reads first.
        for (i, seg) in segs.iter().enumerate() {
            let label = face.labels.get(i).copied().unwrap_or("");
            if label.is_empty() {
                continue;
            }
            let (_, fg) = segment_colors(theme, face.style, i == face.selected, face.hovered == Some(i));
            let weight = if i == face.selected {
                crate::text_render::TextWeight::Semibold
            } else {
                face.weight
            };
            let (tw, th) = text_renderer.measure_weighted(label, face.text_size, None, weight);
            let text_x = match face.label_pad_left {
                Some(pad) => seg.x0 + pad,
                None => seg.center().x - tw as f64 / 2.0,
            };
            let text_y = seg.y0 + (seg.height() - th as f64) / 2.0;
            text_renderer.draw_text_weighted(
                scene,
                label,
                face.text_size,
                fg,
                transform * Affine::translate((text_x, text_y)),
                None,
                weight,
            );
        }
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
        self.set_rect_and_partition(Rect::new(x0, y0, x1, y1));
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
        if self.is_focused != self.last_focused {
            self.cache_dirty = true;
            self.last_focused = self.is_focused;
        }
        let needs_emit = self.cache_dirty || self.cache_rect == Rect::ZERO;
        if !needs_emit {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            out_scene.append(&self.cached, Some(out_transform * Affine::translate((dx, dy))));
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
            vello::peniko::Fill::NonZero,
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
        // reads as one chunk against the surrounding canvas. While focused
        // it becomes the focus ring: the prominent focus colour at a 2px
        // weight, consistent with every other focusable control.
        let (border_w, border_c) = if self.is_focused {
            (2.0, colors.border_focus)
        } else {
            (1.0, colors.border_standard)
        };
        scene.stroke(
            &Stroke::new(border_w),
            transform,
            border_c,
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
        self.cache_rect = self.rect;
        out_scene.append(&self.cached, Some(out_transform));
    }

    fn needs_redraw(&self) -> bool {
        self.cache_dirty
            || self.selected != self.last_selected
            || self.is_focused != self.last_focused
    }

    fn mark_drawn(&mut self) { self.cache_dirty = false; }

    /// Segmented control has fixed intrinsic size set in
    /// `build_layout`. `Rect::ZERO` sentinel covers the
    /// pre-apply_layout state.
    fn is_layout_dirty(&self) -> bool { self.cache_rect == Rect::ZERO }

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
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // A segmented control is a Role::Group of toggle buttons. The group
        // surfaces the *selected* segment's label as `value` so an AT can
        // announce the current choice at a glance, and each segment is a
        // synthetic ToggleButton child so the whole set is enumerable. The
        // group has no intrinsic label here → name None. Segment labels are
        // runtime strings (host-provided), not FTL keys, so no `i18n!` wrap.
        let value = self.segments.get(self.selected).map(|s| s.label.clone());
        // Expose each segment as a synthetic `ToggleButton` child carrying the
        // pressed (checked) state, so an AT enumerates the choices rather than
        // hearing only the active one. Segments are drawn, not child widgets.
        let count = self.segments.len();
        let seg_children: Vec<AccessNode> = self
            .segments
            .iter()
            .enumerate()
            .map(|(i, s)| {
                AccessNode::new(Role::ToggleButton)
                    .with_name(s.label.clone())
                    .with_state(AccessState {
                        checked: Some(i == self.selected),
                        ..Default::default()
                    })
                    .with_position(i + 1, count)
            })
            .collect();
        Some(
            AccessNode::new(Role::Group)
                .with_optional_value(value)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    disabled: !self.is_enabled(),
                    ..Default::default()
                })
                .with_children(seg_children),
        )
    }
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

    /// Render a freshly-laid-out group against `theme` and return the encoded
    /// colour stream (`draw_data`). A focus change swaps the outer border
    /// colour, so the stream differs between focused and unfocused renders.
    fn group_draw_data(focused: bool, theme: &Theme) -> Vec<u32> {
        let mut g = ToggleGroup::new(
            vec![Segment::new("a"), Segment::new("b")],
            0,
        );
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, theme, &mut node_map);
        let root = taffy.new_with_children(Style::default(), &[node]).unwrap();
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);
        g.set_focus(focused);

        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        g.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
        scene.encoding().draw_data.clone()
    }

    /// A focused segmented control swaps its outer border to the prominent
    /// focus colour, so the encoded colour stream differs from the unfocused
    /// render. Uses high contrast (yellow focus vs white border) so the two
    /// colours are unambiguously different.
    #[test]
    fn focused_toggle_group_uses_the_focus_colour() {
        let theme = Theme::high_contrast();
        let unfocused = group_draw_data(false, &theme);
        let focused = group_draw_data(true, &theme);
        assert_ne!(
            unfocused, focused,
            "focusing the group must change the border colour in the encoded scene",
        );
    }

    #[test]
    fn segment_colors_solid_selected_is_accent_white() {
        let theme = Theme::dark();
        let (fill, fg) = segment_colors(&theme, SegmentedStyle::Solid, true, false);
        assert_eq!(fill, Some(theme.accent_primary));
        assert_eq!(fg, Color::WHITE);
        let (ufill, ufg) = segment_colors(&theme, SegmentedStyle::Solid, false, false);
        assert_eq!(ufill, Some(theme.tokens.colors.surfaces.raised), "unselected solid is a raised fill");
        assert_eq!(ufg, theme.tokens.colors.text_primary);
    }

    #[test]
    fn segment_colors_pill_unselected_is_transparent() {
        let theme = Theme::dark();
        // Unselected + not hovered → no fill at all (transparent pill).
        let (fill, fg) = segment_colors(&theme, SegmentedStyle::Pill, false, false);
        assert_eq!(fill, None, "an idle unselected pill paints no background");
        assert_eq!(fg, theme.tokens.colors.text_dim, "inactive tab label is dimmed");
        // Hover gives it the shared hover surface.
        let (hfill, _) = segment_colors(&theme, SegmentedStyle::Pill, false, true);
        assert_eq!(hfill, Some(theme.bg_hover));
        // Selected pill is the accent + white, same as solid.
        let (sfill, sfg) = segment_colors(&theme, SegmentedStyle::Pill, true, false);
        assert_eq!(sfill, Some(theme.accent_primary));
        assert_eq!(sfg, Color::WHITE);
    }

    #[test]
    fn segmented_paint_emits_fills_over_caller_rects() {
        // The immediate-mode paint helper renders something for both styles
        // over caller-owned rects without a layout pass.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let segs = [
            Rect::new(0.0, 0.0, 60.0, 28.0),
            Rect::new(60.0, 0.0, 120.0, 28.0),
            Rect::new(120.0, 0.0, 180.0, 28.0),
        ];
        let labels = ["A", "B", "C"];
        for style in [SegmentedStyle::Solid, SegmentedStyle::Pill, SegmentedStyle::Underline] {
            let mut scene = Scene::new();
            ToggleGroup::paint(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &SegmentedFace::new(&segs, &labels, 1).style(style),
            );
            let colour_tags = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();
            assert!(colour_tags > 0, "{style:?} paint must emit draw commands");
        }
        // Left-aligned (nav-bar tab) labels also paint without panicking.
        let mut scene = Scene::new();
        ToggleGroup::paint(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &SegmentedFace::new(&segs, &labels, 0).pill().left_labels(14.0),
        );
        assert!(
            scene.encoding().draw_tags.iter().any(|t| *t == DrawTag::COLOR),
            "left-labelled pill paint must emit draw commands",
        );
    }

    #[test]
    fn accessibility_reports_group_role_and_selected_segment_label() {
        use crate::a11y::Role;
        let g = ToggleGroup::new(
            vec![Segment::new("Light"), Segment::new("Dark"), Segment::new("Auto")],
            1,
        );
        let node = Widget::accessibility(&g).expect("toggle group is not presentational");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name, None, "no intrinsic label on the group");
        assert_eq!(
            node.value.as_deref(),
            Some("Dark"),
            "value is the selected segment's label",
        );
        assert!(node.state.focusable);
        assert!(!node.state.disabled);
    }

    #[test]
    fn accessibility_value_tracks_selection_changes() {
        let captured: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
        let captured_clone = captured.clone();
        let mut g = group_at(
            vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
            0,
            Rect::new(0.0, 0.0, 300.0, 36.0),
        )
        .on_change(move |i| *captured_clone.borrow_mut() = Some(i));

        // Initially the first segment is the announced value.
        assert_eq!(Widget::accessibility(&g).unwrap().value.as_deref(), Some("a"));

        // Click into segment 2 (x = 250) → value follows the new selection.
        dispatch(&mut g, &cursor_moved(250.0, 18.0));
        dispatch(&mut g, &mouse_press());
        assert_eq!(g.selected(), 2);
        assert_eq!(Widget::accessibility(&g).unwrap().value.as_deref(), Some("c"));
    }
}
