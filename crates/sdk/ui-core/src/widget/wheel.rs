//! How a wheel's motion is read: sideways travel and the gesture that a
//! trackpad's fingers make. The desktop's window strip and the app under the
//! pointer read a wheel by these one rules.

use winit::event::MouseScrollDelta;

/// A wheel event's sideways travel in logical pixels when horizontal motion
/// dominates, positive for a wheel turned (or fingers scrolling) rightward,
/// which scrolls content toward its later columns; `None` for vertical
/// motion. Winit reports pixels physical, so they are divided by
/// `scale_factor`, and a line is a notch of the wheel's own metric. The
/// desktop's window strip and the app under the pointer read a wheel by this
/// one rule.
pub fn sideways_travel(delta: &MouseScrollDelta, scale_factor: f64) -> Option<f64> {
    let line = line_of_wheel();
    let (dx, dy) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * line, f64::from(*y) * line),
        MouseScrollDelta::PixelDelta(p) => {
            let scale = scale_factor.max(f64::EPSILON);
            (p.x / scale, p.y / scale)
        }
    };
    (dx.abs() > dy.abs()).then_some(-dx)
}

/// Logical pixels one notch of a line-stepping wheel scrolls, the metrics
/// token that weighs a line against a touchpad's pixels.
pub fn line_of_wheel() -> f64 {
    f64::from(crate::tokens::Metrics::BASE.wheel_line)
}

pub const WHEEL_GESTURE_GAP: crate::time::Duration = crate::time::Duration::from_millis(200);

/// Touch phases for a wheel whose platform reports none: a browser's wheel
/// events carry no phase, so winit hands every trackpad delta over as
/// `Moved`, and a gesture that never ends leaves a scroll view stretched
/// past its end through the rubber band for good, holding the wheel it
/// should hand to the view around it. The first pixel delta after a pause
/// begins a gesture ([`Self::observe`] makes it `Started`), and a pause of
/// [`WHEEL_GESTURE_GAP`] ends it ([`Self::end_if_due`]), as a native
/// trackpad's lift does. A line delta (a mouse wheel's notch) is no gesture.
#[derive(Debug, Default)]
pub struct PhaselessWheel {
    last: Option<(winit::event::DeviceId, crate::time::Instant)>,
}

impl PhaselessWheel {
    /// Read a wheel `event` arriving at `now`: a pixel delta without a
    /// phase of its own (`Moved`) begins a gesture when none runs, and
    /// continues it otherwise. Anything else passes unchanged.
    pub fn observe(&mut self, event: &mut winit::event::WindowEvent, now: crate::time::Instant) {
        use winit::event::{TouchPhase, WindowEvent};
        let WindowEvent::MouseWheel {
            device_id,
            delta: MouseScrollDelta::PixelDelta(_),
            phase,
        } = event
        else {
            return;
        };
        if *phase != TouchPhase::Moved {
            return;
        }
        if self.deadline().is_none_or(|due| now >= due) {
            *phase = TouchPhase::Started;
        }
        self.last = Some((*device_id, now));
    }

    /// When the gesture running ends unless another delta comes first.
    #[must_use]
    pub fn deadline(&self) -> Option<crate::time::Instant> {
        self.last
            .and_then(|(_, at)| at.checked_add(WHEEL_GESTURE_GAP))
    }

    pub fn end_if_due(&mut self, now: crate::time::Instant) -> Option<winit::event::WindowEvent> {
        if self.deadline().is_none_or(|due| now < due) {
            return None;
        }
        let (device_id, _) = self.last.take()?;
        Some(winit::event::WindowEvent::MouseWheel {
            device_id,
            delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, 0.0)),
            phase: winit::event::TouchPhase::Ended,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{DeviceId, TouchPhase, WindowEvent};

    fn pixels(y: f64) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, y)),
            phase: TouchPhase::Moved,
        }
    }

    fn phase(event: &WindowEvent) -> TouchPhase {
        match event {
            WindowEvent::MouseWheel { phase, .. } => *phase,
            other => panic!("not a wheel: {other:?}"),
        }
    }

    #[test]
    fn a_browsers_trackpad_swipe_begins_runs_and_ends_after_its_pause() {
        let mut wheel = PhaselessWheel::default();
        let start = crate::time::now();
        let mut first = pixels(-40.0);
        wheel.observe(&mut first, start);
        assert_eq!(phase(&first), TouchPhase::Started);
        let mut next = pixels(-40.0);
        let later = start + crate::time::Duration::from_millis(16);
        wheel.observe(&mut next, later);
        assert_eq!(phase(&next), TouchPhase::Moved);
        assert!(
            wheel
                .end_if_due(later + crate::time::Duration::from_millis(100))
                .is_none()
        );
        let due = wheel.deadline().expect("a gesture runs");
        let end = wheel.end_if_due(due).expect("the pause ends it");
        assert_eq!(phase(&end), TouchPhase::Ended);
        assert!(wheel.end_if_due(due).is_none(), "ended once");
        assert!(wheel.deadline().is_none());
        let mut again = pixels(40.0);
        wheel.observe(&mut again, due);
        assert_eq!(
            phase(&again),
            TouchPhase::Started,
            "the next swipe begins anew"
        );
    }

    #[test]
    fn a_notch_or_a_phased_trackpad_is_left_as_it_is() {
        let mut wheel = PhaselessWheel::default();
        let now = crate::time::now();
        let mut notch = WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: TouchPhase::Moved,
        };
        wheel.observe(&mut notch, now);
        assert_eq!(phase(&notch), TouchPhase::Moved);
        let mut lift = WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, 0.0)),
            phase: TouchPhase::Ended,
        };
        wheel.observe(&mut lift, now);
        assert_eq!(phase(&lift), TouchPhase::Ended);
        assert!(wheel.deadline().is_none());
    }
}
