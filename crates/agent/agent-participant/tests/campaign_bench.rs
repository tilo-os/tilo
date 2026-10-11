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

/// One agent of the campaign and the apps its grant covers.
struct Role {
    name: &'static str,
    apps: &'static [&'static str],
}

const ROLES: &[Role] = &[
    Role {
        name: "Mina",
        apps: &[CANVAS, WRITER],
    },
    Role {
        name: "Eli",
        apps: &[SHEETS],
    },
    Role {
        name: "Kai",
        apps: &[SHEETS],
    },
    Role {
        name: "Noor",
        apps: &[SHEETS],
    },
    Role {
        name: "Jules",
        apps: &[SHEETS, WRITER],
    },
    Role {
        name: "Ren",
        apps: &[NOTES, SHEETS],
    },
];

const FACTS: &str =
    "available October 22; $129; chalk-white finish; 14 cm base; three brightness levels";

/// The workflow. Agents have no file access: the photo is handed in as room content
/// (`photo` is its `tilo.content:` address) and every export keeps its result as
/// content, which the host reads back afterwards.
fn steps(photo: &str) -> Vec<Vec<(&'static str, String)>> {
    vec![
        vec![(
            "Mina",
            format!(
                "In Canvas, open the picture at content address {photo}. Adjust it (raise exposure or contrast slightly). \
                 Add a deep-teal (#0F4C5C) filled rectangle panel covering the right 38% of the picture, beside \
                 the lamp. Make the artwork 16:9 at exactly 1520 by 855 pixels, cropping as needed. Export it \
                 as a PNG at scale 1, kept as content."
            ),
        )],
        vec![(
            "Eli",
            "In Sheets, create the campaign plan spreadsheet. On a sheet named \"Headline tests\" \
             enter columns Headline, Visits, Signups, Conversion with these rows: \"Small desk. Clear head.\" \
             240 18; \"Light that stays out of the way.\" 240 12; \"Your desk, finally calm.\" 240 9. Make \
             Conversion a formula Signups/Visits formatted as a percentage, and add a row \"Winner\" naming \
             \"Small desk. Clear head.\" at 7.5%. These are synthetic planning figures."
                .into(),
        )],
        vec![(
            "Ren",
            "In Notes, create a note titled \"Launch headline\" whose text is the winning headline from the \
             campaign spreadsheet: Small desk. Clear head."
                .into(),
        )],
        vec![(
            "Jules",
            format!(
                "In the Word Processor, create the launch page: a new document whose first heading is \"Small \
                 desk. Clear head.\" (documents are named by their first heading), right under it the line \
                 \"Caption: The chalk-white desk light beside the campaign's deep-teal panel.\", then a \
                 short introduction \
                 paragraph for the chalk-white desk light, a \"Specifications\" section, an \"Availability\" \
                 section and an \"FAQ\" section with three questions and answers, using only these facts: \
                 {FACTS}. Check every product claim against those facts, keep the introduction tight, and \
                 end with a paragraph starting \"Review:\" listing what you checked."
            ),
        )],
        // The spreadsheet exists before anyone else adds a sheet to it; each
        // adds its own, with its content, at once.
        vec![
            (
                "Kai",
                "In the Sheets spreadsheet \"Campaign plan\", add a sheet \"Channel allocation\" splitting a \
                 synthetic $4,000 launch budget across Email, Social, Partners and Search."
                    .into(),
            ),
            (
                "Noor",
                "In the Sheets spreadsheet \"Campaign plan\", add a sheet \"Email funnel\" with Sent, Opened, \
                 Clicked and Signed up stages (synthetic figures)."
                    .into(),
            ),
            (
                "Ren",
                "In the Sheets spreadsheet \"Campaign plan\", add a sheet \"Partner dates\" with three \
                 fictional partners and dates in October."
                    .into(),
            ),
            (
                "Jules",
                "In the Sheets spreadsheet \"Campaign plan\", add a sheet \"Budget check\" comparing planned \
                 and allocated spend against the $4,000 budget (synthetic figures)."
                    .into(),
            ),
            (
                "Eli",
                "In the Sheets spreadsheet \"Campaign plan\", add a sheet \"Deliverables\" tracking: campaign \
                 image, launch page, email, partner kit, each with an owner and status."
                    .into(),
            ),
        ],
        vec![(
            "Mina",
            "In Canvas, export the current artwork as a PNG at scale 1 kept as content, then in the Word \
             Processor document \"Small desk. Clear head.\" insert that exported image right under its first \
             heading (others are editing the paragraphs; the caption already follows the heading). Then \
             export that document as DOCX, kept as content."
                .into(),
        )],
        vec![(
            "Eli",
            "Export the Sheets spreadsheet \"Campaign plan\" with all its sheets as an Excel workbook, kept as content."
                .into(),
        )],
        vec![(
            "Ren",
            "Export the note \"Launch headline\" as Markdown, kept as content.".into(),
        )],
    ]
}

