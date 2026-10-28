//! A widget's face, painted once and appended again while it holds.

use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};

/// A face painted at logical coordinates and appended, translated, on later
/// frames while the widget keeps its size: a widget that only moves (a
/// scrolled row, a reflowed toolbar) costs no shaping or encoding.
#[derive(Default)]
pub struct CachedPaint {
    scene: Scene,
    /// Where the face was painted; [`Rect::ZERO`] before the first paint.
    rect: Rect,
}

impl CachedPaint {
    /// Whether nothing is painted yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    /// Whether `rect` has another size than the face was painted at. Exact
    /// on purpose: this detects a change for the cache, it does not compare
    /// numbers that need a tolerance.
    #[must_use]
    pub fn resized(&self, rect: Rect) -> bool {
        rect.width().to_bits() != self.rect.width().to_bits()
            || rect.height().to_bits() != self.rect.height().to_bits()
    }

    /// Append the face to `out` for a widget now at `rect`, translated from
    /// where it was painted.
    pub fn append(&self, out: &mut Scene, transform: Affine, rect: Rect) {
        let moved = Vec2::new(rect.x0 - self.rect.x0, rect.y0 - self.rect.y0);
        out.append(&self.scene, Some(transform.pre_translate(moved)));
    }

    /// The emptied scene to paint the face into, in the coordinates of
    /// `rect`, which later [`Self::append`]s translate from.
    pub fn repaint(&mut self, rect: Rect) -> &mut Scene {
        self.scene.reset();
        self.rect = rect;
        &mut self.scene
    }
}
