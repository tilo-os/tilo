//! Verification commands, run with bounded output and a deadline, and
//! killed (and reaped) on every way out, including the service being
//! stopped.

use std::ffi::OsString;
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;

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

/// A child that is killed and reaped unless it was waited for.
struct Owned(Child);

impl Drop for Owned {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

enum Stream {
    Out(ChildStdout),
    Err(ChildStderr),
}

/// Reads both pipes as they fill until both end, keeping standard output.
fn drain(
    mut streams: Vec<(Stream, usize)>,
    deadline: Instant,
    max_output: usize,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = vec![0u8; 65536];
    while !streams.is_empty() {
        require(Instant::now() < deadline, "release verification timed out")?;
        check_cancelled()?;
        let ready: Vec<bool> = {
            let mut fds: Vec<PollFd<'_>> = streams
                .iter()
                .map(|(stream, _)| {
                    let fd = match stream {
                        Stream::Out(out) => out.as_fd(),
                        Stream::Err(err) => err.as_fd(),
                    };
                    PollFd::from_borrowed_fd(fd, PollFlags::IN)
                })
                .collect();
            let wait = Timespec {
                tv_sec: 0,
                tv_nsec: 20_000_000,
            };
            match poll(&mut fds, Some(&wait)) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(errno) => return Err(Error::from(std::io::Error::from(errno))),
            }
            fds.iter().map(|fd| !fd.revents().is_empty()).collect()
        };
        let mut index: usize = 0;
        for is_ready in ready {
            if !is_ready {
                index = index.saturating_add(1);
                continue;
            }
            let Some((stream, total)) = streams.get_mut(index) else {
                break;
            };
            let read = match stream {
                Stream::Out(out) => out.read(&mut chunk),
                Stream::Err(err) => err.read(&mut chunk),
            }
            .map_err(Error::from)?;
            if read == 0 {
                // The next stream moved into this index.
                streams.remove(index);
                continue;
            }
            *total = total.saturating_add(read);
            require(
                *total <= max_output,
                "release verification output exceeds its bound",
            )?;
            if matches!(stream, Stream::Out(_)) {
                output.extend_from_slice(chunk.get(..read).unwrap_or_default());
            }
            index = index.saturating_add(1);
        }
    }
    Ok(output)
}

/// Waits for the child until the deadline, as `Popen.wait(timeout)`.
fn reap(
    child: &mut Owned,
    deadline: Instant,
    arguments: &[&str],
    timeout: Duration,
) -> Result<ExitStatus> {
    loop {
        if let Some(status) = child.0.try_wait().map_err(Error::from)? {
            return Ok(status);
        }
        check_cancelled()?;
        if Instant::now() >= deadline {
            return Err(Error::Timeout(format!(
                "Command '{arguments:?}' timed out after {} seconds",
                timeout.as_secs_f64()
            )));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Runs `arguments` with stdin closed and returns its standard output.
/// Both pipes are read as they fill; each may carry at most `max_output`
/// bytes, and the whole run must end within `timeout`. `environment` adds
/// variables to the inherited environment.
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
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in environment {
        command.env(key, value);
    }
    let mut child = Owned(
        command
            .spawn()
            .map_err(|e| Error::os(&e, Path::new(program)))?,
    );
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let mut streams: Vec<(Stream, usize)> = Vec::new();
    if let Some(out) = child.0.stdout.take() {
        streams.push((Stream::Out(out), 0));
    }
    if let Some(err) = child.0.stderr.take() {
        streams.push((Stream::Err(err), 0));
    }
    let output = drain(streams, deadline, max_output)?;
    let status = reap(&mut child, deadline, arguments, timeout)?;
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
    String::from_utf8(output).map_err(|e| Error::Value(e.to_string()))
}
