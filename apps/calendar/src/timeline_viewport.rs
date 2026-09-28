//! Stateful viewport for Calendar's week/day timeline.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::Widget;
use ui::theme::Theme;
use ui::tokens::spacing::Spacing;
use ui::widget::Scrollable;

use crate::input_context::InputContext;

pub(super) const TIMELINE_MIN_HOUR_H: f64 = 44.0;
/// Authored 1× width of the hour gutter, and the base size and 1× width of
/// the "HH:00" label it carries. The gutter grows by the label's extra width
/// at larger scales so a 2× "08:00" never runs into the first day column.
const TIMELINE_GUTTER: f64 = 56.0;
pub(super) const TIMELINE_HOUR_TEXT: f32 = 10.0;
const TIMELINE_HOUR_LABEL_W: f64 = 28.0;
/// Authored 1× heights of the day header and the all-day strip; both grow
/// with the measured text lines they hold (see [`TimelineViewport::geometry_for`]).
pub(super) const TIMELINE_HEAD_H: f64 = 56.0;
pub(super) const TIMELINE_ALLDAY_H: f64 = 26.0;
/// Authored 1× minimum height of an event block (a 15-minute event).
const TIMELINE_MIN_BLOCK_H: f64 = 16.0;
/// Base text sizes painted into the header, the all-day strip and blocks.
pub(super) const TIMELINE_WEEKDAY_TEXT: f32 = 11.0;
pub(super) const TIMELINE_DAY_TEXT: f32 = 20.0;
pub(super) const TIMELINE_STRIP_TEXT: f32 = 10.0;
pub(super) const TIMELINE_TITLE_TEXT: f32 = 11.0;
pub(super) const TIMELINE_SUBTITLE_TEXT: f32 = 10.0;

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
    /// Smallest painted block; a one-line title fits at the current scale.
    pub min_block_h: f64,
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
    /// The theme's text scale and spacing, cached from the app's layout pass
    /// so header / strip / block heights follow the type they hold and the
    /// same token pads the painter leaves.
    text_scale: f32,
    spacing: Spacing,
}

impl TimelineViewport {
    pub fn new() -> Self {
        Self {
            scroll: Scrollable::new_vertical().always_visible(),
            seeded: false,
            pinned_hour: None,
            text_scale: 1.0,
            spacing: Spacing::default(),
        }
    }

    /// Cache the theme's text scale and spacing for the geometry solved in
    /// `layout`, so reserve and paint read one set of tokens.
    pub fn set_layout_tokens(&mut self, text_scale: f32, spacing: &Spacing) {
        self.text_scale = text_scale;
        self.spacing = *spacing;
    }

    pub fn text_scale(&self) -> f32 {
        self.text_scale
    }

    pub fn spacing(&self) -> &Spacing {
        &self.spacing
    }

    /// Geometry at 1× with the default spacing — the authored design, used
    /// by tests and by callers that have no scale of their own.
    #[cfg(test)]
    pub fn geometry(area: Rect) -> TimelineGeom {
        Self::geometry_for(area, 1.0, &Spacing::default())
    }

    /// Geometry for `text_scale` and the theme's `spacing`: header, all-day
    /// strip and minimum block heights are the authored 1× values, or the
    /// measured lines plus the same token pads the painter leaves (`s2`
    /// around the header rows, `s1` inside a block) rounded up to a whole
    /// pixel, whichever is taller — so at 1× the design is reproduced and at
    /// 2× scaled type never overlaps.
    pub fn geometry_for(area: Rect, text_scale: f32, spacing: &Spacing) -> TimelineGeom {
        let s1 = f64::from(spacing.s1);
        let s2 = f64::from(spacing.s2);
        let weekday_h = crate::metrics::line_h(TIMELINE_WEEKDAY_TEXT, text_scale);
        let day_h = crate::metrics::line_h(TIMELINE_DAY_TEXT, text_scale);
        let strip_h = crate::metrics::line_h(TIMELINE_STRIP_TEXT, text_scale);
        let title_h = crate::metrics::line_h(TIMELINE_TITLE_TEXT, text_scale);
        let head_h = crate::metrics::at_least_whole(TIMELINE_HEAD_H, 2.0 * s2 + weekday_h + day_h);
        // The strip is inset `s1` inside its band; the label needs its line
        // plus a block pad above and below inside that.
        let allday_h = crate::metrics::at_least_whole(TIMELINE_ALLDAY_H, strip_h + 3.0 * s1);
        // A block always holds its one-line title plus the pad the painter
        // puts above it, so descenders are never clipped at any scale.
        let min_block_h = crate::metrics::at_least_whole(TIMELINE_MIN_BLOCK_H, title_h + s1);
        let body_top = area.y0 + head_h + allday_h;
        let body_bottom = (area.y1 - s2).max(body_top + 40.0);
        let viewport_h = (body_bottom - body_top).max(40.0);
        let hour_h = (viewport_h / 24.0).max(TIMELINE_MIN_HOUR_H);
        let content_h = hour_h * 24.0;
        // The scrollbar lane is one `s3` wide when the day overflows.
        let grid_right = if content_h > viewport_h + 0.5 {
            area.x1 - f64::from(spacing.s3)
        } else {
            area.x1
        };
        let grow = if text_scale.is_finite() {
            f64::from(text_scale.clamp(1.0, 4.0))
        } else {
            1.0
        };
        TimelineGeom {
            gutter: (TIMELINE_GUTTER + TIMELINE_HOUR_LABEL_W * (grow - 1.0)).ceil(),
            head_h,
            allday_h,
            body_top,
            body_bottom,
            hour_h,
            content_h,
            grid_right,
            min_block_h,
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
            x0 + 3.0,
            top,
            x0 + col_w - 3.0,
            bottom.max(top + geom.min_block_h),
        )
    }

