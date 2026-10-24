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
use tilo_relay_protocol::agents::{AgentStatus, TaskState};
use tilo_relay_protocol::delegation::{DelegateScope, Delegation};
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
            StepAction::Open | StepAction::Continue(_) | StepAction::Wait(_) => Ok(()),
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
    let deadline = tilo_test_env::hang_deadline();
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
/// Record a coverage failure, and say it at once: a run the harness ends
/// for time still names every app that failed before it.
fn note(failures: &mut Vec<String>, failure: String) {
    eprintln!("session coverage: {failure}");
    failures.push(failure);
}

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
        tilo_agent_participant::ReplicaActor::Owner,
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
    let state = support::scratch_dir();
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
    let delegation = Delegation::sign(
        &root,
        agent_device.cert.device_public_key,
        tilo_relay_protocol::delegation::Delegate::Agent {
            agent_id: "coverage".into(),
        },
        "Coverage agent",
        [0x7c, 0x3a, 0xed],
        DelegateScope {
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
    let mut agent_presence = tilo_moq::live::peer::MemberPresence::follow(&live, &agent_namespace);

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
            note(&mut failures, format!("{app_id}: no edit: {refused:#?}"));
        }
        let expected = body(agent.app(&route).unwrap());

        // The browser receives the agent's changes in host order and its
        // own mount of the app merges them into the agent's state.
        let deadline = tilo_test_env::hang_deadline();
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
            let seen = received
                .get(&route)
                .map(|changes| page(&catalogue, &route, changes));
            note(
                &mut failures,
                format!(
                    "{app_id}: the browser never converged on the agent's record\n  agent: {expected}\n  browser: {seen:?}"
                ),
            );
        }

        // The browser sees the agent on this record: the first presence
        // frame that focuses it. Every frame names the participant, so there
        // is nothing to wait for once one has arrived.
        let deadline = tilo_test_env::hang_deadline();
        let presence: Option<PresenceFrame> = loop {
            if Instant::now() > deadline {
                break None;
            }
            agent.pump();
            if let Some(datagram) = agent_presence.next(Duration::from_millis(200)).await
                && let Some(LiveFrame::Presence(presence)) =
                    open_live(&browser, datagram.to_vec()).await
                && presence
                    .focus
                    .as_ref()
                    .is_some_and(|focus| focus.app == app_id && focus.record == route.record)
            {
                break Some(presence);
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
                note(
                    &mut failures,
                    format!("{app_id}: the browser never saw the agent on the record"),
                );
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
    let deadline = tilo_test_env::hang_deadline();
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
        appearance: None,
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
    let deadline = tilo_test_env::hang_deadline();
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
            appearance: None,
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
            appearance: None,
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
const SHEETS: &str = "so.tilo.sheets";
const VIDEO: &str = "so.tilo.video-editor";

/// An agent granted `apps`, planning with `planner`, admitted to the room
/// on `host`, its records lent `store`.
async fn planning_agent(
    root: &ed25519_dalek::SigningKey,
    host: (&support::TestHost, &support::Device),
    apps: &[&str],
    planner: Box<dyn tilo_agent_participant::Planner>,
    store: tilo_content::ContentStore,
) -> (AgentParticipant, std::sync::Arc<AgentControl>) {
    planning_agent_as("planner", root, host, apps, (planner, store)).await
}

/// [`planning_agent`], its delegation naming it `id`.
async fn planning_agent_as(
    id: &str,
    root: &ed25519_dalek::SigningKey,
    host: (&support::TestHost, &support::Device),
    apps: &[&str],
    planning: (
        Box<dyn tilo_agent_participant::Planner>,
        tilo_content::ContentStore,
    ),
) -> (AgentParticipant, std::sync::Arc<AgentControl>) {
    planning_agent_labelled((id, id), root, host, apps, planning).await
}

/// [`planning_agent`], its delegation naming it `id` and labelling it
/// `label`, the name its person gave it.
async fn planning_agent_labelled(
    (id, label): (&str, &str),
    root: &ed25519_dalek::SigningKey,
    (host, host_device): (&support::TestHost, &support::Device),
    apps: &[&str],
    (planner, store): (
        Box<dyn tilo_agent_participant::Planner>,
        tilo_content::ContentStore,
    ),
) -> (AgentParticipant, std::sync::Arc<AgentControl>) {
    let agent_device = enrol(
        root,
        &format!("Agent: {id}"),
        CertificationPolicy::agent().ceiling,
    );
    let now = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        root,
        agent_device.cert.device_public_key,
        tilo_relay_protocol::delegation::Delegate::Agent {
            agent_id: id.into(),
        },
        label,
        [0x0f, 0x4c, 0x5c],
        DelegateScope {
            rooms: vec![ROOM.into()],
            apps: apps.iter().map(|app| (*app).to_owned()).collect(),
        },
        now,
        now + 3_600_000_000_000,
    )
    .unwrap();
    host.relay.register_agent(delegation.clone()).unwrap();
    let control = AgentControl::new(delegation.clone());
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
        planner,
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

/// A planner at `url`, an OpenAI-compatible endpoint answering canned turns.
fn endpoint_planner(url: &str) -> Box<dyn tilo_agent_participant::Planner> {
    Box::new(ModelPlanner::new(Box::new(
        tilo_agent_participant::OpenAiModel::new(
            url,
            "canned",
            tilo_agent_participant::ApiKey::None,
            tilo_test_env::hang_bound(),
            tilo_model_gateway::openai::Target::Standard,
        ),
    )))
}

/// An endpoint answering `answers` in order, one connection each, and
/// reporting every request's body.
fn model_endpoint(answers: Vec<serde_json::Value>) -> (String, std::sync::mpsc::Receiver<String>) {
    gated_model_endpoint(answers.into_iter().map(|answer| (answer, None)).collect())
}

/// [`model_endpoint`], an answer with a gate held back until the gate is
/// opened (or dropped): a request in flight while the room changes.
fn gated_model_endpoint(
    answers: Vec<(serde_json::Value, Option<std::sync::mpsc::Receiver<()>>)>,
) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for (answer, gate) in answers {
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
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
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

/// The text of a chat-completions message, written as a string or as text
/// parts.
fn message_text(message: &serde_json::Value) -> String {
    match &message["content"] {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect(),
        _ => String::new(),
    }
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
/// the room has no record of is planned in ONE model request — the record
/// is made before the plan, and the first turn shows what it publishes with
/// its parameters in full.
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
    let set_cells = "sheets.workbook#set-cells";
    let (url, sent) = model_endpoint(vec![tool_call(
        "p1",
        "plan",
        &serde_json::json!({"steps": [{"operation": set_cells,
            "arguments": {"sheet": "00000000-0000-4000-8000-000000000001", "start": "A1",
                "rows": [["Visits", "Signups"], [240, 18]]}}],
            "then": ""}),
    )]);
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[SHEETS],
        endpoint_planner(&url),
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
    let turn = message_text(&first["messages"][1]);
    // The task names Sheets, which the room has no record of: one is made
    // before the plan, so its operations are listed and nothing is opened.
    assert!(turn.contains(set_cells), "{turn}");
    assert!(!turn.contains("app.so.tilo.sheets#open"), "{turn}");

    agent.close();
    host.relay.shutdown().await;
}

/// An agent reads an open record in the planning conversation: the first
/// turn shows what the artwork holds (read before planning) and `draw` in
/// full; the model asks `read_view` for `inspect` and is answered in the
/// same conversation, then plans the band — two model requests and no
/// re-plan — and the read is no step of the task.
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
                "then": ""}),
        ),
    ]);
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[CANVAS],
        endpoint_planner(&url),
        tilo_content::ContentStore::memory(),
    )
    .await;
    let canvas = agent.open(CANVAS, None).unwrap();

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
    let turn = message_text(&requests[0]["messages"][1]);
    assert!(
        turn.contains("canvas.artwork#draw")
            && turn.contains(&format!("(document {})", canvas.record))
            && turn.contains("canvas.artwork#inspect answered"),
        "the first turn shows draw in full and what the artwork holds: {turn}"
    );
    let read = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(message_text)
        .collect::<String>();
    assert!(
        read.contains("canvas.artwork#inspect answered") && read.contains("width"),
        "the read answered in the conversation: {read}"
    );

    agent.close();
    host.relay.shutdown().await;
}

