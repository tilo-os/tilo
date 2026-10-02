//! A simulated browser and an agent share one room on a local host.
//!
//! The browser is the page's session code built natively: HTTPS for
//! documents and remote actions, WebTransport `moq-lite` for presence, and
//! the app mounted from the same component the browser runtime mounts. The
//! agent is a host-side participant over iroh with its own device key,
//! certified under the user's root through the ordinary certification flow
//! (the laptop approves it), and a root-signed delegation the host enforces.
//!
//! The browser spawns the agent through the host's remote actions. The room
//! has no channel keys, so neither the page nor the agent writes a document:
//! the page holds its record, the host retains no document, and the agent's
//! supervision (status, delegation, pause and stop) works without one.

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

#[path = "../../../browser/tilo-browser-peer/tests/support/mod.rs"]
mod support;

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_actions::InvocationId;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_agent_participant::{
    AgentIdentity, AgentParticipant, AgentSupervisor, DirectPlanner, HostedApp, ParticipantConfig,
    Seed, SupervisorHost,
};
use tilo_browser_peer::{DocumentRoute, Host, PeerEvent, PeerHandle, certify};
use tilo_relay::certify::{ApproverDecision, ApproverReply};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig};
use tilo_relay_client::Approver;
use tilo_relay_protocol::agents::{
    AGENTS_PROVIDER, AgentDelegation, AgentScope, AgentStatus, AgentsStatus, MESSAGE_ACTION,
    PAUSE_ACTION, SPAWN_ACTION, STATUS_ACTION, STOP_ACTION, SUPERVISOR_RESOURCE, TaskState,
    agent_resource,
};
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, DeviceCert, RevocationList, UserIdentity,
    issue_device_cert,
};

use common::catalogue;
use support::{Device, ROOM, TestHost, enrol, ready, start_peer, wait_for, workspace};

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
        invocation,
        target: RemoteActionTarget {
            provider: AGENTS_PROVIDER.into(),
            address,
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_beside_a_browser_writes_nothing_in_the_clear() {
    common::isolate_device();
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
    // mounted from its component on a new record. The room has no channel
    // keys, so the page holds the record instead of writing it in the clear.
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
    let mut page = HostedApp::open(&catalogue, SCRATCHPAD, ROOM, "ada", false, Seed::New).unwrap();
    let route = DocumentRoute {
        app_id: SCRATCHPAD.into(),
        cluster_id: ROOM.into(),
        record: page.record().to_owned(),
    };
    browser
        .publish(route.clone(), &page.take_mutation().unwrap())
        .unwrap();
    let durable = browser.barrier().unwrap();

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
    let mut agent = AgentParticipant::start(
        ParticipantConfig {
            host: host.iroh(),
            host_cert: host_cert.clone(),
            room: ROOM.into(),
        },
        identity,
        control.clone(),
        catalogue.clone(),
        Box::new(DirectPlanner),
    )
    .unwrap();

    // 4. The agent takes the task up, but nothing it or the page wrote
    // reaches the host or the page in the clear: the page's change stays
    // held, and the host retains no document at all.
    let host_key = host_cert.device_public_key;
    wait_for("the agent to take its task up", || {
        agent.pump();
        let started = control
            .info()
            .tasks
            .first()
            .is_some_and(|task| task.state != TaskState::Queued);
        async move { started.then_some(()) }
    })
    .await;
    let after = status(&browser, host_key, &mut agent).await;
    assert_eq!(
        after.agents[0].device,
        hex::encode(agent_cert.device_public_key)
    );
    let carried =
        AgentDelegation::decode(&hex::decode(&after.agents[0].delegation).unwrap()).unwrap();
    assert_eq!(carried.verify(&user, tilo_clock::unix_now_ns()), Ok(()));
    assert!(
        durable.try_recv().is_err(),
        "the page's record is held for the room's keys"
    );
    let retained = host
        .relay
        .room_records()
        .retained(workspace())
        .await
        .unwrap();
    assert_eq!(
        common::retained_documents(retained.into_iter().map(|kept| (kept.author, kept.body))),
        (0, 0),
        "the host retains no document from the page or the agent"
    );
    assert!(
        !browser_events
            .drain()
            .iter()
            .any(|event| matches!(event, PeerEvent::Document(document) if document.route == route)),
        "no document reaches the page in the clear"
    );

    // 5. The host keeps the agent to its delegated rooms.
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

    // 6. Pause holds the next task, stop ends the agent.
    let agent_id = agent_resource("writer");
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
    let held = status(&browser, host_key, &mut agent).await;
    assert_eq!(held.agents[0].status, AgentStatus::Paused);
    assert_eq!(held.agents[0].tasks[1].state, TaskState::Queued);
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

    agent.close();
    page.close();
    browser.close();
    host.relay.shutdown().await;
}
