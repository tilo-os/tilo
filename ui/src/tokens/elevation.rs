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
pub struct Shadow { pub offset_y: f32, pub blur: f32, pub color: Color }

#[derive(Clone, Copy, Debug)]
pub struct Elevation { pub e0: Shadow, pub e1: Shadow, pub e2: Shadow, pub e3: Shadow, pub e4: Shadow, pub glass_inner: Shadow, pub glass_outer: Shadow }

impl Default for Elevation {
    fn default() -> Self {
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
            e0: Shadow { offset_y: 0.0, blur: 0.0, color: c(0) },
            e1: Shadow { offset_y: 1.0, blur: 4.0, color: c(24) },
            e2: Shadow { offset_y: 2.0, blur: 8.0, color: c(32) },
            e3: Shadow { offset_y: 3.0, blur: 12.0, color: c(40) },
            e4: Shadow { offset_y: 4.0, blur: 16.0, color: c(48) },
            // The glass pair is a different vocabulary — an inner rim
            // light and an outer bloom for the wheel disc, not a drop
            // shadow — and only its hue is read. Left as declared.
            glass_inner: Shadow { offset_y: 0.0, blur: 1.0, color: Color::from_rgba8(255, 255, 255, 20) },
            glass_outer: Shadow { offset_y: 12.0, blur: 32.0, color: c(120) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned so a future retune has to move `offset_y` and `blur` together.
    #[test]
    fn offset_stays_a_quarter_of_blur() {
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

    /// The ramp rises monotonically in blur and opacity, staying subtle at
    /// the top — `e4`'s alpha above ~0.25 is the regression this guards
    /// against: a painter discarding the alpha field entirely composites at 1.0.
    #[test]
    fn ramp_rises_and_stays_subtle() {
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
