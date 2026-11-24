//! The session's MCP server: how agents reach the action API. `$XDG_RUNTIME_DIR/tilo-session/agent.sock`
//! (0600, in the session's 0700 directory) is where a program of this login reaches this desktop's
//! actions as an agent, never as the person. Framing is newline-delimited MCP JSON-RPC
//! ([`tilo_mcp::session_api`]). Three checks stand between a connection and the desktop: its
//! peer must be this session's own account in this session's namespaces; the person must allow it,
//! as an asked record ([`AskSender`]); and the dispatch then serves it as the person's
//! agent member for that client. Each connection is one agent session
//! ([`crate::agent_sessions`]): its own presence, its own run in history, and its own Stop. The
//! person's own programs reach the desktop as them through Tilo's API ([`crate::api_server`]),
//! never here. "Always allow" is kept in this account's private state.
//!
//! A confined environment (a `ush` guest) has a server of its own, which its `ush` asks for here
//! ([`environments`]): a program there works in the environment's workspace, and the person's
//! answers about it are remembered per environment. A client may ask for a workspace by id instead
//! of the one in front ([`tilo_mcp::session_api::WORKSPACE`]); it is admitted there only if
//! the person is one of its people.

mod agent_key;
mod environments;
mod room;
pub(crate) mod service_steps;
mod task_hosts;

use environments::Origin;
pub(crate) use room::{Place, Room};
pub(crate) use service_steps::invoke_service_step;
use service_steps::{arguments_of, change_through, with_turn_beside};
use task_hosts::SessionCatalogue;

use std::collections::HashMap;

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tilo_action_policy::Lifetime;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Completer, ContentPuts,
    InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_compositor::ReadableWatch;
use tilo_lock::MutexRecover as _;
use tilo_mcp::session_api::{Consent, Hello};
use tilo_seal::secret_service::CallerKey;
use tilo_service::{Answer, AskSender, Asked, Question, Wake};
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_sheets::{word, words};
use tilo_workspace_service::agents::agent_keys::{AgentSigner, Signed};

use crate::Shared;
use crate::agent_sessions::{AgentSessions, Doing};
use crate::app_names::app_name;
use crate::launches::Launches;
use crate::platform::steps::SessionSteps as _;
use tilo_action_policy::AgentSessionId;

// The records a connection reads and changes, which the server answers for.
mod records;
pub(crate) use records::ServiceRecords;

/// The kind of question the server asks the person.
pub(crate) const KIND: &str = "session.mcp-server";

/// How often a waiting connection looks for the person's answer and for its client.
const ASK_EVERY: Duration = Duration::from_millis(50);

/// Where "Always allow" answers are kept, under the session's state directory.
const BINDINGS_FILE: &str = "mcp-server-bindings.json";

/// Where this session's server goes: the login's well-known path in its runtime directory, where
/// its programs look. `None` when there is no runtime directory to put it in.
#[must_use]
pub fn path() -> Option<PathBuf> {
    tilo_mcp::session_api::agent_socket_path(std::env::var_os("XDG_RUNTIME_DIR"))
}

/// What a client asks for, as the sheet shows it: where it would work and what it would be called.
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
    /// `clientInfo.name` , as the client gave it.
    client: String,
    version: Option<String>,
    pid: u32,
    /// `/proc/<pid>/exe` .
    exe: Option<PathBuf>,
    /// The app this session launched that the client descends from: the terminal it runs in.
    app: Option<String>,
    /// The socket of this login it came in by: the session's own, or an environment's.
    entrance: Origin,
}

impl Facts {
    /// The line the person reads first: who asks, and from where.
    fn subject(&self) -> String {
        let program = self.exe.as_deref().and_then(Path::file_name).map_or_else(
            || "?".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let who = words(
            "mcp-server-client",
            &[("client", &self.client), ("program", &program)],
        );
        let who = match &self.app {
            Some(app) => words("mcp-server-client-in-app", &[("who", &who), ("app", app)]),
            None => who,
        };
        match &self.entrance {
            Origin::Host => who,
            Origin::Environment(environment) => words(
                "mcp-server-client-in-environment",
                &[("who", &who), ("environment", &environment.name)],
            ),
        }
    }

    /// The remembered identity of this client: the environment it works in (none on the session's
    /// own socket), the executable the kernel names and the name it gives. `None` without an
    /// executable, and such a client is asked about every time.
    fn key(&self) -> Option<BindingKey> {
        // A program of this login is remembered in the environment it works in, if any.
        Some(BindingKey {
            environment: self.entrance.environment().map(str::to_owned),
            exe: self.exe.clone()?,
            client: self.client.clone(),
        })
    }

    /// The agent id the client is proposed as: its own name.
    fn proposed(&self) -> String {
        tilo_mcp::session_api::proposed_agent_id(&self.client)
    }
}

/// One connection waiting for the person, handed to the loop.
struct Request {
    facts: Facts,
    /// The workspace it asks to work in, by id; `None` for the one in front.
    workspace: Option<String>,
    /// The agent key it presents ([`agent_key`]), which decides it without asking the person.
    key: Option<String>,
    /// What the client calls its session ([`tilo_mcp::session_api::SESSION_NAME`]), as its own
    /// harness names it ("Office Hours"): what the shell shows the agent by. Display only.
    session_name: Option<String>,
    session: AgentSessionId,
    /// Answered by the loop once the person decided and the agent is a member the broker's roster
    /// lists ([`Members`]); dropped by the connection when its client leaves, which withdraws the
    /// question.
    decide: tokio::sync::oneshot::Sender<Consent>,
}

/// A connection the person is being asked about.
struct Waiting {
    facts: Facts,
    workspace: String,
    /// The workspace as the person reads it.
    workspace_name: String,
    session: AgentSessionId,
    /// The program the answer is remembered for (`claude-code`).
    agent: String,
    /// What the client calls its session, when it named one: what the shell shows it by.
    session_name: Option<String>,
    /// The question up for it; dropping it withdraws the question.
    asked: Option<Asked>,
    decide: tokio::sync::oneshot::Sender<Consent>,
}

/// What the person's answer remembers: this client, as this agent.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
struct BindingKey {
    /// The confined environment a program of this login works in, by its id
    /// (`<workspace>/<record>`); absent for the session's own socket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
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

/// The server's listener, bound and ready, before the session serves it.
pub(crate) struct Mounted {
    server: Loop,
}

/// The session's side of the server: it asks the person, admits the agent they answered, and serves
/// what it listened for. It runs on the loop's own pass, the only place that may touch the shell.
pub(crate) struct Loop {
    questions: Option<AskSender>,
    listener: UnixListener,
    /// What ends the loop's wait while a client connects.
    watch: Option<ReadableWatch>,
    requests: Sender<Request>,
    asked: Receiver<Request>,
    /// The broker admitted agents are served from, once the session starts it.
    broker: Option<Arc<Dispatch>>,
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
    /// The places connections are served in: as many as the server serves at once.
    room: Room,
    /// Polls the questions: the loop looks for an answer on each of its own passes, so nothing else
    /// has to wake it.
    waker: Waker,
    /// What the connections ask of the session's own services, answered on the loop's pass.
    services: Arc<SessionServices>,
    /// The control protocol's socket, which a program inside an app this session started is served
    /// at, as that app.
    control: Arc<crate::test_control::Shared>,
    sessions: Arc<AgentSessions>,
    /// The sockets of the confined environments open now, and the instance each agent reached.
    environments: environments::Sockets,
    /// The agent key the person started the session with, when they did.
    agent_key: Option<agent_key::AgentKey>,
    /// What a Terminal window's shell in a workspace placed on a host is opened through
    /// ([`crate::workspace_terminals`]).
    hosting: Option<crate::workspace_terminals::Terminals>,
}

/// What serving one connection takes, on a thread of its own.
#[derive(Clone)]
struct Serving {
    broker: Option<Arc<Dispatch>>,
    services: Arc<SessionServices>,
    requests: Sender<Request>,
    launches: Arc<Launches>,
    wake: Wake,
    control: Arc<crate::test_control::Shared>,
    sessions: Arc<AgentSessions>,
    /// The places connections are served in: as many as the server serves at once.
    room: Room,
    environments: environments::Opener,
    hosting: Option<crate::workspace_terminals::Terminals>,
}

impl Serving {
    /// Serve one connection of this account, from `from`, on its own thread, in the `place` it
    /// took, which it gives back when it ends: nothing it asks for runs before the person admits
    /// it.
    fn serve(&self, place: Place, stream: UnixStream, from: From) {
        let serving = self.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-mcp-server".into())
            .spawn(move || {
                serve_connection(Opened { stream, from }, &serving);
                drop(place);
                // A question for a client that is gone is withdrawn on the next pass.
                (serving.wake)();
            });
        if let Err(error) = spawned {
            // The thread that did not start took the connection, and its place, with it.
            tracing::warn!(%error, "MCP server: a connection could not be served");
        }
    }
}

impl Mounted {
    /// Serve what the server listened for through `dispatch`, the device's one dispatch, which
    /// decides whether the client may act.
    pub(crate) fn serve(mut self, dispatch: Option<Arc<Dispatch>>) -> Loop {
        self.server.broker = dispatch;
        self.server
    }
}

/// The server `options` name, bound and asking `questions` ; `None` , saying why, when it does not
/// open.
pub(crate) fn mount_named(
    options: &crate::Options,
    questions: Option<AskSender>,
    services: Arc<SessionServices>,
    control: Arc<crate::test_control::Shared>,
    sessions: Arc<AgentSessions>,
) -> Option<Mounted> {
    let path = options.mcp_server.as_deref()?;
    match mount(Mounting {
        path,
        questions,
        launches: Arc::clone(&options.launches),
        wake: options.wake.handle(),
        state: options.state.as_deref(),
        agent_key: options.agent_key.as_deref(),
        services,
        control,
        sessions,
    }) {
        Ok(mounted) => Some(mounted),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "the MCP server does not open");
            None
        }
    }
}

