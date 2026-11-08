//! The door of each confined environment open on this login.
//!
//! The session's own door serves only programs in the session's namespaces, so
//! nothing inside a `ush` guest reaches it. When `ush` opens an environment it
//! asks the session, on that door, for a door of the environment's own
//! ([`runtime_portal::environment_door`]): the request passes the door's kernel
//! peer checks like any other connection, so it comes from a program of this
//! login outside every sandbox. The session binds a fresh socket for the
//! environment, beside its own door, and answers its path; `ush` binds that
//! socket into the environment alone, at
//! [`tilo_paths::login::GUEST_AGENT_SOCKET`].
//!
//! A program connecting to an environment's door is served as an agent of
//! that environment, working in the environment's workspace rather than the
//! one in front of the person. Being reachable only inside the environment is
//! what authenticates it, and the kernel says so too: only a process of this
//! account descending from the process holding the door's lease (the
//! environment's own `ush`) is served there, so neither a program of the login
//! outside the environment nor one in another environment is. The person's
//! answer still decides each one, remembered per environment.
//!
//! The connection the request came on is the door's lease: when it closes (the
//! environment ended, or its `ush` did) the socket is removed and the agents it
//! admitted are stopped.

use std::collections::HashMap;
use std::io::{self, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use runtime_portal::environment_door::{Reply, Request};
use tilo_desktop_surface::DesktopHandle;
use tilo_sheets::word;

use super::{ASK_EVERY, MAX_CONNECTIONS, PeerRefusal, Serving, WAIT};

/// Environments with a door open at once; more are refused one.
const MAX_ENVIRONMENTS: usize = 64;

/// The longest an environment's part of an agent id is.
const MAX_PLACE: usize = 40;

/// Where a connection came in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    /// The session's own door: a program of this login outside every sandbox.
    Host,
    /// The door of a confined environment.
    Environment(Arc<Environment>),
}

impl Origin {
    /// The environment a connection is remembered by: its record key, or
    /// `None` for the session's own door.
    pub(super) fn environment(&self) -> Option<&str> {
        match self {
            Self::Host => None,
            Self::Environment(environment) => Some(&environment.id),
        }
    }

    /// Where an agent works from, as its id names it: `host`, or the
    /// environment's record key (its id is `<workspace>/<record>`) in the
    /// canonical form agent ids are written in. Two environments of one key
    /// in two workspaces count their agents' instances together, so every id
    /// stays an agent's own.
    pub(super) fn place(&self) -> String {
        match self {
            Self::Host => "host".to_owned(),
            Self::Environment(environment) => {
                let record = environment
                    .id
                    .rsplit_once('/')
                    .map_or(environment.id.as_str(), |(_, record)| record);
                let place: String = record
                    .chars()
                    .flat_map(char::to_lowercase)
                    .take(MAX_PLACE)
                    .collect();
                if place.is_empty() {
                    "environment".to_owned()
                } else {
                    place
                }
            }
        }
    }

    /// The door's own number, which the agents it admitted are stopped by.
    pub(super) fn door(&self) -> Option<u64> {
        match self {
            Self::Host => None,
            Self::Environment(environment) => Some(environment.door),
        }
    }
}

/// A confined environment with a door open.
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
    /// The process holding the door's lease, and when it started: only its
    /// descendants are served here.
    holder: u32,
    holder_start: Option<u64>,
    /// This door's number among the doors this session opened.
    door: u64,
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

/// The workspace `asked` names on the person's desk, when the person may let
/// an agent of theirs work there: they are one of its people, and not blocked
/// in it. `None` asks for the workspace in front of them. Its id and its name.
///
/// # Errors
/// The reason, as the client is told: no workspace is in front, or the one
/// asked for is not one the person may let an agent into (or not on this
/// desk, which is told the same, so a client learns nothing of the desk).
pub(super) fn workspace(
    handle: &DesktopHandle,
    person: &str,
    asked: Option<&str>,
) -> Result<(String, String), String> {
    let Some(asked) = asked else {
        return handle
            .read(|tree| {
                tree.active_workspace()
                    .map(|held| (held.id().to_string(), held.name.clone()))
            })
            .ok_or_else(|| word("agent-door-no-workspace"));
    };
    handle
        .read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.id().to_string() == asked)
                .filter(|held| held.role_of(person).is_some() && !held.is_blocked(person))
                .map(|held| (held.id().to_string(), held.name.clone()))
        })
        .ok_or_else(|| word("agent-door-workspace-refused"))
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
                return Err(word("agent-door-environment-workspace"));
            }
            // Still on the desk, and still one the person may let it into.
            workspace(handle, person, Some(&environment.workspace))
                .map_err(|_| word("agent-door-workspace-gone"))
        }
    }
}