    pub fn layout(&mut self, area: Rect, initial_hour: f64) {
        let geom = Self::geometry_for(area, self.text_scale, &self.spacing);
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

    pub fn pin_hour(&mut self, hour: f64) {
        self.pinned_hour = Some(hour);
    }

    pub fn handle_event(&mut self, input: &mut InputContext<'_>) -> bool {
        self.scroll.handle_event(
            input.event,
            &mut ui::widget::EventCtx {
                scale_factor: input.scale_factor,
                modifiers: &input.modifiers,
                state: input.state,
                path: input.path,
                handled: input.handled,
                focus_target: input.focus_target,
                node_id: input.node_id,
                taffy: input.taffy,
            },
        );
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
    fn geometry_keeps_legible_hours_maps_columns_and_grows_whole_when_scaled() {
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
        // Case `geometry_reproduces_the_authored_design_at_one_x_and_grows_whole`:
        // The 1× geometry is the authored design on whole pixels; the 2× one
        // holds the scaled lines and stays on whole pixels too.
        {
            let area = Rect::new(0.0, 60.0, 1000.0, 560.0);
            let sp = Spacing::default();
            let one = TimelineViewport::geometry_for(area, 1.0, &sp);
            assert_eq!(one.head_h, TIMELINE_HEAD_H);
            assert_eq!(one.allday_h, TIMELINE_ALLDAY_H);
            // The smallest block always holds its title line plus the pad the
            // painter puts above it (so descenders are never clipped), on a whole
            // pixel, and never shrinks under the authored minimum.
            let title_1x = crate::metrics::line_h(TIMELINE_TITLE_TEXT, 1.0);
            assert!(one.min_block_h >= title_1x + f64::from(sp.s1));
            assert!(one.min_block_h >= TIMELINE_MIN_BLOCK_H);
            assert_eq!(one.min_block_h.fract(), 0.0);
            assert_eq!(one.body_top, area.y0 + TIMELINE_HEAD_H + TIMELINE_ALLDAY_H);
            assert_eq!(one.gutter, TIMELINE_GUTTER);

            let two = TimelineViewport::geometry_for(area, 2.0, &sp);
            // The gutter widens by the hour label's extra width, so "08:00" at 2×
            // stays inside it instead of running into the first column.
            let mut tr = ui::text_render::TextRenderer::new();
            tr.set_text_scale(2.0);
            let (label_w, _) = tr.measure("00:00", TIMELINE_HOUR_TEXT, None);
            assert!(
                two.gutter >= f64::from(sp.s3) + f64::from(label_w),
                "gutter {} must hold the measured 2× hour label ({label_w}) after its inset",
                two.gutter
            );
            assert!(two.gutter > one.gutter);
            for value in [two.head_h, two.allday_h, two.min_block_h, two.gutter] {
                assert_eq!(value.fract(), 0.0, "scaled rows land on whole pixels");
            }
            // Header: both scaled lines plus the `s2` pad above and below them;
            // block: the scaled title plus its `s1` pad — the same tokens the
            // painter lays the text out with.
            assert!(
                two.head_h
                    >= 2.0 * f64::from(sp.s2)
                        + crate::metrics::line_h(TIMELINE_WEEKDAY_TEXT, 2.0)
                        + crate::metrics::line_h(TIMELINE_DAY_TEXT, 2.0)
            );
            assert!(
                two.min_block_h
                    >= crate::metrics::line_h(TIMELINE_TITLE_TEXT, 2.0) + f64::from(sp.s1)
            );
            assert!(two.allday_h > one.allday_h && two.head_h > one.head_h);
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
            viewport.layout(area, 7.23);
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
            let area = Rect::new(
                0.0,
                0.0,
                1000.0,
                600.0 + TIMELINE_HEAD_H + TIMELINE_ALLDAY_H + 8.0,
            );
            let geom = TimelineViewport::geometry(area);
            let mut viewport = TimelineViewport::new();
            viewport.layout(area, 22.0);
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
            viewport.layout(area, 7.0);
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
