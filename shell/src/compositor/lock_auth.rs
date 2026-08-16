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
//!   It is a developer-build affordance: `QOS_LOCK_TEST_PASSWORD` and
//!   `QOS_UNLOCK_HELPER` are both read only when
//!   [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`] is set, so a shipped
//!   `--release` binary cannot be talked into accepting a password of the
//!   caller's choosing. See that module for why the environment is not a
//!   trust boundary here.
//!
//! [`verifier_from_env`] picks between them. The default when neither is
//! configured is **fail-closed** ([`UnavailableVerifier`]): if there is no
//! way to verify a credential, no password is ever accepted — a lock with no
//! verifier stays locked rather than silently letting anyone in.
//!
//! # In-session verify (`HelperVerifier`)
//!
//! A *running* session exposes no greetd socket, so [`GreetdVerifier`] is
//! only reachable during login. To verify the logged-in user's password
//! *inside* a live session, [`HelperVerifier`] spawns the small privileged
//! [`qos-unlock-helper`] binary: it passes the username on argv, the password
//! on the helper's **stdin** (never argv, so it never lands in
//! `/proc/*/cmdline`), and maps the helper's exit code to a verify result.
//! The compositor itself links no PAM — the helper is the only piece that
//! touches the auth stack, keeping the compositor's attack surface small.
//! Any failure to run the check is fail-closed and non-leaky.
//!
//! [`qos-unlock-helper`]: ../../../../crates/session/qos-unlock-helper

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The single non-leaky rejection line shown for any wrong credential,
/// shared by every verifier so unlock never reveals more than login does.
pub const INCORRECT_MSG: &str = "Incorrect username or password";

/// The non-leaky line shown when verification could not be carried out at
/// all (no backend, helper missing, timeout). Distinct from a *rejected*
/// credential, but equally fail-closed: the session stays locked.
pub const UNAVAILABLE_MSG: &str = "Password verification is unavailable";

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
            Err(INCORRECT_MSG.to_string())
        }
    }
}

/// Env var pointing at the in-session verify helper binary
/// ([`HelperVerifier`]). In a **developer build** it takes precedence over
/// the default install path; a `--release` build never reads it (see
/// [`crate::compositor::dev_env`]), because "name the program that decides
/// whether this password unlocks the screen" is not a decision the process
/// environment gets to make on a machine someone actually uses.
pub const UNLOCK_HELPER_ENV: &str = "QOS_UNLOCK_HELPER";

/// Where the helper is installed — by the OS image, and by the host session
/// RPM (`dist/rpm/qos-session.spec`), which puts it at exactly this path so
/// no override is needed to unlock a session on a non-qOS host.
pub const DEFAULT_UNLOCK_HELPER: &str = "/usr/libexec/qos-unlock-helper";

/// Upper bound on a single verify. Generous enough for a slow PAM stack
/// (the `pam_unix` fail delay is ~2s; a fingerprint prompt is longer) but
/// bounded so a wedged helper cannot pin the unlock path forever. Because
/// verification runs on a worker thread (see
/// [`crate::compositor::lock_controller`]), this bound never blocks the
/// compositor loop — it just fails the attempt closed if the helper hangs.
const HELPER_TIMEOUT: Duration = Duration::from_secs(20);

/// The **real in-session** verifier: spawns [`DEFAULT_UNLOCK_HELPER`] (or the
/// [`UNLOCK_HELPER_ENV`] override), writes the password to its stdin, and
/// maps the exit status:
///
/// * `0` → `Ok(())` (authenticated).
/// * `1` → `Err(INCORRECT_MSG)` (PAM denied — non-leaky).
/// * anything else / spawn error / timeout → `Err(UNAVAILABLE_MSG)`
///   (fail-closed: the check could not be trusted, so stay locked).
///
/// The password is passed only on stdin, so it never appears in the argv of
/// the spawned process. The helper's stdout/stderr are discarded, so no raw
/// PAM text can leak into the UI.
#[derive(Debug, Clone)]
pub struct HelperVerifier {
    /// The command to run. `program[0]` is the executable; `program[1..]`
    /// are leading args. The username is appended as the final argv entry.
    /// A single-element vec is the production case
    /// (`[DEFAULT_UNLOCK_HELPER]`); tests inject a fake helper here.
    program: Vec<String>,
    timeout: Duration,
}

impl HelperVerifier {
    /// Build a verifier that runs `program` (executable + leading args).
    pub fn new(program: Vec<String>) -> Self {
        Self {
            program,
            timeout: HELPER_TIMEOUT,
        }
    }

