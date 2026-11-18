//! Agents the server starts itself, driven from a browser session.
//!
//! The server starts `tilo_server::agents::start`: its agent supervisor, one
//! thread per agent, state under a directory. A simulated browser (the page's
//! session code built natively: HTTPS for documents and remote actions) asks
//! for a grant; a root-holding laptop approves the agent and signs its
//! delegation in the same step; the browser gives the agent a task. The agent
//! works on it through the workspace's agent tools, asked of the server's
//! actions through its own session, and the browser receives the notes and
//! sheets it makes. The server then restarts: the agent resumes and takes new
//! work; a stopped agent stays stopped; a revoked grant ends its agent. An
//! action that asks first is put to the person's approving device for that
//! one invocation, and acts only on its answer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::future_not_send,
    clippy::arithmetic_side_effects,
    clippy::similar_names,
    clippy::needless_pass_by_value,
    reason = "in a test these are the assertion, not a hazard; one scenario in order"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::InvokeArgument;
use tilo_agent_session::model::PlanTurns;
use tilo_agent_session::{ApiKey, ModelChoice, Replica, ReplicaActor, Seed};
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server::certify::PendingConfirmation;
use tilo_server::certify::{ApproverReply, ConfirmationAnswer};
use tilo_server::daemon::Server;
use tilo_server_client::Approver;
use tilo_server_protocol::agents::{
    AgentInfo, AgentStatus, AgentsStatus, GrantState, MESSAGE_ACTION, REQUEST_GRANT_ACTION,
    REVOKE_ACTION, SPAWN_ACTION, STOP_ACTION, SUPERVISOR_RESOURCE, TaskState, agent_resource,
};
use tilo_session_client::Outbox;

use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::harness::hosted::{Hosted, Laptop, Options, Unlisted};
use crate::harness::page::Page as Browser;
use crate::support::{Device, WORKSPACE, enrol, workspace};
use tilo_actions::MemberId;

const NOTES: &str = "so.tilo.notes";
const SHEETS: &str = "so.tilo.sheets";
const EBOOK: &str = "so.tilo.ebook";
const CHAT: &str = "so.tilo.chat";

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

/// One tool call, as [`WrittenOut`] makes it.
fn call(name: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"name": name, "input": input})
}

/// A `tilo_do` call: do the action `path` names with `args`.
fn doing(path: &str, args: serde_json::Value) -> serde_json::Value {
    call("tilo_do", serde_json::json!({"path": path, "args": args}))
}

/// [`doing`] with the arguments an action's own names made.
fn doing_args(path: &str, args: serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    doing(path, serde_json::Value::Object(args))
}

/// An action's arguments by the names it gives them.
fn arguments(fields: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
    fields
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect()
}

/// The argument Chat's `post` names the conversation a message goes in by:
/// Chat's own channel.
const CONVERSATION_ARGUMENT: &str = "channel";

/// The argument the library's `shelve` names the topic a book is filed under
/// by: a book's subject, as catalogues say.
const TOPIC_ARGUMENT: &str = "subject";

/// The task asking for `calls`, in order.
fn task(calls: &[serde_json::Value]) -> String {
    serde_json::Value::Array(calls.to_vec()).to_string()
}

/// A task making a note titled `title` through the agent tools.
fn new_note(title: &str) -> String {
    task(&[doing("notes#new-note", serde_json::json!({"title": title}))])
}

/// A task adding a sheet named `name` to the person's workbook.
fn new_sheet(name: &str) -> String {
    task(&[doing("sheets#new-sheet", serde_json::json!({"name": name}))])
}

/// A task that only reads what the Person's Sheets publish.
fn reading_the_sheets() -> String {
    task(&[call("tilo_read", serde_json::json!({"path": "sheets"}))])
}

