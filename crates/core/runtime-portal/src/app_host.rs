//! The device's app host: where a daemon that runs as the device's own account has an app's
//! process started, so the app never runs as that account.
//!
//! The device's daemon decides every launch and keeps its record, but on Tilo OS it runs as the
//! system account `tilo` (root in an image whose daemon is the system's own), and an app is an
//! untrusted package: a process it started itself would hold that account's files, its device
//! key's directory among them, and every descriptor it inherited. An app that keeps a held
//! connection runs with no person signed in, so no person's session can start it either. The
//! daemon hands it to the app host instead: `tilo-app-host.socket` accepts the daemon's
//! connection (it is the daemon's alone, 0600), and systemd starts `tilo-app-host@.service` for
//! it as the unprivileged account `tilo-app` with the connection as its standard input. That
//! process is `tilo-launcher host`, which runs [`serve`]: it starts the launcher's own `run` of
//! the app, in the same sandbox every app runs in, holds it at its launch hold, and tells the
//! daemon its pid with a pidfd that pins it. The daemon records the launch by them and releases
//! it, and only then does the launcher go on.
//!
//! The app never dials the daemon. The daemon makes each of the launch's connections to its
//! services itself (its native line, the collaboration and identity services, document media),
//! bound to the launch it recorded as it makes them, and hands them with the release
//! ([`Handed`]): the service knows whose a connection is because the daemon made it for that
//! launch, so nothing needs to prove it, and no path to the daemon's sockets, which are in a
//! directory only the daemon enters, is ever handed out.
//!
//! The connection stays open while the app runs: what the app writes on its standard error comes
//! back on it as it is written, its end comes back with its wait status, and the daemon stops it
//! by closing its side, which the host answers by ending the app.
//!
//! The host hands the launcher nothing of the daemon's but what the order names and the release
//! carries: the arguments, the environment, the grants file's content, written into a runtime
//! directory of the host's own, and the connections the daemon made. The daemon's state, runtime
//! and socket directories stay out of its reach.

use std::ffi::OsStr;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::packet::{Descriptor, Packet};

/// Where the device's app host listens (`tilo-app-host.socket`).
pub const SOCKET: &str = "/run/tilo-app-host.sock";

/// How long an app has to end after its host asked it to, before it is killed.
const END_GRACE: Duration = Duration::from_secs(10);

/// The most of what an app said that one packet carries back.
const SAID_CHUNK: usize = 8 * 1024;

/// What the daemon asks the app host to start: the launcher's own command for the app, as the
/// daemon would have run it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Order {
    /// The launcher's path.
    pub program: Vec<u8>,
    /// Its arguments, `run` first.
    pub args: Vec<Vec<u8>>,
    /// The variables it is given over the host's own environment, or removed from it (`None`).
    pub env: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    /// The grants the person gave the app here, as the launcher reads them (`--capabilities`):
    /// written into the host's runtime directory, since the daemon's copy is in its own state.
    pub grants: Option<Vec<u8>>,
}

