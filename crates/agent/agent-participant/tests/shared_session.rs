//! A simulated browser and an agent share one room on a local host.
//!
//! The browser is the page's session code built natively: HTTPS for
//! documents and remote actions, WebTransport `moq-lite` for presence, and
//! the app mounted from the same component the browser runtime mounts. The
//! agent is a host-side participant over iroh with its own device key,
//! certified under the user's root through the ordinary certification flow
//! (the laptop approves it), and a root-signed delegation the host enforces.
//!
//! The browser spawns the agent through the host's remote actions; the agent
//! opens the browser's record and edits it through the app's own action;
//! the browser receives the durable change in host order and sees the agent
//! on the record; the agent sees the browser's cursor; pause, resume and
//! stop hold, release and end the agent. Between pumps the agent's loop
//! sleeps until something changes, and a plan of local steps runs back to
//! back.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    clippy::future_not_send,
    reason = "in a test these are the assertion, not a hazard; one scenario in order, \
              driven on the test's own thread (the agent's apps are not Send)"
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

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_actions::InvocationId;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_agent_participant::{
    AgentIdentity, AgentParticipant, AgentSupervisor, DirectPlanner, ParticipantConfig, Replica,
    Seed, SupervisorHost,
};
use tilo_browser_peer::{DocumentRoute, Host, LiveState, PeerEvent, PeerHandle, certify};
use tilo_relay::certify::{ApproverDecision, ApproverReply};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig};
use tilo_relay_client::Approver;
use tilo_relay_protocol::agents::{
    AGENTS_PROVIDER, AgentDelegation, AgentScope, AgentStatus, AgentsStatus, MESSAGE_ACTION,
    PAUSE_ACTION, RESUME_ACTION, SPAWN_ACTION, STATUS_ACTION, STOP_ACTION, SUPERVISOR_RESOURCE,
    TaskState, agent_resource,
};
use tilo_relay_protocol::live::{LiveFrame, LiveParticipant, LiveRecord, ParticipantKind};
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, DeviceCert, RevocationList, UserIdentity,
    issue_device_cert,
};

use common::catalogue;
use support::{Device, ROOM, TestHost, enrol, ready, start_peer, wait_for, workspace};

/// Plans the task `gated` as a slow model would: only once its steps arrive
/// on `release`, noting when the plan was ready. Anything else is planned as
/// written.
struct GatedPlanner {
    release: std::sync::mpsc::Receiver<serde_json::Value>,
    ready: Arc<std::sync::Mutex<Option<Instant>>>,
}

impl tilo_agent_participant::Planner for GatedPlanner {
    fn plan(
        &mut self,
        task: &str,
        context: &tilo_agent_participant::PlanContext,
    ) -> Result<Vec<tilo_agent_participant::Step>, String> {
        use tilo_agent_participant::Planner as _;
        if task != "gated" {
            return DirectPlanner.plan(task, context);
        }
        let steps = self.release.recv().map_err(|error| error.to_string())?;
        let planned = DirectPlanner.plan(&steps.to_string(), context);
        *self.ready.lock().unwrap() = Some(Instant::now());
        planned
    }
}

const SCRATCHPAD: &str = "so.tilo.scratchpad";
const OTHER_ROOM: &str = "0b7f3c2e-5d41-4a8f-9e36-2c1d7a9b4e50";

