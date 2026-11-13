//! Programs a Tilo-side shell runs in its session's guest, one command at a time.
//!
//! The shell runs outside every guest ([`crate::tilo_side`]) and hands a command that is
//! none of its own to the guest's `tilo-ush exec-serve`, holding one end of a socket pair
//! at [`CONTROL_FD`]. For each command it sends a fresh socket of the command's own, on
//! which the [`Request`] and the three standard streams travel; the server answers once,
//! when the program ended ([`Answer`]).
//!
//! A program reading the terminal gets it in the foreground: the server makes the
//! program's process group the terminal's, so Ctrl-C and Ctrl-Z reach it, and the shell
//! takes the terminal back when it ends. Otherwise Ctrl-C reaches the shell, which passes
//! it on as a [`Signal`]. A program that is part of one of the shell's jobs joins the
//! job's process group ([`InJob`]), and a stopped program is reported
//! ([`Answer::Stopped`]) before the final answer.
//!
//! What a program writes lands on disk through the guest's binds, as it did under bash;
//! folding those writes into the environment's actions is the mount's work.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use runtime_portal::fd_transport::{recv_with_fds, send_with_fd, send_with_fds};

use crate::broker::{poll_fds, pollfd};

pub const CONTROL_FD: RawFd = 4;

pub const CONTROL_FD_ARG: &str = "--control-fd";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<InJob>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InJob {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<i32>,
    /// The job has the terminal: so does the program's group.
    pub foreground: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "answer", rename_all = "kebab-case")]
pub enum Answer {
    Exited {
        status: i32,
    },
    NotFound,
    /// It could not be started.
    Failed {
        reason: String,
    },
    Started {
        group: i32,
    },
    Stopped,
}

