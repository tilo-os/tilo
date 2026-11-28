//! A widget's face, painted once and appended again while it holds.

use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};

/// A face painted at logical coordinates and appended, translated, on later
/// frames while the widget keeps its size: a widget that only moves (a
/// scrolled row, a reflowed toolbar) costs no shaping or encoding.
#[derive(Default)]
pub struct CachedPaint {
    scene: Scene,
    rect: Rect,
    /// The display's device scale it was painted for: its text stands on that grid.
    grid: f32,
}

impl CachedPaint {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    /// Whether `rect` has another size than the face was painted at, or the display another
    /// scale. Exact on purpose: this detects a change for the cache, it does not compare
    /// numbers that need a tolerance.
    #[must_use]
    pub fn resized(&self, rect: Rect) -> bool {
        rect.width().to_bits() != self.rect.width().to_bits()
            || rect.height().to_bits() != self.rect.height().to_bits()
            || self.grid.to_bits() != crate::widget::layout::device_scale().to_bits()
    }

    pub fn append(&self, out: &mut Scene, transform: Affine, rect: Rect) {
        let moved = Vec2::new(rect.x0 - self.rect.x0, rect.y0 - self.rect.y0);
        out.append(&self.scene, Some(transform.pre_translate(moved)));
    }

    pub fn scene_mut(&mut self) -> &mut Scene {
        &mut self.scene
    }

    pub fn repaint(&mut self, rect: Rect) -> &mut Scene {
        self.scene.reset();
        self.rect = rect;
        self.grid = crate::widget::layout::device_scale();
        &mut self.scene
    }
}
