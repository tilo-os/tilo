//! `ToggleGroup` — the segmented control. A strip of equal-width segments
//! of which exactly one is chosen. Clicking a segment chooses it and fires
//! the `on_change` callback with the new index.
//!
//! State lives entirely on the widget (no CRDT round-trip) because the
//! segmented control's selection is ephemeral UI sugar — callers persist
//! the choice via the `on_change` callback into their own CRDT slot.
//!
//! The chosen segment is a *lift* (HIG §9.3): a thumb one surface step off
//! the track, set concentrically inside it, with primary ink and no hue.
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

use crate::chrome::{SelectionState, selection_tier_on};
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::colors::ON_FILL_MIN_CONTRAST;
use crate::widget::{ActionEmitter, ControlBindingMetadata, ControlSize, TaffyTreeExt, Widget};

/// Lossless `usize -> f64` widening for segment-strip geometry. Segment
/// counts are bounded far below `u32::MAX`, so routing through `u32` (which
/// `f64` represents exactly) avoids the precision loss a direct cast risks.
fn usize_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "logical pixel counts far below f32 precision"
)]
fn px_f32(value: f64) -> f32 {
    value as f32
}

/// One choice of a [`ToggleGroup`].
pub struct Segment {
    pub label: String,
    /// What the segment shows when its full label does not fit its share
    /// of the strip ("Mon" for "Monday").
    pub short: Option<String>,
}

impl Segment {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            short: None,
        }
    }

    /// The label shown when the full one does not fit.
    #[must_use]
    pub fn short(mut self, label: impl Into<String>) -> Self {
        self.short = Some(label.into());
        self
    }
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
    /// The segment under the pointer, for its hover lift.
    hovered: Option<usize>,
    cached: Scene,
    cache_dirty: bool,
    cache_rect: Rect,
    last_selected: usize,
    /// Drives the focus ring drawn round the track.
    is_focused: bool,
    /// `is_focused` at the last cache emit, so a focus change invalidates
    /// the cached scene.
    last_focused: bool,
    /// `hovered` at the last cache emit.
    last_hovered: Option<usize>,
    /// The theme colours `cached` was painted in, so a theme change
    /// repaints it.
    drawn_palette: Option<[Color; 4]>,
    /// Stretch to the parent's width instead of fitting the labels.
    fill: bool,
    /// What the strip is (see [`SegmentedStyle`]).
    style: SegmentedStyle,
    /// Height and label role.
    control_size: ControlSize,
    /// The geometry seen in `build_layout`, for `measure`.
    metrics: StripMetrics,
    /// Each segment's natural width with its full label and with its short
    /// form, measured with the text renderer; the strip is partitioned from
    /// them.
    naturals: Vec<(f64, f64)>,
    /// Per segment, whether the partition had to fall back to its short
    /// label.
    use_short: Vec<bool>,
    /// The width `measure` settled on, pinned onto the node so a stretching
    /// parent cannot widen a fitted strip.
    measured_width: Option<f32>,
    /// Finger owning the current gesture. Selection commits on release, so a
    /// scroll or cancelled touch never changes tabs on initial contact.
    active_touch: Option<u64>,
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
            hovered: None,
            cached: Scene::new(),
            cache_dirty: true,
            cache_rect: Rect::ZERO,
            last_selected: usize::MAX,
            is_focused: false,
            last_focused: false,
            last_hovered: None,
            drawn_palette: None,
            fill: false,
            style: SegmentedStyle::Segmented,
            control_size: ControlSize::Regular,
            metrics: StripMetrics::stock(ControlSize::Regular),
            naturals: Vec::new(),
            use_short: Vec::new(),
            measured_width: None,
            active_touch: None,
        }
    }

    /// Stretch to the parent's width, segments sharing it equally. Without
    /// this the strip is as wide as its labels need.
    #[must_use]
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }

    /// What the strip is: a segmented control (the default), page tabs, or
    /// a quiet refinement under a primary choice.
    #[must_use]
    pub fn with_style(mut self, style: SegmentedStyle) -> Self {
        self.style = style;
        self.cache_dirty = true;
        self
    }

    /// Height and label role (see [`ControlSize`]).
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
        self.metrics = StripMetrics::stock(size);
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
        if let Some(callback) = self.on_change.as_mut() {
            callback(index);
        }
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
        let (rects, use_short) = if self.naturals.len() == self.segments.len() {
            partition_sized(rect, &self.naturals)
        } else {
            (
                partition(rect, self.segments.len()),
                vec![false; self.segments.len()],
            )
        };
        if rects != self.segment_rects || use_short != self.use_short {
            self.cache_dirty = true;
        }
        self.segment_rects = rects;
        self.use_short = use_short;
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

    /// The number of separators the strip draws at rest: one between each
    /// pair of neighbouring segments that are both unchosen.
    pub fn separator_count(&self) -> usize {
        separators(self.segments.len(), self.selected, self.style).count()
    }
}

