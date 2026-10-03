//! `ToggleGroup` — segmented control. A horizontal strip of equal-width
//! buttons where exactly one segment is selected. Clicking a segment
//! activates it and fires the `on_change` callback with the new index.
//!
//! State lives entirely on the widget (no CRDT round-trip) because the
//! segmented control's selection is ephemeral UI sugar — callers persist
//! the choice via the `on_change` callback into their own CRDT slot, or
//! bind it to typed view state with [`ToggleGroup::from_state`].
//!
//! [`ToggleGroup::vertical`] stacks icon segments into a tool rail: each
//! segment shows its [`Segment::with_icon`] glyph, and its label (with the
//! [`Segment::with_hint`] shortcut) is its tooltip and accessible name.
use crate::widget::{AppModel, ModelPath};
use automerge::{AutoCommit, ObjId};
use std::cell::Cell;
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ON_FILL_MIN_CONTRAST;
use crate::widget::toolbar::Orientation;
use crate::widget::{ActionEmitter, ControlBindingMetadata, TaffyTreeExt, Widget};

/// Lossless `usize -> f64` widening for segment-strip geometry. Segment
/// counts are bounded far below `u32::MAX`, so routing through `u32` (which
/// `f64` represents exactly) avoids the precision loss a direct cast risks.
fn usize_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Lossless `usize -> f32` widening for the same reason, via `u16` (`f32`
/// represents integers up to `2^24` exactly, and no toggle group ever has
/// anywhere near `u16::MAX` segments).
fn usize_to_f32(n: usize) -> f32 {
    f32::from(u16::try_from(n).unwrap_or(u16::MAX))
}

/// Intrinsic strip geometry: 120 px per segment so a two-word label fits
/// without truncation. Its height comes from the theme's HIG panel-row metric,
/// which keeps every segment a full touch target and follows UI density scaling.
/// `apply_layout` re-partitions whatever width the host actually assigns.
const SEGMENT_WIDTH: f32 = 120.0;

pub struct Segment {
    pub label: String,
    /// Drawn instead of the label, which stays the tooltip and the
    /// accessible name.
    pub icon: Option<Icon>,
    /// A short hint shown after the label in the tooltip: the segment's
    /// keyboard shortcut.
    pub hint: Option<String>,
}

impl Segment {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            hint: None,
        }
    }

    /// Show `icon` instead of the label.
    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Name the segment's shortcut in its tooltip (`B`, `Shift+R`).
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The tooltip: the label, and the hint when there is one.
    fn tooltip(&self) -> String {
        match &self.hint {
            Some(hint) => format!("{}  {hint}", self.label),
            None => self.label.clone(),
        }
    }
}

/// Two-way storage of the chosen segment as the caller's own value.
struct Choice {
    /// The index of the value the state holds now, when it changed.
    pull: Box<dyn FnMut() -> Option<usize>>,
    /// Store the value of a chosen segment.
    push: Box<dyn FnMut(usize)>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent cached interaction/layout flags are orthogonal states, not one state machine"
)]
pub struct ToggleGroup {
    id: Option<String>,
    segments: Vec<Segment>,
    selected: usize,
    on_change: Option<Box<dyn FnMut(usize)>>,
    action: Option<ActionEmitter<usize>>,
    native_resource: Option<crate::tilo_actions::Resource>,
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
    /// The theme colours `cached` was painted in, so a theme change
    /// repaints it.
    drawn_palette: Option<[vello::peniko::Color; 4]>,
    /// Fill the parent's available width, allowing compact app navigation on
    /// phone-sized windows while retaining equal hit targets.
    flexible_width: bool,
    /// Visual treatment shared with the immediate-mode segmented painter.
    style: SegmentedStyle,
    /// Finger owning the current gesture. Selection commits on release, so a
    /// scroll or cancelled touch never changes tabs on initial contact.
    active_touch: Option<u64>,
    /// A strip or a stack.
    orientation: Orientation,
    /// The segment under the pointer.
    hovered: Option<usize>,
    /// Where the chosen segment's value lives, when bound.
    choice: Option<Choice>,
}

impl ToggleGroup {
    pub fn new(segments: Vec<Segment>, selected: usize) -> Self {
        let len = segments.len();
        // Saturate rather than panic on an out-of-bounds index — keeps
        // construction infallible. `len.saturating_sub(1)` is also `0` when
        // `len == 0`, so this collapses to the same no-segments case.
        let selected = selected.min(len.saturating_sub(1));
        Self {
            id: None,
            segments,
            selected,
            on_change: None,
            action: None,
            native_resource: None,
            segment_rects: Vec::new(),
            rect: Rect::ZERO,
            last_cursor: Cell::new(Point::ZERO),
            cached: Scene::new(),
            cache_dirty: true,
            cache_rect: Rect::ZERO,
            last_selected: usize::MAX,
            is_focused: false,
            last_focused: false,
            drawn_palette: None,
            flexible_width: false,
            style: SegmentedStyle::Solid,
            active_touch: None,
            orientation: Orientation::Horizontal,
            hovered: None,
            choice: None,
        }
    }

