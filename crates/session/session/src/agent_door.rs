//! The session's agent door.
//!
//! `$XDG_RUNTIME_DIR/tilo-session/agent.sock` (0600, in the session's 0700
//! directory) is where a program of this login — an AI agent in a terminal —
//! reaches this desktop's actions as an *agent*, never as the person.
//! Framing is newline-delimited MCP JSON-RPC, as
//! [`tilo_runtime::session_door`] carries it into this session.
//!
//! Three things stand between a connection and the desktop.
//!
//! **The peer**: this session's own account, in this session's user and
//! mount namespaces. Anything else is told why and let go.
//!
//! **The person** ([`Questions`]): nothing the client sends past its
//! `initialize` is read until the person, on the sheet the question opens,
//! allows it to work in the workspace in front of them as an agent.
//! Answering admits it: the agent becomes a member of that workspace with
//! the Observer preset's grants — the membership the workspace's settings
//! list and revoke, so removing it there ends what the client may do.
//!
//! **The broker**: an admitted connection is served as
//! `Caller::local_agent(agent).in_room(room)`, so discovery and invocation
//! pass the same gate as every other caller.
//!
//! "Allow for this session" and "Always allow" remember that this client is
//! that agent, by the executable the kernel names and the name the client
//! gives, so its next connection is let in without asking wherever that
//! agent is already a member. "Always allow" is kept in this account's
//! private state, never in the workspace document.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_action_broker::collab_app_broker::AppBrokerState;
use tilo_action_policy::roles::AgentPreset;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    ContentPuts, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_clock::unix_now;
use tilo_desktop::workspace_settings as domain;
use tilo_desktop_surface::DesktopHandle;
use tilo_runtime::session_door::{Consent, Hello};
use tilo_service::{Answer, Asked, Question, Questions, Wake};
use tilo_sheets::{word, words};

use crate::launches::Launches;
use crate::plane::DeskAuthority;
use crate::questions::app_name;

/// The kind of question the door asks the person.
pub(crate) const KIND: &str = "session.agent-door";

/// Names the door when it is not the real seat's; honoured by developer
/// builds only.
pub(crate) const ENV: &str = "TILO_AGENT_DOOR";

/// Connections served at once; more are refused.
const MAX_CONNECTIONS: usize = 16;

/// How often a waiting connection looks for the person's answer and for its
/// client.
const ASK_EVERY: Duration = Duration::from_millis(50);

/// Where "Always allow" answers are kept, under the session's state
/// directory.
const BINDINGS_FILE: &str = "agent-door-bindings.json";

/// Where this session's door goes: the seat's own well-known path, or the
/// absolute path a developer named in `TILO_AGENT_DOOR`. `None` when there
/// is no runtime directory to put it in.
#[must_use]
pub fn path() -> Option<PathBuf> {
    path_in(std::env::var_os(ENV), std::env::var_os("XDG_RUNTIME_DIR"))
}

/// [`path`] for the arguments it reads, so what it decides can be checked.
fn path_in(named: Option<OsString>, runtime_dir: Option<OsString>) -> Option<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(named) = named.map(PathBuf::from)
        && named.is_absolute()
    {
        return Some(named);
    }
    tilo_paths::login::session_dir(runtime_dir)
        .map(|directory| directory.join(tilo_paths::login::AGENT_SOCKET_FILE))
}

/// What a client asks for, as the sheet shows it: where it would work and
/// what it would be called.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Admission {
    /// The workspace in front of the person when they were asked.
    pub(crate) workspace: String,
    /// The agent it is admitted as.
    pub(crate) agent: String,
}

/// What the kernel and the client's own `initialize` say about a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Facts {
    /// `clientInfo.name`, as the client gave it.
    client: String,
    version: Option<String>,
    pid: u32,
    /// `/proc/<pid>/exe`.
    exe: Option<PathBuf>,
    /// The app this session launched that the client descends from: the
    /// terminal it runs in.
    app: Option<String>,
}

