//! Annotations: charts over their sampled ranges, and comment threads on
//! their cells. A card is an object (it can be opened, edited and deleted),
//! so it is a `CardFace` with its own title and, for a comment, a stripe in
//! its author's colour.
//!
//! Chart cards stand in a dock under the grid (the grid gives up the
//! height). A comment belongs to its cell: the cell carries a marker in its
//! corner, and selecting the cell (a click on it or its marker, or the
//! comment's `open`) opens the thread as a floating card beside the cell,
//! so it never hides the cell it is about. The plot is genuine content
//! drawing and reads its colour from the theme's series.

use super::{
    Addr, Affine, AnnotationClick, Button, CellValue, ChartKind, ChartSnapshot, ChromeMetrics,
    CommentThread, Fill, HashMap, Point, Rect, Scene, SheetsApp, TextRenderer, TextRole, Theme,
    WindowEvent,
};
use ui::charts::legend;
use ui::charts::{Sparkline, TrendFill, TrendRange, TrendStyle};
use ui::num::f64_of_usize;
use ui::paint::PaintCtx;
use ui::surface::separator;
use ui::widget::{BadgeFace, ButtonFace, ButtonRole, CardFace, ChipStyle, ControlSize, Tone};
use vello::kurbo::Vec2;

/// How many chart cards stand in the dock at once.
const MAX_CHART_CARDS: usize = 3;
/// How many points a series plots, so a broad range cannot turn a paint into
/// an unbounded formula or path build.
const MAX_PLOT_POINTS: usize = 48;
/// How many series a chart plots.
const MAX_SERIES: usize = 6;
/// The rows a grid keeps visible above the dock; a window too short for them
/// shows no dock rather than a sliver of sheet.
const MIN_GRID_ROWS: usize = 4;
/// How many lines of a comment the card shows.
const COMMENT_LINES: usize = 2;

/// Measured rows of a comment card's body, relative to the body's top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CommentRows {
    /// The anchor range line.
    pub range_h: f64,
    /// The comment text, at most [`COMMENT_LINES`] lines.
    pub text_top: f64,
    pub text_h: f64,
    /// The replies line.
    pub replies_top: f64,
    pub actions_top: f64,
    pub height: f64,
}

impl ChromeMetrics {
    /// Height of a chart's plot: the legend's line and a step under it, a
    /// content-sized plot, then a step and the line of category labels.
    pub(super) fn plot_h(&self) -> f64 {
        2.0 * (self.lh_caption + self.s1) + self.s16
    }

