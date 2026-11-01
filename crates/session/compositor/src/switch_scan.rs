//! Switch scanning: a person who drives the desktop with one or two switches
//! walks the focus from control to control and activates where it stops.
//!
//! A two-switch device is any keyboard that emits an ordinary key pair: the
//! first switch steps the focus on (as Tab does), the second activates what
//! has focus (as Return does). With an automatic period the walking happens
//! by itself and one switch is enough. Everything a person sees is drawn by
//! the focused surface itself: the focus ring that the toolkit draws
//! ([`crate::host`]'s surfaces and the clients they stand for), so scanning
//! needs nothing on screen of its own.
//!
//! The machine is pure: key edges and instants in, [`Scan`] steps out.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "scan timing: instants and short periods, far from the end of Instant"
)]

use std::time::{Duration, Instant};

/// What scanning asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scan {
    /// Move the focus to the next control.
    Next,
    /// Activate the control that has the focus.
    Activate,
}

impl Scan {
    /// The evdev keycode that does this on the seat: Tab steps the focus on,
    /// Return presses what has it.
    pub const fn keycode(self) -> u32 {
        match self {
            Self::Next => 15,     // KEY_TAB
            Self::Activate => 28, // KEY_ENTER
        }
    }

    /// The scan actions a key edge stands for: a press of the switch does
    /// it, its release does nothing.
    pub fn edges(action: impl IntoIterator<Item = Self>) -> Vec<(u32, bool)> {
        action
            .into_iter()
            .flat_map(|step| {
                let keycode = step.keycode();
                [(keycode, true), (keycode, false)]
            })
            .collect()
    }
}

/// Which ordinary keys a two-switch device emits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SwitchPair {
    /// F6 and F7.
    #[default]
    F6F7,
    /// F9 and F10.
    F9F10,
    /// The digit row's 1 and 2.
    Digits12,
}

impl SwitchPair {
    /// Every pair, in the order Settings offers them.
    pub const ALL: [Self; 3] = [Self::F6F7, Self::F9F10, Self::Digits12];

    /// The two evdev keycodes: first the switch that steps, then the one
    /// that activates.
    pub const fn keycodes(self) -> (u32, u32) {
        match self {
            Self::F6F7 => (64, 65),
            Self::F9F10 => (67, 68),
            Self::Digits12 => (2, 3),
        }
    }

    /// What the key `keycode` does in this pair, if it is one of its keys.
    pub const fn action(self, keycode: u32) -> Option<Scan> {
        let (next, activate) = self.keycodes();
        if keycode == next {
            Some(Scan::Next)
        } else if keycode == activate {
            Some(Scan::Activate)
        } else {
            None
        }
    }
}

/// The person's switch settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwitchConfig {
    pub pair: SwitchPair,
    /// How often the focus walks on by itself; `None` scans only when the
    /// first switch is pressed.
    pub period: Option<Duration>,
}

/// Whether scanning is on, and when it next walks the focus on.
#[derive(Clone, Debug, Default)]
pub struct Scanner {
    config: Option<SwitchConfig>,
    /// The auto-scan step next due.
    due: Option<Instant>,
}

impl Scanner {
    /// Switch scanning on or off. Switching it off stops the walking.
    pub fn configure(&mut self, config: Option<SwitchConfig>, now: Instant) {
        self.config = config;
        self.due = config
            .and_then(|config| config.period)
            .map(|period| now + period);
    }

    pub fn config(&self) -> Option<SwitchConfig> {
        self.config
    }

    /// What a key edge means while scanning: `None` (and the edge goes on to
    /// everything else) for a key the switch pair does not use, else the
    /// step it asks for — a press asks, a release asks for nothing and is
    /// swallowed so it never reaches a client.
    pub fn key(&mut self, keycode: u32, pressed: bool, now: Instant) -> Option<Option<Scan>> {
        let config = self.config?;
        let action = config.pair.action(keycode)?;
        if pressed {
            // A person's own press restarts the automatic walking, so the
            // focus never moves twice in a row.
            self.due = config.period.map(|period| now + period);
            Some(Some(action))
        } else {
            Some(None)
        }
    }

