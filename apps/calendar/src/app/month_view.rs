//! The month grid: weekday heads, six week rows of day cells, and the event
//! chips inside each cell. Genuine content drawing: every value comes from
//! the theme, every label from a text role.

use chrono::{Datelike, NaiveDate, NaiveDateTime};
use vello::kurbo::{Point, Rect};
use vello::peniko::Fill;

use ui::paint::PaintCtx;
use ui::surface::card;
use ui::text_render::TextStyle;
use ui::tokens::TextRole;
use ui::tokens::colors::SMALL_TEXT_MIN_CONTRAST;

use super::{Action, CalendarApp};
use crate::date;
use crate::draw_util::{hairline, line_height, text_centered};
use crate::model::{Id, Palette};

/// Month-grid geometry for the current theme, solved once per frame and
/// shared by the weekday header, the day cells, their chips and the hit
/// rects pushed alongside them.
struct MonthMetrics {
    /// Weekday header row height.
    header_h: f64,
    /// Inset of the day number and the chips from the cell's edges.
    inset: f64,
    /// Radius of the today disc; every day number is centred on its axis.
    pill_r: f64,
    /// Top of the first event chip inside a cell (also the `OpenDay` strip).
    chips_top: f64,
    /// Event chip height and stacking pitch.
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

/// One visible event in a month cell, as the chip painter needs it.
struct CellEvent {
    id: Id,
    title: String,
    color: Palette,
    all_day: bool,
    start: NaiveDateTime,
}

impl CellEvent {
    fn of(shown: &crate::shown::Shown<'_>) -> Self {
        Self {
            id: shown.event.id,
            title: shown.event.display_title().to_string(),
            color: shown.event.color,
            all_day: shown.event.all_day,
            start: shown.start,
        }
    }
}

impl CalendarApp {
    pub(super) fn draw_month(&self, ctx: &mut PaintCtx<'_>, area: Rect) -> Vec<(Rect, Action)> {
        let theme = ctx.theme;
        let ground = theme.tokens.colors.surfaces.content;
        let mut hits = Vec::new();
        let m = MonthMetrics::new(ctx);
        let cols = 7;
        let rows = 6;
        let cell_w = area.width() / f64::from(cols);
        let grid_top = area.y0 + m.header_h;
        let cell_h = (area.height() - m.header_h) / f64::from(rows);

        // Weekday heads share the day numbers' leading inset.
        for (i, weekday) in ui::format::week().into_iter().enumerate() {
            let label = ui::format::weekday_short(weekday);
            let x = area.x0 + cell_w * i as f64;
            let head = Rect::new(x, area.y0, x + cell_w, grid_top);
            crate::draw_util::text_in_band(
                ctx,
                &label,
                &m.weekday,
                theme.tokens.colors.text_dim,
                x + 2.0 * m.inset,
                head,
                cell_w,
            );
        }

        let grid = date::month_grid(self.anchor);
        let shown = self.visible_by_day(&grid);
        for (idx, (day, events)) in grid.into_iter().zip(shown).enumerate() {
            let row = idx / 7;
            let col = idx % 7;
            let cell = Rect::new(
                area.x0 + cell_w * col as f64,
                grid_top + cell_h * row as f64,
                area.x0 + cell_w * (col as f64 + 1.0),
                grid_top + cell_h * (row as f64 + 1.0),
            );
            let events = events.iter().map(CellEvent::of).collect();
            self.draw_month_cell(ctx, &m, cell, (day, events), &mut hits);
        }

        // Grid rules once, over the cell grounds, derived from the window
        // ground they stand on.
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
                grid_top + cell_h * f64::from(row)
            };
            hairline(
                ctx.scene,
                ctx.transform,
                ground,
                Point::new(area.x0, y),
                Point::new(area.x1, y),
            );
        }
        hits
    }

    /// One day cell: hit targets, the chosen-day lift, the day number (on
    /// the inverted disc for today) and the cell's chips.
    fn draw_month_cell(
        &self,
        ctx: &mut PaintCtx<'_>,
        m: &MonthMetrics,
        cell: Rect,
        (day, events): (NaiveDate, Vec<CellEvent>),
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;

        // Cell-level click targets go FIRST so the chips drawn below win the
        // reverse (topmost-last) hit scan; the date-number strip in turn
        // wins over the empty cell body and ends where the first chip starts.
        hits.push((cell, Action::PickDate(day)));
        hits.push((
            Rect::new(cell.x0, cell.y0, cell.x1, cell.y0 + m.chips_top),
            Action::OpenDay(day),
        ));

        let in_month = day.month() == self.anchor.month();
        let is_today = day == self.today;
        let is_anchor = day == self.anchor;
        let weekend = day.weekday().num_days_from_monday() >= 5;

        // The chosen day is the selection lift of the window ground.
        let cell_ground = if is_anchor {
            ctx.fill_rect(cell, c.intent.selection);
            c.intent.selection
        } else {
            c.surfaces.content
        };

        let num = day.day().to_string();
        let disc = Point::new(cell.x0 + m.inset + m.pill_r, cell.y0 + m.inset + m.pill_r);
        let num_rect = Rect::from_center_size(disc, (2.0 * m.pill_r, 2.0 * m.pill_r));
        if is_today {
            crate::draw_util::disc(ctx, disc, m.pill_r, c.intent.inverted);
            text_centered(
                ctx,
                &num,
                &m.day.clone().strong(),
                c.intent.on_inverted,
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

        Self::draw_month_cell_chips(ctx, m, (cell, cell_ground), (day, &events), hits);
    }

    /// The event chips inside one day cell, clipped to it. Both kinds are a
    /// wash of the calendar colour over the cell's ground: an all-day chip a
    /// deeper one spanning the day, a timed chip a light one with a dot.
    fn draw_month_cell_chips(
        ctx: &mut PaintCtx<'_>,
        m: &MonthMetrics,
        (cell, cell_ground): (Rect, vello::peniko::Color),
        (day, events): (NaiveDate, &[CellEvent]),
        hits: &mut Vec<(Rect, Action)>,
    ) {
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
        // Slots that fit below the day number, at most three. When the day
        // has more events than slots, the last slot carries the "+N more"
        // line instead of a chip, but at least one chip is always drawn.
        let cell_h = cell.height();
        let slots = (((cell_h - m.chips_top) / m.chip_pitch).floor().max(0.0) as usize).clamp(1, 3);
        let max_chips = if events.len() > slots {
            slots.saturating_sub(1).max(1)
        } else {
            events.len()
        };
        let radius = f64::from(theme.tokens.radii.detail);
        let pad = f64::from(sp.s1);
        let dot_r = (m.chip_h / 8.0).round().max(2.0);
        let density = ui::widget::TimelineDensity::of(cell.width(), theme);
        let mut y = cell.y0 + m.chips_top;
        for (i, event) in events.iter().enumerate() {
            let chip = Rect::new(cell.x0 + m.inset, y, cell.x1 - m.inset, y + m.chip_h);
            if i >= max_chips {
                let more = format!("+{} more", events.len() - i);
                crate::draw_util::text_in_band(
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
                    Action::OpenDay(day),
                ));
                break;
            }
            let color = event.color.color(theme);
            if event.all_day {
                let tinted = c.tinted_chip_on(
                    cell_ground,
                    color,
                    crate::draw_util::ALL_DAY_WASH,
                    SMALL_TEXT_MIN_CONTRAST,
                );
                card(ctx.scene, ctx.transform, chip, radius, tinted.fill);
                crate::draw_util::text_in_band(
                    ctx,
                    &event.title,
                    &m.chip,
                    tinted.ink,
                    chip.x0 + pad,
                    chip,
                    chip.width() - 2.0 * pad,
                );
            } else {
                let tinted = c.tinted_chip_on(cell_ground, color, 0.18, SMALL_TEXT_MIN_CONTRAST);
                card(ctx.scene, ctx.transform, chip, radius, tinted.fill);
                // A narrow cell gives the marker's room to the title: the
                // chip's tint already carries the calendar's colour.
                let text_x = if density.chip_marker() {
                    let dot = Point::new(chip.x0 + pad + dot_r, chip.center().y);
                    crate::draw_util::disc(ctx, dot, dot_r, color);
                    dot.x + dot_r + pad
                } else {
                    chip.x0 + pad
                };
                // The start time leads when the chip has room for it; a
                // narrow cell keeps the title, which says more.
                let timed = format!("{} {}", date::fmt_time(&event.start), event.title);
                let room = chip.x1 - pad - text_x;
                let label = if density.chip_marker()
                    && f64::from(ctx.text.measure_style(&timed, None, &m.chip).0) <= room
                {
                    timed
                } else {
                    event.title.clone()
                };
                crate::draw_util::text_in_band(
                    ctx,
                    &label,
                    &m.chip,
                    tinted.ink,
                    text_x,
                    chip,
                    chip.x1 - pad - text_x,
                );
            }
            hits.push((chip, Action::OpenEvent(event.id)));
            y += m.chip_pitch;
        }
        ctx.scene.pop_layer();
    }
}
