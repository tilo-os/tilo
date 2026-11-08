//! Agents hosted by `tilo-relay` itself, driven from a browser session.
//!
//! The host runs `tilo_relay::agents::start`: an [`AgentHost`] whose
//! supervisor is the host's app-serving plane, one thread per agent, state
//! under a directory. A simulated browser (the page's session code built
//! natively: HTTPS for documents and remote actions) asks for a grant; a
//! root-holding laptop approves the agent and signs its delegation in the
//! same step; the browser spawns the agent with a plain-words task planned by
//! the scripted model. The agent edits Notes and Sheets, and the browser
//! receives the edits in the record each one lands in. The host then restarts: the agent
//! resumes and takes new work; a stopped agent stays stopped; a revoked
//! grant ends its agent. An action that asks first is put to the person's
//! approving device for that one invocation, and runs only on its answer.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::future_not_send,
    clippy::arithmetic_side_effects,
    clippy::similar_names,
    clippy::float_cmp,
    clippy::type_complexity,
    reason = "in a test these are the assertion, not a hazard; one scenario in order"
)]

#[path = "../../../browser/tilo-browser-peer/tests/support/participant.rs"]
mod participant;
#[path = "../../../browser/tilo-browser-peer/tests/support/mod.rs"]
mod support;

mod common;
mod harness;

// Agents hold each record in a confined app worker (this binary, started
// again), whose changes are admitted through the Automerge isolate.
tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use std::path::Path;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::remote::InvokeArgument;
use tilo_agent_participant::{ModelChoice, Replica, ReplicaActor, Seed};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay::certify::{ApproverReply, ConfirmationAnswer};
use tilo_relay::daemon::Relay;
use tilo_relay_client::Approver;
use tilo_relay_protocol::agents::{
    AgentStatus, AgentsStatus, GrantState, MESSAGE_ACTION, REQUEST_GRANT_ACTION, REVOKE_ACTION,
    SPAWN_ACTION, STOP_ACTION, SUPERVISOR_RESOURCE, TaskState, agent_resource,
};

use tilo_user_identity::{DeviceCapabilities, RevocationList};

use harness::hosted::{Hosted, Laptop, Options, Unlisted};
use harness::page::Page as Browser;
use support::{Device, WORKSPACE, enrol, workspace};

const NOTES: &str = "so.tilo.notes";
const SHEETS: &str = "so.tilo.sheets";
const EBOOK: &str = "so.tilo.ebook";

/// The host, offering every app the tests drive.
async fn start(root: &SigningKey, host: &Device, state: &Path) -> Hosted {
    start_with(root, host, state, ModelChoice::Scripted).await
}

async fn start_with(root: &SigningKey, host: &Device, state: &Path, model: ModelChoice) -> Hosted {
    harness::hosted::start(
        root,
        host,
        state,
        Options::offering(common::catalogue, model),
    )
    .await
}

/// Stop the host, after checking it carried nothing in the clear.
async fn stop(hosted: Hosted) {
    let retained = hosted
        .relay
        .workspace_records()
        .retained(workspace())
        .await
        .unwrap();
    let (readable, _) = common::member_documents(
        retained
            .into_iter()
            .map(|retained| (retained.author, retained.body)),
        &hosted.device,
    );
    assert_eq!(readable, 0, "nothing reaches the host in the clear");
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}

/// The laptop: a root holder answering every agent request with the
/// agent's certificate and the delegation of exactly what was proposed. The
/// questions an agent puts to its person are the test's to answer.
fn approve_agents(relay: &Relay, root: SigningKey, laptop: &Device) -> tokio::task::JoinHandle<()> {
    Laptop::new([], Unlisted::Pending).spawn(relay, root, laptop)
}

