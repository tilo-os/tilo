//! Agents hosted by `tilo-server` itself, driven from a browser session.
//!
//! The host runs `tilo_server::agents::start`: an [`AgentHost`] whose
//! supervisor is the host's app action server, one thread per agent, state
//! under a directory. A simulated browser (the page's session code built
//! natively: HTTPS for documents and remote actions) asks for a grant; a
//! root-holding laptop approves the agent and signs its delegation in the
//! same step; the browser spawns the agent with a task. The agent works on it
//! through the workspace's agent tools, asked of the host's actions through
//! its own session, and the browser receives the notes it makes. The host
//! then restarts: the agent resumes and takes new work; a stopped agent stays
//! stopped; a revoked grant ends its agent.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "in a test these are the assertion, not a hazard; one scenario in order"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::InvokeArgument;
use tilo_agent_session::model::PlanTurns;
use tilo_agent_session::{ReplicaActor, Seed};
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage,
};
use tilo_server::daemon::Server;
use tilo_server_protocol::agents::{
    AgentStatus, GrantState, MESSAGE_ACTION, REQUEST_GRANT_ACTION, REVOKE_ACTION, SPAWN_ACTION,
    STOP_ACTION, SUPERVISOR_RESOURCE, TaskState, agent_resource,
};

use tilo_user_identity::DeviceCapabilities;

use crate::harness::hosted::{Hosted, Laptop, Options, Unlisted};
use crate::harness::page::Page as Browser;
use crate::support::{Device, WORKSPACE, enrol, workspace};
use tilo_actions::MemberId;

const NOTES: &str = "so.tilo.notes";

/// A model that does what its request writes out: a request that is a JSON
/// list of tool calls (`[{"name": "tilo_do", "input": {…}}]`) is called one
/// call a round, and once every call was made it answers with what the last
/// one said, so the task's answer is the tools' own.
struct WrittenOut;

impl PlanTurns for WrittenOut {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        let words = |message: &Message| {
            message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::Text { text } => Some(text.clone()),
                    Content::ToolResult(result) => Some(result.content.clone()),
                    _ => None,
                })
                .collect::<String>()
        };
        let asked = request
            .messages
            .iter()
            .find(|message| matches!(message.role, Role::User))
            .map(words)
            .unwrap_or_default();
        let calls: Vec<serde_json::Value> = serde_json::from_str(&asked).unwrap_or_default();
        let answered: Vec<&Message> = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, Role::Tool))
            .collect();
        let (content, stop_reason) = match calls.get(answered.len()) {
            Some(call) => (
                vec![Content::ToolCall(ToolCall {
                    id: format!("call-{}", answered.len()),
                    name: call["name"].as_str().unwrap_or_default().to_owned(),
                    input: call["input"].clone(),
                    extra_content: None,
                })],
                StopReason::ToolCalls,
            ),
            None => (
                vec![Content::Text {
                    text: answered
                        .last()
                        .map_or_else(|| "Nothing to do.".to_owned(), |last| words(last)),
                }],
                StopReason::EndTurn,
            ),
        };
        Ok(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content,
            },
            stop_reason,
            usage: Usage::default(),
            cost_micros: 0,
        })
    }

    fn model(&self) -> &'static str {
        "written-out"
    }
}

/// A task making a note titled `title` through the agent tools.
fn new_note(title: &str) -> String {
    serde_json::json!([{"name": "tilo_do",
                        "input": {"path": "notes#new-note", "args": {"title": title}}}])
    .to_string()
}

/// The host, offering every app the tests drive, its agents working with [`WrittenOut`].
async fn start(root: &SigningKey, host: &Device, state: &Path) -> Hosted {
    crate::harness::hosted::start(
        root,
        host,
        state,
        Options::working_with(
            crate::common::catalogue,
            Arc::new(|| Some(Box::new(WrittenOut) as Box<dyn PlanTurns>)),
        ),
    )
    .await
}

/// Stop the host, after checking it carried nothing in the clear.
async fn stop(hosted: Hosted) {
    let retained = hosted
        .server
        .workspace_documents()
        .retained(workspace())
        .await
        .unwrap();
    let (readable, _) = crate::common::member_documents(
        retained
            .into_iter()
            .map(|retained| (retained.author, retained.body)),
        &hosted.device,
    );
    assert_eq!(readable, 0, "nothing reaches the host in the clear");
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.server.shutdown().await;
}

/// The laptop: a root holder answering every agent request with the
/// agent's certificate and the delegation of exactly what was proposed.
fn approve_agents(
    server: &Server,
    root: SigningKey,
    laptop: &Device,
) -> tokio::task::JoinHandle<()> {
    Laptop::new([], Unlisted::Pending).spawn(server, root, laptop)
}

