//! `tilo mcp`: the agent's MCP server over stdio, as the device `tilo login`
//! made of this computer.
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
use tilo_session_host::device::Device as Entered;
use tilo_session_host::session_actions::SessionActions;

use crate::login::{Device, NOT_LOGGED_IN};
use tilo_runtime::mcp_presence;

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
    /// The agent, present in the workspace while this connection lives.
    presence: Arc<mcp_presence::Presence>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // The agent leaves with the connection: nothing of it is drawn
        // after this.
        self.presence.leave();
        self.device.close();
    }
}

/// What the server holds: the device, and its session once entered.
struct Serving {
    device: Option<(SigningKey, Device)>,
    /// The agent run every invocation of this connection belongs to.
    run: String,
    session: tokio::sync::Mutex<Option<Arc<Session>>>,
}

impl Serving {
    /// The device's session, entering the workspace when it is not in it
    /// yet; why not, when it cannot.
    async fn session(&self) -> Result<Arc<Session>, String> {
        let Some((key, device)) = &self.device else {
            return Err(NOT_LOGGED_IN.to_owned());
        };
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
            while peer.status().phase != tilo_session_host::TransportPhase::Closed {
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
    // The person's desktops see the agent present from the moment it enters
    // the workspace, not from its first action.
    let presence = mcp_presence::Presence::new(
        (entered.session(), entered.cert()),
        actions.clone(),
        collab_types::Principal::of_person(user).id().to_owned(),
        (device.workspace.clone(), workspace_id),
        device.agent.clone(),
    )
    .ok_or("this computer's login names no workspace; log in again")?;
    presence.arrive();
    // And see each action it takes in a record as it goes.
    let narrating = Arc::clone(&presence);
    let server = McpServer::new(
        actions,
        device.workspace.clone(),
        collab_types::Principal::of_person(user).id().to_owned(),
    )
    .with_surface(Surface::Agent)
    .with_agent_run(run, device.agent.clone())
    .with_narration(Arc::new(move |acting| narrating.acting(&acting)));
    Ok(Session {
        server,
        device: entered,
        presence,
    })
}

/// `tilo mcp`.
pub fn serve() -> Result<ExitCode> {
    // The agent's connection goes where `tilo login` went: a host whose
    // certificate only a test trusts is reached the same way here.
    crate::login::trust_test_certificates()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let serving = Arc::new(Serving {
        device: crate::login::load(&crate::login::device_dir()?)?,
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
        show(&session, &request, &answer);
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

/// Show the person, beside the agent in their workspace, the tool call
/// `request` made and whether it worked.
fn show(session: &Arc<Session>, request: &Value, answer: &Value) {
    if request.get("method").and_then(Value::as_str) != Some("tools/call") {
        return;
    }
    let failed = answer.pointer("/result/isError") == Some(&Value::Bool(true))
        || answer.get("error").is_some();
    // The workspace sees the agent present, doing this and where: its own
    // presence says it, as every desktop reads it.
    let said = Arc::clone(session);
    let (call, acted, changed) = (
        request.clone(),
        session.server.acted(),
        session.server.take_changed(),
    );
    tokio::spawn(async move { said.presence.called(&call, failed, acted, changed).await });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serving() -> Serving {
        Serving {
            device: None,
            run: "run".into(),
            session: tokio::sync::Mutex::new(None),
        }
    }

    async fn ask(serving: &Serving, line: &str) -> Option<Value> {
        answer(serving, line)
            .await
            .map(|answer| serde_json::from_str(&answer).unwrap())
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
