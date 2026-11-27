//! Where in its window the control a press answered stands, by the press's serial.
//!
//! The compositor attests a press to the window it delivered it to, and for a pointer or a
//! finger it knows where on the window it landed. Only the window knows which of its controls
//! answered it (the one under the pointer, the one that declares the key, else the one the
//! keyboard is in), and that control's rect is what a question the press asks stands beside. The
//! window notes that rect here as it handles the press, and a call the press makes carries it
//! ([`super::RemoteActionFrame::pressed`]). It says where in the app's own window a question
//! stands, and nothing more: the host takes it only for a press the compositor attested in that
//! app's window.
//!
//! The system's own surfaces (the shell's palette, menus and shortcuts) have no window of an app:
//! the press they act on is the one [`handling`] names while they act on it.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::asked::PressRect;

/// The presses whose place is remembered: a window handles one press at a time, and the calls
/// one makes leave soon after it.
const KEPT: usize = 16;

static POINTS: Mutex<VecDeque<(u32, PressRect)>> = Mutex::new(VecDeque::new());

/// The press of `serial` answered the control standing at `at` in its window.
pub fn note(serial: u32, at: PressRect) {
    let mut points = POINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    points.retain(|(kept, _)| *kept != serial);
    if points.len() == KEPT {
        points.pop_front();
    }
    points.push_back((serial, at));
}

/// Where the control the press of `serial` answered stands, when the window said.
#[must_use]
pub fn of(serial: u32) -> Option<PressRect> {
    POINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .rev()
        .find(|(kept, _)| *kept == serial)
        .map(|(_, at)| *at)
}

thread_local! {
    /// The person's press the system's own surfaces are acting on, on this thread, while they do.
    static HANDLING: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Run `act` as what the person's press of `serial` on the system's own surfaces does (a
/// palette's row, a menu's item, a shortcut's key, as the compositor delivered it): a call the
/// person's shell makes for them within it names the press ([`handled`]), and runs as their own
/// act once the compositor's record of that press proves it. `None` runs `act` as no press's.
/// Only what runs inside `act` on this thread names it: a call made later, or for someone else
/// (an agent driving the same surfaces), follows no press.
pub fn handling<R>(serial: Option<u32>, act: impl FnOnce() -> R) -> R {
    /// Puts back the press handled before, however `act` ends.
    struct Restore(Option<u32>);
    impl Drop for Restore {
        fn drop(&mut self) {
            HANDLING.with(|handling| handling.set(self.0));
        }
    }
    let _restore = Restore(HANDLING.with(|handling| handling.replace(serial)));
    act()
}

/// The person's press the system's own surfaces act on now ([`handling`]), if any.
#[must_use]
pub fn handled() -> Option<u32> {
    HANDLING.with(std::cell::Cell::get)
}
