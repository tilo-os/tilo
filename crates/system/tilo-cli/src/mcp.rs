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
use tilo_browser_peer::PeerHandle;
use tilo_browser_peer::login::{action, say_here};
use tilo_browser_peer::session_actions::SessionActions;
use tilo_relay_protocol::web::HereAction;
use tilo_runtime::mcp::{McpServer, Surface};

use crate::login::{Device, NOT_LOGGED_IN};
use crate::mcp_presence;

/// How many of the agent's actions the workspace shows at once.
const SHOWN_ACTIONS: usize = 8;
/// The MCP protocol version this server speaks when it cannot reach the
/// workspace; the projection answers with its own once it can.
const PROTOCOL: &str = "2025-06-18";
/// How long a request waits for this device to enter the workspace.
const ENTERING: Duration = Duration::from_secs(25);

/// This device in the workspace, serving the agent.
struct Session {
    server: McpServer<SessionActions>,
    peer: PeerHandle,
    workspace_id: u64,
    agent: String,
    shown: Mutex<Vec<HereAction>>,
    /// The agent, present in the workspace while this connection lives.
    presence: Arc<mcp_presence::Presence>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // The agent leaves with the connection: nothing of it is drawn
        // after this.
        self.presence.leave();
        self.peer.close();
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
        let session = self.session.lock().await.take();
        drop(session);
    }
}

/// The projection over the workspace `peer` is in, its actions asked of the
/// workspace's host.
fn open(
    (peer, workspace_id): (PeerHandle, u64),
    device: &Device,
    run: &str,
) -> Result<Session, String> {
    let host = device
        .certs()
        .map_err(|error| format!("this computer's login is damaged ({error}); log in again"))?
        .1
        .device_public_key;
    let user: [u8; 32] = hex::decode(&device.user)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("this computer's login names no person; log in again")?;
    let actions = SessionActions::new(&peer, workspace_id, host);
    // The person's desktops see the agent present from the moment it enters
    // the workspace, not from its first action.
    let presence = mcp_presence::Presence::new(
        &peer,
        actions.clone(),
        collab_types::Principal::of_person(user).id().to_owned(),
        (device.workspace.clone(), workspace_id),
        device.agent.clone(),
    );
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
        peer,
        workspace_id,
        agent: device.agent.clone(),
        shown: Mutex::new(Vec::new()),
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
    let session = serving.session().await;
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
    // The workspace sees the agent present, doing this and where, beside the
    // line this connection adds to the person's recent actions.
    let said = Arc::clone(session);
    let (call, acted) = (request.clone(), session.server.acted());
    tokio::spawn(async move { said.presence.called(&call, failed, acted).await });
    let Some((title, resource, app)) = described(request) else {
        return;
    };
    let title = if failed {
        format!("{title} (failed)")
    } else {
        title
    };
    let actions = {
        let mut shown = session
            .shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shown.push(action(&title, &resource, &app));
        let excess = shown.len().saturating_sub(SHOWN_ACTIONS);
        shown.drain(..excess);
        shown.clone()
    };
    let session = Arc::clone(session);
    tokio::spawn(async move {
        let _ = say_here(
            &session.peer,
            session.workspace_id,
            &session.agent,
            actions,
            true,
        )
        .await;
    });
}

/// A tool call as the person reads it: what the agent did and on what.
fn described(request: &Value) -> Option<(String, String, String)> {
    let params = request.get("params")?;
    let tool = params.get("name")?.as_str()?;
    let arguments = params.get("arguments").cloned().unwrap_or_default();
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    // A path names the app first and the record last (`notes/Notebook#append`):
    // the person reads it as the record in the app, never as the tool's path.
    let placed = |title: &str, path: &str| {
        let (app, record) = path_place(path);
        (title.to_owned(), record, app)
    };
    Some(match tool {
        "tilo_ls" => match text("path") {
            path if path.is_empty() => (
                "Looked at the apps".to_owned(),
                String::new(),
                String::new(),
            ),
            path => placed("Looked at", &path),
        },
        "tilo_read" => placed("Read", &text("path")),
        "tilo_find" => ("Searched for".to_owned(), text("query"), String::new()),
        "tilo_do" => {
            let path = text("path");
            if path.is_empty() {
                let calls = arguments
                    .get("calls")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                match calls {
                    [one] => placed(
                        "Ran",
                        one.get("path").and_then(Value::as_str).unwrap_or_default(),
                    ),
                    calls => (
                        "Ran".to_owned(),
                        format!("{} actions", calls.len()),
                        String::new(),
                    ),
                }
            } else {
                placed("Ran", &path)
            }
        }
        "tilo" => ("Ran a program".to_owned(), text("program"), String::new()),
        _ => return None,
    })
}

/// The app and the record a tool path names, as a person reads them:
/// `notes/Launch notes#append-to-note` is "Launch notes" in "Notes", and
/// `terminal#run` is the Terminal itself.
fn path_place(path: &str) -> (String, String) {
    let path = path.split('#').next().unwrap_or_default().trim();
    let mut parts = path
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let app = parts
        .next()
        .map(|app| app.split('@').next().unwrap_or(app))
        .map(|app| {
            let mut letters = app.chars();
            letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let record = parts.next_back().unwrap_or_default().to_owned();
    (app, record)
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

    #[test]
    fn a_call_is_shown_as_what_it_did_and_on_what() {
        let call = |name: &str, arguments: Value| {
            described(
                &json!({"method": "tools/call", "params": {"name": name, "arguments": arguments}}),
            )
        };
        let shown = |title: &str, record: &str, app: &str| {
            Some((title.to_owned(), record.to_owned(), app.to_owned()))
        };
        // The record in its app, never the tool's path or the tool's name.
        assert_eq!(
            call("tilo_do", json!({"path": "docs/Plan#append-paragraph"})),
            shown("Ran", "Plan", "Docs")
        );
        assert_eq!(
            call("tilo_do", json!({"path": "terminal#run"})),
            shown("Ran", "", "Terminal")
        );
        assert_eq!(
            call(
                "tilo_do",
                json!({"calls": [{"path": "notes/Notebook/Launch notes#append-to-note"}]})
            ),
            shown("Ran", "Launch notes", "Notes")
        );
        assert_eq!(
            call(
                "tilo_do",
                json!({"calls": [{"path": "a#b"}, {"path": "c#d"}]})
            ),
            shown("Ran", "2 actions", "")
        );
        assert_eq!(
            call("tilo_ls", json!({})),
            shown("Looked at the apps", "", "")
        );
        assert_eq!(
            call("tilo_find", json!({"query": "budget"})),
            shown("Searched for", "budget", "")
        );
        assert_eq!(call("tilo_status", json!({})), None);
    }
}
