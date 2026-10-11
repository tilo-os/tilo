//! Interactive pie and donut charts.
//!
//! The widget owns interaction geometry as well as painting, so pointer,
//! keyboard/accessibility activation, tooltips and the rendered wedges always
//! agree. Values may be updated without rebuilding the widget tree.
//!
//! Its height comes from the space it is given: the donut and its legend
//! are one unit, centred, the donut as large as the room allows. The legend
//! and the tooltip state a share the same way (whole percent), a slice can
//! add a detail column to the legend (its size), and the tooltip never
//! covers the legend.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "chart geometry is bounded floating-point arithmetic"
)]

use crate::time::{Duration, Instant};
use crate::widget::ModelPath;
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchTracker, InteractiveChart, chart_nav_action, handle_chart_event,
    moved_index,
};
use crate::widget::{TaffyTreeExt, Widget};

const WEDGE_STEPS: u32 = 64;

/// A stable, labelled radial datum. Negative and non-finite values are treated
/// as zero. `id` survives sorting and updates and is returned on activation.
#[derive(Clone, Debug, PartialEq)]
pub struct PieSlice {
    pub id: String,
    pub label: String,
    pub value: f64,
    pub color: Color,
    pub detail: String,
    /// A column after the legend's label and share: the slice's size
    /// ("12.4 GB"). Empty for none.
    pub legend_detail: String,
    /// Whether activation is meaningful. Inspection remains available when false.
    pub interactive: bool,
}

impl PieSlice {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: f64, color: Color) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            color,
            detail: String::new(),
            legend_detail: String::new(),
            interactive: true,
        }
    }

    /// A detail column in the legend (the slice's size), aligned after the
    /// label and its share.
    #[must_use]
    pub fn with_legend_detail(mut self, detail: impl Into<String>) -> Self {
        self.legend_detail = detail.into();
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// Keep the slice visible and hover-inspectable without presenting a dead action.
    #[must_use]
    pub fn non_interactive(mut self) -> Self {
        self.interactive = false;
        self
    }
}

/// Visual configuration shared by pies and donuts.
#[derive(Clone, Copy, Debug)]
pub struct PieStyle {
    /// Hole radius divided by outer radius. `0` is a pie, `0.62` is a donut.
    pub inner_radius: f64,
    /// Angular whitespace between adjacent slices, in radians.
    pub gap: f64,
    /// Rotation in radians; the default starts at twelve o'clock.
    pub start_angle: f64,
    /// Distance the hovered wedge moves away from the centre.
    pub hover_offset: f64,
    /// Outer padding kept available for hover lift and focus treatment.
    pub padding: f64,
    /// Minimum width for the two-line inspector overlay.
    pub tooltip_min_width: f64,
    /// Fixed tooltip width. `None` sizes to content within available bounds.
    pub tooltip_width: Option<f64>,
    pub tooltip_anchor: PieTooltipAnchor,
    pub tooltip_offset: f64,
    pub tooltip_padding: f64,
    pub tooltip: bool,
    pub tooltip_show_percent: bool,
    /// Space at the chart's right edge reserved for an external legend or
    /// adjacent controls. The automatic inspector never enters this area.
    pub tooltip_right_inset: f64,
    /// Smoothly interpolate matching stable slice ids after `set_slices`.
    pub animate_changes: bool,
    pub animation_duration: Duration,
}