impl Facts {
    /// The line the person reads first: who asks, and from where.
    fn subject(&self) -> String {
        let program = self.exe.as_deref().and_then(Path::file_name).map_or_else(
            || "?".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let who = words(
            "agent-door-client",
            &[("client", &self.client), ("program", &program)],
        );
        match &self.app {
            Some(app) => words("agent-door-client-in-app", &[("who", &who), ("app", app)]),
            None => who,
        }
    }

    /// The remembered identity of this client: the executable the kernel
    /// names and the name it gives. `None` without an executable, and such
    /// a client is asked about every time.
    fn key(&self) -> Option<BindingKey> {
        Some(BindingKey {
            exe: self.exe.clone()?,
            client: self.client.clone(),
        })
    }
}

/// One connection waiting for the person, handed to the loop.
struct Request {
    facts: Facts,
    /// Answered by the loop once the person decided and the agent is a
    /// member; dropped by the connection when its client leaves, which
    /// withdraws the question.
    decide: tokio::sync::oneshot::Sender<Consent>,
}

/// A connection the person is being asked about.
struct Waiting {
    facts: Facts,
    room: String,
    proposed: String,
    /// The question up for it; dropping it withdraws the question.
    asked: Option<Asked>,
    decide: tokio::sync::oneshot::Sender<Consent>,
}

/// What the person's answer remembers: this client, as this agent.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
struct BindingKey {
    exe: PathBuf,
    client: String,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredBindings {
    bindings: Vec<StoredBinding>,
}

#[derive(Serialize, Deserialize)]
struct StoredBinding {
    #[serde(flatten)]
    key: BindingKey,
    agent: String,
}

/// The door's listener, bound and ready, before the session serves it.
pub(crate) struct Mounted {
    door: Loop,
}

/// The session's side of the door: it asks the person, admits the agent they
/// answered, and serves what it listened for. It runs on the loop's own
/// pass, the only place that may touch the desk.
pub(crate) struct Loop {
    questions: Option<Questions>,
    listener: UnixListener,
    requests: Sender<Request>,
    asked: Receiver<Request>,
    /// The broker admitted agents are served from, once the session starts
    /// it.
    broker: Option<Arc<AppBrokerState>>,
    waiting: Vec<Waiting>,
    session: HashMap<BindingKey, String>,
    always: HashMap<BindingKey, String>,
    /// Where "Always allow" is kept; `None` keeps it for this session.
    bindings_path: Option<PathBuf>,
    launches: Arc<Launches>,
    wake: Wake,
    /// The desk the broker decides against, kept by the same loop: an
    /// admission made here reaches the broker now, not on its next pass.
    desk: Option<Arc<DeskAuthority>>,
    /// Connections being served now, against the door's cap.
    connections: Arc<AtomicUsize>,
    /// Polls the questions: the loop looks for an answer on each of its own
    /// passes, so nothing else has to wake it.
    waker: Waker,
}

impl Mounted {
    /// Serve what the door listened for from `broker`, whose decisions read
    /// `desk`: an admission on `desk` is what lets the client act.
    pub(crate) fn serve(
        mut self,
        broker: Option<Arc<AppBrokerState>>,
        desk: Option<Arc<DeskAuthority>>,
    ) -> Loop {
        self.door.broker = broker;
        self.door.desk = desk;
        self.door
    }
}

/// Bind the door at `path`, asking `questions` and naming a client's app
/// from `launches`. "Always allow" answers are
/// kept under `state`, when the session keeps anything.
///
/// # Errors
/// The path is not a socket, another session answers on it, or it cannot be
/// bound.
pub(crate) fn mount(
    path: &Path,
    questions: Option<Questions>,
    launches: Arc<Launches>,
    wake: Wake,
    state: Option<&Path>,
) -> io::Result<Mounted> {
    let listener = bind_private(path)?;
    listener.set_nonblocking(true)?;
    let bindings_path = state.map(|directory| directory.join(BINDINGS_FILE));
    let always = bindings_path
        .as_deref()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<StoredBindings>(&bytes).ok())
        .map(|stored| {
            stored
                .bindings
                .into_iter()
                .map(|binding| (binding.key, binding.agent))
                .collect()
        })
        .unwrap_or_default();
    let (requests, asked) = std::sync::mpsc::channel();
    let door = Loop {
        questions,
        listener,
        requests,
        asked,
        broker: None,
        waiting: Vec::new(),
        session: HashMap::new(),
        always,
        bindings_path,
        launches,
        wake,
        desk: None,
        connections: Arc::new(AtomicUsize::new(0)),
        // The loop polls its questions itself, each pass, so an answer
        // needs no wakeup.
        waker: Waker::noop().clone(),
    };
    Ok(Mounted { door })
}

impl Loop {
    /// Take the connections waiting, ask the person about each, and settle
    /// those they answered: the agent becomes a member of the workspace in
    /// front, and the client is served as that agent.
    pub(crate) fn follow(&mut self, handle: &DesktopHandle, person: &str) {
        self.accept();
        while let Ok(request) = self.asked.try_recv() {
            self.request(handle, request, person);
        }
        self.settle_waiting(handle, person);
    }

