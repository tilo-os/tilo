//! `tilo mcp`: the agent's MCP server over stdio. On a Tilo desktop it is
//! the desktop's own MCP server (`agent.sock`), which the person admits the
//! agent at; elsewhere it serves as the device `tilo login` made of this
//! computer.
//!
//! What it serves is the runtime's own projection of a workspace
//! ([`tilo_mcp::McpServer`], the agent surface: `tilo_ls`,
//! `tilo_read`, `tilo_do`, `tilo_find`, `tilo_status`), the same tools an
//! agent gets on a Tilo desktop. Its actions are the workspace's: this
//! device enters the person's workspace on its host as any device of theirs
//! does, and asks the server's own device, which serves every record of the
//! workspace to the person's devices, through that session
//! ([`SessionActions`]). Nothing is decided here: the server authenticates
//! this device and decides each request by the person's own policy, and
//! what an action changes reaches every device of the workspace as an
//! ordinary change.
//!
//! The host keeps one session per device: a second session of the same
//! device cuts the first off. Every agent on this computer runs its own
//! `tilo mcp` (each Claude Code window, `tilo agent check`), all as the one
//! device `tilo login` made, so one process holds the device's session and
//! every `tilo mcp` is a connection to it ([`held`]), as the API server
//! holds one session per key for all of that key's runs.
//!
//! Every connection is one agent run, named for the agent ("Claude Code"),
//! which History groups its actions under; the workspace shows the agent
//! present with what it does while it runs.

pub(crate) mod held;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_session_client::device::Device as Entered;
use tilo_session_client::remote::SessionActions;

use crate::login::{Device, NOT_LOGGED_IN};

/// The MCP protocol version this server speaks when it cannot reach the
/// workspace; the projection answers with its own once it can.
const PROTOCOL: &str = "2025-06-18";
/// How long a request waits for this device to enter the workspace.
const ENTERING: Duration = Duration::from_secs(25);
/// What the host's session says once the person stopped this agent from its card.
const STOPPED: &str = "agent-run-stopped";
/// The longest the agent waits, as it goes, for its session to close: the host
/// then sees the connection end at once, and takes the agent down, rather than
/// after the connection's silence ends. A session's endpoint gives its packet relay up to
/// three seconds to hear it go, and a session entered anew under the same key waits that out.
const CLOSING: Duration = Duration::from_secs(5);

/// This device in the workspace: the one session every connection shares.
pub(crate) struct Session {
    device: Entered,
    /// Tilo's API as this computer's key, over this one session: what the
    /// agents' account and people calls are answered by, without a session
    /// of their own that would cut this one off. `None` when the login's key
    /// does not read as one, and the workspace's own calls still serve.
    api: Option<Arc<tilo_api::direct::Direct>>,
}

impl Session {
    /// Why the session serves no more, once it does not: the person stopped
    /// the agent from its card.
    fn stopped(&self) -> Option<String> {
        self.device
            .session()
            .status()
            .failures
            .into_iter()
            .find(|failure| failure.code == STOPPED)
            .map(|failure| {
                format!(
                    "{} Start a new conversation with your agent to go on.",
                    failure.message
                )
            })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The agent's run ends with the session, and the host that shows it
        // takes it down.
        self.device.close();
    }
}

/// What enters the workspace: the device `tilo login` made, entered once
/// for every connection ([`Shared::session`]).
pub(crate) trait Enter: Send + Sync + 'static {
    /// Enter the workspace as this computer's device.
    fn enter(&self) -> impl std::future::Future<Output = Result<Arc<Session>, String>> + Send;
}

/// This computer's device, as `tilo login` left it, or why there is none to
/// enter with.
struct LoggedIn(Result<(SigningKey, Device), String>);

impl Enter for LoggedIn {
    async fn enter(&self) -> Result<Arc<Session>, String> {
        let (key, device) = self.0.as_ref().map_err(Clone::clone)?;
        let entered = tokio::time::timeout(ENTERING, crate::login::connect(key, device))
            .await
            .map_err(|_| {
                format!(
                    "Tilo couldn't reach the workspace on {} within {} seconds; it is offline, \
                     or this computer's connection is down. Try again in a minute.",
                    crate::login::host_of(&device.host_origin),
                    ENTERING.as_secs()
                )
            })?
            .map_err(|error| {
                crate::login::debug_why("entering the workspace", &error);
                crate::login::enter_said(&error)
            })?;
        let api = tilo_api::client::login_key(None)
            .ok()
            .flatten()
            .and_then(|file| {
                tilo_api::direct::Direct::over(&file, entered.clone())
                    .map_err(|why| tracing::warn!(%why, "Tilo's API is not served here"))
                    .ok()
            })
            .map(Arc::new);
        Ok(Arc::new(Session {
            device: entered,
            api,
        }))
    }
}

