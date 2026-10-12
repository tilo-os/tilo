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
use tilo_actions::door::ActionDescriptor;
use tilo_actions::remote::{
    ActionHostError, CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, RemoteActionTarget, application_open_target,
};
use tilo_agent_participant::{
    AgentControl, AgentIdentity, AgentParticipant, ModelPlanner, ParticipantConfig, Replica, Seed,
    StepAction, record_provider,
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

    // A task in plain words, planned by the model planner: a verb with a
    // text parameter the planner fills from the words.
    let record_of = |app: &str| {
        agent
            .known_records()
            .into_iter()
            .chain(received.keys().cloned())
            .find(|route| route.app_id == app)
            .unwrap()
    };
    let scratchpad = record_of("so.tilo.scratchpad");
    let notes = record_of("so.tilo.notes");
    control
        .message(r#"In the notes, add a new note titled "Said in plain words""#)
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
            && received.get(&notes).is_some_and(|changes| {
                page(&catalogue, &notes, changes).contains("said in plain words")
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let task = control.info().tasks.last().cloned().unwrap();
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(task.steps[0].action, "new-note");

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
        .find(|target| target.address == common::scratchpad_body("").0)
        .expect("the record's declared action is offered");
    assert_eq!(
        offered.rows.len(),
        agent.survey(&scratchpad).len(),
        "the catalogue is the record's whole door, declared and projected"
    );
    let text = common::scratchpad_body("Invoked over actionsV1").1;
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
