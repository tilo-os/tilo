//! Stateful viewport for Calendar's week/day timeline.

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{Modifiers, WindowEvent};

use ui::Widget;
use ui::theme::Theme;
use ui::widget::Scrollable;

pub(super) const TIMELINE_MIN_HOUR_H: f64 = 44.0;
/// Authored 1× heights of the day header and the all-day strip; both grow
/// with the measured text lines they hold (see [`TimelineViewport::geometry_for`]).
pub(super) const TIMELINE_HEAD_H: f64 = 56.0;
pub(super) const TIMELINE_ALLDAY_H: f64 = 26.0;
/// Authored 1× minimum height of an event block (a 15-minute event).
const TIMELINE_MIN_BLOCK_H: f64 = 16.0;
const TIMELINE_BAR_LANE: f64 = 12.0;
/// Base text sizes painted into the header, the all-day strip and blocks.
pub(super) const TIMELINE_WEEKDAY_TEXT: f32 = 11.0;
pub(super) const TIMELINE_DAY_TEXT: f32 = 20.0;
pub(super) const TIMELINE_STRIP_TEXT: f32 = 10.0;
pub(super) const TIMELINE_TITLE_TEXT: f32 = 11.0;
pub(super) const TIMELINE_SUBTITLE_TEXT: f32 = 10.0;
/// Inner padding of the header and blocks (`s2` / `s1` of the spacing scale;
/// literal because this geometry is solved without a theme in hand).
pub(super) const TIMELINE_HEAD_PAD: f64 = 8.0;
pub(super) const TIMELINE_BLOCK_PAD: f64 = 4.0;

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
    /// The theme's text scale, so header / strip / block heights follow the
    /// type they hold. Set from the app's layout pass.
    text_scale: f32,
}

impl TimelineViewport {
    pub fn new() -> Self {
        Self {
            scroll: Scrollable::new_vertical().always_visible(),
            seeded: false,
            pinned_hour: None,
            text_scale: 1.0,
        }
    }

    pub fn set_text_scale(&mut self, text_scale: f32) {
        self.text_scale = text_scale;
    }

    pub fn text_scale(&self) -> f32 {
        self.text_scale
    }

    /// Geometry at 1× — the authored design, used by tests and by callers
    /// that have no scale of their own.
    pub fn geometry(area: Rect) -> TimelineGeom {
        Self::geometry_for(area, 1.0)
    }

    /// Geometry for `text_scale`: header, all-day strip and minimum block
    /// heights are the authored 1× values or the measured lines plus their
    /// padding, whichever is taller, so scaled type never overlaps.
    pub fn geometry_for(area: Rect, text_scale: f32) -> TimelineGeom {
        let weekday_h = crate::metrics::line_h(TIMELINE_WEEKDAY_TEXT, text_scale);
        let day_h = crate::metrics::line_h(TIMELINE_DAY_TEXT, text_scale);
        let strip_h = crate::metrics::line_h(TIMELINE_STRIP_TEXT, text_scale);
        let title_h = crate::metrics::line_h(TIMELINE_TITLE_TEXT, text_scale);
        let head_h = crate::metrics::at_least(
            TIMELINE_HEAD_H,
            2.0 * TIMELINE_HEAD_PAD + weekday_h + day_h,
        );
        // The strip is inset 2px inside its band; the label needs its line
        // plus a block pad above and below inside that.
        let allday_h = crate::metrics::at_least(
            TIMELINE_ALLDAY_H,
            strip_h + 2.0 * TIMELINE_BLOCK_PAD + 4.0,
        );
        let min_block_h =
            crate::metrics::at_least(TIMELINE_MIN_BLOCK_H, title_h + TIMELINE_BLOCK_PAD);
        let body_top = area.y0 + head_h + allday_h;
        let body_bottom = (area.y1 - 8.0).max(body_top + 40.0);
        let viewport_h = (body_bottom - body_top).max(40.0);
        let hour_h = (viewport_h / 24.0).max(TIMELINE_MIN_HOUR_H);
        let content_h = hour_h * 24.0;
        let grid_right = if content_h > viewport_h + 0.5 {
            area.x1 - TIMELINE_BAR_LANE
        } else {
            area.x1
        };
        TimelineGeom {
            gutter: 56.0,
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
        let geom = Self::geometry_for(area, self.text_scale);
        self.scroll.set_viewport_rect(geom.body(area.x0, area.x1));
        self.scroll
            .set_content_size(area.width() as f32, geom.content_h as f32);
        if let Some(hour) = self.pinned_hour {
            self.set_offset_y((hour * geom.hour_h) as f32);
        } else if !self.seeded && self.vertical_overflows() {
            self.set_offset_y((initial_hour * geom.hour_h) as f32);
            self.seeded = true;
        }
    }

    pub fn pin_hour(&mut self, hour: f64) {
        self.pinned_hour = Some(hour);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut handled = false;
        self.scroll.handle_event(
            event,
            scale_factor,
            &modifiers,
            state,
            path,
            &mut handled,
            focus_target,
            node_id,
            taffy,
        );
        handled
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
    fn geometry_keeps_legible_hours_and_maps_columns() {
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

    #[test]
    fn scroll_offset_drives_minute_and_block_hit_geometry() {
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
