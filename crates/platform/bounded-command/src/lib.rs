//! Deadline- and allocation-bounded subprocess execution.
//!
//! Privileged components use small command-line clients at a few platform
//! boundaries; this wrapper keeps a wedged client, a compromised daemon or an
//! inherited pipe in a descendant from pinning or exhausting its caller.

use std::ffi::OsStr;
use std::io::{Read, Write as _};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::time::Duration;

use thiserror::Error;
use zeroize::Zeroizing;

#[cfg(unix)]
mod child_wait;
/// The `Command` helpers domains share: `which`, `run` and bounded capture with one error
/// vocabulary. Native only: the page's build links this crate through the UI's speech engine,
/// and a browser page has no subprocesses to look up or start.
#[cfg(not(target_arch = "wasm32"))]
pub mod command;
#[cfg(unix)]
pub use child_wait::{WakeFd, Wakeup, wait_child_or};

/// Start `command`'s child as the leader of its own process tree, so
/// [`kill_tree`] also reaches every descendant it starts. On Unix this is a
/// new process group named after the child's pid.
pub fn own_process_tree(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    command.process_group(0);
    command
}

/// Start `command`'s child without a console window of its own. On Windows a
/// console program otherwise opens one; elsewhere this changes nothing.
pub fn without_console_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

/// Kill a child started with [`own_process_tree`] together with its
/// descendants, and the child itself as a fallback. The caller still reaps it.
pub fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id())
        && let Some(group) = pid.checked_neg()
    {
        // SAFETY: the child was placed in its own pid-named process group; a
        // negative pid targets exactly that group.
        unsafe { libc::kill(group, libc::SIGKILL) };
    }
    let _ = child.kill();
}