    /// Rows of a comment card's body: the range line, the comment text, the
    /// replies line and the action row, each a measured line box plus a
    /// spacing token.
    pub(super) fn comment_rows(&self) -> CommentRows {
        let range_h = self.lh_caption;
        let text_top = range_h + self.s1;
        let text_h = self.lh_body * f64_of_usize(COMMENT_LINES);
        let replies_top = text_top + text_h + self.s1;
        let actions_top = replies_top + self.lh_caption + self.s3;
        CommentRows {
            range_h,
            text_top,
            text_h,
            replies_top,
            actions_top,
            height: actions_top + self.control_h,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum DockCard {
    /// The chart at this index of the sheet's charts.
    Chart(usize),
}

/// What a dock card needs, measured once per frame with the other chrome
/// metrics.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct CardMetrics {
    /// Wide, landscape cards want one and a half utility panels, grown with text.
    pub width: f64,
    /// Height of a chart card and of a comment card.
    pub chart_h: f64,
    pub chart_compact_h: f64,
    pub chart_single_row_w: f64,
    pub comment_h: f64,
    /// The narrowest a card can be before its action row stops fitting.
    pub chart_min_w: f64,
    pub comment_min_w: f64,
}

impl CardMetrics {
    pub(super) fn resolve(tr: &mut TextRenderer, theme: &Theme, m: &ChromeMetrics) -> Self {
        let width = f64::from(theme.tokens.metrics.panel_width_utility)
            * f64::from(ui::tokens::Metrics::text_growth(theme.text_scale))
            * 1.5;
        let face = CardFace::new().title("Ag");
        // Budget both action rows on narrow cards. Wide cards combine the
        // same commands into one row, giving the sheet back that height.
        let chart_compact_h =
            face.height_for_body(tr, theme, width, m.plot_h() + m.s2 + m.control_h);
        let chart_h = face.height_for_body(
            tr,
            theme,
            width,
            m.plot_h() + m.s2 + 2.0 * m.control_h + m.s1,
        );
        let comment_h = face.height_for_body(tr, theme, width, m.comment_rows().height);
        let pad = face
            .body_rect(tr, theme, Rect::new(0.0, 0.0, width, chart_h))
            .x0;
        let mut row = |labels: &[&str]| -> f64 {
            let buttons: f64 = labels
                .iter()
                .map(|label| {
                    ButtonFace::new(Rect::ZERO, label)
                        .control_size(ControlSize::Small)
                        .intrinsic_width(tr, theme)
                })
                .sum();
            buttons + m.s1 * f64_of_usize(labels.len().saturating_sub(1)) + 2.0 * pad
        };
        let chart_min_w = row(&["Open", "Rename", "Delete"]).max(row(&["Column", "Line", "Cells"]));
        let chart_single_row_w = row(&["Column", "Line", "Cells", "Open", "Rename", "Delete"]);
        let comment_min_w = row(&["Reply", "Edit", "Resolve", "Delete"])
            .max(row(&["Reply", "Edit", "Reopen", "Delete"]));
        Self {
            width,
            chart_h,
            chart_compact_h,
            chart_single_row_w,
            comment_h,
            chart_min_w,
            comment_min_w,
        }
    }

    fn height(&self, card: DockCard, width: f64) -> f64 {
        match card {
            DockCard::Chart(_) if width >= self.chart_single_row_w => self.chart_compact_h,
            DockCard::Chart(_) => self.chart_h,
        }
    }

    fn min_width(&self, card: DockCard) -> f64 {
        match card {
            DockCard::Chart(_) => self.chart_min_w,
        }
    }
}

impl SheetsApp {
    /// Whether `p` is over an annotation card or one of its buttons, as of the
    /// last paint.
    pub(super) fn annotation_hit(&self, p: Point) -> bool {
        self.annotation_clicks
            .iter()
            .any(|(rect, _)| rect.contains(p))
    }

    /// Lay the dock out in `area`, the strip the grid and the dock share: the
    /// height the dock takes (zero when the sheet has no annotations, or the
    /// window is too short to give them up), where each card stands before
    /// the dock is scrolled, and how wide the cards are together.
    ///
    /// The cards stand left to right, each as wide as a utility panel while
    /// they fit. When they do not they share the width,
    /// none narrower than its action row needs, and the dock scrolls
    /// sideways to the ones past its edge, with its bar under them.
    pub(super) fn dock_layout(&self, area: Rect) -> (f64, Vec<(DockCard, Rect)>, f64) {
        let m = &self.metrics;
        let cm = &m.cards;
        // Every card has a place, however narrow the window.
        let cards: Vec<DockCard> = (0..self.sheet_charts.len().min(MAX_CHART_CARDS))
            .map(DockCard::Chart)
            .collect();
        let pad = m.s3;
        let avail = (area.width() - 2.0 * pad).max(0.0);
        let gaps = pad * f64_of_usize(cards.len().saturating_sub(1));
        let mins: Vec<f64> = cards.iter().map(|card| cm.min_width(*card)).collect();
        let room = avail - gaps - mins.iter().sum::<f64>();
        // Every card gets what its action row needs; what is left over grows
        // them toward a utility panel's width, equally.
        let slack: f64 = mins.iter().map(|min| (cm.width - min).max(0.0)).sum();
        let grow = if slack > 0.0 {
            room.max(0.0).min(slack) / slack
        } else {
            0.0
        };
        let widths: Vec<f64> = mins
            .iter()
            .map(|min| min + (cm.width - min).max(0.0) * grow)
            .collect();
        let card_h = cards
            .iter()
            .zip(&widths)
            .map(|(card, width)| cm.height(*card, *width))
            .fold(0.0, f64::max);
        let content_w = widths.iter().sum::<f64>() + gaps + 2.0 * pad;
        // Cards past the edge scroll in, over the bar's lane under them.
        let lane = if content_w > area.width() + 0.5 {
            f64::from(self.chrome_theme.tokens.metrics.scrollbar_gutter)
        } else {
            0.0
        };
        let dock_h = card_h + 2.0 * pad + lane;
        let min_grid = m.grid.col_header_h()
            + f64_of_usize(MIN_GRID_ROWS) * m.grid.default_row_h()
            + self.grid.scrollbar_thickness();
        if cards.is_empty() || area.height() - dock_h < min_grid {
            return (0.0, Vec::new(), 0.0);
        }
        let y0 = area.y1 - dock_h + pad;
        let mut x = area.x0 + pad;
        let placed = cards
            .iter()
            .zip(widths)
            .map(|(card, w)| {
                let rect = Rect::new(x, y0, x + w, y0 + card_h);
                x += w + pad;
                (*card, rect)
            })
            .collect();
        (dock_h, placed, content_w)
    }

    /// Route the wheel, the touchpad and the bar's drag over the dock to its
    /// sideways scroll while cards stand past its edge. Returns whether the
    /// event was consumed.
    pub(super) fn route_dock_scroll(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.dock_scroll.overflows_horizontally() {
            return false;
        }
        let over = self.dock_rect.contains(self.last_mouse);
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        );
        let wheel = matches!(event, WindowEvent::MouseWheel { .. });
        if !(pointer || wheel && over) {
            return false;
        }
        let before = *ctx.handled;
        let changed = self
            .dock_scroll
            .handle_event(event, ctx, &self.chrome_theme);
        if changed {
            self.pending.redraw = true;
        }
        // Motion and presses still reach the cards and the sheet; only a
        // scroll, or a press that the bar took, stops here.
        let took = *ctx.handled && !before;
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            *ctx.handled = before;
            return false;
        }
        took
    }

