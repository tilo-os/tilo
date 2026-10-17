//! Bounded execution of installed consumer probes.
//!
//! The probe runs in its own process group with a cleared environment. Both
//! output streams are capped, a deadline covers the whole run, and the group
//! is killed once the leader has exited so a descendant holding a pipe cannot
//! outlive the check. The leader is observed with `WNOWAIT`, which keeps its
//! PID (and so the group id) reserved until that cleanup is done.
use std::{
    ffi::OsString,
    fmt, io,
    io::{Read, Write},
    os::fd::AsRawFd,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// The fixed environment every installed consumer probe receives.
pub const ENV: [(&str, &str); 3] = [("PATH", "/usr/bin:/bin"), ("LANG", "C"), ("LC_ALL", "C")];

/// A probe that ran to completion but exited unsuccessfully.
#[derive(Debug)]
pub struct CommandFailed;
impl fmt::Display for CommandFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("installed consumer command failed")
    }
}
impl std::error::Error for CommandFailed {}

/// True when `error` is a completed probe's nonzero exit, as opposed to a
/// deadline, bound or spawn failure.
pub fn is_command_failure(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(<dyn std::error::Error + Send + Sync>::is::<CommandFailed>)
}

/// One bounded probe execution.
pub struct Request<'a> {
    pub command: &'a [OsString],
    pub env: &'a [(&'a str, OsString)],
    pub input: Option<&'a [u8]>,
    /// Applies to stdout and stderr independently.
    pub limit: usize,
    pub timeout: Duration,
}

/// Run `command` with only [`ENV`] and return its stdout.
pub fn run_fixed(command: &[OsString], limit: usize, timeout: Duration) -> io::Result<Vec<u8>> {
    let env = ENV.map(|(key, value)| (key, OsString::from(value)));
    run(&Request {
        command,
        env: &env,
        input: None,
        limit,
        timeout,
    })
}

struct Owned {
    child: Child,
    reaped: bool,
}
impl Drop for Owned {
    fn drop(&mut self) {
        // Never signal an already reaped group: its PID may have been recycled.
        if !self.reaped {
            bounded_command::kill_tree(&mut self.child);
            let _ = self.child.wait();
        }
    }
}

