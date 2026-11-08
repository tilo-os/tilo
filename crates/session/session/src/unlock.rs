//! Whether a password unlocks the session.
//! The session links no PAM: a running session has no greetd socket, so it asks the one privileged
//! helper (`tilo-unlock-helper`), the user name on its command line and the password on its
//! standard input (never on the command line, where `/proc/*/cmdline` would show it), and reads the
//! verdict from its exit status. Where the helper is not installed, the login that started this
//! session is checked the way it was admitted: greetd's own PAM conversation, at the socket it left
//! behind. With neither, nothing unlocks.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use zeroize::Zeroizing;

/// Where the image and the host session package install the helper.
pub const UNLOCK_HELPER: &str = "/usr/libexec/tilo-unlock-helper";

/// The variable a development build reads another helper's path from, answering as
/// [`UNLOCK_HELPER`] does: 0 for the person's password, 1 for another, 2 when it cannot check. The
/// session's end-to-end lock proof uses it, because a container has neither the installed helper's
/// PAM stack nor a login to check against. A production build never reads it
/// (`tools/policy/check_dev_overrides_contract.py`).
pub const UNLOCK_HELPER_ENV: &str = "TILO_UNLOCK_HELPER";

/// Whether this build honours [`UNLOCK_HELPER_ENV`]: development builds and the dev image's release
/// ones, the builds that open the test-control socket.
const OVERRIDABLE: bool = cfg!(any(debug_assertions, feature = "dev-overrides"));

/// Upper bound on one check: a slow PAM stack takes seconds, a wedged helper must not keep the lock
/// screen checking forever.
const HELPER_TIMEOUT: Duration = Duration::from_secs(20);

/// What came of checking a password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It is the person's: unlock.
    Unlocks,
    /// It is not.
    Wrong,
    /// The check could not be made: stay locked.
    Unavailable,
}

/// Checks a password for a user. `Send + Sync` : a check runs off the session's loop.
pub trait PasswordVerifier: Send + Sync {
    fn verify(&self, user: &str, password: &str) -> Verdict;

    /// Whether a check can be made here at all. A session whose verifier cannot take one does not
    /// lock: it could not be left again.
    fn available(&self) -> bool {
        true
    }
}

/// The helper as a verifier: `program[0]` the executable, then leading arguments; the user name
/// comes last.
#[derive(Debug, Clone)]
pub struct HelperVerifier {
    program: Vec<String>,
    timeout: Duration,
}

impl HelperVerifier {
    #[must_use]
    pub fn new(program: Vec<String>) -> Self {
        Self {
            program,
            timeout: HELPER_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl PasswordVerifier for HelperVerifier {
    fn verify(&self, user: &str, password: &str) -> Verdict {
        let Some((program, args)) = self.program.split_first() else {
            return Verdict::Unavailable;
        };
        let mut command = Command::new(program);
        command.args(args).arg(user);
        let mut input = Zeroizing::new(Vec::with_capacity(password.len().saturating_add(1)));
        input.extend_from_slice(password.as_bytes());
        input.push(b'\n');
        let Ok(output) =
            bounded_command::run_prepared(command, Some(input.as_slice()), 1024, self.timeout)
        else {
            return Verdict::Unavailable;
        };
        match output.status.code() {
            Some(0) => Verdict::Unlocks,
            Some(1) => Verdict::Wrong,
            // Exit 2, another code or a signal: the check itself failed.
            _ => Verdict::Unavailable,
        }
    }

    fn available(&self) -> bool {
        self.program
            .first()
            .is_some_and(|program| Path::new(program).is_file())
    }
}

/// The login's own check: greetd's PAM conversation at its socket, the one the login was admitted
/// through, stopped before any session starts.
#[derive(Debug, Clone)]
pub struct GreetdVerifier {
    socket: String,
}

impl GreetdVerifier {
    #[must_use]
    pub fn new(socket: impl Into<String>) -> Self {
        Self {
            socket: socket.into(),
        }
    }
}

impl PasswordVerifier for GreetdVerifier {
    fn verify(&self, user: &str, password: &str) -> Verdict {
        use greetd_client::{AuthError, Denial};
        match greetd_client::verify_password(&self.socket, user, password) {
            Ok(()) => Verdict::Unlocks,
            // PAM refused this password: the person may try again.
            Err(AuthError::Denied {
                reason: Denial::Credentials,
                ..
            }) => Verdict::Wrong,
            // The socket is gone, the account is locked, or greetd cannot be talked to: the check
            // could not be made, and the lock stays.
            Err(error) => {
                tracing::warn!(error = ?error, user, "a password could not be checked; the lock stays");
                Verdict::Unavailable
            }
        }
    }
}

/// Nothing can check a password here: nothing unlocks.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unavailable;

impl PasswordVerifier for Unavailable {
    fn verify(&self, _user: &str, _password: &str) -> Verdict {
        Verdict::Unavailable
    }

    fn available(&self) -> bool {
        false
    }
}

/// Whose password the lock checks: the login this session runs as. `$USER` when the login set it,
/// else the name the login database has for the user this session runs as — a session started
/// outside a login still locks with its person's name on the screen, and still takes that person's
/// password.
#[must_use]
pub fn login_user() -> String {
    let named = std::env::var("USER").unwrap_or_default();
    if !named.is_empty() {
        return named;
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(name) = user_name(rustix::process::geteuid().as_raw()) {
            return name;
        }
    }
    String::new()
}

/// The name the login database has for `uid` , when it has one.
#[cfg(target_os = "linux")]
fn user_name(uid: u32) -> Option<String> {
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut found = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: every pointer is to a live local buffer of the stated size;
    // `found` is read back as the success flag.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            pwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut found,
        )
    };
    if status != 0 || found.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded, so `pwd` is initialised and `pw_name`
    // points into `buffer`, which is still alive.
    let name = unsafe { std::ffi::CStr::from_ptr(pwd.assume_init().pw_name) };
    Some(name.to_string_lossy().into_owned())
}

/// What [`installed`] picked to check a password with, named so the choice can be asserted on
/// without taking a verifier apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// The privileged helper at these arguments: its program, then its own.
    Helper(Vec<String>),
    /// greetd's PAM conversation at this socket, for the login that started the session.
    Greetd(String),
    /// Nothing here can check a password.
    Unavailable,
}

