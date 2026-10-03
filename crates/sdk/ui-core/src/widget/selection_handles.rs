//! Selection handles for a vector canvas: the frame around what is
//! selected, its resize handles at the corners and its rotation handle above,
//! drawn in the theme's accent and hit-tested at a constant size on screen
//! whatever the zoom.
//!
//! The frame owns no document state. A canvas keeps its selection, asks
//! [`SelectionHandles::handle_at`] what a press landed on, and turns the drag
//! into its own action with [`SelectionHandles::scale_for`] or
//! [`SelectionHandles::rotation_for`], so every edit still goes through the
//! canvas's published vocabulary.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, Stroke, Vec2};
use vello::peniko::Fill;

use crate::theme::Theme;
use crate::tokens::Metrics;

/// A grip on the selection frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
    /// Above the frame: drag around the centre to rotate.
    Rotate,
}

impl Handle {
    pub const CORNERS: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomRight,
        Self::BottomLeft,
    ];

    /// The corner a resize keeps still: the one opposite this handle.
    pub fn anchor(self, bounds: Rect) -> Point {
        match self {
            Self::TopLeft => Point::new(bounds.x1, bounds.y1),
            Self::TopRight => Point::new(bounds.x0, bounds.y1),
            Self::BottomRight => Point::new(bounds.x0, bounds.y0),
            Self::BottomLeft => Point::new(bounds.x1, bounds.y0),
            Self::Rotate => bounds.center(),
        }
    }
}

/// The frame around a selection, in content coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionHandles {
    pub bounds: Rect,
    /// The pointer target around each handle, in screen pixels.
    target: f64,
}

impl SelectionHandles {
    /// The frame around `bounds`, its handles as easy to hit as any control
    /// under `metrics`.
    pub fn new(bounds: Rect, metrics: &Metrics) -> Self {
        Self {
            bounds,
            target: f64::from(metrics.min_pointer_target),
        }
    }

    /// How big a handle is drawn on screen; its target is larger.
    fn grip(&self) -> f64 {
        self.target * 0.3
    }

    /// How far above the frame the rotation handle sits on screen: one
    /// target, so its target never overlaps a corner's.
    fn rotate_reach(&self) -> f64 {
        self.target
    }

    /// Where `handle` sits in content coordinates at `zoom` (screen pixels
    /// per content unit).
    pub fn position(&self, handle: Handle, zoom: f64) -> Point {
        let b = self.bounds;
        match handle {
            Handle::TopLeft => Point::new(b.x0, b.y0),
            Handle::TopRight => Point::new(b.x1, b.y0),
            Handle::BottomRight => Point::new(b.x1, b.y1),
            Handle::BottomLeft => Point::new(b.x0, b.y1),
            Handle::Rotate => Point::new(b.center().x, b.y0 - self.rotate_reach() / zoom.max(1e-6)),
        }
    }

    /// The handle under `point` (content coordinates), if any.
    pub fn handle_at(&self, point: Point, zoom: f64) -> Option<Handle> {
        let reach = self.target / 2.0 / zoom.max(1e-6);
        std::iter::once(Handle::Rotate)
            .chain(Handle::CORNERS)
            .find(|handle| self.position(*handle, zoom).distance(point) <= reach)
    }

    /// The uniform scale a drag of `handle` from `from` to `to` asks for,
    /// about the opposite corner, and that corner. Never below a tenth.
    pub fn scale_for(&self, handle: Handle, from: Point, to: Point) -> (f64, Point) {
        let anchor = handle.anchor(self.bounds);
        let before = from.distance(anchor).max(1e-6);
        ((to.distance(anchor) / before).max(0.1), anchor)
    }

    /// The rotation in degrees, clockwise, a drag of the rotation handle from
    /// `from` to `to` asks for about the centre; with `snap`, in steps of 15.
    pub fn rotation_for(&self, from: Point, to: Point, snap: bool) -> f64 {
        let centre = self.bounds.center();
        let angle = |point: Point| (point.y - centre.y).atan2(point.x - centre.x);
        let degrees = (angle(to) - angle(from)).to_degrees();
        let degrees = (degrees + 540.0).rem_euclid(360.0) - 180.0;
        if snap {
            (degrees / 15.0).round() * 15.0
        } else {
            degrees
        }
    }

    /// Draw the frame and its handles; `to_screen` maps content to screen.
    pub fn draw(&self, scene: &mut Scene, to_screen: Affine, zoom: f64, theme: &Theme) {
        let zoom = zoom.max(1e-6);
        let accent = theme.tokens.colors.intent.you;
        let surface = theme.tokens.colors.surfaces.base;
        let line = Stroke::new(1.5 / zoom);
        scene.stroke(&line, to_screen, accent, None, &self.bounds);
        let top = Point::new(self.bounds.center().x, self.bounds.y0);
        let rotate = self.position(Handle::Rotate, zoom);
        scene.stroke(&line, to_screen, accent, None, &Line::new(top, rotate));
        let grip = self.grip() / zoom;
        for handle in Handle::CORNERS {
            let at = self.position(handle, zoom);
            let square = Rect::from_center_size(at, (grip, grip));
            scene.fill(Fill::NonZero, to_screen, surface, None, &square);
            scene.stroke(&line, to_screen, accent, None, &square);
        }
        let knob = Circle::new(rotate, grip * 0.6);
        scene.fill(Fill::NonZero, to_screen, surface, None, &knob);
        scene.stroke(&line, to_screen, accent, None, &knob);
    }

    /// The frame moved by `offset`, as during a drag.
    #[must_use]
    pub fn moved(&self, offset: Vec2) -> Self {
        Self {
            bounds: self.bounds + offset,
            ..*self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_hit_at_constant_screen_size_and_turn_drags_into_edits() {
        let bounds = Rect::new(100.0, 100.0, 200.0, 150.0);
        let frame = SelectionHandles::new(bounds, &Metrics::BASE);
        assert_eq!(
            frame.handle_at(Point::new(201.0, 151.0), 1.0),
            Some(Handle::BottomRight)
        );
        // The target is the theme's minimum pointer target, and grows with
        // its UI scale.
        let edge = Point::new(
            200.0 + f64::from(Metrics::BASE.min_pointer_target) * 0.7,
            150.0,
        );
        assert_eq!(frame.handle_at(edge, 1.0), None);
        let doubled = SelectionHandles::new(bounds, &Metrics::BASE.scaled(2.0));
        assert_eq!(doubled.handle_at(edge, 1.0), Some(Handle::BottomRight));
        assert_eq!(frame.handle_at(Point::new(150.0, 125.0), 1.0), None);
        // Zoomed out 4x, a handle reaches four times further in content.
        assert_eq!(
            frame.handle_at(Point::new(140.0, 100.0), 0.25),
            Some(Handle::TopLeft)
        );
        assert_eq!(
            frame.handle_at(Point::new(150.0, 72.0), 1.0),
            Some(Handle::Rotate)
        );

        let (scale, anchor) = frame.scale_for(
            Handle::BottomRight,
            Point::new(200.0, 150.0),
            Point::new(300.0, 200.0),
        );
        assert_eq!(anchor, Point::new(100.0, 100.0));
        assert!((scale - 2.0).abs() < 1e-9);

        let quarter = frame.rotation_for(Point::new(150.0, 72.0), Point::new(250.0, 125.0), true);
        assert!((quarter - 90.0).abs() < 1e-9, "{quarter}");
    }
}
