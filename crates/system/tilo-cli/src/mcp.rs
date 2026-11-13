//! `tilo mcp`: the agent's MCP server over stdio. On a Tilo desktop it is
//! the desktop's own agent door (`agent.sock`), which the person admits the
//! agent at; elsewhere it serves as the device `tilo login` made of this
//! computer.
//!
//! What it serves is the runtime's own projection of a workspace
//! ([`tilo_runtime::mcp::McpServer`], the agent surface: `tilo_ls`,
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

mod held;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_session_client::device::Device as Entered;
use tilo_session_core::remote::SessionActions;

use crate::login::{Device, NOT_LOGGED_IN};
use tilo_runtime::mcp_windows;

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
            .map(|failure| format!("{} Start a new session to go on.", failure.message))
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
                    "Tilo could not enter your workspace on {} within {} seconds; try again.",
                    crate::login::host_of(&device.host_origin),
                    ENTERING.as_secs()
                )
            })?
            .map_err(|error| format!("Tilo could not enter your workspace: {error:#}"))?;
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
    /// did.
    async fn session(&self, fresh: bool) -> Result<Arc<Session>, String> {
        let mut held = self.session.lock().await;
        if let Some(session) = held.as_ref()
            && !(fresh && session.stopped().is_some())
        {
            return Ok(Arc::clone(session));
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

/// One agent connection: its own run over the device's session.
pub(crate) struct Connection<E> {
    shared: Arc<Shared<E>>,
    /// The agent run every invocation of this connection belongs to.
    run: String,
    served: tokio::sync::Mutex<Option<Arc<Served>>>,
}

/// The projection a connection serves, over the session it started on.
struct Served {
    session: Arc<Session>,
    server: McpServer<SessionActions>,
    /// The windows of the records the agent works in.
    windows: Arc<mcp_windows::AgentWindows>,
}

impl<E: Enter> Connection<E> {
    pub(crate) fn new(shared: Arc<Shared<E>>) -> Self {
        Self {
            shared,
            run: hex::encode(rand::random::<[u8; 8]>()),
            served: tokio::sync::Mutex::new(None),
        }
    }

    /// What this connection serves, once the device is in the workspace.
    async fn served(&self) -> Result<Arc<Served>, String> {
        let mut held = self.served.lock().await;
        if let Some(served) = held.as_ref() {
            return Ok(Arc::clone(served));
        }
        let session = self.shared.session(true).await?;
        let device = self.shared.device.as_ref().map_err(Clone::clone)?;
        let served = Arc::new(open(session, device, &self.run)?);
        *held = Some(Arc::clone(&served));
        drop(held);
        Ok(served)
    }
}

/// The projection over the workspace `session` is in, its actions asked of
/// the workspace's host.
fn open(session: Arc<Session>, device: &Device, run: &str) -> Result<Served, String> {
    let workspace_id = session.device.workspace_id();
    let user: [u8; 32] = hex::decode(&device.user)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("this computer's login names no person; log in again")?;
    let actions = session.device.actions(workspace_id);
    let person = collab_types::Principal::of_person(user).id().to_owned();
    // The records the agent works in open as windows on the workspace's
    // desk; the host that executes its run shows it there, doing what it
    // does.
    let windows = Arc::new(mcp_windows::AgentWindows::new(
        actions.clone(),
        person.clone(),
        device.workspace.clone(),
    ));
    let server = McpServer::new(actions, device.workspace.clone(), person)
        .with_surface(Surface::Agent)
        .with_agent_run(run, device.agent.clone());
    Ok(Served {
        session,
        server,
        windows,
    })
}

/// `tilo mcp`: on a Tilo desktop, the desktop's own agent door, which asks the person to let the
/// agent in; elsewhere, this computer's login, entering the person's workspace wherever it is
/// hosted, as a connection to the process holding that device's session ([`held`]), started when
/// none runs. `hold` is that process (`tilo mcp --hold`, which a connection starts).
pub fn serve(hold: bool) -> Result<ExitCode> {
    let device = match tilo_runtime::session_door::desktop_door() {
        Ok(Some(door)) => {
            tilo_runtime::session_door::bridge(&door)?;
            return Ok(ExitCode::SUCCESS);
        }
        Ok(None) => crate::login::load(&crate::login::device_dir()?)?
            .ok_or_else(|| NOT_LOGGED_IN.to_owned()),
        // A desktop whose door is not open yet (its first login is not complete) is no computer
        // that needs a login: the agent hears what it waits for.
        Err(error) => Err(error.to_string()),
    };
    // The agent's connection goes where `tilo login` went: a host whose
    // certificate only a test trusts is reached the same way here.
    crate::login::trust_test_certificates()?;
    let Ok((key, _)) = &device else {
        // Nothing to share: every call says why there is no workspace.
        return held::alone(shared(device));
    };
    let place = held::Place::of(
        &crate::login::device_dir()?,
        &key.verifying_key().to_bytes(),
    );
    if hold {
        return held::hold(&place.map_err(anyhow::Error::msg)?, shared(device));
    }
    match place.and_then(|place| held::reach(&place).map(|stream| (stream, place))) {
        Ok((stream, place)) => held::relay(stream, &place),
        Err(why) => {
            // The agent keeps its tools: this connection holds a session of
            // its own, which another `tilo mcp` of this device would cut off.
            eprintln!("tilo mcp: {why}; serving this connection alone");
            held::alone(shared(device))
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
    let served = connection.served().await.and_then(|served| {
        // The person stopped this agent: its session ended for good, and every call says so.
        match served.session.stopped() {
            Some(stopped) => Err(stopped),
            None => Ok(served),
        }
    });
    let answer = match &served {
        Ok(served) => served.server.handle(request.clone()).await,
        Err(problem) => unreachable_answer(&request, problem),
    }?;
    if let Ok(served) = served {
        show(&served, &request);
    }
    Some(answer.to_string())
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

/// Open the record the tool call `request` worked in as a window where the
/// agent works: the host that executes the run shows what it did.
fn show(served: &Arc<Served>, request: &Value) {
    if request.get("method").and_then(Value::as_str) != Some("tools/call") {
        return;
    }
    // Each record the call made or changed opens where the agent works, as
    // a step of the run the call was.
    let windows = Arc::clone(&served.windows);
    let (changed, run) = (served.server.take_changed(), served.server.run_of(request));
    tokio::spawn(async move { windows.changed(changed, run).await });
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

    /// A Tilo desktop whose door is not open yet (its first login is not complete) is no computer
    /// that needs `tilo login`: the agent hears what it waits for, never "not logged in".
    #[tokio::test]
    async fn a_desktop_whose_door_is_not_open_yet_never_says_not_logged_in() {
        let waiting = tilo_runtime::session_door::route(
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