/// What the server binds with: where it listens, whose questions its clients are admitted by, the
/// apps this session started, where "Always allow" is kept, the
/// session's own services, the workspace governance it holds, and the control socket a program inside an
/// app drives.
pub(crate) struct Mounting<'a> {
    path: &'a Path,
    questions: Option<AskSender>,
    launches: Arc<Launches>,
    wake: Wake,
    state: Option<&'a Path>,
    /// The file holding the agent key the person started the session with.
    agent_key: Option<&'a Path>,
    services: Arc<SessionServices>,
    control: Arc<crate::test_control::Shared>,
    sessions: Arc<AgentSessions>,
}

/// Bind the server `mounting` names. "Always allow" answers are kept under its
/// `state`, when the session keeps anything.
///
/// # Errors
/// The path is not a socket, another session answers on it, or it cannot be
/// bound.
pub(crate) fn mount(mounting: Mounting<'_>) -> io::Result<Mounted> {
    let Mounting {
        path,
        questions,
        launches,
        wake,
        state,
        agent_key,
        services,
        control,
        sessions,
    } = mounting;
    let agent_key = agent_key.and_then(agent_key::AgentKey::kept);
    let listener = bind_private(path)?;
    listener.set_nonblocking(true)?;
    let environments = environments::Sockets::new(
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(tilo_paths::login::ENVIRONMENTS_DIR),
    );
    let bindings_path = state.map(|directory| directory.join(BINDINGS_FILE));
    let (always, lost) = bindings_path
        .as_deref()
        .map_or_else(|| (HashMap::new(), Lost::default()), read_bindings);
    let (requests, asked) = std::sync::mpsc::channel();
    let server = Loop {
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
        room: Room::default(),
        // The loop polls its questions itself, each pass, so an answer needs no wakeup.
        waker: Waker::noop().clone(),
        services,
        control,
        sessions,
        environments,
        agent_key,
        hosting: None,
    };
    Ok(Mounted { server })
}

impl Shared {
    /// Tell the person once about the answers this computer kept and this build could not read: an
    /// agent they let work here is asked about again, and they are told why rather than losing the
    /// answer in silence.
    pub(crate) fn tell_agent_bindings_lost(&self) {
        let mut server = self.mcp_server.borrow_mut();
        let Some(server) = server.as_mut() else {
            return;
        };
        for agent in server.lost.drain(..) {
            self.notifications
                .borrow()
                .tell(&crate::notices::agent_bindings_notice(agent.as_deref()));
        }
    }
}

impl Loop {
    /// Open the shells of Terminal windows in the workspaces placed on a host through `hosting`,
    /// as the device's shell runtime asks.
    pub(crate) fn open_terminals_on(
        &mut self,
        hosting: Option<crate::workspace_terminals::Terminals>,
    ) {
        self.hosting = hosting;
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
    /// person's agent member for the client (`members` finds or adds it), and the client is served
    /// as that agent.
    pub(crate) fn follow(&mut self, handle: &DesktopHandle, person: &str, members: &Members<'_>) {
        self.accept();
        self.environments.follow(handle, person);
        if let Some(key) = self.agent_key.as_mut() {
            key.certify();
        }
        while let Ok(request) = self.asked.try_recv() {
            if request.key.is_some() {
                self.admit_by_key(handle, request, person, members);
            } else {
                self.request(handle, request, person, members);
            }
        }
        self.settle_waiting(person, members);
    }

    /// Accept every waiting connection, each on its own thread.
    fn accept(&self) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    tracing::warn!(%error, "MCP server: accepting a connection failed");
                    return;
                }
            };
            let pressed = identify(&stream).and_then(|(pid, peer)| {
                let from = entrance(pid, &peer)?;
                // Only a program the server would serve is told that it is full.
                let place = self.room.take().ok_or(PeerRefusal::Busy)?;
                Ok((place, from))
            });
            let (place, from) = match pressed {
                Ok(served) => served,
                Err(refusal) => {
                    tracing::info!(?refusal, "MCP server: a connection is refused");
                    refuse(stream, &refusal);
                    continue;
                }
            };
            self.serve(place, stream, from);
        }
    }

    /// Serve one connection of this account on its own thread, in its `place`: nothing it asks
    /// for runs before the person admits it.
    fn serve(&self, place: Place, stream: UnixStream, from: From) {
        self.serving().serve(place, stream, from);
    }

    /// What serving a connection of this server takes.
    fn serving(&self) -> Serving {
        Serving {
            broker: self.broker.clone(),
            services: Arc::clone(&self.services),
            requests: self.requests.clone(),
            launches: Arc::clone(&self.launches),
            wake: Arc::clone(&self.wake),
            control: Arc::clone(&self.control),
            sessions: Arc::clone(&self.sessions),
            room: self.room.clone(),
            environments: self.environments.opener(),
            hosting: self.hosting.clone(),
        }
    }

    /// Ask the person about `request` , unless what they answered before already says it, and keep
    /// it waiting for their answer.
    fn request(
        &mut self,
        handle: &DesktopHandle,
        request: Request,
        person: &str,
        members: &Members<'_>,
    ) {
        let Request {
            facts,
            workspace: asked,
            session,
            session_name,
            decide,
            ..
        } = request;
        let Some(questions) = self.questions.clone() else {
            let _ = decide.send(Consent::Refuse(
                PeerRefusal::Unavailable.message().to_owned(),
            ));
            return;
        };
        let (workspace, name) = match where_it_works(handle, person, &facts, asked.as_deref()) {
            Ok(found) => found,
            Err(reason) => {
                let _ = decide.send(Consent::Refuse(reason));
                return;
            }
        };
        let key = facts.key();
        let remembered = key
            .as_ref()
            .and_then(|key| self.session.get(key).or_else(|| self.always.get(key)))
            .cloned();
        // What the person said ahead of time admits it as a connection of its own: an answer they
        // remembered for this client.
        if let Some(agent) = remembered.clone() {
            tracing::info!(client = %facts.client, pid = facts.pid, %agent, %workspace, "MCP server: what the person said ahead of time admits this client");
            let consent = as_member(
                members,
                &facts,
                (&workspace, person),
                session_name.as_deref(),
            );
            self.sessions.admit(&session, &consent, &agent, &name);
            let _ = decide.send(consent);
            return;
        }
        let agent = remembered.unwrap_or_else(|| facts.proposed());
        let details = serde_json::to_value(Admission {
            workspace: name.clone(),
            agent: facts.client.clone(),
        })
        .unwrap_or_default();
        let label = facts.subject();
        tracing::info!(
            client = %facts.client,
            pid = facts.pid,
            %agent,
            %workspace,
            "MCP server: asking the person"
        );
        let asked = questions.ask(Question {
            handle: format!("mcp-server:{}", uuid::Uuid::new_v4().simple()),
            kind: KIND.to_owned(),
            app_id: None,
            label,
            details,
        });
        self.waiting.push(Waiting {
            facts,
            workspace,
            workspace_name: name,
            session,
            agent,
            session_name,
            asked: Some(asked),
            decide,
        });
    }

    /// Settle the questions the person answered — or whose client left — and let the admitted
    /// agents in.
    fn settle_waiting(&mut self, person: &str, members: &Members<'_>) {
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
                    let consent = self.settle(&waiting, answer, person, members);
                    let _ = waiting.decide.send(consent);
                }
            }
        }
        self.waiting = still;
    }

    /// What the person's answer means for `waiting` : admitted as the agent it proposed, or refused
    /// with the reason.
    fn settle(
        &mut self,
        waiting: &Waiting,
        answer: Option<Answer>,
        person: &str,
        members: &Members<'_>,
    ) -> Consent {
        let Some(choice) = answer.and_then(|answer| answer.choice) else {
            return Consent::Refuse(word("mcp-server-declined"));
        };
        let Some(remembered) = Lifetime::chosen(choice, tilo_clock::unix_now_ms()) else {
            return Consent::Refuse(word("mcp-server-declined"));
        };
        let consent = as_member(
            members,
            &waiting.facts,
            (&waiting.workspace, person),
            waiting.session_name.as_deref(),
        );
        self.sessions.admit(
            &waiting.session,
            &consent,
            &waiting.agent,
            &waiting.workspace_name,
        );
        if !matches!(consent, Consent::Admit { .. }) {
            return consent;
        }
        // What is remembered is the agent, at its place: each later connection the answer admits
        // is an instance of its own.
        if let Some(key) = waiting.facts.key() {
            match remembered {
                // This sheet offers no time to keep the answer until: one admits this connection.
                Lifetime::Once | Lifetime::Until(_) => {}
                Lifetime::Session => {
                    self.session.insert(key, waiting.agent.clone());
                }
                Lifetime::UntilRevoked => {
                    self.always.insert(key, waiting.agent.clone());
                    self.store_always();
                }
            }
        }
        consent
    }

    /// Take back the answers that let `agent` in: a client whose answer this was is asked again
    /// the next time it connects, as if the person had never said "always allow". What it works on
    /// now is ended by whoever asked ([`AgentSessions::stop_program`]). The key of the session's
    /// agent key, in hex, when that was `agent`'s: the person's keys revoke it.
    pub(super) fn revoke(&mut self, agent: &str) -> Option<String> {
        let key = self
            .agent_key
            .as_mut()
            .filter(|key| same_program(&key.agent(), agent))
            .and_then(agent_key::AgentKey::revoke);
        self.always
            .retain(|_, admitted| !same_program(admitted, agent));
        self.session
            .retain(|_, admitted| !same_program(admitted, agent));
        self.store_always();
        key
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
                    "MCP server: what the person remembered is kept for this session only"
                );
                return;
            }
        };
        std::thread::spawn(move || {
            if let Err(error) = tilo_files::native::atomic::write_private(&path, &bytes) {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "MCP server: what the person remembered is kept for this session only"
                );
            }
        });
    }
}

