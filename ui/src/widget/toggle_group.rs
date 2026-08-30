//! `ToggleGroup` — segmented control. A horizontal strip of equal-width
//! buttons where exactly one segment is selected. Clicking a segment
//! activates it and fires the `on_change` callback with the new index.
//!
//! State lives entirely on the widget (no CRDT round-trip) because the
//! segmented control's selection is ephemeral UI sugar — callers persist
//! the choice via the `on_change` callback into their own CRDT slot.
use automerge::{AutoCommit, ObjId};
use std::cell::Cell;
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ON_FILL_MIN_CONTRAST;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

pub struct Segment {
    pub label: String,
}

impl Segment {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

pub struct ToggleGroup {
    segments: Vec<Segment>,
    selected: usize,
    on_change: Option<Box<dyn FnMut(usize)>>,
    /// Indices aligned 1:1 with `segments`; drawing also consults this for
    /// the fill.
    segment_rects: Vec<Rect>,
    rect: Rect,
    /// Captured on `CursorMoved` so the subsequent `MouseInput` (which
    /// carries no position of its own) can hit-test against `segment_rects`.
    last_cursor: Cell<Point>,
    cached: Scene,
    cache_dirty: bool,
    cache_rect: Rect,
    last_selected: usize,
    /// Drives the focus ring drawn in place of the standard outer border.
    is_focused: bool,
    /// `is_focused` at the last cache emit, so a focus change invalidates
    /// the cached scene.
    last_focused: bool,
}

impl ToggleGroup {
    pub fn new(segments: Vec<Segment>, selected: usize) -> Self {
        let len = segments.len();
        // Saturate rather than panic on an out-of-bounds index — keeps
        // construction infallible.
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

    /// Does **not** fire `on_change`, which is reserved for user-driven
    /// changes.
    pub fn set_selected(&mut self, selected: usize) {
        let clamped = selected.min(self.segments.len().saturating_sub(1));
        if self.selected != clamped {
            self.selected = clamped;
            self.cache_dirty = true;
        }
    }

    /// Places the control at an absolute rect without going through taffy,
    /// for an app that hand-positions a shared control over its own chrome.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.set_rect_and_partition(rect);
    }

    /// Shared by the taffy [`Widget::apply_layout`] path and
    /// [`Self::apply_layout_external`] so both compute identical geometry.
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
                // Anchor to `rect.x1` so accumulated rounding doesn't leave
                // a hairline gap on the right edge.
                self.rect.x1
            } else {
                self.rect.x0 + ((i + 1) as f64) * seg_w
            };
            self.segment_rects
                .push(Rect::new(sx0, self.rect.y0, sx1, self.rect.y1));
        }
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.cache_dirty = true;
        }
    }

    /// Exposed so tests can assert the draw pass emits a separator-per-gap
    /// without scraping the Vello scene for exact stroke geometry.
    pub fn separator_count(&self) -> usize {
        self.segments.len().saturating_sub(1)
    }
}

/// Visual style for an immediate-mode segmented control painted via
/// [`ToggleGroup::paint`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SegmentedStyle {
    /// Unified strip: every segment filled, interior separators, one outer
    /// border. Rects must be contiguous.
    #[default]
    Solid,
    /// Discrete pills: only the selected or hovered pill is filled, no
    /// outer border or separators.
    Pill,
    /// No fill; the selected tab carries a short accent underline bar and a
    /// lifted label colour.
    Underline,
}

/// The accent is a themeable token, so the colour that reads on it is not a
/// constant: white is right on a dark accent but falls below contrast on a
/// bright one. Both the retained draw and [`segment_colors`] resolve
/// through here so the strip and its immediate-mode face cannot disagree.
pub fn selected_segment_ink(theme: &Theme) -> Color {
    theme.tokens.colors.on_accent(ON_FILL_MIN_CONTRAST)
}