/// `rect` split into `n` equal segments, the last one anchored to the right
/// edge so accumulated rounding leaves no hairline gap.
fn partition(rect: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let seg_w = rect.width() / usize_to_f64(n);
    let last = n.saturating_sub(1);
    (0..n)
        .map(|i| {
            let x0 = rect.x0 + usize_to_f64(i) * seg_w;
            let x1 = if i == last {
                rect.x1
            } else {
                rect.x0 + usize_to_f64(i.saturating_add(1)) * seg_w
            };
            Rect::new(x0, rect.y0, x1, rect.y1)
        })
        .collect()
}

/// `rect` split among segments of natural widths `naturals` (full label,
/// short label): equal segments while every full label fits an equal share;
/// otherwise each segment as wide as its label needs with the room left
/// shared equally; only when the full labels cannot all fit, the short forms
/// where a segment has one; and only past that, shares in proportion (the
/// labels then elide). Returns the rects and, per segment, whether it shows
/// its short form.
fn partition_sized(rect: Rect, naturals: &[(f64, f64)]) -> (Vec<Rect>, Vec<bool>) {
    let n = naturals.len();
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let avail = rect.width().max(0.0);
    let count = usize_to_f64(n);
    let full: Vec<f64> = naturals.iter().map(|(full, _)| *full).collect();
    let widest = full.iter().copied().fold(0.0, f64::max);
    if widest * count <= avail + 1e-6 {
        return (partition(rect, n), vec![false; n]);
    }
    let fitted = |widths: &[f64]| -> Option<Vec<f64>> {
        let sum: f64 = widths.iter().sum();
        (sum <= avail + 1e-6).then(|| {
            let extra = (avail - sum) / count;
            widths.iter().map(|w| w + extra).collect()
        })
    };
    let (widths, use_short) = if let Some(widths) = fitted(&full) {
        (widths, vec![false; n])
    } else {
        let short: Vec<f64> = naturals
            .iter()
            .map(|(full, short)| full.min(*short))
            .collect();
        let use_short: Vec<bool> = naturals.iter().map(|(full, short)| short < full).collect();
        if let Some(widths) = fitted(&short) {
            (widths, use_short)
        } else {
            let sum: f64 = short.iter().sum::<f64>().max(1.0);
            (short.iter().map(|w| w * avail / sum).collect(), use_short)
        }
    };
    let last = n.saturating_sub(1);
    let mut x = rect.x0;
    let rects = widths
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let x1 = if i == last { rect.x1 } else { x + w };
            let seg = Rect::new(x, rect.y0, x1, rect.y1);
            x = x1;
            seg
        })
        .collect();
    (rects, use_short)
}