/// The device's one session, entered when the first connection needs it.
pub(crate) struct Shared<E> {
    enter: E,
    /// The person and workspace every run acts for, from the login.
    device: Result<Device, String>,
    session: tokio::sync::Mutex<Option<Arc<Session>>>,
    /// The one entry attempt in flight, and what the last one answered. Never held across
    /// [`Enter::enter`]: a call would then queue behind every retry the background sync makes,
    /// each up to [`ENTERING`], and calls that came while the host refused this computer took
    /// as long as the retries happened to last.
    entry: tokio::sync::Mutex<Entry>,
    /// Counts finished attempts, so a call that waits for one hears that it ended.
    attempts: tokio::sync::watch::Sender<u64>,
    /// The program this holder was started from, once it holds the device ([`held::Program`]).
    held_as: std::sync::OnceLock<held::Program>,
    /// Told when a holder of a newer program asked this one to let it hold the device.
    stepping_down: tokio::sync::Notify,
}

#[derive(Default)]
struct Entry {
    /// Why the last attempt could not enter, until one does.
    failed: Option<String>,
    /// Whether an attempt is in flight.
    running: bool,
}

/// Which call is asking for the session: a connection's own call, which may wait for the one
/// attempt already under way, or the background sync's retry, which never waits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ask {
    /// An agent's call: it waits for the attempt that was already started when it asked, so a
    /// holder that is still entering serves it rather than refusing it.
    Agent,
    /// The background retry: it starts an attempt when none runs and otherwise leaves it be.
    Sync,
}

impl<E: Enter> Shared<E> {
    fn new(enter: E, device: Result<Device, String>) -> Self {
        Self {
            enter,
            device,
            session: tokio::sync::Mutex::new(None),
            entry: tokio::sync::Mutex::new(Entry::default()),
            attempts: tokio::sync::watch::channel(0).0,
            held_as: std::sync::OnceLock::new(),
            stepping_down: tokio::sync::Notify::new(),
        }
    }

    /// The device's session, entering the workspace when it is not in it
    /// yet; why not, when it cannot. `fresh` enters again in place of a
    /// session the person stopped, for a connection that starts after they
    /// did. A session that ended otherwise (the host stopped admitting it,
    /// and it waits to be asked again) is entered anew in its place.
    ///
    /// A call never queues behind entry retries: once an attempt failed, every call answers why
    /// at once, from what the device holds of the workspace, and the next attempt is the
    /// background sync's. Only the first attempt, and one a call finds already under way, are
    /// waited for.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the attempt is claimed under the same lock that reads the last failure, so two \
                  calls never both start one"
    )]
    async fn session(&self, fresh: bool, ask: Ask) -> Result<Arc<Session>, String> {
        if let Some(session) = self.held(fresh).await {
            return Ok(session);
        }
        let mut entry = self.entry.lock().await;
        if let Some(why) = entry.failed.clone()
            && ask == Ask::Agent
        {
            return Err(not_in(&why));
        }
        let mut ended = self.attempts.subscribe();
        // The count is read under the same lock that says whether an attempt runs, so the wait
        // below cannot miss the attempt's end.
        ended.borrow_and_update();
        if entry.running {
            if ask == Ask::Sync {
                return Err(not_in("entering the workspace"));
            }
            drop(entry);
            let _ = ended.changed().await;
            if let Some(session) = self.held(fresh).await {
                return Ok(session);
            }
            let entry = self.entry.lock().await;
            return Err(not_in(
                entry
                    .failed
                    .as_deref()
                    .unwrap_or("the session ended before it served"),
            ));
        }
        entry.running = true;
        drop(entry);
        let entered = self.enter.enter().await;
        let mut entry = self.entry.lock().await;
        entry.running = false;
        match entered {
            Ok(session) => {
                entry.failed = None;
                *self.session.lock().await = Some(Arc::clone(&session));
                self.attempts
                    .send_modify(|ended| *ended = ended.wrapping_add(1));
                Ok(session)
            }
            Err(why) => {
                entry.failed = Some(why.clone());
                self.attempts
                    .send_modify(|ended| *ended = ended.wrapping_add(1));
                Err(why)
            }
        }
    }

    /// The session this device holds, while it `kept`s serving for `fresh`.
    async fn held(&self, fresh: bool) -> Option<Arc<Session>> {
        let mut held = self.session.lock().await;
        let session = held.as_ref()?;
        if kept(session.device.is_live(), session.stopped().is_some(), fresh) {
            return Some(Arc::clone(session));
        }
        // Its connection goes before another under the same key dials: a server keeps one
        // connection per key, so the two would cut each other off, and the device would stay out
        // of the workspace for as long as both went on dialing.
        let peer = session.device.session().clone();
        *held = None;
        drop(held);
        peer.close();
        closed(&peer).await;
        None
    }

    /// Leave the workspace as the last connection ends: the agent says it
    /// has left while its session can still say so, and the device closes.
    async fn leaving(&self) {
        let Some(session) = self.session.lock().await.take() else {
            return;
        };
        let peer = session.device.session().clone();
        drop(session);
        closed(&peer).await;
    }
}

