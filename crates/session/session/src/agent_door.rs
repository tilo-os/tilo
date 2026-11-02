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
//! `Caller::local_agent(agent).in_workspace(workspace)`, so discovery and
//! invocation pass the same gate as every other caller.
//!
//! **Acting as the person.** A client may ask to act as the person rather
//! than as an agent of theirs: a REPL (`tilolang repl`), whose lines the
//! person types. That is a request, never authority. The person decides it,
//! either on the same sheet, asked as acting as them, or ahead of time, by
//! naming the program when they start the session (`--admit-repl PATH`,
//! [`mount`]'s `admits`): a connection whose executable is that program and
//! that asks to act as the person is then let in without asking. The kernel's
//! account of the peer (this account, these namespaces) is still checked
//! first; it says the client is the person's, never that they consented.
//!
//! **What it is served.** The broker's catalogue, and beside it the session's
//! own doors (its desk, arrangement, windows and notices) under
//! [`SESSION_PROVIDER`]: a person-admitted client reaches all of them as the
//! person does; an agent only those that read. Its `tilo` programs run in a
//! task host of the connection's own ([`TaskHosts`]), each as its author.
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
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    ConfirmedInvocation, ContentPuts, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_clock::unix_now;
use tilo_desktop::workspace_settings as domain;
use tilo_desktop_surface::DesktopHandle;
use tilo_lock::MutexRecover as _;
use tilo_runtime::session_door::{Consent, Hello, SESSION_PROVIDER};
use tilo_service::{Answer, Asked, Question, Questions, Wake};
use tilo_sheets::{word, words};

use crate::app_names::app_name;
use crate::launches::Launches;
use crate::plane::DeskAuthority;

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
    /// Whether it asks to act as the person themselves, not as an agent.
    #[serde(default)]
    pub(crate) as_person: bool,
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
    /// It asks to act as the person.
    as_person: bool,
    /// Answered by the loop once the person decided and the agent is a
    /// member; dropped by the connection when its client leaves, which
    /// withdraws the question.
    decide: tokio::sync::oneshot::Sender<Consent>,
}

