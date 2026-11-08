//! The session's agent door. `$XDG_RUNTIME_DIR/tilo-session/agent.sock` (0600, in the session's
//! 0700 directory) is where a program of this login — an AI agent in a terminal — reaches this
//! desktop's actions as an *agent*, never as the person. Framing is newline-delimited MCP JSON-RPC
//! ([`tilo_runtime::session_door`]). Three gates stand between a connection and the desktop: its
//! peer must be this session's own account in this session's namespaces; the person must allow it,
//! on the sheet the question opens, to work in the workspace in front of them ([`Questions`]); and
//! the broker then serves it as `Caller::local_person(person).delegated_to(agent)` , so discovery
//! and invocation pass the same gate as every other caller. An agent is not a member; the person's
//! session is what ends it. Acting as the person is a request, never authority: the person decides
//! it on the sheet, or ahead of time by naming the program when they start the session
//! (`--admit-repl PATH`). "Always allow" is kept in this account's private state.
//!
//! A confined environment (a `ush` guest) has a door of its own, which its `ush` asks for here
//! ([`environments`]): a program there works in the environment's workspace, and the person's
//! answers about it are remembered per environment. A client of the session's own door may ask for
//! a workspace by id instead of the one in front ([`tilo_runtime::session_door::WORKSPACE`]); it is
//! admitted there only if the person is one of its people. Every connection the person admits is
//! an agent of its own (`claude-code@host#2`), so its work is attributed to it, and the person's
//! Stop ends it alone.

mod environments;

use environments::Origin;

use std::collections::{BTreeMap, HashMap};
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
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    ContentPuts, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_compositor::ReadableWatch;
use tilo_desktop_surface::DesktopHandle;
use tilo_lock::MutexRecover as _;
use tilo_runtime::session_door::{Consent, Hello};
use tilo_service::{Answer, Asked, Question, Questions, Wake};
use tilo_sheets::{word, words};

use crate::Shared;
use crate::app_names::app_name;
use crate::launches::Launches;
use crate::plane::DeskAuthority;
use crate::remote_door::Remote;

/// The kind of question the door asks the person.
pub(crate) const KIND: &str = "session.agent-door";

/// Names the door when it is not the real seat's; honoured by developer builds only.
pub(crate) const ENV: &str = "TILO_AGENT_DOOR";

/// Connections served at once; more are refused.
const MAX_CONNECTIONS: usize = 16;

/// How often a waiting connection looks for the person's answer and for its client.
const ASK_EVERY: Duration = Duration::from_millis(50);

/// Where "Always allow" answers are kept, under the session's state directory.
const BINDINGS_FILE: &str = "agent-door-bindings.json";

/// Where this session's door goes: the seat's own well-known path, or the absolute path a developer
/// named in `TILO_AGENT_DOOR` . `None` when there is no runtime directory to put it in.
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

/// What a client asks for, as the sheet shows it: where it would work and what it would be called.
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
    /// `clientInfo.name` , as the client gave it.
    client: String,
    version: Option<String>,
    pid: u32,
    /// `/proc/<pid>/exe` .
    exe: Option<PathBuf>,
    /// The app this session launched that the client descends from: the terminal it runs in.
    app: Option<String>,
    /// The other device of the person's it connected from, over iroh ([`crate::remote_door`]);
    /// `None` for a program of this login.
    origin: Option<tilo_runtime::remote_door::Origin>,
    /// The door of this login it came in by: the session's own, or an environment's.
    entrance: Origin,
}

impl Facts {
    /// The line the person reads first: who asks, and from where.
    fn subject(&self) -> String {
        if let Some(origin) = &self.origin {
            return words(
                "agent-door-client-from-device",
                &[("client", &self.client), ("device", &origin.name())],
            );
        }
        let program = self.exe.as_deref().and_then(Path::file_name).map_or_else(
            || "?".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let who = words(
            "agent-door-client",
            &[("client", &self.client), ("program", &program)],
        );
        let who = match &self.app {
            Some(app) => words("agent-door-client-in-app", &[("who", &who), ("app", app)]),
            None => who,
        };
        match &self.entrance {
            Origin::Host => who,
            Origin::Environment(environment) => words(
                "agent-door-client-in-environment",
                &[("who", &who), ("environment", &environment.name)],
            ),
        }
    }

    /// The remembered identity of this client: the environment it works in (none on the session's
    /// own door), the executable the kernel names and the name it gives. `None` without an
    /// executable, and such a client is asked about every time.
    fn key(&self) -> Option<BindingKey> {
        if let Some(origin) = &self.origin {
            // A client on another device is remembered by that device and the name it gives.
            return Some(BindingKey {
                environment: None,
                exe: PathBuf::new(),
                client: self.client.clone(),
                device: Some(hex::encode(origin.key())),
            });
        }
        // A program of this login is remembered in the environment it works in, if any.
        Some(BindingKey {
            environment: self.entrance.environment().map(str::to_owned),
            exe: self.exe.clone()?,
            client: self.client.clone(),
            device: None,
        })
    }

    /// The agent id the client is proposed as: its own name, and the device it came from when it is
    /// another's.
    fn proposed(&self) -> String {
        let own = proposed_agent_id(&self.client);
        match &self.origin {
            Some(origin) => tilo_runtime::remote_door::agent_id(&own, &origin.device),
            None => own,
        }
    }
}

/// One connection waiting for the person, handed to the loop.
struct Request {
    facts: Facts,
    /// It asks to act as the person.
    as_person: bool,
    /// The workspace it asks to work in, by id; `None` for the one in front.
    workspace: Option<String>,
    /// The seat it is served at, once the person answers.
    seat: u64,
    /// Answered by the loop once the person decided and the agent is a member; dropped by the
    /// connection when its client leaves, which withdraws the question.
    decide: tokio::sync::oneshot::Sender<Consent>,
}

/// A connection the person is being asked about.
struct Waiting {
    facts: Facts,
    as_person: bool,
    workspace: String,
    /// The workspace as the person reads it, for its seat on their desk.
    workspace_name: String,
    /// The seat it is served at, once the person answers.
    seat: u64,
    /// The agent it is remembered as, and the agent of its own it is admitted as
    /// (`claude-code@host#2`), shown as `shown` on the desk.
    agent: String,
    proposed: String,
    shown: String,
    /// The question up for it; dropping it withdraws the question.
    asked: Option<Asked>,
    decide: tokio::sync::oneshot::Sender<Consent>,
}

/// What the person's answer remembers: this client, as this agent.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
struct BindingKey {
    /// The confined environment a program of this login works in, by its id
    /// (`<workspace>/<record>`); absent for the session's own door.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
    exe: PathBuf,
    client: String,
    /// The other device of the person's a client connected from, by its key in hex; absent for a
    /// program of this login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device: Option<String>,
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

/// What this computer kept of the person's "Always allow" answers and this build could not read:
/// each agent whose answer is asked again, and whether something was lost without the file saying
/// whose answer it was. The person is told once, because an agent they let work here would
/// otherwise simply be asked about again.
#[derive(Default)]
pub(crate) struct Lost {
    agents: Vec<String>,
    unnamed: bool,
}

impl Lost {
    /// A file nothing could be read from: no answer of it is left, and none of them can be named.
    fn unreadable() -> Self {
        Self {
            agents: Vec::new(),
            unnamed: true,
        }
    }

    /// Each answer this build could not read: the agent it was for, or `None` for a loss the file
    /// does not name.
    pub(crate) fn answers(&self) -> impl Iterator<Item = Option<&str>> {
        self.agents
            .iter()
            .map(|agent| Some(agent.as_str()))
            .chain(self.unnamed.then_some(None))
    }
}

/// The answers kept at `path` , and what in it this build could not read: a row it cannot read
/// costs only that answer, so the person is asked about that agent again and keeps every other
/// answer their file holds.
fn read_bindings(path: &Path) -> (HashMap<BindingKey, String>, Lost) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        // No bindings file yet: nobody chose "Always allow" on this device.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (HashMap::new(), Lost::default());
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "stored agent bindings could not be read");
            return (HashMap::new(), Lost::unreadable());
        }
    };
    let stored: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(stored) => stored,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "stored agent bindings do not read back");
            return (HashMap::new(), Lost::unreadable());
        }
    };
    let Some(rows) = stored.get("bindings").and_then(serde_json::Value::as_array) else {
        tracing::warn!(
            path = %path.display(),
            "stored agent bindings do not read back: no list of answers"
        );
        return (HashMap::new(), Lost::unreadable());
    };
    let mut answers = HashMap::new();
    let mut lost = Lost::default();
    for row in rows {
        match serde_json::from_value::<StoredBinding>(row.clone()) {
            Ok(binding) => {
                answers.insert(binding.key, binding.agent);
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "a stored agent binding does not read back; that answer is asked again"
                );
                match row.get("agent").and_then(serde_json::Value::as_str) {
                    Some(agent) if !lost.agents.iter().any(|lost| lost.as_str() == agent) => {
                        lost.agents.push(agent.to_owned());
                    }
                    Some(_) => {}
                    None => lost.unnamed = true,
                }
            }
        }
    }
    (answers, lost)
}

