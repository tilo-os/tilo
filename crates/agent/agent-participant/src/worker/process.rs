//! The supervisor's handle on one app worker process.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use super::proto::{FRAMING, Mode, PROTOCOL_VERSION, Reply, Request};
use super::{ARG, ENV};
use tilo_lock::MutexRecover as _;

/// How long a worker may take to start, load what it needs and confine
/// itself.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one request may run before the worker is presumed hung and
/// killed.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The only variables a worker inherits: locale and where fonts are found.
/// Never the supervisor's own (model API keys, state paths).
const PASSED_ENV: &[&str] = &[
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "HOME",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "RUST_BACKTRACE",
    "RUST_LOG",
];

/// One running worker. Dropping it ends the process.
pub(crate) struct Process {
    child: Child,
    socket: UnixStream,
}

impl Process {
    /// Start a worker in `mode` and wait until it has confined itself.
    ///
    /// # Errors
    ///
    /// Why it did not start, or that it could not confine itself; nothing of
    /// an app runs then.
    pub(crate) fn start(mode: Mode) -> Result<Self, String> {
        let (ours, theirs) = UnixStream::pair().map_err(|error| error.to_string())?;
        let mut command = Command::new(executable());
        command
            .args([ARG, "--nocapture", "--test-threads=1", "--quiet"])
            .env_clear()
            .env(ENV, mode.as_str())
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for key in PASSED_ENV {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let child =
            spawn(command).map_err(|error| format!("the app worker did not start: {error}"))?;
        let mut process = Self {
            child,
            socket: ours,
        };
        process
            .socket
            .set_read_timeout(Some(START_TIMEOUT))
            .map_err(|error| error.to_string())?;
        match process.receive() {
            Ok(Reply::Ready { version }) if version == PROTOCOL_VERSION => {}
            Ok(Reply::Unconfined(reason)) => {
                return Err(format!(
                    "the app worker could not confine itself, so no app runs: {reason}"
                ));
            }
            Ok(_) => return Err("the app worker speaks another protocol".into()),
            Err(_) => return Err(not_serving()),
        }
        process
            .socket
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|error| error.to_string())?;
        Ok(process)
    }

    /// Send `request` and wait for its reply.
    ///
    /// # Errors
    ///
    /// The worker died, hung or answered with garbage; it is no longer
    /// usable and the caller drops it.
    pub(crate) fn call(&mut self, request: &Request) -> Result<Reply, String> {
        FRAMING
            .write_value(&mut self.socket, request)
            .map_err(|error| format!("the app worker stopped: {error}"))?;
        self.receive()
    }

    fn receive(&mut self) -> Result<Reply, String> {
        match FRAMING.read_value(&mut self.socket) {
            Ok(Some(reply)) => Ok(reply),
            Ok(None) => Err("the app worker stopped".into()),
            Err(error) => Err(format!("the app worker stopped: {error}")),
        }
    }

    /// The worker's process id.
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The running image: on Linux even if the file on disk was replaced since.
/// An empty path where it cannot be named, which then fails to start.
fn executable() -> std::path::PathBuf {
    if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe().unwrap_or_default()
    }
}

fn not_serving() -> String {
    "this executable does not serve app workers: call \
     `tilo_agent_participant::worker::dispatch_early()` at the top of `main`, \
     or add `tilo_agent_participant::app_worker!();` to a test crate"
        .to_owned()
}

/// Start workers from one thread that lives as long as the process.
///
/// A worker asks the kernel to end it when its parent goes away, and on
/// Linux "parent" is the *thread* that forked it: spawning from whichever
/// thread needed a worker would end the worker with that thread.
fn spawn(command: Command) -> std::io::Result<Child> {
    type Job = (Command, std::sync::mpsc::Sender<std::io::Result<Child>>);
    static SPAWNER: std::sync::OnceLock<Mutex<std::sync::mpsc::Sender<Job>>> =
        std::sync::OnceLock::new();
    let gone = || std::io::Error::other("the app worker spawner thread is gone");
    let sender = SPAWNER.get_or_init(|| {
        let (jobs, queue) = std::sync::mpsc::channel::<Job>();
        let started = std::thread::Builder::new()
            .name("app-worker-spawner".into())
            .spawn(move || {
                for (mut command, reply) in queue {
                    let _ = reply.send(command.spawn());
                }
            });
        if let Err(error) = started {
            tracing::error!(%error, "app workers: the spawner thread did not start");
        }
        Mutex::new(jobs)
    });
    let (reply, answer) = std::sync::mpsc::channel();
    sender
        .lock_recover()
        .send((command, reply))
        .map_err(|_| gone())?;
    answer.recv().map_err(|_| gone())?
}