    /// The automatic step due by `now`, if any.
    pub fn tick(&mut self, now: Instant) -> Option<Scan> {
        let due = self.due?;
        if now < due {
            return None;
        }
        let period = self.config?.period?;
        self.due = Some(now + period);
        Some(Scan::Next)
    }

    /// When [`Self::tick`] is next owed.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.due
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test fixture: instants offset by whole milliseconds from one start"
)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn every_pair_steps_on_the_first_key_and_activates_on_the_second() {
        for pair in SwitchPair::ALL {
            let (next, activate) = pair.keycodes();
            assert_eq!(pair.action(next), Some(Scan::Next), "{pair:?}");
            assert_eq!(pair.action(activate), Some(Scan::Activate), "{pair:?}");
            assert_eq!(pair.action(30), None);
        }
    }

    #[test]
    fn the_steps_type_tab_and_return() {
        assert_eq!(
            Scan::edges([Scan::Next]),
            vec![(15, true), (15, false)],
            "Tab"
        );
        assert_eq!(
            Scan::edges([Scan::Activate]),
            vec![(28, true), (28, false)],
            "Return"
        );
    }

    #[test]
    fn off_scans_nothing() {
        let t0 = Instant::now();
        let mut scanner = Scanner::default();
        assert_eq!(scanner.key(64, true, t0), None);
        assert_eq!(scanner.tick(t0 + ms(10_000)), None);
        assert_eq!(scanner.next_deadline(), None);
    }

    #[test]
    fn the_switches_step_and_activate_and_their_releases_go_nowhere() {
        let t0 = Instant::now();
        let mut scanner = Scanner::default();
        scanner.configure(
            Some(SwitchConfig {
                pair: SwitchPair::F6F7,
                period: None,
            }),
            t0,
        );
        assert_eq!(scanner.key(64, true, t0), Some(Some(Scan::Next)));
        assert_eq!(scanner.key(64, false, t0 + ms(30)), Some(None));
        assert_eq!(
            scanner.key(65, true, t0 + ms(400)),
            Some(Some(Scan::Activate))
        );
        assert_eq!(scanner.key(65, false, t0 + ms(430)), Some(None));
        assert_eq!(scanner.key(30, true, t0), None, "letters are not switches");
        assert_eq!(scanner.next_deadline(), None, "no automatic walking");
    }

    #[test]
    fn an_automatic_period_walks_the_focus_and_a_press_restarts_it() {
        let t0 = Instant::now();
        let mut scanner = Scanner::default();
        scanner.configure(
            Some(SwitchConfig {
                pair: SwitchPair::F9F10,
                period: Some(ms(1_000)),
            }),
            t0,
        );
        assert_eq!(scanner.next_deadline(), Some(t0 + ms(1_000)));
        assert_eq!(scanner.tick(t0 + ms(999)), None);
        assert_eq!(scanner.tick(t0 + ms(1_000)), Some(Scan::Next));
        assert_eq!(scanner.next_deadline(), Some(t0 + ms(2_000)));
        // A press at 1.5 s pushes the next automatic step to 2.5 s.
        assert_eq!(
            scanner.key(67, true, t0 + ms(1_500)),
            Some(Some(Scan::Next))
        );
        assert_eq!(scanner.next_deadline(), Some(t0 + ms(2_500)));
        assert_eq!(scanner.tick(t0 + ms(2_400)), None);
        assert_eq!(scanner.tick(t0 + ms(2_500)), Some(Scan::Next));
    }

    #[test]
    fn switching_scanning_off_stops_the_walking() {
        let t0 = Instant::now();
        let mut scanner = Scanner::default();
        scanner.configure(
            Some(SwitchConfig {
                pair: SwitchPair::Digits12,
                period: Some(ms(500)),
            }),
            t0,
        );
        scanner.configure(None, t0 + ms(100));
        assert_eq!(scanner.tick(t0 + ms(10_000)), None);
        assert_eq!(scanner.next_deadline(), None);
        assert_eq!(
            scanner.key(2, true, t0),
            None,
            "switches are ordinary keys again"
        );
    }
}