/// Once `peer`'s session closed, for at most [`CLOSING`]. The session closes on its own task: the
/// process waits for it rather than ending it with the runtime, which would leave the server waiting
/// out the connection's silence, and a new session under the same key waits for it rather than
/// being cut off by it.
async fn closed(peer: &tilo_session_client::Link) {
    let closed = async {
        while peer.status().phase != tilo_session_client::TransportPhase::Closed {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let _ = tokio::time::timeout(CLOSING, closed).await;
}

/// Whether a held session serves on rather than being entered anew: it is `live`, or the person
/// `stopped` its agent, which only a `fresh` connection enters again after.
fn kept(live: bool, stopped: bool, fresh: bool) -> bool {
    if stopped {
        return !fresh;
    }
    live
}

/// Why a call answers nothing while this computer is not in the workspace: the reason, and that
/// the background sync keeps trying, so the agent asks again rather than dropping its tools.
fn not_in(why: &str) -> String {
    format!(
        "This computer isn't in the workspace right now ({why}). Tilo keeps trying in the \
         background; try again in a minute."
    )
}

/// One agent connection: its own run over the device's session.
pub(crate) struct Connection<E> {
    shared: Arc<Shared<E>>,
    /// The agent run every invocation of this connection belongs to.
    run: String,
    served: tokio::sync::Mutex<Option<Arc<Served>>>,
}

/// The projection a connection serves, over the session it opened on. It holds that session
/// weakly: the device's one session is [`Shared`]'s, and a session [`Shared`] let go of (it ended,
/// and the device entered anew) goes then, not with the last agent connection that used it.
struct Served {
    session: std::sync::Weak<Session>,
    server: McpServer<SessionActions>,
}

impl Served {
    /// The session this serves over, while it is the device's and serves; or the one the person
    /// stopped, which every call answers with why.
    fn current(&self) -> Option<Arc<Session>> {
        self.session
            .upgrade()
            .filter(|session| session.device.is_live() || session.stopped().is_some())
    }
}

impl<E: Enter> Connection<E> {
    pub(crate) fn new(shared: Arc<Shared<E>>) -> Self {
        Self {
            shared,
            run: hex::encode(rand::random::<[u8; 8]>()),
            served: tokio::sync::Mutex::new(None),
        }
    }

    /// What this connection serves, and the session it serves over, once the device is in the
    /// workspace. A connection whose session ended (the host stopped admitting it, or the
    /// computer slept through it) serves on over the one the device entered in its place, as the
    /// same agent run.
    async fn served(&self) -> Result<(Arc<Served>, Arc<Session>), String> {
        let mut held = self.served.lock().await;
        if let Some(served) = held.as_ref()
            && let Some(session) = served.current()
        {
            return Ok((Arc::clone(served), session));
        }
        // Only a connection that starts enters again in place of a session the person stopped.
        let session = self.shared.session(held.is_none(), Ask::Agent).await?;
        let device = self.shared.device.as_ref().map_err(Clone::clone)?;
        let served = Arc::new(open(&session, device, &self.run)?);
        *held = Some(Arc::clone(&served));
        drop(held);
        Ok((served, session))
    }
}

/// The projection over the workspace `session` is in, its actions asked of
/// the workspace's host.
fn open(session: &Arc<Session>, device: &Device, run: &str) -> Result<Served, String> {
    let workspace_id = session.device.workspace_id();
    let user: [u8; 32] = hex::decode(&device.user)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(crate::login::LOGIN_DAMAGED)?;
    let actions = session.device.actions(workspace_id);
    let person = hex::encode(user);
    // Every session of this agent, on any computer of the person's, is the one agent the host
    // and the workspace's record of its agents know it by.
    let agent = tilo_actions::remote::device_agent_id(&user, &device.agent);
    // The apps the workspace's host lists: what this connection's task programs compile against
    // there, so `tilo run` works where the workspace is hosted elsewhere.
    let cert = session.device.cert();
    let catalogue = tilo_mcp::task_hosts::WorkspaceCatalogue::new(
        actions.clone(),
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key),
    );
    let server = McpServer::for_agent(actions, device.workspace.clone(), &agent, &person)
        .with_agent_session(run, device.agent.clone())
        .with_task_hosts(Some(Arc::new(tilo_mcp::task_hosts::TaskHosts::new(
            Some(&person),
            Arc::new(catalogue),
        ))));
    Ok(Served {
        session: Arc::downgrade(session),
        server,
    })
}

/// `tilo mcp`: on a Tilo desktop, the desktop's own MCP server, which asks the person to let the
/// agent in; elsewhere, this computer's login, entering the person's workspace wherever it is
/// hosted, as a connection to the process holding that device's session ([`held`]), started when
/// none runs. `hold` is that process (`tilo mcp --hold`, which a connection starts).
pub fn serve(hold: bool) -> Result<ExitCode> {
    let device = match tilo_mcp::session_api::desktop_server() {
        Ok(Some(server)) => {
            tilo_mcp::session_api::bridge(&server)?;
            return Ok(ExitCode::SUCCESS);
        }
        Ok(None) => crate::login::load(&crate::login::device_dir()?)?
            .ok_or_else(|| NOT_LOGGED_IN.to_owned()),
        // A desktop whose server is not open yet (its first login is not complete) is no computer
        // that needs a login: the agent hears what it waits for.
        Err(error) => Err(error.to_string()),
    };
    // The agent's connection goes where `tilo login` went: a host whose
    // certificate only a test trusts is reached the same way here.
    crate::login::trust_test_certificates()?;
    let Ok((key, _)) = &device else {
        // Nothing to share: every call says why there is no workspace.
        return held::alone(&shared(device));
    };
    let place = held::Place::of(
        &crate::login::device_dir()?,
        &key.verifying_key().to_bytes(),
    );
    if hold {
        // The peer's log (its standard error, kept beside the device): what it could not do,
        // and more with `RUST_LOG` (`debug`, `tilo_fs=trace,warn`).
        crate::log_to_stderr(tracing_subscriber::filter::LevelFilter::WARN);
        let home = tilo_paths::home().ok_or_else(|| anyhow::anyhow!("HOME is missing"))?;
        return held::hold(&place.map_err(anyhow::Error::msg)?, shared(device), home);
    }
    let reached = place
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|place| held::reach(place).map(|stream| (stream, place.clone())));
    match reached {
        Ok((stream, place)) => held::relay(stream, &place),
        Err(why) => {
            // This connection starts no session of its own. The device has one live connection,
            // its holder's: a session here would bind the device's endpoint too, take the
            // holder's place at the server and then be taken by the holder's, so neither serves
            // and every call fails with "another endpoint connected with the same endpoint id".
            // The holder is started again in the background; every call says why until it is.
            eprintln!(
                "tilo mcp: {why} This connection waits for the device's holder, which is started \
                 again in the background; it holds no session of its own."
            );
            if let Ok(place) = place {
                std::thread::spawn(move || {
                    loop {
                        let _ = held::ensure(&place);
                        if held::connect(&place).is_some() {
                            return;
                        }
                        std::thread::sleep(Duration::from_secs(3));
                    }
                });
            }
            held::alone(&unreached(&why, &device))
        }
    }
}

