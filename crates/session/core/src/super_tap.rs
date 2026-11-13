//! Super alone: the search a tap opens, and the keys a hold shows while it lasts.
//!
//! The compositor offers every input of the person's here, once, before the keys are routed: a
//! press, a release, the pointer's own presses, a wheel. Super is only the desktop's alone — a
//! key pressed or a button clicked between its press and release makes it a chord's modifier, and
//! nothing opens. The desktop answers what it asked for on its next pass ([`SuperTap::take`]),
//! and wakes for the hold at the instant it is due ([`SuperTap::deadline`]), so nothing polls.

use std::time::Duration;

use web_time::Instant;

/// How long Super may be held alone before it counts as a hold rather than a tap: a press and
/// release with nothing between, within this, is the tap that opens search. Held past it, Super
/// shows the shortcut guide, which letting it go takes away.
pub const HOLD: Duration = Duration::from_millis(300);

/// What Super alone asked the desktop for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// A tap: open search.
    Search,
    /// A hold: show the shortcut guide.
    Hints,
    /// The hold ended, by letting Super go or by the key of a chord: the guide comes down.
    HintsGone,
}

/// Whether the evdev `keycode` is a Super key: the left and right Meta keys, as every keyboard
/// sends the desktop's key.
#[must_use]
pub const fn is_super_key(keycode: u32) -> bool {
    // KEY_LEFTMETA and KEY_RIGHTMETA.
    matches!(keycode, 125 | 126)
}

/// Where the hints a hold showed are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Hints {
    /// No hold of ours shows them.
    #[default]
    None,
    /// The hold showed them, and they are up.
    Up,
    /// What ended the hold has come: the hints go, and the key that ended it still runs.
    Ending,
}

/// Super alone, tapped or held.
#[derive(Debug, Default)]
pub struct SuperTap {
    /// When a lone Super went down, while nothing else has happened since.
    armed: Option<Instant>,
    /// Whether Super is down: a second press of it is the same press repeating, not a new one.
    down: bool,
    hints: Hints,
    /// A tap the desktop has not answered yet.
    searching: bool,
}

impl SuperTap {
    /// Super's own edge at `now`. `withheld` while the session is locked or a client holds the
    /// desktop's shortcuts: Super is that client's then, and nothing here opens.
    pub fn super_key(&mut self, pressed: bool, now: Instant, withheld: bool) {
        if withheld {
            self.withheld();
            return;
        }
        if pressed {
            // Autorepeat of the held key keeps the tap alive; a fresh press starts one.
            if self.down {
                return;
            }
            self.down = true;
            self.armed = Some(now);
            return;
        }
        if !self.down {
            return;
        }
        self.down = false;
        let Some(at) = self.armed.take() else {
            return;
        };
        if self.hints == Hints::None {
            self.searching = now.saturating_duration_since(at) < HOLD;
        } else {
            // The hold's hints go with the key that showed them.
            self.hints = Hints::Ending;
        }
    }

    /// A key of any other kind went down: holding Super while pressing it was a chord, and the
    /// hold shows nothing from here on.
    pub fn chord(&mut self) {
        self.armed = None;
        self.end_hints();
    }

    /// A button, a wheel or a finger: a tap is Super alone, so this makes it none. Hints a hold
    /// showed stay: the person points at them.
    pub fn pointer(&mut self) {
        if self.hints == Hints::None {
            self.armed = None;
        }
    }

    /// The session's lock went up over a hold, or a client took the shortcuts: Super is not the
    /// desktop's from here, and the tap that was on its way opens nothing.
    pub fn withheld(&mut self) {
        self.armed = None;
        self.searching = false;
        self.end_hints();
    }

    /// What the desktop owes Super at `now` , in the order it was asked, each once: the search of
    /// a tap, the keys of a hold that has lasted, and the going of keys a hold showed.
    pub fn take(&mut self, now: Instant) -> Vec<Ask> {
        let mut asks = Vec::new();
        if self.hints == Hints::Ending {
            self.hints = Hints::None;
            asks.push(Ask::HintsGone);
        }
        if std::mem::take(&mut self.searching) {
            asks.push(Ask::Search);
        }
        if let Some(at) = self.armed
            && self.hints == Hints::None
            && now.saturating_duration_since(at) >= HOLD
        {
            self.hints = Hints::Up;
            asks.push(Ask::Hints);
        }
        asks
    }

    /// When a held Super turns into the hints; `None` while nothing of Super's is waiting on the
    /// clock.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        let at = self.armed?;
        (self.hints != Hints::Up)
            .then(|| at.checked_add(HOLD))
            .flatten()
    }

    /// The hints a hold showed are up, and what ended them has come: the press that did is the
    /// person's key, not the guide's, and runs instead of being held back.
    #[must_use]
    pub fn hints_ending(&self) -> bool {
        self.hints == Hints::Ending
    }

    /// The desktop took the hints down, or has nothing of them to show after all: nothing is left
    /// for the release to take away.
    pub fn took_hints(&mut self) {
        self.hints = Hints::None;
    }

    /// The hints a hold showed are over, and the desktop has them to take down.
    fn end_hints(&mut self) {
        if self.hints == Hints::Up {
            self.hints = Hints::Ending;
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test timelines add a few milliseconds to one instant"
)]
mod tests {
    use super::*;