/// An environment's door asked for, waiting for the loop to say where its
/// programs work.
pub(super) struct Opening {
    request: Request,
    decide: tokio::sync::oneshot::Sender<Result<(String, String), String>>,
}

/// The loop's side of the environments' doors: the doors asked for, and the
/// instance each agent id has reached.
pub(super) struct Doors {
    asked: Receiver<Opening>,
    opener: Opener,
    /// The last instance number given each agent at each place
    /// (`claude-code@host`): every connection the person admits is an agent
    /// of its own.
    instances: HashMap<String, u64>,
}

/// What a connection's thread opens an environment's door with.
#[derive(Clone)]
pub(super) struct Opener {
    asks: Sender<Opening>,
    /// Where the environments' doors are bound: beside the session's own.
    directory: Arc<Path>,
    open: Arc<AtomicUsize>,
    doors: Arc<AtomicU64>,
}

impl Doors {
    /// The doors of environments, bound under `directory`. A door left there
    /// by a session that ended is nobody's: this session's door is bound, so
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
                    tracing::warn!(%error, path = %path.display(), "agent door: an ended session's environment door was not removed");
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
                doors: Arc::new(AtomicU64::new(0)),
            },
            instances: HashMap::new(),
        }
    }

    pub(super) fn opener(&self) -> Opener {
        self.opener.clone()
    }

    /// Say where each environment asking for a door works: the workspace its
    /// `ush` named, when the person may let an agent into it, else the one in
    /// front of them.
    pub(super) fn follow(&self, handle: &DesktopHandle, person: &str) {
        while let Ok(opening) = self.asked.try_recv() {
            let answer = workspace(handle, person, opening.request.workspace.as_deref());
            let _ = opening.decide.send(answer);
        }
    }

    /// The id of the next connection the person admits as the agent `at`
    /// (`claude-code@host`), and its instance number: `claude-code@host#2`.
    pub(super) fn instance(&mut self, at: &str) -> (String, u64) {
        let next = self.instances.entry(at.to_owned()).or_insert(0);
        *next = next.saturating_add(1);
        let instance = *next;
        (format!("{at}#{instance}"), instance)
    }
}

/// Answer what `first`, the first line of a connection from `pid` that came in by `entrance`,
/// asks of the environments' doors: the runtime confining an environment asks for the
/// environment's own door, only on the session's door, whose peer checks it passed; a program
/// inside an environment opens none, and is carried to no other device. The connection, when it
/// asked for none of that.
pub(super) fn answer_door_requests(
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
                "agent door: a program inside an environment asked for a door"
            );
            super::refuse(stream, &PeerRefusal::OutsideEnvironment);
            return None;
        }
        open(stream, request, pid, serving.clone());
        return None;
    }
    if *entrance != Origin::Host && tilo_runtime::remote_door::forward_request(first).is_some() {
        // A program in an environment works in that environment's workspace, on this device.
        tilo_runtime::remote_door::refuse_forward(stream, word("agent-door-environment-workspace"));
        return None;
    }
    Some(stream)
}

/// The environment `request` named asks, on `stream` from `holder`, for a
/// door of its own: open it, and keep it open until `stream` closes. Runs on
/// a thread of its own for the environment's life.
pub(super) fn open(mut stream: UnixStream, request: Request, holder: u32, serving: Serving) {
    let opener = serving.environments.clone();
    if opener.open.fetch_add(1, Ordering::Relaxed) >= MAX_ENVIRONMENTS {
        opener.open.fetch_sub(1, Ordering::Relaxed);
        answer(
            &mut stream,
            &Reply::Refused {
                reason: "the session serves as many environments' doors as it takes".to_owned(),
            },
        );
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("tilo-agent-door-environment".into())
        .spawn(move || {
            let opener = serving.environments.clone();
            hold(stream, &request, holder, &serving);
            opener.open.fetch_sub(1, Ordering::Relaxed);
        });
    if let Err(error) = spawned {
        opener.open.fetch_sub(1, Ordering::Relaxed);
        tracing::warn!(%error, "agent door: an environment's door could not be opened");
    }
}

/// Open `request`'s door, serve it, and remove it once `stream` closes.
fn hold(mut stream: UnixStream, request: &Request, holder: u32, serving: &Serving) {
    let opener = &serving.environments;
    let (workspace, workspace_name) = match where_it_works(&stream, request, opener, serving) {
        Ok(found) => found,
        Err(reason) => {
            tracing::info!(environment = %request.environment, %reason, "agent door: an environment's door is refused");
            answer(&mut stream, &Reply::Refused { reason });
            return;
        }
    };
    let door = opener
        .doors
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
            tracing::warn!(%error, path = %path.display(), "agent door: an environment's door could not be bound");
            answer(
                &mut stream,
                &Reply::Refused {
                    reason: "the session could not open a door for this environment".to_owned(),
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
        door,
    });
    tracing::info!(
        environment = %environment.id,
        %workspace,
        path = %path.display(),
        "agent door: an environment's door is open"
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
        tracing::warn!(%error, path = %path.display(), "agent door: an ended environment's door was not removed");
    }
    // The environment ended: so does every agent it let in.
    serving.seats.stop_door(door);
    tracing::info!(environment = %environment.id, "agent door: an environment's door is closed");
}

/// Where the environment `request` names works, as the loop decides on its
/// pass; the reason when it may not have a door, or `stream`, its lease,
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
        if asked.elapsed() > WAIT || !tilo_runtime::session_door::client_connected(stream) {
            return Err("the environment left before its door opened".to_owned());
        }
        std::thread::sleep(ASK_EVERY);
    }
}