    /// A choice among `choices`, kept in `state`: choosing a segment stores
    /// its value, and a value set anywhere else (a keyboard shortcut, the
    /// app) selects its segment.
    pub fn from_state<T: Clone + PartialEq + 'static>(
        choices: Vec<(T, Segment)>,
        state: &crate::LocalState<T>,
    ) -> Self {
        let (values, segments): (Vec<T>, Vec<Segment>) = choices.into_iter().unzip();
        let values = std::rc::Rc::new(values);
        let (current, binding) = crate::local_state::LocalBinding::new(state);
        let binding = std::rc::Rc::new(std::cell::RefCell::new(binding));
        let selected = values.iter().position(|value| *value == current);
        let mut group = Self::new(segments, selected.unwrap_or_default());
        let (pull_values, pull_binding) =
            (std::rc::Rc::clone(&values), std::rc::Rc::clone(&binding));
        group.choice = Some(Choice {
            pull: Box::new(move || {
                let value = pull_binding.borrow_mut().poll()?;
                pull_values.iter().position(|candidate| *candidate == value)
            }),
            push: Box::new(move |index| {
                if let Some(value) = values.get(index) {
                    binding.borrow_mut().publish(value);
                }
            }),
        });
        group
    }

    /// Stack the segments vertically as square icon buttons: a tool rail.
    /// Only the chosen (and hovered) segment is filled.
    #[must_use]
    pub fn vertical(mut self) -> Self {
        self.orientation = Orientation::Vertical;
        self.style = SegmentedStyle::Pill;
        self.cache_dirty = true;
        self
    }

    /// Use the containing width instead of the 120px-per-segment intrinsic
    /// width. Labels remain equal-width and elide through the normal painter.
    #[must_use]
    pub fn with_flexible_width(mut self) -> Self {
        self.flexible_width = true;
        self
    }

    /// Use the same themed treatment as [`SegmentedFace`].
    #[must_use]
    pub fn with_style(mut self, style: SegmentedStyle) -> Self {
        self.style = style;
        self.cache_dirty = true;
        self
    }

    /// A low-chrome tab treatment with a short accent indicator.
    #[must_use]
    pub fn underline(mut self) -> Self {
        self.style = SegmentedStyle::Underline;
        self.cache_dirty = true;
        self
    }

    #[must_use]
    pub fn on_change<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// Route every pointer, keyboard, and accessibility selection through a
    /// caller-owned canonical action. The selected index is still painted
    /// optimistically only after the document/model reflects that action, so
    /// collaborative filters do not acquire a second callback-only path.
    #[must_use]
    pub fn on_action(mut self, binding: crate::tilo_actions::ActionBinding<usize>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.native_resource = None;
        self.on_change = None;
        self
    }

    /// Give this compound control a stable identity for headless driving,
    /// accessibility actions, and resource/action publication.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Publish segment selection as a transient view action while preserving
    /// the familiar closure API. UI, accessibility, search, and agents all
    /// enter through the same typed `index` operation.
    ///
    /// # Panics
    ///
    /// Panics only if the framework-generated integer action contract cannot
    /// encode its own generated payload.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the generated integer schema and action are internally consistent"
    )]
    pub fn on_view<F: FnMut(usize) + 'static>(mut self, id: &str, f: F) -> Self {
        use crate::tilo_actions::{
            ActionBinding, ActionParam, ActionSpec, DiscoveryScope, EncodedAction, OperationKind,
            ParamType, Resource,
        };

        const TYPE_ID: &str = "tilo.ui.segmented.select";
        self.id = Some(id.to_owned());
        let control_id = id.to_owned();
        let mut spec = ActionSpec::typed_operation(
            "select",
            "Select",
            "Choose a segment",
            OperationKind::View,
            TYPE_ID,
            vec![ActionParam::required(
                "index",
                ParamType::Integer,
                "Zero-based segment index",
            )],
            move |args| {
                EncodedAction::encode_operation(
                    TYPE_ID,
                    &serde_json::json!({
                        "control": control_id,
                        "index": args.integer("index").unwrap_or_default(),
                    }),
                )
                .expect("generated segmented action payload is serializable")
            },
        );
        spec.contract.discovery = DiscoveryScope::Launchable;
        let resource = Resource::new(format!("tilo.control:{id}"), "tilo.control.segmented", id)
            .with_action(spec.clone());
        let binding = ActionBinding::new(&resource, &spec, |index: usize| {
            serde_json::Map::from_iter([("index".into(), serde_json::json!(index))])
        })
        .expect("segment index matches the generated integer parameter");
        self.action = Some(ActionEmitter::new(binding));
        self.native_resource = Some(resource);
        self.on_change = Some(Box::new(f));
        self
    }

    fn request_selection(&mut self, index: usize) {
        if self.selected == index {
            return;
        }
        if let Some(action) = &mut self.action {
            if let Err(error) = action.emit(index) {
                tracing::warn!(%error, "segmented action refused its index");
            }
            return;
        }
        self.apply_selection(index);
    }

    fn apply_selection(&mut self, index: usize) {
        self.selected = index;
        self.cache_dirty = true;
        if let Some(choice) = self.choice.as_mut() {
            (choice.push)(index);
        }
        if let Some(callback) = self.on_change.as_mut() {
            callback(index);
        }
    }

    /// The side of a stacked segment and the gap between segments.
    fn stacked_metrics(theme: &Theme) -> (f32, f32) {
        let metrics = &theme.tokens.metrics;
        (
            metrics.min_pointer_target.max(metrics.panel_row_height),
            theme.tokens.spacing.s1,
        )
    }

    fn segment_prefix(&self) -> String {
        self.id
            .as_ref()
            .map_or_else(|| "segment:".to_owned(), |id| format!("{id}:segment:"))
    }

    fn segment_id(&self, index: usize) -> String {
        format!("{}{index}", self.segment_prefix())
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
        if self.orientation == Orientation::Vertical {
            // Square segments down the rail, with the gap the layout left
            // between them.
            let side = self.rect.width();
            let gaps = usize_to_f64(n.saturating_sub(1)).max(1.0);
            let gap = ((self.rect.height() - side * usize_to_f64(n)) / gaps).max(0.0);
            let mut y = self.rect.y0;
            for _ in 0..n {
                self.segment_rects
                    .push(Rect::new(self.rect.x0, y, self.rect.x1, y + side));
                y += side + gap;
            }
        } else {
            self.partition_horizontally(n);
        }
        #[allow(
            clippy::float_cmp,
            reason = "exact equality is intentional: both rects come from the same taffy \
                      layout pass, so a bit-identical width/height means the cached scene is \
                      still valid, not merely close enough"
        )]
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.cache_dirty = true;
        }
    }

    fn partition_horizontally(&mut self, n: usize) {
        let total_w = self.rect.width();
        let seg_w = total_w / usize_to_f64(n);
        let last = n.saturating_sub(1);
        for i in 0..n {
            let sx0 = self.rect.x0 + usize_to_f64(i) * seg_w;
            let sx1 = if i == last {
                // Anchor to `rect.x1` so accumulated rounding doesn't leave
                // a hairline gap on the right edge.
                self.rect.x1
            } else {
                self.rect.x0 + usize_to_f64(i.saturating_add(1)) * seg_w
            };
            self.segment_rects
                .push(Rect::new(sx0, self.rect.y0, sx1, self.rect.y1));
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
                (Some(colors.surfaces.raised), colors.text_primary)
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
    /// Glyphs drawn instead of the labels of the same index.
    icons: &'a [Option<Icon>],
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
            icons: &[],
            selected,
            hovered: None,
            style: SegmentedStyle::Solid,
            radius: 8.0,
            text_size: 13.0,
            weight: crate::text_render::TextWeight::Medium,
            label_pad_left: None,
        }
    }
    #[must_use]
    pub fn style(mut self, style: SegmentedStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn pill(mut self) -> Self {
        self.style = SegmentedStyle::Pill;
        self
    }
    #[must_use]
    pub fn hovered(mut self, idx: Option<usize>) -> Self {
        self.hovered = idx;
        self
    }
    #[must_use]
    pub fn radius(mut self, r: f64) -> Self {
        self.radius = r;
        self
    }
    #[must_use]
    pub fn text_size(mut self, s: f32) -> Self {
        self.text_size = s;
        self
    }
    #[must_use]
    pub fn weight(mut self, w: crate::text_render::TextWeight) -> Self {
        self.weight = w;
        self
    }
    #[must_use]
    pub fn left_labels(mut self, pad: f64) -> Self {
        self.label_pad_left = Some(pad);
        self
    }
    /// Draw these glyphs instead of the labels of the same index.
    #[must_use]
    pub fn icons(mut self, icons: &'a [Option<Icon>]) -> Self {
        self.icons = icons;
        self
    }
}

