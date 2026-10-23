//! Deadline and allocation-bounded subprocess execution.
//!
//! Privileged Tilo components use small command-line clients at a few explicit
//! platform boundaries. This wrapper prevents a wedged client, compromised
//! daemon, or inherited pipe in a descendant from pinning or exhausting its
//! caller.

use std::ffi::OsStr;
use std::io::{Read, Write as _};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use thiserror::Error;
use zeroize::Zeroizing;

#[cfg(unix)]
mod child_wait;
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
        let deadline = Instant::now()
            .checked_add(grace)
            .unwrap_or_else(Instant::now);
        while Instant::now() < deadline {
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
    let deadline = Instant::now().checked_add(timeout);
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
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// [`wait_timeout`], then kill and reap a child still running at the
/// deadline. `None` when it had to be killed.
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

/// Maximum secret/request body accepted by this narrow helper.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

/// A completed child with individually bounded output streams.
#[derive(Debug)]
pub struct Output {
    /// Child exit status.
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    /// Captured standard error.
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

/// An informational copy of bytes already read by the owned stderr capture.
/// Capture remains authoritative; overflow disables only this observation lane.
#[derive(Debug)]
pub enum StderrObservation {
    /// At most 4096 captured bytes, in stream order.
    Bytes(Vec<u8>),
    /// The bounded observation queue overflowed; no subsequent bytes are exposed.
    Overflow,
}

/// Maximum chunks retained by the optional informational stderr observer.
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
    /// Poll the optional bounded informational stderr lane without blocking.
    /// On overflow, pending copies are discarded and this lane stays disabled.
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
    spawn_prepared_inner(command, input, output_limit, timeout, false)
}

/// Spawn with a bounded nonblocking copy of owned stderr capture.
///
/// Up to sixteen 4096-byte chunks await observation. A slow observer disables
/// this informational lane instead of blocking the child or capture readers.
/// Output bytes, limits, cancellation and process-tree ownership are unchanged.
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

/// Run a bounded command whose arguments may contain non-UTF-8 OS strings.
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

/// What a bounded command reads as its standard input.
enum Stdin {
    Null,
    /// Bytes written to it through a pipe.
    Bytes(Zeroizing<Vec<u8>>),
    /// A file it reads itself.
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
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        let cancelled = cancel.is_some_and(|flag| flag.load(Ordering::Acquire));
        if cancelled || Instant::now() >= deadline {
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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    /// For tests of anything but the timeout: long enough that a loaded
    /// machine never turns the expected error into `Error::Timeout`.
    const UNTESTED_TIMEOUT: Duration = Duration::from_secs(60);

    #[test]
    fn a_file_given_as_stdin_is_what_the_program_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("chosen");
        fs::write(&path, b"chosen bytes").unwrap();
        let output = run_prepared_with_stdin(
            Command::new("cat"),
            fs::File::open(&path).unwrap(),
            64,
            UNTESTED_TIMEOUT,
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"chosen bytes");
    }

    #[test]
    fn input_and_output_limits_are_enforced_independently() {
        {
            assert!(matches!(
                run(
                    "true",
                    &[],
                    Some(&vec![0; MAX_INPUT_BYTES + 1]),
                    16,
                    UNTESTED_TIMEOUT
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
                let error =
                    run(helper.to_str().unwrap(), &[], None, 4096, UNTESTED_TIMEOUT).unwrap_err();
                assert!(
                    matches!(error, Error::OutputTooLarge { stream: got, .. } if got == stream),
                    "expected {stream} overflow, got {error:?}"
                );
            }
        }
        {
            let mut command = Command::new("sh");
            command.args(["-c", "head -c 32 /dev/zero; head -c 128 /dev/zero >&2"]);
            let error =
                run_prepared_with_limits(command, None, 64, 32, UNTESTED_TIMEOUT).unwrap_err();
            assert!(matches!(
                error,
                Error::OutputTooLarge {
                    stream: "stderr",
                    limit: 32
                }
            ));
        }
        {
            let bytes = vec![0x5a; MAX_INPUT_BYTES + 1];
            let output = run_prepared_with_input_limit(
                Command::new("cat"),
                Some(&bytes),
                bytes.len(),
                bytes.len(),
                64,
                UNTESTED_TIMEOUT,
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
                    UNTESTED_TIMEOUT,
                ),
                Err(Error::InputTooLarge)
            ));
        }
    }

    /// Runs `call` on its own thread and returns what it returned. Each
    /// child below would outlive the hang bound on its own, so the call
    /// returning at all shows it ended the child rather than waited for it.
    fn returns<T: Send + 'static>(what: &str, call: impl FnOnce() -> T + Send + 'static) -> T {
        let (sender, returned) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(call());
        });
        tilo_test_env::recv(what, &returned)
    }

    /// A child that runs for an hour unless it is ended.
    const OUTLIVES_THE_TEST: &str = "#!/bin/sh\nsleep 3600\n";

    #[test]
    fn timeouts_and_dropped_handles_terminate_children_promptly() {
        {
            let directory = tempfile::tempdir().unwrap();
            let helper = directory.path().join("helper");
            fs::write(&helper, OUTLIVES_THE_TEST).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            let error = returns("the timeout to kill the descendant", move || {
                run(
                    helper.to_str().unwrap(),
                    &[],
                    None,
                    4096,
                    Duration::from_millis(25),
                )
            })
            .unwrap_err();
            assert!(matches!(error, Error::Timeout(_)));
        }
        {
            let directory = tempfile::tempdir().unwrap();
            let helper = directory.path().join("helper");
            fs::write(&helper, OUTLIVES_THE_TEST).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            let error = returns("the timeout to interrupt the child", move || {
                run(
                    helper.to_str().unwrap(),
                    &[],
                    Some(&vec![0; MAX_INPUT_BYTES]),
                    4096,
                    Duration::from_millis(25),
                )
            })
            .unwrap_err();
            assert!(matches!(error, Error::Timeout(_)));
        }
        {
            let mut command = Command::new("sh");
            command.args(["-c", "printf ready; sleep 3600"]);
            let mut child = spawn_prepared(command, None, 64, Duration::from_secs(3600)).unwrap();
            assert!(child.try_wait().is_none());
            returns("dropping the handle to end the child", move || drop(child));
        }
        {
            let mut command = Command::new("sh");
            command.args(["-c", "trap '' TERM; sleep 3600"]);
            let child = own_process_tree(&mut command).spawn().unwrap();
            let mut child = returns("the escalation to kill the tree", move || {
                let mut child = child;
                terminate_tree(&mut child, Duration::from_millis(50));
                child
            });
            assert!(child.try_wait().unwrap().is_some());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_bounded_wait_reaps_an_exited_child_and_kills_only_on_request() {
        let mut quick = Command::new("true").spawn().unwrap();
        assert!(
            wait_or_kill(&mut quick, UNTESTED_TIMEOUT)
                .unwrap()
                .is_some_and(|status| status.success())
        );
        let mut slow = Command::new("sleep").arg("60").spawn().unwrap();
        assert_eq!(
            wait_timeout(&mut slow, Duration::from_millis(50)).unwrap(),
            None
        );
        assert!(
            slow.try_wait().unwrap().is_none(),
            "a plain wait leaves it running"
        );
        assert_eq!(
            wait_or_kill(&mut slow, Duration::from_millis(50)).unwrap(),
            None
        );
        assert!(slow.try_wait().unwrap().is_some(), "killed and reaped");
    }

    #[test]
    fn stderr_observation_is_live_bounded_and_preserves_exact_capture() {
        let directory = tempfile::tempdir().unwrap();
        let first_gate = directory.path().join("first-gate");
        let second_gate = directory.path().join("second-gate");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf 'first\\n' >&2; while [ ! -f \"$1\" ]; do sleep 0.01; done; printf 'second\\n' >&2; while [ ! -f \"$2\" ]; do sleep 0.01; done; printf stdout; exit 7",
            "owned-observer-fixture",
        ]).arg(&first_gate).arg(&second_gate);
        let mut child =
            spawn_prepared_observing_stderr(command, None, 4096, UNTESTED_TIMEOUT).unwrap();
        let mut observed = Vec::new();
        for (expected, gate) in [
            (b"first\n".as_slice(), first_gate),
            (b"first\nsecond\n".as_slice(), second_gate),
        ] {
            tilo_test_env::wait_until("the phase's stderr", || {
                match child.try_stderr() {
                    Some(StderrObservation::Bytes(bytes)) => observed.extend(bytes),
                    Some(StderrObservation::Overflow) => panic!("small gated output overflowed"),
                    None => {}
                }
                observed.len() >= expected.len()
            });
            assert_eq!(
                observed, expected,
                "the phase is observable before its gate is released"
            );
            assert!(
                child.try_wait().is_none(),
                "the same owned invocation is still running"
            );
            fs::write(gate, b"release").unwrap();
        }
        let output =
            tilo_test_env::wait_for("the owned child to complete", || child.try_wait()).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(
            output.stderr, observed,
            "observation never removes or rewrites capture bytes"
        );
    }

    #[test]
    fn a_slow_stderr_observer_does_not_block_capture_or_change_output_limits() {
        let mut command = Command::new("sh");
        command.args(["-c", "head -c 262144 /dev/zero >&2"]);
        let mut child =
            spawn_prepared_observing_stderr(command, None, 262_144, UNTESTED_TIMEOUT).unwrap();
        // A full observation queue that blocked the capture would never
        // let the child finish.
        let output = tilo_test_env::wait_for("the capture to finish", || child.try_wait()).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stderr, vec![0; 262_144]);
        assert!(matches!(
            child.try_stderr(),
            Some(StderrObservation::Overflow)
        ));
        assert!(
            child.try_stderr().is_none(),
            "overflow permanently disables only this lane"
        );
        let mut overflowing = Command::new("sh");
        overflowing.args(["-c", "head -c 8192 /dev/zero >&2"]);
        let mut child =
            spawn_prepared_observing_stderr(overflowing, None, 1024, UNTESTED_TIMEOUT).unwrap();
        let error = tilo_test_env::wait_for("the bounded child to complete", || child.try_wait())
            .unwrap_err();
        assert!(matches!(
            error,
            Error::OutputTooLarge {
                stream: "stderr",
                limit: 1024
            }
        ));
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
            UNTESTED_TIMEOUT,
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"isolated");
        assert_eq!(std::env::var_os("TILO_BOUNDED_COMMAND_TEST"), parent_value);
    }
}