impl Answer {
    pub const fn is_final(&self) -> bool {
        matches!(
            self,
            Self::Exited { .. } | Self::NotFound | Self::Failed { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "signal", rename_all = "kebab-case")]
pub enum Signal {
    Interrupt,
    Kill,
    Terminate,
    /// Stop the program's group until it is continued.
    Stop,
    Continue {
        foreground: bool,
    },
}

impl Signal {
    const fn number(self) -> libc::c_int {
        match self {
            Self::Interrupt => libc::SIGINT,
            Self::Kill => libc::SIGKILL,
            Self::Terminate => libc::SIGTERM,
            Self::Stop => libc::SIGTSTP,
            Self::Continue { .. } => libc::SIGCONT,
        }
    }
}

/// `tilo-ush exec-serve --control-fd 4`: run each command the control socket
/// hands over, until it closes. Answers the exit status.
pub fn serve_main(args: &[String]) -> i32 {
    let fd = match args {
        [flag, fd] if flag == CONTROL_FD_ARG => fd.parse::<RawFd>().ok(),
        _ => None,
    };
    let Some(fd) = fd.filter(|fd| *fd >= 3 && valid_fd(*fd)) else {
        eprintln!("exec-serve: needs {CONTROL_FD_ARG} with an open descriptor");
        return 2;
    };
    let control = unsafe { OwnedFd::from_raw_fd(fd) };
    unsafe { libc::fcntl(control.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    ignore_terminal_signals();
    serve(&control);
    0
}

fn valid_fd(fd: RawFd) -> bool {
    // SAFETY: F_GETFD only probes the descriptor.
    unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
}

/// The server answers to nothing the terminal sends: it is never the
/// terminal's foreground, and gives the terminal to the programs it starts.
fn ignore_terminal_signals() {
    unsafe {
        for signal in [
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGTSTP,
            libc::SIGTTIN,
            libc::SIGTTOU,
            libc::SIGPIPE,
        ] {
            libc::signal(signal, libc::SIG_IGN);
        }
    }
}

/// Run every command `control` hands over, each on a thread of its own,
/// until the shell closes it.
pub fn serve(control: &OwnedFd) {
    loop {
        match recv_with_fds(control.as_fd()) {
            Ok((payload, _)) if payload.is_empty() => return,
            Ok((_, mut fds)) => {
                let Some(socket) = fds.pop().filter(|_| fds.is_empty()) else {
                    continue;
                };
                let socket = UnixStream::from(socket);
                let _ = std::thread::Builder::new()
                    .name("exec-command".into())
                    .spawn(move || serve_command(&socket));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn answer(socket: &UnixStream, answer: &Answer) {
    if let Ok(mut line) = serde_json::to_vec(answer) {
        line.push(b'\n');
        let _ = (&*socket).write_all(&line);
    }
}

fn is_terminal(fd: &OwnedFd) -> bool {
    // SAFETY: isatty only asks about the descriptor.
    unsafe { libc::isatty(fd.as_raw_fd()) == 1 }
}

/// Give the terminal `fd` is to the process group `group`. SIGTTOU is
/// ignored, so asking from the background is allowed.
fn give_terminal(fd: &OwnedFd, group: i32) {
    unsafe { libc::tcsetpgrp(fd.as_raw_fd(), group) };
}

fn serve_command(socket: &UnixStream) {
    let Ok((payload, fds)) = recv_with_fds(socket.as_fd()) else {
        return;
    };
    let Ok(request) = serde_json::from_slice::<Request>(&payload) else {
        return answer(
            socket,
            &Answer::Failed {
                reason: "not a command".into(),
            },
        );
    };
    let Ok([stdin, stdout, stderr]) = <[OwnedFd; 3]>::try_from(fds) else {
        return answer(
            socket,
            &Answer::Failed {
                reason: "a command comes with its three streams".into(),
            },
        );
    };
    let (child, group, terminal) = match start_program(&request, [stdin, stdout, stderr]) {
        Ok(started) => started,
        Err(refused) => return answer(socket, &refused),
    };
    let pid = i32::try_from(child.id()).unwrap_or(0);
    if request.job.is_some() {
        answer(socket, &Answer::Started { group });
    }
    pass_signals(socket, group, terminal);
    let status = if request.job.is_some() {
        wait_in_job(pid, socket)
    } else {
        let mut child = child;
        loop {
            match child.wait() {
                Ok(status) => break exit_status(status),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break 1,
            }
        }
    };
    answer(socket, &Answer::Exited { status });
}

/// Start the program `request` names on `streams`, in a process group of
/// its own or its job's, holding the terminal when it should. Answers it,
/// its group, and the terminal it may hold; or what to answer instead.
fn start_program(
    request: &Request,
    [stdin, stdout, stderr]: [OwnedFd; 3],
) -> Result<(std::process::Child, i32, Option<OwnedFd>), Answer> {
    let Some((name, operands)) = request.argv.split_first() else {
        return Err(Answer::Exited { status: 0 });
    };
    let terminal = {
        let streams: &[&OwnedFd] = if request.job.is_some() {
            &[&stdin, &stdout, &stderr]
        } else {
            &[&stdin]
        };
        streams
            .iter()
            .find(|fd| is_terminal(fd))
            .and_then(|fd| fd.try_clone().ok())
    };
    let mut command = std::process::Command::new(name);
    command
        .args(operands)
        .env_clear()
        .envs(request.env.iter().cloned())
        .current_dir(&request.cwd)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr);
    let joined = request.job.and_then(|job| job.group);
    {
        use std::os::unix::process::CommandExt as _;
        // A process group of its own, or its job's, so the terminal can be
        // given to it.
        command.process_group(joined.unwrap_or(0));
        // SAFETY: between fork and exec this only calls signal(2), which is
        // async-signal-safe: the program gets the terminal's signals back.
        unsafe {
            command.pre_exec(|| {
                for signal in [
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTSTP,
                    libc::SIGTTIN,
                    libc::SIGTTOU,
                    libc::SIGPIPE,
                ] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                Ok(())
            });
        }
    }
    let spawned = match command.spawn() {
        Err(error) if joined.is_some() && error.raw_os_error() == Some(libc::EPERM) => {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
            command.spawn().map(|child| (child, false))
        }
        spawned => spawned.map(|child| (child, joined.is_some())),
    };
    let (child, in_group) = match spawned {
        Ok(spawned) => spawned,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Answer::NotFound);
        }
        Err(error) => {
            return Err(Answer::Failed {
                reason: error.to_string(),
            });
        }
    };
    let pid = i32::try_from(child.id()).unwrap_or(0);
    let group = match joined {
        Some(group) if in_group => group,
        _ => pid,
    };
    let foreground = request.job.is_none_or(|job| job.foreground);
    if foreground && let Some(fd) = &terminal {
        give_terminal(fd, group);
    }
    Ok((child, group, terminal))
}

/// Pass what the shell sends on `socket` on to the process group `group`,
/// until the program ends, giving it `terminal` when it continues in the
/// foreground.
fn pass_signals(socket: &UnixStream, group: i32, terminal: Option<OwnedFd>) {
    // What the shell passes on, until the program ends.
    if let Ok(reader) = socket.try_clone() {
        let _ = std::thread::Builder::new()
            .name("exec-signals".into())
            .spawn(move || {
                for line in BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    let Ok(signal) = serde_json::from_str::<Signal>(&line) else {
                        continue;
                    };
                    if let (Signal::Continue { foreground: true }, Some(fd)) = (signal, &terminal) {
                        give_terminal(fd, group);
                    }
                    unsafe { libc::kill(group.saturating_neg(), signal.number()) };
                }
            });
    }
}

/// Wait for a job's program to end, saying each time it stops. A stop the
/// shell already took back (`fg` before this saw it) is no stop.
fn wait_in_job(pid: i32, socket: &UnixStream) -> i32 {
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: waits for our own child, writing only `status`.
        let waited = unsafe { libc::waitpid(pid, &raw mut status, libc::WUNTRACED) };
        if waited < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return 1;
        }
        if libc::WIFSTOPPED(status) {
            let mut again: libc::c_int = 0;
            let changed =
                unsafe { libc::waitpid(pid, &raw mut again, libc::WCONTINUED | libc::WNOHANG) }
                    == pid;
            if changed && libc::WIFEXITED(again) {
                return libc::WEXITSTATUS(again);
            }
            if changed && libc::WIFSIGNALED(again) {
                return libc::WTERMSIG(again).saturating_add(128);
            }
            if !(changed && libc::WIFCONTINUED(again)) {
                answer(socket, &Answer::Stopped);
            }
            continue;
        }
        if libc::WIFEXITED(status) {
            return libc::WEXITSTATUS(status);
        }
        if libc::WIFSIGNALED(status) {
            return libc::WTERMSIG(status).saturating_add(128);
        }
    }
}

pub fn exit_status(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|s| s.saturating_add(128)))
        .unwrap_or(1)
}

