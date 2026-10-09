//! Agents hosted by `tilo-relay` itself, driven from a browser session.
//!
//! The host runs `tilo_relay::agents::start`: an [`AgentHost`] whose
//! supervisor is the host's app-serving plane, one thread per agent, state
//! under a directory. A simulated browser (the page's session code built
//! natively: HTTPS for documents and remote actions, WebTransport for
//! presence) asks for a grant; a root-holding laptop approves the agent and
//! signs its delegation in the same step; the browser spawns the agent with
//! a plain-words task planned by the scripted model. The agent edits Notes
//! and Sheets, the browser receives the edits and sees the agent's presence
//! move between the records it works in. The host then restarts: the agent
//! resumes and takes new work; a stopped agent stays stopped; a revoked
//! grant ends its agent.

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

#[path = "../../../browser/tilo-browser-peer/tests/support/steward.rs"]
mod steward;
#[path = "../../../browser/tilo-browser-peer/tests/support/mod.rs"]
mod support;

mod common;

// Agents hold each record in a confined app worker (this binary, started
// again), whose changes are admitted through the Automerge isolate.
tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::InvocationId;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_agent_participant::{AgentHost, ModelChoice, Replica, Seed};
use tilo_browser_peer::{DocumentRoute, Host, PeerEvent, PeerEvents, PeerHandle};
use tilo_relay::certify::{ApproverDecision, ApproverReply};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_relay_client::Approver;
use tilo_relay_protocol::agents::{
    AGENTS_PROVIDER, AgentStatus, AgentsStatus, GrantState, MESSAGE_ACTION, REQUEST_GRANT_ACTION,
    REVOKE_ACTION, SPAWN_ACTION, STATUS_ACTION, STOP_ACTION, SUPERVISOR_RESOURCE, TaskState,
    agent_resource,
};
use tilo_relay_protocol::live::{LiveFrame, ParticipantKind, PresenceFrame};
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, RevocationList, issue_device_cert,
};

use support::{Device, ROOM, enrol, ready, start_peer, workspace};

const NOTES: &str = "so.tilo.notes";
const SHEETS: &str = "so.tilo.sheets";

struct Hosted {
    relay: Relay,
    web: WebHandle,
    agents: AgentHost,
}

async fn start(root: &SigningKey, host: &Device, state: &Path) -> Hosted {
    start_with(root, host, state, ModelChoice::Scripted).await
}

async fn start_with(root: &SigningKey, host: &Device, state: &Path, model: ModelChoice) -> Hosted {
    let user = root.verifying_key().to_bytes();
    let mut cfg = tilo_relay::RelayConfig {
        state_root: state.join("relay"),
        ..Default::default()
    };
    cfg.workspaces.push(tilo_relay::WorkspaceBinding {
        id: workspace(),
        pin: true,
        replication: tilo_relay::ReplicationIntent::FullWorkspace,
        admitted_users: vec![hex::encode(user)],
        checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
        shell_authority: None,
    });
    let (relay, agents) = tilo_relay::agents::start(
        cfg,
        RelayIdentity {
            user_public_key: user,
            device_signing_key: host.key.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
        tilo_relay::agents::AgentOptions {
            state_dir: state.join("agents"),
            model,
            catalogue: Some(Arc::new(common::catalogue)),
        },
    )
    .await
    .unwrap();
    let web = relay
        .serve_web(WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: TlsSource::Development {
                hosts: vec!["localhost".into(), "127.0.0.1".into()],
                store: Some(state.join("web-dev-identity")),
            },
            allowed_origins: Vec::new(),
            plain_listen: None,
        })
        .await
        .unwrap();
    Hosted { relay, web, agents }
}

impl Hosted {
    /// Stop the host, after checking it carried nothing in the clear.
    async fn stop(self) {
        let retained = self
            .relay
            .room_records()
            .retained(workspace())
            .await
            .unwrap();
        let (readable, _) = common::retained_documents(
            retained
                .into_iter()
                .map(|retained| (retained.author, retained.body)),
        );
        assert_eq!(readable, 0, "nothing reaches the host in the clear");
        drop(self.web);
        self.agents.shutdown();
        self.relay.shutdown().await;
    }
}