impl Browser {
    /// The document of every `app` record the page received, as JSON, merged
    /// from the changes `author` made.
    fn bodies(&mut self, app: &str, author: [u8; 32]) -> Vec<String> {
        self.pump();
        self.documents
            .iter()
            .filter(|(route, _)| route.app_id == app)
            .map(|(route, changes)| {
                assert!(
                    changes.iter().all(|(from, _)| *from == author),
                    "only the agent edits {app} here"
                );
                // Until the change that sets the record up arrives, the app
                // refuses to open what came before it.
                let page = match common::open(
                    app,
                    "ada",
                    ReplicaActor::Owner,
                    Seed::Existing {
                        record: route_record(&route.record),
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
                    .map(|(route, changes)| (route.record.clone(), changes.len()))
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
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = support::scratch_dir();

    let hosted = start(&root, &host, state.path()).await;

    let mut channels = participant::Participant::start(&hosted.relay, &root).await;
    let approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;

    // 1. One consent step: the page asks, the laptop approves the agent
    // together with the delegation of what was asked.
    let (grant, requested) = request_grant(&mut browser, "Writer", &[NOTES, SHEETS]).await;
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
    assert_eq!(offered.apps, [NOTES, SHEETS]);
    assert!(offered.pairing_code.is_none());

    // 2. The page spawns the agent with plain words; the scripted model
    // plans them against what Notes publishes once it is open.
    let spawned = browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({
                    "grant": grant,
                    "task": "Create a new note titled \"Groceries\"",
                    "room": WORKSPACE,
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
    let steps: Vec<_> = info.tasks[0]
        .steps
        .iter()
        .map(|step| step.action.as_str())
        .collect();
    // The workspace has no Notes record yet: the step names what a new one
    // publishes and opens that record itself.
    assert_eq!(steps, ["new-note"]);
    let agent_key: [u8; 32] = hex::decode(&info.device).unwrap().try_into().unwrap();
    browser.until_edited(NOTES, agent_key, "Groceries").await;

    // 3. The browser sees the agent's edit in the record it works in, then
    // in the next app's.
    browser
        .call(
            &agent_resource(&grant),
            MESSAGE_ACTION,
            InvokeArgument::Text("Add a new sheet named \"Budget\"".into()),
        )
        .await
        .unwrap();
    browser.until_edited(SHEETS, agent_key, "Budget").await;

    // 4. The host restarts. The agent resumes from the state directory and
    // edits the workspace's existing sheet, not a new one.
    browser.peer.close();
    hosted.agents.flush();
    let saved = std::fs::read_to_string(hosted.agents.state_file()).unwrap();
    assert!(saved.contains(&grant) && saved.contains("Budget"));
    approver.abort();
    stop(hosted).await;

    let hosted = start(&root, &host, state.path()).await;

    channels.reconnect(&hosted.relay, &root).await;
    let approver = approve_agents(&hosted.relay, root.clone(), &laptop);
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
    let sheets_before = browser.bodies(SHEETS, agent_key).len();
    browser
        .call(
            &agent_resource(&grant),
            MESSAGE_ACTION,
            InvokeArgument::Text("Add a new sheet named \"Forecast\"".into()),
        )
        .await
        .unwrap();
    browser.until_edited(SHEETS, agent_key, "Forecast").await;
    let bodies = browser.bodies(SHEETS, agent_key);
    assert_eq!(
        bodies.len(),
        sheets_before.max(1),
        "it reopened the workspace's sheet"
    );
    assert!(
        bodies
            .iter()
            .any(|body| body.contains("Budget") && body.contains("Forecast"))
    );

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

    channels.reconnect(&hosted.relay, &root).await;
    let _approver = approve_agents(&hosted.relay, root.clone(), &laptop);
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
                serde_json::json!({"grant": grant, "task": "Create a new note titled \"Later\""})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    browser.until_edited(NOTES, agent_key, "Later").await;
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
fn route_record(record: &str) -> ui::record::RecordId {
    if record.is_empty() {
        ui::record::RecordId::package_primary()
    } else {
        ui::record::RecordId::parse(record).unwrap()
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
        let mut page = common::open(app, "ada", ReplicaActor::Owner, Seed::New).unwrap();
        let route = DocumentRoute {
            app_id: app.into(),
            cluster_id: WORKSPACE.into(),
            record: page.wire_record().to_owned(),
        };
        browser
            .peer
            .publish(route.clone(), &page.take_mutation().unwrap())
            .unwrap();
        Self {
            app: page,
            route,
            merged: 0,
        }
    }

    /// Open the record `creator` has open from another session, once its
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
                let mut app = common::open(
                    SHEETS,
                    "bea",
                    ReplicaActor::Owner,
                    Seed::Existing {
                        record: route_record(&creator.route.record),
                        changes: changes.iter().map(|(_, bytes)| bytes.clone()).collect(),
                    },
                )
                .unwrap();
                if let Some(own) = app.take_mutation() {
                    browser.peer.publish(creator.route.clone(), &own).unwrap();
                }
                return Self {
                    app,
                    route: creator.route.clone(),
                    merged: changes.len(),
                };
            }
            assert!(
                Instant::now() < deadline,
                "the record never reached the second session"
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
                    "the page shows {:?}, not {want:?}: agents {:#?}, browser channels {}",
                    sheet_names(&self.app),
                    status.agents,
                    browser.peer.status().channels
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// An agent's later task edits the record people have open as live as its
/// first one did, and a session that joins afterwards replays both edits
/// once each.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_browser_sees_every_task_of_an_agent_live_and_after_a_reload() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let _approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;

    // The person opens a workbook before any agent exists, and it is in the
    // workspace (sealed and durable on the host) before the agent arrives.
    let mut page = Page::create(&browser, SHEETS);
    page.until_sheets(&mut browser, &["Sheet1"]).await;
    browser.durable().await;

    let grant = approved_grant(&mut browser, "Writer", &[SHEETS]).await;
    browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({
                    "grant": grant,
                    "task": "Add a new sheet named \"Budget\"",
                    "room": WORKSPACE,
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .unwrap();
    page.until_sheets(&mut browser, &["Sheet1", "Budget"]).await;

    browser
        .call(
            &agent_resource(&grant),
            MESSAGE_ACTION,
            InvokeArgument::Text("Add a new sheet named \"Q4\"".into()),
        )
        .await
        .unwrap();
    page.until_sheets(&mut browser, &["Sheet1", "Budget", "Q4"])
        .await;
    browser
        .until("both tasks to complete", |status| {
            status.agents.iter().any(|agent| {
                agent.id == grant
                    && agent.tasks.len() == 2
                    && agent
                        .tasks
                        .iter()
                        .all(|task| task.state == TaskState::Completed)
            })
        })
        .await;

    // A reload: a new session replays the workspace and rebuilds the workbook
    // from what the host kept.
    let reloaded_device = enrol(&root, "browser, reloaded", DeviceCapabilities::member());
    let mut reloaded = Browser::open(&hosted, &reloaded_device, &host).await;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        reloaded.pump();
        if let Some(changes) = reloaded
            .documents
            .get(&page.route)
            .filter(|c| !c.is_empty())
        {
            let replayed = common::open(
                SHEETS,
                "ada",
                ReplicaActor::Owner,
                Seed::Existing {
                    record: route_record(&page.route.record),
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
    stop(hosted).await;
}

/// The page that created a record and a page that joined it both see an
/// agent's first and second task live: nothing the joined mount does may
/// leave the creator behind a change it never received.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_creator_and_a_joined_page_both_see_every_task_live() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let first_device = enrol(&root, "browser", DeviceCapabilities::member());
    let second_device = enrol(&root, "second browser", DeviceCapabilities::member());
    let state = support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let _approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    let mut first = Browser::open(&hosted, &first_device, &host).await;
    let mut second = Browser::open(&hosted, &second_device, &host).await;

    let mut creator = Page::create(&first, SHEETS);
    creator.until_sheets(&mut first, &["Sheet1"]).await;
    let mut joined = Page::join(&mut second, &creator).await;
    joined.until_sheets(&mut second, &["Sheet1"]).await;

    let grant = approved_grant(&mut first, "Writer", &[SHEETS]).await;
    first
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({
                    "grant": grant,
                    "task": "Add a new sheet named \"Budget\"",
                    "room": WORKSPACE,
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .unwrap();
    creator
        .until_sheets(&mut first, &["Sheet1", "Budget"])
        .await;
    joined
        .until_sheets(&mut second, &["Sheet1", "Budget"])
        .await;

    first
        .call(
            &agent_resource(&grant),
            MESSAGE_ACTION,
            InvokeArgument::Text("Add a new sheet named \"Q4\"".into()),
        )
        .await
        .unwrap();
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

/// Metered Cloud AI plans an agent's task end to end: the host runs with
/// `--agent-model cloud:<machine AI base url>` against a local Cloud proxy
/// (`deployment/aws/scripts/local_ai_proxy.py` in the Cloud repo), the task
/// completes on the machine's credits, and once they are gone the next task
/// fails saying so. Runs only when `TILO_E2E_CLOUD_AGENT_MODEL`,
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
    let model = ModelChoice::parse(
        &spec,
        None,
        tilo_agent_participant::ApiKey::File(key_file.into()),
    )
    .unwrap();
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = support::scratch_dir();
    let hosted = start_with(&root, &host, state.path(), model).await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let _approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let mut page = Page::create(&browser, SHEETS);
    page.until_sheets(&mut browser, &["Sheet1"]).await;
    browser.durable().await;

    let grant = approved_grant(&mut browser, "Writer", &[SHEETS]).await;
    browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({"grant": grant, "task": "Add a new sheet named \"Budget\"", "room": WORKSPACE})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
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
    browser
        .call(
            &agent_resource(&grant),
            MESSAGE_ACTION,
            InvokeArgument::Text("Add a new sheet named \"Q4\"".into()),
        )
        .await
        .unwrap();
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
/// the questions the host's agents put to their person, as the test says,
/// and the endpoint it lives on.
async fn person_device(relay: &Relay, laptop: &Device) -> (iroh::endpoint::Endpoint, Approver) {
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let approver = Approver::connect(
        &endpoint,
        relay.node_addr(),
        laptop.cert.clone(),
        &laptop.key,
        &RevocationList::new(),
    )
    .await
    .unwrap();
    (endpoint, approver)
}

/// The one question an agent is waiting on, once it is asked.
async fn asked(device: &mut Approver) -> tilo_relay::certify::PendingConfirmation {
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

fn last_task_is(grant: &str, state: TaskState, tasks: usize) -> impl Fn(&AgentsStatus) -> bool {
    move |status| {
        status.agents.iter().any(|agent| {
            agent.id == grant
                && agent.tasks.len() == tasks
                && agent.tasks.last().is_some_and(|task| task.state == state)
        })
    }
}

/// Put a book on the shelf, then take it off again: Undo puts the row back
/// but not this device's access to the book's file, so its contract asks a
/// person first (`#[action(irreversible)]`). Clearing the calculator's
/// history no longer does: Undo takes that back, so it runs at once.
fn shelve_then_remove() -> String {
    serde_json::json!([
        {
            "app": EBOOK,
            "resource": "ebook.library",
            "action": "shelve",
            "arguments": {
                "book": "book:dusk",
                "title": "Dusk",
                "author": "Ada",
                "subject": "",
                "shared": false,
            },
        },
        {
            "app": EBOOK,
            "resource": "ebook.library",
            "action": "remove-from-library",
            "arguments": {"book": "book:dusk"},
        },
    ])
    .to_string()
}

/// An agent the host runs never confirms by its own word: an action that
/// asks first is put to the person's approving device as the host's
/// question for that one invocation, while the agent shows it is waiting.
/// Allowed once it runs, and only that once; denied it does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosted_agent_asks_the_persons_device_before_an_action_that_asks_first() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let _approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    let (_endpoint, mut device) = person_device(&hosted.relay, &laptop).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;

    let grant = approved_grant(&mut browser, "Careful", &[EBOOK]).await;
    browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({ "grant": grant, "task": shelve_then_remove(), "room": WORKSPACE })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();

    // The person's device is asked about this one invocation, in the words
    // of the record that offers it, while the agent shows it is waiting.
    let question = asked(&mut device).await;
    assert_eq!(question.agent_id, grant);
    assert_eq!(question.agent_label, "Careful");
    assert_eq!(question.workspace, WORKSPACE);
    // The action as the reader offers it, not the agent's own words.
    assert_eq!(question.action_label, "Remove from library");
    assert_eq!(question.resource_label, "Books");
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
                        .activity
                        .as_deref()
                        .is_some_and(|activity| activity.starts_with("Waiting for confirmation"))
            })
        })
        .await;
    let agent = waiting
        .agents
        .iter()
        .find(|agent| agent.id == grant)
        .unwrap();
    assert_eq!(agent.tasks.last().unwrap().state, TaskState::Running);

    // Allowed once: the step runs, and the answer is spent.
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

    // The next time is a new question; denied, nothing runs.
    browser
        .call(
            &agent_resource(&grant),
            MESSAGE_ACTION,
            InvokeArgument::Text(shelve_then_remove()),
        )
        .await
        .unwrap();
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
            "the denied task to fail",
            last_task_is(&grant, TaskState::Failed, 2),
        )
        .await;
    let denied = status
        .agents
        .iter()
        .find(|agent| agent.id == grant)
        .and_then(|agent| agent.tasks.last().cloned())
        .unwrap();
    assert!(
        denied
            .error
            .as_deref()
            .is_some_and(|error| error.contains("confirmation_required")),
        "{denied:?}"
    );
    device.close();
    stop(hosted).await;
}

const CHAT: &str = "so.tilo.chat";

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
                serde_json::json!({"label": label, "rooms": [WORKSPACE], "apps": apps,
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

/// One written step of the workspace's Chat.
fn chat_step(resource: &str, action: &str, arguments: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"app": CHAT, "resource": resource, "action": action, "arguments": arguments})
}

/// Two agents talk in the workspace's Chat. Its messages are kept in parts
/// beside the chat record: what one agent posts goes out on its part's own
/// route, the other, holding the chat record, has it there, and the chat
/// names it by the label the poster's person gave it, which the poster
/// introduced itself with on opening the chat.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_post_in_the_workspaces_chat_and_read_each_other_by_name() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&root, &host, state.path()).await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let _approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let reader = approved_grant(&mut browser, "Reader", &[CHAT]).await;
    let poster = approved_grant(&mut browser, "Poster", &[CHAT]).await;
    let channel = chat::record::CHAT_CHANNEL;

    // The reader starts the workspace's chat (its first channels) and holds it.
    let begin = serde_json::json!([chat_step(
        "chat.workspace",
        "begin-chat",
        &serde_json::json!({"name": "Reader", "initials": "RE", "color": 1}),
    )]);
    let spawn = |grant: &str, task: &serde_json::Value| {
        InvokeArgument::Json(
            serde_json::json!({"grant": grant, "task": task.to_string(), "room": WORKSPACE})
                .as_object()
                .unwrap()
                .clone(),
        )
    };
    browser
        .call(SUPERVISOR_RESOURCE, SPAWN_ACTION, spawn(&reader, &begin))
        .await
        .unwrap();
    browser
        .until(
            "the chat to begin",
            last_task_is(&reader, TaskState::Completed, 1),
        )
        .await;

    // The poster, which never started the chat, posts in it.
    let post = serde_json::json!([chat_step(
        "chat.messages",
        "post",
        &serde_json::json!({"channel": channel, "text": "Hello from the poster"}),
    )]);
    browser
        .call(SUPERVISOR_RESOURCE, SPAWN_ACTION, spawn(&poster, &post))
        .await
        .unwrap();
    browser
        .until("the post", last_task_is(&poster, TaskState::Completed, 1))
        .await;

    // The reader exports the channel from the chat it holds: the post is
    // there, under the poster's name. Asked again until it has arrived.
    let export = serde_json::json!([chat_step(
        "chat.workspace",
        "export-content",
        &serde_json::json!({"channel": channel, "format": "json"}),
    )])
    .to_string();
    let deadline = tilo_test_env::hang_deadline();
    let mut tasks = 1;
    loop {
        browser
            .call(
                &agent_resource(&reader),
                MESSAGE_ACTION,
                InvokeArgument::Text(export.clone()),
            )
            .await
            .unwrap();
        tasks += 1;
        let status = browser
            .until(
                "the export",
                last_task_is(&reader, TaskState::Completed, tasks),
            )
            .await;
        let task = status
            .agents
            .iter()
            .find(|agent| agent.id == reader)
            .and_then(|agent| agent.tasks.last().cloned())
            .unwrap();
        let address = task
            .steps
            .iter()
            .flat_map(|step| step.content.iter())
            .next_back()
            .cloned()
            .expect("the export is kept as content");
        let exported: serde_json::Value =
            serde_json::from_slice(&hosted.agents.content(WORKSPACE, &address).unwrap()).unwrap();
        let posted = exported["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["text"] == "Hello from the poster")
            .cloned();
        if let Some(posted) = posted {
            assert_eq!(posted["authorName"], "Poster", "{posted}");
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
