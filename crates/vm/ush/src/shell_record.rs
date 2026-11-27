//! What each shell the supervisor starts acts as, told to this computer's daemon.
//!
//! A shell the supervisor starts (the device's own login shell, an environment's guest) is its
//! child, a process of the person's account outside every app sandbox: the daemon would judge it,
//! and every program typed in it, as the person. So as it starts one, the supervisor tells the
//! daemon which app the shell is for and in which workspace
//! ([`runtime_portal::native::ShellRecord`]), with the start time it read then. The daemon judges
//! that process and everything below it as that app, with the app's grants in that workspace,
//! and never with the person's authority.
//!
//! That judgement needs the daemon to see the shell's processes, which it does where it runs as
//! the person (macOS, a development run). On Tilo OS it runs as an account of its own and sees
//! none of theirs. So each shell is also served an endpoint of its own ([`ShellEndpoint`]), named
//! to its programs in place of the daemon's: every connection made there is carried over one the
//! daemon makes for the app, and acts as the app whatever process holds it.
//!
//! A computer whose daemon is not running has nothing to judge a connection by: the shell starts
//! and nothing is recorded. A daemon that refuses the record is answered by not starting the
//! shell.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use runtime_portal::native::{ShellLine, ShellRecord};

/// How long the daemon has to answer a record.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// The most a record's answer may hold.
const MAX_ANSWER: u64 = 64 * 1024;

/// How a record was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// The daemon recorded the shell, in this workspace.
    In(String),
    /// The daemon cannot see this account's processes (it runs as an account of its own): the
    /// shell's programs reach it as the app through the shell's endpoint ([`ShellEndpoint`]).
    Unseen,
    /// No daemon runs on this computer to record it.
    NoDaemon,
}

/// Tell this computer's daemon that `pid`, a shell just started, is `app`'s in `workspace`
/// (the runtime's primary one when it is that).
///
/// # Errors
/// The daemon refused the record, saying why, or did not answer it.
pub fn record(
    pid: u32,
    app: &str,
    workspace: &crate::workspace::WorkspaceId,
) -> Result<Recorded, String> {
    let Some(daemon) = crate::images::device_actions() else {
        return Ok(Recorded::NoDaemon);
    };
    let workspace = if workspace.is_primary() {
        ""
    } else {
        workspace.as_str()
    };
    record_at(daemon.native(), pid, app, workspace)
}

/// [`record`], told to the daemon whose native endpoint is `native`, `workspace` being empty for
/// the runtime's primary one.
///
/// # Errors
/// As [`record`].
pub fn record_at(native: &Path, pid: u32, app: &str, workspace: &str) -> Result<Recorded, String> {
    let started =
        start_time(pid).ok_or_else(|| format!("the shell (process {pid}) ended as it started"))?;
    let mut stream = match tilo_files::native::socket::connect(native) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(Recorded::NoDaemon);
        }
        Err(error) => return Err(format!("this computer's daemon is not reached: {error}")),
    };
    let unanswered = |error: io::Error| format!("this computer's daemon did not answer: {error}");
    stream
        .set_read_timeout(Some(ANSWER_WITHIN))
        .map_err(unanswered)?;
    let line = ShellRecord::new(pid, started, app, workspace)
        .line()
        .map_err(|error| error.to_string())?;
    stream.write_all(&line).map_err(unanswered)?;
    stream.flush().map_err(unanswered)?;
    let mut answer = String::new();
    BufReader::new(io::Read::take(&stream, MAX_ANSWER))
        .read_line(&mut answer)
        .map_err(unanswered)?;
    let answer: serde_json::Value = serde_json::from_str(answer.trim())
        .map_err(|error| format!("this computer's daemon answered no record: {error}"))?;
    if let Some(why) = answer.get("error").and_then(serde_json::Value::as_str) {
        return Err(why.to_owned());
    }
    if answer.get("unseen") == Some(&serde_json::Value::Bool(true)) {
        return Ok(Recorded::Unseen);
    }
    answer
        .get("workspace")
        .and_then(serde_json::Value::as_str)
        .map(|workspace| Recorded::In(workspace.to_owned()))
        .ok_or_else(|| format!("this computer's daemon answered no record: {answer}"))
}

