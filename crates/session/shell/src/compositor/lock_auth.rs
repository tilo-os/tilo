//! Session-lock credential verification.
//!
//! The lock screen ([`crate::compositor::lock_controller::LockController`]) must prove the
//! person at the keyboard is the session owner before clearing the lock. [`PasswordVerifier`]
//! is the injection seam for that proof, so the lock state machine can be driven with a
//! deterministic verifier in tests while the shipped binary talks to the auth stack.
//!
//! [`GreetdVerifier`] reuses the greeter's greetd/PAM conversation but stops
//! at `Success`/`Error` instead of starting a session, which is why unlock and login share one
//! non-leaky failure message. [`FixedVerifier`] is the injected headless-test path, selected by
//! `TILO_LOCK_TEST_PASSWORD` and read only when [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`]
//! is set, so a `--release` binary can't be talked into accepting a caller-chosen password.
//! [`verifier_from_env`] picks between them; the default with neither configured is fail-closed ([`UnavailableVerifier`]).
//!
//! A running session exposes no greetd socket, so verifying a live session's password instead
//! spawns the privileged [`tilo-unlock-helper`] binary via [`HelperVerifier`]: username on argv,
//! password on **stdin** (never argv, so it never lands in `/proc/*/cmdline`), exit code mapped
//! to a result. The compositor itself links no PAM — the helper is the only piece that touches the auth stack.
//!
//! [`tilo-unlock-helper`]: ../../../../crates/session/tilo-unlock-helper

#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use zeroize::Zeroizing;

/// The single non-leaky rejection line shown for any wrong credential, shared by every verifier so unlock never reveals more than login does.
pub const INCORRECT_MSG: &str = "Incorrect username or password";

/// The non-leaky line shown when verification could not be carried out at all (no backend, helper missing, timeout) — distinct from a *rejected* credential, but equally fail-closed.
pub const UNAVAILABLE_MSG: &str = "Password verification is unavailable";

/// Abstracts "is `password` correct for `username`?" so the lock state machine can be
/// unit-tested with an injected verifier and run against PAM in production. `Send + Sync` so a verifier can be shared (`Arc`) and later moved onto a worker thread.
pub trait PasswordVerifier: Send + Sync {
    /// `Ok(())` iff the credential is valid. `Err` carries the *user-facing* one-liner for the lock screen — it must never leak which field was wrong or any raw PAM text.
    ///
    /// # Errors
    ///
    /// Returns the non-leaky rejection or unavailability message ([`INCORRECT_MSG`] /
    /// [`UNAVAILABLE_MSG`]) when `password` is wrong for `username`, or verification could not be
    /// carried out at all.
    fn verify(&self, username: &str, password: &str) -> Result<(), String>;
}

/// Reuses the greeter's greetd/PAM `verify_password` exchange against the socket at `sock` (typically `$GREETD_SOCK`).
#[derive(Debug, Clone)]
#[cfg(target_os = "linux")]
pub struct GreetdVerifier {
    sock: String,
}

#[cfg(target_os = "linux")]
impl GreetdVerifier {
    /// Construct a verifier that talks to the greetd socket at `sock`.
    pub fn new(sock: impl Into<String>) -> Self {
        Self { sock: sock.into() }
    }
}

#[cfg(target_os = "linux")]
impl PasswordVerifier for GreetdVerifier {
    fn verify(&self, username: &str, password: &str) -> Result<(), String> {
        greetd_client::verify_password(&self.sock, username, password)
            // `user_message` is the same non-leaky copy the login box shows ("Incorrect username or password"), so unlock never reveals more than login does.
            .map_err(|e| e.user_message())
    }
}

/// A verifier that accepts exactly one known password with no I/O, via `TILO_LOCK_TEST_PASSWORD`
/// — lets the lock/unlock state machine run in headless tests without a PAM/greetd socket. Never used when the env var is unset.
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

/// Env var pointing at the in-session verify helper binary ([`HelperVerifier`]). In a
/// **developer build** it takes precedence over the default install path; a `--release` build
/// never reads it — "name the program that decides whether this password unlocks the screen" is not a decision the process environment gets to make on a real machine.
pub const UNLOCK_HELPER_ENV: &str = "TILO_UNLOCK_HELPER";

/// Where the helper is installed — by the OS image and the host session RPM (`dist/rpm/tilo-session.spec`), at exactly this path so no override is needed on a non-Tilo host.
pub const DEFAULT_UNLOCK_HELPER: &str = match option_env!("TILO_UNLOCK_HELPER_PATH") {
    Some(path) => path,
    None => "/usr/libexec/tilo-unlock-helper",
};

/// Upper bound on a single verify. Generous for a slow PAM stack (`pam_unix` fail delay is
/// ~2s; a fingerprint prompt is longer) but bounded so a wedged helper can't pin the unlock path
/// forever. Runs on a worker thread (see [`crate::compositor::lock_controller`]), so this never blocks the compositor loop — it just fails the attempt closed if the helper hangs.
const HELPER_TIMEOUT: Duration = Duration::from_secs(20);

