//! One painter for everything you choose one of: the window group's tab
//! strip, an in-app `TabBar`, the segments of a `ToggleGroup` and the top
//! bar's selectors (`crates/sdk/ui-core/HIG.md` §9.3).
//!
//! A selector is a [`SelectionState`] resolved against the ground it stands
//! on ([`selector_tier`]): idle paints nothing, hover is a translucent film
//! of the active lift, and active is that lift opaque, with an edge and a
//! lit rim. A docked strip of selectors stands on a body of its own
//! ([`SelectorRole::Strip`]), made of the floating material, so tabs are
//! never an unblurred translucent surface over the desktop. The theme's
//! [`crate::tokens::SelectorFinish`] says what the strip is made of and how
//! strong the rim and the hover film are; high contrast, forced colours and
//! reduced transparency make all of it opaque.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, ColorStop, Extend, Fill, Gradient};

use super::shell::{HAIRLINE_ALPHA, PRESS_SCALE, SelectionState, SelectionTier, panel_ground};
use crate::floating_surface::Material;
use crate::theme::Theme;
use crate::tokens::colors::composite;
use crate::tokens::{Colors, SelectorFinish, StripMaterial};

/// What of a selector is painted.
#[derive(Clone, Copy, Debug)]
pub enum SelectorRole<'a> {
    /// The body a docked strip of selectors stands on: the floating
    /// material over the desktop behind it (or the opaque panel ground),
    /// edged like the panel it is made of.
    Strip,
    /// One selector, in the tier [`selector_tier`] resolved (or one a
    /// caller animating it faded).
    Item(&'a SelectionTier),
}

/// The finish this theme paints selectors with: the theme's own, or
/// [`SelectorFinish::OPAQUE`] where nothing may be translucent.
#[must_use]
pub fn selector_finish(theme: &Theme) -> SelectorFinish {
    theme
        .tokens
        .selector
        .effective(theme.reduce_transparency || theme.forced_colors)
}

/// The ground a docked strip's items stand on: the panel ground its
/// material is tinted with, which is also the whole of it where nothing is
/// blurred.
#[must_use]
pub fn strip_ground(theme: &Theme) -> Color {
    panel_ground(theme)
}

/// How one selector on `ground` paints for `state`.
///
/// | State | Fill | Edge | Lit rim |
/// |---|---|---|---|
/// | idle | none | none | none |
/// | hover | the active lift at the finish's hover opacity | none | none |
/// | active | `lift_on(ground, 1.0)`, opaque | `hairline_on(ground, 26)` | the finish's rim |
/// | pressed | as active, scaled 0.97 | as active | as active |
///
/// The active lift is [`super::selection_tier_on`]'s, so a selector and a
/// selected row move the same distance off the same ground. Hover is a
/// film rather than a half step so a frosted strip still shows through a
/// hovered tab; where the finish is opaque it is that film composited onto
/// the ground. Its opacity is admitted only where it clears
/// [`super::SELECTION_MIN_STEP`] on every ground an opaque strip offers.
#[must_use]
pub fn selector_tier(theme: &Theme, state: SelectionState, ground: Color) -> SelectionTier {
    let colors = &theme.tokens.colors;
    let finish = selector_finish(theme);
    let lifted = state.active || state.pressed;
    let lift = colors.lift_on(ground, 1.0);
    let fill = if lifted {
        lift
    } else if state.hover {
        let film = lift.with_alpha(finish.hover_alpha);
        if finish.translucent() {
            film
        } else {
            composite(film, ground)
        }
    } else {
        Color::TRANSPARENT
    };
    SelectionTier {
        fill,
        border: lifted.then(|| Colors::hairline_on(ground, HAIRLINE_ALPHA)),
        inner_glow: if lifted { finish.rim() } else { None },
        scale: if state.pressed { PRESS_SCALE } else { 1.0 },
    }
}

/// Paint one selector: a strip body or an item, on `shape`.
///
/// An item paints its fill, its edge and its lit rim, and nothing else: no
/// shadow, since a lift means "chosen on the surface" and a shadow "off it"
/// (HIG §9.3). The rim is one pixel inside the edge and fades out
/// `rim_span` of the way down, so it reads as light catching the top of the
/// item rather than a ring around it.
pub fn paint_selector(
    scene: &mut Scene,
    transform: Affine,
    shape: &RoundedRect,
    role: SelectorRole<'_>,
    theme: &Theme,
) {
    let rect = shape.rect();
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    match role {
        SelectorRole::Strip => paint_strip(scene, transform, shape, theme),
        SelectorRole::Item(tier) => paint_item(scene, transform, shape, tier, theme),
    }
}

fn paint_strip(scene: &mut Scene, transform: Affine, shape: &RoundedRect, theme: &Theme) {
    let ground = strip_ground(theme);
    let material = match selector_finish(theme).strip {
        StripMaterial::Floating => Material::Floating,
        StripMaterial::Opaque => Material::Opaque,
    };
    crate::backdrop::fill_over_desktop(scene, transform, shape, material, theme, ground);
    let radius = shape.radii().top_left;
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        crate::surface::edge_on(ground),
        None,
        &crate::surface::edge_outline(shape.rect(), radius),
    );
}

fn paint_item(
    scene: &mut Scene,
    transform: Affine,
    shape: &RoundedRect,
    tier: &SelectionTier,
    theme: &Theme,
) {
    let rect = shape.rect();
    let radius = shape.radii().top_left;
    if tier.fill.components[3] > 0.0 {
        scene.fill(Fill::NonZero, transform, tier.fill, None, shape);
    }
    if let Some(edge) = tier.border {
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            edge,
            None,
            &crate::surface::edge_outline(rect, radius),
        );
    }
    let Some(rim) = tier.inner_glow else {
        return;
    };
    let inner = rect.inset(-1.0);
    if inner.width() <= 0.0 || inner.height() <= 0.0 {
        return; // an item this small has no inside for light to land on
    }
    let span = selector_finish(theme).rim_span.clamp(0.0, 1.0);
    let y = |t: f64| Point::new(inner.x0, inner.y0 + inner.height() * t);
    let light = Gradient::new_linear(y(0.0), y(f64::from(span)))
        .with_extend(Extend::Pad)
        .with_stops(
            &[
                ColorStop {
                    offset: 0.0,
                    color: rim.into(),
                },
                ColorStop {
                    offset: 1.0,
                    color: rim.with_alpha(0.0).into(),
                },
            ][..],
        );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        &light,
        None,
        &crate::surface::edge_outline(inner, (radius - 1.0).max(0.0)),
    );
}

/// The capsule a strip of selectors at `rect` is drawn as, concentric with
/// items inset `inset` inside it at `item_radius`.
#[must_use]
pub fn strip_shape(rect: Rect, item_radius: f64, inset: f64) -> RoundedRect {
    RoundedRect::from_rect(rect, (item_radius + inset).min(rect.height() / 2.0))
}
