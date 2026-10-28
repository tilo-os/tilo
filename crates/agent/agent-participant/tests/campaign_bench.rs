//! Campaign benchmark: the relay's own agent host runs named agents (a GPT
//! model through an OpenAI-compatible endpoint, Claude through Anthropic's
//! Messages API, both via the model gateway's trusted-host client) through a
//! scenario's steps, in the apps the scenario grants. The scaffolding (host,
//! approving laptop, browser session) is that of `host_agents.rs`, so the
//! benchmark exercises exactly the shipped path. Ignored by default; see
//! `campaign_bench` below for the environment it reads.
//!
//! The model key is read from a file by the host's model client, adopted
//! into a vault in the run's private state directory (deleted with it) and
//! never logged; agents' app workers never see it. Outside a test
//! environment (a frozen binary run directly) the benchmark runs itself
//! again inside `tools/with-test-environment.sh`, so it neither reads nor
//! writes the user's keychain, settings or session.

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
    reason = "a benchmark harness: a broken scenario or host is the failure it reports"
)]

#[path = "../../../browser/tilo-browser-peer/tests/support/steward.rs"]
mod steward;
#[path = "../../../browser/tilo-browser-peer/tests/support/mod.rs"]
mod support;

// Agents hold each record in a confined app worker (this binary, started
// again), whose changes are admitted through the Automerge isolate.
tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{
    CataloguePage, CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, RemoteActionTarget, application_open_target,
};
use tilo_actions::{ActionOutput, InvocationId};
use tilo_agent_participant::{AgentHost, ApiKey, AppCatalogue, ModelChoice, record_provider};
use tilo_browser_peer::{DocumentRoute, Host, PeerEvent, PeerEvents, PeerHandle};
use tilo_relay::certify::{ApproverDecision, ApproverReply, ConfirmationAnswer};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_relay_client::Approver;
use tilo_relay_protocol::agents::{
    AGENTS_PROVIDER, AgentStatus, AgentTask, AgentsStatus, GrantState, MESSAGE_ACTION,
    REQUEST_GRANT_ACTION, SPAWN_ACTION, STATUS_ACTION, STOP_ACTION, SUPERVISOR_RESOURCE, TaskState,
    agent_resource,
};
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, RevocationList, issue_device_cert,
};

use support::{Device, ROOM, enrol, ready, start_peer, workspace};

const NOTES: &str = "so.tilo.notes";
const SHEETS: &str = "so.tilo.sheets";
const CANVAS: &str = "so.tilo.canvas";
const WRITER: &str = "so.tilo.word-processor";

/// The task an agent is spawned with to join the room before the work
/// begins: written steps, none of them, so no model is asked.
const JOIN: &str = "[]";

struct Hosted {
    relay: Relay,
    web: WebHandle,
    agents: AgentHost,
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
            // Every app a browser mounts, and Mail, which only an agent's
            // worker holds.
            catalogue: Some(Arc::new(AppCatalogue::native)),
            feedback_reviewer: None,
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
            public_origin: None,
            live_origin: None,
            limits: tilo_relay::web::WebLimits::default(),
            country: None,
            fetch_proxy: None,
        })
        .await
        .unwrap();
    Hosted { relay, web, agents }
}

/// The laptop: a root holder answering every agent request with the
/// agent's certificate and the delegation of exactly what was proposed, and
/// every question an agent puts to its person (an action that asks first):
/// allowed once for the agents in `confirming` (a scenario's `may_confirm`),
/// refused for the rest.
fn approve_agents(
    relay: &Relay,
    root: SigningKey,
    laptop: &Device,
    confirming: Arc<std::sync::Mutex<BTreeSet<String>>>,
) -> tokio::task::JoinHandle<()> {
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
                let now = tilo_clock::unix_now_ns();
                let cert = issue_device_cert(
                    &pending.request,
                    &root,
                    &CertificationPolicy::agent(),
                    &RevocationList::new(),
                    now,
                )
                .unwrap();
                let delegation = agent
                    .proposal
                    .sign(&root, pending.request.device_public_key, now)
                    .unwrap();
                let decided = approver
                    .decide(
                        pending.request_digest,
                        ApproverDecision::IssueDelegate { cert, delegation },
                    )
                    .await;
                assert!(
                    matches!(decided, Ok(ApproverReply::Decided) | Err(_)),
                    "{decided:?}"
                );
            }
            let Ok(questions) = approver.list_confirmations().await else {
                return;
            };
            for question in questions {
                let allowed = confirming.lock().is_ok_and(|agents| {
                    agents.contains(&question.agent_id) || agents.contains(&question.agent_label)
                });
                let answer = if allowed {
                    ConfirmationAnswer::AllowOnce
                } else {
                    ConfirmationAnswer::Deny
                };
                eprintln!(
                    "bench: {} asked to {} · {}: {answer:?}",
                    question.agent_label, question.action_label, question.resource_label
                );
                let _ = approver.answer(question.id, answer).await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
}

