//! The guest's agent socket: where a program the host launched to serve
//! agents finds Tilo's MCP server, at [`tilo_paths::login::GUEST_AGENT_SOCKET`]
//! as an environment's programs do on a device.
//!
//! Nothing in the guest answers there. The agent listens, and hands each
//! connection to the launch whose processes made it, which carries its bytes
//! to the host on the launch's own connection
//! ([`linux_vm_protocol::HostMessage::LaunchServingAgents`]); the host decides
//! what it is. A connection is the launch's when the kernel says it came from
//! a process of the tenant's that is the launched program or descends from it
//! ([`Processes::lineage`]). Anything else, another account's process or one
//! of no launch that serves agents (the Browser's session, a program the
//! tenant started some other way), is closed before a byte of it is read, and
//! the host never hears of it.
//!
//! This is a rule among the guest's ordinary processes and no wall against the machine's own administrator (the tenant
//! has sudo): what a connection may do is decided on the host, where the
//! person who decides is.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use crate::processes::Processes;

/// The agent socket, and the launches it serves.
pub struct AgentSocket {
    path: PathBuf,
    /// The tenant: only their processes are served.
    uid: u32,
    processes: Processes,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Whether the socket is bound and accepted on.
    listening: bool,
    /// Each launch that serves agents, by its program's process, and where its
    /// connections go.
    launches: BTreeMap<u32, Sender<Connected>>,
}

/// A connection a launch's process made, and the executable that process
/// runs.
pub struct Connected {
    pub stream: UnixStream,
    pub program: String,
}

/// The socket held for a launch that is starting.
pub struct Opening<'a> {
    socket: &'a Arc<AgentSocket>,
    state: std::sync::MutexGuard<'a, State>,
}

impl Opening<'_> {
    /// Serve the launch whose program is `pid` for as long as the [`Serving`]
    /// is kept: the connections it and its descendants make arrive, in the
    /// order they made them, on the receiver, which ends with it.
    #[must_use]
    pub fn serve(mut self, pid: u32) -> (Serving, Receiver<Connected>) {
        let (handing, connections) = mpsc::channel();
        self.state.launches.insert(pid, handing);
        let socket = Arc::clone(self.socket);
        drop(self);
        (Serving { socket, pid }, connections)
    }
}

/// One launch served at the agent socket, for as long as this is kept.
pub struct Serving {
    socket: Arc<AgentSocket>,
    pid: u32,
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.socket.state.lock_recover().launches.remove(&self.pid);
    }
}

impl AgentSocket {
    /// The socket at `path`, serving the processes of `uid`, bound when the
    /// first launch asks to be served.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, uid: u32) -> Self {
        Self {
            path: path.into(),
            uid,
            processes: Processes::of_this_machine(),
            state: Mutex::new(State::default()),
        }
    }

    /// Where it is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open the socket for a launch about to start, bound when it is not
    /// yet: until the launch is served ([`Opening::serve`]) no connection is
    /// handed to anyone, so one its program makes at once waits for it
    /// rather than being turned away.
    ///
    /// # Errors
    ///
    /// The socket could not be bound.
    pub fn opening(self: &Arc<Self>) -> io::Result<Opening<'_>> {
        let mut state = self.state.lock_recover();
        if !state.listening {
            let listener = bind(&self.path)?;
            let socket = Arc::clone(self);
            std::thread::Builder::new()
                .name("agent-socket".into())
                .spawn(move || socket.accept(&listener))?;
            state.listening = true;
        }
        Ok(Opening {
            socket: self,
            state,
        })
    }

    fn accept(&self, listener: &UnixListener) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            // A connection of no launch's is closed here, unread.
            if let Some((launch, program)) = self.launch_of(&stream) {
                let _ = launch.send(Connected { stream, program });
            }
        }
    }

    /// The launch the process at the other end of `stream` is of (the
    /// nearest of its lineage that serves agents, when it is the tenant's),
    /// and the executable it runs.
    fn launch_of(&self, stream: &UnixStream) -> Option<(Sender<Connected>, String)> {
        let (pid, uid) = peer(stream)?;
        if uid != self.uid {
            return None;
        }
        let lineage = self.processes.lineage(pid);
        let launch = {
            let state = self.state.lock_recover();
            lineage
                .iter()
                .find_map(|process| state.launches.get(process).cloned())?
        };
        let program = std::fs::read_link(format!("/proc/{pid}/exe"))
            .map(|exe| exe.display().to_string())
            .unwrap_or_default();
        Some((launch, program))
    }
}

/// Bind the socket at `path`, its directory made, anyone's to connect to:
/// the agent decides who is served, by the connecting process.
fn bind(path: &Path) -> io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    // A socket left by an agent before a restore of the machine is nobody's.
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // The guest's own state root can sit deep below its home, past the path a
    // stock bind takes; this reaches any length through the directory.
    let listener = tilo_files::native::socket::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

/// The process and account at the other end of `stream`, as the kernel
/// recorded them when it connected.
fn peer(stream: &UnixStream) -> Option<(u32, u32)> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    // SAFETY: `credentials` and `length` are live locals the kernel writes at
    // most `length` bytes into, for a descriptor `stream` owns for the call.
    let read = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    let pid = u32::try_from(credentials.pid).ok().filter(|pid| *pid > 0)?;
    (read == 0).then_some((pid, credentials.uid))
}
