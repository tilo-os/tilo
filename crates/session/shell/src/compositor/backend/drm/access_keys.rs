//! Keyboard accessibility filters that sit between libinput and xkb: bounce
//! keys and mouse keys.
//!
//! Both are pure state machines over raw evdev keycodes and caller-supplied
//! instants, so their timing is deterministic under test. [`super::runtime`]
//! feeds them physical key edges and turns their verdicts into seat events.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// Whether a key edge continues on to xkb and the seat.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BounceVerdict {
    Accept,
    /// Swallowed as contact bounce. A rejected press also swallows its
    /// release, so the seat never sees half a keystroke.
    Reject,
}

/// Ignores a press of the key that was just released when it arrives within
/// the window: the chatter of a worn switch, or a tremor that taps a key
/// twice.
///
/// Only the most recently released key is remembered. Typing `aba` quickly
/// is two different keys in between and never a bounce; the filter exists for
/// the same key hitting twice in a row.
#[derive(Debug, Default)]
pub struct BounceKeys {
    last_release: Option<(u32, Instant)>,
    /// Keys whose press was rejected and whose release must be swallowed too.
    swallowed: BTreeSet<u32>,
}

impl BounceKeys {
    /// A physical press of `key` at `now`. A zero `window` accepts everything.
    pub fn press(&mut self, key: u32, now: Instant, window: Duration) -> BounceVerdict {
        let bounced = !window.is_zero()
            && self.last_release.is_some_and(|(last, released)| {
                last == key && now.saturating_duration_since(released) < window
            });
        if bounced {
            self.swallowed.insert(key);
            BounceVerdict::Reject
        } else {
            BounceVerdict::Accept
        }
    }

    /// A physical release of `key` at `now`.
    ///
    /// The window restarts on every release, swallowed ones included, so a
    /// switch that keeps chattering stays filtered for as long as it does.
    pub fn release(&mut self, key: u32, now: Instant) -> BounceVerdict {
        self.last_release = Some((key, now));
        if self.swallowed.remove(&key) {
            BounceVerdict::Reject
        } else {
            BounceVerdict::Accept
        }
    }

    /// Forget everything — after a session or device boundary that may have
    /// lost releases.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A pointer button mouse keys can press.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}

/// What one keypad key means while mouse keys is on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeypadKey {
    /// Move in this direction, `-1`, `0` or `1` on each axis (screen
    /// coordinates: `y` grows downwards).
    Move(i8, i8),
    /// `5`: click the selected button.
    Click,
    /// `+`: double-click the selected button.
    DoubleClick,
    /// `0` / Ins: press and hold the selected button, for dragging.
    Hold,
    /// `.` / Del: release every held button.
    Release,
    /// `/`, `*`, `-`: choose the left, middle or right button.
    Select(PointerButton),
}

/// The mouse-keys meaning of an evdev keycode, or `None` for keys that keep
/// their ordinary meaning (Num Lock and keypad Enter included).
pub fn keypad_key(evdev: u32) -> Option<KeypadKey> {
    Some(match evdev {
        71 => KeypadKey::Move(-1, -1),                  // KP_7
        72 => KeypadKey::Move(0, -1),                   // KP_8
        73 => KeypadKey::Move(1, -1),                   // KP_9
        75 => KeypadKey::Move(-1, 0),                   // KP_4
        77 => KeypadKey::Move(1, 0),                    // KP_6
        79 => KeypadKey::Move(-1, 1),                   // KP_1
        80 => KeypadKey::Move(0, 1),                    // KP_2
        81 => KeypadKey::Move(1, 1),                    // KP_3
        76 => KeypadKey::Click,                         // KP_5
        78 => KeypadKey::DoubleClick,                   // KP_PLUS
        82 => KeypadKey::Hold,                          // KP_0 / Ins
        83 => KeypadKey::Release,                       // KP_DOT / Del
        98 => KeypadKey::Select(PointerButton::Left),   // KP_SLASH
        55 => KeypadKey::Select(PointerButton::Middle), // KP_ASTERISK
        74 => KeypadKey::Select(PointerButton::Right),  // KP_MINUS
        _ => return None,
    })
}