impl Default for PieStyle {
    fn default() -> Self {
        Self {
            inner_radius: 0.62,
            gap: 0.018,
            start_angle: -FRAC_PI_2,
            hover_offset: 7.0,
            padding: 14.0,
            tooltip_min_width: 176.0,
            tooltip_width: None,
            tooltip_anchor: PieTooltipAnchor::Pointer,
            tooltip_offset: 18.0,
            tooltip_padding: 14.0,
            tooltip: true,
            tooltip_show_percent: true,
            tooltip_right_inset: 0.0,
            animate_changes: true,
            animation_duration: Duration::from_millis(260),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PieTooltipAnchor {
    #[default]
    Pointer,
    Slice,
}

#[derive(Clone, Copy, Debug)]
struct Sector {
    index: usize,
    start: f64,
    end: f64,
}

/// A responsive interactive pie/donut widget.
pub struct PieChart {
    slices: Vec<PieSlice>,
    style: PieStyle,
    center_title: String,
    center_subtitle: String,
    /// A fixed height; `None` takes the space the parent gives.
    height: Option<f32>,
    /// The theme seen at layout, for measuring the legend.
    theme: Option<Theme>,
    /// Where the legend's entries were painted, for the tooltip to avoid.
    legend_rects: Vec<Rect>,
    /// Keys inspect slices only while the chart has focus.
    focused: bool,
    rect: Rect,
    hovered: Option<usize>,
    activated: Option<String>,
    pointer: Point,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
    transition_from: Option<Vec<PieSlice>>,
    transition_started: Instant,
    reduce_motion: bool,
    /// The chart names its slices in a legend beside or under it.
    legend: bool,
    /// Where the wedges go: the whole rect, less the legend's area.
    chart_rect: Rect,
}

impl PieChart {
    pub fn new(slices: Vec<PieSlice>) -> Self {
        Self {
            slices,
            style: PieStyle::default(),
            center_title: String::new(),
            center_subtitle: String::new(),
            height: None,
            theme: None,
            legend_rects: Vec::new(),
            focused: false,
            rect: Rect::ZERO,
            hovered: None,
            activated: None,
            pointer: Point::ZERO,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
            transition_from: None,
            transition_started: crate::time::now(),
            reduce_motion: false,
            legend: false,
            chart_rect: Rect::ZERO,
        }
    }

    /// Name every slice in a legend (a swatch, its label and its share)
    /// beside the chart when there is room, under it otherwise. The legend
    /// and the wedges never overlap: the chart shrinks to make room.
    #[must_use]
    pub fn with_legend(mut self) -> Self {
        self.legend = true;
        self
    }

    /// The widest legend detail plus its gap, or zero without details.
    fn legend_detail_width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let style = theme.text(TextRole::Caption).numeric();
        let widest = self
            .slices
            .iter()
            .filter(|slice| !slice.legend_detail.is_empty())
            .map(|slice| f64::from(text.measure_style(&slice.legend_detail, None, &style).0))
            .fold(0.0, f64::max);
        if widest > 0.0 {
            (widest + f64::from(theme.tokens.spacing.s4)).ceil()
        } else {
            0.0
        }
    }

    /// Lay the donut and the legend out inside `rect` as one centred unit
    /// and return the rect for the wedges, with each legend entry's rect
    /// (swatch, label and share, then its detail column).
    fn legend_layout(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) -> (Rect, Vec<(usize, Rect)>) {
        if !self.legend || self.slices.is_empty() {
            return (rect, Vec::new());
        }
        let total = self.total();
        let labels: Vec<String> = self
            .slices
            .iter()
            .map(|slice| legend_label(slice, total))
            .collect();
        let detail_w = self.legend_detail_width(text, theme);
        let mut entries =
            crate::widget::chart_legend::measure(text, theme, labels.iter().map(String::as_str));
        let label_w = entries.iter().map(|e| e.width).fold(0.0, f64::max);
        for entry in &mut entries {
            // Details line up in one column after the longest label.
            if detail_w > 0.0 {
                entry.width = label_w + detail_w;
            }
        }
        let gap = f64::from(theme.tokens.spacing.rhythm.section);
        let widest = entries.iter().map(|e| e.width).fold(0.0, f64::max);
        let row = entries.first().map_or(0.0, |e| e.height);
        let side_room = rect.width() - rect.height().min(rect.width() * 0.6);
        if widest + gap <= side_room {
            // Beside: the donut and one legend column, centred together.
            let diameter = (rect.width() - widest - gap).min(rect.height()).max(0.0);
            let unit = diameter + gap + widest;
            let x0 = (rect.center().x - unit / 2.0).max(rect.x0);
            let legend_x = x0 + diameter + gap;
            let block = crate::num::f64_of_usize(entries.len()) * row;
            let mut y = (rect.center().y - block / 2.0).max(rect.y0);
            let items = entries
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let r = Rect::new(
                        legend_x,
                        y,
                        legend_x + e.width.min(rect.x1 - legend_x),
                        y + row,
                    );
                    y += row;
                    (i, r)
                })
                .collect();
            (Rect::new(x0, rect.y0, x0 + diameter, rect.y1), items)
        } else {
            // Under: wrapping rows, each centred, under the donut.
            let rows = crate::widget::chart_legend::wrap(&entries, rect.x0, rect.x1, 0.0, theme);
            let height = rows.iter().map(|r| r.y1).fold(0.0, f64::max);
            let diameter = (rect.height() - gap - height).min(rect.width()).max(0.0);
            let unit = diameter + gap + height;
            let y0 = (rect.center().y - unit / 2.0).max(rect.y0);
            let top = y0 + diameter + gap;
            let mut items: Vec<(usize, Rect)> = Vec::with_capacity(rows.len());
            let mut start = 0;
            while start < rows.len() {
                let line_y = rows[start].y0;
                let end = rows[start..]
                    .iter()
                    .position(|r| (r.y0 - line_y).abs() > 0.5)
                    .map_or(rows.len(), |n| start + n);
                let line = &rows[start..end];
                let x1 = line.iter().map(|r| r.x1).fold(rect.x0, f64::max);
                let shift = ((rect.x1 - x1) / 2.0).max(0.0);
                for (offset, r) in line.iter().enumerate() {
                    items.push((start + offset, *r + vello::kurbo::Vec2::new(shift, top)));
                }
                start = end;
            }
            (Rect::new(rect.x0, y0, rect.x1, y0 + diameter), items)
        }
    }

