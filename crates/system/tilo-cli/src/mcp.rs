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
//! does, and asks the host's own device, which serves every record of the
//! workspace to the person's devices, through that session
//! ([`SessionActions`]). Nothing is decided here: the host authenticates
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
/// after the connection's silence runs out.
const CLOSING: Duration = Duration::from_secs(3);

/// This device in the workspace: the one session every connection shares.
pub(crate) struct Session {
    device: Entered,
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
        Ok(Arc::new(Session { device: entered }))
    }
}

/// The device's one session, entered when the first connection needs it.
pub(crate) struct Shared<E> {
    enter: E,
    /// The person and workspace every run acts for, from the login.
    device: Result<Device, String>,
    session: tokio::sync::Mutex<Option<Arc<Session>>>,
}

impl<E: Enter> Shared<E> {
    fn new(enter: E, device: Result<Device, String>) -> Self {
        Self {
            enter,
            device,
            session: tokio::sync::Mutex::new(None),
        }
    }

    /// The device's session, entering the workspace when it is not in it
    /// yet; why not, when it cannot. `fresh` enters again in place of a
    /// session the person stopped, for a connection that starts after they
    /// did. A session that ended otherwise (the host stopped admitting it,
    /// and it waits to be asked again) is entered anew in its place.
    async fn session(&self, fresh: bool) -> Result<Arc<Session>, String> {
        let mut held = self.session.lock().await;
        if let Some(session) = held.as_ref() {
            if kept(session.device.is_live(), session.stopped().is_some(), fresh) {
                return Ok(Arc::clone(session));
            }
            // Its connection goes before another under the same key dials.
            session.device.close();
        }
        let session = self.enter.enter().await?;
        *held = Some(Arc::clone(&session));
        drop(held);
        Ok(session)
    }

    /// Leave the workspace as the last connection ends: the agent says it
    /// has left while its session can still say so, and the device closes.
    async fn leaving(&self) {
        let Some(session) = self.session.lock().await.take() else {
            return;
        };
        let peer = session.device.session().clone();
        drop(session);
        // The session closes on its own task: the process waits for it rather than ending it
        // with the runtime, which would leave the host waiting out the connection's silence.
        let closed = async {
            while peer.status().phase != tilo_session_client::TransportPhase::Closed {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let _ = tokio::time::timeout(CLOSING, closed).await;
    }
}

/// Whether a held session serves on rather than being entered anew: it is `live`, or the person
/// `stopped` its agent, which only a `fresh` connection enters again after.
fn kept(live: bool, stopped: bool, fresh: bool) -> bool {
    if stopped {
        return !fresh;
    }
    live
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
        let session = self.shared.session(held.is_none()).await?;
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
        // and more with `RUST_LOG` (`debug`, `tilo_fs=trace,warn`). Read as targets and
        // levels: a filter of regular expressions would link a regex engine into every
        // `tilo`, which `tilo mcp` holds in memory for as long as an agent runs.
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;
        let targets = std::env::var("RUST_LOG")
            .ok()
            .and_then(|filter| filter.parse::<tracing_subscriber::filter::Targets>().ok())
            .unwrap_or_else(|| {
                tracing_subscriber::filter::Targets::new()
                    .with_default(tracing_subscriber::filter::LevelFilter::WARN)
            });
        let _ = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_ansi(false),
            )
            .with(targets)
            .try_init();
        let home = tilo_paths::home().ok_or_else(|| anyhow::anyhow!("HOME is missing"))?;
        return held::hold(&place.map_err(anyhow::Error::msg)?, shared(device), home);
    }
    match place.and_then(|place| held::reach(&place).map(|stream| (stream, place))) {
        Ok((stream, place)) => held::relay(stream, &place),
        Err(why) => {
            // The agent keeps its tools: this connection holds a session of
            // its own, which another `tilo mcp` of this device would cut off.
            eprintln!("tilo mcp: {why} This agent's connection works on its own until then.");
            held::alone(&shared(device))
        }
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
    if request.get("method").and_then(Value::as_str) == Some(PEER_STATUS) {
        // Asked by `tilo status`: no agent run, and nothing entered for it.
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        return Some(
            json!({"jsonrpc": "2.0", "id": id, "result": peer_status(connection).await})
                .to_string(),
        );
    }
    if request.get("method").and_then(Value::as_str) == Some(PEER_LIST_AGAIN) {
        // Asked by `tilo folders add`, and after a workspace was made or
        // deleted: the person's workspaces are asked for again now, and the
        // folders are brought in line, rather than the ask waiting out how
        // long the peer keeps a list.
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        list_again(connection).await;
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
        Ok(served) => served.server.handle(request).await,
        Err(problem) => unreachable_answer(&request, problem),
    }?;
    Some(answer.to_string())
}

/// What `tilo status` asks this computer's peer: who it is, which workspaces it shows where, and
/// whether they are in sync.
pub(crate) const PEER_STATUS: &str = "tilo/peer";

/// What `tilo folders add` and the account commands ask the peer: ask the
/// person's host for their workspaces again now, and bring the folders in
/// line with the answer, answering [`PEER_STATUS`].
pub(crate) const PEER_LIST_AGAIN: &str = "tilo/peer/list-again";

/// Have the peer ask the host for the person's workspaces again, and show their
/// folders as the answer says: the workspace just made or deleted is one the
/// folders know at once, not at the peer's next list.
async fn list_again<E: Enter>(connection: &Connection<E>) {
    let Some(peer) = held::PEER.get() else {
        return;
    };
    let session = connection.shared.session.lock().await.clone();
    if let Some(session) = session {
        peer.refresh(&session.device).await;
    }
}

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

/// Ask the peer this computer runs to list the person's workspaces again now
/// and bring the folders in line, answering as [`ask_peer`]; `None` when no
/// peer runs.
pub(crate) fn ask_peer_list_again() -> Option<Value> {
    ask_peer_with(PEER_LIST_AGAIN)
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

    async fn ask(connection: &Connection<LoggedIn>, line: &str) -> Option<Value> {
        answer(connection, line)
            .await
            .map(|answer| serde_json::from_str(&answer).unwrap())
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