/// Steps written out run as written: a look renders the artwork as content
/// the room keeps, and the task's reply names it. A step that takes what
/// the step before it produced, after one that produced nothing, is refused
/// before it runs, saying why.
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
    let store = tilo_content::ContentStore::memory();
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[CANVAS],
        Box::new(tilo_agent_participant::DirectPlanner),
        store.clone(),
    )
    .await;
    let canvas = agent.open(CANVAS, None).unwrap();
    let operations = agent.survey(&canvas);
    let address = |action: &str| {
        operations
            .iter()
            .find(|descriptor| descriptor.address.action == action)
            .unwrap_or_else(|| panic!("the artwork publishes {action}"))
            .address
            .clone()
    };
    let step = |action: &str, arguments: serde_json::Value| {
        let address = address(action);
        serde_json::json!({
            "app": CANVAS,
            "record": canvas.record,
            "resource": address.resource,
            "action": address.action,
            "arguments": arguments,
        })
    };
    let draw = step(
        "draw",
        serde_json::json!({"marks": [{"shape": {"kind": "rect", "x": 0, "y": 0,
            "width": 10, "height": 10}, "fill": "#0F4C5C"}]}),
    );

    let looked = run_task(
        &mut agent,
        &control,
        &serde_json::json!([
            draw,
            step("look", serde_json::json!({"format": "png", "size": 64})),
        ])
        .to_string(),
    );
    assert_eq!(looked.state, TaskState::Completed, "{looked:?}");
    let reply = control.info().conversation.last().unwrap().text.clone();
    let produced = reply
        .strip_prefix("Done: 2 actions. Produced ")
        .and_then(|rest| rest.strip_suffix('.'))
        .unwrap_or_else(|| panic!("the reply names what was produced: {reply}"));
    let hash = tilo_actions::content_hash(produced).expect("a content address");
    let png = store.read(hash).expect("the room keeps the look");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    // Each step's record names the content it produced: the look's
    // picture, and nothing for the draw.
    assert_eq!(looked.steps.len(), 2, "{looked:?}");
    assert!(looked.steps[0].content.is_empty(), "{looked:?}");
    assert_eq!(looked.steps[1].content, vec![produced.to_owned()]);

    // An export the room keeps (no `path`) is a read: the agent runs it
    // without asking its person, and its step names the picture.
    let exported = run_task(
        &mut agent,
        &control,
        &serde_json::json!([step("export", serde_json::json!({"format": "png"}))]).to_string(),
    );
    assert_eq!(exported.state, TaskState::Completed, "{exported:?}");
    let [address] = exported.steps[0].content.as_slice() else {
        panic!("the export's step names one piece of content: {exported:?}");
    };
    let picture = store
        .read(tilo_actions::content_hash(address).unwrap())
        .expect("the room keeps the export");
    assert_eq!(&picture[..8], b"\x89PNG\r\n\x1a\n");

    let mut carried = step("draw", serde_json::json!({"marks": []}));
    carried["carries"] = true.into();
    let another = step(
        "draw",
        serde_json::json!({"marks": [{"shape": {"kind": "rect", "x": 20, "y": 20,
            "width": 5, "height": 5}, "fill": "#E9C46A"}]}),
    );
    let refused = run_task(
        &mut agent,
        &control,
        &serde_json::json!([another, carried]).to_string(),
    );
    assert_eq!(refused.state, TaskState::Failed, "{refused:?}");
    let error = refused.error.unwrap_or_default();
    assert!(
        error.contains("draw takes what the step before it produced")
            && error.contains("no preceding action result"),
        "{error}"
    );

    agent.close();
    host.relay.shutdown().await;
}