/// The agents the person admitted at this door, as their desk seats them: one presence per agent,
/// from the moment the person lets its first client in until the last of its connections ends. A
/// shell shows each as its own presence on the bar.
pub(crate) struct Seats {
    inner: std::sync::Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Bumped on every change, so a reading made before it is not mistaken for the desk now.
    revision: u64,
    /// The number the door gives each connection it serves.
    next: u64,
    /// The connections the door is serving, by that number: the person's Stop on an agent takes
    /// every one of its own.
    connections: BTreeMap<u64, Connection>,
}

struct Connection {
    /// The agent the person admitted it as: empty until they answer.
    agent: String,
    /// What the client called itself, as the sheet showed it.
    client: String,
    /// The workspace it works in, by name.
    workspace: String,
    /// The environment's door it came in by, by number: none for the session's own. It ends with
    /// that environment.
    door: Option<u64>,
    /// The connection itself, shut when the person stops its seat.
    stream: Option<UnixStream>,
}

impl Seats {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(Inner::default()),
        })
    }

    /// A connection by the environment's door `door` (none for any other), before the person
    /// answers for it: nothing of it shows until it is admitted.
    fn joining(&self, door: Option<u64>) -> u64 {
        let mut inner = self.inner.lock_recover();
        inner.next = inner.next.wrapping_add(1);
        let seat = inner.next;
        inner.connections.insert(
            seat,
            Connection {
                agent: String::new(),
                client: String::new(),
                workspace: String::new(),
                door,
                stream: None,
            },
        );
        seat
    }

    /// The person admitted connection `seat` as `agent` , a client that calls itself `client` ,
    /// working in `workspace` : what the bar shows from now on.
    fn admitted(&self, seat: u64, agent: &str, client: &str, workspace: &str) {
        let mut inner = self.inner.lock_recover();
        let Some(connection) = inner.connections.get_mut(&seat) else {
            // It ended while the person was answering: nothing is served.
            return;
        };
        agent.clone_into(&mut connection.agent);
        client.clone_into(&mut connection.client);
        workspace.clone_into(&mut connection.workspace);
        inner.revision = inner.revision.wrapping_add(1);
    }

    /// The connection `seat` names, which ending its seat closes.
    fn attached(&self, seat: u64, stream: &UnixStream) {
        let copy = match stream.try_clone() {
            Ok(copy) => Some(copy),
            Err(error) => {
                // Without its own handle the seat cannot end this connection;
                // the client keeps it until it leaves by itself.
                tracing::warn!(%error, "an agent seat's connection could not be duplicated");
                None
            }
        };
        if let Some(connection) = self.inner.lock_recover().connections.get_mut(&seat) {
            connection.stream = copy;
        }
    }

    /// The connection `seat` names ended; its agent's seat goes with its last.
    fn left(&self, seat: u64) {
        let mut inner = self.inner.lock_recover();
        if inner.connections.remove(&seat).is_none() {
            return;
        }
        inner.revision = inner.revision.wrapping_add(1);
    }

    /// End every connection served as `agent` , as the person's Stop on its seat does. Whether
    /// there was one.
    pub(crate) fn stop(&self, agent: &str) -> bool {
        self.end(|connection| connection.agent == agent)
    }

    /// End every connection that came in by the environment's door `door`, as that environment
    /// ends.
    fn stop_door(&self, door: u64) -> bool {
        self.end(|connection| connection.door == Some(door))
    }

    /// End every connection `ending` picks. Whether there was one.
    fn end(&self, ending: impl Fn(&Connection) -> bool) -> bool {
        let ended: Vec<Connection> = {
            let mut inner = self.inner.lock_recover();
            let seats: Vec<u64> = inner
                .connections
                .iter()
                .filter(|(_, connection)| ending(connection))
                .map(|(seat, _)| *seat)
                .collect();
            if seats.is_empty() {
                return false;
            }
            let ended = seats
                .into_iter()
                .filter_map(|seat| inner.connections.remove(&seat))
                .collect();
            inner.revision = inner.revision.wrapping_add(1);
            ended
        };
        for connection in ended {
            if let Some(stream) = connection.stream {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        true
    }

    /// The agents seated here now, as the desk's shell lists them.
    pub(crate) fn readings(&self) -> Vec<tilo_surface_host::shell_part::AgentReading> {
        let inner = self.inner.lock_recover();
        // One presence per agent, in the order their keys come: a client with two connections shows
        // once.
        let mut agents: BTreeMap<String, tilo_surface_host::shell_part::AgentReading> =
            BTreeMap::new();
        for connection in inner.connections.values() {
            if connection.agent.is_empty() {
                continue;
            }
            agents.entry(connection.agent.clone()).or_insert_with(|| {
                tilo_surface_host::shell_part::AgentReading {
                    key: connection.agent.clone(),
                    name: connection.client.clone(),
                    state: "running".to_owned(),
                    doing: connection.workspace.clone(),
                    mark: String::new(),
                    unseen: 0,
                    seat: true,
                }
            });
        }
        drop(inner);
        agents.into_values().collect()
    }

    /// What the seats were made from, without making them: changes only when an agent was admitted
    /// or left.
    pub(crate) fn revision(&self) -> u64 {
        self.inner.lock_recover().revision
    }
}

/// The door's listener, bound and ready, before the session serves it.
pub(crate) struct Mounted {
    door: Loop,
}

/// The session's side of the door: it asks the person, admits the agent they answered, and serves
/// what it listened for. It runs on the loop's own pass, the only place that may touch the desk.
pub(crate) struct Loop {
    questions: Option<Questions>,
    listener: UnixListener,
    /// What ends the loop's wait while a client connects.
    watch: Option<ReadableWatch>,
    requests: Sender<Request>,
    asked: Receiver<Request>,
    /// The broker admitted agents are served from, once the session starts it.
    broker: Option<Arc<AppBrokerState>>,
    waiting: Vec<Waiting>,
    session: HashMap<BindingKey, String>,
    always: HashMap<BindingKey, String>,
    /// The answers about agents this computer kept and this build could not read, until the
    /// session's pass tells the person once.
    lost: Vec<Option<String>>,
    /// Where "Always allow" is kept; `None` keeps it for this session.
    bindings_path: Option<PathBuf>,
    launches: Arc<Launches>,
    wake: Wake,
    /// The desk the broker decides against, kept by the same loop: an admission made here reaches
    /// the broker now, not on its next pass.
    desk: Option<Arc<DeskAuthority>>,
    /// Connections being served now, against the door's cap.
    connections: Arc<AtomicUsize>,
    /// Polls the questions: the loop looks for an answer on each of its own passes, so nothing else
    /// has to wake it.
    waker: Waker,
    /// The programs the person named, when they started the session, as the ones they admit to act
    /// as them: canonical executable paths.
    admits: Vec<PathBuf>,
    /// The clients the person let act as them for this session.
    person_session: std::collections::HashSet<BindingKey>,
    /// What the connections ask of the session's own doors, answered on the loop's pass.
    doors: Arc<SessionDoors>,
    /// The control protocol's door, which a program inside an app this session started is served
    /// at, as that app.
    control: Arc<crate::test_control::Door>,
    /// The agents the person admitted, as their desk seats them.
    seats: Arc<Seats>,
    /// What the person's other devices connect with, and how this one reaches theirs
    /// ([`crate::remote_door`]).
    remote: Option<Arc<Remote>>,
    /// The doors of the confined environments open now, and the instance each agent reached.
    environments: environments::Doors,
}

/// What serving one connection takes, on a thread of its own.
#[derive(Clone)]
struct Serving {
    broker: Option<Arc<AppBrokerState>>,
    remote: Option<Arc<Remote>>,
    doors: Arc<SessionDoors>,
    requests: Sender<Request>,
    launches: Arc<Launches>,
    wake: Wake,
    control: Arc<crate::test_control::Door>,
    seats: Arc<Seats>,
    /// Connections being served now, against the door's cap.
    connections: Arc<AtomicUsize>,
    environments: environments::Opener,
}

impl Serving {
    /// Serve one connection of this account, from `from`, on its own thread: nothing it asks for
    /// runs before the person admits it.
    fn serve(&self, stream: UnixStream, from: From) {
        let serving = self.clone();
        let connections = Arc::clone(&self.connections);
        connections.fetch_add(1, Ordering::Relaxed);
        let spawned = std::thread::Builder::new()
            .name("tilo-agent-door".into())
            .spawn(move || {
                serve_connection(Opened { stream, from }, &serving);
                serving.connections.fetch_sub(1, Ordering::Relaxed);
                // A question for a client that is gone is withdrawn on the next pass.
                (serving.wake)();
            });
        if let Err(error) = spawned {
            connections.fetch_sub(1, Ordering::Relaxed);
            tracing::warn!(%error, "agent door: a connection could not be served");
        }
    }
}

impl Mounted {
    /// Serve what the door listened for from `broker` , whose decisions read `desk` : an admission
    /// on `desk` is what lets the client act.
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

/// The door `options` name, bound and asking `questions` ; `None` , saying why, when it does not
/// open.
pub(crate) fn mount_named(
    options: &crate::Options,
    questions: Option<Questions>,
    doors: Arc<SessionDoors>,
    control: Arc<crate::test_control::Door>,
    seats: Arc<Seats>,
) -> Option<Mounted> {
    let path = options.agent_door.as_deref()?;
    match mount(Doorway {
        path,
        questions,
        launches: Arc::clone(&options.launches),
        wake: options.wake.handle(),
        state: options.state.as_deref(),
        admits: &options.admit_repl,
        doors,
        control,
        seats,
    }) {
        Ok(mounted) => Some(mounted),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "the agent door does not open");
            None
        }
    }
}

