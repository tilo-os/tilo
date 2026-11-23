//! The socket of each confined environment open on this login.
//!
//! The session's own socket serves only programs in the session's namespaces, so
//! nothing inside a `ush` guest reaches it. When `ush` opens an environment it
//! asks the session, on that socket, for a socket of the environment's own
//! ([`runtime_portal::environment_socket`]): the request passes the socket's kernel
//! peer checks like any other connection, so it comes from a program of this
//! login outside every sandbox. The session binds a fresh socket for the
//! environment, beside its own socket, and answers its path; `ush` binds that
//! socket into the environment alone, at
//! [`tilo_paths::login::GUEST_AGENT_SOCKET`].
//!
//! A program connecting to an environment's socket is served as an agent of
//! that environment, working in the environment's workspace rather than the
//! one in front of the person. Being reachable only inside the environment is
//! what authenticates it, and the kernel says so too: only a process of this
//! account descending from the process holding the socket's lease (the
//! environment's own `ush`) is served there, so neither a program of the login
//! outside the environment nor one in another environment is. The person's
//! answer still decides each one, remembered per environment.
//!
//! The connection the request came on is the socket's lease: when it closes (the
//! environment ended, or its `ush` did) the socket is removed and the agents it
//! admitted are stopped.

use std::io::{self, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use runtime_portal::environment_socket::{Reply, Request};
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_sheets::word;

use super::{ASK_EVERY, PeerRefusal, Serving, WAIT};

/// Environments with a socket open at once; more are refused one.
const MAX_ENVIRONMENTS: usize = 64;

/// Where a connection came in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    /// The session's own socket: a program of this login outside every sandbox.
    Host,
    /// The socket of a confined environment.
    Environment(Arc<Environment>),
}

impl Origin {
    /// The environment a connection is remembered by: its record key, or
    /// `None` for the session's own socket.
    pub(super) fn environment(&self) -> Option<&str> {
        match self {
            Self::Host => None,
            Self::Environment(environment) => Some(&environment.id),
        }
    }

    /// The environment's server a connection came in by, as its agent session is recorded:
    /// the server's number, which the sessions it admitted are stopped by, and the
    /// environment's id and name, which the shell lists beside a session's name.
    pub(super) fn entered(&self) -> Option<crate::agent_sessions::Entered> {
        match self {
            Self::Host => None,
            Self::Environment(environment) => Some(crate::agent_sessions::Entered {
                socket: environment.socket,
                id: environment.id.clone(),
                name: environment.name.clone(),
            }),
        }
    }
}

/// A confined environment with a socket open.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Environment {
    /// Its id on this device, `<workspace>/<record>`: what the person's
    /// answers about it are remembered by.
    pub(super) id: String,
    /// What the person calls it.
    pub(super) name: String,
    /// The workspace its programs work in, by id, and as the person reads it.
    pub(super) workspace: String,
    pub(super) workspace_name: String,
    /// The process holding the socket's lease, and when it started: only its
    /// descendants are served here.
    holder: u32,
    holder_start: Option<u64>,
    /// This socket's number among the sockets this session opened.
    socket: u64,
}

impl Environment {
    /// Whether `pid` runs inside this environment: a descendant of the
    /// process holding its lease, while that process is the one that asked.
    fn holds(&self, pid: u32) -> bool {
        self.holder_start.is_some()
            && start_time(self.holder) == self.holder_start
            && descends_from(pid, self.holder)
    }
}

/// The workspace `asked` names on the person's shell, when the person may let
/// an agent of theirs work there: they are one of its people, and not blocked
/// in it. `None` asks for the workspace in front of them. Its id and its name.
///
/// # Errors
/// The reason, as the client is told: no workspace is in front, or the one
/// asked for is not one the person may let an agent into (or not on this
/// shell, which is told the same, so a client learns nothing of the shell).
pub(super) fn workspace(
    handle: &DesktopHandle,
    person: &str,
    asked: Option<&str>,
) -> Result<(String, String), String> {
    let Some(asked) = asked else {
        return handle
            .read(|tree| {
                tree.active_workspace().map(|held| {
                    (
                        held.id().to_string(),
                        tilo_session_core::desktop::names::workspace(&held.name),
                    )
                })
            })
            .ok_or_else(|| word("mcp-server-no-workspace"));
    };
    handle
        .read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.id().to_string() == asked)
                .filter(|held| held.role_of(person).is_some() && !held.is_blocked(person))
                .map(|held| {
                    (
                        held.id().to_string(),
                        tilo_session_core::desktop::names::workspace(&held.name),
                    )
                })
        })
        .ok_or_else(|| word("mcp-server-workspace-refused"))
}