/// A video still an agent asks for in a task is drawn by its host's render
/// service, not the record's worker, and kept as room content its step
/// names. Needs the render helper (`cargo build -p tilo-video-render --bin
/// tilo-video-render`); without it the test says so and returns, unless
/// `TILO_REQUIRE_MEDIA=1`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_s_video_still_is_drawn_by_the_hosts_render_service() {
    if !tilo_agent_participant::render::available() {
        assert!(
            std::env::var_os("TILO_REQUIRE_MEDIA").is_none(),
            "the render helper is required"
        );
        eprintln!("skipped: no tilo-video-render beside the test or at $TILO_VIDEO_RENDER");
        return;
    }
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
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[VIDEO],
        Box::new(tilo_agent_participant::DirectPlanner),
        store.clone(),
    )
    .await;
    let video = agent.open(VIDEO, None).unwrap();
    let operations = agent.survey(&video);
    let step = |action: &str, arguments: serde_json::Value| {
        let address = operations
            .iter()
            .find(|descriptor| descriptor.address.action == action)
            .unwrap_or_else(|| panic!("the project publishes {action}"))
            .address
            .clone();
        serde_json::json!({
            "app": VIDEO,
            "record": video.record,
            "resource": address.resource,
            "action": address.action,
            "arguments": arguments,
        })
    };
    // A one-second title on one track.
    let mut project = video_editor::Project::new("Agent cut");
    project.set_frame_size(320, 180);
    let track = video_editor::model::Track {
        name: "V1".into(),
        kind: video_editor::model::TrackKind::Video,
        index: 1,
        ..video_editor::model::Track::default()
    };
    let title = video_editor::model::MediaItem {
        name: "Title".into(),
        kind: video_editor::model::MediaKind::Text,
        duration: 1.0,
        text: Some(video_editor::model::TextSource::new("Agent")),
        ..video_editor::model::MediaItem::default()
    };
    let (track_id, title_id) = (track.id, title.id);
    project.tracks.push(track);
    project.media.push(title);
    let clip = project.add_clip(track_id, title_id, 0.0).unwrap();
    project.clip_mut(clip).unwrap().out_point = 1.0;
    let task = run_task(
        &mut agent,
        &control,
        &serde_json::json!([
            step(
                "field-edit",
                serde_json::json!({"edit": video_editor::resource::load_project(&project)})
            ),
            step(
                "view-frame-content",
                serde_json::json!({"at": 0.5, "width": 160})
            ),
        ])
        .to_string(),
    );
    assert_eq!(task.state, TaskState::Completed, "{task:?}");
    let [address] = task.steps[1].content.as_slice() else {
        panic!("the still's step names one piece of content: {task:?}");
    };
    let png = store
        .read(tilo_actions::content_hash(address).unwrap())
        .expect("the room keeps the still");
    assert_eq!(png.get(..4), Some(&b"\x89PNG"[..]));

    agent.close();
    host.relay.shutdown().await;
}

/// Steps written as a scenario writes them: the document's body named as a
/// field of the record (`word-processor.document/body`), which the record
/// does not publish as a resource of its own, and a picture carried from
/// the step before into an operation of a record that was not open when
/// the steps were read. Both run, unasked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn written_steps_name_a_records_field_and_carry_content_into_it() {
    const WRITER: &str = "so.tilo.word-processor";
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
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[CANVAS, WRITER],
        Box::new(tilo_agent_participant::DirectPlanner),
        store.clone(),
    )
    .await;
    let task = run_task(
        &mut agent,
        &control,
        &serde_json::json!([
            {"app": WRITER, "resource": "word-processor.document/body", "action": "replace-body",
             "arguments": {"markdown": "# Summary\n\nSee below.\n\n# Results\n\nNumbers by region"}},
            {"app": CANVAS, "resource": "canvas.artwork", "action": "draw",
             "arguments": {"marks": [{"shape": {"kind": "rect", "x": 0, "y": 0,
                 "width": 10, "height": 10}, "fill": "#0F4C5C"}]}},
            {"app": CANVAS, "resource": "canvas.artwork", "action": "export",
             "arguments": {"format": "png"}},
            {"app": WRITER, "resource": "word-processor.document", "action": "place-image",
             "arguments": {"after": "Results"}, "carries": true},
        ])
        .to_string(),
    );
    assert_eq!(task.state, TaskState::Completed, "{task:?}");
    assert_eq!(task.steps.len(), 4, "{task:?}");

    agent.close();
    host.relay.shutdown().await;
}

/// The id of a new workbook's first sheet, `Sheet1`.
const FIRST_SHEET: &str = "00000000-0000-4000-8000-000000000001";