/// Start the process holding this computer's device session when none is up, so the workspace
/// commands acting as the device reach it: they are calls to its API ([`held`]), never a
/// session of their own, which would cut that one off. Nothing, when this computer did not log
/// in.
///
/// # Errors
/// The login is damaged, or the holder did not start.
pub(crate) fn ensure_held() -> Result<()> {
    let dir = crate::login::device_dir()?;
    let Some((key, _)) = crate::login::load(&dir)? else {
        return Ok(());
    };
    let place =
        held::Place::of(&dir, &key.verifying_key().to_bytes()).map_err(anyhow::Error::msg)?;
    held::ensure(&place).map_err(anyhow::Error::msg)
}

/// A connection with no holder to reach: it holds no session, so every call says why, while
/// the background starts the device's one holder again.
fn unreached(why: &str, device: &Result<(SigningKey, Device), String>) -> Arc<Shared<NoSession>> {
    let login = device
        .as_ref()
        .map(|(_, device)| device.clone())
        .map_err(Clone::clone);
    Arc::new(Shared::new(NoSession(not_in(why)), login))
}

/// An entry that never enters: this connection has no session by design.
struct NoSession(String);

impl Enter for NoSession {
    async fn enter(&self) -> Result<Arc<Session>, String> {
        Err(self.0.clone())
    }
}