    /// Accept every waiting connection, each on its own thread.
    fn accept(&self) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    tracing::warn!(%error, "agent door: accepting a connection failed");
                    return;
                }
            };
            let pressed = identify(&stream).and_then(|(pid, peer)| {
                if self.connections.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    return Err(PeerRefusal::Busy);
                }
                peer_decision(&peer)?;
                Ok(pid)
            });
            let pid = match pressed {
                Ok(pid) => pid,
                Err(refusal) => {
                    tracing::info!(?refusal, "agent door: a connection is refused");
                    refuse(stream, &refusal);
                    continue;
                }
            };
            self.serve(stream, pid);
        }
    }

    /// Serve one connection of this account on its own thread: nothing it
    /// asks for runs before the person admits it.
    fn serve(&self, stream: UnixStream, pid: u32) {
        let Some(broker) = self.broker.clone() else {
            refuse(stream, &PeerRefusal::Unavailable);
            return;
        };
        let requests = self.requests.clone();
        let launches = Arc::clone(&self.launches);
        let wake = Arc::clone(&self.wake);
        let connections = Arc::clone(&self.connections);
        connections.fetch_add(1, Ordering::Relaxed);
        let served = Arc::clone(&connections);
        let spawned = std::thread::Builder::new()
            .name("tilo-agent-door".into())
            .spawn(move || {
                serve_connection(stream, pid, broker, &requests, &launches, &wake);
                served.fetch_sub(1, Ordering::Relaxed);
                // A question for a client that is gone is withdrawn on the
                // next pass.
                wake();
            });
        if let Err(error) = spawned {
            connections.fetch_sub(1, Ordering::Relaxed);
            tracing::warn!(%error, "agent door: a connection could not be served");
        }
    }

    /// Ask the person about `request`, unless what they answered before
    /// already says it, and keep it waiting for their answer.
    fn request(&mut self, handle: &DesktopHandle, request: Request, person: &str) {
        let Request { facts, decide } = request;
        let Some(questions) = self.questions.clone() else {
            let _ = decide.send(Consent::Refuse(
                PeerRefusal::Unavailable.message().to_owned(),
            ));
            return;
        };
        let now = unix_now();
        let Some((room, workspace)) = handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| (workspace.room().to_string(), workspace.name.clone()))
        }) else {
            let _ = decide.send(Consent::Refuse(word("agent-door-no-workspace")));
            return;
        };
        let key = facts.key();
        let remembered = key
            .as_ref()
            .and_then(|key| self.session.get(key).or_else(|| self.always.get(key)))
            .cloned();
        if let Some(agent) = &remembered
            && Self::member(handle, &room, agent, now)
        {
            // What the person said to remember: this client is this agent,
            // let in wherever that agent is already a member.
            let _ = decide.send(Consent::Admit {
                room,
                agent: agent.clone(),
                person: person.to_owned(),
            });
            return;
        }
        let proposed = remembered.unwrap_or_else(|| proposed_agent_id(&facts.client));
        let details = serde_json::to_value(Admission {
            workspace,
            agent: proposed.clone(),
        })
        .unwrap_or_default();
        let label = facts.subject();
        tracing::info!(
            client = %facts.client,
            pid = facts.pid,
            agent = %proposed,
            %room,
            "agent door: asking the person"
        );
        let asked = questions.ask(Question {
            handle: format!("agent-door:{}", uuid::Uuid::new_v4().simple()),
            kind: KIND.to_owned(),
            app_id: None,
            label,
            details,
        });
        self.waiting.push(Waiting {
            facts,
            room,
            proposed,
            asked: Some(asked),
            decide,
        });
    }

    /// Settle the questions the person answered — or whose client left —
    /// and let the admitted agents in.
    fn settle_waiting(&mut self, handle: &DesktopHandle, person: &str) {
        let mut still = Vec::with_capacity(self.waiting.len());
        for mut waiting in std::mem::take(&mut self.waiting) {
            if waiting.decide.is_closed() {
                // The client left; dropping `asked` withdraws the question.
                continue;
            }
            let Some(asked) = waiting.asked.as_mut() else {
                still.push(waiting);
                continue;
            };
            let answer = {
                let mut context = Context::from_waker(&self.waker);
                std::pin::Pin::new(asked).poll(&mut context)
            };
            match answer {
                Poll::Pending => still.push(waiting),
                Poll::Ready(answer) => {
                    waiting.asked = None;
                    let consent = self.settle(handle, &waiting, answer, person);
                    let _ = waiting.decide.send(consent);
                }
            }
        }
        self.waiting = still;
    }

    /// What the person's answer means for `waiting`: admitted as the agent
    /// it proposed, or refused with the reason.
    fn settle(
        &mut self,
        handle: &DesktopHandle,
        waiting: &Waiting,
        answer: Option<Answer>,
        person: &str,
    ) -> Consent {
        let Some(choice) = answer.and_then(|answer| answer.choice) else {
            return Consent::Refuse(word("agent-door-declined"));
        };
        let remembered = match choice {
            tilo_portal::consent::ALLOW_ONCE => Memory::Once,
            tilo_portal::consent::ALLOW_SESSION => Memory::Session,
            tilo_portal::consent::ALLOW_ALWAYS => Memory::Always,
            _ => return Consent::Refuse(word("agent-door-declined")),
        };
        if let Err(reason) = self.admit(handle, &waiting.room, &waiting.proposed, person) {
            return Consent::Refuse(reason);
        }
        if let Some(key) = waiting.facts.key() {
            match remembered {
                Memory::Once => {}
                Memory::Session => {
                    self.session.insert(key, waiting.proposed.clone());
                }
                Memory::Always => {
                    self.always.insert(key, waiting.proposed.clone());
                    self.store_always();
                }
            }
        }
        Consent::Admit {
            room: waiting.room.clone(),
            agent: waiting.proposed.clone(),
            person: person.to_owned(),
        }
    }

    /// Make `agent` a member of the workspace in `room` with the Observer
    /// preset's grants, unless it already is one.
    ///
    /// # Errors
    /// The workspace is gone, or the person does not administer it.
    fn admit(
        &self,
        handle: &DesktopHandle,
        room: &str,
        agent: &str,
        person: &str,
    ) -> Result<(), String> {
        let now = unix_now();
        let mut tree = handle.read(Clone::clone);
        let Some(workspace) = tree
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room().to_string() == room)
        else {
            return Err(word("agent-door-workspace-gone"));
        };
        if workspace.agent_role_at(agent, now).is_some() {
            return Ok(());
        }
        let actor = person.to_owned();
        if domain::admit_agent(workspace, &actor, agent, AgentPreset::Observer).is_empty() {
            return Err(words("agent-door-not-administrator", &[("agent", agent)]));
        }
        handle.replace_tree(tree.clone());
        if let Some(desk) = &self.desk {
            desk.publish(tree);
        }
        Ok(())
    }

    /// Whether `agent` is a live member of the workspace in `room`.
    fn member(handle: &DesktopHandle, room: &str, agent: &str, now: u64) -> bool {
        handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|workspace| workspace.room().to_string() == room)
                .is_some_and(|workspace| workspace.agent_role_at(agent, now).is_some())
        })
    }

    /// Keep "Always allow" in this account's private state. The loop does
    /// not wait for the disk.
    fn store_always(&self) {
        let Some(path) = self.bindings_path.clone() else {
            return;
        };
        let stored = StoredBindings {
            bindings: self
                .always
                .iter()
                .map(|(key, agent)| StoredBinding {
                    key: key.clone(),
                    agent: agent.clone(),
                })
                .collect(),
        };
        let bytes = match serde_json::to_vec_pretty(&stored) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "agent door: what the person remembered is kept for this session only"
                );
                return;
            }
        };
        std::thread::spawn(move || {
            if let Err(error) = write_private(&path, &bytes) {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "agent door: what the person remembered is kept for this session only"
                );
            }
        });
    }
}