impl Browser {
    /// The document of every `app` record the page received, as JSON, each
    /// change of it `author`'s.
    fn bodies(&mut self, app: &str, author: [u8; 32]) -> Vec<String> {
        self.pump();
        self.documents
            .iter()
            .filter(|(route, _)| route.app_id == app)
            .map(|(route, changes)| {
                assert!(
                    changes.iter().all(|(from, _)| *from == author),
                    "only the host edits {app} here, for the agent"
                );
                // A part of a chain (the parts Notes keeps its notes in) is
                // no document the app opens on its own: its changes are read
                // as they merge.
                if route_document(&route.document).chain_part().is_some() {
                    let mut doc = automerge::AutoCommit::new();
                    for (_, bytes) in changes {
                        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
                    }
                    return serde_json::to_string(&automerge::AutoSerde::from(&doc)).unwrap();
                }
                // Until the change that sets the record up arrives, the app
                // refuses to open what came before it.
                let page = match crate::common::open(
                    app,
                    &MemberId::person("ada"),
                    ReplicaActor::Owner,
                    Seed::Existing {
                        record: route_document(&route.document),
                        changes: changes.iter().map(|(_, bytes)| bytes.clone()).collect(),
                    },
                ) {
                    Ok(page) => page,
                    Err(error) => return format!("(does not open yet: {error})"),
                };
                page.read(|doc| serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap())
                    .unwrap()
            })
            .collect()
    }