/// The selection policy, as a pure function of what the machine has: the helper a build names or
/// the installed helper, which check against the machine's own accounts; else the login's greetd
/// socket, which checked the password the login took; else nothing. A socket named empty is none.
#[must_use]
pub fn select(helper: Option<Vec<String>>, greetd_socket: Option<String>) -> Choice {
    if let Some(program) = helper {
        return Choice::Helper(program);
    }
    match greetd_socket {
        Some(socket) if !socket.trim().is_empty() => Choice::Greetd(socket),
        _ => Choice::Unavailable,
    }
}

/// The verifier of this machine: the helper a development build names or the installed one, else
/// the greetd conversation of the login that started this session, else none.
#[must_use]
pub fn installed() -> std::sync::Arc<dyn PasswordVerifier> {
    match select(
        helper_program(
            OVERRIDABLE,
            std::env::var_os(UNLOCK_HELPER_ENV),
            Path::new(UNLOCK_HELPER).is_file(),
        ),
        std::env::var("GREETD_SOCK").ok(),
    ) {
        Choice::Helper(program) => std::sync::Arc::new(HelperVerifier::new(program)),
        Choice::Greetd(socket) => std::sync::Arc::new(GreetdVerifier::new(socket)),
        Choice::Unavailable => std::sync::Arc::new(Unavailable),
    }
}