    const SUPER: u32 = 125;
    const W: u32 = 17;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    /// A tap: the press and the release with nothing between, well inside the hold.
    #[test]
    fn a_lone_tap_opens_search_on_its_release() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.super_key(true, t0, false);
        assert_eq!(
            tap.take(ms(t0, 50)),
            [],
            "nothing opens while Super is held"
        );
        tap.super_key(false, ms(t0, 90), false);
        assert_eq!(tap.take(ms(t0, 90)), [Ask::Search]);
        assert_eq!(tap.take(ms(t0, 120)), [], "one tap, one search");
    }

    /// A hold: the keys show once it has lasted, and letting Super go takes them away.
    #[test]
    fn a_hold_shows_the_keys_and_letting_go_takes_them_away() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.super_key(true, t0, false);
        assert_eq!(
            tap.deadline(),
            Some(t0 + HOLD),
            "the loop wakes for the hold, not frame by frame"
        );
        assert_eq!(
            tap.take(ms(t0, 299)),
            [],
            "not yet: this may still be a tap"
        );
        assert_eq!(tap.take(t0 + HOLD), [Ask::Hints]);
        assert_eq!(tap.deadline(), None, "the hold is shown: nothing is due");
        assert_eq!(tap.take(ms(t0, 900)), [], "one hold, one showing");
        tap.super_key(false, ms(t0, 1200), false);
        assert_eq!(tap.take(ms(t0, 1200)), [Ask::HintsGone]);
        assert_eq!(tap.take(ms(t0, 1300)), [], "gone once");
    }

    /// Autorepeat of the held key is the same press: it neither starts a new tap nor restarts the
    /// hold's clock.
    #[test]
    fn the_held_keys_own_repeats_keep_the_same_press() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.super_key(true, t0, false);
        tap.super_key(true, ms(t0, 200), false);
        assert_eq!(
            tap.deadline(),
            Some(t0 + HOLD),
            "a repeat does not start the hold again"
        );
        tap.super_key(true, ms(t0, 400), false);
        assert_eq!(tap.take(ms(t0, 400)), [Ask::Hints]);
        tap.super_key(false, ms(t0, 500), false);
        assert_eq!(tap.take(ms(t0, 500)), [Ask::HintsGone]);
    }

    /// A key pressed between Super's press and its release: Super was that chord's modifier, so
    /// neither the tap nor the hold opens anything.
    #[test]
    fn a_key_between_the_press_and_the_release_makes_it_a_chord() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.super_key(true, t0, false);
        tap.chord();
        assert_eq!(tap.deadline(), None, "nothing of the hold is left to show");
        tap.super_key(false, ms(t0, 60), false);
        assert_eq!(tap.take(ms(t0, 60)), [], "Super+W opens no search");
        assert_eq!(tap.deadline(), None);
    }

    /// A chord that came after the hints showed ends them first: they go on the pass that runs
    /// the chord, and are not the guide's to hold the chord back with.
    #[test]
    fn a_chord_after_a_hold_takes_the_hints_away_before_it_runs() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.super_key(true, t0, false);
        assert_eq!(tap.take(ms(t0, 400)), [Ask::Hints]);
        tap.chord();
        assert!(
            tap.hints_ending(),
            "the key that ends the hold is the person's"
        );
        assert_eq!(tap.take(ms(t0, 450)), [Ask::HintsGone]);
        assert!(!tap.hints_ending());
        tap.super_key(false, ms(t0, 900), false);
        assert_eq!(
            tap.take(ms(t0, 900)),
            [],
            "the release has nothing left to take away"
        );
    }

    /// A button, a wheel or a finger between the press and the release makes the tap none; the
    /// hints of a hold stay, since the person is pointing at them.
    #[test]
    fn a_pointer_press_makes_the_tap_none_and_leaves_a_holds_hints() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.super_key(true, t0, false);
        tap.pointer();
        tap.super_key(false, ms(t0, 60), false);
        assert_eq!(tap.take(ms(t0, 60)), [], "Super and a click are no tap");

        let mut tap = SuperTap::default();
        tap.super_key(true, t0, false);
        assert_eq!(tap.take(ms(t0, 400)), [Ask::Hints]);
        tap.pointer();
        assert_eq!(
            tap.take(ms(t0, 420)),
            [],
            "the hints stand under the pointer"
        );
        tap.super_key(false, ms(t0, 500), false);
        assert_eq!(tap.take(ms(t0, 500)), [Ask::HintsGone]);
    }

    /// A locked session, or one whose client holds the shortcuts, opens nothing of Super's — not
    /// even one that was held when the lock came up.
    #[test]
    fn a_withheld_super_opens_nothing() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.super_key(true, t0, true);
        tap.super_key(false, ms(t0, 60), true);
        assert_eq!(tap.take(ms(t0, 60)), []);
        assert_eq!(tap.deadline(), None);

        let mut tap = SuperTap::default();
        tap.super_key(true, t0, false);
        assert_eq!(tap.take(ms(t0, 400)), [Ask::Hints]);
        tap.withheld();
        assert_eq!(tap.take(ms(t0, 450)), [Ask::HintsGone]);
    }

    /// The Super key is the Meta key, whose evdev code is all the feed needs.
    #[test]
    fn the_super_keys_are_the_meta_keys() {
        assert!(is_super_key(SUPER));
        assert!(is_super_key(126));
        assert!(!is_super_key(W));
    }

    /// The desktop's own pass shows the hints, and the release hides them there; a hold ended
    /// before any pass showed it leaves nothing to hide.
    #[test]
    fn hints_taken_by_the_desktop_are_not_hidden_again() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.super_key(true, t0, false);
        assert_eq!(tap.take(ms(t0, 400)), [Ask::Hints]);
        tap.took_hints();
        tap.super_key(false, ms(t0, 500), false);
        assert_eq!(tap.take(ms(t0, 500)), []);
    }
}
