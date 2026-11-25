//! The person's own presses, as the compositor that delivered them recorded them.
//!
//! A call an app makes runs as the person using it only when their own input is behind it: a
//! click or a key the compositor delivered to one of the app's windows while that window had
//! focus, and never input an agent's seat or another device sent through the shared seat. The
//! compositor saw that input, so it is the one that can say so: each press it delivers to a
//! window is recorded with the input serial it issued to that window, and where in the window a
//! pointer or a finger pressed, and the session hands it to the device's daemon over its shell
//! link ([`runtime_collab::wire::shell::ShellCall::Press`]), which decides whether a call naming
//! the serial follows it, and stands a question that call raises where the press was.

use std::collections::VecDeque;
use std::sync::Mutex;

/// The window a press on the compositor's own surfaces (the shell's palette, menus and
/// shortcuts) stands for: no client's window, so no app's call names it, and only the person's
/// own calls their shell makes prove themselves with it.
pub const SHELL: u64 = u64::MAX;

/// How many presses wait to be handed over: enough for what the person can press before the
/// session's next pass, and bounded so a session that fell behind is not grown without end.
const KEPT: usize = 64;

/// One press of the person's in a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersonPress {
    /// The window it reached, by its id.
    pub window: u64,
    /// The input serial the compositor issued to that window with it.
    pub serial: u32,
    /// Where in the window it was, in logical pixels from the window's top-left corner: a
    /// pointer's or a finger's press; `None` for a key.
    pub at: Option<(i32, i32)>,
}

/// The person's presses this compositor delivered, each with the serial it issued to the window
/// that received it, waiting to be handed to the daemon.
#[derive(Debug, Default)]
pub struct PersonPresses {
    pending: Mutex<VecDeque<PersonPress>>,
}

impl PersonPresses {
    /// The person pressed in `window`, delivered with the input serial `serial`, at `at` in it.
    pub fn note(&self, window: u64, serial: u32, at: Option<(i32, i32)>) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        pending.push_back(PersonPress { window, serial, at });
        while pending.len() > KEPT {
            let _ = pending.pop_front();
        }
    }

    /// The presses delivered since this was last asked, oldest first: what the session hands the
    /// device's daemon.
    pub fn take(&self) -> Vec<PersonPress> {
        let Ok(mut pending) = self.pending.lock() else {
            return Vec::new();
        };
        pending.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(window: u64, serial: u32) -> PersonPress {
        PersonPress {
            window,
            serial,
            at: None,
        }
    }

    #[test]
    fn a_press_is_handed_over_once() {
        let presses = PersonPresses::default();
        presses.note(7, 11, None);
        presses.note(8, 12, Some((40, 18)));
        assert_eq!(
            presses.take(),
            [
                press(7, 11),
                PersonPress {
                    at: Some((40, 18)),
                    ..press(8, 12)
                }
            ]
        );
        assert!(presses.take().is_empty(), "each press is handed over once");
    }

    #[test]
    fn presses_past_the_bound_fall_out_oldest_first() {
        let presses = PersonPresses::default();
        let kept = u32::try_from(KEPT).expect("the bound fits a serial");
        for serial in 0..(kept + 2) {
            presses.note(3, serial, None);
        }
        let taken = presses.take();
        assert_eq!(taken.len(), KEPT);
        assert_eq!(taken.first(), Some(&press(3, 2)), "the oldest fell out");
        assert_eq!(taken.last(), Some(&press(3, kept + 1)));
    }
}
