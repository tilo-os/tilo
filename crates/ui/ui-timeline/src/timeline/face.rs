//! Painting the time grid and the month grid, the geometry every view of them shares.

use jiff::civil::Date;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::TimelineDensity;
use tilo_ui_base::format;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::text_render::TextStyle;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::tokens::colors::SMALL_TEXT_MIN_CONTRAST;
use tilo_ui_base::{surface::card, surface::edge_on};

use super::{
    Ghost, MonthTarget, Split, TimelineGeom, TimelineItem, TimelineTarget, TimelineText,
    TimelineTokens, block_rect, column_width, geometry, hour_label, in_split, minute_text,
    splits_for,
};

/// The wash an all-day event is tinted with on the ground it sits on.
pub const ALL_DAY_WASH: f32 = 0.34;

fn hairline(scene: &mut Scene, transform: Affine, surface: Color, p0: Point, p1: Point) {
    let color = edge_on(surface);
    let rect = if (p0.x - p1.x).abs() < f64::EPSILON {
        let x = p0.x.floor();
        Rect::new(x, p0.y.min(p1.y), x + 1.0, p0.y.max(p1.y))
    } else {
        let y = p0.y.floor();
        Rect::new(p0.x.min(p1.x), y, p0.x.max(p1.x), y + 1.0)
    };
    scene.fill(Fill::NonZero, transform, color, None, &rect);
}

fn stroke_line(
    scene: &mut Scene,
    transform: Affine,
    color: Color,
    start: Point,
    end: Point,
    width: f64,
) {
    scene.stroke(
        &Stroke::new(width),
        transform,
        color,
        None,
        &Line::new(start, end),
    );
}

fn disc(ctx: &mut PaintCtx<'_>, center: Point, radius: f64, color: Color) {
    ctx.scene.fill(
        Fill::NonZero,
        ctx.transform,
        color,
        None,
        &Circle::new(center, radius),
    );
}

fn text_centered(ctx: &mut PaintCtx<'_>, text: &str, style: &TextStyle, color: Color, rect: Rect) {
    let (w, h) = ctx.text.measure_style(text, None, style);
    let origin = Point::new(
        rect.center().x - f64::from(w) / 2.0,
        rect.center().y - f64::from(h) / 2.0,
    );
    ctx.text(text, style, color, origin, None);
}

fn text_in_band(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    style: &TextStyle,
    color: Color,
    x: f64,
    band: Rect,
    max_width: f64,
) {
    let shown = ctx.text.elide_to_width_style(
        text,
        tilo_ui_base::num::f32_of_f64(max_width.max(0.0)),
        style,
    );
    let (_, h) = ctx.text.measure_style(&shown, None, style);
    let top = tilo_ui_base::text_metrics::centered_text_top(band.y0, band.height(), f64::from(h));
    ctx.text(&shown, style, color, Point::new(x, top), None);
}

fn line_height(ctx: &mut PaintCtx<'_>, style: &TextStyle) -> f64 {
    f64::from(ctx.text.measure_style("Ag", None, style).1)
}

fn wrap_lines(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    style: &TextStyle,
    width: f64,
    max_lines: usize,
) -> Vec<String> {
    let fits = |ctx: &mut PaintCtx<'_>, line: &str| {
        f64::from(ctx.text.measure_style(line, None, style).0) <= width
    };
    let elide = |ctx: &mut PaintCtx<'_>, line: &str| {
        ctx.text
            .elide_to_width_style(line, tilo_ui_base::num::f32_of_f64(width.max(0.0)), style)
    };
    let mut lines = Vec::new();
    let mut words = text.split_whitespace().peekable();
    while words.peek().is_some() {
        if lines.len() + 1 >= max_lines {
            let rest: Vec<&str> = words.collect();
            lines.push(elide(ctx, &rest.join(" ")));
            break;
        }
        let mut line = String::new();
        while let Some(word) = words.peek() {
            let candidate = if line.is_empty() {
                (*word).to_string()
            } else {
                format!("{line} {word}")
            };
            if line.is_empty() || fits(ctx, &candidate) {
                line = candidate;
                words.next();
            } else {
                break;
            }
        }
        let shown = if fits(ctx, &line) {
            line
        } else {
            elide(ctx, &line)
        };
        lines.push(shown);
    }
    lines
}

