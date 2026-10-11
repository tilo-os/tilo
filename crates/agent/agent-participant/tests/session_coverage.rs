//! Every web app over the real session: an agent (iroh, its own certified
//! device and delegation) opens the app, makes an edit through a published
//! operation found without app knowledge, and a browser (HTTPS documents,
//! WebTransport presence) receives the change authored by the agent, merges
//! it into its own replica of the record, and sees the agent on it.
//! Finally a natural-language task goes through `ModelPlanner`.

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
    clippy::single_match_else,
    reason = "in a test these are the assertion, not a hazard; one scenario in order, \
              driven on the test's own thread (the agent's apps are not Send)"
)]

mod common;

// Agents hold each record in a confined app worker (this binary, started
// again), whose changes are admitted through the Automerge isolate.
tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();
#[path = "../../../browser/tilo-browser-peer/tests/support/steward.rs"]
mod steward;
#[path = "../../../browser/tilo-browser-peer/tests/support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use common::{Driveable, KeywordModel, catalogue, find_edit};
use tilo_actions::InvocationId;
use tilo_actions::door::ActionAddress;
use tilo_actions::door::ActionDescriptor;
use tilo_actions::remote::{
    ActionHostError, CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, RemoteActionTarget, application_open_target,
};
use tilo_agent_participant::{
    AgentControl, AgentIdentity, AgentParticipant, DirectPlanner, ModelPlanner, ParticipantConfig,
    Replica, Seed, StepAction, record_provider,
};
use tilo_browser_peer::{DocumentRoute, Host, PeerEvent, PeerHandle};
use tilo_relay_protocol::agents::{AgentDelegation, AgentScope, AgentStatus, TaskState};
use tilo_relay_protocol::live::{LiveFrame, ParticipantKind, PresenceFrame};
use tilo_user_identity::{CertificationPolicy, DeviceCapabilities, UserIdentity};

use support::{ROOM, enrol, ready, start_host, start_peer, wait_for, workspace};

const ATTEMPTS: usize = 40;

struct Agent<'a> {
    agent: &'a mut AgentParticipant,
    route: DocumentRoute,
}

impl Driveable for Agent<'_> {
    fn survey(&mut self) -> Vec<ActionDescriptor> {
        self.agent.survey(&self.route)
    }
    fn body(&mut self) -> String {
        body(self.agent.app(&self.route).unwrap())
    }
    fn run(&mut self, action: &StepAction) -> Result<(), String> {
        match action {
            StepAction::Invoke { address, arguments } => self
                .agent
                .invoke(&self.route, address, arguments)
                .map(|_| ()),
            StepAction::Open | StepAction::Continue(_) => Ok(()),
        }
    }
}