/// The indices `i` whose right edge carries a separator: between two
/// unchosen neighbours on a segmented track, nowhere else.
fn separators(n: usize, selected: usize, style: SegmentedStyle) -> impl Iterator<Item = usize> {
    (0..n.saturating_sub(1)).filter(move |i| {
        style == SegmentedStyle::Segmented && *i != selected && i.saturating_add(1) != selected
    })
}

/// What a segmented strip is. The style never chooses a colour: the chosen
/// segment is always a lift with primary ink.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SegmentedStyle {
    /// A segmented control: a raised track with an edge, the chosen
    /// segment a thumb lifted off it, concentric inside it.
    #[default]
    Segmented,
    /// Page tabs: no track; the chosen tab is primary ink over a 2 px
    /// primary-ink underline.
    Tabs,
    /// A quiet refinement under a primary choice (a sort order under a
    /// view switcher): no track; the chosen segment is a lift of the ground
    /// it stands on.
    Quiet,
}

/// The geometry of a strip of one [`ControlSize`] in one theme.
#[derive(Clone, Debug, PartialEq)]
struct StripMetrics {
    height: f64,
    /// Inset of the thumb inside the track.
    inset: f64,
    /// Label inset inside each segment.
    pad_x: f64,
    track_radius: f64,
    thumb_radius: f64,
    label: TextStyle,
}

impl StripMetrics {
    fn of(size: ControlSize, theme: &Theme) -> Self {
        // The thumb is concentric with the track: its inset is the step
        // between the control and item radii.
        let radii = &theme.tokens.radii;
        let inset = (radii.control - radii.item).max(0.0);
        Self {
            height: size.height(theme),
            inset: f64::from(inset),
            pad_x: f64::from(theme.tokens.spacing.s3),
            track_radius: f64::from(radii.control),
            thumb_radius: f64::from(radii.concentric(radii.control, inset)),
            label: size.label_style(theme),
        }
    }

    fn stock(size: ControlSize) -> Self {
        static STOCK: std::sync::LazyLock<[StripMetrics; 3]> = std::sync::LazyLock::new(|| {
            let theme = Theme::dark();
            [
                StripMetrics::of(ControlSize::Small, &theme),
                StripMetrics::of(ControlSize::Regular, &theme),
                StripMetrics::of(ControlSize::Large, &theme),
            ]
        });
        let index = match size {
            ControlSize::Small => 0,
            ControlSize::Regular => 1,
            ControlSize::Large => 2,
        };
        STOCK
            .get(index)
            .cloned()
            .unwrap_or_else(|| Self::of(size, &Theme::dark()))
    }

    /// One segment's natural width for `label`: the chosen (strong) label
    /// plus its padding, never narrower than the strip is tall.
    fn natural(&self, label: &str, text: &mut TextRenderer) -> f64 {
        let w = f64::from(
            text.measure_style(label, None, &self.label.clone().strong())
                .0,
        );
        (w + 2.0 * self.pad_x).max(self.height).ceil()
    }

    /// The narrowest strip that shows every full label: equal segments as
    /// wide as the widest label plus its padding.
    fn intrinsic_width(&self, labels: &[&str], text: &mut TextRenderer) -> f64 {
        let widest = labels
            .iter()
            .map(|label| {
                f64::from(
                    text.measure_style(label, None, &self.label.clone().strong())
                        .0,
                )
            })
            .fold(0.0, f64::max);
        let segment = (widest + 2.0 * self.pad_x).max(self.height);
        (segment * usize_to_f64(labels.len()) + 2.0 * self.inset).ceil()
    }
}

