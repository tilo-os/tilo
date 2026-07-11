//! The lock screen's input + authentication state machine.
//!
//! While the session is locked ([`crate::compositor::idle::SessionManager::is_locked`]),
//! the compositor routes **all** keyboard input into this controller instead
//! of to apps/chrome — that redirection is the input half of the lock's
//! security boundary. The controller accumulates a password, and only a
//! credential the [`PasswordVerifier`] accepts produces
//! [`UnlockOutcome::Unlocked`]. Nothing else — no key, no client message —
//! can return `Unlocked`, so the session cannot be bypassed: the only path
//! out is a correct password.
//!
//! This is deliberately a pure, I/O-light struct (the one I/O is the injected
//! verifier's `verify` call) so the whole lock/unlock flow is unit-testable
//! on the real code path with a fake verifier — see [`crate::compositor::lock_auth`].

use std::sync::Arc;

use super::lock_auth::PasswordVerifier;

/// Upper bound on the buffered password length. Generous for any real
/// passphrase; bounds memory against a key held down / a paste flood while
/// locked.
const MAX_PASSWORD_LEN: usize = 1024;

/// Result of feeding input to the controller: did this input unlock?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockOutcome {
    /// The session remains locked (wrong/empty password, or a non-submit
    /// key). The overlay stays up and input stays captured.
    StillLocked,
    /// The credential was accepted — the caller must clear the compositor
    /// lock and restore normal presentation + input.
    Unlocked,
}

/// A normalised key the lock screen understands, produced by mapping either
/// a winit `WindowEvent` (live backend) or a test-control `key`/`type`
/// command (headless harness) — so both drive the exact same routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockKey {
    /// Printable text to append (one typed character or a `type` string).
    Text(String),
    /// Delete the last character of the buffer.
    Backspace,
    /// Attempt to unlock with the current buffer (Enter).
    Submit,
    /// Clear the current input without attempting (Escape).
    Cancel,
    /// Consumed but with no effect on the buffer (e.g. an arrow key). Still
    /// swallowed — never routed to apps while locked.
    Ignored,
}

/// Lock-screen input + auth. Holds the typed password, the last error, and
/// the injected [`PasswordVerifier`]. See the module docs for the security
/// invariant.
pub struct LockController {
    verifier: Arc<dyn PasswordVerifier>,
    username: String,
    buffer: String,
    error: Option<String>,
}

impl LockController {
    /// Build a controller that authenticates `username` via `verifier`.
    pub fn new(verifier: Arc<dyn PasswordVerifier>, username: impl Into<String>) -> Self {
        Self {
            verifier,
            username: username.into(),
            buffer: String::new(),
            error: None,
        }
    }