/// The workspace a connection from `origin` that asked for `asked` works in:
/// an environment's own, and only it; else the one the client asked for, or
/// the one in front of the person ([`workspace`]).
///
/// # Errors
/// The reason, as the client is told.
pub(super) fn workspace_for(
    handle: &DesktopHandle,
    person: &str,
    origin: &Origin,
    asked: Option<&str>,
) -> Result<(String, String), String> {
    match origin {
        Origin::Host => workspace(handle, person, asked),
        Origin::Environment(environment) => {
            if asked.is_some_and(|asked| asked != environment.workspace) {
                return Err(word("mcp-server-environment-workspace"));
            }
            // Still on the shell, and still one the person may let it into.
            workspace(handle, person, Some(&environment.workspace))
                .map_err(|_| word("mcp-server-workspace-gone"))
        }
    }
}

/// An environment's socket asked for, waiting for the loop to say where its
/// programs work.
pub(super) struct Opening {
    request: Request,
    decide: tokio::sync::oneshot::Sender<Result<(String, String), String>>,
}

/// The loop's side of the environments' sockets: the sockets asked for, and the
/// instance each agent id has reached.
pub(super) struct Sockets {
    asked: Receiver<Opening>,
    opener: Opener,
}

/// What a connection's thread opens an environment's socket with.
#[derive(Clone)]
pub(super) struct Opener {
    asks: Sender<Opening>,
    /// Where the environments' sockets are bound: beside the session's own.
    directory: Arc<Path>,
    open: Arc<AtomicUsize>,
    sockets: Arc<AtomicU64>,
}

impl Sockets {
    /// The sockets of environments, bound under `directory`. A socket left there
    /// by a session that ended is nobody's: this session's socket is bound, so
    /// none other of this login serves there.
    pub(super) fn new(directory: PathBuf) -> Self {
        if let Ok(entries) = std::fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "sock")
                    && let Err(error) = std::fs::remove_file(&path)
                {
                    tracing::warn!(%error, path = %path.display(), "MCP server: an ended session's environment socket was not removed");
                }
            }
        }
        let (asks, asked) = std::sync::mpsc::channel();
        Self {
            asked,
            opener: Opener {
                asks,
                directory: directory.into(),
                open: Arc::new(AtomicUsize::new(0)),
                sockets: Arc::new(AtomicU64::new(0)),
            },
        }
    }

    pub(super) fn opener(&self) -> Opener {
        self.opener.clone()
    }

    /// Say where each environment asking for a socket works: the workspace its
    /// `ush` named, when the person may let an agent into it, else the one in
    /// front of them.
    pub(super) fn follow(&self, handle: &DesktopHandle, person: &str) {
        while let Ok(opening) = self.asked.try_recv() {
            let answer = workspace(handle, person, opening.request.workspace.as_deref());
            let _ = opening.decide.send(answer);
        }
    }
}

/// Answer what `first`, the first line of a connection from `pid` that came in by `entrance`,
/// asks of the environments' sockets: the runtime confining an environment asks for the
/// environment's own socket, only on the session's socket, whose peer checks it passed; a program
/// inside an environment opens none, and is carried to no other device. The connection, when it
/// asked for none of that.
pub(super) fn answer_socket_requests(
    stream: UnixStream,
    first: &[u8],
    pid: u32,
    entrance: &Origin,
    serving: &Serving,
) -> Option<UnixStream> {
    if let Some(request) = Request::of(first) {
        if *entrance != Origin::Host {
            tracing::info!(
                pid,
                "MCP server: a program inside an environment asked for a socket"
            );
            super::refuse(stream, &PeerRefusal::OutsideEnvironment);
            return None;
        }
        open(stream, request, pid, serving.clone());
        return None;
    }
    if let Some(request) = runtime_portal::workspace_terminal::Request::of(first) {
        // Only the device's shell runtime, outside every environment, carries a hosted
        // workspace's shell to a Terminal window.
        if *entrance != Origin::Host {
            tracing::info!(
                pid,
                "MCP server: a program inside an environment asked for a workspace's terminal"
            );
            super::refuse(stream, &PeerRefusal::OutsideEnvironment);
            return None;
        }
        crate::workspace_terminals::open(stream, request, serving.hosting.clone());
        return None;
    }
    Some(stream)
}