impl ToggleGroup {
    /// Paint the background/border layer for [`Self::paint`]: the filled
    /// segments plus whatever chrome the style calls for (outer border and
    /// separators for `Solid`, nothing extra for `Pill`, an underline bar
    /// for `Underline`). Split out of `paint` to keep it under the
    /// line-count lint; `segs` is guaranteed non-empty by the caller.
    fn paint_style_layer(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        face: &SegmentedFace<'_>,
        segs: &[Rect],
    ) {
        let colors = &theme.tokens.colors;
        match face.style {
            SegmentedStyle::Solid => {
                let (Some(first), Some(last)) = (segs.first(), segs.last()) else {
                    return;
                };
                let outer = Rect::new(first.x0, first.y0, last.x1, first.y1);
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
                // Separators and outer edge derive from the track (HIG
                // §9.5); separators stop `s1` short of the edge so they
                // never run into the corner radius.
                let track_edge = crate::surface::edge_on(colors.surfaces.raised);
                let inset = f64::from(theme.tokens.spacing.s1);
                let sep = Stroke::new(1.0);
                for seg in segs.iter().take(segs.len().saturating_sub(1)) {
                    let x = seg.x1;
                    scene.stroke(
                        &sep,
                        transform,
                        track_edge,
                        None,
                        &Line::new(
                            Point::new(x, outer.y0 + inset),
                            Point::new(x, outer.y1 - inset),
                        ),
                    );
                }
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    track_edge,
                    None,
                    &crate::surface::edge_outline(outer, face.radius),
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
    }

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

        Self::paint_style_layer(scene, transform, theme, face, segs);

        // Selected pills use a slightly heavier weight so the active choice
        // reads first.
        for (i, seg) in segs.iter().enumerate() {
            let (_, fg) = segment_colors(
                theme,
                face.style,
                i == face.selected,
                face.hovered == Some(i),
            );
            if let Some(icon) = face.icons.get(i).copied().flatten() {
                let side = (seg.width().min(seg.height()) * 0.5).clamp(14.0, 22.0);
                let glyph = Rect::from_center_size(seg.center(), (side, side));
                icon.draw(scene, transform, glyph, fg);
                continue;
            }
            let label = face.labels.get(i).copied().unwrap_or("");
            if label.is_empty() {
                continue;
            }
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
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix multiplication; it cannot \
                          overflow or panic"
            )]
            let label_transform = transform * Affine::translate((text_x, text_y));
            text_renderer.draw_text_weighted(
                scene,
                label,
                face.text_size,
                fg,
                label_transform,
                None,
                weight,
            );
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent, handled: &mut bool) {
        if !self.is_focused
            || !self.is_enabled()
            || key.state != ElementState::Pressed
            || key.repeat
        {
            return;
        }
        let len = self.segments.len();
        let selection = match key.logical_key {
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) if len > 0 => Some(
                self.selected
                    .checked_sub(1)
                    .unwrap_or_else(|| len.saturating_sub(1)),
            ),
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) if len > 0 => {
                Some(if self.selected == len.saturating_sub(1) {
                    0
                } else {
                    self.selected.saturating_add(1)
                })
            }
            Key::Named(NamedKey::Home) if len > 0 => Some(0),
            Key::Named(NamedKey::End) if len > 0 => Some(len.saturating_sub(1)),
            // A segmented control is single-select: activation confirms its
            // current choice rather than toggling it off.
            Key::Named(NamedKey::Enter | NamedKey::Space) => Some(self.selected),
            _ => None,
        };
        if let Some(selection) = selection {
            *handled = true;
            self.request_selection(selection);
        }
    }

    fn handle_pointer_press(
        &mut self,
        state: ElementState,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        if state != ElementState::Pressed || !self.is_enabled() {
            return;
        }
        let pos = self.last_cursor.get();
        if let Some(index) = self
            .segment_rects
            .iter()
            .position(|segment| segment.contains(pos))
        {
            *handled = true;
            *focus_target = Some(node_id);
            self.request_selection(index);
        }
    }

    fn handle_touch_event(
        &mut self,
        touch: &winit::event::Touch,
        scale_factor: f64,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        if !self.is_enabled() {
            return;
        }
        let pos = Point::new(
            touch.location.x / scale_factor,
            touch.location.y / scale_factor,
        );
        match touch.phase {
            winit::event::TouchPhase::Started
                if self.active_touch.is_none() && self.rect.contains(pos) =>
            {
                self.active_touch = Some(touch.id);
                *focus_target = Some(node_id);
                *handled = true;
            }
            winit::event::TouchPhase::Moved if self.active_touch == Some(touch.id) => {
                *handled = true;
            }
            winit::event::TouchPhase::Ended if self.active_touch == Some(touch.id) => {
                self.active_touch = None;
                if let Some(index) = self
                    .segment_rects
                    .iter()
                    .position(|segment| segment.contains(pos))
                {
                    self.request_selection(index);
                }
                *handled = true;
            }
            winit::event::TouchPhase::Cancelled if self.active_touch == Some(touch.id) => {
                self.active_touch = None;
                *handled = true;
            }
            _ => {}
        }
    }
}

