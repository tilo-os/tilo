//! Keys for hosted surfaces: which surface a key goes to, and its repeat.
//! A key reaches a hosted surface in one of two ways. A surface that wants the keyboard (an open
//! launcher, menu or panel) takes every key from the focused window. Any surface may also claim a
//! single press ([`SurfaceHost::claim_key`]) whatever has the keyboard: the desktop's chords, the
//! volume and brightness keys. A claimed press's release goes to the same surface, and the window
//! never sees either.
//! Clients repeat held keys themselves from the seat's repeat info; hosted surfaces get their
//! repeats from here, at the same delay and rate, while the key that started it stays held.
//! A client may also ask for the keys the desktop answers itself
//! (`zwp_keyboard_shortcuts_inhibit_manager_v1`): virtual machines, remote desktops and game
//! streaming want the machine they show to behave like the one in front. The inhibitor is granted
//! only to the surface holding keyboard focus while no hosted surface is taking input and the
//! session is unlocked, follows focus, and stays off for the surface Ctrl+Alt+Escape took it from
//! until focus moves on. It suspends only the desktop's own chords: hardware keys, the release
//! chord and locking the session are decided before any client sees a key and stay with the
//! compositor.

use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitor, KeyboardShortcutsInhibitorSeat,
};

use crate::state::State;
use tilo_session_core::surface::HostKey;

/// How held keys repeat: after `delay` , `rate` times a second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyRepeat {
    pub delay: Duration,
    /// Repeats a second; 0 turns repeating off.
    pub rate: u32,
}

impl Default for KeyRepeat {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(400),
            rate: 30,
        }
    }
}

impl KeyRepeat {
    fn interval(self) -> Option<Duration> {
        Duration::from_secs(1).checked_div(self.rate)
    }
}

/// A key repeating for a hosted surface.
#[derive(Debug)]
struct Repeating {
    host: usize,
    key: HostKey,
    due: Instant,
}

/// The keys hosted surfaces hold, and the one repeating.
#[derive(Debug, Default)]
pub(crate) struct HostKeys {
    /// Each key whose press went to a hosted surface, by evdev code, and that surface: its release
    /// goes there too.
    held: Vec<(u32, usize)>,
    repeating: Option<Repeating>,
    pub(crate) repeat: KeyRepeat,
}

impl HostKeys {
    /// The surface `keycode` 's press went to, forgetting it: the key was released.
    pub(crate) fn release(&mut self, keycode: u32) -> Option<usize> {
        if self
            .repeating
            .as_ref()
            .is_some_and(|repeating| repeating.key.keycode == keycode)
        {
            self.repeating = None;
        }
        let index = self.held.iter().position(|(held, _)| *held == keycode)?;
        Some(self.held.remove(index).1)
    }

    /// `key` 's press went to surface `host` ; it repeats there when `repeats` . Another key
    /// pressed stops what repeated before, as on every keyboard.
    pub(crate) fn press(&mut self, host: usize, key: &HostKey, repeats: bool, now: Instant) {
        self.held.retain(|(held, _)| *held != key.keycode);
        self.held.push((key.keycode, host));
        self.repeating = (repeats && self.repeat.interval().is_some()).then(|| Repeating {
            host,
            key: HostKey {
                repeat: true,
                ..key.clone()
            },
            due: now.checked_add(self.repeat.delay).unwrap_or(now),
        });
    }

    /// When the next repeat is due.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.repeating.as_ref().map(|repeating| repeating.due)
    }

    /// The repeat due by `now` : the surface and the key, or nothing.
    /// A beat the loop came to more than an interval late is gone, not owed: the person's press and
    /// its release may both be behind the stall, and a burst of catch-up repeats would type the key
    /// again for a press they already let go. The next repeat comes an interval from now, so a key
    /// still held carries on repeating once the loop is keeping time.
    pub(crate) fn due(&mut self, now: Instant) -> Vec<(usize, HostKey)> {
        let Some(interval) = self.repeat.interval() else {
            self.repeating = None;
            return Vec::new();
        };
        let Some(repeating) = self.repeating.as_mut() else {
            return Vec::new();
        };
        let Some(late) = now.checked_duration_since(repeating.due) else {
            return Vec::new();
        };
        repeating.due = now.checked_add(interval).unwrap_or(now);
        if late > interval {
            return Vec::new();
        }
        vec![(repeating.host, repeating.key.clone())]
    }
}

/// Whose shortcuts the desktop is holding back, and whose it took them from with the release chord.
#[derive(Debug, Default)]
pub(crate) struct ShortcutInhibit {
    /// The surface whose inhibitor is active.
    active: Option<WlSurface>,
    /// The surface the person took the shortcuts back from; not granted again until keyboard focus
    /// moves elsewhere.
    suspended: Option<WlSurface>,
}

impl ShortcutInhibit {
    /// Whether some client holds the desktop's own chords.
    pub(crate) fn is_active(&self) -> bool {
        self.active.is_some()
    }
}

