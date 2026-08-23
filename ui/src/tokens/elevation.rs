use vello::peniko::Color;

/// One step of the elevation ramp — a complete description of a drop
/// shadow, and the **only** description a surface is allowed to use.
///
/// - `offset_y` — how far the shadow sits below the surface. Held at a
///   quarter of `blur` all the way up the ramp. An offset that is large
///   against the blur makes the lower edge far denser than the sides, and
///   the shadow stops reading as depth and starts reading as a dark bar
///   parked under one edge.
/// - `blur` — a CSS-style blur radius. [`crate::surface::shadow`] halves
///   it to get the Gaussian's standard deviation, so the falloff dies out
///   about `1.25 * blur` past the surface's own outline.
/// - `color` — hue **and strength**. The alpha is the shadow's opacity and
///   it is honoured. It is not a decorative field for a painter to drop on
///   the floor: doing exactly that is how the quick-settings panel came to
///   render a 43 %-black token as a 100 %-black smear.
#[derive(Clone, Copy, Debug)]
pub struct Shadow { pub offset_y: f32, pub blur: f32, pub color: Color }

#[derive(Clone, Copy, Debug)]
pub struct Elevation { pub e0: Shadow, pub e1: Shadow, pub e2: Shadow, pub e3: Shadow, pub e4: Shadow, pub glass_inner: Shadow, pub glass_outer: Shadow }

impl Default for Elevation {
    fn default() -> Self {
        let c = |a: u8| Color::from_rgba8(0, 0, 0, a);
        Self {
            // The ramp is `blur` doubling-ish with `offset_y = blur / 4`
            // and an alpha that grows far more slowly than the blur does.
            //
            // Both of those are corrections. The ramp used to reach
            // `{ offset_y: 8, blur: 24, alpha: 110 }` at `e4`, and a panel
            // wearing it put a 24 px skirt around itself that was seven
            // times denser under its bottom edge than beside it — the
            // offset was a third of the blur, so the falloff was shoved
            // almost entirely to one side. A shadow reads as depth only
            // while it is close to even; past that it reads as a
            // rectangle drawn slightly wrong.
            //
            // Alpha is deliberately low. A drop shadow is a secondary cue
            // here: a floating surface is separated first by its own
            // ground (`chrome::shell::panel_ground`) and its edge
            // (`chrome::shell::panel_border`), and on the high-contrast
            // palette — black chrome on a black desktop — a black shadow
            // contributes nothing at all and the edge is doing the entire
            // job. Elevation that has to be *strong* to work is elevation
            // compensating for a surface that does not separate.
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

    /// Every rung of the drop-shadow ramp keeps `offset_y` at a quarter of
    /// `blur`.
    ///
    /// This is the invariant that keeps a shadow even. When the offset
    /// grows relative to the blur, the Gaussian slides off one side: the
    /// bottom edge darkens, the top edge disappears, and the surface
    /// stops looking lifted and starts looking like it has a bar under
    /// it. Pinned so a future retune has to move both numbers together.
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

    /// The ramp rises monotonically in both blur and opacity, and stays
    /// subtle at the top.
    ///
    /// The ceiling is the point. `e4` is what a floating panel wears, and
    /// a panel is separated by its ground and its edge before a shadow is
    /// consulted at all. An `e4` alpha above ~0.25 is the regime the
    /// 2026-08 report called "way, way too large"; the value that drew
    /// that report composited at 1.0 because the painter discarded this
    /// field entirely.
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
