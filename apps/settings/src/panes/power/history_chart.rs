//! The battery's charge over the last day or week: the charge as a line
//! over time with the area under it, the stretches spent plugged in shaded,
//! hours or days along the time axis, and the charge at any moment under
//! the pointer. The library's plot draws it; this module says what to plot.
//!
//! The history is the shell's power monitor's (`power-history.json`: one
//! point per quarter hour at most). Where the machine was off or asleep
//! there are no points, and the line breaks rather than inventing a slope.

use std::collections::HashMap;

use os_actions::BatteryHistoryPoint;
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{
    AxisTick, CartesianAnnotation, CartesianPlot, CartesianStyle, LegendPosition, PlotAxis,
    PlotDatum, PlotSeries, SeriesKind, Widget,
};
use vello::peniko::Color;

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

/// The charge chart: the library's plot, built again when the theme's
/// ink or chart height changes.
pub struct HistoryChart {
    points: Vec<BatteryHistoryPoint>,
    span: Span,
    /// Now, in minutes since the Unix epoch: the chart's right edge.
    now: u64,
    /// This device's offset from UTC in seconds, for the hour and day
    /// labels.
    local_offset: i64,
    plot: CartesianPlot,
    /// The ink, the plugged-in tint and the height the plot was built with.
    built_for: Option<(Color, Color, f32)>,
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
            plot: CartesianPlot::new(Vec::new()),
            built_for: None,
        }
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
    /// The charge as plotted: each run of points, a missing point between
    /// runs so the line breaks where the machine was off.
    fn charge(&self) -> Vec<PlotDatum> {
        let mut data = Vec::with_capacity(self.points.len());
        for (index, run) in self.runs().into_iter().enumerate() {
            if index > 0
                && let Some(first) = run.first()
            {
                data.push(PlotDatum::missing(
                    format!("gap-{index}"),
                    first.unix_minute as f64,
                ));
            }
            data.extend(run.iter().map(|point| self.datum(point)));
        }
        data
    }

    fn datum(&self, point: &BatteryHistoryPoint) -> PlotDatum {
        PlotDatum::new(
            point.unix_minute.to_string(),
            point.unix_minute as f64,
            f64::from(point.capacity),
        )
        .labelled(ui::format::percent(f64::from(point.capacity) / 100.0))
        .with_detail(self.time_label(point.unix_minute))
    }

    /// The stretches spent plugged in, from the first reading on power to
    /// the reading after the last.
    fn plugged(&self) -> Vec<(u64, u64)> {
        let mut stretches: Vec<(u64, u64)> = Vec::new();
        for run in self.runs() {
            for pair in run.windows(2) {
                if let [a, b] = pair
                    && a.on_ac
                {
                    match stretches.last_mut() {
                        Some(last) if last.1 == a.unix_minute => last.1 = b.unix_minute,
                        _ => stretches.push((a.unix_minute, b.unix_minute)),
                    }
                }
            }
        }
        stretches
    }

    fn build(&self, theme: &Theme, height: f32) -> CartesianPlot {
        let colors = &theme.tokens.colors;
        let from = self.now.saturating_sub(self.span.minutes());
        let x_axis = PlotAxis::default()
            .fixed(from as f64, self.now as f64)
            .with_ticks(
                self.ticks()
                    .into_iter()
                    .map(|(minute, label)| AxisTick::new(minute as f64, label)),
            );
        let y_axis = PlotAxis::default()
            .fixed(0.0, 100.0)
            .with_ticks([0_u32, 50, 100].map(|charge| {
                AxisTick::new(
                    f64::from(charge),
                    ui::format::percent(f64::from(charge) / 100.0),
                )
            }))
            // Empty, half and full stay labelled however short the plot is.
            .with_tick_density(3, f64::from(theme.tokens.spacing.s6));
        // A reading with no neighbour on either side is a dot, not an
        // invisible line.
        let lone: Vec<PlotDatum> = self
            .runs()
            .into_iter()
            .filter(|run| run.len() == 1)
            .flatten()
            .map(|point| self.datum(point))
            .collect();
        let name = t("power-battery-title");
        // A charge level is a value: the accent as a mark, never the text
        // colour (docs/design-language.md, "Slider and progress").
        let value = colors.intent.value_fill;
        let mut series = vec![
            PlotSeries::new("charge", name.clone(), value, self.charge())
                .kind(SeriesKind::Area)
                .fill_alpha(26)
                .connect_gaps(false),
        ];
        if !lone.is_empty() {
            series.push(PlotSeries::new("lone", name, value, lone).kind(SeriesKind::Scatter));
        }
        CartesianPlot::new(series)
            .with_annotations(self.plugged().into_iter().enumerate().map(
                |(index, (start, end))| {
                    CartesianAnnotation::x_band(
                        format!("plugged-{index}"),
                        start as f64,
                        end as f64,
                        colors.semantic.success,
                    )
                    .fill_alpha(48)
                    .interactive(false)
                },
            ))
            .with_style(CartesianStyle {
                x_axis,
                y_axis,
                legend: LegendPosition::Hidden,
                interactive_legend: false,
                ..CartesianStyle::default()
            })
            .with_height(height)
            .with_empty_message(t("power-history-empty"))
            .described(
                t_args(
                    "power-history-name",
                    &[("span", ArgValue::from(self.span.label()))],
                ),
                self.summary(),
            )
            .with_id("power.history")
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

impl ui::widget::Decorator for HistoryChart {
    type Inner = CartesianPlot;

    fn inner(&self) -> &CartesianPlot {
        &self.plot
    }

    fn inner_mut(&mut self) -> &mut CartesianPlot {
        &mut self.plot
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let height = HEIGHT * theme.tokens.metrics.min_pointer_target
            / ui::tokens::Metrics::BASE.min_pointer_target;
        let colors = &theme.tokens.colors;
        let key = (colors.intent.value_fill, colors.semantic.success, height);
        if self.built_for != Some(key) {
            self.plot = self.build(theme, height);
            self.built_for = Some(key);
        }
        Widget::build_layout(&mut self.plot, taffy, theme, node_map)
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

    #[test]
    fn the_plot_breaks_at_gaps_and_shades_the_time_plugged_in() {
        let now = 100_000;
        let points = [
            point(now - 200, 85, false),
            point(now - 60, 80, true),
            point(now - 45, 82, true),
            point(now - 30, 84, false),
            point(now - 15, 83, false),
        ];
        let chart = HistoryChart::new(&points, Span::Day, now, 0);
        let charge = chart.charge();
        assert_eq!(
            charge.len(),
            points.len() + 1,
            "one missing point at the gap"
        );
        assert!(
            charge[1].y.is_nan(),
            "the line breaks after the lone reading"
        );
        assert_eq!(
            chart.plugged(),
            [(now - 60, now - 30)],
            "consecutive readings on power are one stretch"
        );
        let plot = chart.build(&ui::theme::Theme::dark(), HEIGHT);
        assert_eq!(plot.annotations().len(), 1);
        assert_eq!(Widget::test_id(&plot), Some("power.history"));
        let node = Widget::accessibility(&plot).expect("plot semantics");
        assert_eq!(node.value.as_deref(), Some(chart.summary().as_str()));
    }

    #[test]
    fn the_charge_is_drawn_in_the_value_fill_in_either_theme() {
        let now = 100_000;
        let points = [
            point(now - 200, 85, false),
            point(now - 30, 84, false),
            point(now - 15, 83, false),
        ];
        let chart = HistoryChart::new(&points, Span::Day, now, 0);
        for theme in [ui::theme::Theme::light(), ui::theme::Theme::dark()] {
            let plot = chart.build(&theme, HEIGHT);
            let value = theme.tokens.colors.intent.value_fill;
            assert_ne!(value, theme.tokens.colors.text_primary);
            assert_eq!(plot.series().len(), 2, "the run and the lone reading");
            for series in plot.series() {
                assert_eq!(series.color, value, "{}", series.id);
            }
        }
    }
}
