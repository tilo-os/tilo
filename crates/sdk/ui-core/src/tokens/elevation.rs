use vello::peniko::Color;

/// One step of the elevation ramp — a complete description of a drop
/// shadow, and the **only** description a surface is allowed to use.
///
/// - `offset_y` — how far the shadow sits below the surface, held at a
///   quarter of `blur` all the way up the ramp; larger and the shadow
///   reads as a dark bar under one edge rather than depth.
/// - `blur` — a CSS-style blur radius; [`crate::surface::shadow`] halves
///   it for the Gaussian's standard deviation, so falloff dies out about
///   `1.25 * blur` past the surface's outline.
/// - `color` — hue **and strength**. Alpha is honoured, not decorative:
///   dropping it is how the quick-settings panel once rendered a
///   43%-black token as a 100%-black smear.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub offset_y: f32,
    pub blur: f32,
    pub color: Color,
}

impl Shadow {
    /// The same shadow with its alpha scaled by `factor` — the one way an
    /// animating surface fades its elevation with itself.
    #[must_use]
    pub fn with_alpha_scaled(self, factor: f32) -> Self {
        Self {
            offset_y: self.offset_y,
            blur: self.blur,
            color: self.color.multiply_alpha(factor),
        }
    }
}

/// How much of the dark material's floating shadow a **light** material
/// keeps: the design language's light floating layer (alpha 0.14) over the
/// dark one (0.38).
///
/// A drop shadow is black in either polarity, so equal alpha does not buy
/// equal weight: the same wash that a dark ground swallows whole lands hard
/// on a pale one.
pub const LIGHT_SHADOW_SCALE: f32 = 0.14 / 0.38;

/// A shadow of `blur` at `offset_y`, black at `alpha`.
const fn black(offset_y: f32, blur: f32, alpha: u8) -> Shadow {
    Shadow {
        offset_y,
        blur,
        color: Color::from_rgba8(0, 0, 0, alpha),
    }
}

/// Elevation (`docs/design-language.md`, "Surfaces"): a window tile casts
/// one soft shadow that lifts it off the desktop ground; a floating thing
/// casts two, a tight contact layer that defines its edge and a wide one
/// that reads as distance. Bars, cards and selected controls cast none: a
/// selection is a lift ([`crate::tokens::Intents::selection`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Elevation {
    /// The soft shadow under a window tile on the desktop ground, painted
    /// at the tile's radius.
    pub window: Shadow,
    /// The wide layer of a floating thing's shadow — a menu, popover, the
    /// palette, a sheet, quick settings, a toast, a tooltip, a dragged
    /// object. Paint both layers with [`crate::surface::floating_shadow`].
    pub floating: Shadow,
    /// The tight layer at a floating surface's foot, under the wide one: it
    /// defines the edge where the wide shadow alone reads as haze.
    pub contact: Shadow,
    /// The 1 px inner hairline a floating surface draws just inside its
    /// outline: white on a dark material, black on a light one.
    pub floating_edge: Color,
    /// The wheel disc's rim light: a different vocabulary from a drop
    /// shadow, of which only the hue is read.
    pub glass_inner: Shadow,
    /// The wheel disc's outer bloom. Only the hue is read.
    pub glass_outer: Shadow,
    /// The factor this material's shadow was scaled by relative to the dark
    /// one, for a widget composing its own soft stack (a sprite, a ring).
    pub shadow_scale: f32,
}

impl Elevation {
    /// Elevation on a **dark** material.
    pub fn dark() -> Self {
        Self {
            // 0.28, 0.38 and 0.30 of black.
            window: black(2.0, 16.0, 71),
            floating: black(12.0, 32.0, 97),
            contact: black(1.0, 2.0, 77),
            // White at 7 %.
            floating_edge: Color::from_rgba8(255, 255, 255, 18),
            glass_inner: Shadow {
                offset_y: 0.0,
                blur: 1.0,
                color: Color::from_rgba8(255, 255, 255, 20),
            },
            glass_outer: black(12.0, 32.0, 120),
            shadow_scale: 1.0,
        }
    }

    /// Elevation on a **light** material: the same geometry, lighter, and
    /// a black inner hairline.
    pub fn light() -> Self {
        let d = Self::dark();
        Self {
            // 0.10, 0.14 and 0.08 of black.
            window: black(2.0, 16.0, 26),
            floating: black(12.0, 32.0, 36),
            contact: black(1.0, 2.0, 20),
            // Black at 8 %.
            floating_edge: Color::from_rgba8(0, 0, 0, 20),
            glass_inner: d.glass_inner,
            glass_outer: d.glass_outer,
            shadow_scale: LIGHT_SHADOW_SCALE,
        }
    }
}

impl Default for Elevation {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_compat::ColorCompat;

    #[test]
    fn windows_cast_one_soft_shadow_and_floating_things_two() {
        // phase: the_design_languages_numbers
        {
            let d = Elevation::dark();
            let l = Elevation::light();
            let alpha = |s: Shadow| (s.color.a(), s.offset_y, s.blur);
            assert_eq!(alpha(d.window), (71, 2.0, 16.0));
            assert_eq!(alpha(l.window), (26, 2.0, 16.0));
            assert_eq!(alpha(d.contact), (77, 1.0, 2.0));
            assert_eq!(alpha(l.contact), (20, 1.0, 2.0));
            assert_eq!(alpha(d.floating), (97, 12.0, 32.0));
            assert_eq!(alpha(l.floating), (36, 12.0, 32.0));
        }
        // phase: light_keeps_the_geometry_and_softens_the_weight
        {
            let d = Elevation::dark();
            let l = Elevation::light();
            for (dark, light) in [
                (d.window, l.window),
                (d.floating, l.floating),
                (d.contact, l.contact),
            ] {
                assert!(light.color.a() < dark.color.a());
                assert!((light.blur - dark.blur).abs() < f32::EPSILON);
                assert!((light.offset_y - dark.offset_y).abs() < f32::EPSILON);
            }
        }
        // phase: the_contact_layer_is_tight_and_the_wide_layer_soft
        {
            let e = Elevation::default();
            assert!(e.contact.blur < 4.0, "tight");
            assert!(e.floating.blur >= 24.0, "wide");
            assert!(e.floating.color.components[3] <= 0.40, "soft");
        }
        // phase: the_inner_hairline_is_white_on_dark_and_black_on_light
        {
            let d = Elevation::dark().floating_edge;
            assert_eq!([d.r(), d.g(), d.b(), d.a()], [255, 255, 255, 18]);
            let l = Elevation::light().floating_edge;
            assert_eq!([l.r(), l.g(), l.b(), l.a()], [0, 0, 0, 20]);
        }
    }
}
