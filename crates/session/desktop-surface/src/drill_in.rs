//! Shared geometry and paint scaffolding for the Quick Settings panel.
//!
//! One rhythm for every page (HIG §7b, §9.6) and one Control-role painter
//! (§9.1, §9.5) that the root page and the Wi-Fi, Bluetooth and VPN
//! drill-ins paint their tiles, cards, slider block and rows with. This
//! deliberately stops at structure common to every page: header/back, row
//! placement and the calm list-row visual language. Device-specific glyphs,
//! state and actions stay in their owning panes.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;

use ui::TextRole;
use ui::chrome::{ChromeRole, SelectionState, selection_tier};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::{Metrics, Spacing};
use ui::widget::{Button, ButtonFace};

// ---------- one rhythm for the whole panel (HIG §7b) ----------

/// Panel padding: the root's grid, every list row and every header chip
/// start this far inside the panel edge (`s4`, HIG §9.6).
pub const PANEL_PAD: f64 = Spacing::BASE.s4 as f64;
/// Gap between Panel sections — title band, tiles, sliders, cards, power
/// row (`s4`, HIG §9.6).
pub const SECTION_GAP: f64 = Spacing::BASE.s4 as f64;
/// Gap between sibling controls of equal weight in a grid: tile ↔ tile,
/// card ↔ card, chip ↔ chip (`s3`).
pub const SIBLING_GAP: f64 = Spacing::BASE.s3 as f64;
/// Gap between related rows that read as one group: list rows, the two
/// slider bands, a card's icon row and its label block (`s2`).
pub const RELATED_GAP: f64 = Spacing::BASE.s2 as f64;
/// Vertical padding inside a Control (`s2`).
pub const CONTROL_PAD: f64 = Spacing::BASE.s2 as f64;
/// Leading / trailing inset of a glyph, toggle or chevron inside a Control
/// (`s3`).
pub const CONTROL_INSET: f64 = Spacing::BASE.s3 as f64;
/// A list row / Control height (HIG §9.6: 44).
pub const ROW_HEIGHT: f64 = Metrics::BASE.panel_row_height as f64;
/// The standard row glyph (`icon_md`).
pub const GLYPH: f64 = Metrics::BASE.icon_md as f64;
/// The pointer floor: header chips, the edit-mode action button and the
/// drill-in target on a card are this square (HIG §9.6).
pub const TARGET: f64 = Metrics::BASE.min_pointer_target as f64;
/// The drill-in title band: a pointer-floor back button on `s2` of air.
pub const HEADER_HEIGHT: f64 = TARGET + 2.0 * RELATED_GAP;
/// Where a drill-in's first row starts: the title band plus one related
/// step.
pub const LIST_TOP: f64 = HEADER_HEIGHT + RELATED_GAP;
/// The compact switch on a Quick Settings row: as tall as the row glyph and
/// two knob positions wide less the `s1` they share.
pub const TOGGLE_H: f64 = GLYPH;
pub const TOGGLE_W: f64 = 2.0 * TOGGLE_H - Spacing::BASE.s1 as f64;
/// The "connected" dot on a row's trailing inset: half a glyph.
pub const STATUS_DOT: f64 = GLYPH / 2.0;
/// Idle floor of a Control on the panel, on the `lift` ramp under the hover
/// tier (the `power_menu` / drill-in reference floor). Reach it through
/// [`control_fill`], the one definition every shell Control paints with.
const CONTROL_FLOOR_LIFT_T: f32 = 0.35;

#[allow(
    clippy::cast_precision_loss,
    reason = "quick-settings row counts are tiny"
)]
fn usize_to_f64_lossy(value: usize) -> f64 {
    value as f64
}

// ---------- the Control role, painted once ----------

/// A Control's fill: the shared hover tier when hovered, otherwise a quiet
/// floor on the same ramp so tiles and rows read as surfaces rather than as
/// bare text on the panel. Both states sit on one ordered ramp, so hover is
/// always the more prominent (asserted per theme in the tests).
pub fn control_fill(theme: &Theme, hovered: bool) -> Color {
    if hovered {
        selection_tier(theme, SelectionState::new(false, true, false)).fill
    } else {
        theme.tokens.colors.lift(CONTROL_FLOOR_LIFT_T)
    }
}

