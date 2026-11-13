//! The `Command` helpers every per-domain module uses: bounded subprocess execution, so
//! every shell-out shares one error vocabulary (missing binary, non-zero exit, spawn
//! failure) and hands it straight to `tracing::warn!`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::DEFAULT_TIMEOUT;

/// Eight MiB is far above legitimate status or list output, and bounds what a compromised daemon or a
/// network-derived response can make the caller hold.
pub(crate) const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// A binary does not appear on or vanish from `$PATH` during a session, so the probe is memoised: one
/// spawn per binary per session.
fn which_cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Walks `$PATH` in-process rather than shelling out to `which(1)`, which the image does not carry.
pub(crate) fn which(binary: &str) -> bool {
    if let Ok(cache) = which_cache().lock()
        && let Some(hit) = cache.get(binary)
    {
        return *hit;
    }
    let found = path_lookup(binary);
    if let Ok(mut cache) = which_cache().lock() {
        cache.insert(binary.to_string(), found);
    }
    found
}

/// A name with a separator is a path, not a `$PATH` lookup, as `execvp` treats it.
fn path_lookup(binary: &str) -> bool {
    fn is_executable_file(p: &std::path::Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }

    #[cfg(test)]
    tests::PATH_LOOKUPS.with(|lookups| lookups.set(lookups.get().saturating_add(1)));

    if binary.is_empty() {
        return false;
    }
    if binary.contains('/') {
        return is_executable_file(std::path::Path::new(binary));
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .any(|dir| is_executable_file(&dir.join(binary)))
}

/// Runs `binary args...` under [`DEFAULT_TIMEOUT`], capturing stdout. A wedged or restarting daemon
/// otherwise leaves its client blocked with no bound, and on the compositor's thread that stops the
/// desktop painting; a deadline turns it into a stale reading, which callers already handle. stdout and
/// stderr are drained on their own threads, because polling while the child writes into an unread pipe
/// deadlocks once the pipe buffer fills.
pub(crate) fn run_capture(binary: &str, args: &[&str]) -> Result<String, String> {
    run_capture_within(binary, args, DEFAULT_TIMEOUT)
}

pub(crate) fn run_capture_within(
    binary: &str,
    args: &[&str],
    timeout: std::time::Duration,
) -> Result<String, String> {
    if !which(binary) {
        return Err(format!("{binary} not installed"));
    }
    let output = bounded_command::run(binary, args, None, MAX_CAPTURE_BYTES, timeout)
        .map_err(|error| bounded_error(binary, error))?;
    if !output.status.success() {
        let stderr = output.stderr_diagnostic();
        return Err(format!(
            "{binary} exited with status {:?}: {stderr}",
            output.status.code()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{binary} stdout not utf-8: {e}"))
}

pub(crate) fn run(binary: &str, args: &[&str]) -> Result<(), String> {
    let _ = run_capture(binary, args)?;
    Ok(())
}

fn bounded_error(binary: &str, error: bounded_command::Error) -> String {
    match error {
        bounded_command::Error::InputTooLarge => {
            format!("{binary} input exceeded the safety limit")
        }
        bounded_command::Error::OutputTooLarge { limit, .. } => {
            format!("{binary} output exceeded the {limit} byte safety limit")
        }
        bounded_command::Error::Timeout(milliseconds) => {
            format!("{binary} timed out after {milliseconds} ms")
        }
        other => format!("{binary} process failed: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    thread_local! {
        pub(super) static PATH_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[test]
    fn helper_commands_are_bounded_by_output_limits_and_deadlines() {
        {
            assert!(which("head"), "head is required for the output-limit probe");
            let bytes = (MAX_CAPTURE_BYTES + 4096).to_string();
            let args = ["-c", bytes.as_str(), "/dev/zero"];
            // The output ceiling is under test; the deadline only catches a hang.
            let capture = run_capture_within("head", &args, tilo_test_env::hang_bound())
                .expect_err("oversized stdout must be rejected");
            assert!(capture.contains("output exceeded"), "{capture}");
        }
        // A command that never exits is killed at the deadline and reported as a timeout rather than
        // blocking its caller.
        {
            // No `sleep` binary means the deadline cannot be exercised. Fail rather than skip: a check
            // that cannot look is not a check that passed.
            assert!(
                which("sleep"),
                "`sleep` is required to exercise the run_capture deadline"
            );
            // The child outlives the hang bound, so returning within it proves the kill fired at
            // `run_capture`'s own deadline.
            let over = tilo_test_env::hang_bound().as_secs() * 2;
            let arg = format!("{over}");
            let started = Instant::now();
            let result = run_capture("sleep", &[arg.as_str()]);
            let elapsed = started.elapsed();

            let err = result.expect_err("a command outliving the deadline must not report success");
            assert!(
                err.contains("timed out"),
                "expected a timeout error, got: {err}"
            );
            assert!(
                elapsed < tilo_test_env::hang_bound(),
                "run_capture returned after {elapsed:?}; its {DEFAULT_TIMEOUT:?} kill path did \
                 not fire"
            );
        }
    }

    #[test]
    fn which_resolves_from_path_memoised_and_reports_missing_binaries() {
        // The probe answers from `$PATH` itself: asserting on `sh`, which is always present, keeps a
        // third binary out of the question.
        {
            assert!(
                which("sh"),
                "`sh` is on $PATH in every environment this runs in; a probe \
                 that cannot see it is answering about itself"
            );
            assert!(
                !which("tilo-no-such-binary-should-ever-exist"),
                "a name that is on no $PATH entry must not be reported as found"
            );
            // A name containing a separator is a path, not a $PATH lookup.
            assert!(which("/bin/sh") || which("/usr/bin/sh"));
            assert!(!which("/definitely/not/here/sh"));
            assert!(!which(""), "the empty name is not a binary");
        }
        // `which` answers from its memo after the first probe, so repeated shell-outs do not pay a
        // second spawn.
        {
            let first = which("sh");
            let looked = PATH_LOOKUPS.with(std::cell::Cell::get);
            for _ in 0..200 {
                assert_eq!(which("sh"), first);
            }
            let lookups = PATH_LOOKUPS.with(std::cell::Cell::get) - looked;
            assert_eq!(
                lookups, 0,
                "200 memoised probes looked up $PATH {lookups} times"
            );
        }
        // Setters that shell out never panic when the binary is missing: they return a `"<binary> not
        // installed"` error.
        {
            let r = run("definitely-not-a-real-binary-tilo-test", &["--help"]);
            assert!(r.is_err());
            let msg = r.unwrap_err();
            assert!(msg.contains("not installed"), "got: {msg}");
        }
    }
}
