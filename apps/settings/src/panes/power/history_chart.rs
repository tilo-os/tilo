//! The battery's charge over the last day or week: the charge as a line
//! over time with the area under it, the stretches spent plugged in marked
//! along the bottom, hours or days along the time axis, and the charge at
//! any moment under the pointer.
//!
//! The history is the shell's power monitor's (`power-history.json`: one
//! point per quarter hour at most). Where the machine was off or asleep
//! there are no points, and the line breaks rather than inventing a slope.

use std::collections::HashMap;

use os_actions::BatteryHistoryPoint;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::{TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use crate::i18n::{t, t_args};
use ui::i18n::ArgValue;

/// How far back the chart looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    Day,
    Week,
}

impl Span {
    pub const ALL: [Span; 2] = [Span::Day, Span::Week];

    fn minutes(self) -> u64 {
        match self {
            Span::Day => 24 * 60,
            Span::Week => 7 * 24 * 60,
        }
    }

    pub fn label(self) -> String {
        t(match self {
            Span::Day => "power-history-day",
            Span::Week => "power-history-week",
        })
    }
}

/// A gap between two points longer than this is time the machine was off
/// or asleep: the line breaks there.
const GAP_MINUTES: u64 = 50;
/// The chart's height at the default text size.
const HEIGHT: f32 = ui::tokens::Metrics::BASE.chart_height;

/// The charge chart.
pub struct HistoryChart {
    points: Vec<BatteryHistoryPoint>,
    span: Span,
    /// Now, in minutes since the Unix epoch: the chart's right edge.
    now: u64,
    /// This device's offset from UTC in seconds, for the hour and day
    /// labels.
    local_offset: i64,
    rect: Rect,
    hover: Option<Point>,
}

impl HistoryChart {
    pub fn new(points: &[BatteryHistoryPoint], span: Span, now: u64, local_offset: i64) -> Self {
        let from = now.saturating_sub(span.minutes());
        Self {
            points: points
                .iter()
                .filter(|point| point.unix_minute >= from && point.unix_minute <= now)
                .cloned()
                .collect(),
            span,
            now,
            local_offset,
            rect: Rect::ZERO,
            hover: None,
        }
    }

    /// Where the plot sits: the reading under the pointer takes a line
    /// above it, the time labels a line under it and the charge labels a
    /// column on its right.
    fn plot(&self, theme: &Theme) -> Rect {
        let caption = f64::from(theme.text(ui::TextRole::Caption).line_height_px());
        let gutter = f64::from(theme.tokens.spacing.s8) + f64::from(theme.tokens.spacing.s2);
        Rect::new(
            self.rect.x0,
            self.rect.y0 + caption + f64::from(theme.tokens.spacing.s2),
            (self.rect.x1 - gutter).max(self.rect.x0 + 1.0),
            (self.rect.y1 - caption - f64::from(theme.tokens.spacing.s2))
                .max(self.rect.y0 + caption),
        )
    }

    fn x_of(&self, plot: Rect, minute: u64) -> f64 {
        let from = self.now.saturating_sub(self.span.minutes());
        plot.x0 + (minute.saturating_sub(from)) as f64 / self.span.minutes() as f64 * plot.width()
    }

    fn y_of(plot: Rect, capacity: u32) -> f64 {
        plot.y1 - f64::from(capacity.min(100)) / 100.0 * plot.height()
    }

    /// The runs of points with no gap between them.
    fn runs(&self) -> Vec<&[BatteryHistoryPoint]> {
        let mut runs = Vec::new();
        let mut start = 0;
        for (index, pair) in self.points.windows(2).enumerate() {
            if let [a, b] = pair
                && b.unix_minute.saturating_sub(a.unix_minute) > GAP_MINUTES
            {
                runs.extend(self.points.get(start..=index));
                start = index + 1;
            }
        }
        runs.extend(self.points.get(start..));
        runs.retain(|run| !run.is_empty());
        runs
    }