/// Send `message` from the browser to the agent's device and wait for the
/// agent's answer, pumping the agent meanwhile.
async fn ask(
    browser: &tilo_browser_peer::PeerHandle,
    events: &tilo_browser_peer::PeerEvents,
    agent: &mut AgentParticipant,
    agent_key: [u8; 32],
    message: RemoteActionMessage,
    documents: &mut Vec<tilo_browser_peer::OpenedDocument>,
) -> RemoteActionMessage {
    browser
        .send_remote_action(
            workspace(),
            agent_key,
            RemoteActionFrame::new(message).encode().unwrap(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "the agent did not answer");
        agent.pump();
        // Take the whole batch before answering: a document behind the
        // answer would otherwise be dropped with it.
        let mut answer = None;
        for event in events.drain() {
            match event {
                PeerEvent::RemoteAction { from, frame, .. } if from == agent_key => {
                    answer
                        .get_or_insert_with(|| RemoteActionFrame::decode(&frame).unwrap().message);
                }
                PeerEvent::RemoteActionUndeliverable { reason, .. } => {
                    panic!("undeliverable: {reason}")
                }
                PeerEvent::Document(document) => documents.push(*document),
                _ => {}
            }
        }
        if let Some(answer) = answer {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn body(app: &impl common::Body) -> String {
    app.body()
}

/// The browser's copy of `route`: its own mount, seeded from every change
/// it received.
fn page(
    catalogue: &tilo_agent_participant::AppCatalogue,
    route: &DocumentRoute,
    changes: &[Vec<u8>],
) -> String {
    // The room's own record (Chat's) is not mounted from a copy: its
    // document is what the browser holds.
    let Some(record) = ui::record::RecordId::parse(&route.record) else {
        use tilo_automerge::ApplyIncoming as _;
        let mut doc = automerge::AutoCommit::new();
        for change in changes {
            doc.load_incoming(change).unwrap();
        }
        return serde_json::to_string(&automerge::AutoSerde::from(&doc)).unwrap();
    };
    let app = Replica::open(
        catalogue,
        &route.app_id,
        ROOM,
        "ada",
        false,
        Seed::Existing {
            record,
            changes: changes.to_vec(),
        },
    )
    .unwrap();
    body(&app)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_web_app_over_the_session() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let user = root.verifying_key().to_bytes();
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let catalogue = catalogue();

    // The agent: its own device under the root with no capabilities, and a
    // delegation covering this room and every web app.
    let agent_device = enrol(
        &root,
        "Agent: Coverage",
        CertificationPolicy::agent().ceiling,
    );
    let now = tilo_clock::unix_now_ns();
    let delegation = AgentDelegation::sign(
        &root,
        agent_device.cert.device_public_key,
        "coverage",
        "Coverage agent",
        [0x7c, 0x3a, 0xed],
        AgentScope {
            rooms: vec![ROOM.into()],
            apps: catalogue.iter().map(|d| d.app_id.to_owned()).collect(),
            may_confirm: false,
        },
        now,
        now + 3_600_000_000_000,
    )
    .unwrap();
    host.relay.register_agent(delegation.clone()).unwrap();
    let agent_key = agent_device.cert.device_public_key;
    let control = AgentControl::new(delegation.clone());
    let mut agent = AgentParticipant::start(
        ParticipantConfig {
            host: host.iroh(),
            host_cert: host_device.cert.clone(),
            room: ROOM.into(),
            content: tilo_content::ContentStore::memory(),
        },
        AgentIdentity {
            key: agent_device.key.clone(),
            cert: agent_device.cert.clone(),
            delegation,
        },
        control.clone(),
        catalogue.clone(),
        Box::new(ModelPlanner::new(Box::new(KeywordModel))),
    )
    .unwrap();

    // The browser: HTTPS for documents, WebTransport for presence.
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let browser = start_peer(
        Host::Https(host.https()),
        browser_device.cert.clone(),
        host_device.cert.clone(),
        browser_device.key.clone(),
    );
    let browser_events = browser.events();
    browser.join(workspace()).unwrap();
    ready(&[&browser]).await;
    wait_for("the agent to be admitted", || {
        agent.pump();
        let waiting = control.status() == AgentStatus::Waiting;
        async move { waiting.then_some(()) }
    })
    .await;

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
    let own = tilo_moq::live::namespace_path(&grant.room, &grant.publisher).unwrap();
    let live = tilo_moq::live::peer::Peer::connect(&url, &trust, &grant.token, Some(&own))
        .await
        .unwrap();
    let agent_namespace =
        tilo_moq::live::namespace_path(&grant.room, &hex::encode(agent_key)).unwrap();
    let mut track = None;

    let mut received: BTreeMap<DocumentRoute, Vec<Vec<u8>>> = BTreeMap::new();
    let mut table = String::from(
        "| App | Edit | Browser merged agent's change | Agent shown on the record |\n|---|---|---|---|\n",
    );
    let mut failures = Vec::new();
    for definition in catalogue.iter() {
        let app_id = definition.app_id;
        let route = agent.open(app_id, None).unwrap();
        let (edit, refused) = find_edit(
            &mut Agent {
                agent: &mut agent,
                route: route.clone(),
            },
            ATTEMPTS,
        );
        let local = common::LOCAL_STATE.iter().any(|(id, _)| *id == app_id);
        if edit.is_none() && !local {
            failures.push(format!("{app_id}: no edit: {refused:#?}"));
        }
        let expected = body(agent.app(&route).unwrap());

        // The browser receives the agent's changes in host order and its
        // own mount of the app merges them into the agent's state.
        let deadline = Instant::now() + Duration::from_secs(20);
        let merged = loop {
            agent.pump();
            for event in browser_events.drain() {
                if let PeerEvent::Document(document) = event {
                    assert_eq!(document.author, agent_key, "only the agent edits here");
                    if let Some(
                        collab_types::DocumentMutation::Snapshot(bytes)
                        | collab_types::DocumentMutation::Incremental(bytes),
                    ) = document.mutation()
                    {
                        received
                            .entry(document.route.clone())
                            .or_default()
                            .push(bytes);
                    }
                }
            }
            if let Some(changes) = received.get(&route)
                && page(&catalogue, &route, changes) == expected
            {
                break true;
            }
            if Instant::now() > deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        if !merged {
            failures.push(format!(
                "{app_id}: the browser never converged on the agent's record"
            ));
        }

        // The browser sees the agent on this record: the latest frame after
        // the first that focuses it (presence repeats every second).
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut latest: Option<(Instant, PresenceFrame)> = None;
        let presence: Option<PresenceFrame> = loop {
            if let Some((seen, frame)) = &latest
                && seen.elapsed() > Duration::from_millis(1_500)
            {
                break Some(frame.clone());
            }
            if Instant::now() > deadline {
                break latest.map(|(_, frame)| frame);
            }
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
                && let Some(LiveFrame::Presence(presence)) =
                    open_live(&browser, datagram.payload.to_vec()).await
                && presence
                    .focus
                    .as_ref()
                    .is_some_and(|focus| focus.app == app_id && focus.record == route.record)
            {
                let seen = latest.as_ref().map_or_else(Instant::now, |(seen, _)| *seen);
                latest = Some((seen, presence));
            }
        };
        let cursor = match &presence {
            Some(presence) => {
                let participant = presence.participant.as_ref().unwrap();
                assert_eq!(participant.kind, ParticipantKind::Agent);
                assert_eq!(participant.label, "Coverage agent");
                "yes".to_owned()
            }
            None => {
                failures.push(format!(
                    "{app_id}: the browser never saw the agent on the record"
                ));
                "no".to_owned()
            }
        };
        let label = edit.as_ref().map_or_else(
            || "— (device-local state)".to_owned(),
            |edit| format!("`{}`", edit.label),
        );
        let _ = writeln!(
            table,
            "| {app_id} | {label} | {} | {cursor} |",
            if merged { "yes" } else { "no" }
        );
    }
    eprintln!("{table}");

    // A task in plain words, planned by the model planner.
    let scratchpad = agent
        .known_records()
        .into_iter()
        .chain(received.keys().cloned())
        .find(|route| route.app_id == "so.tilo.scratchpad")
        .unwrap();
    control
        .message(r#"In the scratchpad, edit the note to say "Said in plain words""#)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "the plain-words task never landed: {:?}",
            control.info().tasks
        );
        agent.pump();
        for event in browser_events.drain() {
            if let PeerEvent::Document(document) = event
                && let Some(
                    collab_types::DocumentMutation::Snapshot(bytes)
                    | collab_types::DocumentMutation::Incremental(bytes),
                ) = document.mutation()
            {
                received
                    .entry(document.route.clone())
                    .or_default()
                    .push(bytes);
            }
        }
        let done = control.info().tasks.last().is_some_and(|task| {
            task.state != TaskState::Running && task.state != TaskState::Queued
        });
        if done
            && page(&catalogue, &scratchpad, &received[&scratchpad]).contains("said in plain words")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let task = control.info().tasks.last().cloned().unwrap();
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(task.steps[0].action, "edit");

    // actionsV1 for apps: the browser drives the agent's open records with
    // the remote-action frames it uses for everything else.
    let mut documents = Vec::new();
    let RemoteActionMessage::CatalogueResponse {
        correlation: 7,
        result: Ok(offered),
    } = ask(
        &browser,
        &browser_events,
        &mut agent,
        agent_key,
        RemoteActionMessage::CatalogueRequest {
            correlation: 7,
            query: CatalogueQuery {
                provider: Some(record_provider(&scratchpad)),
                max_results: 200,
                ..CatalogueQuery::default()
            },
        },
        &mut documents,
    )
    .await
    else {
        panic!("no catalogue");
    };
    let edit = offered
        .rows
        .iter()
        .filter_map(tilo_actions::remote::CatalogueRow::target)
        .find(|target| target.address == ActionAddress::new("scratchpad.note", "edit"))
        .expect("the record's declared action is offered");
    assert_eq!(
        offered.rows.len(),
        agent.survey(&scratchpad).len(),
        "the catalogue is the record's whole door, declared and projected"
    );
    let mut text = serde_json::Map::new();
    text.insert("text".into(), "Invoked over actionsV1".into());
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: edit.clone(),
        argument: InvokeArgument::Json(text),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
    };
    let RemoteActionMessage::Receipt(receipt) = ask(
        &browser,
        &browser_events,
        &mut agent,
        agent_key,
        RemoteActionMessage::Invoke(request.clone()),
        &mut documents,
    )
    .await
    else {
        panic!("no receipt");
    };
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{receipt:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "the actionsV1 edit never reached the browser"
        );
        agent.pump();
        for event in browser_events.drain() {
            if let PeerEvent::Document(document) = event {
                documents.push(*document);
            }
        }
        for document in std::mem::take(&mut documents) {
            if let Some(
                collab_types::DocumentMutation::Snapshot(bytes)
                | collab_types::DocumentMutation::Incremental(bytes),
            ) = document.mutation()
            {
                received
                    .entry(document.route.clone())
                    .or_default()
                    .push(bytes);
            }
        }
        if page(&catalogue, &scratchpad, &received[&scratchpad]).contains("Invoked over actionsV1")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // A retry reports the original receipt; another request under the same
    // id is a conflict.
    let RemoteActionMessage::Receipt(again) = ask(
        &browser,
        &browser_events,
        &mut agent,
        agent_key,
        RemoteActionMessage::Invoke(request.clone()),
        &mut documents,
    )
    .await
    else {
        panic!("no receipt");
    };
    assert_eq!(again, receipt);
    let RemoteActionMessage::Receipt(conflict) = ask(
        &browser,
        &browser_events,
        &mut agent,
        agent_key,
        RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            argument: InvokeArgument::None,
            ..request
        }),
        &mut documents,
    )
    .await
    else {
        panic!("no receipt");
    };
    assert_eq!(
        conflict.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::InvocationConflict
        }
    );
    // The room's provider opens an app on a new record.
    let RemoteActionMessage::Receipt(opened) = ask(
        &browser,
        &browser_events,
        &mut agent,
        agent_key,
        RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: application_open_target(ROOM, "so.tilo.notes"),
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
        }),
        &mut documents,
    )
    .await
    else {
        panic!("no receipt");
    };
    let ReceiptStatus::Completed { output } = &opened.status else {
        panic!("{opened:?}");
    };
    assert!(
        output.value.as_ref().unwrap()["provider"]
            .as_str()
            .unwrap()
            .starts_with("record:so.tilo.notes:")
    );
    let RemoteActionMessage::Receipt(unknown) = ask(
        &browser,
        &browser_events,
        &mut agent,
        agent_key,
        RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: "record:so.tilo.notes:nope".into(),
                address: edit.address,
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
        }),
        &mut documents,
    )
    .await
    else {
        panic!("no receipt");
    };
    assert_eq!(
        unknown.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::UnknownAction
        }
    );

    assert!(
        failures.is_empty(),
        "session coverage:\n{}",
        failures.join("\n")
    );
    let _ = user;
    drop(live);
    agent.close();
    browser.close();
    let retained = host
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
    host.relay.shutdown().await;
}