/// Which program checks a password, as `program[0]` and its leading arguments: the one a build that
/// honours [`UNLOCK_HELPER_ENV`] names, else the installed helper when it is there, else none.
fn helper_program(
    overrides: bool,
    named: Option<std::ffi::OsString>,
    installed: bool,
) -> Option<Vec<String>> {
    if overrides && let Some(named) = named.filter(|path| !path.is_empty()) {
        return Some(vec![PathBuf::from(named).to_string_lossy().into_owned()]);
    }
    installed.then(|| vec![UNLOCK_HELPER.to_owned()])
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    /// A `/bin/sh` script with `body` standing in for the helper, run as `sh <script>` so the fresh
    /// file is read, not executed (no race with a concurrent fork holding it open for writing).
    fn fake_helper(dir: &Path, body: &str) -> HelperVerifier {
        let path = dir.join("helper.sh");
        let mut file = std::fs::File::create(&path).unwrap();
        write!(file, "{body}").unwrap();
        file.sync_all().unwrap();
        HelperVerifier::new(vec![
            "/bin/sh".to_owned(),
            path.to_string_lossy().into_owned(),
        ])
    }

    #[test]
    fn the_helpers_exit_status_is_the_verdict_and_the_password_goes_on_stdin_only() {
        let dir = tempfile::tempdir().unwrap();
        let argv = dir.path().join("argv");
        let helper = fake_helper(
            dir.path(),
            &format!(
                "printf '%s' \"$*\" > {}\nread -r pw\n[ \"$pw\" = \"s3cr3t\" ] && exit 0\nexit 1\n",
                argv.display()
            ),
        );
        assert_eq!(helper.verify("bob", "s3cr3t"), Verdict::Unlocks);
        assert_eq!(helper.verify("bob", "wrong"), Verdict::Wrong);
        let recorded = std::fs::read_to_string(&argv).unwrap();
        assert!(
            recorded.contains("bob"),
            "the user is on argv: {recorded:?}"
        );
        assert!(!recorded.contains("wrong") && !recorded.contains("s3cr3t"));

        let failing = fake_helper(dir.path(), "read -r pw\nexit 2\n");
        assert_eq!(failing.verify("bob", "s3cr3t"), Verdict::Unavailable);
        let missing = HelperVerifier::new(vec!["/nonexistent/tilo-unlock-helper".to_owned()]);
        assert_eq!(missing.verify("bob", "s3cr3t"), Verdict::Unavailable);
        let silent = fake_helper(dir.path(), "exec sleep 1000000\n")
            .with_timeout(Duration::from_millis(150));
        assert_eq!(silent.verify("bob", "s3cr3t"), Verdict::Unavailable);
    }

    /// A session started outside a login shell knows who is at the desk from the login database, so
    /// its lock still checks that person's password.
    #[test]
    fn the_login_the_session_runs_as_is_always_named() {
        assert!(!login_user().is_empty(), "the lock knows whose it is");
        #[cfg(target_os = "linux")]
        {
            let uid = rustix::process::geteuid().as_raw();
            let named = user_name(uid).expect("this login is in the login database");
            assert!(!named.is_empty() && !named.contains(':'), "{named:?}");
            assert_eq!(user_name(u32::MAX), None, "a uid no login has");
        }
    }

    /// A session that cannot check a password says so before it locks,
    /// rather than locking a screen nobody can leave.
    #[test]
    fn a_verifier_says_whether_it_can_check_a_password_at_all() {
        assert!(!Unavailable.available());
        let dir = tempfile::tempdir().unwrap();
        let helper = fake_helper(dir.path(), "read -r pw\nexit 1\n");
        assert!(helper.available());
        assert!(
            !HelperVerifier::new(vec!["/nonexistent/tilo-unlock-helper".to_owned()]).available()
        );
        let expected = !matches!(
            select(
                helper_program(
                    OVERRIDABLE,
                    std::env::var_os(UNLOCK_HELPER_ENV),
                    Path::new(UNLOCK_HELPER).is_file()
                ),
                std::env::var("GREETD_SOCK").ok()
            ),
            Choice::Unavailable
        );
        assert_eq!(
            installed().available(),
            expected,
            "the helper, or the login's greetd, is what checks a password"
        );
    }

    /// The helper checks against the machine's accounts; where it is not installed, the login's own
    /// greetd conversation is the fallback, and with neither the lock can never be lifted.
    #[test]
    fn the_helper_else_the_logins_greetd_else_nothing() {
        assert_eq!(
            select(
                Some(vec![UNLOCK_HELPER.to_owned()]),
                Some("/run/greetd.sock".to_owned())
            ),
            Choice::Helper(vec![UNLOCK_HELPER.to_owned()]),
            "the installed helper is what checks a password on this machine"
        );
        assert_eq!(
            select(None, Some("/run/greetd.sock".to_owned())),
            Choice::Greetd("/run/greetd.sock".to_owned()),
            "a login's greetd conversation is the fallback"
        );
        assert_eq!(select(None, None), Choice::Unavailable);
        assert_eq!(
            select(None, Some("  ".to_owned())),
            Choice::Unavailable,
            "an empty name is no socket"
        );
    }

    /// A greetd socket that is not there checks nothing: the lock stays, and says the check could
    /// not be made.
    #[test]
    fn a_greetd_conversation_that_cannot_be_reached_locks_out() {
        let verifier = GreetdVerifier::new("/nonexistent/greetd.sock");
        assert_eq!(verifier.verify("ada", "sesame"), Verdict::Unavailable);
    }

    /// A development build may name the helper that checks a password, and a production build never
    /// hears of it: the variable is no way into a shipped desktop.
    #[test]
    fn a_development_build_may_name_the_helper_and_a_production_one_never_does() {
        let named = || Some(std::ffi::OsString::from("/scratch/helper"));
        assert_eq!(
            helper_program(true, named(), true),
            Some(vec!["/scratch/helper".to_owned()]),
            "a development build checks with the helper it was given"
        );
        assert_eq!(
            helper_program(true, named(), false),
            Some(vec!["/scratch/helper".to_owned()]),
            "and needs no installed helper to do it"
        );
        assert_eq!(
            helper_program(false, named(), true),
            Some(vec![UNLOCK_HELPER.to_owned()]),
            "a production build checks with the installed helper alone"
        );
        assert_eq!(
            helper_program(false, named(), false),
            None,
            "and with none, nothing unlocks"
        );
        assert_eq!(
            helper_program(true, Some(std::ffi::OsString::new()), true),
            Some(vec![UNLOCK_HELPER.to_owned()]),
            "an empty name is no name"
        );
    }
}
