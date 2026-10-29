//! Programs a Tilo-side shell runs in its session's guest, one command at a
//! time (tier 3).
//!
//! The shell runs outside every guest ([`crate::tilo_side`]). When a command
//! is none of its own, it hands exactly that command to the session's guest:
//! the guest's first process is `tilo-ush exec-serve`, holding one end of a
//! socket pair at [`CONTROL_FD`]. For each command the shell sends, on that
//! control socket, a fresh socket of the command's own; on it, the
//! [`Request`] with the command's three standard streams as descriptors, and
//! the server answers once, when the program ended ([`Answer`]). Several
//! commands run at once (`a | b`, both programs), each on its own socket.
//!
//! The streams are what the shell's are: the session's terminal itself
//! where the command's stream is the terminal (an editor needs it), else a
//! pipe the shell feeds or drains. A program reading the terminal gets it in
//! the foreground: the server makes the program's process group the
//! terminal's, so Ctrl-C and Ctrl-Z reach the program, and the shell takes
//! the terminal back when it ends. Otherwise Ctrl-C reaches the shell, which
//! passes it on as a [`Signal`].
//!
//! What a program writes in the guest lands on disk directly, through the
//! guest's binds of the granted directories, as it did under bash: folding
//! those writes into the environment's actions is the mount's work, still to
//! come.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use runtime_portal::fd_transport::{recv_with_fds, send_with_fd, send_with_fds};

use crate::broker::{poll_fds, pollfd};

/// Where the guest's first process holds the control socket.
pub const CONTROL_FD: RawFd = 4;

/// The argument that names [`CONTROL_FD`] to `exec-serve`.
pub const CONTROL_FD_ARG: &str = "--control-fd";

/// One command, as the shell asks for it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub argv: Vec<String>,
    /// The program's whole environment.
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

/// What became of a command.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "answer", rename_all = "kebab-case")]
pub enum Answer {
    Exited {
        status: i32,
    },
    /// The guest has no such program.
    NotFound,
    /// It could not be started.
    Failed {
        reason: String,
    },
}

/// What the shell passes on to a running program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "signal", rename_all = "kebab-case")]
pub enum Signal {
    /// Ctrl-C.
    Interrupt,
    /// `timeout` ran out.
    Kill,
}

// The server: the guest's first process.

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
    // SAFETY: the descriptor was handed to this process to own, and is open.
    let control = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: sets a flag on the descriptor just adopted.
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
    // SAFETY: setting dispositions to SIG_IGN is always sound.
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
    let Some((name, operands)) = request.argv.split_first() else {
        return answer(socket, &Answer::Exited { status: 0 });
    };
    // SAFETY: isatty only asks about the descriptor.
    let terminal = unsafe { libc::isatty(stdin.as_raw_fd()) } == 1;
    let terminal_fd = stdin.try_clone().ok();
    let mut command = std::process::Command::new(name);
    command
        .args(operands)
        .env_clear()
        .envs(request.env)
        .current_dir(&request.cwd)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr);
    {
        use std::os::unix::process::CommandExt as _;
        // A process group of its own, so the terminal can be given to it.
        command.process_group(0);
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
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return answer(socket, &Answer::NotFound);
        }
        Err(error) => {
            return answer(
                socket,
                &Answer::Failed {
                    reason: error.to_string(),
                },
            );
        }
    };
    let group = i32::try_from(child.id()).unwrap_or(0);
    if terminal && let Some(fd) = &terminal_fd {
        // SAFETY: gives the terminal on our own descriptor to the group we
        // just made; SIGTTOU is ignored, so asking from the background is
        // allowed.
        unsafe { libc::tcsetpgrp(fd.as_raw_fd(), group) };
    }
    drop(terminal_fd);
    // What the shell passes on, until the program ends.
    if let Ok(reader) = socket.try_clone() {
        let _ = std::thread::Builder::new()
            .name("exec-signals".into())
            .spawn(move || {
                for line in BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    let signal = match serde_json::from_str::<Signal>(&line) {
                        Ok(Signal::Interrupt) => libc::SIGINT,
                        Ok(Signal::Kill) => libc::SIGKILL,
                        Err(_) => continue,
                    };
                    // SAFETY: signals the program's own process group.
                    unsafe { libc::kill(group.saturating_neg(), signal) };
                }
            });
    }
    let status = loop {
        match child.wait() {
            Ok(status) => break exit_status(status),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break 1,
        }
    };
    answer(socket, &Answer::Exited { status });
}

/// A program's exit status as a shell reports it: 128 plus the signal that
/// ended it.
pub fn exit_status(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|s| s.saturating_add(128)))
        .unwrap_or(1)
}

// The client: the Tilo-side shell's half.

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
    let (ours, theirs) = UnixStream::pair()?;
    send_with_fd(control, b"command", &theirs)?;
    drop(theirs);
    let payload = serde_json::to_vec(request).map_err(std::io::Error::other)?;
    send_with_fds(
        &ours,
        &payload,
        &[streams[0].as_fd(), streams[1].as_fd(), streams[2].as_fd()],
    )?;
    // The program holds them now; ours would keep its pipes open.
    drop(streams);
    let mut reader = BufReader::new(ours.try_clone()?);
    let mut passed_on = false;
    let mut stopped = false;
    loop {
        let mut fds = vec![pollfd(ours.as_raw_fd(), libc::POLLIN)];
        if let Some(interrupts) = interrupts.filter(|_| !passed_on) {
            fds.push(pollfd(interrupts.as_raw_fd(), libc::POLLIN));
        }
        // Looked at only while something may stop it early (`timeout`);
        // otherwise this sleeps until the guest answers or Ctrl-C.
        poll_fds(&mut fds, stop.map(|_| Duration::from_millis(20)))?;
        if !stopped && stop.is_some_and(|stop| stop()) {
            stopped = true;
            signal(&ours, Signal::Kill);
        }
        if fds.get(1).is_some_and(|fd| fd.revents != 0) {
            passed_on = true;
            signal(&ours, Signal::Interrupt);
        }
        if fds.first().is_some_and(|fd| fd.revents != 0) {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the guest went away before the program ended",
                ));
            }
            return serde_json::from_str(&line).map_err(std::io::Error::other);
        }
    }
}

fn signal(socket: &UnixStream, signal: Signal) {
    if let Ok(mut line) = serde_json::to_vec(&signal) {
        line.push(b'\n');
        let _ = (&*socket).write_all(&line);
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

    /// The server on a thread, the client asking it: a command runs with the
    /// streams it was handed, a missing one is not found, and `timeout`'s
    /// stop kills one that would run on.
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