/// How the session finds or adds the person's agent member for an MCP client in a workspace: by
/// workspace id and client name, answering the agent's id and the name the workspace calls it by
/// once the roster the broker reads lists it ([`Shared::mcp_member`]). A connection admitted on this answer joins the broker at once, on
/// its own thread, so the answer never names an agent the broker would not find.
pub(crate) type Members<'a> = dyn Fn(&str, &str) -> Result<(String, String), String> + 'a;

/// Admit the client `facts` describes, in `workspace` for `person`, as the person's agent member
/// for its client, under the name the workspace calls that member by — or the session the client
/// itself named, when its harness names one ("Office Hours"), which is what presence, notices and
/// history show it by: every connection of it reads as the member, and the shell tells two of them
/// apart by where each works from, beside the name. Refused where that member cannot be found,
/// added or listed.
fn as_member(
    members: &Members<'_>,
    facts: &Facts,
    (workspace, person): (&str, &str),
    session_name: Option<&str>,
) -> Consent {
    let named = session_name.map(str::trim).filter(|name| !name.is_empty());
    match members(workspace, &facts.client) {
        Ok((agent, name)) => Consent::Admit {
            workspace: workspace.to_owned(),
            agent,
            person: person.to_owned(),
            name: named.map_or(name, str::to_owned),
        },
        Err(reason) => {
            tracing::warn!(client = %facts.client, %workspace, %reason, "MCP server: the client's agent member is not added");
            Consent::Refuse(reason)
        }
    }
}

/// Where the client `facts` describes works, by id and as the person reads it, when it may work
/// here at all: its environment's, the one it asked for by id,
/// or the one in front. The reason it may not, as it is told.
fn where_it_works(
    handle: &DesktopHandle,
    person: &str,
    facts: &Facts,
    asked: Option<&str>,
) -> Result<(String, String), String> {
    let found = environments::workspace_for(handle, person, &facts.entrance, asked);
    if let Err(reason) = &found {
        tracing::info!(client = %facts.client, pid = facts.pid, ?asked, %reason, "MCP server: a client's workspace is refused");
    }
    found
}

/// Whether the agent `remembered` is the program the agent `connected` is an instance of. An answer
/// is remembered for the program by the id its client proposed (`claude-code`), and what the answer
/// admits is that program at this device, as an instance of its own (`claude-code@host#2`): its own
/// id begins with the one remembered, at a place or an instance of it.
fn same_program(remembered: &str, running: &str) -> bool {
    let rest = running.strip_prefix(remembered);
    match rest {
        Some(rest) => rest.is_empty() || rest.starts_with(['@', '#']),
        None => false,
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
    tilo_failure::ignored!(
        "a path with no live listener is the path to bind",
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
            // Nothing is there, or what is there is not a socket: bind it
            // below, where a socket that answers proves itself live.
            Ok(_) | Err(_) => {}
        }
    );
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

/// What the kernel says about the process on the other end.
pub(crate) struct Peer {
    uid: libc::uid_t,
    /// Whether it shares this session's user and mount namespaces.
    shares_namespaces: bool,
    /// Whether the kernel would not say which namespaces it is in: a process that made itself
    /// non-dumpable, as one does that loads the device key.
    hidden: bool,
}

/// Why a connection was refused before anyone was asked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PeerRefusal {
    Unidentified,
    OtherAccount,
    Sandboxed,
    /// This session serves no agents: it has no actions published yet.
    Unavailable,
    /// As many connections as the server serves at once are being served ([`room`]).
    Busy,
    /// A connection to an environment's socket from a process outside that environment.
    OutsideEnvironment,
}

impl PeerRefusal {
    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::Unidentified => "the MCP server could not identify the connecting process",
            Self::OtherAccount => {
                "the MCP server serves only programs of the account whose session it is"
            }
            Self::Sandboxed => "programs inside a sandboxed app can't connect here",
            Self::Unavailable => "this desktop isn't serving its actions yet; try again shortly",
            Self::Busy => room::NO_ROOM,
            Self::OutsideEnvironment => {
                "this socket serves only programs inside the environment it belongs to"
            }
        }
    }
}

/// Which entrance the peer `pid`, whose facts the kernel gave as `peer`, comes in by: the login's
/// own program, or a process of this account the kernel hides from the session, which is admitted
/// to prove it holds the device identity for its environments' sockets and its workspaces'
/// terminals ([`holds_device_identity`], [`serve_shell_runtime`]). A hidden process is not one
/// sharing the session's namespaces, so it is [`PeerRefusal::Sandboxed`] to [`peer_decision`]: the
/// proof is what tells it from a program a sandbox put elsewhere.
fn entrance(pid: u32, peer: &Peer) -> Result<From, PeerRefusal> {
    match peer_decision(peer) {
        Ok(()) => Ok(From::Login(pid)),
        Err(PeerRefusal::Sandboxed) if peer.hidden => Ok(From::ShellRuntime(pid)),
        Err(refusal) => Err(refusal),
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
    let shares = shares_security_namespaces(pid);
    Ok((
        pid,
        Peer {
            uid: credentials.uid,
            shares_namespaces: shares == Some(true),
            hidden: shares.is_none(),
        },
    ))
}

/// Whether `proof` shows that the process `pid` holds the device identity, checked with `key`,
/// the session's own ([`tilo_secrets::device_session_key`]): made over that process's own
/// statement ([`runtime_portal::holder_proof::statement`]) for the request `op`.
///
/// That is how the login's shell runtime is told from a sandboxed program, although the kernel
/// shows the session neither its namespaces nor its executable: it reads the device key (its
/// tool builder signs with it), which makes it non-dumpable, and no sandbox hands a program that
/// key. A proof is the process's alone: one copied to another process, or kept past its end,
/// proves nothing there.
fn holds_device_identity(key: Option<&CallerKey>, pid: u32, op: &str, proof: Option<&str>) -> bool {
    let (Some(key), Some(proof)) = (key, proof) else {
        return false;
    };
    let Some(statement) = runtime_portal::holder_proof::statement(pid) else {
        return false;
    };
    hex::decode(proof).is_ok_and(|proof| key.verify(&statement, op, b"", &proof))
}

/// Whether `pid` runs in this process's user and mount namespaces: what tells a program of this
/// login from one a sandbox put elsewhere. `None` when the kernel does not show them to this
/// process, which is no program this server knows by its namespaces.
fn shares_security_namespaces(pid: u32) -> Option<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let same = |space: &str| {
        let ours = std::fs::metadata(format!("/proc/self/ns/{space}")).ok()?;
        let theirs = std::fs::metadata(format!("/proc/{pid}/ns/{space}")).ok()?;
        Some(ours.dev() == theirs.dev() && ours.ino() == theirs.ino())
    };
    Some(same("user")? && same("mnt")?)
}

/// What the person is shown about the connection from `pid`, which came in by `entrance`.
fn facts_of(pid: u32, launches: &Launches, entrance: Origin) -> Facts {
    Facts {
        client: String::new(),
        version: None,
        pid,
        exe: std::fs::read_link(format!("/proc/{pid}/exe")).ok(),
        app: launches.app_of_descendant(pid).map(|app| app_name(&app)),
        entrance,
    }
}

/// Tell a refused client why, as a JSON-RPC error, then let it go.
fn refuse(mut stream: UnixStream, refusal: &PeerRefusal) {
    use std::io::Write as _;
    let reply = tilo_mcp::session_api::error_reply(&serde_json::Value::Null, refusal.message());
    let _ = stream.set_nonblocking(false);
    if let Err(error) = stream.set_write_timeout(Some(Duration::from_secs(1))) {
        tracing::warn!(%error, "a refused client's connection has no write timeout");
    }
    if let Err(error) = stream.write_all(format!("{reply}\n").as_bytes()) {
        tracing::warn!(%error, "a refused agent was not told why");
    }
}

/// What a control client that is not a program inside an app this session started is told: the
/// pointer, the keys and the frames of the person's shell are theirs, and an agent acts through the
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

/// What a connection asks of the session's own services: answered on the loop's pass, the only place
/// that may touch them.
enum ApiAsk {
    /// What the session's one search finds by `needle`: the same search the
    /// palette and text typed on the canvas run, answered here because the
    /// search lives on the loop's thread.
    Find {
        needle: String,
        /// The workspace an agent asking works in, which alone it finds in; `None` for the person.
        workspace: Option<String>,
        done: crate::search::FoundSearch,
    },
    /// Run one of their actions as a step of an agent's turn, already decided
    /// by the broker where it lands: `slot` is its arguments in their slot
    /// form, as the broker carries them, and `beside` the place the ask is in
    /// (the turn's conversation, or empty for the place in front). No
    /// `reads_only`: the broker decided the step.
    Step {
        actor: tilo_actions::Actor,
        address: tilo_actions::ActionAddress,
        slot: Option<String>,
        /// The place the ask is in, by the resource that place shows: the conversation of the
        /// turn an agent asks in, or empty for the place in front.
        beside: String,
        /// The run the step is one of, as its invocation names it.
        run: Option<tilo_actions::WorkGroup>,
        done: tilo_actions::remote::Completer<
            Result<Option<tilo_actions::ActionOutput>, ActionCallError>,
        >,
    },
    /// The packages the session runs itself, which a task's host loads to run programs against:
    /// each one's compiled module, its description embedded, the packages it uses first
    /// ([`task_hosts`] adds every installed app).
    Apps(tilo_actions::remote::Completer<Vec<runtime_portal::process_link::App>>),
    /// The newest reading of the resource `resource` of the app `app`, one the session's own
    /// services publish (the device's sound, the session's windows, Settings' sections): what a
    /// task's program reads within the program it runs, answered on the loop's pass, where the
    /// services are. `None` for a resource no service publishes.
    Reading {
        app: String,
        resource: String,
        done: tilo_actions::remote::Completer<Option<Published>>,
    },
    /// Run the change `change` of a record a service keeps for a package the session runs itself,
    /// as `actor` , on the pass: its answer as the services give it, or `None` when no service keeps
    /// that app's record.
    Change {
        actor: tilo_action_policy::Acting,
        change: tilo_actions::remote::ResourceChange,
        done: tilo_actions::remote::Completer<Option<Result<serde_json::Value, ActionCallError>>>,
    },
}