    /// The time-axis ticks: `(minute, label)` at every sixth hour of the
    /// local day, or at every local midnight.
    fn ticks(&self) -> Vec<(u64, String)> {
        let from = self.now.saturating_sub(self.span.minutes());
        let offset_minutes = self.local_offset / 60;
        let local = |minute: u64| minute as i64 + offset_minutes;
        let step: i64 = match self.span {
            Span::Day => 6 * 60,
            Span::Week => 24 * 60,
        };
        let first = (local(from).div_euclid(step) + 1) * step;
        let style = ui::clock::TimeStyle::configured();
        let mut ticks = Vec::new();
        let mut at = first;
        while at <= local(self.now) {
            let minute = u64::try_from(at - offset_minutes).unwrap_or_default();
            let label = match self.span {
                Span::Day => {
                    let of_day = at.rem_euclid(24 * 60);
                    style.format_hm((of_day / 60) as u32, 0)
                }
                Span::Week => weekday_label(at.div_euclid(24 * 60)),
            };
            ticks.push((minute, label));
            at += step;
        }
        ticks
    }

    /// The point nearest the pointer, while it is over the plot.
    fn hovered(&self, theme: &Theme) -> Option<&BatteryHistoryPoint> {
        let pointer = self.hover?;
        let plot = self.plot(theme);
        if !plot.inflate(0.0, 8.0).contains(pointer) {
            return None;
        }
        self.points.iter().min_by(|a, b| {
            let da = (self.x_of(plot, a.unix_minute) - pointer.x).abs();
            let db = (self.x_of(plot, b.unix_minute) - pointer.x).abs();
            da.total_cmp(&db)
        })
    }

    fn time_label(&self, minute: u64) -> String {
        let local = minute as i64 + self.local_offset / 60;
        let of_day = local.rem_euclid(24 * 60);
        let time = ui::clock::TimeStyle::configured()
            .format_hm((of_day / 60) as u32, (of_day % 60) as u32);
        match self.span {
            Span::Day => time,
            Span::Week => t_args(
                "power-history-at",
                &[
                    (
                        "day",
                        ArgValue::from(weekday_label(local.div_euclid(24 * 60))),
                    ),
                    ("time", ArgValue::from(time)),
                ],
            ),
        }
    }

    /// What the chart says to someone who cannot see it.
    fn summary(&self) -> String {
        let (Some(low), Some(high), Some(last)) = (
            self.points.iter().map(|point| point.capacity).min(),
            self.points.iter().map(|point| point.capacity).max(),
            self.points.last(),
        ) else {
            return t("power-history-empty");
        };
        t_args(
            "power-history-summary",
            &[
                (
                    "low",
                    ArgValue::from(ui::format::percent(f64::from(low) / 100.0)),
                ),
                (
                    "high",
                    ArgValue::from(ui::format::percent(f64::from(high) / 100.0)),
                ),
                (
                    "last",
                    ArgValue::from(ui::format::percent(f64::from(last.capacity) / 100.0)),
                ),
            ],
        )
    }
}

/// The short name of the weekday of local day `day` (days since 1970-01-01,
/// a Thursday).
fn weekday_label(day: i64) -> String {
    let from_monday = u32::try_from((day + 3).rem_euclid(7)).unwrap_or_default();
    ui::format::week()
        .into_iter()
        .find(|weekday| weekday.num_days_from_monday() == from_monday)
        .map(ui::format::weekday_short)
        .unwrap_or_default()
}

/// What one frame of the chart paints with.
struct Painter<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut ui::text_render::TextRenderer,
    theme: &'a Theme,
    plot: Rect,
}

impl Painter<'_> {
    fn caption(&self) -> ui::text_render::TextStyle {
        self.theme.text(ui::TextRole::Caption)
    }

    fn rule(&mut self, from: (f64, f64), to: (f64, f64), color: vello::peniko::Color) {
        // ratchet-allow(raw_paint): the charge chart draws measured battery data, content rather than chrome
        self.scene.stroke(
            &Stroke::new(1.0),
            self.transform,
            color,
            None,
            &Line::new(from, to),
        );
    }

    fn label(&mut self, label: &str, at: (f64, f64), color: vello::peniko::Color) {
        let caption = self.caption();
        self.text.draw_style(
            self.scene,
            label,
            color,
            self.transform * Affine::translate(at),
            None,
            &caption,
        );
    }

    fn width_of(&mut self, label: &str) -> f64 {
        let caption = self.caption();
        f64::from(self.text.measure_style(label, None, &caption).0)
    }

    fn centred(&mut self, message: &str) {
        let width = self.width_of(message);
        let line = f64::from(self.caption().line_height_px());
        let at = (
            self.plot.center().x - width / 2.0,
            self.plot.center().y - line / 2.0,
        );
        self.label(message, at, self.theme.tokens.colors.text_dim);
    }

    fn fill(&mut self, shape: &impl vello::kurbo::Shape, color: vello::peniko::Color) {
        self.scene
            .fill(Fill::NonZero, self.transform, color, None, shape);
    }
}

