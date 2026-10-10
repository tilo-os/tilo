//! Highly configurable interactive Cartesian plots.
//!
//! [`CartesianPlot`] deliberately separates data, presentation and axes. A
//! series can change between line, smooth line, step, area, scatter, bubble,
//! bar and lollipop without changing its data model. Multiple kinds may be
//! mixed in one plot and share scales, hover inspection and accessibility.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "plot projection and vector geometry are bounded floating-point arithmetic"
)]

use crate::time::{Duration, Instant};
use crate::widget::ModelPath;
use std::cell::RefCell;
use std::collections::HashSet;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::paint::with_alpha;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Widget;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_legend;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchAction, ChartTouchTracker, chart_nav_action, moved_index,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AxisRange {
    Auto,
    Fixed { min: f64, max: f64 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AxisScale {
    #[default]
    Linear,
    Log10,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AxisTick {
    pub value: f64,
    pub label: String,
}

/// Built-in axis label formats for common presentation needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AxisLabelFormat {
    #[default]
    Auto,
    Decimal(u8),
    Compact,
    Percent(u8),
    Bytes(u8),
    Scientific(u8),
    /// Adaptive seconds, milliseconds, minutes, hours, or days.
    Duration(u8),
    Currency {
        symbol: char,
        places: u8,
    },
}

impl AxisTick {
    pub fn new(value: f64, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
        }
    }
}

/// Axis configuration. Explicit ticks are useful for dates and categories;
/// when empty, the plot produces `tick_count` compact numeric labels.
#[derive(Clone, Debug, PartialEq)]
pub struct PlotAxis {
    pub range: AxisRange,
    pub scale: AxisScale,
    pub ticks: Vec<AxisTick>,
    pub tick_count: u8,
    pub title: String,
    pub grid: bool,
    pub include_zero: bool,
    pub label_format: AxisLabelFormat,
    /// Minimum logical-pixel distance between generated ticks.
    pub min_tick_spacing: f64,
}

impl Default for PlotAxis {
    fn default() -> Self {
        Self {
            range: AxisRange::Auto,
            scale: AxisScale::Linear,
            ticks: Vec::new(),
            tick_count: 5,
            title: String::new(),
            grid: true,
            include_zero: false,
            label_format: AxisLabelFormat::Auto,
            min_tick_spacing: 72.0,
        }
    }
}

impl PlotAxis {
    #[must_use]
    pub fn fixed(mut self, min: f64, max: f64) -> Self {
        self.range = AxisRange::Fixed { min, max };
        self
    }

    #[must_use]
    pub fn logarithmic(mut self) -> Self {
        self.scale = AxisScale::Log10;
        self.include_zero = false;
        self
    }

    #[must_use]
    pub fn with_ticks(mut self, ticks: impl IntoIterator<Item = AxisTick>) -> Self {
        self.ticks = ticks.into_iter().collect();
        self
    }

    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    #[must_use]
    pub fn formatted(mut self, format: AxisLabelFormat) -> Self {
        self.label_format = format;
        self
    }

    #[must_use]
    pub fn with_tick_density(mut self, maximum: u8, min_spacing: f64) -> Self {
        self.tick_count = maximum.clamp(2, 20);
        self.min_tick_spacing = min_spacing.max(24.0);
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlotDatum {
    pub id: String,
    pub x: f64,
    pub y: f64,
    /// Bubble radius in logical pixels. Ignored by non-bubble series.
    pub radius: f64,
    pub label: String,
    pub detail: String,
}

impl PlotDatum {
    pub fn new(id: impl Into<String>, x: f64, y: f64) -> Self {
        Self {
            id: id.into(),
            x,
            y,
            radius: 5.0,
            label: String::new(),
            detail: String::new(),
        }
    }

    /// Create an explicit missing sample. Connected series break at missing
    /// samples unless their series opts into connecting gaps.
    pub fn missing(id: impl Into<String>, x: f64) -> Self {
        Self::new(id, x, f64::NAN)
    }

    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SeriesKind {
    Line,
    Smooth,
    Step,
    Area,
    SmoothArea,
    StepArea,
    Scatter,
    Bubble,
    Bar { width: f64 },
    Lollipop,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkerShape {
    #[default]
    Circle,
    Square,
    Diamond,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlotSeries {
    pub id: String,
    pub name: String,
    pub color: Color,
    pub kind: SeriesKind,
    pub data: Vec<PlotDatum>,
    pub stroke_width: f64,
    pub marker_radius: f64,
    /// Dash/gap lengths in logical pixels. Empty means a solid stroke.
    pub line_dash: Vec<f64>,
    pub fill_alpha: u8,
    pub marker_shape: MarkerShape,
    pub bar_radius: f64,
    /// Enables logarithmic-time pointer candidate lookup. The caller promises
    /// finite, ascending X values and should keep that invariant on updates.
    pub x_sorted: bool,
    pub connect_gaps: bool,
    /// Override the plot-wide line-marker preference for this series.
    pub show_markers: Option<bool>,
}

impl PlotSeries {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        color: Color,
        data: Vec<PlotDatum>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            color,
            kind: SeriesKind::Line,
            data,
            stroke_width: 2.0,
            marker_radius: 4.0,
            line_dash: Vec::new(),
            fill_alpha: 52,
            marker_shape: MarkerShape::Circle,
            bar_radius: 4.0,
            x_sorted: false,
            connect_gaps: false,
            show_markers: None,
        }
    }

    #[must_use]
    pub fn kind(mut self, kind: SeriesKind) -> Self {
        self.kind = kind;
        self
    }

    #[must_use]
    pub fn stroke_width(mut self, width: f64) -> Self {
        self.stroke_width = width.max(0.5);
        self
    }

    #[must_use]
    pub fn marker_radius(mut self, radius: f64) -> Self {
        self.marker_radius = radius.max(1.0);
        self
    }

    #[must_use]
    pub fn dashed(mut self, pattern: impl IntoIterator<Item = f64>) -> Self {
        self.line_dash = pattern
            .into_iter()
            .filter(|length| length.is_finite() && *length > 0.0)
            .collect();
        self
    }

    #[must_use]
    pub fn fill_alpha(mut self, alpha: u8) -> Self {
        self.fill_alpha = alpha;
        self
    }

    #[must_use]
    pub fn marker_shape(mut self, shape: MarkerShape) -> Self {
        self.marker_shape = shape;
        self
    }

    #[must_use]
    pub fn bar_radius(mut self, radius: f64) -> Self {
        self.bar_radius = radius.max(0.0);
        self
    }

    /// Declare that datum X values are finite and ascending. This preserves
    /// exact hover results while avoiding a full scan on every pointer move.
    #[must_use]
    pub fn sorted_by_x(mut self) -> Self {
        self.x_sorted = true;
        self
    }

    #[must_use]
    pub fn connect_gaps(mut self, connect: bool) -> Self {
        self.connect_gaps = connect;
        self
    }

    #[must_use]
    pub fn show_markers(mut self, show: bool) -> Self {
        self.show_markers = Some(show);
        self
    }
}

/// A reference line, highlighted interval, or labelled point rendered in the
/// plot's data coordinate system.
#[derive(Clone, Debug, PartialEq)]
pub enum CartesianAnnotationKind {
    XLine(f64),
    YLine(f64),
    XBand { start: f64, end: f64 },
    YBand { start: f64, end: f64 },
    Point { x: f64, y: f64 },
}

/// Presentation for an analytical annotation. Annotations deliberately use
/// data coordinates, so they remain correct through resize, zoom, and pan.
#[derive(Clone, Debug, PartialEq)]
pub struct CartesianAnnotation {
    pub id: String,
    pub kind: CartesianAnnotationKind,
    pub label: String,
    pub detail: String,
    pub color: Color,
    pub stroke_width: f64,
    pub dashed: bool,
    pub fill_alpha: u8,
    pub interactive: bool,
}

impl CartesianAnnotation {
    pub fn x_line(id: impl Into<String>, value: f64, color: Color) -> Self {
        Self::new(id, CartesianAnnotationKind::XLine(value), color)
    }

    pub fn y_line(id: impl Into<String>, value: f64, color: Color) -> Self {
        Self::new(id, CartesianAnnotationKind::YLine(value), color)
    }

    pub fn x_band(id: impl Into<String>, start: f64, end: f64, color: Color) -> Self {
        Self::new(id, CartesianAnnotationKind::XBand { start, end }, color)
    }

    pub fn point(id: impl Into<String>, x: f64, y: f64, color: Color) -> Self {
        Self::new(id, CartesianAnnotationKind::Point { x, y }, color)
    }

    fn new(id: impl Into<String>, kind: CartesianAnnotationKind, color: Color) -> Self {
        Self {
            id: id.into(),
            kind,
            label: String::new(),
            detail: String::new(),
            color,
            stroke_width: 1.5,
            dashed: true,
            fill_alpha: 30,
            interactive: true,
        }
    }

    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn stroke_width(mut self, width: f64) -> Self {
        self.stroke_width = width.max(0.5);
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    #[must_use]
    pub fn solid(mut self) -> Self {
        self.dashed = false;
        self
    }

    #[must_use]
    pub fn fill_alpha(mut self, alpha: u8) -> Self {
        self.fill_alpha = alpha;
        self
    }

    #[must_use]
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegendPosition {
    Hidden,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CartesianZoom {
    #[default]
    Disabled,
    X,
    XY,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CartesianPan {
    #[default]
    Disabled,
    X,
    XY,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CartesianBrush {
    #[default]
    Disabled,
    X,
    XY,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CartesianTooltipAnchor {
    #[default]
    Pointer,
    Datum,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CartesianBrushSelection {
    pub x: AxisRange,
    pub y: Option<AxisRange>,
}

#[derive(Clone, Debug)]
pub struct CartesianTooltipStyle {
    pub enabled: bool,
    pub anchor: CartesianTooltipAnchor,
    /// Fixed tooltip width. `None` sizes to content within the chart bounds.
    pub width: Option<f64>,
    pub min_width: f64,
    pub offset: f64,
    pub padding: f64,
    /// Append formatted x/y coordinates even when a datum has custom detail.
    pub show_coordinates: bool,
}

impl Default for CartesianTooltipStyle {
    fn default() -> Self {
        Self {
            enabled: true,
            anchor: CartesianTooltipAnchor::Pointer,
            width: None,
            min_width: 140.0,
            offset: 18.0,
            padding: 14.0,
            show_coordinates: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CartesianRenderingStyle {
    pub show_crosshair: bool,
    pub show_markers_on_lines: bool,
    /// Reduce dense connected series to a spike-preserving pixel budget.
    /// Scatter, bubble, bar, and lollipop series are never sampled.
    pub adaptive_sampling: bool,
    /// Approximate connected-series point budget per logical plot pixel.
    pub max_points_per_pixel: f64,
}

impl Default for CartesianRenderingStyle {
    fn default() -> Self {
        Self {
            show_crosshair: true,
            show_markers_on_lines: false,
            adaptive_sampling: true,
            max_points_per_pixel: 2.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CartesianGesturePolicy {
    /// Require Ctrl/Command for wheel zoom so embedded plots remain
    /// scroll-friendly. Disable only in a dedicated chart viewport.
    pub zoom_modifier_required: bool,
    /// Require Alt while beginning a pointer pan so charts in scrollable
    /// layouts do not steal ordinary drags.
    pub pan_modifier_required: bool,
    /// Require Shift while beginning a pointer brush.
    pub brush_modifier_required: bool,
}

impl Default for CartesianGesturePolicy {
    fn default() -> Self {
        Self {
            zoom_modifier_required: true,
            pan_modifier_required: true,
            brush_modifier_required: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CartesianAnimationStyle {
    /// Smoothly interpolate matching stable datum ids after `set_series`.
    pub enabled: bool,
    pub duration: Duration,
}

impl Default for CartesianAnimationStyle {
    fn default() -> Self {
        Self {
            enabled: true,
            duration: Duration::from_millis(240),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CartesianStyle {
    pub x_axis: PlotAxis,
    pub y_axis: PlotAxis,
    pub legend: LegendPosition,
    pub rendering: CartesianRenderingStyle,
    pub tooltip: CartesianTooltipStyle,
    pub plot_padding: f64,
    pub hit_radius: f64,
    pub zoom: CartesianZoom,
    pub pan: CartesianPan,
    pub gestures: CartesianGesturePolicy,
    /// Smallest visible fraction of the original data domain.
    pub min_zoom_fraction: f64,
    /// Let pointer and accessibility activation toggle series from the legend.
    pub interactive_legend: bool,
    pub brush: CartesianBrush,
    pub animation: CartesianAnimationStyle,
}

impl Default for CartesianStyle {
    fn default() -> Self {
        Self {
            x_axis: PlotAxis::default(),
            y_axis: PlotAxis::default(),
            legend: LegendPosition::Top,
            rendering: CartesianRenderingStyle::default(),
            tooltip: CartesianTooltipStyle::default(),
            plot_padding: 12.0,
            hit_radius: 28.0,
            zoom: CartesianZoom::Disabled,
            pan: CartesianPan::Disabled,
            gestures: CartesianGesturePolicy::default(),
            min_zoom_fraction: 0.01,
            interactive_legend: true,
            brush: CartesianBrush::Disabled,
            animation: CartesianAnimationStyle::default(),
        }
    }
}

impl CartesianStyle {
    /// Configure responsive min/max sampling for connected series. Values are
    /// clamped to a practical range; pass `false` for exact path rendering.
    #[must_use]
    pub fn with_adaptive_sampling(mut self, enabled: bool, points_per_pixel: f64) -> Self {
        self.rendering.adaptive_sampling = enabled;
        self.rendering.max_points_per_pixel = if points_per_pixel.is_finite() {
            points_per_pixel.clamp(0.25, 16.0)
        } else {
            2.0
        };
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Domain {
    min: f64,
    max: f64,
}

struct AnnotationPaintContext<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
    plot: Rect,
    x_domain: Domain,
    y_domain: Domain,
}

#[derive(Clone, Debug)]
struct DomainCache {
    x_axis: PlotAxis,
    y_axis: PlotAxis,
    x: Option<Domain>,
    y: Option<Domain>,
}

impl Domain {
    fn normalized(min: f64, max: f64) -> Self {
        if min.is_finite() && max.is_finite() && max > min {
            Self { min, max }
        } else {
            let center = if min.is_finite() { min } else { 0.0 };
            Self {
                min: center - 0.5,
                max: center + 0.5,
            }
        }
    }

    fn from_range(range: AxisRange) -> Option<Self> {
        match range {
            AxisRange::Fixed { min, max } if min.is_finite() && max.is_finite() && max > min => {
                Some(Self { min, max })
            }
            AxisRange::Auto | AxisRange::Fixed { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Hovered {
    series: usize,
    datum: usize,
    point: Point,
}

pub struct CartesianPlot {
    series: Vec<PlotSeries>,
    annotations: Vec<CartesianAnnotation>,
    style: CartesianStyle,
    title: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    hovered: Option<Hovered>,
    hovered_annotation: Option<usize>,
    activated: Option<(String, String)>,
    annotation_activated: Option<String>,
    x_view: Option<Domain>,
    y_view: Option<Domain>,
    view_changed: bool,
    hidden_series: HashSet<String>,
    legend_hits: Vec<(String, Rect)>,
    visibility_changed: Option<(String, bool)>,
    keyboard_inspection: bool,
    brush_start: Option<Point>,
    brush_current: Point,
    pan_last: Option<Point>,
    brushed: Option<CartesianBrushSelection>,
    transition_from: Option<Vec<PlotSeries>>,
    transition_started: Instant,
    reduce_motion: bool,
    domain_cache: RefCell<Option<DomainCache>>,
    touch: ChartTouchTracker,
    /// The bands around the plot, measured from the theme and the labels
    /// at the last draw.
    bands: Bands,
    /// Keyboard inspection only while the plot has focus.
    focused: bool,
}

/// The space around a plot: tick labels, axis titles, the chart's title and
/// its legend, each from the theme's type and spacing and measured text.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Bands {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    /// Where the legend's first row starts.
    legend_top: f64,
    gap: f64,
    tick_line: f64,
}

impl Bands {
    /// Before the first draw: the stock theme's caption metrics.
    const STOCK: Self = Self {
        left: 56.0,
        top: 30.0,
        right: 12.0,
        bottom: 34.0,
        legend_top: 12.0,
        gap: 8.0,
        tick_line: 16.0,
    };
}

impl CartesianPlot {
    fn transform(value: f64, scale: AxisScale) -> Option<f64> {
        match scale {
            AxisScale::Linear => value.is_finite().then_some(value),
            AxisScale::Log10 => (value.is_finite() && value > 0.0).then(|| value.log10()),
        }
    }
    fn inverse(value: f64, scale: AxisScale) -> f64 {
        match scale {
            AxisScale::Linear => value,
            AxisScale::Log10 => 10.0_f64.powf(value),
        }
    }
    fn project(
        value: f64,
        domain: Domain,
        scale: AxisScale,
        origin: f64,
        span: f64,
    ) -> Option<f64> {
        let value = Self::transform(value, scale)?;
        let min = Self::transform(domain.min, scale)?;
        let max = Self::transform(domain.max, scale)?;
        Some(((value - min) / (max - min)).mul_add(span, origin))
    }
    pub fn new(series: Vec<PlotSeries>) -> Self {
        Self {
            series,
            annotations: Vec::new(),
            style: CartesianStyle::default(),
            title: String::new(),
            height: 320.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            hovered: None,
            hovered_annotation: None,
            activated: None,
            annotation_activated: None,
            x_view: None,
            y_view: None,
            view_changed: false,
            hidden_series: HashSet::new(),
            legend_hits: Vec::new(),
            visibility_changed: None,
            keyboard_inspection: false,
            brush_start: None,
            brush_current: Point::ZERO,
            pan_last: None,
            brushed: None,
            transition_from: None,
            transition_started: crate::time::now(),
            reduce_motion: false,
            domain_cache: RefCell::new(None),
            touch: ChartTouchTracker::default(),
            bands: Bands::STOCK,
            focused: false,
        }
    }

    #[must_use]
    pub fn styled(mut self, style: CartesianStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: CartesianStyle) -> Self {
        self.styled(style)
    }
    #[must_use]
    pub fn with_annotations(
        mut self,
        annotations: impl IntoIterator<Item = CartesianAnnotation>,
    ) -> Self {
        self.annotations = annotations.into_iter().collect();
        self
    }
    pub fn annotations(&self) -> &[CartesianAnnotation] {
        &self.annotations
    }
    pub fn annotations_mut(&mut self) -> &mut Vec<CartesianAnnotation> {
        &mut self.annotations
    }
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height.max(100.0);
        self
    }
    pub fn set_series(&mut self, series: Vec<PlotSeries>) {
        if self.series == series {
            return;
        }
        let previous = std::mem::replace(&mut self.series, series);
        if self.style.animation.enabled
            && !self.reduce_motion
            && !self.style.animation.duration.is_zero()
        {
            self.transition_from = Some(previous);
            self.transition_started = crate::time::now();
        } else {
            self.transition_from = None;
        }
        self.hidden_series
            .retain(|id| self.series.iter().any(|series| series.id == *id));
        self.hovered = None;
        self.invalidate_domains();
    }
    /// Replace data without motion, useful for initial hydration, restored
    /// sessions, and deterministic capture.
    pub fn set_series_immediate(&mut self, series: Vec<PlotSeries>) {
        self.series = series;
        self.transition_from = None;
        self.hidden_series
            .retain(|id| self.series.iter().any(|series| series.id == *id));
        self.hovered = None;
        self.invalidate_domains();
    }
    pub fn finish_animation(&mut self) {
        self.transition_from = None;
    }
    pub fn is_animating(&self) -> bool {
        !self.reduce_motion
            && self.style.animation.enabled
            && self.transition_from.is_some()
            && self.transition_started.elapsed() < self.style.animation.duration
    }

    fn animation_progress_at(&self, now: Instant) -> f64 {
        if self.reduce_motion
            || !self.style.animation.enabled
            || self.transition_from.is_none()
            || self.style.animation.duration.is_zero()
        {
            return 1.0;
        }
        let raw = now
            .saturating_duration_since(self.transition_started)
            .as_secs_f64()
            / self.style.animation.duration.as_secs_f64();
        crate::animation::ease_out(raw.clamp(0.0, 1.0))
    }

    fn animated_xy(&self, series: usize, datum: usize, now: Instant) -> Option<(f64, f64)> {
        let target_series = self.series.get(series)?;
        let target = target_series.data.get(datum)?;
        let progress = self.animation_progress_at(now);
        if progress >= 1.0 {
            return Some((target.x, target.y));
        }
        let previous = self
            .transition_from
            .as_ref()?
            .iter()
            .find(|series| series.id == target_series.id)?
            .data
            .iter()
            .find(|datum| datum.id == target.id)?;
        Some((
            previous.x + (target.x - previous.x) * progress,
            previous.y + (target.y - previous.y) * progress,
        ))
    }
    pub fn is_series_visible(&self, id: &str) -> bool {
        self.series.iter().any(|series| series.id == id) && !self.hidden_series.contains(id)
    }
    pub fn set_series_visible(&mut self, id: &str, visible: bool) -> bool {
        if !self.series.iter().any(|series| series.id == id) {
            return false;
        }
        let changed = if visible {
            self.hidden_series.remove(id)
        } else {
            self.hidden_series.insert(id.to_owned())
        };
        if changed {
            self.hovered = None;
            self.visibility_changed = Some((id.to_owned(), visible));
            self.invalidate_domains();
        }
        changed
    }
    pub fn toggle_series(&mut self, id: &str) -> bool {
        let visible = !self.is_series_visible(id);
        self.set_series_visible(id, visible)
    }
    pub fn take_visibility_changed(&mut self) -> Option<(String, bool)> {
        self.visibility_changed.take()
    }
    pub fn inspect(&mut self, series_id: &str, datum_id: &str) -> bool {
        let target = self.series.iter().enumerate().find_map(|(series, values)| {
            if self.hidden_series.contains(&values.id) || values.id != series_id {
                return None;
            }
            values
                .data
                .iter()
                .position(|datum| datum.id == datum_id)
                .map(|datum| (series, datum))
        });
        self.hovered = target.and_then(|(series, datum)| {
            let datum_ref = self.series.get(series)?.data.get(datum)?;
            self.map_datum(
                self.plot_rect(),
                self.domain(&self.style.x_axis, true),
                self.domain(&self.style.y_axis, false),
                datum_ref,
            )
            .map(|point| Hovered {
                series,
                datum,
                point,
            })
        });
        self.keyboard_inspection = self.hovered.is_some();
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.hovered_annotation = None;
        self.keyboard_inspection = false;
    }
    pub fn inspect_annotation(&mut self, id: &str) -> bool {
        self.hovered_annotation = self
            .annotations
            .iter()
            .position(|annotation| annotation.id == id && annotation.interactive);
        if self.hovered_annotation.is_some() {
            self.hovered = None;
            self.keyboard_inspection = true;
        }
        self.hovered_annotation.is_some()
    }
    /// Restore automatic/fixed axis domains after interactive zooming.
    pub fn reset_view(&mut self) {
        self.x_view = None;
        self.y_view = None;
        self.view_changed = true;
        self.update_hover();
    }
    /// Current interactive viewport, if zoom has changed it.
    pub fn viewport(&self) -> (Option<AxisRange>, Option<AxisRange>) {
        let range = |domain: Option<Domain>| {
            domain.map(|domain| AxisRange::Fixed {
                min: domain.min,
                max: domain.max,
            })
        };
        (range(self.x_view), range(self.y_view))
    }
    /// Set a viewport in data coordinates. `None` or `AxisRange::Auto` restores
    /// that axis to its configured domain. Invalid or empty fixed ranges are
    /// rejected without changing the current view.
    pub fn set_viewport(&mut self, x: Option<AxisRange>, y: Option<AxisRange>) -> bool {
        let parse = |range: Option<AxisRange>, scale: AxisScale| match range {
            None | Some(AxisRange::Auto) => Ok(None),
            Some(range) => Domain::from_range(range)
                .filter(|domain| {
                    Self::transform(domain.min, scale).is_some()
                        && Self::transform(domain.max, scale).is_some()
                })
                .map(Some)
                .ok_or(()),
        };
        let (Ok(x), Ok(y)) = (
            parse(x, self.style.x_axis.scale),
            parse(y, self.style.y_axis.scale),
        ) else {
            return false;
        };
        if self.x_view.map(|d| (d.min, d.max)) == x.map(|d| (d.min, d.max))
            && self.y_view.map(|d| (d.min, d.max)) == y.map(|d| (d.min, d.max))
        {
            return false;
        }
        self.x_view = x;
        self.y_view = y;
        self.view_changed = true;
        self.update_hover();
        true
    }
    /// Pan by fractions of the visible width and height. This is convenient
    /// for responsive buttons, trackpads, and synchronized overview charts.
    pub fn pan_view_fraction(&mut self, x_fraction: f64, y_fraction: f64) -> bool {
        let x = self
            .x_view
            .unwrap_or_else(|| self.base_domain(&self.style.x_axis, true));
        let y = self
            .y_view
            .unwrap_or_else(|| self.base_domain(&self.style.y_axis, false));
        if !x_fraction.is_finite() || !y_fraction.is_finite() {
            return false;
        }
        let shift = |domain: Domain, fraction: f64, scale: AxisScale| {
            let min = Self::transform(domain.min, scale)?;
            let max = Self::transform(domain.max, scale)?;
            let delta = (max - min) * fraction;
            Some(Domain {
                min: Self::inverse(min + delta, scale),
                max: Self::inverse(max + delta, scale),
            })
        };
        let (Some(x), Some(y)) = (
            shift(x, x_fraction, self.style.x_axis.scale),
            shift(y, y_fraction, self.style.y_axis.scale),
        ) else {
            return false;
        };
        self.set_viewport(
            Some(AxisRange::Fixed {
                min: x.min,
                max: x.max,
            }),
            Some(AxisRange::Fixed {
                min: y.min,
                max: y.max,
            }),
        )
    }
    fn pan_view_pixels(&mut self, dx: f64, dy: f64, plot: Rect) -> bool {
        if !dx.is_finite()
            || !dy.is_finite()
            || plot.width() <= 0.0
            || plot.height() <= 0.0
            || self.style.pan == CartesianPan::Disabled
        {
            return false;
        }
        let shift = |domain: Domain, pixels: f64, extent: f64, scale: AxisScale| {
            let min = Self::transform(domain.min, scale)?;
            let max = Self::transform(domain.max, scale)?;
            let delta = pixels / extent * (max - min);
            Some(Domain {
                min: Self::inverse(min + delta, scale),
                max: Self::inverse(max + delta, scale),
            })
        };
        let x_current = self
            .x_view
            .unwrap_or_else(|| self.base_domain(&self.style.x_axis, true));
        let Some(x) = shift(x_current, -dx, plot.width(), self.style.x_axis.scale) else {
            return false;
        };
        let x = Some(AxisRange::Fixed {
            min: x.min,
            max: x.max,
        });
        let y = if self.style.pan == CartesianPan::XY {
            let current = self
                .y_view
                .unwrap_or_else(|| self.base_domain(&self.style.y_axis, false));
            let Some(y) = shift(current, dy, plot.height(), self.style.y_axis.scale) else {
                return false;
            };
            Some(AxisRange::Fixed {
                min: y.min,
                max: y.max,
            })
        } else {
            self.viewport().1
        };
        self.set_viewport(x, y)
    }
    /// Promote a brush result to the active viewport, enabling concise
    /// brush-to-zoom and linked-detail workflows.
    pub fn zoom_to_selection(&mut self, selection: CartesianBrushSelection) -> bool {
        self.set_viewport(Some(selection.x), selection.y)
    }
    pub fn style_mut(&mut self) -> &mut CartesianStyle {
        self.invalidate_domains();
        &mut self.style
    }
    pub fn style(&self) -> &CartesianStyle {
        &self.style
    }
    pub fn take_activated(&mut self) -> Option<(String, String)> {
        self.activated.take()
    }

    fn plot_rect(&self) -> Rect {
        let b = self.bands;
        Rect::new(
            self.rect.x0 + b.left,
            self.rect.y0 + b.top,
            (self.rect.x1 - b.right).max(self.rect.x0 + b.left + 1.0),
            (self.rect.y1 - b.bottom).max(self.rect.y0 + b.top + 1.0),
        )
    }

    /// Measure the bands around the plot in `theme`: the widest y tick
    /// label and the tick, title and legend lines, each a spacing step from
    /// its neighbour.
    fn measure_bands(&self, tr: &mut TextRenderer, theme: &Theme) -> Bands {
        let spacing = &theme.tokens.spacing;
        let gap = f64::from(spacing.s2);
        let tight = f64::from(spacing.s1);
        let caption = theme.text(TextRole::Caption);
        let tick_line = f64::from(tr.measure_style("0", None, &caption).1);
        let title_line = f64::from(
            tr.measure_style("Ag", None, &theme.text(TextRole::Heading))
                .1,
        );
        let yd = self.domain(&self.style.y_axis, false);
        let widest_tick = Self::ticks(&self.style.y_axis, yd, self.rect.height().max(1.0))
            .iter()
            .map(|tick| f64::from(tr.measure_style(&tick.label, None, &caption).0))
            .fold(0.0, f64::max);
        let y_title = if self.style.y_axis.title.is_empty() {
            0.0
        } else {
            tick_line + tight
        };
        let x_title = if self.style.x_axis.title.is_empty() {
            0.0
        } else {
            tick_line + tight
        };
        // The y labels start on the plot's leading edge, the content edge
        // the text around it stands on.
        let left = (y_title + widest_tick + gap).ceil();
        let right = self.style.plot_padding.max(gap);
        let legend_rows = if self.style.legend == LegendPosition::Hidden {
            0
        } else {
            chart_legend::rows(
                self.series.iter().map(|series| series.name.as_str()),
                self.rect.x0,
                self.rect.x1 - right,
            )
        };
        let legend = if legend_rows == 0 {
            0.0
        } else {
            crate::num::f64_of_usize(legend_rows) * chart_legend::ROW_HEIGHT + gap
        };
        let title = if self.title.is_empty() {
            0.0
        } else {
            title_line + gap
        };
        let legend_top = gap + title;
        // The top tick label is centred on the top gridline: half a line of
        // it sits above the plot, clear of the legend by a step.
        let top = legend_top
            + if self.style.legend == LegendPosition::Top {
                legend
            } else {
                0.0
            }
            + tick_line / 2.0
            + gap;
        let bottom = gap
            + tick_line
            + x_title
            + gap
            + if self.style.legend == LegendPosition::Bottom {
                legend
            } else {
                0.0
            };
        Bands {
            left,
            top: top.ceil(),
            right,
            bottom: bottom.ceil(),
            legend_top,
            gap,
            tick_line,
        }
    }

    fn domain(&self, axis: &PlotAxis, x_axis: bool) -> Domain {
        if let Some(view) = if x_axis { self.x_view } else { self.y_view } {
            return view;
        }
        self.base_domain(axis, x_axis)
    }

    fn base_domain(&self, axis: &PlotAxis, x_axis: bool) -> Domain {
        {
            let cache = self.domain_cache.borrow();
            if let Some(cache) = cache.as_ref()
                && cache.x_axis == self.style.x_axis
                && cache.y_axis == self.style.y_axis
                && let Some(domain) = if x_axis { cache.x } else { cache.y }
            {
                return domain;
            }
        }
        let domain = self.compute_base_domain(axis, x_axis);
        let mut cache = self.domain_cache.borrow_mut();
        if cache.as_ref().is_none_or(|cache| {
            cache.x_axis != self.style.x_axis || cache.y_axis != self.style.y_axis
        }) {
            *cache = Some(DomainCache {
                x_axis: self.style.x_axis.clone(),
                y_axis: self.style.y_axis.clone(),
                x: None,
                y: None,
            });
        }
        if let Some(cache) = cache.as_mut() {
            if x_axis {
                cache.x = Some(domain);
            } else {
                cache.y = Some(domain);
            }
        }
        domain
    }

    fn invalidate_domains(&self) {
        *self.domain_cache.borrow_mut() = None;
    }

    fn compute_base_domain(&self, axis: &PlotAxis, x_axis: bool) -> Domain {
        if let AxisRange::Fixed { min, max } = axis.range {
            if max > min
                && Self::transform(min, axis.scale).is_some()
                && Self::transform(max, axis.scale).is_some()
            {
                return Domain { min, max };
            }
            return if axis.scale == AxisScale::Log10 {
                Domain {
                    min: 1.0,
                    max: 10.0,
                }
            } else {
                Domain::normalized(min, max)
            };
        }
        let values = self
            .series
            .iter()
            .filter(|series| !self.hidden_series.contains(&series.id))
            .flat_map(|series| series.data.iter())
            .map(|datum| if x_axis { datum.x } else { datum.y })
            .filter(|value| Self::transform(*value, axis.scale).is_some());
        let (mut min, mut max) = values
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
                (min.min(value), max.max(value))
            });
        if axis.include_zero && axis.scale == AxisScale::Linear {
            min = min.min(0.0);
            max = max.max(0.0);
        }
        if axis.scale == AxisScale::Log10 {
            if !min.is_finite() || !max.is_finite() {
                return Domain {
                    min: 1.0,
                    max: 10.0,
                };
            }
            if max <= min {
                return Domain {
                    min: min / 10.0_f64.sqrt(),
                    max: min * 10.0_f64.sqrt(),
                };
            }
            let log_min = min.log10();
            let log_max = max.log10();
            let padding = (log_max - log_min) * 0.06;
            return Domain {
                min: 10.0_f64.powf(log_min - padding),
                max: 10.0_f64.powf(log_max + padding),
            };
        }
        let domain = Domain::normalized(min, max);
        let padding = (domain.max - domain.min) * 0.06;
        let padded_min = if axis.include_zero && domain.min >= 0.0 {
            0.0
        } else {
            domain.min - padding
        };
        let padded_max = if axis.include_zero && domain.max <= 0.0 {
            0.0
        } else {
            domain.max + padding
        };
        Domain::normalized(padded_min, padded_max)
    }

    fn zoom_domain(
        current: Domain,
        base: Domain,
        anchor: f64,
        factor: f64,
        minimum: f64,
        scale: AxisScale,
    ) -> Domain {
        let scaled = |domain: Domain| Domain {
            min: Self::transform(domain.min, scale).unwrap_or(domain.min),
            max: Self::transform(domain.max, scale).unwrap_or(domain.max),
        };
        let current = scaled(current);
        let base = scaled(base);
        let anchor = Self::transform(anchor, scale).unwrap_or(anchor);
        let min_span = (base.max - base.min) * minimum.clamp(0.000_001, 1.0);
        let span =
            ((current.max - current.min) * factor).clamp(min_span, (base.max - base.min) * 128.0);
        let ratio = ((anchor - current.min) / (current.max - current.min)).clamp(0.0, 1.0);
        Domain {
            min: Self::inverse(anchor - span * ratio, scale),
            max: Self::inverse(anchor + span * (1.0 - ratio), scale),
        }
    }

    fn zoom_at_pointer(&mut self, delta: f64) {
        let plot = self.plot_rect();
        if !plot.contains(self.pointer) || self.style.zoom == CartesianZoom::Disabled {
            return;
        }
        let factor = (-delta * 0.12).exp().clamp(0.5, 2.0);
        let base_x = self.base_domain(&self.style.x_axis, true);
        let current_x = self.x_view.unwrap_or(base_x);
        let x_anchor = Self::unproject(
            self.pointer.x,
            current_x,
            self.style.x_axis.scale,
            plot.x0,
            plot.width(),
        );
        self.x_view = Some(Self::zoom_domain(
            current_x,
            base_x,
            x_anchor,
            factor,
            self.style.min_zoom_fraction,
            self.style.x_axis.scale,
        ));
        if self.style.zoom == CartesianZoom::XY {
            let base_y = self.base_domain(&self.style.y_axis, false);
            let current_y = self.y_view.unwrap_or(base_y);
            let y_anchor = Self::unproject(
                self.pointer.y,
                current_y,
                self.style.y_axis.scale,
                plot.y1,
                -plot.height(),
            );
            self.y_view = Some(Self::zoom_domain(
                current_y,
                base_y,
                y_anchor,
                factor,
                self.style.min_zoom_fraction,
                self.style.y_axis.scale,
            ));
        }
        self.view_changed = true;
        self.update_hover();
    }

    fn map(
        plot: Rect,
        x_domain: Domain,
        y_domain: Domain,
        x_scale: AxisScale,
        y_scale: AxisScale,
        datum: &PlotDatum,
    ) -> Option<Point> {
        Self::map_xy(plot, x_domain, y_domain, x_scale, y_scale, datum.x, datum.y)
    }

    fn map_xy(
        plot: Rect,
        x_domain: Domain,
        y_domain: Domain,
        x_scale: AxisScale,
        y_scale: AxisScale,
        x_value: f64,
        y_value: f64,
    ) -> Option<Point> {
        let x = Self::project(x_value, x_domain, x_scale, plot.x0, plot.width())?;
        let y = Self::project(y_value, y_domain, y_scale, plot.y1, -plot.height())?;
        Some(Point::new(x, y))
    }

    fn map_datum(
        &self,
        plot: Rect,
        x_domain: Domain,
        y_domain: Domain,
        datum: &PlotDatum,
    ) -> Option<Point> {
        Self::map(
            plot,
            x_domain,
            y_domain,
            self.style.x_axis.scale,
            self.style.y_axis.scale,
            datum,
        )
    }

    fn map_animated_datum(
        &self,
        series: usize,
        datum: usize,
        plot: Rect,
        x_domain: Domain,
        y_domain: Domain,
        now: Instant,
    ) -> Option<Point> {
        let (x, y) = self.animated_xy(series, datum, now)?;
        Self::map_xy(
            plot,
            x_domain,
            y_domain,
            self.style.x_axis.scale,
            self.style.y_axis.scale,
            x,
            y,
        )
    }

    fn unproject(value: f64, domain: Domain, scale: AxisScale, origin: f64, span: f64) -> f64 {
        let min = Self::transform(domain.min, scale).unwrap_or(domain.min);
        let max = Self::transform(domain.max, scale).unwrap_or(domain.max);
        Self::inverse(min + (value - origin) / span * (max - min), scale)
    }

    fn brush_rect(&self) -> Option<Rect> {
        let start = self.brush_start?;
        let plot = self.plot_rect();
        let current = self.brush_current;
        let x0 = start.x.min(current.x).clamp(plot.x0, plot.x1);
        let x1 = start.x.max(current.x).clamp(plot.x0, plot.x1);
        let (y0, y1) = if self.style.brush == CartesianBrush::X {
            (plot.y0, plot.y1)
        } else {
            (
                start.y.min(current.y).clamp(plot.y0, plot.y1),
                start.y.max(current.y).clamp(plot.y0, plot.y1),
            )
        };
        Some(Rect::new(x0, y0, x1, y1))
    }

    fn brush_selection_from_rect(
        rect: Rect,
        plot: Rect,
        x_domain: Domain,
        y_domain: Domain,
        x_scale: AxisScale,
        y_scale: AxisScale,
        mode: CartesianBrush,
    ) -> Option<CartesianBrushSelection> {
        if rect.width() < 4.0 || (mode == CartesianBrush::XY && rect.height() < 4.0) {
            return None;
        }
        let x = AxisRange::Fixed {
            min: Self::unproject(rect.x0, x_domain, x_scale, plot.x0, plot.width()),
            max: Self::unproject(rect.x1, x_domain, x_scale, plot.x0, plot.width()),
        };
        let y = (mode == CartesianBrush::XY).then(|| AxisRange::Fixed {
            min: Self::unproject(rect.y1, y_domain, y_scale, plot.y1, -plot.height()),
            max: Self::unproject(rect.y0, y_domain, y_scale, plot.y1, -plot.height()),
        });
        Some(CartesianBrushSelection { x, y })
    }

    fn finish_brush(&mut self) {
        let Some(rect) = self.brush_rect() else {
            return;
        };
        self.brush_start = None;
        let plot = self.plot_rect();
        let xd = self.domain(&self.style.x_axis, true);
        let yd = self.domain(&self.style.y_axis, false);
        self.brushed = Self::brush_selection_from_rect(
            rect,
            plot,
            xd,
            yd,
            self.style.x_axis.scale,
            self.style.y_axis.scale,
            self.style.brush,
        );
    }

    fn ticks(axis: &PlotAxis, domain: Domain, span: f64) -> Vec<AxisTick> {
        if !axis.ticks.is_empty() {
            let available = crate::num::usize_of_f64((span / axis.min_tick_spacing).floor()).max(2);
            if axis.ticks.len() <= available {
                return axis.ticks.clone();
            }
            let last = axis.ticks.len().saturating_sub(1);
            return (0..available)
                .filter_map(|index| {
                    let position = index
                        .saturating_mul(last)
                        .checked_div(available.saturating_sub(1))?;
                    axis.ticks.get(position).cloned()
                })
                .collect();
        }
        let responsive_count =
            crate::num::u8_of_f64((span / axis.min_tick_spacing).floor()).saturating_add(1);
        let count = axis.tick_count.clamp(2, 20).min(responsive_count.max(2));
        if axis.scale == AxisScale::Log10 {
            let first = crate::num::i32_of_f64(domain.min.log10().ceil());
            let last = crate::num::i32_of_f64(domain.max.log10().floor());
            let decades = if last >= first {
                (first..=last)
                    .map(|power| 10.0_f64.powi(power))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            if decades.len() >= 2 {
                let available = usize::from(count);
                let sample_count = available.min(decades.len());
                let last = decades.len().saturating_sub(1);
                return (0..sample_count)
                    .filter_map(|index| {
                        let position = index
                            .saturating_mul(last)
                            .checked_div(sample_count.saturating_sub(1))?;
                        let value = *decades.get(position)?;
                        Some(AxisTick::new(
                            value,
                            format_axis_value(value, axis.label_format),
                        ))
                    })
                    .collect();
            }
        }
        if axis.scale == AxisScale::Linear {
            // Round numbers inside the domain, not the domain cut in equal
            // parts: an axis reads 0, 10, 20, never 0, 11.5, 23.
            let values = crate::charts::nice_ticks(domain.min, domain.max, usize::from(count));
            if values.len() >= 2 {
                return values
                    .into_iter()
                    .map(|value| AxisTick::new(value, format_axis_value(value, axis.label_format)))
                    .collect();
            }
        }
        (0..count)
            .map(|index| {
                let fraction = f64::from(index) / f64::from(count - 1);
                let min = Self::transform(domain.min, axis.scale).unwrap_or(domain.min);
                let max = Self::transform(domain.max, axis.scale).unwrap_or(domain.max);
                let value = Self::inverse(min + (max - min) * fraction, axis.scale);
                let label = format_axis_value(value, axis.label_format);
                AxisTick::new(value, label)
            })
            .collect()
    }

    fn update_hover(&mut self) {
        let plot = self.plot_rect();
        if !plot
            .inflate(self.style.hit_radius, self.style.hit_radius)
            .contains(self.pointer)
        {
            self.hovered = None;
            self.hovered_annotation = None;
            return;
        }
        let xd = self.domain(&self.style.x_axis, true);
        let yd = self.domain(&self.style.y_axis, false);
        let now = crate::time::now();
        let mut nearest: Option<(f64, Hovered)> = None;
        for (series_index, series) in self.series.iter().enumerate() {
            if self.hidden_series.contains(&series.id) {
                continue;
            }
            for datum_index in self.hover_range(series, plot, xd) {
                let Some(point) =
                    self.map_animated_datum(series_index, datum_index, plot, xd, yd, now)
                else {
                    continue;
                };
                let distance = point.distance(self.pointer);
                if distance <= self.style.hit_radius
                    && nearest.is_none_or(|current| distance < current.0)
                {
                    nearest = Some((
                        distance,
                        Hovered {
                            series: series_index,
                            datum: datum_index,
                            point,
                        },
                    ));
                }
            }
        }
        self.hovered = nearest.map(|(_, hovered)| hovered);
        self.hovered_annotation = self.annotation_at(self.pointer, plot, xd, yd);
    }

    fn hover_range(
        &self,
        series: &PlotSeries,
        plot: Rect,
        x_domain: Domain,
    ) -> std::ops::Range<usize> {
        let count = series.data.len();
        if !series.x_sorted || count < 32 || self.is_animating() || plot.width() <= 0.0 {
            return 0..count;
        }
        let a = Self::unproject(
            self.pointer.x - self.style.hit_radius,
            x_domain,
            self.style.x_axis.scale,
            plot.x0,
            plot.width(),
        );
        let b = Self::unproject(
            self.pointer.x + self.style.hit_radius,
            x_domain,
            self.style.x_axis.scale,
            plot.x0,
            plot.width(),
        );
        let low = a.min(b);
        let high = a.max(b);
        let start = series
            .data
            .partition_point(|datum| datum.x < low)
            .saturating_sub(1);
        let end = series
            .data
            .partition_point(|datum| datum.x <= high)
            .saturating_add(1)
            .min(count);
        start..end.max(start)
    }

    fn annotation_at(&self, pointer: Point, plot: Rect, xd: Domain, yd: Domain) -> Option<usize> {
        if !plot.contains(pointer) {
            return None;
        }
        self.annotations
            .iter()
            .enumerate()
            .filter(|(_, annotation)| annotation.interactive)
            .filter_map(|(index, annotation)| {
                let distance = match annotation.kind {
                    CartesianAnnotationKind::XLine(value) => {
                        Self::project(value, xd, self.style.x_axis.scale, plot.x0, plot.width())
                            .map(|x| (pointer.x - x).abs())?
                    }
                    CartesianAnnotationKind::YLine(value) => {
                        Self::project(value, yd, self.style.y_axis.scale, plot.y1, -plot.height())
                            .map(|y| (pointer.y - y).abs())?
                    }
                    CartesianAnnotationKind::XBand { start, end } => {
                        let a = Self::project(
                            start,
                            xd,
                            self.style.x_axis.scale,
                            plot.x0,
                            plot.width(),
                        )?;
                        let b =
                            Self::project(end, xd, self.style.x_axis.scale, plot.x0, plot.width())?;
                        if pointer.x >= a.min(b) && pointer.x <= a.max(b) {
                            5.0
                        } else {
                            f64::INFINITY
                        }
                    }
                    CartesianAnnotationKind::YBand { start, end } => {
                        let a = Self::project(
                            start,
                            yd,
                            self.style.y_axis.scale,
                            plot.y1,
                            -plot.height(),
                        )?;
                        let b = Self::project(
                            end,
                            yd,
                            self.style.y_axis.scale,
                            plot.y1,
                            -plot.height(),
                        )?;
                        if pointer.y >= a.min(b) && pointer.y <= a.max(b) {
                            5.0
                        } else {
                            f64::INFINITY
                        }
                    }
                    CartesianAnnotationKind::Point { x, y } => {
                        let x =
                            Self::project(x, xd, self.style.x_axis.scale, plot.x0, plot.width())?;
                        let y =
                            Self::project(y, yd, self.style.y_axis.scale, plot.y1, -plot.height())?;
                        pointer.distance(Point::new(x, y))
                    }
                };
                (distance <= 10.0).then_some((index, distance))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, _)| index)
    }

    fn annotation_anchor(&self, index: usize, plot: Rect, xd: Domain, yd: Domain) -> Option<Point> {
        let annotation = self.annotations.get(index)?;
        match annotation.kind {
            CartesianAnnotationKind::XLine(value) => {
                Self::project(value, xd, self.style.x_axis.scale, plot.x0, plot.width())
                    .map(|x| Point::new(x, plot.y0.midpoint(plot.y1)))
            }
            CartesianAnnotationKind::YLine(value) => {
                Self::project(value, yd, self.style.y_axis.scale, plot.y1, -plot.height())
                    .map(|y| Point::new(plot.x0.midpoint(plot.x1), y))
            }
            CartesianAnnotationKind::XBand { start, end } => {
                let a = Self::project(start, xd, self.style.x_axis.scale, plot.x0, plot.width())?;
                let b = Self::project(end, xd, self.style.x_axis.scale, plot.x0, plot.width())?;
                Some(Point::new(a.midpoint(b), plot.y0.midpoint(plot.y1)))
            }
            CartesianAnnotationKind::YBand { start, end } => {
                let a = Self::project(start, yd, self.style.y_axis.scale, plot.y1, -plot.height())?;
                let b = Self::project(end, yd, self.style.y_axis.scale, plot.y1, -plot.height())?;
                Some(Point::new(plot.x0.midpoint(plot.x1), a.midpoint(b)))
            }
            CartesianAnnotationKind::Point { x, y } => Some(Point::new(
                Self::project(x, xd, self.style.x_axis.scale, plot.x0, plot.width())?,
                Self::project(y, yd, self.style.y_axis.scale, plot.y1, -plot.height())?,
            )),
        }
        .filter(|point| plot.contains(*point))
    }

    fn draw_axes(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        plot: Rect,
        xd: Domain,
        yd: Domain,
    ) {
        let grid = with_alpha(theme.tokens.colors.border_standard, 100);
        let size = theme.text(TextRole::Caption);
        for tick in Self::ticks(&self.style.y_axis, yd, plot.height()) {
            let Some(y) = Self::project(
                tick.value,
                yd,
                self.style.y_axis.scale,
                plot.y1,
                -plot.height(),
            ) else {
                continue;
            };
            if self.style.y_axis.grid {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    grid,
                    None,
                    &Line::new((plot.x0, y), (plot.x1, y)),
                );
            }
            let (width, height) = tr.measure_style(&tick.label, None, &size);
            tr.draw_style(
                scene,
                &tick.label,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        plot.x0 - f64::from(width) - self.bands.gap,
                        y - f64::from(height) / 2.0,
                    )),
                None,
                &size,
            );
        }
        for tick in Self::ticks(&self.style.x_axis, xd, plot.width()) {
            let Some(x) = Self::project(
                tick.value,
                xd,
                self.style.x_axis.scale,
                plot.x0,
                plot.width(),
            ) else {
                continue;
            };
            if self.style.x_axis.grid {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    grid,
                    None,
                    &Line::new((x, plot.y0), (x, plot.y1)),
                );
            }
            let (width, _) = tr.measure_style(&tick.label, None, &size);
            tr.draw_style(
                scene,
                &tick.label,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((x - f64::from(width) / 2.0, plot.y1 + self.bands.gap)),
                None,
                &size,
            );
        }
        let title_style = theme.text(TextRole::Caption);
        if !self.style.x_axis.title.is_empty() {
            let title = Self::fitted_axis_title(&self.style.x_axis.title, plot.width());
            let (width, _) = tr.measure_style(&title, None, &title_style);
            tr.draw_style(
                scene,
                &title,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        plot.center().x - f64::from(width) / 2.0,
                        plot.y1 + self.bands.gap + self.bands.tick_line + self.bands.gap / 2.0,
                    )),
                None,
                &title_style.clone().with_weight(TextWeight::Semibold),
            );
        }
        if !self.style.y_axis.title.is_empty() {
            let title = Self::fitted_axis_title(&self.style.y_axis.title, plot.height());
            let (width, _) = tr.measure_style(&title, None, &title_style);
            tr.draw_style(
                scene,
                &title,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((self.rect.x0, plot.center().y + f64::from(width) / 2.0))
                    * Affine::rotate(-std::f64::consts::FRAC_PI_2),
                None,
                &title_style.clone().with_weight(TextWeight::Semibold),
            );
        }
    }

    fn fitted_axis_title(title: &str, available: f64) -> String {
        let maximum = crate::num::usize_of_f64((available.max(14.0) / 7.0).floor()).max(2);
        if title.chars().count() <= maximum {
            return title.to_owned();
        }
        let mut fitted = title
            .chars()
            .take(maximum.saturating_sub(1))
            .collect::<String>();
        fitted.push('…');
        fitted
    }

    fn series_path(points: &[Point], kind: SeriesKind) -> BezPath {
        let mut path = BezPath::new();
        let Some(first) = points.first() else {
            return path;
        };
        path.move_to(*first);
        for pair in points.windows(2) {
            let Some(previous) = pair.first() else {
                continue;
            };
            let Some(next) = pair.get(1) else {
                continue;
            };
            match kind {
                SeriesKind::Step | SeriesKind::StepArea => {
                    path.line_to((next.x, previous.y));
                    path.line_to(*next);
                }
                SeriesKind::Smooth | SeriesKind::SmoothArea => {
                    let midpoint = previous.x.midpoint(next.x);
                    path.curve_to((midpoint, previous.y), (midpoint, next.y), (next.x, next.y));
                }
                _ => path.line_to(*next),
            }
        }
        path
    }

    fn series_stroke(series: &PlotSeries) -> Stroke {
        let stroke = Stroke::new(series.stroke_width);
        if series.line_dash.is_empty() {
            stroke
        } else {
            stroke.with_dashes(0.0, series.line_dash.iter().copied())
        }
    }

    fn draw_marker(
        scene: &mut Scene,
        transform: Affine,
        color: Color,
        point: Point,
        radius: f64,
        shape: MarkerShape,
    ) {
        match shape {
            MarkerShape::Circle => scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &Circle::new(point, radius),
            ),
            MarkerShape::Square => scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(
                    Rect::new(
                        point.x - radius,
                        point.y - radius,
                        point.x + radius,
                        point.y + radius,
                    ),
                    (radius * 0.3).min(3.0),
                ),
            ),
            MarkerShape::Diamond => {
                let mut path = BezPath::new();
                path.move_to((point.x, point.y - radius));
                path.line_to((point.x + radius, point.y));
                path.line_to((point.x, point.y + radius));
                path.line_to((point.x - radius, point.y));
                path.close_path();
                scene.fill(Fill::NonZero, transform, color, None, &path);
            }
        }
    }

    fn point_segments(
        points: impl IntoIterator<Item = Option<Point>>,
        connect: bool,
    ) -> Vec<Vec<Point>> {
        let mut segments = Vec::new();
        let mut segment = Vec::new();
        for point in points {
            if let Some(point) = point {
                segment.push(point);
            } else if !connect && !segment.is_empty() {
                segments.push(std::mem::take(&mut segment));
            }
        }
        if !segment.is_empty() {
            segments.push(segment);
        }
        segments
    }

    fn y_baseline(&self, plot: Rect, domain: Domain) -> f64 {
        if self.style.y_axis.scale == AxisScale::Linear && domain.min <= 0.0 && domain.max >= 0.0 {
            Self::project(0.0, domain, AxisScale::Linear, plot.y1, -plot.height())
                .unwrap_or(plot.y1)
        } else {
            plot.y1
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all series kinds share projection, selection and ordering in one paint pass"
    )]
    fn draw_series(
        &self,
        scene: &mut Scene,
        transform: Affine,
        plot: Rect,
        xd: Domain,
        yd: Domain,
        now: Instant,
    ) {
        for (series_index, series) in self.series.iter().enumerate() {
            if self.hidden_series.contains(&series.id) {
                continue;
            }
            let projected = self
                .render_indices(series, plot)
                .into_iter()
                .filter_map(|index| {
                    let datum = series.data.get(index)?;
                    Some((
                        index,
                        datum,
                        self.map_animated_datum(series_index, index, plot, xd, yd, now),
                    ))
                })
                .collect::<Vec<_>>();
            let points = projected
                .iter()
                .filter_map(|(_, _, point)| *point)
                .collect::<Vec<_>>();
            let segments = Self::point_segments(
                projected.iter().map(|(_, _, point)| *point),
                series.connect_gaps,
            );
            match series.kind {
                SeriesKind::Line | SeriesKind::Smooth | SeriesKind::Step => {
                    for segment in &segments {
                        scene.stroke(
                            &Self::series_stroke(series),
                            transform,
                            series.color,
                            None,
                            &Self::series_path(segment, series.kind),
                        );
                    }
                }
                SeriesKind::Area | SeriesKind::SmoothArea | SeriesKind::StepArea => {
                    for segment in &segments {
                        let (Some(first), Some(last)) = (segment.first(), segment.last()) else {
                            continue;
                        };
                        let baseline = self.y_baseline(plot, yd);
                        let path_kind = match series.kind {
                            SeriesKind::SmoothArea => SeriesKind::Smooth,
                            SeriesKind::StepArea => SeriesKind::Step,
                            _ => SeriesKind::Line,
                        };
                        let mut area = Self::series_path(segment, path_kind);
                        area.line_to((last.x, baseline));
                        area.line_to((first.x, baseline));
                        area.close_path();
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            with_alpha(series.color, series.fill_alpha),
                            None,
                            &area,
                        );
                        scene.stroke(
                            &Self::series_stroke(series),
                            transform,
                            series.color,
                            None,
                            &Self::series_path(segment, path_kind),
                        );
                    }
                }
                SeriesKind::Bar { width } => {
                    let zero = self.y_baseline(plot, yd);
                    for point in &points {
                        let rect = Rect::new(
                            point.x - width / 2.0,
                            point.y.min(zero),
                            point.x + width / 2.0,
                            point.y.max(zero),
                        );
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            series.color,
                            None,
                            &RoundedRect::from_rect(rect, series.bar_radius),
                        );
                    }
                }
                SeriesKind::Lollipop => {
                    let zero = self.y_baseline(plot, yd);
                    for point in &points {
                        scene.stroke(
                            &Stroke::new(series.stroke_width),
                            transform,
                            with_alpha(series.color, 180),
                            None,
                            &Line::new((point.x, zero), *point),
                        );
                        Self::draw_marker(
                            scene,
                            transform,
                            series.color,
                            *point,
                            series.marker_radius,
                            series.marker_shape,
                        );
                    }
                }
                SeriesKind::Scatter | SeriesKind::Bubble => {}
            }
            let markers = series.show_markers.unwrap_or(
                matches!(series.kind, SeriesKind::Scatter | SeriesKind::Bubble)
                    || self.style.rendering.show_markers_on_lines,
            );
            if markers {
                for &(datum_index, datum, point) in &projected {
                    let Some(point) = point else {
                        continue;
                    };
                    let radius = if series.kind == SeriesKind::Bubble {
                        datum.radius
                    } else {
                        series.marker_radius
                    };
                    let selected = self.hovered.is_some_and(|hovered| {
                        hovered.series == series_index && hovered.datum == datum_index
                    });
                    if selected {
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            with_alpha(series.color, 55),
                            None,
                            &Circle::new(point, radius + 6.0),
                        );
                    }
                    Self::draw_marker(
                        scene,
                        transform,
                        series.color,
                        point,
                        radius,
                        series.marker_shape,
                    );
                }
            }
        }
    }

    fn render_indices(&self, series: &PlotSeries, plot: Rect) -> Vec<usize> {
        let connected = matches!(
            series.kind,
            SeriesKind::Line
                | SeriesKind::Smooth
                | SeriesKind::Step
                | SeriesKind::Area
                | SeriesKind::SmoothArea
                | SeriesKind::StepArea
        );
        let count = series.data.len();
        let points_per_pixel = self.style.rendering.max_points_per_pixel.clamp(0.25, 16.0);
        let target = crate::num::usize_of_f64((plot.width().max(1.0) * points_per_pixel).ceil());
        let target = target.max(16);
        if !self.style.rendering.adaptive_sampling || !connected || count <= target || count <= 2 {
            return (0..count).collect();
        }

        // Two extrema per contiguous bucket preserve narrow peaks and troughs
        // that stride-based sampling would silently erase.
        let buckets = target.saturating_sub(2).checked_div(2).unwrap_or(1).max(1);
        let interior = count - 2;
        let bucket_size = interior.div_ceil(buckets);
        let mut indices = Vec::with_capacity(target);
        indices.push(0);
        for start in (1..count - 1).step_by(bucket_size) {
            let end = (start + bucket_size).min(count - 1);
            let mut min = start;
            let mut max = start;
            let Some(bucket) = series.data.get(start..end) else {
                continue;
            };
            let mut min_y = bucket.first().map_or(0.0, |datum| datum.y);
            let mut max_y = min_y;
            for (offset, datum) in bucket.iter().enumerate().skip(1) {
                let index = start.saturating_add(offset);
                if datum.y < min_y {
                    min = index;
                    min_y = datum.y;
                }
                if datum.y > max_y {
                    max = index;
                    max_y = datum.y;
                }
            }
            if min <= max {
                indices.push(min);
                if max != min {
                    indices.push(max);
                }
            } else {
                indices.push(max);
                indices.push(min);
            }
        }
        indices.push(count - 1);
        if !series.connect_gaps {
            indices.extend(
                series
                    .data
                    .iter()
                    .enumerate()
                    .filter(|(_, datum)| {
                        Self::transform(datum.x, self.style.x_axis.scale).is_none()
                            || Self::transform(datum.y, self.style.y_axis.scale).is_none()
                    })
                    .map(|(index, _)| index),
            );
            indices.sort_unstable();
            indices.dedup();
        }
        indices
    }

    fn annotation_stroke(annotation: &CartesianAnnotation, hovered: bool) -> Stroke {
        let stroke = Stroke::new(annotation.stroke_width + if hovered { 1.5 } else { 0.0 });
        if annotation.dashed {
            stroke.with_dashes(0.0, [6.0, 5.0])
        } else {
            stroke
        }
    }

    fn draw_annotations(&self, context: &mut AnnotationPaintContext<'_>) {
        for (index, annotation) in self.annotations.iter().enumerate() {
            let hovered = self.hovered_annotation == Some(index);
            let anchor = self.draw_annotation_shape(context, annotation, hovered);
            Self::draw_annotation_label(context, annotation, anchor);
        }
    }

    fn draw_annotation_shape(
        &self,
        context: &mut AnnotationPaintContext<'_>,
        annotation: &CartesianAnnotation,
        hovered: bool,
    ) -> Option<Point> {
        match annotation.kind {
            CartesianAnnotationKind::XLine(value) => {
                self.draw_annotation_line(context, annotation, hovered, value, true)
            }
            CartesianAnnotationKind::YLine(value) => {
                self.draw_annotation_line(context, annotation, hovered, value, false)
            }
            CartesianAnnotationKind::XBand { start, end } => {
                self.draw_annotation_band(context, annotation, hovered, (start, end), true)
            }
            CartesianAnnotationKind::YBand { start, end } => {
                self.draw_annotation_band(context, annotation, hovered, (start, end), false)
            }
            CartesianAnnotationKind::Point { x, y } => {
                self.draw_annotation_point(context, annotation, hovered, x, y)
            }
        }
    }

    fn draw_annotation_line(
        &self,
        context: &mut AnnotationPaintContext<'_>,
        annotation: &CartesianAnnotation,
        hovered: bool,
        value: f64,
        vertical: bool,
    ) -> Option<Point> {
        let plot = context.plot;
        let (domain, scale, origin, extent) = if vertical {
            (
                context.x_domain,
                self.style.x_axis.scale,
                plot.x0,
                plot.width(),
            )
        } else {
            (
                context.y_domain,
                self.style.y_axis.scale,
                plot.y1,
                -plot.height(),
            )
        };
        let coordinate = Self::project(value, domain, scale, origin, extent)?;
        let within_plot = if vertical {
            coordinate >= plot.x0 && coordinate <= plot.x1
        } else {
            coordinate >= plot.y0 && coordinate <= plot.y1
        };
        if !within_plot {
            return None;
        }
        let line = if vertical {
            Line::new((coordinate, plot.y0), (coordinate, plot.y1))
        } else {
            Line::new((plot.x0, coordinate), (plot.x1, coordinate))
        };
        context.scene.stroke(
            &Self::annotation_stroke(annotation, hovered),
            context.transform,
            annotation.color,
            None,
            &line,
        );
        Some(if vertical {
            Point::new(coordinate + 6.0, plot.y0 + 6.0)
        } else {
            Point::new(plot.x0 + 6.0, coordinate - 26.0)
        })
    }

    fn draw_annotation_band(
        &self,
        context: &mut AnnotationPaintContext<'_>,
        annotation: &CartesianAnnotation,
        hovered: bool,
        range: (f64, f64),
        vertical: bool,
    ) -> Option<Point> {
        let plot = context.plot;
        let (domain, scale, origin, extent, lower, upper) = if vertical {
            (
                context.x_domain,
                self.style.x_axis.scale,
                plot.x0,
                plot.width(),
                plot.x0,
                plot.x1,
            )
        } else {
            (
                context.y_domain,
                self.style.y_axis.scale,
                plot.y1,
                -plot.height(),
                plot.y0,
                plot.y1,
            )
        };
        let a = Self::project(range.0, domain, scale, origin, extent)?;
        let b = Self::project(range.1, domain, scale, origin, extent)?;
        let start = a.min(b).clamp(lower, upper);
        let end = a.max(b).clamp(lower, upper);
        if end <= start {
            return None;
        }
        let rect = if vertical {
            Rect::new(start, plot.y0, end, plot.y1)
        } else {
            Rect::new(plot.x0, start, plot.x1, end)
        };
        let fill_alpha = annotation
            .fill_alpha
            .saturating_add(if hovered { 28 } else { 0 });
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            with_alpha(annotation.color, fill_alpha),
            None,
            &rect,
        );
        Some(if vertical {
            Point::new(start + 6.0, plot.y0 + 6.0)
        } else {
            Point::new(plot.x0 + 6.0, start + 6.0)
        })
    }

    fn draw_annotation_point(
        &self,
        context: &mut AnnotationPaintContext<'_>,
        annotation: &CartesianAnnotation,
        hovered: bool,
        x: f64,
        y: f64,
    ) -> Option<Point> {
        let plot = context.plot;
        let x = Self::project(
            x,
            context.x_domain,
            self.style.x_axis.scale,
            plot.x0,
            plot.width(),
        )?;
        let y = Self::project(
            y,
            context.y_domain,
            self.style.y_axis.scale,
            plot.y1,
            -plot.height(),
        )?;
        if !plot.contains(Point::new(x, y)) {
            return None;
        }
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            with_alpha(annotation.color, 55),
            None,
            &Circle::new((x, y), if hovered { 12.0 } else { 9.0 }),
        );
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            annotation.color,
            None,
            &Circle::new((x, y), 4.0),
        );
        Some(Point::new(x + 8.0, y - 28.0))
    }

    fn draw_annotation_label(
        context: &mut AnnotationPaintContext<'_>,
        annotation: &CartesianAnnotation,
        anchor: Option<Point>,
    ) {
        let Some(anchor) = anchor.filter(|_| !annotation.label.is_empty()) else {
            return;
        };
        let style = context.theme.text(TextRole::Caption);
        let (width, height) = context.text.measure_style(&annotation.label, None, &style);
        let width = f64::from(width) + 16.0;
        let height = f64::from(height) + 10.0;
        let plot = context.plot;
        let x = anchor
            .x
            .clamp(plot.x0 + 4.0, (plot.x1 - width - 4.0).max(plot.x0 + 4.0));
        let y = anchor
            .y
            .clamp(plot.y0 + 4.0, (plot.y1 - height - 4.0).max(plot.y0 + 4.0));
        let rect = RoundedRect::from_rect(Rect::new(x, y, x + width, y + height), 5.0);
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            context.theme.tokens.colors.surfaces.overlay,
            None,
            &rect,
        );
        context.scene.stroke(
            &Stroke::new(1.0),
            context.transform,
            with_alpha(annotation.color, 160),
            None,
            &rect,
        );
        context.text.draw_style(
            context.scene,
            &annotation.label,
            context.theme.tokens.colors.text_primary,
            context.transform * Affine::translate((x + 8.0, y + 5.0)),
            None,
            &style,
        );
    }

    fn draw_legend(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.legend_hits.clear();
        if self.style.legend == LegendPosition::Hidden {
            return;
        }
        // The key stands on the content edge, like the labels beside it.
        let left = self.rect.x0;
        let right = self.rect.x1 - self.bands.right;
        let y = if self.style.legend == LegendPosition::Top {
            self.rect.y0 + self.bands.legend_top
        } else {
            let rows = chart_legend::rows(
                self.series.iter().map(|series| series.name.as_str()),
                left,
                right,
            );
            self.rect.y1 - crate::num::f64_of_usize(rows) * chart_legend::ROW_HEIGHT
        };
        let items = chart_legend::layout(
            self.series.iter().map(|series| series.name.as_str()),
            left,
            right,
            y,
        );
        for (series, item) in self.series.iter().zip(items) {
            let x = item.x0;
            let y = item.y0;
            let hidden = self.hidden_series.contains(&series.id);
            let color = if hidden {
                with_alpha(series.color, 70)
            } else {
                series.color
            };
            let text_color = if hidden {
                with_alpha(theme.tokens.colors.text_dim, 100)
            } else {
                theme.tokens.colors.text_dim
            };
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &Circle::new((x + 4.0, y + 6.0), 4.0),
            );
            tr.draw_style(
                scene,
                &series.name,
                text_color,
                transform * Affine::translate((x + 14.0, y)),
                Some(crate::num::f32_of_f64((item.width() - 18.0).max(1.0))),
                &theme.text(TextRole::Caption),
            );
            self.legend_hits.push((series.id.clone(), item));
        }
    }

    fn draw_tooltip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(hovered) = self.hovered.filter(|_| self.style.tooltip.enabled) else {
            return;
        };
        let Some(series) = self.series.get(hovered.series) else {
            return;
        };
        let Some(datum) = series.data.get(hovered.datum) else {
            return;
        };
        if self.style.rendering.show_crosshair {
            let plot = self.plot_rect();
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                with_alpha(theme.tokens.colors.text_dim, 110),
                None,
                &Line::new((hovered.point.x, plot.y0), (hovered.point.x, plot.y1)),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                with_alpha(theme.tokens.colors.text_dim, 110),
                None,
                &Line::new((plot.x0, hovered.point.y), (plot.x1, hovered.point.y)),
            );
        }
        let title = if datum.label.is_empty() {
            &series.name
        } else {
            &datum.label
        };
        let coordinates = format!(
            "x {}  ·  y {}",
            format_axis_value(datum.x, self.style.x_axis.label_format),
            format_axis_value(datum.y, self.style.y_axis.label_format)
        );
        let detail = if datum.detail.is_empty() {
            coordinates
        } else if self.style.tooltip.show_coordinates {
            format!("{}  ·  {coordinates}", datum.detail)
        } else {
            datum.detail.clone()
        };
        let (title_width, _) = tr.measure_style(title, None, &theme.text(TextRole::Caption));
        let (detail_width, _) = tr.measure_style(&detail, None, &theme.text(TextRole::Caption));
        let padding = self.style.tooltip.padding.clamp(8.0, 32.0);
        let content_width = f64::from(title_width.max(detail_width));
        let available_width = (self.rect.width() - 16.0).max(1.0);
        let width = self
            .style
            .tooltip
            .width
            .unwrap_or_else(|| content_width.max(self.style.tooltip.min_width) + padding * 2.0)
            .clamp(80.0_f64.min(available_width), available_width);
        let anchor = match self.style.tooltip.anchor {
            CartesianTooltipAnchor::Pointer if self.rect.contains(self.pointer) => self.pointer,
            CartesianTooltipAnchor::Pointer | CartesianTooltipAnchor::Datum => hovered.point,
        };
        let height = 38.0 + padding * 2.0;
        let rect = Self::tooltip_rect(
            anchor,
            (width, height),
            self.rect,
            self.style.tooltip.offset.max(0.0),
        );
        let x = rect.x0;
        let y = rect.y0;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
        );
        tr.draw_style(
            scene,
            title,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x + padding, y + padding - 2.0)),
            Some(crate::num::f32_of_f64((width - padding * 2.0).max(1.0))),
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
        );
        tr.draw_style(
            scene,
            &detail,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((x + padding, y + padding + 24.0)),
            Some(crate::num::f32_of_f64((width - padding * 2.0).max(1.0))),
            &theme.text(TextRole::Caption),
        );
    }

    fn draw_annotation_tooltip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.hovered.is_some() || !self.style.tooltip.enabled {
            return;
        }
        let Some(annotation) = self
            .hovered_annotation
            .and_then(|index| self.annotations.get(index))
        else {
            return;
        };
        let title = if annotation.label.is_empty() {
            annotation.id.as_str()
        } else {
            annotation.label.as_str()
        };
        let detail = if annotation.detail.is_empty() {
            "Chart annotation"
        } else {
            annotation.detail.as_str()
        };
        let (title_width, _) = tr.measure_style(title, None, &theme.text(TextRole::Caption));
        let (detail_width, _) = tr.measure_style(detail, None, &theme.text(TextRole::Caption));
        let padding = self.style.tooltip.padding.clamp(8.0, 32.0);
        let available_width = (self.rect.width() - 16.0).max(1.0);
        let width = self
            .style
            .tooltip
            .width
            .unwrap_or_else(|| {
                f64::from(title_width.max(detail_width)).max(self.style.tooltip.min_width)
                    + padding * 2.0
            })
            .clamp(80.0_f64.min(available_width), available_width);
        let height = 38.0 + padding * 2.0;
        let rect = Self::tooltip_rect(
            self.pointer,
            (width, height),
            self.rect,
            self.style.tooltip.offset.max(4.0),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            with_alpha(annotation.color, 180),
            None,
            &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
        );
        tr.draw_style(
            scene,
            title,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((rect.x0 + padding, rect.y0 + padding - 2.0)),
            Some(crate::num::f32_of_f64((width - padding * 2.0).max(1.0))),
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
        );
        tr.draw_style(
            scene,
            detail,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((rect.x0 + padding, rect.y0 + padding + 24.0)),
            Some(crate::num::f32_of_f64((width - padding * 2.0).max(1.0))),
            &theme.text(TextRole::Caption),
        );
    }

    fn activate_inspected(&mut self) -> bool {
        if let Some(hovered) = self.hovered
            && let (Some(series), Some(datum)) = (
                self.series.get(hovered.series),
                self.series
                    .get(hovered.series)
                    .and_then(|series| series.data.get(hovered.datum)),
            )
        {
            self.activated = Some((series.id.clone(), datum.id.clone()));
            return true;
        }
        if let Some(annotation) = self
            .hovered_annotation
            .and_then(|index| self.annotations.get(index))
        {
            self.annotation_activated = Some(annotation.id.clone());
            return true;
        }
        false
    }

    fn begin_pointer_gesture(&mut self, modifiers: winit::event::Modifiers) -> bool {
        if !self.plot_rect().contains(self.pointer) {
            return false;
        }
        if self.style.pan != CartesianPan::Disabled
            && (!self.style.gestures.pan_modifier_required || modifiers.state().alt_key())
        {
            self.pan_last = Some(self.pointer);
        } else if self.style.brush != CartesianBrush::Disabled
            && (!self.style.gestures.brush_modifier_required || modifiers.state().shift_key())
        {
            self.brush_start = Some(self.pointer);
            self.brush_current = self.pointer;
        } else {
            return false;
        }
        self.hovered = None;
        self.hovered_annotation = None;
        true
    }

    fn finish_pointer_gesture(&mut self) -> bool {
        if self.pan_last.take().is_some() {
            return true;
        }
        if self.brush_start.is_some() {
            self.finish_brush();
            return true;
        }
        if self.style.interactive_legend
            && let Some(id) = self
                .legend_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .map(|(id, _)| id.clone())
        {
            return self.toggle_series(&id);
        }
        self.activate_inspected()
    }

    fn move_keyboard_inspection(&mut self, action: ChartNavAction) {
        let x_scale = self.style.x_axis.scale;
        let y_scale = self.style.y_axis.scale;
        let targets = self
            .series
            .iter()
            .enumerate()
            .filter(|(_, series)| !self.hidden_series.contains(&series.id))
            .flat_map(|(series_index, series)| {
                series
                    .data
                    .iter()
                    .enumerate()
                    .filter(move |(_, datum)| {
                        Self::transform(datum.x, x_scale).is_some()
                            && Self::transform(datum.y, y_scale).is_some()
                    })
                    .map(move |(datum_index, _)| (series_index, datum_index))
            })
            .collect::<Vec<_>>();
        let position = self.hovered.and_then(|hovered| {
            targets
                .iter()
                .position(|target| *target == (hovered.series, hovered.datum))
        });
        self.hovered = moved_index(position, targets.len(), action).and_then(|position| {
            targets.get(position).and_then(|&(series, datum)| {
                self.map_animated_datum(
                    series,
                    datum,
                    self.plot_rect(),
                    self.domain(&self.style.x_axis, true),
                    self.domain(&self.style.y_axis, false),
                    crate::time::now(),
                )
                .map(|point| Hovered {
                    series,
                    datum,
                    point,
                })
            })
        });
        self.hovered_annotation = None;
        self.keyboard_inspection = self.hovered.is_some();
    }

    fn handle_touch_action(&mut self, action: ChartTouchAction) -> bool {
        match action {
            ChartTouchAction::Inspect(point) => {
                self.pointer = point;
                self.update_hover();
                self.keyboard_inspection = false;
                false
            }
            ChartTouchAction::Activate(point) => {
                self.pointer = point;
                self.update_hover();
                self.activate_inspected()
            }
            ChartTouchAction::Clear => {
                self.clear_inspection();
                false
            }
        }
    }

    fn update_pointer(&mut self, point: Point) -> bool {
        self.pointer = point;
        let consumed = if let Some(previous) = self.pan_last.replace(self.pointer) {
            let delta = self.pointer - previous;
            self.pan_view_pixels(delta.x, delta.y, self.plot_rect());
            true
        } else if self.brush_start.is_some() {
            self.brush_current = self.pointer;
            true
        } else {
            self.update_hover();
            false
        };
        if consumed {
            self.hovered = None;
            self.hovered_annotation = None;
        }
        self.keyboard_inspection = false;
        consumed
    }

    fn tooltip_rect(anchor: Point, size: (f64, f64), bounds: Rect, offset: f64) -> Rect {
        let (width, height) = size;
        let mut x = anchor.x + offset;
        let mut y = anchor.y + offset;
        if x + width > bounds.x1 {
            x = anchor.x - width - offset;
        }
        if y + height > bounds.y1 {
            y = anchor.y - height - offset;
        }
        x = x.clamp(bounds.x0, (bounds.x1 - width).max(bounds.x0));
        y = y.clamp(bounds.y0, (bounds.y1 - height).max(bounds.y0));
        Rect::new(x, y, x + width, y + height)
    }
}

fn format_axis_value(value: f64, format: AxisLabelFormat) -> String {
    match format {
        AxisLabelFormat::Auto => {
            if value.abs() >= 1000.0 {
                format_compact(value)
            } else if value.abs() >= 10.0 {
                format!("{value:.0}")
            } else {
                format!("{value:.1}")
            }
        }
        AxisLabelFormat::Decimal(places) => {
            format!("{value:.places$}", places = usize::from(places.min(8)))
        }
        AxisLabelFormat::Compact => format_compact(value),
        AxisLabelFormat::Percent(places) => format!(
            "{percent:.places$}%",
            percent = value * 100.0,
            places = usize::from(places.min(8))
        ),
        AxisLabelFormat::Bytes(places) => format_bytes(value, places),
        AxisLabelFormat::Scientific(places) => {
            format!("{value:.places$e}", places = usize::from(places.min(8)))
        }
        AxisLabelFormat::Duration(places) => format_duration(value, places),
        AxisLabelFormat::Currency { symbol, places } => {
            let sign = if value.is_sign_negative() { "-" } else { "" };
            format!(
                "{sign}{symbol}{absolute:.places$}",
                absolute = value.abs(),
                places = usize::from(places.min(8))
            )
        }
    }
}

fn format_duration(seconds: f64, places: u8) -> String {
    let absolute = seconds.abs();
    let (scaled, unit) = if absolute >= 86_400.0 {
        (seconds / 86_400.0, "d")
    } else if absolute >= 3_600.0 {
        (seconds / 3_600.0, "h")
    } else if absolute >= 60.0 {
        (seconds / 60.0, "min")
    } else if absolute >= 1.0 {
        (seconds, "s")
    } else if absolute >= 0.001 {
        (seconds * 1_000.0, "ms")
    } else {
        (seconds * 1_000_000.0, "µs")
    };
    format!(
        "{scaled:.places$} {unit}",
        places = usize::from(places.min(8))
    )
}

fn format_bytes(value: f64, places: u8) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut scaled = value.abs();
    let mut unit = 0;
    while scaled >= 1024.0 && unit + 1 < UNITS.len() {
        scaled /= 1024.0;
        unit += 1;
    }
    let signed = scaled.copysign(value);
    format!(
        "{signed:.places$} {}",
        UNITS.get(unit).copied().unwrap_or("PiB"),
        places = usize::from(places.min(8))
    )
}

impl Widget for CartesianPlot {
    crate::widget::chart_navigation::chart_leaf!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.reduce_motion = theme.reduce_motion;
        let now = crate::time::now();
        self.bands = self.measure_bands(tr, theme);
        let plot = self.plot_rect();
        let xd = self.domain(&self.style.x_axis, true);
        let yd = self.domain(&self.style.y_axis, false);
        if self.keyboard_inspection
            && let Some(hovered) = self.hovered
        {
            if let Some(point) =
                self.map_animated_datum(hovered.series, hovered.datum, plot, xd, yd, now)
            {
                self.hovered = Some(Hovered { point, ..hovered });
                self.pointer = point;
            } else {
                self.hovered = None;
            }
        }
        if self.keyboard_inspection
            && let Some(index) = self.hovered_annotation
        {
            if let Some(point) = self.annotation_anchor(index, plot, xd, yd) {
                self.pointer = point;
            } else {
                self.hovered_annotation = None;
            }
        }
        if !self.title.is_empty() {
            tr.draw_style(
                scene,
                &self.title,
                theme.tokens.colors.text_primary,
                transform
                    * Affine::translate((
                        self.rect.x0 + self.bands.gap,
                        self.rect.y0 + self.bands.gap,
                    )),
                None,
                &theme.text(TextRole::Heading),
            );
        }
        self.draw_axes(scene, transform, tr, theme, plot, xd, yd);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &plot);
        self.draw_annotations(&mut AnnotationPaintContext {
            scene,
            transform,
            text: tr,
            theme,
            plot,
            x_domain: xd,
            y_domain: yd,
        });
        self.draw_series(scene, transform, plot, xd, yd, now);
        if let Some(brush) = self.brush_rect() {
            scene.fill(
                Fill::NonZero,
                transform,
                with_alpha(theme.tokens.colors.intent.inverted, 42),
                None,
                &brush,
            );
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                theme.tokens.colors.border_focus,
                None,
                &brush,
            );
        }
        scene.pop_layer();
        self.draw_legend(scene, transform, tr, theme);
        self.draw_tooltip(scene, transform, tr, theme);
        self.draw_annotation_tooltip(scene, transform, tr, theme);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let Some(action) = self.touch.handle(event, ctx.scale_factor) {
            if self.handle_touch_action(action) {
                *ctx.handled = true;
                *ctx.focus_target = Some(ctx.node_id);
            }
            return;
        }
        // Keys inspect the plot only while it has focus; otherwise they
        // belong to whatever does.
        if self.focused
            && let Some(action) = chart_nav_action(event)
        {
            if action == ChartNavAction::Activate {
                let _ = self.activate_inspected();
            } else {
                self.move_keyboard_inspection(action);
            }
            *ctx.handled = true;
            *ctx.focus_target = Some(ctx.node_id);
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if self.update_pointer(point) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorLeft { .. } if !self.keyboard_inspection => {
                self.hovered = None;
                self.hovered_annotation = None;
            }
            WindowEvent::Focused(false) => {
                self.pan_last = None;
                self.brush_start = None;
                self.hovered = None;
                self.hovered_annotation = None;
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.style.zoom != CartesianZoom::Disabled
                    && (!self.style.gestures.zoom_modifier_required
                        || ctx.modifiers.state().control_key()
                        || ctx.modifiers.state().super_key()) =>
            {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(position) => position.y / 80.0,
                };
                if self.plot_rect().contains(self.pointer) && amount != 0.0 {
                    self.zoom_at_pointer(amount);
                    *ctx.handled = true;
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::PinchGesture { delta, .. }
                if self.style.zoom != CartesianZoom::Disabled
                    && self.plot_rect().contains(self.pointer) =>
            {
                // A positive delta means magnification. Pinch is already an
                // unambiguous zoom gesture, so it never requires a modifier.
                self.zoom_at_pointer(*delta);
                *ctx.handled = true;
                *ctx.focus_target = Some(ctx.node_id);
            }
            WindowEvent::PanGesture { delta, .. }
                if self.style.pan != CartesianPan::Disabled
                    && self.plot_rect().contains(self.pointer)
                    && (!self.style.gestures.pan_modifier_required
                        || ctx.modifiers.state().alt_key()) =>
            {
                if self.pan_view_pixels(f64::from(delta.x), f64::from(delta.y), self.plot_rect()) {
                    self.hovered = None;
                    self.hovered_annotation = None;
                    *ctx.handled = true;
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.begin_pointer_gesture(*ctx.modifiers) => {
                *ctx.handled = true;
                *ctx.focus_target = Some(ctx.node_id);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.finish_pointer_gesture() => {
                *ctx.handled = true;
                *ctx.focus_target = Some(ctx.node_id);
            }
            _ => {}
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let x_scale = self.style.x_axis.scale;
        let y_scale = self.style.y_axis.scale;
        let mut children: Vec<AccessNode> = self
            .series
            .iter()
            .filter(|series| !self.hidden_series.contains(&series.id))
            .flat_map(|series| {
                series
                    .data
                    .iter()
                    .filter(|datum| {
                        Self::transform(datum.x, x_scale).is_some()
                            && Self::transform(datum.y, y_scale).is_some()
                    })
                    .map(|datum| {
                        let target = format!("{}:{}", series.id, datum.id);
                        AccessNode::new(Role::Button)
                            .with_action_target(target)
                            .with_name(if datum.label.is_empty() {
                                format!("{} data point", series.name)
                            } else {
                                datum.label.clone()
                            })
                            .with_description(if datum.detail.is_empty() {
                                format!("x {}, y {}", datum.x, datum.y)
                            } else {
                                datum.detail.clone()
                            })
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                    })
            })
            .collect();
        if self.style.interactive_legend && self.style.legend != LegendPosition::Hidden {
            children.extend(self.series.iter().map(|series| {
                AccessNode::new(Role::ToggleButton)
                    .with_action_target(format!("plot-legend:{}", series.id))
                    .with_name(format!("Show {}", series.name))
                    .with_description("Toggle this series in the plot")
                    .with_state(AccessState {
                        focusable: true,
                        checked: Some(!self.hidden_series.contains(&series.id)),
                        ..Default::default()
                    })
            }));
        }
        children.extend(
            self.annotations
                .iter()
                .filter(|annotation| annotation.interactive)
                .map(|annotation| {
                    AccessNode::new(Role::Button)
                        .with_action_target(format!("plot-annotation:{}", annotation.id))
                        .with_name(if annotation.label.is_empty() {
                            annotation.id.clone()
                        } else {
                            annotation.label.clone()
                        })
                        .with_description(if annotation.detail.is_empty() {
                            "Chart annotation".to_owned()
                        } else {
                            annotation.detail.clone()
                        })
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        })
                }),
        );
        Some(
            AccessNode::new(Role::Canvas)
                .with_name(if self.title.is_empty() {
                    "Plot"
                } else {
                    &self.title
                })
                .with_children(children),
        )
    }
    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = key_path.last() else {
            return false;
        };
        if self.style.interactive_legend {
            let legend_id = self.series.iter().find_map(|series| {
                let target = format!("plot-legend:{}", series.id);
                (crate::a11y::hash_key(&target) == *key).then(|| series.id.clone())
            });
            if let Some(id) = legend_id {
                return self.toggle_series(&id);
            }
        }
        if let Some(annotation) = self.annotations.iter().find(|annotation| {
            annotation.interactive
                && crate::a11y::hash_key(&format!("plot-annotation:{}", annotation.id)) == *key
        }) {
            self.annotation_activated = Some(annotation.id.clone());
            return true;
        }
        for series in &self.series {
            for datum in &series.data {
                let target = format!("{}:{}", series.id, datum.id);
                if crate::a11y::hash_key(&target) == *key {
                    self.activated = Some((series.id.clone(), datum.id.clone()));
                    return true;
                }
            }
        }
        false
    }
    fn name(&self) -> &'static str {
        "CartesianPlot"
    }
    fn needs_animation_frame(&self) -> bool {
        self.is_animating()
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if !focused && self.keyboard_inspection {
            self.keyboard_inspection = false;
            self.hovered = None;
            self.hovered_annotation = None;
        }
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn wants_pointer_capture(&self) -> bool {
        self.pan_last.is_some() || self.brush_start.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_domains_and_ticks_expand_preserve_and_thin() {
        fn constant_domains_are_expanded() {
            let domain = Domain::normalized(5.0, 5.0);
            assert!(domain.max > domain.min);
            assert_eq!(domain.min.midpoint(domain.max), 5.0);
        }

        fn default_style_auto_ranges_both_axes() {
            let style = CartesianStyle::default();
            assert_eq!(style.x_axis.range, AxisRange::Auto);
            assert_eq!(style.y_axis.range, AxisRange::Auto);
        }

        fn explicit_ticks_are_preserved() {
            let axis = PlotAxis::default()
                .with_ticks([AxisTick::new(0.0, "Start"), AxisTick::new(1.0, "End")]);
            assert_eq!(
                CartesianPlot::ticks(&axis, Domain::normalized(0.0, 1.0), 320.0),
                axis.ticks
            );
        }

        fn narrow_axes_reduce_tick_density_but_keep_endpoints() {
            let axis = PlotAxis::default().with_ticks(
                (0..10).map(|value| AxisTick::new(f64::from(value), value.to_string())),
            );
            let ticks = CartesianPlot::ticks(&axis, Domain::normalized(0.0, 9.0), 150.0);
            assert_eq!(ticks.len(), 2);
            assert_eq!(ticks.first().map(|tick| tick.label.as_str()), Some("0"));
            assert_eq!(ticks.last().map(|tick| tick.label.as_str()), Some("9"));
        }

        fn domain_cache_is_invalidated_by_data_and_visibility_changes() {
            let near = PlotSeries::new(
                "near",
                "Near",
                Color::WHITE,
                vec![PlotDatum::new("a", 1.0, 1.0)],
            );
            let far = PlotSeries::new(
                "far",
                "Far",
                Color::WHITE,
                vec![PlotDatum::new("b", 1_000.0, 1_000.0)],
            );
            let mut plot = CartesianPlot::new(vec![near.clone(), far]);
            let initial = plot.base_domain(&plot.style.x_axis, true);
            let cached = plot.base_domain(&plot.style.x_axis, true);
            assert_eq!(initial, cached);
            assert!(plot.set_series_visible("far", false));
            let hidden = plot.base_domain(&plot.style.x_axis, true);
            assert!(hidden.max < 10.0);
            plot.set_series_immediate(vec![
                near,
                PlotSeries::new(
                    "new",
                    "New",
                    Color::WHITE,
                    vec![PlotDatum::new("c", 50.0, 50.0)],
                ),
            ]);
            let replaced = plot.base_domain(&plot.style.x_axis, true);
            assert!(replaced.max > 50.0);
        }

        fn series_visibility_is_observable_and_changes_auto_domain() {
            let color = Color::from_rgb8(0, 0, 0);
            let mut plot = CartesianPlot::new(vec![
                PlotSeries::new("near", "Near", color, vec![PlotDatum::new("a", 0.0, 1.0)]),
                PlotSeries::new("far", "Far", color, vec![PlotDatum::new("b", 100.0, 100.0)]),
            ]);
            let before = plot.base_domain(&PlotAxis::default(), true);
            assert!(plot.set_series_visible("far", false));
            let after = plot.base_domain(&PlotAxis::default(), true);
            assert!(after.max < before.max);
            assert_eq!(plot.take_visibility_changed(), Some(("far".into(), false)));
            assert!(!plot.is_series_visible("far"));
        }

        constant_domains_are_expanded();
        default_style_auto_ranges_both_axes();
        explicit_ticks_are_preserved();
        narrow_axes_reduce_tick_density_but_keep_endpoints();
        domain_cache_is_invalidated_by_data_and_visibility_changes();
        series_visibility_is_observable_and_changes_auto_domain();
    }

    #[test]
    fn axis_titles_formats_and_tooltips_fit_the_chart() {
        fn axis_titles_fit_responsive_space_with_unicode_safe_ellipsis() {
            assert_eq!(
                CartesianPlot::fitted_axis_title("Latency", 200.0),
                "Latency"
            );
            assert_eq!(
                CartesianPlot::fitted_axis_title("Requests / second", 42.0),
                "Reque…"
            );
            assert_eq!(CartesianPlot::fitted_axis_title("Διακύμανση", 28.0), "Δια…");
        }

        fn built_in_axis_formats_cover_units() {
            assert_eq!(
                format_axis_value(0.425, AxisLabelFormat::Percent(1)),
                "42.5%"
            );
            assert_eq!(
                format_axis_value(2048.0, AxisLabelFormat::Bytes(1)),
                "2.0 KiB"
            );
            assert_eq!(format_axis_value(1500.0, AxisLabelFormat::Compact), "1.5K");
            assert_eq!(
                format_axis_value(0.0125, AxisLabelFormat::Duration(1)),
                "12.5 ms"
            );
            assert_eq!(
                format_axis_value(
                    -12.5,
                    AxisLabelFormat::Currency {
                        symbol: '$',
                        places: 2
                    }
                ),
                "-$12.50"
            );
            assert_eq!(
                format_axis_value(12_500.0, AxisLabelFormat::Scientific(2)),
                "1.25e4"
            );
        }

        fn compact_number_format_uses_suffixes() {
            assert_eq!(format_compact(1_500_000.0), "1.5M");
        }

        fn tooltip_placement_flips_and_clamps_to_chart_bounds() {
            let bounds = Rect::new(0.0, 0.0, 300.0, 200.0);
            let rect =
                CartesianPlot::tooltip_rect(Point::new(290.0, 190.0), (120.0, 60.0), bounds, 16.0);
            assert_eq!(rect, Rect::new(154.0, 114.0, 274.0, 174.0));
            let oversized =
                CartesianPlot::tooltip_rect(Point::new(10.0, 10.0), (300.0, 200.0), bounds, 16.0);
            assert_eq!(oversized, bounds);
        }

        axis_titles_fit_responsive_space_with_unicode_safe_ellipsis();
        built_in_axis_formats_cover_units();
        compact_number_format_uses_suffixes();
        tooltip_placement_flips_and_clamps_to_chart_bounds();
    }

    #[test]
    fn viewport_navigation_zooms_pans_and_survives_invalid_input() {
        fn cursor_anchored_zoom_keeps_anchor_stable() {
            let base = Domain::normalized(0.0, 100.0);
            let zoomed = CartesianPlot::zoom_domain(base, base, 25.0, 0.5, 0.01, AxisScale::Linear);
            assert_eq!(zoomed.min, 12.5);
            assert_eq!(zoomed.max, 62.5);
            assert_eq!((25.0 - zoomed.min) / (zoomed.max - zoomed.min), 0.25);
        }

        fn viewport_can_be_restored_panned_and_reset() {
            let color = Color::from_rgb8(0, 0, 0);
            let mut plot = CartesianPlot::new(vec![PlotSeries::new(
                "series",
                "Series",
                color,
                vec![
                    PlotDatum::new("a", 0.0, 0.0),
                    PlotDatum::new("b", 100.0, 50.0),
                ],
            )]);
            assert!(plot.set_viewport(
                Some(AxisRange::Fixed {
                    min: 10.0,
                    max: 30.0
                }),
                Some(AxisRange::Fixed {
                    min: 5.0,
                    max: 15.0
                }),
            ));
            assert!(plot.pan_view_fraction(0.5, -0.5));
            assert_eq!(
                plot.viewport(),
                (
                    Some(AxisRange::Fixed {
                        min: 20.0,
                        max: 40.0
                    }),
                    Some(AxisRange::Fixed {
                        min: 0.0,
                        max: 10.0
                    })
                )
            );
            plot.reset_view();
            assert_eq!(plot.viewport(), (None, None));
        }

        fn pixel_pan_is_scale_aware_and_x_only_preserves_y() {
            let mut plot = CartesianPlot::new(Vec::new());
            plot.style.pan = CartesianPan::X;
            assert!(plot.set_viewport(
                Some(AxisRange::Fixed {
                    min: 0.0,
                    max: 100.0
                }),
                Some(AxisRange::Fixed {
                    min: 5.0,
                    max: 15.0
                }),
            ));
            assert!(plot.pan_view_pixels(100.0, 50.0, Rect::new(0.0, 0.0, 400.0, 200.0)));
            assert_eq!(
                plot.viewport(),
                (
                    Some(AxisRange::Fixed {
                        min: -25.0,
                        max: 75.0
                    }),
                    Some(AxisRange::Fixed {
                        min: 5.0,
                        max: 15.0
                    })
                )
            );

            plot.style.x_axis = PlotAxis::default().logarithmic();
            assert!(plot.set_viewport(
                Some(AxisRange::Fixed {
                    min: 1.0,
                    max: 1000.0
                }),
                None
            ));
            assert!(plot.pan_view_pixels(100.0, 0.0, Rect::new(0.0, 0.0, 300.0, 100.0)));
            let Some(AxisRange::Fixed { min, max }) = plot.viewport().0 else {
                panic!()
            };
            assert!((min - 0.1).abs() < 1e-9);
            assert!((max - 100.0).abs() < 1e-9);
        }

        fn pixel_pan_rejects_invalid_geometry_and_captures_active_drags() {
            let mut plot = CartesianPlot::new(Vec::new());
            plot.style.pan = CartesianPan::XY;
            assert!(!plot.pan_view_pixels(f64::NAN, 0.0, Rect::new(0.0, 0.0, 100.0, 100.0)));
            assert!(!plot.pan_view_pixels(1.0, 0.0, Rect::ZERO));
            assert!(!plot.wants_pointer_capture());
            plot.pan_last = Some(Point::ZERO);
            assert!(plot.wants_pointer_capture());
        }

        fn invalid_viewports_do_not_destroy_current_navigation() {
            let mut plot = CartesianPlot::new(Vec::new());
            assert!(plot.set_viewport(Some(AxisRange::Fixed { min: 1.0, max: 2.0 }), None,));
            assert!(!plot.set_viewport(Some(AxisRange::Fixed { min: 2.0, max: 1.0 }), None,));
            assert_eq!(
                plot.viewport(),
                (Some(AxisRange::Fixed { min: 1.0, max: 2.0 }), None)
            );
        }

        cursor_anchored_zoom_keeps_anchor_stable();
        viewport_can_be_restored_panned_and_reset();
        pixel_pan_is_scale_aware_and_x_only_preserves_y();
        pixel_pan_rejects_invalid_geometry_and_captures_active_drags();
        invalid_viewports_do_not_destroy_current_navigation();
    }

    #[test]
    fn logarithmic_axes_project_tick_zoom_and_reject_non_positive() {
        fn logarithmic_projection_and_unprojection_round_trip() {
            let domain = Domain {
                min: 1.0,
                max: 1000.0,
            };
            let projected = CartesianPlot::project(10.0, domain, AxisScale::Log10, 0.0, 300.0)
                .expect("positive log value");
            assert!((projected - 100.0).abs() < 1e-9);
            let value = CartesianPlot::unproject(projected, domain, AxisScale::Log10, 0.0, 300.0);
            assert!((value - 10.0).abs() < 1e-9);
            assert!(CartesianPlot::project(0.0, domain, AxisScale::Log10, 0.0, 300.0).is_none());
        }

        fn logarithmic_ticks_prefer_decades_and_respect_density() {
            let axis = PlotAxis::default().logarithmic().with_tick_density(4, 60.0);
            let ticks = CartesianPlot::ticks(
                &axis,
                Domain {
                    min: 1.0,
                    max: 1000.0,
                },
                320.0,
            );
            assert_eq!(
                ticks.iter().map(|tick| tick.value).collect::<Vec<_>>(),
                vec![1.0, 10.0, 100.0, 1000.0]
            );
        }

        fn logarithmic_zoom_and_brush_stay_in_data_coordinates() {
            let base = Domain {
                min: 1.0,
                max: 1000.0,
            };
            let zoomed = CartesianPlot::zoom_domain(base, base, 10.0, 0.5, 0.01, AxisScale::Log10);
            assert!((zoomed.min.log10() - 0.5).abs() < 1e-9);
            assert!((zoomed.max.log10() - 2.0).abs() < 1e-9);
            let selection = CartesianPlot::brush_selection_from_rect(
                Rect::new(100.0, 0.0, 200.0, 100.0),
                Rect::new(0.0, 0.0, 300.0, 100.0),
                base,
                Domain::normalized(0.0, 1.0),
                AxisScale::Log10,
                AxisScale::Linear,
                CartesianBrush::X,
            )
            .unwrap();
            assert_eq!(
                selection.x,
                AxisRange::Fixed {
                    min: 10.0,
                    max: 100.0
                }
            );
        }

        fn logarithmic_viewports_reject_non_positive_ranges() {
            let mut plot = CartesianPlot::new(Vec::new());
            plot.style.x_axis = PlotAxis::default().logarithmic();
            assert!(!plot.set_viewport(
                Some(AxisRange::Fixed {
                    min: 0.0,
                    max: 10.0
                }),
                None,
            ));
            assert!(plot.set_viewport(
                Some(AxisRange::Fixed {
                    min: 1.0,
                    max: 10.0
                }),
                None,
            ));
        }

        logarithmic_projection_and_unprojection_round_trip();
        logarithmic_ticks_prefer_decades_and_respect_density();
        logarithmic_zoom_and_brush_stay_in_data_coordinates();
        logarithmic_viewports_reject_non_positive_ranges();
    }

    #[test]
    fn series_builders_sampling_and_gaps_preserve_data() {
        fn annotations_have_concise_builders_and_remain_mutable() {
            let color = Color::from_rgb8(1, 2, 3);
            let mut plot = CartesianPlot::new(Vec::new()).with_annotations([
                CartesianAnnotation::y_line("limit", 10.0, color)
                    .labelled("Limit")
                    .stroke_width(2.0)
                    .solid(),
                CartesianAnnotation::x_band("window", 2.0, 4.0, color).fill_alpha(42),
            ]);
            assert_eq!(plot.annotations().len(), 2);
            assert_eq!(plot.annotations()[0].id, "limit");
            assert!(!plot.annotations()[0].dashed);
            assert_eq!(plot.annotations()[1].fill_alpha, 42);
            plot.annotations_mut().clear();
            assert!(plot.annotations().is_empty());
        }

        fn series_visual_builders_sanitize_and_preserve_art_direction() {
            let series = PlotSeries::new("s", "Series", Color::WHITE, Vec::new())
                .kind(SeriesKind::SmoothArea)
                .dashed([8.0, -2.0, f64::NAN, 3.0])
                .fill_alpha(90)
                .marker_shape(MarkerShape::Diamond)
                .bar_radius(-4.0);
            assert_eq!(series.line_dash, vec![8.0, 3.0]);
            assert_eq!(series.fill_alpha, 90);
            assert_eq!(series.marker_shape, MarkerShape::Diamond);
            assert_eq!(series.bar_radius, 0.0);
        }

        fn adaptive_sampling_preserves_extrema_and_respects_pixel_budget() {
            let mut data = (0..10_000)
                .map(|index| PlotDatum::new(index.to_string(), f64::from(index), 0.0))
                .collect::<Vec<_>>();
            data[1_337].y = 900.0;
            data[7_331].y = -700.0;
            let line = PlotSeries::new("dense", "Dense", Color::WHITE, data);
            let plot = CartesianPlot::new(vec![line.clone()]);
            let sampled = plot.render_indices(&line, Rect::new(0.0, 0.0, 100.0, 100.0));
            assert!(sampled.len() <= 202, "{} sampled points", sampled.len());
            assert_eq!(sampled.first(), Some(&0));
            assert_eq!(sampled.last(), Some(&9_999));
            assert!(sampled.contains(&1_337));
            assert!(sampled.contains(&7_331));
            assert!(sampled.windows(2).all(|pair| pair[0] < pair[1]));

            let scatter = line.kind(SeriesKind::Scatter);
            assert_eq!(
                plot.render_indices(&scatter, Rect::new(0.0, 0.0, 100.0, 100.0))
                    .len(),
                10_000
            );
        }

        fn sampling_configuration_is_sanitized_and_can_be_disabled() {
            let style = CartesianStyle::default().with_adaptive_sampling(false, f64::NAN);
            assert!(!style.rendering.adaptive_sampling);
            assert_eq!(style.rendering.max_points_per_pixel, 2.0);
            let style = style.with_adaptive_sampling(true, 100.0);
            assert_eq!(style.rendering.max_points_per_pixel, 16.0);
        }

        fn missing_samples_break_paths_unless_connection_is_requested() {
            let a = Point::new(0.0, 0.0);
            let b = Point::new(1.0, 1.0);
            let c = Point::new(2.0, 2.0);
            let broken = CartesianPlot::point_segments([Some(a), Some(b), None, Some(c)], false);
            assert_eq!(broken, vec![vec![a, b], vec![c]]);
            let connected = CartesianPlot::point_segments([Some(a), Some(b), None, Some(c)], true);
            assert_eq!(connected, vec![vec![a, b, c]]);

            let series = PlotSeries::new(
                "gapped",
                "Gapped",
                Color::WHITE,
                (0..1_000)
                    .map(|index| {
                        if index == 501 {
                            PlotDatum::missing("gap", f64::from(index))
                        } else {
                            PlotDatum::new(index.to_string(), f64::from(index), f64::from(index))
                        }
                    })
                    .collect(),
            );
            let plot = CartesianPlot::new(vec![series.clone()]);
            assert!(
                plot.render_indices(&series, Rect::new(0.0, 0.0, 40.0, 100.0))
                    .contains(&501)
            );
        }

        annotations_have_concise_builders_and_remain_mutable();
        series_visual_builders_sanitize_and_preserve_art_direction();
        adaptive_sampling_preserves_extrema_and_respects_pixel_budget();
        sampling_configuration_is_sanitized_and_can_be_disabled();
        missing_samples_break_paths_unless_connection_is_requested();
    }

    #[test]
    fn hover_annotations_and_live_updates_are_exact_and_stable() {
        fn sorted_series_hover_uses_a_small_exact_x_window() {
            let series = PlotSeries::new(
                "sorted",
                "Sorted",
                Color::WHITE,
                (0..10_000)
                    .map(|index| PlotDatum::new(index.to_string(), f64::from(index), 1.0))
                    .collect(),
            )
            .sorted_by_x();
            let mut plot = CartesianPlot::new(vec![series.clone()]);
            plot.pointer = Point::new(500.0, 50.0);
            plot.style.hit_radius = 10.0;
            let range = plot.hover_range(
                &series,
                Rect::new(0.0, 0.0, 1_000.0, 100.0),
                Domain::normalized(0.0, 10_000.0),
            );
            assert!(range.contains(&5_000));
            assert!(range.len() < 250, "{} candidates", range.len());
        }

        fn annotations_are_hit_testable_and_programmatically_inspectable() {
            let color = Color::WHITE;
            let mut plot = CartesianPlot::new(Vec::new()).with_annotations([
                CartesianAnnotation::x_line("marker", 5.0, color).with_detail("Release"),
                CartesianAnnotation::x_band("window", 7.0, 9.0, color),
                CartesianAnnotation::y_line("decoration", 2.0, color).interactive(false),
            ]);
            plot.style.x_axis = PlotAxis::default().fixed(0.0, 10.0);
            plot.style.y_axis = PlotAxis::default().fixed(0.0, 10.0);
            let area = Rect::new(0.0, 0.0, 100.0, 100.0);
            let domain = Domain::normalized(0.0, 10.0);
            assert_eq!(
                plot.annotation_at(Point::new(50.0, 40.0), area, domain, domain),
                Some(0)
            );
            assert_eq!(
                plot.annotation_at(Point::new(80.0, 40.0), area, domain, domain),
                Some(1)
            );
            assert!(plot.inspect_annotation("marker"));
            assert!(!plot.inspect_annotation("decoration"));
            assert!(!plot.inspect_annotation("missing"));
        }

        fn stable_ids_interpolate_live_updates_and_reduced_motion_snaps() {
            let color = Color::from_rgb8(1, 2, 3);
            let old = PlotSeries::new(
                "series",
                "Series",
                color,
                vec![PlotDatum::new("point", 0.0, 10.0)],
            );
            let mut plot = CartesianPlot::new(vec![old]);
            plot.style.animation.duration = Duration::from_millis(200);
            plot.set_series(vec![PlotSeries::new(
                "series",
                "Series",
                color,
                vec![PlotDatum::new("point", 100.0, 30.0)],
            )]);
            let midpoint = crate::time::now();
            plot.transition_started = midpoint.checked_sub(Duration::from_millis(100)).unwrap();
            let (x, y) = plot.animated_xy(0, 0, midpoint).unwrap();
            assert!(x > 50.0 && x < 100.0, "ease-out midpoint: {x}");
            assert!(y > 20.0 && y < 30.0, "ease-out midpoint: {y}");
            assert!(plot.needs_animation_frame());
            plot.reduce_motion = true;
            assert_eq!(plot.animated_xy(0, 0, midpoint), Some((100.0, 30.0)));
            assert!(!plot.needs_animation_frame());
        }

        fn identical_series_updates_are_noops_and_immediate_updates_snap() {
            let series = vec![PlotSeries::new(
                "series",
                "Series",
                Color::WHITE,
                vec![PlotDatum::new("point", 1.0, 2.0)],
            )];
            let mut plot = CartesianPlot::new(series.clone());
            let started = plot.transition_started;
            plot.set_series(series);
            assert_eq!(plot.transition_started, started);
            assert!(!plot.is_animating());
            plot.set_series_immediate(vec![PlotSeries::new(
                "series",
                "Series",
                Color::WHITE,
                vec![PlotDatum::new("point", 3.0, 4.0)],
            )]);
            assert!(!plot.is_animating());
            assert_eq!(plot.series[0].data[0].x, 3.0);
        }

        sorted_series_hover_uses_a_small_exact_x_window();
        annotations_are_hit_testable_and_programmatically_inspectable();
        stable_ids_interpolate_live_updates_and_reduced_motion_snaps();
        identical_series_updates_are_noops_and_immediate_updates_snap();
    }

    #[test]
    fn brushes_convert_pixels_to_domains_and_ignore_tiny_drags() {
        fn brush_converts_pixels_to_x_domain() {
            let selection = CartesianPlot::brush_selection_from_rect(
                Rect::new(60.0, 20.0, 160.0, 120.0),
                Rect::new(10.0, 20.0, 210.0, 120.0),
                Domain::normalized(0.0, 100.0),
                Domain::normalized(-10.0, 10.0),
                AxisScale::Linear,
                AxisScale::Linear,
                CartesianBrush::X,
            )
            .unwrap();
            assert_eq!(
                selection.x,
                AxisRange::Fixed {
                    min: 25.0,
                    max: 75.0
                }
            );
            assert_eq!(selection.y, None);
        }

        fn xy_brush_converts_inverted_y_domain() {
            let selection = CartesianPlot::brush_selection_from_rect(
                Rect::new(50.0, 25.0, 150.0, 75.0),
                Rect::new(0.0, 0.0, 200.0, 100.0),
                Domain::normalized(0.0, 20.0),
                Domain::normalized(0.0, 100.0),
                AxisScale::Linear,
                AxisScale::Linear,
                CartesianBrush::XY,
            )
            .unwrap();
            assert_eq!(
                selection.x,
                AxisRange::Fixed {
                    min: 5.0,
                    max: 15.0
                }
            );
            assert_eq!(
                selection.y,
                Some(AxisRange::Fixed {
                    min: 25.0,
                    max: 75.0
                })
            );
        }

        fn tiny_brushes_are_ignored() {
            assert!(
                CartesianPlot::brush_selection_from_rect(
                    Rect::new(0.0, 0.0, 3.0, 100.0),
                    Rect::new(0.0, 0.0, 100.0, 100.0),
                    Domain::normalized(0.0, 1.0),
                    Domain::normalized(0.0, 1.0),
                    AxisScale::Linear,
                    AxisScale::Linear,
                    CartesianBrush::X,
                )
                .is_none()
            );
        }

        brush_converts_pixels_to_x_domain();
        xy_brush_converts_inverted_y_domain();
        tiny_brushes_are_ignored();
    }
}
