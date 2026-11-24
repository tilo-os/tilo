//! The person's own presses, as the compositor that delivered them recorded them.
//!
//! A call an app makes runs as the person using it only when their own input is behind it: a
//! click or a key the compositor delivered to one of the app's windows while that window had
//! focus, and never input an agent's seat or another device sent through the shared seat. The
//! compositor saw that input, so it is the one that can say so: each press it delivers to a
//! window is recorded with the input serial it issued to that window, and the session hands it
//! to the device's daemon over its shell link
//! ([`runtime_collab::wire::shell::ShellCall::Press`]), which decides whether a call naming the
//! serial follows it.

use std::collections::VecDeque;
use std::sync::Mutex;

/// How many presses wait to be handed over: enough for what the person can press before the
/// session's next pass, and bounded so a session that fell behind is not grown without end.
const KEPT: usize = 64;

/// The person's presses this compositor delivered, each with the serial it issued to the window
/// that received it, waiting to be handed to the daemon.
#[derive(Debug, Default)]
pub struct PersonPresses {
    pending: Mutex<VecDeque<(u64, u32)>>,
}

impl PersonPresses {
    /// The person pressed in `window`, delivered with the input serial `serial`.
    pub fn note(&self, window: u64, serial: u32) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        pending.push_back((window, serial));
        while pending.len() > KEPT {
            let _ = pending.pop_front();
        }
    }

    /// The presses delivered since this was last asked, each window and its serial, oldest
    /// first: what the session hands the device's daemon.
    pub fn take(&self) -> Vec<(u64, u32)> {
        let Ok(mut pending) = self.pending.lock() else {
            return Vec::new();
        };
        pending.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_is_handed_over_once() {
        let presses = PersonPresses::default();
        presses.note(7, 11);
        presses.note(8, 12);
        assert_eq!(presses.take(), [(7, 11), (8, 12)]);
        assert!(presses.take().is_empty(), "each press is handed over once");
    }

    #[test]
    fn presses_past_the_bound_fall_out_oldest_first() {
        let presses = PersonPresses::default();
        let kept = u32::try_from(KEPT).expect("the bound fits a serial");
        for serial in 0..(kept + 2) {
            presses.note(3, serial);
        }
        let taken = presses.take();
        assert_eq!(taken.len(), KEPT);
        assert_eq!(taken.first(), Some(&(3, 2)), "the oldest fell out");
        assert_eq!(taken.last(), Some(&(3, kept + 1)));
    }
}