impl ToggleGroup {
    /// Apply a `tilo.ui.segmented.select` addressed to this control.
    /// `Ok(false)` when the operation is not this control's.
    fn select_segment_operation(
        &mut self,
        kind: crate::tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<bool, String> {
        if kind != crate::tilo_actions::OperationKind::View
            || type_id != "tilo.ui.segmented.select"
            || self.action.is_none()
        {
            return Ok(false);
        }
        let value: serde_json::Value =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        if value.get("control").and_then(serde_json::Value::as_str) != self.id.as_deref() {
            return Ok(false);
        }
        let index = value
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|index| *index < self.segments.len())
            .ok_or_else(|| "segmented action index is out of range".to_owned())?;
        self.apply_selection(index);
        Ok(true)
    }
}

impl Widget for ToggleGroup {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.orientation == Orientation::Vertical {
            let (side, gap) = Self::stacked_metrics(theme);
            let n = usize_to_f32(self.segments.len().max(1));
            let node = taffy.leaf_node(Style {
                size: Size {
                    width: Dimension::length(side),
                    height: Dimension::length(side * n + gap * (n - 1.0)),
                },
                flex_shrink: 0.0,
                ..Default::default()
            });
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        }
        // Segments always share the available width equally in `apply_layout`.
        let width = usize_to_f32(self.segments.len().max(1)) * SEGMENT_WIDTH;
        let node = taffy.leaf_node(Style {
            size: Size {
                width: if self.flexible_width {
                    Dimension::percent(1.0)
                } else {
                    Dimension::length(width)
                },
                height: Dimension::length(
                    theme
                        .tokens
                        .metrics
                        .panel_row_height
                        .max(theme.tokens.metrics.min_pointer_target),
                ),
            },
            // A segmented strip has an intrinsic label-bearing width. In a
            // horizontal `Scrollable`, shrinking it to the viewport defeats
            // scrolling and makes labels collide. Non-scrollable parents can
            // clip it, which prevents overlapping controls.
            // Width is already resolved as a percentage in flexible mode;
            // never let a tall sibling squash the 44px touch target.
            flex_shrink: 0.0,
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
        let palette = [
            theme.accent_primary,
            theme.bg_hover,
            theme.tokens.colors.text_primary,
            theme.tokens.colors.surfaces.base,
        ];
        if self.drawn_palette != Some(palette) {
            self.drawn_palette = Some(palette);
            self.cache_dirty = true;
        }
        let needs_emit = self.cache_dirty || self.cache_rect == Rect::ZERO;
        if !needs_emit {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix multiplication; it cannot \
                          overflow or panic"
            )]
            let cached_transform = out_transform * Affine::translate((dx, dy));
            out_scene.append(&self.cached, Some(cached_transform));
            return;
        }
        self.cached.reset();
        let scene = &mut self.cached;
        let transform = Affine::IDENTITY;