/// The content reference an answer names (`tilo.content:<hash>`).
fn content_in(text: &str) -> String {
    let start = text
        .find("tilo.content:")
        .unwrap_or_else(|| panic!("no content named in {text}"));
    let reference: String = text[start + "tilo.content:".len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    format!("tilo.content:{}", reference.trim_end_matches('.'))
}

/// The server, offering every app the tests drive, its agents working with [`WrittenOut`].
async fn start(root: &SigningKey, home: &Device, state: &Path) -> Hosted {
    crate::harness::hosted::start(
        root,
        home,
        state,
        Options::working_with(
            crate::common::catalogue,
            Arc::new(|| Some(Box::new(WrittenOut) as Box<dyn PlanTurns>)),
        ),
    )
    .await
}

/// [`start`] with `model` answering the agents' turns.
async fn start_with(root: &SigningKey, home: &Device, state: &Path, model: ModelChoice) -> Hosted {
    crate::harness::hosted::start(
        root,
        home,
        state,
        Options::offering(crate::common::catalogue, model),
    )
    .await
}

/// Stop the server, after checking it carried nothing in the clear.
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
    assert_eq!(readable, 0, "nothing reaches the server in the clear");
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
                    "only the server edits {app} here, for the agent"
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
                // Until the change that sets the document up arrives, the app
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

    /// Wait until the page holds `text` in one of `app`'s documents.
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
    let home = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    // The server takes every action the agent's tools ask for and makes its
    // change as the workspace's own, under its own device.
    let maker = home.cert.device_public_key;

    let hosted = start(&root, &home, state.path()).await;

    let approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &home).await;

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
    // through the agent tools, asked of the server through the agent's session.
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

    // 4. The server restarts. The agent resumes from the state directory and
    // takes new work.
    browser.peer.close();
    hosted.agents.flush();
    let saved = std::fs::read_to_string(hosted.agents.state_file()).unwrap();
    assert!(saved.contains(&grant) && saved.contains("Budget"));
    approver.abort();
    stop(hosted).await;

    let hosted = start(&root, &home, state.path()).await;

    let approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &home).await;
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

    let hosted = start(&root, &home, state.path()).await;

    let _approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &home).await;
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
    let kept: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert!(
        kept["grants"][0].get("key").is_none(),
        "a revoked grant's key is forgotten"
    );
    browser.peer.close();
    stop(hosted).await;
}