impl Order {
    /// The launcher `program` run with `args`.
    #[must_use]
    pub fn new(program: &Path, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Self {
        Self {
            program: program.as_os_str().as_bytes().to_vec(),
            args: args
                .into_iter()
                .map(|arg| arg.as_ref().as_bytes().to_vec())
                .collect(),
            ..Self::default()
        }
    }
}

/// What the daemon says to the host.
#[derive(Debug, Serialize, Deserialize)]
enum ToHost {
    /// Start this.
    Order(Order),
    /// The launch is recorded: let it go on, with the connections the daemon made for it, and
    /// the connection its launch names, if any, in that order.
    Release {
        #[serde(with = "crate::packet::optional_fd")]
        native: Option<Descriptor>,
        #[serde(with = "crate::packet::optional_fd")]
        collab: Option<Descriptor>,
        #[serde(with = "crate::packet::optional_fd")]
        identity: Option<Descriptor>,
        #[serde(with = "crate::packet::optional_fd")]
        blobs: Option<Descriptor>,
        #[serde(with = "crate::packet::optional_fd")]
        link: Option<Descriptor>,
    },
}

impl Packet for ToHost {}

/// The connections the daemon made for a hosted launch to its services, each bound to the launch
/// it recorded as it was made. The launcher takes them by the names the daemon puts on its command
/// (`--handed-service`), in [`Self::ORDER`], the order the release hands them.
#[derive(Debug, Default)]
pub struct Handed {
    /// The daemon's native line, answered as the app.
    pub native: Option<Descriptor>,
    /// The collaboration service, attested as the launch.
    pub collab: Option<Descriptor>,
    /// The identity service, its handshake written.
    pub identity: Option<Descriptor>,
    /// Document media, its scope fixed.
    pub blobs: Option<Descriptor>,
}

impl Handed {
    /// The services, by the name the launcher knows each by, in the order the release hands them.
    pub const ORDER: [&'static str; 4] = ["native", "collab", "identity", "blobs"];
}

/// What the host says to the daemon.
#[derive(Debug, Serialize, Deserialize)]
enum FromHost {
    /// The launcher started as `pid`, which `process` pins, and waits at its launch hold.
    Started {
        pid: u32,
        #[serde(with = "crate::packet::fd")]
        process: Descriptor,
    },
    /// The launcher could not be started, and why.
    Refused(String),
    /// What it wrote on its standard error.
    Said(Vec<u8>),
    /// It ended, with this wait status.
    Ended { status: i32 },
}

impl Packet for FromHost {}

/// Serve the daemon's order on `link`, the connection the socket's service was handed: start the
/// launcher, hold it until the daemon recorded it, then follow it until it ends or the daemon
/// closes the connection, and say how it ended.
///
/// # Errors
/// The connection carried no order, or the host could not read or answer on it.
pub fn serve(link: &Descriptor) -> io::Result<()> {
    let Some(ToHost::Order(order)) = ToHost::receive(link)? else {
        return Err(invalid("the app host was asked nothing to start"));
    };
    let runtime = match RuntimeDir::make(&order) {
        Ok(runtime) => runtime,
        Err(error) => {
            let said = format!("the app host could not prepare the launch: {error}");
            FromHost::Refused(said.clone()).send(link)?;
            return Err(io::Error::other(said));
        }
    };
    let mut command = Command::new(OsStr::from_bytes(&order.program));
    let mut args = order.args.iter().map(|arg| OsStr::from_bytes(arg));
    // The grants go right after the launcher's subcommand: everything after `--` is the app's.
    if let Some(subcommand) = args.next() {
        command.arg(subcommand);
    }
    if let Some(grants) = &runtime.grants {
        command.arg("--capabilities").arg(grants);
    }
    command.args(args);
    for (name, value) in &order.env {
        let name = OsStr::from_bytes(name);
        match value {
            Some(value) => command.env(name, OsStr::from_bytes(value)),
            None => command.env_remove(name),
        };
    }
    command
        .env("XDG_RUNTIME_DIR", &runtime.dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped());
    let hold = crate::launch_hold::install(&mut command)?;
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let said = format!(
                "the app host could not start {}: {error}",
                OsStr::from_bytes(&order.program).to_string_lossy()
            );
            FromHost::Refused(said.clone()).send(link)?;
            return Err(io::Error::other(said));
        }
    };
    drop(command);
    let pid = child.id();
    let process = match pidfd(pid) {
        Ok(process) => process,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            FromHost::Refused(format!("the app host could not pin its launch: {error}"))
                .send(link)?;
            return Err(error);
        }
    };
    FromHost::Started {
        pid,
        process: process.try_clone()?,
    }
    .send(link)?;
    // The daemon went, or recorded nothing: the launch never goes on.
    let Some(ToHost::Release {
        native,
        collab,
        identity,
        blobs,
        link: launch_link,
    }) = ToHost::receive(link)?
    else {
        drop(hold);
        let _ = child.kill();
        let status = child.wait()?;
        drop(runtime);
        let _ = FromHost::Ended {
            status: raw_status(status),
        }
        .send(link);
        return Ok(());
    };
    let handed = [native, collab, identity, blobs, launch_link];
    let links: Vec<BorrowedFd<'_>> = handed.iter().flatten().map(|fd| fd.as_fd()).collect();
    hold.release_with_links(&links)?;
    let stderr = child.stderr.take();
    let status = follow(link, &process, stderr)?;
    // Reaped once the pidfd said it ended, so this does not wait.
    let status = child.try_wait()?.map_or(status, raw_status);
    // Gone before the daemon hears it ended, so nothing of the launch outlives what it is told.
    drop(runtime);
    let _ = FromHost::Ended { status }.send(link);
    Ok(())
}

