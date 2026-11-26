//! Interactive pie and donut charts. The widget owns interaction geometry as well as painting,
//! so pointer, keyboard/accessibility activation, tooltips and the rendered wedges always
//! agree.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "chart geometry is bounded floating-point arithmetic"
)]

use crate::time::{Duration, Instant};
use std::f64::consts::{FRAC_PI_2, TAU};

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{Color, Fill};

use crate::widget::chart_geometry;
use crate::widget::chart_readout::Readout;
use crate::widget::plot::{Plot, PlotFrame, PlotItem, PlotMarks};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{
    motion::Direction, motion::MotionSpec, motion::MotionValue, motion::Pace, motion::Reduced,
    motion::Shortcut,
};
use tilo_ui_base::{text_render::TextRenderer, text_render::TextWeight};
use tilo_ui_base::{tokens::EffectiveMotion, tokens::MotionSource, tokens::TextRole};

/// A stable, labelled radial datum. Negative and non-finite values are treated
/// as zero. `id` survives sorting and updates and is returned on activation.
#[derive(Clone, Debug, PartialEq)]
pub struct PieSlice {
    pub id: String,
    pub label: String,
    pub value: f64,
    pub color: Color,
    /// Its place in the theme's palette, when it is coloured by that and not by `color`: the
    /// colour then follows the theme the slice is drawn with.
    pub palette: Option<usize>,
    pub detail: String,
    /// A column after the legend's label and share: the slice's size
    /// ("12.4 GB"). Empty for none.
    pub legend_detail: String,
    pub interactive: bool,
}

