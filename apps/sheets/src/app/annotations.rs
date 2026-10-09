//! The annotation dock: a chart over its sampled range and a comment thread
//! anchored to a range. A card is an object (it can be opened, edited and
//! deleted), so it is a `CardFace` with its own title and, for a comment, a
//! stripe in its author's colour. The cards stand in a row under the grid
//! (the grid gives up the height) rather than floating over it, so an
//! annotation never hides the cells it talks about. The plot is genuine
//! content drawing and reads its colour from the theme's series.

use super::{
    Addr, Affine, AnnotationClick, Button, CellValue, ChartKind, ChartSnapshot, ChromeMetrics,
    Fill, HashMap, Point, Rect, Scene, SheetsApp, TextRenderer, TextRole, Theme, WindowEvent,
    count_f64,
};
use ui::charts::legend;
use ui::charts::{Sparkline, TrendFill, TrendRange, TrendStyle};
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
const MIN_GRID_ROWS: usize = 6;
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
    /// The action row.
    pub actions_top: f64,
    pub height: f64,
}

impl ChromeMetrics {
    /// Height of a chart's plot: the legend's line and a step under it, then
    /// a content-sized plot.
    pub(super) fn plot_h(&self) -> f64 {
        self.lh_caption + self.s1 + self.s16 + self.s6
    }

    /// Rows of a comment card's body: the range line, the comment text, the
    /// replies line and the action row, each a measured line box plus a
    /// spacing token.
    pub(super) fn comment_rows(&self) -> CommentRows {
        let range_h = self.lh_caption;
        let text_top = range_h + self.s1;
        let text_h = self.lh_body * count_f64(COMMENT_LINES);
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

/// A card in the dock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum DockCard {
    /// The sheet's first comment thread.
    Comment,
    /// The chart at this index of the sheet's charts.
    Chart(usize),
}

/// What a dock card needs, measured once per frame with the other chrome
/// metrics.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct CardMetrics {
    /// The width a card wants: the utility-panel width, grown with the text.
    pub width: f64,
    /// Height of a chart card and of a comment card.
    pub chart_h: f64,
    pub comment_h: f64,
    /// The narrowest a card can be before its action row stops fitting.
    pub chart_min_w: f64,
    pub comment_min_w: f64,
}