    /// Paint a row of small buttons from `x0` along `y`, registering each as a
    /// click target; the one choosing `picked` shows on.
    fn draw_card_actions(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        origin: (f64, f64),
        actions: Vec<(&'static str, ButtonRole, AnnotationClick)>,
        picked: Option<ChartKind>,
    ) {
        let (mut x, y) = origin;
        let gap = self.metrics.s1;
        let h = self.metrics.control_h;
        for (label, role, action) in actions {
            let probe = ButtonFace::new(Rect::ZERO, label)
                .role(role)
                .control_size(ControlSize::Small);
            let w = probe.intrinsic_width(ctx.text, ctx.theme);
            let rect = Rect::new(x, y, x + w, y + h);
            let on = matches!(
                (&action, picked),
                (AnnotationClick::SetChartKind(_, kind), Some(current)) if *kind == current
            );
            let face = ButtonFace::new(rect, label)
                .role(role)
                .control_size(ControlSize::Small)
                .toggled(on)
                .hovered(rect.contains(self.last_mouse));
            Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
            self.annotation_clicks
                .push((face.hit_rect(ctx.theme), action));
            x += w + gap;
        }
    }

    /// Paint the dock: one hairline on its first pixel row, then its cards.
    pub(super) fn draw_annotation_dock(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.dock_cards.is_empty() {
            return;
        }
        separator(
            scene,
            transform,
            self.dock_rect.x0,
            self.dock_rect.x1,
            self.dock_rect.y0 + 0.5,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        let Some(cache) = &self.cache else { return };
        let cards = self.dock_cards.clone();
        let series: Vec<PlotData> = cards
            .iter()
            .map(|(card, _)| match card {
                DockCard::Chart(i) => self
                    .sheet_charts
                    .get(*i)
                    .map(|chart| chart_data(chart, &cache.values))
                    .unwrap_or_default(),
            })
            .collect();
        // The cards stand where the dock is scrolled to, clipped to the
        // dock; their click targets are kept only where they show.
        let shift = Vec2::new(-self.dock_scroll.offset().x, 0.0);
        let shown = self.dock_scroll.content_rect();
        let first_click = self.annotation_clicks.len();
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shown,
        );
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        for ((card, rect), series) in cards.into_iter().zip(series) {
            let rect = rect + shift;
            if rect.intersect(shown).is_zero_area() {
                continue;
            }
            match card {
                DockCard::Chart(i) => {
                    let Some(chart) = self.sheet_charts.get(i).cloned() else {
                        continue;
                    };
                    self.draw_chart_card(&mut ctx, rect, &chart, &series);
                }
            }
        }
        scene.pop_layer();
        self.dock_scroll.paint_bars(scene, transform, theme);
        let clicks = self.annotation_clicks.split_off(first_click);
        self.annotation_clicks.extend(
            clicks
                .into_iter()
                .map(|(rect, action)| (rect.intersect(shown), action))
                .filter(|(rect, _)| !rect.is_zero_area()),
        );
    }

    /// A chart uses its whole body for the plot. The header's More control
    /// explicitly toggles management; only then do the action rows appear.
    fn draw_chart_card(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        card: Rect,
        chart: &ChartSnapshot,
        data: &PlotData,
    ) {
        let gap = self.metrics.s2;
        let managing = self.managed_chart.as_ref() == Some(&chart.id);
        let h = self.metrics.control_h;
        let padding = CardFace::new().body_rect(ctx.text, ctx.theme, card).x0 - card.x0;
        let title = ctx.text.elide_to_width_style(
            &chart.title,
            ui::num::f32_of_f64((card.width() - 2.0 * padding - h - gap).max(0.0)),
            &ctx.theme.text(TextRole::Subheading),
        );
        let face = CardFace::new().title(&title);
        face.paint(ctx, card);
        let body = face.body_rect(ctx.text, ctx.theme, card);
        let header_y = card.y0 + padding + self.metrics.lh_label / 2.0;
        let more = Rect::from_center_size(Point::new(body.x1 - h / 2.0, header_y), (h, h));
        let button = ButtonFace::icon_only(more, ui::icons::Icon::MoreHorizontal, "Manage chart")
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small)
            .toggled(managing)
            .hovered(more.contains(self.last_mouse));
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &button);
        self.annotation_clicks.push((
            button.hit_rect(ctx.theme),
            AnnotationClick::ManageChart(chart.id.clone()),
        ));
        if !managing {
            Self::draw_plot(ctx, body, chart.kind, data);
            return;
        }
        let single_row = card.width() >= self.metrics.cards.chart_single_row_w;
        let rows = if single_row {
            self.metrics.control_h
        } else {
            self.metrics.control_h * 2.0 + self.metrics.s1
        };
        let plot = Rect::new(
            body.x0,
            body.y0,
            body.x1,
            (body.y1 - gap - rows).max(body.y0),
        );
        Self::draw_plot(ctx, plot, chart.kind, data);
        let mut actions = vec![
            (
                ChartKind::Column.label(),
                ButtonRole::Secondary,
                AnnotationClick::SetChartKind(chart.id.clone(), ChartKind::Column),
            ),
            (
                ChartKind::Line.label(),
                ButtonRole::Secondary,
                AnnotationClick::SetChartKind(chart.id.clone(), ChartKind::Line),
            ),
            (
                "Cells",
                ButtonRole::Secondary,
                AnnotationClick::ChartCells(chart.id.clone()),
            ),
        ];
        let object_actions = vec![
            (
                "Open",
                ButtonRole::Secondary,
                AnnotationClick::Open(chart.start),
            ),
            (
                "Rename",
                ButtonRole::Secondary,
                AnnotationClick::EditChart(chart.id.clone()),
            ),
            (
                "Delete",
                ButtonRole::Destructive,
                AnnotationClick::DeleteChart(chart.id.clone()),
            ),
        ];
        if single_row {
            actions.extend(object_actions);
        } else {
            self.draw_card_actions(
                ctx,
                (
                    body.x0,
                    plot.y1 + gap + self.metrics.control_h + self.metrics.s1,
                ),
                object_actions,
                None,
            );
        }
        self.draw_card_actions(ctx, (body.x0, plot.y1 + gap), actions, Some(chart.kind));
    }

    /// The chart in the theme's series colours: a legend naming each series
    /// over the plot, a value axis with its ticks and rules at the plot's
    /// leading edge, then columns grouped by category from a zero baseline,
    /// or one line per series, with each category's label under it.
    fn draw_plot(ctx: &mut PaintCtx<'_>, area: Rect, kind: ChartKind, data: &PlotData) {
        let series = data.series.as_slice();
        let theme = ctx.theme;
        let caption = theme.text(TextRole::Caption);
        let (_, line) = ctx.text.measure_style("Ag", None, &caption);
        let line = f64::from(line);
        if series
            .iter()
            .all(|series| series.numbers().next().is_none())
        {
            ctx.text.draw_style(
                ctx.scene,
                "No numbers in this range",
                theme.tokens.colors.text_dim,
                ctx.transform
                    * Affine::translate((area.x0, ChromeMetrics::centred_top(area, line))),
                None,
                &caption,
            );
            return;
        }
        let sp = &theme.tokens.spacing;
        let step = f64::from(sp.s1);

        Self::draw_legend(ctx, area, line, series);

        // The value axis: rounded ticks over the range, each a rule across
        // the plot and its value in the gutter, the half line it needs
        // above the top rule and below the bottom one kept inside the card.
        let all = || series.iter().flat_map(PlotSeries::numbers);
        let max = all().fold(f64::NEG_INFINITY, f64::max);
        let min = all().fold(f64::INFINITY, f64::min);
        // Columns stand on zero, so their heights compare honestly.
        let (lo, hi) = match kind {
            ChartKind::Column => (min.min(0.0), max.max(0.0)),
            ChartKind::Line => (min, max),
        };
        let mut ticks = ui::charts::nice_ticks(lo, hi, 3);
        // A series of one value has one tick: its range has no width, and a
        // step of none has no precision to label it at.
        ticks.dedup();
        let tick_step = match ticks.as_slice() {
            [first, second, ..] => second - first,
            _ => 1.0,
        };
        // The rules reach past both ends of the data, so no mark stands
        // above the top value read off the axis.
        while let Some(&top) = ticks.last()
            && top < hi - tick_step * 1e-9
            && ticks.len() < 8
        {
            ticks.push(top + tick_step);
        }
        while let Some(&bottom) = ticks.first()
            && bottom > lo + tick_step * 1e-9
            && ticks.len() < 8
        {
            ticks.insert(0, bottom - tick_step);
        }
        let (lo, hi) = (
            ticks.first().copied().unwrap_or(lo).min(lo),
            ticks.last().copied().unwrap_or(hi).max(hi),
        );
        let span = (hi - lo).max(f64::EPSILON);
        let labels: Vec<String> = ticks
            .iter()
            .map(|tick| ui::charts::format_tick(*tick, tick_step))
            .collect();
        let gutter = labels
            .iter()
            .map(|label| f64::from(ctx.text.measure_style(label, None, &caption).0))
            .fold(0.0, f64::max)
            + f64::from(sp.s2);
        // The category labels take a line under the plot; without them the
        // bottom tick's half line is all it keeps.
        let foot = if data.categories.is_empty() {
            line / 2.0
        } else {
            line + step
        };
        let plot = Rect::new(
            area.x0 + gutter,
            area.y0 + line + step + line / 2.0,
            area.x1,
            (area.y1 - foot).max(area.y0 + line + step + line / 2.0 + 1.0),
        );
        let y_of = |value: f64| plot.y1 - (value - lo) / span * plot.height();
        for (tick, label) in ticks.iter().zip(&labels) {
            let y = y_of(*tick);
            // The card's own hairline, the edge its raised ground derives.
            separator(
                ctx.scene,
                ctx.transform,
                plot.x0,
                plot.x1,
                y,
                0.0,
                theme.tokens.colors.surfaces.raised,
            );
            let (w, _) = ctx.text.measure_style(label, None, &caption);
            ctx.text.draw_style(
                ctx.scene,
                label,
                theme.tokens.colors.text_dim,
                ctx.transform
                    * Affine::translate((
                        plot.x0 - f64::from(sp.s2) - f64::from(w),
                        y - line / 2.0,
                    )),
                None,
                &caption,
            );
        }

        Self::draw_marks(ctx, plot, (lo, span), kind, series);
        Self::draw_categories(ctx, plot, kind, &data.categories);
    }

    /// Each category's label under the plot, centred on its cluster of
    /// columns or its point on the lines, elided to the room it has.
    fn draw_categories(ctx: &mut PaintCtx<'_>, plot: Rect, kind: ChartKind, categories: &[String]) {
        if categories.is_empty() {
            return;
        }
        let theme = ctx.theme;
        let caption = theme.text(TextRole::Caption);
        let step = f64::from(theme.tokens.spacing.s1);
        let n = categories.len();
        let slot = plot.width() / f64_of_usize(n.max(1));
        let centres: Vec<f64> = match kind {
            ChartKind::Column => (0..n)
                .map(|i| plot.x0 + slot * (f64_of_usize(i) + 0.5))
                .collect(),
            ChartKind::Line => {
                let dot = Self::line_dot(theme);
                let zeros = vec![0.0_f64; n];
                Sparkline::points(
                    plot.inflate(-dot, 0.0),
                    &zeros,
                    TrendRange { lo: 0.0, hi: 1.0 },
                )
                .into_iter()
                .map(|point| point.x)
                .collect()
            }
        };
        let room = (slot - step).max(0.0);
        for (label, x) in categories.iter().zip(centres) {
            let shown = ctx
                .text
                .elide_to_width_style(label, ui::num::f32_of_f64(room), &caption);
            let (w, _) = ctx.text.measure_style(&shown, None, &caption);
            let w = f64::from(w);
            let left = (x - w / 2.0).clamp(plot.x0, (plot.x1 - w).max(plot.x0));
            ctx.text.draw_style(
                ctx.scene,
                &shown,
                theme.tokens.colors.text_dim,
                ctx.transform * Affine::translate((left, plot.y1 + step)),
                None,
                &caption,
            );
        }
    }

    /// The radius of a line chart's dot on every sample.
    fn line_dot(theme: &Theme) -> f64 {
        2.5 * f64::from(theme.text_scale).max(1.0)
    }

    /// The legend over the plot: one row, each entry its swatch and the
    /// series' name, sharing the width when they do not all fit.
    fn draw_legend(ctx: &mut PaintCtx<'_>, area: Rect, line: f64, series: &[PlotSeries]) {
        let theme = ctx.theme;
        let sp = &theme.tokens.spacing;
        let names: Vec<&str> = series.iter().map(|series| series.name.as_str()).collect();
        let entries = legend::measure(ctx.text, theme, names.iter().copied());
        let gap = f64::from(sp.rhythm.section);
        let gaps = gap * f64_of_usize(entries.len().saturating_sub(1));
        let natural: f64 = entries.iter().map(|entry| entry.width).sum::<f64>() + gaps;
        let share = if natural > area.width() {
            (area.width() - gaps).max(0.0) / f64_of_usize(entries.len().max(1))
        } else {
            f64::INFINITY
        };
        let mut x = area.x0;
        for (s, (entry, name)) in entries.iter().zip(&names).enumerate() {
            let w = entry.width.min(share);
            let rect = Rect::new(x, area.y0, x + w, area.y0 + line);
            legend::paint_entry(
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
                rect,
                theme.tokens.colors.presence.series(s),
                name,
            );
            x += w + gap;
        }
    }

    /// The series' marks in `plot`, over the value range `lo` to `lo + span`.
    fn draw_marks(
        ctx: &mut PaintCtx<'_>,
        plot: Rect,
        (lo, span): (f64, f64),
        kind: ChartKind,
        series: &[PlotSeries],
    ) {
        let theme = ctx.theme;
        let y_of = |value: f64| plot.y1 - (value - lo) / span * plot.height();
        let inset = f64::from(theme.tokens.spacing.s1) / 2.0;
        let points = series
            .iter()
            .map(|series| series.values.len())
            .max()
            .unwrap_or(0);
        match kind {
            ChartKind::Column => {
                let cluster = plot.width() / f64_of_usize(points.max(1));
                let bar_w = (cluster - 2.0 * inset) / f64_of_usize(series.len().max(1));
                for (s, line) in series.iter().enumerate() {
                    let ink = theme.tokens.colors.presence.series(s);
                    for (i, value) in line.values.iter().enumerate() {
                        let Some(value) = value else { continue };
                        let x0 =
                            plot.x0 + cluster * f64_of_usize(i) + inset + bar_w * f64_of_usize(s);
                        let (a, b) = (y_of(*value), y_of(0.0));
                        let (top, bottom) = (a.min(b), a.max(b));
                        let bar = Rect::new(x0, top, x0 + bar_w, bottom.max(top + 1.0));
                        ctx.scene
                            .fill(Fill::NonZero, ctx.transform, ink, None, &bar);
                    }
                }
            }
            ChartKind::Line => {
                // The toolkit's trend line, a dot on every sample, kept a
                // dot's radius in from the plot's sides.
                let dot = Self::line_dot(theme);
                let style = TrendStyle::CHART
                    .with_fill(TrendFill::None)
                    .with_markers(dot);
                let range = TrendRange { lo, hi: lo + span };
                for (s, line) in series.iter().enumerate() {
                    let numbers: Vec<f64> = line.numbers().collect();
                    Sparkline::paint(
                        ctx.scene,
                        ctx.transform,
                        plot.inflate(-dot, 0.0),
                        &numbers,
                        range,
                        theme.tokens.colors.presence.series(s),
                        &style,
                    );
                }
            }
        }
    }

    /// The thread open on the sheet: the one on the selected cell.
    pub(super) fn open_thread(&self) -> Option<&CommentThread> {
        let (col, row) = self.grid.active();
        self.sheet_comments.iter().find(|thread| {
            let (c0, c1) = (
                thread.start.0.min(thread.end.0),
                thread.start.0.max(thread.end.0),
            );
            let (r0, r1) = (
                thread.start.1.min(thread.end.1),
                thread.start.1.max(thread.end.1),
            );
            (c0..=c1).contains(&col) && (r0..=r1).contains(&row)
        })
    }

    /// The cells a thread is about, on screen.
    pub(super) fn thread_rect(&self, thread: &CommentThread) -> Rect {
        let a = self.grid.cell_screen_rect(
            thread.start.0.min(thread.end.0),
            thread.start.1.min(thread.end.1),
        );
        let b = self.grid.cell_screen_rect(
            thread.start.0.max(thread.end.0),
            thread.start.1.max(thread.end.1),
        );
        a.union(b)
    }

    /// Where the open thread's card floats: beside its cells, on the side
    /// with room (after them, else before them) level with their first row,
    /// else under or over them, kept inside the sheet. `None` while the cells are scrolled out of
    /// view.
    pub(super) fn comment_card_rect(&self, thread: &CommentThread) -> Option<Rect> {
        let body = self.grid.body_rect();
        let cells = self.thread_rect(thread);
        if cells.intersect(body).is_zero_area() {
            return None;
        }
        let m = &self.metrics;
        let gap = m.s2;
        let w = m
            .cards
            .width
            .min(body.width() - 2.0 * gap)
            .max(m.cards.comment_min_w.min(body.width()));
        let h = m.cards.comment_h;
        // Beside the cells where there is room, level with them; else under
        // them, else over them; else pinned inside the sheet.
        let level = cells
            .y0
            .min(body.y1 - gap - h)
            .max(body.y0 + gap.min(body.height()));
        let aligned = cells
            .x0
            .min(body.x1 - gap - w)
            .max(body.x0 + gap.min(body.width()));
        let (x0, y0) = if cells.x1 + gap + w <= body.x1 - gap {
            (cells.x1 + gap, level)
        } else if cells.x0 - gap - w >= body.x0 + gap {
            (cells.x0 - gap - w, level)
        } else if cells.y1 + gap + h <= body.y1 - gap {
            (aligned, cells.y1 + gap)
        } else if cells.y0 - gap - h >= body.y0 + gap {
            (aligned, cells.y0 - gap - h)
        } else {
            ((body.x1 - gap - w).max(body.x0), level)
        };
        Some(Rect::new(x0, y0, x0 + w, y0 + h))
    }

    /// The marker of every thread whose cells are in view: the cell in the
    /// top trailing corner of its cells, and its tone, the author's colour
    /// while the thread is open for discussion, neutral once it is resolved.
    pub(super) fn comment_markers(&self) -> Vec<(Rect, Tone)> {
        let body = self.grid.body_rect();
        self.sheet_comments
            .iter()
            .filter_map(|thread| {
                let corner = self.grid.cell_screen_rect(
                    thread.start.0.max(thread.end.0),
                    thread.start.1.min(thread.end.1),
                );
                if corner.intersect(body).is_zero_area() {
                    return None;
                }
                let tone = if thread.resolved {
                    Tone::Neutral
                } else {
                    Tone::of_identity(&thread.author)
                };
                Some((corner, tone))
            })
            .collect()
    }

    /// A marker in the corner of every commented cell.
    pub(super) fn draw_comment_markers(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let markers = self.comment_markers();
        if markers.is_empty() {
            return;
        }
        let size = self.metrics.s2;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.grid.body_rect(),
        );
        let inset = self.metrics.s1;
        let round = f64::from(theme.tokens.radii.full);
        for (cell, tone) in markers {
            ui::paint::fill_round(
                scene,
                transform,
                comment_marker(cell, size, inset),
                round,
                tone.colour(theme),
            );
        }
        scene.pop_layer();
    }

    /// The open thread, floating beside its cell.
    pub(super) fn draw_comment_popover(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(thread) = self.open_thread().cloned() else {
            return;
        };
        let Some(card) = self.comment_card_rect(&thread) else {
            return;
        };
        ui::widget::floating_surface::paint_card_shadow(scene, transform, card, 1.0, theme);
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        self.draw_comment_card(&mut ctx, card, &thread);
    }

    /// A comment thread as a card: who wrote it and on what range, the
    /// comment, its replies, and what can be done with it.
    fn draw_comment_card(&mut self, ctx: &mut PaintCtx<'_>, card: Rect, thread: &CommentThread) {
        let theme = ctx.theme;
        let rows = self.metrics.comment_rows();
        let author = self.author_name(&thread.author);
        let face = CardFace::new()
            .title(&author)
            .leading_stripe(Tone::of_identity(&thread.author))
            .pressable();
        face.paint(ctx, card);
        let body = face.body_rect(ctx.text, theme, card);
        let colors = &theme.tokens.colors;

        // The anchor range, and a badge when the thread is resolved.
        let range = if thread.start == thread.end {
            crate::model::addr_to_a1(thread.start.0, thread.start.1)
        } else {
            format!(
                "{}:{}",
                crate::model::addr_to_a1(thread.start.0, thread.start.1),
                crate::model::addr_to_a1(thread.end.0, thread.end.1)
            )
        };
        let caption = theme.text(TextRole::Caption).numeric();
        ctx.text.draw_style(
            ctx.scene,
            &range,
            colors.text_dim,
            ctx.transform * Affine::translate((body.x0, body.y0)),
            None,
            &caption,
        );
        if thread.resolved {
            let w = BadgeFace::intrinsic_width("Resolved", ctx.text, theme);
            let h = BadgeFace::height(theme);
            let badge = Rect::new(body.x1 - w, body.y0, body.x1, body.y0 + h);
            BadgeFace::new(badge, "Resolved")
                .style(ChipStyle::Tinted(Tone::Success))
                .paint(ctx.scene, ctx.transform, ctx.text, theme);
        }

        // The comment itself, at most two lines.
        let style = theme.text(TextRole::Body);
        let lines =
            ctx.text
                .clamp_to_lines(&thread.body, &style, body.width() as f32, COMMENT_LINES);
        for (i, line) in lines.iter().enumerate() {
            ctx.text.draw_style(
                ctx.scene,
                line,
                colors.text_primary,
                ctx.transform
                    * Affine::translate((
                        body.x0,
                        body.y0 + rows.text_top + self.metrics.lh_body * f64_of_usize(i),
                    )),
                None,
                &style,
            );
        }

        // How many replies it has.
        let replies = match thread.replies.len() {
            0 => "No replies".to_owned(),
            1 => "1 reply".to_owned(),
            n => format!("{n} replies"),
        };
        ctx.text.draw_style(
            ctx.scene,
            &replies,
            colors.text_dim,
            ctx.transform * Affine::translate((body.x0, body.y0 + rows.replies_top)),
            None,
            &theme.text(TextRole::Caption),
        );

        self.draw_card_actions(
            ctx,
            (body.x0, body.y0 + rows.actions_top),
            vec![
                (
                    "Reply",
                    ButtonRole::Secondary,
                    AnnotationClick::Reply(thread.id.clone()),
                ),
                (
                    "Edit",
                    ButtonRole::Secondary,
                    AnnotationClick::EditComment(thread.id.clone()),
                ),
                (
                    if thread.resolved { "Reopen" } else { "Resolve" },
                    ButtonRole::Secondary,
                    AnnotationClick::ToggleComment(thread.id.clone(), !thread.resolved),
                ),
                (
                    "Delete",
                    ButtonRole::Destructive,
                    AnnotationClick::DeleteComment(thread.id.clone()),
                ),
            ],
            None,
        );
        // The rest of the card opens the thread's range. Registered after the
        // buttons, so a button wins where they overlap.
        self.annotation_clicks
            .push((card, AnnotationClick::Open(thread.start)));
    }
}