    /// Override the per-attempt timeout (tests use a short one).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Poll a spawned child to completion or `None` on timeout. Uses
/// `try_wait` so the parent keeps the handle and can kill a hung child.
/// Runs on the verify worker thread, so the short sleep never touches the
/// compositor loop.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

impl PasswordVerifier for HelperVerifier {
    fn verify(&self, username: &str, password: &str) -> Result<(), String> {
        if self.program.is_empty() {
            return Err(UNAVAILABLE_MSG.to_string());
        }
        let mut cmd = Command::new(&self.program[0]);
        cmd.args(&self.program[1..]);
        cmd.arg(username);
        cmd.stdin(Stdio::piped());
        // Discard the helper's output so no raw PAM/diagnostic text can leak.
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            // Could not even launch the helper — fail closed, non-leaky.
            Err(_) => return Err(UNAVAILABLE_MSG.to_string()),
        };

        // Hand the password over on stdin (newline-terminated), then close
        // the pipe so the helper sees EOF. The payload is tiny (< pipe
        // buffer), so this write never blocks.
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(password.as_bytes());
            let _ = stdin.write_all(b"\n");
            // `stdin` drops here → EOF for the helper.
        }

        match wait_with_timeout(&mut child, self.timeout) {
            Some(status) if status.success() => Ok(()),
            Some(status) if status.code() == Some(1) => Err(INCORRECT_MSG.to_string()),
            // Exit 2, another code, or a signal: the check itself failed.
            Some(_) => Err(UNAVAILABLE_MSG.to_string()),
            None => {
                // Timed out — kill the hung helper and fail closed.
                let _ = child.kill();
                let _ = child.wait();
                Err(UNAVAILABLE_MSG.to_string())
            }
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
        Err(UNAVAILABLE_MSG.to_string())
    }
}

/// Env var injecting a fixed unlock password ([`FixedVerifier`]) for the
/// headless harness / display-less dev. Read only in developer builds — a
/// `--release` binary ignores it entirely, so a shipped session cannot be
/// handed a password to accept. See [`crate::compositor::dev_env`].
pub const LOCK_TEST_PASSWORD_ENV: &str = "QOS_LOCK_TEST_PASSWORD";

/// Which verifier [`select_verifier`] picked. Named so the selection policy
/// can be asserted on directly, without having to introspect a
/// `dyn PasswordVerifier`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierChoice {
    /// [`FixedVerifier`] on the given password. Developer builds only.
    Fixed(String),
    /// [`HelperVerifier`] on the given program path.
    Helper(String),
    /// [`GreetdVerifier`] on the given socket path.
    Greetd(String),
    /// [`UnavailableVerifier`] — nothing can verify, so nothing unlocks.
    Unavailable,
}

/// The verifier selection policy, as a pure function of its inputs.
///
/// `dev_overrides` is [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`]:
/// when it is `false` the two developer variables are not consulted at all,
/// whatever they contain. Passing it in rather than reading the `cfg` here
/// is what lets the release-build behaviour be tested from a debug test run.
///
/// Priority order:
///
/// 1. (dev builds only) `QOS_LOCK_TEST_PASSWORD` set → [`VerifierChoice::Fixed`].
/// 2. (dev builds only) `QOS_UNLOCK_HELPER` set → [`VerifierChoice::Helper`]
///    on that path, for a helper installed somewhere other than the default.
/// 3. [`DEFAULT_UNLOCK_HELPER`] exists on disk → [`VerifierChoice::Helper`]
///    (the **real in-session PAM path**, on the OS image and on a host that
///    installed the session RPM).
/// 4. `GREETD_SOCK` set → [`VerifierChoice::Greetd`] (login-time greetd path;
///    a live session has no such socket, so this is effectively login-only).
/// 5. else → [`VerifierChoice::Unavailable`] (fail-closed).
pub fn select_verifier(
    dev_overrides: bool,
    test_password: Option<String>,
    helper_override: Option<String>,
    helper_installed: bool,
    greetd_sock: Option<String>,
) -> VerifierChoice {
    if dev_overrides {
        if let Some(pw) = test_password {
            return VerifierChoice::Fixed(pw);
        }
        if let Some(prog) = helper_override {
            if !prog.trim().is_empty() {
                return VerifierChoice::Helper(prog);
            }
        }
    }
    if helper_installed {
        return VerifierChoice::Helper(DEFAULT_UNLOCK_HELPER.to_string());
    }
    if let Some(sock) = greetd_sock {
        return VerifierChoice::Greetd(sock);
    }
    VerifierChoice::Unavailable
}

