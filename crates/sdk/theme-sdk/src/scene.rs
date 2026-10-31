//! What a painter returns: a scene of paths and brushes, and the shape that
//! takes the pointer.

use kurbo::{Affine, BezPath, Point};
use peniko::Color;

/// A colour stop of a gradient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub offset: f32,
    pub color: Color,
}

/// A small picture a painter tiles a fill with: an ordered-dither
/// threshold pattern, a hatch, a pixel texture. Straight-alpha RGBA8, rows
/// top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct PartImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl PartImage {
    /// Largest edge a pattern may have, in texels. A pattern is a tile;
    /// pictures are assets.
    pub const MAX_EDGE: u32 = 256;

    /// Whether the pixels fill the stated size and the size is a tile's.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= Self::MAX_EDGE
            && self.height <= Self::MAX_EDGE
            && u64::from(self.width)
                .checked_mul(u64::from(self.height))
                .and_then(|texels| texels.checked_mul(4))
                .and_then(|len| usize::try_from(len).ok())
                .is_some_and(|len| len == self.rgba.len())
    }
}

/// How the backdrop is bent where a part shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lens {
    /// Displacement at the shape's rim, logical pixels, easing to none a
    /// rim's width in: a thick lens. `0` shows the backdrop straight.
    pub refraction: f32,
}

/// What a path is filled or stroked with.
#[derive(Clone, Debug, PartialEq)]
pub enum Brush {
    Solid(Color),
    /// A linear gradient from `start` to `end`, in part coordinates.
    Linear {
        start: Point,
        end: Point,
        stops: Vec<Stop>,
    },
    /// A radial gradient about `center`, `radius` logical pixels.
    Radial {
        center: Point,
        radius: f32,
        stops: Vec<Stop>,
    },
    /// `image` repeated, each texel `scale` logical pixels square: a
    /// dither, a hatch, pixel art. Drawn without smoothing.
    Pattern {
        image: PartImage,
        scale: f32,
    },
    /// What lies below the part, blurred by `blur` logical pixels and bent
    /// by `lens`, where the toolkit has a backdrop pass; `fallback` where it
    /// has not (no pass, reduced transparency, a capture). The toolkit then
    /// holds what shows inside the band around the part's fill
    /// (`PartTokens::fill`, the tint a painter lays over it), so ink laid on
    /// it keeps its contrast over any backdrop; the painter cannot skip
    /// that.
    Backdrop {
        blur: f32,
        lens: Lens,
        fallback: Color,
    },
}

/// One drawing operation, in part coordinates (origin at the part's top
/// left, logical pixels).
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Fill {
        path: BezPath,
        brush: Brush,
        /// Maps the brush's coordinates into the part's: moves a pattern or
        /// a gradient without moving the path.
        brush_transform: Affine,
    },
    Stroke {
        path: BezPath,
        width: f32,
        brush: Brush,
    },
    /// Everything up to the matching [`Op::PopLayer`] is clipped to `clip`
    /// and drawn at `alpha`.
    PushLayer {
        clip: BezPath,
        alpha: f32,
    },
    PopLayer,
}

/// A part, drawn: operations in paint order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartScene {
    pub ops: Vec<Op>,
}

impl PartScene {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fill `path` with `brush`.
    pub fn fill(&mut self, path: impl Into<BezPath>, brush: Brush) -> &mut Self {
        self.ops.push(Op::Fill {
            path: path.into(),
            brush,
            brush_transform: Affine::IDENTITY,
        });
        self
    }

    /// Fill `path` with `brush` placed by `brush_transform`.
    pub fn fill_placed(
        &mut self,
        path: impl Into<BezPath>,
        brush: Brush,
        brush_transform: Affine,
    ) -> &mut Self {
        self.ops.push(Op::Fill {
            path: path.into(),
            brush,
            brush_transform,
        });
        self
    }

    /// Stroke `path`, `width` logical pixels wide, centred on it.
    pub fn stroke(&mut self, path: impl Into<BezPath>, width: f32, brush: Brush) -> &mut Self {
        self.ops.push(Op::Stroke {
            path: path.into(),
            width,
            brush,
        });
        self
    }

    /// Draw what `layer` draws clipped to `clip`, at `alpha`.
    pub fn layer(
        &mut self,
        clip: impl Into<BezPath>,
        alpha: f32,
        layer: impl FnOnce(&mut Self),
    ) -> &mut Self {
        self.ops.push(Op::PushLayer {
            clip: clip.into(),
            alpha,
        });
        layer(self);
        self.ops.push(Op::PopLayer);
        self
    }

    /// Whether any fill shows the backdrop. Such a part is drawn every
    /// frame (what is below it moves), so the toolkit does not cache it.
    #[must_use]
    pub fn samples_backdrop(&self) -> bool {
        self.ops.iter().any(|op| match op {
            Op::Fill { brush, .. } | Op::Stroke { brush, .. } => {
                matches!(brush, Brush::Backdrop { .. })
            }
            Op::PushLayer { .. } | Op::PopLayer => false,
        })
    }
}

/// A part as its painter drew it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaintedPart {
    pub scene: PartScene,
    /// Where the part takes the pointer, in part coordinates: the painted
    /// shape. A triangle button takes clicks inside its triangle only; the
    /// focus ring and the accessibility bounds follow it. `None`: the
    /// part's rect.
    pub hit: Option<BezPath>,
}

impl PaintedPart {
    /// A part drawn by `scene` that takes the pointer in its rect.
    #[must_use]
    pub fn new(scene: PartScene) -> Self {
        Self { scene, hit: None }
    }

    /// This part, taking the pointer only inside `shape`.
    #[must_use]
    pub fn with_hit(mut self, shape: impl Into<BezPath>) -> Self {
        self.hit = Some(shape.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::{Rect, Shape};

    #[test]
    fn a_scene_samples_the_backdrop_only_where_a_fill_shows_it() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0).to_path(0.1);
        let mut scene = PartScene::new();
        scene.fill(rect.clone(), Brush::Solid(Color::BLACK));
        assert!(!scene.samples_backdrop());
        scene.layer(rect.clone(), 0.5, |inner| {
            inner.fill(
                rect.clone(),
                Brush::Backdrop {
                    blur: 8.0,
                    lens: Lens::default(),
                    fallback: Color::WHITE,
                },
            );
        });
        assert!(scene.samples_backdrop());
        assert!(matches!(scene.ops.last(), Some(Op::PopLayer)));
    }

    #[test]
    fn a_pattern_is_a_tile_whose_pixels_fill_its_size() {
        let tile = |width, height, len| PartImage {
            width,
            height,
            rgba: vec![0; len],
        };
        assert!(tile(4, 4, 64).is_well_formed());
        assert!(!tile(4, 4, 63).is_well_formed());
        assert!(!tile(0, 4, 0).is_well_formed());
        assert!(!tile(512, 1, 2048).is_well_formed());
    }
}
