//! Stateful viewport for Calendar's week/day timeline.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::Widget;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Scrollable, TimelineDensity};

use crate::input_context::InputContext;

#[cfg(test)]
pub(super) const TIMELINE_MIN_HOUR_H: f64 = ui::tokens::Metrics::BASE.panel_row_height as f64;

#[derive(Clone, Debug)]
pub(super) struct TimelineType {
    pub weekday: TextStyle,
    pub day: TextStyle,
    pub strip: TextStyle,
    pub title: TextStyle,
    pub subtitle: TextStyle,
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
            hour_w: [0, 10, 12, 22]
                .into_iter()
                .map(|hour| f64::from(text.measure_style(&hour_label(hour), None, &ty.hour).0))
                .fold(0.0, f64::max),
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
        ui::text_render::with_layout_renderer(&theme, |text| Self::of(&theme, text))
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TimelineGeom {
    pub gutter: f64,
    pub head_h: f64,
    pub allday_h: f64,
    pub allday_rows: usize,
    pub body_top: f64,
    pub body_bottom: f64,
    pub hour_h: f64,
    pub content_h: f64,
    pub grid_right: f64,
    pub density: TimelineDensity,
    pub min_block_h: f64,
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
    days_len: usize,
    allday_rows: usize,
    tokens: TimelineTokens,
    laid_out_hour_h: f64,
}

impl TimelineViewport {
    pub fn new() -> Self {
        Self {
            scroll: Scrollable::new_vertical().always_visible(),
            seeded: false,
            pinned_hour: None,
            days_len: 7,
            allday_rows: 1,
            tokens: TimelineTokens::default(),
            laid_out_hour_h: 0.0,
        }
    }

    pub fn set_layout_theme(&mut self, theme: &Theme) {
        self.tokens =
            ui::text_render::with_layout_renderer(theme, |text| TimelineTokens::of(theme, text));
    }

    pub fn current_geometry(&self, area: Rect) -> TimelineGeom {
        Self::geometry_rows(area, &self.tokens, self.days_len, self.allday_rows)
    }

    #[cfg(test)]
    pub fn geometry(area: Rect) -> TimelineGeom {
        Self::geometry_for(area, &TimelineTokens::default(), 7)
    }

    #[cfg(test)]
    pub fn geometry_for(area: Rect, t: &TimelineTokens, days_len: usize) -> TimelineGeom {
        Self::geometry_rows(area, t, days_len, 1)
    }

    pub fn geometry_rows(
        area: Rect,
        t: &TimelineTokens,
        days_len: usize,
        rows: usize,
    ) -> TimelineGeom {
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
        let allday_h = match rows {
            0 => t.s1,
            1 => t.strip_h * density.chip_lines() as f64 + 3.0 * t.s1,
            rows => {
                let rows = rows as f64;
                rows * (t.strip_h + t.s1) + 2.0 * t.s1
            }
        }
        .ceil();
        let min_block_h = (t.title_h + t.s1).ceil();
        let body_top = area.y0 + head_h + allday_h;
        let body_bottom = (area.y1 - t.s2).max(body_top + min_hour_h);
        let viewport_h = (body_bottom - body_top).max(min_hour_h);
        let hour_h = (viewport_h / 24.0).max(min_hour_h);
        let content_h = hour_h * 24.0;
        let grid_right = if content_h > viewport_h + 0.5 {
            area.x1 - t.s3
        } else {
            area.x1
        };
        TimelineGeom {
            gutter,
            head_h,
            allday_h,
            allday_rows: rows,
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

    pub fn layout(
        &mut self,
        area: Rect,
        initial_hour: f64,
        (days_len, allday_rows): (usize, usize),
    ) {
        self.days_len = days_len.max(1);
        self.allday_rows = allday_rows;
        let geom = self.current_geometry(area);
        let shown_hour =
            (self.laid_out_hour_h > 0.0).then(|| f64::from(self.offset_y()) / self.laid_out_hour_h);
        self.scroll.set_viewport_rect(geom.body(area.x0, area.x1));
        self.scroll
            .set_content_size(area.width() as f32, geom.content_h as f32);
        if let Some(hour) = shown_hour {
            self.set_offset_y((hour * geom.hour_h) as f32);
        }
        self.laid_out_hour_h = geom.hour_h;
        if let Some(hour) = self.pinned_hour {
            self.set_offset_y((hour * geom.hour_h) as f32);
        } else if !self.seeded && self.vertical_overflows() {
            // Seed on a whole hour: the hour label is painted just below its rule, so a fractional seed clips it
            // under the all-day strip. Capped at the last whole hour the scroll range reaches.
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

pub(crate) fn hour_label(hour: u32) -> String {
    chrono::NaiveTime::from_hms_opt(hour, 0, 0)
        .map(|time| ui::format::time(&time))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_scale_and_density_keeps_the_hour_in_view() {
        let area = Rect::new(0.0, 60.0, 1020.0, 810.0);
        let mut viewport = TimelineViewport::new();
        viewport.layout(area, 8.0, (7, 1));
        for (scale, days) in [(1.5, 7), (2.0, 1), (1.0, 7)] {
            viewport.set_layout_theme(&Theme::dark().with_ui_scale(scale));
            viewport.layout(area, 8.0, (days, 1));
            let geom = viewport.current_geometry(area);
            assert_eq!(
                viewport.minute_at(geom, geom.body_top),
                8 * 60,
                "{scale} {days}"
            );
        }
    }

    #[test]
    fn geometry_keeps_legible_hours_maps_columns_and_grows_with_the_text() {
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
        {
            let area = Rect::new(0.0, 60.0, 1000.0, 560.0);
            let large = Theme::dark().with_text_scale(2.0);
            let tokens = ui::text_render::with_layout_renderer(&large, |text| {
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
        {
            let area = Rect::new(0.0, 60.0, 1000.0, 560.0);
            let geom = TimelineViewport::geometry(area);
            let mut viewport = TimelineViewport::new();
            viewport.layout(area, 7.23, (7, 1));
            assert!(viewport.vertical_overflows());
            assert!((f64::from(viewport.offset_y()) - 7.0 * geom.hour_h).abs() < 0.01);
            let rule_y = geom.body_top + 7.0 * geom.hour_h - f64::from(viewport.offset_y());
            assert!((rule_y - geom.body_top).abs() < 0.01);
            assert_eq!(viewport.minute_at(geom, geom.body_top), 7 * 60);
        }
        {
            let probe = TimelineViewport::geometry(Rect::new(0.0, 0.0, 1000.0, 1000.0));
            let area = Rect::new(
                0.0,
                0.0,
                1000.0,
                600.0 + probe.head_h + probe.allday_h + 8.0,
            );
            let geom = TimelineViewport::geometry(area);
            let mut viewport = TimelineViewport::new();
            viewport.layout(area, 22.0, (7, 1));
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
        {
            let area = Rect::new(0.0, 60.0, 1000.0, 560.0);
            let geom = TimelineViewport::geometry(area);
            let mut viewport = TimelineViewport::new();
            viewport.layout(area, 7.0, (7, 1));
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