const CANVAS: &str = "so.tilo.canvas";
const WRITER: &str = "so.tilo.word-processor";
const SHEETS: &str = "so.tilo.sheets";

/// Content a step produces is reported with the step and in the task's
/// reply, and is what a later step that carries content is given: Canvas
/// opens a picture the room holds by the address the task names, exports
/// the artwork as content the room keeps, and a step on another record
/// (another worker) that takes content right after a step that produced
/// none is refused before it runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_reports_the_content_its_steps_produced() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let agent_device = enrol(
        &root,
        "Agent: Carrier",
        CertificationPolicy::agent().ceiling,
    );
    let now = tilo_clock::unix_now_ns();
    let delegation = AgentDelegation::sign(
        &root,
        agent_device.cert.device_public_key,
        "carrier",
        "Carrier",
        [0x0f, 0x4c, 0x5c],
        AgentScope {
            rooms: vec![ROOM.into()],
            apps: vec![CANVAS.into(), WRITER.into()],
            may_confirm: false,
        },
        now,
        now + 3_600_000_000_000,
    )
    .unwrap();
    host.relay.register_agent(delegation.clone()).unwrap();

    // The room holds a picture; the task names it only by its address.
    let store = tilo_content::ContentStore::memory();
    let picture = ui::png::encode_rgba8(
        2,
        2,
        &[
            255, 160, 40, 255, 30, 70, 180, 255, 15, 76, 92, 255, 240, 240, 230, 255,
        ],
    )
    .unwrap();
    let address = tilo_actions::content_resource(&store.put(&picture).unwrap()).unwrap();
    let control = AgentControl::new(delegation.clone());
    let mut agent = AgentParticipant::start(
        ParticipantConfig {
            host: host.iroh(),
            host_cert: host_device.cert.clone(),
            room: ROOM.into(),
            content: store.clone(),
        },
        AgentIdentity {
            key: agent_device.key.clone(),
            cert: agent_device.cert.clone(),
            delegation,
        },
        control.clone(),
        catalogue(),
        Box::new(DirectPlanner),
    )
    .unwrap();
    wait_for("the agent to be admitted", || {
        agent.pump();
        let waiting = control.status() == AgentStatus::Waiting;
        async move { waiting.then_some(()) }
    })
    .await;

    let canvas = agent.open(CANVAS, None).unwrap();
    let writer = agent.open(WRITER, None).unwrap();
    let canvas_operations = agent.app(&canvas).unwrap().survey_with_effects();
    let named = |action: &str, label: Option<&str>| {
        canvas_operations
            .iter()
            .find(|descriptor| {
                descriptor.address.action == action
                    && label.is_none_or(|label| descriptor.action_label == label)
            })
            .unwrap_or_else(|| panic!("the artwork publishes {action}"))
            .address
            .clone()
    };
    let open = named("open", Some("Open picture"));
    let export = named("export", None);
    let step = |route: &DocumentRoute, address: &ActionAddress, arguments: serde_json::Value| {
        serde_json::json!({
            "app": route.app_id,
            "record": route.record,
            "resource": address.resource,
            "action": address.action,
            "arguments": arguments,
        })
    };
    let ended = |id: &str| {
        control
            .info()
            .tasks
            .into_iter()
            .find(|task| task.id == id)
            .filter(|task| !matches!(task.state, TaskState::Queued | TaskState::Running))
    };

    let exporting = control
        .message(
            &serde_json::json!([
                step(&canvas, &open, serde_json::json!({"path": address})),
                step(&canvas, &export, serde_json::json!({})),
            ])
            .to_string(),
        )
        .unwrap();
    agent.run_until(Some(Instant::now() + Duration::from_secs(60)), |_| {
        ended(&exporting).is_some()
    });
    let task = ended(&exporting).expect("the task ended");
    assert_eq!(task.state, TaskState::Completed, "{task:?}");
    let [opened, exported] = task.steps.as_slice() else {
        panic!("two steps: {task:?}");
    };
    assert!(opened.content.is_empty(), "{opened:?}");
    let [produced] = exported.content.as_slice() else {
        panic!("the export names its one picture: {exported:?}");
    };
    let hash = tilo_actions::content_hash(produced).expect("a content address");
    let png = store.read(hash).expect("the room keeps the export");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let reply = control.info().conversation.last().unwrap().text.clone();
    assert_eq!(reply, format!("Done: 2 actions. Produced {produced}."));

    // A step that takes content, on another record, in a task whose steps
    // before it produced none: refused before it runs, naming why.
    let mut place = step(
        &writer,
        &ActionAddress::new("tilo.rich-text", "place-image"),
        serde_json::json!({}),
    );
    place["carries"] = true.into();
    let refused = control
        .message(
            &serde_json::json!([
                step(&canvas, &open, serde_json::json!({"path": address})),
                place,
            ])
            .to_string(),
        )
        .unwrap();
    agent.run_until(Some(Instant::now() + Duration::from_secs(60)), |_| {
        ended(&refused).is_some()
    });
    let task = ended(&refused).expect("the task ended");
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    let error = task.error.unwrap_or_default();
    assert!(
        error.contains("place-image takes what the step before it produced")
            && error.contains("no preceding action result"),
        "{error}"
    );

    // What the export produced, carried to the Writer record (another
    // worker) by the record's own document operation: no view, no caret,
    // and the record then shows the very picture the export kept.
    let mut insert = step(
        &writer,
        &ActionAddress::new("tilo.rich-text", "insert-image"),
        serde_json::json!({}),
    );
    insert["carries"] = true.into();
    let inserting = control
        .message(
            &serde_json::json!([
                step(&canvas, &open, serde_json::json!({"path": address})),
                step(&canvas, &export, serde_json::json!({})),
                insert,
            ])
            .to_string(),
        )
        .unwrap();
    agent.run_until(Some(Instant::now() + Duration::from_secs(60)), |_| {
        ended(&inserting).is_some()
    });
    let task = ended(&inserting).expect("the task ended");
    assert_eq!(task.state, TaskState::Completed, "{task:?}");
    let [_, exported, _] = task.steps.as_slice() else {
        panic!("three steps: {task:?}");
    };
    let [produced] = exported.content.as_slice() else {
        panic!("the export names its one picture: {exported:?}");
    };
    let hash = tilo_actions::content_hash(produced).expect("a content address");
    let (images, shown) = agent
        .app(&writer)
        .unwrap()
        .read(|doc| {
            let state =
                <tilo_document::RichText as ui::tilo_resource::Resource>::read(doc).unwrap();
            (
                state.featured("image").len(),
                state.content().contains(hash),
            )
        })
        .unwrap();
    assert_eq!(images, 1, "one picture is inserted");
    assert!(shown, "the record names the export's content {hash}");

    agent.close();
    host.relay.shutdown().await;
}

