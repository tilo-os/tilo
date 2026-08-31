//! Shared `Command` helpers used by every per-domain module.
//!
//! Wraps the `which`-check + `Command::output` flow once so every shell-out shares one error
//! vocabulary — missing binary, non-zero exit, spawn failure — returned as `Result<_, String>`
//! so call-sites can hand it straight to `tracing::warn!`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::DEFAULT_TIMEOUT;

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
    if !which(binary) {
        return Err(format!("{binary} not installed"));
    }
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{binary} spawn failed: {e}"))?;

    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    // `Instant + Duration` panics on overflow; a deadline this near can never overflow in
    // practice, but fall back to "already due" rather than let the operator panic.
    let deadline = Instant::now()
        .checked_add(DEFAULT_TIMEOUT)
        .unwrap_or_else(Instant::now);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => return Err(format!("{binary} wait failed: {e}")),
        }
        if Instant::now() >= deadline {
            // Killing closes both pipes, letting the reader threads below finish rather than parking forever.
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    };

    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();

    let Some(status) = status else {
        return Err(format!(
            "{binary} timed out after {} ms",
            DEFAULT_TIMEOUT.as_millis()
        ));
    };
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(format!(
            "{binary} exited with status {:?}: {stderr}",
            status.code()
        ));
    }
    String::from_utf8(stdout).map_err(|e| format!("{binary} stdout not utf-8: {e}"))
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
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{binary} spawn failed: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|e| format!("{binary} stdin failed: {e}"))?;
    }
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = out_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    });
    let err_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = err_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("{binary} wait failed: {e}"))?
        {
            break Some(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let stdout = out_reader.join().unwrap_or_default();
    // Drain stderr to avoid a pipe deadlock, but deliberately never return it:
    // interactive clients may echo a rejected secret in diagnostics.
    let _stderr = err_reader.join().unwrap_or_default();
    let Some(status) = status else {
        return Err(format!(
            "{binary} timed out after {} ms",
            timeout.as_millis()
        ));
    };
    if !status.success() {
        return Err(format!(
            "{binary} rejected the request (status {:?})",
            status.code()
        ));
    }
    String::from_utf8(stdout).map_err(|e| format!("{binary} stdout not utf-8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that never exits is killed at the deadline and reported as a timeout rather
    /// than blocking its caller forever — the property the compositor depends on, since the
    /// shell-out helpers run on a worker that would otherwise leak a thread per attempt.
    /// Asserting on `run_capture` itself (not a mock) makes the guarantee real for every getter
    /// and setter, since they all route through it.
    #[test]
    fn a_hanging_command_is_killed_at_the_deadline() {
        // No `sleep` binary means the deadline cannot be exercised. Fail rather than skip: a check
        // that cannot look is not a check that passed.
        assert!(
            which("sleep"),
            "`sleep` is required to exercise the run_capture deadline"
        );
        let over = DEFAULT_TIMEOUT.as_secs_f64() * 4.0;
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
            elapsed < DEFAULT_TIMEOUT * 2,
            "run_capture returned after {elapsed:?}, past its own {DEFAULT_TIMEOUT:?} \
             deadline — the kill path did not fire"
        );
    }

    /// The probe answers from `$PATH` itself, with no third binary in the question — asserting
    /// on a binary guaranteed present (`sh`) is what makes "the answer is about the subject, not
    /// the prober" checkable, since Fedora's container image ships no `which(1)` to shell out to.
    #[test]
    fn which_answers_from_path_without_the_which_binary() {
        assert!(
            which("sh"),
            "`sh` is on $PATH in every environment this runs in; a probe \
             that cannot see it is answering about itself"
        );
        assert!(
            !which("qos-no-such-binary-should-ever-exist"),
            "a name that is on no $PATH entry must not be reported as found"
        );
        // A name containing a separator is a path, not a $PATH lookup.
        assert!(which("/bin/sh") || which("/usr/bin/sh"));
        assert!(!which("/definitely/not/here/sh"));
        assert!(!which(""), "the empty name is not a binary");
    }

    /// `which` answers from its memo after the first probe, so repeated shell-outs stop paying a
    /// second process spawn to re-ask a question whose answer cannot change.
    #[test]
    fn which_is_memoised_across_calls() {
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

    /// Smoke test: setters that shell out should never panic when the underlying binary is
    /// missing — they must return a `"<binary> not installed"` error string.
    #[test]
    fn missing_binary_returns_not_installed_error() {
        let r = run("definitely-not-a-real-binary-qos-test", &["--help"]);
        assert!(r.is_err());
        let msg = r.unwrap_err();
        assert!(msg.contains("not installed"), "got: {msg}");
    }
}