/// Resolved across both the resting ground and the hover surface at once,
/// since `bg_hover` sits close enough to the dim tier that a label all but
/// dissolves on hover — so a tab must not change colour under the pointer.
pub fn unselected_segment_ink(theme: &Theme) -> Color {
    theme.tokens.colors.ink_on_any(
        &[theme.tokens.colors.surfaces.base, theme.bg_hover],
        theme.tokens.colors.text_dim,
        ON_FILL_MIN_CONTRAST,
    )
}

/// `(fill, label)` colours for one segment; `fill == None` means
/// transparent.
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
                (Some(theme.accent_primary), selected_segment_ink(theme))
            } else {
                let fill = colors.surfaces.raised;
                (Some(fill), colors.readable_on(fill, colors.text_primary))
            }
        }
        SegmentedStyle::Pill => {
            if selected {
                (Some(theme.accent_primary), selected_segment_ink(theme))
            } else if hovered {
                (Some(theme.bg_hover), unselected_segment_ink(theme))
            } else {
                (None, unselected_segment_ink(theme))
            }
        }
        SegmentedStyle::Underline => {
            // No fill; the selected tab's label lifts to the primary colour,
            // the accent underline (drawn in `paint`) carries the selection.
            let fg = if selected || hovered {
                colors.text_primary
            } else {
                unselected_segment_ink(theme)
            };
            (None, fg)
        }
    }
}

/// Configuration for [`ToggleGroup::paint`]. The caller owns the per-segment
/// rects and hit-test; this paints the themed look over them.
pub struct SegmentedFace<'a> {
    segments: &'a [Rect],
    labels: &'a [&'a str],
    selected: usize,
    hovered: Option<usize>,
    style: SegmentedStyle,
    radius: f64,
    text_size: f32,
    weight: crate::text_render::TextWeight,
    /// `Some(pad)` left-aligns labels instead of centring.
    label_pad_left: Option<f64>,
}

impl<'a> SegmentedFace<'a> {
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
    pub fn style(mut self, style: SegmentedStyle) -> Self {
        self.style = style;
        self
    }
    pub fn pill(mut self) -> Self {
        self.style = SegmentedStyle::Pill;
        self
    }
    pub fn hovered(mut self, idx: Option<usize>) -> Self {
        self.hovered = idx;
        self
    }
    pub fn radius(mut self, r: f64) -> Self {
        self.radius = r;
        self
    }
    pub fn text_size(mut self, s: f32) -> Self {
        self.text_size = s;
        self
    }
    pub fn weight(mut self, w: crate::text_render::TextWeight) -> Self {
        self.weight = w;
        self
    }
    pub fn left_labels(mut self, pad: f64) -> Self {
        self.label_pad_left = Some(pad);
        self
    }
}