/// The rest of [`a_task_is_planned_again_with_what_a_view_answered`]'s task.
const BAND: &str = "add a filled rectangle over the right 38% of the picture";

/// Runs the written-out steps, then plans [`BAND`] again from what the
/// steps answered: the artwork's size, as its view found it.
struct Measuring {
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl tilo_agent_participant::Planner for Measuring {
    fn plan(
        &mut self,
        task: &str,
        context: &tilo_agent_participant::PlanContext,
    ) -> Result<Vec<tilo_agent_participant::Step>, String> {
        use tilo_agent_participant::Step;
        if task != BAND {
            let mut steps = DirectPlanner.plan(task, context)?;
            steps.push(Step {
                app: CANVAS.into(),
                record: None,
                action: StepAction::Continue(BAND.into()),
                carries: false,
            });
            return Ok(steps);
        }
        let inspected = context
            .observed
            .iter()
            .rfind(|seen| seen.contains("#inspect answered "))
            .ok_or("the plan was shown no inspection")?;
        self.seen.lock().unwrap().push(inspected.clone());
        let answer: serde_json::Value = serde_json::from_str(
            inspected
                .find('{')
                .and_then(|start| inspected.get(start..))
                .ok_or("no answer")?,
        )
        .map_err(|error| error.to_string())?;
        let size = |side: &str| {
            answer["artwork"][side]
                .as_f64()
                .ok_or_else(|| format!("no {side}"))
        };
        let (width, height) = (size("width")?, size("height")?);
        let (route, descriptors) = context
            .open
            .iter()
            .find(|(route, _)| route.app_id == CANVAS)
            .ok_or("the artwork is not open")?;
        let draw = descriptors
            .iter()
            .find(|descriptor| descriptor.address.action == "draw")
            .ok_or("the artwork draws no shapes")?;
        let arguments = serde_json::json!({"marks": [{
            "id": "band",
            "shape": {"kind": "rect", "x": width * 0.62, "y": 0, "width": width * 0.38,
                      "height": height},
            "fill": "#000000",
        }]});
        Ok(vec![Step {
            app: CANVAS.into(),
            record: Some(route.record.clone()),
            action: StepAction::Invoke {
                address: draw.address.clone(),
                arguments: arguments.as_object().unwrap().clone(),
            },
            carries: false,
        }])
    }
}

/// An agent learns what an edit needs through the record's views: the
/// canvas's `inspect` is offered with its operations, runs in the record's
/// mounted component, and what it answers — the artwork's size — is what
/// the rest of the task is planned again with, so a band over the right
/// 38% of the picture lands where the picture's size puts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_is_planned_again_with_what_a_view_answered() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let agent_device = enrol(
        &root,
        "Agent: Painter",
        CertificationPolicy::agent().ceiling,
    );
    let now = tilo_clock::unix_now_ns();
    let delegation = AgentDelegation::sign(
        &root,
        agent_device.cert.device_public_key,
        "painter",
        "Painter",
        [0x0f, 0x4c, 0x5c],
        AgentScope {
            rooms: vec![ROOM.into()],
            apps: vec![CANVAS.into()],
            may_confirm: false,
        },
        now,
        now + 3_600_000_000_000,
    )
    .unwrap();
    host.relay.register_agent(delegation.clone()).unwrap();

    let store = tilo_content::ContentStore::memory();
    let picture = ui::png::encode_rgba8(100, 50, &vec![0x80; 100 * 50 * 4]).unwrap();
    let address = tilo_actions::content_resource(&store.put(&picture).unwrap()).unwrap();
    let control = AgentControl::new(delegation.clone());
    let seen = std::sync::Arc::default();
    let mut agent = AgentParticipant::start(
        ParticipantConfig {
            host: host.iroh(),
            host_cert: host_device.cert.clone(),
            room: ROOM.into(),
            content: store.clone(),
        },
        AgentIdentity {
            key: agent_device.key.clone(),
            cert: agent_device.cert.clone(),
            delegation,
        },
        control.clone(),
        catalogue(),
        Box::new(Measuring {
            seen: std::sync::Arc::clone(&seen),
        }),
    )
    .unwrap();
    wait_for("the agent to be admitted", || {
        agent.pump();
        let waiting = control.status() == AgentStatus::Waiting;
        async move { waiting.then_some(()) }
    })
    .await;

    let canvas = agent.open(CANVAS, None).unwrap();
    let operations = agent.app(&canvas).unwrap().survey_with_effects();
    let named = |action: &str, label: Option<&str>| {
        operations
            .iter()
            .find(|descriptor| {
                descriptor.address.action == action
                    && label.is_none_or(|label| descriptor.action_label == label)
            })
            .unwrap_or_else(|| panic!("the artwork offers {action}"))
            .clone()
    };
    let open = named("open", Some("Open picture")).address;
    let inspect = named("inspect", None);
    assert_eq!(inspect.kind, tilo_actions::OperationKind::View);
    let step = |address: &ActionAddress, arguments: serde_json::Value| {
        serde_json::json!({
            "app": canvas.app_id,
            "record": canvas.record,
            "resource": address.resource,
            "action": address.action,
            "arguments": arguments,
        })
    };
    let task = control
        .message(
            &serde_json::json!([
                step(&open, serde_json::json!({"path": address})),
                step(&inspect.address, serde_json::json!({})),
            ])
            .to_string(),
        )
        .unwrap();
    let ended = || {
        control
            .info()
            .tasks
            .into_iter()
            .find(|info| info.id == task)
            .filter(|info| !matches!(info.state, TaskState::Queued | TaskState::Running))
    };
    agent.run_until(Some(Instant::now() + Duration::from_secs(60)), |_| {
        ended().is_some()
    });
    let info = ended().expect("the task ended");
    assert_eq!(info.state, TaskState::Completed, "{info:?}");
    let seen = seen.lock().unwrap().clone();
    let [inspected] = seen.as_slice() else {
        panic!("the rest was planned once, with the inspection: {seen:?}");
    };
    assert!(
        inspected.contains(r#""width":100"#) && inspected.contains(r#""height":50"#),
        "{inspected}"
    );

    // The band covers the right 38% of the 100 x 50 picture.
    let output = agent
        .invoke(&canvas, &inspect.address, &serde_json::Map::new())
        .unwrap()
        .unwrap();
    let marks = output.value.unwrap()["marks_bottom_first"].clone();
    let band = marks
        .as_array()
        .unwrap()
        .iter()
        .find(|mark| mark["id"] == "band")
        .unwrap_or_else(|| panic!("the band was drawn: {marks}"))
        .clone();
    let bounds: Vec<f64> = band["bounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|side| side.as_f64().unwrap())
        .collect();
    assert_eq!(bounds, [62.0, 0.0, 38.0, 50.0], "{band}");

    agent.close();
    host.relay.shutdown().await;
}

/// The title Agent A gives its Writer document, and what Agent B adds.
const LAUNCH_PAGE: &str = "Launch page";
const DOORS: &str = "Doors open at nine";

/// Agent B's model, scripted, through `apps/agent`'s discovery tools: it
/// finds the room's document by its title in `list_applications`, opens it
/// by its record, and once its operations are listed appends a paragraph.
struct OpensByTitle {
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl tilo_agent_participant::PlanModel for OpensByTitle {
    fn choose(
        &mut self,
        _task: &str,
        catalogue: &tilo_agent::surface::Catalogue,
        most_steps: Option<usize>,
    ) -> Result<tilo_agent::tools::Answer, String> {
        use tilo_agent::tools;
        let mut revealed = tools::Revealed::new();
        if let Some(append) = catalogue
            .offers
            .iter()
            .find(|offer| offer.address().action == "append-paragraph")
        {
            self.seen.lock().unwrap().push("append-paragraph".into());
            tools::answer(
                tools::LIST_OPERATIONS,
                &serde_json::json!({ "application": WRITER }),
                catalogue,
                &mut revealed,
            );
            return tools::read_plan(
                &serde_json::json!({
                    "steps": [{"operation": append.id(), "arguments": {"text": DOORS}}],
                    "why": "the launch page is open",
                }),
                &revealed,
                most_steps,
            );
        }
        let listing = tools::answer(
            tools::LIST_APPLICATIONS,
            &serde_json::json!({}),
            catalogue,
            &mut revealed,
        );
        self.seen.lock().unwrap().push(listing.clone());
        let open = catalogue
            .offers
            .iter()
            .find(|offer| {
                tilo_agent::surface::opens_record(offer.address()).is_some()
                    && offer
                        .descriptor
                        .action_label
                        .contains(&format!("\"{LAUNCH_PAGE}\""))
            })
            .ok_or_else(|| format!("no offer opens the launch page: {listing}"))?;
        if !listing.contains(&format!("\"{LAUNCH_PAGE}\" → `{}`", open.id())) {
            return Err(format!(
                "the listing does not name the launch page: {listing}"
            ));
        }
        tools::read_plan(
            &serde_json::json!({
                "steps": [{"operation": open.id(), "arguments": {}}],
                "then": format!("append a paragraph saying \"{DOORS}\""),
                "why": "the launch page is the room's document",
            }),
            &revealed,
            most_steps,
        )
    }
}

/// An agent asked to edit a document another agent made finds it by its
/// title: Agent A writes a Writer document headed "Launch page"; Agent B,
/// asked about it by that title, opens that record (not a new one) before
/// its model is asked, so the model is shown the record's own operations
/// and plans the paragraph in one turn, and the paragraph lands in A's
/// document.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_opens_another_agents_document_by_its_title() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let now = tilo_clock::unix_now_ns();
    let start = |id: &str, name: &str, planner: Box<dyn tilo_agent_participant::Planner>| {
        let device = enrol(
            &root,
            &format!("Agent: {name}"),
            CertificationPolicy::agent().ceiling,
        );
        let delegation = AgentDelegation::sign(
            &root,
            device.cert.device_public_key,
            id,
            name,
            [0x0f, 0x4c, 0x5c],
            AgentScope {
                rooms: vec![ROOM.into()],
                apps: vec![WRITER.into()],
                may_confirm: false,
            },
            now,
            now + 3_600_000_000_000,
        )
        .unwrap();
        host.relay.register_agent(delegation.clone()).unwrap();
        let control = AgentControl::new(delegation.clone());
        let agent = AgentParticipant::start(
            ParticipantConfig {
                host: host.iroh(),
                host_cert: host_device.cert.clone(),
                room: ROOM.into(),
                content: tilo_content::ContentStore::memory(),
            },
            AgentIdentity {
                key: device.key.clone(),
                cert: device.cert,
                delegation,
            },
            control.clone(),
            catalogue(),
            planner,
        )
        .unwrap();
        (agent, control)
    };
    let (mut author, author_control) = start("author", "Author", Box::new(DirectPlanner));
    let seen = std::sync::Arc::default();
    let (mut editor, editor_control) = start(
        "editor",
        "Editor",
        Box::new(ModelPlanner::new(Box::new(OpensByTitle {
            seen: std::sync::Arc::clone(&seen),
        }))),
    );
    wait_for("both agents to be admitted", || {
        author.pump();
        editor.pump();
        let waiting = author_control.status() == AgentStatus::Waiting
            && editor_control.status() == AgentStatus::Waiting;
        async move { waiting.then_some(()) }
    })
    .await;

    // Agent A's document, headed "Launch page".
    let page = author.open(WRITER, None).unwrap();
    let rich_text = |action: &str| ActionAddress::new("tilo.rich-text", action);
    let arguments = |value: serde_json::Value| value.as_object().unwrap().clone();
    author
        .invoke(
            &page,
            &rich_text("append-paragraph"),
            &arguments(serde_json::json!({"text": LAUNCH_PAGE})),
        )
        .unwrap();
    author.pump();
    // The paragraph appended last is the body's last.
    let block = author
        .app(&page)
        .unwrap()
        .read(|doc| {
            let text = tilo_document::RichText::plain_text_under(doc, &automerge::ROOT).unwrap();
            assert!(text.contains(LAUNCH_PAGE), "{text}");
            let state =
                <tilo_document::RichText as ui::tilo_resource::Resource>::read(doc).unwrap();
            state.body().all().last().map(str::to_owned)
        })
        .unwrap()
        .expect("the paragraph was appended");
    author
        .invoke(
            &page,
            &rich_text("set-style"),
            &arguments(serde_json::json!({"block": block, "kind": "heading", "level": 1})),
        )
        .unwrap();

    // Agent B sees the record, and is asked about it by its title only.
    wait_for("the editor to see the launch page", || {
        author.pump();
        editor.pump();
        let seen = editor.known_records().contains(&page);
        async move { seen.then_some(()) }
    })
    .await;
    let task = editor_control
        .message(&format!(
            "Add a paragraph saying \"{DOORS}\" to the {LAUNCH_PAGE}"
        ))
        .unwrap();
    let ended = || {
        editor_control
            .info()
            .tasks
            .into_iter()
            .find(|info| info.id == task)
            .filter(|info| !matches!(info.state, TaskState::Queued | TaskState::Running))
    };
    editor.run_until(Some(Instant::now() + Duration::from_secs(60)), |_| {
        ended().is_some()
    });
    let info = ended().expect("the task ended");
    assert_eq!(info.state, TaskState::Completed, "{info:?}");
    let actions: Vec<(&str, &str)> = info
        .steps
        .iter()
        .map(|step| (step.resource.as_str(), step.action.as_str()))
        .collect();
    assert_eq!(
        actions,
        [("tilo.rich-text", "append-paragraph")],
        "the record named by its title was open before the plan: {actions:?}"
    );
    assert!(editor.app(&page).is_some(), "the editor holds A's record");
    let asked = seen.lock().unwrap().clone();
    assert_eq!(
        asked,
        ["append-paragraph"],
        "one model turn, shown the record's operations"
    );

    // The paragraph lands in Agent A's own document.
    let landed = |author: &AgentParticipant| {
        author.app(&page).unwrap().read(|doc| {
            tilo_document::RichText::plain_text_under(doc, &automerge::ROOT)
                .is_ok_and(|text| text.contains(LAUNCH_PAGE) && text.contains(DOORS))
        }) == Some(true)
    };
    author.run_until(Some(Instant::now() + Duration::from_secs(30)), |author| {
        landed(author)
    });
    assert!(landed(&author), "B's paragraph reached A's record");

    author.close();
    editor.close();
    host.relay.shutdown().await;
}

/// A model behind a local chat-completions endpoint that answers
/// `answers` in order, reporting every request's body.
fn model_endpoint(answers: Vec<serde_json::Value>) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for answer in answers {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            sent.send(String::from_utf8(body).unwrap()).unwrap();
            let answer = answer.to_string();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{answer}",
                answer.len()
            );
            reader.into_inner().write_all(reply.as_bytes()).unwrap();
        }
    });
    (url, received)
}

