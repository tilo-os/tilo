//! Shared `Command` helpers used by every per-domain module.
//!
//! Wraps bounded subprocess execution once so every shell-out shares one error
//! vocabulary — missing binary, non-zero exit, spawn failure — returned as `Result<_, String>`
//! so call-sites can hand it straight to `tracing::warn!`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::DEFAULT_TIMEOUT;

/// Maximum stdout or stderr retained from one OS helper. Eight MiB is far
/// above legitimate status/list output while bounding a compromised daemon or
/// network-derived response before it can exhaust the desktop process.
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// Memo for [`which`]. A binary doesn't appear on or vanish from `$PATH` during a session;
/// Quick-settings alone asked seven times per open, so caching cuts the probe to one spawn
/// per binary per session.
fn which_cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Probe whether `binary` is on `$PATH`. Memoised for the process lifetime — see
/// [`which_cache`]. Walks `$PATH` in-process rather than shelling out to `which(1)`, which
/// Fedora's container image does not ship — removing a third binary from the question and
/// costing no spawn at all.
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

/// Is `binary` an executable file reachable from `$PATH`? A name with a separator in it is a
/// path, not a `$PATH` lookup, and is answered directly — the same split `execvp` makes.
fn path_lookup(binary: &str) -> bool {
    fn is_executable_file(p: &std::path::Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }

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

/// Run `binary args...` under [`DEFAULT_TIMEOUT`], capturing stdout.
///
/// The deadline is the point: these helpers drive OS tools (`nmcli`, `bluetoothctl`, `wpctl`)
/// that talk to daemons over D-Bus, and a wedged or restarting daemon leaves its client
/// blocked with no bound — if that thread is the compositor's, the whole desktop stops
/// painting. A deadline turns that into a stale reading, which every caller already handles
/// by logging and keeping the previous value.
///
/// stdout and stderr are drained on their own threads: polling `try_wait` while the child
/// writes into an unread pipe deadlocks once the child outfills the pipe buffer, which
/// `nmcli device wifi list` on a busy band is comfortably large enough to do.
pub(crate) fn run_capture(binary: &str, args: &[&str]) -> Result<String, String> {
    run_capture_within(binary, args, DEFAULT_TIMEOUT)
}

/// [`run_capture`] with an explicit deadline.
fn run_capture_within(
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
        let stderr = sanitized_diagnostic(&output.stderr);
        return Err(format!(
            "{binary} exited with status {:?}: {stderr}",
            output.status.code()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{binary} stdout not utf-8: {e}"))
}

/// Run `binary args...` and discard stdout. Same error semantics as [`run_capture`]. Used
/// by setter shell-outs.
pub(crate) fn run(binary: &str, args: &[&str]) -> Result<(), String> {
    let _ = run_capture(binary, args)?;
    Ok(())
}

/// Run a client with bounded stdin, keeping secrets out of argv and the
/// process list. Used for `NetworkManager`'s `--ask` flow.
pub(crate) fn run_with_input(
    binary: &str,
    args: &[&str],
    input: &[u8],
    timeout: std::time::Duration,
) -> Result<String, String> {
    if !which(binary) {
        return Err(format!("{binary} not installed"));
    }
    let output = bounded_command::run(binary, args, Some(input), MAX_CAPTURE_BYTES, timeout)
        .map_err(|error| bounded_error(binary, error))?;
    // Never return stderr: interactive clients may echo a rejected secret in diagnostics.
    if !output.status.success() {
        return Err(format!(
            "{binary} rejected the request (status {:?})",
            output.status.code()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{binary} stdout not utf-8: {e}"))
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

fn sanitized_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_control() || matches!(character, ' ' | '\n' | '\t'))
        .take(512)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn helper_commands_are_bounded_by_output_limits_and_deadlines() {
        // Phase `helper_output_limits_fail_closed_on_real_subprocesses`.
        {
            assert!(which("head"), "head is required for the output-limit probe");
            let bytes = (MAX_CAPTURE_BYTES + 4096).to_string();
            let args = ["-c", bytes.as_str(), "/dev/zero"];
            // The output ceiling is under test; the deadline only catches a hang.
            let capture = run_capture_within("head", &args, tilo_test_env::hang_bound())
                .expect_err("oversized stdout must be rejected");
            assert!(capture.contains("output exceeded"), "{capture}");

            let with_input = run_with_input("head", &args, b"", tilo_test_env::hang_bound())
                .expect_err("secret-input helper must share the same output ceiling");
            assert!(with_input.contains("output exceeded"), "{with_input}");
        }
        // Phase `a_hanging_command_is_killed_at_the_deadline`.
        // A command that never exits is killed at the deadline and reported as a timeout rather
        // than blocking its caller forever — the property the compositor depends on, since the
        // shell-out helpers run on a worker that would otherwise leak a thread per attempt.
        // The test calls `run_capture`, so the same guarantee applies to every getter and setter
        // that routes through it.
        {
            // No `sleep` binary means the deadline cannot be exercised. Fail rather than skip: a check
            // that cannot look is not a check that passed.
            assert!(
                which("sleep"),
                "`sleep` is required to exercise the run_capture deadline"
            );
            // The child outlives the hang bound, so returning within it proves the
            // kill fired at `run_capture`'s own deadline.
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
        // Phase `which_answers_from_path_without_the_which_binary`.
        // The probe answers from `$PATH` itself, with no third binary in the question — asserting
        // on a binary guaranteed present (`sh`) is what makes "the answer is about the subject, not
        // the prober" checkable, since Fedora's container image ships no `which(1)` to shell out to.
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
        // Phase `which_is_memoised_across_calls`.
        // `which` answers from its memo after the first probe, so repeated shell-outs stop paying a
        // second process spawn to re-ask a question whose answer cannot change.
        {
            let first = which("sh");
            let started = Instant::now();
            for _ in 0..200 {
                assert_eq!(which("sh"), first);
            }
            assert!(
                started.elapsed() < std::time::Duration::from_millis(200),
                "200 memoised probes took {:?} — they are still spawning",
                started.elapsed()
            );
        }
        // Phase `missing_binary_returns_not_installed_error`.
        // Smoke test: setters that shell out should never panic when the underlying binary is
        // missing — they must return a `"<binary> not installed"` error string.
        {
            let r = run("definitely-not-a-real-binary-tilo-test", &["--help"]);
            assert!(r.is_err());
            let msg = r.unwrap_err();
            assert!(msg.contains("not installed"), "got: {msg}");
        }
    }
}
