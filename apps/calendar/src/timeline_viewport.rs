//! Stateful viewport for Calendar's week/day timeline.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::Widget;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Scrollable, TimelineDensity};

use crate::input_context::InputContext;

/// The shortest an hour row gets: a list row's height, so a half-hour block
/// still holds its title.
#[cfg(test)]
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const"
)]
pub(super) const TIMELINE_MIN_HOUR_H: f64 = ui::tokens::Metrics::BASE.panel_row_height as f64;

/// The text styles the timeline paints, by what each piece of text is.
#[derive(Clone, Debug)]
pub(super) struct TimelineType {
    /// "Mon" over each day column.
    pub weekday: TextStyle,
    /// The day number under it.
    pub day: TextStyle,
    /// An all-day event's title in its strip.
    pub strip: TextStyle,
    /// A block's title and its time line.
    pub title: TextStyle,
    pub subtitle: TextStyle,
    /// "08:00" in the hour gutter.
    pub hour: TextStyle,
}

impl TimelineType {
    pub fn of(theme: &Theme) -> Self {
        Self {
            weekday: theme.text(TextRole::Caption),
            day: theme.text(TextRole::Title).numeric(),
            strip: theme.text(TextRole::Caption),
            title: theme.text(TextRole::Caption).strong(),
            subtitle: theme.text(TextRole::Caption),
            hour: theme.text(TextRole::Caption).numeric(),
        }
    }
}

/// The measured parts of the timeline's geometry for one theme: line
/// heights, the hour gutter and the spacing it is padded with.
#[derive(Clone, Copy, Debug)]
pub(super) struct TimelineTokens {
    weekday_h: f64,
    day_h: f64,
    strip_h: f64,
    title_h: f64,
    hour_w: f64,
    s1: f64,
    s2: f64,
    s3: f64,
    /// The day column width below which the grid is narrow, and the
    /// shortest hour at each density ([`TimelineDensity`]).
    narrow_below: f64,
    min_hour_regular: f64,
    min_hour_narrow: f64,
}

impl TimelineTokens {
    pub fn of(theme: &Theme, text: &mut TextRenderer) -> Self {
        let ty = TimelineType::of(theme);
        let line = |text: &mut TextRenderer, style: &TextStyle| {
            f64::from(text.measure_style("Ag", None, style).1)
        };
        let sp = &theme.tokens.spacing;
        let title_h = line(text, &ty.title);
        Self {
            weekday_h: line(text, &ty.weekday),
            day_h: line(text, &ty.day),
            strip_h: line(text, &ty.strip),
            title_h,
            hour_w: f64::from(text.measure_style("00:00", None, &ty.hour).0),
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s3: f64::from(sp.s3),
            narrow_below: TimelineDensity::narrow_below(theme),
            min_hour_regular: TimelineDensity::Regular.min_hour_height(theme, title_h),
            min_hour_narrow: TimelineDensity::Narrow.min_hour_height(theme, title_h),
        }
    }
}

impl Default for TimelineTokens {
    fn default() -> Self {
        let theme = Theme::dark();
        ui::text_render::with_layout_renderer(theme.text_scale, |text| Self::of(&theme, text))
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TimelineGeom {
    pub gutter: f64,
    /// Day header height (weekday label + day number).
    pub head_h: f64,
    /// All-day strip height beneath the header.
    pub allday_h: f64,
    pub body_top: f64,
    pub body_bottom: f64,
    pub hour_h: f64,
    pub content_h: f64,
    pub grid_right: f64,
    /// How much each day column shows (its width against the type).
    pub density: TimelineDensity,
    /// Smallest painted block; a one-line title fits at the current scale.
    pub min_block_h: f64,
    /// How far a block stands in from its column's edges.
    pub block_inset: f64,
}

impl TimelineGeom {
    pub fn body(self, x0: f64, x1: f64) -> Rect {
        Rect::new(x0, self.body_top, x1, self.body_bottom)
    }
}

pub(super) struct TimelineViewport {
    scroll: Scrollable,
    seeded: bool,
    pinned_hour: Option<f64>,
    /// How many day columns the last layout laid out.
    days_len: usize,
    /// The measured type and spacing of the theme of the app's last layout
    /// pass, so header / strip / block heights follow the type they hold.
    tokens: TimelineTokens,
}

impl TimelineViewport {
    pub fn new() -> Self {
        Self {
            scroll: Scrollable::new_vertical().always_visible(),
            seeded: false,
            pinned_hour: None,
            days_len: 7,
            tokens: TimelineTokens::default(),
        }
    }

    /// Measure the theme's type for the geometry solved in `layout`, so
    /// reserve and paint read one set of tokens.
    pub fn set_layout_theme(&mut self, theme: &Theme) {
        self.tokens = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
            TimelineTokens::of(theme, text)
        });
    }

