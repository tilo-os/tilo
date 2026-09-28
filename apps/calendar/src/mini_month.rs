//! Compact calendar navigator shown at the bottom of the sidebar.
//!
//! The component owns the reserved footer geometry, painting, and pointer
//! targets. It deliberately does not own the selected date: that date is the
//! application's shared anchor for month, week, day, and agenda views. The
//! root translates [`Outcome`] values into changes to that shared policy.

use std::collections::HashSet;
use ui::paint::PaintCtx;

use chrono::{Datelike, NaiveDate};
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::Fill;

use crate::app::{center_text, draw_chevron};
use crate::date;
use crate::draw_util::{fill_round, stroke_round};
use ui::surface::separator;

const RESERVED_HEIGHT: f64 = 196.0;
const BOTTOM_INSET: f64 = 12.0;
const DIVIDER_GAP: f64 = 8.0;
const HORIZONTAL_PAD: f64 = 18.0;
const HEADER_HEIGHT: f64 = 18.0;
const WEEKDAY_TOP: f64 = 24.0;
const WEEKDAY_HEIGHT: f64 = 14.0;
const GRID_GAP: f64 = 4.0;
const CELL_HEIGHT: f64 = 22.0;

/// Navigation intent produced by a mini-month pointer target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    SelectDate(NaiveDate),
    ShiftMonth(i32),
}

#[derive(Clone, Copy, Debug)]
struct Layout {
    region: Rect,
    divider_y: f64,
    previous: Rect,
    next: Rect,
    grid_x0: f64,
    cell_width: f64,
    weekday_y: f64,
    grid_y: f64,
}

/// Sidebar mini-month renderer and semantic target builder, over the month
/// around `anchor` with `today` and the dates carrying events marked.
pub(crate) struct MiniMonth<'a> {
    pub(crate) anchor: NaiveDate,
    pub(crate) today: NaiveDate,
    pub(crate) event_dates: &'a HashSet<NaiveDate>,
}

impl MiniMonth<'_> {
    /// Upper boundary reserved for this component. Sidebar content may use the
    /// space above it, while the mini-month decides whether the remaining room
    /// is sufficient to paint without overlap.
    pub(crate) fn content_limit(sidebar: Rect) -> f64 {
        sidebar.y1 - RESERVED_HEIGHT
    }

    /// Paint the component if `occupied_until` has not entered its reserved
    /// footer band. Targets are rebuilt from the same layout used for drawing.
    #[allow(
        clippy::too_many_lines,
        reason = "the component paints one compact header and one fixed 42-cell grid; keeping the shared geometry in one pass makes its targets and pixels visibly correspond"
    )]
    pub(crate) fn draw(
        &self,
        ctx: &mut PaintCtx<'_>,
        sidebar: Rect,
        occupied_until: f64,
        pointer: Point,
    ) -> Vec<(Rect, Outcome)> {
        let Some(layout) = Self::layout(sidebar, occupied_until) else {
            return Vec::new();
        };
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = ctx;
        let (transform, theme) = (*transform, *theme);
        let (anchor, today, event_dates) = (self.anchor, self.today, self.event_dates);
        let colors = &theme.tokens.colors;

        // The rule above the navigator is derived against the sidebar's own
        // raised ground (HIG §9.5), inset like a Panel separator.
        separator(
            scene,
            transform,
            sidebar.x0,
            sidebar.x1,
            layout.divider_y,
            HORIZONTAL_PAD,
            colors.surfaces.raised,
        );
        text.draw_text(
            scene,
            &date::month_title(anchor),
            13.0,
            colors.text_primary,
            transform * Affine::translate((layout.region.x0 + HORIZONTAL_PAD, layout.region.y0)),
            None,
        );
        if layout.previous.contains(pointer) {
            fill_round(scene, transform, layout.previous, 6.0, colors.lift(0.35));
        }
        if layout.next.contains(pointer) {
            fill_round(scene, transform, layout.next, 6.0, colors.lift(0.35));
        }
        draw_chevron(
            scene,
            transform,
            colors.neutral[7],
            layout.previous.center(),
            true,
        );
        draw_chevron(
            scene,
            transform,
            colors.neutral[7],
            layout.next.center(),
            false,
        );

        for (index, label) in date::WEEKDAY_NARROW.iter().enumerate() {
            center_text(
                scene,
                text,
                label,
                10.0,
                colors.text_dim,
                Rect::new(
                    layout.grid_x0 + layout.cell_width * index as f64,
                    layout.weekday_y,
                    layout.grid_x0 + layout.cell_width * (index as f64 + 1.0),
                    layout.weekday_y + WEEKDAY_HEIGHT,
                ),
                transform,
            );
        }

        for (index, day) in date::month_grid(anchor).iter().enumerate() {
            let cell = layout.day_cell(index);
            let in_month = day.month() == anchor.month();
            let is_today = *day == today;
            let is_anchor = *day == anchor;

            if is_today {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &Circle::new(cell.center(), 10.0),
                );
            } else if is_anchor {
                stroke_round(
                    scene,
                    transform,
                    cell.inflate(-3.0, -2.0),
                    7.0,
                    colors.accent[5],
                    1.2,
                );
            } else if cell.contains(pointer) {
                fill_round(
                    scene,
                    transform,
                    cell.inflate(-3.0, -2.0),
                    7.0,
                    colors.lift(0.35),
                );
            }
            let number_color = if is_today {
                colors.on_accent(4.5)
            } else if !in_month {
                colors.neutral[4]
            } else {
                colors.neutral[8]
            };
            center_text(
                scene,
                text,
                &day.day().to_string(),
                11.0,
                number_color,
                cell,
                transform,
            );
            if event_dates.contains(day) && !is_today {
                let dot = Point::new(cell.center().x, cell.y1 - 2.5);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &Circle::new(dot, 1.6),
                );
            }
        }
        layout.targets(anchor)
    }

    fn layout(sidebar: Rect, occupied_until: f64) -> Option<Layout> {
        let mini_top = Self::content_limit(sidebar);
        if occupied_until >= mini_top - DIVIDER_GAP {
            return None;
        }

        let region = Rect::new(sidebar.x0, mini_top, sidebar.x1, sidebar.y1 - BOTTOM_INSET);
        let previous = Rect::new(
            region.x1 - HORIZONTAL_PAD - 46.0,
            region.y0 - 4.0,
            region.x1 - HORIZONTAL_PAD - 24.0,
            region.y0 + HEADER_HEIGHT,
        );
        let next = Rect::new(
            region.x1 - HORIZONTAL_PAD - 22.0,
            region.y0 - 4.0,
            region.x1 - HORIZONTAL_PAD,
            region.y0 + HEADER_HEIGHT,
        );
        let grid_x0 = region.x0 + HORIZONTAL_PAD;
        let weekday_y = region.y0 + WEEKDAY_TOP;
        Some(Layout {
            region,
            divider_y: mini_top - DIVIDER_GAP,
            previous,
            next,
            grid_x0,
            cell_width: (region.width() - 2.0 * HORIZONTAL_PAD) / 7.0,
            weekday_y,
            grid_y: weekday_y + WEEKDAY_HEIGHT + GRID_GAP,
        })
    }
}

