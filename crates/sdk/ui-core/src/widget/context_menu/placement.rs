//! Where a menu's panel stands: below the item it opened from, or above it where below has less
//! room, kept inside the window and on the display's pixels the frame draws on.

use super::{
    ANCHOR_GAP, ContextMenu, PADDING, PanelAlign, PopoverSide, ROW_HEIGHT, Rect, floating_surface,
};

impl ContextMenu {
    /// Stand the rows in the panel as it is now: a box that no longer holds
    /// them scrolls them back inside it.
    pub(super) fn place_scroll(&mut self) {
        if self.anchor.is_none() {
            return;
        }
        self.list.set_rect(self.rect);
    }

    /// Mirrors the `build_layout` + `apply_layout` geometry without a taffy
    /// round-trip. A no-op when the menu has never been positioned. Lets a
    /// standalone consumer (one that owns a `ContextMenu` as a field rather
    /// than a tree node) hit-test and draw the menu directly.
    pub fn layout_at_position(&mut self) {
        let was = self.rect;
        self.place_at_position();
        if self.rect != was {
            self.dirty = true;
        }
    }

    /// The panel's box for the state it is in.
    ///
    /// One placement for every way a menu opens: below its anchor, or above
    /// it where below has less room, hugging the anchor's leading edge,
    /// kept inside the window, and on the device pixels the frame draws on.
    fn place_at_position(&mut self) {
        let Some(anchor) = self.anchor else {
            return;
        };
        let Some(viewport) = self.viewport else {
            // Not in a window yet (opened between frames): stand it at the
            // anchor's leading edge until the first paint places it in the
            // window it is presented in.
            let height =
                PADDING * 2.0 + self.query_height() + self.content_height().max(ROW_HEIGHT);
            let width = self.legacy_width();
            self.rect = floating_surface::on_device_pixels(Rect::new(
                anchor.x0,
                anchor.y0,
                anchor.x0 + width,
                anchor.y0 + height,
            ));
            self.place_scroll();
            return;
        };
        // Every anchored floating panel keeps the same air from the
        // window's edges (`floating_surface::viewport_margin`, the `s2`
        // step), popovers and menus alike. A window too small for the
        // menu and its air gives up the air first, on that axis only.
        let full = self.content_height() + PADDING * 2.0 + self.query_height();
        let margin_x = PADDING
            .min((viewport.width() - self.measured_width) / 2.0)
            .max(0.0);
        let margin_y = PADDING.min((viewport.height() - full) / 2.0).max(0.0);
        let viewport = Rect::new(
            viewport.x0 + margin_x,
            viewport.y0 + margin_y,
            viewport.x1 - margin_x,
            viewport.y1 - margin_y,
        );
        let width = self.measured_width.min(viewport.width().max(0.0));
        // A pointer position opens the menu right at it; a control or a row
        // opens it `s1` off its edge.
        let gap = if is_point(anchor) { 0.0 } else { ANCHOR_GAP };
        if matches!(self.side, PopoverSide::Left | PopoverSide::Right) {
            // A nested menu opens beside its row, its first row level
            // with it: the panel starts one inset above the row.
            let height = full.min(viewport.height().max(0.0));
            let row = anchor.inflate(0.0, PADDING);
            self.rect = floating_surface::on_device_pixels(
                floating_surface::place_aligned(
                    row,
                    (width, height),
                    self.side,
                    PanelAlign::Leading,
                    0.0,
                    viewport,
                )
                .0,
            );
            self.place_scroll();
            return;
        }
        let below = floating_surface::anchored_room(anchor, PopoverSide::Below, gap, viewport);
        let above = floating_surface::anchored_room(anchor, PopoverSide::Above, gap, viewport);
        // Taller than the room on its side, a menu opened from a control or
        // a row takes the side with more room and scrolls its rows, so what
        // it stands against stays in view. A menu opened at the pointer has
        // no control to keep clear: it opens from the point, covers it where
        // the window's edge leaves no room, and never jumps to the point's
        // other side.
        let room = if below >= full {
            below
        } else {
            below.max(above)
        };
        let one_row = (self.row_height + PADDING * 2.0).min(full);
        let beside = gap > 0.0 && room >= one_row;
        let rect = if beside {
            let height = full.min(room).min(viewport.height().max(0.0));
            floating_surface::place_aligned(
                anchor,
                (width, height),
                PopoverSide::Below,
                self.align,
                gap,
                viewport,
            )
            .0
        } else {
            let height = full.min(viewport.height().max(0.0));
            let x = anchor
                .x0
                .clamp(viewport.x0, (viewport.x1 - width).max(viewport.x0));
            let y = anchor
                .y0
                .clamp(viewport.y0, (viewport.y1 - height).max(viewport.y0));
            Rect::new(x, y, x + width, y + height)
        };
        // Rows the panel cannot show at once scroll within it, and the panel
        // ends with the last row it shows whole: a row cut down to a sliver
        // at its edge is a target a person can see but cannot aim at.
        let rect = self.whole_last_row(rect);
        // Narrowed by a query, it keeps the top edge it opened with and
        // gives up height at the bottom: the field stays put.
        let rect = match self.pinned_top {
            Some(top) if top + rect.height() <= viewport.y1 + 0.5 => {
                Rect::new(rect.x0, top, rect.x1, top + rect.height())
            }
            _ => rect,
        };
        self.rect = floating_surface::on_device_pixels(rect);
        self.place_scroll();
    }

    /// `rect` with the panel's body cut back to the last row it holds whole, when it holds more
    /// rows than fit: what it cannot show scrolls in instead of being half-shown.
    fn whole_last_row(&self, rect: Rect) -> Rect {
        let room = self.body_room(rect.height());
        if self.content_height() <= room + 0.5 {
            return rect;
        }
        let mut whole = 0.0;
        for &index in &self.shown {
            let height = self.item_height(index);
            if whole + height > room + 0.5 {
                break;
            }
            whole += height;
        }
        if whole <= 0.0 {
            return rect;
        }
        let height = whole + PADDING * 2.0 + self.query_height();
        Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + height)
    }

    /// The room a panel `height` tall gives its rows: what is left of it under the query field
    /// and its own air.
    fn body_room(&self, height: f64) -> f64 {
        (height - PADDING * 2.0 - self.query_height()).max(0.0)
    }
}

/// A box with no area: the point a pointer opened a menu at.
pub(super) fn is_point(at: Rect) -> bool {
    at.width() == 0.0 && at.height() == 0.0
}