    pub fn tokens(&self) -> TimelineTokens {
        self.tokens
    }

    /// The day columns of the last layout.
    pub fn days_len(&self) -> usize {
        self.days_len
    }

    /// Geometry of a week in the default dark theme, used by tests.
    #[cfg(test)]
    pub fn geometry(area: Rect) -> TimelineGeom {
        Self::geometry_for(area, &TimelineTokens::default(), 7)
    }

    /// Geometry from the measured `tokens`: the header holds the weekday and
    /// the day number with `s2` above and below, the all-day strip one
    /// caption line in its padding, the smallest block a title line, and the
    /// gutter an hour label with `s3` either side. `days_len` columns share
    /// the width, which decides the [`TimelineDensity`]: a narrow grid
    /// gives a half hour two title lines and the all-day strip two.
    pub fn geometry_for(area: Rect, t: &TimelineTokens, days_len: usize) -> TimelineGeom {
        let gutter = (t.hour_w + 2.0 * t.s3).ceil();
        let column = (area.width() - gutter - t.s3) / days_len.max(1) as f64;
        let density = if column < t.narrow_below {
            TimelineDensity::Narrow
        } else {
            TimelineDensity::Regular
        };
        let min_hour_h = match density {
            TimelineDensity::Regular => t.min_hour_regular,
            TimelineDensity::Narrow => t.min_hour_narrow,
        };
        let head_h = (2.0 * t.s2 + t.weekday_h + t.day_h).ceil();
        let allday_h = (t.strip_h * density.chip_lines() as f64 + 3.0 * t.s1).ceil();
        let min_block_h = (t.title_h + t.s1).ceil();
        let body_top = area.y0 + head_h + allday_h;
        let body_bottom = (area.y1 - t.s2).max(body_top + min_hour_h);
        let viewport_h = (body_bottom - body_top).max(min_hour_h);
        let hour_h = (viewport_h / 24.0).max(min_hour_h);
        let content_h = hour_h * 24.0;
        // The scrollbar lane is one `s3` wide when the day overflows.
        let grid_right = if content_h > viewport_h + 0.5 {
            area.x1 - t.s3
        } else {
            area.x1
        };
        TimelineGeom {
            gutter,
            head_h,
            allday_h,
            body_top,
            body_bottom,
            hour_h,
            content_h,
            grid_right,
            density,
            min_block_h,
            block_inset: t.s1 / 2.0,
        }
    }

    pub fn column_width(area: Rect, days_len: usize, geom: TimelineGeom) -> f64 {
        ((geom.grid_right - (area.x0 + geom.gutter)) / days_len.max(1) as f64).max(1.0)
    }