impl CardMetrics {
    pub(super) fn resolve(tr: &mut TextRenderer, theme: &Theme, m: &ChromeMetrics) -> Self {
        let width = f64::from(theme.tokens.metrics.panel_width_utility)
            * f64::from(ui::tokens::Metrics::text_growth(theme.text_scale));
        let face = CardFace::new().title("Ag");
        // The plot gives the type picker's row its height, so a chart card
        // stands no taller than one with a single row of actions.
        let chart_h = face.height_for_body(tr, theme, width, m.plot_h() + m.s2 + m.control_h);
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
            buttons + m.s1 * count_f64(labels.len().saturating_sub(1)) + 2.0 * pad
        };
        let chart_min_w = row(&["Open", "Rename", "Delete"]).max(row(&["Column", "Line", "Cells"]));
        let comment_min_w = row(&["Reply", "Edit", "Resolve", "Delete"])
            .max(row(&["Reply", "Edit", "Reopen", "Delete"]));
        Self {
            width,
            chart_h,
            comment_h,
            chart_min_w,
            comment_min_w,
        }
    }

    fn height(&self, card: DockCard) -> f64 {
        match card {
            DockCard::Comment => self.comment_h,
            DockCard::Chart(_) => self.chart_h,
        }
    }

    fn min_width(&self, card: DockCard) -> f64 {
        match card {
            DockCard::Comment => self.comment_min_w,
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
    /// The cards stand left to right, the comment first, each as wide as a
    /// utility panel while they fit. When they do not they share the width,
    /// none narrower than its action row needs, and the dock scrolls
    /// sideways to the ones past its edge, with its bar under them.
    pub(super) fn dock_layout(&self, area: Rect) -> (f64, Vec<(DockCard, Rect)>, f64) {
        let m = &self.metrics;
        let cm = &m.cards;
        let mut cards: Vec<DockCard> = Vec::new();
        // Every card has a place, however narrow the window.
        if !self.sheet_comments.is_empty() {
            cards.push(DockCard::Comment);
        }
        cards.extend((0..self.sheet_charts.len().min(MAX_CHART_CARDS)).map(DockCard::Chart));
        let pad = m.s3;
        let card_h = cards
            .iter()
            .map(|card| cm.height(*card))
            .fold(0.0, f64::max);
        let avail = (area.width() - 2.0 * pad).max(0.0);
        let gaps = pad * count_f64(cards.len().saturating_sub(1));
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
        let content_w = widths.iter().sum::<f64>() + gaps + 2.0 * pad;
        // Cards past the edge scroll in, over the bar's lane under them.
        let lane = if content_w > area.width() + 0.5 {
            f64::from(self.chrome_theme.tokens.metrics.scrollbar_gutter)
        } else {
            0.0
        };
        let dock_h = card_h + 2.0 * pad + lane;
        let min_grid = m.grid.col_header_h()
            + count_f64(MIN_GRID_ROWS) * m.grid.default_row_h()
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
        let series: Vec<Vec<PlotSeries>> = cards
            .iter()
            .map(|(card, _)| match card {
                DockCard::Chart(i) => self
                    .sheet_charts
                    .get(*i)
                    .map(|chart| chart_series(chart, &cache.values))
                    .unwrap_or_default(),
                DockCard::Comment => Vec::new(),
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
                DockCard::Comment => self.draw_comment_card(&mut ctx, rect),
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

    /// One chart card: its title, its plot in the room above its two rows of
    /// controls, the type picker and Cells, then Open, Rename and Delete.
    fn draw_chart_card(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        card: Rect,
        chart: &ChartSnapshot,
        series: &[PlotSeries],
    ) {
        let gap = self.metrics.s2;
        let face = CardFace::new().title(&chart.title);
        face.paint(ctx, card);
        let body = face.body_rect(ctx.text, ctx.theme, card);
        let rows = self.metrics.control_h * 2.0 + self.metrics.s1;
        let plot = Rect::new(
            body.x0,
            body.y0,
            body.x1,
            (body.y1 - gap - rows).max(body.y0),
        );
        Self::draw_plot(ctx, plot, chart.kind, series);
        self.draw_card_actions(
            ctx,
            (body.x0, plot.y1 + gap),
            vec![
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
            ],
            Some(chart.kind),
        );
        self.draw_card_actions(
            ctx,
            (
                body.x0,
                plot.y1 + gap + self.metrics.control_h + self.metrics.s1,
            ),
            vec![
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
            ],
            None,
        );
    }

    /// The chart in the theme's series colours: a legend naming each series
    /// over the plot, a value axis with its ticks and rules at the plot's
    /// leading edge, then columns grouped by point from a zero baseline, or
    /// one line per series.
    fn draw_plot(ctx: &mut PaintCtx<'_>, area: Rect, kind: ChartKind, series: &[PlotSeries]) {
        let theme = ctx.theme;
        let caption = theme.text(TextRole::Caption);
        let (_, line) = ctx.text.measure_style("Ag", None, &caption);
        let line = f64::from(line);
        if series.iter().all(|series| series.values.is_empty()) {
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
        let all = || {
            series
                .iter()
                .flat_map(|series| series.values.iter().copied())
        };
        let max = all().fold(f64::NEG_INFINITY, f64::max);
        let min = all().fold(f64::INFINITY, f64::min);
        // Columns stand on zero, so their heights compare honestly.
        let (lo, hi) = match kind {
            ChartKind::Column => (min.min(0.0), max.max(0.0)),
            ChartKind::Line => (min, max),
        };
        let mut ticks = ui::charts::nice_ticks(lo, hi, 3);
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
        let plot = Rect::new(
            area.x0 + gutter,
            area.y0 + line + step + line / 2.0,
            area.x1,
            (area.y1 - line / 2.0).max(area.y0 + line + step + line / 2.0 + 1.0),
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
    }

    /// The legend over the plot: one row, each entry its swatch and the
    /// series' name, sharing the width when they do not all fit.
    fn draw_legend(ctx: &mut PaintCtx<'_>, area: Rect, line: f64, series: &[PlotSeries]) {
        let theme = ctx.theme;
        let sp = &theme.tokens.spacing;
        let names: Vec<&str> = series.iter().map(|series| series.name.as_str()).collect();
        let entries = legend::measure(ctx.text, theme, names.iter().copied());
        let gap = f64::from(sp.rhythm.section);
        let gaps = gap * count_f64(entries.len().saturating_sub(1));
        let natural: f64 = entries.iter().map(|entry| entry.width).sum::<f64>() + gaps;
        let share = if natural > area.width() {
            (area.width() - gaps).max(0.0) / count_f64(entries.len().max(1))
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
                let cluster = plot.width() / count_f64(points.max(1));
                let bar_w = (cluster - 2.0 * inset) / count_f64(series.len().max(1));
                for (s, line) in series.iter().enumerate() {
                    let ink = theme.tokens.colors.presence.series(s);
                    for (i, value) in line.values.iter().enumerate() {
                        let x0 = plot.x0 + cluster * count_f64(i) + inset + bar_w * count_f64(s);
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
                let dot = 2.5 * f64::from(theme.text_scale).max(1.0);
                let style = TrendStyle::CHART
                    .with_fill(TrendFill::None)
                    .with_markers(dot);
                let range = TrendRange { lo, hi: lo + span };
                for (s, line) in series.iter().enumerate() {
                    Sparkline::paint(
                        ctx.scene,
                        ctx.transform,
                        plot.inflate(-dot, 0.0),
                        &line.values,
                        range,
                        theme.tokens.colors.presence.series(s),
                        &style,
                    );
                }
            }
        }
    }

    /// The first comment thread of the active sheet, as a card: who wrote it
    /// and on what range, the comment, its replies, and what can be done with
    /// it.
    #[allow(
        clippy::too_many_lines,
        reason = "the card's rows are painted top to bottom from one shared set of measured offsets"
    )]
    fn draw_comment_card(&mut self, ctx: &mut PaintCtx<'_>, card: Rect) {
        let Some(thread) = self.sheet_comments.first().cloned() else {
            return;
        };
        let theme = ctx.theme;
        let rows = self.metrics.comment_rows();
        let face = CardFace::new()
            .title(&thread.author)
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
                        body.y0 + rows.text_top + self.metrics.lh_body * count_f64(i),
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

/// One series of a chart: its name and its numbers in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct PlotSeries {
    pub name: String,
    pub values: Vec<f64>,
}

/// The series a chart plots from the numbers of its range. A range taller
/// than it is wide plots its columns, a wider one its rows (the usual
/// convention), so a single column or row is one series of its values in
/// order. A series is named by the text that heads it, or else by its
/// column letter or row number. Text, errors and blanks are skipped.
fn chart_series(chart: &ChartSnapshot, values: &HashMap<Addr, CellValue>) -> Vec<PlotSeries> {
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
    lines
        .take(MAX_SERIES)
        .map(|line| {
            let name = match values.get(&at(line, *steps.start())) {
                Some(CellValue::Text(text)) if !text.trim().is_empty() => text.trim().to_owned(),
                _ if by_columns => crate::model::col_to_letters(line),
                _ => format!("Row {}", line + 1),
            };
            let values = steps
                .clone()
                .filter_map(|step| match values.get(&at(line, step)) {
                    Some(CellValue::Number(value)) => Some(*value),
                    _ => None,
                })
                .take(MAX_PLOT_POINTS)
                .collect();
            PlotSeries { name, values }
        })
        .collect()
}