/// A connection the person is being asked about.
struct Waiting {
    facts: Facts,
    as_person: bool,
    workspace: String,
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
    /// The programs the person named, when they started the session, as the
    /// ones they admit to act as them: canonical executable paths.
    admits: Vec<PathBuf>,
    /// The clients the person let act as them for this session.
    person_session: std::collections::HashSet<BindingKey>,
    /// What the connections ask of the session's own doors, answered on the
    /// loop's pass.
    doors: Arc<SessionDoors>,
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

/// The door `options` name, bound and asking `questions`; `None`, saying
/// why, when it does not open.
pub(crate) fn mount_named(
    options: &crate::Options,
    questions: Option<Questions>,
    doors: Arc<SessionDoors>,
) -> Option<Mounted> {
    let path = options.agent_door.as_deref()?;
    match mount(
        path,
        questions,
        Arc::clone(&options.launches),
        options.wake.handle(),
        options.state.as_deref(),
        &options.admit_repl,
        doors,
    ) {
        Ok(mounted) => Some(mounted),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "the agent door does not open");
            None
        }
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
    admits: &[PathBuf],
    doors: Arc<SessionDoors>,
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
    // A program named by a path that does not resolve is admitted by none.
    let admits = admits
        .iter()
        .filter_map(|program| match std::fs::canonicalize(program) {
            Ok(program) => Some(program),
            Err(error) => {
                tracing::warn!(%error, program = %program.display(), "agent door: the program named to admit is not there");
                None
            }
        })
        .collect();
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
        admits,
        person_session: std::collections::HashSet::new(),
        doors,
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
        let session = Arc::clone(&self.doors);
        let launches = Arc::clone(&self.launches);
        let wake = Arc::clone(&self.wake);
        let connections = Arc::clone(&self.connections);
        connections.fetch_add(1, Ordering::Relaxed);
        let served = Arc::clone(&connections);
        let spawned = std::thread::Builder::new()
            .name("tilo-agent-door".into())
            .spawn(move || {
                serve_connection(stream, pid, broker, session, &requests, &launches, &wake);
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
        let Request {
            facts,
            as_person,
            decide,
        } = request;
        let Some(questions) = self.questions.clone() else {
            let _ = decide.send(Consent::Refuse(
                PeerRefusal::Unavailable.message().to_owned(),
            ));
            return;
        };
        let now = unix_now();
        let Some((workspace, name)) = handle.read(|tree| {
            tree.active_workspace()
                .map(|held| (held.room().to_string(), held.name.clone()))
        }) else {
            let _ = decide.send(Consent::Refuse(word("agent-door-no-workspace")));
            return;
        };
        let key = facts.key();
        if as_person {
            // The person named this program when they started the session, or let this client
            // act as them earlier in it: their decision, made already.
            let named = facts
                .exe
                .as_ref()
                .is_some_and(|exe| self.admits.iter().any(|admitted| admitted == exe));
            let earlier = key
                .as_ref()
                .is_some_and(|key| self.person_session.contains(key));
            if named || earlier {
                tracing::info!(client = %facts.client, pid = facts.pid, named, "agent door: the person admitted this client to act as them");
                let _ = decide.send(Consent::Person {
                    workspace,
                    person: person.to_owned(),
                });
                return;
            }
        }
        let remembered = key
            .as_ref()
            .and_then(|key| self.session.get(key).or_else(|| self.always.get(key)))
            .cloned();
        if !as_person
            && let Some(agent) = &remembered
            && Self::member(handle, &workspace, agent, now)
        {
            // What the person said to remember: this client is this agent,
            // let in wherever that agent is already a member.
            let _ = decide.send(Consent::Admit {
                workspace,
                agent: agent.clone(),
                person: person.to_owned(),
            });
            return;
        }
        let proposed = remembered.unwrap_or_else(|| proposed_agent_id(&facts.client));
        let details = serde_json::to_value(Admission {
            workspace: name,
            agent: proposed.clone(),
            as_person,
        })
        .unwrap_or_default();
        let label = facts.subject();
        tracing::info!(
            client = %facts.client,
            pid = facts.pid,
            agent = %proposed,
            %workspace,
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
            as_person,
            workspace,
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
        if waiting.as_person {
            // Acting as the person makes no one a member: it is them. What they let it do for
            // the session is remembered for the session only; a later session asks again
            // unless they name the program when they start it.
            if !matches!(remembered, Memory::Once)
                && let Some(key) = waiting.facts.key()
            {
                self.person_session.insert(key);
            }
            return Consent::Person {
                workspace: waiting.workspace.clone(),
                person: person.to_owned(),
            };
        }
        if let Err(reason) = self.admit(handle, &waiting.workspace, &waiting.proposed, person) {
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
            workspace: waiting.workspace.clone(),
            agent: waiting.proposed.clone(),
            person: person.to_owned(),
        }
    }

    /// Make `agent` a member of the workspace `workspace` with the Observer
    /// preset's grants, unless it already is one.
    ///
    /// # Errors
    /// The workspace is gone, or the person does not administer it.
    fn admit(
        &self,
        handle: &DesktopHandle,
        workspace: &str,
        agent: &str,
        person: &str,
    ) -> Result<(), String> {
        let now = unix_now();
        let mut tree = handle.read(Clone::clone);
        let Some(workspace) = tree
            .workspaces
            .iter_mut()
            .find(|held| held.room().to_string() == workspace)
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

    /// Whether `agent` is a live member of the workspace `workspace`.
    fn member(handle: &DesktopHandle, workspace: &str, agent: &str, now: u64) -> bool {
        handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.room().to_string() == workspace)
                .is_some_and(|held| held.agent_role_at(agent, now).is_some())
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
pub(crate) fn bind_private(path: &Path) -> io::Result<UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt as _, PermissionsExt};
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a private socket path names no file",
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
pub(crate) struct Peer {
    uid: libc::uid_t,
    /// Whether it shares this session's user and mount namespaces.
    shares_namespaces: bool,
}

/// Why a connection was refused before anyone was asked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PeerRefusal {
    Unidentified,
    OtherAccount,
    Sandboxed,
    /// This session serves no agents: it has no actions published yet.
    Unavailable,
    /// As many connections as it takes are being served.
    Busy,
}

impl PeerRefusal {
    pub(crate) fn message(&self) -> &'static str {
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
pub(crate) fn peer_decision(peer: &Peer) -> Result<(), PeerRefusal> {
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
pub(crate) fn identify(stream: &UnixStream) -> Result<(u32, Peer), PeerRefusal> {
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

/// What a connection asks of the session's own doors: answered on the loop's
/// pass, the only place that may touch them.
enum DoorAsk {
    /// Everything they publish now, and the revision it stands at.
    Resources(tilo_actions::remote::Completer<Offered>),
    /// Run one of their actions as `actor`; `reads_only` refuses one that
    /// changes anything.
    Invoke {
        actor: String,
        reads_only: bool,
        address: tilo_actions::ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
        done: tilo_actions::remote::Completer<
            Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError>,
        >,
    },
}

/// What the session's own doors offer now, and the revision it stands at:
/// equal revisions offer the same, so a cursor a page of it answered reads
/// on in the next page of the same listing.
struct Offered {
    resources: Vec<tilo_actions::ResourceSpec>,
    revision: String,
}

impl Offered {
    fn of(doors: &tilo_surface_host::Doors) -> Self {
        let resources = doors.resources();
        let revision = doors.revision().map_or_else(
            // A surface that cannot tell where it stands: what is offered
            // names the revision itself.
            || {
                use std::hash::{Hash as _, Hasher as _};
                let mut hasher = std::hash::DefaultHasher::new();
                for descriptor in tilo_actions::door::survey(&resources) {
                    descriptor.address.resource.hash(&mut hasher);
                    descriptor.address.action.hash(&mut hasher);
                }
                format!("session.{:016x}", hasher.finish())
            },
            |revision| format!("session.{revision:?}"),
        );
        Self {
            resources,
            revision,
        }
    }
}

/// The seam between the door's connections and the session's own doors.
pub(crate) struct SessionDoors {
    asks: std::sync::Mutex<Vec<DoorAsk>>,
    wake: Wake,
}

impl SessionDoors {
    /// The seam `wake` wakes the loop on, shared by every connection that
    /// asks the session's own doors.
    pub(crate) fn new(wake: Wake) -> Self {
        Self {
            asks: std::sync::Mutex::new(Vec::new()),
            wake,
        }
    }

    fn ask(&self, ask: DoorAsk) {
        self.asks.lock_recover().push(ask);
        (self.wake)();
    }

    /// Answer what the connections asked since the last pass, from `doors`;
    /// `person` is whose calls are their own, not someone's for them.
    pub(crate) fn answer(&self, doors: &tilo_surface_host::Doors, person: &str) {
        let asked = std::mem::take(&mut *self.asks.lock_recover());
        for ask in asked {
            match ask {
                DoorAsk::Resources(done) => done.complete(Offered::of(doors)),
                DoorAsk::Invoke {
                    actor,
                    reads_only,
                    address,
                    arguments,
                    done,
                } => {
                    let reads = tilo_actions::door::survey(&doors.resources())
                        .into_iter()
                        .find(|descriptor| descriptor.address == address)
                        .is_some_and(|descriptor| {
                            descriptor.kind == tilo_actions::OperationKind::View
                        });
                    let ran = if reads_only && !reads {
                        Err(tilo_actions::DoorError::Unavailable {
                            code: "not_permitted".into(),
                            reason: word("agent-door-session-reads-only"),
                        })
                    } else if actor == person {
                        doors.invoke(&address, &arguments)
                    } else {
                        doors.invoke_as(&actor, &address, &arguments)
                    };
                    done.complete(ran);
                }
            }
        }
    }
}

/// What the session's own actions answered, by invocation, for a later look.
type Answered =
    std::collections::VecDeque<(tilo_actions::InvocationId, tilo_actions::InvocationUpdate)>;

/// How many answered session actions a connection remembers.
const ANSWERED_KEPT: usize = 256;

/// The broker as an admitted client's door, with the session's own doors
/// beside it: every call passes the gate as the agent or person and workspace the
/// connection was admitted for.
#[derive(Clone)]
pub(crate) struct Host {
    /// The session's broker, once it starts one. Without it (a session in a
    /// window of another desktop) the session's own doors are all it offers.
    state: Option<Arc<AppBrokerState>>,
    session: Arc<SessionDoors>,
    answered: Arc<std::sync::Mutex<Answered>>,
}

impl Host {
    /// The door over `state`, with `session` beside it.
    pub(crate) fn new(state: Option<Arc<AppBrokerState>>, session: Arc<SessionDoors>) -> Self {
        Self {
            state,
            session,
            answered: Arc::default(),
        }
    }

    /// Who `caller` acts as at the session's doors, and whether it may only
    /// read there: an agent reads them; the person does all they offer, as
    /// their own input does.
    fn at_session(caller: &Caller) -> (String, bool) {
        use tilo_actions::remote::CallerPrincipal;
        match caller.principal() {
            CallerPrincipal::Person(person) => (person.clone(), false),
            CallerPrincipal::Agent(agent) => (format!("agent:{agent}"), true),
            other => (format!("{other:?}"), true),
        }
    }

    /// The session's own rows `caller` is offered, as a page of `query`: the
    /// page's cursor reads on to the next, as for any other provider.
    fn session_page(&self, caller: &Caller, query: &CatalogueQuery) -> Pending<CataloguePage> {
        let (_, reads_only) = Self::at_session(caller);
        let (done, offered) = tilo_actions::remote::pending();
        self.session.ask(DoorAsk::Resources(done));
        let query = query.clone();
        offered.map(
            move |Offered {
                      resources,
                      revision,
                  }| {
                let rows: Vec<_> = tilo_actions::door::survey(&resources)
                    .into_iter()
                    .filter(|descriptor| {
                        !reads_only || descriptor.kind == tilo_actions::OperationKind::View
                    })
                    .map(|descriptor| {
                        tilo_actions::remote::CatalogueRow::for_action(
                            SESSION_PROVIDER,
                            SESSION_PROVIDER,
                            descriptor,
                        )
                    })
                    .collect();
                CataloguePage::from_rows(revision, rows, &query)
            },
        )
    }
}

impl ActionHost for Host {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let workspace = caller.workspace().unwrap_or_default().to_owned();
        let session_provider = CatalogueProvider {
            provider: SESSION_PROVIDER.to_owned(),
            app_id: SESSION_PROVIDER.to_owned(),
            workspace,
            record_id: String::new(),
            browsable: true,
        };
        match query.provider.as_deref() {
            Some(SESSION_PROVIDER) => self.session_page(caller, &query).map(Ok),
            Some(_) => match &self.state {
                Some(state) => state.action_query(caller, &query),
                None => Pending::ready(Err(ActionHostError::Unavailable {
                    reason: "this desktop's actions are not running here".to_owned(),
                })),
            },
            None => {
                let Some(state) = &self.state else {
                    // A cursor this host never printed is nobody's page here.
                    if !query.cursor.is_empty() || query.revision.is_some() {
                        return Pending::ready(Err(ActionHostError::StaleRevision));
                    }
                    let session = self.session_page(caller, &query);
                    return session.map(move |page| {
                        Ok(CataloguePage {
                            providers: vec![session_provider],
                            ..page
                        })
                    });
                };
                // A page asked for by cursor is a page of the listing it was cut from, which
                // the broker alone knows; the session's rows stand in its first page only.
                if !query.cursor.is_empty() || query.revision.is_some() {
                    return state.action_query(caller, &query);
                }
                // The broker's page, with the session's own first page beside
                // it: a cursor of the broker's pages the broker alone.
                let session = self.session_page(caller, &query).map(|page| page.rows);
                let (done, merged) = tilo_actions::remote::pending();
                state.action_query(caller, &query).then(move |page| {
                    let Some(page) = page else {
                        return;
                    };
                    session.then(move |rows| {
                        done.complete(page.map(|mut page| {
                            page.rows.extend(rows.unwrap_or_default());
                            page.providers.push(session_provider);
                            page
                        }));
                    });
                });
                merged
            }
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if request.target.provider != SESSION_PROVIDER {
            return match &self.state {
                // A person at their own seat: the broker retains the seat's
                // certificate custody for the call, as it does for the
                // palette's, so what the person may do here is what the
                // person may do anywhere.
                Some(state) => state.action_invoke_local_person(caller, &request),
                None => Pending::ready(Err(ActionHostError::Unavailable {
                    reason: "this desktop's apps are not running here".to_owned(),
                })),
            };
        }
        let (actor, reads_only) = Self::at_session(caller);
        let arguments = match request.argument {
            tilo_actions::remote::InvokeArgument::Json(arguments) => arguments,
            tilo_actions::remote::InvokeArgument::Text(_)
            | tilo_actions::remote::InvokeArgument::None => serde_json::Map::new(),
        };
        let invocation = request.invocation;
        let (done, ran) = tilo_actions::remote::pending();
        self.session.ask(DoorAsk::Invoke {
            actor,
            reads_only,
            address: request.target.address,
            arguments,
            done,
        });
        let answered = Arc::clone(&self.answered);
        ran.map(move |ran| {
            use tilo_actions::remote::ReceiptStatus;
            let status = match ran {
                Ok(ui::host_operation::ResourceOperationStart::Completed(output)) => {
                    ReceiptStatus::Completed { output }
                }
                Ok(ui::host_operation::ResourceOperationStart::Accepted) => ReceiptStatus::Accepted,
                Err(error) => ReceiptStatus::Rejected {
                    error: crate::surfaces::refusal(&error),
                },
            };
            if let Some(update) = crate::surfaces::outcome(invocation, &status) {
                let mut answered = answered.lock_recover();
                if answered.len() >= ANSWERED_KEPT {
                    answered.pop_front();
                }
                answered.push_back((invocation, update));
            }
            Ok(InvocationReceipt { invocation, status })
        })
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        let ours = self
            .answered
            .lock_recover()
            .iter()
            .rev()
            .find(|(invocation, _)| invocation == id)
            .map(|(_, update)| InvocationState::new(*id, update.clone()));
        ours.or_else(|| {
            self.state
                .as_ref()
                .and_then(|state| state.action_observe(caller, id))
        })
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.state
            .as_ref()
            .and_then(|state| state.action_next_change_after_now(caller, id))
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionHostError>> {
        match &self.state {
            Some(state) => state.action_cancel(caller, id),
            None => Pending::ready(Err(ActionHostError::UnknownInvocation)),
        }
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionHostError>> {
        match &self.state {
            Some(state) => state.request_witness(caller, None, request),
            None => Pending::ready(Err(ActionHostError::ConfirmationRequired)),
        }
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        self.state
            .as_ref()
            .and_then(tilo_action_broker::collab_app_broker::AppBrokerState::content_puts)
    }
}

/// Where the session's door starts a connection's task host: a thread of the
/// session's own running the task's tilolang host (`tilolang_host::task`)
/// over the program connection. The host holds no authority: it decides
/// nothing and writes no record, and everything its programs read or change
/// comes back over the connection to the door, which does it as the
/// program's author, through the same gate as every other call.
#[derive(Default)]
struct TaskHosts {
    /// The person the connection was admitted for: `me` in its programs.
    person: std::sync::Mutex<Option<String>>,
}

impl tilo_runtime::mcp::TaskHosts for TaskHosts {
    fn start(&self, workspace: &str, task: &str) -> io::Result<std::os::fd::OwnedFd> {
        let (door_end, host_end) = UnixStream::pair()?;
        // The session runs no installed tilolang app's package yet: the task's programs name
        // the platform's own (the device and its arrangement, Settings).
        let encoded = runtime_portal::tilolang_host::encode_modules(&[]);
        let modules = tilo_render_protocol::shm::sealed_blob("tilolang-modules", &encoded)?;
        let request = runtime_portal::tilolang_host::TaskRequest {
            task: task.to_owned(),
            room: workspace.to_owned(),
            person: self.person.lock_recover().clone(),
            modules_len: encoded.len() as u64,
            records_under: std::collections::BTreeMap::new(),
        };
        std::thread::Builder::new()
            .name("tilo-task-host".into())
            .spawn(move || {
                let fds = runtime_portal::tilolang_host::TaskFds {
                    program: host_end.into(),
                    modules,
                };
                let opened = tilolang_host::task::Task::open(&request, fds);
                let task = match opened {
                    Ok(task) => task,
                    Err(error) => {
                        tracing::warn!(task = request.task, %error, "agent door: a task's host did not start");
                        return;
                    }
                };
                // Every line the door sends is answered; the host ends when the door closes
                // the connection, with the client that started it.
                while task.serve() {}
                tracing::debug!(task = request.task, "agent door: a task's host ended");
            })?;
        Ok(door_end.into())
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
    session: Arc<SessionDoors>,
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
    let tasks = Arc::new(TaskHosts::default());
    let admitted = Arc::clone(&tasks);
    let ended = tilo_runtime::session_door::serve_after(
        stream,
        Some(first),
        || Ok(Host::new(Some(broker), session)),
        Some(tasks as Arc<dyn tilo_runtime::mcp::TaskHosts>),
        |hello, connected| {
            let consent = ask(requests, wake, facts, hello, connected);
            if let Consent::Admit { person, .. } | Consent::Person { person, .. } = &consent {
                *admitted.person.lock_recover() = Some(person.clone());
            }
            consent
        },
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
    let request = Request {
        facts,
        as_person: hello.as_person,
        decide,
    };
    if requests.send(request).is_err() {
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