    pub fn column_at(area: Rect, days_len: usize, geom: TimelineGeom, x: f64) -> usize {
        // An empty range has no column to land on, and `clamp(0, -1)` is a
        // panic — `min` above `max` — not a saturating no-op. Callers read the
        // result through `.get()`, so 0 resolves to `None` for them.
        let Some(last) = days_len.checked_sub(1) else {
            return 0;
        };
        let grid_x0 = area.x0 + geom.gutter;
        let col_w = Self::column_width(area, days_len, geom);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "column_width floors at 1.0, so the quotient is bounded by the grid width"
        )]
        let col = ((x - grid_x0) / col_w).floor() as i64;
        usize::try_from(col.clamp(0, last as i64)).unwrap_or(0)
    }

    pub fn minute_at(&self, geom: TimelineGeom, y: f64) -> i64 {
        let content_y = (y - geom.body_top + f64::from(self.offset_y())).clamp(0.0, geom.content_h);
        (((content_y / geom.hour_h) * 60.0).round() as i64).clamp(0, 24 * 60)
    }

    pub fn block_rect(
        &self,
        area: Rect,
        days_len: usize,
        geom: TimelineGeom,
        col: usize,
        start_min: i64,
        end_min: i64,
    ) -> Rect {
        let col_w = Self::column_width(area, days_len, geom);
        let x0 = area.x0 + geom.gutter + col_w * col as f64;
        let offset = f64::from(self.offset_y());
        let top = geom.body_top + (start_min as f64 / 60.0) * geom.hour_h - offset;
        let bottom = geom.body_top + (end_min as f64 / 60.0) * geom.hour_h - offset;
        Rect::new(
            x0 + geom.block_inset,
            top,
            x0 + col_w - geom.block_inset,
            bottom.max(top + geom.min_block_h),
        )
    }

    pub fn layout(&mut self, area: Rect, initial_hour: f64, days_len: usize) {
        self.days_len = days_len.max(1);
        let geom = Self::geometry_for(area, &self.tokens, self.days_len);
        self.scroll.set_viewport_rect(geom.body(area.x0, area.x1));
        self.scroll
            .set_content_size(area.width() as f32, geom.content_h as f32);
        if let Some(hour) = self.pinned_hour {
            self.set_offset_y((hour * geom.hour_h) as f32);
        } else if !self.seeded && self.vertical_overflows() {
            // Seed on a whole hour: the hour label is painted just below its
            // rule, so a fractional seed ("now − 1 h" at 08:14 is 7.23) puts
            // the first rule above the body and clips that label under the
            // all-day strip. Flooring keeps the intended hour in view with
            // its label fully inside the body — and the hour is also capped at
            // the last whole hour the scroll range can reach, because the
            // `Scrollable` would otherwise clamp the offset to a fractional
            // maximum and clip the first label the same way.
            let viewport_h = geom.body_bottom - geom.body_top;
            let max_hours = ((geom.content_h - viewport_h).max(0.0) / geom.hour_h).floor();
            let hour = initial_hour.max(0.0).floor().min(max_hours);
            self.set_offset_y((hour * geom.hour_h) as f32);
            self.seeded = true;
        }
    }

    pub fn handle_event(&mut self, input: &mut InputContext<'_>) -> bool {
        self.scroll
            .handle_event(input.event, &mut input.event_ctx());
        *input.handled
    }

    pub fn draw(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        self.scroll.draw_bar_overlay(scene, transform, theme);
    }

    pub fn wants_pointer_capture(&self) -> bool {
        self.scroll.wants_pointer_capture()
    }
    pub fn offset_y(&self) -> f32 {
        self.scroll.offset_y()
    }
    pub fn set_offset_y(&mut self, offset: f32) {
        self.scroll.set_offset_y(offset);
    }
    pub fn vertical_overflows(&self) -> bool {
        self.scroll.vertical_overflows()
    }
    pub fn needs_redraw(&self) -> bool {
        self.scroll.needs_redraw()
    }
    pub fn mark_drawn(&mut self) {
        self.scroll.mark_drawn();
    }
    pub fn needs_animation_frame(&self) -> bool {
        self.scroll.needs_animation_frame()
    }
    pub fn is_layout_dirty(&self) -> bool {
        self.scroll.is_layout_dirty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_keeps_legible_hours_maps_columns_and_grows_with_the_text() {
        // Case `geometry_keeps_legible_hours_and_maps_columns`.
        {
            let area = Rect::new(0.0, 60.0, 1000.0, 560.0);
            let geom = TimelineViewport::geometry(area);
            assert_eq!(geom.hour_h, TIMELINE_MIN_HOUR_H);
            assert_eq!(geom.content_h, 24.0 * TIMELINE_MIN_HOUR_H);
            let width = TimelineViewport::column_width(area, 7, geom);
            assert_eq!(
                TimelineViewport::column_at(area, 7, geom, area.x0 + geom.gutter + width * 3.5),
                3
            );
        }
        // Case `scaled_text_grows_the_header_strip_blocks_and_gutter`.
        {
            let area = Rect::new(0.0, 60.0, 1000.0, 560.0);
            let large = Theme::dark().with_text_scale(2.0);
            let tokens = ui::text_render::with_layout_renderer(large.text_scale, |text| {
                TimelineTokens::of(&large, text)
            });
            let one = TimelineViewport::geometry(area);
            let two = TimelineViewport::geometry_for(area, &tokens, 7);
            for value in [two.head_h, two.allday_h, two.min_block_h, two.gutter] {
                assert_eq!(value.fract(), 0.0, "scaled rows land on whole pixels");
            }
            assert!(two.gutter > one.gutter);
            assert!(two.head_h > one.head_h && two.allday_h > one.allday_h);
            assert!(two.min_block_h > one.min_block_h);
        }
    }

    #[test]
    fn scroll_seeds_on_whole_hours_and_drives_minute_and_block_geometry() {
        // Case `seeded_scroll_lands_on_a_whole_hour`:
        // A fractional focus hour ("now − 1 h") seeds on the whole hour below
        // it, so the first visible hour rule sits exactly on the body top and
        // its label — painted just under the rule — is never clipped by the
        // all-day strip.
        {
            let area = Rect::new(0.0, 60.0, 1000.0, 560.0);
            let geom = TimelineViewport::geometry(area);
            let mut viewport = TimelineViewport::new();
            viewport.layout(area, 7.23, 7);
            assert!(viewport.vertical_overflows());
            assert!((f64::from(viewport.offset_y()) - 7.0 * geom.hour_h).abs() < 0.01);
            // The 07:00 rule is at the body top; the label below it is inside.
            let rule_y = geom.body_top + 7.0 * geom.hour_h - f64::from(viewport.offset_y());
            assert!((rule_y - geom.body_top).abs() < 0.01);
            assert_eq!(viewport.minute_at(geom, geom.body_top), 7 * 60);
        }
        // Case `seeded_scroll_caps_at_the_last_whole_hour_the_range_allows`:
        // When the seeded hour lies past what the scroll range can reach (a tall
        // 2× header in a short window), the seed snaps to the last whole hour
        // that fits instead of letting the `Scrollable` clamp the offset to a
        // fractional maximum — which would clip the first label just the same.
        {
            // 24 × 44 = 1056 of content in a 600-tall body: at most 456 / 44 =
            // 10.36 hours of scroll, so hour 9 is the last whole one that fits.
            let probe = TimelineViewport::geometry(Rect::new(0.0, 0.0, 1000.0, 1000.0));
            let area = Rect::new(
                0.0,
                0.0,
                1000.0,
                600.0 + probe.head_h + probe.allday_h + 8.0,
            );
            let geom = TimelineViewport::geometry(area);
            let mut viewport = TimelineViewport::new();
            viewport.layout(area, 22.0, 7);
            let offset = f64::from(viewport.offset_y());
            assert!(
                (offset / geom.hour_h).fract().abs() < 1e-6,
                "seed is a whole hour"
            );
            assert!(offset + (geom.body_bottom - geom.body_top) <= geom.content_h + 1e-6);
            let rule_y = geom.body_top + (offset / geom.hour_h).round() * geom.hour_h - offset;
            assert!(
                (rule_y - geom.body_top).abs() < 1e-6,
                "an hour rule sits on the body top"
            );
        }
        // Case `scroll_offset_drives_minute_and_block_hit_geometry`.
        {
            let area = Rect::new(0.0, 60.0, 1000.0, 560.0);
            let geom = TimelineViewport::geometry(area);
            let mut viewport = TimelineViewport::new();
            viewport.layout(area, 7.0, 7);
            viewport.set_offset_y((geom.hour_h * 2.0) as f32);
            assert_eq!(viewport.minute_at(geom, geom.body_top), 120);
            let block = viewport.block_rect(area, 7, geom, 2, 120, 180);
            assert!((block.y0 - geom.body_top).abs() < 0.01);
            assert_eq!(
                TimelineViewport::column_at(area, 7, geom, block.center().x),
                2
            );
        }
    }
}