impl PieSlice {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: f64, color: Color) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            color,
            palette: None,
            detail: String::new(),
            legend_detail: String::new(),
            interactive: true,
        }
    }

    /// A slice in the colour the theme's palette has at `index`, whichever theme draws it.
    pub fn from_palette(
        id: impl Into<String>,
        label: impl Into<String>,
        value: f64,
        index: usize,
    ) -> Self {
        Self {
            palette: Some(index),
            ..Self::new(id, label, value, Color::TRANSPARENT)
        }
    }

    fn color_in(&self, theme: &Theme) -> Color {
        self.palette.map_or(self.color, |index| {
            tilo_ui_base::chart_palette::ChartPalette::from_theme(theme).categorical(index)
        })
    }

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

    #[must_use]
    pub fn non_interactive(mut self) -> Self {
        self.interactive = false;
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PieStyle {
    /// Hole radius divided by outer radius. `0` is a pie, `0.62` is a donut.
    pub inner_radius: f64,
    pub gap: f64,
    /// Rotation in radians; the default starts at twelve o'clock.
    pub start_angle: f64,
    pub hover_offset: f64,
    pub padding: f64,
    pub tooltip_anchor: PieTooltipAnchor,
    pub tooltip: bool,
    pub tooltip_show_percent: bool,
    /// Move matching stable slice ids to their new values after
    /// `set_slices`, on the standard list change (`motion::ListChange`).
    pub animate_changes: bool,
}

impl Default for PieStyle {
    fn default() -> Self {
        Self {
            inner_radius: 0.62,
            gap: 0.018,
            start_angle: -FRAC_PI_2,
            hover_offset: 7.0,
            padding: 14.0,
            tooltip_anchor: PieTooltipAnchor::Pointer,
            tooltip: true,
            tooltip_show_percent: true,
            animate_changes: true,
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

pub struct PieMarks {
    slices: Vec<PieSlice>,
    style: PieStyle,
    center_title: String,
    center_subtitle: String,
    chart_rect: Rect,
    /// The legend's entries as the layout placed them: the slice's index and
    /// its rect (swatch, label and share, then its detail column).
    legend_entries: Vec<(usize, Rect)>,
    legend_detail_w: f64,
    frame: PlotFrame<usize>,
    transition_from: Option<Vec<PieSlice>>,
    transition_started: Instant,
    /// Each slice's rise toward the pointer, from none to the whole `hover_offset`, on the shared
    /// spring: a slice lifts and sinks as the pointer comes and goes, never in one jump.
    lifts: Vec<MotionValue<f64>>,
    reduce_motion: bool,
    legend: bool,
    /// What a slice's value counts, written after it in the readout ("GB", "h").
    unit: String,
}

pub type PieChart = Plot<PieMarks>;

mod pie_chart_component;
pub use pie_chart_component::{PieChartProps, PiePart, slice_description};

impl PieChart {
    pub fn new(slices: Vec<PieSlice>) -> Self {
        Plot::from_marks(PieMarks {
            slices,
            style: PieStyle::default(),
            center_title: String::new(),
            center_subtitle: String::new(),
            chart_rect: Rect::ZERO,
            legend_entries: Vec::new(),
            legend_detail_w: 0.0,
            frame: PlotFrame::fill(80.0),
            transition_from: None,
            transition_started: crate::time::now(),
            lifts: Vec::new(),
            reduce_motion: false,
            legend: false,
            unit: String::new(),
        })
    }

    pub fn set_slices(&mut self, slices: Vec<PieSlice>) {
        self.mark_dirty();
        self.marks.set_slices(slices);
    }
    /// Replace data without motion, useful for initial hydration, restored
    /// sessions, and deterministic capture.
    pub fn set_slices_immediate(&mut self, slices: Vec<PieSlice>) {
        self.mark_dirty();
        self.marks.set_slices_immediate(slices);
    }
    pub fn is_animating(&self) -> bool {
        self.marks.is_animating()
    }
    pub fn set_center(&mut self, title: impl Into<String>, subtitle: impl Into<String>) {
        self.mark_dirty();
        self.marks.set_center(title, subtitle);
    }
    pub fn hovered(&self) -> Option<&PieSlice> {
        self.marks.hovered()
    }
    /// Name every slice in a legend (a swatch, its label and its share) beside the chart when
    /// it fits, under it otherwise.
    #[must_use]
    pub fn with_legend(mut self) -> Self {
        self.marks.legend = true;
        self
    }
    #[must_use]
    pub fn with_style(mut self, style: PieStyle) -> Self {
        self.marks.style = style;
        self
    }
    #[must_use]
    pub fn with_center(mut self, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        self.marks.set_center(title, subtitle);
        self
    }
    pub fn finish_animation(&mut self) {
        self.marks.transition_from = None;
    }
    /// Inspect a slice by stable ID. Useful for keyboard navigation,
    /// coordinated views, guided tours, and deterministic visual tests.
    pub fn inspect(&mut self, id: &str) -> bool {
        let slice = self.marks.slices.iter().position(|slice| slice.id == id);
        self.inspect_item(slice)
    }
}

impl PieMarks {
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

    /// Lay the donut and the legend out inside `rect`, the donut on its leading edge with the
    /// legend beside it (or centred under it where it is too narrow), and return the rect for the wedges, with each legend entry's rect
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
            if detail_w > 0.0 {
                entry.width = label_w + detail_w;
            }
        }
        let gap = f64::from(theme.tokens.spacing.rhythm.section);
        let widest = entries.iter().map(|e| e.width).fold(0.0, f64::max);
        let row = entries.first().map_or(0.0, |e| e.height);
        let side_width = rect.width() - rect.height().min(rect.width() * 0.6);
        if widest + gap <= side_width {
            let diameter = (rect.width() - widest - gap).min(rect.height()).max(0.0);
            // The donut starts on the chart's edge, where its title and the words around it do.
            let x0 = rect.x0;
            let legend_x = x0 + diameter + gap;
            let block = tilo_ui_base::num::f64_of_usize(entries.len()) * row;
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
        let title = crate::widget::chart_navigation::title_band(theme, &self.frame.title);
        let line = f64::from(theme.text(TextRole::Body).line_height_px());
        let diameter = (line * 14.0).min(width).max(line * 6.0);
        if !self.legend || self.slices.is_empty() {
            return title + diameter;
        }
        let probe = Rect::new(0.0, 0.0, width, diameter);
        let (_, items) = self.legend_layout(text, theme, probe);
        // Beside the donut, the legend starts on its top band; under it, past its foot.
        let beside = items.first().is_some_and(|(_, r)| r.y0 < diameter);
        if beside {
            let block = items.iter().map(|(_, r)| r.y1).fold(0.0, f64::max)
                - items
                    .iter()
                    .map(|(_, r)| r.y0)
                    .fold(f64::INFINITY, f64::min);
            title + diameter.max(block)
        } else {
            let rows = items.iter().map(|(_, r)| r.y1).fold(0.0, f64::max)
                - items
                    .iter()
                    .map(|(_, r)| r.y0)
                    .fold(f64::INFINITY, f64::min);
            title + diameter + f64::from(theme.tokens.spacing.rhythm.section) + rows.max(0.0)
        }
    }

    /// Lay the donut and the legend out inside the frame's rect, under its title, measured in
    /// the theme's own type.
    fn place(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let mut area = self.frame.rect;
        area.y0 = (area.y0 + crate::widget::chart_navigation::title_band(theme, &self.frame.title))
            .min(area.y1);
        let (chart_rect, legend) = self.legend_layout(text, theme, area);
        self.chart_rect = chart_rect;
        self.legend_detail_w = self.legend_detail_width(text, theme);
        self.legend_entries = legend;
    }

    fn set_slices(&mut self, slices: Vec<PieSlice>) {
        if self.slices == slices {
            return;
        }
        let hovered_id = self.hovered().map(|slice| slice.id.clone());
        let previous = std::mem::replace(&mut self.slices, slices);
        if self.style.animate_changes && !self.reduce_motion {
            self.transition_from = Some(previous);
            self.transition_started = crate::time::now();
        } else {
            self.transition_from = None;
        }
        self.frame.inspection.hovered =
            hovered_id.and_then(|id| self.slices.iter().position(|slice| slice.id == id));
        self.lifts.truncate(self.slices.len());
    }
    /// Replace data without motion, useful for initial hydration, restored
    /// sessions, and deterministic capture.
    fn set_slices_immediate(&mut self, slices: Vec<PieSlice>) {
        let hovered_id = self.hovered().map(|slice| slice.id.clone());
        self.slices = slices;
        self.transition_from = None;
        self.frame.inspection.hovered =
            hovered_id.and_then(|id| self.slices.iter().position(|slice| slice.id == id));
        self.lifts.truncate(self.slices.len());
    }
    fn is_animating(&self) -> bool {
        (!self.reduce_motion
            && self.style.animate_changes
            && self.transition_from.is_some()
            && crate::time::now().saturating_duration_since(self.transition_started)
                < data_change_time())
            || self
                .lifts
                .iter()
                .any(|lift| lift.is_moving(tilo_ui_base::motion::frame_now()))
    }
    /// Each slice's lift toward the slice the pointer is on, on the shared spring: a slice rises
    /// as the pointer reaches it and sinks as the pointer leaves, at once when motion is reduced
    /// or the theme holds still.
    fn advance_lifts(&mut self, now: Instant, motion: EffectiveMotion) {
        self.lifts
            .resize_with(self.slices.len(), || MotionValue::new(0.0));
        let spec = MotionSpec {
            enter: Pace::Fast,
            exit: Pace::Fast,
            reduced: Reduced::Lands,
            shortcut: Shortcut::Lands,
        };
        let source = if self.frame.inspection.keyboard {
            MotionSource::Keyboard
        } else {
            MotionSource::Pointer
        };
        let hovered = self.frame.inspection.hovered;
        for (index, lift) in self.lifts.iter_mut().enumerate() {
            let on = hovered == Some(index)
                && self
                    .slices
                    .get(index)
                    .is_some_and(|slice| slice.interactive);
            let timing = spec.timing(Direction::of(on), motion, source);
            lift.set(if on { 1.0 } else { 0.0 }, timing, now);
        }
    }
    /// What of the inspection the picture itself shows: the slice lifted under the pointer and
    /// the legend entry beside it. Its lift is drawn into the picture, so the page redraws the
    /// picture while the slice is still moving.
    fn shown_inspection_epoch(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.frame.inspection.hovered.hash(&mut hash);
        // Where each lift stands now, not where it was last drawn: the page takes the picture
        // again while a slice is still moving, so the spring advances a frame at a time there
        // as it does where the frame itself draws it.
        let now = tilo_ui_base::motion::frame_now();
        for lift in &self.lifts {
            lift.get(now).to_bits().hash(&mut hash);
        }
        hash.finish()
    }
    fn set_center(&mut self, title: impl Into<String>, subtitle: impl Into<String>) {
        self.center_title = title.into();
        self.center_subtitle = subtitle.into();
    }
    fn hovered(&self) -> Option<&PieSlice> {
        self.frame
            .inspection
            .hovered
            .and_then(|i| self.slices.get(i))
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
        if self.reduce_motion || !self.style.animate_changes || self.transition_from.is_none() {
            return 1.0;
        }
        data_change_progress(now.saturating_duration_since(self.transition_started))
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
        let area = self.chart_rect;
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
        self.sectors_at(tilo_ui_base::motion::frame_now())
            .into_iter()
            .find(|s| angle >= s.start && angle < s.end)
            .map(|s| s.index)
    }

    /// Where slice `index` reads out from: the middle of its ring.
    fn slice_anchor(&self, index: usize) -> Option<Point> {
        let sector = self
            .sectors_at(tilo_ui_base::motion::frame_now())
            .into_iter()
            .find(|sector| sector.index == index)?;
        let (center, outer, inner) = self.geometry();
        let angle = sector.start.midpoint(sector.end);
        let radius = inner.midpoint(outer);
        Some(Point::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        ))
    }

    /// The slice whose legend entry `point` is on.
    fn legend_hit(&self, point: Point) -> Option<usize> {
        self.legend_entries
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(index, _)| *index)
            .filter(|index| {
                self.slices
                    .get(*index)
                    .is_some_and(|s| Self::value(s) > 0.0)
            })
    }

    /// A slice's value as the readout says it: the value in its unit, then its share of the
    /// whole.
    fn tooltip_detail(&self, slice: &PieSlice, total: f64) -> String {
        let value = crate::widget::chart_readout::number(slice.value, &self.unit);
        if self.style.tooltip_show_percent {
            format!("{value} ({})", share_text(Self::value(slice), total))
        } else {
            value
        }
    }
}

impl PlotMarks for PieMarks {
    type Hover = usize;
    type Activated = String;
    const NAME: &'static str = "PieChart";
    const LABEL: &'static str = "Chart";

    fn frame(&self) -> &PlotFrame<usize> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<usize> {
        &mut self.frame
    }
    fn natural_height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        self.natural_height(text, theme, width)
    }
    fn place(&mut self, text: &mut TextRenderer, theme: &Theme) {
        self.place(text, theme);
    }
    /// The slice under the pointer, or the one whose legend entry it is on.
    fn hit(&self, point: Point) -> Option<usize> {
        self.hit_test(point).or_else(|| self.legend_hit(point))
    }
    fn activation(&self, &index: &usize) -> Option<String> {
        let slice = self.slices.get(index)?;
        slice.interactive.then(|| slice.id.clone())
    }
    fn keyboard_items(&self) -> Vec<usize> {
        let sectors = self.sectors_at(tilo_ui_base::motion::frame_now());
        sectors.into_iter().map(|sector| sector.index).collect()
    }
    /// Keyboard inspection leaves the pointer alone: the tooltip places itself
    /// per [`PieTooltipAnchor`].
    fn anchor(&self, _index: &usize) -> Option<Point> {
        None
    }
    fn items(&self) -> Vec<PlotItem<String>> {
        let total = self.total();
        self.slices
            .iter()
            .filter(|s| Self::value(s) > 0.0)
            .map(|s| {
                let share = share_text(Self::value(s), total);
                let description = [share.as_str(), s.legend_detail.as_str(), s.detail.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
                PlotItem {
                    target: s.id.clone(),
                    name: s.label.clone(),
                    description,
                    activates: s.interactive.then(|| s.id.clone()),
                }
            })
            .collect()
    }
    fn is_animating(&self) -> bool {
        self.is_animating()
    }
    fn shown_inspection(&self) -> u64 {
        self.shown_inspection_epoch()
    }
    /// The slice's name, its value and its share, beside the pointer on the slice, or beside
    /// the slice when the keyboard or its legend entry chose it, clear of the legend.
    fn readout(&self, theme: &Theme) -> Option<Readout> {
        let index = self
            .frame
            .inspection
            .hovered
            .filter(|_| self.style.tooltip)?;
        let slice = self.slices.get(index)?;
        let pointer = self.frame.inspection.pointer;
        let on_slice = self.hit_test(pointer) == Some(index);
        let anchor = match self.style.tooltip_anchor {
            PieTooltipAnchor::Pointer if on_slice && !self.frame.inspection.keyboard => pointer,
            PieTooltipAnchor::Pointer | PieTooltipAnchor::Slice => self.slice_anchor(index)?,
        };
        let mut readout = Readout::new("", anchor, self.frame.rect);
        readout.avoid = self.legend_entries.iter().map(|(_, rect)| *rect).collect();
        readout.value(
            Some(slice.color_in(theme)),
            slice.label.clone(),
            self.tooltip_detail(slice, self.total()),
            false,
        );
        readout.note(slice.detail.clone());
        Some(readout)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "wedges, legend, centre copy and tooltip share one paint pass"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.reduce_motion = theme.reduce_motion;
        crate::widget::chart_navigation::draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
        let now = tilo_ui_base::motion::frame_now();
        self.advance_lifts(
            now,
            EffectiveMotion::new(theme.tokens.motion, theme.reduce_motion),
        );
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
            let lift = self
                .lifts
                .get(sector.index)
                .map_or(0.0, |lift| lift.get(now).clamp(0.0, 1.0))
                * self.style.hover_offset;
            let center = Point::new(
                base_center.x + lift * mid.cos(),
                base_center.y + lift * mid.sin(),
            );
            let half_gap = (self.style.gap / 2.0).min((sector.end - sector.start) * 0.2);
            scene.fill(
                Fill::NonZero,
                transform,
                slice.color_in(theme),
                None,
                &chart_geometry::ring_sector(
                    center,
                    inner,
                    outer,
                    sector.start + half_gap,
                    sector.end - half_gap,
                ),
            );
        }
        if !self.legend_entries.is_empty() {
            let total = self.total();
            let detail_w = self.legend_detail_w;
            let detail_style = theme.text(TextRole::Caption).numeric();
            for (index, rect) in &self.legend_entries {
                let Some(slice) = self.slices.get(*index) else {
                    continue;
                };
                let label_rect = Rect::new(rect.x0, rect.y0, rect.x1 - detail_w, rect.y1);
                crate::widget::chart_legend::paint_entry_emphasised(
                    scene,
                    transform,
                    tr,
                    theme,
                    label_rect,
                    slice.color_in(theme),
                    &legend_label(slice, total),
                    self.frame.inspection.hovered == Some(*index),
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
            share_text(PieMarks::value(slice), total)
        )
    } else {
        slice.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_ui_base::widget::Widget;

    tilo_test_env::scenario! {
        fn a_pie_places_its_wedges_legend_and_tooltips;

        phase the_legend_never_overlaps_the_wedges;
        {
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
                chart.marks.frame.rect = rect;
                chart.marks.place(&mut text, &theme);
                let (center, outer, _) = chart.marks.geometry();
                let disc = Rect::from_center_size(center, (2.0 * outer, 2.0 * outer));
                assert_eq!(chart.marks.legend_entries.len(), 4);
                for (_, entry) in &chart.marks.legend_entries {
                    assert!(
                        disc.intersect(*entry).area() <= 0.0,
                        "{rect:?}: legend {entry:?} overlaps the chart {disc:?}"
                    );
                    assert!(rect.contains(entry.origin()) && entry.x1 <= rect.x1 + 0.5);
                }
            }
            assert_eq!(legend_label(&slices[0], 76.0), "Photos 55%");
        }

        phase pie_updates_interpolate_by_stable_id_or_snap;
        {
            {
                let mut chart = PieChart::new(vec![
                    PieSlice::new("a", "A", 3.0, Color::WHITE),
                    PieSlice::new("b", "B", 1.0, Color::BLACK),
                ]);
                chart.set_slices(vec![
                    PieSlice::new("a", "A", 1.0, Color::WHITE),
                    PieSlice::new("b", "B", 3.0, Color::BLACK),
                    PieSlice::new("c", "C", 2.0, Color::from_rgb8(1, 2, 3)),
                ]);
                let midpoint = crate::time::now();
                chart.marks.transition_started =
                    midpoint.checked_sub(Duration::from_millis(100)).unwrap();
                let progress = chart.marks.animation_progress_at(midpoint);
                assert!(chart.marks.animated_value(0, progress) > 1.0);
                assert!(chart.marks.animated_value(0, progress) < 3.0);
                assert!(chart.marks.animated_value(2, progress) > 0.0);
                assert!(chart.marks.animated_value(2, progress) < 2.0);
                let sweep: f64 = chart
                    .marks
                    .sectors_for_progress(progress)
                    .iter()
                    .map(|sector| sector.end - sector.start)
                    .sum();
                assert!((sweep - TAU).abs() < 1e-9);
                assert!(chart.needs_animation_frame());
                chart.marks.reduce_motion = true;
                assert_eq!(chart.marks.animation_progress_at(midpoint), 1.0);
                assert!(!chart.needs_animation_frame());
            }

            {
                let slices = vec![PieSlice::new("a", "A", 1.0, Color::WHITE)];
                let mut chart = PieChart::new(slices.clone());
                let started = chart.marks.transition_started;
                chart.set_slices(slices);
                assert_eq!(chart.marks.transition_started, started);
                assert!(!chart.is_animating());
                chart.set_slices_immediate(vec![PieSlice::new("a", "A", 2.0, Color::WHITE)]);
                assert!(!chart.is_animating());
                assert_eq!(chart.marks.slices[0].value, 2.0);
            }
        }

        phase legend_and_tooltip_state_a_share_alike_and_details_line_up;
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let mut chart = PieChart::new(vec![
                PieSlice::new("photos", "Photos", 42.4, Color::WHITE)
                    .with_legend_detail("42.4 GB")
                    .with_detail("1,204 files"),
                PieSlice::new("music", "Music", 57.6, Color::WHITE).with_legend_detail("57.6 GB"),
            ])
            .with_legend();
            assert_eq!(legend_label(&chart.marks.slices[0], 100.0), "Photos 42%");
            let tooltip = chart
                .marks
                .tooltip_detail(&chart.marks.slices[0].clone(), 100.0);
            assert_eq!(tooltip, "42.4 (42%)");
            assert!(!tooltip.contains('\u{b7}'), "no dot chains");
            let rect = Rect::new(0.0, 0.0, 640.0, 300.0);
            chart.marks.frame.rect = rect;
            chart.marks.place(&mut text, &theme);
            let legend = chart.marks.legend_entries.clone();
            let area = chart.marks.chart_rect;
            // Beside: the donut starts on the chart's edge, its legend after it.
            assert!((area.x0 - rect.x0).abs() < 0.5, "{area:?} starts on {rect:?}");
            assert!(legend.iter().all(|(_, r)| r.x0 > area.x1), "{legend:?} after {area:?}");
            let widths: Vec<f64> = legend.iter().map(|(_, r)| r.width()).collect();
            assert!(
                (widths[0] - widths[1]).abs() < 0.5,
                "details share a column"
            );
            let natural = chart.marks.natural_height(&mut text, &theme, 640.0);
            assert!(natural > 100.0 && natural < 640.0, "{natural}");
            assert_eq!(share_text(0.2, 100.0), "<1%");
        }

        phase a_legend_and_a_wedge_are_hit_where_the_layout_places_them;
        {
            let theme = Theme::dark();
            let chart = placed_pie(&theme);

            // A legend entry is no wedge, and pointing at it inspects its slice.
            let (entry, legend) = chart.marks.legend_entries[0];
            assert!(
                chart.marks.hit_test(legend.center()).is_none(),
                "{legend:?} is the legend, not a wedge"
            );
            assert_eq!(chart.marks.hit(legend.center()), Some(entry));
            let mut inspected = placed_pie(&theme);
            inspected.marks.frame.inspection.pointer = legend.center();
            crate::widget::chart_navigation::InteractiveChart::update_hover(&mut inspected);
            let readout = inspected.marks.readout(&theme).expect("its slice reads out");
            assert!(
                readout.anchor != legend.center()
                    && readout.avoid.contains(&legend),
                "it reads out at its slice, clear of the legend: {readout:?}"
            );

            let (center, outer, inner) = chart.marks.geometry();
            let sector = chart.marks.sectors_for_progress(1.0)[1];
            let angle = sector.start.midpoint(sector.end);
            let radius = inner.midpoint(outer);
            let point = Point::new(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
            );
            assert_eq!(
                chart.marks.hit(point),
                Some(sector.index),
                "the wedge drawn at {point:?} is the one a hover names"
            );
        }

        phase a_pie_slice_lifts_on_the_shared_spring;
        {
            let theme = Theme::dark();
            let mut chart = PieChart::new(vec![
                PieSlice::new("a", "A", 1.0, Color::WHITE),
                PieSlice::new("b", "B", 1.0, Color::BLACK),
            ]);
            let motion = EffectiveMotion::new(theme.tokens.motion, false);
            let start = crate::time::now();
            chart.marks.frame.inspection.hovered = Some(1);
            chart.marks.advance_lifts(start, motion);
            assert!(
                chart.marks.lifts[1].get(start).abs() < 1e-9,
                "the slice rises from where it stood"
            );
            let middle = start + Duration::from_millis(40);
            let level = chart.marks.lifts[1].get(middle);
            assert!(level > 0.05 && level < 0.95, "on its way: {level}");
            assert!(chart.marks.lifts[1].is_moving(middle));
            assert!(chart.needs_animation_frame(), "a rising slice asks for frames");
            let rest = start + Duration::from_millis(600);
            assert!((chart.marks.lifts[1].get(rest) - 1.0).abs() < 1e-6);
            assert!(!chart.marks.lifts[1].is_moving(rest));
            // Reduced motion lands at once: the slice is down on the frame the pointer leaves.
            chart.marks.frame.inspection.hovered = None;
            let reduced = EffectiveMotion::new(theme.tokens.motion, true);
            chart.marks.advance_lifts(rest, reduced);
            assert!(
                chart.marks.lifts[1].get(rest).abs() < 1e-9,
                "reduced motion sinks it at once"
            );
        }
    }

    /// A pie with a legend laid out, and nothing painted: the wedges and the
    /// legend stand where the layout placed them, so a hover lands on what
    /// the chart draws.
    fn placed_pie(theme: &Theme) -> PieChart {
        let mut chart = PieChart::new(vec![
            PieSlice::new("photos", "Photos", 42.0, Color::WHITE),
            PieSlice::new("music", "Music", 21.0, Color::WHITE),
            PieSlice::new("documents", "Documents and spreadsheets", 9.0, Color::WHITE),
            PieSlice::new("other", "Other", 4.0, Color::WHITE),
        ])
        .with_legend();
        crate::testing::lay_out_in(&mut chart, 640.0, 300.0, theme).expect("the chart lays out");
        chart
    }
}

/// How far a chart's data change has come `elapsed` after it, `0..=1`: its
/// values move to their new places on the standard list change.
pub(crate) fn data_change_progress(elapsed: Duration) -> f64 {
    tilo_ui_base::motion::ListChange::MOVE.progress_after(
        tilo_ui_base::motion::Direction::Enter,
        tilo_ui_base::tokens::EffectiveMotion::default(),
        tilo_ui_base::tokens::MotionSource::Pointer,
        elapsed,
    )
}

pub(crate) fn data_change_time() -> Duration {
    tilo_ui_base::motion::ListChange::MOVE.settle_time(
        tilo_ui_base::motion::Direction::Enter,
        tilo_ui_base::tokens::EffectiveMotion::default(),
        tilo_ui_base::tokens::MotionSource::Pointer,
    )
}