/// The environment `request` named asks, on `stream` from `holder`, for a
/// socket of its own: open it, and keep it open until `stream` closes. Runs on
/// a thread of its own for the environment's life.
pub(super) fn open(mut stream: UnixStream, request: Request, holder: u32, serving: Serving) {
    let opener = serving.environments.clone();
    if opener.open.fetch_add(1, Ordering::Relaxed) >= MAX_ENVIRONMENTS {
        opener.open.fetch_sub(1, Ordering::Relaxed);
        answer(
            &mut stream,
            &Reply::Refused {
                reason: "the session serves as many environments' sockets as it takes".to_owned(),
            },
        );
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("tilo-mcp-server-environment".into())
        .spawn(move || {
            let opener = serving.environments.clone();
            hold(stream, &request, holder, &serving);
            opener.open.fetch_sub(1, Ordering::Relaxed);
        });
    if let Err(error) = spawned {
        opener.open.fetch_sub(1, Ordering::Relaxed);
        tracing::warn!(%error, "MCP server: an environment's socket could not be opened");
    }
}

/// Open `request`'s socket, serve it, and remove it once `stream` closes.
fn hold(mut stream: UnixStream, request: &Request, holder: u32, serving: &Serving) {
    let opener = &serving.environments;
    let (workspace, workspace_name) = match where_it_works(&stream, request, opener, serving) {
        Ok(found) => found,
        Err(reason) => {
            tracing::info!(environment = %request.environment, %reason, "MCP server: an environment's socket is refused");
            answer(&mut stream, &Reply::Refused { reason });
            return;
        }
    };
    let socket = opener
        .sockets
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    let path = opener.directory.join(format!(
        "{:016x}.sock",
        uuid::Uuid::new_v4().as_u64_pair().0
    ));
    let listener = match super::bind_private(&path).and_then(|listener| {
        listener.set_nonblocking(true)?;
        Ok(listener)
    }) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "MCP server: an environment's socket could not be bound");
            answer(
                &mut stream,
                &Reply::Refused {
                    reason: "the session could not open a socket for this environment".to_owned(),
                },
            );
            return;
        }
    };
    let environment = Arc::new(Environment {
        id: request.environment.clone(),
        name: request.name.clone(),
        workspace: workspace.clone(),
        workspace_name,
        holder,
        holder_start: start_time(holder),
        socket,
    });
    tracing::info!(
        environment = %environment.id,
        %workspace,
        path = %path.display(),
        "MCP server: an environment's socket is open"
    );
    answer(
        &mut stream,
        &Reply::Opened {
            path: path.clone(),
            workspace,
        },
    );
    serve(&stream, &listener, &environment, serving);
    drop(listener);
    if let Err(error) = std::fs::remove_file(&path) {
        tracing::warn!(%error, path = %path.display(), "MCP server: an ended environment's socket was not removed");
    }
    // The environment ended: so does every agent it let in.
    serving.sessions.stop_environment(socket);
    tracing::info!(environment = %environment.id, "MCP server: an environment's socket is closed");
}

/// Where the environment `request` names works, as the loop decides on its
/// pass; the reason when it may not have a socket, or `stream`, its lease,
/// closed while it waited.
fn where_it_works(
    stream: &UnixStream,
    request: &Request,
    opener: &Opener,
    serving: &Serving,
) -> Result<(String, String), String> {
    let (decide, mut decided) = tokio::sync::oneshot::channel();
    opener
        .asks
        .send(Opening {
            request: request.clone(),
            decide,
        })
        .map_err(|_| "this session is ending".to_owned())?;
    (serving.wake)();
    let asked = std::time::Instant::now();
    loop {
        match decided.try_recv() {
            Ok(answer) => return answer,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                return Err("this session is ending".to_owned());
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
        }
        if asked.elapsed() > WAIT || !tilo_mcp::session_api::client_connected(stream) {
            return Err("the environment left before its socket opened".to_owned());
        }
        std::thread::sleep(ASK_EVERY);
    }
}

/// Write `reply` on the lease.
fn answer(stream: &mut UnixStream, reply: &Reply) {
    if let Err(error) = stream.write_all(&reply.line()) {
        // Its closed lease closes any socket it was given.
        tracing::info!(%error, "MCP server: an environment left before its answer");
    }
}

/// Serve `environment`'s socket `listener` until `lease` closes: each process of
/// the environment that connects is served as its agent; anything else is
/// refused.
fn serve(
    lease: &UnixStream,
    listener: &UnixListener,
    environment: &Arc<Environment>,
    serving: &Serving,
) {
    let mut ended = [0_u8; 64];
    loop {
        let mut fds = [
            libc::pollfd {
                fd: lease.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two live pollfds, for descriptors this thread holds open for
        // the whole call.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            tracing::warn!(%error, "MCP server: an environment's socket stopped waiting");
            return;
        }
        let [lease_ready, socket_ready] = fds;
        if lease_ready.revents != 0 {
            // The lease says nothing more: anything but its end is ignored.
            match (&*lease).read(&mut ended) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    tracing::info!(%error, environment = %environment.id, "MCP server: an environment's lease failed, so its socket closes");
                    return;
                }
            }
        }
        if socket_ready.revents != 0 {
            accept(listener, environment, serving);
        }
    }
}

