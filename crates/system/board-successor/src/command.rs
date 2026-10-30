//! Verification commands, run with bounded output and a deadline, and
//! killed (and reaped) on every way out, including the service being
//! stopped.

use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use crate::{Error, Result, require};

/// The signal that asked this process to stop, 0 for none.
static CANCELLED: AtomicI32 = AtomicI32::new(0);

extern "C" fn cancel(signal: libc::c_int) {
    CANCELLED.store(signal, Ordering::SeqCst);
}

/// Turns SIGTERM and SIGINT into a cancellation that the running command
/// loop sees within 20 ms: the child is killed and reaped, temporary
/// credentials are removed, and the process exits with 128 + the signal.
pub fn install_cancellation() {
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler only stores into an atomic, which is
        // async-signal-safe; the sigaction is fully initialised (no
        // SA_RESTART, so a blocking poll returns EINTR and the loop checks
        // the flag at once).
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = cancel as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(signal, &raw const action, std::ptr::null_mut());
        }
    }
}

/// Fails with the pending cancellation, if any.
///
/// # Errors
/// `Error::Cancelled` once SIGTERM or SIGINT arrived.
pub fn check_cancelled() -> Result<()> {
    match CANCELLED.load(Ordering::SeqCst) {
        0 => Ok(()),
        signal => Err(Error::Cancelled(signal)),
    }
}

/// Runs `arguments` with stdin closed and returns its standard output.
/// Each output stream may carry at most `max_output` bytes, and the whole run
/// must end within `timeout`. `environment` adds variables to the inherited
/// environment. A stop request kills and reaps the command's process tree.
///
/// # Errors
/// The command could not start, timed out, wrote too much, exited non-zero,
/// wrote output that is not UTF-8, or the service was stopped.
pub fn run(
    arguments: &[&str],
    environment: &[(&str, OsString)],
    timeout: Duration,
    max_output: usize,
) -> Result<String> {
    check_cancelled()?;
    let (program, rest) = arguments
        .split_first()
        .ok_or_else(|| Error::value("no command"))?;
    let mut command = Command::new(program);
    command
        .args(rest)
        .envs(environment.iter().map(|(key, value)| (key, value)));
    let mut running = bounded_command::spawn_prepared(command, None, max_output, timeout)
        .map_err(|error| Error::Value(error.to_string()))?;
    // Dropping `running` on a stop request kills and reaps the command.
    let outcome = loop {
        if let Some(outcome) = running.try_wait() {
            break outcome;
        }
        check_cancelled()?;
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = outcome.map_err(|error| match error {
        bounded_command::Error::Process(error) => Error::os(&error, Path::new(program)),
        bounded_command::Error::OutputTooLarge { .. } => {
            Error::value("release verification output exceeds its bound")
        }
        bounded_command::Error::Timeout(_) => Error::Timeout(format!(
            "Command '{arguments:?}' timed out after {} seconds",
            timeout.as_secs_f64()
        )),
        other => Error::Value(other.to_string()),
    })?;
    let status = output.status;
    let code = status
        .code()
        .unwrap_or_else(|| status.signal().map_or(-1, i32::saturating_neg));
    let name = Path::new(program).file_name().map_or_else(
        || (*program).to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    require(
        code == 0,
        format!("{name} refused release verification (exit {code})"),
    )?;
    String::from_utf8(output.stdout).map_err(|e| Error::Value(e.to_string()))
}