fn shared(device: Result<(SigningKey, Device), String>) -> Arc<Shared<LoggedIn>> {
    let login = device
        .as_ref()
        .map(|(_, device)| device.clone())
        .map_err(Clone::clone);
    Arc::new(Shared::new(LoggedIn(device), login))
}

/// The answer to one JSON-RPC line on `connection`, or `None` for a
/// notification.
async fn answer<E: Enter>(connection: &Connection<E>, line: &str) -> Option<String> {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": error.to_string()}})
                    .to_string(),
            );
        }
    };
    if request.get("method").and_then(Value::as_str) == Some(STEP_DOWN) {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let newer = request
            .get("params")
            .and_then(|params| params.get("program"))
            .and_then(|program| serde_json::from_value::<held::Program>(program.clone()).ok());
        let steps_down = connection
            .shared
            .held_as
            .get()
            .zip(newer)
            .is_some_and(|(own, newer)| newer.replaces(own));
        if steps_down {
            connection.shared.stepping_down.notify_one();
        }
        return Some(
            json!({"jsonrpc": "2.0", "id": id, "result": {"stepping_down": steps_down, "pid": std::process::id()}})
                .to_string(),
        );
    }
    if request.get("method").and_then(Value::as_str) == Some(PEER_STATUS) {
        // Asked by `tilo status`: no agent run, and nothing entered for it.
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        return Some(
            json!({"jsonrpc": "2.0", "id": id, "result": peer_status(connection).await})
                .to_string(),
        );
    }
    let served = connection.served().await.and_then(|(served, session)| {
        // The person stopped this agent: its session ended for good, and every call says so.
        match session.stopped() {
            Some(stopped) => Err(stopped),
            None => Ok(served),
        }
    });
    let answer = match &served {
        Ok(served) => match account_or_people(request, connection) {
            Ok(call) => match served.current().and_then(|session| session.api.clone()) {
                Some(api) => Some(api.mcp(call.clone()).await.unwrap_or_else(|problem| {
                    unreachable_answer(&call, &problem).unwrap_or(Value::Null)
                })),
                None => unreachable_answer(
                    &call,
                    "this computer's login does not reach the account here; log in again",
                ),
            },
            Err(request) => served.server.handle(request).await,
        },
        Err(problem) => unreachable_answer(&request, problem),
    }?;
    Some(answer.to_string())
}

