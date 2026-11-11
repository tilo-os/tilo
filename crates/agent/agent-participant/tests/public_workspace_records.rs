//! An agent in a public workspace — one listed in its host's directory, which
//! anyone may walk into — is replayed its governance and the shared desktop,
//! and is sent every other record once it opens it. So a record it does not
//! hold there is one the workspace may still send, never one the workspace
//! lacks: an agent that took it as new founded a document of its own, and the
//! first change it published forked the workspace's.
//!
//! Over the real session, a member's device writes a note and the workspace's
//! own chat, whose newest part is its second; an agent joining afterwards
//! opens the note by its id, and opens Chat naming no record. Both waits for
//! what the workspace holds and opens the member's documents, and neither
//! founds one; a message the agent posts goes to the chat's newest part.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::future_not_send,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; one scenario in order, \
              driven on the test's own thread (the agent's apps are not Send)"
)]

mod common;

tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();
#[path = "../../../session/host/tests/support/participant.rs"]
mod participant;
#[path = "../../../session/host/tests/support/mod.rs"]
mod support;

use std::sync::Mutex;

use collab_types::DocumentMutation;
use common::{Body as _, Driveable};
use tilo_actions::door::ActionDescriptor;
use tilo_agent_participant::{
    AgentControl, AgentIdentity, AgentParticipant, ParticipantConfig, PlanContext, Planner,
    Replica, ReplicaActor, Seed, Step, StepAction,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::agents::{AgentStatus, TaskState};
use tilo_relay_protocol::delegation::{DelegateScope, Delegation};
use tilo_relay_protocol::directory::Listing;
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_session_host::{PeerEvent, PeerHandle};
use tilo_user_identity::{CertificationPolicy, DeviceCapabilities, UserIdentity};

use support::{door, enrol, enter, start_workspace_host, wait_for};

const WRITER: &str = "so.tilo.word-processor";
const CHAT: &str = "so.tilo.chat";

/// The channel the member made in the workspace's chat.
const MEMBER_CHANNEL: &str = "front-desk";

/// What the scripted model plans, by what the task names: the record the
/// member wrote (named outright), or the app whose one record the workspace
/// keeps.
struct Scripted {
    /// The route of the note the member's device wrote.
    note: String,
    /// What each plan was asked for, in order.
    planned: std::sync::Arc<Mutex<Vec<String>>>,
}

impl Planner for Scripted {
    fn plan(&mut self, task: &str, _context: &PlanContext) -> Result<Vec<Step>, String> {
        self.planned.lock().unwrap().push(task.to_owned());
        let (app, record) = if task.contains("note") {
            (WRITER.to_owned(), Some(self.note.clone()))
        } else {
            (CHAT.to_owned(), None)
        };
        Ok(vec![Step {
            app,
            record,
            action: StepAction::Open,
            carries: None,
            serves: None,
        }])
    }
}

/// How many operations the member's window tries before it gives up on
/// writing: the same bound an agent's own edit search keeps.
const EDITS: usize = 40;

/// The member's replica driven the way a caller with no knowledge of the app
/// drives one: through whatever operation the record publishes that changes
/// it.
struct Held<'a>(&'a mut Replica);

impl Driveable for Held<'_> {
    fn survey(&mut self) -> Vec<ActionDescriptor> {
        self.0.survey()
    }

    fn body(&mut self) -> String {
        self.0
            .read(|doc| serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap())
            .unwrap()
    }

    fn run(&mut self, action: &StepAction) -> Result<(), String> {
        match action {
            StepAction::Invoke { address, arguments } => self
                .0
                .invoke(address, arguments, false)
                .map(|_| ())
                .map_err(|error| format!("{}: {error}", error.code())),
            StepAction::Open | StepAction::Continue(_) | StepAction::Wait(_) => Ok(()),
        }
    }
}

/// Whether `peer`'s channel for `route` is ready.
fn keyed(peer: &PeerHandle, route: &DocumentRoute) -> bool {
    let prefix = format!("route {}/{} ", route.app_id, route.record);
    peer.status()
        .channels
        .lines()
        .any(|line| line.starts_with(&prefix) && line.contains(": ready;"))
}