/// How long the person's answer is remembered.
enum Memory {
    Once,
    Session,
    Always,
}

/// The agent id a client is proposed as: its own name, folded into the
/// canonical form agent ids are written in (`Claude Code` → `claude-code`).
fn proposed_agent_id(client: &str) -> String {
    let mut id = String::new();
    for character in client.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            id.push(character);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
        if id.len() >= 64 {
            break;
        }
    }
    let id = id.trim_matches('-');
    if id.is_empty() {
        "agent".to_owned()
    } else {
        id.to_owned()
    }
}

/// Bind `path` for this account alone: 0600 in a 0700 directory, staged and
/// renamed into place so nothing sees a socket it cannot connect to. A path
/// that is something else, or that a live session answers on, is refused.
fn bind_private(path: &Path) -> io::Result<UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt as _, PermissionsExt};
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the agent door's path names no file",
        ));
    }
    if !parent.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(existing) if !existing.file_type().is_socket() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Ok(_) if tilo_files::native::socket::connect(path).is_ok() => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("{} already has a live listener", path.display()),
            ));
        }
        Ok(_) | Err(_) => {}
    }
    // Short on purpose: a Unix socket path is capped near 108 bytes, and the
    // staged path has to fit wherever the final one does.
    let staging = parent.join(format!(".t{:08x}", uuid::Uuid::new_v4().as_fields().0));
    std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let staged = staging.join("s");
    let bound = tilo_files::native::socket::bind(&staged).and_then(|listener| {
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&staged, path)?;
        Ok(listener)
    });
    let _ = std::fs::remove_file(&staged);
    let _ = std::fs::remove_dir(&staging);
    bound
}