/// What the cells `range` of the first sheet of the workbook at `route`
/// show, row by row.
fn sheet_rows(
    agent: &mut AgentParticipant,
    route: &DocumentRoute,
    range: &str,
) -> serde_json::Value {
    let read = tilo_actions::door::ActionAddress::new("sheets.workbook", "read-cells");
    let arguments = serde_json::json!({"sheet": "Sheet1", "range": range});
    agent
        .invoke(route, &read, arguments.as_object().unwrap())
        .unwrap()
        .and_then(|output| output.value)
        .map(|value| value["rows"].clone())
        .unwrap_or_default()
}

/// A step quotes a value a formula computed and computes with it
/// (`{{Sheet1!B4|$}}`, `{{= Sheet1!B4 / 3 }}`): the host fills both in
/// from the workbook as the step runs, so the model never adds the figures
/// up itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_step_takes_the_value_a_formula_computed() {
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
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[SHEETS],
        Box::new(tilo_agent_participant::DirectPlanner),
        tilo_content::ContentStore::memory(),
    )
    .await;
    let sheets = agent.open(SHEETS, None).unwrap();
    let set_cells = |start: &str, rows: serde_json::Value| {
        serde_json::json!({"app": SHEETS, "record": sheets.record, "resource": "sheets.workbook",
            "action": "set-cells", "arguments": {"sheet": FIRST_SHEET, "start": start, "rows": rows}})
    };
    let task = run_task(
        &mut agent,
        &control,
        &serde_json::json!([
            set_cells(
                "A1",
                serde_json::json!([
                    ["Item", "Cost"],
                    ["Venue", 100],
                    ["Food", 200],
                    ["Total", "=SUM(B2:B3)"]
                ])
            ),
            set_cells(
                "C1",
                serde_json::json!([["Total {{Sheet1!B4|$}}", "{{= Sheet1!B4 / 3 }}"]])
            ),
        ])
        .to_string(),
    );
    assert_eq!(task.state, TaskState::Completed, "{task:?}");
    assert_eq!(
        sheet_rows(&mut agent, &sheets, "C1:D1"),
        serde_json::json!([["Total $300.00", 100.0]])
    );

    agent.close();
    host.relay.shutdown().await;
}

const MESSAGES: &str = "so.tilo.messages";

/// A step quotes one message among several alike by a rule (the newest from
/// a sender after a time) and a part of it by a pattern: the host picks the
/// message and takes the code out, so the model copies neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_step_takes_a_field_of_the_message_a_rule_picks() {
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
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[SHEETS, MESSAGES],
        Box::new(tilo_agent_participant::DirectPlanner),
        tilo_content::ContentStore::memory(),
    )
    .await;
    let sheets = agent.open(SHEETS, None).unwrap();
    let messages = agent.open(MESSAGES, None).unwrap();
    let deliver = |from: &str, body: &str, date: &str| {
        serde_json::json!({"app": MESSAGES, "record": messages.record, "resource": "tilo.messages",
            "action": "deliver-message", "arguments": {"from": from, "body": body, "date": date}})
    };
    let quoted = "messages/search-messages?query=code#/messages\
                  [from=parcelly, after 2026-10-14T09:15Z, newest]";
    let task = run_task(
        &mut agent,
        &control,
        &serde_json::json!([
            deliver("Parcelly", "Parcelly login code: 330817.", "2026-10-13T17:42:10Z"),
            deliver("Parcelly", "Parcelly login code: 552398.", "2026-10-14T09:15:41Z"),
            deliver("Northwind Bank", "Your sign-in code is 615204.", "2026-10-14T09:16:00Z"),
            deliver("Parcelly", "Parcelly: your new login code: 771046.", "2026-10-14T09:16:20Z"),
            {"app": SHEETS, "record": sheets.record, "resource": "sheets.workbook",
             "action": "set-cells", "arguments": {"sheet": FIRST_SHEET, "start": "A1", "rows": [[
                format!("{{{{{quoted}/body|match:(\\d{{6}})}}}}"),
                format!("Received {{{{{quoted}/date|long}}}}"),
             ]]}},
        ])
        .to_string(),
    );
    assert_eq!(task.state, TaskState::Completed, "{task:?}");
    assert_eq!(
        sheet_rows(&mut agent, &sheets, "A1:B1"),
        serde_json::json!([[771_046.0, "Received October 14, 2026"]])
    );

    agent.close();
    host.relay.shutdown().await;
}

/// A value that is not there fails its step with the reference named, and
/// the task's repair plans around it with that reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_value_goes_to_repair_naming_its_reference() {
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
    let set_cells = |rows: serde_json::Value| {
        tool_call(
            "p1",
            "plan",
            &serde_json::json!({"steps": [{"operation": "sheets.workbook#set-cells",
                "arguments": {"sheet": FIRST_SHEET, "start": "A1", "rows": rows}}], "then": ""}),
        )
    };
    let (url, sent) = model_endpoint(vec![
        set_cells(serde_json::json!([["{{Sheet1!Z99}}"]])),
        set_cells(serde_json::json!([["No figure yet"]])),
    ]);
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[SHEETS],
        endpoint_planner(&url),
        tilo_content::ContentStore::memory(),
    )
    .await;
    let sheets = agent.open(SHEETS, None).unwrap();

    let info = run_task(
        &mut agent,
        &control,
        "In Sheets, put the figure from Z99 in A1",
    );
    assert_eq!(info.state, TaskState::Completed, "{info:?}");
    let failed = info
        .steps
        .iter()
        .find(|step| step.state == TaskState::Failed)
        .and_then(|step| step.error.clone())
        .unwrap_or_default();
    assert!(
        failed.contains("{{Sheet1!Z99}}") && failed.contains("empty"),
        "the failed step names its reference: {info:?}"
    );
    let requests: Vec<serde_json::Value> = sent
        .try_iter()
        .map(|body| serde_json::from_str(&body).unwrap())
        .collect();
    assert_eq!(requests.len(), 2, "a plan and its repair");
    let repair = message_text(&requests[1]["messages"][1]);
    assert!(
        repair.contains("{{Sheet1!Z99}}") && repair.contains("failed"),
        "the repair is told which reference: {repair}"
    );
    assert_eq!(
        sheet_rows(&mut agent, &sheets, "A1"),
        serde_json::json!([["No figure yet"]])
    );

    agent.close();
    host.relay.shutdown().await;
}

