//! The input serial of the person's press this window is handling, once it handled one.
//!
//! The compositor issues a serial with every input event it delivers, and the window reads it
//! with the event. An invocation a control makes is that press's: it names the serial, and the
//! host admits it as the person's own when the compositor issued that serial to this window. An
//! app cannot mint one, and the serial names no authority of its own: only the compositor's
//! record of the press it delivered, which the session hands the device's daemon, makes it count.
//!
//! The serial is kept per thread: the window's events, and the controls that answer them, are
//! that thread's.

use std::cell::Cell;

thread_local! {
    static NEWEST: Cell<Option<u32>> = const { Cell::new(None) };
}

/// The window is handling a press delivered with `serial`: an invocation made while it handles
/// it names it. Every other event leaves the press it follows where it was.
pub fn note_event(event: &winit::event::WindowEvent, serial: u32) {
    use winit::event::{ElementState, TouchPhase, WindowEvent};
    let press = match event {
        WindowEvent::MouseInput { state, .. } => *state == ElementState::Pressed,
        WindowEvent::KeyboardInput { event, .. } => event.state == ElementState::Pressed,
        WindowEvent::Touch(touch) => touch.phase == TouchPhase::Started,
        _ => false,
    };
    if press {
        NEWEST.with(|newest| newest.set(Some(serial)));
    }
}

/// The serial of the newest input this thread's window handled, while it handled one: what an
/// invocation made now names as the press it followed.
#[must_use]
pub fn current() -> Option<u32> {
    NEWEST.with(Cell::get)
}

/// The window is handling the press of `serial`: what the host that delivers the person's input
/// with it says, and what an invocation made now names.
pub fn note(serial: u32) {
    NEWEST.with(|newest| newest.set(Some(serial)));
}

/// The press this window is handling answered the control standing at `rect` in it (in the
/// window's logical pixels): a call the press makes carries the control's centre as where the
/// person's question stands. What a key press says, which has no point of its own.
pub fn note_control(rect: crate::kurbo::Rect) {
    if let Some(serial) = current() {
        tilo_actions::remote::press_points::note(serial, centre_of(rect));
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a control's place in its window is a few thousand pixels at most"
)]
fn centre_of(rect: crate::kurbo::Rect) -> tilo_actions::remote::asked::PressPoint {
    let centre = rect.center();
    tilo_actions::remote::asked::PressPoint {
        x: centre.x.round() as i32,
        y: centre.y.round() as i32,
    }
}

/// Forget the newest input, so a test's press does not outlive it on a reused thread.
#[cfg(test)]
pub(crate) fn forget() {
    NEWEST.with(|newest| newest.set(None));
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{ElementState, MouseButton, WindowEvent};

    #[allow(
        unsafe_code,
        reason = "winit's dummy device is how a test names no device"
    )]
    #[test]
    fn a_press_sets_the_serial_and_other_events_leave_it() {
        forget();
        note_event(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            7,
        );
        assert_eq!(
            current(),
            Some(7),
            "the press an invocation made now follows"
        );
        note_event(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
            9,
        );
        assert_eq!(current(), Some(7), "a release is no press");
        note_event(&WindowEvent::RedrawRequested, 11);
        assert_eq!(current(), Some(7), "a redraw is no press");
        forget();
        assert_eq!(current(), None);
    }
}
