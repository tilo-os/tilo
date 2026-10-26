//! The lock screen's input + authentication state machine.
//!
//! While the session is locked ([`crate::compositor::idle::SessionManager::is_locked`]),
//! the compositor routes **all** keyboard input into this controller instead
//! of to apps/chrome — that redirection is the input half of the lock's
//! security boundary. Only a credential the [`PasswordVerifier`] accepts
//! produces [`UnlockOutcome::Unlocked`]; nothing else can, so the session
//! cannot be bypassed.
//!
//! # Asynchronous verification
//!
//! Verifying a credential can be slow (a PAM helper subprocess today,
//! biometrics later), and running that on the compositor's run loop would
//! freeze the whole desktop for the duration. So [`submit`] never verifies
//! inline: it hands `(username, password)` to a worker thread and returns
//! immediately, leaving the session locked. The run loop polls
//! [`poll_verification`] each tick; the session unlocks **only** when that
//! poll observes the worker's success. While a verify is in flight the
//! field is frozen and [`is_verifying`] is `true`.
//!
//! [`submit`]: LockController::submit
//! [`poll_verification`]: LockController::poll_verification
//! [`is_verifying`]: LockController::is_verifying

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::lock_auth::PasswordVerifier;
use ui::text_selection::prev_char_boundary;
use zeroize::{Zeroize, Zeroizing};

/// The non-leaky error shown if the verify worker vanishes without a result
/// (thread panic / dropped channel). Fail-closed: treat it as "stay locked",
/// never as an unlock.
const WORKER_LOST_MSG: &str = "Password verification is unavailable";

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
    /// An input method's composition in progress. Shown as part of the
    /// field's mask, never as text; a later [`LockKey::Text`] commits it.
    Preedit(String),
    /// Consumed but with no effect on the buffer (e.g. an arrow key). Still
    /// swallowed — never routed to apps while locked.
    Ignored,
}

/// A verification currently running on a worker thread. The controller holds
/// the receiving end of a one-shot channel; the worker sends the `verify`
/// result exactly once. Dropping this cancels the controller's interest (the
/// worker still completes but its result is discarded).
struct PendingVerify {
    rx: Receiver<Result<(), String>>,
}

/// Lock-screen input + auth. Holds the typed password, the last error, the
/// injected [`PasswordVerifier`], and any in-flight verification. See the
/// module docs for the security invariant + the async model.
pub struct LockController {
    verifier: Arc<dyn PasswordVerifier>,
    username: String,
    buffer: String,
    /// The input method's composition in progress. Never part of the
    /// credential and never rendered as text, so a composing password is
    /// exactly as unreadable as a typed one.
    preedit: String,
    error: Option<String>,
    /// `Some` while a verify runs on a worker thread; `None` when idle.
    pending: Option<PendingVerify>,
}

impl LockController {
    /// Build a controller that authenticates `username` via `verifier`.
    pub fn new(verifier: Arc<dyn PasswordVerifier>, username: impl Into<String>) -> Self {
        Self {
            verifier,
            username: username.into(),
            buffer: String::new(),
            preedit: String::new(),
            error: None,
            pending: None,
        }
    }

    /// Reset transient state on a fresh lock: clear any half-typed password,
    /// stale error, and any in-flight verification so the screen starts empty
    /// each time it engages. Dropping `pending` cancels interest in a verify
    /// left over from a prior lock episode.
    pub fn reset(&mut self) {
        self.buffer.zeroize();
        self.preedit.zeroize();
        self.error = None;
        self.pending = None;
    }

    /// Whether a verification is currently running on the worker thread. The
    /// overlay uses this to show a "Verifying…" state and freeze the field.
    pub fn is_verifying(&self) -> bool {
        self.pending.is_some()
    }

