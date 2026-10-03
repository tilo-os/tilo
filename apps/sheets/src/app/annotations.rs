//! The floating annotation cards: a chart over its sampled range and a
//! comment thread anchored to a range. A card is an object (it can be opened,
//! edited and deleted), so it is a `CardFace` with its own title and, for a
//! comment, a stripe in its author's colour. The plot is genuine content
//! drawing and reads its colour from the theme's series.

use super::{
    Addr, Affine, AnnotationClick, Button, CellValue, ChartKind, ChartSnapshot, ChromeMetrics,
    Fill, HashMap, Line, Point, Rect, Scene, SheetsApp, Stroke, TextRenderer, TextRole, Theme,
    count_f64,
};
use ui::paint::PaintCtx;
use ui::widget::{BadgeFace, ButtonFace, ButtonRole, CardFace, ChipStyle, ControlSize, Tone};

/// How many chart cards float over a sheet at once.
const MAX_CHART_CARDS: usize = 3;
/// How many points a chart plots, so a broad range cannot turn a paint into an
/// unbounded formula or path build.
const MAX_PLOT_POINTS: usize = 48;
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
    /// Height of a chart's plot: content-sized, not text-sized.
    pub(super) fn plot_h(&self) -> f64 {
        self.s16 + self.s6
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

/// A card's width: the utility-panel width, kept inside the grid.
fn card_width(grid: Rect, theme: &Theme) -> f64 {
    let pad = f64::from(theme.tokens.spacing.s3);
    // Its buttons are text, so it grows with the text scale.
    let wanted = f64::from(theme.tokens.metrics.panel_width_utility)
        * f64::from(ui::tokens::Metrics::text_growth(theme.text_scale));
    wanted.min((grid.width() - 2.0 * pad).max(0.0))
}

impl SheetsApp {
    /// Whether `p` is over an annotation card or one of its buttons, as of the
    /// last paint.
    pub(super) fn annotation_hit(&self, p: Point) -> bool {
        self.annotation_clicks
            .iter()
            .any(|(rect, _)| rect.contains(p))
    }

    /// Where a floating annotation card may sit: the top of the sheet body
    /// (below the column-header band) and its right edge (left of the
    /// vertical scrollbar gutter). Cards keep `s3` inside both.
    pub(super) fn annotation_body_bounds(&self) -> (f64, f64) {
        (
            self.grid_rect.y0 + self.metrics.grid.col_header_h(),
            self.grid_rect.x1 - self.grid.scrollbar_thickness(),
        )
    }

    /// Paint a row of small buttons from `x0` along `y`, registering each as a
    /// click target. Returns where the row ended.
    fn draw_card_actions(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        origin: (f64, f64),
        actions: Vec<(&'static str, ButtonRole, AnnotationClick)>,
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
            let face = ButtonFace::new(rect, label)
                .role(role)
                .control_size(ControlSize::Small)
                .hovered(rect.contains(self.last_mouse));
            Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
            self.annotation_clicks
                .push((face.hit_rect(ctx.theme), action));
            x += w + gap;
        }
    }

    /// Render the active sheet's small, data-derived chart collection. Charts
    /// sample only their selected rectangle and cap the plotted points.
    pub(super) fn draw_charts(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(cache) = &self.cache else { return };
        // The cards float over the sheet body, `s3` inside the header bands
        // and the scrollbar gutter (never on either), stacked `s3` apart.
        let (body_top, body_right) = self.annotation_body_bounds();
        let pad = self.metrics.s3;
        let bottom = self.grid_rect.y1 - self.grid.scrollbar_thickness();
        let plot_h = self.metrics.plot_h();
        let gap = self.metrics.s2;
        let body_h = plot_h + gap + self.metrics.control_h;
        let width = card_width(self.grid_rect, theme);
        let charts: Vec<ChartSnapshot> = self
            .sheet_charts
            .iter()
            .take(MAX_CHART_CARDS)
            .cloned()
            .collect();
        let values = cache.values.clone();
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        let mut y0 = body_top + pad;
        for chart in &charts {
            let face = CardFace::new().title(&chart.title);
            let height = face.height_for_body(ctx.text, theme, width, body_h);
            if y0 + height > bottom {
                break;
            }
            let x1 = body_right - pad;
            let card = Rect::new(x1 - width, y0, x1, y0 + height);
            face.paint(&mut ctx, card);
            let body = face.body_rect(ctx.text, theme, card);
            let plot = Rect::new(body.x0, body.y0, body.x1, body.y0 + plot_h);
            Self::draw_plot(&mut ctx, plot, chart, &values);
            self.draw_card_actions(
                &mut ctx,
                (body.x0, plot.y1 + gap),
                vec![
                    (
                        "Open",
                        ButtonRole::Secondary,
                        AnnotationClick::Open(chart.start),
                    ),
                    (
                        "Edit",
                        ButtonRole::Secondary,
                        AnnotationClick::EditChart(chart.id.clone()),
                    ),
                    (
                        "Delete",
                        ButtonRole::Destructive,
                        AnnotationClick::DeleteChart(chart.id.clone()),
                    ),
                ],
            );
            y0 += height + pad;
        }
    }

    /// The chart's plot in its series colour: columns or a line over at most
    /// [`MAX_PLOT_POINTS`] numbers of the range.
    fn draw_plot(
        ctx: &mut PaintCtx<'_>,
        plot: Rect,
        chart: &ChartSnapshot,
        values: &HashMap<Addr, CellValue>,
    ) {
        let theme = ctx.theme;
        let mut points = Vec::new();
        'collect: for row in chart.start.1..=chart.end.1 {
            for col in chart.start.0..=chart.end.0 {
                if let Some(CellValue::Number(value)) = values.get(&(col, row)) {
                    points.push(*value);
                }
                if points.len() == MAX_PLOT_POINTS {
                    break 'collect;
                }
            }
        }
        if points.is_empty() {
            let style = theme.text(TextRole::Caption);
            let (_, h) = ctx.text.measure_style("Ag", None, &style);
            ctx.text.draw_style(
                ctx.scene,
                "No numbers in this range",
                theme.tokens.colors.text_dim,
                ctx.transform
                    * Affine::translate((plot.x0, ChromeMetrics::centred_top(plot, f64::from(h)))),
                None,
                &style,
            );
            return;
        }
        let max = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min = points.iter().copied().fold(f64::INFINITY, f64::min);
        let span = (max - min).abs().max(1.0);
        let ink = theme.tokens.colors.presence.series(0);
        let inset = f64::from(theme.tokens.spacing.s1) / 2.0;
        match chart.kind {
            ChartKind::Column => {
                let step = plot.width() / count_f64(points.len());
                for (i, value) in points.iter().enumerate() {
                    let h = ((value - min) / span * plot.height()).max(2.0);
                    let x0 = plot.x0 + step * count_f64(i);
                    let bar = Rect::new(x0 + inset, plot.y1 - h, x0 + step - inset, plot.y1);
                    ctx.scene
                        .fill(Fill::NonZero, ctx.transform, ink, None, &bar);
                }
            }
            ChartKind::Line => {
                let step = if points.len() > 1 {
                    plot.width() / count_f64(points.len() - 1)
                } else {
                    0.0
                };
                let at = |i: usize, value: f64| {
                    Point::new(
                        plot.x0 + step * count_f64(i),
                        plot.y1 - (value - min) / span * plot.height(),
                    )
                };
                for (i, pair) in points.windows(2).enumerate() {
                    let [first, second] = pair else { continue };
                    ctx.scene.stroke(
                        &Stroke::new(2.0),
                        ctx.transform,
                        ink,
                        None,
                        &Line::new(at(i, *first), at(i + 1, *second)),
                    );
                }
            }
        }
    }

    /// The first comment thread of the active sheet, as a card over the
    /// sheet's top-left: who wrote it and on what range, the comment, its
    /// replies, and what can be done with it.
    #[allow(
        clippy::too_many_lines,
        reason = "the card's rows are painted top to bottom from one shared set of measured offsets"
    )]
    pub(super) fn draw_comment_threads(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(thread) = self.sheet_comments.first().cloned() else {
            return;
        };
        let width = card_width(self.grid_rect, theme);
        let rows = self.metrics.comment_rows();
        // The card floats `s3` inside the row and column header bands, so it
        // never covers a header label.
        let (body_top, _) = self.annotation_body_bounds();
        let pad = self.metrics.s3;
        let x0 = self.grid_rect.x0 + self.metrics.grid.row_header_w() + pad;
        let y0 = body_top + pad;
        let face = CardFace::new()
            .title(&thread.author)
            .leading_stripe(Tone::of_identity(&thread.author))
            .pressable();
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        let height = face.height_for_body(ctx.text, theme, width, rows.height);
        let card = Rect::new(x0, y0, x0 + width, y0 + height);
        face.paint(&mut ctx, card);
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
            &mut ctx,
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
        );
        // The rest of the card opens the thread's range. Registered after the
        // buttons, so a button wins where they overlap.
        self.annotation_clicks
            .push((card, AnnotationClick::Open(thread.start)));
    }
}
