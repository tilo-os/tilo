//! Typing assists: keys that wait until they are meant (slow keys) and
//! modifiers that stay down until the chord is typed (sticky keys).
//!
//! Both are pure state machines over raw evdev keycodes and caller-supplied
//! instants, so their timing is deterministic under test.
//! [`crate::backend::drm::access_keys`] puts them, with bounce keys and mouse
//! keys, in front of the compositor's input routing.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "assist timing: instants and short delays, far from the end of Instant"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// How soon a modifier pressed a second time after its first press counts as
/// pressed twice: from then on it stays down until it is pressed again.
pub const LOCK_WINDOW: Duration = Duration::from_millis(400);

/// Whether an evdev keycode is a modifier key: shift, control, alt, super.
pub fn is_modifier(keycode: u32) -> bool {
    matches!(
        keycode,
        // KEY_LEFTSHIFT, KEY_RIGHTSHIFT
        42 | 54
        // KEY_LEFTCTRL, KEY_RIGHTCTRL
        | 29 | 97
        // KEY_LEFTALT, KEY_RIGHTALT
        | 56 | 100
        // KEY_LEFTMETA, KEY_RIGHTMETA
        | 125 | 126
    )
}

/// A key edge to deliver: an evdev keycode and whether it is a press.
pub type Edge = (u32, bool);

/// Sticky keys: a modifier stays down until the key it modifies has been
/// typed, so a chord can be typed one key at a time. Pressing a modifier
/// twice in quick succession locks it down until it is pressed again.
///
/// A modifier's press is delivered as it happens, so every application sees
/// the modifier the moment the person pressed it; what changes is that its
/// physical release is swallowed and the release is sent when the chord is
/// done.
#[derive(Debug, Default)]
pub struct StickyKeys {
    /// Modifiers held down by the latch, and whether each is locked.
    latched: BTreeMap<u32, Latched>,
    /// The last modifier press, for the second press that locks it.
    last_press: Option<(u32, Instant)>,
    /// Keys whose press was delivered, so the latch lets go once the last of
    /// them is released.
    typing: BTreeSet<u32>,
    /// Modifiers whose physical key is down, so hardware repeat is ignored.
    down: BTreeSet<u32>,
    /// Releases of modifiers whose press was never delivered (the press that
    /// unlocked it), which must go nowhere.
    unlocked: BTreeSet<u32>,
}

/// How long a latched modifier stays down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Latched {
    /// Until the chord it was pressed for has been typed.
    Once,
    /// Until it is pressed again.
    Locked,
}

impl StickyKeys {
    /// The edges a physical press stands for at `now`: its own press for any
    /// key, nothing for a latched modifier that is already down (or a
    /// hardware repeat), and a release for a locked modifier being unlocked.
    pub fn press(&mut self, keycode: u32, now: Instant) -> Vec<Edge> {
        if !is_modifier(keycode) {
            self.typing.insert(keycode);
            return vec![(keycode, true)];
        }
        if !self.down.insert(keycode) {
            // Hardware repeat of a held modifier is not a second press.
            return Vec::new();
        }
        if self.latched.get(&keycode).copied() == Some(Latched::Locked) {
            // Pressing a locked modifier lets it go; its own release then
            // belongs to a press that was never delivered.
            self.latched.remove(&keycode);
            self.unlocked.insert(keycode);
            self.last_press = None;
            return vec![(keycode, false)];
        }
        // A second press soon after the first locks it; a later one is a
        // new chord for a latch that is already down.
        let again = self.last_press.is_some_and(|(last, at)| {
            last == keycode && now.saturating_duration_since(at) < LOCK_WINDOW
        });
        let held = !self.latched.is_empty();
        self.latched.insert(
            keycode,
            if again {
                Latched::Locked
            } else {
                Latched::Once
            },
        );
        self.last_press = (!again).then_some((keycode, now));
        if again || held {
            // It has been down since the first press.
            Vec::new()
        } else {
            vec![(keycode, true)]
        }
    }

    /// The edges a physical release stands for: the key's own release for an
    /// ordinary key, followed by the releases of the latch once the last key
    /// of the chord has come up; none for a latched modifier, which stays
    /// down.
    pub fn release(&mut self, keycode: u32, _now: Instant) -> Vec<Edge> {
        self.down.remove(&keycode);
        if !is_modifier(keycode) {
            self.typing.remove(&keycode);
            let mut edges = vec![(keycode, false)];
            if self.typing.is_empty() {
                edges.extend(self.let_go());
            }
            return edges;
        }
        if self.unlocked.remove(&keycode) {
            // The press that unlocked this modifier is the one we delivered
            // as its release.
            return Vec::new();
        }
        Vec::new()
    }

    /// Let go of every modifier that is not locked, returning its release.
    fn let_go(&mut self) -> Vec<Edge> {
        let releasing: Vec<u32> = self
            .latched
            .iter()
            .filter(|(_, state)| **state == Latched::Once)
            .map(|(&keycode, _)| keycode)
            .collect();
        for keycode in &releasing {
            self.latched.remove(keycode);
        }
        self.last_press = None;
        releasing
            .into_iter()
            .map(|keycode| (keycode, false))
            .collect()
    }