impl Layout {
    fn day_cell(self, index: usize) -> Rect {
        let row = (index / 7) as f64;
        let column = (index % 7) as f64;
        Rect::new(
            self.grid_x0 + self.cell_width * column,
            self.grid_y + CELL_HEIGHT * row,
            self.grid_x0 + self.cell_width * (column + 1.0),
            self.grid_y + CELL_HEIGHT * (row + 1.0),
        )
    }

    fn targets(self, anchor: NaiveDate) -> Vec<(Rect, Outcome)> {
        let mut targets = vec![
            (self.previous, Outcome::ShiftMonth(-1)),
            (self.next, Outcome::ShiftMonth(1)),
        ];
        targets.extend(
            date::month_grid(anchor)
                .iter()
                .enumerate()
                .map(|(index, day)| (self.day_cell(index), Outcome::SelectDate(*day))),
        );
        targets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn layout_tracks_the_sidebar_exposes_grid_targets_and_hides_when_crowded() {
        // Targets match the six-week grid and month controls.
        let anchor = date(2026, 9, 15);
        let sidebar = Rect::new(20.0, 10.0, 272.0, 810.0);
        let targets = MiniMonth::layout(sidebar, 200.0).unwrap().targets(anchor);

        assert_eq!(targets.len(), 44, "two chevrons plus 42 dates");
        assert_eq!(targets[0].1, Outcome::ShiftMonth(-1));
        assert_eq!(targets[1].1, Outcome::ShiftMonth(1));
        assert_eq!(
            targets[2].1,
            Outcome::SelectDate(date::month_grid(anchor)[0])
        );
        assert_eq!(
            targets[43].1,
            Outcome::SelectDate(date::month_grid(anchor)[41])
        );

        // Layout tracks the sidebar instead of caching first-frame geometry.
        let first = MiniMonth::layout(Rect::new(0.0, 0.0, 252.0, 800.0), 100.0)
            .unwrap()
            .day_cell(0);

        let resized = MiniMonth::layout(Rect::new(40.0, 30.0, 332.0, 930.0), 100.0)
            .unwrap()
            .day_cell(0);
        assert!(resized.x0 > first.x0);
        assert!(resized.width() > first.width());
        assert!(resized.y0 > first.y0);

        // A crowded sidebar hides the component.
        let sidebar = Rect::new(0.0, 0.0, 252.0, 800.0);
        assert!(MiniMonth::layout(sidebar, 100.0).is_some());
        assert!(MiniMonth::layout(sidebar, MiniMonth::content_limit(sidebar)).is_none());
    }
}