/// The boundary between the server's connections and the session's own services.
pub(crate) struct SessionServices {
    asks: std::sync::Mutex<Vec<ApiAsk>>,
    wake: Wake,
    /// Who sits here: the person a program typed in a conversation runs as.
    person: String,
    /// The session's own actions, which it hands to a connection's services as they are read on
    /// its pass: `None` on a session that serves no broker and lists them itself.
    actions: std::sync::OnceLock<Arc<crate::session_actions::SessionActions>>,
    /// The workspace each of the shell's windows stands in, by its tab, as of the loop's last
    /// pass: an agent works in one workspace, and the shell's windows of another are not its.
    tabs: std::sync::Mutex<HashMap<u64, String>>,
    /// The shell's workspaces, as of the loop's last pass: each one's decimal id, name and the id it
    /// is shared under, by which an action names a workspace.
    workspaces: std::sync::Mutex<Vec<[String; 2]>>,
    /// The workspaces on the shell this session is founding or about to found, by their id, as of
    /// the loop's last pass: an ask of one of them waits for the founding and is delivered once it
    /// lands, rather than refused for a workspace this device is making.
    founding: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// The asks held for a workspace this session is founding, until the loop delivers them.
    held: std::sync::Mutex<Vec<HeldFounding>>,
    /// The device's tilolang processes, which start a task's host as they start every app's
    /// process: unset until the session reaches them, and on a session that reaches none.
    processes: std::sync::OnceLock<runtime_portal::device_processes::Device>,
    /// How the person's workspaces stand, to tell a workspace this device holds from one it is
    /// founding even between passes (a founding is under way while the loop waits for it).
    view: std::sync::OnceLock<Arc<crate::workspace_view::WorkspaceView>>,
    /// The apps whose records the session's services keep, the packages it runs itself: only
    /// their records are read and changed on the loop's pass. Every other app's are the
    /// workspace's, which a connection reads without waiting for a pass.
    kept: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// Why the apps of each shell workspace this device holds but is not admitted to yet run
    /// nothing, by the workspace's id, as of the loop's last pass ([`Self::keep_awaiting`]).
    awaiting: std::sync::Mutex<HashMap<String, String>>,
    /// When [`Self::awaiting`] was last read: once a second, not on every pass.
    awaited_at: std::sync::Mutex<Option<std::time::Instant>>,
    /// What changed in the records of the workspaces this device holds, for a watch of a whole
    /// workspace ([`crate::change_feed`]).
    changes: Arc<crate::change_feed::Feeds>,
    /// The follows watching the resources the session's services publish themselves, woken when
    /// what they publish moves ([`Self::watch_reading`]).
    readings_woken: std::sync::Mutex<Vec<tilo_actions::remote::ResourceWake>>,
    /// The services' revision the follows were last woken at: a pass that published nothing new
    /// wakes none.
    readings_at: std::sync::Mutex<u64>,
}

/// An ask of a workspace this session is founding, held until the founding lands: the broker
/// takes it then as it would have at the ask, so nothing of a workspace this device is making is
/// refused or dropped.
struct HeldFounding {
    /// The workspace the ask names, by its id.
    workspace: String,
    caller: Caller,
    request: InvokeRequest,
    /// The dispatch that would have taken it: the same one, once the workspace is the service's.
    state: Option<Arc<Dispatch>>,
    done: Completer<Result<InvocationReceipt, ActionCallError>>,
}

/// How often the loop reads again which workspaces wait for their host to admit this device: the
/// admission arrives from the host, at most once per workspace.
const AWAITED_EVERY: Duration = Duration::from_secs(1);

/// How long a connection's thread waits on the session's pass for an answer (its record, a change,
/// the modules): the loop answers each pass, so this is only a bound on a frozen session rather
/// than a wait that should ever reach it.
const WAIT: Duration = Duration::from_secs(30);

/// A session that did not answer within [`WAIT`]: it is frozen, and nothing is done elsewhere in
/// its place.
fn unanswered() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "the session did not answer".to_owned(),
    }
}

/// How long a service's `find` waits for its sources before answering with the rows that came: a
/// source reading a disk, a mailbox or a service off the loop has its own time, and the find is
/// answered with what is in rather than the loop or the caller waiting longer than this. The
/// answer says in its log which source did not (a called tool reads the rows, not the log).
const FIND_WITHIN: Duration = Duration::from_secs(10);

impl crate::platform::steps::SessionSteps for SessionServices {
    /// Run the session's own action `address` as `actor` with `slot` (its
    /// arguments in the slot form the broker carries), as a step of an agent's
    /// turn the broker already decided: answered on the loop's pass, which is
    /// where the layout and the windows are.
    fn invoke_step(
        &self,
        actor: tilo_actions::Actor,
        address: tilo_actions::ActionAddress,
        slot: Option<String>,
        beside: String,
        run: Option<tilo_actions::WorkGroup>,
        done: tilo_actions::remote::Completer<
            Result<Option<tilo_actions::ActionOutput>, ActionCallError>,
        >,
    ) {
        self.ask(ApiAsk::Step {
            actor,
            address,
            slot,
            beside,
            run,
            done,
        });
    }
}

impl SessionServices {
    /// Who sits here: the person a program typed in a conversation, or one an agent writes in its
    /// task, runs as.
    pub(crate) fn person(&self) -> &str {
        &self.person
    }

    /// Whether the session's services keep `app`'s records (a package it runs itself): those are
    /// read and changed on the loop's pass, every other app's are the workspace's.
    pub(crate) fn keeps(&self, app: &str) -> bool {
        self.kept.lock_recover().contains(app)
    }

    /// The services `wake` wakes the loop for, shared by every connection that asks the session's own
    /// services, for the person sitting here.
    pub(crate) fn new(wake: Wake, person: &str) -> Self {
        Self {
            asks: std::sync::Mutex::new(Vec::new()),
            wake,
            person: person.to_owned(),
            actions: std::sync::OnceLock::new(),
            tabs: std::sync::Mutex::new(HashMap::new()),
            workspaces: std::sync::Mutex::new(Vec::new()),
            kept: std::sync::Mutex::default(),
            awaiting: std::sync::Mutex::default(),
            awaited_at: std::sync::Mutex::default(),
            changes: Arc::default(),
            founding: std::sync::Mutex::default(),
            held: std::sync::Mutex::default(),
            processes: std::sync::OnceLock::new(),
            view: std::sync::OnceLock::new(),
            readings_woken: std::sync::Mutex::default(),
            readings_at: std::sync::Mutex::default(),
        }
    }

    /// Follow what the session's services publish: wake `wake` once they move, as a write lands a
    /// record ([`Self::readings_moved`]). The reader decides again for itself whether the reading
    /// it follows actually changed.
    pub(crate) fn watch_reading(&self, wake: &tilo_actions::remote::ResourceWake) {
        let mut woken = self.readings_woken.lock_recover();
        woken.retain(tilo_actions::remote::ResourceWake::waiting);
        woken.push(wake.clone());
    }

    /// What the session's services publish moved to `revision`: wake every follow waiting on it,
    /// once for that revision. A follow woken with nothing new waits again; nothing polls on a
    /// timer of its own.
    pub(crate) fn readings_moved(&self, revision: u64) {
        let mut at = self.readings_at.lock_recover();
        if *at == revision {
            return;
        }
        *at = revision;
        drop(at);
        for wake in std::mem::take(&mut *self.readings_woken.lock_recover()) {
            wake.wake();
        }
    }

    /// Start a task's host through `processes` from now on: the device's, which start every
    /// tilolang process of this computer.
    pub(crate) fn keep_processes(&self, processes: runtime_portal::device_processes::Device) {
        let _ = self.processes.set(processes);
    }

    /// Where the programs of a task run as `person` here are done: each task's host, which the
    /// device's processes start over this session's catalogue. `None` on a session that reaches
    /// no tilolang process.
    pub(crate) fn task_hosts(
        self: &Arc<Self>,
        person: Option<&str>,
    ) -> Option<Arc<tilo_runtime::device::tasks::TaskHosts>> {
        let processes = self.processes.get()?;
        Some(Arc::new(tilo_runtime::device::tasks::TaskHosts::new(
            Arc::new(processes.clone()),
            person,
            Arc::new(SessionCatalogue::new(Arc::clone(self))),
        )))
    }

    /// Follow the changes of the workspaces' records in the device's workspace service `reads`,
    /// woken by what `changes` notes.
    pub(crate) fn keep_records(
        &self,
        reads: std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        changes: crate::platform::records::Changes,
    ) {
        self.changes.keep(reads, changes);
    }

    /// Keep which of the shell's workspaces this device holds but its host has not taken it in
    /// yet, read from `view` on the loop's pass: a machine copied from one of the person's
    /// devices is a device of its own, and only the workspace's host admits it, once it reaches
    /// that host. Until then the workspace's apps take part in nothing there, so what is asked of
    /// them is refused at once, saying so ([`Self::awaited_reason`]).
    pub(crate) fn keep_awaiting(
        &self,
        handle: &DesktopHandle,
        view: &crate::workspace_view::WorkspaceView,
    ) {
        {
            let mut at = self.awaited_at.lock_recover();
            let now = std::time::Instant::now();
            if at.is_some_and(|at| now.duration_since(at) < AWAITED_EVERY) {
                return;
            }
            *at = Some(now);
        }
        let awaiting = handle.read(|tree| {
            tree.workspaces
                .iter()
                .filter_map(|workspace| {
                    let host = view.awaited_host(workspace.id())?;
                    Some((
                        workspace.id().to_string(),
                        awaited_sentence(
                            &tilo_session_core::desktop::names::workspace(&workspace.name),
                            &host,
                        ),
                    ))
                })
                .collect()
        });
        *self.awaiting.lock_recover() = awaiting;
    }