/// `(fill, label)` of one segment; `fill == None` means nothing is painted
/// under it. On a [`SegmentedStyle::Segmented`] track the fill is the thumb.
pub fn segment_colors(
    theme: &Theme,
    style: SegmentedStyle,
    selected: bool,
    hovered: bool,
) -> (Option<Color>, Color) {
    let colors = &theme.tokens.colors;
    let ground = match style {
        SegmentedStyle::Segmented => colors.surfaces.raised,
        SegmentedStyle::Tabs | SegmentedStyle::Quiet => colors.surfaces.content,
    };
    let tier = selection_tier_on(theme, SelectionState::new(selected, hovered, false), ground);
    let chosen = selection_tier_on(theme, SelectionState::active(true), ground).fill;
    let hover = selection_tier_on(theme, SelectionState::new(false, true, false), ground).fill;
    // Resolved over the ground and its hover lift at once, so an idle label
    // does not change colour under the pointer.
    let quiet_ink = colors.ink_on_any(&[ground, hover], colors.text_dim, ON_FILL_MIN_CONTRAST);
    match style {
        SegmentedStyle::Segmented | SegmentedStyle::Quiet => {
            let fill = (selected || hovered).then_some(tier.fill);
            let ink = if selected {
                colors.ink_on_any(&[chosen], colors.text_primary, ON_FILL_MIN_CONTRAST)
            } else {
                quiet_ink
            };
            (fill, ink)
        }
        SegmentedStyle::Tabs => {
            // An inactive tab's label holds its colour under the pointer;
            // the underline alone marks the chosen one.
            let ink = if selected {
                colors.ink_on_any(&[ground], colors.text_primary, ON_FILL_MIN_CONTRAST)
            } else {
                quiet_ink
            };
            (None, ink)
        }
    }
}

/// An immediate-mode segmented strip over caller-owned segment rects (the
/// rects must be contiguous). The caller owns the hit-test; this paints the
/// themed look over them. Size the strip with [`SegmentedFace::height`] and
/// [`SegmentedFace::intrinsic_width`].
pub struct SegmentedFace<'a> {
    segments: &'a [Rect],
    labels: &'a [&'a str],
    selected: usize,
    hovered: Option<usize>,
    focused: bool,
    style: SegmentedStyle,
    control_size: ControlSize,
}

impl<'a> SegmentedFace<'a> {
    pub fn new(segments: &'a [Rect], labels: &'a [&'a str], selected: usize) -> Self {
        Self {
            segments,
            labels,
            selected,
            hovered: None,
            focused: false,
            style: SegmentedStyle::Segmented,
            control_size: ControlSize::Regular,
        }
    }
    #[must_use]
    pub fn style(mut self, style: SegmentedStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn hovered(mut self, idx: Option<usize>) -> Self {
        self.hovered = idx;
        self
    }
    /// Draw the focus ring, in your colour, round the strip.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
    /// Label role; the host sizes the rects with [`Self::height`].
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
        self
    }
    /// The height of a strip of `size` in `theme`.
    pub fn height(size: ControlSize, theme: &Theme) -> f64 {
        StripMetrics::of(size, theme).height
    }
    /// The narrowest strip that shows every full label in `theme`.
    pub fn intrinsic_width(
        labels: &[&str],
        size: ControlSize,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        StripMetrics::of(size, theme).intrinsic_width(labels, text)
    }
    /// The segment rects of a strip in `rect`: equal while every label fits
    /// an equal share, otherwise each as wide as its label needs with the
    /// rest shared, as a `ToggleGroup` partitions itself.
    pub fn partition(
        rect: Rect,
        labels: &[&str],
        size: ControlSize,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<Rect> {
        let metrics = StripMetrics::of(size, theme);
        let naturals: Vec<(f64, f64)> = labels
            .iter()
            .map(|label| {
                let w = metrics.natural(label, text);
                (w, w)
            })
            .collect();
        partition_sized(rect, &naturals).0
    }
}

impl ToggleGroup {
    /// The track (and its separators) under a [`SegmentedStyle::Segmented`]
    /// strip, or the underline of [`SegmentedStyle::Tabs`].
    fn paint_ground(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        face: &SegmentedFace<'_>,
        metrics: &StripMetrics,
        outer: Rect,
    ) {
        let colors = &theme.tokens.colors;
        match face.style {
            SegmentedStyle::Segmented => {
                let track = colors.surfaces.raised;
                let edge = crate::surface::edge_on(track);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    track,
                    None,
                    &RoundedRect::from_rect(outer, metrics.track_radius),
                );
                if !face.focused {
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        edge,
                        None,
                        &crate::surface::edge_outline(outer, metrics.track_radius),
                    );
                }
                // Separators stop short of the edge so they never run into
                // the corner radius, and vanish beside the thumb.
                let inset = f64::from(theme.tokens.spacing.s2);
                for i in separators(face.segments.len(), face.selected, face.style) {
                    let Some(seg) = face.segments.get(i) else {
                        continue;
                    };
                    let x = seg.x1.round() - 0.5;
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        edge,
                        None,
                        &Line::new(
                            Point::new(x, outer.y0 + inset),
                            Point::new(x, outer.y1 - inset),
                        ),
                    );
                }
            }
            SegmentedStyle::Tabs => {
                if let Some(seg) = face.segments.get(face.selected) {
                    let thickness = 2.0 * f64::from(theme.ui_scale);
                    let inset = metrics.pad_x.min((seg.width() / 2.0 - 2.0).max(0.0));
                    let bar = Rect::new(seg.x0 + inset, seg.y1 - thickness, seg.x1 - inset, seg.y1);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        colors.text_primary,
                        None,
                        &RoundedRect::from_rect(bar, thickness / 2.0),
                    );
                }
            }
            SegmentedStyle::Quiet => {}
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
        let (Some(first), Some(last)) = (segs.first(), segs.last()) else {
            return;
        };
        let metrics = StripMetrics::of(face.control_size, theme);
        let outer = Rect::new(first.x0, first.y0, last.x1, first.y1);
        Self::paint_ground(scene, transform, theme, face, &metrics, outer);

