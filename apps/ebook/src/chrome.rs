//! The reader's chrome vocabulary. Every panel, card, row, pill, rail and bar
//! edge the app paints itself goes through here, so the look is derived from
//! the OS theme in one place (HIG §7, §9.3, §9.5) instead of being restated —
//! and drifting — at eighty call sites. The book page itself is not chrome:
//! it keeps the reader palette and does not come through this module.
//!
//! Edges are never declared: a card's edge derives from its own fill
//! ([`ui::surface::edge_on`]), a floating panel wears the shared Panel border,
//! and "which one is chosen" is the shared lift tier — never an accent wash,
//! which the accent needs to keep meaning *live* (HIG §9.4).

use ui::chrome::{
    ChromeRole, SelectionState, fill_scrim, panel_border, panel_ground, panel_shadow,
    selection_tier,
};
use ui::surface::{edge_on, edge_outline};
use ui::theme::Theme;
use ui::tokens::metrics::Metrics;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

/// One rhythm for the reader's chrome (HIG §7b), as compile-time steps of
/// the base spacing scale so a screen never carries two different gaps for
/// one relationship.
pub mod rhythm {
    use ui::tokens::spacing::Spacing;

    /// Icon ↔ its label; the smallest gap two neighbours may ever have.
    pub const TIGHT: f64 = Spacing::BASE.s1 as f64;
    /// Related rows that read as one group (title + explanation, label + control).
    pub const RELATED: f64 = Spacing::BASE.s2 as f64;
    /// Siblings of equal weight: toolbar controls, list rows, cards in a rail.
    pub const SIBLING: f64 = Spacing::BASE.s3 as f64;
    /// Padding inside a panel or card; the gap between two sections.
    pub const SECTION: f64 = Spacing::BASE.s4 as f64;
    /// Before the next group; the page gutter.
    pub const GROUP: f64 = Spacing::BASE.s6 as f64;
}

/// How strongly a modal sheet dims what it covers. One value for every sheet
/// (scheme editor, font picker, colour picker) so they read as one family.
pub const SHEET_SCRIM: f32 = 0.45;

/// The face of one row in a dropdown menu — the pointer-target metric plus a
/// tight step; the pitch adds [`rhythm::RELATED`] so two adjacent lifted rows
/// never touch. Shared by the shelf's sort menu and the settings' default-font
/// menu, which had drifted to 34 and 40.
pub const MENU_ROW_H: f64 = Metrics::BASE.min_pointer_target as f64 + rhythm::TIGHT;

/// The ordered lift ramp of a floored row (HIG §9.3's caveat): idle, hover
/// and chosen are three points on one `Colors::lift` ramp against the ground,
/// so the chosen row is always the most prominent and the hover always sits
/// between — no mixing of a floor with the tier.
pub const FLOOR_IDLE_T: f32 = 0.35;
pub const FLOOR_HOVER_T: f32 = 0.65;

/// A floating Panel (menu, popover, sheet, composer): elevation, `panel_ground`
/// and the derived Panel border at the Panel radius.
pub fn panel(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect) {
    let radius = ChromeRole::Panel.radius(theme, rect.height());
    panel_shadow(scene, t, rect, radius, theme, 1.0);
    scene.fill(
        Fill::NonZero,
        t,
        panel_ground(theme),
        None,
        &RoundedRect::from_rect(rect, radius),
    );
    scene.stroke(
        &Stroke::new(1.0),
        t,
        panel_border(theme),
        None,
        &edge_outline(rect, radius),
    );
}

/// A raised card on the app base (a settings row, an annotation card, the
/// library's empty-state card) with its derived rest edge.
pub fn card(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, radius: f64) {
    ui::surface::card(scene, t, rect, radius, theme.tokens.colors.surfaces.raised);
}

/// A card that sits *inside* a raised surface (a comment row on a panel, a
/// binding pill on a shortcut row): one tier up, `overlay`, with its edge.
pub fn card_on_raised(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, radius: f64) {
    ui::surface::card(scene, t, rect, radius, theme.tokens.colors.surfaces.overlay);
}