    /// Why an action of `provider` 's app is refused: the workspace it runs in is one this device
    /// holds but is not admitted to yet. `None` for any other provider.
    fn awaited_reason(&self, provider: &str) -> Option<String> {
        let workspace = app_provider_workspace(provider)?;
        self.awaiting.lock_recover().get(workspace).cloned()
    }

    /// The session's services keep `app`'s records from now on.
    pub(crate) fn keep_records_of(&self, app: &str) {
        self.kept.lock_recover().insert(app.to_owned());
    }

    /// Keep how the person's workspaces stand: what tells a workspace this device holds from one
    /// it is founding, even between passes.
    pub(crate) fn keep_view(&self, view: Arc<crate::workspace_view::WorkspaceView>) {
        let _ = self.view.set(view);
    }

    /// Keep the workspaces this session is founding, as of the loop's last pass: an ask of one of
    /// them waits for the founding ([`Self::hold_founding`]), and what waits is delivered when it
    /// is no longer being founded ([`Self::release_founding`]).
    pub(crate) fn keep_founding(&self, founding: std::collections::BTreeSet<String>) {
        *self.founding.lock_recover() = founding;
    }

    /// Whether `workspace` is one this session is founding now: on the shell, made here, and not
    /// this device's yet, or a founding of it under way.
    pub(crate) fn founding_here(&self, workspace: &str) -> bool {
        self.founding.lock_recover().contains(workspace)
            || self
                .view
                .get()
                .is_some_and(|view| founding_of(view, workspace))
    }

    /// Hold an ask of the workspace `workspace` until this session's founding of it lands, and
    /// answer the asker then: the broker takes it as it would have at the ask, so nothing of a
    /// workspace this device is making is refused or dropped.
    pub(crate) fn hold_founding(
        &self,
        workspace: String,
        caller: Caller,
        request: InvokeRequest,
        state: Option<Arc<Dispatch>>,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (done, answer) = tilo_actions::remote::pending();
        self.held.lock_recover().push(HeldFounding {
            workspace,
            caller,
            request,
            state,
            done,
        });
        (self.wake)();
        answer
    }

    /// Deliver every ask held for a workspace this session is no longer founding: as of the loop's
    /// last pass it is this device's now, or the founding was given up, and the asker hears what
    /// the broker says either way.
    pub(crate) fn release_founding(&self) {
        let mut ready = Vec::new();
        {
            let mut held = self.held.lock_recover();
            let mut kept = Vec::with_capacity(held.len());
            for ask in std::mem::take(&mut *held) {
                if self.founding_here(&ask.workspace) {
                    kept.push(ask);
                } else {
                    ready.push(ask);
                }
            }
            *held = kept;
        }
        for ask in ready {
            // The broker's answer waits on this session's loop for a launch to start, so it is
            // taken off the loop, as every ask the broker takes is: the loop that holds the ask is
            // the one that would have to answer it.
            let deliver = async move {
                let delivered = match &ask.state {
                    Some(dispatch) => dispatch.invoke(&ask.caller, ask.request),
                    None => Pending::ready(Err(ActionCallError::Unavailable {
                        reason: "this desktop's apps are not running here".to_owned(),
                    })),
                };
                let done = ask.done;
                delivered.then(move |answer| {
                    if let Some(answer) = answer {
                        done.complete(answer);
                    }
                });
            };
            match tilo_rt::try_app() {
                Ok(runtime) => {
                    runtime.spawn(deliver);
                }
                Err(error) => tracing::warn!(
                    %error,
                    "no task delivers an ask held for a workspace this session was founding"
                ),
            }
        }
    }

    /// Keep where the shell's windows stand now, read from `handle` on the loop's pass.
    pub(crate) fn keep_tabs(&self, handle: &DesktopHandle) {
        let tabs = handle.read(|tree| {
            let mut tabs = HashMap::new();
            for workspace in &tree.workspaces {
                let id = workspace.id().to_string();
                for tab in workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                {
                    tabs.insert(tab.id, id.clone());
                }
            }
            tabs
        });
        *self.tabs.lock_recover() = tabs;
        let workspaces = handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| {
                    [
                        workspace.id().to_string(),
                        tilo_session_core::desktop::names::workspace(&workspace.name),
                    ]
                })
                .collect()
        });
        *self.workspaces.lock_recover() = workspaces;
    }

    /// Whether `request` branches a workspace other than `workspace` , or one the shell does not
    /// know ([`crate::workspace_branch::branches_elsewhere`]).
    fn branches_elsewhere(&self, request: &InvokeRequest, workspace: &str) -> bool {
        let workspaces = self.workspaces.lock_recover().clone();
        crate::workspace_branch::branches_elsewhere(request, &workspaces, workspace)
    }

    /// Whether the shell's window `resource` (`desktop:tab:<tab>`) stands in a workspace other than
    /// `workspace`: a window the shell no longer knows is no one's.
    fn window_elsewhere(&self, resource: &str, workspace: &str) -> bool {
        tilo_session_core::desktop::window::resource_tab(resource).is_some_and(|tab| {
            self.tabs
                .lock_recover()
                .get(&tab)
                .is_none_or(|stands| stands != workspace)
        })
    }

    /// The newest reading of the resource `resource` the app `app` publishes, asked on the loop's
    /// pass, where the services are: what a task's program reads within the program it runs, and
    /// what the shell's parts are handed. `None` for a resource no service publishes.
    ///
    /// # Errors
    ///
    /// Why the session did not answer: it is frozen ([`unanswered`]).
    pub(crate) fn reading(
        &self,
        app: &str,
        resource: &str,
    ) -> Result<Option<Published>, ActionCallError> {
        let (done, answered) = tilo_actions::remote::pending();
        self.ask(ApiAsk::Reading {
            app: app.to_owned(),
            resource: resource.to_owned(),
            done,
        });
        answered
            .wait_timeout(WAIT)
            .map(Option::flatten)
            .map_err(|_| unanswered())
    }

    /// The session's own actions these services hand a connection, once the session has them.
    pub(crate) fn keep_actions(&self, actions: Arc<crate::session_actions::SessionActions>) {
        let _ = self.actions.set(actions);
    }

    /// The shell's workspaces, as of the loop's last pass: each one's id and name.
    pub(crate) fn workspaces(&self) -> Vec<[String; 2]> {
        self.workspaces.lock_recover().clone()
    }

    /// The session's own actions, once they are made.
    pub(crate) fn actions(&self) -> Option<Arc<crate::session_actions::SessionActions>> {
        self.actions.get().cloned()
    }

    fn ask(&self, ask: ApiAsk) {
        self.asks.lock_recover().push(ask);
        (self.wake)();
    }

    /// What the session's one search finds by `needle` , answered on the loop's pass: the search
    /// lives on the thread holding the desktop.
    pub(crate) fn find(
        &self,
        needle: String,
        workspace: Option<String>,
        done: crate::search::FoundSearch,
    ) {
        self.ask(ApiAsk::Find {
            needle,
            workspace,
            done,
        });
    }

    /// Answer what the connections asked since the last pass, from `surfaces`,
    /// the `services` the session runs, and the one `search`; `shows` names
    /// the resources a place can show, as the layout holds them, and
    /// `person` is whose calls are their own, not someone's for them.
    ///
    /// The services are borrowed briefly, one ask at a time: a change they carry out is the
    /// published action's own, so a run never needs the services held across a turn of the
    /// loop, and no borrow outlives a single answer.
    pub(crate) fn answer(
        &self,
        surfaces: &tilo_session_core::surfaces::Surfaces,
        services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
        search: &crate::search::SessionSearch,
        shows: &std::rc::Rc<dyn Fn(&str) -> bool>,
        look: &mut tilo_session_core::surfaces::Look<'_>,
    ) {
        let asked = std::mem::take(&mut *self.asks.lock_recover());
        for ask in asked {
            match ask {
                ApiAsk::Find {
                    needle,
                    workspace,
                    done,
                } => {
                    // Asked, never waited for here: the sources answer on their own threads, and
                    // the loop takes the answer up on a later pass ([`crate::search::SessionSearch::poll_finds`]).
                    // A source that never answers cuts the find off at the caller's own deadline.
                    search.find_soon(
                        &needle,
                        workspace.as_deref(),
                        std::rc::Rc::clone(shows),
                        FIND_WITHIN,
                        done,
                    );
                }
                ApiAsk::Step {
                    actor,
                    address,
                    slot,
                    beside,
                    run,
                    done,
                } => match arguments_of(slot.as_deref()) {
                    // A step a service publishes runs there, on the record the service keeps —
                    // Chat's `record-work` on a turn's summons is the session's Chat service's,
                    // not the layout's. Every other step is the session's own services': the
                    // layout, the windows and the notices. Any resource a service publishes,
                    // at any depth: a note in a notebook the session keeps is the service's as
                    // its notebook is.
                    Ok(arguments)
                        if tilo_actions::api::resolve(&services.borrow().resources(), &address)
                            .is_some() =>
                    {
                        invoke_service_step(
                            services,
                            &actor,
                            run.as_ref(),
                            &address,
                            &arguments,
                            &beside,
                            done,
                        );
                    }
                    // The system's `open` stands a place beside the conversation of the turn that
                    // asked when it names none (`docs/designs/app-language/one-app-model.md` §3).
                    Ok(arguments) => done.complete(
                        surfaces
                            .invoke_as(
                                &actor,
                                &address,
                                &with_turn_beside(&address, &arguments, &beside),
                                look,
                            )
                            .map(ui::host_operation::ResourceOperationStart::completed)
                            .map_err(|error| ActionCallError::from_api(&error)),
                    ),
                    Err(why) => done.complete(Err(why)),
                },
                ApiAsk::Apps(done) => {
                    done.complete(session_packages(services));
                }
                ApiAsk::Reading {
                    app,
                    resource,
                    done,
                } => {
                    let services = services.borrow();
                    done.complete(
                        published_reading(&services, &app, &resource).map(|reading| Published {
                            kind: published_kind(&services, &resource),
                            reading,
                        }),
                    );
                }
                ApiAsk::Change {
                    actor,
                    change,
                    done,
                } => done.complete(change_through(services, &actor, &change)),
            }
        }
        // A step that ran here may have moved what the services publish: a follow of a reading
        // reads again, rather than waiting for a pass that publishes nothing new.
        self.readings_moved(services.borrow().revision());
    }
}

