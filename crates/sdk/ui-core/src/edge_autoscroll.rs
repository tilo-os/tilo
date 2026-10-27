//! Edge autoscroll: a drag resting in the band along a viewport's edge
//! scrolls the viewport that way, from nothing at the band's inner side to
//! full speed at the edge and beyond it. Window strips, reorderable lists
//! and trees, block drags and text selections all scroll by this one model.

use crate::time::{Duration, Instant};
use crate::tokens::motion::{
    EDGE_AUTOSCROLL_BAND, EDGE_AUTOSCROLL_MAX_STEP, EDGE_AUTOSCROLL_PX_PER_S,
};

/// How fast, in px/s, a viewport spanning `start..end` on one axis scrolls
/// under a drag at `pointer` on that axis: negative toward `start`, `0`
/// outside the edge bands or when the viewport is too small to hold both.
#[must_use]
pub fn velocity(pointer: f64, start: f64, end: f64) -> f64 {
    if end - start <= EDGE_AUTOSCROLL_BAND * 2.0 {
        return 0.0;
    }
    let into_start = start + EDGE_AUTOSCROLL_BAND - pointer;
    let into_end = pointer - (end - EDGE_AUTOSCROLL_BAND);
    if into_start > 0.0 {
        -EDGE_AUTOSCROLL_PX_PER_S * (into_start / EDGE_AUTOSCROLL_BAND).min(1.0)
    } else if into_end > 0.0 {
        EDGE_AUTOSCROLL_PX_PER_S * (into_end / EDGE_AUTOSCROLL_BAND).min(1.0)
    } else {
        0.0
    }
}

/// [`velocity`] for a drag that was pressed at `press`, over a band `band`
/// deep: it scrolls toward an edge only once the pointer has gone further
/// that way than where it was pressed. A press already inside the band
/// does not scroll by itself, and a drag back inward never scrolls outward.
#[must_use]
pub fn velocity_past_press(pointer: f64, press: f64, start: f64, end: f64, band: f64) -> f64 {
    if end - start <= band * 2.0 {
        return 0.0;
    }
    let inner_start = (start + band).min(press);
    let inner_end = (end - band).max(press);
    if pointer < inner_start {
        -EDGE_AUTOSCROLL_PX_PER_S * ((inner_start - pointer) / band).min(1.0)
    } else if pointer > inner_end {
        EDGE_AUTOSCROLL_PX_PER_S * ((pointer - inner_end) / band).min(1.0)
    } else {
        0.0
    }
}

/// How far `velocity` scrolls over a frame of `elapsed`; a stalled frame
/// counts as at most [`EDGE_AUTOSCROLL_MAX_STEP`], so it does not fling.
#[must_use]
pub fn distance(velocity: f64, elapsed: Duration) -> f64 {
    velocity * elapsed.min(EDGE_AUTOSCROLL_MAX_STEP).as_secs_f64()
}

/// The frame clock of one edge autoscroll: the first frame in a band only
/// starts it, each later one scrolls by the time since the last, and leaving
/// the band stops it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EdgeAutoscroll {
    last: Option<Instant>,
}

impl EdgeAutoscroll {
    /// How far to scroll at `now` for `velocity` ([`velocity`]).
    pub fn advance(&mut self, velocity: f64, now: Instant) -> f64 {
        if velocity.abs() < f64::EPSILON {
            self.last = None;
            return 0.0;
        }
        let elapsed = self
            .last
            .replace(now)
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last));
        distance(velocity, elapsed)
    }

    pub fn stop(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_pressed_in_a_band_scrolls_only_past_where_it_was_pressed() {
        let band = 8.0;
        assert_eq!(velocity_past_press(3.0, 3.0, 0.0, 1000.0, band), 0.0);
        assert_eq!(velocity_past_press(6.0, 3.0, 0.0, 1000.0, band), 0.0);
        let past = velocity_past_press(1.0, 3.0, 0.0, 1000.0, band);
        assert!(
            (past + EDGE_AUTOSCROLL_PX_PER_S / 4.0).abs() < 1e-9,
            "{past}"
        );
        assert_eq!(velocity_past_press(997.0, 997.0, 0.0, 1000.0, band), 0.0);
        assert!(velocity_past_press(999.0, 997.0, 0.0, 1000.0, band) > 0.0);
        // Pressed well inside: the band is the usual one.
        assert_eq!(velocity_past_press(10.0, 500.0, 0.0, 1000.0, band), 0.0);
        assert_eq!(
            velocity_past_press(0.0, 500.0, 0.0, 1000.0, band),
            -EDGE_AUTOSCROLL_PX_PER_S
        );
    }

    #[test]
    fn speed_grows_through_the_band_and_holds_past_the_edge() {
        let band = EDGE_AUTOSCROLL_BAND;
        assert_eq!(velocity(500.0, 0.0, 1000.0), 0.0);
        assert_eq!(velocity(band, 0.0, 1000.0), 0.0);
        let half = velocity(band / 2.0, 0.0, 1000.0);
        assert!((half + EDGE_AUTOSCROLL_PX_PER_S / 2.0).abs() < 1e-9);
        assert_eq!(velocity(0.0, 0.0, 1000.0), -EDGE_AUTOSCROLL_PX_PER_S);
        assert_eq!(velocity(-50.0, 0.0, 1000.0), -EDGE_AUTOSCROLL_PX_PER_S);
        assert_eq!(velocity(1050.0, 0.0, 1000.0), EDGE_AUTOSCROLL_PX_PER_S);
        assert_eq!(
            velocity(1.0, 0.0, band * 2.0),
            0.0,
            "too small for both bands"
        );
    }

    #[test]
    fn the_clock_starts_on_entry_caps_stalls_and_stops_on_exit() {
        let t0 = crate::time::now();
        let mut clock = EdgeAutoscroll::default();
        assert_eq!(clock.advance(100.0, t0), 0.0);
        let step = clock.advance(100.0, t0 + Duration::from_millis(10));
        assert!((step - 1.0).abs() < 1e-9);
        let stalled = clock.advance(100.0, t0 + Duration::from_secs(5));
        assert!((stalled - distance(100.0, EDGE_AUTOSCROLL_MAX_STEP)).abs() < 1e-9);
        assert_eq!(clock.advance(0.0, t0 + Duration::from_secs(6)), 0.0);
        assert_eq!(clock.advance(100.0, t0 + Duration::from_secs(7)), 0.0);
    }
}