    /// Every modifier whose press was delivered, for switching sticky keys
    /// off: its release must be delivered too, or the seat keeps the
    /// modifier down forever.
    pub fn reset(&mut self) -> Vec<Edge> {
        let edges: Vec<Edge> = self
            .latched
            .keys()
            .map(|&keycode| (keycode, false))
            .collect();
        *self = Self::default();
        edges
    }

    /// The modifiers held down right now, locked ones included.
    pub fn latched(&self) -> impl Iterator<Item = u32> + '_ {
        self.latched.keys().copied()
    }
}

/// What a press offered to slow keys means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlowPress {
    /// The key has been held long enough: deliver the press now.
    Deliver,
    /// The press waits; it is delivered if the key is still down once the
    /// delay has passed, and dropped if it is released first.
    Wait,
}

/// What a release offered to slow keys means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlowRelease {
    /// Deliver the release: its press was delivered.
    Deliver,
    /// Drop the release: it came before the press was ever delivered.
    Drop,
}

/// Slow keys: a key must be held for a moment before it counts, so a brush
/// against the keyboard types nothing.
#[derive(Debug, Default)]
pub struct SlowKeys {
    /// Presses held back until their delay passes, with the instant they
    /// are due.
    waiting: BTreeMap<u32, Instant>,
    /// Keys whose press was delivered and whose release is still owed.
    down: BTreeSet<u32>,
}

impl SlowKeys {
    /// Offer a physical press at `now`, with `delay` the time the key must
    /// be held (`None` delivers at once).
    pub fn press(&mut self, keycode: u32, now: Instant, delay: Option<Duration>) -> SlowPress {
        match delay {
            Some(delay) if !delay.is_zero() => {
                self.waiting.entry(keycode).or_insert(now + delay);
                SlowPress::Wait
            }
            _ => {
                self.down.insert(keycode);
                SlowPress::Deliver
            }
        }
    }

    /// Offer a physical release at `now`.
    pub fn release(&mut self, keycode: u32, _now: Instant) -> SlowRelease {
        if self.waiting.remove(&keycode).is_some() {
            return SlowRelease::Drop;
        }
        self.down.remove(&keycode);
        SlowRelease::Deliver
    }

    /// The presses whose delay has passed by `now`, in the order they were
    /// pressed.
    pub fn due(&mut self, now: Instant) -> Vec<u32> {
        let due: Vec<u32> = self
            .waiting
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(&keycode, _)| keycode)
            .collect();
        for keycode in &due {
            self.waiting.remove(keycode);
            self.down.insert(*keycode);
        }
        due
    }

    /// When [`Self::due`] is next owed.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.waiting.values().copied().min()
    }

    /// Forget every held key, for switching slow keys off.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test fixture: instants offset by whole milliseconds from one start"
)]
mod tests {
    use super::*;

    const SHIFT: u32 = 42;
    const CTRL: u32 = 29;
    const A: u32 = 30;
    const B: u32 = 48;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn modifiers_are_recognised_and_letters_are_not() {
        for code in [42, 54, 29, 97, 56, 100, 125, 126] {
            assert!(is_modifier(code), "{code}");
        }
        for code in [30, 15, 28, 76, 126 + 1] {
            assert!(!is_modifier(code), "{code}");
        }
    }

    #[test]
    fn a_sticky_modifier_stays_down_for_exactly_one_chord() {
        let t0 = Instant::now();
        let mut sticky = StickyKeys::default();
        assert_eq!(
            sticky.press(SHIFT, t0),
            vec![(SHIFT, true)],
            "pressed at once"
        );
        // Its physical release changes nothing.
        assert_eq!(sticky.release(SHIFT, t0 + ms(50)), Vec::<Edge>::new());
        assert_eq!(sticky.latched().collect::<Vec<_>>(), vec![SHIFT]);
        // Typing a key and letting go of it lets the modifier go.
        assert_eq!(sticky.press(A, t0 + ms(200)), vec![(A, true)]);
        assert_eq!(
            sticky.release(A, t0 + ms(300)),
            vec![(A, false), (SHIFT, false)]
        );
        assert_eq!(sticky.latched().count(), 0, "used up");
    }

    #[test]
    fn a_modifier_pressed_twice_stays_down_until_it_is_pressed_again() {
        let t0 = Instant::now();
        let mut sticky = StickyKeys::default();
        assert_eq!(sticky.press(CTRL, t0), vec![(CTRL, true)]);
        assert_eq!(sticky.release(CTRL, t0 + ms(40)), Vec::<Edge>::new());
        assert_eq!(
            sticky.press(CTRL, t0 + ms(100)),
            Vec::<Edge>::new(),
            "locked, already down"
        );
        assert_eq!(sticky.release(CTRL, t0 + ms(150)), Vec::<Edge>::new());
        // Typing and releasing keys does not let a locked modifier go.
        assert_eq!(sticky.press(A, t0 + ms(200)), vec![(A, true)]);
        assert_eq!(sticky.release(A, t0 + ms(400)), vec![(A, false)]);
        // Pressing it again lets it go, and its own release goes nowhere.
        assert_eq!(sticky.press(CTRL, t0 + ms(900)), vec![(CTRL, false)]);
        assert!(sticky.release(CTRL, t0 + ms(950)).is_empty());
        assert_eq!(sticky.latched().count(), 0);
    }