    /// The height the chart wants at `width`: a donut as wide as it may
    /// be, with its legend beside or under it.
    fn natural_height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let line = f64::from(theme.text(TextRole::Body).line_height_px());
        let diameter = (line * 14.0).min(width).max(line * 6.0);
        if !self.legend || self.slices.is_empty() {
            return diameter;
        }
        // Lay out at the donut's height to see where the legend lands.
        let probe = Rect::new(0.0, 0.0, width, diameter);
        let (_, items) = self.legend_layout(text, theme, probe);
        let beside = items.first().is_some_and(|(_, r)| r.x0 > probe.center().x);
        if beside {
            let block = items.iter().map(|(_, r)| r.y1).fold(0.0, f64::max)
                - items
                    .iter()
                    .map(|(_, r)| r.y0)
                    .fold(f64::INFINITY, f64::min);
            diameter.max(block)
        } else {
            let rows = items.iter().map(|(_, r)| r.y1).fold(0.0, f64::max)
                - items
                    .iter()
                    .map(|(_, r)| r.y0)
                    .fold(f64::INFINITY, f64::min);
            diameter + f64::from(theme.tokens.spacing.rhythm.section) + rows.max(0.0)
        }
    }

    #[must_use]
    pub fn with_style(mut self, style: PieStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn styled(self, style: PieStyle) -> Self {
        self.with_style(style)
    }
    pub fn style(&self) -> &PieStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut PieStyle {
        &mut self.style
    }
    #[must_use]
    pub fn with_center(mut self, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        self.center_title = title.into();
        self.center_subtitle = subtitle.into();
        self
    }
    /// A fixed height instead of the space the parent gives.
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = Some(height.max(80.0));
        self
    }
    pub fn set_slices(&mut self, slices: Vec<PieSlice>) {
        if self.slices == slices {
            return;
        }
        let hovered_id = self.hovered().map(|slice| slice.id.clone());
        let previous = std::mem::replace(&mut self.slices, slices);
        if self.style.animate_changes
            && !self.reduce_motion
            && !self.style.animation_duration.is_zero()
        {
            self.transition_from = Some(previous);
            self.transition_started = crate::time::now();
        } else {
            self.transition_from = None;
        }
        self.hovered =
            hovered_id.and_then(|id| self.slices.iter().position(|slice| slice.id == id));
    }
    /// Replace data without motion, useful for initial hydration, restored
    /// sessions, and deterministic capture.
    pub fn set_slices_immediate(&mut self, slices: Vec<PieSlice>) {
        let hovered_id = self.hovered().map(|slice| slice.id.clone());
        self.slices = slices;
        self.transition_from = None;
        self.hovered =
            hovered_id.and_then(|id| self.slices.iter().position(|slice| slice.id == id));
    }
    pub fn finish_animation(&mut self) {
        self.transition_from = None;
    }
    pub fn is_animating(&self) -> bool {
        !self.reduce_motion
            && self.style.animate_changes
            && self.transition_from.is_some()
            && self.transition_started.elapsed() < self.style.animation_duration
    }
    pub fn set_center(&mut self, title: impl Into<String>, subtitle: impl Into<String>) {
        self.center_title = title.into();
        self.center_subtitle = subtitle.into();
    }
    pub fn hovered(&self) -> Option<&PieSlice> {
        self.hovered.and_then(|i| self.slices.get(i))
    }
    /// Inspect a slice by stable ID. Useful for keyboard navigation,
    /// coordinated views, guided tours, and deterministic visual tests.
    pub fn inspect(&mut self, id: &str) -> bool {
        self.hovered = self.slices.iter().position(|slice| slice.id == id);
        self.keyboard_inspection = self.hovered.is_some();
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
    }
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }

    fn value(slice: &PieSlice) -> f64 {
        if slice.value.is_finite() {
            slice.value.max(0.0)
        } else {
            0.0
        }
    }
    fn total(&self) -> f64 {
        self.slices.iter().map(Self::value).sum()
    }
    fn animation_progress_at(&self, now: Instant) -> f64 {
        if self.reduce_motion
            || !self.style.animate_changes
            || self.transition_from.is_none()
            || self.style.animation_duration.is_zero()
        {
            return 1.0;
        }
        let raw = now
            .saturating_duration_since(self.transition_started)
            .as_secs_f64()
            / self.style.animation_duration.as_secs_f64();
        crate::animation::ease_out(raw.clamp(0.0, 1.0))
    }
    fn animated_value(&self, index: usize, progress: f64) -> f64 {
        let Some(target) = self.slices.get(index) else {
            return 0.0;
        };
        let target_value = Self::value(target);
        if progress >= 1.0 {
            return target_value;
        }
        let previous = self
            .transition_from
            .as_ref()
            .and_then(|slices| slices.iter().find(|slice| slice.id == target.id))
            .map_or(0.0, Self::value);
        previous + (target_value - previous) * progress
    }
    fn sectors_at(&self, now: Instant) -> Vec<Sector> {
        self.sectors_for_progress(self.animation_progress_at(now))
    }
    fn sectors_for_progress(&self, progress: f64) -> Vec<Sector> {
        let values = (0..self.slices.len())
            .map(|index| self.animated_value(index, progress))
            .collect::<Vec<_>>();
        let total: f64 = values.iter().sum();
        if total <= f64::EPSILON {
            return Vec::new();
        }
        let mut angle = self.style.start_angle;
        values
            .into_iter()
            .enumerate()
            .filter_map(|(index, value)| {
                if value <= f64::EPSILON {
                    return None;
                }
                let sweep = value / total * TAU;
                let sector = Sector {
                    index,
                    start: angle,
                    end: angle + sweep,
                };
                angle += sweep;
                Some(sector)
            })
            .collect()
    }
    fn geometry(&self) -> (Point, f64, f64) {
        let area = if self.chart_rect == Rect::ZERO {
            self.rect
        } else {
            self.chart_rect
        };
        let center = area.center();
        let outer = (area.width().min(area.height()) / 2.0 - self.style.padding).max(1.0);
        (
            center,
            outer,
            outer * self.style.inner_radius.clamp(0.0, 0.92),
        )
    }

    fn hit_test(&self, point: Point) -> Option<usize> {
        let (center, outer, inner) = self.geometry();
        let dx = point.x - center.x;
        let dy = point.y - center.y;
        let radius = dx.hypot(dy);
        if radius < inner || radius > outer + self.style.hover_offset {
            return None;
        }
        let angle =
            (dy.atan2(dx) - self.style.start_angle).rem_euclid(TAU) + self.style.start_angle;
        self.sectors_at(crate::time::now())
            .into_iter()
            .find(|s| angle >= s.start && angle < s.end)
            .map(|s| s.index)
    }

    fn wedge(center: Point, inner: f64, outer: f64, start: f64, end: f64) -> BezPath {
        let mut path = BezPath::new();
        let sweep = end - start;
        let polar = |r: f64, a: f64| Point::new(center.x + r * a.cos(), center.y + r * a.sin());
        path.move_to(polar(outer, start));
        for step in 1..=WEDGE_STEPS {
            let a = start + sweep * f64::from(step) / f64::from(WEDGE_STEPS);
            path.line_to(polar(outer, a));
        }
        if inner <= 0.0 {
            path.line_to(center);
        } else {
            path.line_to(polar(inner, end));
            for step in 1..=WEDGE_STEPS {
                let a = end - sweep * f64::from(step) / f64::from(WEDGE_STEPS);
                path.line_to(polar(inner, a));
            }
        }
        path.close_path();
        path
    }

    fn draw_tooltip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(slice) = self.hovered().filter(|_| self.style.tooltip) else {
            return;
        };
        let total = self.total();
        let slice_anchor = || {
            self.sectors_at(crate::time::now())
                .into_iter()
                .find(|sector| self.hovered == Some(sector.index))
                .map_or_else(
                    || self.rect.center(),
                    |sector| {
                        let (center, outer, inner) = self.geometry();
                        let angle = sector.start.midpoint(sector.end);
                        let radius = inner.midpoint(outer);
                        Point::new(
                            center.x + radius * angle.cos(),
                            center.y + radius * angle.sin(),
                        )
                    },
                )
        };
        let anchor = match self.style.tooltip_anchor {
            PieTooltipAnchor::Pointer if self.rect.contains(self.pointer) => self.pointer,
            PieTooltipAnchor::Pointer | PieTooltipAnchor::Slice => slice_anchor(),
        };
        let title_style = theme.text(TextRole::Caption);
        let body_style = theme.text(TextRole::Caption);
        let detail = self.tooltip_detail(slice, total);
        let (tw, _) = tr.measure_style(&slice.label, None, &title_style);
        let (dw, _) = tr.measure_style(&detail, None, &body_style);
        let padding = self.style.tooltip_padding.clamp(8.0, 32.0);
        let available_width =
            (self.rect.width() - self.style.tooltip_right_inset.max(0.0) - 16.0).max(80.0);
        let width = self
            .style
            .tooltip_width
            .unwrap_or_else(|| {
                f64::from(tw.max(dw)).max(self.style.tooltip_min_width) + padding * 2.0
            })
            .clamp(80.0, available_width);
        let height = 38.0 + padding * 2.0;
        let safe_right = (self.rect.x1 - self.style.tooltip_right_inset.max(0.0))
            .max(self.rect.x0 + width + 8.0);
        let offset = self.style.tooltip_offset.max(4.0);
        let mut x = anchor.x + offset;
        let mut y = anchor.y + offset;
        if x + width > safe_right {
            x = safe_right - width - 8.0;
        }
        x = x.max(self.rect.x0 + 8.0);
        if y + height > self.rect.y1 {
            y = anchor.y - height - offset;
        }
        let (center, _, inner) = self.geometry();
        let protected = inner * 0.58;
        if x < center.x + protected
            && x + width > center.x - protected
            && y < center.y + protected
            && y + height > center.y - protected
        {
            y = (center.y - protected - height - 8.0).max(self.rect.y0 + 8.0);
        }
        y = y.clamp(
            self.rect.y0 + 8.0,
            (self.rect.y1 - height - 8.0).max(self.rect.y0 + 8.0),
        );
        // Never over the legend: try the other side of the anchor, then
        // above it, then above and to the other side.
        let covers = |r: Rect| {
            self.legend_rects
                .iter()
                .any(|legend| r.intersect(*legend).area() > 0.0)
        };
        let mut rect = Rect::new(x, y, x + width, y + height);
        if covers(rect) {
            let left = (anchor.x - offset - width).max(self.rect.x0 + 8.0);
            let above = (anchor.y - offset - height).max(self.rect.y0 + 8.0);
            if let Some(free) = [
                Rect::new(left, y, left + width, y + height),
                Rect::new(x, above, x + width, above + height),
                Rect::new(left, above, left + width, above + height),
            ]
            .into_iter()
            .find(|candidate| !covers(*candidate))
            {
                rect = free;
            }
        }
        let (x, y) = (rect.x0, rect.y0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
        );
        tr.draw_style(
            scene,
            &slice.label,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x + padding, y + padding - 2.0)),
            Some(crate::num::f32_of_f64((width - padding * 2.0).max(1.0))),
            &title_style.clone().with_weight(TextWeight::Semibold),
        );
        tr.draw_style(
            scene,
            &detail,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((x + padding, y + padding + 24.0)),
            Some(crate::num::f32_of_f64((width - padding * 2.0).max(1.0))),
            &body_style,
        );
    }

    fn tooltip_detail(&self, slice: &PieSlice, total: f64) -> String {
        let share = share_text(Self::value(slice), total);
        match (slice.detail.is_empty(), self.style.tooltip_show_percent) {
            (true, true) => share,
            (false, true) => format!("{}, {share}", slice.detail),
            (_, false) => slice.detail.clone(),
        }
    }
}