/// The laptop: a root holder answering every agent request with the
/// agent's certificate and the delegation of exactly what was proposed.
fn approve_agents(relay: &Relay, root: SigningKey, laptop: &Device) -> tokio::task::JoinHandle<()> {
    let addr = relay.node_addr();
    let cert = laptop.cert.clone();
    let key = laptop.key.clone();
    tokio::spawn(async move {
        let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap();
        let mut approver = Approver::connect(&endpoint, addr, cert, &key, &RevocationList::new())
            .await
            .unwrap();
        loop {
            let proposals = approver.list_agent_proposals().await.unwrap();
            for pending in approver.list_pending().await.unwrap() {
                let Some(agent) = proposals
                    .iter()
                    .find(|agent| agent.request_digest == pending.request_digest)
                else {
                    continue;
                };
                assert!(pending.request.device_label.starts_with("Agent: "));
                let now = tilo_clock::unix_now_ns();
                let cert = issue_device_cert(
                    &pending.request,
                    &root,
                    &CertificationPolicy::agent(),
                    &RevocationList::new(),
                    now,
                )
                .unwrap();
                // A bare certificate is refused: an agent is approved with
                // its delegation or not at all.
                assert!(matches!(
                    approver
                        .decide(
                            pending.request_digest,
                            ApproverDecision::Issue(cert.clone())
                        )
                        .await
                        .unwrap(),
                    ApproverReply::Refused { .. }
                ));
                let delegation = agent
                    .proposal
                    .sign(&root, pending.request.device_public_key, now)
                    .unwrap();
                assert_eq!(
                    approver
                        .decide(
                            pending.request_digest,
                            ApproverDecision::IssueAgent { cert, delegation }
                        )
                        .await
                        .unwrap(),
                    ApproverReply::Decided
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
}

/// A browser session: its peer, and every durable change it received.
struct Browser {
    peer: PeerHandle,
    events: PeerEvents,
    host_key: [u8; 32],
    documents: BTreeMap<DocumentRoute, Vec<([u8; 32], Vec<u8>)>>,
}

impl Browser {
    async fn open(hosted: &Hosted, device: &Device, host: &Device) -> Self {
        let https = support::https::DevHttps::new(
            hosted.web.addr,
            hosted.web.certificate_sha256.as_ref().unwrap(),
        );
        let peer = start_peer(
            Host::Https(https),
            device.cert.clone(),
            host.cert.clone(),
            device.key.clone(),
        );
        let events = peer.events();
        peer.join(workspace()).unwrap();
        ready(&[&peer]).await;
        Self {
            peer,
            events,
            host_key: host.cert.device_public_key,
            documents: BTreeMap::new(),
        }
    }

    fn take(&mut self, event: PeerEvent) -> Option<PeerEvent> {
        if let PeerEvent::Document(document) = &event {
            if let Some(mutation) = document.mutation() {
                let bytes = match mutation {
                    collab_types::DocumentMutation::Snapshot(bytes)
                    | collab_types::DocumentMutation::Incremental(bytes) => bytes,
                };
                self.documents
                    .entry(document.route.clone())
                    .or_default()
                    .push((document.author, bytes));
            }
            return None;
        }
        Some(event)
    }

    fn pump(&mut self) {
        for event in self.events.drain() {
            let _ = self.take(event);
        }
    }

    /// One remote action to the host's agent supervisor.
    async fn call(
        &mut self,
        resource: &str,
        action: &str,
        argument: InvokeArgument,
    ) -> Result<serde_json::Value, String> {
        let invocation = InvocationId::new();
        let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: AGENTS_PROVIDER.into(),
                address: ActionAddress::new(resource, action),
            },
            argument,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
        }))
        .encode()
        .unwrap();
        self.peer
            .send_remote_action(workspace(), self.host_key, frame)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                Instant::now() < deadline,
                "no receipt for {resource}#{action}"
            );
            // Take the whole batch before answering: a document behind the
            // receipt would otherwise be dropped with it.
            let mut answer = None;
            for event in self.events.drain() {
                let Some(event) = self.take(event) else {
                    continue;
                };
                match event {
                    PeerEvent::RemoteAction { from, frame, .. } if from == self.host_key => {
                        if let RemoteActionMessage::Receipt(receipt) =
                            RemoteActionFrame::decode(&frame).unwrap().message
                            && receipt.invocation == invocation
                        {
                            answer = Some(match receipt.status {
                                ReceiptStatus::Completed { output } => {
                                    Ok(output.value.unwrap_or_default())
                                }
                                other => Err(format!("{other:?}")),
                            });
                        }
                    }
                    PeerEvent::RemoteActionUndeliverable { reason, .. } => {
                        panic!("undeliverable: {reason}")
                    }
                    _ => {}
                }
            }
            if let Some(answer) = answer {
                return answer;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn status(&mut self) -> AgentsStatus {
        serde_json::from_value(
            self.call(SUPERVISOR_RESOURCE, STATUS_ACTION, InvokeArgument::None)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    /// Wait until everything this page published is durable on the host.
    async fn durable(&self) {
        let receipt = self.peer.barrier().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match receipt.try_recv() {
                Ok(result) => return result.unwrap(),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(error) => panic!("the barrier ended: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "the page's edits never became durable"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Wait until `done` holds for the supervisor's status.
    async fn until(&mut self, what: &str, done: impl Fn(&AgentsStatus) -> bool) -> AgentsStatus {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = self.status().await;
            if done(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "{what}: {status:#?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The document of every `app` record the browser received, as JSON,
    /// merged from the changes `author` made.
    fn bodies(&mut self, app: &str, author: [u8; 32]) -> Vec<String> {
        self.pump();
        let catalogue = common::catalogue();
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
                let page = match Replica::open(
                    &catalogue,
                    app,
                    ROOM,
                    "ada",
                    false,
                    Seed::Existing {
                        record: ui::record::RecordId::parse(&route.record).unwrap(),
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

    async fn until_edited(&mut self, app: &str, author: [u8; 32], text: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
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

    /// The agent's presence track, over the browser's WebTransport session.
    async fn watch(&self, web: &WebHandle, agent: &str) -> Presence {
        let (grant_tx, grant_rx) = tokio::sync::oneshot::channel();
        self.peer
            .live_grant_then(
                workspace(),
                Box::new(move |grant| {
                    let _ = grant_tx.send(grant);
                }),
            )
            .unwrap();
        let grant = grant_rx.await.unwrap().unwrap();
        let url: url::Url = format!("https://127.0.0.1:{}/moq/live", web.addr.port())
            .parse()
            .unwrap();
        let trust =
            tilo_moq::carrier::Trust::Fingerprints(vec![web.certificate_sha256.clone().unwrap()]);
        let own = tilo_moq::live::namespace_path(&grant.room, &grant.publisher).unwrap();
        let live = tilo_moq::live::peer::Peer::connect(&url, &trust, &grant.token, Some(&own))
            .await
            .unwrap();
        let namespace = tilo_moq::live::namespace_path(&grant.room, agent).unwrap();
        let (frames, rx) = tokio::sync::mpsc::unbounded_channel();
        let peer = self.peer.clone();
        let task = tokio::spawn(async move {
            let mut track = loop {
                if let Ok(Some(broadcast)) =
                    tokio::time::timeout(Duration::from_millis(200), live.member(&namespace)).await
                    && let Ok(track) = broadcast
                        .track(tilo_moq::live::PRESENCE_TRACK)
                        .unwrap()
                        .subscribe(None)
                        .await
                {
                    break track;
                }
            };
            while let Ok(Some(datagram)) = track.recv_datagram().await {
                // Live frames arrive sealed: the page opens them with its keys.
                let (tx, opened) = tokio::sync::oneshot::channel();
                if peer
                    .open_live_then(
                        workspace(),
                        datagram.payload.to_vec(),
                        Box::new(move |frame| {
                            let _ = tx.send(frame);
                        }),
                    )
                    .is_err()
                {
                    break;
                }
                let Ok(Some(frame)) = opened.await else {
                    continue;
                };
                if let Ok(LiveFrame::Presence(presence)) = LiveFrame::decode(&frame)
                    && frames.send(presence).is_err()
                {
                    break;
                }
            }
            drop(live);
        });
        Presence { rx, task }
    }
}

/// The agent's presence frames, as the browser's live session receives them.
struct Presence {
    rx: tokio::sync::mpsc::UnboundedReceiver<PresenceFrame>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Presence {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Presence {
    /// The next presence frame for which `wanted` holds.
    async fn until(
        &mut self,
        what: &str,
        wanted: impl Fn(&PresenceFrame) -> bool,
    ) -> PresenceFrame {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the agent's cursor never showed {what}");
            if let Ok(Some(presence)) = tokio::time::timeout(left, self.rx.recv()).await
                && wanted(&presence)
            {
                return presence;
            }
        }
    }
}

fn activity(presence: &PresenceFrame) -> String {
    presence
        .participant
        .as_ref()
        .and_then(|participant| participant.activity.clone())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_runs_agents_a_browser_spawns_and_they_survive_a_restart() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();

    let hosted = start(&root, &host, state.path()).await;

    let mut steward = steward::Steward::start(&hosted.relay, &root).await;
    let approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;

    // 1. One consent step: the page asks, the laptop approves the agent
    // together with the delegation of what was asked.
    let requested = browser
        .call(
            SUPERVISOR_RESOURCE,
            REQUEST_GRANT_ACTION,
            InvokeArgument::Json(
                serde_json::json!({
                    "label": "Writer",
                    "rooms": [ROOM],
                    "apps": [NOTES, SHEETS],
                    "expiresInSecs": 3600,
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .unwrap();
    let grant = requested["grant"].as_str().unwrap().to_owned();
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
                    "room": ROOM,
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
    assert_eq!(info.room.as_deref(), Some(ROOM));
    let steps: Vec<_> = info.tasks[0]
        .steps
        .iter()
        .map(|step| step.action.as_str())
        .collect();
    assert_eq!(steps, ["open", "new-note"]);
    let agent_key: [u8; 32] = hex::decode(&info.device).unwrap().try_into().unwrap();
    browser.until_edited(NOTES, agent_key, "Groceries").await;

    // 3. The browser sees the agent on the record it works in, with what
    // it is doing, then moving to the next app's record.
    let mut presence = browser.watch(&hosted.web, &info.device).await;
    let on_notes = presence
        .until("its Notes activity", |presence| {
            activity(presence).contains("New note")
        })
        .await;
    let participant = on_notes.participant.clone().unwrap();
    assert_eq!(participant.kind, ParticipantKind::Agent);
    assert_eq!(participant.label, "Writer");
    assert_eq!(on_notes.focus.as_ref().unwrap().app, NOTES);
    browser
        .call(
            &agent_resource(&grant),
            MESSAGE_ACTION,
            InvokeArgument::Text("Add a new sheet named \"Budget\"".into()),
        )
        .await
        .unwrap();
    let on_sheets = presence
        .until("its Sheets activity", |presence| {
            activity(presence).contains("New sheet")
        })
        .await;
    assert_eq!(on_sheets.focus.as_ref().unwrap().app, SHEETS);
    browser.until_edited(SHEETS, agent_key, "Budget").await;
    drop(presence);

    // 4. The host restarts. The agent resumes from the state directory and
    // edits the room's existing sheet, not a new one.
    browser.peer.close();
    hosted.agents.flush();
    let saved = std::fs::read_to_string(hosted.agents.state_file()).unwrap();
    assert!(saved.contains(&grant) && saved.contains("Budget"));
    approver.abort();
    hosted.stop().await;

    let hosted = start(&root, &host, state.path()).await;

    steward.reconnect(&hosted.relay, &root).await;
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
        "it reopened the room's sheet"
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
    hosted.stop().await;

    let hosted = start(&root, &host, state.path()).await;

    steward.reconnect(&hosted.relay, &root).await;
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
    hosted.stop().await;
}

/// The sheets of a workbook, in order.
fn sheet_names(app: &Replica) -> Vec<String> {
    let body: serde_json::Value = app
        .read(|doc| serde_json::to_value(automerge::AutoSerde::from(doc)).unwrap())
        .unwrap();
    body["wb"]["sheets"]
        .as_array()
        .map(|sheets| {
            sheets
                .iter()
                .map(|sheet| sheet["name"].as_str().unwrap_or_default().to_owned())
                .collect()
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
        let mut page =
            Replica::open(&common::catalogue(), app, ROOM, "ada", false, Seed::New).unwrap();
        let route = DocumentRoute {
            app_id: app.into(),
            cluster_id: ROOM.into(),
            record: page.record().to_owned(),
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
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            browser.pump();
            if let Some(changes) = browser
                .documents
                .get(&creator.route)
                .filter(|c| !c.is_empty())
            {
                let mut app = Replica::open(
                    &common::catalogue(),
                    SHEETS,
                    ROOM,
                    "bea",
                    false,
                    Seed::Existing {
                        record: ui::record::RecordId::parse(&creator.route.record).unwrap(),
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
        let deadline = Instant::now() + Duration::from_secs(30);
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
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&root, &host, state.path()).await;
    let _steward = steward::Steward::start(&hosted.relay, &root).await;
    let _approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;

    // The person opens a workbook before any agent exists, and it is in the
    // room (sealed and durable on the host) before the agent arrives.
    let mut page = Page::create(&browser, SHEETS);
    page.until_sheets(&mut browser, &["Sheet1"]).await;
    browser.durable().await;

    let requested = browser
        .call(
            SUPERVISOR_RESOURCE,
            REQUEST_GRANT_ACTION,
            InvokeArgument::Json(
                serde_json::json!({
                    "label": "Writer",
                    "rooms": [ROOM],
                    "apps": [SHEETS],
                    "expiresInSecs": 3600,
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .unwrap();
    let grant = requested["grant"].as_str().unwrap().to_owned();
    browser
        .until("the grant to be approved", |status| {
            status
                .grants
                .iter()
                .any(|g| g.id == grant && g.state == GrantState::Approved)
        })
        .await;
    browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({
                    "grant": grant,
                    "task": "Add a new sheet named \"Budget\"",
                    "room": ROOM,
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

    // A reload: a new session replays the room and rebuilds the workbook
    // from what the host kept.
    let reloaded_device = enrol(&root, "browser, reloaded", DeviceCapabilities::member());
    let mut reloaded = Browser::open(&hosted, &reloaded_device, &host).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        reloaded.pump();
        if let Some(changes) = reloaded
            .documents
            .get(&page.route)
            .filter(|c| !c.is_empty())
        {
            let replayed = Replica::open(
                &common::catalogue(),
                SHEETS,
                ROOM,
                "ada",
                false,
                Seed::Existing {
                    record: ui::record::RecordId::parse(&page.route.record).unwrap(),
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
    hosted.stop().await;
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
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&root, &host, state.path()).await;
    let _steward = steward::Steward::start(&hosted.relay, &root).await;
    let _approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    let mut first = Browser::open(&hosted, &first_device, &host).await;
    let mut second = Browser::open(&hosted, &second_device, &host).await;

    let mut creator = Page::create(&first, SHEETS);
    creator.until_sheets(&mut first, &["Sheet1"]).await;
    let mut joined = Page::join(&mut second, &creator).await;
    joined.until_sheets(&mut second, &["Sheet1"]).await;

    let requested = first
        .call(
            SUPERVISOR_RESOURCE,
            REQUEST_GRANT_ACTION,
            InvokeArgument::Json(
                serde_json::json!({
                    "label": "Writer",
                    "rooms": [ROOM],
                    "apps": [SHEETS],
                    "expiresInSecs": 3600,
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .unwrap();
    let grant = requested["grant"].as_str().unwrap().to_owned();
    first
        .until("the grant to be approved", |status| {
            status
                .grants
                .iter()
                .any(|g| g.id == grant && g.state == GrantState::Approved)
        })
        .await;
    first
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({
                    "grant": grant,
                    "task": "Add a new sheet named \"Budget\"",
                    "room": ROOM,
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
    hosted.stop().await;
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
    let state = tempfile::tempdir().unwrap();
    let hosted = start_with(&root, &host, state.path(), model).await;
    let _steward = steward::Steward::start(&hosted.relay, &root).await;
    let _approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let mut page = Page::create(&browser, SHEETS);
    page.until_sheets(&mut browser, &["Sheet1"]).await;
    browser.durable().await;

    let requested = browser
        .call(
            SUPERVISOR_RESOURCE,
            REQUEST_GRANT_ACTION,
            InvokeArgument::Json(
                serde_json::json!({"label": "Writer", "rooms": [ROOM], "apps": [SHEETS], "expiresInSecs": 3600})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    let grant = requested["grant"].as_str().unwrap().to_owned();
    browser
        .until("the grant to be approved", |status| {
            status
                .grants
                .iter()
                .any(|g| g.id == grant && g.state == GrantState::Approved)
        })
        .await;
    browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({"grant": grant, "task": "Add a new sheet named \"Budget\"", "room": ROOM})
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
    hosted.stop().await;
}