/// Select a verifier from the environment via [`select_verifier`].
///
/// Returned behind an `Arc` so the run loop and the verify worker thread can
/// share one verifier.
pub fn verifier_from_env() -> Arc<dyn PasswordVerifier> {
    let dev = crate::compositor::dev_env::DEV_ENV_OVERRIDES;
    // Reading the two developer variables is itself skipped in a shipped
    // build, so they cannot influence anything — not even by being present.
    let (test_password, helper_override) = if dev {
        (
            std::env::var(LOCK_TEST_PASSWORD_ENV).ok(),
            std::env::var(UNLOCK_HELPER_ENV).ok(),
        )
    } else {
        (None, None)
    };
    let choice = select_verifier(
        dev,
        test_password,
        helper_override,
        Path::new(DEFAULT_UNLOCK_HELPER).exists(),
        std::env::var("GREETD_SOCK").ok(),
    );
    match choice {
        VerifierChoice::Fixed(pw) => Arc::new(FixedVerifier::new(pw)),
        VerifierChoice::Helper(prog) => Arc::new(HelperVerifier::new(vec![prog])),
        VerifierChoice::Greetd(sock) => Arc::new(GreetdVerifier::new(sock)),
        VerifierChoice::Unavailable => Arc::new(UnavailableVerifier),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- select_verifier: the developer-override gate -----------------
    //
    // These drive the policy function with `dev_overrides` explicitly, so
    // the *shipped* behaviour is exercised from an ordinary debug test run
    // instead of only being reachable in a build the suite never runs.

    #[test]
    fn dev_build_lets_the_test_password_win() {
        assert_eq!(
            select_verifier(
                true,
                Some("letmein".into()),
                Some("/tmp/helper".into()),
                true,
                Some("/run/greetd.sock".into()),
            ),
            VerifierChoice::Fixed("letmein".into()),
        );
    }

    #[test]
    fn shipped_build_ignores_the_test_password_entirely() {
        // Same inputs, gate closed. The injected password must not be able
        // to displace the real helper — this is the whole point of the gate.
        assert_eq!(
            select_verifier(
                false,
                Some("letmein".into()),
                None,
                true,
                Some("/run/greetd.sock".into()),
            ),
            VerifierChoice::Helper(DEFAULT_UNLOCK_HELPER.into()),
        );
    }

    #[test]
    fn shipped_build_ignores_the_helper_override_entirely() {
        assert_eq!(
            select_verifier(false, None, Some("/tmp/evil".into()), true, None),
            VerifierChoice::Helper(DEFAULT_UNLOCK_HELPER.into()),
        );
    }

    #[test]
    fn shipped_build_with_no_helper_installed_falls_closed_not_to_the_override() {
        // With nothing real to verify against, an ignored override must
        // leave the session unverifiable rather than becoming the fallback.
        assert_eq!(
            select_verifier(
                false,
                Some("letmein".into()),
                Some("/tmp/evil".into()),
                false,
                None,
            ),
            VerifierChoice::Unavailable,
        );
    }

    #[test]
    fn dev_helper_override_beats_the_installed_helper() {
        assert_eq!(
            select_verifier(true, None, Some("/tmp/helper".into()), true, None),
            VerifierChoice::Helper("/tmp/helper".into()),
        );
    }

    #[test]
    fn blank_helper_override_is_not_a_path() {
        // An empty or whitespace-only override would otherwise spawn "" and
        // fail closed on every attempt; fall through to the real helper.
        assert_eq!(
            select_verifier(true, None, Some("   ".into()), true, None),
            VerifierChoice::Helper(DEFAULT_UNLOCK_HELPER.into()),
        );
    }

    #[test]
    fn greetd_is_the_login_time_fallback_when_no_helper_is_installed() {
        assert_eq!(
            select_verifier(false, None, None, false, Some("/run/greetd.sock".into())),
            VerifierChoice::Greetd("/run/greetd.sock".into()),
        );
    }

    #[test]
    fn nothing_configured_is_fail_closed() {
        assert_eq!(
            select_verifier(false, None, None, false, None),
            VerifierChoice::Unavailable,
        );
    }

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

    // ---- HelperVerifier (real subprocess spawn) ----------------------
    //
    // These exercise the *real* production mechanism — spawn a helper, feed
    // the password on stdin, map the exit code — against a controllable fake
    // helper script standing in for `qos-unlock-helper`. Only the PAM check
    // itself is injected (the script decides the exit code); the spawn +
    // stdin + exit-code + timeout plumbing is the shipped path.

    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static FAKE_SEQ: AtomicU32 = AtomicU32::new(0);

    /// Write a `/bin/sh` script with `body` and return a [`HelperVerifier`]
    /// that runs it via `sh <script>` (username appended as the trailing
    /// argv, password on stdin — exactly like the real helper). Invoking
    /// through `/bin/sh` means the just-written file is *read* by sh, never
    /// directly `exec`'d, so it can't hit an `ETXTBSY` race with a concurrent
    /// fork while the file is fresh.
    fn fake_helper(body: &str) -> (HelperVerifier, PathBuf) {
        let n = FAKE_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "qos-fake-unlock-helper-{}-{}.sh",
            std::process::id(),
            n
        ));
        let mut f = std::fs::File::create(&path).expect("create fake helper");
        write!(f, "{body}").expect("write fake helper");
        f.sync_all().ok();
        drop(f);
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod fake helper");
        let v = HelperVerifier::new(vec!["/bin/sh".to_string(), path.to_string_lossy().into_owned()]);
        (v, path)
    }

    #[test]
    #[serial_test::serial(lock_helper)]
    fn helper_exit0_unlocks_exit1_stays_locked_with_nonleaky_msg() {
        // Read the password from stdin; accept exactly one value.
        let (v, path) = fake_helper("read -r pw\n[ \"$pw\" = \"s3cr3t\" ] && exit 0\nexit 1\n");
        assert!(v.verify("alice", "s3cr3t").is_ok(), "correct password unlocks");
        // Wrong → denied, with the shared non-leaky wording.
        assert_eq!(v.verify("alice", "wrong").unwrap_err(), INCORRECT_MSG);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[serial_test::serial(lock_helper)]
    fn helper_error_exit_is_unavailable_fail_closed() {
        // Exit 2 = "could not run the check" → fail closed, distinct wording.
        let (v, path) = fake_helper("read -r pw\nexit 2\n");
        assert_eq!(v.verify("alice", "whatever").unwrap_err(), UNAVAILABLE_MSG);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[serial_test::serial(lock_helper)]
    fn helper_spawn_failure_is_unavailable_fail_closed() {
        // A non-existent program can never authenticate anyone.
        let v = HelperVerifier::new(vec![
            "/nonexistent/definitely/not/a/qos-unlock-helper".to_string(),
        ]);
        assert_eq!(v.verify("alice", "s3cr3t").unwrap_err(), UNAVAILABLE_MSG);
    }

    #[test]
    #[serial_test::serial(lock_helper)]
    fn helper_timeout_is_unavailable_and_returns_promptly() {
        // A helper that never answers must not hang the verify — the bound
        // fires and the attempt fails closed. `exec sleep` so the killed child
        // is the sleeper itself (no orphaned grandchild).
        let (v, path) = fake_helper("exec sleep 30\n");
        let v = v.with_timeout(Duration::from_millis(150));
        let start = Instant::now();
        assert_eq!(v.verify("alice", "s3cr3t").unwrap_err(), UNAVAILABLE_MSG);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "verify must return promptly on timeout, took {:?}",
            start.elapsed()
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[serial_test::serial(lock_helper)]
    fn helper_receives_password_on_stdin_not_argv() {
        // Prove the secret is passed on stdin only: the script records its
        // argv to a file; the password must be absent from it and the
        // username present.
        let n = FAKE_SEQ.fetch_add(1, Ordering::Relaxed);
        let args_out = std::env::temp_dir().join(format!(
            "qos-fake-helper-argv-{}-{}.txt",
            std::process::id(),
            n
        ));
        let (v, path) = fake_helper(&format!(
            "printf '%s' \"$*\" > {out}\nread -r pw\nexit 0\n",
            out = args_out.to_string_lossy()
        ));
        assert!(v.verify("bob", "topsecret-pw").is_ok());
        let recorded = std::fs::read_to_string(&args_out).unwrap_or_default();
        assert!(
            !recorded.contains("topsecret-pw"),
            "password must never appear in the helper's argv (got {recorded:?})"
        );
        assert!(
            recorded.contains("bob"),
            "username should be passed as argv (got {recorded:?})"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&args_out);
    }

    #[test]
    fn helper_verifier_is_send_sync_for_worker_thread() {
        // The async unlock path moves an `Arc<dyn PasswordVerifier>` onto a
        // worker thread, so the helper must be `Send + Sync`.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HelperVerifier>();
    }
}