impl InteractiveChart for PieChart {
    fn interaction(&mut self) -> (&mut Point, &mut bool, &mut ChartTouchTracker) {
        (
            &mut self.pointer,
            &mut self.keyboard_inspection,
            &mut self.touch,
        )
    }
    fn update_hover(&mut self) {
        self.hovered = self.hit_test(self.pointer);
    }
    fn clear_hover(&mut self) {
        self.hovered = None;
    }
    fn activate_hovered(&mut self) -> bool {
        let Some(slice) = self.hovered.and_then(|index| self.slices.get(index)) else {
            return false;
        };
        if !slice.interactive {
            return false;
        }
        self.activated = Some(slice.id.clone());
        true
    }
    fn move_keyboard_inspection(&mut self, action: ChartNavAction) -> bool {
        let indices = self
            .sectors_at(crate::time::now())
            .into_iter()
            .map(|sector| sector.index)
            .collect::<Vec<_>>();
        let position = self
            .hovered
            .and_then(|index| indices.iter().position(|candidate| *candidate == index));
        self.hovered = moved_index(position, indices.len(), action)
            .and_then(|position| indices.get(position).copied());
        self.hovered.is_some()
    }
}

impl Widget for PieChart {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: self.height.map_or_else(Dimension::auto, Dimension::length),
            },
            // Without a fixed height the chart takes the room it is given.
            flex_grow: if self.height.is_some() { 0.0 } else { 1.0 },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            _ => 320.0,
        });
        let height = match (self.height, &self.theme) {
            (Some(height), _) => height,
            (None, Some(theme)) => {
                let theme = theme.clone();
                crate::num::f32_of_f64(self.natural_height(text, &theme, f64::from(width)))
            }
            (None, None) => 320.0,
        };
        taffy::Size {
            width,
            height: known.height.unwrap_or(height),
        }
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }
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
        let (chart_rect, legend) = self.legend_layout(tr, theme, self.rect);
        self.chart_rect = chart_rect;
        let (base_center, outer, inner) = self.geometry();
        let sectors = self.sectors_at(now);
        if sectors.is_empty() {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &Circle::new(base_center, outer),
            );
        }
        for sector in sectors {
            let Some(slice) = self.slices.get(sector.index) else {
                continue;
            };
            let mid = sector.start.midpoint(sector.end);
            let lift = if self.hovered == Some(sector.index) && slice.interactive {
                self.style.hover_offset
            } else {
                0.0
            };
            let center = Point::new(
                base_center.x + lift * mid.cos(),
                base_center.y + lift * mid.sin(),
            );
            let half_gap = (self.style.gap / 2.0).min((sector.end - sector.start) * 0.2);
            scene.fill(
                Fill::NonZero,
                transform,
                slice.color,
                None,
                &Self::wedge(
                    center,
                    inner,
                    outer,
                    sector.start + half_gap,
                    sector.end - half_gap,
                ),
            );
        }
        self.legend_rects = legend.iter().map(|(_, rect)| *rect).collect();
        if !legend.is_empty() {
            let total = self.total();
            let detail_w = self.legend_detail_width(tr, theme);
            let detail_style = theme.text(TextRole::Caption).numeric();
            for (index, rect) in &legend {
                let Some(slice) = self.slices.get(*index) else {
                    continue;
                };
                let label_rect = Rect::new(rect.x0, rect.y0, rect.x1 - detail_w, rect.y1);
                crate::widget::chart_legend::paint_entry(
                    scene,
                    transform,
                    tr,
                    theme,
                    label_rect,
                    slice.color,
                    &legend_label(slice, total),
                );
                if detail_w > 0.0 && !slice.legend_detail.is_empty() {
                    let (w, h) = tr.measure_style(&slice.legend_detail, None, &detail_style);
                    tr.draw_style(
                        scene,
                        &slice.legend_detail,
                        theme.text_dim,
                        transform
                            * Affine::translate((
                                rect.x1 - f64::from(w),
                                rect.center().y - f64::from(h) / 2.0,
                            )),
                        None,
                        &detail_style,
                    );
                }
            }
        }
        let title_style = theme.text(TextRole::LargeTitle);
        let (w, h) = tr.measure_style(
            &self.center_title,
            None,
            &title_style.clone().with_weight(TextWeight::Bold),
        );
        tr.draw_style(
            scene,
            &self.center_title,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((
                    base_center.x - f64::from(w) / 2.0,
                    base_center.y - f64::from(h),
                )),
            None,
            &title_style.clone().with_weight(TextWeight::Bold),
        );
        let sub_style = theme.text(TextRole::Caption);
        let (sw, _) = tr.measure_style(&self.center_subtitle, None, &sub_style);
        tr.draw_style(
            scene,
            &self.center_subtitle,
            theme.tokens.colors.text_dim,
            transform
                * Affine::translate((base_center.x - f64::from(sw) / 2.0, base_center.y + 8.0)),
            None,
            &sub_style,
        );
        self.draw_tooltip(scene, transform, tr, theme);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Keys inspect the pie only while it has focus; otherwise they belong
        // to whatever does.
        if self.focused || chart_nav_action(event).is_none() {
            handle_chart_event(self, event, ctx);
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let total = self.total();
        let children = self
            .slices
            .iter()
            .filter(|s| Self::value(s) > 0.0)
            .map(|s| {
                let share = share_text(Self::value(s), total);
                let description = [share.as_str(), s.legend_detail.as_str(), s.detail.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
                let node = AccessNode::new(if s.interactive {
                    Role::Button
                } else {
                    Role::StaticText
                })
                .with_name(&s.label)
                .with_description(description);
                if s.interactive {
                    node.with_action_target(&s.id).with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
                } else {
                    node
                }
            })
            .collect();
        Some(
            AccessNode::new(Role::Canvas)
                .with_name("Chart")
                .with_children(children),
        )
    }
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let Some(id) = crate::widget::chart_navigation::activated_id(
            action,
            key_path,
            self.slices.iter().filter(|s| s.interactive).map(|s| &s.id),
        ) else {
            return false;
        };
        self.activated = Some(id.clone());
        true
    }
    fn name(&self) -> &'static str {
        "PieChart"
    }
    fn get_rect(&self) -> Rect {
        self.rect
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
            self.clear_inspection();
        }
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

