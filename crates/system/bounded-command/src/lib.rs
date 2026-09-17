//! Deadline and allocation-bounded subprocess execution.
//!
//! Privileged Tilo components use small command-line clients at a few explicit
//! platform boundaries. This wrapper prevents a wedged client, compromised
//! daemon, or inherited pipe in a descendant from pinning or exhausting its
//! caller.

use std::ffi::OsStr;
use std::io::{Read, Write as _};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use thiserror::Error;
use zeroize::Zeroizing;

/// Maximum secret/request body accepted by this narrow helper.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

/// A completed child with individually bounded output streams.
#[derive(Debug)]
pub struct Output {
    /// Child exit status.
    pub status: ExitStatus,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
}

/// Failures before a bounded child completes successfully or unsuccessfully.
#[derive(Debug, Error)]
pub enum Error {
    /// Caller attempted to feed an unexpectedly large request or secret.
    #[error("command input exceeds its configured safety limit")]
    InputTooLarge,
    /// Child could not be created or observed.
    #[error("command process failed: {0}")]
    Process(#[from] std::io::Error),
    /// One output stream crossed its allocation ceiling.
    #[error("command {stream} exceeds {limit} bytes")]
    OutputTooLarge {
        /// Stream which crossed the limit.
        stream: &'static str,
        /// Configured maximum bytes.
        limit: usize,
    },
    /// Child and its descendants did not finish before the deadline.
    #[error("command timed out after {0} ms")]
    Timeout(u128),
    /// A reader thread panicked.
    #[error("command output reader failed")]
    Reader,
    /// The bounded stdin writer failed or panicked.
    #[error("command input writer failed")]
    Writer,
    /// The owner cancelled an asynchronous command.
    #[error("command was cancelled")]
    Cancelled,
}

/// A nonblocking bounded command which cancels its process group when dropped.
pub struct AsyncCommand {
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<Output, Error>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl AsyncCommand {
    /// Return a completed result without blocking, or `None` while it runs.
    pub fn try_wait(&mut self) -> Option<Result<Output, Error>> {
        match self.result.try_recv() {
            Ok(result) => {
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.worker.take().map(std::thread::JoinHandle::join);
                Some(Err(Error::Reader))
            }
        }
    }
}

impl Drop for AsyncCommand {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Spawn a caller-prepared bounded command for nonblocking polling.
///
/// Dropping the returned handle cancels and reaps the child process group.
///
/// # Errors
///
/// Returns [`Error::InputTooLarge`] before spawning a worker when needed.
pub fn spawn_prepared(
    command: Command,
    input: Option<&[u8]>,
    output_limit: usize,
    timeout: Duration,
) -> Result<AsyncCommand, Error> {
    if input.is_some_and(|bytes| bytes.len() > MAX_INPUT_BYTES) {
        return Err(Error::InputTooLarge);
    }
    let input = input.map(|bytes| Zeroizing::new(bytes.to_vec()));
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let (sender, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let outcome = run_prepared_inner(
            command,
            input,
            output_limit,
            output_limit,
            timeout,
            Some(&worker_cancel),
        );
        let _ = sender.send(outcome);
    });
    Ok(AsyncCommand {
        cancel,
        result,
        worker: Some(worker),
    })
}

fn read_bounded(pipe: impl Read, limit: usize) -> Vec<u8> {
    let read_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::new();
    let _ = pipe.take(read_limit).read_to_end(&mut bytes);
    bytes
}

/// Run one fixed program/argv transaction with bounded input, output, and time.
///
/// `stdout` and `stderr` each receive their own `output_limit`. On timeout the
/// complete child process group is killed, closing pipes inherited by helper
/// descendants as well.
///
/// # Errors
///
/// Returns an error for oversized input/output, spawn/wait failures, a reader
/// failure, or expiration of `timeout`. A non-zero child status is returned in
/// [`Output`] for the caller to interpret without exposing unbounded diagnostics.
pub fn run(
    program: &str,
    args: &[&str],
    input: Option<&[u8]>,
    output_limit: usize,
    timeout: Duration,
) -> Result<Output, Error> {
    run_os(program, args, input, output_limit, timeout)
}

/// Run a bounded command whose arguments may contain non-UTF-8 OS strings.
///
/// # Errors
///
/// Returns the same bounded process, input, output, and timeout failures as
/// [`run`].
pub fn run_os(
    program: impl AsRef<OsStr>,
    args: &[impl AsRef<OsStr>],
    input: Option<&[u8]>,
    output_limit: usize,
    timeout: Duration,
) -> Result<Output, Error> {
    let mut command = Command::new(program);
    command.args(args);
    run_prepared(command, input, output_limit, timeout)
}

/// Run a bounded UTF-8 command with an explicit environment overlay.
///
/// # Errors
///
/// Returns the same bounded process, input, output, and timeout failures as
/// [`run`].
pub fn run_with_env(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
    input: Option<&[u8]>,
    output_limit: usize,
    timeout: Duration,
) -> Result<Output, Error> {
    let mut command = Command::new(program);
    command.args(args).envs(env.iter().copied());
    run_prepared(command, input, output_limit, timeout)
}

/// Run a caller-prepared command while replacing its standard streams and
/// process-group configuration with the bounded policy.
///
/// # Errors
///
/// Returns the same bounded process, input, output, and timeout failures as
/// [`run`].
pub fn run_prepared(
    command: Command,
    input: Option<&[u8]>,
    output_limit: usize,
    timeout: Duration,
) -> Result<Output, Error> {
    run_prepared_with_limits(command, input, output_limit, output_limit, timeout)
}

/// Run a caller-prepared command with independent output-stream ceilings.
///
/// This variant is intended for helpers whose primary output is legitimately
/// much larger than their diagnostics, such as image acquisition tools.
///
/// # Errors
///
/// Returns the same bounded process, input, output, and timeout failures as
/// [`run_prepared`].
pub fn run_prepared_with_limits(
    command: Command,
    input: Option<&[u8]>,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
) -> Result<Output, Error> {
    run_prepared_with_input_limit(
        command,
        input,
        MAX_INPUT_BYTES,
        stdout_limit,
        stderr_limit,
        timeout,
    )
}

/// Run a prepared command with caller-selected input and stream ceilings.
///
/// Most callers should retain [`MAX_INPUT_BYTES`] through [`run_prepared`].
/// This entry point exists for bounded bulk payloads such as print documents.
///
/// # Errors
///
/// Returns the same bounded process, input, output, and timeout failures as
/// [`run_prepared`].
pub fn run_prepared_with_input_limit(
    command: Command,
    input: Option<&[u8]>,
    input_limit: usize,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
) -> Result<Output, Error> {
    if input.is_some_and(|bytes| bytes.len() > input_limit) {
        return Err(Error::InputTooLarge);
    }
    run_prepared_inner(
        command,
        input.map(|bytes| Zeroizing::new(bytes.to_vec())),
        stdout_limit,
        stderr_limit,
        timeout,
        None,
    )
}

fn run_prepared_inner(
    mut command: Command,
    input: Option<Zeroizing<Vec<u8>>>,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<Output, Error> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn()?;
    let input_writer = if let Some(bytes) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Process(std::io::Error::other("command stdin is unavailable")))?;
        Some(std::thread::spawn(move || stdin.write_all(&bytes)))
    } else {
        None
    };

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Process(std::io::Error::other("command stdout is unavailable")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Process(std::io::Error::other("command stderr is unavailable")))?;
    let out_reader = std::thread::spawn(move || read_bounded(stdout, stdout_limit));
    let err_reader = std::thread::spawn(move || read_bounded(stderr, stderr_limit));
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        let cancelled = cancel.is_some_and(|flag| flag.load(Ordering::Acquire));
        if cancelled || Instant::now() >= deadline {
            if let Ok(pid) = i32::try_from(child.id())
                && let Some(group) = pid.checked_neg()
            {
                // SAFETY: the child was placed in its own pid-named process
                // group; a negative pid targets exactly that group.
                unsafe { libc::kill(group, libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = out_reader.join().map_err(|_| Error::Reader)?;
    let stderr = err_reader.join().map_err(|_| Error::Reader)?;
    let input_result = input_writer.map(std::thread::JoinHandle::join);
    let Some(status) = status else {
        return Err(if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            Error::Cancelled
        } else {
            Error::Timeout(timeout.as_millis())
        });
    };
    if let Some(result) = input_result {
        result.map_err(|_| Error::Writer)?.map_err(Error::Process)?;
    }
    if stdout.len() > stdout_limit {
        return Err(Error::OutputTooLarge {
            stream: "stdout",
            limit: stdout_limit,
        });
    }
    if stderr.len() > stderr_limit {
        return Err(Error::OutputTooLarge {
            stream: "stderr",
            limit: stderr_limit,
        });
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn rejects_input_and_both_output_stream_overflows() {
        assert!(matches!(
            run(
                "true",
                &[],
                Some(&vec![0; MAX_INPUT_BYTES + 1]),
                16,
                Duration::from_secs(1)
            ),
            Err(Error::InputTooLarge)
        ));
        for (script, stream) in [
            ("#!/bin/sh\nhead -c 8192 /dev/zero\n", "stdout"),
            ("#!/bin/sh\nhead -c 8192 /dev/zero >&2\n", "stderr"),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let helper = directory.path().join("helper");
            fs::write(&helper, script).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            let error = run(
                helper.to_str().unwrap(),
                &[],
                None,
                4096,
                Duration::from_secs(1),
            )
            .unwrap_err();
            assert!(matches!(error, Error::OutputTooLarge { stream: got, .. } if got == stream));
        }
    }

    #[test]
    fn enforces_independent_output_limits() {
        let mut command = Command::new("sh");
        command.args(["-c", "head -c 32 /dev/zero; head -c 128 /dev/zero >&2"]);
        let error =
            run_prepared_with_limits(command, None, 64, 32, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(
            error,
            Error::OutputTooLarge {
                stream: "stderr",
                limit: 32
            }
        ));
    }

    #[test]
    fn explicit_input_limit_supports_bounded_bulk_payloads() {
        let bytes = vec![0x5a; MAX_INPUT_BYTES + 1];
        let output = run_prepared_with_input_limit(
            Command::new("cat"),
            Some(&bytes),
            bytes.len(),
            bytes.len(),
            64,
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, bytes);

        assert!(matches!(
            run_prepared_with_input_limit(
                Command::new("cat"),
                Some(&bytes),
                bytes.len() - 1,
                bytes.len(),
                64,
                Duration::from_secs(1),
            ),
            Err(Error::InputTooLarge)
        ));
    }

    #[test]
    fn timeout_kills_descendants_which_inherit_output_pipes() {
        let directory = tempfile::tempdir().unwrap();
        let helper = directory.path().join("helper");
        fs::write(&helper, "#!/bin/sh\nsleep 5\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let error = run(
            helper.to_str().unwrap(),
            &[],
            None,
            4096,
            Duration::from_millis(25),
        )
        .unwrap_err();
        assert!(matches!(error, Error::Timeout(_)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn timeout_interrupts_a_child_which_does_not_read_input() {
        let directory = tempfile::tempdir().unwrap();
        let helper = directory.path().join("helper");
        fs::write(&helper, "#!/bin/sh\nsleep 5\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let error = run(
            helper.to_str().unwrap(),
            &[],
            Some(&vec![0; MAX_INPUT_BYTES]),
            4096,
            Duration::from_millis(25),
        )
        .unwrap_err();
        assert!(matches!(error, Error::Timeout(_)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn explicit_environment_overlay_reaches_only_the_child() {
        let parent_value = std::env::var_os("TILO_BOUNDED_COMMAND_TEST");
        let output = run_with_env(
            "sh",
            &["-c", "printf %s \"$TILO_BOUNDED_COMMAND_TEST\""],
            &[("TILO_BOUNDED_COMMAND_TEST", "isolated")],
            None,
            64,
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"isolated");
        assert_eq!(std::env::var_os("TILO_BOUNDED_COMMAND_TEST"), parent_value);
    }

    #[test]
    fn asynchronous_handle_is_nonblocking_and_drop_cancels_promptly() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf ready; sleep 5"]);
        let mut child = spawn_prepared(command, None, 64, Duration::from_secs(30)).unwrap();
        assert!(child.try_wait().is_none());

        let started = Instant::now();
        drop(child);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