/// Stroke a derived edge on an arbitrary filled surface — a colour swatch, a
/// generated cover — measured against the fill it sits on.
pub fn edge(scene: &mut Scene, t: Affine, fill: Color, rect: Rect, radius: f64) {
    scene.stroke(
        &Stroke::new(1.0),
        t,
        edge_on(fill),
        None,
        &edge_outline(rect, radius),
    );
}

/// Seat content of unknown colour (real cover art, a photo) against the
/// `ground` it lies on: the 1 px edge is stroked just *outside* the content,
/// on the ground it was derived from, so a dark image on a light shelf and a
/// light image on a dark shelf both get a frame that registers.
pub fn seat(scene: &mut Scene, t: Affine, ground: Color, rect: Rect, radius: f64) {
    scene.stroke(
        &Stroke::new(1.0),
        t,
        edge_on(ground),
        None,
        &RoundedRect::from_rect(rect.inset(0.5), radius + 0.5),
    );
}

/// The shared lift tier for a row, tile or menu item (HIG §9.3): hover is
/// `lift(0.5)`, chosen is `lift(1.0)` with the derived hairline and inner
/// glow. With `floor` the row has a face of its own at rest — idle, hover and
/// chosen are then three points on one ordered lift ramp ([`FLOOR_IDLE_T`],
/// [`FLOOR_HOVER_T`], 1.0) and the derived edge stays on in every state, so a
/// list reads as rows rather than as bare text and the outline never vanishes
/// under the pointer.
pub fn row(
    scene: &mut Scene,
    t: Affine,
    theme: &Theme,
    rect: Rect,
    radius: f64,
    state: SelectionState,
    floor: bool,
) {
    let colors = &theme.tokens.colors;
    let tier = selection_tier(theme, state);
    let lifted = state.active || state.pressed;
    let shape = RoundedRect::from_rect(rect, radius);
    if floor {
        let fill = colors.lift(floor_ramp(state));
        scene.fill(Fill::NonZero, t, fill, None, &shape);
        if !lifted {
            edge(scene, t, fill, rect, radius);
        }
    } else if lifted || state.hover {
        scene.fill(Fill::NonZero, t, tier.fill, None, &shape);
    }
    if let Some(border) = tier.border {
        scene.stroke(
            &Stroke::new(1.0),
            t,
            border,
            None,
            &edge_outline(rect, radius),
        );
    }
    if let Some(glow) = tier.inner_glow {
        scene.stroke(
            &Stroke::new(1.0),
            t,
            glow,
            None,
            &edge_outline(rect.inset(-1.0), (radius - 1.0).max(0.0)),
        );
    }
}

/// Where on the `Colors::lift` ramp a floored row's face sits for `state`.
pub fn floor_ramp(state: SelectionState) -> f32 {
    if state.active || state.pressed {
        1.0
    } else if state.hover {
        FLOOR_HOVER_T
    } else {
        FLOOR_IDLE_T
    }
}

/// A quiet chip or pill at rest (HIG §9.5 "Chip quiet"): `fill` with its
/// derived edge; hover swaps in the theme's hover surface. The chosen state
/// is the caller's — an accent fill with no edge.
pub fn quiet_pill(
    scene: &mut Scene,
    t: Affine,
    theme: &Theme,
    rect: Rect,
    radius: f64,
    fill: Color,
    hover: bool,
) {
    let fill = if hover { theme.bg_hover } else { fill };
    ui::surface::card(scene, t, rect, radius, fill);
}

/// The one hairline a Bar-role surface (a top bar) draws along `y`, derived
/// against the bar's own fill so it reads on every preset. A Bar's edge runs
/// full-bleed: there is no corner radius for it to run into.
pub fn bar_edge(scene: &mut Scene, t: Affine, fill: Color, x0: f64, x1: f64, y: f64) {
    scene.fill(
        Fill::NonZero,
        t,
        edge_on(fill),
        None,
        &Rect::new(x0, y - 1.0, x1, y),
    );
}