/// What painting a day or week grid found: the geometry it used, what a press lands on, and how
/// many items it drew (so a view can check it only drew what its viewport shows).
pub struct TimelineReport {
    pub geom: TimelineGeom,
    pub hits: Vec<(Rect, TimelineTarget)>,
    pub painted: usize,
}

/// Report of painting a month grid.
pub struct MonthReport {
    pub hits: Vec<(Rect, MonthTarget)>,
    pub painted: usize,
}

/// The day and week grid: hour rows, a day header with an all-day lane, events at their times,
/// and the line where now is.
pub struct TimelineFace<'a> {
    pub days: &'a [Date],
    pub tokens: &'a TimelineTokens,
    pub today: Date,
    pub now: Option<(Date, i64)>,
    pub items: &'a [TimelineItem],
    pub untitled: &'a str,
    pub allday_rows: usize,
    pub ghost: Option<&'a Ghost>,
    /// The item being dragged: left out of the grid, where its ghost stands instead.
    pub moving: Option<String>,
}

impl TimelineFace<'_> {
    /// Paint the grid into `area`, scrolled by `offset_y`, and return what a press lands on.
    #[must_use]
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, area: Rect, offset_y: f64) -> TimelineReport {
        let theme = ctx.theme;
        let ground = theme.tokens.colors.surfaces.content;
        let mut hits = Vec::new();
        let mut painted = 0;

        let geom = geometry(area, self.tokens, self.days.len(), self.allday_rows);
        self.draw_headers(ctx, area, geom, &mut hits);
        hairline(
            ctx.scene,
            ctx.transform,
            ground,
            Point::new(area.x0, geom.body_top),
            Point::new(geom.grid_right, geom.body_top),
        );

        let body = geom.body(area.x0, area.x1);
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        self.draw_grid(ctx, area, geom, offset_y);
        self.draw_events(ctx, area, geom, offset_y, &body, &mut hits, &mut painted);
        if let Some(ghost) = self.ghost {
            self.draw_ghost(ctx, area, geom, offset_y, ghost);
        }
        ctx.scene.pop_layer();

        TimelineReport {
            geom,
            hits,
            painted,
        }
    }

    fn column(&self, day: Date) -> Option<usize> {
        self.days.iter().position(|each| *each == day)
    }

    fn draw_headers(
        &self,
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        geom: TimelineGeom,
        hits: &mut Vec<(Rect, TimelineTarget)>,
    ) {
        let ty = &self.tokens.text;
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let (s1, s2) = (
            f64::from(theme.tokens.spacing.s1),
            f64::from(theme.tokens.spacing.s2),
        );
        let grid_x0 = area.x0 + geom.gutter;
        let col_w = column_width(area, self.days.len(), geom);
        let weekday_h = line_height(ctx, &ty.weekday);
        let day_h = line_height(ctx, &ty.day);
        let weekday_y = area.y0 + s2;
        let day_y = weekday_y + weekday_h;
        for (index, day) in self.days.iter().enumerate() {
            let x0 = grid_x0 + col_w * index as f64;
            let header = Rect::new(x0, area.y0, x0 + col_w, area.y0 + geom.head_h);
            let is_today = *day == self.today;
            let weekend = day.weekday().to_monday_zero_offset() >= 5;
            let text_x = x0 + s2;
            let weekday = format::weekday_short(day.weekday());
            ctx.text(
                &weekday,
                &ty.weekday,
                if weekend {
                    colors.text_dim
                } else {
                    colors.text_primary
                },
                Point::new(text_x, weekday_y),
                None,
            );
            let number = day.day().to_string();
            if is_today {
                let number_w = f64::from(ctx.text.measure_style(&number, None, &ty.day).0);
                let side = day_h.max(number_w + s2);
                let disc = Rect::new(text_x - s1, day_y, text_x - s1 + side, day_y + day_h);
                ctx.fill_round(disc, day_h / 2.0, colors.intent.accent);
                text_centered(ctx, &number, &ty.day, colors.intent.on_accent, disc);
            } else {
                ctx.text(
                    &number,
                    &ty.day,
                    if weekend {
                        colors.text_dim
                    } else {
                        colors.text_primary
                    },
                    Point::new(text_x, day_y),
                    None,
                );
            }
            hits.push((header, TimelineTarget::OpenDay(*day)));

            let mut day_items: Vec<&TimelineItem> = self
                .items
                .iter()
                .filter(|item| item.day == *day && item.all_day)
                .collect();
            day_items.sort_by_key(|item| std::cmp::Reverse(item.nights()));
            let rows = geom.allday_rows;
            let chip_h = if rows <= 1 {
                geom.allday_h - 2.0 * s1
            } else {
                line_height(ctx, &ty.strip)
            };
            for (row, item) in day_items.iter().enumerate().take(rows) {
                let top = area.y0 + geom.head_h + s1 + row as f64 * (chip_h + s1);
                let strip = Rect::new(
                    x0 + geom.block_inset,
                    top,
                    x0 + col_w - geom.block_inset,
                    top + chip_h,
                );
                if row + 1 == rows && day_items.len() > rows {
                    let more = format!("+{} more", day_items.len() - row);
                    text_in_band(
                        ctx,
                        &more,
                        &ty.strip,
                        colors.text_dim,
                        strip.x0 + s1 + s1 / 2.0,
                        strip,
                        strip.width() - 3.0 * s1,
                    );
                    hits.push((strip, TimelineTarget::OpenDay(*day)));
                    break;
                }
                let density = if rows <= 1 {
                    geom.density
                } else {
                    TimelineDensity::Regular
                };
                Self::draw_all_day_chip(ctx, ty, strip, item, density);
                hits.push((strip, TimelineTarget::Select(item.id.clone())));
            }
        }
    }

    fn draw_all_day_chip(
        ctx: &mut PaintCtx<'_>,
        ty: &TimelineText,
        strip: Rect,
        item: &TimelineItem,
        density: TimelineDensity,
    ) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let s1 = f64::from(theme.tokens.spacing.s1);
        let tinted = colors.tinted_chip_on(
            colors.surfaces.content,
            item.color,
            ALL_DAY_WASH,
            SMALL_TEXT_MIN_CONTRAST,
        );
        card(
            ctx.scene,
            ctx.transform,
            strip,
            f64::from(theme.tokens.radii.detail),
            tinted.fill,
        );
        let width = strip.width() - 3.0 * s1;
        let lines = wrap_lines(ctx, &item.title, &ty.strip, width, density.chip_lines());
        let strip_h = line_height(ctx, &ty.strip);
        #[allow(clippy::cast_precision_loss, reason = "at most two lines")]
        let block_h = strip_h * lines.len() as f64;
        let mut y = strip.center().y - block_h / 2.0;
        for line in &lines {
            let band = Rect::new(strip.x0, y, strip.x1, y + strip_h);
            text_in_band(
                ctx,
                line,
                &ty.strip,
                tinted.ink,
                strip.x0 + s1 + s1 / 2.0,
                band,
                width,
            );
            y += strip_h;
        }
    }

    fn draw_grid(&self, ctx: &mut PaintCtx<'_>, area: Rect, geom: TimelineGeom, offset_y: f64) {
        let ty = &self.tokens.text;
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let ground = colors.surfaces.content;
        let s1 = f64::from(theme.tokens.spacing.s1);
        let s3 = f64::from(theme.tokens.spacing.s3);
        let grid_x0 = area.x0 + geom.gutter;
        let col_w = column_width(area, self.days.len(), geom);
        let y_at = |minute: f64| geom.body_top + minute / 60.0 * geom.hour_h - offset_y;
        let body = geom.body(area.x0, area.x1);
        for hour in 0..=24 {
            let y = y_at(f64::from(hour) * 60.0);
            hairline(
                ctx.scene,
                ctx.transform,
                ground,
                Point::new(grid_x0, y),
                Point::new(geom.grid_right, y),
            );
            if hour < 24 {
                ctx.text(
                    &hour_label(hour),
                    &ty.hour,
                    colors.text_dim,
                    Point::new(area.x0 + s3, y + s1 / 2.0),
                    None,
                );
            }
        }
        for index in 0..=self.days.len() {
            let x = grid_x0 + col_w * index as f64;
            hairline(
                ctx.scene,
                ctx.transform,
                ground,
                Point::new(x, body.y0),
                Point::new(x, body.y1),
            );
        }
        if let Some((now_day, now_min)) = self.now
            && let Some(index) = self.column(now_day)
        {
            let y = y_at(now_min as f64);
            let x0 = grid_x0 + col_w * index as f64;
            let live = colors.semantic.danger;
            stroke_line(
                ctx.scene,
                ctx.transform,
                live,
                Point::new(x0, y),
                Point::new(x0 + col_w, y),
                s1 / 2.0,
            );
            disc(ctx, Point::new(x0, y), s1, live);
        }
    }

    fn item_splits(&self) -> Vec<Split> {
        let mut out = vec![Split::default(); self.items.len()];
        for day in self.days {
            let indices: Vec<usize> = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.day == *day && !item.all_day)
                .map(|(index, _)| index)
                .collect();
            let intervals: Vec<(i64, i64)> = indices
                .iter()
                .map(|&index| {
                    let item = &self.items[index];
                    (item.start_min, item.end_min)
                })
                .collect();
            let stacks: Vec<i64> = indices
                .iter()
                .map(|&index| self.items[index].stack)
                .collect();
            for (slot, split) in indices.into_iter().zip(splits_for(&intervals, &stacks)) {
                out[slot] = split;
            }
        }
        out
    }

    fn draw_events(
        &self,
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        geom: TimelineGeom,
        offset_y: f64,
        body: &Rect,
        hits: &mut Vec<(Rect, TimelineTarget)>,
        painted: &mut usize,
    ) {
        let ty = &self.tokens.text;
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let s1 = f64::from(theme.tokens.spacing.s1);
        let radius = f64::from(theme.tokens.radii.item);
        let item_splits = self.item_splits();
        for (index, item) in self.items.iter().enumerate() {
            if item.all_day || self.moving.as_deref() == Some(item.id.as_str()) {
                continue;
            }
            let Some(col) = self.column(item.day) else {
                continue;
            };
            let block = in_split(
                block_rect(
                    area,
                    self.days.len(),
                    geom,
                    offset_y,
                    col,
                    item.start_min,
                    item.end_min,
                ),
                item_splits[index],
            );
            let visible = block.intersect(*body);
            if visible.width() <= 0.0 || visible.height() <= 0.0 {
                continue;
            }
            *painted += 1;
            let tinted = colors.tinted_chip_on(
                colors.surfaces.content,
                item.color,
                0.22,
                SMALL_TEXT_MIN_CONTRAST,
            );
            card(ctx.scene, ctx.transform, block, radius, tinted.fill);
            ctx.scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                ctx.transform,
                &RoundedRect::from_rect(block, radius),
            );
            ctx.fill_rect(
                Rect::new(block.x0, block.y0, block.x0 + s1, block.y1),
                item.color,
            );
            let text_x = block.x0 + 2.0 * s1;
            let text_w = (block.x1 - s1 - text_x).max(0.0);
            let title_h = line_height(ctx, &ty.title);
            let subtitle_h = line_height(ctx, &ty.subtitle);
            let top = block.y0.max(body.y0.min(block.y1 - title_h - 2.0 * s1));
            let room = block.y1 - top - 2.0 * s1;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a positive count of lines"
            )]
            let max_lines = ((room / title_h).floor() as usize).max(1);
            let mut y = top + s1;
            for line in wrap_lines(ctx, &item.title, &ty.title, text_w, max_lines) {
                ctx.text(&line, &ty.title, tinted.ink, Point::new(text_x, y), None);
                y += title_h;
            }
            let time = format::range(
                &minute_text(item.start_min),
                &minute_text(item.clock_end_min.min(super::MINUTES_PER_DAY)),
            );
            for line in [time.as_str(), item.location.as_str()] {
                if line.is_empty() || y + subtitle_h > block.y1 - s1 + 0.5 {
                    break;
                }
                let line = ctx.text.elide_to_width_style(
                    line,
                    tilo_ui_base::num::f32_of_f64(text_w),
                    &ty.subtitle,
                );
                ctx.text(&line, &ty.subtitle, tinted.ink, Point::new(text_x, y), None);
                y += subtitle_h;
            }
            ctx.scene.pop_layer();
            hits.push((visible, TimelineTarget::Select(item.id.clone())));
        }
    }

    fn draw_ghost(
        &self,
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        geom: TimelineGeom,
        offset_y: f64,
        ghost: &Ghost,
    ) {
        let ty = &self.tokens.text;
        let theme = ctx.theme;
        let s1 = f64::from(theme.tokens.spacing.s1);
        let radius = f64::from(theme.tokens.radii.item);
        let block = block_rect(
            area,
            self.days.len(),
            geom,
            offset_y,
            ghost.column,
            ghost.start_min,
            ghost.end_min,
        );
        ctx.fill_round(block, radius, ghost.fill);
        ctx.stroke_round(block, radius, ghost.edge, s1 / 2.0);
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &block,
        );
        let ink = theme.text_primary;
        let text_x = block.x0 + 2.0 * s1;
        let title = ctx.text.elide_to_width_style(
            &ghost.title,
            tilo_ui_base::num::f32_of_f64((block.x1 - s1 - text_x).max(0.0)),
            &ty.title,
        );
        let title_h = line_height(ctx, &ty.title);
        let subtitle_h = line_height(ctx, &ty.subtitle);
        ctx.text(
            &title,
            &ty.title,
            ink,
            Point::new(text_x, block.y0 + s1),
            None,
        );
        if block.height() > s1 + title_h + subtitle_h {
            ctx.text(
                &format::range(&minute_text(ghost.start_min), &minute_text(ghost.end_min)),
                &ty.subtitle,
                ink,
                Point::new(text_x, block.y0 + s1 + title_h),
                None,
            );
        }
        ctx.scene.pop_layer();
    }
}