        // The thumb (and the hover lift) of every style but tabs: a lift set
        // concentrically inside the segment.
        if face.style != SegmentedStyle::Tabs {
            for (i, seg) in segs.iter().enumerate() {
                let selected = i == face.selected;
                let hovered = face.hovered == Some(i) && !selected;
                let (Some(fill), _) = segment_colors(theme, face.style, selected, hovered) else {
                    continue;
                };
                let thumb = seg.inset(-metrics.inset);
                let shape = RoundedRect::from_rect(thumb, metrics.thumb_radius);
                scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
                let ground = if face.style == SegmentedStyle::Segmented {
                    theme.tokens.colors.surfaces.raised
                } else {
                    theme.tokens.colors.surfaces.content
                };
                let tier =
                    selection_tier_on(theme, SelectionState::new(selected, hovered, false), ground);
                if let Some(border) = tier.border {
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        border,
                        None,
                        &crate::surface::edge_outline(thumb, metrics.thumb_radius),
                    );
                }
            }
        }

        // Segment labels are `Label`s; the chosen one is set `.strong()` so
        // it reads first. A label that does not fit falls back to its short
        // form, then elides.
        let selected_style = metrics.label.clone().strong();
        for (i, seg) in segs.iter().enumerate() {
            let label = face.labels.get(i).copied().unwrap_or("");
            if label.is_empty() {
                continue;
            }
            let selected = i == face.selected;
            let (_, ink) = segment_colors(theme, face.style, selected, face.hovered == Some(i));
            let style = if selected {
                &selected_style
            } else {
                &metrics.label
            };
            let room = (seg.width() - 2.0 * metrics.pad_x.min(seg.width() / 4.0)).max(1.0);
            let shown = text_renderer.elide_to_width_style(label, px_f32(room), style);
            let (tw, th) = text_renderer.measure_style(&shown, None, style);
            let text_x = seg.center().x - f64::from(tw) / 2.0;
            let text_y = seg.y0 + (seg.height() - f64::from(th)) / 2.0;
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix multiplication; it cannot \
                          overflow or panic"
            )]
            let label_transform = transform * Affine::translate((text_x, text_y));
            text_renderer.draw_style(scene, &shown, ink, label_transform, None, style);
        }

        // Keyboard focus is your colour round the whole strip.
        if face.focused {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.intent.you,
                None,
                &crate::surface::edge_outline(outer, metrics.track_radius),
            );
        }
    }

    /// The labels this strip shows: each full label, or its short form where
    /// the partition had to fall back to it.
    fn shown_labels(&self) -> Vec<String> {
        self.segments
            .iter()
            .enumerate()
            .map(|(i, segment)| match &segment.short {
                Some(short) if self.use_short.get(i).copied().unwrap_or(false) => short.clone(),
                _ => segment.label.clone(),
            })
            .collect()
    }

    /// Measure every segment's natural widths; returns whether they changed.
    fn measure_naturals(&mut self, text: &mut TextRenderer) -> bool {
        let metrics = self.metrics.clone();
        let naturals: Vec<(f64, f64)> = self
            .segments
            .iter()
            .map(|segment| {
                let full = metrics.natural(&segment.label, text);
                let short = segment
                    .short
                    .as_deref()
                    .map_or(full, |short| metrics.natural(short, text));
                (full, short)
            })
            .collect();
        if naturals == self.naturals {
            return false;
        }
        self.naturals = naturals;
        true
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
        let metrics = StripMetrics::of(self.control_size, theme);
        if metrics != self.metrics {
            self.metrics = metrics;
            self.cache_dirty = true;
        }
        self.measured_width = None;
        let node = taffy.leaf_node(Style {
            size: Size {
                // Fit the labels (measured in `measure`) unless asked to fill.
                width: if self.fill {
                    Dimension::percent(1.0)
                } else {
                    Dimension::auto()
                },
                height: Dimension::length(px_f32(self.metrics.height)),
            },
            max_size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            // A segmented strip has an intrinsic label-bearing width; a
            // parent without room caps it (`max_size`) and the labels fall
            // back to their short forms, then elide.
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        _available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        let labels: Vec<&str> = self.segments.iter().map(|s| s.label.as_str()).collect();
        let intrinsic = px_f32(self.metrics.intrinsic_width(&labels, text));
        self.measure_naturals(text);
        if !self.fill {
            self.measured_width = Some(intrinsic);
        }
        Size {
            width: known.width.unwrap_or(intrinsic),
            height: known.height.unwrap_or(px_f32(self.metrics.height)),
        }
    }

    fn adjust_layout_style(&mut self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        let Some(width) = self.measured_width.filter(|_| !self.fill) else {
            return false;
        };
        let Ok(style) = taffy.style(node) else {
            return false;
        };
        let width = Dimension::length(width);
        if style.size.width == width {
            return false;
        }
        let mut style = style.clone();
        style.size.width = width;
        taffy.set_style(node, style).is_ok()
    }

    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        self.measure_naturals(text);
        if self.fill {
            return false;
        }
        if self.measured_width.is_none() {
            let labels: Vec<&str> = self.segments.iter().map(|s| s.label.as_str()).collect();
            self.measured_width = Some(px_f32(self.metrics.intrinsic_width(&labels, text)));
        }
        self.adjust_layout_style(taffy, node)
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
        if self.hovered != self.last_hovered {
            self.cache_dirty = true;
            self.last_hovered = self.hovered;
        }
        let colors = &theme.tokens.colors;
        let palette = [
            colors.surfaces.raised,
            colors.surfaces.content,
            colors.text_primary,
            colors.intent.you,
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
        // The partition follows the measured labels; a strip laid out before
        // its labels were measured (or in another theme) re-partitions here,
        // before it paints and before the next hit-test.
        let metrics = StripMetrics::of(self.control_size, theme);
        if metrics != self.metrics {
            self.metrics = metrics;
        }
        if self.measure_naturals(text_renderer) {
            self.set_rect_and_partition(self.rect);
        }
        let shown = self.shown_labels();
        let labels: Vec<&str> = shown.iter().map(String::as_str).collect();
        self.cached.reset();
        let face = SegmentedFace::new(&self.segment_rects, &labels, self.selected)
            .style(self.style)
            .control_size(self.control_size)
            .hovered(self.hovered)
            .focused(self.is_focused);
        Self::paint(
            &mut self.cached,
            Affine::IDENTITY,
            text_renderer,
            theme,
            &face,
        );
        self.cache_rect = self.rect;
        out_scene.append(&self.cached, Some(out_transform));
    }

    fn needs_redraw(&self) -> bool {
        self.cache_dirty
            || self.selected != self.last_selected
            || self.is_focused != self.last_focused
            || self.hovered != self.last_hovered
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
                self.hovered = self
                    .segment_rects
                    .iter()
                    .position(|segment| segment.contains(logical_pos));
            }
            WindowEvent::CursorLeft { .. } => self.hovered = None,
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
            return Vec::new();
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
    use crate::tokens::TextRole;
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
                        (run.font_size - theme.tokens.typography.size(TextRole::Label)).abs()
                            < 0.001,
                        "{scale}x text must paint once at resolved size {}, got {}",
                        theme.tokens.typography.size(TextRole::Label),
                        run.font_size
                    );
                }
            }
        }

        // Case `retained_group_fits_its_labels_at_its_control_height`.
        {
            let harness = crate::headless::FrameHarness::new(
                crate::State::new(),
                Box::new(
                    crate::widget::Column::new()
                        .with_no_padding()
                        .push(Box::new(ToggleGroup::new(
                            vec![Segment::new("World"), Segment::new("Timers")],
                            0,
                        ))),
                ),
                (600, 200),
            );
            let rect = harness.inspection().root.children[0].rect;
            assert_eq!(rect[3], ControlSize::Regular.height(&Theme::dark()));
            assert!(
                rect[2] < 300.0,
                "a fitted strip does not stretch to its parent: {rect:?}"
            );
            let mut text = TextRenderer::new();
            let widest = text
                .measure_style(
                    "Timers",
                    None,
                    &Theme::dark().text(TextRole::Label).strong(),
                )
                .0;
            assert!(
                rect[2] / 2.0
                    >= f64::from(widest) + 2.0 * f64::from(Theme::dark().tokens.spacing.s3),
                "each segment holds the widest label: {rect:?}"
            );
            let harness = crate::headless::FrameHarness::new(
                crate::State::new(),
                Box::new(
                    crate::widget::Column::new()
                        .with_no_padding()
                        .push(Box::new(
                            ToggleGroup::new(
                                vec![Segment::new("World"), Segment::new("Timers")],
                                0,
                            )
                            .fill(),
                        )),
                ),
                (600, 200),
            );
            assert_eq!(harness.inspection().root.children[0].rect[2], 600.0);
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
                1,
                "a separator stands only between two unchosen neighbours"
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
                SegmentedStyle::Segmented,
                SegmentedStyle::Tabs,
                SegmentedStyle::Quiet,
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
        }

        // Case `a_label_that_does_not_fit_falls_back_to_its_short_form`.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let mut group = group_at(
                vec![
                    Segment::new("Monday").short("Mon"),
                    Segment::new("Tuesday").short("Tue"),
                ],
                0,
                Rect::new(0.0, 0.0, 90.0, 32.0),
            );
            group.metrics = StripMetrics::of(ControlSize::Regular, &theme);
            group.measure_naturals(&mut text);
            group.set_rect_and_partition(Rect::new(0.0, 0.0, 90.0, 32.0));
            assert_eq!(group.shown_labels(), ["Mon", "Tue"]);
            group.set_rect_and_partition(Rect::new(0.0, 0.0, 400.0, 32.0));
            assert_eq!(group.shown_labels(), ["Monday", "Tuesday"]);
        }

        // Case `segments_size_to_their_labels_before_any_label_shortens`.
        // "Day / Week / Year" in a strip too narrow for three equal "Week"
        // segments but wide enough for the three labels: no segment elides,
        // the wider label gets the wider segment.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let mut group = group_at(
                vec![
                    Segment::new("Day"),
                    Segment::new("Week"),
                    Segment::new("Year"),
                ],
                1,
                Rect::new(0.0, 0.0, 400.0, 32.0),
            );
            group.metrics = StripMetrics::of(ControlSize::Regular, &theme);
            group.measure_naturals(&mut text);
            let naturals: Vec<f64> = group.naturals.iter().map(|(full, _)| *full).collect();
            let widest = naturals.iter().copied().fold(0.0, f64::max);
            let sum: f64 = naturals.iter().sum();
            assert!(
                sum < widest * 3.0,
                "the labels differ in width: {naturals:?}"
            );

            // Room for three equal widest segments: equal widths.
            group.set_rect_and_partition(Rect::new(0.0, 0.0, (widest * 3.0).ceil(), 32.0));
            let widths: Vec<f64> = group.segment_rects.iter().map(Rect::width).collect();
            assert!(
                widths.iter().all(|w| (w - widths[0]).abs() < 1e-6),
                "{widths:?}"
            );

            // Less than that but room for every label: sized to the labels.
            let tight = (sum + widest * 3.0) / 2.0;
            group.set_rect_and_partition(Rect::new(0.0, 0.0, tight, 32.0));
            for (rect, natural) in group.segment_rects.iter().zip(&naturals) {
                assert!(rect.width() + 1e-6 >= *natural, "{rect:?} holds {natural}");
            }
            assert_eq!(group.shown_labels(), ["Day", "Week", "Year"]);
            assert!(
                (group.segment_rects.last().unwrap().x1 - tight).abs() < 1e-6,
                "the strip is filled exactly"
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

        // Case `the_chosen_segment_is_a_colourless_lift_with_readable_ink`.
        {
            use crate::a11y::contrast_ratio;
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let colors = &theme.tokens.colors;
                for (style, ground) in [
                    (SegmentedStyle::Segmented, colors.surfaces.raised),
                    (SegmentedStyle::Quiet, colors.surfaces.content),
                ] {
                    let (fill, ink) = segment_colors(&theme, style, true, false);
                    let fill = fill.expect("the chosen segment is lifted");
                    assert_eq!(
                        fill,
                        selection_tier_on(&theme, SelectionState::active(true), ground).fill,
                        "{} {style:?}: the thumb is the selection tier of its ground",
                        theme.name
                    );
                    let chroma = crate::color::Oklch::from_color(fill).c;
                    assert!(
                        chroma <= 0.02,
                        "{} {style:?}: the chosen segment carries no hue ({chroma})",
                        theme.name
                    );
                    assert!(contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST);
                    let (idle, idle_ink) = segment_colors(&theme, style, false, false);
                    assert_eq!(idle, None, "{}: an idle segment paints nothing", theme.name);
                    assert!(
                        contrast_ratio(idle_ink, ground)
                            < contrast_ratio(colors.text_primary, ground),
                        "{} {style:?}: an idle label is quieter than the chosen one",
                        theme.name
                    );
                    let (hover, hover_ink) = segment_colors(&theme, style, false, true);
                    assert!(hover.is_some(), "a hovered segment lifts halfway");
                    assert_eq!(hover_ink, idle_ink, "the label holds its colour on hover");
                }
                let (fill, ink) = segment_colors(&theme, SegmentedStyle::Tabs, true, false);
                assert_eq!(fill, None, "a tab is marked by its underline");
                assert!(contrast_ratio(ink, colors.surfaces.content) >= ON_FILL_MIN_CONTRAST);
            }
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
