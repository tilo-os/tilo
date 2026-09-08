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
        self.rows.clear();
        self.rows.extend((0..row_count).map(|index| {
            let y = panel.y0 + list_top + usize_to_f64_lossy(index) * ROW_HEIGHT;
            Rect::new(
                panel.x0 + ROW_INSET,
                y,
                panel.x1 - ROW_INSET,
                y + ROW_HEIGHT - 4.0,
            )
        }));
    }

    pub fn back_contains(&self, point: Point) -> bool {
        self.back.contains(point)
    }

    pub fn row_at(&self, point: Point) -> Option<usize> {
        self.rows.iter().position(|row| row.contains(point))
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
        let label_width = (row.x1 - 72.0 - (glyph.x1 + 12.0)).max(1.0);
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