/// Write `reply` on the lease.
fn answer(stream: &mut UnixStream, reply: &Reply) {
    if let Err(error) = stream.write_all(&reply.line()) {
        // Its closed lease closes any door it was given.
        tracing::info!(%error, "agent door: an environment left before its answer");
    }
}

/// Serve `environment`'s door `listener` until `lease` closes: each process of
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
            tracing::warn!(%error, "agent door: an environment's door stopped waiting");
            return;
        }
        let [lease_ready, door_ready] = fds;
        if lease_ready.revents != 0 {
            // The lease says nothing more: anything but its end is ignored.
            match (&*lease).read(&mut ended) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    tracing::info!(%error, environment = %environment.id, "agent door: an environment's lease failed, so its door closes");
                    return;
                }
            }
        }
        if door_ready.revents != 0 {
            accept(listener, environment, serving);
        }
    }
}

/// Accept what is waiting at `environment`'s door.
fn accept(listener: &UnixListener, environment: &Arc<Environment>, serving: &Serving) {
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                tracing::warn!(%error, "agent door: accepting at an environment's door failed");
                return;
            }
        };
        let admitted = super::identify(&stream).and_then(|(pid, peer)| {
            if serving.connections.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                return Err(PeerRefusal::Busy);
            }
            environment_decision(&peer, pid, environment)?;
            Ok(pid)
        });
        match admitted {
            Ok(pid) => serving.serve(
                stream,
                super::From::Environment(pid, Arc::clone(environment)),
            ),
            Err(refusal) => {
                tracing::info!(?refusal, environment = %environment.id, "agent door: a connection to an environment's door is refused");
                super::refuse(stream, &refusal);
            }
        }
    }
}

/// Decide one connection at `environment`'s door from its peer's facts: this
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
    // ignored-failure: a stat field that is no number names no parent
    stat_fields(pid)?.get(1)?.parse().ok()
}

/// When `pid` started, in clock ticks since boot: what tells it from a later
/// process given the same number.
fn start_time(pid: u32) -> Option<u64> {
    // ignored-failure: a stat field that is no number names no process start
    stat_fields(pid)?.get(19)?.parse().ok()
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
            door: 1,
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

    #[test]
    fn an_environment_is_named_in_an_agent_id_by_its_record() {
        let inside = |id: &str| {
            Origin::Environment(Arc::new(Environment {
                id: id.into(),
                ..environment(std::process::id())
            }))
        };
        assert_eq!(
            inside("8b0e8f5e-6c1f-4f4e-9d4a-1f2e3d4c5b6a/Dev").place(),
            "dev"
        );
        assert_eq!(inside("personal/default").place(), "default");
        assert_eq!(inside("dev").place(), "dev");
        assert_eq!(
            inside("personal/default").environment(),
            Some("personal/default"),
            "remembered by the whole id"
        );
    }

    #[test]
    fn each_admitted_connection_is_an_agent_of_its_own() {
        let mut doors = Doors::new(PathBuf::from("/nowhere"));
        let host = Origin::Host;
        let inside = Origin::Environment(Arc::new(environment(std::process::id())));
        assert_eq!(
            doors.instance(&format!("claude-code@{}", host.place())),
            ("claude-code@host#1".to_owned(), 1)
        );
        assert_eq!(
            doors.instance(&format!("claude-code@{}", host.place())),
            ("claude-code@host#2".to_owned(), 2)
        );
        assert_eq!(
            doors.instance(&format!("claude-code@{}", inside.place())),
            ("claude-code@dev.box#1".to_owned(), 1),
            "each place counts its own"
        );
        assert_eq!(
            doors.instance(&format!("codex@{}", host.place())),
            ("codex@host#1".to_owned(), 1)
        );
        let id = doors.instance(&format!("claude-code@{}", inside.place())).0;
        tilo_peer_state::workspace_membership::valid_agent_id(&id)
            .expect("an id a workspace names an agent by");
        assert_eq!(inside.environment(), Some("Dev.Box"));
        assert_eq!(host.environment(), None);
    }
}