/// One command handed to the guest, until its final answer.
pub struct Command {
    socket: UnixStream,
    reader: BufReader<UnixStream>,
}

#[derive(Debug, Default)]
pub struct Woke {
    pub answer: Option<Answer>,
    pub ready: Vec<bool>,
}

pub struct Signaller(UnixStream);

impl Signaller {
    pub fn send(&self, signal: Signal) {
        if let Ok(mut line) = serde_json::to_vec(&signal) {
            line.push(b'\n');
            let _ = (&self.0).write_all(&line);
        }
    }
}

impl Command {
    pub fn start(
        control: &UnixStream,
        request: &Request,
        streams: [OwnedFd; 3],
    ) -> std::io::Result<Self> {
        let (ours, theirs) = UnixStream::pair()?;
        send_with_fd(control, b"command", &theirs)?;
        drop(theirs);
        let payload = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        send_with_fds(
            &ours,
            &payload,
            &[streams[0].as_fd(), streams[1].as_fd(), streams[2].as_fd()],
        )?;
        drop(streams);
        let reader = BufReader::new(ours.try_clone()?);
        Ok(Self {
            socket: ours,
            reader,
        })
    }

    pub fn signaller(&self) -> std::io::Result<Signaller> {
        Ok(Signaller(self.socket.try_clone()?))
    }

    pub fn signal(&self, signal: Signal) {
        Signaller(match self.socket.try_clone() {
            Ok(socket) => socket,
            Err(_) => return,
        })
        .send(signal);
    }