/// Write `bytes` to `path`, readable by this account only.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let staged = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&staged)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&staged, path)
}

/// What the kernel says about the process on the other end.
struct Peer {
    uid: libc::uid_t,
    /// Whether it shares this session's user and mount namespaces.
    shares_namespaces: bool,
}

/// Why a connection was refused before anyone was asked.
#[derive(Debug, PartialEq, Eq)]
enum PeerRefusal {
    Unidentified,
    OtherAccount,
    Sandboxed,
    /// This session serves no agents: it has no actions published yet.
    Unavailable,
    /// As many connections as it takes are being served.
    Busy,
}

impl PeerRefusal {
    fn message(&self) -> &'static str {
        match self {
            Self::Unidentified => "the agent door could not identify the connecting process",
            Self::OtherAccount => {
                "the agent door serves only programs of the account whose session it is"
            }
            Self::Sandboxed => "programs inside a sandboxed app can't connect here",
            Self::Unavailable => "this desktop isn't serving its actions yet; try again shortly",
            Self::Busy => "the agent door is serving as many connections as it takes",
        }
    }
}

/// Decide one connection from its peer's facts: this session's account, in
/// this session's namespaces.
fn peer_decision(peer: &Peer) -> Result<(), PeerRefusal> {
    if peer.uid != rustix::process::geteuid().as_raw() {
        return Err(PeerRefusal::OtherAccount);
    }
    if !peer.shares_namespaces {
        return Err(PeerRefusal::Sandboxed);
    }
    Ok(())
}