/// Run one probe under the bounds in `request`.
#[allow(
    clippy::too_many_lines,
    reason = "one poll loop owns both pipes and the process group"
)]
pub fn run(request: &Request<'_>) -> io::Result<Vec<u8>> {
    let (program, arguments) = request
        .command
        .split_first()
        .ok_or_else(|| super::invalid("empty consumer command"))?;
    let mut command = Command::new(program);
    command.args(arguments).env_clear();
    for (key, value) in request.env {
        command.env(key, value);
    }
    bounded_command::own_process_tree(&mut command);
    command
        .stdin(if request.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut owned = Owned {
        child: command.spawn()?,
        reaped: false,
    };
    if let Some(input) = request.input {
        let mut stdin = owned.child.stdin.take().ok_or_else(closed)?;
        stdin.write_all(input)?;
    }
    let mut stdout = owned.child.stdout.take().ok_or_else(closed)?;
    let mut stderr = owned.child.stderr.take().ok_or_else(closed)?;
    for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
        // SAFETY: fcntl only sets nonblocking mode on these privately held pipes.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let deadline = Instant::now()
        .checked_add(request.timeout)
        .ok_or_else(|| super::invalid("consumer command deadline exceeded"))?;
    let pid = i32::try_from(owned.child.id()).map_err(|_| closed())?;
    let mut output = Vec::new();
    let mut diagnostics = 0_usize;
    let mut open = [true, true];
    while open.iter().any(|open| *open) {
        if Instant::now() >= deadline {
            return Err(super::invalid("consumer command deadline exceeded"));
        }
        let mut polls = [
            libc::pollfd {
                fd: if open[0] { stdout.as_raw_fd() } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if open[1] { stderr.as_raw_fd() } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100));
        // SAFETY: two initialized pollfd entries live for the duration of the call.
        let ready = unsafe {
            libc::poll(
                polls.as_mut_ptr(),
                2,
                i32::try_from(wait.as_millis()).unwrap_or(100),
            )
        };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        for (index, (poll, open)) in polls.iter().zip(open.iter_mut()).enumerate() {
            if !*open || poll.revents == 0 {
                continue;
            }
            let mut buffer = [0_u8; 4096];
            let read = if index == 0 {
                stdout.read(&mut buffer)
            } else {
                stderr.read(&mut buffer)
            };
            match read {
                Ok(0) => *open = false,
                Ok(count) => {
                    let total = if index == 0 {
                        output.extend(buffer.iter().take(count));
                        output.len()
                    } else {
                        diagnostics = diagnostics.saturating_add(count);
                        diagnostics
                    };
                    if total > request.limit {
                        return Err(super::invalid("consumer command output exceeds bound"));
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: observe only our child and keep its PID reserved until group cleanup.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                pid.cast_unsigned(),
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful waitid initializes the zeroed siginfo, including no-status.
        if unsafe { info.assume_init().si_pid() } == pid {
            break;
        }
        if Instant::now() >= deadline {
            return Err(super::invalid("consumer command deadline exceeded"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    bounded_command::kill_tree(&mut owned.child);
    let status = owned.child.wait()?;
    owned.reaped = true;
    if status.success() {
        Ok(output)
    } else {
        Err(io::Error::other(CommandFailed))
    }
}

fn closed() -> io::Error {
    io::Error::other("consumer command pipe is unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Vec<OsString> {
        vec!["/bin/sh".into(), "-c".into(), script.into()]
    }

    #[test]
    fn bounded_native_command() {
        assert_eq!(
            run_fixed(&shell("printf ok"), 4096, Duration::from_secs(10)).unwrap(),
            b"ok"
        );
        let failed = run_fixed(&shell("exit 7"), 4096, Duration::from_secs(10)).unwrap_err();
        assert!(is_command_failure(&failed));
        for (script, timeout) in [
            ("head -c 4097 /dev/zero", Duration::from_secs(10)),
            ("head -c 4097 /dev/zero >&2", Duration::from_secs(10)),
            ("sleep 60 & exit 0", Duration::from_millis(100)),
        ] {
            let error = run_fixed(&shell(script), 4096, timeout).unwrap_err();
            assert!(!is_command_failure(&error), "{script}");
        }
        // Cargo exports CARGO_MANIFEST_DIR to this test; the probe never sees it.
        assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
        let env = [("ONLY", OsString::from("this"))];
        let echoed = run(&Request {
            command: &shell("cat; printf '%s:%s' \"$ONLY\" \"${CARGO_MANIFEST_DIR-unset}\""),
            env: &env,
            input: Some(b"in:"),
            limit: 4096,
            timeout: Duration::from_secs(10),
        })
        .unwrap();
        assert_eq!(echoed, b"in:this:unset");
    }

    #[test]
    fn deadline_retires_child_holding_stdout_after_leader_exit() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("child.pid");
        let command = [
            "/bin/sh".into(),
            "-c".into(),
            "sleep 60 & echo $! > \"$1\"; exit 0".into(),
            "fixture".into(),
            pid_file.clone().into_os_string(),
        ];
        assert!(run_fixed(&command, 4096, Duration::from_millis(250)).is_err());
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SAFETY: pidfd_open observes the captured synthetic child; it grants no signal.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
        } else {
            use std::os::fd::FromRawFd;
            // SAFETY: successful pidfd_open returns this uniquely owned descriptor.
            let file = unsafe { std::fs::File::from_raw_fd(i32::try_from(fd).unwrap()) };
            let mut poll = libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: writable single pollfd retained throughout the bounded wait.
            assert_eq!(unsafe { libc::poll(&raw mut poll, 1, 1000) }, 1);
            assert_ne!(
                poll.revents & libc::POLLIN,
                0,
                "Synthetic descendant remained live"
            );
        }
    }
}
