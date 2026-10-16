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

/// How much of the dark material's shadow a **light** material keeps.
///
/// A drop shadow is black in either polarity, so equal alpha does not buy
/// equal weight: the same wash that a dark ground swallows whole lands hard
/// on a pale one. At 45 % the light shadow reads as the same lift.
pub const LIGHT_SHADOW_SCALE: f32 = 0.45;

/// Elevation, by the design language's one rule: **only floating things
/// cast a shadow.** Window tiles, bars, cards and selected controls have
/// none — a tile is separated by the ground and its window edge, a
/// selection is a lift ([`crate::tokens::Intents::selection`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Elevation {
    /// The one shadow a floating thing casts — a menu, popover, the
    /// palette, a sheet, quick settings, a toast, a tooltip, a dragged
    /// object: soft and wide, so it reads as distance rather than as an
    /// outline. Paint it with [`crate::surface::shadow`] at the surface's
    /// own radius.
    pub floating: Shadow,
    /// A tight shadow at a floating surface's foot, under the wide one: it
    /// defines the edge of a surface that has no outline of its own (a
    /// notification card) where the wide shadow alone reads as haze.
    pub contact: Shadow,
    /// The 1 px inner light edge a floating surface draws on a dark
    /// material, just inside its outline. Transparent on a light material.
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
            // Wide (blur 32) and soft (25 %); `offset_y` stays a quarter of
            // `blur`, or the falloff slides onto the bottom edge and reads as
            // a dark bar under the panel instead of distance.
            floating: Shadow {
                offset_y: 8.0,
                blur: 32.0,
                color: Color::from_rgba8(0, 0, 0, 60),
            },
            contact: Shadow {
                offset_y: 1.0,
                blur: 3.0,
                color: Color::from_rgba8(0, 0, 0, 72),
            },
            floating_edge: Color::from_rgba8(255, 255, 255, 20),
            glass_inner: Shadow {
                offset_y: 0.0,
                blur: 1.0,
                color: Color::from_rgba8(255, 255, 255, 20),
            },
            glass_outer: Shadow {
                offset_y: 12.0,
                blur: 32.0,
                color: Color::from_rgba8(0, 0, 0, 120),
            },
            shadow_scale: 1.0,
        }
    }

    /// Elevation on a **light** material — the same geometry, softened by
    /// [`LIGHT_SHADOW_SCALE`], with no inner edge.
    pub fn light() -> Self {
        let d = Self::dark();
        Self {
            floating: d.floating.with_alpha_scaled(LIGHT_SHADOW_SCALE),
            contact: d.contact.with_alpha_scaled(LIGHT_SHADOW_SCALE),
            floating_edge: Color::TRANSPARENT,
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
    fn floating_is_one_soft_wide_shadow_softer_on_light() {
        // phase: light_keeps_the_geometry_and_softens_the_weight
        {
            let d = Elevation::dark();
            let l = Elevation::light();
            assert!(l.floating.color.a() < d.floating.color.a());
            assert!((l.floating.blur - d.floating.blur).abs() < f32::EPSILON);
            assert!((l.floating.offset_y - d.floating.offset_y).abs() < f32::EPSILON);
            assert!((l.shadow_scale - LIGHT_SHADOW_SCALE).abs() < f32::EPSILON);
        }
        // phase: offset_stays_a_quarter_of_blur_and_the_shadow_stays_soft
        {
            let e = Elevation::default();
            assert!((e.floating.offset_y * 4.0 - e.floating.blur).abs() < f32::EPSILON);
            assert!(e.floating.blur >= 24.0, "wide");
            assert!(e.floating.color.components[3] <= 0.25, "soft");
        }
        // phase: only_the_dark_material_draws_an_inner_light_edge
        {
            let d = Elevation::dark().floating_edge;
            assert_eq!([d.r(), d.g(), d.b()], [255, 255, 255]);
            assert!(
                d.a() > 0 && d.a() < 64,
                "a whisper of light, not an outline"
            );
            assert_eq!(Elevation::light().floating_edge.a(), 0);
        }
    }
}