/// Stop a child started with [`own_process_tree`] and reap it. With a
/// non-zero `grace`, the tree is first asked to exit (`SIGTERM` on Unix) and
/// gets that long to do so: until the child and, on Unix, every process left
/// in its group are gone. Whatever remains is then killed with [`kill_tree`].
pub fn terminate_tree(child: &mut Child, grace: Duration) {
    #[cfg(unix)]
    if !grace.is_zero()
        && let Ok(pid) = i32::try_from(child.id())
        && let Some(group) = pid.checked_neg()
    {
        // SAFETY: the child was placed in its own pid-named process group; a
        // negative pid targets exactly that group.
        unsafe { libc::kill(group, libc::SIGTERM) };
        // A grace past what the clock can name waits for the tree, as a timeout does.
        let deadline = tilo_clock::now().checked_add(grace);
        while deadline.is_none_or(|deadline| tilo_clock::now() < deadline) {
            // The leader exiting is not enough: a worker it started may still
            // be winding down in the group. Signal 0 only probes for one.
            // SAFETY: as above; signal 0 delivers nothing.
            if child.try_wait().ok().flatten().is_some()
                && unsafe { libc::kill(group, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    #[cfg(not(unix))]
    let _ = grace;
    kill_tree(child);
    let _ = child.wait();
}

/// Wait up to `timeout` for `child` to exit, and reap it. `None` when it is
/// still running at the deadline; it is left running. On Unix this sleeps in
/// [`wait_child_or`] rather than polling.
pub fn wait_timeout(child: &mut Child, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    let deadline = tilo_clock::now().checked_add(timeout);
    #[cfg(unix)]
    {
        Ok(match wait_child_or(child, None, deadline)? {
            Wakeup::Exited(status) => Some(status),
            Wakeup::Woken | Wakeup::Deadline => None,
        })
    }
    #[cfg(not(unix))]
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if deadline.is_some_and(|deadline| tilo_clock::now() >= deadline) {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// [`wait_timeout`], then kill and reap a child still running at the deadline.
/// `None` when it had to be killed.
pub fn wait_or_kill(child: &mut Child, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    let status = wait_timeout(child, timeout)?;
    if status.is_none() {
        let _ = child.kill();
        child.wait()?;
    }
    Ok(status)
}

/// Mark a script or program file executable for everyone and writable only
/// by its owner (mode `0755` on Unix). A no-op where executability is not a
/// file permission.
pub fn make_executable(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Maximum secret or request body this wrapper accepts.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    /// Standard error as a short diagnostic safe to log or show a person: lossy
    /// UTF-8 without control characters other than space, newline and tab, at
    /// most 512 characters, trimmed.
    #[must_use]
    pub fn stderr_diagnostic(&self) -> String {
        String::from_utf8_lossy(&self.stderr)
            .chars()
            .filter(|character| !character.is_control() || matches!(character, ' ' | '\n' | '\t'))
            .take(512)
            .collect::<String>()
            .trim()
            .to_string()
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("command input exceeds its configured safety limit")]
    InputTooLarge,
    #[error("command process failed: {0}")]
    Process(#[from] std::io::Error),
    #[error("command {stream} exceeds {limit} bytes")]
    OutputTooLarge { stream: &'static str, limit: usize },
    #[error("command timed out after {0} ms")]
    Timeout(u128),
    #[error("command output reader failed")]
    Reader,
    #[error("command input writer failed")]
    Writer,
    #[error("command was cancelled")]
    Cancelled,
}

/// An informational copy of bytes the owned stderr capture already read. Capture
/// stays authoritative; overflow disables only this queue.
#[derive(Debug)]
pub enum StderrObservation {
    /// At most 4096 captured bytes, in stream order.
    Bytes(Vec<u8>),
    Overflow,
}

pub const STDERR_OBSERVATION_CHUNKS: usize = 16;

struct StderrCapture {
    sender: SyncSender<Vec<u8>>,
    overflow: Arc<AtomicBool>,
}

struct StderrObservations {
    receiver: Receiver<Vec<u8>>,
    overflow: Arc<AtomicBool>,
    disabled: bool,
}

/// A nonblocking bounded command which cancels its process group when dropped.
pub struct AsyncCommand {
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<Output, Error>>,
    worker: Option<std::thread::JoinHandle<()>>,
    stderr_observations: Option<StderrObservations>,
}

impl AsyncCommand {
    /// Poll the optional stderr queue without blocking. On overflow, pending copies
    /// are dropped and the queue stays disabled.
    pub fn try_stderr(&mut self) -> Option<StderrObservation> {
        let observations = self.stderr_observations.as_mut()?;
        if observations.disabled {
            return None;
        }
        if observations.overflow.load(Ordering::Acquire) {
            observations.disabled = true;
            while observations.receiver.try_recv().is_ok() {}
            return Some(StderrObservation::Overflow);
        }
        observations
            .receiver
            .try_recv()
            .ok()
            .map(StderrObservation::Bytes)
    }

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

/// Spawn a caller-prepared bounded command for nonblocking polling. Dropping the
/// handle cancels and reaps the process group.
///
/// # Errors
///
/// [`Error::InputTooLarge`] before spawning a worker.
pub fn spawn_prepared(
    command: Command,
    input: Option<&[u8]>,
    output_limit: usize,
    timeout: Duration,
) -> Result<AsyncCommand, Error> {
    spawn_prepared_inner(command, input, output_limit, timeout, false)
}

/// Spawn with a bounded nonblocking copy of the owned stderr capture.
///
/// Up to sixteen 4096-byte chunks await observation; a slow observer disables this
/// informational queue instead of blocking the child or the capture readers. Output
/// bytes, limits, cancellation and process-tree ownership are unchanged.
pub fn spawn_prepared_observing_stderr(
    command: Command,
    input: Option<&[u8]>,
    output_limit: usize,
    timeout: Duration,
) -> Result<AsyncCommand, Error> {
    spawn_prepared_inner(command, input, output_limit, timeout, true)
}

fn spawn_prepared_inner(
    command: Command,
    input: Option<&[u8]>,
    output_limit: usize,
    timeout: Duration,
    observe_stderr: bool,
) -> Result<AsyncCommand, Error> {
    if input.is_some_and(|bytes| bytes.len() > MAX_INPUT_BYTES) {
        return Err(Error::InputTooLarge);
    }
    let input = input.map(|bytes| Zeroizing::new(bytes.to_vec()));
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let (sender, result) = std::sync::mpsc::channel();
    let (capture, stderr_observations) = if observe_stderr {
        let (sender, receiver) = std::sync::mpsc::sync_channel(STDERR_OBSERVATION_CHUNKS);
        let overflow = Arc::new(AtomicBool::new(false));
        (
            Some(StderrCapture {
                sender,
                overflow: Arc::clone(&overflow),
            }),
            Some(StderrObservations {
                receiver,
                overflow,
                disabled: false,
            }),
        )
    } else {
        (None, None)
    };
    let worker = std::thread::spawn(move || {
        let outcome = run_prepared_inner(
            command,
            input.map_or(Stdin::Null, Stdin::Bytes),
            output_limit,
            output_limit,
            timeout,
            Some(&worker_cancel),
            capture,
        );
        let _ = sender.send(outcome);
    });
    Ok(AsyncCommand {
        cancel,
        result,
        worker: Some(worker),
        stderr_observations,
    })
}

fn read_bounded(pipe: impl Read, limit: usize) -> Vec<u8> {
    let read_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::new();
    let _ = pipe.take(read_limit).read_to_end(&mut bytes);
    bytes
}

fn read_bounded_observed(pipe: impl Read, limit: usize, capture: Option<StderrCapture>) -> Vec<u8> {
    let Some(capture) = capture else {
        return read_bounded(pipe, limit);
    };
    let read_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut pipe = pipe.take(read_limit);
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let count = match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        let Some(captured) = chunk.get(..count) else {
            break;
        };
        bytes.extend_from_slice(captured);
        if !capture.overflow.load(Ordering::Acquire)
            && capture.sender.try_send(captured.to_vec()).is_err()
        {
            capture.overflow.store(true, Ordering::Release);
        }
    }
    bytes
}

/// Run one fixed program/argv transaction with bounded input, output, and time.
///
/// `stdout` and `stderr` each receive their own `output_limit`. On timeout the
/// complete child process group is killed, closing pipes inherited by helper
/// descendants as well.
pub fn run(
    program: &str,
    args: &[&str],
    input: Option<&[u8]>,
    output_limit: usize,
    timeout: Duration,
) -> Result<Output, Error> {
    run_os(program, args, input, output_limit, timeout)
}

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

/// Run a caller-prepared command, replacing its standard streams and process-group
/// configuration with the bounded policy.
pub fn run_prepared(
    command: Command,
    input: Option<&[u8]>,
    output_limit: usize,
    timeout: Duration,
) -> Result<Output, Error> {
    run_prepared_with_limits(command, input, output_limit, output_limit, timeout)
}

/// Run a caller-prepared command with independent output-stream ceilings, for
/// helpers whose primary output is legitimately much larger than their diagnostics,
/// such as image acquisition tools.
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

/// Run a prepared command with caller-selected input and stream ceilings, for
/// bounded bulk payloads such as print documents. Most callers want
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
        input.map_or(Stdin::Null, |bytes| {
            Stdin::Bytes(Zeroizing::new(bytes.to_vec()))
        }),
        stdout_limit,
        stderr_limit,
        timeout,
        None,
        None,
    )
}

/// Run a prepared command reading `file` as its standard input: a handle the
/// caller holds (a file the person chose) given to the program as it is,
/// rather than a path it would open itself.
pub fn run_prepared_with_stdin(
    command: Command,
    file: std::fs::File,
    output_limit: usize,
    timeout: Duration,
) -> Result<Output, Error> {
    run_prepared_inner(
        command,
        Stdin::File(file),
        output_limit,
        output_limit,
        timeout,
        None,
        None,
    )
}

enum Stdin {
    Null,
    Bytes(Zeroizing<Vec<u8>>),
    File(std::fs::File),
}

fn run_prepared_inner(
    mut command: Command,
    input: Stdin,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
    stderr_capture: Option<StderrCapture>,
) -> Result<Output, Error> {
    let (stdin, input) = match input {
        Stdin::Null => (Stdio::null(), None),
        Stdin::Bytes(bytes) => (Stdio::piped(), Some(bytes)),
        Stdin::File(file) => (Stdio::from(file), None),
    };
    command
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    own_process_tree(&mut command);
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
    let err_reader =
        std::thread::spawn(move || read_bounded_observed(stderr, stderr_limit, stderr_capture));
    // A timeout past what the clock can name (`Duration::MAX`) is no deadline at all.
    let deadline = tilo_clock::now().checked_add(timeout);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        let cancelled = cancel.is_some_and(|flag| flag.load(Ordering::Acquire));
        if cancelled || deadline.is_some_and(|deadline| tilo_clock::now() >= deadline) {
            kill_tree(&mut child);
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
