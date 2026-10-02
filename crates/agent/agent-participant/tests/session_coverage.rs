//! Every web app over the real session: an agent (iroh, its own certified
//! device and delegation) opens the app and makes a user-equivalent edit
//! found without app knowledge; a natural-language task goes through
//! `ModelPlanner`; a browser drives the agent's records over remote actions.
//! The room has no channel keys, so none of the agent's edits leaves it in
//! the clear: the host retains no document and the browser receives none.

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
#[path = "../../../browser/tilo-browser-peer/tests/support/mod.rs"]
mod support;

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
    AgentControl, AgentIdentity, AgentParticipant, Control, HostedApp, ModelPlanner,
    ParticipantConfig, StepAction, record_provider,
};
use tilo_browser_peer::{DocumentRoute, Host, PeerEvent};
use tilo_relay_protocol::agents::{AgentDelegation, AgentScope, AgentStatus, TaskState};
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
    fn controls(&mut self) -> Vec<Control> {
        self.agent.controls(&self.route)
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
            StepAction::Operate { control, input } => {
                self.agent.operate(&self.route, control, input)
            }
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

fn body(app: &HostedApp) -> String {
    app.read(|doc| serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap())
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_web_app_over_the_session() {
    common::isolate_device();
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

    let mut table = String::from("| App | Edit | Via |\n|---|---|---|\n");
    let mut failures = Vec::new();
    let mut opened = Vec::new();
    for definition in catalogue.iter() {
        let app_id = definition.app_id;
        let route = agent.open(app_id, None).unwrap();
        opened.push(route.clone());
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
        let (label, via) = edit.as_ref().map_or_else(
            || ("— (device-local state)".to_owned(), "—"),
            |edit| (format!("`{}`", edit.label), edit.path),
        );
        let _ = writeln!(table, "| {app_id} | {label} | {via} |");
    }
    eprintln!("{table}");

    // A task in plain words, planned by the model planner, lands in the
    // agent's own copy of the record.
    let scratchpad = opened
        .into_iter()
        .find(|route| route.app_id == "so.tilo.scratchpad")
        .unwrap();
    control
        .message(r#"In the scratchpad, edit the note to say "Said in plain words""#)
        .unwrap();
    wait_for("the plain-words task to finish", || {
        agent.pump();
        let done = control.info().tasks.last().is_some_and(|task| {
            task.state != TaskState::Running && task.state != TaskState::Queued
        });
        async move { done.then_some(()) }
    })
    .await;
    assert!(
        body(agent.app(&scratchpad).unwrap())
            .to_lowercase()
            .contains("said in plain words")
    );
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
        invocation: InvocationId::new(),
        target: edit.clone(),
        argument: InvokeArgument::Json(text),
        context: None,
        confirmation: None,
        revision: None,
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
    assert!(
        body(agent.app(&scratchpad).unwrap()).contains("Invoked over actionsV1"),
        "the invoked edit is in the agent's copy"
    );
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
            invocation: InvocationId::new(),
            target: application_open_target(ROOM, "so.tilo.notes"),
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
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
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: "record:so.tilo.notes:nope".into(),
                address: edit.address,
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
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

    // The room has no channel keys: none of the agent's edits reached the
    // host or the browser in the clear.
    let retained = host
        .relay
        .room_records()
        .retained(workspace())
        .await
        .unwrap();
    assert_eq!(
        common::retained_documents(retained.into_iter().map(|kept| (kept.author, kept.body))),
        (0, 0),
        "the host retains no document"
    );
    agent.pump();
    documents.extend(
        browser_events
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                PeerEvent::Document(document) => Some(*document),
                _ => None,
            }),
    );
    assert!(documents.is_empty(), "no document reaches the browser");

    assert!(
        failures.is_empty(),
        "session coverage:\n{}",
        failures.join("\n")
    );
    let _ = user;
    agent.close();
    browser.close();
    host.relay.shutdown().await;
}
