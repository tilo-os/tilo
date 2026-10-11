//! Campaign benchmark: the relay's own agent host runs named agents (an
//! `OpenAI` or Anthropic model) through Canvas, Sheets, Notes and the Word Processor. Scaffolding
//! (host, approving laptop, browser session) is copied from `host_agents.rs` so
//! the benchmark exercises exactly the shipped path. Ignored by default; see
//! `campaign_bench` below for the environment it reads.
//!
//! Original `host_agents.rs` header:
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
    dead_code,
    unused_imports,
    reason = "scaffolding shared verbatim with host_agents.rs"
)]
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
    AGENTS_PROVIDER, AgentStatus, AgentTask, AgentsStatus, GrantState, MESSAGE_ACTION,
    REQUEST_GRANT_ACTION, REVOKE_ACTION, SPAWN_ACTION, STATUS_ACTION, STOP_ACTION,
    SUPERVISOR_RESOURCE, TaskState, agent_resource,
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
            // The host closing ends the approver; a benchmark run is over then.
            let (Ok(proposals), Ok(pending_list)) = (
                approver.list_agent_proposals().await,
                approver.list_pending().await,
            ) else {
                return;
            };
            for pending in pending_list {
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

const CANVAS: &str = "so.tilo.canvas";
const WRITER: &str = "so.tilo.word-processor";

/// A benchmark workload, as `bench/campaign/SCENARIOS.md` (in the benchmark
/// harness) describes it: the agents and their grants, room content, steps as
/// a dependency graph, the outputs kept and harness-side faults.
struct Scenario {
    id: String,
    agents: Vec<Role>,
    content: Vec<(String, std::path::PathBuf)>,
    steps: Vec<Stage>,
    outputs: Vec<(String, String, String)>,
    faults: Vec<Fault>,
}

/// One agent of a scenario and the apps its grant covers.
struct Role {
    name: String,
    apps: Vec<String>,
}

/// A step: tasks issued together once the steps it comes after have ended.
struct Stage {
    id: String,
    after: Vec<usize>,
    tasks: Vec<(String, String)>,
}

/// A fault the harness causes `at` after the clock starts.
struct Fault {
    kind: String,
    at: Duration,
    agent: Option<String>,
}

/// The app id a scenario's short app name stands for.
fn app_id(name: &str) -> String {
    match name {
        "canvas" => CANVAS.into(),
        "sheets" => SHEETS.into(),
        "notes" => NOTES.into(),
        "word-processor" | "writer" => WRITER.into(),
        other => other.into(),
    }
}

fn text_list(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

impl Scenario {
    /// The scenario at `path`; content paths are relative to its directory.
    fn load(path: &Path) -> Self {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("the scenario file")).unwrap();
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let field = |item: &serde_json::Value, key: &str| {
            item[key]
                .as_str()
                .unwrap_or_else(|| panic!("a scenario's {key}"))
                .to_owned()
        };
        let agents = value["agents"]
            .as_array()
            .expect("the scenario's agents")
            .iter()
            .map(|agent| Role {
                name: field(agent, "name"),
                apps: text_list(&agent["apps"])
                    .iter()
                    .map(|app| app_id(app))
                    .collect(),
            })
            .collect();
        let content = value["content"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, file)| (name.clone(), base.join(file.as_str().unwrap())))
            .collect();
        let raw = value["steps"].as_array().expect("the scenario's steps");
        let ids: Vec<String> = raw.iter().map(|step| field(step, "id")).collect();
        let steps = raw
            .iter()
            .zip(&ids)
            .map(|(step, id)| Stage {
                id: id.clone(),
                after: text_list(&step["after"])
                    .iter()
                    .map(|need| {
                        ids.iter().position(|id| id == need).unwrap_or_else(|| {
                            panic!("step {id} comes after an unknown step {need}")
                        })
                    })
                    .collect(),
                tasks: step["tasks"]
                    .as_array()
                    .expect("a step's tasks")
                    .iter()
                    .map(|task| (field(task, "agent"), field(task, "text")))
                    .collect(),
            })
            .collect();
        let outputs = value["outputs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|output| {
                (
                    field(output, "step"),
                    field(output, "agent"),
                    field(output, "file"),
                )
            })
            .collect();
        let faults = value["faults"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|fault| Fault {
                kind: field(fault, "kind"),
                at: Duration::from_secs_f64(fault["at_seconds"].as_f64().unwrap_or(0.0)),
                agent: fault["agent"].as_str().map(str::to_owned),
            })
            .collect();
        Self {
            id: field(&value, "id"),
            agents,
            content,
            steps,
            outputs,
            faults,
        }
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

/// Request and wait for one approved grant.
async fn grant_for(browser: &mut Browser, role: &Role) -> String {
    let requested = browser
        .call(
            SUPERVISOR_RESOURCE,
            REQUEST_GRANT_ACTION,
            InvokeArgument::Json(
                serde_json::json!({"label": role.name, "rooms": [ROOM], "apps": role.apps, "expiresInSecs": 86_400})
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
    grant
}

/// Spawn the agent of `grant` in the room.
async fn spawn(browser: &mut Browser, grant: &str) {
    browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({"grant": grant, "room": ROOM})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
}

/// Give the agent of `grant` the task `text`; its task id.
async fn message(browser: &mut Browser, grant: &str, text: &str) -> String {
    let answer = browser
        .call(
            &agent_resource(grant),
            MESSAGE_ACTION,
            InvokeArgument::Text(text.to_owned()),
        )
        .await
        .unwrap();
    answer["taskId"].as_str().unwrap_or_default().to_owned()
}

/// The task `id` of the agent of `grant`, as `status` has it.
fn task_in(status: &AgentsStatus, grant: &str, id: &str) -> Option<AgentTask> {
    status
        .agents
        .iter()
        .filter(|agent| agent.id == grant)
        .flat_map(|agent| agent.tasks.iter())
        .find(|task| task.id == id)
        .cloned()
}

fn ended(task: Option<&AgentTask>) -> bool {
    task.is_some_and(|t| t.state != TaskState::Queued && t.state != TaskState::Running)
}

/// One issued task of a step.
struct Issued {
    agent: String,
    grant: String,
    text: String,
    task: String,
    launched_ms: u128,
    restarted: u32,
}

/// Environment:
/// - `TILO_BENCH_OUT`: run directory (outputs, `tasks.json`, `timing.json`).
/// - `TILO_BENCH_SCENARIO`: the scenario file (`bench/campaign/scenarios/*.json`).
/// - `TILO_AGENT_API_KEY_FILE`: the model provider's key; `TILO_BENCH_MODEL`
///   (`gpt-6-astra`; a `claude-` model goes to `TILO_BENCH_ANTHROPIC_URL`, a
///   model behind `TILO_BENCH_OPENAI_COMPAT_URL` to chat completions there);
///   `TILO_BENCH_BASE_URL` (`https://api.openai.com/v1`); `TILO_BENCH_TASK_SECS` (600).
/// - `TILO_AGENT_USAGE_LOG`: where each model call's usage is logged (the run
///   directory's `usage.jsonl`).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "campaign benchmark: spends real model tokens; run explicitly"]
async fn campaign_bench() {
    // `TILO_BENCH_TRACE` (an env filter, e.g. `tilo_agent_participant=debug`)
    // writes the agents' trace to stderr, which the harness keeps.
    if let Ok(filter) = std::env::var("TILO_BENCH_TRACE") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_writer(std::io::stderr)
            .try_init();
    }
    let out = std::path::PathBuf::from(std::env::var("TILO_BENCH_OUT").expect("TILO_BENCH_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let out = out.canonicalize().unwrap();
    let scenario = Scenario::load(Path::new(
        &std::env::var("TILO_BENCH_SCENARIO").expect("TILO_BENCH_SCENARIO"),
    ));
    let key = std::path::PathBuf::from(
        std::env::var("TILO_AGENT_API_KEY_FILE").expect("TILO_AGENT_API_KEY_FILE"),
    );
    let model_name = env_or("TILO_BENCH_MODEL", "gpt-6-astra");
    // Claude through Anthropic's Messages API; OpenAI's reasoning models take
    // function tools with reasoning only on the Responses API; a local
    // server (Ollama, llama.cpp) through chat completions.
    let spec = if let Ok(url) = std::env::var("TILO_BENCH_OPENAI_COMPAT_URL") {
        format!("openai:{url}")
    } else if model_name.starts_with("claude") {
        format!(
            "anthropic:{}",
            env_or("TILO_BENCH_ANTHROPIC_URL", "https://api.anthropic.com/v1")
        )
    } else {
        format!(
            "responses:{}",
            env_or("TILO_BENCH_BASE_URL", "https://api.openai.com/v1")
        )
    };
    let model = ModelChoice::parse(
        &spec,
        Some(&model_name),
        tilo_agent_participant::ApiKey::File(key),
    )
    .unwrap();
    let task_limit = Duration::from_secs(env_or("TILO_BENCH_TASK_SECS", "600").parse().unwrap());

    let (root, _) = tilo_user_identity::UserIdentity::generate("Bench", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start_with(&root, &host, state.path(), model).await;
    let _steward = steward::Steward::start(&hosted.relay, &root).await;
    let _approver = approve_agents(&hosted.relay, root.clone(), &laptop);
    eprintln!("bench: host started for scenario {}", scenario.id);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;

    let mut grants = BTreeMap::new();
    for role in &scenario.agents {
        grants.insert(role.name.clone(), grant_for(&mut browser, role).await);
    }
    eprintln!("bench: {} grants approved", grants.len());
    // The team joins the room before the work begins, as people are in it
    // before anyone hands out tasks: each agent holds the room's keys when
    // the records it will work on are made. The clock starts after.
    for grant in grants.values() {
        spawn(&mut browser, grant).await;
    }
    browser
        .until("every agent to join the room", |status| {
            grants.values().all(|grant| {
                status
                    .agents
                    .iter()
                    .any(|a| a.id == *grant && a.status == AgentStatus::Waiting)
            })
        })
        .await;
    eprintln!("bench: agents joined");
    let mut addresses = BTreeMap::new();
    for (name, file) in &scenario.content {
        let address = hosted
            .agents
            .put_content(ROOM, &std::fs::read(file).unwrap())
            .unwrap();
        addresses.insert(format!("{{content:{name}}}"), address);
    }
    let fill = |text: &str| {
        addresses
            .iter()
            .fold(text.to_owned(), |text, (slot, address)| {
                text.replace(slot, address)
            })
    };

    // Steps run as a dependency graph: each starts once the steps it needs
    // have ended, so independent work proceeds at the same time. An agent
    // still works its own tasks in order.
    let run_started = Instant::now();
    let started = std::time::SystemTime::now();
    let mut launched: BTreeMap<usize, (Instant, Vec<Issued>)> = BTreeMap::new();
    let mut finished_steps: BTreeMap<usize, f64> = BTreeMap::new();
    let mut log: Vec<serde_json::Value> = Vec::new();
    let mut faults_done = vec![false; scenario.faults.len()];
    let mut fault_log = Vec::new();
    while finished_steps.len() < scenario.steps.len() {
        for (index, stage) in scenario.steps.iter().enumerate() {
            if launched.contains_key(&index)
                || !stage
                    .after
                    .iter()
                    .all(|need| finished_steps.contains_key(need))
            {
                continue;
            }
            let mut issued = Vec::new();
            for (agent, text) in &stage.tasks {
                let grant = grants
                    .get(agent)
                    .unwrap_or_else(|| panic!("step {} names an unknown agent {agent}", stage.id))
                    .clone();
                let text = fill(text);
                let task = message(&mut browser, &grant, &text).await;
                issued.push(Issued {
                    agent: agent.clone(),
                    grant,
                    text,
                    task,
                    launched_ms: unix_ms(),
                    restarted: 0,
                });
            }
            eprintln!(
                "bench: step {} started at {:.1}s",
                stage.id,
                run_started.elapsed().as_secs_f64()
            );
            launched.insert(index, (Instant::now(), issued));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        // Harness-side faults that are due.
        for (number, fault) in scenario.faults.iter().enumerate() {
            if faults_done[number] || run_started.elapsed() < fault.at {
                continue;
            }
            faults_done[number] = true;
            let outcome = match (fault.kind.as_str(), fault.agent.as_ref()) {
                ("kill_agent", Some(agent)) => {
                    let grant = grants[agent].clone();
                    restart_agent(&mut browser, &grant, &mut launched).await
                }
                (kind, _) => format!("unsupported fault {kind}"),
            };
            eprintln!(
                "bench: fault {} at {:.1}s: {outcome}",
                fault.kind,
                run_started.elapsed().as_secs_f64()
            );
            fault_log.push(serde_json::json!({
                "kind": fault.kind,
                "agent": fault.agent,
                "at_seconds": run_started.elapsed().as_secs_f64(),
                "outcome": outcome,
            }));
        }
        let status = browser.status().await;
        for (&index, (step_started, issued)) in &launched {
            if finished_steps.contains_key(&index) {
                continue;
            }
            let done = issued
                .iter()
                .all(|one| ended(task_in(&status, &one.grant, &one.task).as_ref()));
            if !done && step_started.elapsed() < task_limit {
                continue;
            }
            finished_steps.insert(index, run_started.elapsed().as_secs_f64());
            for one in issued {
                let task = task_in(&status, &one.grant, &one.task);
                log.push(serde_json::json!({
                    "step": scenario.steps[index].id,
                    "agent": one.agent,
                    "agent_id": one.grant,
                    "task_id": one.task,
                    "task": one.text,
                    "state": task.as_ref().map(|t| format!("{:?}", t.state)),
                    "error": task.as_ref().and_then(|t| t.error.clone()),
                    "actions": task.as_ref().map(|t| t.steps.iter().map(|s| s.action.clone()).collect::<Vec<_>>()),
                    "apps": task.as_ref().map(|t| t.steps.iter().map(|s| s.app.clone()).collect::<Vec<_>>()),
                    "resources": task.as_ref().map(|t| t.steps.iter().map(|s| s.resource.clone()).collect::<Vec<_>>()),
                    "content": task.as_ref().map(|t| t.steps.iter().flat_map(|s| s.content.clone()).collect::<Vec<_>>()),
                    "restarted": one.restarted,
                    "launched_at_unix_ms": one.launched_ms,
                    "ended_at_unix_ms": unix_ms(),
                    "step_seconds": step_started.elapsed().as_secs_f64(),
                    "ended_at_seconds": run_started.elapsed().as_secs_f64(),
                }));
            }
            std::fs::write(
                out.join("tasks.json"),
                serde_json::to_string_pretty(&log).unwrap(),
            )
            .unwrap();
        }
    }
    let finished = std::time::SystemTime::now();
    // The last content each output's task produced.
    let produced = |step: &str, agent: &str| {
        log.iter()
            .filter(|row| row["step"] == step && row["agent"] == agent)
            .filter_map(|row| {
                row["content"]
                    .as_array()?
                    .last()?
                    .as_str()
                    .map(str::to_owned)
            })
            .last()
    };
    for (step, agent, file) in &scenario.outputs {
        if let Some(bytes) =
            produced(step, agent).and_then(|address| hosted.agents.content(ROOM, &address))
        {
            std::fs::write(out.join(file), bytes).unwrap();
        }
    }
    let unix = |t: std::time::SystemTime| {
        t.duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    };
    std::fs::write(
        out.join("timing.json"),
        serde_json::json!({"scenario": scenario.id, "started_at": unix(started), "finished_at": unix(finished),
                           "agents": grants, "faults": fault_log})
            .to_string(),
    )
    .unwrap();
    drop(browser);
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}

/// Stop the agent of `grant`, spawn it again on the same grant and give it
/// again each of its launched tasks that had not ended, as a supervisor
/// restarting a crashed worker would. Says what happened.
async fn restart_agent(
    browser: &mut Browser,
    grant: &str,
    launched: &mut BTreeMap<usize, (Instant, Vec<Issued>)>,
) -> String {
    let status = browser.status().await;
    let unfinished: Vec<(usize, usize)> = launched
        .iter()
        .flat_map(|(&step, (_, issued))| {
            issued
                .iter()
                .enumerate()
                .filter(|(_, one)| {
                    one.grant == grant && !ended(task_in(&status, grant, &one.task).as_ref())
                })
                .map(move |(slot, _)| (step, slot))
        })
        .collect();
    if let Err(error) = browser
        .call(&agent_resource(grant), STOP_ACTION, InvokeArgument::None)
        .await
    {
        return format!("stop failed: {error:?}");
    }
    browser
        .until("the stopped agent to leave", |status| {
            !status
                .agents
                .iter()
                .any(|a| a.id == grant && a.status != AgentStatus::Stopped)
        })
        .await;
    // `TILO_BENCH_RESPAWN_MS`: how long the stopped agent stays down.
    let down = env_or("TILO_BENCH_RESPAWN_MS", "0").parse().unwrap_or(0);
    tokio::time::sleep(Duration::from_millis(down)).await;
    spawn(browser, grant).await;
    browser
        .until("the restarted agent to join", |status| {
            status
                .agents
                .iter()
                .any(|a| a.id == grant && a.status == AgentStatus::Waiting)
        })
        .await;
    for &(step, slot) in &unfinished {
        if let Some((_, issued)) = launched.get_mut(&step) {
            let text = issued[slot].text.clone();
            issued[slot].task = message(browser, grant, &text).await;
            issued[slot].restarted += 1;
        }
    }
    format!(
        "restarted; {} unfinished task(s) given again",
        unfinished.len()
    )
}