/// Follow the released launch until it ends: what it says goes to the daemon as it is said, and a
/// daemon that closed its side has it ended. Its wait status, as far as it is known here.
fn follow(
    link: &Descriptor,
    process: &OwnedFd,
    mut stderr: Option<std::process::ChildStderr>,
) -> io::Result<i32> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let mut daemon_gone = false;
    let mut ending_since: Option<Instant> = None;
    let mut chunk = vec![0_u8; SAID_CHUNK];
    loop {
        let mut fds = Vec::with_capacity(3);
        fds.push(PollFd::new(process, PollFlags::IN));
        let link_at = (!daemon_gone).then(|| {
            let at = fds.len();
            fds.push(PollFd::new(link, PollFlags::IN));
            at
        });
        let stderr_at = stderr.as_ref().map(|pipe| {
            let at = fds.len();
            fds.push(PollFd::from_borrowed_fd(pipe.as_fd(), PollFlags::IN));
            at
        });
        let timeout = ending_since.map(|_| Timespec {
            tv_sec: 1,
            tv_nsec: 0,
        });
        match poll(&mut fds, timeout.as_ref()) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
        let ready = |at: Option<usize>| {
            at.and_then(|at| fds.get(at))
                .is_some_and(|fd| !fd.revents().is_empty())
        };
        let (ended, link_ready, said) = (ready(Some(0)), ready(link_at), ready(stderr_at));
        drop(fds);
        if said && let Some(pipe) = stderr.as_mut() {
            let read = pipe.read(&mut chunk)?;
            match chunk.get(..read).filter(|bytes| !bytes.is_empty()) {
                Some(bytes) => {
                    // The app's words still go where the host's own go, the journal, and to the
                    // daemon, which answers a start that ends as it starts with them.
                    let _ = io::stderr().write_all(bytes);
                    if !daemon_gone {
                        let _ = FromHost::Said(bytes.to_vec()).send(link);
                    }
                }
                None => stderr = None,
            }
        }
        if link_ready {
            // A second release, or anything else, is no order to end.
            if !matches!(ToHost::receive(link), Ok(Some(_))) {
                daemon_gone = true;
                if ending_since.is_none() {
                    ending_since = Some(Instant::now());
                    let _ =
                        rustix::process::pidfd_send_signal(process, rustix::process::Signal::TERM);
                }
            }
        }
        if ended {
            // What it said last is read before its end is told, without waiting on a process it
            // left behind that still holds the pipe.
            if let Some(mut pipe) = stderr.take()
                && nonblocking(pipe.as_fd()).is_ok()
            {
                while let Ok(read) = pipe.read(&mut chunk) {
                    let Some(bytes) = chunk.get(..read).filter(|bytes| !bytes.is_empty()) else {
                        break;
                    };
                    let _ = io::stderr().write_all(bytes);
                    let _ = FromHost::Said(bytes.to_vec()).send(link);
                }
            }
            return Ok(0);
        }
        if ending_since.is_some_and(|since| since.elapsed() >= END_GRACE) {
            let _ = rustix::process::pidfd_send_signal(process, rustix::process::Signal::KILL);
        }
    }
}

/// The host's own runtime directory for one launch, where its grants are written. It holds no
/// service: the launch is handed its connections.
struct RuntimeDir {
    dir: PathBuf,
    grants: Option<PathBuf>,
}