/// The host with its agent supervisor as the app-serving plane.
async fn start_host(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    supervisor: &Arc<AgentSupervisor>,
) -> TestHost {
    let user = root.verifying_key().to_bytes();
    let mut cfg = tilo_relay::RelayConfig {
        state_root: state.to_path_buf(),
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
    cfg.services.app_serving = true;
    let relay = Relay::start_with_app_serving(
        cfg,
        RelayIdentity {
            user_public_key: user,
            device_signing_key: host.key.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
        Some(Arc::new(tilo_relay::ActionHostPlane::new(SupervisorHost(
            supervisor.clone(),
        )))),
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
    TestHost { relay, web }
}

/// The laptop approves the one pending request as an agent device: a
/// capability-less certificate, the consent step for the agent's identity.
async fn approve_agent(host: &TestHost, root: &SigningKey, laptop: &Device) -> String {
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let mut approver = Approver::connect(
        &endpoint,
        host.relay.node_addr(),
        laptop.cert.clone(),
        &laptop.key,
        &RevocationList::new(),
    )
    .await
    .unwrap();
    let pending = loop {
        if let Some(pending) = approver.list_pending().await.unwrap().pop() {
            break pending;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(pending.request.device_label.starts_with("Agent: "));
    let cert = issue_device_cert(
        &pending.request,
        root,
        &CertificationPolicy::agent(),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(
        approver
            .decide(pending.request_digest, ApproverDecision::Issue(cert))
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    approver.close();
    pending.pairing_code
}

/// One remote action from the browser to the host's agent supervisor.
async fn call(
    browser: &PeerHandle,
    host_device: [u8; 32],
    agent: Option<&mut AgentParticipant>,
    address: ActionAddress,
    argument: InvokeArgument,
) -> InvocationReceipt {
    let invocation = InvocationId::new();
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: AGENTS_PROVIDER.into(),
            address,
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
    }))
    .encode()
    .unwrap();
    let events = browser.events();
    browser
        .send_remote_action(workspace(), host_device, frame)
        .unwrap();
    let mut agent = agent;
    wait_for("the host's receipt", || {
        if let Some(agent) = agent.as_deref_mut() {
            agent.pump();
        }
        let receipt = events.drain().into_iter().find_map(|event| match event {
            PeerEvent::RemoteAction { from, frame, .. } if from == host_device => {
                match RemoteActionFrame::decode(&frame).unwrap().message {
                    RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation => {
                        Some(receipt)
                    }
                    _ => None,
                }
            }
            PeerEvent::RemoteActionUndeliverable { reason, .. } => {
                panic!("undeliverable: {reason}")
            }
            _ => None,
        });
        async move { receipt }
    })
    .await
}

fn completed(receipt: &InvocationReceipt) -> serde_json::Value {
    match &receipt.status {
        ReceiptStatus::Completed { output } => output.value.clone().unwrap_or_default(),
        other => panic!("not completed: {other:?}"),
    }
}

async fn status(
    browser: &PeerHandle,
    host_device: [u8; 32],
    agent: &mut AgentParticipant,
) -> AgentsStatus {
    let receipt = call(
        browser,
        host_device,
        Some(agent),
        ActionAddress::new(SUPERVISOR_RESOURCE, STATUS_ACTION),
        InvokeArgument::None,
    )
    .await;
    serde_json::from_value(completed(&receipt)).unwrap()
}

/// Apply every durable change of `route` the browser receives, checking
/// each was authored by `author`, until the record contains `text`.
async fn receive_until(
    agent: &mut AgentParticipant,
    events: &tilo_browser_peer::PeerEvents,
    route: &DocumentRoute,
    page: &mut Replica,
    author: [u8; 32],
    text: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !body(page).contains(text) {
        assert!(
            Instant::now() < deadline,
            "the browser's record never carried {text:?}: {}\nagent tasks: {:?}\nagent records: {:?}\nagent's copy: {:?}\nagent channels: {}",
            body(page),
            agent.control().info().tasks,
            agent.known_records(),
            agent.app(route).map(body),
            agent.peer().status().channels,
        );
        agent.pump();
        for event in events.drain() {
            if let PeerEvent::Document(document) = event
                && document.route == *route
            {
                assert_eq!(document.author, author, "only the agent edits here");
                page.apply(&document.mutation().unwrap()).unwrap();
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn body(app: &impl common::Body) -> String {
    app.body()
}

/// Open a sealed live frame with `peer`'s channel keys.
async fn open_live(peer: &PeerHandle, frame: Vec<u8>) -> Option<LiveFrame> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    peer.open_live_then(
        workspace(),
        frame,
        Box::new(move |opened| {
            let _ = tx.send(opened);
        }),
    )
    .ok()?;
    LiveFrame::decode(&rx.await.ok()??).ok()
}

/// Seal a clear live frame with `peer`'s channel keys for wire publish.
async fn seal_live(peer: &PeerHandle, frame: Vec<u8>) -> Vec<u8> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    peer.seal_live_then(
        workspace(),
        frame,
        Box::new(move |sealed| {
            let _ = tx.send(sealed);
        }),
    )
    .unwrap();
    rx.await.unwrap().expect("the page holds the record's keys")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_edits_beside_a_browser_and_both_see_each_other() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let user = root.verifying_key().to_bytes();
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();

    // The supervisor hands each spawned agent to this thread, which drives
    // its participant (in a host, its own thread does).
    let (spawned_tx, spawned_rx) = std::sync::mpsc::channel();
    let spawned_tx = std::sync::Mutex::new(spawned_tx);
    let supervisor = AgentSupervisor::new(
        user,
        Arc::new(move |identity, control| {
            spawned_tx
                .lock()
                .unwrap()
                .send((identity, control))
                .map_err(|error| error.to_string())
        }),
    );
    let host = start_host(&root, &host_device, state.path(), &supervisor).await;
    let host_cert: DeviceCert = host_device.cert.clone();
    let _steward = steward::Steward::start(&host.relay, &root).await;

    // 1. The agent's identity: its own key, certified under the root by the
    // laptop (consent), and a root-signed delegation the host registers.
    let agent_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (code_tx, code_rx) = tokio::sync::oneshot::channel::<String>();
    let certifying = tokio::spawn({
        let iroh = host.iroh();
        let key = agent_key.clone();
        async move {
            certify(&iroh, user, "Agent: Writer helper", &key, |code| {
                let _ = code_tx.send(code.to_string());
            })
            .await
        }
    });
    let shown = code_rx.await.unwrap();
    assert_eq!(approve_agent(&host, &root, &laptop).await, shown);
    let (agent_cert, _) = certifying.await.unwrap().unwrap();
    assert_eq!(
        agent_cert.capabilities,
        CertificationPolicy::agent().ceiling
    );
    let now = tilo_clock::unix_now_ns();
    let delegation = AgentDelegation::sign(
        &root,
        agent_cert.device_public_key,
        "writer",
        "Writer helper",
        [0x7c, 0x3a, 0xed],
        AgentScope {
            rooms: vec![ROOM.into()],
            apps: vec![SCRATCHPAD.into()],
            may_confirm: false,
        },
        now,
        now + 3_600_000_000_000,
    )
    .unwrap();
    host.relay.register_agent(delegation.clone()).unwrap();
    supervisor.add_grant(AgentIdentity {
        key: agent_key,
        cert: agent_cert.clone(),
        delegation: delegation.clone(),
    });

    // 2. The browser: a certified device over HTTPS, with Scratchpad
    // mounted from its component on a new record, committed to the host.
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let browser = start_peer(
        Host::Https(host.https()),
        browser_device.cert.clone(),
        host_cert.clone(),
        browser_device.key.clone(),
    );
    let browser_events = browser.events();
    browser.join(workspace()).unwrap();
    ready(&[&browser]).await;
    let catalogue = catalogue();
    let mut page = Replica::open(&catalogue, SCRATCHPAD, ROOM, "ada", false, Seed::New).unwrap();
    let route = DocumentRoute {
        app_id: SCRATCHPAD.into(),
        cluster_id: ROOM.into(),
        record: page.record().to_owned(),
    };
    browser
        .publish(route.clone(), &page.take_mutation().unwrap())
        .unwrap();
    let receipt = browser.barrier().unwrap();
    wait_for("the page's record to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();

    // 3. The browser spawns the agent through the host, with a task that
    // edits the page's record through Scratchpad's own action.
    let task = serde_json::json!([{
        "app": SCRATCHPAD,
        "record": route.record,
        "resource": "scratchpad.note",
        "action": "edit",
        "arguments": {"text": "Drafted by the agent"},
    }])
    .to_string();
    let mut spawn_arguments = serde_json::Map::new();
    spawn_arguments.insert("grant".into(), "writer".into());
    spawn_arguments.insert("task".into(), task.into());
    let spawned = call(
        &browser,
        host_cert.device_public_key,
        None,
        ActionAddress::new(SUPERVISOR_RESOURCE, SPAWN_ACTION),
        InvokeArgument::Json(spawn_arguments),
    )
    .await;
    assert_eq!(completed(&spawned)["agentId"], "writer");
    let (identity, control) = spawned_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (release_plan, release) = std::sync::mpsc::channel();
    let plan_ready = Arc::new(std::sync::Mutex::new(None));
    let mut agent = AgentParticipant::start(
        ParticipantConfig {
            host: host.iroh(),
            host_cert: host_cert.clone(),
            room: ROOM.into(),
        },
        identity,
        control.clone(),
        catalogue.clone(),
        Box::new(GatedPlanner {
            release,
            ready: Arc::clone(&plan_ready),
        }),
    )
    .unwrap();

    // 4. The agent's edit reaches the browser as durable changes in host
    // order, authored by the agent's device.
    receive_until(
        &mut agent,
        &browser_events,
        &route,
        &mut page,
        agent_cert.device_public_key,
        "Drafted by the agent",
    )
    .await;
    let status_after = status(&browser, host_cert.device_public_key, &mut agent).await;
    let info = &status_after.agents[0];
    assert_eq!(info.status, AgentStatus::Waiting);
    assert_eq!(info.tasks[0].state, TaskState::Completed);
    assert_eq!(info.tasks[0].steps[0].action, "edit");
    assert_eq!(info.device, hex::encode(agent_cert.device_public_key));
    let carried = AgentDelegation::decode(&hex::decode(&info.delegation).unwrap()).unwrap();
    assert_eq!(carried.verify(&user, tilo_clock::unix_now_ns()), Ok(()));

    // 5. The browser's live session over WebTransport sees the agent:
    // presence of kind agent, focused on the record.
    let (grant_tx, grant_rx) = tokio::sync::oneshot::channel();
    browser
        .live_grant_then(
            workspace(),
            Box::new(move |grant| {
                let _ = grant_tx.send(grant);
            }),
        )
        .unwrap();
    let grant = grant_rx.await.unwrap().unwrap();
    let url: url::Url = format!("https://127.0.0.1:{}/moq/live", host.web.addr.port())
        .parse()
        .unwrap();
    let trust =
        tilo_moq::carrier::Trust::Fingerprints(vec![host.web.certificate_sha256.clone().unwrap()]);
    let own_namespace = tilo_moq::live::namespace_path(&grant.room, &grant.publisher).unwrap();
    let mut live =
        tilo_moq::live::peer::Peer::connect(&url, &trust, &grant.token, Some(&own_namespace))
            .await
            .unwrap();
    let agent_namespace =
        tilo_moq::live::namespace_path(&grant.room, &hex::encode(agent_cert.device_public_key))
            .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut track = None;
    let presence = loop {
        assert!(
            Instant::now() < deadline,
            "the agent's presence never reached the browser"
        );
        agent.pump();
        let Some(subscribed) = track.as_mut() else {
            if let Ok(Some(broadcast)) =
                tokio::time::timeout(Duration::from_millis(200), live.member(&agent_namespace))
                    .await
            {
                track = broadcast
                    .track(tilo_moq::live::PRESENCE_TRACK)
                    .unwrap()
                    .subscribe(None)
                    .await
                    .ok();
            }
            continue;
        };
        if let Ok(Ok(Some(datagram))) =
            tokio::time::timeout(Duration::from_millis(200), subscribed.recv_datagram()).await
        {
            let Some(frame) = open_live(&browser, datagram.payload.to_vec()).await else {
                continue;
            };
            match frame {
                LiveFrame::Presence(presence) => break presence,
                LiveFrame::Ops(_) => {}
            }
        }
    };
    let participant = presence.participant.expect("the agent says what it is");
    assert_eq!(participant.kind, ParticipantKind::Agent);
    assert_eq!(participant.label, "Writer helper");
    assert_eq!(participant.color, [0x7c, 0x3a, 0xed]);
    assert_eq!(
        presence.focus.map(|focus| (focus.app, focus.record)),
        Some((SCRATCHPAD.to_owned(), route.record.clone()))
    );

    // 6. The browser's cursor, published over WebTransport, reaches the
    // agent (an iroh peer) through the host's bridge.
    let browser_key = browser_device.cert.device_public_key;
    let mut page_live = LiveState::starting_now();
    let deadline = Instant::now() + Duration::from_secs(20);
    let seen = loop {
        assert!(
            Instant::now() < deadline,
            "the browser's cursor never reached the agent"
        );
        let frame = page_live
            .publish_presence(tilo_clock::unix_now_ns() / 1_000_000, |presence| {
                presence.focus = Some(LiveRecord {
                    app: SCRATCHPAD.into(),
                    record: route.record.clone(),
                });
                presence.pointer = Some([12.0, 34.0]);
                presence.participant = Some(LiveParticipant {
                    kind: ParticipantKind::Person,
                    label: "Ada".into(),
                    color: [0x0e, 0xa5, 0xe9],
                    activity: None,
                });
            })
            .unwrap();
        live.publish_presence(&seal_live(&browser, frame).await);
        agent.pump();
        if let Some(member) = agent
            .remote_members()
            .into_iter()
            .find(|member| member.publisher == browser_key && member.presence.is_some())
        {
            break member;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let seen = seen.presence.unwrap();
    assert_eq!(seen.pointer, Some([12.0, 34.0]));
    assert_eq!(
        seen.participant.map(|participant| participant.kind),
        Some(ParticipantKind::Person)
    );

    // 7. The host keeps the agent to its delegated rooms.
    agent
        .peer()
        .join(tilo_relay_protocol::room_workspace_id(OTHER_ROOM).unwrap())
        .unwrap();
    wait_for("the host to refuse a room outside the delegation", || {
        agent.pump();
        let refused = agent
            .peer()
            .status()
            .failures
            .iter()
            .any(|failure| failure.code == "permission-required");
        async move { refused.then_some(()) }
    })
    .await;

    // 8. Pause holds the next task, resume releases it, stop ends the agent.
    let agent_id = agent_resource("writer");
    let host_key = host_cert.device_public_key;
    completed(
        &call(
            &browser,
            host_key,
            Some(&mut agent),
            ActionAddress::new(&agent_id, PAUSE_ACTION),
            InvokeArgument::None,
        )
        .await,
    );
    let second = serde_json::json!({
        "app": SCRATCHPAD,
        "record": route.record,
        "resource": "scratchpad.note",
        "action": "edit",
        "arguments": {"text": "Second draft"},
    })
    .to_string();
    completed(
        &call(
            &browser,
            host_key,
            Some(&mut agent),
            ActionAddress::new(&agent_id, MESSAGE_ACTION),
            InvokeArgument::Text(second),
        )
        .await,
    );
    for _ in 0..30 {
        agent.pump();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let held = status(&browser, host_key, &mut agent).await;
    assert_eq!(held.agents[0].status, AgentStatus::Paused);
    assert_eq!(held.agents[0].tasks[1].state, TaskState::Queued);
    assert!(
        !browser_events
            .drain()
            .iter()
            .any(|event| matches!(event, PeerEvent::Document(document) if document.route == route)),
        "a paused agent edits nothing"
    );
    completed(
        &call(
            &browser,
            host_key,
            Some(&mut agent),
            ActionAddress::new(&agent_id, RESUME_ACTION),
            InvokeArgument::None,
        )
        .await,
    );
    receive_until(
        &mut agent,
        &browser_events,
        &route,
        &mut page,
        agent_cert.device_public_key,
        "Second draft",
    )
    .await;

    // 9. Idle, the agent's loop sleeps until something changes: no pump on
    // an interval, only its presence keep-alive while it shows on a record
    // (a loop on a 20 ms interval pumped 50 times a second).
    let idle = agent.run_until(Some(Instant::now() + Duration::from_secs(1)), |_| false);
    eprintln!("idle for a second: {idle:?}");
    assert!(
        idle.pumps <= 10,
        "an idle agent pumped {} times in a second",
        idle.pumps
    );

    // 10. A plan of local steps runs back to back: the loop never waits
    // between them (a loop on a 20 ms interval took one step per pump).
    let steps: Vec<serde_json::Value> = (1..=10)
        .map(|n| {
            serde_json::json!({
                "app": SCRATCHPAD,
                "record": route.record,
                "resource": "scratchpad.note",
                "action": "edit",
                "arguments": {"text": format!("Step {n} of ten")},
            })
        })
        .collect();
    let ten = control
        .message(&serde_json::Value::Array(steps).to_string())
        .unwrap();
    let ended = |id: &str| {
        control
            .info()
            .tasks
            .into_iter()
            .find(|task| task.id == id)
            .filter(|task| !matches!(task.state, TaskState::Queued | TaskState::Running))
    };
    let started = Instant::now();
    let run = agent.run_until(Some(Instant::now() + Duration::from_secs(20)), |_| {
        ended(&ten).is_some()
    });
    let took = started.elapsed();
    eprintln!("ten local steps: {run:?} in {took:?}");
    let task = ended(&ten).expect("the ten-step task ended");
    assert_eq!(task.state, TaskState::Completed, "{task:?}");
    assert_eq!(task.steps.len(), 10);
    assert_eq!(
        run.waits, 0,
        "the loop waited between local steps: {run:?} in {took:?}"
    );
    receive_until(
        &mut agent,
        &browser_events,
        &route,
        &mut page,
        agent_cert.device_public_key,
        "Step 10 of ten",
    )
    .await;

    // 11. A plan that takes a while (a model call) is made beside the loop:
    // the loop goes on pumping meanwhile, and the plan's first step runs as
    // soon as it is ready.
    let planned_steps = serde_json::json!([{
        "app": SCRATCHPAD,
        "record": route.record,
        "resource": "scratchpad.note",
        "action": "edit",
        "arguments": {"text": "Planned beside the loop"},
    }]);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_500));
        release_plan.send(planned_steps).unwrap();
    });
    let gated = control.message("gated").unwrap();
    let started = Instant::now();
    agent.pump();
    agent.pump();
    let pumped = started.elapsed();
    eprintln!("two pumps while a 1.5 s plan was being made took {pumped:?}");
    let mut first_step = None;
    let run = agent.run_until(Some(Instant::now() + Duration::from_secs(20)), |_| {
        let task = control
            .info()
            .tasks
            .into_iter()
            .find(|task| task.id == gated);
        if first_step.is_none() && task.as_ref().is_some_and(|task| !task.steps.is_empty()) {
            first_step = Some(Instant::now());
        }
        task.is_some_and(|task| !matches!(task.state, TaskState::Queued | TaskState::Running))
    });
    releaser.join().unwrap();
    let task = ended(&gated).expect("the planned task ended");
    assert_eq!(task.state, TaskState::Completed, "{task:?}");
    let ready = plan_ready.lock().unwrap().expect("the plan was made");
    let to_first_step = first_step
        .expect("the plan's first step ran")
        .saturating_duration_since(ready);
    eprintln!("plan ready to its first step done: {to_first_step:?} ({run:?})");
    assert!(
        pumped < Duration::from_millis(1_000),
        "the loop waited for the planner: two pumps took {pumped:?}"
    );
    assert!(
        to_first_step < Duration::from_millis(500),
        "the plan's first step waited {to_first_step:?} after the plan was ready"
    );
    receive_until(
        &mut agent,
        &browser_events,
        &route,
        &mut page,
        agent_cert.device_public_key,
        "Planned beside the loop",
    )
    .await;
    completed(
        &call(
            &browser,
            host_key,
            Some(&mut agent),
            ActionAddress::new(&agent_id, STOP_ACTION),
            InvokeArgument::None,
        )
        .await,
    );
    agent.pump();
    assert_eq!(control.status(), AgentStatus::Stopped);
    let refused = call(
        &browser,
        host_key,
        Some(&mut agent),
        ActionAddress::new(&agent_id, MESSAGE_ACTION),
        InvokeArgument::Text("anything".into()),
    )
    .await;
    assert!(
        matches!(refused.status, ReceiptStatus::Rejected { .. }),
        "a stopped agent takes no task: {refused:?}"
    );

    // Every edit the host carried, the page's and the agent's, is sealed.
    let retained = host
        .relay
        .room_records()
        .retained(workspace())
        .await
        .unwrap();
    let (readable, sealed) = common::retained_documents(
        retained
            .into_iter()
            .map(|retained| (retained.author, retained.body)),
    );
    assert_eq!(readable, 0, "nothing reaches the host in the clear");
    assert!(sealed > 0);

    drop(live);
    agent.close();
    browser.close();
    host.relay.shutdown().await;
}
