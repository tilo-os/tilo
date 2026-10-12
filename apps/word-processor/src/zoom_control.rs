//! The page scale: the local zoom preference and the transform it gives the
//! page. The control itself is the status band's zoom field.

use vello::kurbo::{Affine, Point};

pub(crate) const MIN: f64 = 0.4;
pub(crate) const MAX: f64 = 3.0;
pub(crate) const STEP: f64 = 0.1;

/// Owns the local zoom factor and the point the page scales about.
pub(crate) struct ZoomControl {
    factor: f64,
    anchor: Point,
}

impl ZoomControl {
    pub(crate) fn new() -> Self {
        Self {
            factor: 1.0,
            anchor: Point::ZERO,
        }
    }

    pub(crate) fn factor(&self) -> f64 {
        self.factor
    }

    pub(crate) fn anchor(&self) -> Point {
        self.anchor
    }

    pub(crate) fn set_anchor(&mut self, anchor: Point) {
        self.anchor = anchor;
    }

    pub(crate) fn set(&mut self, factor: f64) -> bool {
        let factor = factor.clamp(MIN, MAX);
        if (factor - self.factor).abs() <= 1e-6 {
            return false;
        }
        self.factor = factor;
        true
    }

    pub(crate) fn step(&mut self, delta: f64) -> bool {
        self.set(((self.factor + delta) * 10.0).round() / 10.0)
    }

    pub(crate) fn transform(&self, base: Affine) -> Affine {
        base * Affine::scale_about(self.factor, self.anchor)
    }

    pub(crate) fn screen_to_content(&self, point: Point) -> Point {
        Point::new(
            self.anchor.x + (point.x - self.anchor.x) / self.factor,
            self.anchor.y + (point.y - self.anchor.y) / self.factor,
        )
    }

    #[cfg(test)]
    pub(crate) fn content_x_to_screen(&self, x: f64) -> f64 {
        self.anchor.x + (x - self.anchor.x) * self.factor
    }

    #[cfg(test)]
    pub(crate) fn screen_x_to_content(&self, x: f64) -> f64 {
        self.anchor.x + (x - self.anchor.x) / self.factor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scale is clamped with the anchor fixed, and coordinate conversion round-trips.
    #[test]
    fn zoom_scale_is_clamped_about_a_fixed_anchor_and_converts_back() {
        // Phase: scale_is_clamped_and_anchor_is_fixed.
        {
            let mut zoom = ZoomControl::new();
            zoom.set_anchor(Point::new(50.0, 20.0));
            zoom.set(99.0);
            assert_eq!(zoom.factor(), MAX);
            assert_eq!(
                zoom.transform(Affine::IDENTITY) * zoom.anchor(),
                zoom.anchor()
            );
            zoom.set(0.0);
            assert_eq!(zoom.factor(), MIN);
        }

        // Phase: coordinate_conversion_round_trips.
        {
            let mut zoom = ZoomControl::new();
            zoom.set_anchor(Point::new(100.0, 0.0));
            zoom.set(1.7);
            let content = 240.0;
            let screen = zoom.content_x_to_screen(content);
            assert!((zoom.screen_x_to_content(screen) - content).abs() < 1e-9);
        }
    }
}