/// What the door binds with: where it listens, whose questions its clients are admitted by, the
/// apps this session started, where "Always allow" is kept, the programs named to admit, the
/// session's own doors, the workspace governance it holds, and the control door a program inside an
/// app drives.
pub(crate) struct Doorway<'a> {
    path: &'a Path,
    questions: Option<Questions>,
    launches: Arc<Launches>,
    wake: Wake,
    state: Option<&'a Path>,
    admits: &'a [PathBuf],
    doors: Arc<SessionDoors>,
    control: Arc<crate::test_control::Door>,
    seats: Arc<Seats>,
}

/// Bind the door `doorway` names. "Always allow" answers are kept under its
/// `state`, when the session keeps anything.
///
/// # Errors
/// The path is not a socket, another session answers on it, or it cannot be
/// bound.
pub(crate) fn mount(doorway: Doorway<'_>) -> io::Result<Mounted> {
    let Doorway {
        path,
        questions,
        launches,
        wake,
        state,
        admits,
        doors,
        control,
        seats,
    } = doorway;
    let listener = bind_private(path)?;
    listener.set_nonblocking(true)?;
    let environments = environments::Doors::new(
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(tilo_paths::login::ENVIRONMENT_DOORS_DIR),
    );
    let bindings_path = state.map(|directory| directory.join(BINDINGS_FILE));
    let (always, lost) = bindings_path
        .as_deref()
        .map_or_else(|| (HashMap::new(), Lost::default()), read_bindings);
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
        watch: None,
        requests,
        asked,
        broker: None,
        waiting: Vec::new(),
        session: HashMap::new(),
        always,
        lost: lost
            .answers()
            .map(|agent| agent.map(str::to_owned))
            .collect(),
        bindings_path,
        launches,
        wake,
        desk: None,
        connections: Arc::new(AtomicUsize::new(0)),
        // The loop polls its questions itself, each pass, so an answer needs no wakeup.
        waker: Waker::noop().clone(),
        admits,
        person_session: std::collections::HashSet::new(),
        doors,
        control,
        seats,
        remote: None,
        environments,
    };
    Ok(Mounted { door })
}

impl Shared {
    /// Tell the person once about the answers this computer kept and this build could not read: an
    /// agent they let work here is asked about again, and they are told why rather than losing the
    /// answer in silence.
    pub(crate) fn tell_agent_bindings_lost(&self) {
        let mut door = self.agent_door.borrow_mut();
        let Some(door) = door.as_mut() else {
            return;
        };
        for agent in door.lost.drain(..) {
            self.notifications
                .borrow()
                .tell(crate::notices::agent_bindings_notice(agent.as_deref()));
        }
    }
}

impl Loop {
    /// Serve what the person's other devices connect with through `remote`,
    /// and carry this login's clients to theirs.
    pub(crate) fn hear_remote(&mut self, remote: Arc<Remote>) {
        remote.serve();
        self.remote = Some(remote);
    }

    /// End the loop's wait when a client connects, so it is asked about on the
    /// pass that follows.
    ///
    /// # Errors
    /// The loop took no more sources.
    pub(crate) fn wake_on(
        &mut self,
        compositor: &mut tilo_compositor::Compositor,
    ) -> io::Result<()> {
        self.watch = Some(crate::sockets::watch(compositor, &self.listener)?);
        Ok(())
    }

