//! The app's process: started as the keeper's child in a process group of its own, ended with
//! the keeper (a kept app without a display is worse than a closed one).

use std::ffi::OsString;
use std::io::{self, Read, Write as _};
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::wire::End;

/// What the app is told of X11.
pub(crate) enum X11 {
    /// The launch's own display, `:N`, and the file its cookie is in.
    Display(u32, std::path::PathBuf),
    /// The keeper was to serve one and could not: no display at all, never one the app's
    /// environment named before.
    Missing,
    /// Whatever its environment says.
    Unchanged,
}

pub(crate) struct App {
    child: Child,
    /// The OOM kills the cgroup the keeper and its app share had counted when the app started.
    oom_kills: Option<u64>,
    pub pid: u32,
    pub started_at: Option<u64>,
    /// Readable once the process ended.
    pub pidfd: OwnedFd,
    /// Set by each of the readers of what the app wrote, once it read the stream to its end.
    streams: Vec<Arc<AtomicBool>>,
}

impl App {
    /// Start `argv` with `socket` as its Wayland display, and `x11` as its X11 one, keeping what
    /// it writes in `dir` for the desktop that follows a restart.
    pub(crate) fn spawn(
        argv: &[OsString],
        socket: &Path,
        x11: X11,
        dir: &Path,
    ) -> io::Result<Self> {
        let Some((program, rest)) = argv.split_first() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no command to start",
            ));
        };
        let keeper = std::process::id();
        let mut command = Command::new(program);
        command
            .args(rest)
            // An absolute path: it reaches the keeper's socket whatever runtime directory the
            // app (or the sandbox it starts) sees.
            .env("WAYLAND_DISPLAY", socket)
            .env_remove("WAYLAND_SOCKET")
            .process_group(0)
            // Read here, never left to a desktop: the session reading the keeper's own streams
            // goes away in a restart, and the app must not block on a pipe nobody drains.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match x11 {
            X11::Display(number, authority) => {
                command
                    .env("DISPLAY", format!(":{number}"))
                    .env("XAUTHORITY", authority);
            }
            X11::Missing => {
                command.env_remove("DISPLAY").env_remove("XAUTHORITY");
            }
            X11::Unchanged => {}
        }
        // SAFETY: only async-signal-safe calls between fork and exec: signal, prctl, getppid and
        // _exit.
        unsafe {
            command.pre_exec(move || {
                // The keeper ignores SIGPIPE; the app starts with the default every program
                // expects.
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                // The keeper may have died before the request took hold.
                if u32::try_from(libc::getppid()).ok() != Some(keeper) {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let pid = child.id();
        let pidfd = pidfd_open(pid)?;
        let mut streams = Vec::new();
        if let Some(pipe) = child.stdout.take() {
            keep_stream(pipe, Stream::Out, dir, &mut streams);
        }
        if let Some(pipe) = child.stderr.take() {
            keep_stream(pipe, Stream::Err, dir, &mut streams);
        }
        Ok(Self {
            started_at: tilo_process::start_time(pid),
            oom_kills: own_oom_kills(),
            child,
            pid,
            pidfd,
            streams,
        })
    }

    /// How the process ended, once it did.
    pub(crate) fn try_end(&mut self) -> Option<End> {
        let end = match self.child.try_wait() {
            Ok(Some(status)) => Some(match status.signal() {
                // Read while the keeper still holds the cgroup open: once its last process ends
                // the cgroup is removed, and with it what the kernel counted.
                Some(signal) => End::Signalled {
                    signal,
                    memory: signal == libc::SIGKILL
                        && own_oom_kills().is_some_and(|now| now > self.oom_kills.unwrap_or(0)),
                },
                None => End::Exited {
                    code: status.code().unwrap_or_default(),
                },
            }),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "the app's process could not be waited for");
                None
            }
        }?;
        // The session that follows hears how the app ended only once what it wrote in its last
        // instant is in the keeper's file.
        self.drain(Duration::from_millis(500));
        Some(end)
    }

    /// Wait for the readers to reach the end of their streams, or `within` passes: a program of
    /// the app's still holding a stream open keeps its reader waiting longer than the report can.
    fn drain(&self, within: Duration) {
        let deadline = Instant::now().checked_add(within);
        for stream in &self.streams {
            while !stream.load(Ordering::Acquire)
                && deadline.is_none_or(|deadline| Instant::now() < deadline)
            {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// Send `signal` to the app's process group.
    pub(crate) fn signal(&self, signal: i32) {
        let Ok(group) = libc::pid_t::try_from(self.pid) else {
            return;
        };
        // SAFETY: kill takes plain integers; a negative pid names the group the app leads.
        if unsafe { libc::kill(group.saturating_neg(), signal) } != 0 {
            tracing::warn!(error = %io::Error::last_os_error(), signal, "the app could not be signalled");
        }
    }
}

pub(crate) fn pidfd_open(pid: u32) -> io::Result<OwnedFd> {
    let pid = libc::pid_t::try_from(pid).map_err(io::Error::other)?;
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = i32::try_from(fd).map_err(io::Error::other)?;
    // SAFETY: the kernel just returned this descriptor, owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// One of the app's standard streams, as the keeper passes it on.
#[derive(Clone, Copy)]
enum Stream {
    Out,
    Err,
}

impl Stream {
    fn name(self) -> &'static str {
        match self {
            Self::Out => "stdout",
            Self::Err => "stderr",
        }
    }

    /// Write what the app wrote on this stream of the keeper's, which the launcher passes to the
    /// session. A session that is going away reads nothing; the write ends in a broken pipe,
    /// which is no reason to stop reading what the app says.
    fn write(self, bytes: &[u8]) {
        match self {
            Self::Out => {
                let mut out = io::stdout().lock();
                let _ = out.write_all(bytes);
                let _ = out.flush();
            }
            Self::Err => {
                let mut err = io::stderr().lock();
                let _ = err.write_all(bytes);
                let _ = err.flush();
            }
        }
    }
}

/// Read the app's `stream` from `pipe`, or say why nobody reads it: an app blocks when the pipe
/// fills.
fn keep_stream(
    pipe: impl Read + Send + 'static,
    stream: Stream,
    dir: &Path,
    streams: &mut Vec<Arc<AtomicBool>>,
) {
    match pass_on(pipe, stream, crate::output::path(dir, stream.name())) {
        Ok(ended) => streams.push(ended),
        Err(error) => tracing::warn!(
            %error,
            stream = stream.name(),
            "an app's stream is read by no one: it blocks when the pipe fills"
        ),
    }
}

/// Read `from` to its end: what the app wrote goes on `to` and into the bounded file at `path`,
/// which a desktop that comes after a restart reads. The returned flag is set once the stream
/// ended.
fn pass_on(
    mut from: impl Read + Send + 'static,
    to: Stream,
    path: PathBuf,
) -> io::Result<Arc<AtomicBool>> {
    let ended = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&ended);
    std::thread::Builder::new()
        .name(format!("app-{}", to.name()))
        .spawn(move || {
            let mut kept = match crate::output::Ring::create(&path) {
                Ok(ring) => Some(ring),
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "what the app writes is not kept for the desktop that follows"
                    );
                    None
                }
            };
            let mut buffer = [0_u8; 4096];
            loop {
                match from.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let bytes = buffer.get(..read).unwrap_or_default();
                        let written = kept.as_mut().is_none_or(|ring| match ring.write(bytes) {
                            Ok(()) => true,
                            Err(error) => {
                                tracing::warn!(
                                    path = %path.display(),
                                    %error,
                                    "what the app writes is not kept"
                                );
                                false
                            }
                        });
                        if !written {
                            kept = None;
                        }
                        to.write(bytes);
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            finished.store(true, Ordering::Release);
        })?;
    Ok(ended)
}

/// The OOM kills `memory.events` of the keeper's own cgroup counts: those of its app, which shares
/// it.
fn own_oom_kills() -> Option<u64> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = cgroup.lines().find_map(|line| line.strip_prefix("0::"))?;
    oom_kills_in(&Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

/// The OOM kills the cgroup `dir` counts, its own and whole-group ones.
fn oom_kills_in(dir: &Path) -> Option<u64> {
    let events = std::fs::read_to_string(dir.join("memory.events")).ok()?;
    let count = |key: &str| {
        events
            .lines()
            .find_map(|line| {
                line.strip_prefix(key)?
                    .strip_prefix(' ')?
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .unwrap_or(0)
    };
    Some(count("oom_kill").saturating_add(count("oom_group_kill")))
}

#[cfg(test)]
mod tests {
    use super::oom_kills_in;

    #[test]
    fn a_cgroups_oom_kills_are_read_from_its_memory_events() {
        let dir = tempfile::tempdir().expect("a directory");
        assert_eq!(oom_kills_in(dir.path()), None, "no cgroup there");
        std::fs::write(
            dir.path().join("memory.events"),
            "low 0\nhigh 0\nmax 12\noom 2\noom_kill 1\noom_group_kill 0\n",
        )
        .expect("the events");
        assert_eq!(oom_kills_in(dir.path()), Some(1));
    }
}