/// Where a commented cell's marker sits: a dot of `size` in the cell's top
/// trailing corner, `inset` in from both edges, never more than half the
/// cell so a narrow column still shows its text.
fn comment_marker(cell: Rect, size: f64, inset: f64) -> Rect {
    let size = size.min(cell.width() / 2.0).min(cell.height() / 2.0);
    let inset = inset.min((cell.height() - size).max(0.0) / 2.0);
    Rect::new(
        cell.x1 - inset - size,
        cell.y0 + inset,
        cell.x1 - inset,
        cell.y0 + inset + size,
    )
}

/// One series of a chart: its name and its number at each category, `None`
/// where that cell holds none.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct PlotSeries {
    pub name: String,
    pub values: Vec<Option<f64>>,
}

impl PlotSeries {
    /// The numbers it plots, in order, gaps skipped.
    fn numbers(&self) -> impl Iterator<Item = f64> + '_ {
        self.values.iter().filter_map(|value| *value)
    }
}

/// What a chart plots: the label of each category (empty when the range has
/// no label column or row) and the series measured at each.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct PlotData {
    pub categories: Vec<String>,
    pub series: Vec<PlotSeries>,
}

/// What a chart plots from its range. A range taller than it is wide plots
/// its columns, a wider one its rows (the usual convention). A leading line
/// of labels (a column of names with no numbers in it) is the category
/// axis, never a series; a line heading the numbers names each series, or
/// else its column letter or row number does. A line with no numbers at all
/// is no series. Errors and blanks are gaps.
fn chart_data(chart: &ChartSnapshot, values: &HashMap<Addr, CellValue>) -> PlotData {
    let (c0, r0) = chart.start;
    let (c1, r1) = chart.end;
    let by_columns = r1.saturating_sub(r0) >= c1.saturating_sub(c0);
    let (lines, steps) = if by_columns {
        (c0..=c1, r0..=r1)
    } else {
        (r0..=r1, c0..=c1)
    };
    let at = |line, step| {
        if by_columns {
            (line, step)
        } else {
            (step, line)
        }
    };
    let text = |line, step| match values.get(&at(line, step)) {
        Some(CellValue::Text(text)) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        _ => None,
    };
    let number = |line, step| match values.get(&at(line, step)) {
        Some(CellValue::Number(value)) => Some(*value),
        _ => None,
    };
    let first_step = *steps.start();
    // The first line labels the categories when it holds text and no number.
    let first_line = *lines.start();
    let labelled = lines.clone().count() > 1
        && steps.clone().all(|step| number(first_line, step).is_none())
        && steps.clone().any(|step| text(first_line, step).is_some());
    let series_lines: Vec<u32> = lines.skip(usize::from(labelled)).collect();
    // The first step heads the series when one of them is headed by text.
    let headed = series_lines
        .iter()
        .any(|&line| text(line, first_step).is_some());
    let data_steps: Vec<u32> = steps
        .skip(usize::from(headed))
        .filter(|&step| {
            series_lines
                .iter()
                .any(|&line| number(line, step).is_some())
        })
        .take(MAX_PLOT_POINTS)
        .collect();
    let categories = if labelled {
        data_steps
            .iter()
            .map(|&step| text(first_line, step).unwrap_or_default())
            .collect()
    } else {
        Vec::new()
    };
    let series = series_lines
        .into_iter()
        .filter(|&line| data_steps.iter().any(|&step| number(line, step).is_some()))
        .take(MAX_SERIES)
        .map(|line| {
            let name = match text(line, first_step) {
                Some(name) if headed => name,
                _ if by_columns => crate::model::col_to_letters(line),
                _ => format!("Row {}", line + 1),
            };
            let values = data_steps.iter().map(|&step| number(line, step)).collect();
            PlotSeries { name, values }
        })
        .collect();
    PlotData { categories, series }
}