    /// Take the connections waiting, ask the person about each, and settle those they answered: the
    /// agent becomes a member of the workspace in front, and the client is served as that agent.
    pub(crate) fn follow(&mut self, handle: &DesktopHandle, person: &str) {
        self.accept();
        self.accept_remote();
        self.environments.follow(handle, person);
        while let Ok(request) = self.asked.try_recv() {
            self.request(handle, request, person);
        }
        self.settle_waiting(person);
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
            self.serve(stream, From::Login(pid));
        }
    }

    /// Accept every connection the person's other devices opened since the last pass, each on its
    /// own thread, as a connection of this login is.
    fn accept_remote(&self) {
        let Some(remote) = &self.remote else {
            return;
        };
        for arrival in remote.take() {
            if self.connections.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                refuse(arrival.stream, &PeerRefusal::Busy);
                continue;
            }
            self.serve(arrival.stream, From::Device(Box::new(arrival.origin)));
        }
    }

    /// Serve one connection of this account on its own thread: nothing it asks for runs before the
    /// person admits it.
    fn serve(&self, stream: UnixStream, from: From) {
        self.serving().serve(stream, from);
    }

    /// What serving a connection of this door takes.
    fn serving(&self) -> Serving {
        Serving {
            broker: self.broker.clone(),
            remote: self.remote.clone(),
            doors: Arc::clone(&self.doors),
            requests: self.requests.clone(),
            launches: Arc::clone(&self.launches),
            wake: Arc::clone(&self.wake),
            control: Arc::clone(&self.control),
            seats: Arc::clone(&self.seats),
            connections: Arc::clone(&self.connections),
            environments: self.environments.opener(),
        }
    }

    /// Ask the person about `request` , unless what they answered before already says it, and keep
    /// it waiting for their answer.
    fn request(&mut self, handle: &DesktopHandle, request: Request, person: &str) {
        let Request {
            facts,
            as_person,
            workspace: asked,
            seat,
            decide,
        } = request;
        let Some(questions) = self.questions.clone() else {
            let _ = decide.send(Consent::Refuse(
                PeerRefusal::Unavailable.message().to_owned(),
            ));
            return;
        };
        let (workspace, name) =
            match where_it_works(handle, person, &facts, as_person, asked.as_deref()) {
                Ok(found) => found,
                Err(reason) => {
                    let _ = decide.send(Consent::Refuse(reason));
                    return;
                }
            };
        let key = facts.key();
        if as_person {
            // The person named this program when they started the session, or let this client act
            // as them earlier in it: their decision, made already.
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
        // What the person said ahead of time admits it as a connection of its own: an answer they
        // remembered for this client, or this device's setting that lets agents from their other
        // devices in without being asked.
        let ahead = remembered.clone().filter(|_| !as_person).or_else(|| {
            (facts.origin.is_some() && Remote::accepts_own_devices()).then(|| facts.proposed())
        });
        if let Some(agent) = ahead {
            let (agent, shown) = self.agent_of(&agent, &facts);
            tracing::info!(client = %facts.client, pid = facts.pid, %agent, %workspace, "agent door: what the person said ahead of time admits this client");
            self.seats.admitted(seat, &agent, &shown, &name);
            let _ = decide.send(Consent::Admit {
                workspace,
                agent,
                person: person.to_owned(),
            });
            return;
        }
        let agent = remembered.unwrap_or_else(|| facts.proposed());
        let (proposed, shown) = if as_person {
            (agent.clone(), facts.client.clone())
        } else {
            self.agent_of(&agent, &facts)
        };
        let details = serde_json::to_value(Admission {
            workspace: name.clone(),
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
            workspace_name: name,
            seat,
            agent,
            proposed,
            shown,
            asked: Some(asked),
            decide,
        });
    }

    /// The agent of its own the client `facts` describes is admitted as, when the person admits it
    /// as `agent`, and how their desk shows it: its instance among the clients of that agent at its
    /// place (`claude-code@host#2`, `Claude Code #2`, `Claude Code in dev #2`). An agent from
    /// another device names its device already (`claude-code@laptop-3f2a`).
    fn agent_of(&mut self, agent: &str, facts: &Facts) -> (String, String) {
        let at = if facts.origin.is_some() {
            agent.to_owned()
        } else {
            format!("{agent}@{}", facts.entrance.place())
        };
        let (id, instance) = self.environments.instance(&at);
        let instance = instance.to_string();
        let shown = match &facts.entrance {
            Origin::Environment(environment) => words(
                "agent-door-seat-in-environment",
                &[
                    ("client", &facts.client),
                    ("environment", &environment.name),
                    ("instance", &instance),
                ],
            ),
            Origin::Host => words(
                "agent-door-seat",
                &[("client", &facts.client), ("instance", &instance)],
            ),
        };
        (id, shown)
    }

    /// Settle the questions the person answered — or whose client left — and let the admitted
    /// agents in.
    fn settle_waiting(&mut self, person: &str) {
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
                    let consent = self.settle(&waiting, answer, person);
                    let _ = waiting.decide.send(consent);
                }
            }
        }
        self.waiting = still;
    }

    /// What the person's answer means for `waiting` : admitted as the agent it proposed, or refused
    /// with the reason.
    fn settle(&mut self, waiting: &Waiting, answer: Option<Answer>, person: &str) -> Consent {
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
            // Acting as the person makes no one a member: it is them. What they let it do for the
            // session is remembered for the session only; a later session asks again unless they
            // name the program when they start it.
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
        self.seats.admitted(
            waiting.seat,
            &waiting.proposed,
            &waiting.shown,
            &waiting.workspace_name,
        );
        // What is remembered is the agent, at its place: each later connection the answer admits
        // is an instance of its own.
        if let Some(key) = waiting.facts.key() {
            match remembered {
                Memory::Once => {}
                Memory::Session => {
                    self.session.insert(key, waiting.agent.clone());
                }
                Memory::Always => {
                    self.always.insert(key, waiting.agent.clone());
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

    /// Keep "Always allow" in this account's private state. The loop does not wait for the disk.
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

/// Where the client `facts` describes works, by id and as the person reads it, when it may work
/// here at all: the workspace its device asked for, its environment's, the one it asked for by id,
/// or the one in front. The reason it may not, as it is told: only a program of this login, and
/// none inside an environment, acts as the person (`as_person`).
fn where_it_works(
    handle: &DesktopHandle,
    person: &str,
    facts: &Facts,
    as_person: bool,
    asked: Option<&str>,
) -> Result<(String, String), String> {
    if as_person && facts.origin.is_some() {
        return Err(word("agent-door-person-from-device"));
    }
    if as_person && matches!(facts.entrance, Origin::Environment(_)) {
        // A confined environment's programs are agents: none acts as the person, whose authority
        // reaches past the environment.
        return Err(word("agent-door-environment-as-person"));
    }
    let found = match &facts.origin {
        Some(origin) => workspace_for(handle, origin.workspace.as_deref())
            .ok_or_else(|| word("agent-door-no-workspace")),
        None => environments::workspace_for(handle, person, &facts.entrance, asked),
    };
    if let Err(reason) = &found {
        tracing::info!(client = %facts.client, pid = facts.pid, ?asked, %reason, "agent door: a client's workspace is refused");
    }
    found
}

/// The workspace a client works in, by id and as the person reads it: the one
/// an agent from another device asked for (`asked`), when this desk holds it;
/// else the one in front, as for any other client.
fn workspace_for(handle: &DesktopHandle, asked: Option<&str>) -> Option<(String, String)> {
    handle.read(|tree| {
        asked
            .and_then(|asked| {
                tree.workspaces
                    .iter()
                    .find(|held| !held.id().is_nil() && held.id().to_string() == asked)
            })
            .or_else(|| tree.active_workspace())
            .map(|held| (held.id().to_string(), held.name.clone()))
    })
}

/// How long the person's answer is remembered.
enum Memory {
    Once,
    Session,
    Always,
}

/// The agent id a client is proposed as: its own name, folded into the canonical form agent ids are
/// written in (`Claude Code` → `claude-code` ).
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

/// Bind `path` for this account alone: 0600 in a 0700 directory, staged and renamed into place so
/// nothing sees a socket it cannot connect to. A path that is something else, or that a live
/// session answers on, is refused.
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
        // Nothing is there, or what is there is not a socket: bind it below,
        // where a socket that answers proves itself live.
        // ignored-failure: a path with no live listener is the path to bind
        Ok(_) | Err(_) => {}
    }
    // Short on purpose: a Unix socket path is capped near 108 bytes, and the staged path has to fit
    // wherever the final one does.
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

/// Write `bytes` to `path` , readable by this account only.
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
    /// A connection to an environment's door from a process outside that environment.
    OutsideEnvironment,
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
            Self::OutsideEnvironment => {
                "this door serves only programs inside the environment it belongs to"
            }
        }
    }
}

/// Decide one connection from its peer's facts: this session's account, in this session's
/// namespaces.
pub(crate) fn peer_decision(peer: &Peer) -> Result<(), PeerRefusal> {
    if peer.uid != rustix::process::geteuid().as_raw() {
        return Err(PeerRefusal::OtherAccount);
    }
    if !peer.shares_namespaces {
        return Err(PeerRefusal::Sandboxed);
    }
    Ok(())
}

/// The kernel's account of an accepted connection's peer: its process, and whether it is this
/// login's own.
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

/// Whether `pid` runs in this process's user and mount namespaces: what tells a program of this
/// login from one a sandbox put elsewhere.
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

/// What the person is shown about the connection from `pid`, which came in by `entrance`.
fn facts_of(pid: u32, launches: &Launches, entrance: Origin) -> Facts {
    Facts {
        client: String::new(),
        version: None,
        pid,
        exe: std::fs::read_link(format!("/proc/{pid}/exe")).ok(),
        app: launches.app_of_descendant(pid).map(|app| app_name(&app)),
        origin: None,
        entrance,
    }
}

/// Tell a refused client why, as a JSON-RPC error, then let it go.
fn refuse(mut stream: UnixStream, refusal: &PeerRefusal) {
    use std::io::Write as _;
    let reply =
        tilo_runtime::session_door::error_reply(&serde_json::Value::Null, refusal.message());
    let _ = stream.set_nonblocking(false);
    if let Err(error) = stream.set_write_timeout(Some(Duration::from_secs(1))) {
        tracing::warn!(%error, "a refused client's connection has no write timeout");
    }
    if let Err(error) = stream.write_all(format!("{reply}\n").as_bytes()) {
        tracing::warn!(%error, "a refused agent was not told why");
    }
}

/// What a control client that is not a program inside an app this session started is told: the
/// pointer, the keys and the frames of the person's desk are theirs, and an agent acts through the
/// actions a resource publishes.
const CONTROL_IN_APP_ONLY: &str = "The control protocol is served here to a program running \
     inside an app this session started, as that app, and to nothing else: this program is \
     not one.";

/// Tell a control client it is served nothing here, in the control protocol's own framing, then let
/// it go.
fn refuse_control(mut stream: UnixStream, message: &str) {
    use std::io::Write as _;
    let reply = serde_json::json!({"err": {
        "code": "not_allowed",
        "message": message,
    }});
    if let Err(error) = stream.set_write_timeout(Some(Duration::from_secs(1))) {
        tracing::warn!(%error, "a control client's connection has no write timeout");
    }
    if let Err(error) = stream.write_all(format!("{reply}\n").as_bytes()) {
        tracing::warn!(%error, "a control client was not told why it is served nothing");
    }
}