/// The kernel's account of an accepted connection's peer: its process, and
/// whether it is this login's own.
fn identify(stream: &UnixStream) -> Result<(u32, Peer), PeerRefusal> {
    use std::os::fd::AsRawFd as _;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>())
        .map_err(|_| PeerRefusal::Unidentified)?;
    // SAFETY: `stream` owns the descriptor for the whole call, and the
    // kernel writes at most the length this buffer declares.
    let read = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if read != 0 {
        return Err(PeerRefusal::Unidentified);
    }
    let pid = u32::try_from(credentials.pid)
        .ok()
        .filter(|pid| *pid != 0)
        .ok_or(PeerRefusal::Unidentified)?;
    Ok((
        pid,
        Peer {
            uid: credentials.uid,
            shares_namespaces: shares_security_namespaces(pid),
        },
    ))
}

/// Whether `pid` runs in this process's user and mount namespaces: what
/// tells a program of this login from one a sandbox put elsewhere.
fn shares_security_namespaces(pid: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let same = |space: &str| {
        let ours = std::fs::metadata(format!("/proc/self/ns/{space}"));
        let theirs = std::fs::metadata(format!("/proc/{pid}/ns/{space}"));
        match (ours, theirs) {
            (Ok(ours), Ok(theirs)) => ours.dev() == theirs.dev() && ours.ino() == theirs.ino(),
            // A process that cannot be read is not one this door knows.
            _ => false,
        }
    };
    same("user") && same("mnt")
}

/// What the person is shown about the connection from `pid`.
fn facts_of(pid: u32, launches: &Launches) -> Facts {
    Facts {
        client: String::new(),
        version: None,
        pid,
        exe: std::fs::read_link(format!("/proc/{pid}/exe")).ok(),
        app: launches.app_of_descendant(pid).map(|app| app_name(&app)),
    }
}

/// Tell a refused client why, as a JSON-RPC error, then let it go.
fn refuse(mut stream: UnixStream, refusal: &PeerRefusal) {
    use std::io::Write as _;
    let reply =
        tilo_runtime::session_door::error_reply(&serde_json::Value::Null, refusal.message());
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = stream.write_all(format!("{reply}\n").as_bytes());
}

/// Tell a control client it is served nothing here, in the control
/// protocol's own framing, then let it go.
fn refuse_control(mut stream: UnixStream) {
    use std::io::Write as _;
    let reply = serde_json::json!({"err": {
        "code": "not_allowed",
        "message": "This desktop's agent door does not serve programs inside apps.",
    }});
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = stream.write_all(format!("{reply}\n").as_bytes());
}

/// The broker as an admitted agent's door: every call passes the gate as
/// the agent and room the connection was admitted for.
struct Host {
    state: Arc<AppBrokerState>,
}

