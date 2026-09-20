//! Shared geometry and paint scaffolding for quick-settings drill-in lists.
//!
//! This deliberately stops at structure common to Wi-Fi, Bluetooth and VPN:
//! header/back, row placement and the calm list-row visual language.
//! Device-specific glyphs, state and actions stay in their owning panes.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace};

const HEADER_HEIGHT: f64 = 48.0;
const LIST_TOP: f64 = 56.0;
const ROW_HEIGHT: f64 = 56.0;
/// Horizontal inset of every drill-in row from the panel edge.
///
/// `spacing.s3` at 1×, one step tighter than the root page's `PAD` (16).
/// The Wi-Fi pane's hotspot row (`quick_settings_wifi.rs:88,90,109,149`)
/// still restates the same 12 px as literals, so today the two only move
/// together by hand; the constant is public so the pane owner can read it
/// and retire those literals.
pub const ROW_INSET: f64 = 12.0;

#[allow(
    clippy::cast_precision_loss,
    reason = "quick-settings row counts are tiny"
)]
fn usize_to_f64_lossy(value: usize) -> f64 {
    value as f64
}

/// Theme typography tokens already carry the user's text scale, and the
/// renderer scales every authored size again; divide once so the scale is
/// applied exactly once (HIG §7a). Identity while the renderer is at 1×.
fn type_size(text: &TextRenderer, resolved: f32) -> f32 {
    resolved / text.text_scale()
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
        let (width, height) = text.measure(title, type_size(text, theme.tokens.typography.scale.sm), None);
        text.draw_text_weighted(
            scene,
            title,
            type_size(text, theme.tokens.typography.scale.sm),
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
        let (width, height) = text.measure(message, type_size(text, theme.tokens.typography.scale.sm), None);
        text.draw_text(
            scene,
            message,
            type_size(text, theme.tokens.typography.scale.sm),
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
        // Control role (HIG §9.1/§9.2): the row's surface is a lift measured
        // against the panel ground, never a ramp slot, and its radius is the
        // Control radius rather than a literal that happens to equal it.
        // Hover is the shared selection tier (§9.3); the idle floor sits on
        // the same `lift` ramp below it, so the two states stay ordered on
        // every theme (asserted in `tests::row_floor_stays_below_the_hover_tier`).
        let row_fill = discovery_row_fill(theme, hovered);
        scene.fill(
            Fill::NonZero,
            transform,
            row_fill,
            None,
            &RoundedRect::from_rect(
                row,
                ui::chrome::ChromeRole::Control.radius(theme, row.height()),
            ),
        );
        let inset = f64::from(theme.tokens.spacing.s3);
        let glyph_px = f64::from(theme.tokens.metrics.icon_md);
        let glyph = Rect::new(
            row.x0 + inset,
            row.y0 + (row.height() - glyph_px) / 2.0,
            row.x0 + inset + glyph_px,
            row.y0 + f64::midpoint(row.height(), glyph_px),
        );
        // Wi-Fi strength, Bluetooth battery and connection status all occupy
        // the trailing edge of a discovery row. Never let a long SSID or
        // device name paint underneath those truthful status indicators.
        let label_width = (row.x1 - 72.0 - (glyph.x1 + inset)).max(1.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the bounded quick-settings row width is represented as f32 by the text shaper"
        )]
        let label =
            text.elide_to_width(label, type_size(text, theme.tokens.typography.scale.sm), label_width as f32);
        let (_, label_height) = text.measure_weighted(
            &label,
            type_size(text, theme.tokens.typography.scale.sm),
            None,
            TextWeight::Medium,
        );
        text.draw_text_weighted(
            scene,
            &label,
            type_size(text, theme.tokens.typography.scale.sm),
            theme.tokens.colors.text_primary,
            transform.pre_translate(
                (
                    glyph.x1 + inset,
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

/// Idle floor of a discovery row, on the `lift` ramp under the hover tier.
const ROW_FLOOR_LIFT_T: f32 = 0.35;

/// A discovery row's fill: the shared hover tier when hovered, otherwise a
/// quiet floor on the same ramp so a list of two reads as rows rather than
/// as bare text on the panel.
fn discovery_row_fill(theme: &Theme, hovered: bool) -> vello::peniko::Color {
    if hovered {
        ui::chrome::selection_tier(theme, ui::chrome::SelectionState::new(false, true, false)).fill
    } else {
        theme.tokens.colors.lift(ROW_FLOOR_LIFT_T)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::a11y::relative_luminance;

    /// HIG §9.3: a control that gives its idle state a floor must keep every
    /// state on one ordered ramp, or the hovered row can end up quieter
    /// than the idle one on a palette whose `lift` falls back to a derived
    /// step. `lift` moves from `surfaces.base`, so the order is measured
    /// from there: hover must travel further along the ramp than the floor.
    #[test]
    fn row_floor_stays_below_the_hover_tier() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let origin = relative_luminance(theme.tokens.colors.surfaces.base);
            let idle = relative_luminance(discovery_row_fill(&theme, false));
            let hover = relative_luminance(discovery_row_fill(&theme, true));
            assert!(
                (hover - origin).abs() > (idle - origin).abs(),
                "{}: hover ({hover:.4}) must move further from base ({origin:.4}) than idle ({idle:.4})",
                theme.name
            );
        }
    }

    /// The glyph box a pane paints into is the standard `icon_md` glyph on
    /// the `s3` inset, centred on the row — not a 20 in a 12 that happens to
    /// match those tokens today.
    #[test]
    fn discovery_row_glyph_is_the_token_glyph_centred_on_the_row() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let row = Rect::new(100.0, 200.0, 420.0, 252.0);
        let glyph = DrillInList::draw_discovery_row(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            row,
            0,
            None,
            "Coffee shop",
        );
        assert_eq!(glyph.width(), f64::from(theme.tokens.metrics.icon_md));
        assert_eq!(glyph.x0, row.x0 + f64::from(theme.tokens.spacing.s3));
        assert!((glyph.center().y - row.center().y).abs() < 1e-9);
    }
}