impl HistoryChart {
    /// The charge scale (rules at empty, half and full, labelled on the
    /// right) and the time axis.
    fn draw_scale(&self, painter: &mut Painter<'_>) {
        let colors = &painter.theme.tokens.colors;
        let (subtle, tertiary) = (colors.overlay_border_subtle, colors.text_tertiary);
        let plot = painter.plot;
        let gap = f64::from(painter.theme.tokens.spacing.s2);
        let line = f64::from(painter.caption().line_height_px());
        for capacity in [0_u32, 50, 100] {
            let y = Self::y_of(plot, capacity);
            painter.rule((plot.x0, y), (plot.x1, y), subtle);
            let label = ui::format::percent(f64::from(capacity) / 100.0);
            painter.label(&label, (plot.x1 + gap, y - line / 2.0), tertiary);
        }
        for (minute, label) in self.ticks() {
            let x = self.x_of(plot, minute);
            painter.rule((x, plot.y0), (x, plot.y1), subtle);
            let width = painter.width_of(&label);
            let left = (x - width / 2.0).clamp(plot.x0, (plot.x1 - width).max(plot.x0));
            painter.label(&label, (left, plot.y1 + gap), tertiary);
        }
    }

    /// The stretches plugged in along the bottom, then the area under each
    /// run of the charge and its line.
    fn draw_charge(&self, painter: &mut Painter<'_>) {
        let colors = &painter.theme.tokens.colors;
        let (plugged, ink, content) = (
            colors.semantic.success,
            colors.text_primary,
            // ratchet-allow(literal_alpha): the charge chart draws measured battery data, content rather than chrome
            colors.text_primary.multiply_alpha(0.10),
        );
        let plot = painter.plot;
        let band = f64::from(painter.theme.tokens.spacing.s1).max(3.0);
        let runs = self.runs();
        for run in &runs {
            for pair in run.windows(2) {
                if let [a, b] = pair
                    && a.on_ac
                {
                    let (x0, x1) = (
                        self.x_of(plot, a.unix_minute),
                        self.x_of(plot, b.unix_minute),
                    );
                    painter.fill(&Rect::new(x0, plot.y1 - band, x1, plot.y1), plugged);
                }
            }
        }
        let mut line = BezPath::new();
        for run in &runs {
            let at = |point: &BatteryHistoryPoint| {
                Point::new(
                    self.x_of(plot, point.unix_minute),
                    Self::y_of(plot, point.capacity),
                )
            };
            let (Some(first), Some(last)) = (run.first(), run.last()) else {
                continue;
            };
            if run.len() == 1 {
                // A lone reading is a dot, not an invisible line.
                painter.fill(&Circle::new(at(first), 2.0), ink);
                continue;
            }
            let mut area = BezPath::new();
            area.move_to((at(first).x, plot.y1));
            line.move_to(at(first));
            for point in *run {
                area.line_to(at(point));
                line.line_to(at(point));
            }
            area.line_to((at(last).x, plot.y1));
            area.close_path();
            painter.fill(&area, content);
        }
        // ratchet-allow(raw_paint): the charge chart draws measured battery data, content rather than chrome
        painter.scene.stroke(
            &Stroke::new(1.75)
                .with_join(vello::kurbo::Join::Round)
                .with_caps(vello::kurbo::Cap::Round),
            painter.transform,
            ink,
            None,
            &line,
        );
    }

