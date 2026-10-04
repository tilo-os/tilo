//! Playback glyphs the shared icon set does not carry yet.
//!
//! Everything else Music shows comes from [`ui::icons`]. Shuffle and the two
//! repeat modes are traced from the same Lucide set (v0.546.0, ISC licensed)
//! on its 24-unit grid and stroked exactly like a [`ui::icons::Icon`]: the
//! theme's 1.5 px line, round caps and joins, centred in the caller's rect.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Join, Rect, Stroke};
use vello::peniko::Color;

/// A playback-mode glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Shuffle,
    Repeat,
    RepeatOne,
}

impl Glyph {
    fn paths(self) -> &'static [&'static str] {
        match self {
            Self::Shuffle => &[
                "m18 14 4 4-4 4",
                "m18 2 4 4-4 4",
                "M2 18h1.4c1.3 0 2.5-.6 3.3-1.7l6.1-8.6c.7-1.1 2-1.7 3.3-1.7H22",
                "M2 6h1.9c1.5 0 2.9.9 3.6 2.2",
                "M22 18h-5.9c-1.3 0-2.6-.7-3.3-1.8l-.5-.8",
            ],
            Self::Repeat => &[
                "m17 2 4 4-4 4",
                "M3 11v-1a4 4 0 0 1 4-4h14",
                "m7 22-4-4 4-4",
                "M21 13v1a4 4 0 0 1-4 4H3",
            ],
            Self::RepeatOne => &[
                "m17 2 4 4-4 4",
                "M3 11v-1a4 4 0 0 1 4-4h14",
                "m7 22-4-4 4-4",
                "M21 13v1a4 4 0 0 1-4 4H3",
                "M11 10h1v4",
            ],
        }
    }

    /// Stroke the glyph centred in `rect` at the shared icon line width.
    pub fn draw(self, scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
        let side = rect.width().min(rect.height());
        if side <= 0.0 {
            return;
        }
        let scale = side / ui::icons::LUCIDE_VIEWBOX;
        let origin = (
            rect.x0 + (rect.width() - side) / 2.0,
            rect.y0 + (rect.height() - side) / 2.0,
        );
        let local = transform * Affine::translate(origin) * Affine::scale(scale);
        let line = Stroke::new(ui::icons::LUCIDE_DEFAULT_STROKE_PX / scale)
            .with_caps(Cap::Round)
            .with_join(Join::Round);
        let mut path = BezPath::new();
        for d in self.paths() {
            if let Ok(sub) = BezPath::from_svg(d) {
                path.extend(sub);
            }
        }
        scene.stroke(&line, local, color, None, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_parses_into_a_drawable_path() {
        for glyph in [Glyph::Shuffle, Glyph::Repeat, Glyph::RepeatOne] {
            for d in glyph.paths() {
                let path = BezPath::from_svg(d).expect("lucide path literal parses");
                assert!(path.elements().len() > 1, "{glyph:?} has a stroke");
            }
        }
        assert_ne!(
            Glyph::Repeat.paths().len(),
            Glyph::RepeatOne.paths().len(),
            "repeat-one carries its own mark, so the two modes read apart"
        );
    }
}