/// A docked rail (contents, annotations): the app base with a derived
/// hairline down its `edge_x` side. The rail stays on `surfaces.base` because
/// its rows are painted with the shared selection tier, which lifts from
/// `base` — a lifted rail would swallow the tier's hover step.
pub fn rail(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, edge_x: f64) {
    let fill = theme.tokens.colors.surfaces.base;
    scene.fill(Fill::NonZero, t, fill, None, &rect);
    let x0 = if edge_x >= rect.x1 - 0.5 {
        edge_x - 1.0
    } else {
        edge_x
    };
    scene.fill(
        Fill::NonZero,
        t,
        edge_on(fill),
        None,
        &Rect::new(x0, rect.y0, x0 + 1.0, rect.y1),
    );
}

/// Dim the content under a modal sheet with the shared scrim.
pub fn sheet_scrim(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect) {
    fill_scrim(scene, t, rect, SHEET_SCRIM, theme);
}

/// The hit rect for a small painted control: the control's own rect grown
/// symmetrically to at least `metrics.min_pointer_target` on each side, so a
/// 22 px toggle track or a 24 px glyph button is still a real pointer target
/// without painting bigger.
pub fn pointer_target(rect: Rect, theme: &Theme) -> Rect {
    let min = f64::from(theme.tokens.metrics.min_pointer_target);
    let dx = ((min - rect.width()) / 2.0).max(0.0);
    let dy = ((min - rect.height()) / 2.0).max(0.0);
    rect.inflate(dx, dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::a11y::relative_luminance;
    use ui::surface::edge_delta;
    use ui::tokens::colors::HAIRLINE_MIN_DELTA;

    /// Chrome edges register and floored rows keep order on every preset; pointer targets grow to the metric and rhythm steps are monotone.
    #[test]
    fn chrome_metrics_register_and_grow_on_every_preset() {
        // --- phase: every_chrome_edge_registers_on_every_preset ---
        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let c = &theme.tokens.colors;
                for (name, fill) in [
                    ("raised card", c.surfaces.raised),
                    ("overlay card", c.surfaces.overlay),
                    ("rail / bar on base", c.surfaces.base),
                    ("floored row", c.lift(FLOOR_IDLE_T)),
                    ("hovered floored row", c.lift(FLOOR_HOVER_T)),
                ] {
                    let delta = edge_delta(edge_on(fill), fill);
                    assert!(delta >= HAIRLINE_MIN_DELTA, "{name}: Δ={delta}");
                }
            }
        }

        // --- phase: floored_rows_keep_their_order_on_every_preset ---
        // HIG §9.3's caveat: a control with its own floor derives every state
        // from one ordered ramp against the ground, so the chosen row is never
        // less prominent than an idle one — on every preset.
        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let c = &theme.tokens.colors;
                let base = relative_luminance(c.surfaces.base);
                let dist = |state: SelectionState| {
                    (relative_luminance(c.lift(floor_ramp(state))) - base).abs()
                };
                let idle = dist(SelectionState::default());
                let hover = dist(SelectionState::new(false, true, false));
                let active = dist(SelectionState::active(true));
                assert!(idle < hover, "idle {idle} !< hover {hover}");
                assert!(hover < active, "hover {hover} !< active {active}");
            }
        }

        // --- phase: pointer_targets_grow_to_the_metric_and_never_shrink ---
        {
            let theme = Theme::dark();
            let min = f64::from(theme.tokens.metrics.min_pointer_target);
            let small = pointer_target(Rect::new(0.0, 0.0, 22.0, 22.0), &theme);
            assert_eq!(small.width(), min);
            assert_eq!(small.height(), min);
            assert_eq!(small.center(), Rect::new(0.0, 0.0, 22.0, 22.0).center());
            let big = Rect::new(0.0, 0.0, 200.0, 40.0);
            assert_eq!(pointer_target(big, &theme), big);
        }

        // --- phase: rhythm_is_monotone_token_steps ---
        {
            const { assert!(rhythm::TIGHT < rhythm::RELATED) };
            const { assert!(rhythm::RELATED < rhythm::SIBLING) };
            const { assert!(rhythm::SIBLING < rhythm::SECTION) };
            const { assert!(rhythm::SECTION < rhythm::GROUP) };
            assert_eq!(rhythm::TIGHT, 4.0);
            assert_eq!(rhythm::SIBLING, 12.0);
        }
    }
}
