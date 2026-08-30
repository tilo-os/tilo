//! Heuristic Caps-Lock detection.
//!
//! winit's `ModifiersState` reports only held modifiers (SHIFT/CONTROL/ALT/
//! SUPER) — no lock-state bit, so Caps Lock can't be read directly. We infer
//! it from each typed letter and the Shift state that produced it:
//!
//! | typed letter | Shift held | conclusion      |
//! |--------------|------------|-----------------|
//! | UPPER (`A`)  | no         | Caps Lock **on**  |
//! | UPPER (`A`)  | yes        | Caps Lock **off** |
//! | lower (`a`)  | yes        | Caps Lock **on**  (Shift inverts the lock) |
//! | lower (`a`)  | no         | Caps Lock **off** |
//!
//! Non-alphabetic input leaves the estimate unchanged. Standard approach for
//! login screens; correct from the user's first typed letter.

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

    /// Returns the updated [`on`](Self::on) state; non-ASCII-alphabetic
    /// characters leave it unchanged. Result is order-independent for a
    /// run typed under one Shift state, so callers may pass every char.
    pub fn observe(&mut self, ch: char, shift: bool) -> bool {
        // See the module-level truth table.
        if ch.is_ascii_uppercase() {
            self.on = !shift;
        } else if ch.is_ascii_lowercase() {
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
        c.observe('A', false);
        assert!(c.on());
        assert!(!c.observe('B', true));
        assert!(!c.on());
    }

    #[test]
    fn lowercase_with_shift_means_on() {
        let mut c = CapsLockEstimate::new();
        assert!(c.observe('a', true));
        assert!(c.on());
    }

    #[test]
    fn lowercase_without_shift_means_off() {
        let mut c = CapsLockEstimate::new();
        c.observe('A', false);
        assert!(!c.observe('b', false));
        assert!(!c.on());
    }

    #[test]
    fn non_alpha_leaves_estimate_unchanged() {
        let mut c = CapsLockEstimate::new();
        c.observe('A', false);
        assert!(c.on(), "precondition: on");
        for ch in ['1', '!', ' ', '-', '\n'] {
            assert!(c.observe(ch, false), "non-alpha must not flip {ch:?}");
        }
        assert!(c.on(), "still on after non-alpha input");
    }

    #[test]
    fn observe_text_folds_a_run() {
        let mut c = CapsLockEstimate::new();
        c.observe_text("HELLO", false);
        assert!(c.on());
        c.observe_text("world", false);
        assert!(!c.on());
    }
}