impl ActionHost for Host {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.state.action_query(caller, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.state.action_invoke(caller, None, None, &request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.state.action_observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.state.action_next_change_after_now(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionHostError>> {
        self.state.action_cancel(caller, id)
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionHostError>> {
        self.state.request_witness(caller, None, request)
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        self.state.content_puts()
    }
}

/// Serve one connection of this account, on its own thread: read what it
/// opens with, ask the person, and only once they admit it answer what it
/// asked. A refusal answers its `initialize` with a JSON-RPC error and
/// closes.
fn serve_connection(
    stream: UnixStream,
    pid: u32,
    broker: Arc<AppBrokerState>,
    requests: &Sender<Request>,
    launches: &Launches,
    wake: &Wake,
) {
    let mut stream = stream;
    let first = match tilo_runtime::session_door::read_first_line(&mut stream) {
        Ok(first) => first,
        Err(error) => {
            tracing::debug!(%error, "agent door: the client left before its first request");
            return;
        }
    };
    if tilo_runtime::session_door::is_control_request(&first) {
        tracing::info!(pid, "agent door: a program asked in the control protocol");
        refuse_control(stream);
        return;
    }
    let facts = facts_of(pid, launches);
    let ended = tilo_runtime::session_door::serve_after(
        stream,
        Some(first),
        || Ok(Host { state: broker }),
        None,
        |hello, connected| ask(requests, wake, facts, hello, connected),
    );
    if let Err(error) = ended {
        tracing::debug!(%error, "agent door: the client disconnected");
    }
}

/// Hand one client to the loop's question and wait for the person's answer,
/// giving up when the client leaves.
fn ask(
    requests: &Sender<Request>,
    wake: &Wake,
    mut facts: Facts,
    hello: &Hello,
    connected: &dyn Fn() -> bool,
) -> Consent {
    facts.client.clone_from(&hello.client_name);
    facts.version.clone_from(&hello.client_version);
    let (decide, mut decided) = tokio::sync::oneshot::channel();
    if requests.send(Request { facts, decide }).is_err() {
        return Consent::Refuse("this session is ending".into());
    }
    wake();
    loop {
        match decided.try_recv() {
            Ok(consent) => return consent,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                return Consent::Refuse(word("agent-door-withdrawn"));
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
        }
        if !connected() {
            // Dropping `decided` withdraws the question.
            return Consent::Refuse(word("agent-door-withdrawn"));
        }
        std::thread::sleep(ASK_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_door_is_the_seats_own_unless_a_developer_named_one() {
        let seat = path_in(None, Some("/run/user/1000".into()));
        assert_eq!(
            seat,
            Some(PathBuf::from("/run/user/1000/tilo-session/agent.sock")),
            "the seat's door"
        );
        let named = path_in(
            Some("/run/an-agent-door".into()),
            Some("/run/user/1000".into()),
        );
        assert_eq!(
            named,
            Some(PathBuf::from("/run/an-agent-door")),
            "a developer's own door"
        );
        assert_eq!(
            path_in(Some("relative".into()), Some("/run/user/1000".into())),
            seat,
            "only an absolute path is a developer's"
        );
        assert_eq!(path_in(None, None), None, "nowhere to listen");
    }

    #[test]
    fn a_client_is_proposed_as_a_canonical_agent_id() {
        assert_eq!(proposed_agent_id("Claude Code"), "claude-code");
        assert_eq!(proposed_agent_id("  Claude   Code 2.1 "), "claude-code-2-1");
        assert_eq!(proposed_agent_id("!!!"), "agent");
        assert_eq!(proposed_agent_id("MCP client"), "mcp-client");
    }

    #[test]
    fn the_peer_must_be_this_accounts_own_process_in_its_namespaces() {
        let own = rustix::process::geteuid().as_raw();
        let shares = |uid, namespaces| {
            peer_decision(&Peer {
                uid,
                shares_namespaces: namespaces,
            })
        };
        assert_eq!(shares(own, true), Ok(()));
        assert_eq!(
            shares(own.saturating_add(1), true),
            Err(PeerRefusal::OtherAccount),
            "another account"
        );
        assert_eq!(
            shares(own, false),
            Err(PeerRefusal::Sandboxed),
            "a program inside a sandbox"
        );
    }
}