/// The Control radius for `rect` (`radii.control`, HIG §9.1).
pub fn control_radius(theme: &Theme, rect: Rect) -> f64 {
    ChromeRole::Control.radius(theme, rect.height())
}

/// Paint one Control-role surface (HIG §9.1 / §9.5): its fill with the
/// derived rest edge (`ui::surface::card`) at the Control radius, and the
/// toolkit `Button`'s 2 px focus ring on top when `focused`. One painter for
/// every tile, card, slider block, power row, edit row and list row in the
/// panel, so they cannot drift apart again.
pub fn paint_control(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    rect: Rect,
    hovered: bool,
    focused: bool,
) {
    let radius = control_radius(theme, rect);
    ui::surface::card(scene, transform, rect, radius, control_fill(theme, hovered));
    if focused {
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.border_focus,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
    }
}

// ---------- row geometry: everything sits on the row's centre line ----------

/// A `glyph`-sized box on `row`'s leading `inset`, centred on its centre
/// line.
pub fn leading_glyph(row: Rect, inset: f64, glyph: f64) -> Rect {
    let y = row.center().y - glyph / 2.0;
    Rect::new(row.x0 + inset, y, row.x0 + inset + glyph, y + glyph)
}

/// A `glyph`-sized box on `row`'s trailing `inset` (a chevron), centred on
/// its centre line.
pub fn trailing_glyph(row: Rect, inset: f64, glyph: f64) -> Rect {
    let y = row.center().y - glyph / 2.0;
    Rect::new(row.x1 - inset - glyph, y, row.x1 - inset, y + glyph)
}

/// A `w × h` toggle on `row`'s trailing `inset`, centred on its centre line.
pub fn trailing_toggle(row: Rect, inset: f64, w: f64, h: f64) -> Rect {
    let y = row.center().y - h / 2.0;
    Rect::new(row.x1 - inset - w, y, row.x1 - inset, y + h)
}

/// Centre of the "connected" dot: a [`STATUS_DOT`] disc on the trailing
/// `inset`.
pub fn trailing_dot_center(row: Rect, inset: f64) -> Point {
    Point::new(row.x1 - inset - STATUS_DOT / 2.0, row.center().y)
}

/// The status glyph slot before the dot (a battery, signal bars): one related
/// step inboard of it, centred on the row's centre line.
pub fn trailing_status_glyph(row: Rect, inset: f64, glyph: f64) -> Rect {
    let x1 = row.x1 - inset - STATUS_DOT - RELATED_GAP;
    let y = row.center().y - glyph / 2.0;
    Rect::new(x1 - glyph, y, x1, y + glyph)
}

/// Top of a text block `h` tall centred on `row`'s centre line.
pub fn centred_text_top(row: Rect, h: f64) -> f64 {
    row.center().y - h / 2.0
}