        let radius = f64::from(theme.tokens.radii.md);
        let outer_shape = RoundedRect::from_rect(self.rect, radius);
        let labels: Vec<&str> = self.segments.iter().map(|s| s.label.as_str()).collect();
        let icons: Vec<Option<Icon>> = self.segments.iter().map(|s| s.icon).collect();
        let face = SegmentedFace::new(&self.segment_rects, &labels, self.selected)
            .icons(&icons)
            .hovered(self.hovered)
            .style(self.style)
            .text_size(crate::text_metrics::renderer_type_size(
                text_renderer,
                theme.tokens.typography.scale.sm,
            ))
            .radius(radius);
        Self::paint(scene, transform, text_renderer, theme, &face);

        // Keyboard focus must remain unmistakable on every style. For Solid,
        // this focus stroke intentionally covers the resting standard border.
        if self.is_focused {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.border_focus,
                None,
                &outer_shape,
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::KeyboardInput { event, .. } => self.handle_key_event(event, ctx.handled),
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor.set(logical_pos);
                let hovered = self
                    .segment_rects
                    .iter()
                    .position(|segment| segment.contains(logical_pos));
                if hovered != self.hovered {
                    self.hovered = hovered;
                    self.cache_dirty = true;
                }
            }
            WindowEvent::CursorLeft { .. } if self.hovered.is_some() => {
                self.hovered = None;
                self.cache_dirty = true;
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => self.handle_pointer_press(
                *element_state,
                ctx.handled,
                ctx.focus_target,
                ctx.node_id,
            ),
            WindowEvent::Touch(touch) => {
                self.handle_touch_event(
                    touch,
                    ctx.scale_factor,
                    ctx.handled,
                    ctx.focus_target,
                    ctx.node_id,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ToggleGroup"
    }

    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        if let Some(index) = self.choice.as_mut().and_then(|choice| (choice.pull)()) {
            self.set_selected(index);
        }
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        // Icon segments show no label, so their label is the tooltip.
        let index = self
            .segment_rects
            .iter()
            .position(|segment| segment.contains(point))?;
        let segment = self.segments.get(index)?;
        let rect = *self.segment_rects.get(index)?;
        (segment.icon.is_some() || segment.hint.is_some()).then(|| (rect, segment.tooltip()))
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

    fn is_enabled(&self) -> bool {
        self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        self.native_resource
            .as_ref()
            .map(|_| std::iter::once("tilo.ui.segmented.select".to_owned()).collect())
            .unwrap_or_default()
    }

    fn start_operation(
        &mut self,
        op: &super::ResourceOperation<'_>,
        _doc: &mut AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<super::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .select_segment_operation(request.kind, request.type_id, request.payload)?
            .then(|| {
                super::ResourceOperationStart::Completed(
                    crate::tilo_actions::ActionOutput::default(),
                )
            }))
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
                let node = AccessNode::new(Role::ToggleButton)
                    .with_action_target(self.segment_id(i))
                    .with_name(s.label.clone())
                    .with_state(AccessState {
                        checked: Some(i == self.selected),
                        ..Default::default()
                    })
                    .with_position(i.saturating_add(1), count);
                // Where the segment is drawn, so pointing at it lands on it.
                match self.segment_rects.get(i) {
                    Some(rect) => node.with_bounds(*rect),
                    None => node,
                }
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

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        let prefix = self.segment_prefix();
        let Some(index) = target
            .strip_prefix(&prefix)
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < self.segments.len())
        else {
            return false;
        };
        if !matches!(
            action,
            crate::a11y::Action::Activate | crate::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        self.request_selection(index);
        true
    }
}

impl crate::resources::PublishesResources for ToggleGroup {
    fn resources(&self, _doc: &AutoCommit) -> Vec<crate::tilo_actions::Resource> {
        self.native_resource.iter().cloned().collect()
    }
}

impl crate::widget::controls::ActionControls for ToggleGroup {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(action) = &mut self.action {
            let _ = action.install_custody(domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<crate::tilo_actions::ControlInvocation> {
        self.action.as_mut().and_then(ActionEmitter::take)
    }
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let Some(action) = &self.action else {
            // A choice kept in view state is made through the segments'
            // own semantic actions.
            if self.choice.is_none() {
                return Vec::new();
            }
            return (0..self.segments.len())
                .map(|index| ControlBindingMetadata {
                    control_id: self.segment_id(index),
                    kind: crate::widget::ControlBindingKind::SemanticAction(self.segment_id(index)),
                })
                .collect();
        };
        (0..self.segments.len())
            .map(|index| action.metadata(self.segment_id(index)))
            .collect()
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<crate::tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            action.refresh_availability(allowed);
        }
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