#[cfg(test)]
mod chart_data_tests {
    use super::*;

    fn sheet(rows: &[&[&str]]) -> HashMap<Addr, CellValue> {
        let mut values = HashMap::new();
        for (r, row) in (0_u32..).zip(rows) {
            for (c, cell) in (0_u32..).zip(row.iter()) {
                let value = cell
                    .parse::<f64>()
                    .map_or_else(|_| CellValue::Text((*cell).to_owned()), CellValue::Number);
                if !cell.is_empty() {
                    values.insert((c, r), value);
                }
            }
        }
        values
    }

    fn chart(start: Addr, end: Addr) -> ChartSnapshot {
        ChartSnapshot {
            id: "c".into(),
            title: "Sales by region".into(),
            kind: ChartKind::Column,
            start,
            end,
        }
    }

    #[test]
    fn the_label_column_is_the_category_axis_and_never_a_series() {
        let values = sheet(&[
            &["Region", "Q1", "Q2", "Q3", "Q4"],
            &["North", "18200", "21450", "19800", "24300"],
            &["South", "15600", "16900", "18750", "20100"],
            &["East", "12400", "14800", "16300", "17900"],
            &["West", "20900", "19700", "22600", "26800"],
        ]);
        let data = chart_data(&chart((0, 0), (4, 4)), &values);
        assert_eq!(data.categories, ["North", "South", "East", "West"]);
        let names: Vec<&str> = data.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Q1", "Q2", "Q3", "Q4"], "Region is no series");
        assert_eq!(
            data.series[0].values,
            [Some(18200.0), Some(15600.0), Some(12400.0), Some(20900.0)]
        );

        // Wider than tall: the label row is the axis, each row a series.
        let values = sheet(&[
            &["Month", "Jan", "Feb", "Mar", "Apr", "May"],
            &["Rent", "1250", "1250", "1250", "1250", "1250"],
            &["Food", "420", "386", "", "402", "399"],
        ]);
        let data = chart_data(&chart((0, 0), (5, 2)), &values);
        assert_eq!(data.categories, ["Jan", "Feb", "Mar", "Apr", "May"]);
        let names: Vec<&str> = data.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Rent", "Food"]);
        assert_eq!(
            data.series[1].values[2], None,
            "a blank is a gap, not a shift"
        );

        // Numbers alone: no axis labels, no phantom series.
        let values = sheet(&[&["3"], &["5"], &["8"]]);
        let data = chart_data(&chart((0, 0), (0, 2)), &values);
        assert!(data.categories.is_empty());
        assert_eq!(data.series.len(), 1);
        assert_eq!(data.series[0].name, "A");
    }
}