/// A chat-completions turn calling `name` with `input`.
fn tool_call(id: &str, name: &str, input: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
        "tool_calls": [{"id": id, "type": "function", "function": {"name": name,
            "arguments": input.to_string()}}]}, "finish_reason": "tool_calls"}]})
}

/// An agent granted `apps`, planning with a model at `url`, admitted to a
/// room on `host`, with `store` as its content.
async fn model_agent(
    root: &ed25519_dalek::SigningKey,
    host: &support::TestHost,
    host_device: &support::Device,
    apps: &[&str],
    url: &str,
    store: tilo_content::ContentStore,
) -> (AgentParticipant, std::sync::Arc<AgentControl>) {
    let agent_device = enrol(root, "Agent: Planner", CertificationPolicy::agent().ceiling);
    let now = tilo_clock::unix_now_ns();
    let delegation = AgentDelegation::sign(
        root,
        agent_device.cert.device_public_key,
        "planner",
        "Planner",
        [0x0f, 0x4c, 0x5c],
        AgentScope {
            rooms: vec![ROOM.into()],
            apps: apps.iter().map(|app| (*app).to_owned()).collect(),
            may_confirm: false,
        },
        now,
        now + 3_600_000_000_000,
    )
    .unwrap();
    host.relay.register_agent(delegation.clone()).unwrap();
    let control = AgentControl::new(delegation.clone());
    let model = tilo_agent_participant::OpenAiModel::new(
        url,
        "canned",
        tilo_agent_participant::ApiKey::None,
        Duration::from_secs(30),
        tilo_model_gateway::openai::Target::Standard,
    );
    let mut agent = AgentParticipant::start(
        ParticipantConfig {
            host: host.iroh(),
            host_cert: host_device.cert.clone(),
            room: ROOM.into(),
            content: store,
        },
        AgentIdentity {
            key: agent_device.key.clone(),
            cert: agent_device.cert.clone(),
            delegation,
        },
        control.clone(),
        catalogue(),
        Box::new(ModelPlanner::new(Box::new(model))),
    )
    .unwrap();
    wait_for("the agent to be admitted", || {
        agent.pump();
        let waiting = control.status() == AgentStatus::Waiting;
        async move { waiting.then_some(()) }
    })
    .await;
    (agent, control)
}