impl ToggleGroup {
    /// For canvas apps that own their own hit-testing: call this to paint,
    /// keep your own hit-test to fire the selection.
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
                let outer = Rect::new(segs[0].x0, segs[0].y0, segs[segs.len() - 1].x1, segs[0].y1);
                let outer_shape = RoundedRect::from_rect(outer, face.radius);
                // Clip per-segment fills to the rounded outer so the end
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
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    colors.border_standard,
                    None,
                    &outer_shape,
                );
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
                // Short accent underline bar inset from the tab's edges.
                if let Some(seg) = segs.get(face.selected) {
                    let inset = 12.0_f64.min((seg.width() / 2.0 - 2.0).max(0.0));
                    let bar = Rect::new(seg.x0 + inset, seg.y1 - 2.0, seg.x1 - inset, seg.y1);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        theme.accent_primary,
                        None,
                        &bar,
                    );
                }
            }
        }

        // Selected pills use a slightly heavier weight so the active choice
        // reads first.
        for (i, seg) in segs.iter().enumerate() {
            let label = face.labels.get(i).copied().unwrap_or("");
            if label.is_empty() {
                continue;
            }
            let (_, fg) = segment_colors(
                theme,
                face.style,
                i == face.selected,
                face.hovered == Some(i),
            );
            let weight = if i == face.selected {
                crate::text_render::TextWeight::Semibold
            } else {
                face.weight
            };
            let (tw, th) = text_renderer.measure_weighted(label, face.text_size, None, weight);
            let text_x = match face.label_pad_left {
                Some(pad) => seg.x0 + pad,
                None => seg.center().x - f64::from(tw) / 2.0,
            };
            let text_y = seg.y0 + (seg.height() - f64::from(th)) / 2.0;
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
        // 120px per segment, 36px tall; segments always share the
        // available width equally in `apply_layout`.
        let width = (self.segments.len().max(1) as f32) * 120.0;
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(width),
                height: Dimension::length(36.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x0 = origin.x + f64::from(layout.location.x);
        let y0 = origin.y + f64::from(layout.location.y);
        let x1 = x0 + f64::from(layout.size.width);
        let y1 = y0 + f64::from(layout.size.height);
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
            out_scene.append(
                &self.cached,
                Some(out_transform * Affine::translate((dx, dy))),
            );
            return;
        }
        self.cached.reset();
        let scene = &mut self.cached;
        let transform = Affine::IDENTITY;

        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.md);
        let outer_shape = RoundedRect::from_rect(self.rect, radius);

        // Clip each fill against the outer rounded rect so the end segments
        // inherit the corner radius.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &outer_shape,
        );
        for (i, seg_rect) in self.segment_rects.iter().enumerate() {
            // Through `segment_colors` rather than re-deriving the palette, so
            // the retained control and `paint` cannot drift apart.
            let (fill, _) = segment_colors(theme, SegmentedStyle::Solid, i == self.selected, false);
            let Some(fill) = fill else { continue };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                fill,
                None,
                seg_rect,
            );
        }
        scene.pop_layer();

        // Each `stroke(Line, ...)` call is a single `DrawTag::COLOR` entry —
        // what `separator_count` accounts for at the API level.
        let separator_color = colors.border_standard;
        let separator_stroke = Stroke::new(1.0);
        for i in 0..self.segments.len().saturating_sub(1) {
            let x = self.segment_rects[i].x1;
            let line = Line::new(Point::new(x, self.rect.y0), Point::new(x, self.rect.y1));
            scene.stroke(&separator_stroke, transform, separator_color, None, &line);
        }

        // Focus ring: prominent focus colour at 2px.
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

        // Vertical centering uses the typography scale because cosmic-text's
        // measure returns the visual line height.
        let text_size = theme.tokens.typography.scale.sm;
        for (i, seg_rect) in self.segment_rects.iter().enumerate() {
            let label = &self.segments[i].label;
            let (_, text_color) =
                segment_colors(theme, SegmentedStyle::Solid, i == self.selected, false);
            let (tw, th) = text_renderer.measure(label, text_size, None);
            let text_x = seg_rect.center().x - (f64::from(tw) / 2.0);
            let text_y = seg_rect.y0 + (seg_rect.height() - f64::from(th)) / 2.0;
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

    fn mark_drawn(&mut self) {
        self.cache_dirty = false;
    }

    /// Fixed intrinsic size; `Rect::ZERO` sentinel covers the
    /// pre-`apply_layout` state.
    fn is_layout_dirty(&self) -> bool {
        self.cache_rect == Rect::ZERO
    }

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
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
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

    fn name(&self) -> &'static str {
        "ToggleGroup"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Segment labels are runtime strings (host-provided), not FTL
        // keys, so no `i18n!` wrap.
        let value = self.segments.get(self.selected).map(|s| s.label.clone());
        // Segments are drawn, not child widgets, so expose each as a
        // synthetic child so an AT enumerates the choices, not just the
        // active one.
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

    /// An **inactive** segment that carries a fill must put a label on it that
    /// clears WCAG AA for body text, in every theme.
    ///
    /// The hovered pill did not: `text_dim` on the hover surface measures
    /// 2.96:1 in Catppuccin Latte, 3.11:1 in Beige, 3.46:1 in the dark
    /// default and 3.49:1 in the light one — below AA in all 14 presets. The
    /// closely-related `neutral[3]` fill tier under the same label measures
    /// 4.26:1 in Light against 4.69:1 in Dark, which is the shape of the
    /// problem: a pairing that scrapes past the bar in the theme it was
    /// authored in and fails in the others.
    ///
    /// Sweeping every theme is the point. A check against the default alone
    /// cannot see a defect whose whole nature is that it is theme-dependent.
    #[test]
    fn inactive_segment_labels_clear_aa_in_every_theme() {
        use crate::a11y::contrast_ratio;
        use crate::color_compat::ColorCompat;

        let mut checked = 0;
        for name in Theme::ALL {
            let theme = Theme::named(name).unwrap();
            for style in [
                SegmentedStyle::Solid,
                SegmentedStyle::Pill,
                SegmentedStyle::Underline,
            ] {
                for hovered in [false, true] {
                    let (fill, label) = segment_colors(&theme, style, false, hovered);
                    // A transparent segment's label lands on the host's ground,
                    // which the theme's own text-tier invariants already cover.
                    let Some(fill) = fill else { continue };
                    assert_eq!(fill.a(), 255, "{name}/{style:?}: segment fills are opaque");
                    let r = contrast_ratio(label, fill);
                    assert!(
                        r >= 4.5,
                        "{name} {style:?} (hovered={hovered}): label on fill is \
                         {r:.2}:1, below AA",
                    );
                    checked += 1;
                }
            }
        }
        // Solid unselected (×2 hover states) + Pill hovered, per theme. Pinned
        // so a style that stops returning a fill silently empties this sweep.
        assert_eq!(
            checked,
            Theme::ALL.len() * 3,
            "every themed fill was measured"
        );
    }

    /// The correction must be inert wherever the pairing already passed, so a
    /// theme that was designed correctly is not repainted. The dark default's
    /// unselected solid segment keeps primary text on the raised card, and its
    /// `neutral[3]`/`text_dim` pairing — 4.69:1, just over the bar — keeps the
    /// dim tier the design chose.
    #[test]
    fn themes_that_already_passed_are_left_alone() {
        use crate::a11y::contrast_ratio;

        let t = Theme::dark();
        let c = &t.tokens.colors;
        let (fill, label) = segment_colors(&t, SegmentedStyle::Solid, false, false);
        assert_eq!(fill, Some(c.surfaces.raised));
        assert_eq!(label, c.text_primary);

        assert!(
            contrast_ratio(c.text_dim, c.neutral[3]) >= 4.5,
            "precondition: dark passes"
        );
        assert_eq!(c.readable_on(c.neutral[3], c.text_dim), c.text_dim);
    }

    /// Fixed rect so tests can simulate clicks without a full taffy pass.
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
                    let x1 = if i == n - 1 {
                        rect.x1
                    } else {
                        rect.x0 + ((i + 1) as f64) * w
                    };
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
        let node = tree.leaf_node(Style::default());
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
        let g = ToggleGroup::new(vec![Segment::new("a"), Segment::new("b")], 0);
        assert_eq!(g.selected(), 0);
    }

    #[test]
    fn toggle_group_click_segment_two_selects_index_two() {
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
        let mut g = ToggleGroup::new(
            vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
            0,
        );
        // Run a real layout pass so `segment_rects` is populated.
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);

        assert_eq!(
            g.separator_count(),
            2,
            "3-segment group must report 2 separators"
        );

        // `>=` since fills/border/text also contribute `DrawTag::COLOR`
        // entries; the floor proves the separator strokes were emitted.
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

    /// A focus change swaps the outer border colour, so the encoded stream
    /// differs between focused and unfocused renders.
    fn group_draw_data(focused: bool, theme: &Theme) -> Vec<u32> {
        let mut g = ToggleGroup::new(vec![Segment::new("a"), Segment::new("b")], 0);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);
        g.set_focus(focused);

        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        g.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
        scene.encoding().draw_data.clone()
    }

    /// Uses high contrast (yellow focus vs white border) so the two colours
    /// are unambiguously different.
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
    fn segment_colors_solid_selected_is_accent_and_readable() {
        use crate::a11y::contrast_ratio;
        let theme = Theme::dark();
        let (fill, fg) = segment_colors(&theme, SegmentedStyle::Solid, true, false);
        assert_eq!(fill, Some(theme.accent_primary));
        let r = contrast_ratio(fg, fill.unwrap());
        assert!(
            r >= ON_FILL_MIN_CONTRAST,
            "selected segment label is {r:.2}:1 on the accent"
        );
        let (ufill, ufg) = segment_colors(&theme, SegmentedStyle::Solid, false, false);
        assert_eq!(
            ufill,
            Some(theme.tokens.colors.surfaces.raised),
            "unselected solid is a raised fill"
        );
        assert_eq!(ufg, theme.tokens.colors.text_primary);
    }

    #[test]
    fn segment_colors_pill_unselected_is_transparent() {
        let theme = Theme::dark();
        let (fill, fg) = segment_colors(&theme, SegmentedStyle::Pill, false, false);
        assert_eq!(fill, None, "an idle unselected pill paints no background");
        use crate::a11y::contrast_ratio;
        let base = theme.tokens.colors.surfaces.base;
        assert!(
            contrast_ratio(fg, base) < contrast_ratio(theme.tokens.colors.text_primary, base),
            "inactive tab label must stay quieter than the selected tier",
        );
        assert!(contrast_ratio(fg, theme.bg_hover) >= ON_FILL_MIN_CONTRAST);
        // Hover must not repaint the label — a colour change under the
        // pointer reads as a bug.
        let (hfill, hfg) = segment_colors(&theme, SegmentedStyle::Pill, false, true);
        assert_eq!(hfill, Some(theme.bg_hover));
        assert_eq!(hfg, fg, "the inactive label holds its colour on hover");
        let (sfill, sfg) = segment_colors(&theme, SegmentedStyle::Pill, true, false);
        assert_eq!(sfill, Some(theme.accent_primary));
        assert_eq!(
            sfg,
            segment_colors(&theme, SegmentedStyle::Solid, true, false).1,
            "pill and solid resolve the selected label the same way",
        );
        assert!(contrast_ratio(sfg, sfill.unwrap()) >= ON_FILL_MIN_CONTRAST);
    }

    #[test]
    fn segmented_paint_emits_fills_over_caller_rects() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let segs = [
            Rect::new(0.0, 0.0, 60.0, 28.0),
            Rect::new(60.0, 0.0, 120.0, 28.0),
            Rect::new(120.0, 0.0, 180.0, 28.0),
        ];
        let labels = ["A", "B", "C"];
        for style in [
            SegmentedStyle::Solid,
            SegmentedStyle::Pill,
            SegmentedStyle::Underline,
        ] {
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
        let mut scene = Scene::new();
        ToggleGroup::paint(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &SegmentedFace::new(&segs, &labels, 0)
                .pill()
                .left_labels(14.0),
        );
        assert!(
            scene.encoding().draw_tags.contains(&DrawTag::COLOR),
            "left-labelled pill paint must emit draw commands",
        );
    }

    #[test]
    fn accessibility_reports_group_role_and_selected_segment_label() {
        use crate::a11y::Role;
        let g = ToggleGroup::new(
            vec![
                Segment::new("Light"),
                Segment::new("Dark"),
                Segment::new("Auto"),
            ],
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

        assert_eq!(
            Widget::accessibility(&g).unwrap().value.as_deref(),
            Some("a")
        );

        // Click into segment 2 → value follows the new selection.
        dispatch(&mut g, &cursor_moved(250.0, 18.0));
        dispatch(&mut g, &mouse_press());
        assert_eq!(g.selected(), 2);
        assert_eq!(
            Widget::accessibility(&g).unwrap().value.as_deref(),
            Some("c")
        );
    }
}
