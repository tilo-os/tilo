//! Colour that changes across a mark, as a painter mixes a passage's colour
//! while laying it in: a sky lightening toward the horizon, a glow around
//! the moon.

use serde::{Deserialize, Serialize};

use crate::colour::{Rgb, from_linear, to_linear};

/// How a field's colour runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldKind {
    /// Along the line from `from` to `to`.
    Linear,
    /// Outward from `from`, reaching the last stop at the distance of `to`.
    Radial,
}

/// A colour gradient over the canvas, in the coordinates of its mark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColourField {
    pub kind: FieldKind,
    pub from: (f32, f32),
    pub to: (f32, f32),
    /// Offsets `0..=1` in ascending order, with linear colours and how
    /// much paint is laid there, `0..=1`: a stop at zero fades the paint
    /// out, so a glow or a bank of mist dies away into what is beneath.
    pub stops: Vec<(f32, Rgb, f32)>,
}

impl ColourField {
    /// A field through `stops` (offset, colour, alpha), in any order;
    /// `None` with fewer than two.
    pub fn new(
        kind: FieldKind,
        from: (f32, f32),
        to: (f32, f32),
        stops: Vec<(f32, Rgb, f32)>,
    ) -> Option<Self> {
        let mut stops: Vec<(f32, Rgb, f32)> = stops
            .into_iter()
            .filter(|(at, _, alpha)| at.is_finite() && alpha.is_finite())
            .map(|(at, colour, alpha)| (at.clamp(0.0, 1.0), colour, alpha.clamp(0.0, 1.0)))
            .collect();
        if stops.len() < 2 || ![from.0, from.1, to.0, to.1].iter().all(|v| v.is_finite()) {
            return None;
        }
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        Some(Self {
            kind,
            from,
            to,
            stops,
        })
    }

    /// This field with its coordinates multiplied by `scale`.
    #[must_use]
    pub fn scaled(&self, scale: f32) -> Self {
        Self {
            from: (self.from.0 * scale, self.from.1 * scale),
            to: (self.to.0 * scale, self.to.1 * scale),
            ..self.clone()
        }
    }

    /// This field moved by `(dx, dy)` cells.
    #[must_use]
    pub fn translated(&self, dx: f32, dy: f32) -> Self {
        Self {
            from: (self.from.0 + dx, self.from.1 + dy),
            to: (self.to.0 + dx, self.to.1 + dy),
            ..self.clone()
        }
    }

    /// The colour at a position. Stops mix in sRGB, as a painter judges a
    /// gradation by eye, so a dark-to-light run has no muddy middle.
    pub fn at(&self, x: f32, y: f32) -> Rgb {
        self.sample(x, y).0
    }

    /// How much paint is laid at a position, `0..=1`.
    pub fn alpha(&self, x: f32, y: f32) -> f32 {
        self.sample(x, y).1
    }

    /// The colour and alpha at a position. Toward a faded stop the colour
    /// stays that of the paint still being laid.
    pub fn sample(&self, x: f32, y: f32) -> (Rgb, f32) {
        let (dx, dy) = (self.to.0 - self.from.0, self.to.1 - self.from.1);
        let t = match self.kind {
            FieldKind::Linear => {
                let length = dx.mul_add(dx, dy * dy).max(1e-6);
                ((x - self.from.0) * dx + (y - self.from.1) * dy) / length
            }
            FieldKind::Radial => (x - self.from.0).hypot(y - self.from.1) / dx.hypot(dy).max(1e-6),
        };
        let t = t.clamp(0.0, 1.0);
        let mut below = self
            .stops
            .first()
            .copied()
            .unwrap_or((0.0, Rgb::BLACK, 1.0));
        for &stop in &self.stops {
            if stop.0 >= t {
                let span = stop.0 - below.0;
                let share = if span > 1e-6 {
                    (t - below.0) / span
                } else {
                    1.0
                };
                let (from_weight, to_weight) = (below.2 * (1.0 - share), stop.2 * share);
                let alpha = from_weight + to_weight;
                // Weigh each stop's colour by its paint, so fading toward
                // a clear stop does not drift toward that stop's colour.
                let colour_share = if alpha > 1e-6 {
                    to_weight / alpha
                } else {
                    share
                };
                return (mix_srgb(below.1, stop.1, colour_share), alpha);
            }
            below = stop;
        }
        (below.1, below.2)
    }
}

fn mix_srgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let channel = |a: f32, b: f32| {
        let (a, b) = (from_linear(a), from_linear(b));
        to_linear(a + (b - a) * t)
    };
    Rgb::new(channel(a.r, b.r), channel(a.g, b.g), channel(a.b, b.b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linear_field_runs_through_its_stops_and_holds_past_the_ends() {
        let dark = Rgb::from_srgb8(40, 50, 90);
        let light = Rgb::from_srgb8(230, 210, 170);
        let field = ColourField::new(
            FieldKind::Linear,
            (0.0, 0.0),
            (0.0, 100.0),
            vec![(1.0, light, 1.0), (0.0, dark, 1.0)],
        )
        .unwrap();
        assert_eq!(field.at(5.0, -20.0).to_hex(), dark.to_hex());
        assert_eq!(field.at(5.0, 140.0).to_hex(), light.to_hex());
        assert_eq!(field.at(50.0, 50.0).to_hex(), "#878282");
        let radial = ColourField::new(
            FieldKind::Radial,
            (50.0, 50.0),
            (50.0, 60.0),
            vec![(0.0, light, 1.0), (1.0, dark, 1.0)],
        )
        .unwrap();
        assert_eq!(radial.at(50.0, 50.0).to_hex(), light.to_hex());
        assert_eq!(radial.at(80.0, 50.0).to_hex(), dark.to_hex());
        assert!(
            ColourField::new(
                FieldKind::Linear,
                (0.0, 0.0),
                (1.0, 0.0),
                vec![(0.0, dark, 1.0)]
            )
            .is_none()
        );
    }

    /// Toward a clear stop the paint thins out but keeps its own colour.
    #[test]
    fn a_field_fades_out_toward_a_clear_stop() {
        let glow = Rgb::from_srgb8(250, 230, 190);
        let field = ColourField::new(
            FieldKind::Radial,
            (0.0, 0.0),
            (100.0, 0.0),
            vec![(0.0, glow, 1.0), (1.0, Rgb::BLACK, 0.0)],
        )
        .unwrap();
        assert!((field.alpha(0.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((field.alpha(50.0, 0.0) - 0.5).abs() < 1e-3);
        assert!(field.alpha(120.0, 0.0) < 1e-6);
        assert_eq!(field.at(75.0, 0.0).to_hex(), glow.to_hex());
    }
}