/// Two agents adding a row by its key at once each keep their row: an
/// upsert names the row by what it holds, and a new one is its own row,
/// never the next one by position, which the other agent is writing too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upserts_by_key_from_two_agents_overwrite_nothing() {
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
    let (mut first, _) = planning_agent(
        &root,
        (&host, &host_device),
        &[SHEETS],
        Box::new(tilo_agent_participant::DirectPlanner),
        tilo_content::ContentStore::memory(),
    )
    .await;
    let (mut second, _) = planning_agent_as(
        "second",
        &root,
        (&host, &host_device),
        &[SHEETS],
        (
            Box::new(tilo_agent_participant::DirectPlanner),
            tilo_content::ContentStore::memory(),
        ),
    )
    .await;
    let sheets = first.open(SHEETS, None).unwrap();
    let address = |action: &str| tilo_actions::door::ActionAddress::new("sheets.workbook", action);
    let headers = serde_json::json!({"sheet": FIRST_SHEET, "start": "A1",
        "rows": [["Name", "Seats"], ["Ana", 1]]});
    first
        .invoke(&sheets, &address("set-cells"), headers.as_object().unwrap())
        .unwrap();
    let theirs = wait_for("the second agent to open the workbook", || {
        first.pump();
        second.pump();
        let opened = second
            .open(SHEETS, Some(&sheets.record))
            .ok()
            .filter(|route| sheet_rows(&mut second, route, "A2") == serde_json::json!([["Ana"]]));
        async move { opened }
    })
    .await;

    // Neither has seen the other's upsert when it makes its own.
    let upsert = |name: &str, seats: i64| serde_json::json!({"sheet": FIRST_SHEET, "match": {"Name": name}, "value": {"Seats": seats}});
    first
        .invoke(
            &sheets,
            &address("upsert-item"),
            upsert("Ben", 2).as_object().unwrap(),
        )
        .unwrap();
    second
        .invoke(
            &theirs,
            &address("upsert-item"),
            upsert("Cy", 3).as_object().unwrap(),
        )
        .unwrap();
    second
        .invoke(
            &theirs,
            &address("upsert-item"),
            upsert("ana", 4).as_object().unwrap(),
        )
        .unwrap();
    let rows = wait_for("both agents to hold both rows", || {
        first.pump();
        second.pump();
        let ours = sheet_rows(&mut first, &sheets, "A2:B4");
        let same = ours == sheet_rows(&mut second, &theirs, "A2:B4");
        let full = ours
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| !row[0].is_null()));
        async move { (same && full).then_some(ours) }
    })
    .await;
    let mut rows: Vec<String> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|row| format!("{} {}", row[0].as_str().unwrap(), row[1]))
        .collect();
    rows.sort();
    assert_eq!(rows, ["Ana 4.0", "Ben 2.0", "Cy 3.0"]);

    first.close();
    second.close();
    host.relay.shutdown().await;
}

const CHAT: &str = "so.tilo.chat";

/// The last message of a chat-completions request's body: the turn it
/// asks the model to answer.
fn last_turn(body: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(body).unwrap();
    request["messages"]
        .as_array()
        .unwrap()
        .last()
        .map(message_text)
        .unwrap_or_default()
}