    /// Fixed rect so tests can simulate clicks without a full taffy pass.
    fn group_at(segments: Vec<Segment>, selected: usize, rect: Rect) -> ToggleGroup {
        let mut g = ToggleGroup::new(segments, selected);
        g.rect = rect;
        // Populate segment_rects manually (mirrors apply_layout).
        let n = g.segments.len();
        if n > 0 {
            let w = rect.width() / usize_to_f64(n);
            let last = n.saturating_sub(1);
            g.segment_rects = (0..n)
                .map(|i| {
                    let x0 = rect.x0 + usize_to_f64(i) * w;
                    let x1 = if i == last {
                        rect.x1
                    } else {
                        rect.x0 + usize_to_f64(i.saturating_add(1)) * w
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

    fn key_press(key: NamedKey) -> WindowEvent {
        // SAFETY: the non-public `KeyEventExtra` field accepts its all-zero
        // representation; every public field used by routing is set below.
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.state = ElementState::Pressed;
        event.logical_key = Key::Named(key);
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        }
    }

    fn touch(phase: winit::event::TouchPhase, point: Point) -> WindowEvent {
        WindowEvent::Touch(winit::event::Touch {
            device_id: unsafe { std::mem::zeroed() },
            phase,
            location: winit::dpi::PhysicalPosition::new(point.x, point.y),
            force: None,
            id: 7,
        })
    }
    fn dispatch(g: &mut ToggleGroup, event: &WindowEvent) {
        let _ = dispatch_handled(g, event);
    }

    fn dispatch_handled(g: &mut ToggleGroup, event: &WindowEvent) -> bool {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        g.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
        handled
    }

    #[test]
    fn toggle_group_geometry_scales_labels_once_and_is_touch_sized() {
        // Case `retained_labels_apply_resolved_text_scale_exactly_once`.
        {
            for scale in [1.0, 2.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut text = crate::font_catalog::fixture_renderer();
                text.set_text_scale(scale);
                let mut group = group_at(
                    vec![Segment::new("Params"), Segment::new("Headers")],
                    0,
                    Rect::new(0.0, 0.0, 384.0, 52.0),
                );
                let mut scene = Scene::new();
                group.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &PresenceCtx::default(),
                );
                let runs = &scene.encoding().resources.glyph_runs;
                assert!(!runs.is_empty());
                for run in runs {
                    assert!(
                        (run.font_size - theme.tokens.typography.scale.sm).abs() < 0.001,
                        "{scale}x text must paint once at resolved size {}, got {}",
                        theme.tokens.typography.scale.sm,
                        run.font_size
                    );
                }
            }
        }

        // Case `retained_group_has_touch_sized_intrinsic_height`.
        {
            let mut group =
                ToggleGroup::new(vec![Segment::new("World"), Segment::new("Timers")], 0);
            let mut tree = TaffyTree::new();
            let mut map = HashMap::new();
            let node = group.build_layout(&mut tree, &Theme::dark(), &mut map);
            tree.compute_layout(node, taffy::Size::MAX_CONTENT).unwrap();
            group.apply_layout(&tree, node, Point::ZERO);
            assert!(group.get_rect().height() >= 44.0);
        }

        // Case `toggle_group_paints_separator_lines_between_segments`.
        {
            let mut g = ToggleGroup::new(
                vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
                0,
            );
            // Run a layout pass so `segment_rects` is populated.
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

        // Case `segmented_paint_emits_fills_over_caller_rects`.
        {
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
    }

    #[test]
    fn toggle_group_selection_follows_pointer_touch_and_keyboard() {
        // Case `toggle_group_initial_selection_zero`.
        {
            let g = ToggleGroup::new(vec![Segment::new("a"), Segment::new("b")], 0);
            assert_eq!(g.selected(), 0);
        }

        // Case `toggle_group_click_segment_two_selects_index_two`.
        {
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

        // Case `focused_toggle_group_supports_keyboard_navigation_and_activation`.
        {
            let changes = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&changes);
            let mut group = ToggleGroup::new(
                vec![
                    Segment::new("One"),
                    Segment::new("Two"),
                    Segment::new("Three"),
                ],
                0,
            )
            .on_change(move |index| captured.borrow_mut().push(index));
            group.set_focus(true);

            assert!(dispatch_handled(
                &mut group,
                &key_press(NamedKey::ArrowRight)
            ));
            assert_eq!(group.selected(), 1);
            assert!(dispatch_handled(&mut group, &key_press(NamedKey::End)));
            assert_eq!(group.selected(), 2);
            assert!(dispatch_handled(
                &mut group,
                &key_press(NamedKey::ArrowRight)
            ));
            assert_eq!(group.selected(), 0, "arrow navigation wraps");
            assert!(dispatch_handled(&mut group, &key_press(NamedKey::Enter)));
            assert_eq!(&*changes.borrow(), &[1, 2, 0]);

            group.set_focus(false);
            assert!(!dispatch_handled(
                &mut group,
                &key_press(NamedKey::ArrowRight)
            ));
            assert_eq!(
                group.selected(),
                0,
                "unfocused controls do not consume keys"
            );
        }

        // Case `touch_commits_on_release_and_keyboard_arrows_wrap`.
        {
            let mut group = group_at(
                vec![
                    Segment::new("World"),
                    Segment::new("Alarms"),
                    Segment::new("Timers"),
                ],
                0,
                Rect::new(0.0, 0.0, 300.0, 44.0),
            );
            dispatch(
                &mut group,
                &touch(winit::event::TouchPhase::Started, Point::new(250.0, 22.0)),
            );
            assert_eq!(group.selected(), 0, "touch down does not switch a tab");
            dispatch(
                &mut group,
                &touch(winit::event::TouchPhase::Ended, Point::new(250.0, 22.0)),
            );
            assert_eq!(group.selected(), 2);

            group.set_focus(true);
            dispatch(&mut group, &key_press(NamedKey::ArrowRight));
            assert_eq!(group.selected(), 0, "right arrow wraps to the first tab");
            dispatch(&mut group, &key_press(NamedKey::End));
            assert_eq!(group.selected(), 2);
        }

        // Case `unavailable_toggle_group_rejects_pointer_touch_and_keyboard_input`.
        {
            let mut group = group_at(
                vec![Segment::new("Light"), Segment::new("Dark")],
                0,
                Rect::new(0.0, 0.0, 200.0, 44.0),
            )
            .on_view("appearance", |_| {});
            crate::widget::controls::refresh_control_action_availability(
                &mut group,
                &std::collections::HashSet::new(),
            );
            group.set_focus(true);
            assert!(!Widget::is_enabled(&group));

            dispatch(&mut group, &cursor_moved(150.0, 22.0));
            assert!(!dispatch_handled(&mut group, &mouse_press()));
            assert!(!dispatch_handled(
                &mut group,
                &touch(winit::event::TouchPhase::Started, Point::new(150.0, 22.0)),
            ));
            assert!(!dispatch_handled(
                &mut group,
                &key_press(NamedKey::ArrowRight),
            ));
            assert_eq!(group.selected(), 0);
        }
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

    #[test]
    fn segment_colours_follow_selection_focus_and_style() {
        // Case `focused_toggle_group_uses_the_focus_colour`.
        // Uses high contrast (yellow focus vs white border) so the two colours
        // are unambiguously different.
        {
            let theme = Theme::high_contrast();
            let unfocused = group_draw_data(false, &theme);
            let focused = group_draw_data(true, &theme);
            assert_ne!(
                unfocused, focused,
                "focusing the group must change the border colour in the encoded scene",
            );
        }

        // Case `segment_colors_solid_selected_is_accent_and_readable`.
        {
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

        // Case `segment_colors_pill_unselected_is_transparent`.
        {
            use crate::a11y::contrast_ratio;
            let theme = Theme::dark();
            let (fill, fg) = segment_colors(&theme, SegmentedStyle::Pill, false, false);
            assert_eq!(fill, None, "an idle unselected pill paints no background");
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
    }

    #[test]
    fn accessibility_and_actions_expose_and_drive_the_selected_segment() {
        // Case `accessibility_reports_group_role_and_selected_segment_label`.
        {
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

        // Case `accessibility_value_tracks_selection_changes`.
        {
            // Nothing reads `captured` back in this test (only the accessibility
            // value is asserted), so the closure can just own it — no need for
            // the clone-and-keep-a-handle dance the other click test uses.
            let captured: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
            let mut g = group_at(
                vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
                0,
                Rect::new(0.0, 0.0, 300.0, 36.0),
            )
            .on_change(move |i| *captured.borrow_mut() = Some(i));

            assert_eq!(
                Widget::accessibility(&g).unwrap().value.as_deref(),
                Some("a")
            );
            let segments = Widget::accessibility(&g).unwrap().children;
            assert_eq!(
                segments.get(2).and_then(|segment| segment.bounds),
                g.segment_rects.get(2).map(|rect| (*rect).into()),
                "each segment is placed where it is drawn"
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

        // Case `synthetic_segment_activation_selects_and_fires_on_change`.
        {
            let changed = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&changed);
            let mut group = ToggleGroup::new(vec![Segment::new("Light"), Segment::new("Dark")], 0)
                .on_change(move |index| captured.borrow_mut().push(index));

            assert!(Widget::perform_semantic_action(
                &mut group,
                "segment:1",
                &crate::a11y::Action::Activate,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(group.selected(), 1);
            assert_eq!(&*changed.borrow(), &[1]);
        }

        // Case `on_view_publishes_and_executes_one_typed_resource_action`.
        {
            let changed = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&changed);
            let mut group = ToggleGroup::new(vec![Segment::new("Light"), Segment::new("Dark")], 0)
                .on_view("appearance", move |index| captured.borrow_mut().push(index));

            let resources = crate::resources::resources(&group, &AutoCommit::new());
            assert_eq!(resources.len(), 1);
            let resource = &resources[0];
            assert_eq!(resource.id.as_str(), "tilo.control:appearance");
            assert_eq!(resource.actions.len(), 1);
            let spec = &resource.actions[0];
            assert_eq!(spec.kind, crate::tilo_actions::OperationKind::View);
            assert_eq!(
                spec.contract.discovery,
                crate::tilo_actions::DiscoveryScope::Launchable
            );
            assert_eq!(spec.params()[0].name, "index");

            assert!(Widget::perform_semantic_action(
                &mut group,
                "appearance:segment:1",
                &crate::a11y::Action::Activate,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(
                group.selected(),
                0,
                "UI intent does not bypass the action host"
            );
            assert!(changed.borrow().is_empty());

            let invocation = crate::widget::controls::take_control_invocation(&mut group).unwrap();
            assert_eq!(
                invocation.address,
                crate::tilo_actions::ActionAddress::new("tilo.control:appearance", "select")
            );
            assert_eq!(invocation.arguments["index"], 1);
            let encoded = spec.build_json(&invocation.arguments).unwrap();
            assert_eq!(encoded.type_id, "tilo.ui.segmented.select");
            let output = Widget::start_operation(
                &mut group,
                &crate::widget::ResourceOperation::local(
                    crate::tilo_actions::InvocationId::new(),
                    crate::tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut AutoCommit::new(),
                &automerge::ROOT,
            )
            .map(|started| started.and_then(crate::widget::ResourceOperationStart::completed))
            .unwrap();
            assert!(output.is_some());
            assert_eq!(group.selected(), 1);
            assert_eq!(&*changed.borrow(), &[1]);
        }
    }
}
