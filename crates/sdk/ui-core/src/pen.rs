//! The stylus behind the pointer event a widget is handling.
//!
//! A pen moves the cursor and clicks as a mouse does, so every widget works
//! with one unchanged. A widget that wants more of it — a brush following
//! pressure and tilt, the eraser end — asks
//! [`EventCtx::pen`](crate::widget::EventCtx::pen) while it handles a
//! `CursorMoved` or `MouseInput`.
//!
//! The platform reports a pen as `WindowEvent::PenSample` immediately before
//! each pointer event it makes. Hosts pass every window event through
//! [`PenInput::route`]: it keeps the sample, which no widget sees as an event
//! of its own, and hands it to the pointer event that follows for exactly
//! that event's dispatch. A mouse's events carry none.

use std::cell::Cell;

use winit::event::WindowEvent;

/// What a stylus reported with one pointer event.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PenSample {
    /// How hard the tip is pressed, `0.0..=1.0`: `0.0` while it hovers, and
    /// on a pen that reports no pressure.
    pub pressure: f64,
    /// Tilt in degrees from upright towards the window's positive x and y
    /// axes, each within `-90.0..=90.0`: `(0.0, 0.0)` upright, and on a pen
    /// that reports no tilt.
    pub tilt: (f64, f64),
    /// The eraser end is in use rather than the tip.
    pub eraser: bool,
}

impl PenSample {
    /// The sample a `WindowEvent::PenSample` carries, `None` for any other
    /// event.
    #[must_use]
    pub fn from_event(event: &WindowEvent) -> Option<Self> {
        match *event {
            WindowEvent::PenSample {
                pressure,
                tilt,
                eraser,
                ..
            } => Some(Self {
                pressure,
                tilt,
                eraser,
            }),
            _ => None,
        }
    }
}

thread_local! {
    /// The sample of the pointer event being dispatched on this thread.
    static CURRENT: Cell<Option<PenSample>> = const { Cell::new(None) };
}

/// The stylus behind the pointer event being dispatched, if a stylus made it.
#[must_use]
pub(crate) fn current() -> Option<PenSample> {
    CURRENT.with(Cell::get)
}

/// One host's pen samples on their way to the pointer events they describe.
#[derive(Debug, Default)]
pub struct PenInput {
    pending: Option<PenSample>,
}

impl PenInput {
    /// Route one window event before the host dispatches it. A pen sample is
    /// kept and `None` returned: the host drops the event. Anything else is
    /// dispatched while the returned scope lives; for a `CursorMoved` or
    /// `MouseInput` the kept sample, if any, is the pen widgets see.
    #[must_use]
    pub fn route(&mut self, event: &WindowEvent) -> Option<PenScope> {
        if let Some(sample) = PenSample::from_event(event) {
            self.pending = Some(sample);
            return None;
        }
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        );
        let sample = if pointer { self.pending.take() } else { None };
        Some(PenScope(match sample {
            Some(sample) => {
                Restore::Previous(CURRENT.with(|current| current.replace(Some(sample))))
            }
            None => Restore::Nothing,
        }))
    }
}

/// While alive, the pen of the pointer event being dispatched.
#[must_use]
pub struct PenScope(Restore);

enum Restore {
    /// This event's pen is set; the one before it comes back on drop.
    Previous(Option<PenSample>),
    /// Nothing was set, so that an event re-entering dispatch keeps the
    /// outer event's pen.
    Nothing,
}

impl Drop for PenScope {
    fn drop(&mut self) {
        if let Restore::Previous(previous) = self.0 {
            CURRENT.with(|current| current.set(previous));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;
    use winit::event::DeviceId;

    fn sample(pressure: f64, eraser: bool) -> WindowEvent {
        WindowEvent::PenSample {
            device_id: DeviceId::dummy(),
            pressure,
            tilt: (20.0, -10.0),
            eraser,
        }
    }

    fn moved() -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(4.0, 5.0),
        }
    }

    use crate::synthetic_input::mouse_press;

    /// The pen a widget would see while `event` is dispatched.
    fn seen(input: &mut PenInput, event: &WindowEvent) -> Option<PenSample> {
        let scope = input.route(event).expect("dispatched");
        let pen = current();
        drop(scope);
        pen
    }

    #[test]
    fn a_sample_reaches_exactly_the_pointer_event_after_it() {
        let mut input = PenInput::default();
        assert!(input.route(&sample(0.4, false)).is_none(), "never an event");
        let pen = PenSample {
            pressure: 0.4,
            tilt: (20.0, -10.0),
            eraser: false,
        };
        assert_eq!(seen(&mut input, &moved()), Some(pen));
        assert_eq!(seen(&mut input, &mouse_press()), None, "a mouse's press");
        assert_eq!(current(), None, "nothing outside a dispatch");

        assert!(input.route(&sample(1.0, true)).is_none());
        let unrelated = WindowEvent::Focused(true);
        assert_eq!(
            seen(&mut input, &unrelated),
            None,
            "other events neither see nor spend the sample"
        );
        assert_eq!(
            seen(&mut input, &mouse_press()),
            Some(PenSample {
                pressure: 1.0,
                tilt: (20.0, -10.0),
                eraser: true,
            })
        );
    }

    #[test]
    fn an_event_dispatched_inside_another_keeps_its_pen() {
        let mut input = PenInput::default();
        let _ = input.route(&sample(0.7, false));
        let outer = input.route(&moved()).expect("dispatched");
        // A host that hands the same event on to a second dispatch path.
        let inner = input.route(&moved()).expect("dispatched");
        assert_eq!(current().map(|pen| pen.pressure), Some(0.7));
        drop(inner);
        assert_eq!(current().map(|pen| pen.pressure), Some(0.7));
        drop(outer);
        assert_eq!(current(), None);
    }
}
