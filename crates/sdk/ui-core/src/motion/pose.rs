//! [`Pose`]: how a moving surface is drawn this frame: an opacity and a
//! transform in its own coordinates.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

/// Below this opacity a surface is not drawn at all.
const INVISIBLE: f32 = 1.0 / 512.0;

/// An opacity and a transform (in the surface's own coordinates, applied
/// before the surface's placement).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub opacity: f32,
    pub transform: Affine,
}

impl Default for Pose {
    fn default() -> Self {
        Self::REST
    }
}

impl Pose {
    /// Fully shown, where it rests.
    pub const REST: Self = Self {
        opacity: 1.0,
        transform: Affine::IDENTITY,
    };

    /// Whether this is the resting pose: nothing to undo, no layer needed.
    #[must_use]
    pub fn is_rest(&self) -> bool {
        self.opacity >= 1.0 && self.transform == Affine::IDENTITY
    }

    /// Whether anything of it shows.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.opacity > INVISIBLE
    }

    /// The pose with its opacity multiplied by `factor`.
    #[must_use]
    pub fn fade(mut self, factor: f32) -> Self {
        self.opacity *= factor.clamp(0.0, 1.0);
        self
    }

    /// The transform to draw the surface with, placed by `placement`.
    #[must_use]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine * Affine is kurbo's float matrix product: it cannot overflow or panic"
    )]
    pub fn applied_to(&self, placement: Affine) -> Affine {
        placement * self.transform
    }

    /// Where a point drawn at `point` lies in the surface's resting layout:
    /// what hit testing uses while the surface moves, so the control under
    /// the pointer is the one drawn under it.
    #[must_use]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine * Point is kurbo's float matrix product: it cannot overflow or panic"
    )]
    pub fn to_local(&self, point: Point) -> Point {
        if self.transform.determinant().abs() <= f64::EPSILON {
            return point;
        }
        self.transform.inverse() * point
    }

    /// Where a point of the resting layout is drawn.
    #[must_use]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine * Point is kurbo's float matrix product: it cannot overflow or panic"
    )]
    pub fn to_drawn(&self, point: Point) -> Point {
        self.transform * point
    }

    /// The bounds `rect` is drawn in.
    #[must_use]
    pub fn drawn_rect(&self, rect: Rect) -> Rect {
        self.transform.transform_rect_bbox(rect)
    }

    /// Draw a surface in this pose. `placement` is where the caller would draw
    /// it at rest; `bounds` (in the surface's coordinates) is everything it
    /// paints, shadows included, which the opacity layer clips to. `paint`
    /// gets the transform to draw with. Nothing is drawn when invisible, and
    /// no layer is pushed at full opacity.
    pub fn paint(
        &self,
        scene: &mut Scene,
        placement: Affine,
        bounds: Rect,
        paint: impl FnOnce(&mut Scene, Affine),
    ) {
        if !self.is_visible() {
            return;
        }
        let xform = self.applied_to(placement);
        if self.opacity >= 1.0 {
            paint(scene, xform);
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            self.opacity,
            xform,
            &bounds,
        );
        paint(scene, xform);
        scene.pop_layer();
    }
}
