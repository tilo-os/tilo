//! Shared geometry and paint scaffolding for quick-settings drill-in lists.
//!
//! This deliberately stops at structure common to Wi-Fi, Bluetooth and VPN:
//! header/back, row placement and the calm list-row visual language.
//! Device-specific glyphs, state and actions stay in their owning panes.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace};

const HEADER_HEIGHT: f64 = 48.0;
const LIST_TOP: f64 = 56.0;
const ROW_HEIGHT: f64 = 56.0;
const ROW_INSET: f64 = 12.0;

#[allow(
    clippy::cast_precision_loss,
    reason = "quick-settings row counts are tiny"
)]
fn usize_to_f64_lossy(value: usize) -> f64 {
    value as f64
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DrillInOutcome {
    #[default]
    None,
    Back,
    Changed,
}

#[derive(Default)]
pub struct DrillInList {
    back: Rect,
    rows: Vec<Rect>,
    first_visible: usize,
    row_count: usize,
    visible_capacity: usize,
    wheel_remainder: f64,
}

impl DrillInList {
    pub fn layout(&mut self, panel: Rect, row_count: usize) {
        self.layout_from(panel, row_count, LIST_TOP);
    }

    /// Layout a list below a pane-owned compact control.  Wi-Fi uses this
    /// for the Hotspot row so a nonessential radio does not occupy the root
    /// dashboard while remaining discoverable with the rest of Wi-Fi.
    pub fn layout_from(&mut self, panel: Rect, row_count: usize, list_top: f64) {
        self.back = Rect::new(
            panel.x0 + 8.0,
            panel.y0 + 8.0,
            panel.x0 + 40.0,
            panel.y0 + 40.0,
        );
        self.row_count = row_count;
        self.visible_capacity = Self::capacity(panel, list_top);
        self.first_visible = self
            .first_visible
            .min(row_count.saturating_sub(self.visible_capacity));
        let visible = self
            .visible_capacity
            .min(row_count.saturating_sub(self.first_visible));
        self.rows.clear();
        self.rows.extend((0..visible).map(|slot| {
            let y = panel.y0 + list_top + usize_to_f64_lossy(slot) * ROW_HEIGHT;
            Rect::new(
                panel.x0 + ROW_INSET,
                y,
                panel.x1 - ROW_INSET,
                y + ROW_HEIGHT - 4.0,
            )
        }));
    }

    fn capacity(panel: Rect, list_top: f64) -> usize {
        let available = (panel.height() - list_top - ROW_INSET).max(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative panel pixels divided by a fixed row height fit usize"
        )]
        let rows = ((available + 4.0) / ROW_HEIGHT).floor() as usize;
        rows
    }

    #[cfg(test)]
    pub fn visible_capacity(&self) -> usize {
        self.visible_capacity
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn visible_indices(&self) -> std::ops::Range<usize> {
        self.first_visible..self.first_visible.saturating_add(self.rows.len())
    }

    /// Pure projection of the current scroll window, used by semantic/test
    /// geometry before the next paint has had a chance to refresh `rows`.
    pub fn visible_rects(
        &self,
        panel: Rect,
        row_count: usize,
        list_top: f64,
    ) -> Vec<(usize, Rect)> {
        let capacity = Self::capacity(panel, list_top);
        let first = self.first_visible.min(row_count.saturating_sub(capacity));
        let visible = capacity.min(row_count.saturating_sub(first));
        (0..visible)
            .map(|slot| {
                let y = panel.y0 + list_top + usize_to_f64_lossy(slot) * ROW_HEIGHT;
                (
                    first.saturating_add(slot),
                    Rect::new(
                        panel.x0 + ROW_INSET,
                        y,
                        panel.x1 - ROW_INSET,
                        y + ROW_HEIGHT - 4.0,
                    ),
                )
            })
            .collect()
    }

    pub fn row_for(&self, index: usize) -> Option<&Rect> {
        index
            .checked_sub(self.first_visible)
            .and_then(|slot| self.rows.get(slot))
    }

    /// Move the bounded viewport by whole rows. Positive movement reveals
    /// later entries; negative movement returns toward the first entry.
    pub fn scroll_by(
        &mut self,
        panel: Rect,
        row_count: usize,
        list_top: f64,
        movement: isize,
    ) -> bool {
        self.layout_from(panel, row_count, list_top);
        let max_start = row_count.saturating_sub(self.visible_capacity);
        let next = if movement.is_negative() {
            self.first_visible.saturating_sub(movement.unsigned_abs())
        } else {
            self.first_visible
                .saturating_add(usize::try_from(movement).unwrap_or(usize::MAX))
        }
        .min(max_start);
        if next == self.first_visible {
            return false;
        }
        self.first_visible = next;
        self.layout_from(panel, row_count, list_top);
        true
    }

    /// Accumulate touchpad fractions without leaking a wheel gesture through
    /// to the workspace behind the modal panel.
    pub fn scroll_wheel(
        &mut self,
        panel: Rect,
        row_count: usize,
        list_top: f64,
        rows: f64,
    ) -> bool {
        self.wheel_remainder += rows;
        let whole = self.wheel_remainder.trunc();
        if whole.abs() < 1.0 {
            return false;
        }
        self.wheel_remainder -= whole;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "wheel input is bounded; only complete accumulated rows are converted"
        )]
        self.scroll_by(panel, row_count, list_top, whole as isize)
    }

    /// Keep keyboard or accessibility focus inside the painted viewport.
    pub fn ensure_visible(
        &mut self,
        panel: Rect,
        row_count: usize,
        list_top: f64,
        index: usize,
    ) -> bool {
        self.layout_from(panel, row_count, list_top);
        if index >= row_count || self.visible_capacity == 0 {
            return false;
        }
        let next = if index < self.first_visible {
            index
        } else if index >= self.first_visible.saturating_add(self.visible_capacity) {
            index
                .saturating_add(1)
                .saturating_sub(self.visible_capacity)
        } else {
            self.first_visible
        };
        if next == self.first_visible {
            return false;
        }
        self.first_visible = next;
        self.layout_from(panel, row_count, list_top);
        true
    }

    pub fn back_contains(&self, point: Point) -> bool {
        self.back.contains(point)
    }

    pub fn row_at(&self, point: Point) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.contains(point))
            .map(|slot| self.first_visible.saturating_add(slot))
    }

    pub fn rows(&self) -> &[Rect] {
        &self.rows
    }

    #[cfg(test)]
    pub fn back_center(&self) -> Point {
        self.back.center()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, shared verbatim by every \
                  drill-in pane (wifi/bluetooth/vpn) that opens with this header"
    )]
    pub fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        cursor: Option<Point>,
        title: &str,
    ) {
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(self.back, "")
                .ghost()
                .icon(ui::icons::Icon::ChevronLeft)
                .hovered(cursor.is_some_and(|point| self.back.contains(point)))
                .accessible_name("Back"),
        );
        let (width, height) = text.measure(title, theme.tokens.typography.scale.sm, None);
        text.draw_text_weighted(
            scene,
            title,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            transform.pre_translate(
                (
                    panel.center().x - f64::from(width) / 2.0,
                    panel.y0 + (HEADER_HEIGHT - f64::from(height)) / 2.0,
                )
                    .into(),
            ),
            None,
            TextWeight::Semibold,
        );
    }

    pub fn draw_empty(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        message: &str,
    ) {
        let (width, height) = text.measure(message, theme.tokens.typography.scale.sm, None);
        text.draw_text(
            scene,
            message,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_dim,
            transform.pre_translate(
                (
                    panel.center().x - f64::from(width) / 2.0,
                    panel.y0 + LIST_TOP + 24.0 - f64::from(height) / 2.0,
                )
                    .into(),
            ),
            None,
        );
    }

    /// Shared row treatment and label placement for discovery lists. Every
    /// real item gets the same quiet surface; the former special-cased first
    /// item made a list of two look like one selected card plus stray text.
    /// Returns the leading glyph box for the owning pane to paint.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, shared verbatim by every \
                  drill-in pane (wifi/bluetooth/vpn) that paints rows with it"
    )]
    pub fn draw_discovery_row(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        _index: usize,
        cursor: Option<Point>,
        trailing_reserve: f64,
        label: &str,
    ) -> Rect {
        let hovered = cursor.is_some_and(|point| row.contains(point));
        let row_fill = if hovered {
            theme.tokens.colors.neutral[3].with_alpha_factor(0.72)
        } else {
            theme.tokens.colors.neutral[2].with_alpha_factor(0.42)
        };
        scene.fill(
            Fill::NonZero,
            transform,
            row_fill,
            None,
            &RoundedRect::from_rect(row, 12.0),
        );
        let glyph = Rect::new(
            row.x0 + 12.0,
            row.y0 + (row.height() - 20.0) / 2.0,
            row.x0 + 32.0,
            row.y0 + f64::midpoint(row.height(), 20.0),
        );
        // Wi-Fi strength, Bluetooth battery and connection status all occupy
        // the trailing edge of a discovery row. Never let a long SSID or
        // device name paint underneath those truthful status indicators.
        let label_width = (row.x1 - trailing_reserve - (glyph.x1 + 12.0)).max(1.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the bounded quick-settings row width is represented as f32 by the text shaper"
        )]
        let label =
            text.elide_to_width(label, theme.tokens.typography.scale.sm, label_width as f32);
        let (_, label_height) = text.measure_weighted(
            &label,
            theme.tokens.typography.scale.sm,
            None,
            TextWeight::Medium,
        );
        text.draw_text_weighted(
            scene,
            &label,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            transform.pre_translate(
                (
                    glyph.x1 + 12.0,
                    row.center().y - f64::from(label_height) / 2.0,
                )
                    .into(),
            ),
            None,
            TextWeight::Medium,
        );
        glyph
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_panel_bounds_rows_and_scrolls_by_data_index() {
        let panel = Rect::new(600.0, 40.0, 948.0, 240.0);
        let mut list = DrillInList::default();
        list.layout(panel, 12);
        assert_eq!(list.visible_capacity(), 2);
        assert_eq!(list.visible_indices(), 0..2);
        assert!(list.rows().iter().all(|row| row.y1 <= panel.y1 - ROW_INSET));

        assert!(list.scroll_by(panel, 12, LIST_TOP, 3));
        assert_eq!(list.visible_indices(), 3..5);
        assert_eq!(list.row_at(list.rows()[0].center()), Some(3));

        let normal = Rect::new(1200.0, 48.0, 1576.0, 600.0);
        list.layout(normal, 12);
        assert_eq!(list.visible_capacity(), 8);
        assert!(
            list.rows()
                .iter()
                .all(|row| row.y1 <= normal.y1 - ROW_INSET)
        );
    }

    #[test]
    fn focus_visibility_moves_the_smallest_possible_window() {
        let panel = Rect::new(600.0, 40.0, 948.0, 240.0);
        let mut list = DrillInList::default();
        list.layout(panel, 12);
        assert!(list.ensure_visible(panel, 12, LIST_TOP, 7));
        assert_eq!(list.visible_indices(), 6..8);
        assert!(!list.ensure_visible(panel, 12, LIST_TOP, 6));
        assert!(list.ensure_visible(panel, 12, LIST_TOP, 1));
        assert_eq!(list.visible_indices(), 1..3);
    }

    #[test]
    fn fractional_wheel_motion_accumulates_without_moving_early() {
        let panel = Rect::new(600.0, 40.0, 948.0, 240.0);
        let mut list = DrillInList::default();
        list.layout(panel, 12);
        assert!(!list.scroll_wheel(panel, 12, LIST_TOP, 0.4));
        assert!(!list.scroll_wheel(panel, 12, LIST_TOP, 0.4));
        assert!(list.scroll_wheel(panel, 12, LIST_TOP, 0.4));
        assert_eq!(
            list.visible_indices(),
            1..3,
            "0.2 rows remain after one step"
        );
    }
}
