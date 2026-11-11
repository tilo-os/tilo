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
//! Every connection is one agent run, named for the agent ("Claude Code"),
//! which History groups its actions under; the workspace shows the agent
//! present with what it does while it runs.

use std::io::{BufRead as _, Write as _};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_session_core::remote::SessionActions;
use tilo_session_host::device::Device as Entered;

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

/// This device in the workspace, serving the agent.
struct Session {
    server: McpServer<SessionActions>,
    device: Entered,
    /// The windows of the records the agent works in.
    windows: Arc<mcp_windows::AgentWindows>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // The agent's run ends with the connection, and the host that shows
        // it takes it down.
        self.device.close();
    }
}

/// What the server holds: the device, and its session once entered.
struct Serving {
    /// This computer's login, or why there is none to enter with.
    device: Result<(SigningKey, Device), String>,
    /// The agent run every invocation of this connection belongs to.
    run: String,
    session: tokio::sync::Mutex<Option<Arc<Session>>>,
}

impl Serving {
    /// The device's session, entering the workspace when it is not in it
    /// yet; why not, when it cannot.
    async fn session(&self) -> Result<Arc<Session>, String> {
        let (key, device) = self.device.as_ref().map_err(Clone::clone)?;
        let mut held = self.session.lock().await;
        if let Some(session) = held.as_ref() {
            return Ok(Arc::clone(session));
        }
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
        let session = Arc::new(open(entered, device, &self.run)?);
        *held = Some(Arc::clone(&session));
        drop(held);
        Ok(session)
    }

    /// Leave the workspace as the connection ends: the agent says it has
    /// left while its session can still say so, and the device closes.
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

/// The projection over the workspace `entered` is in, its actions asked of
/// the workspace's host.
fn open(entered: Entered, device: &Device, run: &str) -> Result<Session, String> {
    let workspace_id = entered.workspace_id();
    let user: [u8; 32] = hex::decode(&device.user)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("this computer's login names no person; log in again")?;
    let actions = entered.actions(workspace_id);
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
    Ok(Session {
        server,
        device: entered,
        windows,
    })
}

/// `tilo mcp`: on a Tilo desktop, the desktop's own agent door, which asks the person to let the
/// agent in; elsewhere, this computer's login, entering the person's workspace wherever it is
/// hosted.
pub fn serve() -> Result<ExitCode> {
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let serving = Arc::new(Serving {
        device,
        run: hex::encode(rand::random::<[u8; 8]>()),
        session: tokio::sync::Mutex::new(None),
    });
    // Enter the workspace while the agent starts, not at its first call.
    let entering = Arc::clone(&serving);
    runtime.spawn(async move {
        let _ = entering.session().await;
    });
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let mut answering = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let (serving, out) = (Arc::clone(&serving), Arc::clone(&out));
        answering.retain(|task: &tokio::task::JoinHandle<()>| !task.is_finished());
        answering.push(runtime.spawn(async move {
            if let Some(answer) = answer(&serving, &line).await {
                let mut out = out
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = writeln!(out, "{answer}");
                let _ = out.flush();
            }
        }));
    }
    runtime.block_on(async {
        for task in answering {
            let _ = task.await;
        }
        // The connection ended (its input closed): the agent leaves the
        // workspace now, while its session can still say so.
        serving.leaving().await;
    });
    Ok(ExitCode::SUCCESS)
}

/// The answer to one JSON-RPC line, or `None` for a notification.
async fn answer(serving: &Serving, line: &str) -> Option<String> {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": error.to_string()}})
                    .to_string(),
            );
        }
    };
    let session = serving.session().await.and_then(|session| {
        // The person stopped this agent: its session ended for good, and every call says so.
        let stopped = session
            .device
            .session()
            .status()
            .failures
            .into_iter()
            .find(|failure| failure.code == STOPPED);
        match stopped {
            Some(failure) => Err(format!("{} Start a new session to go on.", failure.message)),
            None => Ok(session),
        }
    });
    let answer = match &session {
        Ok(session) => session.server.handle(request.clone()).await,
        Err(problem) => unreachable_answer(&request, problem),
    }?;
    if let Ok(session) = session {
        show(&session, &request);
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
fn show(session: &Arc<Session>, request: &Value) {
    if request.get("method").and_then(Value::as_str) != Some("tools/call") {
        return;
    }
    // Each record the call made or changed opens where the agent works, as
    // a step of the run the call was.
    let windows = Arc::clone(&session.windows);
    let (changed, run) = (
        session.server.take_changed(),
        session.server.run_of(request),
    );
    tokio::spawn(async move { windows.changed(changed, run).await });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serving() -> Serving {
        Serving {
            device: Err(NOT_LOGGED_IN.to_owned()),
            run: "run".into(),
            session: tokio::sync::Mutex::new(None),
        }
    }

    async fn ask(serving: &Serving, line: &str) -> Option<Value> {
        answer(serving, line)
            .await
            .map(|answer| serde_json::from_str(&answer).unwrap())
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
        let serving = Serving {
            device: Err(waiting.clone()),
            run: "run".into(),
            session: tokio::sync::Mutex::new(None),
        };
        let init = ask(
            &serving,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .await
        .unwrap();
        let said = init["result"]["instructions"].as_str().unwrap();
        assert_eq!(said, waiting);
        assert!(!said.contains("not logged in"), "{said}");
        assert!(said.contains("first login"), "{said}");
    }

    #[tokio::test]
    async fn without_a_login_the_server_says_how_to_log_in_and_offers_nothing() {
        let serving = serving();
        let init = ask(
            &serving,
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
                &serving,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .await
            .is_none()
        );
        let listed = ask(
            &serving,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        )
        .await
        .unwrap();
        assert_eq!(listed["result"]["tools"], json!([]));
        let called = ask(
            &serving,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tilo_ls"}}"#,
        )
        .await
        .unwrap();
        assert_eq!(called["result"]["isError"], true);
    }
}