/// What a connection asks of the session's own doors: answered on the loop's pass, the only place
/// that may touch them.
enum DoorAsk {
    /// What the session's one search finds by `needle`: the same search the
    /// palette and text typed on the canvas run, answered here because the
    /// search lives on the loop's thread.
    Find {
        needle: String,
        done: crate::search::FoundSearch,
    },
    /// Run one of their actions as a step of an agent's turn, already decided
    /// by the broker where it lands: `slot` is its arguments in their slot
    /// form, as the broker carries them. No `reads_only`: the broker decided
    /// the step.
    Step {
        actor: String,
        address: tilo_actions::ActionAddress,
        slot: Option<String>,
        done: tilo_actions::remote::Completer<Result<Option<serde_json::Value>, String>>,
    },
    /// The record of the package the session runs itself, by its app (`so.tilo.shell`): its
    /// document and the packages a program reading it loads, or `None` for one no service keeps a
    /// record of.
    Record {
        app: String,
        done: tilo_actions::remote::Completer<Option<tilo_service::Record>>,
    },
    /// Every app a task's programs may name, which its host loads to run them against: the compiled
    /// module of each package the session runs itself (its description embedded, the packages it
    /// uses first), and the signed surface of each native app this device has.
    Apps(tilo_actions::remote::Completer<Vec<runtime_portal::tilolang_host::App>>),
    /// Run the change `change` of a record a service keeps for a package the session runs itself,
    /// as `actor` , on the pass: its answer as the door gives it, or `None` when no service keeps
    /// that app's record.
    Change {
        actor: tilo_actions::remote::CallerPrincipal,
        change: tilo_actions::remote::RecordChange,
        done: tilo_actions::remote::Completer<Option<Result<serde_json::Value, ActionHostError>>>,
    },
}

/// The seam between the door's connections and the session's own doors.
pub(crate) struct SessionDoors {
    asks: std::sync::Mutex<Vec<DoorAsk>>,
    wake: Wake,
    /// Who sits here: the person a program typed in a conversation runs as.
    person: String,
    /// The session's own actions, which it hands to a connection's door as they are read on
    /// its pass: `None` on a session that serves no broker and lists them itself.
    actions: std::sync::OnceLock<Arc<crate::session_actions::SessionActions>>,
}

/// How long a connection's thread waits on the session's pass for an answer (its record, a change,
/// the modules): the loop answers each pass, so this is only a bound on a frozen session rather
/// than a wait that should ever reach it.
const WAIT: Duration = Duration::from_secs(30);

/// A session that did not answer within [`WAIT`]: it is frozen, and nothing is done elsewhere in
/// its place.
fn unanswered() -> ActionHostError {
    ActionHostError::Unavailable {
        reason: "the session did not answer".to_owned(),
    }
}

impl SessionDoors {
    /// Who sits here: the person a program typed in a conversation, or one an agent writes in its
    /// task, runs as.
    pub(crate) fn person(&self) -> &str {
        &self.person
    }

    /// The seam `wake` wakes the loop on, shared by every connection that asks the session's own
    /// doors, for the person sitting here.
    pub(crate) fn new(wake: Wake, person: &str) -> Self {
        Self {
            asks: std::sync::Mutex::new(Vec::new()),
            wake,
            person: person.to_owned(),
            actions: std::sync::OnceLock::new(),
        }
    }

    /// The session's own actions this door hands a connection, once the session has them.
    pub(crate) fn keep_actions(&self, actions: Arc<crate::session_actions::SessionActions>) {
        let _ = self.actions.set(actions);
    }

    /// The session's own actions, once they are made.
    pub(crate) fn actions(&self) -> Option<Arc<crate::session_actions::SessionActions>> {
        self.actions.get().cloned()
    }

    fn ask(&self, ask: DoorAsk) {
        self.asks.lock_recover().push(ask);
        (self.wake)();
    }

    /// What the session's one search finds by `needle` , answered on the loop's pass: the search
    /// lives on the thread holding the desktop.
    pub(crate) fn find(&self, needle: String, done: crate::search::FoundSearch) {
        self.ask(DoorAsk::Find { needle, done });
    }

    /// Run the session's own action `address` as `actor` with `slot` (its
    /// arguments in the slot form the broker carries), as a step of an agent's
    /// turn the broker already decided: answered on the loop's pass, which is
    /// where the arrangement and the windows are.
    pub(crate) fn invoke_step(
        &self,
        actor: String,
        address: tilo_actions::ActionAddress,
        slot: Option<String>,
        done: tilo_actions::remote::Completer<Result<Option<serde_json::Value>, String>>,
    ) {
        self.ask(DoorAsk::Step {
            actor,
            address,
            slot,
            done,
        });
    }

    /// Answer what the connections asked since the last pass, from `doors`,
    /// the `services` the session runs, and the one `search`; `shows` names
    /// the resources a place can show, as the arrangement holds them, and
    /// `person` is whose calls are their own, not someone's for them.
    ///
    /// The services are borrowed briefly, one ask at a time: a change they carry out is the
    /// published action's own, so a run never needs the services held across a turn of the
    /// loop, and no borrow outlives a single answer.
    pub(crate) fn answer(
        &self,
        doors: &tilo_surface_host::Doors,
        services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
        search: &crate::search::SessionSearch,
        shows: &dyn Fn(&str) -> bool,
        look: &mut tilo_surface_host::Look<'_>,
    ) {
        let asked = std::mem::take(&mut *self.asks.lock_recover());
        for ask in asked {
            match ask {
                DoorAsk::Find { needle, done } => {
                    done.complete(crate::search::found_all(
                        &needle,
                        search.find(&needle),
                        shows,
                    ));
                }
                DoorAsk::Step {
                    actor,
                    address,
                    slot,
                    done,
                } => {
                    let ran = match arguments_of(slot.as_deref()) {
                        Ok(arguments) => {
                            // A step a service publishes runs there, on the record the service
                            // keeps — Chat's `record-work` on a turn's summons is the session's
                            // Chat service's, not the arrangement's. Every other step is the
                            // session's own doors': the arrangement, the windows and the notices.
                            let served = services
                                .borrow()
                                .resources()
                                .iter()
                                .any(|resource| resource.id.as_str() == address.resource);
                            if served {
                                invoke_service_step(services, &actor, &address, &arguments)
                            } else {
                                doors
                                    .invoke_as(&actor, &address, &arguments, look)
                                    .map(|started| match started {
                                        ui::host_operation::ResourceOperationStart::Completed(
                                            output,
                                        ) => Some(serde_json::json!({
                                            "value": output.value,
                                            "resources": output.resources,
                                        })),
                                        ui::host_operation::ResourceOperationStart::Accepted => {
                                            None
                                        }
                                    })
                                    .map_err(|error| error.to_string())
                            }
                        }
                        Err(why) => Err(why),
                    };
                    done.complete(ran);
                }
                DoorAsk::Record { app, done } => {
                    done.complete(services.borrow().record(&app));
                }
                DoorAsk::Apps(done) => {
                    done.complete(catalogue(services));
                }
                DoorAsk::Change {
                    actor,
                    change,
                    done,
                } => done.complete(change_through(services, &actor, &change)),
            }
        }
    }
}

/// The typed arguments a step's slot carries: none for none. A slot that is not a typed object
/// names no argument of a session action, which all take their arguments by name.
fn arguments_of(slot: Option<&str>) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let Some(slot) = slot else {
        return Ok(serde_json::Map::new());
    };
    let json = slot
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .ok_or("a session action takes its arguments by name")?;
    serde_json::from_str(json).map_err(|error| format!("the step's arguments do not read: {error}"))
}

