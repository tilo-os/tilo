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
#[derive(Clone, Copy, Debug)]
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

/// How much of the dark ladder's alpha a **light** theme's shadows keep.
///
/// A drop shadow is black in either polarity, so equal alpha does not buy
/// equal weight: the same wash that a dark ground swallows whole lands hard
/// on a pale one. The dark ladder measures 1.03:1 to 1.08:1 against its own
/// ground — a seat, not a cue — while the identical alphas measure 1.43:1 to
/// 3.11:1 on a light ground, which is heavier than the theme's own card
/// border. Scaling to 45 % puts the light tiers at roughly 6 / 10 / 14 / 20 %
/// black, the range light-theme elevation actually lives in.
pub const LIGHT_SHADOW_SCALE: f32 = 0.45;

#[derive(Clone, Copy, Debug)]
pub struct Elevation {
    pub e0: Shadow,
    pub e1: Shadow,
    pub e2: Shadow,
    pub e3: Shadow,
    pub e4: Shadow,
    pub glass_inner: Shadow,
    pub glass_outer: Shadow,
    /// The factor this theme's shadows were scaled by, relative to the dark
    /// ladder. Exposed so a widget that composes its own shadow stack out of
    /// several rings — rather than taking a single tier through
    /// [`crate::surface::elevation`] — can stay per-theme without duplicating
    /// the reasoning above.
    pub shadow_scale: f32,
}

impl Elevation {
    /// The ladder for a **dark** theme.
    pub fn dark() -> Self {
        let c = |a: u8| Color::from_rgba8(0, 0, 0, a);
        Self {
            // `blur` roughly doubles each rung with `offset_y = blur / 4` — a
            // third-of-blur offset would shove the falloff almost entirely
            // to the bottom, seven times denser there than beside.
            //
            // Alpha grows far more slowly than blur and stays low: a
            // floating surface is separated first by its own ground
            // (`panel_ground`) and edge (`panel_border`) — on the
            // high-contrast palette a black shadow on a black desktop
            // contributes nothing, the edge does the job. Elevation that
            // must be *strong* to work is compensating for a surface that
            // doesn't separate.
            e0: Shadow {
                offset_y: 0.0,
                blur: 0.0,
                color: c(0),
            },
            e1: Shadow {
                offset_y: 1.0,
                blur: 4.0,
                color: c(24),
            },
            e2: Shadow {
                offset_y: 2.0,
                blur: 8.0,
                color: c(32),
            },
            e3: Shadow {
                offset_y: 3.0,
                blur: 12.0,
                color: c(40),
            },
            e4: Shadow {
                offset_y: 4.0,
                blur: 16.0,
                color: c(48),
            },
            // The glass pair is a different vocabulary — an inner rim
            // light and an outer bloom for the wheel disc, not a drop
            // shadow — and only its hue is read. Left as declared.
            glass_inner: Shadow {
                offset_y: 0.0,
                blur: 1.0,
                color: Color::from_rgba8(255, 255, 255, 20),
            },
            glass_outer: Shadow {
                offset_y: 12.0,
                blur: 32.0,
                color: c(120),
            },
            shadow_scale: 1.0,
        }
    }