/// Run `text` as a task of `agent` to its end.
fn run_task(
    agent: &mut AgentParticipant,
    control: &AgentControl,
    text: &str,
) -> tilo_relay_protocol::agents::AgentTask {
    let task = control.message(text).unwrap();
    let ended = || {
        control
            .info()
            .tasks
            .into_iter()
            .find(|info| info.id == task)
            .filter(|info| !matches!(info.state, TaskState::Queued | TaskState::Running))
    };
    agent.run_until(Some(Instant::now() + Duration::from_secs(60)), |_| {
        ended().is_some()
    });
    ended().expect("the task ended")
}

/// The offline measure of model calls per task: a one-app task over an app
/// with nothing open is planned in ONE model request — the first turn
/// shows what a new record of the app publishes, with its parameters in
/// full, and the step naming one opens that record itself — where it took
/// an open, a re-plan and a describe before.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_one_app_task_is_planned_in_one_model_request() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let set_cells = "sheets.sheet:00000000-0000-4000-8000-000000000001#set-cells";
    let (url, sent) = model_endpoint(vec![tool_call(
        "p1",
        "plan",
        &serde_json::json!({"steps": [{"operation": set_cells,
            "arguments": {"start": "A1", "rows": [["Visits", "Signups"], [240, 18]]}}],
            "why": "one sheet", "then": null}),
    )]);
    let (mut agent, control) = model_agent(
        &root,
        &host,
        &host_device,
        &[SHEETS],
        &url,
        tilo_content::ContentStore::memory(),
    )
    .await;

    let info = run_task(
        &mut agent,
        &control,
        "In Sheets, enter Visits and Signups in the first row and 240 and 18 under them",
    );
    assert_eq!(info.state, TaskState::Completed, "{info:?}");
    let actions: Vec<&str> = info.steps.iter().map(|step| step.action.as_str()).collect();
    assert_eq!(actions, ["set-cells"], "no open, no re-plan: {info:?}");
    let requests: Vec<String> = sent.try_iter().collect();
    assert_eq!(requests.len(), 1, "one model request for the task");
    let first: serde_json::Value = serde_json::from_str(&requests[0]).unwrap();
    let turn = first["messages"][1]["content"].as_str().unwrap();
    assert!(
        turn.contains(set_cells)
            && turn.contains("rows: array (required)")
            && turn.contains("not open yet: planning any of its operations"),
        "{turn}"
    );
    assert!(!turn.contains("app.so.tilo.sheets#open"), "{turn}");

    agent.close();
    host.relay.shutdown().await;
}