    /// Wait for the server's next answer, until one of `wakes` is readable,
    /// or until `timeout`.
    ///
    /// # Errors
    ///
    /// The guest went away before the command's final answer.
    pub fn next(
        &mut self,
        wakes: &[BorrowedFd<'_>],
        timeout: Option<Duration>,
    ) -> std::io::Result<Woke> {
        // An answer already read along with the one before it.
        if self.reader.buffer().contains(&b'\n') {
            return Ok(Woke {
                answer: Some(self.read_answer()?),
                ready: vec![false; wakes.len()],
            });
        }
        let mut fds = vec![pollfd(self.socket.as_raw_fd(), libc::POLLIN)];
        fds.extend(wakes.iter().map(|fd| pollfd(fd.as_raw_fd(), libc::POLLIN)));
        poll_fds(&mut fds, timeout)?;
        let ready = fds.iter().skip(1).map(|fd| fd.revents != 0).collect();
        let answer = match fds.first() {
            Some(fd) if fd.revents != 0 => Some(self.read_answer()?),
            _ => None,
        };
        Ok(Woke { answer, ready })
    }

    fn read_answer(&mut self) -> std::io::Result<Answer> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the guest went away before the program ended",
            ));
        }
        serde_json::from_str(&line).map_err(std::io::Error::other)
    }
}

/// Run `request` on the guest whose control socket is `control`, with
/// `streams` as its standard input, output and error. Waits until it ends,
/// passing Ctrl-C on when `interrupts` becomes readable and killing it once
/// `stop` says so.
///
/// # Errors
///
/// The guest did not take the command or went away before answering.
pub fn run(
    control: &UnixStream,
    request: &Request,
    streams: [OwnedFd; 3],
    interrupts: Option<BorrowedFd<'_>>,
    stop: Option<&dyn Fn() -> bool>,
) -> std::io::Result<Answer> {
    let mut command = Command::start(control, request, streams)?;
    let mut passed_on = false;
    let mut stopped = false;
    loop {
        let wakes: Vec<BorrowedFd<'_>> = interrupts.filter(|_| !passed_on).into_iter().collect();
        // Looked at only while something may stop it early (`timeout`);
        // otherwise this sleeps until the guest answers or Ctrl-C.
        let woke = command.next(&wakes, stop.map(|_| Duration::from_millis(20)))?;
        if !stopped && stop.is_some_and(|stop| stop()) {
            stopped = true;
            command.signal(Signal::Kill);
        }
        if woke.ready.first().copied().unwrap_or(false) {
            passed_on = true;
            command.signal(Signal::Interrupt);
        }
        if let Some(answer) = woke.answer.filter(Answer::is_final) {
            return Ok(answer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn null() -> OwnedFd {
        std::fs::File::open("/dev/null").unwrap().into()
    }

    fn pipe() -> (OwnedFd, OwnedFd) {
        let (reader, writer) = std::io::pipe().unwrap();
        (reader.into(), writer.into())
    }

    #[test]
    fn a_command_runs_on_the_streams_it_was_handed() {
        let (control, served) = UnixStream::pair().unwrap();
        let served = OwnedFd::from(served);
        let serving = std::thread::spawn(move || serve(&served));

        let (read_out, write_out) = pipe();
        let request = Request {
            argv: vec!["sh".into(), "-c".into(), "echo hi; exit 3".into()],
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            cwd: "/".into(),
            job: None,
        };
        let answer = run(&control, &request, [null(), write_out, null()], None, None).unwrap();
        assert_eq!(answer, Answer::Exited { status: 3 });
        let mut printed = String::new();
        std::io::Read::read_to_string(&mut std::fs::File::from(read_out), &mut printed).unwrap();
        assert_eq!(printed, "hi\n");

        let missing = Request {
            argv: vec!["no-such-program-here".into()],
            ..request.clone()
        };
        let answer = run(&control, &missing, [null(), null(), null()], None, None).unwrap();
        assert_eq!(answer, Answer::NotFound);

        let forever = Request {
            argv: vec!["sleep".into(), "30".into()],
            ..request
        };
        let asked = std::time::Instant::now();
        let stop = || asked.elapsed() > Duration::from_millis(100);
        let answer = run(
            &control,
            &forever,
            [null(), null(), null()],
            None,
            Some(&stop),
        )
        .unwrap();
        assert_eq!(
            answer,
            Answer::Exited {
                status: 128 + libc::SIGKILL
            }
        );

        drop(control);
        serving.join().unwrap();
    }
}