    /// The charge at the moment under the pointer: a rule and a dot on the
    /// line, and the charge and the time in the line above the plot.
    fn draw_reading(&self, painter: &mut Painter<'_>, point: &BatteryHistoryPoint) {
        let colors = &painter.theme.tokens.colors;
        let (strong, content, ink) = (
            colors.overlay_border_strong,
            colors.surfaces.content,
            colors.text_primary,
        );
        let plot = painter.plot;
        let at = Point::new(
            self.x_of(plot, point.unix_minute),
            Self::y_of(plot, point.capacity),
        );
        painter.rule((at.x, plot.y0), (at.x, plot.y1), strong);
        painter.fill(&Circle::new(at, 5.0), content);
        painter.fill(&Circle::new(at, 3.5), ink);
        let label = t_args(
            "power-history-reading",
            &[
                (
                    "charge",
                    ArgValue::from(ui::format::percent(f64::from(point.capacity) / 100.0)),
                ),
                ("time", ArgValue::from(self.time_label(point.unix_minute))),
            ],
        );
        let width = painter.width_of(&label);
        let left = (at.x - width / 2.0).clamp(plot.x0, (plot.x1 - width).max(plot.x0));
        painter.label(&label, (left, self.rect.y0), ink);
    }
}

impl Widget for HistoryChart {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let height = HEIGHT * theme.tokens.metrics.min_pointer_target
            / ui::tokens::Metrics::BASE.min_pointer_target;
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(height),
            },
            min_size: Size {
                width: Dimension::length(0.0),
                height: Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        let plot = self.plot(theme);
        let mut painter = Painter {
            scene,
            transform,
            text,
            theme,
            plot,
        };
        self.draw_scale(&mut painter);
        if self.points.is_empty() {
            painter.centred(&t("power-history-empty"));
            return;
        }
        self.draw_charge(&mut painter);
        if let Some(point) = self.hovered(theme) {
            self.draw_reading(&mut painter, point);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let at = ctx.logical(position);
                let hover = self.rect.contains(at).then_some(at);
                if hover != self.hover {
                    self.hover = hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorLeft { .. } if self.hover.is_some() => {
                self.hover = None;
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "BatteryHistoryChart"
    }

    fn test_id(&self) -> Option<&str> {
        Some("power.history")
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Image)
                .with_name(t_args(
                    "power-history-name",
                    &[("span", ArgValue::from(self.span.label()))],
                ))
                .with_value(self.summary()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(minute: u64, capacity: u32, on_ac: bool) -> BatteryHistoryPoint {
        BatteryHistoryPoint::sampled(minute, capacity, None, on_ac)
    }

    /// The chart shows only its span, and breaks its line where the machine
    /// was off.
    #[test]
    fn the_line_breaks_where_the_machine_was_off() {
        let now = 100_000;
        let points = [
            point(now - 2000, 90, false),
            point(now - 60, 80, false),
            point(now - 45, 79, false),
            point(now - 30, 78, true),
            point(now - 200, 85, false),
        ];
        let mut sorted = points.to_vec();
        sorted.sort_by_key(|point| point.unix_minute);
        let chart = HistoryChart::new(&sorted, Span::Day, now, 0);
        assert_eq!(
            chart.points.len(),
            4,
            "a point older than a day is left out"
        );
        let runs: Vec<usize> = chart.runs().iter().map(|run| run.len()).collect();
        assert_eq!(runs, [1, 3], "two hours without points break the line");
        let week = HistoryChart::new(&sorted, Span::Week, now, 0);
        assert_eq!(week.points.len(), 5);
    }

    #[test]
    fn the_time_axis_ticks_fall_on_local_hours_and_midnights() {
        // 2026-03-20 13:05 UTC, in a zone two hours ahead.
        let now = 1_774_011_900 / 60;
        let day = HistoryChart::new(&[], Span::Day, now, 7200);
        let ticks = day.ticks();
        assert_eq!(ticks.len(), 4, "{ticks:?}");
        for (minute, _) in &ticks {
            assert_eq!((minute + 120) % (6 * 60), 0, "a sixth local hour: {minute}");
            assert!(*minute <= now && *minute > now - 24 * 60);
        }
        let week = HistoryChart::new(&[], Span::Week, now, 7200);
        let ticks = week.ticks();
        assert_eq!(ticks.len(), 7, "{ticks:?}");
        for (minute, _) in &ticks {
            assert_eq!((minute + 120) % (24 * 60), 0, "a local midnight: {minute}");
        }
        // 1970-01-01 was a Thursday; 2026-03-20 is a Friday.
        assert_eq!(weekday_label(0), weekday_label(7));
        assert_ne!(weekday_label(0), weekday_label(1));
    }
}