/// A share as the legend and the tooltip both state it: whole percent, and
/// "<1%" for a sliver rather than a misleading "0%".
fn share_text(value: f64, total: f64) -> String {
    if total <= 0.0 {
        return "0%".to_owned();
    }
    let percent = value / total * 100.0;
    if percent > 0.0 && percent < 0.5 {
        "<1%".to_owned()
    } else {
        format!("{}%", percent.round())
    }
}

/// A legend entry: the slice's label and its share, as a secondary line
/// would put it ("Photos 42%"), never joined by a dot.
fn legend_label(slice: &PieSlice, total: f64) -> String {
    if total > 0.0 {
        format!(
            "{} {}",
            slice.label,
            share_text(PieChart::value(slice), total)
        )
    } else {
        slice.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legend_never_overlaps_the_wedges() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let slices = vec![
            PieSlice::new("photos", "Photos", 42.0, Color::WHITE),
            PieSlice::new("music", "Music", 21.0, Color::WHITE),
            PieSlice::new("documents", "Documents and spreadsheets", 9.0, Color::WHITE),
            PieSlice::new("other", "Other", 4.0, Color::WHITE),
        ];
        for rect in [
            Rect::new(0.0, 0.0, 640.0, 300.0),
            Rect::new(0.0, 0.0, 300.0, 420.0),
        ] {
            let mut chart = PieChart::new(slices.clone()).with_legend();
            chart.rect = rect;
            let (area, legend) = chart.legend_layout(&mut text, &theme, rect);
            chart.chart_rect = area;
            let (center, outer, _) = chart.geometry();
            let disc = Rect::from_center_size(center, (2.0 * outer, 2.0 * outer));
            assert_eq!(legend.len(), 4);
            for (_, entry) in &legend {
                assert!(
                    disc.intersect(*entry).area() <= 0.0,
                    "{rect:?}: legend {entry:?} overlaps the chart {disc:?}"
                );
                assert!(rect.contains(entry.origin()) && entry.x1 <= rect.x1 + 0.5);
            }
        }
        assert_eq!(legend_label(&slices[0], 76.0), "Photos 55%");
    }

    fn chart() -> PieChart {
        let mut chart = PieChart::new(vec![
            PieSlice::new("a", "A", 3.0, Color::WHITE),
            PieSlice::new("b", "B", 1.0, Color::BLACK),
        ]);
        chart.rect = Rect::new(0.0, 0.0, 200.0, 200.0);
        chart
    }

    #[test]
    fn pie_geometry_covers_a_turn_and_ignores_holes_and_invalid_values() {
        // Case `sectors_cover_a_complete_turn`.
        {
            let sectors = chart().sectors_for_progress(1.0);
            assert_eq!(sectors.len(), 2);
            let sweep: f64 = sectors.iter().map(|sector| sector.end - sector.start).sum();
            assert!((sweep - TAU).abs() < 1e-9);
        }

        // Case `donut_hole_is_not_interactive`.
        {
            let chart = chart();
            assert_eq!(chart.hit_test(chart.rect.center()), None);
        }

        // Case `invalid_values_do_not_poison_the_total`.
        {
            let chart = PieChart::new(vec![
                PieSlice::new("negative", "Negative", -9.0, Color::WHITE),
                PieSlice::new("nan", "NaN", f64::NAN, Color::WHITE),
                PieSlice::new("valid", "Valid", 4.0, Color::WHITE),
            ]);
            assert_eq!(chart.total(), 4.0);
            assert_eq!(chart.sectors_for_progress(1.0).len(), 1);
        }
    }

    #[test]
    fn pie_inspection_and_tooltips_are_configurable_and_not_actions() {
        // Case `inspect_only_slices_are_not_accessibility_actions`.
        {
            let chart = PieChart::new(vec![
                PieSlice::new("files", "Files", 4.0, Color::WHITE).non_interactive(),
            ]);
            let accessibility = chart
                .accessibility()
                .unwrap_or_else(|| AccessNode::new(Role::Canvas));
            assert_eq!(accessibility.children[0].role, Role::StaticText);
            assert!(!accessibility.children[0].state.focusable);
        }

        // Case `tooltip_presentation_is_fully_configurable`.
        {
            let style = PieStyle {
                tooltip: false,
                tooltip_anchor: PieTooltipAnchor::Slice,
                tooltip_width: Some(240.0),
                tooltip_min_width: 120.0,
                tooltip_offset: 8.0,
                tooltip_padding: 20.0,
                tooltip_show_percent: false,
                tooltip_right_inset: 180.0,
                ..PieStyle::default()
            };
            assert!(!style.tooltip);
            assert_eq!(style.tooltip_anchor, PieTooltipAnchor::Slice);
            assert_eq!(style.tooltip_width, Some(240.0));
            assert!(!style.tooltip_show_percent);
            assert_eq!(style.tooltip_right_inset, 180.0);
        }
    }

    #[test]
    fn pie_updates_interpolate_by_stable_id_or_snap() {
        // Case `stable_slice_ids_interpolate_and_reduced_motion_snaps`.
        {
            let mut chart = PieChart::new(vec![
                PieSlice::new("a", "A", 3.0, Color::WHITE),
                PieSlice::new("b", "B", 1.0, Color::BLACK),
            ]);
            chart.style.animation_duration = Duration::from_millis(200);
            chart.set_slices(vec![
                PieSlice::new("a", "A", 1.0, Color::WHITE),
                PieSlice::new("b", "B", 3.0, Color::BLACK),
                PieSlice::new("c", "C", 2.0, Color::from_rgb8(1, 2, 3)),
            ]);
            let midpoint = crate::time::now();
            chart.transition_started = midpoint.checked_sub(Duration::from_millis(100)).unwrap();
            let progress = chart.animation_progress_at(midpoint);
            assert!(chart.animated_value(0, progress) > 1.0);
            assert!(chart.animated_value(0, progress) < 3.0);
            assert!(chart.animated_value(2, progress) > 0.0);
            assert!(chart.animated_value(2, progress) < 2.0);
            let sweep: f64 = chart
                .sectors_for_progress(progress)
                .iter()
                .map(|sector| sector.end - sector.start)
                .sum();
            assert!((sweep - TAU).abs() < 1e-9);
            assert!(chart.needs_animation_frame());
            chart.reduce_motion = true;
            assert_eq!(chart.animation_progress_at(midpoint), 1.0);
            assert!(!chart.needs_animation_frame());
        }

        // Case `identical_updates_do_not_restart_motion_and_immediate_updates_snap`.
        {
            let slices = vec![PieSlice::new("a", "A", 1.0, Color::WHITE)];
            let mut chart = PieChart::new(slices.clone());
            let started = chart.transition_started;
            chart.set_slices(slices);
            assert_eq!(chart.transition_started, started);
            assert!(!chart.is_animating());
            chart.set_slices_immediate(vec![PieSlice::new("a", "A", 2.0, Color::WHITE)]);
            assert!(!chart.is_animating());
            assert_eq!(chart.slices[0].value, 2.0);
        }
    }

    #[test]
    fn legend_and_tooltip_state_a_share_alike_and_details_line_up() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut chart = PieChart::new(vec![
            PieSlice::new("photos", "Photos", 42.4, Color::WHITE)
                .with_legend_detail("42.4 GB")
                .with_detail("1,204 files"),
            PieSlice::new("music", "Music", 57.6, Color::WHITE).with_legend_detail("57.6 GB"),
        ])
        .with_legend();
        assert_eq!(legend_label(&chart.slices[0], 100.0), "Photos 42%");
        let tooltip = chart.tooltip_detail(&chart.slices[0].clone(), 100.0);
        assert_eq!(tooltip, "1,204 files, 42%");
        assert!(!tooltip.contains('\u{b7}'), "no dot chains");
        let rect = Rect::new(0.0, 0.0, 640.0, 300.0);
        chart.rect = rect;
        let (area, legend) = chart.legend_layout(&mut text, &theme, rect);
        // Beside: donut and legend centred as one unit.
        let left_gap = area.x0 - rect.x0;
        let right_gap = rect.x1 - legend.iter().map(|(_, r)| r.x1).fold(0.0, f64::max);
        assert!(
            (left_gap - right_gap).abs() < 2.0,
            "{left_gap} vs {right_gap}"
        );
        let widths: Vec<f64> = legend.iter().map(|(_, r)| r.width()).collect();
        assert!(
            (widths[0] - widths[1]).abs() < 0.5,
            "details share a column"
        );
        // Without a fixed height the chart asks for room from its width.
        let natural = chart.natural_height(&mut text, &theme, 640.0);
        assert!(natural > 100.0 && natural < 640.0, "{natural}");
        assert_eq!(share_text(0.2, 100.0), "<1%");
    }
}
