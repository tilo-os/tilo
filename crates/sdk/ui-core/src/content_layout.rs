//! Responsive content-width policies for app surfaces.
//!
//! Full-bleed canvases and dense data grids legitimately use the whole window.
//! Focused CRUD, settings, and collection surfaces usually should not: without
//! a shared lane, search fields and row content stretch across large monitors.

use vello::kurbo::Rect;

pub const FOCUSED_CONTENT_MAX_WIDTH: f64 = 960.0;
pub const FOCUSED_CONTENT_GUTTER: f64 = 12.0;
pub const CONTROL_BAR_SINGLE_ROW_HEIGHT: f64 = 42.0;
pub const CONTROL_BAR_WRAPPED_HEIGHT: f64 = 76.0;
pub const CONTROL_BAR_STACKED_HEIGHT: f64 = 110.0;

/// Shared one-row/two-row policy for dense command and find bars. Callers
/// declare the width their inline composition genuinely needs; the same value
/// then drives reserved layout height and paint-time row placement, preventing
/// controls from being clipped by a one-off app breakpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResponsiveControlBar {
    inline_min_width: f64,
    wrapped_min_width: f64,
}

impl ResponsiveControlBar {
    pub fn new(inline_min_width: f64) -> Self {
        Self {
            inline_min_width: inline_min_width.max(0.0),
            wrapped_min_width: 0.0,
        }
    }

    /// Width below which the two-row composition is no longer viable and a
    /// third row must be reserved. Leave unset when two rows can shrink
    /// fluidly all the way down.
    #[must_use]
    pub fn wrapped_min_width(mut self, width: f64) -> Self {
        self.wrapped_min_width = width.max(0.0).min(self.inline_min_width);
        self
    }

    pub fn row_count(self, available_width: f64) -> usize {
        let width = available_width.max(0.0);
        if width >= self.inline_min_width {
            1
        } else if width >= self.wrapped_min_width {
            2
        } else {
            3
        }
    }

    pub fn is_wrapped(self, available_width: f64) -> bool {
        self.row_count(available_width) > 1
    }

    pub fn height(self, available_width: f64) -> f64 {
        match self.row_count(available_width) {
            1 => CONTROL_BAR_SINGLE_ROW_HEIGHT,
            2 => CONTROL_BAR_WRAPPED_HEIGHT,
            _ => CONTROL_BAR_STACKED_HEIGHT,
        }
    }

    /// Vertical centre of row `0` or `1`. A single-row bar always resolves to
    /// its centre; wrapped rows keep equal 34px lanes inside 4px outer insets.
    pub fn row_center(self, bounds: Rect, row: usize) -> f64 {
        if !self.is_wrapped(bounds.width()) {
            return bounds.center().y;
        }
        let row_offset = match row {
            0 => 0.0,
            1 => 34.0,
            _ => 68.0,
        };
        bounds.y0 + 4.0 + 17.0 + row_offset
    }
}

/// A centered, horizontally constrained lane that remains fluid on compact
/// viewports. Vertical bounds always match the supplied viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusedContentLane {
    max_width: f64,
    gutter: f64,
}

impl Default for FocusedContentLane {
    fn default() -> Self {
        Self {
            max_width: FOCUSED_CONTENT_MAX_WIDTH,
            gutter: FOCUSED_CONTENT_GUTTER,
        }
    }
}

impl FocusedContentLane {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn max_width(mut self, max_width: f64) -> Self {
        self.max_width = max_width.max(0.0);
        self
    }

    #[must_use]
    pub fn gutter(mut self, gutter: f64) -> Self {
        self.gutter = gutter.max(0.0);
        self
    }

    #[must_use]
    pub fn layout(self, viewport: Rect) -> Rect {
        let available = (viewport.width() - self.gutter * 2.0).max(0.0);
        let width = available.min(self.max_width);
        let x0 = viewport.center().x - width / 2.0;
        Rect::new(x0, viewport.y0, x0 + width, viewport.y1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_lanes_and_control_bars_reflow_and_collapse_safely() {
        // Case `focused_lane_is_centered_wide_and_fluid_compact`.
        {
            let lane = FocusedContentLane::new();
            assert_eq!(
                lane.layout(Rect::new(0.0, 0.0, 1280.0, 800.0)),
                Rect::new(160.0, 0.0, 1120.0, 800.0)
            );
            assert_eq!(
                lane.layout(Rect::new(0.0, 0.0, 320.0, 640.0)),
                Rect::new(12.0, 0.0, 308.0, 640.0)
            );
        }

        // Case `invalid_custom_metrics_collapse_safely`.
        {
            let lane = FocusedContentLane::new().max_width(-1.0).gutter(-1.0);
            assert_eq!(lane.layout(Rect::new(0.0, 0.0, 400.0, 600.0)).width(), 0.0);
        }

        // Case `responsive_control_bar_reflows_and_uses_the_same_reserved_height`.
        {
            let policy = ResponsiveControlBar::new(880.0).wrapped_min_width(440.0);
            assert_eq!(policy.height(1000.0), CONTROL_BAR_SINGLE_ROW_HEIGHT);
            assert_eq!(policy.height(640.0), CONTROL_BAR_WRAPPED_HEIGHT);
            assert_eq!(policy.height(360.0), CONTROL_BAR_STACKED_HEIGHT);
            assert_eq!(policy.row_count(360.0), 3);
            let narrow = Rect::new(0.0, 10.0, 640.0, 86.0);
            assert_eq!(policy.row_center(narrow, 0), 31.0);
            assert_eq!(policy.row_center(narrow, 1), 65.0);
            let stacked = Rect::new(0.0, 10.0, 360.0, 120.0);
            assert_eq!(policy.row_center(stacked, 2), 99.0);
            assert_eq!(
                policy.row_center(Rect::new(0.0, 10.0, 1000.0, 52.0), 1),
                31.0
            );
        }
    }
}
