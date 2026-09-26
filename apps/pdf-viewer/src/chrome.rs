//! The viewer's chrome vocabulary: every bar hairline, panel, card, row and
//! chip the app paints itself goes through here, so edges derive from the OS
//! theme in one place (HIG §7, §9.3, §9.5) instead of being restated as a
//! declared `border_standard` at each call site — which is one shade off the
//! raised surface on the dark preset and therefore invisible.
//!
//! The page raster, its markup and the thumbnails are document content, not
//! chrome, and do not come through this module.

use ui::chrome::{
    GLOW_ALPHA, PANEL_BORDER_ALPHA, SelectionState, panel_border, panel_ground, panel_shadow,
    selection_tier,
};
use ui::surface::{edge_on, edge_on_with, edge_outline};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

/// One rhythm for the viewer's chrome (HIG §7b), as compile-time steps of
/// the base spacing scale so a bar or sidebar never carries two different
/// gaps for one relationship.
pub mod rhythm {
    use ui::tokens::spacing::Spacing;

    /// The smallest gap two neighbours may ever have; a control's face inset.
    pub const TIGHT: f64 = Spacing::BASE.s1 as f64;
    /// Controls inside one cluster (the tool buttons, the two search toggles).
    pub const RELATED: f64 = Spacing::BASE.s2 as f64;
    /// Siblings of equal weight: clusters on the bar, cards and rows in a list.
    pub const SIBLING: f64 = Spacing::BASE.s3 as f64;
    /// Padding inside a card or panel body; the gap between two sections.
    pub const SECTION: f64 = Spacing::BASE.s4 as f64;
}

/// A floating Panel (the context menu): elevation, `panel_ground` and the
/// derived Panel border at `radius`.
pub fn panel(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, radius: f64) {
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

/// A card or field with its derived rest edge. When `selected`, the card is
/// the chosen one (HIG §9.3): the same face under the stronger Panel-strength
/// edge and an inner glow, never an accent wash. Used for the surfaces that
/// sit on the raised sidebar, where the shared tier's `lift` would coincide
/// with the panel behind them.
pub fn card(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, fill: Color, selected: bool) {
    ui::surface::card(scene, t, rect, radius, fill);
    if selected {
        scene.stroke(
            &Stroke::new(1.0),
            t,
            edge_on_with(fill, PANEL_BORDER_ALPHA),
            None,
            &edge_outline(rect, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            edge_on_with(fill, GLOW_ALPHA),
            None,
            &edge_outline(rect.inset(-1.0), (radius - 1.0).max(0.0)),
        );
    }
}

/// The shared lift tier for a row, tool button or menu item (HIG §9.3):
/// hover is `lift(0.5)`, chosen is `lift(1.0)` with the derived hairline and
/// inner glow. Idle paints nothing.
pub fn row(
    scene: &mut Scene,
    t: Affine,
    theme: &Theme,
    rect: Rect,
    radius: f64,
    state: SelectionState,
) {
    let tier = selection_tier(theme, state);
    if state.active || state.pressed || state.hover {
        scene.fill(
            Fill::NonZero,
            t,
            tier.fill,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
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

/// A quiet chip at rest (HIG §9.5 "Chip quiet"): the raised face with its
/// derived edge. The chosen state is the caller's — an accent fill, no edge.
pub fn quiet_pill(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, radius: f64) {
    ui::surface::card(scene, t, rect, radius, theme.tokens.colors.surfaces.raised);
}

/// The one hairline a Bar-role surface (toolbar, find bar, tab strip) draws
/// along its bottom edge `y`, derived against the bar's own fill so it reads
/// on every preset. A Bar's edge runs full-bleed.
pub fn bar_edge(scene: &mut Scene, t: Affine, fill: Color, x0: f64, x1: f64, y: f64) {
    scene.fill(
        Fill::NonZero,
        t,
        edge_on(fill),
        None,
        &Rect::new(x0, y - 1.0, x1, y),
    );
}

/// A vertical hairline along `x` (a docked panel's divider), derived against
/// the panel fill beside it.
pub fn divider(scene: &mut Scene, t: Affine, fill: Color, x: f64, y0: f64, y1: f64) {
    scene.fill(
        Fill::NonZero,
        t,
        edge_on(fill),
        None,
        &Rect::new(x, y0, x + 1.0, y1),
    );
}

/// The hit rect for a small painted control: the control's own rect grown
/// symmetrically to at least `metrics.min_pointer_target` on each side, so a
/// 24 px glyph button is still a real pointer target without painting bigger.
pub fn pointer_target(rect: Rect, theme: &Theme) -> Rect {
    let min = f64::from(theme.tokens.metrics.min_pointer_target);
    let dx = ((min - rect.width()) / 2.0).max(0.0);
    let dy = ((min - rect.height()) / 2.0).max(0.0);
    rect.inflate(dx, dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::surface::edge_delta;
    use ui::tokens::colors::HAIRLINE_MIN_DELTA;

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the rhythm tokens are exact literals, not computed values"
    )]
    fn chrome_edges_pointer_targets_and_rhythm_follow_theme_tokens() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let c = &theme.tokens.colors;
            for (name, fill) in [
                ("toolbar", c.surfaces.raised),
                ("find bar", c.surfaces.glass),
                ("sidebar", c.surfaces.raised),
                ("comment card", c.surfaces.overlay),
                ("field", c.surfaces.base),
            ] {
                let delta = edge_delta(edge_on(fill), fill);
                assert!(delta >= HAIRLINE_MIN_DELTA, "{name}: Δ={delta}");
            }
            // The chosen card's stronger edge is at least the rest edge.
            let overlay = c.surfaces.overlay;
            assert!(
                edge_delta(edge_on_with(overlay, PANEL_BORDER_ALPHA), overlay)
                    >= edge_delta(edge_on(overlay), overlay)
            );
        }

        let theme = Theme::dark();
        let min = f64::from(theme.tokens.metrics.min_pointer_target);
        let small = pointer_target(Rect::new(0.0, 0.0, 24.0, 24.0), &theme);
        assert_eq!((small.width(), small.height()), (min, min));
        let big = Rect::new(0.0, 0.0, 200.0, 40.0);
        assert_eq!(pointer_target(big, &theme), big);

        const { assert!(rhythm::TIGHT < rhythm::RELATED) };
        const { assert!(rhythm::RELATED < rhythm::SIBLING) };
        const { assert!(rhythm::SIBLING < rhythm::SECTION) };
        assert_eq!(rhythm::TIGHT, 4.0);
        assert_eq!(rhythm::SECTION, 16.0);
    }
}
