//! Heuristic Caps-Lock detection.
//!
//! **What winit provides.** winit 0.30's [`winit::event::Modifiers`] (and the
//! underlying `ModifiersState`) only report the *held* modifier keys —
//! `SHIFT`, `CONTROL`, `ALT` and `SUPER`. There is deliberately **no**
//! lock-state bit: `ModifiersState` cannot tell you whether Caps Lock (or
//! Num Lock) is currently engaged. Winit treats lock keys as ordinary keys
//! and folds their effect into the resolved character, so the only signal
//! available to a cross-platform app is the *character that was produced*.
//!
//! **The heuristic.** We infer the lock state from each typed letter and the
//! Shift state that accompanied it:
//!
//! | typed letter | Shift held | conclusion      |
//! |--------------|------------|-----------------|
//! | UPPER (`A`)  | no         | Caps Lock **on**  |
//! | UPPER (`A`)  | yes        | Caps Lock **off** |
//! | lower (`a`)  | yes        | Caps Lock **on**  (Shift inverts the lock) |
//! | lower (`a`)  | no         | Caps Lock **off** |
//!
//! Non-alphabetic input (digits, punctuation, whitespace) carries no Caps
//! Lock signal and leaves the estimate unchanged. This is the standard
//! approach login screens use when the windowing layer can't report the
//! lock directly; it's correct the moment the user types their first
//! letter, which on a password field is immediately.

/// A running estimate of whether Caps Lock is engaged, inferred from typed
/// letters. Cheap to copy and to keep alive; update it by feeding each typed
/// character (and the Shift state that produced it) through [`observe`] /
/// [`observe_text`].
///
/// [`observe`]: CapsLockEstimate::observe
/// [`observe_text`]: CapsLockEstimate::observe_text
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapsLockEstimate {
    on: bool,
}

impl CapsLockEstimate {
    /// A fresh estimate with Caps Lock assumed **off**.
    pub fn new() -> Self {
        Self { on: false }
    }

    /// The current best guess: `true` iff the most recent alphabetic
    /// keystroke implied Caps Lock is engaged.
    pub fn on(&self) -> bool {
        self.on
    }

    /// Fold a single typed character (and whether Shift was held when it was
    /// produced) into the estimate. Returns the updated [`on`](Self::on)
    /// state. Non-ASCII-alphabetic characters leave the estimate unchanged
    /// (and return the prior state).
    ///
    /// Only the *first* char of a key's text matters for the signal, but
    /// callers may pass each char; the result is order-independent for a
    /// run typed under one Shift state.
    pub fn observe(&mut self, ch: char, shift: bool) -> bool {
        if ch.is_ascii_uppercase() {
            // An uppercase glyph with Shift up means the lock supplied the
            // case; with Shift down the Shift supplied it (lock off).
            self.on = !shift;
        } else if ch.is_ascii_lowercase() {
            // A lowercase glyph while Shift is held means the lock is on and
            // Shift inverted it; lowercase with Shift up means lock off.
            self.on = shift;
        }
        self.on
    }

    /// Fold every character of a typed `text` run (all produced under the
    /// same `shift` state — as winit delivers a single `KeyEvent`).
    pub fn observe_text(&mut self, text: &str, shift: bool) {
        for ch in text.chars() {
            self.observe(ch, shift);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off() {
        assert!(!CapsLockEstimate::new().on());
        assert!(!CapsLockEstimate::default().on());
    }

    #[test]
    fn uppercase_without_shift_means_on() {
        let mut c = CapsLockEstimate::new();
        assert!(c.observe('A', false));
        assert!(c.on());
    }

    #[test]
    fn uppercase_with_shift_means_off() {
        let mut c = CapsLockEstimate::new();
        // Seed "on", then prove a Shift+upper clears it.
        c.observe('A', false);
        assert!(c.on());
        assert!(!c.observe('B', true));
        assert!(!c.on());
    }

    #[test]
    fn lowercase_with_shift_means_on() {
        // Caps Lock on + Shift held yields a lowercase glyph.
        let mut c = CapsLockEstimate::new();
        assert!(c.observe('a', true));
        assert!(c.on());
    }

    #[test]
    fn lowercase_without_shift_means_off() {
        let mut c = CapsLockEstimate::new();
        c.observe('A', false); // on
        assert!(!c.observe('b', false)); // normal typing clears it
        assert!(!c.on());
    }

    #[test]
    fn non_alpha_leaves_estimate_unchanged() {
        let mut c = CapsLockEstimate::new();
        c.observe('A', false);
        assert!(c.on(), "precondition: on");
        // Digits / punctuation / space carry no signal.
        for ch in ['1', '!', ' ', '-', '\n'] {
            assert!(c.observe(ch, false), "non-alpha must not flip {ch:?}");
        }
        assert!(c.on(), "still on after non-alpha input");
    }

    #[test]
    fn observe_text_folds_a_run() {
        let mut c = CapsLockEstimate::new();
        c.observe_text("HELLO", false); // all-caps, no shift => on
        assert!(c.on());
        c.observe_text("world", false); // normal => off
        assert!(!c.on());
    }
}
