use vello::peniko::Color;

#[derive(Clone, Debug)]
pub struct Shadow { pub offset_y: f32, pub blur: f32, pub color: Color }

impl Shadow {
    /// The same shadow with its alpha scaled by `factor`.
    fn with_alpha_scaled(&self, factor: f32) -> Self {
        let a = (self.color.to_rgba8().a as f32 * factor).round().clamp(0.0, 255.0) as u8;
        Self {
            offset_y: self.offset_y,
            blur: self.blur,
            color: Color::from_rgba8(0, 0, 0, a),
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

#[derive(Clone, Debug)]
pub struct Elevation {
    pub e0: Shadow,
    pub e1: Shadow,
    pub e2: Shadow,
    pub e3: Shadow,
    pub e4: Shadow,
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
            e0: Shadow { offset_y: 0.0, blur: 0.0, color: c(0) },
            e1: Shadow { offset_y: 1.0, blur: 2.0, color: c(40) },
            e2: Shadow { offset_y: 2.0, blur: 6.0, color: c(60) },
            e3: Shadow { offset_y: 4.0, blur: 12.0, color: c(80) },
            e4: Shadow { offset_y: 8.0, blur: 24.0, color: c(110) },
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
            shadow_scale: LIGHT_SHADOW_SCALE,
        }
    }
}

impl Default for Elevation {
    fn default() -> Self { Self::dark() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::contrast_ratio;
    use crate::theme::Theme;

    fn alpha(s: &Shadow) -> u8 { s.color.to_rgba8().a }

    /// A drop shadow is black either way, so the ladder cannot be one shared
    /// set of alphas: what a dark ground swallows lands hard on a pale one.
    #[test]
    fn light_themes_get_a_softer_ladder_than_dark_ones() {
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

    /// The ladder must stay a ladder in both polarities: each tier heavier
    /// than the one below it.
    #[test]
    fn every_ladder_is_monotonic() {
        for (name, e) in [("dark", Elevation::dark()), ("light", Elevation::light())] {
            let steps = [alpha(&e.e0), alpha(&e.e1), alpha(&e.e2), alpha(&e.e3), alpha(&e.e4)];
            for w in steps.windows(2) {
                assert!(w[1] > w[0], "{name}: the ladder must climb, got {steps:?}");
            }
        }
    }

    /// The defect the split exists to prevent, pinned at the shape that
    /// actually broke.
    ///
    /// A widget that fakes a soft shadow with a stack of concentric rings
    /// composites every ring where they overlap, so the weight compounds. With
    /// the dark ladder's alphas on a white card that stack landed on `#A6A6A6`
    /// — a 2.12:1 band, heavier than the card border and heavy enough that a
    /// tab strip read as a solid grey slab. Scaled, the same stack lands near
    /// `#D3D3D3`.
    #[test]
    fn a_stacked_ring_shadow_stays_soft_on_every_light_theme() {
        // The ring alphas `tabs.rs` composes, before the per-theme scale.
        const RINGS: [f32; 4] = [51.0, 30.0, 15.0, 5.0];

        for name in Theme::ALL {
            let t = Theme::named(name).unwrap();
            let scale = t.tokens.elevation.shadow_scale;
            if scale >= 1.0 {
                continue; // dark ladder: black on a dark ground is already soft
            }
            let ground = t.tokens.colors.surfaces.raised;
            let (mut r, mut g, mut b) = (
                ground.to_rgba8().r as f32,
                ground.to_rgba8().g as f32,
                ground.to_rgba8().b as f32,
            );
            for ring in RINGS {
                let a = (ring * scale).round() / 255.0;
                r *= 1.0 - a;
                g *= 1.0 - a;
                b *= 1.0 - a;
            }
            let seen = Color::from_rgb8(r.round() as u8, g.round() as u8, b.round() as u8);
            let weight = contrast_ratio(seen, ground);
            assert!(
                weight < 1.60,
                "{name}: a stacked ring shadow composites to {weight:.2}:1 on the card \
                 — at the unscaled alphas this was 2.12:1 and read as a solid band",
            );
        }
    }
}