    /// Reset transient state on a fresh lock: clear any half-typed password
    /// and stale error so the screen starts empty each time it engages.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.error = None;
    }

    /// Append printable text (a typed char or a pasted/`type`d string),
    /// clamped to [`MAX_PASSWORD_LEN`]. Control characters are ignored.
    /// Typing clears a previously shown error.
    pub fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            self.push_char(c);
        }
    }

    /// Append a single printable character (see [`Self::push_str`]).
    pub fn push_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        if self.buffer.chars().count() >= MAX_PASSWORD_LEN {
            return;
        }
        self.error = None;
        self.buffer.push(c);
    }

    /// Remove the last character. Clears any shown error.
    pub fn backspace(&mut self) {
        self.error = None;
        self.buffer.pop();
    }

    /// Clear the buffer (Escape). Also clears any shown error.
    pub fn clear_input(&mut self) {
        self.buffer.clear();
        self.error = None;
    }

    /// Attempt to unlock with the current buffer.
    ///
    /// An empty buffer is a no-op ([`UnlockOutcome::StillLocked`], no verify
    /// call). Otherwise the credential is checked by the injected verifier:
    /// on success the controller resets and returns [`UnlockOutcome::Unlocked`];
    /// on failure it records the (non-leaky) error, clears the buffer, and
    /// stays [`UnlockOutcome::StillLocked`]. This is the *only* method that
    /// can ever return `Unlocked`.
    pub fn submit(&mut self) -> UnlockOutcome {
        if self.buffer.is_empty() {
            return UnlockOutcome::StillLocked;
        }
        match self.verifier.verify(&self.username, &self.buffer) {
            Ok(()) => {
                self.reset();
                UnlockOutcome::Unlocked
            }
            Err(message) => {
                self.error = Some(message);
                self.buffer.clear();
                UnlockOutcome::StillLocked
            }
        }
    }

    /// Route a normalised key into the controller. The single entry point the
    /// compositor calls for every keystroke while locked, so the live backend
    /// and the headless harness share one routing path.
    pub fn handle_key(&mut self, key: LockKey) -> UnlockOutcome {
        match key {
            LockKey::Text(s) => {
                self.push_str(&s);
                UnlockOutcome::StillLocked
            }
            LockKey::Backspace => {
                self.backspace();
                UnlockOutcome::StillLocked
            }
            LockKey::Cancel => {
                self.clear_input();
                UnlockOutcome::StillLocked
            }
            LockKey::Ignored => UnlockOutcome::StillLocked,
            LockKey::Submit => self.submit(),
        }
    }

    /// Number of characters currently buffered (for the masked field).
    pub fn buffer_len(&self) -> usize {
        self.buffer.chars().count()
    }

    /// The last authentication error, if any (shown under the field).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::lock_auth::FixedVerifier;

    fn controller(correct: &str) -> LockController {
        LockController::new(Arc::new(FixedVerifier::new(correct)), "alice")
    }

    #[test]
    fn wrong_password_stays_locked_and_records_error() {
        let mut c = controller("hunter2");
        c.push_str("wrong");
        assert_eq!(c.buffer_len(), 5);
        assert_eq!(c.submit(), UnlockOutcome::StillLocked);
        // Buffer cleared, error shown, still locked.
        assert_eq!(c.buffer_len(), 0);
        assert_eq!(c.error(), Some("Incorrect username or password"));
    }

    #[test]
    fn correct_password_unlocks() {
        let mut c = controller("hunter2");
        c.push_str("hunter2");
        assert_eq!(c.submit(), UnlockOutcome::Unlocked);
        // Reset after a successful unlock.
        assert_eq!(c.buffer_len(), 0);
        assert_eq!(c.error(), None);
    }

    #[test]
    fn empty_submit_is_a_noop_and_never_unlocks() {
        let mut c = controller("hunter2");
        assert_eq!(c.submit(), UnlockOutcome::StillLocked);
        assert_eq!(c.error(), None);
    }

    #[test]
    fn typing_after_a_failure_clears_the_error() {
        let mut c = controller("hunter2");
        c.push_str("bad");
        c.submit();
        assert!(c.error().is_some());
        c.push_char('h');
        assert_eq!(c.error(), None);
        assert_eq!(c.buffer_len(), 1);
    }

    #[test]
    fn backspace_and_cancel_edit_the_buffer() {
        let mut c = controller("hunter2");
        c.push_str("hunt");
        c.backspace();
        assert_eq!(c.buffer_len(), 3);
        c.clear_input();
        assert_eq!(c.buffer_len(), 0);
    }

    #[test]
    fn control_characters_are_not_buffered() {
        let mut c = controller("hunter2");
        c.push_str("a\nb\tc");
        // Only the three printable chars are kept.
        assert_eq!(c.buffer_len(), 3);
    }

    #[test]
    fn handle_key_routes_text_backspace_submit() {
        let mut c = controller("pw");
        assert_eq!(
            c.handle_key(LockKey::Text("p".into())),
            UnlockOutcome::StillLocked
        );
        assert_eq!(
            c.handle_key(LockKey::Text("x".into())),
            UnlockOutcome::StillLocked
        );
        assert_eq!(c.handle_key(LockKey::Backspace), UnlockOutcome::StillLocked);
        assert_eq!(c.buffer_len(), 1);
        // "p" + submit is wrong ("pw" expected).
        assert_eq!(c.handle_key(LockKey::Submit), UnlockOutcome::StillLocked);
        // Now type the correct password and submit.
        c.handle_key(LockKey::Text("pw".into()));
        assert_eq!(c.handle_key(LockKey::Submit), UnlockOutcome::Unlocked);
    }

    #[test]
    fn ignored_key_is_swallowed_without_effect() {
        let mut c = controller("pw");
        c.push_str("ab");
        assert_eq!(c.handle_key(LockKey::Ignored), UnlockOutcome::StillLocked);
        assert_eq!(c.buffer_len(), 2);
    }
}