/// What the person's own steps answered, by invocation, for a later look.
type Answered =
    std::collections::VecDeque<(tilo_actions::InvocationId, tilo_actions::InvocationUpdate)>;

/// How many answered session steps a connection remembers.
const ANSWERED_KEPT: usize = 256;

/// The device's one dispatch, as this session's person, over the shell's link to its daemon: what
/// every call a connection makes outside the session's own steps is decided by, as the caller
/// that made it.
pub(crate) type Dispatch = runtime_collab::ShellDispatch;

/// The broker as an admitted client's API: every call passes the check as the agent or person
/// and workspace the connection was admitted for, and the session's own actions are among what
/// the same broker publishes.
#[derive(Clone)]
pub(crate) struct Host {
    /// The session's broker, once it starts one. Without it (a session in a window of another
    /// desktop) the session's own services are all it offers.
    state: Option<Arc<Dispatch>>,
    /// What a connection asks the session on its pass: the one search, the records a package
    /// the session runs itself keeps, and the person's own steps on its actions.
    session: Arc<SessionServices>,
    /// What the person's own steps answered, by invocation, for a later look.
    answered: Arc<std::sync::Mutex<Answered>>,
    /// The session's own actions, when this session serves them: a desktop with no broker
    /// lists them itself.
    session_actions: Option<Arc<crate::session_actions::SessionActions>>,
    /// The agent session an admitted agent's connection is: each step it takes says there what
    /// it is doing. `None` for the person's own connection.
    agent_session: Option<Arc<SessionAt>>,
}

/// What the catalogue says of one action a session was offered: the words the person reads the step
/// by ("Replace text at a place"), and the kind of step its own declaration names.
type NamedStep = (String, Option<tilo_actions::StepKind>);

/// The agent session one admitted connection is, and the apps of what it was offered.
struct SessionAt {
    sessions: Arc<AgentSessions>,
    session: AgentSessionId,
    /// What tells the loop the session moved.
    wake: Wake,
    /// The app each provider it was offered is of, by provider: what names the app a step of its
    /// is in.
    apps: std::sync::Mutex<HashMap<String, String>>,
    /// What the catalogue calls each action it was offered, by provider and action.
    named: std::sync::Mutex<HashMap<(String, String), NamedStep>>,
}

impl SessionAt {
    /// Most providers a session keeps the app of: a client that lists more is told its apps afresh.
    const APPS: usize = 4_096;

    /// The agent was offered `rows`.
    fn offered(&self, rows: &[tilo_actions::remote::CatalogueRow]) {
        let overflow = {
            let mut apps = self.apps.lock_recover();
            let overflow = apps.len() > Self::APPS;
            if overflow {
                apps.clear();
            }
            for row in rows {
                if !row.app_id.is_empty() {
                    apps.insert(row.provider.clone(), row.app_id.clone());
                }
            }
            overflow
        };
        let mut named = self.named.lock_recover();
        if overflow {
            named.clear();
        }
        for row in rows {
            if let Some(action) = &row.action {
                let words = [
                    action.action_label.trim(),
                    action.description.as_deref().unwrap_or_default().trim(),
                ]
                .into_iter()
                .find(|words| !words.is_empty());
                if let Some(words) = words {
                    named.insert(
                        (row.provider.clone(), action.address.action.clone()),
                        (words.to_owned(), action.contract.step),
                    );
                }
            }
        }
    }

    /// The agent takes the step `request` names: everyone in its workspace reads it beside its
    /// face, in the catalogue's own words for the step and on the record it acts in.
    fn acting(&self, request: &InvokeRequest) {
        let provider = &request.target.provider;
        let app = self.apps.lock_recover().get(provider).cloned();
        let (action, step) = self
            .named
            .lock_recover()
            .get(&(provider.clone(), request.target.address.action.clone()))
            .cloned()
            .unwrap_or_else(|| {
                (
                    tilo_actions::action_label(&request.target.address.action),
                    None,
                )
            });
        if self
            .sessions
            .acting(&self.session, Doing { action, step, app })
        {
            (self.wake)();
        }
    }
}

impl Host {
    /// The API over `state` , with `session` beside it.
    pub(crate) fn new(state: Option<Arc<Dispatch>>, session: Arc<SessionServices>) -> Self {
        Self {
            state,
            session,
            answered: Arc::default(),
            session_actions: None,
            agent_session: None,
        }
    }

    /// The same server, for the agent session `session` of `sessions`: what it does is said there,
    /// and `wake` tells the loop.
    fn in_agent_session(
        mut self,
        sessions: &Arc<AgentSessions>,
        session: AgentSessionId,
        wake: &Wake,
    ) -> Self {
        self.agent_session = Some(Arc::new(SessionAt {
            sessions: Arc::clone(sessions),
            session,
            wake: Arc::clone(wake),
            apps: std::sync::Mutex::default(),
            named: std::sync::Mutex::default(),
        }));
        self
    }

    /// The same API, listing and answering the session's own actions from `actions` when this
    /// desktop runs no broker.
    pub(crate) fn with_session_actions(
        mut self,
        actions: Option<Arc<crate::session_actions::SessionActions>>,
    ) -> Self {
        self.session_actions = actions;
        self
    }

