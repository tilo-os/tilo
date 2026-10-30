//! Whether a password unlocks the session.
//!
//! The session links no PAM: a running session has no greetd socket, so it
//! asks the one privileged helper (`tilo-unlock-helper`), the user name on
//! its command line and the password on its standard input (never on the
//! command line, where `/proc/*/cmdline` would show it), and reads the
//! verdict from its exit status. With no helper installed nothing unlocks.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use zeroize::Zeroizing;

/// Where the image and the host session package install the helper.
pub const UNLOCK_HELPER: &str = "/usr/libexec/tilo-unlock-helper";

/// Upper bound on one check: a slow PAM stack takes seconds, a wedged
/// helper must not keep the lock screen checking forever.
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

/// Checks a password for a user. `Send + Sync`: a check runs off the
/// session's loop.
pub trait PasswordVerifier: Send + Sync {
    fn verify(&self, user: &str, password: &str) -> Verdict;

    /// Whether a password can be checked at all here.
    fn available(&self) -> bool {
        true
    }
}

/// The helper as a verifier: `program[0]` the executable, then leading
/// arguments; the user name comes last.
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

/// The verifier of this machine: the installed helper, else none.
#[must_use]
pub fn installed() -> std::sync::Arc<dyn PasswordVerifier> {
    if Path::new(UNLOCK_HELPER).exists() {
        std::sync::Arc::new(HelperVerifier::new(vec![UNLOCK_HELPER.to_owned()]))
    } else {
        std::sync::Arc::new(Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    /// A `/bin/sh` script with `body` standing in for the helper, run as
    /// `sh <script>` so the fresh file is read, not executed (no race with
    /// a concurrent fork holding it open for writing).
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
}
