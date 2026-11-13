//! Where the active tab's slots stand in the bar: in the window's title's row for one tab, past the
//! strip's own controls, across a row of their own under several, and between the two on the tabs'
//! motion — and how much of that row the strip's own "+" takes, which yields where the slots need
//! the room.
use super::{ICON_W, PILL_PAD_X, Rect, TOOLBAR_LEAD_GAP, TOOLBAR_ROW_HEIGHT, TabBar};
use crate::widget::window_controls;

impl TabBar {
    /// Where the active tab's slots stand: in the title's row for one tab, past the strip's own
    /// leading controls (so the title never paints under the "+"), across a row of their own under
    /// several, and between the two on the tabs' motion.
    pub(super) fn toolbar_row_rect(&self) -> Rect {
        let tab_row = self.tab_row();
        if let Some(menu) = self.menu_button_row(tab_row) {
            return menu;
        }
        let solo = {
            let x0 = self.tabs_end() + TOOLBAR_LEAD_GAP;
            let x1 = (self.trailing_limit(tab_row) - self.search_control.unwrap_or(0.0)).max(x0);
            Rect::new(x0, tab_row.y0, x1, tab_row.y1)
        };
        let lead =
            self.pill_row().x0 + PILL_PAD_X + ICON_W / 2.0 - window_controls::CONTROL_PX / 2.0;
        let below = Rect::new(
            lead,
            tab_row.y1,
            (self.rect.x1 - window_controls::EDGE_INSET - self.search_control.unwrap_or(0.0))
                .max(lead),
            tab_row.y1 + TOOLBAR_ROW_HEIGHT,
        );
        let p = self.tabbed_progress();
        if p <= 0.0 {
            solo
        } else if p >= 1.0 {
            below
        } else {
            Rect::new(
                solo.x0 + (below.x0 - solo.x0) * p,
                solo.y0 + (below.y0 - solo.y0) * p,
                solo.x1 + (below.x1 - solo.x1) * p,
                solo.y1 + (below.y1 - solo.y1) * p,
            )
        }
    }

    pub(super) fn lay_out_toolbar_row(&mut self) {
        self.touch();
        self.show_toolbar_row();
        self.toolbar_row.lay_out(self.toolbar_row_rect());
        // The strip's own "+" stands between the window's icon and the app's slots, which start
        // past it so neither paints over the other. A narrow window has no room for both: the "+"
        // gives up its place, so the app's Back, its name and its actions keep their own, and the
        // window's search keeps the room it reaches back over.
        if self.strip_squeezes_slots() {
            self.new_tab_rect = Rect::ZERO;
            self.toolbar_row.lay_out(self.toolbar_row_rect());
        }
        let strip = self.toolbar_row.strip_height();
        let top = self.rect.y0 + self.rows_height();
        self.toolbar_row.place_strip(if strip > 0.0 {
            Rect::new(
                self.rect.x0 + window_controls::EDGE_INSET,
                top,
                (self.rect.x1 - window_controls::EDGE_INSET).max(self.rect.x0),
                top + strip,
            )
        } else {
            Rect::ZERO
        });
    }

    /// Whether the strip's "+" is what leaves the active tab's slots short of the room they need,
    /// while the slots stand in the title's row beside it: their buttons' places and the app's
    /// location, or the room the window's search reaches back over to keep its query typeable.
    pub(super) fn strip_squeezes_slots(&self) -> bool {
        if self.new_tab_rect.area() <= 0.0
            || self.active_slots().is_none()
            || self.tabbed_progress() > 0.0
        {
            return false;
        }
        self.toolbar_row.crowded() || self.search_would_be_cramped()
    }

    fn search_would_be_cramped(&self) -> bool {
        let Some(least) = self.search_least else {
            return false;
        };
        if self.search_control.is_none() {
            return false;
        }
        let row = self.toolbar_row_rect();
        let span =
            |x0: f64| row.x1 + window_controls::CONTROL_GAP + window_controls::CONTROL_PX - x0;
        span(row.x0) < least && span(self.strip_lead_end() + TOOLBAR_LEAD_GAP) >= least
    }

    fn strip_lead_end(&self) -> f64 {
        self.tab_rects
            .first()
            .copied()
            .filter(|rect| rect.area() > 0.0)
            .map_or(self.pill_row().x0, |rect| rect.x1)
    }
}