/// A pointer button edge mouse keys wants delivered.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ButtonEdge {
    pub button: PointerButton,
    pub pressed: bool,
}

/// How far one tap of a direction key moves the pointer, in logical pixels:
/// small enough to land on a one-pixel target.
pub const NUDGE_PX: f64 = 1.0; // pointer travel on the desktop, not themed UI spacing
/// How long a direction key must be held before the pointer glides.
pub const GLIDE_DELAY: Duration = Duration::from_millis(150);
/// Glide speed as it starts, in logical pixels per second.
pub const START_SPEED: f64 = 60.0; // pointer travel on the desktop, not themed UI spacing
/// Glide speed once fully accelerated, in logical pixels per second.
pub const MAX_SPEED: f64 = 900.0; // pointer travel on the desktop, not themed UI spacing
/// How long the glide takes to reach [`MAX_SPEED`].
pub const ACCEL_TIME: Duration = Duration::from_millis(1_200);
/// Motion cadence while gliding.
pub const TICK: Duration = Duration::from_millis(16);

/// Distance glided along one axis after holding a direction for `held`.
///
/// Speed ramps linearly from [`START_SPEED`] to [`MAX_SPEED`] over
/// [`ACCEL_TIME`] once [`GLIDE_DELAY`] has passed, so distance is its exact
/// integral. Motion between two ticks is the difference of two samples,
/// which makes the path independent of how often the compositor wakes.
pub fn glide_distance(held: Duration) -> f64 {
    let Some(gliding) = held.checked_sub(GLIDE_DELAY) else {
        return 0.0;
    };
    let accel = ACCEL_TIME.as_secs_f64();
    let t = gliding.as_secs_f64();
    let ramp = |t: f64| START_SPEED * t + (MAX_SPEED - START_SPEED) * t * t / (2.0 * accel);
    if t <= accel {
        ramp(t)
    } else {
        ramp(accel) + MAX_SPEED * (t - accel)
    }
}

/// The keypad as a pointer: direction keys glide with acceleration, the rest
/// click, hold and release the selected button.
#[derive(Debug)]
pub struct MouseKeys {
    /// Direction keys currently down, by evdev code.
    directions: BTreeSet<(u32, i8, i8)>,
    /// Every keypad key whose press was consumed, so only those releases are.
    consumed: BTreeSet<u32>,
    /// When the first of the currently held direction keys went down.
    glide_start: Option<Instant>,
    /// The instant motion was last reported up to.
    reported_until: Option<Instant>,
    selected: PointerButton,
    held_buttons: BTreeSet<PointerButton>,
}

impl Default for MouseKeys {
    fn default() -> Self {
        Self {
            directions: BTreeSet::new(),
            consumed: BTreeSet::new(),
            glide_start: None,
            reported_until: None,
            selected: PointerButton::Left,
            held_buttons: BTreeSet::new(),
        }
    }
}

/// The result of offering one key edge to mouse keys.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MouseKeysOutput {
    /// Immediate pointer motion in logical pixels (a tap's nudge).
    pub motion: (f64, f64),
    pub buttons: Vec<ButtonEdge>,
}

impl MouseKeys {
    /// The button `5`, `+` and `0` act on.
    pub fn selected(&self) -> PointerButton {
        self.selected
    }