impl RuntimeDir {
    /// Make it under the unit's runtime directory (`$RUNTIME_DIRECTORY`), or the temporary
    /// directory where the host runs outside its unit.
    fn make(order: &Order) -> io::Result<Self> {
        let base = std::env::var_os("RUNTIME_DIRECTORY")
            .filter(|dir| !dir.is_empty())
            .map_or_else(std::env::temp_dir, PathBuf::from);
        let dir = base.join(format!("launch-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let grants = order
            .grants
            .as_ref()
            .map(|grants| {
                let path = dir.join("grants.json");
                std::fs::write(&path, grants).map(|()| path)
            })
            .transpose()?;
        Ok(Self { dir, grants })
    }
}

impl Drop for RuntimeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An app the app host started for this daemon.
#[derive(Debug)]
pub struct Hosted {
    pid: u32,
    process: Arc<OwnedFd>,
    link: Arc<Descriptor>,
    ended: Arc<(Mutex<Option<Ended>>, Condvar)>,
}

/// How an app the app host ran ended: its wait status, when the host said one before it went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ended(pub Option<i32>);

/// What stops an app the app host runs: the daemon's side of its connection.
#[derive(Clone, Debug)]
pub struct Stop(Arc<Descriptor>);

impl Stop {
    /// Ask the host to end the app: it ends it, and says so.
    pub fn stop(&self) {
        let _ = rustix::net::shutdown(&*self.0, rustix::net::Shutdown::Write);
    }
}

/// Have the app host listening at `socket` start `order`, its words handed to `said` as they come.
/// Answers once the launcher waits at its launch hold: [`Hosted::release`] lets it go on.
///
/// # Errors
/// No app host listens there, or it refused the order or could not start it.
pub fn order(
    socket: &Path,
    order: Order,
    mut said: impl FnMut(&[u8]) + Send + 'static,
) -> io::Result<Hosted> {
    let link = rustix::net::socket_with(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::SEQPACKET,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::net::connect(&link, &rustix::net::SocketAddrUnix::new(socket)?)?;
    ToHost::Order(order).send(&link)?;
    let (pid, process) = loop {
        match FromHost::receive(&link)? {
            Some(FromHost::Started { pid, process }) => break (pid, process),
            Some(FromHost::Said(bytes)) => said(&bytes),
            Some(FromHost::Refused(why)) => return Err(io::Error::other(why)),
            Some(FromHost::Ended { status }) => {
                return Err(io::Error::other(format!(
                    "the app host ended its launch before it started ({status})"
                )));
            }
            None => {
                return Err(io::Error::other(
                    "the app host closed its connection before it started the launch",
                ));
            }
        }
    };
    let link = Arc::new(link);
    let ended = Arc::new((Mutex::new(None), Condvar::new()));
    let (reading, ending) = (Arc::clone(&link), Arc::clone(&ended));
    std::thread::Builder::new()
        .name("tilo-app-host".into())
        .spawn(move || {
            // Until the host says the app ended, or its connection ends without saying: then
            // nothing more is known of how.
            let status = loop {
                match FromHost::receive(&reading) {
                    Ok(Some(FromHost::Said(bytes))) => said(&bytes),
                    Ok(Some(FromHost::Ended { status })) => break Some(status),
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break None,
                }
            };
            let (lock, ended) = &*ending;
            *lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Ended(status));
            ended.notify_all();
        })?;
    Ok(Hosted {
        pid,
        process: Arc::new(process),
        link,
        ended,
    })
}

impl Hosted {
    /// The launcher's pid, as the daemon's endpoints see it connect.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// The pidfd that pins the launcher's process.
    #[must_use]
    pub fn process(&self) -> Arc<OwnedFd> {
        Arc::clone(&self.process)
    }

    /// What stops it.
    #[must_use]
    pub fn stopper(&self) -> Stop {
        Stop(Arc::clone(&self.link))
    }

    /// Let the launcher go on, handing it the connections the daemon made for it, then `link` as
    /// its launch hold would.
    ///
    /// # Errors
    /// The host is gone.
    pub fn release(&self, handed: Handed, link: Option<BorrowedFd<'_>>) -> io::Result<()> {
        let Handed {
            native,
            collab,
            identity,
            blobs,
        } = handed;
        ToHost::Release {
            native,
            collab,
            identity,
            blobs,
            link: link.map(|fd| fd.try_clone_to_owned()).transpose()?,
        }
        .send(&self.link)
    }

    /// How it ended, once it did.
    #[must_use]
    pub fn try_wait(&self) -> Option<Ended> {
        *self
            .ended
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait for it to end: its wait status, when the host said one.
    #[must_use]
    pub fn wait(&self) -> Option<i32> {
        let (lock, ended) = &*self.ended;
        let mut status = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(Ended(status)) = *status {
                return status;
            }
            status = ended
                .wait(status)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// End it.
    pub fn stop(&self) {
        self.stopper().stop();
    }
}

/// Whether the process `pidfd` pins still runs: its pid is still its own while it does.
#[must_use]
pub fn runs(pidfd: &OwnedFd) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let mut fds = [PollFd::new(pidfd, PollFlags::IN)];
    let now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    matches!(poll(&mut fds, Some(&now)), Ok(0))
}

/// A pidfd of `pid`, this host's child.
fn pidfd(pid: u32) -> io::Result<OwnedFd> {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| invalid("no such process"))?;
    Ok(rustix::process::pidfd_open(
        pid,
        rustix::process::PidfdFlags::empty(),
    )?)
}

/// Make reads of `fd` answer at once when nothing waits.
fn nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: `fd` is open for the call; these read and set its status flags only.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    // SAFETY: as above.
    if flags == -1
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn raw_status(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status.into_raw()
}

fn invalid(what: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

/// The host's connection, as `tilo-launcher host` is handed it on its standard input: moved off
/// descriptor 0, which then reads nothing, so nothing the host starts inherits it.
///
/// # Errors
/// Descriptor 0 could not be moved.
pub fn link_on_stdin() -> io::Result<Descriptor> {
    // SAFETY: descriptor 0 is the connection systemd handed this process; it is duplicated
    // before descriptor 0 is replaced, and nothing else holds it.
    let stdin = unsafe { BorrowedFd::borrow_raw(0) };
    let link = rustix::io::fcntl_dupfd_cloexec(stdin, 3)?;
    let null = std::fs::File::open("/dev/null")?;
    // SAFETY: both are descriptors this process owns; replacing 0 closes the inherited copy.
    if unsafe { libc::dup2(null.as_raw_fd(), 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(link)
}

#[cfg(test)]
#[path = "app_host_tests.rs"]
mod tests;