/// Run `address` as `actor` through the session's services, which publish the actions its own
/// doors do not: what a service keeps (Chat's `record-work` on a turn's summons, the arrangement's
/// place read from a service) is invoked on the same pass, on the record the service writes. The
/// `actor` an agent names is `agent:<id>`, attributed as it is at an app's record origin; a person
/// acts as themselves.
pub(crate) fn invoke_service_step(
    services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
    actor: &str,
    address: &tilo_actions::ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<serde_json::Value>, String> {
    use tilo_service::Started;
    let invoker = tilo_actions::ActionInvoker::actor(actor);
    let invoker = if actor.starts_with("agent:") {
        invoker.with_kind(tilo_actions::ActorKind::Agent)
    } else {
        invoker
    };
    match services
        .borrow_mut()
        .invoke_as(&invoker, &address.resource, &address.action, arguments)
    {
        Ok(Started::Done(output)) => Ok(Some(serde_json::json!({
            "value": output.value,
            "resources": output.resources,
        }))),
        Ok(Started::Accepted) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Run the change `change` of a record `services` keep for a package the session runs itself (the
/// shell), as `actor` : the published action is the record's own, run on the pass that publishes
/// it, as the record's origin runs it. `None` when no service keeps that app's record, which is
/// then the workspace's, changed through the broker.
fn change_through(
    services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
    actor: &tilo_actions::remote::CallerPrincipal,
    change: &tilo_actions::remote::RecordChange,
) -> Option<Result<serde_json::Value, ActionHostError>> {
    use tilo_actions::remote::CallerPrincipal;
    // Who the change is attributed to: the agent acting for its person, or the person.
    let invoker = match actor {
        CallerPrincipal::Agent(agent) => {
            tilo_actions::ActionInvoker::actor(format!("agent:{agent}"))
                .with_kind(tilo_actions::ActorKind::Agent)
        }
        CallerPrincipal::Person(person) => tilo_actions::ActionInvoker::actor(person.clone()),
        _ => return None,
    };
    services.borrow().record(&change.app)?;
    let ran = services.borrow_mut().invoke_as(
        &invoker,
        &change.resource,
        &change.action,
        &change.arguments,
    );
    Some(match ran {
        Ok(output) => {
            let document = services
                .borrow()
                .record(&change.app)
                .map(|record| record.document);
            let output = match output {
                tilo_service::Started::Done(output) => serde_json::json!({
                    "value": output.value,
                    "resources": output.resources,
                }),
                tilo_service::Started::Accepted => serde_json::Value::Null,
            };
            Ok(serde_json::json!({"ok": output, "change": document}))
        }
        Err(error) => Err(ActionHostError::Unavailable {
            reason: error.to_string(),
        }),
    })
}

/// What the person's own steps answered, by invocation, for a later look.
type Answered =
    std::collections::VecDeque<(tilo_actions::InvocationId, tilo_actions::InvocationUpdate)>;

/// How many answered session steps a connection remembers.
const ANSWERED_KEPT: usize = 256;

/// The broker as an admitted client's door: every call passes the gate as the agent or person
/// and workspace the connection was admitted for, and the session's own actions are among what
/// the same broker publishes.
#[derive(Clone)]
pub(crate) struct Host {
    /// The session's broker, once it starts one. Without it (a session in a window of another
    /// desktop) the session's own doors are all it offers.
    state: Option<Arc<AppBrokerState>>,
    /// What a connection asks the session on its pass: the one search, the records a package
    /// the session runs itself keeps, and the person's own steps on its actions.
    session: Arc<SessionDoors>,
    /// What the person's own steps answered, by invocation, for a later look.
    answered: Arc<std::sync::Mutex<Answered>>,
    /// The session's own actions, when this session serves them: a desktop with no broker
    /// lists them itself.
    session_actions: Option<Arc<crate::session_actions::SessionActions>>,
}

impl Host {
    /// The door over `state` , with `session` beside it.
    pub(crate) fn new(state: Option<Arc<AppBrokerState>>, session: Arc<SessionDoors>) -> Self {
        Self {
            state,
            session,
            answered: Arc::default(),
            session_actions: None,
        }
    }

    /// The same door, listing and answering the session's own actions from `actions` when this
    /// desktop runs no broker.
    pub(crate) fn with_session_actions(
        mut self,
        actions: Option<Arc<crate::session_actions::SessionActions>>,
    ) -> Self {
        self.session_actions = actions;
        self
    }

    /// An invocation the person at this seat made through the seat's own control doors: the same as
    /// [`ActionHost::invoke`], except that the broker keeps the seat's certificate custody for it
    /// ([`AppBrokerState::action_invoke_local_person`]), as it does for the palette's actions, so
    /// what the person may do here is what the person may do anywhere.
    /// Only the seat's own control sockets call it — the owner's door, whose peer is the account
    /// whose desktop this is ([`crate::test_control::owner_decision`]), and the development
    /// test-control socket, which drives the seat as the person. An agent, or a person over a
    /// network, MCP or app connection, invokes as itself through [`ActionHost::invoke`], and the
    /// broker's per-action and per-role permissions decide; the broker refuses custody to anything
    /// but a person at the native seat.
    pub(crate) fn invoke_at_seat(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.invoke_within(caller, request, true)
    }

    /// The broker's answer to `request` for `caller` , at the seat or as the caller itself.
    fn invoke_within(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        at_seat: bool,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        // The seat's own doors, and a desktop with no broker at all, reach the session's own
        // actions on the loop: the broker is the gate for a caller on another's word, and the
        // person at this seat is nobody's delegate.
        let own = request.target.provider == wire_provider();
        let person = matches!(
            caller.principal(),
            tilo_actions::remote::CallerPrincipal::Person(_)
        );
        if person && (own || self.state.is_none()) {
            return self.invoke_person_step(caller, request);
        }
        // An agent's read of the session's own actions needs no turn and no durably signed
        // admission: it changes nothing, and the catalogue already held it to the caller's
        // grants. Its change is the turn's, and the broker decides it (shell.md §8.1).
        if own && self.reads(&request) {
            return self.invoke_read_step(caller, request);
        }
        match &self.state {
            Some(state) if at_seat => state.action_invoke_local_person(caller, &request),
            Some(state) => state.action_invoke(caller, None, None, &request),
            None => Pending::ready(Err(ActionHostError::Unavailable {
                reason: "this desktop's apps are not running here".to_owned(),
            })),
        }
    }

    /// Whether the step `request` names reads what it acts on, as the session's own door
    /// describes it. A read changes nothing, so it needs neither a turn nor a durable admission.
    fn reads(&self, request: &InvokeRequest) -> bool {
        let rows = match &self.session_actions {
            Some(actions) => actions.offered(),
            None => Vec::new(),
        };
        rows.into_iter()
            .find(|placed| placed.action.address == request.target.address)
            .is_some_and(|placed| placed.action.kind == tilo_actions::OperationKind::View)
    }

    /// An agent's read of the session's own actions: handed to the loop, which is where they
    /// run, as the person's own step is, and answered with what it read.
    fn invoke_read_step(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let actor = match caller.principal() {
            tilo_actions::remote::CallerPrincipal::Agent(agent) => format!("agent:{agent}"),
            _ => return Pending::ready(Err(ActionHostError::NotPermitted)),
        };
        self.invoke_step_at(actor, request)
    }

    /// The session's own actions as rows, as the kind of publisher its own door is, for a
    /// desktop that runs no broker.
    fn session_rows(&self) -> Vec<tilo_actions::remote::CatalogueRow> {
        let rows = match &self.session_actions {
            Some(actions) => actions.offered(),
            None => Vec::new(),
        };
        rows.into_iter()
            .map(|placed| {
                tilo_actions::remote::CatalogueRow::for_placed_action(
                    wire_provider(),
                    wire_provider(),
                    placed,
                )
            })
            .collect()
    }

    /// A person's own step: handed to the loop, which is where the session's own doors are,
    /// and answered as the door answers an invocation.
    fn invoke_person_step(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let actor = match caller.principal() {
            tilo_actions::remote::CallerPrincipal::Person(person) => person.clone(),
            _ => return Pending::ready(Err(ActionHostError::NotPermitted)),
        };
        self.invoke_step_at(actor, request)
    }

    /// The session's own action `request` names, carried out on the loop's pass as `actor` (the
    /// person at this seat, or an agent reading): where the arrangement and the windows are.
    fn invoke_step_at(
        &self,
        actor: String,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let slot = request.argument.to_slot();
        let invocation = request.invocation;
        let (done, ran) = tilo_actions::remote::pending();
        self.session
            .invoke_step(actor, request.target.address, slot, done);
        let answered = Arc::clone(&self.answered);
        ran.map(move |ran| {
            use tilo_actions::remote::ReceiptStatus;
            let status = match ran {
                Ok(output) => ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: output,
                        resources: Vec::new(),
                    },
                },
                Err(reason) => ReceiptStatus::Rejected {
                    error: ActionHostError::Refused { reason },
                },
            };
            // The connection reads its own step back by its invocation id, as it reads any
            // other action it took.
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
}

impl ActionHost for Host {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        match &self.state {
            Some(state) => state.action_query(caller, &query),
            // A desktop that runs no broker (a session in a window of another, a test with no
            // action plane) still publishes its own actions: they are read on the loop and
            // held here, so the door lists them itself.
            None => Pending::ready(Ok(CataloguePage::from_rows(
                String::new(),
                self.session_rows(),
                &query,
            ))),
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        // A person typing at a REPL is the seat: their own step is carried out on the loop's
        // pass, where the arrangement and the windows are, as their own input is — never
        // through the broker, which decides for an actor on another's word but has no
        // certificate for the person alone. An agent's step is the turn's, and the broker
        // decides it exactly as it decides the agent's app action in that turn.
        // An admitted connection invokes as itself: the broker decides by its per-action and
        // per-role permissions. Seat custody belongs to the seat's own control doors alone
        // ([`Host::invoke_at_seat`]), and the person's own steps on the session go to the loop
        // wherever they come from ([`Host::invoke_within`]).
        self.invoke_within(caller, request, false)
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

    /// The session's one search, the one the palette and text typed on the canvas run: what an
    /// agent's `find` answers with, asked on the loop's pass as the session's own doors are.
    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        let session = Arc::clone(&self.session);
        Some(Arc::new(crate::search::SearchThrough::new(Arc::new(
            move |needle: String, done: crate::search::FoundSearch| session.find(needle, done),
        ))) as Arc<dyn tilo_actions::remote::ResourceSearch>)
    }

    /// The records a connection reads outside their apps: the workspace's own, as the broker
    /// decides them, and the records of packages this session runs itself (the shell), which its
    /// services keep: a program reads them by the app, as it reads Notes', with no case of its own.
    fn record_reads(&self) -> Option<Arc<dyn tilo_actions::remote::RecordReads>> {
        Some(Arc::new(ServiceReads {
            state: self.state.as_ref().map(Arc::downgrade),
            session: Arc::clone(&self.session),
        }))
    }

    /// The changes a connection makes to those records: a package the session runs itself is
    /// changed through its own services, and any other app's at its record's origin.
    fn record_changes(&self) -> Option<Arc<dyn tilo_actions::remote::RecordChanges>> {
        Some(Arc::new(ServiceChanges {
            state: self.state.as_ref().map(Arc::downgrade),
            session: Arc::clone(&self.session),
        }))
    }
}

/// The records the session's services keep for packages it runs itself (the shell), read by a
/// connection as any app's: the record of each such package is published as its app's committed
/// document, and the workspace's own through the broker it starts.
struct ServiceReads {
    /// The broker, once the session starts one: a workspace's own records are read through it.
    state: Option<std::sync::Weak<AppBrokerState>>,
    /// What a connection asks the session on its pass: the record a package it runs itself keeps.
    session: Arc<SessionDoors>,
}

impl tilo_actions::remote::RecordReads for ServiceReads {
    fn read(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::RecordAddress,
    ) -> Result<tilo_actions::remote::RecordCopy, ActionHostError> {
        let (done, asked) = tilo_actions::remote::pending();
        self.session.ask(DoorAsk::Record {
            app: address.app.clone(),
            done,
        });
        match asked.wait_timeout(WAIT) {
            Ok(Some(Some(record))) => {
                return Ok(tilo_actions::remote::RecordCopy::Document(record.document));
            }
            // A session that did not answer keeps the record still: reading the workspace's copy
            // instead would answer from somewhere else.
            Err(_) => return Err(unanswered()),
            Ok(None | Some(None)) => {}
        }
        // Any other app's record is the workspace's, read through the broker.
        let Some(state) = self.state.as_ref().and_then(std::sync::Weak::upgrade) else {
            return Err(ActionHostError::Unavailable {
                reason: "this desktop keeps no records of its own".to_owned(),
            });
        };
        let Some(reads) = state.record_reads() else {
            return Err(ActionHostError::Unavailable {
                reason: tilo_actions::remote::RECORD_READS_UNAVAILABLE.to_owned(),
            });
        };
        reads.read(caller, address)
    }
}

/// The records the session's services keep for packages it runs itself, changed by a connection as
/// any app's: the change is the published action's own, run on the pass that publishes the records,
/// by the service keeping them.
struct ServiceChanges {
    /// The broker, once the session starts one: a workspace's own records are changed at their
    /// origin through it.
    state: Option<std::sync::Weak<AppBrokerState>>,
    /// What a connection asks the session on its pass.
    session: Arc<SessionDoors>,
}

impl tilo_actions::remote::RecordChanges for ServiceChanges {
    fn apply(
        &self,
        caller: &Caller,
        change: &tilo_actions::remote::RecordChange,
    ) -> Result<serde_json::Value, ActionHostError> {
        let (done, answered) = tilo_actions::remote::pending();
        self.session.ask(DoorAsk::Change {
            actor: caller.principal().clone(),
            change: change.clone(),
            done,
        });
        match answered.wait_timeout(WAIT) {
            Ok(Some(Some(answer))) => return answer,
            // The session may still make the change: making it at the origin as well would make it
            // twice.
            Err(_) => return Err(unanswered()),
            // No service keeps a record of that app: the workspace's own, changed at its origin
            // through the broker.
            Ok(None | Some(None)) => {}
        }
        let Some(state) = self.state.as_ref().and_then(std::sync::Weak::upgrade) else {
            return Err(ActionHostError::Unavailable {
                reason: "this desktop keeps no records of its own".to_owned(),
            });
        };
        let Some(changes) = state.record_changes() else {
            return Err(ActionHostError::Unavailable {
                reason: "this desktop applies no task's changes at a record's origin".to_owned(),
            });
        };
        changes.apply(caller, change)
    }
}

/// Every app a task's programs may name: the compiled module of each package the session runs
/// itself, and the signed surface of each native app installed on this device.
/// One catalogue, whoever reads it: `tilolang check` reads the same surfaces from the apps'
/// directories, and both compile a program against the packages they read from them.
fn catalogue(
    services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
) -> Vec<runtime_portal::tilolang_host::App> {
    use runtime_portal::tilolang_host::{App, Module};
    let mut apps: Vec<App> = services
        .borrow()
        .records()
        .into_iter()
        .flat_map(|record| record.packages)
        .map(|wasm| {
            App::Module(Module {
                wasm: wasm.to_vec(),
                ids: String::new(),
                imports: Vec::new(),
            })
        })
        .collect();
    let installed = match app_catalogue::apps::signed_surfaces_from_env() {
        Ok(installed) => installed,
        Err(error) => {
            tracing::warn!(%error, "agent door: the installed apps' surfaces are not read");
            Vec::new()
        }
    };
    for (app, title, surface) in installed {
        let Ok(surface) = String::from_utf8(surface) else {
            tracing::warn!(%app, "agent door: an app's surface is not text");
            continue;
        };
        if apps.iter().any(|known| match known {
            App::Surface { app: named, .. } => *named == app,
            App::Module(_) => false,
        }) {
            continue;
        }
        apps.push(App::Surface {
            app,
            title,
            surface,
        });
    }
    apps
}

/// Where the session's door starts a connection's task host: a thread of the session's own running
/// the task's tilolang host (`tilolang_host::task`) over the program connection. The host holds no
/// authority: it decides nothing and writes no record, and everything its programs read or change
/// comes back over the connection to the door, which does it as the program's author, through the
/// same gate as every other call.
pub(crate) struct TaskHosts {
    /// The person the connection was admitted for: `me` in its programs.
    person: std::sync::Mutex<Option<String>>,
    /// What a connection asks the session's own doors, for the records the packages it runs itself
    /// keep: its task host names the modules they are read by, before it starts, on the loop's
    /// pass.
    session: Arc<SessionDoors>,
}

impl TaskHosts {
    /// The hosts of the task connections `person` asks about, whose programs run as `person`,
    /// reading the session's own records through `session`.
    pub(crate) fn new(person: &str, session: Arc<SessionDoors>) -> Self {
        Self {
            person: std::sync::Mutex::new(Some(person.to_owned())),
            session,
        }
    }
}

impl tilo_runtime::mcp::TaskHosts for TaskHosts {
    fn start(&self, workspace: &str, task: &str) -> io::Result<std::os::fd::OwnedFd> {
        let (door_end, host_end) = UnixStream::pair()?;
        // The person's own connection into the same session: the lines they type run there,
        // where the `let`s the agent's programs bound are.
        let (_person_ours, person_host) = UnixStream::pair()?;
        // The apps the task's programs are compiled against: every package the session runs itself
        // (the shell), whose records a program reads by the app, every native app this device has,
        // from its signed surface, and the platform's own (the device and its arrangement,
        // Settings).
        let (done, whole) = tilo_actions::remote::pending();
        self.session.ask(DoorAsk::Apps(done));
        let apps = whole.wait_timeout(WAIT).ok().flatten().unwrap_or_default();
        let encoded = runtime_portal::tilolang_host::encode_apps(&apps);
        let apps = tilo_render_protocol::shm::sealed_blob("tilolang-apps", &encoded)?;
        let request = runtime_portal::tilolang_host::TaskRequest {
            task: task.to_owned(),
            room: workspace.to_owned(),
            person: self.person.lock_recover().clone(),
            apps_len: encoded.len() as u64,
            records_under: std::collections::BTreeMap::new(),
        };
        std::thread::Builder::new()
            .name("tilo-task-host".into())
            .spawn(move || {
                let fds = runtime_portal::tilolang_host::TaskFds {
                    program: host_end.into(),
                    apps,
                };
                let opened =
                    tilolang_host::task::Task::open_with(&request, fds, Some(person_host));
                let task = match opened {
                    Ok(task) => task,
                    Err(error) => {
                        tracing::warn!(task = request.task, %error, "agent door: a task's host did not start");
                        return;
                    }
                };
                // Every line the door or the person sends is answered; the host ends when the door
                // closes the connection, with the client that started it.
                while task.serve_all() {}
                tracing::debug!(task = request.task, "agent door: a task's host ended");
            })?;
        Ok(door_end.into())
    }
}

/// A client the door accepted: its socket, and where it came from.
struct Opened {
    stream: UnixStream,
    from: From,
}

/// Where a connection came from.
enum From {
    /// A program of this login, by the pid that opened it: how the door knows whether it is a
    /// program inside an app.
    Login(u32),
    /// A program inside a confined environment of this login, by its pid, at that environment's
    /// own door ([`environments`]).
    Environment(u32, Arc<environments::Environment>),
    /// Another of the person's devices, over iroh ([`crate::remote_door`]).
    Device(Box<tilo_runtime::remote_door::Origin>),
}

/// Serve one connection of this account, on its own thread: read what it opens with, ask the
/// person, and only once they admit it answer what it asked. A refusal answers its `initialize`
/// with a JSON-RPC error and closes.
fn serve_connection(opened: Opened, serving: &Serving) {
    let Serving {
        broker,
        remote,
        doors: session,
        requests,
        launches,
        wake,
        control,
        seats,
        ..
    } = serving;
    let (broker, remote) = (broker.clone(), remote.clone());
    let Opened { mut stream, from } = opened;
    let first = match tilo_runtime::session_door::read_first_line(&mut stream) {
        Ok(first) => first,
        Err(error) => {
            // ignored-failure: a client that left before asking needs no answer
            tracing::debug!(%error, "agent door: the client left before its first request");
            return;
        }
    };
    let (pid, entrance) = match from {
        From::Login(pid) => (pid, Origin::Host),
        From::Environment(pid, environment) => (pid, Origin::Environment(environment)),
        From::Device(origin) => {
            // Another device's agent is served MCP and nothing else: what is not an `initialize` is
            // refused before anyone is asked.
            let Some(broker) = broker else {
                refuse(stream, &PeerRefusal::Unavailable);
                return;
            };
            let facts = Facts {
                client: String::new(),
                version: None,
                pid: 0,
                exe: None,
                app: None,
                origin: Some(*origin),
                entrance: Origin::Host,
            };
            serve_admitted(
                stream,
                first,
                facts,
                (broker, remote),
                session,
                (requests, wake, seats),
            );
            return;
        }
    };
    let Some(stream) = environments::answer_door_requests(stream, &first, pid, &entrance, serving)
    else {
        return;
    };
    if let Some(forward) = tilo_runtime::remote_door::forward_request(&first) {
        // A client of this login asks to be carried to another of the person's devices, which
        // decides whether it works there.
        tracing::info!(pid, device = %forward.device, "agent door: a client asks to work on another device");
        match remote {
            Some(remote) => remote.forward(stream, &forward),
            None => tilo_runtime::remote_door::refuse_forward(
                stream,
                "this desktop reaches none of your other devices".into(),
            ),
        }
        return;
    }
    if tilo_runtime::session_door::is_control_request(&first) {
        // Only a program inside an app this session started drives the control protocol: it acts as
        // that app, in the workspace the app was started into, and the broker's permissions decide
        // for it.
        let Some(app) = launches.app_of_descendant(pid) else {
            tracing::info!(
                pid,
                "agent door: a control connection is outside this session's apps"
            );
            refuse_control(stream, CONTROL_IN_APP_ONLY);
            return;
        };
        let Some(workspace) = launches.workspace_of(pid, &app) else {
            tracing::info!(pid, %app, "agent door: a control connection's app is not running");
            refuse_control(stream, CONTROL_IN_APP_ONLY);
            return;
        };
        tracing::info!(pid, %app, %workspace, "agent door: a program in an app asked in the control protocol");
        let first = String::from_utf8_lossy(&first).into_owned();
        crate::test_control::serve_app(stream, first, app_name(&app), workspace, control);
        return;
    }
    let Some(broker) = broker else {
        refuse(stream, &PeerRefusal::Unavailable);
        return;
    };
    let facts = facts_of(pid, launches, entrance);
    serve_admitted(
        stream,
        first,
        facts,
        (broker, remote),
        session,
        (requests, wake, seats),
    );
}

/// Ask the person about one connection whose first line, `first` , opened the MCP door, and serve
/// it as whom they admit it as, through `broker` .
fn serve_admitted(
    stream: UnixStream,
    first: Vec<u8>,
    facts: Facts,
    (broker, remote): (Arc<AppBrokerState>, Option<Arc<Remote>>),
    session: &Arc<SessionDoors>,
    (requests, wake, seats): (&Sender<Request>, &Wake, &Seats),
) {
    let tasks = Arc::new(TaskHosts::new(&session.person, Arc::clone(session)));
    let admitted = Arc::clone(&tasks);
    let seat = seats.joining(facts.entrance.door());
    let seat_stream = match stream.try_clone() {
        Ok(copy) => Some(copy),
        Err(error) => {
            tracing::warn!(%error, "an agent's seat has no handle on its connection");
            None
        }
    };
    let ended = tilo_runtime::session_door::serve_with(
        stream,
        Some(first),
        || Ok(Host::new(Some(broker), Arc::clone(session)).with_session_actions(session.actions())),
        tilo_runtime::session_door::Beside {
            tasks: Some(tasks as Arc<dyn tilo_runtime::mcp::TaskHosts>),
            devices: remote
                .map(|remote| remote as Arc<dyn tilo_runtime::remote_door::DeviceDirectory>),
        },
        |hello, connected| {
            let consent = ask(requests, wake, facts, hello, connected, seat);
            if let Consent::Admit { person, .. } | Consent::Person { person, .. } = &consent {
                *admitted.person.lock_recover() = Some(person.clone());
            }
            if let Consent::Admit { .. } = &consent
                && let Some(stream) = &seat_stream
            {
                seats.attached(seat, stream);
            }
            consent
        },
    );
    seats.left(seat);
    if let Err(error) = ended {
        // ignored-failure: a client that disconnected needs no answer
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
    seat: u64,
) -> Consent {
    facts.client.clone_from(&hello.client_name);
    facts.version.clone_from(&hello.client_version);
    let (decide, mut decided) = tokio::sync::oneshot::channel();
    let request = Request {
        facts,
        as_person: hello.as_person,
        workspace: hello.workspace.clone(),
        seat,
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

/// The provider the session's own actions are published under.
fn wire_provider() -> &'static str {
    tilo_actions::remote::SESSION_ACTIONS_PROVIDER
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
    fn a_seated_agent_shows_on_the_desk_and_its_stop_ends_its_connection() {
        use std::io::Read as _;
        let seats = Seats::new();
        let (served, client) = UnixStream::pair().unwrap();
        let asked = seats.joining(None);
        assert!(
            seats.readings().is_empty(),
            "nothing shows before the answer"
        );
        let shown = seats.revision();
        seats.admitted(asked, "claude-code", "Claude Code", "Studio");
        seats.attached(asked, &served);
        assert!(seats.revision() > shown, "the desk is told");
        let readings = seats.readings();
        assert_eq!(readings.len(), 1, "one seat");
        assert_eq!(readings[0].key, "claude-code");
        assert_eq!(readings[0].name, "Claude Code");
        assert_eq!(readings[0].doing, "Studio");
        assert!(readings[0].seat, "a seat, not a run's agent");
        // One agent, two connections: the bar shows one presence, and the person's Stop ends both.
        let (second_served, second_client) = UnixStream::pair().unwrap();
        let other = seats.joining(None);
        seats.admitted(other, "claude-code", "Claude Code", "Studio");
        seats.attached(other, &second_served);
        assert_eq!(seats.readings().len(), 1, "one presence per agent");
        assert!(seats.stop("claude-code"), "the person's Stop");
        for client in [client, second_client] {
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut byte = [0u8; 1];
            let mut ended = &client;
            assert_eq!(
                ended.read(&mut byte).unwrap_or(0),
                0,
                "the connection ended"
            );
        }
        assert!(seats.readings().is_empty(), "the seat left with it");
        assert!(!seats.stop("claude-code"), "nothing to stop twice");
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
