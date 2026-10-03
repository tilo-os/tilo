//! Small drawing primitives shared by Calendar's view components.
//!
//! Every edge the calendar paints itself is *derived* (HIG §7, §9.5): a card,
//! chip or pill goes through [`ui::surface::card`] / [`ui::surface::pill`],
//! whose 1 px edge is measured against the fill it sits on, and a floating
//! panel takes the Panel role's ground and border. The helpers here compose
//! those toolkit primitives with the calendar's own state (translucent event
//! tints, selection) — none of them declares a border colour of its own.

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::chrome::{GLOW_ALPHA, panel_border, panel_ground};
pub(crate) use ui::paint::{fill_rect, fill_round, stroke_round};
use ui::surface::{edge_on, edge_outline, shadow};
use ui::theme::Theme;
use ui::tokens::colors::{Colors, over};

/// A floating Panel (HIG §9.1): the theme's elevation shadow, the Panel
/// ground and the derived Panel border — the same material as every shell
/// flyout, so an event popover or the modal editor card reads as chrome
/// rather than as a bare rounded fill on the calendar.
pub(crate) fn panel(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect, radius: f64) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    shadow(scene, transform, rect, radius, &theme.tokens.elevation.e3);
    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, transform, panel_ground(theme), None, &shape);
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        panel_border(theme),
        None,
        &edge_outline(rect, radius),
    );
}

/// The surface tier a chosen row lifts *to* from `ground`: whichever of the
/// theme's semantic tiers (raised, base, or the lift fallback a preset with
/// one flat tier gets) moves furthest from the ground it is painted on.
/// `chrome::selection_tier` lifts base → raised; a sidebar or segmented
/// track that already sits on `raised` would lift onto itself, so the lift
/// is measured against the actual ground instead (HIG §9.3: a lift is a
/// contrast step, whichever way the palette makes one).
pub(crate) fn selection_fill(theme: &Theme, ground: Color) -> Color {
    use ui::a11y::relative_luminance;
    let c = &theme.tokens.colors;
    let g = relative_luminance(ground);
    [c.surfaces.raised, c.surfaces.base, c.lift(1.0)]
        .into_iter()
        .max_by(|a, b| {
            (relative_luminance(*a) - g)
                .abs()
                .total_cmp(&(relative_luminance(*b) - g).abs())
        })
        .unwrap_or_else(|| c.lift(1.0))
}

/// What a chosen-from row on `ground` is filled with in its state: the full
/// lift when active, the quiet half-lift on hover, nothing when idle. Shared
/// by [`selected_row`] and by anything painted *inside* such a row (a count
/// badge), which has to derive against what is actually under it.
pub(crate) fn row_fill(theme: &Theme, ground: Color, active: bool, hover: bool) -> Option<Color> {
    let lifted = selection_fill(theme, ground);
    if active {
        Some(lifted)
    } else if hover {
        Some(over(ground, lifted, 0.5))
    } else {
        None
    }
}

/// What is actually under something painted *inside* a chosen-from row on
/// `ground` — the row's lift when it has one, the ground itself otherwise —
/// so a count badge can derive its own contrast step against it.
pub(crate) fn row_ground(theme: &Theme, ground: Color, active: bool, hover: bool) -> Color {
    row_fill(theme, ground, active, hover).unwrap_or(ground)
}

/// A chosen-from row or segment (HIG §9.3): the selection *lift* off the
/// `ground` it sits on — fill, derived hairline and inner glow — never an
/// accent wash. Hover alone is the quiet half-lift; an idle row paints
/// nothing.
pub(crate) fn selected_row(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    ground: Color,
    rect: Rect,
    radius: f64,
    active: bool,
    hover: bool,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let Some(fill) = row_fill(theme, ground, active, hover) else {
        return;
    };
    scene.fill(
        Fill::NonZero,
        transform,
        fill,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
    if active {
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            edge_on(fill),
            None,
            &edge_outline(rect, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            Colors::hairline_on(fill, GLOW_ALPHA),
            None,
            &edge_outline(rect.inset(-1.0), (radius - 1.0).max(0.0)),
        );
    }
}

/// A 1 px rule between `p0` and `p1`, derived against `surface` — the
/// grid line, column edge or pane divider the calendar draws on its own
/// ground. Painted as a filled rect snapped to the device pixel that
/// contains the coordinate (`x.floor()..x.floor() + 1`), so an integer or
/// fractional grid position paints exactly one crisp row of pixels rather
/// than a half-alpha smear across two. A rule on a surface's right or
/// bottom edge is therefore passed `x1 - 1.0` / `y1 - 1.0` — the last
/// pixel inside the surface.
pub(crate) fn hairline(scene: &mut Scene, transform: Affine, surface: Color, p0: Point, p1: Point) {
    let color = edge_on(surface);
    let rect = if (p0.x - p1.x).abs() < f64::EPSILON {
        let x = p0.x.floor();
        Rect::new(x, p0.y.min(p1.y), x + 1.0, p0.y.max(p1.y))
    } else {
        let y = p0.y.floor();
        Rect::new(p0.x.min(p1.x), y, p0.x.max(p1.x), y + 1.0)
    };
    scene.fill(Fill::NonZero, transform, color, None, &rect);
}

pub(crate) fn stroke_line(
    scene: &mut Scene,
    transform: Affine,
    color: Color,
    start: Point,
    end: Point,
    width: f64,
) {
    scene.stroke(
        &Stroke::new(width),
        transform,
        color,
        None,
        &Line::new(start, end),
    );
}