    /// Offer a physical key edge. `None` means the key is not mouse keys'
    /// to take and continues on as ordinary typing.
    pub fn key(&mut self, evdev: u32, pressed: bool, now: Instant) -> Option<MouseKeysOutput> {
        if !pressed {
            if !self.consumed.remove(&evdev) {
                return None;
            }
            self.directions.retain(|(code, _, _)| *code != evdev);
            if self.directions.is_empty() {
                self.glide_start = None;
                self.reported_until = None;
            }
            return Some(MouseKeysOutput::default());
        }
        let meaning = keypad_key(evdev)?;
        // A held key's hardware repeat is not a new press.
        if !self.consumed.insert(evdev) {
            return Some(MouseKeysOutput::default());
        }
        let mut out = MouseKeysOutput::default();
        match meaning {
            KeypadKey::Move(dx, dy) => {
                if self.directions.is_empty() {
                    self.glide_start = Some(now);
                    self.reported_until = Some(now);
                }
                self.directions.insert((evdev, dx, dy));
                out.motion = (f64::from(dx) * NUDGE_PX, f64::from(dy) * NUDGE_PX);
            }
            KeypadKey::Click => {
                out.buttons.extend(self.click());
            }
            KeypadKey::DoubleClick => {
                out.buttons.extend(self.click());
                out.buttons.extend(self.click());
            }
            KeypadKey::Hold => {
                if self.held_buttons.insert(self.selected) {
                    out.buttons.push(ButtonEdge {
                        button: self.selected,
                        pressed: true,
                    });
                }
            }
            KeypadKey::Release => {
                out.buttons.extend(self.release_all());
            }
            KeypadKey::Select(button) => {
                self.selected = button;
            }
        }
        Some(out)
    }

    fn click(&self) -> [ButtonEdge; 2] {
        [
            ButtonEdge {
                button: self.selected,
                pressed: true,
            },
            ButtonEdge {
                button: self.selected,
                pressed: false,
            },
        ]
    }

    fn release_all(&mut self) -> Vec<ButtonEdge> {
        std::mem::take(&mut self.held_buttons)
            .into_iter()
            .map(|button| ButtonEdge {
                button,
                pressed: false,
            })
            .collect()
    }

    /// The combined direction of every held direction key; opposite keys
    /// cancel.
    fn direction(&self) -> (f64, f64) {
        let (x, y) = self
            .directions
            .iter()
            .fold((0_i32, 0_i32), |(x, y), (_, dx, dy)| {
                (
                    x.saturating_add(i32::from(*dx)),
                    y.saturating_add(i32::from(*dy)),
                )
            });
        let (x, y) = (f64::from(x.signum()), f64::from(y.signum()));
        // A diagonal covers the same distance as a straight line in the
        // same time, rather than √2 more.
        if x != 0.0 && y != 0.0 {
            (
                x * std::f64::consts::FRAC_1_SQRT_2,
                y * std::f64::consts::FRAC_1_SQRT_2,
            )
        } else {
            (x, y)
        }
    }

    /// Glide owed since the last call, in logical pixels.
    pub fn motion(&mut self, now: Instant) -> (f64, f64) {
        let (Some(start), Some(from)) = (self.glide_start, self.reported_until) else {
            return (0.0, 0.0);
        };
        if now <= from {
            return (0.0, 0.0);
        }
        let distance = glide_distance(now.saturating_duration_since(start))
            - glide_distance(from.saturating_duration_since(start));
        self.reported_until = Some(now);
        let (x, y) = self.direction();
        (x * distance, y * distance)
    }