/// An agent reads an open record in the planning conversation: the first
/// turn shows what the artwork holds (its size, read before planning) and
/// `draw` in full; the model asks `read_view` for `inspect` and is answered
/// in the same conversation, then plans the band — two model requests and
/// no re-plan — and the band lands over the right 38% of the picture.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_view_is_read_in_the_planning_conversation() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let store = tilo_content::ContentStore::memory();
    let picture = ui::png::encode_rgba8(100, 50, &vec![0x80; 100 * 50 * 4]).unwrap();
    let address = tilo_actions::content_resource(&store.put(&picture).unwrap()).unwrap();
    let (url, sent) = model_endpoint(vec![
        tool_call(
            "r1",
            tilo_agent_participant::READ_VIEW,
            &serde_json::json!({"operation": "canvas.artwork#inspect", "arguments": {}}),
        ),
        tool_call(
            "p1",
            "plan",
            &serde_json::json!({"steps": [{"operation": "canvas.artwork#draw",
                "arguments": {"marks": [{"id": "band", "fill": "#0F4C5C",
                    "shape": {"kind": "rect", "x": 62, "y": 0, "width": 38, "height": 50}}]}}],
                "why": "measured", "then": null}),
        ),
    ]);
    let (mut agent, control) =
        model_agent(&root, &host, &host_device, &[CANVAS], &url, store.clone()).await;

    // The picture is opened by written-out steps (no model).
    let canvas = agent.open(CANVAS, None).unwrap();
    let open = agent
        .app(&canvas)
        .unwrap()
        .survey_with_effects()
        .into_iter()
        .find(|descriptor| {
            descriptor.address.action == "open" && descriptor.action_label == "Open picture"
        })
        .unwrap()
        .address;
    let opened = run_task(
        &mut agent,
        &control,
        &serde_json::json!({"app": CANVAS, "record": canvas.record, "resource": open.resource,
            "action": open.action, "arguments": {"path": address}})
        .to_string(),
    );
    assert_eq!(opened.state, TaskState::Completed, "{opened:?}");

    let info = run_task(
        &mut agent,
        &control,
        "In Canvas, add a filled rectangle over the right 38% of the picture",
    );
    assert_eq!(info.state, TaskState::Completed, "{info:?}");
    let actions: Vec<&str> = info.steps.iter().map(|step| step.action.as_str()).collect();
    assert_eq!(
        actions,
        ["draw"],
        "the read is no step of its own: {info:?}"
    );
    let requests: Vec<serde_json::Value> = sent
        .try_iter()
        .map(|body| serde_json::from_str(&body).unwrap())
        .collect();
    assert_eq!(requests.len(), 2, "a read and a plan, no re-plan");
    let turn = requests[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        turn.contains("marks: array (required)")
            && turn.contains(&format!("(document {})", canvas.record))
            && turn.contains("canvas.artwork#inspect answered"),
        "the first turn shows draw in full and what the artwork holds: {turn}"
    );
    let read = requests[1]["messages"][3]["content"].as_str().unwrap();
    assert!(
        read.contains("canvas.artwork#inspect answered")
            && read.contains(r#""width":100"#)
            && read.contains(r#""height":50"#),
        "the read answered in the conversation: {read}"
    );

    let inspect = agent
        .app(&canvas)
        .unwrap()
        .survey_with_effects()
        .into_iter()
        .find(|descriptor| descriptor.address.action == "inspect")
        .unwrap()
        .address;
    let output = agent
        .invoke(&canvas, &inspect, &serde_json::Map::new())
        .unwrap()
        .unwrap();
    let marks = output.value.unwrap()["marks_bottom_first"].clone();
    let band = marks
        .as_array()
        .unwrap()
        .iter()
        .find(|mark| mark["id"] == "band")
        .unwrap_or_else(|| panic!("the band was drawn: {marks}"))
        .clone();
    let bounds: Vec<f64> = band["bounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|side| side.as_f64().unwrap())
        .collect();
    assert_eq!(bounds, [62.0, 0.0, 38.0, 50.0], "{band}");

    agent.close();
    host.relay.shutdown().await;
}