impl State {
    /// Grant or withdraw shortcut inhibition for this moment: only the focused surface, and only
    /// while the session is unlocked and no hosted surface is taking input.
    pub(crate) fn sync_shortcut_inhibit(&mut self) {
        let allowed = !self.locked() && !self.hosts.iter().any(|host| host.wants_keyboard());
        let focus = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        if self.shortcut_inhibit.suspended.is_some() && self.shortcut_inhibit.suspended != focus {
            self.shortcut_inhibit.suspended = None;
        }
        let target = focus
            .filter(|_| allowed)
            .filter(|surface| self.shortcut_inhibit.suspended.as_ref() != Some(surface))
            .filter(|surface| Self::inhibitor_for(&self.seat, surface).is_some());
        if self.shortcut_inhibit.active == target {
            return;
        }
        if let Some(previous) = self.shortcut_inhibit.active.take()
            && let Some(inhibitor) = Self::inhibitor_for(&self.seat, &previous)
        {
            inhibitor.inactivate();
        }
        if let Some(surface) = target
            && let Some(inhibitor) = Self::inhibitor_for(&self.seat, &surface)
        {
            inhibitor.activate();
            self.shortcut_inhibit.active = Some(surface);
        }
    }

    /// The release chord: give the desktop's shortcuts back now. `false` when no client held them.
    pub(crate) fn suspend_shortcut_inhibit(&mut self) -> bool {
        let Some(surface) = self.shortcut_inhibit.active.take() else {
            return false;
        };
        if let Some(inhibitor) = Self::inhibitor_for(&self.seat, &surface) {
            inhibitor.inactivate();
        }
        self.shortcut_inhibit.suspended = Some(surface);
        true
    }

    /// A client's inhibitor object is gone: whatever it held, it no longer holds.
    pub(crate) fn inhibitor_gone(&mut self, surface: &WlSurface) {
        if self.shortcut_inhibit.active.as_ref() == Some(surface) {
            self.shortcut_inhibit.active = None;
        }
        if self.shortcut_inhibit.suspended.as_ref() == Some(surface) {
            self.shortcut_inhibit.suspended = None;
        }
    }

    /// The client's inhibitor on `surface` , if it asked for one.
    fn inhibitor_for(
        seat: &smithay::input::Seat<Self>,
        surface: &WlSurface,
    ) -> Option<KeyboardShortcutsInhibitor> {
        seat.keyboard_shortcuts_inhibitor_for_surface(surface)
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test clocks: instants a few seconds apart"
)]
mod tests {
    use super::*;
    use tilo_session_core::surface::KeyModifiers;

    fn key(keycode: u32) -> HostKey {
        HostKey {
            keycode,
            pressed: true,
            repeat: false,
            keysym: 0x61,
            unmodified_keysym: 0x61,
            latin_keysym: 0x61,
            text: Some("a".to_owned()),
            modifiers: KeyModifiers::default(),
        }
    }

    #[test]
    fn a_held_key_repeats_after_the_delay_at_the_rate_until_released() {
        let mut keys = HostKeys {
            repeat: KeyRepeat {
                delay: Duration::from_millis(100),
                rate: 10,
            },
            ..HostKeys::default()
        };
        let start = Instant::now();
        keys.press(2, &key(30), true, start);
        assert!(keys.due(start + Duration::from_millis(99)).is_empty());
        let first = keys.due(start + Duration::from_millis(100));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, 2);
        assert!(first[0].1.repeat && first[0].1.pressed);
        // One interval on, the key repeats again; the next one is due a beat after it, not at the
        // beat the first repeat kept.
        assert_eq!(keys.due(start + Duration::from_millis(200)).len(), 1);
        assert_eq!(keys.due(start + Duration::from_millis(300)).len(), 1);
        assert_eq!(keys.release(30), Some(2));
        assert!(keys.next_due().is_none());
        assert!(keys.due(start + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn another_press_stops_the_repeat_and_keys_that_do_not_repeat_never_start() {
        let mut keys = HostKeys::default();
        let now = Instant::now();
        keys.press(0, &key(30), true, now);
        keys.press(0, &key(42), false, now);
        assert!(keys.next_due().is_none());
        assert_eq!(keys.release(42), Some(0));
        assert_eq!(keys.release(30), Some(0));
        assert_eq!(keys.release(30), None);
    }

    #[test]
    fn a_stalled_loop_does_not_type_the_key_again() {
        let mut keys = HostKeys::default();
        let now = Instant::now();
        keys.press(0, &key(30), true, now);
        // The press and the release are both behind the stall: nothing of the beat the loop missed
        // is typed, and the key is not left owing a burst of repeats when the loop keeps time
        // again.
        assert!(keys.due(now + Duration::from_secs(1)).is_empty());
        assert!(
            keys.next_due()
                .is_some_and(|due| due > now + Duration::from_secs(1))
        );
        // Keeping time again, a key still held repeats on the beat.
        assert_eq!(
            keys.due(now + Duration::from_secs(1) + Duration::from_millis(34))
                .len(),
            1
        );
    }
}