/// The document a route names. A workspace-record app (Notes, Chat)
/// publishes its package-primary document under the empty id.
fn route_document(id: &str) -> ui::resource::DocumentId {
    if id.is_empty() {
        ui::resource::DocumentId::package_primary()
    } else {
        ui::resource::DocumentId::parse(id).unwrap()
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

/// Spawn `grant`'s agent on `task` in the workspace.
async fn spawn(browser: &mut Browser, grant: &str, task: String) {
    browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({"grant": grant, "task": task, "workspace": WORKSPACE})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
}

/// Give `grant`'s agent its next task.
async fn next_task(browser: &mut Browser, grant: &str, task: String) {
    browser
        .call(
            &agent_resource(grant),
            MESSAGE_ACTION,
            InvokeArgument::Text(task),
        )
        .await
        .unwrap();
}

/// Whether `grant`'s agent's `tasks`th task stands at `state`.
fn last_task_is(grant: &str, state: TaskState, tasks: usize) -> impl Fn(&AgentsStatus) -> bool {
    move |status| {
        status.agents.iter().any(|agent| {
            agent.id == grant
                && agent.tasks.len() == tasks
                && agent.tasks.last().is_some_and(|task| task.state == state)
        })
    }
}

/// The sheets of a workbook, in order.
fn sheet_names(app: &Replica) -> Vec<String> {
    app.read(|doc| {
        <sheets::Workbook as ui::tilo_resource::Resource>::read(doc)
            .map(|workbook| {
                workbook
                    .sheets
                    .values()
                    .map(|sheet| sheet.name.0.clone())
                    .collect()
            })
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

/// A person's workbook, open in a browser session: it publishes what the
/// person does and merges what everyone else committed.
struct Page {
    app: Replica,
    route: DocumentRoute,
    merged: usize,
}

impl Page {
    fn create(browser: &Browser, app: &str) -> Self {
        let mut page = crate::common::open(
            app,
            &MemberId::person("ada"),
            ReplicaActor::Owner,
            Seed::New,
        )
        .unwrap();
        let route = DocumentRoute {
            app_id: app.into(),
            cluster_id: WORKSPACE.into(),
            document: page.wire_document().to_owned(),
        };
        browser
            .peer
            .write(route.clone(), &page.take_mutation().unwrap().unwrap())
            .unwrap();
        Self {
            app: page,
            route,
            merged: 0,
        }
    }

    /// Open the workbook `creator` has open from another session, once its
    /// content has arrived, as a second browser does, and publish what the
    /// mount itself wrote.
    async fn join(browser: &mut Browser, creator: &Self) -> Self {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            browser.pump();
            if let Some(changes) = browser
                .documents
                .get(&creator.route)
                .filter(|c| !c.is_empty())
            {
                let mut app = crate::common::open(
                    SHEETS,
                    &MemberId::person("bea"),
                    ReplicaActor::Owner,
                    Seed::Existing {
                        record: route_document(&creator.route.document),
                        changes: changes.iter().map(|(_, bytes)| bytes.clone()).collect(),
                    },
                )
                .unwrap();
                if let Some(own) = app.take_mutation().unwrap() {
                    browser.peer.write(creator.route.clone(), &own).unwrap();
                }
                return Self {
                    app,
                    route: creator.route.clone(),
                    merged: changes.len(),
                };
            }
            assert!(
                Instant::now() < deadline,
                "the workbook never reached the second session"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Merge the changes the session received since the last call.
    fn merge(&mut self, browser: &mut Browser) {
        browser.pump();
        let changes = browser
            .documents
            .get(&self.route)
            .cloned()
            .unwrap_or_default();
        for (_, bytes) in changes.iter().skip(self.merged) {
            self.app
                .apply(&collab_types::DocumentMutation::Incremental(bytes.clone()))
                .unwrap();
        }
        self.merged = changes.len();
    }

    async fn until_sheets(&mut self, browser: &mut Browser, want: &[&str]) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.merge(browser);
            if sheet_names(&self.app) == want {
                return;
            }
            if Instant::now() >= deadline {
                let status = browser.status().await;
                panic!(
                    "the page shows {:?}, not {want:?}: agents {:#?}, browser {}",
                    sheet_names(&self.app),
                    status.agents,
                    browser.peer.status().key_state
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// An agent's later task edits the workbook people have open as live as its
/// first one did, and a session that joins afterwards replays both edits
/// once each. A task that only reads opens nothing; the first edit puts the
/// workbook's window on the layout the person's page wrote.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_browser_sees_every_task_of_an_agent_live_and_after_a_reload() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let home = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &home, state.path()).await;
    let _approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &home).await;
    browser.put_layout();

    // The person opens a workbook before any agent exists, and it is in the
    // workspace (sealed and durable on the server) before the agent arrives.
    let mut page = Page::create(&browser, SHEETS);
    page.until_sheets(&mut browser, &["Sheet1"]).await;
    browser.durable().await;

    let grant = approved_grant(&mut browser, "Writer", &[SHEETS]).await;
    spawn(&mut browser, &grant, reading_the_sheets()).await;
    browser
        .until(
            "the reading task to end",
            last_task_is(&grant, TaskState::Completed, 1),
        )
        .await;
    assert!(
        browser.layout().is_some_and(|desktop| !desktop
            .records
            .iter()
            .any(|(_, shown)| *shown == page.route.document)),
        "a task that only read opened nothing"
    );

    next_task(&mut browser, &grant, new_sheet("Budget")).await;
    page.until_sheets(&mut browser, &["Sheet1", "Budget"]).await;

    next_task(&mut browser, &grant, new_sheet("Q4")).await;
    page.until_sheets(&mut browser, &["Sheet1", "Budget", "Q4"])
        .await;
    let done = browser
        .until("every task to complete", |status| {
            status.agents.iter().any(|agent| {
                agent.id == grant
                    && agent.tasks.len() == 3
                    && agent.tasks[1..]
                        .iter()
                        .all(|task| task.state == TaskState::Completed)
            })
        })
        .await;
    let first = &done
        .agents
        .iter()
        .find(|agent| agent.id == grant)
        .unwrap()
        .tasks;
    assert_eq!(
        first.iter().map(|task| task.state).collect::<Vec<_>>(),
        [TaskState::Completed; 3],
        "{first:?}"
    );

    // A reload: a new session replays the workspace and rebuilds the workbook
    // from what the server kept.
    let reloaded_device = enrol(&root, "browser, reloaded", DeviceCapabilities::member());
    let mut reloaded = Browser::open(&hosted, &reloaded_device, &home).await;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        reloaded.pump();
        if let Some(changes) = reloaded
            .documents
            .get(&page.route)
            .filter(|c| !c.is_empty())
        {
            let replayed = crate::common::open(
                SHEETS,
                &MemberId::person("ada"),
                ReplicaActor::Owner,
                Seed::Existing {
                    record: route_document(&page.route.document),
                    changes: changes.iter().map(|(_, bytes)| bytes.clone()).collect(),
                },
            );
            if let Ok(replayed) = replayed {
                if sheet_names(&replayed) == ["Sheet1", "Budget", "Q4"] {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "after a reload the page shows {:?}",
                    sheet_names(&replayed)
                );
            }
        }
        assert!(Instant::now() < deadline, "the reload replayed nothing");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The workbook the agent worked in is open on the workspace's layout, once,
    // as a window of the person's that the agent's task opened.
    let person = hex::encode(root.verifying_key().to_bytes());
    loop {
        let desktop = reloaded.layout();
        let windows: Vec<u64> = desktop
            .iter()
            .flat_map(|desktop| &desktop.records)
            .filter(|(_, shown)| *shown == page.route.document)
            .map(|(tab, _)| *tab)
            .collect();
        if let (Some(desktop), [tab]) = (&desktop, windows.as_slice()) {
            assert_eq!(desktop.authors.get(tab), Some(&person), "{desktop:?}");
            let opened = desktop.opened_by.get(tab);
            assert!(
                opened.is_some_and(|opened| !opened.run.is_empty() && !opened.label.is_empty()),
                "the window names the agent's task: {desktop:?}"
            );
            break;
        }
        assert!(
            windows.len() < 2 && Instant::now() < deadline,
            "the agent's workbook is open on the layout once: {desktop:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stop(hosted).await;
}

/// The page that created a workbook and a page that joined it both see an
/// agent's first and second task live: nothing the joined mount does may
/// leave the creator behind a change it never received.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_creator_and_a_joined_page_both_see_every_task_live() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let home = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let first_device = enrol(&root, "browser", DeviceCapabilities::member());
    let second_device = enrol(&root, "second browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &home, state.path()).await;
    let _approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let mut first = Browser::open(&hosted, &first_device, &home).await;
    let mut second = Browser::open(&hosted, &second_device, &home).await;

    let mut creator = Page::create(&first, SHEETS);
    creator.until_sheets(&mut first, &["Sheet1"]).await;
    let mut joined = Page::join(&mut second, &creator).await;
    joined.until_sheets(&mut second, &["Sheet1"]).await;

    let grant = approved_grant(&mut first, "Writer", &[SHEETS]).await;
    spawn(&mut first, &grant, new_sheet("Budget")).await;
    creator
        .until_sheets(&mut first, &["Sheet1", "Budget"])
        .await;
    joined
        .until_sheets(&mut second, &["Sheet1", "Budget"])
        .await;

    next_task(&mut first, &grant, new_sheet("Q4")).await;
    creator
        .until_sheets(&mut first, &["Sheet1", "Budget", "Q4"])
        .await;
    joined
        .until_sheets(&mut second, &["Sheet1", "Budget", "Q4"])
        .await;
    stop(hosted).await;
}

/// POST `body` to `url` (plain HTTP on loopback) and return the answer body.
fn post_local(url: &str, body: &str) -> String {
    use std::io::{Read as _, Write as _};
    let rest = url.strip_prefix("http://").unwrap();
    let (authority, path) = rest.split_at(rest.find('/').unwrap());
    let mut stream = std::net::TcpStream::connect(authority).unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_owned()
}

/// Metered Cloud AI plans an agent's task end to end: the server starts with
/// `--agent-model cloud:<machine AI base url>` against a local Cloud proxy
/// (`deployment/aws/scripts/local_ai_proxy.py` in the Cloud repo), the task
/// completes on the machine's credits, and once they are gone the next task
/// fails saying so. Only when `TILO_E2E_CLOUD_AGENT_MODEL`,
/// `TILO_AGENT_API_KEY_FILE` and `TILO_E2E_CLOUD_DEV_URL` are set.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_plans_on_metered_cloud_credits_until_they_run_out() {
    let (Ok(spec), Ok(key_file), Ok(dev)) = (
        std::env::var("TILO_E2E_CLOUD_AGENT_MODEL"),
        std::env::var("TILO_AGENT_API_KEY_FILE"),
        std::env::var("TILO_E2E_CLOUD_DEV_URL"),
    ) else {
        eprintln!("skipped: no local Cloud AI proxy configured");
        return;
    };
    let model = ModelChoice::parse(&spec, None, ApiKey::File(key_file.into())).unwrap();
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let home = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start_with(&root, &home, state.path(), model).await;
    let _approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &home).await;
    let mut page = Page::create(&browser, SHEETS);
    page.until_sheets(&mut browser, &["Sheet1"]).await;
    browser.durable().await;

    let grant = approved_grant(&mut browser, "Writer", &[SHEETS]).await;
    spawn(
        &mut browser,
        &grant,
        "Add a new sheet named \"Budget\"".to_owned(),
    )
    .await;
    let status = browser
        .until("the metered task to finish", |status| {
            status.agents.iter().any(|agent| {
                agent.id == grant
                    && agent.tasks.first().is_some_and(|task| {
                        matches!(task.state, TaskState::Completed | TaskState::Failed)
                    })
            })
        })
        .await;
    let first = &status
        .agents
        .iter()
        .find(|agent| agent.id == grant)
        .unwrap()
        .tasks[0];
    eprintln!("first task: {first:?}");
    assert_eq!(first.state, TaskState::Completed, "{first:?}");
    page.until_sheets(&mut browser, &["Sheet1", "Budget"]).await;

    let balance = post_local(&format!("{dev}/_dev/balance"), r#"{"availableMicros":0}"#);
    eprintln!("balance now: {balance}");
    next_task(
        &mut browser,
        &grant,
        "Add a new sheet named \"Q4\"".to_owned(),
    )
    .await;
    let status = browser
        .until("the unfunded task to fail", |status| {
            status.agents.iter().any(|agent| {
                agent.id == grant
                    && agent
                        .tasks
                        .get(1)
                        .is_some_and(|task| task.state == TaskState::Failed)
            })
        })
        .await;
    let failed = &status
        .agents
        .iter()
        .find(|agent| agent.id == grant)
        .unwrap()
        .tasks[1];
    eprintln!("second task: {failed:?}");
    assert!(
        failed
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("out of AI credits"),
        "{failed:?}"
    );
    stop(hosted).await;
}

/// The person's own device: a root holder's approver session that answers
/// the questions the server's agents put to their person, as the test says,
/// and the endpoint it lives on.
async fn person_device(server: &Server, laptop: &Device) -> (iroh::endpoint::Endpoint, Approver) {
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let approver = Approver::connect(
        &endpoint,
        server.node_addr(),
        laptop.cert.clone(),
        &laptop.key,
        &RevocationList::new(),
    )
    .await
    .unwrap();
    (endpoint, approver)
}

/// The one question an agent is waiting on, once it is asked.
async fn asked(device: &mut Approver) -> PendingConfirmation {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut asked = device.list_confirmations().await.unwrap();
        if let Some(question) = asked.pop() {
            assert!(asked.is_empty(), "one question per invocation: {asked:?}");
            return question;
        }
        assert!(Instant::now() < deadline, "the agent never asked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Put a book on the shelf, then take it off again: Undo puts the row back
/// but not this device's access to the book's file, so its contract asks a
/// person first (`#[action(irreversible)]`). The shelf shows the book on it
/// when the second call asks to remove it.
fn shelve_then_remove() -> String {
    let mut shelf = arguments(&[
        ("book", serde_json::json!("book:dusk")),
        ("title", serde_json::json!("Dusk")),
        ("author", serde_json::json!("Ada")),
        ("shared", serde_json::json!(false)),
    ]);
    shelf.insert(TOPIC_ARGUMENT.to_owned(), serde_json::json!(""));
    task(&[
        doing("ebook#open", serde_json::json!({})),
        doing_args("ebook#shelve", shelf),
        doing(
            "ebook/1 book#remove-from-library",
            serde_json::json!({"book": "book:dusk"}),
        ),
    ])
}

/// An agent the server starts never confirms by its own word: an action that
/// asks first is put to the person's approving device as the server's
/// question for that one invocation, while the agent shows it is waiting.
/// Allowed once it acts, and only that once; denied it does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosted_agent_asks_the_persons_device_before_an_action_that_asks_first() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let home = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &home, state.path()).await;
    let _approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let (_endpoint, mut device) = person_device(&hosted.server, &laptop).await;
    let mut browser = Browser::open(&hosted, &browser_device, &home).await;

    let grant = approved_grant(&mut browser, "Careful", &[EBOOK]).await;
    spawn(&mut browser, &grant, shelve_then_remove()).await;

    // The person's device is asked about this one invocation, in the words
    // of the document that offers it, while the agent shows it is waiting.
    let question = asked(&mut device).await;
    assert_eq!(
        question.agent_label, "Careful",
        "the agent by the name the person approved"
    );
    assert_eq!(
        question.workspace, "Workspace",
        "the workspace by the name people know it"
    );
    // The action as the reader offers it, not the agent's own words.
    assert_eq!(question.action_label, "Remove from library");
    // The document by the name its app gives the state it holds: the book the
    // agent shelved is on the shelf when it asks to remove it, and one of them
    // is a book.
    assert_eq!(question.resource_label, "1 book");
    assert_eq!(question.risk, "destructive");
    assert!(
        question.question().starts_with("Careful wants to ")
            && question.question().ends_with(", for you"),
        "{}",
        question.question()
    );
    let waiting = browser
        .until("the agent to wait for its person", |status| {
            status.agents.iter().any(|agent| {
                agent.id == grant
                    && agent
                        .tasks
                        .last()
                        .is_some_and(|task| task.state == TaskState::Running)
                    && agent
                        .activity
                        .as_deref()
                        .is_some_and(|activity| activity.contains("remove-from-library"))
            })
        })
        .await;
    let agent = waiting
        .agents
        .iter()
        .find(|agent| agent.id == grant)
        .unwrap();
    assert_eq!(agent.tasks.last().unwrap().state, TaskState::Running);

    // Allowed once: the step happens, and the answer is spent.
    assert_eq!(
        device
            .answer(question.id, ConfirmationAnswer::AllowOnce)
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    browser
        .until(
            "the allowed task to complete",
            last_task_is(&grant, TaskState::Completed, 1),
        )
        .await;
    assert!(matches!(
        device
            .answer(question.id, ConfirmationAnswer::AllowOnce)
            .await
            .unwrap(),
        ApproverReply::Refused { .. }
    ));

    // The next time is a new question; denied, nothing happens.
    next_task(&mut browser, &grant, shelve_then_remove()).await;
    let again = asked(&mut device).await;
    assert_ne!(again.id, question.id, "every invocation is asked anew");
    assert_eq!(
        device
            .answer(again.id, ConfirmationAnswer::Deny)
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    let status = browser
        .until(
            "the denied task to end",
            last_task_is(&grant, TaskState::Completed, 2),
        )
        .await;
    let agent = status
        .agents
        .iter()
        .find(|agent| agent.id == grant)
        .unwrap()
        .clone();
    let denied = agent.tasks.last().unwrap();
    // The removal is the step the person refused, and its words are the
    // answer the agent ends with.
    let refused = denied
        .steps
        .iter()
        .find(|step| step.action == "remove-from-library")
        .unwrap();
    assert_eq!(refused.state, TaskState::Failed, "{denied:?}");
    assert!(
        refused
            .error
            .as_deref()
            .is_some_and(|error| error.contains("did not allow")),
        "{denied:?}"
    );
    assert!(
        last_words(&agent).contains("did not allow"),
        "{:?}",
        agent.conversation
    );
    device.close();
    stop(hosted).await;
}

/// The agent's last word to its person.
fn last_words(agent: &AgentInfo) -> String {
    agent
        .conversation
        .last()
        .map(|message| message.text.clone())
        .unwrap_or_default()
}

/// Two agents talk in the workspace's Chat. Its messages are kept in parts
/// beside the chat itself: what one agent posts goes out on its part's own
/// route, the other, holding the chat, has it there, and reads its author as
/// the agent its person's grant named, never by its key.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_post_in_the_workspaces_chat_and_read_each_other_by_name() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let home = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &home, state.path()).await;
    let _approver = approve_agents(&hosted.server, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &home).await;
    let reader = approved_grant(&mut browser, "Reader", &[CHAT]).await;
    let poster = approved_grant(&mut browser, "Poster", &[CHAT]).await;
    // The conversation every chat opens with: `#chat`, under its fixed id.
    let conversation = "c_chat";

    // The reader starts the workspace's chat (its first conversations) and holds it.
    spawn(
        &mut browser,
        &reader,
        task(&[doing("chat#begin-chat", serde_json::json!({}))]),
    )
    .await;
    browser
        .until(
            "the chat to begin",
            last_task_is(&reader, TaskState::Completed, 1),
        )
        .await;

    // The poster, which never started the chat, posts in it.
    let mut post = arguments(&[("text", serde_json::json!("Hello from the poster"))]);
    post.insert(
        CONVERSATION_ARGUMENT.to_owned(),
        serde_json::json!(conversation),
    );
    spawn(
        &mut browser,
        &poster,
        task(&[doing_args("chat/Messages#post", post)]),
    )
    .await;
    browser
        .until("the post", last_task_is(&poster, TaskState::Completed, 1))
        .await;

    // The reader exports from the chat it holds: the post is there, under the
    // poster's name. Asked again until it has arrived.
    let mut export_arguments = arguments(&[("format", serde_json::json!("json"))]);
    export_arguments.insert(
        CONVERSATION_ARGUMENT.to_owned(),
        serde_json::json!(conversation),
    );
    let export = task(&[doing_args("chat#export-content", export_arguments)]);
    let deadline = tilo_test_env::hang_deadline();
    let mut tasks = 1;
    loop {
        next_task(&mut browser, &reader, export.clone()).await;
        tasks += 1;
        let status = browser
            .until(
                "the export",
                last_task_is(&reader, TaskState::Completed, tasks),
            )
            .await;
        let agent = status
            .agents
            .iter()
            .find(|agent| agent.id == reader)
            .unwrap()
            .clone();
        let reference = content_in(&last_words(&agent));
        let exported: serde_json::Value =
            serde_json::from_slice(&hosted.agents.content(WORKSPACE, &reference).unwrap()).unwrap();
        let posted = exported["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["text"] == "Hello from the poster")
            .cloned();
        if let Some(posted) = posted {
            // The agent wrote it as the agent its person's grant named, by the name they
            // approved: not its key, and never another agent of the same person.
            assert_eq!(posted["authorName"], "Poster", "{posted}");
            assert_eq!(posted["author"]["agent"], poster.as_str(), "{posted}");
            assert_ne!(posted["author"]["agent"], reader.as_str(), "{posted}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the reader never had the post: {exported}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stop(hosted).await;
}
