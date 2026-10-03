//! Where the desktop magnifier looks, and how it maps the scene.
//!
//! The magnifier follows whichever of the pointer and the keyboard focus moved last: someone
//! tabbing through a form without touching the mouse sees the control they reached, and moving
//! the mouse hands the view back to the pointer. Both views scale about that point, so the point
//! itself stays where it really is on screen and whatever is under the pointer remains exactly
//! clickable.

use std::time::Instant;

use vello::kurbo::{Affine, Point, Rect};

/// The lens's largest size; smaller viewports get [`LENS_VIEWPORT_FRACTION`] of their size.
pub const LENS_SIZE: (f64, f64) = (420.0, 280.0);
/// The largest share of the viewport the lens may cover.
pub const LENS_VIEWPORT_FRACTION: f64 = 0.7;

/// Tracks the pointer and the keyboard focus rectangle and picks the magnifier's centre.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FocusTracker {
    pointer: Option<(Point, Instant)>,
    focus: Option<(Rect, Instant)>,
}

impl FocusTracker {
    /// Record what the pointer and the keyboard focus look like at `now`. Only a change counts as
    /// movement: a pointer resting where it was does not take the view back from the focus.
    pub fn observe(&mut self, pointer: Option<Point>, focus: Option<Rect>, now: Instant) {
        match (pointer, self.pointer) {
            (Some(p), Some((seen, _))) if p == seen => {}
            (Some(p), _) => self.pointer = Some((p, now)),
            (None, _) => self.pointer = None,
        }
        match (focus, self.focus) {
            (Some(r), Some((seen, _))) if r == seen => {}
            (Some(r), _) if r.area() > 0.0 => self.focus = Some((r, now)),
            _ => self.focus = None,
        }
    }

    /// The point to magnify: the focus centre when the focus moved after the pointer last did,
    /// otherwise the pointer.
    pub fn centre(&self) -> Option<Point> {
        match (self.pointer, self.focus) {
            (Some((_, pointer_at)), Some((focus, focus_at))) if focus_at > pointer_at => {
                Some(focus.center())
            }
            (Some((pointer, _)), _) => Some(pointer),
            (None, Some((focus, _))) => Some(focus.center()),
            (None, None) => None,
        }
    }
}

/// Scale the scene by `zoom` about `centre`, which stays fixed on screen.
pub fn magnify(centre: Point, zoom: f64) -> Affine {
    Affine::scale_about(zoom, centre)
}

/// The lens's rectangle around `centre` in a viewport of `viewport` size.
pub fn lens_rect(centre: Point, viewport: (f64, f64)) -> Rect {
    let w = LENS_SIZE.0.min(viewport.0 * LENS_VIEWPORT_FRACTION);
    let h = LENS_SIZE.1.min(viewport.1 * LENS_VIEWPORT_FRACTION);
    Rect::from_center_size(centre, (w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_lens_follows_the_keyboard_focus_until_the_pointer_moves() {
        let t0 = Instant::now();
        let mut tracker = FocusTracker::default();
        let pointer = Point::new(100.0, 100.0);
        let button = Rect::new(400.0, 300.0, 500.0, 340.0);
        tracker.observe(Some(pointer), None, t0);
        assert_eq!(tracker.centre(), Some(pointer));

        tracker.observe(Some(pointer), Some(button), t0 + Duration::from_millis(10));
        assert_eq!(
            tracker.centre(),
            Some(button.center()),
            "Tab moved focus while the pointer rested"
        );

        let next_field = Rect::new(400.0, 360.0, 700.0, 400.0);
        tracker.observe(
            Some(pointer),
            Some(next_field),
            t0 + Duration::from_millis(20),
        );
        assert_eq!(tracker.centre(), Some(next_field.center()));

        let moved = Point::new(120.0, 90.0);
        tracker.observe(
            Some(moved),
            Some(next_field),
            t0 + Duration::from_millis(30),
        );
        assert_eq!(
            tracker.centre(),
            Some(moved),
            "moving the mouse takes the view back"
        );
    }

    #[test]
    fn without_a_pointer_the_focus_leads_and_empty_focus_is_ignored() {
        let t0 = Instant::now();
        let mut tracker = FocusTracker::default();
        assert_eq!(tracker.centre(), None);
        tracker.observe(None, Some(Rect::new(0.0, 0.0, 20.0, 10.0)), t0);
        assert_eq!(tracker.centre(), Some(Point::new(10.0, 5.0)));
        tracker.observe(Some(Point::new(3.0, 4.0)), Some(Rect::ZERO), t0);
        assert_eq!(tracker.centre(), Some(Point::new(3.0, 4.0)));
    }

    #[test]
    fn magnifying_keeps_the_centre_in_place() {
        let centre = Point::new(300.0, 200.0);
        let transform = magnify(centre, 3.0);
        assert_eq!(transform * centre, centre);
        assert_eq!(
            transform * Point::new(310.0, 200.0),
            Point::new(330.0, 200.0)
        );
    }

    #[test]
    fn the_lens_shrinks_on_small_viewports() {
        let lens = lens_rect(Point::new(100.0, 100.0), (400.0, 300.0));
        assert_eq!(lens.width(), 280.0);
        assert_eq!(lens.height(), 210.0);
        assert_eq!(lens.center(), Point::new(100.0, 100.0));
    }
}
