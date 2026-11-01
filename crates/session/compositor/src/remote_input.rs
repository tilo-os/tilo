//! What input from another device crosses before it reaches this seat.
//!
//! Input that did not come from this machine's own keyboards, pointers and
//! touchscreens is not the person's: it arrives from a portal
//! `RemoteDesktop` or `InputCapture` session, a share, or a remote-typing
//! link. It reaches the seat only while the person allows it, and one
//! keystroke — Ctrl+Alt+Escape, pressed on the person's own keyboard —
//! takes that permission away at once and keeps it away: sharing comes back
//! only when the person turns it on again
//! ([`crate::Compositor::allow_remote_input`]).
//!
//! Beside the cut, the person's own input wins: a hand on this machine's
//! keyboard or mouse in the last [`PHYSICAL_PRIORITY`] says the person is
//! taking over, and remote input waits for them to stop. A locked session
//! takes no remote input either; the lock screen is not a way into the
//! machine.
//!
//! The chord is tracked here, not by the focused window: a window that
//! wants Escape still gets one, but never the press that completes the cut.
//! The same chord gives a locked pointer and an inhibited client their
//! freedom back first ([`crate::state::State::handle_input`]); only when no
//! client holds either does it cut.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// How long the person's own input keeps remote input out.
pub const PHYSICAL_PRIORITY: Duration = Duration::from_millis(500);

/// The keys the cut chord is made of (evdev codes).
mod keycode {
    /// The key that completes the chord.
    pub(super) const ESCAPE: u32 = 1;
    pub(super) const LEFT_ALT: u32 = 56;
    pub(super) const RIGHT_ALT: u32 = 100;
    pub(super) const LEFT_CTRL: u32 = 29;
    pub(super) const RIGHT_CTRL: u32 = 97;
}

/// Whether the person's held keys complete the cut chord.
fn completes_cut(held: &BTreeSet<u32>) -> bool {
    let ctrl = held.contains(&keycode::LEFT_CTRL) || held.contains(&keycode::RIGHT_CTRL);
    let alt = held.contains(&keycode::LEFT_ALT) || held.contains(&keycode::RIGHT_ALT);
    ctrl && alt && held.contains(&keycode::ESCAPE)
}

/// The firewall remote input crosses, and the person's own keys that shut
/// it.
#[derive(Debug, Default)]
pub(crate) struct Firewall {
    /// The person cut remote input; it stays cut until they allow it again.
    cut: bool,
    /// The keys the person's own keyboard holds down.
    held: BTreeSet<u32>,
    /// When this machine's own input last did anything.
    last_local: Option<Instant>,
}

impl Firewall {
    /// Whether remote input is cut.
    pub(crate) fn is_cut(&self) -> bool {
        self.cut
    }

    /// Cut remote input now, whoever asked; false when it already was.
    pub(crate) fn cut(&mut self) -> bool {
        let was = std::mem::replace(&mut self.cut, true);
        !was
    }

    /// Let remote input in again.
    pub(crate) fn allow(&mut self) {
        self.cut = false;
    }

    /// Note an event from this machine's own input at `now`.
    pub(crate) fn note_local(&mut self, now: Instant) {
        self.last_local = Some(now);
    }

    /// Track one key of the person's own keyboard; true when it completes
    /// Ctrl+Alt+Escape, which the caller keeps from the desktop and acts on
    /// ([`Firewall::cut`], or handing a client's pointer and shortcuts back
    /// first).
    pub(crate) fn chord(&mut self, keycode: u32, pressed: bool) -> bool {
        if !pressed {
            self.held.remove(&keycode);
            return false;
        }
        self.held.insert(keycode);
        keycode == keycode::ESCAPE && completes_cut(&self.held)
    }

    /// Whether remote input may reach the seat at `now`.
    pub(crate) fn admits(&self, now: Instant, locked: bool) -> bool {
        if self.cut || locked {
            return false;
        }
        self.last_local.is_none_or(|local| {
            now.checked_duration_since(local)
                .is_some_and(|elapsed| elapsed >= PHYSICAL_PRIORITY)
        })
    }

    /// Why input was refused, for the log; `None` when it was let in.
    pub(crate) fn refusal(&self, now: Instant, locked: bool) -> Option<&'static str> {
        if self.cut {
            Some("cut")
        } else if locked {
            Some("session-locked")
        } else if !self.admits(now, locked) {
            Some("physical-priority")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Press `keycodes` in order and let each go again, as fingers do.
    fn press(firewall: &mut Firewall, keycodes: &[u32]) {
        for &keycode in keycodes {
            firewall.chord(keycode, true);
        }
        for &keycode in keycodes.iter().rev() {
            firewall.chord(keycode, false);
        }
    }

    #[test]
    fn ctrl_alt_escape_cuts_remote_input_and_nothing_else_does() {
        let now = Instant::now();
        let mut firewall = Firewall::default();
        assert!(firewall.admits(now, false));
        // Escape alone, and Ctrl+Alt with another key, are the window's.
        press(&mut firewall, &[keycode::ESCAPE]);
        press(&mut firewall, &[keycode::LEFT_CTRL, keycode::LEFT_ALT, 30]);
        assert!(!firewall.is_cut(), "neither chord cuts");
        // The chord cuts, and its Escape never reaches the window.
        assert!(
            !firewall.chord(keycode::LEFT_CTRL, true),
            "the modifiers alone are the window's"
        );
        assert!(!firewall.chord(keycode::RIGHT_ALT, true));
        assert!(
            firewall.chord(keycode::ESCAPE, true),
            "the completing press is the chord's"
        );
        // The compositor decides what the chord means; cutting is one answer.
        firewall.cut();
        assert!(firewall.is_cut());
        assert!(!firewall.admits(now + PHYSICAL_PRIORITY, false));
        assert_eq!(
            firewall.refusal(now + PHYSICAL_PRIORITY, false),
            Some("cut")
        );
        // It stays cut until the person allows it again.
        firewall.chord(keycode::ESCAPE, false);
        firewall.allow();
        assert!(firewall.admits(now + PHYSICAL_PRIORITY, false));
    }

    #[test]
    fn the_persons_own_input_holds_remote_input_back_and_a_lock_refuses_it() {
        let now = Instant::now();
        let mut firewall = Firewall::default();
        firewall.note_local(now);
        assert!(!firewall.admits(now, false));
        assert!(!firewall.admits(now + Duration::from_millis(1), false));
        assert!(firewall.admits(now + PHYSICAL_PRIORITY, false));
        assert!(!firewall.admits(now + PHYSICAL_PRIORITY, true));
        assert_eq!(
            firewall.refusal(now + PHYSICAL_PRIORITY, true),
            Some("session-locked")
        );
        assert_eq!(firewall.refusal(now + PHYSICAL_PRIORITY, false), None);
    }
}