/// One day's cell in the month grid: its rectangle, the day, where its chips start, and the
/// events that stand on it that day.
struct MonthCell<'a> {
    rect: Rect,
    day: Date,
    chips_top: f64,
    items: &'a [&'a TimelineItem],
}

struct MonthMetrics {
    header_h: f64,
    inset: f64,
    pill_r: f64,
    chips_top: f64,
    chip_h: f64,
    chip_pitch: f64,
    weekday: TextStyle,
    day: TextStyle,
    chip: TextStyle,
}

impl MonthMetrics {
    fn new(ctx: &mut PaintCtx<'_>) -> Self {
        let theme = ctx.theme;
        let sp = &theme.tokens.spacing;
        let weekday = theme.text(TextRole::Caption);
        let day = theme.text(TextRole::Label).numeric();
        let chip = theme.text(TextRole::Caption);
        let (s1, s2) = (f64::from(sp.s1), f64::from(sp.s2));
        let header_h = (line_height(ctx, &weekday) + 2.0 * s2).ceil();
        let pill_r = f64::midpoint(line_height(ctx, &day), s1).ceil();
        let chips_top = s1 + 2.0 * pill_r + s1;
        let chip_h = (line_height(ctx, &chip) + s1).ceil();
        Self {
            header_h,
            inset: s1,
            pill_r,
            chips_top,
            chip_h,
            chip_pitch: chip_h + s1 / 2.0,
            weekday,
            day,
            chip,
        }
    }
}

