//! Session-lock credential verification.
//!
//! The lock screen ([`crate::compositor::lock_controller::LockController`])
//! must prove the person at the keyboard is the session owner before it
//! clears the lock. This module is the **injection seam** for that proof:
//! the [`PasswordVerifier`] trait abstracts "is this password correct for
//! this user?" so the compositor's lock state machine can be driven with a
//! deterministic fake in tests while the shipped binary talks to the real
//! auth stack.
//!
//! # Real vs. injected
//!
//! * [`GreetdVerifier`] — the **real** path. It reuses the greeter's greetd
//!   conversation ([`greeter::greetd_client::verify_password`]), which runs
//!   the same PAM stack login does but stops at the authoritative
//!   `Success`/`Error` instead of starting a session. This is why unlock and
//!   login share one non-leaky failure message.
//! * [`FixedVerifier`] — the **injected** path for headless tests and
//!   display-less dev sessions, selected by `QOS_LOCK_TEST_PASSWORD`. It
//!   compares against a known constant with no I/O, so the lock/unlock state
//!   machine is exercised on the real code path without a PAM/greetd socket.
//!
//! [`verifier_from_env`] picks between them. The default when neither is
//! configured is **fail-closed** ([`UnavailableVerifier`]): if there is no
//! way to verify a credential, no password is ever accepted — a lock with no
//! verifier stays locked rather than silently letting anyone in.

use std::sync::Arc;

/// Abstracts "is `password` correct for `username`?" so the lock state
/// machine can be unit-tested with a fake and run against real PAM in
/// production. `Send + Sync` so a single verifier can be shared (`Arc`) and,
/// in a later revision, moved onto a worker thread.
pub trait PasswordVerifier: Send + Sync {
    /// `Ok(())` iff the credential is valid. `Err` carries the *user-facing*
    /// one-liner to show on the lock screen — it must never leak which field
    /// was wrong or any raw PAM text.
    fn verify(&self, username: &str, password: &str) -> Result<(), String>;
}

/// The real verifier: reuses the greeter's greetd/PAM `verify_password`
/// exchange against the socket at `sock` (typically `$GREETD_SOCK`).
#[derive(Debug, Clone)]
pub struct GreetdVerifier {
    sock: String,
}

impl GreetdVerifier {
    /// Construct a verifier that talks to the greetd socket at `sock`.
    pub fn new(sock: impl Into<String>) -> Self {
        Self { sock: sock.into() }
    }
}

impl PasswordVerifier for GreetdVerifier {
    fn verify(&self, username: &str, password: &str) -> Result<(), String> {
        greeter::greetd_client::verify_password(&self.sock, username, password)
            // `user_message` is the same non-leaky copy the login box shows
            // ("Incorrect username or password"), so unlock never reveals
            // more than login does.
            .map_err(|e| e.user_message())
    }
}

/// A verifier that accepts exactly one known password with no I/O. Used by
/// the headless test harness (and display-less dev sessions) via
/// `QOS_LOCK_TEST_PASSWORD`, so the lock/unlock state machine runs on the
/// real code path without a PAM/greetd socket. Never used when
/// `QOS_LOCK_TEST_PASSWORD` is unset.
#[derive(Debug, Clone)]
pub struct FixedVerifier {
    password: String,
}

impl FixedVerifier {
    /// Accept exactly `password`.
    pub fn new(password: impl Into<String>) -> Self {
        Self {
            password: password.into(),
        }
    }
}

impl PasswordVerifier for FixedVerifier {
    fn verify(&self, _username: &str, password: &str) -> Result<(), String> {
        if password == self.password {
            Ok(())
        } else {
            // Match the greetd path's non-leaky wording exactly.
            Err("Incorrect username or password".to_string())
        }
    }
}

/// Fail-closed verifier used when no real or injected verifier is configured:
/// every attempt is rejected, so a locked session with no auth backend stays
/// locked rather than accepting anything. This is the safe default — never
/// silently unlock.
#[derive(Debug, Clone, Default)]
pub struct UnavailableVerifier;

impl PasswordVerifier for UnavailableVerifier {
    fn verify(&self, _username: &str, _password: &str) -> Result<(), String> {
        Err("Password verification is unavailable".to_string())
    }
}

/// Env var injecting a fixed unlock password ([`FixedVerifier`]) for the
/// headless harness / display-less dev. Unset in production.
pub const LOCK_TEST_PASSWORD_ENV: &str = "QOS_LOCK_TEST_PASSWORD";

/// Select a verifier from the environment:
///
/// * `QOS_LOCK_TEST_PASSWORD` set → [`FixedVerifier`] (injected/test).
/// * else `GREETD_SOCK` set → [`GreetdVerifier`] (real PAM path).
/// * else → [`UnavailableVerifier`] (fail-closed).
///
/// Returned behind an `Arc` so the run loop and (future) worker threads can
/// share one verifier.
pub fn verifier_from_env() -> Arc<dyn PasswordVerifier> {
    if let Ok(pw) = std::env::var(LOCK_TEST_PASSWORD_ENV) {
        return Arc::new(FixedVerifier::new(pw));
    }
    if let Ok(sock) = std::env::var("GREETD_SOCK") {
        return Arc::new(GreetdVerifier::new(sock));
    }
    Arc::new(UnavailableVerifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_verifier_accepts_only_the_configured_password() {
        let v = FixedVerifier::new("hunter2");
        assert!(v.verify("alice", "hunter2").is_ok());
        assert!(v.verify("alice", "wrong").is_err());
        // The rejection message is the non-leaky login copy.
        assert_eq!(
            v.verify("alice", "wrong").unwrap_err(),
            "Incorrect username or password"
        );
    }

    #[test]
    fn unavailable_verifier_rejects_everything_fail_closed() {
        let v = UnavailableVerifier;
        assert!(v.verify("alice", "").is_err());
        assert!(v.verify("alice", "anything").is_err());
    }

    #[test]
    fn verifier_from_env_prefers_injected_fixed_password() {
        // Serialised via the process env; keep it self-contained.
        unsafe { std::env::set_var(LOCK_TEST_PASSWORD_ENV, "letmein") };
        let v = verifier_from_env();
        assert!(v.verify("bob", "letmein").is_ok());
        assert!(v.verify("bob", "nope").is_err());
        unsafe { std::env::remove_var(LOCK_TEST_PASSWORD_ENV) };
    }
}