/// The endpoint a shell's programs reach this computer's daemon at: a socket of the shell's own,
/// named to them by [`tilo_machine::DAEMON_NATIVE_VAR`]. For each connection a program makes
/// there the runtime asks the daemon for a connection of the shell's app in its workspace
/// ([`runtime_portal::native::ShellLine`]) and carries the program's bytes over it, so the program
/// is answered as the app whether or not the daemon can see its process: on Tilo OS the daemon
/// runs as an account of its own, and sees none of the person's. Gone with the shell's session.
pub struct ShellEndpoint {
    path: PathBuf,
    listener: Arc<UnixListener>,
}

impl ShellEndpoint {
    /// Serve the shell `session` of `app` in `workspace` (empty: the runtime's primary one) an
    /// endpoint in `dir`, its connections carried to the daemon at `native`.
    ///
    /// # Errors
    /// The socket cannot be made.
    pub fn serve(
        dir: &Path,
        session: &str,
        native: PathBuf,
        (app, workspace): (&str, &str),
    ) -> io::Result<Self> {
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let path = dir.join(format!("{session}.sock"));
        let _ = std::fs::remove_file(&path);
        let listener = Arc::new(tilo_files::native::socket::bind(&path)?);
        let accepting = Arc::clone(&listener);
        let ask = ShellLine::new(app, workspace)
            .line()
            .map_err(io::Error::other)?;
        std::thread::Builder::new()
            .name("ush-shell-daemon".into())
            .spawn(move || {
                for program in accepting.incoming() {
                    let Ok(program) = program else {
                        return;
                    };
                    let (native, ask) = (native.clone(), ask.clone());
                    let _ = std::thread::Builder::new()
                        .name("ush-shell-line".into())
                        .spawn(move || carry(&program, &native, &ask));
                }
            })?;
        Ok(Self { path, listener })
    }

    /// Where the shell's programs reach the daemon: what [`tilo_machine::DAEMON_NATIVE_VAR`] names
    /// in the shell's environment.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ShellEndpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        // Wakes the accepting thread, which ends; connections already carried go on until their
        // program or the daemon closes them.
        // SAFETY: shutdown only changes the state of a socket this endpoint owns.
        unsafe {
            libc::shutdown(self.listener.as_raw_fd(), libc::SHUT_RDWR);
        }
    }
}

/// Carry `program`'s connection over a connection of the shell's app that the daemon at `native`
/// makes when asked `ask`. A refusal is told to the program as the daemon said it.
fn carry(program: &UnixStream, native: &Path, ask: &[u8]) {
    let refuse = |why: &str| {
        let _ = writeln!(
            &*program,
            "{}",
            serde_json::json!({ "error": format!("this shell does not reach Tilo: {why}") })
        );
    };
    let mut daemon = match tilo_files::native::socket::connect(native) {
        Ok(daemon) => daemon,
        Err(error) => return refuse(&error.to_string()),
    };
    if let Err(error) = daemon.write_all(ask).and_then(|()| daemon.flush()) {
        return refuse(&error.to_string());
    }
    let line = match runtime_portal::fd_transport::recv_with_fd(&daemon) {
        Ok((_, Some(line))) => UnixStream::from(line),
        Ok((answer, None)) => {
            let said: serde_json::Value = serde_json::from_slice(&answer).unwrap_or_default();
            let why = said
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map_or_else(
                    || String::from_utf8_lossy(&answer).into_owned(),
                    str::to_owned,
                );
            return refuse(why.trim());
        }
        Err(error) => return refuse(&error.to_string()),
    };
    drop(daemon);
    let (Ok(mut from_program), Ok(mut to_line)) = (program.try_clone(), line.try_clone()) else {
        return;
    };
    let up = std::thread::spawn(move || {
        let _ = io::copy(&mut from_program, &mut to_line);
        let _ = to_line.shutdown(std::net::Shutdown::Write);
    });
    let _ = io::copy(&mut &line, &mut &*program);
    let _ = program.shutdown(std::net::Shutdown::Both);
    let _ = up.join();
}

/// When process `pid` started, as the kernel counts it: field 22 of `/proc/<pid>/stat`, what the
/// daemon reads to tell the shell from a later process given its pid.
#[must_use]
pub fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is parenthesised and may hold spaces or parentheses: fields resume after
    // its last `)`, at field 3.
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(22 - 3)?.parse().ok()
}
