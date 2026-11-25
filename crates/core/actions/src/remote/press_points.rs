//! Where in its window the control a key press answered stands, by the press's serial.
//!
//! The compositor attests a key press to the window it delivered it to, but a key has no point:
//! only the window knows which of its controls the key answered (the one that declares it, else
//! the one the keyboard is in). The window notes that control's place here as it handles the
//! press, and a call the press makes carries it ([`super::RemoteActionFrame::pressed`]). It says
//! where in the app's own window a question stands, and nothing more: the host takes it only for
//! a press the compositor attested in that app's window, and a pointer's own point wins.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::asked::PressPoint;

/// The presses whose place is remembered: a window handles one press at a time, and the calls
/// one makes leave soon after it.
const KEPT: usize = 16;

static POINTS: Mutex<VecDeque<(u32, PressPoint)>> = Mutex::new(VecDeque::new());

/// The press of `serial` answered the control standing at `at` in its window.
pub fn note(serial: u32, at: PressPoint) {
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
pub fn of(serial: u32) -> Option<PressPoint> {
    POINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .rev()
        .find(|(kept, _)| *kept == serial)
        .map(|(_, at)| *at)
}