/// Write `document` to `route` from `member`'s device, once the workspace's
/// host has admitted it to the record's channel, and wait until the host has
/// it: what the workspace holds of that record from then on.
async fn written(member: &PeerHandle, route: &DocumentRoute, document: Vec<u8>) {
    member.open_route(route.clone()).unwrap();
    let route_keyed = route.clone();
    wait_for("the member's channel to be keyed", || async {
        keyed(member, &route_keyed).then_some(())
    })
    .await;
    member
        .publish(route.clone(), &DocumentMutation::Snapshot(document))
        .unwrap();
    let receipt = member.barrier().unwrap();
    wait_for("the member's write to be durable on the host", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_waits_for_a_public_workspaces_records_and_never_founds_them() {
    // The workspace, public: anyone may walk in, and its host sends a device
    // its records when it opens them.
    let (owner, _) = UserIdentity::generate("Owner", 1);
    let host_device = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = support::scratch_dir();
    let host = start_workspace_host(&owner, &host_device, state.path(), false).await;
    let workspaces = host.relay.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host_device.cert,
            Listing {
                description: "Everyone".into(),
                default_role: WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let workspace = lounge.workspace.clone();

    // A member came in through the door and wrote the workspace's records.
    let (member_root, _) = UserIdentity::generate("Member", 2);
    let member_device = enrol(&member_root, "member-browser", DeviceCapabilities::member());
    let member = enter(
        &host,
        door(
            &host,
            &owner,
            &host_device,
            &workspace,
            member_root.verifying_key().to_bytes(),
        )
        .await,
        &member_device.cert,
        &member_device.key,
        &host_device.cert,
        lounge.workspace_id,
    )
    .await;

    // The note, as the member's window made it: a document of its own with
    // the text they wrote.
    let mut note =
        common::open_in(&workspace, WRITER, "member", ReplicaActor::Owner, Seed::New).unwrap();
    let (edit, refused) = common::find_edit(&mut Held(&mut note), EDITS);
    assert!(
        edit.is_some(),
        "the member cannot write a document: {refused:#?}"
    );
    let note_route = DocumentRoute {
        app_id: WRITER.into(),
        cluster_id: workspace.clone(),
        record: note.record_id().route().to_owned(),
    };
    assert!(
        !note_route.record.is_empty(),
        "the note is a record of its own, not the app's record of the workspace"
    );
    written(
        &member,
        &note_route,
        note.read(|doc| doc.clone().save()).unwrap(),
    )
    .await;

    // The workspace's chat, as a member makes it: the record every window of
    // the app in this workspace opens, with a channel of their own in it.
    let chat = common::open_in(&workspace, CHAT, "member", ReplicaActor::Owner, Seed::New).unwrap();
    let mut chat_document = chat.read(Clone::clone).unwrap();
    common::add_channel(&mut chat_document, "member", MEMBER_CHANNEL, None);
    let chat_route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.clone(),
        record: String::new(),
    };
    written(&member, &chat_route, chat_document.save()).await;
    // Its messages fill parts: the newest the workspace holds is part 1,
    // which a device holding no part asks for by the chain's head.
    let channel_id = chat::projection::read(&chat_document)
        .channel_rows()
        .into_iter()
        .find(|channel| channel.name == MEMBER_CHANNEL)
        .expect("the member's channel")
        .id
        .as_str()
        .to_owned();
    // Part 1, as a member's window starts it in this workspace's chat.
    let member_records = support::scratch_dir();
    let store = ui::record::RecordOwners::new(member_records.path().to_owned())
        .open(CHAT, &workspace)
        .unwrap();
    let base = std::rc::Rc::new(chat::demo::Workspace::of(chat_document.clone()).read());
    let chain = ui::framework::Chain::<chat::parts::ChatPart>::detached_in(
        store,
        <chat::ChatShell as ui::framework::ResourceApp>::ID,
        "member",
        move || std::rc::Rc::clone(&base),
    )
    .unwrap();
    chain
        .write_start(1, &chat::demo::demo_part("member"))
        .unwrap();
    let newest = chain.part_bytes(1).expect("part 1 is held");
    let part_route = |index: u64| DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.clone(),
        record: chat::parts::part(index).route().to_owned(),
    };
    written(&member, &part_route(1), newest).await;

    // The agent: its own device under the owner, delegated the workspace and
    // the two apps; its model's plan opens what the task names.
    let agent_device = enrol(&owner, "Agent: Ed", CertificationPolicy::agent().ceiling);
    let agent_key = agent_device.cert.device_public_key;
    let now = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &owner,
        agent_device.cert.device_public_key,
        tilo_relay_protocol::delegation::Delegate::Agent {
            agent_id: "ed".into(),
        },
        "Ed",
        [0x2a, 0x7c, 0xd1],
        DelegateScope {
            rooms: vec![workspace.clone()],
            apps: vec![WRITER.into(), CHAT.into()],
        },
        now,
        now.checked_add(3_600_000_000_000).unwrap(),
    )
    .unwrap();
    host.relay
        .register_agent(&agent_device.cert, delegation.clone())
        .await
        .unwrap();
    let _laptop = participant::Participant::start(&host.relay, &owner).await;
    let planned = std::sync::Arc::new(Mutex::new(Vec::new()));
    let control = AgentControl::new(delegation.clone());
    let mut agent = AgentParticipant::start(
        ParticipantConfig {
            host: host.iroh(),
            host_cert: host_device.cert.clone(),
            workspace: workspace.clone(),
            workspace_name: None,
            content: tilo_content::ContentStore::memory(),
        },
        AgentIdentity {
            key: agent_device.key.clone(),
            cert: agent_device.cert.clone(),
            delegation,
        },
        control.clone(),
        common::catalogue(),
        Box::new(Scripted {
            note: note_route.record.clone(),
            planned: std::sync::Arc::clone(&planned),
        }),
    )
    .unwrap();
    wait_for("the agent to be admitted", || {
        agent.pump();
        let waiting = control.status() == AgentStatus::Waiting;
        async move { waiting.then_some(()) }
    })
    .await;

    // A public workspace sent it neither record: what it holds is what it
    // opened, and it opened nothing yet.
    assert!(
        agent.app(&note_route).is_none() && !agent.known_records().contains(&note_route),
        "the member's note reached the agent before it opened one"
    );
    assert!(
        agent.app(&chat_route).is_none() && !agent.known_records().contains(&chat_route),
        "the workspace's chat reached the agent before it opened it"
    );

    // The note's own record: asked for by id, it is waited for and arrives.
    control
        .message("Open the note the workspace already has")
        .unwrap();
    run_task(&mut agent, &control);
    let task = control.info().tasks.last().cloned().unwrap();
    assert_eq!(task.state, TaskState::Completed, "{task:?}");
    let opened = agent
        .app(&note_route)
        .expect("the agent opened the member's note");
    assert!(
        opened.body().contains(common::WRITTEN),
        "the agent holds the member's note: {}",
        opened.body()
    );
    let record = note.read(|doc| doc.clone().get_heads());
    assert_eq!(
        opened.read(|doc| doc.clone().get_heads()),
        record,
        "the agent opened the very document the member wrote"
    );

    // An app that keeps one record per workspace: a step naming none opens
    // the workspace's own chat rather than founding a second one. What the
    // member holds is read by the member's own page, so a document the agent
    // wrote there is seen as it lands.
    let _ = member.drain();
    control.message("Open the workspace's chat").unwrap();
    run_task(&mut agent, &control);
    let task = control.info().tasks.last().cloned().unwrap();
    assert_eq!(task.state, TaskState::Completed, "{task:?}");
    let held = agent
        .app(&chat_route)
        .expect("the agent opened the workspace's chat");
    assert!(
        held.body().contains(MEMBER_CHANNEL),
        "the agent holds the chat the member made, with their channel: {}",
        held.body()
    );
    let founded = member
        .drain()
        .into_iter()
        .filter(|event| {
            matches!(event, PeerEvent::Landed { route, author: Some(author) }
                if *route == chat_route && *author == agent_key)
        })
        .count();
    assert!(
        founded <= 1,
        "the agent founded a chat of its own in the workspace's: it writes only \
         its own introduction there ({founded} documents)"
    );

    // A message the agent posts goes to the chat's newest part, which it
    // asked the workspace for, never to a first part of its own.
    let post = held
        .survey()
        .into_iter()
        .map(|descriptor| descriptor.address)
        .find(|address| address.action == "post")
        .expect("the chat offers to post");
    let arguments = serde_json::json!({
        "channel": channel_id, "text": "the agent's message"
    });
    let arguments = arguments.as_object().unwrap().clone();
    let _ = member.drain();
    wait_for("the agent's message posted", || {
        let posted = agent.invoke(&chat_route, &post, &arguments);
        agent.pump();
        async move { posted.ok().map(drop) }
    })
    .await;
    let parts_written = |events: Vec<PeerEvent>| -> Vec<String> {
        events
            .into_iter()
            .filter_map(|event| match event {
                PeerEvent::Document(document)
                    if document.author == agent_key
                        && chat::parts::part_index(&ui::record::RecordId::parse(
                            &document.route.record,
                        )?)
                        .is_some() =>
                {
                    Some(document.route.record)
                }
                _ => None,
            })
            .collect()
    };
    let mut written_parts = Vec::new();
    // One written into a part of the agent's own never reaches the member,
    // who follows the chat's newest part.
    wait_for(
        "the agent's message to reach the member in the newest part",
        || {
            agent.pump();
            written_parts.extend(parts_written(member.drain()));
            let done = !written_parts.is_empty();
            async move { done.then_some(()) }
        },
    )
    .await;
    assert!(
        written_parts
            .iter()
            .all(|record| *record == part_route(1).record),
        "the agent wrote into a part other than the chat's newest: {written_parts:?}"
    );

    // The plans are the two the tasks asked for: the second task was not
    // planned against a chat the agent had founded.
    let planned = planned.lock().unwrap().clone();
    assert_eq!(planned.len(), 2, "{planned:?}");
    assert!(planned[1].contains("chat"), "{planned:?}");
}

/// Run the agent's current task to its end.
fn run_task(agent: &mut AgentParticipant, control: &AgentControl) {
    let deadline = tilo_test_env::hang_deadline();
    agent.run_until(Some(deadline), |_| {
        control
            .info()
            .tasks
            .last()
            .is_some_and(|task| !matches!(task.state, TaskState::Queued | TaskState::Running))
    });
}