/// Accept what is waiting at `environment`'s socket.
fn accept(listener: &UnixListener, environment: &Arc<Environment>, serving: &Serving) {
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                tracing::warn!(%error, "MCP server: accepting at an environment's socket failed");
                return;
            }
        };
        let admitted = super::identify(&stream).and_then(|(pid, peer)| {
            environment_decision(&peer, pid, environment)?;
            // Only a program of this environment is told that the socket is full.
            let place = serving.room.take().ok_or(PeerRefusal::Busy)?;
            Ok((pid, place))
        });
        match admitted {
            Ok((pid, place)) => serving.serve(
                place,
                stream,
                super::From::Environment(pid, Arc::clone(environment)),
            ),
            Err(refusal) => {
                tracing::info!(?refusal, environment = %environment.id, "MCP server: a connection to an environment's socket is refused");
                super::refuse(stream, &refusal);
            }
        }
    }
}

/// Decide one connection at `environment`'s socket from its peer's facts: this
/// session's account, in a process of that environment.
pub(super) fn environment_decision(
    peer: &super::Peer,
    pid: u32,
    environment: &Environment,
) -> Result<(), PeerRefusal> {
    if peer.uid != rustix::process::geteuid().as_raw() {
        return Err(PeerRefusal::OtherAccount);
    }
    if !environment.holds(pid) {
        return Err(PeerRefusal::OutsideEnvironment);
    }
    Ok(())
}

/// Whether `pid` descends from `ancestor`, by the parents `/proc` names: a
/// process is not its own descendant.
fn descends_from(pid: u32, ancestor: u32) -> bool {
    /// Deeper than any guest's process tree, and short of a loop.
    const DEPTH: usize = 128;
    let mut current = pid;
    for _ in 0..DEPTH {
        let Some(parent) = parent_of(current) else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        if parent <= 1 || parent == current {
            return false;
        }
        current = parent;
    }
    false
}

/// The fields of `/proc/<pid>/stat` after its command: the state first.
fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command is parenthesised and may hold spaces and parentheses.
    let rest = stat.rsplit_once(") ")?.1;
    Some(rest.split_whitespace().map(str::to_owned).collect())
}

/// The process that started `pid`.
fn parent_of(pid: u32) -> Option<u32> {
    tilo_failure::ignored!(
        "a stat field that is no number names no parent",
        stat_fields(pid)?.get(1)?.parse().ok()
    )
}

/// When `pid` started, in clock ticks since boot: what tells it from a later
/// process given the same number.
fn start_time(pid: u32) -> Option<u64> {
    tilo_failure::ignored!(
        "a stat field that is no number names no process start",
        stat_fields(pid)?.get(19)?.parse().ok()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(holder: u32) -> Environment {
        Environment {
            id: "Dev.Box".into(),
            name: "Dev box".into(),
            workspace: "w1".into(),
            workspace_name: "Studio".into(),
            holder,
            holder_start: start_time(holder),
            socket: 1,
        }
    }

    #[test]
    fn only_a_descendant_of_the_environments_holder_is_inside_it() {
        use tilo_test_env::SpawnUnderEnvLock as _;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn_locked()
            .expect("a child process");
        let own = std::process::id();
        assert!(environment(own).holds(child.id()), "a child is inside");
        assert!(!environment(own).holds(own), "the holder itself is not");
        assert!(
            !environment(child.id()).holds(own),
            "nor is the process that started the holder"
        );
        let peer = super::super::Peer {
            uid: rustix::process::geteuid().as_raw(),
            shares_namespaces: true,
            hidden: false,
        };
        assert_eq!(
            environment_decision(&peer, child.id(), &environment(own)),
            Ok(())
        );
        assert_eq!(
            environment_decision(&peer, own, &environment(child.id())),
            Err(PeerRefusal::OutsideEnvironment)
        );
        let other = super::super::Peer {
            uid: rustix::process::geteuid().as_raw().saturating_add(1),
            shares_namespaces: false,
            hidden: false,
        };
        assert_eq!(
            environment_decision(&other, child.id(), &environment(own)),
            Err(PeerRefusal::OtherAccount)
        );
        let _ = child.kill();
        let _ = child.wait();
        let gone = Environment {
            holder_start: Some(0),
            ..environment(own)
        };
        assert!(
            !gone.holds(std::process::id()),
            "a holder whose start differs is another process given the same number"
        );
    }
}
