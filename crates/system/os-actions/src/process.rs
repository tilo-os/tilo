//! Shared `Command` helpers used by every per-domain module.
//!
//! Wrapping the `which`-check + `Command::output` flow once gives every
//! shell-out the same error vocabulary: `"<binary> not installed"` for a
//! missing binary, `"<binary> exited with status …: <stderr>"` for a
//! non-zero exit, `"<binary> spawn failed: …"` if `Command::output`
//! itself fails. The wrappers return `Result<_, String>` so call-sites
//! can hand the string straight to `tracing::warn!`.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::DEFAULT_TIMEOUT;

/// Memo for [`which`]. A binary does not appear on or vanish from `$PATH`
/// during a session, and every shell-out used to pay a second process
/// spawn to re-ask. Quick-settings alone asked seven times per open, so
/// half of its process spawns were this probe answering the same
/// question. Cached, the probe costs one spawn per binary per session.
fn which_cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Probe whether `binary` is on `$PATH`. Memoised for the process
/// lifetime — see [`which_cache`].
///
/// The lookup walks `$PATH` here rather than shelling out to `which`.
/// It used to spawn `which`, which made the answer depend on a binary
/// that is not part of a base system any more: Fedora's container image
/// ships no `which(1)`, so the spawn failed, the `unwrap_or(false)` read
/// that as "not on `$PATH`", and every probe answered no. A missing
/// probe tool reported as a missing subject — the failure mode where a
/// check cannot look and says so in the voice of having looked. Doing
/// the walk in-process removes the third binary from the question, and
/// costs no spawn at all.
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

/// Is `binary` an executable file reachable from `$PATH`?
///
/// A name with a separator in it is a path, not a `$PATH` lookup, and is
/// answered directly — the same split `execvp` makes.
fn path_lookup(binary: &str) -> bool {
    fn is_executable_file(p: &std::path::Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
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
/// The deadline is the point of this wrapper. These helpers drive OS
/// tools — `nmcli`, `bluetoothctl`, `wpctl` — that talk to daemons over
/// D-Bus, and a daemon that is wedged, restarting, or waiting on a
/// hardware rfkill leaves its client blocked with no bound at all. This
/// previously had no timeout, so one stuck `bluetoothctl` was enough to
/// hold whichever thread called it forever; when that thread was the
/// compositor's, the whole desktop stopped painting. A deadline turns
/// that into a stale reading, which every caller here already handles:
/// they log and keep the previous value.
///
/// stdout and stderr are drained on their own threads. Polling
/// `try_wait` while the child writes into a pipe nobody is reading
/// deadlocks as soon as the child outfills the pipe buffer — and
/// `nmcli device wifi list` on a busy band is comfortably large enough
/// to do it.
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

    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => return Err(format!("{binary} wait failed: {e}")),
        }
        if Instant::now() >= deadline {
            // Killing closes both pipes, which is what lets the reader
            // threads below finish rather than parking forever.
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

/// Run `binary args...` and discard stdout. Same error semantics as
/// [`run_capture`]. Used by setter shell-outs.
pub(crate) fn run(binary: &str, args: &[&str]) -> Result<(), String> {
    let _ = run_capture(binary, args)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that never exits is killed at the deadline and reported
    /// as a timeout rather than blocking its caller forever.
    ///
    /// This is the property the compositor depends on: the shell-out
    /// helpers run on a worker, but a worker that can never finish leaks
    /// a thread per attempt and never delivers a reading. Asserting on
    /// `run_capture` itself (not a mock) is what makes the guarantee real
    /// for every getter and setter in this crate, since they all route
    /// through it.
    #[test]
    fn a_hanging_command_is_killed_at_the_deadline() {
        // No `sleep` binary means the deadline cannot be exercised.
        // Fail rather than skip: a check that cannot look is not a
        // check that passed.
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
        assert!(err.contains("timed out"), "expected a timeout error, got: {err}");
        assert!(
            elapsed < DEFAULT_TIMEOUT * 2,
            "run_capture returned after {elapsed:?}, past its own {DEFAULT_TIMEOUT:?} \
             deadline — the kill path did not fire"
        );
    }

    /// The probe answers from `$PATH` itself, with no third binary in
    /// the question.
    ///
    /// This is a regression test with a name: the probe used to spawn
    /// `which(1)`, and Fedora's container image does not ship one. The
    /// spawn failed, that was read as "the binary is not installed", and
    /// every shell-out in this crate was told its tool was missing —
    /// including the deadline test below, which then refused to run at
    /// all. Asserting on a binary that is guaranteed present (`sh`, which
    /// `run_capture`'s own callers rely on) is what makes "the answer is
    /// about the subject, not about the prober" checkable.
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

    /// `which` answers from its memo after the first probe, so repeated
    /// shell-outs stop paying a second process spawn each to re-ask a
    /// question whose answer cannot change.
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

    /// Smoke test: setters that shell out should never panic when the
    /// underlying binary is missing — they must return a
    /// `"<binary> not installed"` error string.
    #[test]
    fn missing_binary_returns_not_installed_error() {
        let r = run("definitely-not-a-real-binary-qos-test", &["--help"]);
        assert!(r.is_err());
        let msg = r.unwrap_err();
        assert!(msg.contains("not installed"), "got: {msg}");
    }
}