    #[test]
    fn holding_a_modifier_does_not_lock_it() {
        let t0 = Instant::now();
        let mut sticky = StickyKeys::default();
        assert_eq!(sticky.press(SHIFT, t0), vec![(SHIFT, true)]);
        // The keyboard's repeat arrives as another press while it is down.
        assert_eq!(sticky.press(SHIFT, t0 + ms(500)), Vec::<Edge>::new());
        assert_eq!(sticky.press(A, t0 + ms(600)), vec![(A, true)]);
        assert_eq!(
            sticky.release(A, t0 + ms(700)),
            vec![(A, false), (SHIFT, false)],
            "still one chord, not locked"
        );
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the window is a few hundred milliseconds"
    )]
    fn a_slow_second_press_latches_once_and_not_twice() {
        let t0 = Instant::now();
        let mut sticky = StickyKeys::default();
        let _ = sticky.press(SHIFT, t0);
        let _ = sticky.release(SHIFT, t0 + ms(20));
        // Well past the window: this is a new chord, still a single latch.
        let _ = sticky.press(SHIFT, t0 + ms(LOCK_WINDOW.as_millis() as u64 + 100));
        let _ = sticky.press(A, t0 + ms(800));
        assert_eq!(
            sticky.release(A, t0 + ms(1_000)),
            vec![(A, false), (SHIFT, false)],
            "not locked, so it lets go"
        );
    }

    #[test]
    fn two_keys_held_at_once_keep_the_modifier_until_both_are_released() {
        let t0 = Instant::now();
        let mut sticky = StickyKeys::default();
        let _ = sticky.press(SHIFT, t0);
        let _ = sticky.press(A, t0 + ms(10));
        let _ = sticky.press(B, t0 + ms(20));
        assert_eq!(sticky.release(B, t0 + ms(100)), vec![(B, false)]);
        assert_eq!(sticky.latched().collect::<Vec<_>>(), vec![SHIFT]);
        assert_eq!(
            sticky.release(A, t0 + ms(200)),
            vec![(A, false), (SHIFT, false)]
        );
    }

    #[test]
    fn switching_sticky_keys_off_releases_what_it_holds() {
        let t0 = Instant::now();
        let mut sticky = StickyKeys::default();
        let _ = sticky.press(SHIFT, t0);
        let _ = sticky.press(CTRL, t0 + ms(10));
        assert_eq!(sticky.reset(), vec![(CTRL, false), (SHIFT, false)]);
        assert_eq!(sticky.latched().count(), 0);
        assert_eq!(sticky.release(SHIFT, t0 + ms(20)), Vec::<Edge>::new());
    }

    #[test]
    fn a_held_key_is_delivered_once_its_delay_passes() {
        let t0 = Instant::now();
        let mut slow = SlowKeys::default();
        assert_eq!(slow.press(A, t0, Some(ms(100))), SlowPress::Wait);
        assert_eq!(slow.due(t0 + ms(99)), Vec::<u32>::new());
        assert_eq!(slow.next_deadline(), Some(t0 + ms(100)));
        assert_eq!(slow.due(t0 + ms(100)), vec![A]);
        assert_eq!(slow.next_deadline(), None);
        assert_eq!(slow.release(A, t0 + ms(150)), SlowRelease::Deliver);
    }

    #[test]
    fn a_brush_is_dropped_on_both_edges() {
        let t0 = Instant::now();
        let mut slow = SlowKeys::default();
        assert_eq!(slow.press(A, t0, Some(ms(200))), SlowPress::Wait);
        assert_eq!(slow.release(A, t0 + ms(60)), SlowRelease::Drop);
        assert_eq!(slow.due(t0 + ms(500)), Vec::<u32>::new(), "no late press");
        assert_eq!(slow.next_deadline(), None);
    }

    #[test]
    fn without_a_delay_everything_is_delivered_at_once() {
        let t0 = Instant::now();
        let mut slow = SlowKeys::default();
        assert_eq!(slow.press(A, t0, None), SlowPress::Deliver);
        assert_eq!(slow.press(A, t0, Some(Duration::ZERO)), SlowPress::Deliver);
        assert_eq!(slow.next_deadline(), None);
        assert_eq!(slow.release(A, t0), SlowRelease::Deliver);
    }

    #[test]
    fn switching_slow_keys_off_forgets_a_waiting_press() {
        let t0 = Instant::now();
        let mut slow = SlowKeys::default();
        assert_eq!(slow.press(A, t0, Some(ms(100))), SlowPress::Wait);
        slow.reset();
        assert_eq!(slow.due(t0 + ms(500)), Vec::<u32>::new());
    }
}