    /// The ladder for a **light** theme — the same geometry, softened by
    /// [`LIGHT_SHADOW_SCALE`].
    pub fn light() -> Self {
        let d = Self::dark();
        Self {
            e0: d.e0.with_alpha_scaled(LIGHT_SHADOW_SCALE),
            e1: d.e1.with_alpha_scaled(LIGHT_SHADOW_SCALE),
            e2: d.e2.with_alpha_scaled(LIGHT_SHADOW_SCALE),
            e3: d.e3.with_alpha_scaled(LIGHT_SHADOW_SCALE),
            e4: d.e4.with_alpha_scaled(LIGHT_SHADOW_SCALE),
            // The glass pair is a different vocabulary — an inner rim
            // light and an outer bloom, not a drop shadow — and only its
            // hue is read, so it passes through unscaled.
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
    use crate::a11y::contrast_ratio;
    use crate::theme::Theme;

    fn alpha(s: &Shadow) -> u8 {
        s.color.to_rgba8().a
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn elevation_ladders_are_monotonic_soft_and_proportioned() {
        // phase: light_themes_get_a_softer_ladder_than_dark_ones
        // A drop shadow is black either way, so the ladder cannot be one shared
        // set of alphas: what a dark ground swallows lands hard on a pale one.
        {
            let d = Elevation::dark();
            let l = Elevation::light();
            for (tier, dark, light) in [
                ("e1", &d.e1, &l.e1),
                ("e2", &d.e2, &l.e2),
                ("e3", &d.e3, &l.e3),
                ("e4", &d.e4, &l.e4),
            ] {
                assert!(alpha(light) < alpha(dark), "{tier}: light must be softer");
                // Geometry is shared — only the weight differs.
                assert_eq!(light.blur, dark.blur, "{tier}: same blur");
                assert_eq!(light.offset_y, dark.offset_y, "{tier}: same offset");
            }
            assert_eq!(l.shadow_scale, LIGHT_SHADOW_SCALE);
            assert_eq!(d.shadow_scale, 1.0);
        }
        // phase: every_ladder_is_monotonic
        // The ladder must stay a ladder in both polarities: each tier heavier
        // than the one below it.
        {
            for (name, e) in [("dark", Elevation::dark()), ("light", Elevation::light())] {
                let steps = [
                    alpha(&e.e0),
                    alpha(&e.e1),
                    alpha(&e.e2),
                    alpha(&e.e3),
                    alpha(&e.e4),
                ];
                for w in steps.windows(2) {
                    assert!(w[1] > w[0], "{name}: the ladder must climb, got {steps:?}");
                }
            }
        }
        // phase: a_stacked_ring_shadow_stays_soft_on_every_light_theme
        // The defect the split exists to prevent, pinned at the shape that
        // actually broke.
        //
        // A widget that approximates a soft shadow with a stack of concentric rings
        // composites every ring where they overlap, so the weight compounds. With
        // the dark ladder's alphas on a white card that stack landed on `#A6A6A6`
        // — a 2.12:1 band, heavier than the card border and heavy enough that a
        // tab strip read as a solid grey slab. Scaled, the same stack lands near
        // `#D3D3D3`.
        {
            // The ring alphas `tabs.rs` composes, before the per-theme scale.
            const RINGS: [f32; 4] = [51.0, 30.0, 15.0, 5.0];

            for name in Theme::ALL {
                let theme = Theme::named(name).unwrap();
                let scale = theme.tokens.elevation.shadow_scale;
                if scale >= 1.0 {
                    continue; // dark ladder: black on a dark ground is already soft
                }
                let ground = theme.tokens.colors.surfaces.raised;
                let ground_channels = ground.to_rgba8();
                let (mut red, mut green, mut blue) = (
                    f32::from(ground_channels.r),
                    f32::from(ground_channels.g),
                    f32::from(ground_channels.b),
                );
                for ring in RINGS {
                    let alpha = (ring * scale).round() / 255.0;
                    red *= 1.0 - alpha;
                    green *= 1.0 - alpha;
                    blue *= 1.0 - alpha;
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "multiplying u8-derived channels by 0.0..=1.0 alphas keeps each rounded channel within 0..=255"
                )]
                let seen =
                    Color::from_rgb8(red.round() as u8, green.round() as u8, blue.round() as u8);
                let weight = contrast_ratio(seen, ground);
                assert!(
                    weight < 1.60,
                    "{name}: a stacked ring shadow composites to {weight:.2}:1 on the card \
                 — at the unscaled alphas this was 2.12:1 and read as a solid band",
                );
            }
        }
        // phase: offset_stays_a_quarter_of_blur
        // Pinned so a future retune has to move `offset_y` and `blur` together.
        {
            let e = Elevation::default();
            for (name, s) in [("e1", &e.e1), ("e2", &e.e2), ("e3", &e.e3), ("e4", &e.e4)] {
                assert_eq!(
                    s.offset_y * 4.0,
                    s.blur,
                    "{name}: offset_y must stay blur/4 so the falloff does not \
                 collapse onto the bottom edge (offset {} blur {})",
                    s.offset_y,
                    s.blur
                );
            }
        }
        // phase: ramp_rises_and_stays_subtle
        // The ramp rises monotonically in blur and opacity, staying subtle at
        // the top — `e4`'s alpha above ~0.25 is the regression this guards
        // against: a painter discarding the alpha field entirely composites at 1.0.
        {
            let e = Elevation::default();
            let steps = [&e.e1, &e.e2, &e.e3, &e.e4];
            for w in steps.windows(2) {
                assert!(w[1].blur > w[0].blur, "blur must rise up the ramp");
                assert!(
                    w[1].color.components[3] > w[0].color.components[3],
                    "opacity must rise up the ramp"
                );
            }
            assert!(
                e.e4.color.components[3] <= 0.25,
                "e4 is a panel's elevation and must stay subtle; a heavier \
             shadow is elevation compensating for a surface that does not \
             separate on its own (got {})",
                e.e4.color.components[3]
            );
        }
    }
}