    /// Append printable text (a typed char or a pasted/`type`d string),
    /// clamped to `MAX_PASSWORD_LEN`. Control characters are ignored.
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
        // Text that commits ends the composition it came from.
        self.preedit.zeroize();
        self.error = None;
        self.buffer.push(c);
    }

    /// Remove the last user-perceived character. Clears any shown error.
    pub fn backspace(&mut self) {
        self.error = None;
        self.preedit.zeroize();
        let boundary = prev_char_boundary(&self.buffer, self.buffer.len());
        self.buffer.truncate(boundary);
    }

    /// Clear the buffer (Escape). Also clears any shown error.
    pub fn clear_input(&mut self) {
        self.buffer.zeroize();
        self.preedit.zeroize();
        self.error = None;
    }

    /// Show the input method's composition in progress. It is bounded and
    /// contains no control characters, and it is ignored while a verify is
    /// in flight because the field is frozen then.
    pub fn set_preedit(&mut self, text: &str) {
        if self.is_verifying() {
            return;
        }
        self.preedit.zeroize();
        self.preedit.extend(
            text.chars()
                .filter(|c| !c.is_control())
                .take(MAX_PASSWORD_LEN),
        );
    }

    /// The composition in progress, if any. Never the credential itself.
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    /// How many mask marks the field shows: committed characters plus the
    /// composition, bounded like the committed mask.
    pub fn mask_len(&self) -> usize {
        self.buffer
            .chars()
            .count()
            .saturating_add(self.preedit.chars().count())
            .min(MAX_PASSWORD_LEN)
    }

    /// Submit a private semantic credential to the ordinary verification worker.
    ///
    /// # Errors
    /// Refuses a concurrent attempt or an empty, oversized, or control-containing
    /// credential without retaining it or changing the current attempt.
    pub fn submit_secret(&mut self, mut password: Zeroizing<String>) -> Result<(), &'static str> {
        if self.is_verifying() {
            return Err("Password verification is already running");
        }
        if password.is_empty()
            || password.chars().count() > MAX_PASSWORD_LEN
            || password.chars().any(char::is_control)
        {
            return Err("The password is empty or exceeds the supported input limits");
        }
        self.buffer.zeroize();
        self.preedit.zeroize();
        self.buffer = std::mem::take(&mut *password);
        self.submit();
        if self.is_verifying() {
            Ok(())
        } else {
            Err(WORKER_LOST_MSG)
        }
    }

    /// Begin an unlock attempt with the current buffer, **asynchronously**.
    ///
    /// This never verifies inline. An empty buffer, or a verify already in
    /// flight, is a no-op. Otherwise it moves the typed password onto a worker
    /// thread that runs the injected verifier, clears the buffer (the
    /// plaintext now lives only on the worker), and returns
    /// [`UnlockOutcome::StillLocked`] immediately — so the caller's run loop
    /// is never blocked by a slow verify. The actual unlock is delivered later
    /// by [`Self::poll_verification`]; `submit` itself can **never** return
    /// `Unlocked`.
    pub fn submit(&mut self) -> UnlockOutcome {
        if self.buffer.is_empty() || self.pending.is_some() {
            return UnlockOutcome::StillLocked;
        }
        // Move the credential onto a worker thread. The controller keeps no
        // plaintext copy; the password lives only in the closure until the
        // verify returns.
        let verifier = Arc::clone(&self.verifier);
        let username = self.username.clone();
        self.preedit.zeroize();
        let password = Zeroizing::new(std::mem::take(&mut self.buffer));
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("tilo-lock-verify".to_string())
            .spawn(move || {
                let result = verifier.verify(&username, &password);
                // If the controller reset/unlocked meanwhile, the receiver is
                // gone and this send is a harmless no-op.
                let _ = tx.send(result);
            }) {
            Ok(_handle) => {
                // The flight record says an attempt was made and how it
                // ended, never what was typed.
                eprintln!("tilo compositor: lock: unlock attempt started");
                self.error = None;
                self.pending = Some(PendingVerify { rx });
            }
            Err(_) => {
                // Could not spawn the verify worker — fail closed, non-leaky,
                // never crash the compositor loop.
                self.error = Some(WORKER_LOST_MSG.to_string());
            }
        }
        UnlockOutcome::StillLocked
    }

    /// Poll a pending verification for its result. The run loop calls this
    /// every tick; it is the **only** method that can return
    /// [`UnlockOutcome::Unlocked`].
    ///
    /// * No verify in flight, or still running → `None` (keep the lock).
    /// * Worker reported success → reset + `Some(Unlocked)`.
    /// * Worker reported failure → record the (non-leaky) error, stay locked,
    ///   `Some(StillLocked)`.
    /// * Worker vanished without a result → fail-closed non-leaky error,
    ///   `Some(StillLocked)`.
    pub fn poll_verification(&mut self) -> Option<UnlockOutcome> {
        let pending = self.pending.as_ref()?;
        match pending.rx.try_recv() {
            Ok(Ok(())) => {
                eprintln!("tilo compositor: lock: unlock attempt verified");
                self.reset();
                Some(UnlockOutcome::Unlocked)
            }
            Ok(Err(message)) => {
                eprintln!("tilo compositor: lock: unlock attempt refused");
                self.pending = None;
                self.error = Some(message);
                self.buffer.zeroize();
                Some(UnlockOutcome::StillLocked)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                // The worker dropped its sender without a result — treat as a
                // failed check, never as an unlock.
                eprintln!("tilo compositor: lock: unlock attempt lost its verifier");
                self.pending = None;
                self.error = Some(WORKER_LOST_MSG.to_string());
                self.buffer.zeroize();
                Some(UnlockOutcome::StillLocked)
            }
        }
    }

    /// Route a normalised key into the controller. The single entry point the
    /// compositor calls for every keystroke while locked, so the live backend
    /// and the headless harness share one routing path.
    ///
    /// While a verify is in flight the field is frozen: every key is swallowed
    /// (still never routed to apps) so a second attempt can't race the first
    /// and the "Verifying…" state stays stable until the result arrives.
    pub fn handle_key(&mut self, key: LockKey) -> UnlockOutcome {
        if self.is_verifying() {
            return UnlockOutcome::StillLocked;
        }
        match key {
            LockKey::Text(s) => {
                self.push_str(&s);
                UnlockOutcome::StillLocked
            }
            LockKey::Backspace => {
                self.backspace();
                UnlockOutcome::StillLocked
            }
            LockKey::Preedit(text) => {
                self.set_preedit(&text);
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
    use crate::compositor::lock_auth::{FixedVerifier, PasswordVerifier};
    use std::sync::{Condvar, Mutex};

    fn controller(correct: &str) -> LockController {
        LockController::new(Arc::new(FixedVerifier::new(correct)), "alice")
    }

    /// Poll a controller until its in-flight verify resolves.
    fn poll_to_completion(c: &mut LockController) -> UnlockOutcome {
        tilo_test_env::wait_for("the verify to resolve", || c.poll_verification())
    }

    /// Submit the current buffer and drive the async verify to completion.
    /// Asserts the invariant that `submit` never unlocks synchronously.
    fn submit_and_wait(c: &mut LockController) -> UnlockOutcome {
        assert_eq!(
            c.submit(),
            UnlockOutcome::StillLocked,
            "submit must start the verify asynchronously, never unlock inline"
        );
        poll_to_completion(c)
    }

    #[test]
    fn password_submission_unlocks_only_on_the_right_password() {
        {
            let mut c = controller("hunter2");
            c.push_str("wrong");
            assert_eq!(c.buffer_len(), 5);
            assert_eq!(submit_and_wait(&mut c), UnlockOutcome::StillLocked);
            // Buffer cleared, error shown, still locked.
            assert_eq!(c.buffer_len(), 0);
            assert_eq!(c.error(), Some("Incorrect username or password"));
        }
        {
            let mut c = controller("hunter2");
            c.push_str("hunter2");
            assert_eq!(submit_and_wait(&mut c), UnlockOutcome::Unlocked);
            // Reset after a successful unlock.
            assert_eq!(c.buffer_len(), 0);
            assert_eq!(c.error(), None);
            assert!(!c.is_verifying());
        }
        {
            let mut c = controller("hunter2");
            assert_eq!(c.submit(), UnlockOutcome::StillLocked);
            // No verify was started, so nothing to poll.
            assert!(!c.is_verifying());
            assert_eq!(c.poll_verification(), None);
            assert_eq!(c.error(), None);
        }
        {
            let mut c = controller("hunter2");
            c.push_str("bad");
            assert_eq!(submit_and_wait(&mut c), UnlockOutcome::StillLocked);
            assert!(c.error().is_some());
            c.push_char('h');
            assert_eq!(c.error(), None);
            assert_eq!(c.buffer_len(), 1);
        }
    }

    #[test]
    fn password_editing_works_on_graphemes_and_ignores_controls() {
        {
            let mut c = controller("hunter2");
            c.push_str("hunt");
            c.backspace();
            assert_eq!(c.buffer_len(), 3);
            c.clear_input();
            assert_eq!(c.buffer_len(), 0);
        }
        {
            let mut c = controller("unused");
            c.push_str("e\u{301}");
            assert_eq!(
                c.buffer_len(),
                2,
                "the fixture contains two Unicode scalars"
            );
            c.backspace();
            assert_eq!(c.buffer_len(), 0);
        }
        {
            let mut c = controller("unused");
            c.push_str("👩‍💻");
            assert_eq!(
                c.buffer_len(),
                3,
                "the fixture contains three Unicode scalars"
            );
            c.backspace();
            assert_eq!(c.buffer_len(), 0);
        }
        {
            let mut c = controller("hunter2");
            c.push_str("a\nb\tc");
            // Only the three printable chars are kept.
            assert_eq!(c.buffer_len(), 3);
        }
        {
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
            // "p" + submit is wrong ("pw" expected): submit is async, so it
            // returns StillLocked; the failed result arrives via the poll.
            assert_eq!(c.handle_key(LockKey::Submit), UnlockOutcome::StillLocked);
            assert_eq!(poll_to_completion(&mut c), UnlockOutcome::StillLocked);
            // Now type the correct password and submit → unlock via the poll.
            c.handle_key(LockKey::Text("pw".into()));
            assert_eq!(c.handle_key(LockKey::Submit), UnlockOutcome::StillLocked);
            assert_eq!(poll_to_completion(&mut c), UnlockOutcome::Unlocked);
        }
        {
            let mut c = controller("pw");
            c.push_str("ab");
            assert_eq!(c.handle_key(LockKey::Ignored), UnlockOutcome::StillLocked);
            assert_eq!(c.buffer_len(), 2);
        }
    }

    /// A verifier that parks in `verify` until the test releases a gate, so
    /// tests can observe the "verify in flight" window deterministically and
    /// prove the controller does not block on it.
    struct GateVerifier {
        gate: Arc<(Mutex<bool>, Condvar)>,
        result: Result<(), String>,
    }

    impl GateVerifier {
        fn new(result: Result<(), String>) -> (Self, Arc<(Mutex<bool>, Condvar)>) {
            let gate = Arc::new((Mutex::new(false), Condvar::new()));
            (
                Self {
                    gate: Arc::clone(&gate),
                    result,
                },
                gate,
            )
        }
    }

    impl PasswordVerifier for GateVerifier {
        fn verify(&self, _username: &str, _password: &str) -> Result<(), String> {
            let (lock, cvar) = &*self.gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cvar.wait(released).unwrap();
            }
            // Nothing below touches the mutex — drop the guard before the
            // (lock-independent) clone rather than holding it needlessly.
            drop(released);
            self.result.clone()
        }
    }

    fn release(gate: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, cvar) = &**gate;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }

    #[test]
    fn async_verification_never_blocks_and_can_be_cancelled() {
        {
            let (v, gate) = GateVerifier::new(Ok(()));
            let mut c = LockController::new(Arc::new(v), "alice");
            c.push_str("pw");
            // Submit returns while the verifier is still parked on its gate,
            // which only this thread releases: it cannot have waited on the
            // verify, so the compositor run loop would not be blocked here.
            assert_eq!(c.submit(), UnlockOutcome::StillLocked);
            // The verify is in flight; polling never unlocks while it is parked.
            assert!(c.is_verifying());
            assert_eq!(c.poll_verification(), None);
            assert_eq!(c.poll_verification(), None);
            assert!(c.is_verifying(), "still locked + verifying while parked");
            // Release the worker: only now does a poll observe success and unlock.
            release(&gate);
            assert_eq!(poll_to_completion(&mut c), UnlockOutcome::Unlocked);
            assert!(!c.is_verifying());
        }
        {
            let (v, gate) = GateVerifier::new(Err("Incorrect username or password".to_string()));
            let mut c = LockController::new(Arc::new(v), "alice");
            c.push_str("wrong");
            assert_eq!(c.submit(), UnlockOutcome::StillLocked);
            assert_eq!(c.poll_verification(), None, "no result while parked");
            release(&gate);
            assert_eq!(poll_to_completion(&mut c), UnlockOutcome::StillLocked);
            assert_eq!(c.error(), Some("Incorrect username or password"));
            assert!(!c.is_verifying());
            assert_eq!(c.buffer_len(), 0);
        }
        {
            let (v, gate) = GateVerifier::new(Ok(()));
            let mut c = LockController::new(Arc::new(v), "alice");
            c.push_str("pw");
            assert_eq!(c.submit(), UnlockOutcome::StillLocked);
            assert!(c.is_verifying());
            // Keystrokes during a verify must not edit the field or start a
            // second attempt.
            assert_eq!(
                c.handle_key(LockKey::Text("x".into())),
                UnlockOutcome::StillLocked
            );
            assert_eq!(c.buffer_len(), 0, "text is swallowed while verifying");
            assert_eq!(c.handle_key(LockKey::Submit), UnlockOutcome::StillLocked);
            assert!(
                c.is_verifying(),
                "a second submit does not replace the verify"
            );
            release(&gate);
            assert_eq!(poll_to_completion(&mut c), UnlockOutcome::Unlocked);
        }
        {
            let (v, gate) = GateVerifier::new(Ok(()));
            let mut c = LockController::new(Arc::new(v), "alice");
            c.push_str("pw");
            assert_eq!(c.submit(), UnlockOutcome::StillLocked);
            assert!(c.is_verifying());
            // A fresh lock resets: the in-flight verify is abandoned and can no
            // longer unlock the session.
            c.reset();
            assert!(!c.is_verifying());
            release(&gate);
            // Even after the worker finishes, a poll never unlocks — interest was
            // dropped, so there is nothing to observe.
            assert_eq!(c.poll_verification(), None);
        }
    }
}