fn discovery_label_width(row: Rect, glyph: Rect, trailing_reserve: f64) -> f64 {
    let label_x = glyph.x1 + CONTROL_INSET;
    // Every discovery row owns two fixed trailing status slots. Individual
    // panes may reserve more (for example Wi-Fi's connection status), but
    // never less than the shared glyph/dot geometry requires.
    let status_slots = 2.0 * GLYPH + CONTROL_INSET + RELATED_GAP;
    (row.x1 - trailing_reserve.max(status_slots) - label_x).max(1.0)
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
        self.back = Self::back_rect(panel);
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
            let y = panel.y0 + list_top + usize_to_f64_lossy(slot) * (ROW_HEIGHT + RELATED_GAP);
            Rect::new(
                panel.x0 + PANEL_PAD,
                y,
                panel.x1 - PANEL_PAD,
                y + ROW_HEIGHT,
            )
        }));
    }

    fn capacity(panel: Rect, list_top: f64) -> usize {
        let available = (panel.height() - list_top - PANEL_PAD).max(0.0);
        let pitch = ROW_HEIGHT + RELATED_GAP;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative panel pixels divided by the fixed row pitch fit usize"
        )]
        {
            ((available + RELATED_GAP) / pitch).floor() as usize
        }
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn visible_indices(&self) -> std::ops::Range<usize> {
        self.first_visible..self.first_visible.saturating_add(self.rows.len())
    }

    /// Pure projection of the current scroll window. Semantic geometry can
    /// use this before paint without mutating the retained list.
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
                let y = panel.y0 + list_top + usize_to_f64_lossy(slot) * (ROW_HEIGHT + RELATED_GAP);
                (
                    first.saturating_add(slot),
                    Rect::new(
                        panel.x0 + PANEL_PAD,
                        y,
                        panel.x1 - PANEL_PAD,
                        y + ROW_HEIGHT,
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

    /// Accumulate touchpad fractions until they amount to a whole row.
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

    /// The pointer-floor back button in the title band's leading corner, on
    /// `s2` of air. The overlay host's chevron hit rect reads the same
    /// function, so paint and input cannot disagree.
    pub fn back_rect(panel: Rect) -> Rect {
        Rect::new(
            panel.x0 + RELATED_GAP,
            panel.y0 + RELATED_GAP,
            panel.x0 + RELATED_GAP + TARGET,
            panel.y0 + RELATED_GAP + TARGET,
        )
    }

    /// The height a drill-in needs for `rows` rows below `list_top`: an
    /// empty list keeps one row's band for its message, then the panel
    /// padding closes the page.
    pub fn list_height(list_top: f64, rows: usize) -> f64 {
        let rows = usize_to_f64_lossy(rows.max(1));
        list_top + rows * ROW_HEIGHT + (rows - 1.0) * RELATED_GAP + PANEL_PAD
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

    /// Centre of the Back button, where a press pops the drill-in.
    pub fn back_center(&self) -> Point {
        self.back.center()
    }

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
                .accessible_name(&crate::i18n::t("drill-in-back")),
        );
        let style = theme.text(TextRole::Caption);
        let (width, height) = text.measure_style(
            title,
            None,
            &style.clone().with_weight(TextWeight::Semibold),
        );
        text.draw_style(
            scene,
            title,
            theme.tokens.colors.text_primary,
            transform.pre_translate(
                (
                    panel.center().x - f64::from(width) / 2.0,
                    panel.y0 + (HEADER_HEIGHT - f64::from(height)) / 2.0,
                )
                    .into(),
            ),
            None,
            &style.with_weight(TextWeight::Semibold),
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
        Self::draw_empty_from(scene, transform, text, theme, panel, message, LIST_TOP);
    }

    /// Centred dim empty-state message in the band the first row would
    /// occupy under `list_top` — without it the pane read as broken rather
    /// than "nothing here yet".
    pub fn draw_empty_from(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        message: &str,
        list_top: f64,
    ) {
        let style = theme.text(TextRole::Caption);
        let (width, height) = text.measure_style(message, None, &style);
        text.draw_style(
            scene,
            message,
            theme.tokens.colors.text_dim,
            transform.pre_translate(
                (
                    panel.center().x - f64::from(width) / 2.0,
                    panel.y0 + list_top + (ROW_HEIGHT - f64::from(height)) / 2.0,
                )
                    .into(),
            ),
            None,
            &style,
        );
    }

    /// Shared row treatment and label placement for discovery lists. Every
    /// real item gets the same Control surface; the former special-cased
    /// first item made a list of two look like one selected card plus stray
    /// text. Returns the leading glyph box for the owning pane to paint.
    pub fn draw_discovery_row(
        ctx: &mut ui::paint::PaintCtx<'_>,
        row: Rect,
        cursor: Option<Point>,
        trailing_reserve: f64,
        label: &str,
    ) -> Rect {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let hovered = cursor.is_some_and(|point| row.contains(point));
        paint_control(scene, transform, theme, row, hovered, false);
        let glyph = leading_glyph(row, CONTROL_INSET, GLYPH);
        // Wi-Fi strength, Bluetooth battery and the connected dot occupy the
        // trailing edge of a discovery row: two glyph slots on the inset and
        // a related step. Never let a long SSID or device name paint under
        // those truthful status indicators.
        let label_x = glyph.x1 + CONTROL_INSET;
        let label_width = discovery_label_width(row, glyph, trailing_reserve);
        let style = theme.text(TextRole::Caption);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the bounded quick-settings row width is represented as f32 by the text shaper"
        )]
        let label = text.elide_to_width_style(label, label_width as f32, &style);
        let (_, label_height) =
            text.measure_style(&label, None, &style.clone().with_weight(TextWeight::Medium));
        text.draw_style(
            scene,
            &label,
            theme.tokens.colors.text_primary,
            transform
                .pre_translate((label_x, centred_text_top(row, f64::from(label_height))).into()),
            None,
            &style.clone().with_weight(TextWeight::Medium),
        );
        glyph
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::a11y::relative_luminance;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn drill_in_rows_keep_token_tiers_glyphs_and_panel_rhythm() {
        /// HIG §9.3: a control that gives its idle state a floor must keep every
        /// state on one ordered ramp, or the hovered row can end up quieter
        /// than the idle one on a palette whose `lift` falls back to a derived
        /// step. `lift` moves from `surfaces.base`, so the order is measured
        /// from there: hover must travel further along the ramp than the floor.
        fn row_floor_stays_below_the_hover_tier() {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let origin = relative_luminance(theme.tokens.colors.surfaces.base);
                let idle = relative_luminance(control_fill(&theme, false));
                let hover = relative_luminance(control_fill(&theme, true));
                assert!(
                    (hover - origin).abs() > (idle - origin).abs(),
                    "{}: hover ({hover:.4}) must move further from base ({origin:.4}) than idle ({idle:.4})",
                    theme.name
                );
            }
        }

        /// HIG §9.5 rest edges: on every preset a Control on the panel is
        /// visible at rest — its derived edge registers against its own fill,
        /// and fill or edge (whichever the eye meets first at the boundary)
        /// steps away from the Panel ground it sits on.
        fn control_surface_registers_on_the_panel_ground_on_every_preset() {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let panel = ui::chrome::panel_ground(&theme);
                let fill = control_fill(&theme, false);
                let edge = ui::surface::edge_on(fill);
                let min = ui::tokens::colors::HAIRLINE_MIN_DELTA;
                assert!(
                    ui::surface::edge_delta(edge, fill) >= min,
                    "{}: the rest edge must register on the control's own fill",
                    theme.name
                );
                let step = (relative_luminance(fill) - relative_luminance(panel)).abs();
                let edge_on_panel = ui::surface::edge_delta(edge, panel);
                assert!(
                    step >= min || edge_on_panel >= min,
                    "{}: fill step {step:.4} and edge-on-panel {edge_on_panel:.4} are both under {min}",
                    theme.name
                );
            }
        }

        /// The glyph box a pane paints into is the standard `icon_md` glyph on
        /// the `s3` inset, centred on the row — not a 20 in a 12 that happens to
        /// match those tokens today.
        fn discovery_row_glyph_is_the_token_glyph_centred_on_the_row() {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let mut scene = Scene::new();
            let row = Rect::new(100.0, 200.0, 420.0, 244.0);
            let glyph = DrillInList::draw_discovery_row(
                &mut ui::paint::PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
                row,
                None,
                0.0,
                "Coffee shop",
            );
            assert_eq!(glyph.width(), f64::from(theme.tokens.metrics.icon_md));
            assert_eq!(glyph.x0, row.x0 + f64::from(theme.tokens.spacing.s3));
            assert!((glyph.center().y - row.center().y).abs() < 1e-9);
        }

        fn discovery_label_width_honours_pane_reserve_and_status_floor() {
            let row = Rect::new(100.0, 200.0, 500.0, 244.0);
            let glyph = leading_glyph(row, CONTROL_INSET, GLYPH);
            let status_floor = 2.0 * GLYPH + CONTROL_INSET + RELATED_GAP;

            assert_eq!(
                discovery_label_width(row, glyph, 0.0),
                discovery_label_width(row, glyph, status_floor)
            );
            assert_eq!(
                discovery_label_width(row, glyph, status_floor + 32.0),
                discovery_label_width(row, glyph, status_floor) - 32.0
            );
        }

        /// One rhythm (HIG §7b): rows are the Control height on the panel
        /// padding, one related step apart, and the trailing slots never reach
        /// into each other or the row's inset.
        fn list_rows_keep_the_panel_rhythm_and_trailing_slots_do_not_touch() {
            let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
            let mut list = DrillInList::default();
            list.layout(panel, 3);
            let rows = list.rows();
            assert_eq!(rows[0].x0, panel.x0 + PANEL_PAD);
            assert_eq!(rows[0].x1, panel.x1 - PANEL_PAD);
            assert_eq!(rows[0].y0, panel.y0 + LIST_TOP);
            assert_eq!(
                rows[0].height(),
                f64::from(ui::tokens::Metrics::BASE.panel_row_height)
            );
            assert_eq!(
                rows[1].y0 - rows[0].y1,
                f64::from(ui::tokens::Spacing::BASE.s2)
            );
            assert!(DrillInList::back_rect(panel).y1 + RELATED_GAP <= rows[0].y0);
            let row = rows[0];
            let glyph = leading_glyph(row, CONTROL_INSET, GLYPH);
            let status = trailing_status_glyph(row, CONTROL_INSET, GLYPH);
            let dot = trailing_dot_center(row, CONTROL_INSET);
            assert!(glyph.x1 + CONTROL_INSET < status.x0);
            assert!(status.x1 + RELATED_GAP <= dot.x - STATUS_DOT / 2.0);
            assert_eq!(dot.x + STATUS_DOT / 2.0, row.x1 - CONTROL_INSET);
            assert_eq!(
                DrillInList::list_height(LIST_TOP, 3),
                rows[2].y1 - panel.y0 + PANEL_PAD
            );
        }

        /// The rhythm is compile-time (`Spacing::BASE` / `Metrics::BASE`, HIG
        /// §7b) because hit-testing and the preferred height are resolved with
        /// no theme in hand and must agree with paint to the pixel. That is only
        /// honest while the values equal what a preset theme resolves at 1×;
        /// this pins it, so a token change cannot silently split the two.
        fn compile_time_rhythm_equals_the_preset_tokens_at_one_x() {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let spacing = &theme.tokens.spacing;
                let metrics = &theme.tokens.metrics;
                assert_eq!(PANEL_PAD, f64::from(spacing.s4), "{}", theme.name);
                assert_eq!(SECTION_GAP, f64::from(spacing.s4), "{}", theme.name);
                assert_eq!(SIBLING_GAP, f64::from(spacing.s3), "{}", theme.name);
                assert_eq!(RELATED_GAP, f64::from(spacing.s2), "{}", theme.name);
                assert_eq!(CONTROL_PAD, f64::from(spacing.s2), "{}", theme.name);
                assert_eq!(CONTROL_INSET, f64::from(spacing.s3), "{}", theme.name);
                assert_eq!(
                    ROW_HEIGHT,
                    f64::from(metrics.panel_row_height),
                    "{}",
                    theme.name
                );
                assert_eq!(GLYPH, f64::from(metrics.icon_md), "{}", theme.name);
                assert_eq!(
                    TARGET,
                    f64::from(metrics.min_pointer_target),
                    "{}",
                    theme.name
                );
                assert_eq!(
                    TOGGLE_W,
                    2.0 * TOGGLE_H - f64::from(spacing.s1),
                    "{}",
                    theme.name
                );
            }
        }

        row_floor_stays_below_the_hover_tier();
        control_surface_registers_on_the_panel_ground_on_every_preset();
        discovery_row_glyph_is_the_token_glyph_centred_on_the_row();
        discovery_label_width_honours_pane_reserve_and_status_floor();
        list_rows_keep_the_panel_rhythm_and_trailing_slots_do_not_touch();
        compile_time_rhythm_equals_the_preset_tokens_at_one_x();
    }
}