    /// The answer to `request` for `caller`: the session's own step on its loop, else the device's
    /// dispatch's, which decides it as the caller itself (the person at this device as the person
    /// the session's link was made for).
    fn invoke_within(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // An app of a workspace this device is not admitted to yet takes part in nothing there:
        // asked of it, an action would wait for an app that never answers. It is refused now, with
        // the reason, for the person and an agent alike.
        if let Some(reason) = self.session.awaited_reason(&request.target.provider) {
            return Pending::ready(Err(ActionCallError::Unavailable { reason }));
        }
        // This device's own sockets, and a desktop with no broker at all, reach the session's own
        // actions on the loop: the broker is the check for a caller on another's word, and the
        // person at this device is nobody's delegate.
        let own = request.target.provider == wire_provider();
        let person = caller.acting().is_person();
        // An agent works in one workspace: a shell window of another is not its to read or act on,
        // whatever it names it by.
        if !person
            && let Some(workspace) = caller.workspace()
            && self
                .session
                .window_elsewhere(request.target.address.resource.as_str(), workspace)
        {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        // Nor may it branch another workspace: what it would copy is not its to read.
        if !person
            && let Some(workspace) = caller.workspace()
            && self.session.branches_elsewhere(&request, workspace)
        {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        // What the session offers people alone (ending an agent's session) is no agent's step: it
        // is refused at once, saying why, never put to the person on a sheet that stands over the
        // shell until they answer it.
        if !person
            && own
            && self
                .placed(&request)
                .is_some_and(|placed| !placed.action.contract.reaches_agents())
        {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "this action is for people only; ask the person to run it".to_owned(),
            }));
        }
        if !person && let Some(session) = &self.agent_session {
            session.acting(&request);
        }
        // The session's own steps run on the loop, where the layout and the windows are. A step
        // its own API publishes nothing of is a host step's — the person's own tilolang line,
        // which runs in the session's program host — and goes to the broker, which starts it,
        // stops it when the person cancels, and answers when it ends.
        if person && ((own && self.placed(&request).is_some()) || self.state.is_none()) {
            return self.invoke_person_step(caller, request);
        }
        // An agent's read of the session's own actions needs no turn and no durably signed
        // admission: it changes nothing, and the catalogue already held it to the caller's
        // grants. Its change is the turn's, and the broker decides it (shell.md §8.1).
        if own && self.reads(&request) {
            return self.invoke_read_step(caller, request);
        }
        // An ask of a workspace this session is founding on its own service lands once the
        // workspace is the service's: the ask waits for the founding, as one of a workspace this
        // device joined waits for its enrollment, and is delivered then. It is never refused for a
        // workspace this device is making, and never dropped.
        if let Some(workspace) = tilo_actions::remote::provider_workspace(&request.target.provider)
            .or_else(|| app_provider_workspace(&request.target.provider))
            && self.session.founding_here(workspace)
        {
            return self.session.hold_founding(
                workspace.to_owned(),
                caller.clone(),
                request,
                self.state.clone(),
            );
        }
        match &self.state {
            Some(dispatch) => dispatch.invoke(caller, request),
            None => Pending::ready(Err(ActionCallError::Unavailable {
                reason: "this desktop's apps are not running here".to_owned(),
            })),
        }
    }

    /// The session's own step `request` names, as its services publish it.
    fn placed(&self, request: &InvokeRequest) -> Option<tilo_actions::api::PlacedAction> {
        self.session_actions
            .as_ref()?
            .offered()
            .into_iter()
            .find(|placed| placed.action.address == request.target.address)
    }

    /// Whether the step `request` names reads what it acts on, as the session's own services
    /// describes it. A read changes nothing, so it needs neither a turn nor a durable admission.
    fn reads(&self, request: &InvokeRequest) -> bool {
        self.placed(request)
            .is_some_and(|placed| placed.action.kind == tilo_actions::OperationKind::View)
    }

    /// An agent's read of the session's own actions: handed to the loop, which is where they
    /// run, as the person's own step is, and answered with what it read.
    fn invoke_read_step(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Some(agent) = caller.acting().agent_id() else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        let actor = tilo_actions::MemberId::agent(agent).into();
        self.invoke_step_at(actor, request, caller.turn())
    }

    /// The session's own actions as rows, as the kind of publisher its own services are, for a
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

    /// A person's own step: handed to the loop, which is where the session's own services are,
    /// and answered as the services answer an invocation.
    fn invoke_person_step(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Some(person) = caller.acting().person_id() else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        let actor = tilo_actions::MemberId::person(person).into();
        self.invoke_step_at(actor, request, caller.turn())
    }

    /// The session's own action `request` names, carried out on the loop's pass as `actor` (the
    /// person at this device, or an agent reading): where the layout and the windows are.
    fn invoke_step_at(
        &self,
        actor: tilo_actions::Actor,
        request: InvokeRequest,
        turn: Option<&tilo_action_policy::turn::Turn>,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let slot = request.argument.to_slot();
        let invocation = request.invocation;
        // The place the ask is in: the turn's conversation, when the step is an agent's; else the
        // place in front. A place the step opens stands beside it
        // (`docs/designs/app-language/one-app-model.md` §3).
        let beside = service_steps::turn_beside(turn);
        let run = request.context.and_then(|context| context.group);
        let (done, ran) = tilo_actions::remote::pending();
        self.session
            .invoke_step(actor, request.target.address, slot, beside, run, done);
        let answered = Arc::clone(&self.answered);
        ran.map(move |ran| {
            use tilo_actions::remote::ReceiptStatus;
            let status = match ran {
                Ok(Some(output)) => ReceiptStatus::Completed { output },
                Ok(None) => ReceiptStatus::Accepted,
                Err(error) => ReceiptStatus::Rejected { error },
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

impl ActionProvider for Host {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        // An agent works in one workspace: the shell's windows of another (each listed by the title
        // of what it shows) are not its to read or act on. The person's own catalogue is theirs.
        let confined = if caller.acting().is_person() {
            None
        } else {
            caller.workspace().map(str::to_owned)
        };
        if let Some(workspace) = confined {
            let session = Arc::clone(&self.session);
            let agent_session = self.agent_session.clone();
            let page = match &self.state {
                Some(dispatch) => dispatch.query(caller, query),
                None => Pending::ready(Ok(CataloguePage::from_rows(
                    String::new(),
                    self.session_rows(),
                    &query,
                ))),
            };
            return page.map(move |page| {
                page.map(|mut page| {
                    // A step for people alone stays listed: asking for it is refused, saying why.
                    page.rows.retain(|row| {
                        !session.window_elsewhere(&row.resource.id, &workspace)
                            && !session.window_elsewhere(&row.parent_resource, &workspace)
                    });
                    if let Some(agent_session) = &agent_session {
                        agent_session.offered(&page.rows);
                    }
                    page
                })
            });
        }
        match &self.state {
            Some(dispatch) => dispatch.query(caller, query),
            // A desktop that runs no broker (a session in a window of another, a test with no
            // app wire) still publishes its own actions: they are read on the loop and
            // held here, so the services list them itself.
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
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // A person typing at a REPL is the person at this device: their own step is carried out on
        // the loop's pass, where the layout and the windows are, as their own input is. An agent's
        // step is the turn's, and the device's dispatch decides it exactly as it decides the
        // agent's app action in that turn. An admitted connection invokes as itself, and the
        // person's own steps on the session go to the loop wherever they come from
        // ([`Host::invoke_within`]).
        self.invoke_within(caller, request)
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
                .and_then(|dispatch| dispatch.observe(caller, id))
        })
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.state
            .as_ref()
            .and_then(|dispatch| dispatch.next_change(caller, id))
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        match &self.state {
            Some(dispatch) => dispatch.cancel(caller, id),
            None => Pending::ready(Err(ActionCallError::UnknownInvocation)),
        }
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        self.state
            .as_ref()
            .and_then(|dispatch| dispatch.content_puts())
    }

    /// The session's one search, the one the palette and text typed on the canvas run: what an
    /// agent's `find` answers with, asked on the loop's pass as the session's own services are.
    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        let session = Arc::clone(&self.session);
        Some(Arc::new(crate::search::SearchThrough::new(Arc::new(
            move |needle: String, workspace: Option<String>, done: crate::search::FoundSearch| {
                session.find(needle, workspace, done);
            },
        ))) as Arc<dyn tilo_actions::remote::ResourceSearch>)
    }

    /// What changed in the records of the workspace a connection works in, from this device's
    /// replica of them: what a watch of the whole workspace reads.
    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        Some(Arc::clone(&self.session.changes) as Arc<dyn tilo_actions::remote::ChangeFeed>)
    }

    /// The next write landing in this device's replica of any of its workspaces, which is what
    /// a watch of its records and of a resource's history reads. `None` for its roster: this
    /// host keeps none to say who is in the workspace, so a watch of presence holds until its
    /// own deadline.
    fn changed(
        &self,
        _caller: &Caller,
        scope: tilo_actions::remote::WatchScope,
    ) -> Option<tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>>>
    {
        match scope {
            tilo_actions::remote::WatchScope::Records => {
                self.session.changes.watching().map(|landed| landed.map(Ok))
            }
            tilo_actions::remote::WatchScope::Here => None,
        }
    }

    /// The records a connection reads outside their apps: the workspace's own, as the broker
    /// decides them, and the records of packages this session runs itself (the shell), which its
    /// services keep: a program reads them by the app, as it reads Notes', with no case of its own.
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        Some(Arc::new(self.service_records()))
    }

    /// The changes a connection makes to those records: a package the session runs itself is
    /// changed through its own services, and any other app's at its record's origin.
    fn resource_changes(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceChanges>> {
        Some(Arc::new(self.service_records()))
    }
}

impl Host {
    fn service_records(&self) -> ServiceRecords {
        ServiceRecords {
            state: self.state.as_ref().map(Arc::downgrade),
            session: Arc::clone(&self.session),
            agent_session: self
                .agent_session
                .as_ref()
                .map(|at| (Arc::clone(&at.sessions), at.session.clone())),
        }
    }
}

/// A reading of a resource the session publishes, and the kind it is published as (`None` for a
/// resource no service of this device serves, which no program reads: with no kind there is no
/// node to decide its read at).
pub(crate) struct Published {
    kind: Option<String>,
    reading: serde_json::Value,
}



/// The kind the session's services publish the resource `resource` as, when one serves it.
fn published_kind(services: &tilo_service::Services, resource: &str) -> Option<String> {
    services
        .resources()
        .into_iter()
        .find(|spec| spec.id.as_str() == resource)
        .map(|spec| spec.kind)
}

/// The packages the session runs itself (the shell), each its compiled module: what a task's
/// programs read those records by, answered on the loop's pass, where the services are.
fn session_packages(
    services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
) -> Vec<runtime_portal::process_link::App> {
    use runtime_portal::process_link::{App, Module};
    let services = services.borrow();
    services
        .apps()
        .iter()
        .filter_map(|app| services.module(app))
        .map(|wasm| {
            App::Module(Module {
                wasm: wasm.to_vec(),
                ids: String::new(),
                imports: Vec::new(),
            })
        })
        .collect()
}

/// The reading of the resource `resource` the app `app` publishes, asked by a program on the
/// loop's pass: what the session's services read it as. Only the resources the session itself
/// publishes are read here — an app's record an agent asks for is read through the record API,
/// never from a service. A resource no service of this device serves (no Bluetooth, no battery)
/// is published empty, as the shell's own parts are handed it: a program reads its fields as
/// absent (off, empty) and goes on; `None` for a resource the session does not publish at all.
fn published_reading(
    services: &tilo_service::Services,
    app: &str,
    resource: &str,
) -> Option<serde_json::Value> {
    published_root(app, resource).then(|| {
        services
            .read(&tilolang_tilo::platform_apps::on_device(
                resource,
                &tilo_app::framework::settings_device(),
            ))
            .unwrap_or_else(|| serde_json::json!({}))
    })
}

/// Whether the session publishes the resource `resource` as the app `app`'s: the device's,
/// Settings' and the session's own roots, each a resource the session's services read
/// ([`tilolang_tilo::platform_apps::platform_roots`]). A resource of any other app is its
/// record's, read through the record API.
fn published_root(app: &str, resource: &str) -> bool {
    tilolang_tilo::platform_apps::platform_roots()
        .iter()
        .any(|(published, ty)| *published == resource && ty.starts_with(&format!("{app}#")))
}

/// A client the server accepted: its socket, and where it came from.
struct Opened {
    stream: UnixStream,
    from: From,
}

/// Where a connection came from.
enum From {
    /// A program of this login, by the pid that opened it: how the server knows whether it is a
    /// program inside an app.
    Login(u32),
    /// A program inside a confined environment of this login, by its pid, at that environment's
    /// own socket ([`environments`]).
    Environment(u32, Arc<environments::Environment>),
    /// A process of this account the kernel hides from the session, by the pid that opened it:
    /// the login's shell runtime once it proves it holds the device identity
    /// ([`holds_device_identity`]), served its environments' sockets and its workspaces'
    /// terminals and nothing else.
    ShellRuntime(u32),
}

/// A client that left before asking needs no answer: the server lets it go
/// without one.
fn left_before_asking(error: &std::io::Error) {
    tilo_failure::ignored!(
        "a client that left before asking needs no answer",
        tracing::debug!(%error, "MCP server: the client left before its first request")
    );
}