    /// When the compositor must next call [`Self::motion`], if anything is
    /// gliding.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.direction() == (0.0, 0.0) {
            return None;
        }
        let start = self.glide_start?;
        let from = self.reported_until?;
        // Sleep through the initial delay instead of ticking into nothing.
        let glide_from = start.checked_add(GLIDE_DELAY).unwrap_or(start);
        Some(from.max(glide_from).checked_add(TICK).unwrap_or(from))
    }

    /// Stop gliding and release held buttons, returning their release edges.
    /// Used when mouse keys is switched off or input state is reset.
    pub fn reset(&mut self) -> Vec<ButtonEdge> {
        let releases = self.release_all();
        let selected = self.selected;
        *self = Self::default();
        self.selected = selected;
        releases
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    clippy::arithmetic_side_effects,
    reason = "motion fixtures are exact sums of the documented constants, offset from one test instant"
)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    // bounce keys

    /// One step of a bounce-keys timing table: at `at` ms, `key` goes down
    /// or up, and the filter should say `expect`.
    struct Step {
        at: u64,
        key: u32,
        pressed: bool,
        expect: BounceVerdict,
    }

    fn run(window: Duration, steps: &[Step]) {
        let t0 = Instant::now();
        let mut filter = BounceKeys::default();
        for (index, step) in steps.iter().enumerate() {
            let now = t0 + ms(step.at);
            let verdict = if step.pressed {
                filter.press(step.key, now, window)
            } else {
                filter.release(step.key, now)
            };
            assert_eq!(
                verdict,
                step.expect,
                "step {index}: key {} {} at {} ms",
                step.key,
                if step.pressed { "down" } else { "up" },
                step.at
            );
        }
    }

    const fn down(at: u64, key: u32, expect: BounceVerdict) -> Step {
        Step {
            at,
            key,
            pressed: true,
            expect,
        }
    }

    const fn up(at: u64, key: u32, expect: BounceVerdict) -> Step {
        Step {
            at,
            key,
            pressed: false,
            expect,
        }
    }

    const A: u32 = 30;
    const B: u32 = 48;
    use BounceVerdict::{Accept as OK, Reject as NO};

    #[test]
    fn bounce_keys_timing_table() {
        run(
            ms(300),
            &[
                // A clean keystroke.
                down(0, A, OK),
                up(80, A, OK),
                // The same key 100 ms after release: a bounce, and its
                // release goes with it.
                down(180, A, NO),
                up(200, A, NO),
                // Still inside the window, which restarted at 200 ms.
                down(450, A, NO),
                up(470, A, NO),
                // 300 ms after the last release: a real keystroke again.
                down(770, A, OK),
                up(850, A, OK),
                // A different key right away is never a bounce.
                down(860, B, OK),
                up(900, B, OK),
                // A again: B was the last release, so A is not a repeat.
                down(910, A, OK),
                up(990, A, OK),
            ],
        );
    }

    #[test]
    fn the_window_boundary_is_exclusive() {
        run(
            ms(300),
            &[
                down(0, A, OK),
                up(10, A, OK),
                down(309, A, NO),
                up(309, A, NO),
                down(609, A, OK),
            ],
        );
    }

    #[test]
    fn a_zero_window_accepts_everything() {
        run(
            Duration::ZERO,
            &[down(0, A, OK), up(1, A, OK), down(2, A, OK), up(3, A, OK)],
        );
    }

    #[test]
    fn a_held_key_is_one_keystroke_however_long_it_is_held() {
        // Bounce keys measures from the release; holding never counts.
        run(
            ms(300),
            &[down(0, A, OK), up(5_000, A, OK), down(5_400, A, OK)],
        );
    }

    #[test]
    fn reset_forgets_the_last_release() {
        let t0 = Instant::now();
        let mut filter = BounceKeys::default();
        assert_eq!(filter.press(A, t0, ms(300)), BounceVerdict::Accept);
        assert_eq!(filter.release(A, t0 + ms(10)), BounceVerdict::Accept);
        filter.reset();
        assert_eq!(filter.press(A, t0 + ms(20), ms(300)), BounceVerdict::Accept);
    }

    // mouse keys

    const KP_1: u32 = 79;
    const KP_2: u32 = 80;
    const KP_4: u32 = 75;
    const KP_5: u32 = 76;
    const KP_6: u32 = 77;
    const KP_8: u32 = 72;
    const KP_0: u32 = 82;
    const KP_DOT: u32 = 83;
    const KP_PLUS: u32 = 78;
    const KP_SLASH: u32 = 98;
    const KP_STAR: u32 = 55;
    const KP_MINUS: u32 = 74;

    fn edges(list: &[(PointerButton, bool)]) -> Vec<ButtonEdge> {
        list.iter()
            .map(|&(button, pressed)| ButtonEdge { button, pressed })
            .collect()
    }

    #[test]
    fn the_keypad_layout_matches_the_documented_map() {
        assert_eq!(keypad_key(KP_8), Some(KeypadKey::Move(0, -1)));
        assert_eq!(keypad_key(KP_2), Some(KeypadKey::Move(0, 1)));
        assert_eq!(keypad_key(KP_4), Some(KeypadKey::Move(-1, 0)));
        assert_eq!(keypad_key(KP_6), Some(KeypadKey::Move(1, 0)));
        assert_eq!(keypad_key(KP_1), Some(KeypadKey::Move(-1, 1)));
        assert_eq!(keypad_key(KP_5), Some(KeypadKey::Click));
        assert_eq!(keypad_key(KP_0), Some(KeypadKey::Hold));
        assert_eq!(keypad_key(KP_DOT), Some(KeypadKey::Release));
        assert_eq!(
            keypad_key(KP_MINUS),
            Some(KeypadKey::Select(PointerButton::Right))
        );
        // Num Lock, keypad Enter and the main row stay ordinary keys.
        assert_eq!(keypad_key(69), None);
        assert_eq!(keypad_key(96), None);
        assert_eq!(keypad_key(30), None);
    }

    #[test]
    fn a_tap_nudges_one_pixel_and_does_not_glide() {
        let t0 = Instant::now();
        let mut keys = MouseKeys::default();
        let out = keys.key(KP_6, true, t0).expect("keypad 6 is mouse keys'");
        assert_eq!(out.motion, (NUDGE_PX, 0.0));
        assert_eq!(keys.motion(t0 + ms(100)), (0.0, 0.0), "inside the delay");
        assert!(keys.key(KP_6, false, t0 + ms(120)).is_some());
        assert_eq!(keys.motion(t0 + ms(2_000)), (0.0, 0.0));
        assert_eq!(keys.next_deadline(), None);
    }

    #[test]
    fn glide_distance_accelerates_then_holds_max_speed() {
        assert_eq!(glide_distance(ms(0)), 0.0);
        assert_eq!(glide_distance(GLIDE_DELAY), 0.0);
        // Half a second into the glide: v0·t + (vmax−v0)·t²/(2·T).
        let half = glide_distance(GLIDE_DELAY + ms(500));
        let expected = START_SPEED * 0.5 + (MAX_SPEED - START_SPEED) * 0.25 / (2.0 * 1.2);
        assert!((half - expected).abs() < 1e-9, "{half} vs {expected}");
        // Once accelerated, each further second adds MAX_SPEED.
        let a = glide_distance(GLIDE_DELAY + ACCEL_TIME + ms(1_000));
        let b = glide_distance(GLIDE_DELAY + ACCEL_TIME + ms(2_000));
        assert!((b - a - MAX_SPEED).abs() < 1e-9);
        // Speed never decreases: equal slices cover ever more ground.
        let mut last = 0.0;
        for i in 1..40 {
            let slice = glide_distance(ms(50 * i)) - glide_distance(ms(50 * (i - 1)));
            assert!(slice >= last - 1e-9, "slice {i} slowed down");
            last = slice;
        }
    }

    #[test]
    fn holding_a_direction_glides_the_same_path_at_any_tick_rate() {
        let t0 = Instant::now();
        let mut coarse = MouseKeys::default();
        let mut fine = MouseKeys::default();
        let _ = coarse.key(KP_2, true, t0);
        let _ = fine.key(KP_2, true, t0);
        let mut coarse_y = 0.0;
        let mut fine_y = 0.0;
        for step in 1..=10 {
            coarse_y += coarse.motion(t0 + ms(200 * step)).1;
        }
        for step in 1..=250 {
            fine_y += fine.motion(t0 + ms(8 * step)).1;
        }
        let expected = glide_distance(ms(2_000));
        assert!((coarse_y - expected).abs() < 1e-6);
        assert!((fine_y - expected).abs() < 1e-6);
    }

    #[test]
    fn diagonals_and_opposites_combine_held_directions() {
        let t0 = Instant::now();
        let mut keys = MouseKeys::default();
        let _ = keys.key(KP_1, true, t0);
        let (x, y) = keys.motion(t0 + ms(1_000));
        assert!(x < 0.0 && y > 0.0);
        assert!((x.hypot(y) - glide_distance(ms(1_000))).abs() < 1e-9);

        let mut keys = MouseKeys::default();
        let _ = keys.key(KP_4, true, t0);
        let _ = keys.key(KP_6, true, t0);
        assert_eq!(keys.motion(t0 + ms(1_000)), (0.0, 0.0));
        assert_eq!(keys.next_deadline(), None, "cancelled directions sleep");
    }

    #[test]
    fn adding_a_direction_keeps_the_acceleration() {
        let t0 = Instant::now();
        let mut keys = MouseKeys::default();
        let _ = keys.key(KP_6, true, t0);
        let _ = keys.motion(t0 + ms(1_000));
        let _ = keys.key(KP_2, true, t0 + ms(1_000));
        let (x, y) = keys.motion(t0 + ms(1_100));
        let slice = glide_distance(ms(1_100)) - glide_distance(ms(1_000));
        assert!((x.hypot(y) - slice).abs() < 1e-9);
    }

    #[test]
    fn deadlines_skip_the_delay_then_tick() {
        let t0 = Instant::now();
        let mut keys = MouseKeys::default();
        let _ = keys.key(KP_8, true, t0);
        assert_eq!(keys.next_deadline(), Some(t0 + GLIDE_DELAY + TICK));
        let _ = keys.motion(t0 + ms(500));
        assert_eq!(keys.next_deadline(), Some(t0 + ms(500) + TICK));
    }

    #[test]
    fn click_double_click_hold_and_release_use_the_selected_button() {
        use PointerButton::{Left, Middle, Right};
        let t = Instant::now();
        let mut keys = MouseKeys::default();
        let click = keys.key(KP_5, true, t).unwrap();
        assert_eq!(click.buttons, edges(&[(Left, true), (Left, false)]));
        let _ = keys.key(KP_5, false, t);

        let _ = keys.key(KP_MINUS, true, t);
        let _ = keys.key(KP_MINUS, false, t);
        assert_eq!(keys.selected(), Right);
        let double = keys.key(KP_PLUS, true, t).unwrap();
        assert_eq!(
            double.buttons,
            edges(&[(Right, true), (Right, false), (Right, true), (Right, false)])
        );
        let _ = keys.key(KP_PLUS, false, t);

        let _ = keys.key(KP_STAR, true, t);
        let _ = keys.key(KP_STAR, false, t);
        let hold = keys.key(KP_0, true, t).unwrap();
        assert_eq!(hold.buttons, edges(&[(Middle, true)]));
        let _ = keys.key(KP_0, false, t);
        let again = keys.key(KP_0, true, t).unwrap();
        assert!(
            again.buttons.is_empty(),
            "a held button is not pressed twice"
        );
        let _ = keys.key(KP_0, false, t);

        let _ = keys.key(KP_SLASH, true, t);
        let _ = keys.key(KP_SLASH, false, t);
        assert_eq!(keys.selected(), Left);
        let release = keys.key(KP_DOT, true, t).unwrap();
        assert_eq!(release.buttons, edges(&[(Middle, false)]));
    }

    #[test]
    fn only_releases_of_consumed_presses_are_taken() {
        let t = Instant::now();
        let mut keys = MouseKeys::default();
        // Pressed before mouse keys came on: its release belongs to the seat.
        assert_eq!(keys.key(KP_5, false, t), None);
        // Ordinary keys are never taken.
        assert_eq!(keys.key(30, true, t), None);
        // Hardware repeat of a held keypad key is swallowed without effect.
        assert!(keys.key(KP_5, true, t).is_some());
        assert_eq!(keys.key(KP_5, true, t), Some(MouseKeysOutput::default()));
    }

    #[test]
    fn reset_releases_held_buttons_and_stops_gliding() {
        let t = Instant::now();
        let mut keys = MouseKeys::default();
        let _ = keys.key(KP_0, true, t);
        let _ = keys.key(KP_6, true, t);
        assert_eq!(keys.reset(), edges(&[(PointerButton::Left, false)]));
        assert_eq!(keys.next_deadline(), None);
        assert_eq!(keys.key(KP_6, false, t), None, "released after reset");
    }
}