/// The **real in-session** verifier: spawns [`DEFAULT_UNLOCK_HELPER`] (or the
/// [`UNLOCK_HELPER_ENV`] override), writes the password to its stdin, and maps the exit status —
/// `0` → `Ok(())`, `1` → `Err(INCORRECT_MSG)` (PAM denied), anything else / spawn error / timeout → `Err(UNAVAILABLE_MSG)` (fail-closed).
///
/// The password is passed only on stdin, never in argv; the helper's stdout/stderr are discarded so no raw PAM text can leak into the UI.
#[derive(Debug, Clone)]
pub struct HelperVerifier {
    /// The command to run: `program[0]` is the executable, `program[1..]` leading args, username
    /// appended as the final argv entry. Production is a single-element vec (`[DEFAULT_UNLOCK_HELPER]`); tests inject a fake helper.
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
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl PasswordVerifier for HelperVerifier {
    fn verify(&self, username: &str, password: &str) -> Result<(), String> {
        let Some((program, args)) = self.program.split_first() else {
            return Err(UNAVAILABLE_MSG.to_string());
        };
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd.arg(username);
        let mut input = Zeroizing::new(Vec::with_capacity(password.len().saturating_add(1)));
        input.extend_from_slice(password.as_bytes());
        input.push(b'\n');
        let Ok(output) =
            bounded_command::run_prepared(cmd, Some(input.as_slice()), 1024, self.timeout)
        else {
            return Err(UNAVAILABLE_MSG.to_string());
        };
        match output.status.code() {
            Some(0) => Ok(()),
            Some(1) => Err(INCORRECT_MSG.to_string()),
            // Exit 2, another code, or a signal: the check itself failed.
            _ => Err(UNAVAILABLE_MSG.to_string()),
        }
    }
}

/// Fail-closed verifier used when no verifier is configured: every attempt is rejected, so a locked session with no auth backend stays locked.
#[derive(Debug, Clone, Default)]
pub struct UnavailableVerifier;

impl PasswordVerifier for UnavailableVerifier {
    fn verify(&self, _username: &str, _password: &str) -> Result<(), String> {
        Err(UNAVAILABLE_MSG.to_string())
    }
}

/// Env var injecting a fixed unlock password ([`FixedVerifier`]) for the headless harness /
/// display-less dev. Read only in developer builds — a `--release` binary ignores it entirely. See [`crate::compositor::dev_env`].
pub const LOCK_TEST_PASSWORD_ENV: &str = "TILO_LOCK_TEST_PASSWORD";

/// Which verifier [`select_verifier`] picked — named so the selection policy can be asserted on directly, without introspecting a `dyn PasswordVerifier`.
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

/// The verifier selection policy, as a pure function of its inputs. `dev_overrides` is
/// [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`]: when `false` the two developer variables aren't consulted at all — passed in rather than read from `cfg` here, so release-build behaviour can be tested from a debug run.
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
        if let Some(prog) = helper_override
            && !prog.trim().is_empty()
        {
            return VerifierChoice::Helper(prog);
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

/// Select a verifier from the environment via [`select_verifier`]. Returned behind an `Arc` so the run loop and the verify worker thread can share one verifier.
#[cfg(target_os = "linux")]
pub fn verifier_from_env() -> Arc<dyn PasswordVerifier> {
    let dev = crate::compositor::dev_env::DEV_ENV_OVERRIDES;
    // Reading the two developer variables is itself skipped in a shipped build, so they cannot influence anything — not even by being present.
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

#[cfg(not(target_os = "linux"))]
pub fn verifier_from_env() -> std::sync::Arc<dyn PasswordVerifier> {
    std::sync::Arc::new(UnavailableVerifier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    // select_verifier: the developer-override gate — drives the policy function with `dev_overrides`
    // explicitly, so shipped behaviour is exercised from an ordinary debug test run instead of only being reachable in a build the suite never runs.

    #[test]
    fn lock_verifier_selection_is_gated_by_build_and_fails_closed() {
        // Phase `dev_build_lets_the_test_password_win`.
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
        // Phase `shipped_build_ignores_the_test_password_entirely`.
        // Same inputs, gate closed. The injected password must not be able to
        // displace the configured helper.
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
        // Phase `shipped_build_ignores_the_helper_override_entirely`.
        assert_eq!(
            select_verifier(false, None, Some("/tmp/evil".into()), true, None),
            VerifierChoice::Helper(DEFAULT_UNLOCK_HELPER.into()),
        );
        // Phase `shipped_build_with_no_helper_installed_falls_closed_not_to_the_override`.
        // With no verifier to use, an ignored override must leave the session
        // unverifiable rather than becoming the fallback.
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
        // Phase `dev_helper_override_beats_the_installed_helper`.
        assert_eq!(
            select_verifier(true, None, Some("/tmp/helper".into()), true, None),
            VerifierChoice::Helper("/tmp/helper".into()),
        );
        // Phase `blank_helper_override_is_not_a_path`.
        // An empty or whitespace-only override would otherwise spawn "" and
        // fail closed on every attempt; fall through to the helper.
        assert_eq!(
            select_verifier(true, None, Some("   ".into()), true, None),
            VerifierChoice::Helper(DEFAULT_UNLOCK_HELPER.into()),
        );
        // Phase `greetd_is_the_login_time_fallback_when_no_helper_is_installed`.
        assert_eq!(
            select_verifier(false, None, None, false, Some("/run/greetd.sock".into())),
            VerifierChoice::Greetd("/run/greetd.sock".into()),
        );
        // Phase `nothing_configured_is_fail_closed`.
        assert_eq!(
            select_verifier(false, None, None, false, None),
            VerifierChoice::Unavailable,
        );
    }

    #[test]
    fn fixed_and_unavailable_verifiers_are_exact_and_send() {
        // Phase `fixed_verifier_accepts_only_the_configured_password`.
        {
            let v = FixedVerifier::new("hunter2");
            assert!(v.verify("alice", "hunter2").is_ok());
            assert!(v.verify("alice", "wrong").is_err());
            // The rejection message is the non-leaky login copy.
            assert_eq!(
                v.verify("alice", "wrong").unwrap_err(),
                "Incorrect username or password"
            );
        }
        // Phase `unavailable_verifier_rejects_everything_fail_closed`.
        {
            let v = UnavailableVerifier;
            assert!(v.verify("alice", "").is_err());
            assert!(v.verify("alice", "anything").is_err());
        }
        // Phase `helper_verifier_is_send_sync_for_worker_thread`.
        {
            // The async unlock path moves an `Arc<dyn PasswordVerifier>` onto a worker thread, so the helper must be `Send + Sync`.
            fn assert_send_sync<T: Send + Sync>() {}
            assert_send_sync::<HelperVerifier>();
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn verifier_from_env_prefers_injected_fixed_password() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(LOCK_TEST_PASSWORD_ENV, "letmein");
        let v = verifier_from_env();
        assert!(v.verify("bob", "letmein").is_ok());
        assert!(v.verify("bob", "nope").is_err());
    }

    // HelperVerifier (real subprocess spawn): exercises the *real* production mechanism — spawn a helper, feed the password on stdin, map the exit code — against a fake helper script standing in for `tilo-unlock-helper`.

    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static FAKE_SEQ: AtomicU32 = AtomicU32::new(0);

    /// Write a `/bin/sh` script with `body` and return a [`HelperVerifier`] that runs it via
    /// `sh <script>` (username on argv, password on stdin, like the helper);
    /// invoked through `/bin/sh` so the freshly-written file is *read*, not directly `exec`'d,
    /// avoiding an `ETXTBSY` race with a concurrent fork.
    fn fake_helper(body: &str) -> (HelperVerifier, PathBuf) {
        let n = FAKE_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "tilo-fake-unlock-helper-{}-{}.sh",
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
        let v = HelperVerifier::new(vec![
            "/bin/sh".to_string(),
            path.to_string_lossy().into_owned(),
        ]);
        (v, path)
    }

    #[test]
    #[serial_test::serial(lock_helper)]
    fn lock_helper_exit_status_timeout_and_stdin_contract() {
        // Phase `helper_exit0_unlocks_exit1_stays_locked_with_nonleaky_msg`.
        {
            // Read the password from stdin; accept exactly one value.
            let (v, path) = fake_helper("read -r pw\n[ \"$pw\" = \"s3cr3t\" ] && exit 0\nexit 1\n");
            assert!(
                v.verify("alice", "s3cr3t").is_ok(),
                "correct password unlocks"
            );
            // Wrong → denied, with the shared non-leaky wording.
            assert_eq!(v.verify("alice", "wrong").unwrap_err(), INCORRECT_MSG);
            let _ = std::fs::remove_file(&path);
        }
        // Phase `helper_error_exit_is_unavailable_fail_closed`.
        {
            // Exit 2 = "could not run the check" → fail closed, distinct wording.
            let (v, path) = fake_helper("read -r pw\nexit 2\n");
            assert_eq!(v.verify("alice", "whatever").unwrap_err(), UNAVAILABLE_MSG);
            let _ = std::fs::remove_file(&path);
        }
        // Phase `helper_spawn_failure_is_unavailable_fail_closed`.
        {
            // A non-existent program can never authenticate anyone.
            let v = HelperVerifier::new(vec![
                "/nonexistent/definitely/not/a/tilo-unlock-helper".to_string(),
            ]);
            assert_eq!(v.verify("alice", "s3cr3t").unwrap_err(), UNAVAILABLE_MSG);
        }
        // Phase `helper_timeout_is_unavailable_and_returns_promptly`.
        {
            // A helper that never answers must not hang the verify — the bound fires and the attempt fails closed. `exec sleep` so the killed child is the sleeper itself (no orphaned grandchild).
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
        // Phase `helper_receives_password_on_stdin_not_argv`.
        {
            // Prove the secret is passed on stdin only: the script records its argv to a file; the password must be absent from it and the username present.
            let n = FAKE_SEQ.fetch_add(1, Ordering::Relaxed);
            let args_out = std::env::temp_dir().join(format!(
                "tilo-fake-helper-argv-{}-{}.txt",
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
    }
}