/// A browser session driving the host's agent supervisor, and the room's
/// records through the host's room actions as the room's owner.
struct Browser {
    peer: PeerHandle,
    events: PeerEvents,
    host_key: [u8; 32],
    /// How many changes the page has received of each record: an owner's
    /// change is heard in the room before the agents act on it.
    heard: std::sync::Mutex<BTreeMap<DocumentRoute, usize>>,
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
            heard: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// Take what the page received: the host's answers `pick` accepts (the
    /// first one), and the records' changes, counted.
    fn pump<T>(&self, pick: &impl Fn(RemoteActionMessage) -> Option<T>) -> Option<T> {
        let mut picked = None;
        for event in self.events.drain() {
            match event {
                PeerEvent::RemoteAction { from, frame, .. } if from == self.host_key => {
                    if picked.is_none() {
                        picked = pick(RemoteActionFrame::decode(&frame).unwrap().message);
                    }
                }
                PeerEvent::Document(document) => {
                    *self
                        .heard
                        .lock()
                        .unwrap()
                        .entry(document.route.clone())
                        .or_default() += 1;
                }
                PeerEvent::RemoteActionUndeliverable { reason, .. } => {
                    panic!("undeliverable: {reason}")
                }
                _ => {}
            }
        }
        picked
    }

    /// Send `message` to the host and wait for the answer `pick` accepts.
    async fn exchange<T>(
        &self,
        message: RemoteActionMessage,
        what: &str,
        pick: impl Fn(RemoteActionMessage) -> Option<T>,
    ) -> T {
        let frame = RemoteActionFrame::new(message).encode().unwrap();
        self.peer
            .send_remote_action(workspace(), self.host_key, frame)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            assert!(Instant::now() < deadline, "no answer: {what}");
            if let Some(answer) = self.pump(&pick) {
                return answer;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// One remote action to the host: to `provider` (the agent supervisor,
    /// a record of the room, the room itself).
    async fn invoke(
        &self,
        provider: &str,
        address: ActionAddress,
        argument: InvokeArgument,
    ) -> Result<ActionOutput, String> {
        let invocation = InvocationId::new();
        let what = format!("{provider} {}#{}", address.resource, address.action);
        let request = RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: provider.into(),
                address,
            },
            argument,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        });
        self.exchange(request, &what, |message| match message {
            RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation => {
                Some(match receipt.status {
                    ReceiptStatus::Completed { output } => Ok(output),
                    other => Err(format!("{other:?}")),
                })
            }
            _ => None,
        })
        .await
    }

    /// One remote action to the host's agent supervisor.
    async fn call(
        &self,
        resource: &str,
        action: &str,
        argument: InvokeArgument,
    ) -> Result<serde_json::Value, String> {
        self.invoke(
            AGENTS_PROVIDER,
            ActionAddress::new(resource, action),
            argument,
        )
        .await
        .map(|output| output.value.unwrap_or_default())
    }

    /// The host's catalogue of the room's records and their actions.
    async fn catalogue(&self, query: CatalogueQuery) -> Result<CataloguePage, String> {
        static CORRELATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let correlation = CORRELATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.exchange(
            RemoteActionMessage::CatalogueRequest { correlation, query },
            "the room's catalogue",
            |message| match message {
                RemoteActionMessage::CatalogueResponse {
                    correlation: got,
                    result,
                } if got == correlation => Some(result.map_err(|error| format!("{error:?}"))),
                _ => None,
            },
        )
        .await
    }

    /// How many changes of `route` the page has received.
    fn heard(&self, route: &DocumentRoute) -> usize {
        self.heard.lock().unwrap().get(route).copied().unwrap_or(0)
    }

    /// Wait until the page has received more than `past` changes of
    /// `route`; `false` when none came in time.
    async fn until_heard(&self, route: &DocumentRoute, past: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let _: Option<()> = self.pump(&|_| None);
            if self.heard(route) > past {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn status(&self) -> AgentsStatus {
        serde_json::from_value(
            self.call(SUPERVISOR_RESOURCE, STATUS_ACTION, InvokeArgument::None)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    /// Wait until `done` holds for the supervisor's status.
    async fn until(&self, what: &str, done: impl Fn(&AgentsStatus) -> bool) -> AgentsStatus {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let status = self.status().await;
            if done(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "{what}: {status:#?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// A benchmark workload, as `bench/campaign/SCENARIOS.md` (in the benchmark
/// harness) describes it: the agents and their grants, room content, steps as
/// a dependency graph, the outputs kept and harness-side faults and events.
struct Scenario {
    id: String,
    agents: Vec<Role>,
    content: Vec<(String, PathBuf)>,
    steps: Vec<Stage>,
    outputs: Vec<(String, String, String)>,
    faults: Vec<Fault>,
    events: Vec<Event>,
}

/// Something that happens in the room while agents work: `agent` is given
/// `text` (usually written steps, so no model is asked) `at` after the clock
/// starts — an email arriving, a chat message, a correction.
struct Event {
    at: Duration,
    agent: String,
    text: String,
    label: String,
}

/// One agent of a scenario and the apps its grant covers.
struct Role {
    name: String,
    apps: Vec<String>,
    /// Its person allows the actions it asks about first (deleting clips);
    /// the others' questions are refused.
    may_confirm: bool,
}

/// A step: tasks issued together once the steps it comes after have ended.
struct Stage {
    id: String,
    after: Vec<usize>,
    tasks: Vec<Task>,
}

/// One task of a step: `text` for `agent`, or written steps the room's owner
/// carries out (`"as": "owner"` on the task or on its step).
struct Task {
    agent: String,
    text: String,
    actor: Actor,
}

/// Who carries out a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Actor {
    /// The agent it names, as its grant allows.
    Agent,
    /// The room's owner, through the host's room actions: setup an agent's
    /// grant does not reach (making a Chat channel is the room's moderation,
    /// and an agent acts as an Editor at most). The agent the task names is
    /// whom the scenario has it stand for; it is not asked.
    Owner,
}

impl Actor {
    /// The actor `item`'s `as` names, else `inherited`.
    fn of(item: &serde_json::Value, inherited: Self) -> Self {
        match item["as"].as_str() {
            None => inherited,
            Some("owner") => Self::Owner,
            Some("agent") => Self::Agent,
            Some(other) => {
                panic!("a scenario's task runs as an agent or as the owner, not {other:?}")
            }
        }
    }
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
        other if !other.contains('.') => format!("so.tilo.{other}"),
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
                may_confirm: agent["may_confirm"].as_bool().unwrap_or(false),
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
                    .map(|task| Task {
                        agent: field(task, "agent"),
                        text: field(task, "text"),
                        actor: Actor::of(task, Actor::of(step, Actor::Agent)),
                    })
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
        let events = value["events"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|event| Event {
                at: Duration::from_secs_f64(event["at_seconds"].as_f64().unwrap_or(0.0)),
                agent: field(event, "agent"),
                text: field(event, "text"),
                label: event["label"].as_str().unwrap_or("event").to_owned(),
            })
            .collect();
        Self {
            id: field(&value, "id"),
            agents,
            content,
            steps,
            outputs,
            faults,
            events,
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

/// The model the agents plan with, from the environment (see
/// [`campaign_bench`]), its key adopted into a vault under `keys`.
fn model_from_env(keys: &Path) -> ModelChoice {
    let model = env_or("TILO_BENCH_MODEL", "gpt-6-astra");
    if model == "scripted" {
        return ModelChoice::Scripted;
    }
    let key = ApiKey::File(PathBuf::from(
        std::env::var("TILO_AGENT_API_KEY_FILE").expect("TILO_AGENT_API_KEY_FILE"),
    ));
    let spec = if let Ok(url) = std::env::var("TILO_BENCH_OPENAI_COMPAT_URL") {
        format!("openai:{url}")
    } else if model.starts_with("claude") {
        format!(
            "anthropic:{}",
            env_or("TILO_BENCH_ANTHROPIC_URL", "https://api.anthropic.com")
        )
    } else {
        format!(
            "openai:{}",
            env_or("TILO_BENCH_BASE_URL", "https://api.openai.com/v1")
        )
    };
    ModelChoice::parse(&spec, Some(&model), key)
        .unwrap_or_else(|error| panic!("the model {model} at {spec}: {error}"))
        .with_items_from(&|name| std::env::var(name).ok())
        .unwrap_or_else(|error| panic!("the item model: {error}"))
        .keeping_keys_in(keys.to_owned())
}

/// Outside a test environment (the frozen binary run as it is), run this
/// test again inside `tools/with-test-environment.sh`, so the run neither
/// reads nor writes the user's keychain, settings or session, and exit with
/// its status. `false` when this process is the one to run the benchmark.
fn ran_isolated() -> bool {
    if std::env::var_os("TILO_TEST_ENVIRONMENT").is_some() {
        return false;
    }
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/with-test-environment.sh");
    if !script.is_file() {
        eprintln!(
            "bench: {} is missing; running outside a test environment",
            script.display()
        );
        return false;
    }
    let status = std::process::Command::new(&script)
        .arg(std::env::current_exe().unwrap())
        .args(std::env::args_os().skip(1))
        .status()
        .unwrap_or_else(|error| panic!("{}: {error}", script.display()));
    #[allow(
        clippy::exit,
        reason = "the isolated run is the benchmark: its status is this process's"
    )]
    std::process::exit(status.code().unwrap_or(1));
}

/// Request and wait for one approved grant.
async fn grant_for(browser: &Browser, role: &Role) -> String {
    let requested = browser
        .call(
            SUPERVISOR_RESOURCE,
            REQUEST_GRANT_ACTION,
            InvokeArgument::Json(
                serde_json::json!({"label": role.name, "rooms": [ROOM], "apps": role.apps,
                                   "expiresInSecs": 86_400})
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

/// Spawn the agent of `grant` in the room with nothing to do.
async fn spawn(browser: &Browser, grant: &str) {
    browser
        .call(
            SUPERVISOR_RESOURCE,
            SPAWN_ACTION,
            InvokeArgument::Json(
                serde_json::json!({"grant": grant, "task": JOIN, "room": ROOM})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
}

/// Give the agent of `grant` the task `text`; its task id.
async fn message(browser: &Browser, grant: &str, text: &str) -> String {
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

/// Whether the agent of `grant` waits for work: spawned, running no task.
fn idle(status: &AgentsStatus, grant: &str) -> bool {
    status.agents.iter().any(|agent| {
        agent.id == grant
            && agent.status == AgentStatus::Waiting
            && agent
                .tasks
                .iter()
                .all(|task| task.state != TaskState::Running)
    })
}

/// One issued task of a step.
struct Issued {
    agent: String,
    grant: String,
    text: String,
    task: String,
    launched_ms: u128,
    restarted: u32,
    /// When each of its steps was first seen (status is polled every
    /// 250 ms), for replaying the run.
    seen_ms: Vec<u128>,
}

/// The run's tasks so far as `tasks.json` rows.
fn task_row(
    scenario: &Scenario,
    index: usize,
    one: &Issued,
    task: Option<&AgentTask>,
    (step_seconds, ended_at_seconds): (f64, f64),
) -> serde_json::Value {
    serde_json::json!({
        "step": scenario.steps[index].id,
        "agent": one.agent,
        "agent_id": one.grant,
        "task_id": one.task,
        "task": one.text,
        "state": task.map(|t| format!("{:?}", t.state)),
        "error": task.and_then(|t| t.error.clone()),
        "actions": task.map(|t| t.steps.iter().map(|s| s.action.clone()).collect::<Vec<_>>()),
        "apps": task.map(|t| t.steps.iter().map(|s| s.app.clone()).collect::<Vec<_>>()),
        "resources": task.map(|t| t.steps.iter().map(|s| s.resource.clone()).collect::<Vec<_>>()),
        "content": task.map(|t| t.steps.iter().flat_map(|s| s.content.clone()).collect::<Vec<_>>()),
        "content_actions": task.map(|t| t.steps.iter().flat_map(|s| s.content.iter().map(|_| s.action.clone())).collect::<Vec<_>>()),
        "restarted": one.restarted,
        "step_seen_unix_ms": one.seen_ms,
        "launched_at_unix_ms": one.launched_ms,
        "ended_at_unix_ms": unix_ms(),
        "step_seconds": step_seconds,
        "ended_at_seconds": ended_at_seconds,
    })
}

/// The content the output `(step, agent)` stands for in `log`: the last its
/// task exported, else the last it produced. A `look` after the export is
/// the agent viewing its work, not the file it was asked for.
fn produced(log: &[serde_json::Value], step: &str, agent: &str) -> Option<String> {
    log.iter()
        .filter(|row| row["step"] == step && row["agent"] == agent)
        .filter_map(|row| {
            let content = row["content"].as_array()?;
            let actions = row["content_actions"].as_array();
            let exported = actions.and_then(|actions| {
                actions
                    .iter()
                    .zip(content)
                    .filter(|(action, _)| action.as_str().is_some_and(|a| a.starts_with("export")))
                    .map(|(_, address)| address)
                    .next_back()
            });
            exported
                .or_else(|| content.last())?
                .as_str()
                .map(str::to_owned)
        })
        .next_back()
}

/// Who the action log and `tasks.json` name for what the room's owner did.
const OWNER: &str = "owner";

/// What the room's owner did for one task: each of its written steps through
/// the host's room actions, in order, until one failed.
#[derive(Default)]
struct OwnerRun {
    actions: Vec<String>,
    apps: Vec<String>,
    resources: Vec<String>,
    error: Option<String>,
}

impl OwnerRun {
    /// Its `tasks.json` row, as an agent's task has one (`as` says who did it).
    fn row(&self, step: &str, agent: &str, text: &str, launched_ms: u128) -> serde_json::Value {
        serde_json::json!({
            "step": step,
            "agent": agent,
            "agent_id": OWNER,
            "as": OWNER,
            "task_id": "",
            "task": text,
            "state": if self.error.is_none() { "Completed" } else { "Failed" },
            "error": self.error,
            "actions": self.actions,
            "apps": self.apps,
            "resources": self.resources,
            "content": [],
            "content_actions": [],
            "restarted": 0,
            "step_seen_unix_ms": [],
            "launched_at_unix_ms": launched_ms,
            "ended_at_unix_ms": unix_ms(),
        })
    }
}

/// Carry out the written steps `text` as the room's owner: each through the
/// host's room actions on the room's record of its app (opened first when
/// the room has none), as a person acting on a record nobody has open. Then
/// wait until the room has heard every change, so agents act on them.
async fn run_as_owner(browser: &Browser, text: &str) -> OwnerRun {
    let steps: Vec<serde_json::Value> = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("an owner's task is written steps: {error}"));
    let mut run = OwnerRun::default();
    let mut changed: BTreeMap<DocumentRoute, usize> = BTreeMap::new();
    for step in &steps {
        let app = app_id(step["app"].as_str().expect("a written step's app"));
        let resource = step["resource"].as_str().unwrap_or_default().to_owned();
        let action = step["action"]
            .as_str()
            .expect("a written step's action")
            .to_owned();
        let arguments = step["arguments"].as_object().cloned().unwrap_or_default();
        let started_ms = unix_ms();
        let route = owner_record(browser, &app).await;
        let outcome = match &route {
            Ok(route) => {
                let past = browser.heard(route);
                let outcome = browser
                    .invoke(
                        &record_provider(route),
                        ActionAddress::new(resource.clone(), action.clone()),
                        InvokeArgument::Json(arguments.clone()),
                    )
                    .await;
                if outcome.is_ok() {
                    changed.entry(route.clone()).or_insert(past);
                }
                outcome
            }
            Err(error) => Err(error.clone()),
        };
        log_owner_action(
            route.as_ref().ok(),
            &app,
            &resource,
            &action,
            &arguments,
            started_ms,
            &outcome,
        );
        run.actions.push(action.clone());
        run.apps.push(app);
        run.resources.push(resource);
        if let Err(error) = outcome {
            run.error = Some(format!("{action}: {error}"));
            break;
        }
    }
    for (route, past) in &changed {
        if !browser.until_heard(route, *past).await {
            eprintln!(
                "bench: the owner's change to {} {} was not heard in time",
                route.app_id, route.record
            );
        }
    }
    // The agents hear the room as the page does, over the same host.
    tokio::time::sleep(Duration::from_millis(250)).await;
    run
}

/// The room's record of `app` an owner's written step acts on: the one the
/// room holds, else one the host opens for the owner.
async fn owner_record(browser: &Browser, app: &str) -> Result<DocumentRoute, String> {
    let page = browser.catalogue(CatalogueQuery::default()).await?;
    let held: Vec<DocumentRoute> = page
        .providers
        .iter()
        .filter(|provider| provider.app_id == app && !provider.record_id.is_empty())
        .filter(|provider| {
            ui::record::RecordId::parse(&provider.record_id)
                .is_none_or(|id| id.chain_part().is_none())
        })
        .map(|provider| DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: ROOM.to_owned(),
            record: provider.record_id.clone(),
        })
        .collect();
    match held.as_slice() {
        [one] => Ok(one.clone()),
        [] => {
            let open = application_open_target(ROOM, app);
            let opened = browser
                .invoke(&open.provider, open.address, InvokeArgument::None)
                .await?;
            let record = opened
                .value
                .as_ref()
                .and_then(|value| value["record"].as_str())
                .ok_or_else(|| format!("the host opened no record of {app}"))?;
            Ok(DocumentRoute {
                app_id: app.to_owned(),
                cluster_id: ROOM.to_owned(),
                record: record.to_owned(),
            })
        }
        many => Err(format!(
            "the room holds {} records of {app}; an owner's written step names none",
            many.len()
        )),
    }
}

/// One line of `TILO_AGENT_ACTION_LOG` for an action the owner ran, as an
/// agent's are written, `as` saying who ran it: a replay runs it as the
/// owner too.
fn log_owner_action(
    route: Option<&DocumentRoute>,
    app: &str,
    resource: &str,
    action: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
    started_ms: u128,
    outcome: &Result<ActionOutput, String>,
) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("TILO_AGENT_ACTION_LOG") else {
        return;
    };
    let output = outcome.as_ref().ok();
    let line = serde_json::json!({
        "started_unix_ms": started_ms,
        "ended_unix_ms": unix_ms(),
        "agent": OWNER,
        "as": OWNER,
        "app": app,
        "record": route.map_or("", |route| route.record.as_str()),
        "resource": resource,
        "action": action,
        "arguments": arguments,
        "ok": outcome.is_ok(),
        "error": outcome.as_ref().err(),
        "produced": output.map(|output| {
            output
                .resources
                .iter()
                .filter(|resource| tilo_actions::content_hash(resource).is_some())
                .cloned()
                .collect::<Vec<_>>()
        }).unwrap_or_default(),
        "resources": output.map(|output| output.resources.clone()),
        "value": output.and_then(|output| output.value.clone()),
    });
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()))
        .unwrap();
}

/// Environment:
/// - `TILO_BENCH_OUT`: run directory (outputs, `tasks.json`, `timing.json`,
///   `content/`).
/// - `TILO_BENCH_SCENARIO`: the scenario file (`bench/campaign/scenarios/*.json`).
/// - `TILO_BENCH_MODEL` (`gpt-6-astra`): a `claude-` model plans through
///   Anthropic's Messages API at `TILO_BENCH_ANTHROPIC_URL`
///   (`https://api.anthropic.com`); any other through `OpenAI`'s Responses
///   API at `TILO_BENCH_BASE_URL` (`https://api.openai.com/v1`; another
///   address takes chat completions unless `TILO_AGENT_OPENAI_API` is
///   `responses`), or through chat completions at
///   `TILO_BENCH_OPENAI_COMPAT_URL` when set (a local server or a proxy);
///   `scripted` plans with no model and needs no key (written steps).
/// - `TILO_AGENT_API_KEY_FILE`: the provider's key, read by the host's model
///   client before each call.
/// - `TILO_AGENT_DECOMPOSE_ITEM_MODEL`, with `TILO_AGENT_DECOMPOSE=1`: the
///   items of a coordinated task plan with this model, the coordinator with
///   `TILO_BENCH_MODEL`; its key in `TILO_AGENT_DECOMPOSE_ITEM_API_KEY_FILE`
///   and its preset in `TILO_AGENT_DECOMPOSE_ITEM_PRESET`
///   ([`tilo_agent_participant::profile::DECOMPOSE_ITEM_MODEL_ENV`]).
/// - `TILO_BENCH_TASK_SECS` (600): how long a step may run.
/// - `TILO_AGENT_USAGE_LOG`, `TILO_AGENT_ACTION_LOG`, `TILO_AGENT_PLAN_LOG`
///   and any other `TILO_AGENT_*` setting reach the agents as they are.
/// - `TILO_BENCH_TRACE`: an env filter for the agents' trace on stderr.
/// - `TILO_BENCH_RESPAWN_MS` (0): how long a killed agent stays down.
/// - `TILO_BENCH_REQUIRE_DONE`: fail unless every task completed and every
///   output was produced.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "campaign benchmark: spends real model tokens; run explicitly"]
async fn campaign_bench() {
    if ran_isolated() {
        return;
    }
    if let Ok(filter) = std::env::var("TILO_BENCH_TRACE") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_writer(std::io::stderr)
            .try_init();
    }
    let out = PathBuf::from(std::env::var("TILO_BENCH_OUT").expect("TILO_BENCH_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let out = out.canonicalize().unwrap();
    let scenario = Scenario::load(Path::new(
        &std::env::var("TILO_BENCH_SCENARIO").expect("TILO_BENCH_SCENARIO"),
    ));
    let state = tempfile::tempdir().unwrap();
    let model = model_from_env(&state.path().join("model-keys"));
    eprintln!("bench: agents plan with {}", model.describe());
    let task_limit = Duration::from_secs(env_or("TILO_BENCH_TASK_SECS", "600").parse().unwrap());

    let (root, _) = tilo_user_identity::UserIdentity::generate("Bench", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let hosted = start_with(&root, &host, state.path(), model).await;
    let _steward = steward::Steward::start(&hosted.relay, &root).await;
    let confirming = Arc::new(std::sync::Mutex::new(BTreeSet::new()));
    let _approver = approve_agents(
        &hosted.relay,
        root.clone(),
        &laptop,
        Arc::clone(&confirming),
    );
    eprintln!("bench: host started for scenario {}", scenario.id);
    let browser = Browser::open(&hosted, &browser_device, &host).await;

    let mut grants = BTreeMap::new();
    for role in &scenario.agents {
        grants.insert(role.name.clone(), grant_for(&browser, role).await);
    }
    eprintln!("bench: {} grants approved", grants.len());
    if let Ok(mut agents) = confirming.lock() {
        agents.extend(
            scenario
                .agents
                .iter()
                .filter(|role| role.may_confirm)
                .flat_map(|role| [Some(role.name.clone()), grants.get(&role.name).cloned()])
                .flatten(),
        );
    }
    // The team joins the room before the work begins, as people are in it
    // before anyone hands out tasks: each agent holds the room's keys when
    // the records it will work on are made. The clock starts after.
    for grant in grants.values() {
        spawn(&browser, grant).await;
    }
    browser
        .until("every agent to join the room", |status| {
            grants.values().all(|grant| idle(status, grant))
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
    let mut events_done = vec![false; scenario.events.len()];
    let mut fault_log = Vec::new();
    // The rows of the tasks the owner carried out, by step, kept with the
    // step's agents' when it ends.
    let mut owner_rows: BTreeMap<usize, Vec<serde_json::Value>> = BTreeMap::new();
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
            // The owner's tasks first, carried out here and heard in the
            // room before the step's agents are asked to act on what they
            // made.
            let mut rows = Vec::new();
            for task in stage.tasks.iter().filter(|task| task.actor == Actor::Owner) {
                let launched_ms = unix_ms();
                let text = fill(&task.text);
                let run = run_as_owner(&browser, &text).await;
                eprintln!(
                    "bench: step {}: the owner ran {} written step(s) for {}{}",
                    stage.id,
                    run.actions.len(),
                    task.agent,
                    run.error
                        .as_ref()
                        .map_or_else(String::new, |error| format!("; failed: {error}"))
                );
                rows.push(run.row(&stage.id, &task.agent, &text, launched_ms));
            }
            owner_rows.insert(index, rows);
            let mut issued = Vec::new();
            for Task { agent, text, .. } in
                stage.tasks.iter().filter(|task| task.actor == Actor::Agent)
            {
                let grant = grants
                    .get(agent)
                    .unwrap_or_else(|| panic!("step {} names an unknown agent {agent}", stage.id))
                    .clone();
                let text = fill(text);
                let task = message(&browser, &grant, &text).await;
                issued.push(Issued {
                    agent: agent.clone(),
                    grant,
                    text,
                    task,
                    launched_ms: unix_ms(),
                    restarted: 0,
                    seen_ms: Vec::new(),
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
        // Events that are due happen in the room now.
        for (number, event) in scenario.events.iter().enumerate() {
            if events_done[number] || run_started.elapsed() < event.at {
                continue;
            }
            events_done[number] = true;
            let grant = grants[&event.agent].clone();
            let task = message(&browser, &grant, &fill(&event.text)).await;
            eprintln!(
                "bench: event {} at {:.1}s",
                event.label,
                run_started.elapsed().as_secs_f64()
            );
            fault_log.push(serde_json::json!({
                "kind": "event",
                "label": event.label,
                "agent": event.agent,
                "task_id": task,
                "at_seconds": run_started.elapsed().as_secs_f64(),
            }));
        }
        // Harness-side faults that are due.
        for (number, fault) in scenario.faults.iter().enumerate() {
            if faults_done[number] || run_started.elapsed() < fault.at {
                continue;
            }
            faults_done[number] = true;
            let outcome = match (fault.kind.as_str(), fault.agent.as_ref()) {
                ("kill_agent", Some(agent)) => {
                    let grant = grants[agent].clone();
                    restart_agent(&browser, &grant, &mut launched).await
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
        let now = unix_ms();
        for (_, issued) in launched.values_mut() {
            for one in issued.iter_mut() {
                if let Some(task) = task_in(&status, &one.grant, &one.task) {
                    while one.seen_ms.len() < task.steps.len() {
                        one.seen_ms.push(now);
                    }
                }
            }
        }
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
            for mut row in owner_rows.remove(&index).unwrap_or_default() {
                row["step_seconds"] = step_started.elapsed().as_secs_f64().into();
                row["ended_at_seconds"] = run_started.elapsed().as_secs_f64().into();
                log.push(row);
            }
            for one in issued {
                let task = task_in(&status, &one.grant, &one.task);
                log.push(task_row(
                    &scenario,
                    index,
                    one,
                    task.as_ref(),
                    (
                        step_started.elapsed().as_secs_f64(),
                        run_started.elapsed().as_secs_f64(),
                    ),
                ));
            }
            std::fs::write(
                out.join("tasks.json"),
                serde_json::to_string_pretty(&log).unwrap(),
            )
            .unwrap();
        }
    }
    let finished = std::time::SystemTime::now();
    let mut missing = Vec::new();
    for (step, agent, file) in &scenario.outputs {
        if let Some(bytes) =
            produced(&log, step, agent).and_then(|address| hosted.agents.content(ROOM, &address))
        {
            std::fs::write(out.join(file), bytes).unwrap();
        } else {
            missing.push(file.clone());
        }
    }
    // Every piece of content the run made, by its address, so a replay
    // (`actions.jsonl` applied again) finds what one step handed another.
    let content_dir = out.join("content");
    std::fs::create_dir_all(&content_dir).unwrap();
    for address in log
        .iter()
        .filter_map(|row| row["content"].as_array())
        .flatten()
        .filter_map(serde_json::Value::as_str)
    {
        if let Some(bytes) = hosted.agents.content(ROOM, address) {
            let name = address.rsplit(':').next().unwrap_or(address);
            std::fs::write(content_dir.join(name), bytes).unwrap();
        }
    }
    let unfinished: Vec<String> = log
        .iter()
        .filter(|row| row["state"] != "Completed")
        .map(|row| format!("{} ({}): {}", row["step"], row["agent"], row["error"]))
        .collect();
    let unix = |t: std::time::SystemTime| {
        t.duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    };
    std::fs::write(
        out.join("timing.json"),
        serde_json::json!({"scenario": scenario.id, "started_at": unix(started),
                           "finished_at": unix(finished), "agents": grants,
                           "faults": fault_log, "missing_outputs": missing})
        .to_string(),
    )
    .unwrap();
    eprintln!(
        "bench: done in {:.1}s; {} unfinished task(s), missing outputs {missing:?}",
        run_started.elapsed().as_secs_f64(),
        unfinished.len()
    );
    drop(browser);
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
    if std::env::var_os("TILO_BENCH_REQUIRE_DONE").is_some() {
        assert!(
            unfinished.is_empty() && missing.is_empty(),
            "tasks not completed: {unfinished:?}; outputs missing: {missing:?}"
        );
    }
}

/// Stop the agent of `grant`, spawn it again on the same grant and give it
/// again each of its launched tasks that had not ended, as a supervisor
/// restarting a crashed worker would. Says what happened.
async fn restart_agent(
    browser: &Browser,
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
    let down = env_or("TILO_BENCH_RESPAWN_MS", "0").parse().unwrap_or(0);
    tokio::time::sleep(Duration::from_millis(down)).await;
    spawn(browser, grant).await;
    browser
        .until("the restarted agent to join", |status| idle(status, grant))
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