/// `request` when it is a tool call on the person's account (`account/…`:
/// their workspaces and keys) or a workspace's people
/// (`workspace/members#…`), which Tilo's API answers as for any key of
/// theirs, with the people's calls in the workspace this computer entered
/// unless they name another; any other request back, for the workspace's own
/// projection.
fn account_or_people<E: Enter>(
    mut request: Value,
    connection: &Connection<E>,
) -> Result<Value, Value> {
    let tool = request.pointer("/params/name").and_then(Value::as_str);
    if !matches!(tool, Some("tilo_ls" | "tilo_read" | "tilo_do")) {
        return Err(request);
    }
    let path = request
        .pointer("/params/arguments/path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let account = path.split(['/', '#']).next() == Some("account");
    let people = path.split('#').next() == Some("workspace/members")
        || path.starts_with("workspace/members/");
    if !account && !people {
        return Err(request);
    }
    if people
        && let Ok(device) = &connection.shared.device
        && let Some(arguments) = request
            .pointer_mut("/params/arguments")
            .and_then(Value::as_object_mut)
    {
        arguments
            .entry("workspace")
            .or_insert_with(|| json!(device.workspace));
    }
    Ok(request)
}

/// What `tilo status` asks this computer's peer: who it is, which workspaces it shows where, and
/// whether they are in sync.
pub(crate) const PEER_STATUS: &str = "tilo/peer";

/// What a holder starting from a newer program asks the one holding the device: to let it hold the
/// device instead, with the program it was started from ([`held::Program`]).
pub(crate) const STEP_DOWN: &str = "tilo/stepDown";

/// The peer's answer to [`PEER_STATUS`].
async fn peer_status<E: Enter>(connection: &Connection<E>) -> Value {
    let device = connection.shared.device.as_ref().ok();
    let session = connection.shared.session.lock().await.clone();
    let phase = session.as_ref().map_or_else(
        || "entering".to_owned(),
        |session| format!("{:?}", session.device.session().status().phase).to_lowercase(),
    );
    let (mounted, failed, workspaces, complete) = match held::PEER.get() {
        Some(peer) => {
            let (workspaces, complete) = peer.workspaces().await;
            (peer.mounted().await, peer.failures(), workspaces, complete)
        }
        None => (Vec::new(), Vec::new(), Vec::new(), false),
    };
    let not_in = held::NOT_IN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    json!({
        "pid": std::process::id(),
        "device": device.map(|device| &device.label),
        "person": device.map(|device| &device.user),
        "host": device.map(|device| &device.host_origin),
        "session": phase,
        "not_in": not_in,
        "mounted": mounted,
        "workspaces": workspaces,
        "workspaces_complete": complete,
        "failed": failed.into_iter().map(|(workspace, why)| json!({"workspace": workspace, "why": why})).collect::<Vec<_>>(),
    })
}

/// Ask the peer this computer runs for its [`PEER_STATUS`]; `None` when none runs (it is not
/// started for asking).
pub(crate) fn ask_peer() -> Option<Value> {
    ask_peer_with(PEER_STATUS)
}

/// The peer's answer to `method`, a request of its control path; `None` when
/// no peer runs.
fn ask_peer_with(method: &str) -> Option<Value> {
    use std::io::{BufRead as _, Write as _};
    let (key, _) = crate::login::load(&crate::login::device_dir().ok()?).ok()??;
    let place = held::Place::of(
        &crate::login::device_dir().ok()?,
        &key.verifying_key().to_bytes(),
    )
    .ok()?;
    let mut stream = held::connect(&place)?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    writeln!(
        stream,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": method})
    )
    .ok()?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line).ok()?;
    let answer: Value = serde_json::from_str(&line).ok()?;
    answer.get("result").cloned()
}