/// The plain text of the chat's message parts `agent` holds beside its
/// chat record at `route`.
fn chat_parts_text(agent: &AgentParticipant, route: &DocumentRoute) -> String {
    let Some(chat) = agent.app(route) else {
        return String::new();
    };
    chat.records_beside()
        .iter()
        .filter_map(|part| chat.read_record(part, tilo_agent_participant::search::plain_text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// An agent planning a task hears of a chat post another agent made
/// meanwhile with the post itself and its author's name, and an agent
/// waiting in the chat for a post naming its author wakes when that
/// author posts it. The chat keeps its messages in parts beside its
/// record: what the waiter compares is what the chat shows as a message,
/// its author and its text, not the record's roster alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chat_post_by_another_agent_is_news_with_its_text_and_wakes_a_wait() {
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
    let plan = |id: &str, input: serde_json::Value| tool_call(id, "plan", &input);
    let (gate, held) = std::sync::mpsc::channel();
    let (url, sent) = gated_model_endpoint(vec![
        (
            plan(
                "p1",
                serde_json::json!({"steps": [], "then": "", "wait_for": {"app": "chat",
                "contains": "Rae LR-2041 code entered", "seconds": 60}}),
            ),
            Some(held),
        ),
        (
            plan(
                "p2",
                serde_json::json!({"steps": [], "then": "", "wait_for": {"app": "chat",
                "contains": "Rae LR-2042 code entered", "seconds": 60}}),
            ),
            None,
        ),
        (
            plan(
                "p3",
                serde_json::json!({"steps": [], "reply": "Both codes were entered."}),
            ),
            None,
        ),
        (
            plan(
                "p4",
                serde_json::json!({"steps": [], "reply": "Both codes were entered."}),
            ),
            None,
        ),
    ]);
    let (mut waiter, control) = planning_agent_labelled(
        ("waiter-1c", "Ana"),
        &root,
        (&host, &host_device),
        &[CHAT],
        (endpoint_planner(&url), tilo_content::ContentStore::memory()),
    )
    .await;
    let (mut poster, poster_control) = planning_agent_labelled(
        ("relay-9e", "Rae"),
        &root,
        (&host, &host_device),
        &[CHAT],
        (
            Box::new(tilo_agent_participant::DirectPlanner),
            tilo_content::ContentStore::memory(),
        ),
    )
    .await;
    let theirs = poster.open(CHAT, None).unwrap();
    let begin = serde_json::json!({"name": "Rae", "initials": "RA", "color": 2});
    poster
        .invoke(
            &theirs,
            &tilo_actions::door::ActionAddress::new("chat.workspace", "begin-chat"),
            begin.as_object().unwrap(),
        )
        .unwrap();
    // Posted as a written task, as a step of a plan is.
    let post = |poster: &mut AgentParticipant, text: &str| {
        let step = serde_json::json!([{"app": CHAT, "resource": "chat.messages",
            "action": "post", "arguments": {"channel": chat::record::CHAT_CHANNEL, "text": text}}]);
        let task = poster_control.message(&step.to_string()).unwrap();
        poster.run_until(Some(Instant::now() + Duration::from_secs(60)), |_| {
            !poster_control.is_running(&task)
                && !poster_control
                    .info()
                    .tasks
                    .iter()
                    .any(|info| info.id == task && info.state == TaskState::Queued)
        });
        let info = poster_control
            .info()
            .tasks
            .into_iter()
            .find(|info| info.id == task)
            .unwrap();
        assert_eq!(info.state, TaskState::Completed, "{info:?}");
    };
    post(&mut poster, "LR-2041 code requested");
    let ours = waiter.open(CHAT, None).unwrap();
    wait_for("the waiter to hold the first post", || {
        waiter.pump();
        poster.pump();
        let held = chat_parts_text(&waiter, &ours).contains("LR-2041 code requested");
        async move { held.then_some(()) }
    })
    .await;

    let task = control
        .message("Wait in Chat until Rae posts the codes for LR-2041 and LR-2042, then say so")
        .unwrap();
    wait_for("the first plan to be asked for", || {
        waiter.pump();
        poster.pump();
        let request = sent.try_recv().ok();
        async move { request }
    })
    .await;
    // While the plan is being made, Rae posts what it will wait for.
    post(&mut poster, "LR-2041 code entered");
    wait_for("the waiter to hold the post", || {
        waiter.pump();
        poster.pump();
        let held = chat_parts_text(&waiter, &ours).contains("LR-2041 code entered");
        async move { held.then_some(()) }
    })
    .await;
    gate.send(()).unwrap();
    let told = last_turn(
        &wait_for("the plan again for the news", || {
            waiter.pump();
            poster.pump();
            let request = sent.try_recv().ok();
            async move { request }
        })
        .await,
    );
    let news = told
        .split_once("While you worked, new messages arrived (chat).")
        .and_then(|(_, rest)| rest.split_once("Read them first."))
        .map_or_else(
            || panic!("the re-plan is told of the news: {told}"),
            |(news, _)| news,
        );
    assert!(news.contains("changed by Rae"), "{news}");
    assert!(news.contains("LR-2041 code entered"), "{news}");
    assert!(
        !news.contains("LR-2041 code requested"),
        "what the room held before the plan is not news: {news}"
    );

    // The re-plan waits for the second code; Rae posts it meanwhile.
    wait_for("the waiter to wait", || {
        waiter.pump();
        poster.pump();
        let waiting = control.is_running(&task)
            && control
                .activity()
                .is_some_and(|activity| activity.starts_with("Waiting for a message"));
        async move { waiting.then_some(()) }
    })
    .await;
    post(&mut poster, "LR-2042 code entered");
    let told = last_turn(
        &wait_for("the waiter to wake", || {
            waiter.pump();
            poster.pump();
            let request = sent.try_recv().ok();
            async move { request }
        })
        .await,
    );
    assert!(
        told.contains("What you waited for arrived in chat"),
        "{told}"
    );
    assert!(told.contains("LR-2042 code entered"), "{told}");
    assert!(!told.contains("nothing arrived"), "{told}");

    waiter.close();
    poster.close();
    host.relay.shutdown().await;
}

/// Post `text` in the chat's `#chat` channel as `poster`, as a written
/// task runs a plan's step.
fn post_in_chat(poster: &mut AgentParticipant, control: &AgentControl, text: &str) {
    let step = serde_json::json!([{"app": CHAT, "resource": "chat.messages",
        "action": "post", "arguments": {"channel": chat::record::CHAT_CHANNEL, "text": text}}]);
    assert_eq!(
        run_task(poster, control, &step.to_string()).state,
        TaskState::Completed
    );
}

/// The plain text of the chat record `agent` holds at `route` (its roster,
/// its channels), without its messages.
fn chat_record_text(agent: &AgentParticipant, route: &DocumentRoute) -> String {
    agent
        .app(route)
        .and_then(|chat| chat.read(tilo_agent_participant::search::plain_text))
        .unwrap_or_default()
}

/// Pump every agent of `agents` for `long`, as their loops would run.
async fn pump_for(agents: &mut [&mut AgentParticipant], long: Duration) {
    let until = Instant::now() + long;
    while Instant::now() < until {
        for agent in agents.iter_mut() {
            agent.pump();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Three agents in one chat, as a sign-in relay runs: Ana (planned by the
/// model at `url`) waits for Rae's post; Rae made the chat; Ben is a third
/// agent who has not opened it yet.
struct RelayRoom {
    host: support::TestHost,
    _steward: steward::Steward,
    ana: (AgentParticipant, std::sync::Arc<AgentControl>),
    rae: (AgentParticipant, std::sync::Arc<AgentControl>),
    ben: (AgentParticipant, std::sync::Arc<AgentControl>),
    _state: tempfile::TempDir,
}

impl RelayRoom {
    async fn start(url: &str) -> Self {
        let (ada, _) = UserIdentity::generate("Ada", 1);
        let host_device = enrol(&ada, "home-server", DeviceCapabilities::host());
        let state = tempfile::tempdir().unwrap();
        let host = start_host(
            &ada,
            &host_device,
            state.path(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await;
        let steward = steward::Steward::start(&host.relay, &ada).await;
        let direct = || {
            (
                Box::new(tilo_agent_participant::DirectPlanner)
                    as Box<dyn tilo_agent_participant::Planner>,
                tilo_content::ContentStore::memory(),
            )
        };
        let ana = planning_agent_labelled(
            ("ana-121641", "Ana"),
            &ada,
            (&host, &host_device),
            &[CHAT],
            (endpoint_planner(url), tilo_content::ContentStore::memory()),
        )
        .await;
        let mut rae = planning_agent_labelled(
            ("rae-cf1531", "Rae"),
            &ada,
            (&host, &host_device),
            &[CHAT],
            direct(),
        )
        .await;
        let ben = planning_agent_labelled(
            ("ben-5920f2", "Ben"),
            &ada,
            (&host, &host_device),
            &[CHAT],
            direct(),
        )
        .await;
        let chat = rae.0.open(CHAT, None).unwrap();
        let begin = serde_json::json!({"name": "Rae", "initials": "RA", "color": 2});
        rae.0
            .invoke(
                &chat,
                &tilo_actions::door::ActionAddress::new("chat.workspace", "begin-chat"),
                begin.as_object().unwrap(),
            )
            .unwrap();
        post_in_chat(&mut rae.0, &rae.1, "LR-2041 code requested");
        let mut room = Self {
            host,
            _steward: steward,
            ana,
            rae,
            ben,
            _state: state,
        };
        wait_for("Ana to see the chat", || {
            room.ana.0.pump();
            room.rae.0.pump();
            let seen = room
                .ana
                .0
                .known_records()
                .iter()
                .any(|route| route.app_id == CHAT);
            async move { seen.then_some(()) }
        })
        .await;
        let ours = room.ana.0.open(CHAT, None).unwrap();
        wait_for("Ana to hold the first post", || {
            room.ana.0.pump();
            room.rae.0.pump();
            let held = chat_parts_text(&room.ana.0, &ours).contains("LR-2041 code requested");
            async move { held.then_some(()) }
        })
        .await;
        room
    }

    /// Ben opens the chat, introducing himself in its roster, and Ana
    /// holds his introduction.
    async fn ben_joins(&mut self) {
        let ours = self.ana.0.open(CHAT, None).unwrap();
        wait_for("Ben to see the chat", || {
            self.ben.0.pump();
            self.rae.0.pump();
            let seen = self.ben.0.known_records().contains(&ours);
            async move { seen.then_some(()) }
        })
        .await;
        let theirs = self.ben.0.open(CHAT, None).unwrap();
        assert_eq!(theirs, ours);
        wait_for("Ben to hold the first post", || {
            self.ben.0.pump();
            self.rae.0.pump();
            let held = chat_parts_text(&self.ben.0, &theirs).contains("LR-2041 code requested");
            async move { held.then_some(()) }
        })
        .await;
        wait_for("Ana to hold Ben's introduction", || {
            self.ana.0.pump();
            self.ben.0.pump();
            self.rae.0.pump();
            let held = chat_record_text(&self.ana.0, &ours).contains("Ben");
            async move { held.then_some(()) }
        })
        .await;
    }

    async fn pump_for(&mut self, long: Duration) {
        pump_for(
            &mut [&mut self.ana.0, &mut self.rae.0, &mut self.ben.0],
            long,
        )
        .await;
    }

    async fn next_request(
        &mut self,
        sent: &std::sync::mpsc::Receiver<String>,
        what: &str,
    ) -> String {
        wait_for(what, || {
            self.ana.0.pump();
            self.rae.0.pump();
            self.ben.0.pump();
            let request = sent.try_recv().ok();
            async move { request }
        })
        .await
    }

    async fn ana_waits(&mut self, task: &str) {
        wait_for("Ana to wait", || {
            self.ana.0.pump();
            self.rae.0.pump();
            self.ben.0.pump();
            let waiting = self.ana.1.is_running(task)
                && self
                    .ana
                    .1
                    .activity()
                    .is_some_and(|activity| activity.starts_with("Waiting for a message"));
            async move { waiting.then_some(()) }
        })
        .await;
    }

    async fn ana_ends(&mut self, task: &str) -> TaskState {
        wait_for("Ana's task to end", || {
            self.ana.0.pump();
            self.rae.0.pump();
            self.ben.0.pump();
            let ended = self
                .ana
                .1
                .info()
                .tasks
                .into_iter()
                .find(|info| info.id == task)
                .map(|info| info.state)
                .filter(|state| !matches!(state, TaskState::Queued | TaskState::Running));
            async move { ended }
        })
        .await
    }

    async fn close(self) {
        self.ana.0.close();
        self.rae.0.close();
        self.ben.0.close();
        self.host.relay.shutdown().await;
    }
}

/// Another agent introducing itself in the chat (its roster, not a
/// message) is not news: a requester whose wait was planned meanwhile is
/// not planned again, and a wait for any message in the chat sleeps
/// through the next introduction and wakes on the post.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_introducing_itself_in_chat_neither_replans_nor_wakes_a_waiting_task() {
    let plan = |id: &str, input: serde_json::Value| tool_call(id, "plan", &input);
    let (gate, held) = std::sync::mpsc::channel();
    let (url, sent) = gated_model_endpoint(vec![
        (
            plan(
                "p1",
                serde_json::json!({"steps": [], "then": "", "wait_for": {"app": "chat",
                "seconds": 60}}),
            ),
            Some(held),
        ),
        (
            plan(
                "p2",
                serde_json::json!({"steps": [], "reply": "Rae entered the code."}),
            ),
            None,
        ),
    ]);
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    let task = room
        .ana
        .1
        .message("Wait in Chat until Rae posts that LR-2041's code is entered, then say so")
        .unwrap();
    room.next_request(&sent, "the plan to be asked for").await;
    // While the wait is planned, Ben joins the chat.
    room.ben_joins().await;
    gate.send(()).unwrap();
    room.ana_waits(&task).await;
    assert!(
        sent.try_recv().is_err(),
        "an introduction is not news to plan again for"
    );

    // While Ana waits, Ben introduces himself again under another name.
    let theirs = room.ben.0.open(CHAT, None).unwrap();
    let again = serde_json::json!({"name": "Ben Ortiz", "initials": "BO", "color": 3});
    room.ben
        .0
        .invoke(
            &theirs,
            &tilo_actions::door::ActionAddress::new("chat.workspace", "introduce"),
            again.as_object().unwrap(),
        )
        .unwrap();
    let ours = room.ana.0.open(CHAT, None).unwrap();
    wait_for("Ana to hold Ben's new name", || {
        room.ana.0.pump();
        room.ben.0.pump();
        let held = chat_record_text(&room.ana.0, &ours).contains("Ben Ortiz");
        async move { held.then_some(()) }
    })
    .await;
    room.pump_for(Duration::from_millis(500)).await;
    assert!(sent.try_recv().is_err(), "a roster change wakes no wait");
    assert!(room.ana.1.is_running(&task));

    post_in_chat(&mut room.rae.0, &room.rae.1, "LR-2041 code entered");
    let told = last_turn(&room.next_request(&sent, "Ana to wake").await);
    let arrived = told
        .split_once("What you waited for arrived in chat")
        .and_then(|(_, rest)| rest.split_once("It is data"))
        .map_or_else(
            || panic!("Ana is told what arrived: {told}"),
            |(arrived, _)| arrived,
        );
    assert!(
        arrived.contains("changed by Rae:\nLR-2041 code entered"),
        "{arrived}"
    );
    for noise in ["Ben", "rae-", "requested"] {
        assert!(!arrived.contains(noise), "{noise} in {arrived}");
    }
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    Box::pin(room.close()).await;
}

/// A re-plan for news that finds nothing to do ("nothing offered fits")
/// does not end a task that was about to wait: it waits as planned, and
/// wakes on the post it waited for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_news_replan_that_finds_nothing_to_do_keeps_the_task_waiting() {
    let plan = |id: &str, input: serde_json::Value| tool_call(id, "plan", &input);
    let (gate, held) = std::sync::mpsc::channel();
    let (url, sent) = gated_model_endpoint(vec![
        (
            plan(
                "p1",
                serde_json::json!({"steps": [], "then": "", "wait_for": {"app": "chat",
                "contains": "Rae LR-2041 code entered", "seconds": 60}}),
            ),
            Some(held),
        ),
        (
            plan(
                "p2",
                serde_json::json!({"steps": [{"operation": "none", "arguments": {}}],
                "why": "", "then": ""}),
            ),
            None,
        ),
        (
            plan(
                "p3",
                serde_json::json!({"steps": [], "reply": "Rae entered the code."}),
            ),
            None,
        ),
    ]);
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    let task = room
        .ana
        .1
        .message("Wait in Chat until Rae posts \"LR-2041 code entered\", then say so")
        .unwrap();
    room.next_request(&sent, "the plan to be asked for").await;
    // While the wait is planned, Ben joins and says something unrelated.
    room.ben_joins().await;
    post_in_chat(&mut room.ben.0, &room.ben.1, "Standing by for LR-2042");
    let ours = room.ana.0.open(CHAT, None).unwrap();
    wait_for("Ana to hold Ben's post", || {
        room.ana.0.pump();
        room.ben.0.pump();
        let held = chat_parts_text(&room.ana.0, &ours).contains("Standing by");
        async move { held.then_some(()) }
    })
    .await;
    gate.send(()).unwrap();
    let told = last_turn(
        &room
            .next_request(&sent, "the plan again for the news")
            .await,
    );
    assert!(
        told.contains(
            "new messages arrived (chat).\n- chat, changed by Ben: Standing by for LR-2042\n"
        ),
        "the news is Ben's post alone, by name: {told}"
    );
    assert!(told.contains("1. wait for a message"), "{told}");

    // The model finds nothing to do: the task waits as it had planned.
    room.ana_waits(&task).await;
    room.pump_for(Duration::from_millis(300)).await;
    assert!(room.ana.1.is_running(&task));
    post_in_chat(&mut room.rae.0, &room.rae.1, "LR-2041 code entered");
    let told = last_turn(&room.next_request(&sent, "Ana to wake").await);
    assert!(
        told.contains("What you waited for arrived in chat")
            && told.contains("LR-2041 code entered"),
        "{told}"
    );
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    Box::pin(room.close()).await;
}