/// Serve one connection of this account, on its own thread: read what it opens with, ask the
/// person, and only once they admit it answer what it asked. A refusal answers its `initialize`
/// with a JSON-RPC error and closes.
fn serve_connection(opened: Opened, serving: &Serving) {
    let Serving {
        broker,
        services: session,
        requests,
        launches,
        wake,
        control,
        sessions,
        ..
    } = serving;
    let broker = broker.clone();
    let Opened { mut stream, from } = opened;
    let first = match tilo_mcp::session_api::read_first_line(&mut stream) {
        Ok(first) => first,
        Err(error) => return left_before_asking(&error),
    };
    let (pid, entrance) = match from {
        From::ShellRuntime(pid) => {
            serve_shell_runtime(stream, &first, pid, serving);
            return;
        }
        From::Login(pid) => (pid, Origin::Host),
        From::Environment(pid, environment) => (pid, Origin::Environment(environment)),
    };
    let Some(stream) =
        environments::answer_socket_requests(stream, &first, pid, &entrance, serving)
    else {
        return;
    };
    if tilo_mcp::session_api::is_control_request(&first) {
        // Only a program inside an app this session started drives the control protocol: it acts as
        // that app, in the workspace the app was started into, and the broker's permissions decide
        // for it.
        let Some(app) = launches.app_of_descendant(pid) else {
            tracing::info!(
                pid,
                "MCP server: a control connection is outside this session's apps"
            );
            refuse_control(stream, CONTROL_IN_APP_ONLY);
            return;
        };
        let Some(workspace) = launches.workspace_of(pid, &app) else {
            tracing::info!(pid, %app, "MCP server: a control connection's app is not running");
            refuse_control(stream, CONTROL_IN_APP_ONLY);
            return;
        };
        tracing::info!(pid, %app, %workspace, "MCP server: a program in an app asked in the control protocol");
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
        &broker,
        session,
        (requests, wake, sessions),
    );
}

/// The login's shell runtime, by `pid`, which the kernel hides from the session: it asks, in
/// `first`, for an environment's socket or a workspace's terminal, proving it holds the device
/// identity, and is served nothing else.
fn serve_shell_runtime(stream: UnixStream, first: &[u8], pid: u32, serving: &Serving) {
    let key = tilo_secrets::device_session_key();
    let proven = |op: &str, proof: &Option<String>| {
        holds_device_identity(key.as_ref(), pid, op, proof.as_deref())
    };
    if let Some(request) = runtime_portal::environment_socket::Request::of(first)
        && proven(&request.op, &request.proof)
    {
        environments::open(stream, request, pid, serving.clone());
    } else if let Some(request) = runtime_portal::workspace_terminal::Request::of(first)
        && proven(&request.op, &request.proof)
    {
        crate::workspace_terminals::open(stream, request, serving.hosting.clone());
    } else {
        tracing::info!(
            pid,
            "MCP server: a hidden process proved no device identity for an environment's socket or a workspace's terminal"
        );
        refuse(stream, &PeerRefusal::Sandboxed);
    }
}

/// Ask the person about one connection whose first line, `first` , opened the MCP server, and serve
/// it as the agent they admit it as, through `broker`, every call it makes signed by that agent's
/// own key.
fn serve_admitted(
    stream: UnixStream,
    first: Vec<u8>,
    facts: Facts,
    broker: &Arc<Dispatch>,
    session: &Arc<SessionServices>,
    (requests, wake, sessions): (&Sender<Request>, &Wake, &Arc<AgentSessions>),
) {
    let tasks = person_tasks(session);
    let admitted = tasks.clone();
    // The agent the person admitted: its workspace, its person and the agent member it is a
    // session of, from the answer to the host the connection is then served through.
    let agent: Arc<std::sync::Mutex<Option<[String; 4]>>> = Arc::default();
    let starting = Arc::clone(&agent);
    let agent_session = sessions.joining(facts.entrance.entered());
    let session_stream = match stream.try_clone() {
        Ok(copy) => Some(copy),
        Err(error) => {
            tracing::warn!(%error, "an agent session has no handle on its connection");
            None
        }
    };
    let ended = tilo_mcp::session_api::serve_with(
        stream,
        Some(first),
        || {
            let Some([workspace, person, member, name]) = starting.lock_recover().take() else {
                return Err(word("mcp-server-not-started"));
            };
            let host = Host::new(Some(Arc::clone(broker)), Arc::clone(session))
                .with_session_actions(session.actions())
                .in_agent_session(sessions, agent_session.clone(), wake);
            let name = sessions.name(&agent_session).unwrap_or(name);
            // An agent with no key of its own acts on nothing: the client is told why rather than
            // refused action by action.
            let (signer, caller) = tilo_workspace_service::agents::agent_keys::this_device()
                .and_then(|keys| AgentSigner::grant(keys, &workspace, (&member, &name), &person))
                .map_err(|why| {
                    tracing::warn!(%workspace, agent = %name, %why, "MCP server: the agent has no key of its own here");
                    format!("{}: {why}", word("mcp-server-not-started"))
                })?;
            sessions.certified(&agent_session);
            Ok((
                Signed::new(host, Some(signer)),
                caller.in_agent_session(agent_session.as_str()),
            ))
        },
        tilo_mcp::session_api::Beside {
            tasks: tasks.map(|tasks| tasks as Arc<dyn tilo_mcp::TaskHosts>),
        },
        |hello, connected| {
            let consent = ask(
                requests,
                wake,
                facts,
                hello,
                connected,
                agent_session.clone(),
            );
            if let Consent::Admit { person, .. } = &consent
                && let Some(admitted) = &admitted
            {
                admitted.set_person(Some(person));
            }
            if let Consent::Admit {
                workspace,
                person,
                agent: member,
                name,
            } = &consent
            {
                *agent.lock_recover() = Some([
                    workspace.clone(),
                    person.clone(),
                    member.clone(),
                    name.clone(),
                ]);
            }
            if let Consent::Admit { .. } = &consent
                && let Some(stream) = &session_stream
            {
                sessions.attached(&agent_session, stream);
            }
            consent
        },
    );
    sessions.left(&agent_session);
    if let Err(error) = ended {
        tilo_failure::ignored!(
            "a client that disconnected needs no answer",
            tracing::debug!(%error, "MCP server: the client disconnected")
        );
    }
}

/// Where the programs of a task of the person's sitting here are done: over the session's own
/// catalogue.
pub(crate) fn person_tasks(
    session: &Arc<SessionServices>,
) -> Option<Arc<tilo_runtime::device::tasks::TaskHosts>> {
    session.task_hosts(Some(&session.person))
}

/// Hand one client to the loop's question and wait for the person's answer,
/// giving up when the client leaves.
fn ask(
    requests: &Sender<Request>,
    wake: &Wake,
    mut facts: Facts,
    hello: &Hello,
    connected: &dyn Fn() -> bool,
    session: AgentSessionId,
) -> Consent {
    facts.client.clone_from(&hello.client_name);
    facts.version.clone_from(&hello.client_version);
    let (decide, mut decided) = tokio::sync::oneshot::channel();
    let request = Request {
        facts,
        workspace: hello.workspace.clone(),
        key: hello.key.clone(),
        session_name: hello.session_name.clone(),
        session,
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
                return Consent::Refuse(word("mcp-server-withdrawn"));
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
        }
        if !connected() {
            // Dropping `decided` withdraws the question.
            return Consent::Refuse(word("mcp-server-withdrawn"));
        }
        std::thread::sleep(ASK_EVERY);
    }
}

/// The provider the session's own actions are published under.
fn wire_provider() -> &'static str {
    tilo_actions::remote::SESSION_ACTIONS_PROVIDER
}

/// The workspace an app's own provider names: `application:<workspace>:<app>…`, an app the shell
/// opened there, or `launch:<workspace>:<app>…`, one it would start there.
fn app_provider_workspace(provider: &str) -> Option<&str> {
    let rest = provider
        .strip_prefix("application:")
        .or_else(|| provider.strip_prefix("launch:"))?;
    let (workspace, _) = rest.split_once(':')?;
    (!workspace.is_empty()).then_some(workspace)
}

/// Whether a founding of `workspace` is under way on `view` now, for a provider naming it by id.
fn founding_of(view: &crate::workspace_view::WorkspaceView, workspace: &str) -> bool {
    tilo_desktop_model::WorkspaceId::parse(workspace).is_some_and(|id| view.is_founding(id))
}

/// Why nothing of `workspace` (its id) runs here yet: the shell holds it, but `view` says it has not been
/// admitted to it by its host ([`awaited_sentence`]). `None` once admitted, for a workspace the shell
/// does not hold, or one that is no workspace's id.
pub(crate) fn awaited_reason(
    view: &crate::workspace_view::WorkspaceView,
    handle: &DesktopHandle,
    workspace: &str,
) -> Option<String> {
    let id = tilo_desktop_model::WorkspaceId::parse(workspace)?;
    let host = view.awaited_host(id)?;
    let name = handle
        .read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.id() == id)
                .map(|held| tilo_session_core::desktop::names::workspace(&held.name))
        })
        .unwrap_or_else(|| workspace.to_owned());
    Some(awaited_sentence(&name, &host))
}

/// What a person and an agent are told of an app's action in the workspace `name` , which this
/// device holds but `host` has not admitted it to yet.
pub(crate) fn awaited_sentence(name: &str, host: &str) -> String {
    let host = if host.is_empty() { "its host" } else { host };
    format!(
        "this device hasn't been admitted to {name} yet: {host}, the workspace's host, must be \
         reachable once to take it in"
    )
}

#[cfg(test)]
mod sessions_in_the_shells_words {
    use super::environments::Origin;
    use super::{Consent, Facts, as_member};

    /// A connection is admitted under the session its own harness names, when its client names
    /// one: that is the name presence, notices and history show it by. A connection that names
    /// none keeps the name the workspace calls its agent member by.
    #[test]
    fn a_named_session_is_admitted_by_its_own_name() {
        let facts = Facts {
            client: "Claude Code".to_owned(),
            version: None,
            pid: std::process::id(),
            exe: None,
            app: None,
            entrance: Origin::Host,
        };
        let member =
            |_: &str, _: &str| Ok(("agent:claude-code".to_owned(), "Claude Code".to_owned()));
        let admitted = |named: Option<&str>| match as_member(
            &member,
            &facts,
            ("workspace", "person"),
            named,
        ) {
            Consent::Admit { name, .. } => name,
            Consent::Refuse(why) => panic!("refused: {why}"),
        };
        assert_eq!(admitted(Some("Office Hours")), "Office Hours");
        assert_eq!(
            admitted(Some("  ")),
            "Claude Code",
            "a blank name names nothing"
        );
        assert_eq!(admitted(None), "Claude Code");
    }
}

#[cfg(test)]
mod tests;