/// What the server answers while the workspace cannot be reached: who it is,
/// no tools, and why for any call.
fn unreachable_answer(request: &Value, problem: &str) -> Option<Value> {
    let id = request.get("id").cloned()?;
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "initialize" => json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "tilo", "version": env!("CARGO_PKG_VERSION")},
            "instructions": problem,
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": []}),
        "tools/call" => json!({"content": [{"type": "text", "text": problem}], "isError": true}),
        _ => {
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("no method {method}")}}));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection_saying(why: &str) -> Connection<LoggedIn> {
        Connection::new(Arc::new(Shared::new(
            LoggedIn(Err(why.to_owned())),
            Err(why.to_owned()),
        )))
    }

    fn connection() -> Connection<LoggedIn> {
        connection_saying(NOT_LOGGED_IN)
    }

    async fn ask<E: Enter>(connection: &Connection<E>, line: &str) -> Option<Value> {
        answer(connection, line)
            .await
            .map(|answer| serde_json::from_str(&answer).unwrap())
    }

    /// A device whose entry the host refuses, each attempt taking `taking`: how many attempts
    /// were started, and the reason every one ended with.
    struct SlowEntry {
        attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        taking: Duration,
    }

    impl Enter for std::sync::Arc<SlowEntry> {
        async fn enter(&self) -> Result<Arc<Session>, String> {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(self.taking).await;
            Err("the host refused this computer's entry".to_owned())
        }
    }

    /// A connection that reached no holder holds no session of its own — the device has one
    /// live connection — and every call answers why while the holder is started again.
    #[tokio::test]
    async fn a_connection_that_reached_no_holder_answers_why_and_holds_no_session() {
        let connection = Connection::new(unreached(
            "Tilo's background sync isn't answering on this computer.",
            &Err("unused".into()),
        ));
        let called = ask(
            &connection,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tilo_ls"}}"#,
        )
        .await
        .unwrap();
        assert_eq!(called["result"]["isError"], true);
        let said = called["result"]["content"][0]["text"].as_str().unwrap();
        assert!(said.contains("keeps trying in the background"), "{said}");
    }

    /// Two agents on a computer whose host refuses its entry: the first call may wait for the
    /// attempt under way, and every call after it answers at once with why, instead of queueing
    /// behind the background sync's retries and taking as long as they happen to last.
    #[tokio::test]
    async fn an_agents_call_never_queues_behind_entry_retries() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let enter = std::sync::Arc::new(SlowEntry {
            attempts: std::sync::Arc::clone(&attempts),
            taking: Duration::from_millis(300),
        });
        let shared = std::sync::Arc::new(Shared::new(
            std::sync::Arc::clone(&enter),
            Err("unused".into()),
        ));
        // The holder enters as it starts: a call arriving now waits for that attempt, so a host
        // that answers serves it.
        let starting = tokio::spawn({
            let shared = std::sync::Arc::clone(&shared);
            async move { shared.session(false, Ask::Sync).await.map(drop) }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let said = shared
            .session(true, Ask::Agent)
            .await
            .err()
            .expect("the entry refused this computer");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "one attempt, not one per caller"
        );
        assert!(
            said.contains("the host refused this computer's entry"),
            "{said}"
        );
        assert!(starting.await.unwrap().is_err());
        // A retry runs; the next agent's call does not wait for it.
        let retrying = tokio::spawn({
            let shared = std::sync::Arc::clone(&shared);
            async move { shared.session(false, Ask::Sync).await.map(drop) }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let started = std::time::Instant::now();
        let said = shared
            .session(true, Ask::Agent)
            .await
            .err()
            .expect("the entry refused this computer");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "an agent's call waited {} ms behind a retry",
            started.elapsed().as_millis()
        );
        assert!(
            said.contains("This computer isn't in the workspace right now"),
            "{said}"
        );
        assert!(said.contains("keeps trying in the background"), "{said}");
        assert!(retrying.await.unwrap().map(drop).is_err());
        // Nor does it start one of its own: the retries are the background sync's.
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(shared.session(true, Ask::Agent).await.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_session_that_ended_is_entered_anew_and_a_stopped_one_only_for_a_new_connection() {
        // Live: it serves every caller.
        assert!(kept(true, false, false));
        assert!(kept(true, false, true));
        // Ended (the host did not admit it, and it waits to be asked again): the holder's
        // next look enters the workspace anew rather than holding a dead session for good.
        assert!(!kept(false, false, false));
        assert!(!kept(false, false, true));
        // The person stopped the agent: the holder keeps that, and a new conversation
        // enters again.
        assert!(kept(false, true, false));
        assert!(!kept(false, true, true));
    }

    #[tokio::test]
    async fn without_a_login_the_server_says_how_to_log_in_and_offers_nothing() {
        let connection = connection();
        let init = ask(
            &connection,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "tilo");
        assert!(
            init["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("tilo login")
        );
        assert!(
            ask(
                &connection,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .await
            .is_none()
        );
        let listed = ask(
            &connection,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        )
        .await
        .unwrap();
        assert_eq!(listed["result"]["tools"], json!([]));
        let called = ask(
            &connection,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tilo_ls"}}"#,
        )
        .await
        .unwrap();
        assert_eq!(called["result"]["isError"], true);
    }

    /// A Tilo desktop whose server is not open yet (its first login is not complete) is no computer
    /// that needs `tilo login`: the agent hears what it waits for, never "not logged in".
    #[tokio::test]
    async fn a_desktop_whose_server_is_not_open_yet_never_says_not_logged_in() {
        let waiting = tilo_mcp::session_api::route(
            false,
            None,
            true,
            Some("/run/user/1000/tilo-session/agent.sock".into()),
            |_| false,
        )
        .unwrap_err()
        .to_string();
        let connection = connection_saying(&waiting);
        let init = ask(
            &connection,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .await
        .unwrap();
        let said = init["result"]["instructions"].as_str().unwrap();
        assert_eq!(said, waiting);
        assert!(!said.contains("not logged in"), "{said}");
        assert!(said.contains("first login"), "{said}");
    }
}