/// The month grid: weekday heads, six weeks of day cells, and the events in each cell, with
/// multi-day events drawn as one bar across the days they cover.
pub struct MonthFace<'a> {
    pub anchor: Date,
    pub today: Date,
    pub items: &'a [TimelineItem],
    pub untitled: &'a str,
}

impl MonthFace<'_> {
    #[must_use]
    pub fn paint(
        &self,
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        dropped_on: Option<Date>,
    ) -> MonthReport {
        let theme = ctx.theme;
        let ground = theme.tokens.colors.surfaces.content;
        let mut hits = Vec::new();
        let mut painted = 0;
        let m = MonthMetrics::new(ctx);
        let cols = 7;
        let rows = 6;
        let cell_w = area.width() / f64::from(cols);
        let grid_top = area.y0 + m.header_h;
        let cell_h = (area.height() - m.header_h) / rows as f64;

        for (i, weekday) in format::week().into_iter().enumerate() {
            let label = format::weekday_short(weekday);
            let x = area.x0 + cell_w * i as f64;
            let head = Rect::new(x, area.y0, x + cell_w, grid_top);
            text_in_band(
                ctx,
                &label,
                &m.weekday,
                theme.tokens.colors.text_dim,
                x + 2.0 * m.inset,
                head,
                cell_w,
            );
        }

        let grid = super::month_grid(self.anchor);
        for row in 0..rows {
            let week = &grid[row * 7..row * 7 + 7];
            // The multi-day events that cross this week, laid in lanes across the days they cover.
            let mut spanning: Vec<&TimelineItem> = self
                .items
                .iter()
                .filter(|item| {
                    item.spans_days()
                        && item.day < week[6] + jiff::Span::new().days(1)
                        && item.end_day >= week[0]
                })
                .collect();
            spanning.sort_by_key(|item| (item.day, std::cmp::Reverse(item.nights())));
            let mut lanes: Vec<i64> = Vec::new();
            let mut bars: Vec<(Rect, &TimelineItem)> = Vec::new();
            for item in spanning {
                let start = week.iter().position(|d| *d >= item.day).unwrap_or(0);
                let end = week
                    .iter()
                    .rposition(|d| *d <= item.end_day)
                    .map_or(6, |index| index);
                let mut lane = lanes
                    .iter()
                    .position(|lane_end| *lane_end < i64::try_from(start).unwrap_or(0));
                if lane.is_none() {
                    lanes.push(-1);
                    lane = Some(lanes.len() - 1);
                }
                let Some(free) = lane else {
                    continue;
                };
                lanes[free] = i64::try_from(end).unwrap_or(0);
                let x0 = area.x0 + cell_w * start as f64 + m.inset;
                let x1 = area.x0 + cell_w * (end + 1) as f64 - m.inset;
                let top = grid_top + cell_h * row as f64 + m.inset + free as f64 * m.chip_pitch;
                bars.push((Rect::new(x0, top, x1, top + m.chip_h), item));
            }
            let lane_rows = lanes.len();
            for (bar, item) in &bars {
                let tinted = theme.tokens.colors.tinted_chip_on(
                    ground,
                    item.color,
                    if item.all_day { ALL_DAY_WASH } else { 0.22 },
                    SMALL_TEXT_MIN_CONTRAST,
                );
                card(
                    ctx.scene,
                    ctx.transform,
                    *bar,
                    f64::from(theme.tokens.radii.detail),
                    tinted.fill,
                );
                text_in_band(
                    ctx,
                    &item.title,
                    &m.chip,
                    tinted.ink,
                    bar.x0 + m.inset,
                    *bar,
                    bar.width() - 2.0 * m.inset,
                );
                hits.push((*bar, MonthTarget::OpenEvent(item.id.clone())));
                painted += 1;
            }
            let row_top = grid_top + cell_h * row as f64;
            let chips_top = m.chips_top + lane_rows as f64 * m.chip_pitch;
            for col in 0..7 {
                let day = week[col];
                let cell = Rect::new(
                    area.x0 + cell_w * col as f64,
                    row_top,
                    area.x0 + cell_w * (col as f64 + 1.0),
                    row_top + cell_h,
                );
                let day_items: Vec<&TimelineItem> = self
                    .items
                    .iter()
                    .filter(|item| item.day == day && !item.spans_days())
                    .collect();
                let month_cell = MonthCell {
                    rect: cell,
                    day,
                    chips_top,
                    items: &day_items,
                };
                let count = self.draw_month_cell(ctx, &m, month_cell, dropped_on, &mut hits);
                painted += count;
            }
        }

        for col in 0..=cols {
            let x = if col == cols {
                area.x1 - 1.0
            } else {
                area.x0 + cell_w * f64::from(col)
            };
            hairline(
                ctx.scene,
                ctx.transform,
                ground,
                Point::new(x, grid_top),
                Point::new(x, area.y1),
            );
        }
        for row in 0..=rows {
            let y = if row == rows {
                area.y1 - 1.0
            } else {
                grid_top + cell_h * row as f64
            };
            hairline(
                ctx.scene,
                ctx.transform,
                ground,
                Point::new(area.x0, y),
                Point::new(area.x1, y),
            );
        }

        MonthReport { hits, painted }
    }

    fn draw_month_cell(
        &self,
        ctx: &mut PaintCtx<'_>,
        m: &MonthMetrics,
        cell: MonthCell<'_>,
        dropped_on: Option<Date>,
        hits: &mut Vec<(Rect, MonthTarget)>,
    ) -> usize {
        let MonthCell {
            rect: cell,
            day,
            chips_top,
            items,
        } = cell;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        hits.push((cell, MonthTarget::PickDate(day)));
        hits.push((
            Rect::new(cell.x0, cell.y0, cell.x1, cell.y0 + chips_top),
            MonthTarget::OpenDay(day),
        ));
        let in_month = day.month() == self.anchor.month();
        let is_today = day == self.today;
        let is_anchor = day == self.anchor;
        let weekend = day.weekday().to_monday_zero_offset() >= 5;
        let cell_ground = if is_anchor || dropped_on == Some(day) {
            ctx.fill_rect(cell, c.intent.selection);
            c.intent.selection
        } else {
            c.surfaces.content
        };

        let num = day.day().to_string();
        let pill_center = Point::new(cell.x0 + m.inset + m.pill_r, cell.y0 + m.inset + m.pill_r);
        let num_rect = Rect::from_center_size(pill_center, (2.0 * m.pill_r, 2.0 * m.pill_r));
        if is_today {
            disc(ctx, pill_center, m.pill_r, c.intent.accent);
            text_centered(
                ctx,
                &num,
                &m.day.clone().strong(),
                c.intent.on_accent,
                num_rect,
            );
        } else {
            let ink = if !in_month {
                c.text_tertiary
            } else if weekend {
                c.text_dim
            } else {
                c.text_primary
            };
            text_centered(ctx, &num, &m.day, ink, num_rect);
        }

        self.draw_chips(
            ctx,
            m,
            MonthCell {
                rect: cell,
                day,
                chips_top,
                items,
            },
            cell_ground,
            hits,
        )
    }

    fn draw_chips(
        &self,
        ctx: &mut PaintCtx<'_>,
        m: &MonthMetrics,
        cell: MonthCell<'_>,
        cell_ground: Color,
        hits: &mut Vec<(Rect, MonthTarget)>,
    ) -> usize {
        let MonthCell {
            rect: cell,
            day,
            chips_top,
            items,
        } = cell;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &cell,
        );
        let cell_h = cell.height();
        let available = (cell_h - chips_top).max(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small positive slot count"
        )]
        let slots = ((available / m.chip_pitch).floor().max(0.0) as usize).clamp(1, 3);
        let max_chips = if items.len() > slots {
            slots.saturating_sub(1).max(1)
        } else {
            items.len()
        };
        let radius = f64::from(theme.tokens.radii.detail);
        let pad = f64::from(sp.s1);
        let dot_r = (m.chip_h / 8.0).round().max(2.0);
        let density = TimelineDensity::of(cell.width(), theme);
        let mut y = cell.y0 + chips_top;
        let mut painted = 0;
        for (i, item) in items.iter().enumerate() {
            let chip = Rect::new(cell.x0 + m.inset, y, cell.x1 - m.inset, y + m.chip_h);
            if i >= max_chips {
                let more = format!("+{} more", items.len() - i);
                text_in_band(
                    ctx,
                    &more,
                    &m.chip,
                    c.text_dim,
                    chip.x0 + pad,
                    chip,
                    chip.width() - 2.0 * pad,
                );
                hits.push((
                    Rect::new(chip.x0, chip.y0, chip.x1, chip.y1.min(cell.y1)),
                    MonthTarget::OpenDay(day),
                ));
                break;
            }
            if item.all_day {
                let tinted = c.tinted_chip_on(
                    cell_ground,
                    item.color,
                    ALL_DAY_WASH,
                    SMALL_TEXT_MIN_CONTRAST,
                );
                card(ctx.scene, ctx.transform, chip, radius, tinted.fill);
                text_in_band(
                    ctx,
                    &item.title,
                    &m.chip,
                    tinted.ink,
                    chip.x0 + pad,
                    chip,
                    chip.width() - 2.0 * pad,
                );
            } else {
                let tinted =
                    c.tinted_chip_on(cell_ground, item.color, 0.18, SMALL_TEXT_MIN_CONTRAST);
                card(ctx.scene, ctx.transform, chip, radius, tinted.fill);
                let text_x = if density.chip_marker() {
                    let dot = Point::new(chip.x0 + pad + dot_r, chip.center().y);
                    disc(ctx, dot, dot_r, item.color);
                    dot.x + dot_r + pad
                } else {
                    chip.x0 + pad
                };
                let timed = format!("{} {}", minute_text(item.start_min), item.title);
                let room = chip.x1 - pad - text_x;
                let label = if density.chip_marker()
                    && f64::from(ctx.text.measure_style(&timed, None, &m.chip).0) <= room
                {
                    timed
                } else {
                    item.title.clone()
                };
                text_in_band(
                    ctx,
                    &label,
                    &m.chip,
                    tinted.ink,
                    text_x,
                    chip,
                    chip.x1 - pad - text_x,
                );
            }
            hits.push((chip, MonthTarget::OpenEvent(item.id.clone())));
            painted += 1;
            y += m.chip_pitch;
        }
        ctx.scene.pop_layer();
        painted
    }
}