    /// Wait until the page holds `text` in one of `app`'s records.
    async fn until_edited(&mut self, app: &str, author: [u8; 32], text: &str) {
        let deadline = tilo_test_env::hang_deadline();
        while !self
            .bodies(app, author)
            .iter()
            .any(|body| body.contains(text))
        {
            if Instant::now() >= deadline {
                let received: Vec<_> = self
                    .documents
                    .iter()
                    .map(|(route, changes)| (route.document.clone(), changes.len()))
                    .collect();
                let bodies = self.bodies(app, author);
                let status = self.status().await;
                panic!(
                    "the browser never saw {text:?} in {app}: received {received:?}, \
                     bodies {bodies:?}, agents {:#?}, browser {:#?}",
                    status.agents,
                    self.peer.status()
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_runs_agents_a_browser_spawns_and_they_survive_a_restart() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    // The host runs every action the agent's tools ask for and makes its
    // change as the workspace's own, under its own device.
    let maker = host.cert.device_public_key;

    let hosted = start(&root, &host, state.path()).await;

    let approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;

    // 1. One consent step: the page asks, the laptop approves the agent
    // together with the delegation of what was asked.
    let (grant, requested) = request_grant(&mut browser, "Writer", &[NOTES]).await;
    assert_eq!(requested["pairingCode"].as_str().unwrap().len(), 6);
    let approved = browser
        .until("the grant to be approved", |status| {
            status
                .grants
                .iter()
                .any(|g| g.id == grant && g.state == GrantState::Approved)
        })
        .await;
    let offered = approved.grants.iter().find(|g| g.id == grant).unwrap();
    assert_eq!(offered.apps, [NOTES]);
    assert!(offered.pairing_code.is_none());

    // 2. The page spawns the agent with a task; its model makes a note
    // through the agent tools, asked of the host through the agent's session.
    let spawned = browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({
                    "grant": grant,
                    "task": new_note("Groceries"),
                    "workspace": WORKSPACE,
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(spawned["agentId"], grant.as_str());
    let status = browser
        .until("the first task to finish", |status| {
            status.agents.iter().any(|agent| {
                agent.id == grant
                    && agent.tasks.first().is_some_and(|task| {
                        task.state != TaskState::Queued && task.state != TaskState::Running
                    })
            })
        })
        .await;
    let info = status
        .agents
        .iter()
        .find(|agent| agent.id == grant)
        .unwrap()
        .clone();
    assert_eq!(
        info.tasks[0].state,
        TaskState::Completed,
        "{:?}",
        info.tasks[0]
    );
    assert_eq!(info.workspace.as_deref(), Some(WORKSPACE));
    // Each action the agent ran is a step of its task, on the app it named.
    let steps: Vec<_> = info.tasks[0]
        .steps
        .iter()
        .map(|step| (step.app.as_str(), step.action.as_str(), step.state))
        .collect();
    assert_eq!(
        steps,
        [("notes", "new-note", TaskState::Completed)],
        "{:?}",
        info.tasks[0]
    );
    browser.until_edited(NOTES, maker, "Groceries").await;

    // 3. A message is the agent's next task.
    browser
        .call(
            &agent_resource(&grant),
            MESSAGE_ACTION,
            InvokeArgument::Text(new_note("Budget")),
        )
        .await
        .unwrap();
    browser.until_edited(NOTES, maker, "Budget").await;

    // 4. The host restarts. The agent resumes from the state directory and
    // takes new work.
    browser.peer.close();
    hosted.agents.flush();
    let saved = std::fs::read_to_string(hosted.agents.state_file()).unwrap();
    assert!(saved.contains(&grant) && saved.contains("Budget"));
    approver.abort();
    stop(hosted).await;

    let hosted = start(&root, &host, state.path()).await;

    let approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let resumed = browser
        .until("the agent to resume", |status| {
            status
                .agents
                .iter()
                .any(|agent| agent.id == grant && agent.status == AgentStatus::Waiting)
        })
        .await;
    let info = resumed
        .agents
        .iter()
        .find(|agent| agent.id == grant)
        .unwrap();
    assert_eq!(info.tasks.len(), 2, "its task log survived");
    browser
        .call(
            &agent_resource(&grant),
            MESSAGE_ACTION,
            InvokeArgument::Text(new_note("Forecast")),
        )
        .await
        .unwrap();
    browser.until_edited(NOTES, maker, "Forecast").await;

    // 5. Stopped stays stopped across a restart.
    browser
        .call(&agent_resource(&grant), STOP_ACTION, InvokeArgument::None)
        .await
        .unwrap();
    browser
        .until("the agent to stop", |status| {
            status
                .agents
                .iter()
                .any(|agent| agent.id == grant && agent.status == AgentStatus::Stopped)
        })
        .await;
    browser.peer.close();
    approver.abort();
    stop(hosted).await;

    let hosted = start(&root, &host, state.path()).await;

    let _approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let after = browser.status().await;
    let info = after.agents.iter().find(|agent| agent.id == grant).unwrap();
    assert_eq!(info.status, AgentStatus::Stopped);
    assert!(
        browser
            .call(
                &agent_resource(&grant),
                MESSAGE_ACTION,
                InvokeArgument::Text("anything".into())
            )
            .await
            .is_err(),
        "a stopped agent takes no task"
    );

    // 6. The grant still spawns a fresh agent; revoking it ends the agent
    // and the grant.
    browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({"grant": grant, "task": new_note("Later")})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    browser.until_edited(NOTES, maker, "Later").await;
    browser
        .call(&agent_resource(&grant), REVOKE_ACTION, InvokeArgument::None)
        .await
        .unwrap();
    let revoked = browser
        .until("the grant to be revoked", |status| {
            status
                .grants
                .iter()
                .any(|g| g.id == grant && g.state == GrantState::Revoked)
        })
        .await;
    assert!(
        revoked
            .agents
            .iter()
            .any(|agent| agent.id == grant && agent.status == AgentStatus::Stopped)
    );
    assert!(
        browser
            .call(
                SUPERVISOR_RESOURCE,
                SPAWN_ACTION,
                InvokeArgument::Json(
                    serde_json::json!({"grant": grant, "task": "anything"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .is_err(),
        "a revoked grant spawns nothing"
    );
    hosted.agents.flush();
    let saved = std::fs::read_to_string(hosted.agents.state_file()).unwrap();
    let record: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert!(
        record["grants"][0].get("key").is_none(),
        "a revoked grant's key is forgotten"
    );
    browser.peer.close();
    stop(hosted).await;
}

/// The record a document route names. A workspace-record app (Notes, Chat)
/// publishes its package-primary record under the empty record id.
fn route_document(record: &str) -> ui::resource::DocumentId {
    if record.is_empty() {
        ui::resource::DocumentId::package_primary()
    } else {
        ui::resource::DocumentId::parse(record).unwrap()
    }
}

/// Ask for a grant labelled `label` covering `apps`: its id, and what the
/// page answered with.
async fn request_grant(
    browser: &mut Browser,
    label: &str,
    apps: &[&str],
) -> (String, serde_json::Value) {
    let requested = browser
        .call(
            SUPERVISOR_RESOURCE,
            REQUEST_GRANT_ACTION,
            InvokeArgument::Json(
                serde_json::json!({"label": label, "workspaces": [WORKSPACE], "apps": apps,
                                   "expiresInSecs": 3600})
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .unwrap();
    let grant = requested["grant"].as_str().unwrap().to_owned();
    (grant, requested)
}

/// [`request_grant`], waiting until the laptop approves it.
async fn approved_grant(browser: &mut Browser, label: &str, apps: &[&str]) -> String {
    let (grant, _) = request_grant(browser, label, apps).await;
    browser
        .until("the grant to be approved", |status| {
            status
                .grants
                .iter()
                .any(|g| g.id == grant && g.state == GrantState::Approved)
        })
        .await;
    grant
}