/// Which steps (1-based) each step waits for: three rounds. First the image
/// (1), the spreadsheet (2), the note (3) and the checked page (4) at once;
/// then the other sheets, one or two each (5), and the image placed in the
/// page and the page exported (6); then the workbook's export (7) and the
/// note's (8).
const DEPENDS: &[&[usize]] = &[&[], &[], &[], &[], &[2], &[1, 4], &[5], &[3]];

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
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

/// Wait (up to `limit`) until every listed task of these agents has ended.
async fn wait_tasks(
    browser: &mut Browser,
    wanted: &[(String, usize)],
    limit: Duration,
) -> AgentsStatus {
    let deadline = Instant::now() + limit;
    loop {
        let status = browser.status().await;
        let ended = wanted.iter().all(|(agent, index)| {
            status.agents.iter().any(|a| {
                a.id == *agent
                    && a.tasks.get(*index).is_some_and(|t| {
                        t.state != TaskState::Queued && t.state != TaskState::Running
                    })
            })
        });
        if ended || Instant::now() >= deadline {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Environment:
/// - `TILO_BENCH_OUT`: run directory (deliverables, `tasks.json`, `usage.jsonl`).
/// - `TILO_BENCH_PHOTO`: the source photo, handed to the agents as room content.
/// - `TILO_AGENT_API_KEY_FILE`: the model provider's key; `TILO_BENCH_MODEL`
///   (`gpt-6-astra`; a `claude-` model goes to `TILO_BENCH_ANTHROPIC_URL`);
///   `TILO_BENCH_BASE_URL` (`https://api.openai.com/v1`); `TILO_BENCH_TASK_SECS` (600).
/// - `TILO_AGENT_USAGE_LOG`: where each model call's usage is logged (the run
///   directory's `usage.jsonl`).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "campaign benchmark: spends real model tokens; run explicitly"]
async fn campaign_bench() {
    let out = std::path::PathBuf::from(std::env::var("TILO_BENCH_OUT").expect("TILO_BENCH_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let out = out.canonicalize().unwrap();
    let photo = std::env::var("TILO_BENCH_PHOTO").expect("TILO_BENCH_PHOTO");
    let key = std::path::PathBuf::from(
        std::env::var("TILO_AGENT_API_KEY_FILE").expect("TILO_AGENT_API_KEY_FILE"),
    );
    let model_name = env_or("TILO_BENCH_MODEL", "gpt-6-astra");
    // Claude through Anthropic's Messages API; OpenAI's reasoning models take
    // function tools with reasoning only on the Responses API.
    let spec = if model_name.starts_with("claude") {
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
    eprintln!("bench: host started");
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    eprintln!("bench: browser open");

    let mut grants = BTreeMap::new();
    for role in ROLES {
        grants.insert(role.name, grant_for(&mut browser, role).await);
        eprintln!("bench: grant approved for {}", role.name);
    }
    // The team joins the room before the work begins, as people are in it
    // before anyone hands out tasks: each agent holds the room's keys when
    // the records it will work on are made. The clock starts after.
    for role in ROLES {
        browser
            .call(
                SUPERVISOR_RESOURCE,
                SPAWN_ACTION,
                InvokeArgument::Json(
                    serde_json::json!({"grant": grants[role.name], "room": ROOM})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();
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
    let mut spawned = BTreeMap::<&str, usize>::new();
    let mut log = Vec::new();
    let photo = hosted
        .agents
        .put_content(ROOM, &std::fs::read(&photo).unwrap())
        .unwrap();
    // Steps run as a dependency graph: each starts once the steps it needs have
    // ended, so independent work (the image, the spreadsheet, the note, the page)
    // proceeds at the same time. An agent still works its own tasks in order.
    let steps = steps(&photo);
    assert_eq!(steps.len(), DEPENDS.len());
    let run_started = Instant::now();
    let started = std::time::SystemTime::now();
    let mut launched: BTreeMap<usize, (Instant, Vec<(String, usize)>)> = BTreeMap::new();
    let mut ended: BTreeMap<usize, f64> = BTreeMap::new();
    while ended.len() < steps.len() {
        for (index, step) in steps.iter().enumerate() {
            let number = index + 1;
            if launched.contains_key(&number)
                || !DEPENDS[index].iter().all(|need| ended.contains_key(need))
            {
                continue;
            }
            let mut wanted = Vec::new();
            for (name, task) in step {
                let grant = grants[name].clone();
                let count = spawned.entry(name).or_insert(0);
                browser
                    .call(
                        &agent_resource(&grant),
                        MESSAGE_ACTION,
                        InvokeArgument::Text(task.clone()),
                    )
                    .await
                    .unwrap();
                wanted.push((grant, *count));
                *count += 1;
            }
            eprintln!(
                "bench: step {number} started at {:.1}s (unix ms {})",
                run_started.elapsed().as_secs_f64(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            );
            launched.insert(number, (Instant::now(), wanted));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        let status = browser.status().await;
        let task_of = |grant: &str, index: usize| {
            status
                .agents
                .iter()
                .find(|a| a.id == grant)
                .and_then(|a| a.tasks.get(index))
                .cloned()
        };
        for (&number, (started, wanted)) in &launched {
            if ended.contains_key(&number) {
                continue;
            }
            let done = wanted.iter().all(|(grant, index)| {
                task_of(grant, *index)
                    .is_some_and(|t| t.state != TaskState::Queued && t.state != TaskState::Running)
            });
            let timed_out = started.elapsed() >= task_limit;
            if !done && !timed_out {
                continue;
            }
            ended.insert(number, run_started.elapsed().as_secs_f64());
            for ((name, text), (grant, task_index)) in steps[number - 1].iter().zip(wanted) {
                let task = task_of(grant, *task_index);
                log.push(serde_json::json!({
                    "step": number,
                    "agent": name,
                    "agent_id": grant,
                    "task": text,
                    "state": task.as_ref().map(|t| format!("{:?}", t.state)),
                    "error": task.as_ref().and_then(|t| t.error.clone()),
                    "actions": task.as_ref().map(|t| t.steps.iter().map(|s| s.action.clone()).collect::<Vec<_>>()),
                    "content": task.as_ref().map(|t| t.steps.iter().flat_map(|s| s.content.clone()).collect::<Vec<_>>()),
                    "step_seconds": started.elapsed().as_secs_f64(),
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
    // The last content each export step produced, by the step that asked for it.
    let produced = |step: usize, agent: &str| {
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
    for (step, agent, file) in [
        (1, "Mina", "asset.png"),
        (7, "Eli", "campaign.xlsx"),
        (8, "Ren", "notes.md"),
        (6, "Mina", "launch-page.docx"),
    ] {
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
        serde_json::json!({"started_at": unix(started), "finished_at": unix(finished),
                           "agents": grants.iter().map(|(n, g)| (n.to_string(), g.clone())).collect::<BTreeMap<_, _>>()})
            .to_string(),
    )
    .unwrap();
    drop(browser);
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}
