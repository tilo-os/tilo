//! Text typed on the canvas that asks (`docs/designs/app-language/shell.md` §8.1,
//! "Asking from the canvas").
//!
//! The canvas entry reads "Find something, or ask". A line that finds something
//! places it, as it always did (`arrangement_placing.rs`). A line that is a
//! question, or that names nothing, is not refused: the session posts it as the
//! person into their ask conversation with the agent the workspace keeps, and
//! that conversation stands at the caret, so the person sees what they asked and
//! the agent's answer arrives in the place that asked it.
//!
//! Every step here is the production path: the canvas's own `place-found`
//! action on `shell:arrangement`, the session's Chat record opened through the
//! same store every app of the workspace opens, and Chat's own
//! `create-channel-with-id` and `post` on it. Nothing here writes Chat's state
//! by hand.
//!
//! What is stubbed: the agent. This file proves the ask reaches the conversation
//! — the channel exists, it takes the kept agent as a participant, and it holds
//! the person's message — which is exactly the turn `conversations.rs` derives
//! and the broker delivers. The agent's answer is [`agent_answers.rs`]'s.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use std::time::Duration;

use chat::record::{Chat, Participant};
use tilo_app::tilo_workspace_agent::{
    KIND as AGENT_KIND, OWNER as AGENT_OWNER, WorkspaceAgents as AgentRecord,
    workspace_agents as agent_actions,
};
use tilo_compositor::OutputConfig;
use tilo_resource::Resource as _;
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};
use ui::record::workspace::WorkspaceRecords;
use ui::record::{RecordId, RecordOwners};

use common::{
    CANVAS_ENTRY, HEIGHT, PERSON, WIDTH, arrangement, on_screen, private_home, show_preset, until,
};

/// The agent the workspace keeps for [`PERSON`]: the id its actor works as.
const AGENT: &str = "assistant";

/// The workspace in front, as its id, and the grants its records are decided by.
fn workspace(session: &Session) -> (String, tilo_action_policy::WorkspacePolicy) {
    session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|held| (held.id().to_string(), held.policy.clone()))
        })
        .expect("a workspace is in front")
}

/// The workspace's records as an app of it opens them, as this device's person.
fn records(person: &str, workspace: &str) -> WorkspaceRecords {
    WorkspaceRecords::in_workspace(
        RecordOwners::new(tilo_cli_kit::data_home()),
        workspace.to_owned(),
        ui::permissions::UserId(person.to_owned()),
    )
}

/// Chat's record of `workspace`, opened as the session opens it.
fn chat_record(session: &Session) -> chat::Chat {
    use tilo_clock::wall;
    let (workspace, policy) = workspace(session);
    let records = records(PERSON, &workspace);
    let permissions = ui::permissions::PermissionContext::from_standing(
        &workspace,
        tilo_action_policy::Standing::person(policy, PERSON.to_owned()),
    )
    .with_policy_time_sampler(std::sync::Arc::new(|| Ok(wall())));
    let _ = permissions;
    let id = RecordId::package_primary();
    let opened = records
        .open_record_at("so.tilo.chat", id.as_str(), chat::Chat::KIND, &|doc| {
            chat::Chat::initialize(doc).map_err(|error| error.to_string())
        })
        .expect("Chat's record of the workspace");
    let reader = opened
        .read(tilo_resource::Reader::<chat::Chat>::read)
        .expect("the record reads")
        .expect("the record is Chat's");
    reader.state_owned()
}

/// The headless session with the shell as the whole screen, the tilolang apps
/// a development session seeds, and the agent door at `root`; the framed canvas
/// in front.
fn session(root: &std::path::Path) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(common::seeded_apps())],
            questions: Some(Questions::new()),
            action_plane: ActionPlane::At(root.join("plane.sock")),
            agent_door: Some(root.join("agent.sock")),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    show_preset(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    session
}

/// Keep `AGENT` for [`PERSON`] in the workspace's record of its agents, as the
/// person's own Settings tab does when they make one: the ask conversation
/// needs someone to ask, and the record is where "who works here" lives.
fn keep_an_agent(session: &mut Session) {
    use tilo_resource::Id;
    let (workspace, policy) = workspace(session);
    let id = RecordId::package_primary();
    let records = records(PERSON, &workspace);
    let opened = records
        .open_record_at(AGENT_OWNER, id.as_str(), AGENT_KIND, &|doc| {
            AgentRecord::initialize(doc).map_err(|error| error.to_string())
        })
        .expect("the workspace's agents record");
    let reader = opened
        .read(tilo_resource::Reader::<AgentRecord>::read)
        .expect("the record reads")
        .expect("the record is the agents'");
    let _ = reader;
    let record = tilo_app::framework::WorkspaceRecord::<AgentRecord>::open_at(
        &records,
        AGENT_OWNER,
        id.as_str(),
        ("chat", "Chat"),
        None,
        None,
    )
    .expect("the agents record opens");
    let name = tilo_resource::NonBlank::parse(AGENT).expect("a name");
    record
        .invoke(&AgentRecord::CREATE_AGENT.with(agent_actions::CreateAgent {
            name,
            keeper: None,
            id: Some(AGENT.to_owned()),
            model: None,
            colour: Some(0),
        }))
        .expect("the person makes an agent");
    let _ = Id::from_raw("");
    // The record's write lands in the store the pass reads.
    for _ in 0..3 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// The channels Chat's record of the workspace holds, as `(id, participants)`.
fn channels(chat: &Chat) -> Vec<(String, Vec<String>)> {
    chat.channels
        .iter()
        .map(|(id, channel)| {
            (
                id.as_str().to_owned(),
                channel
                    .participants
                    .iter()
                    .map(|key| key.0.clone())
                    .collect(),
            )
        })
        .collect()
}

/// Type "what is on my task list?" in the canvas entry and press Enter: the
/// person asking, through the shell's own action.
fn ask_on_the_canvas(session: &mut Session, text: &str) {
    common::type_text(session, text);
    for pressed in [true, false] {
        session
            .compositor()
            .input(tilo_compositor::InputEvent::Key {
                keycode: 28,
                pressed,
            })
            .expect("a key");
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// A question typed in the canvas entry posts into the person's ask
/// conversation with the kept agent, and that conversation stands on the
/// canvas where they typed.
#[test]
fn a_question_typed_on_the_canvas_asks_the_kept_agent() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    keep_an_agent(&mut session);
    // The entry takes the keys.
    common::press(&mut session, CANVAS_ENTRY);

    let before: Vec<String> = channels(&chat_record(&session))
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    ask_on_the_canvas(&mut session, "what is on my task list?");

    // A place stands that was not there, and it is the ask conversation: a
    // channel Chat holds with the agent as a participant.
    let placed = until(
        &mut session,
        "the ask conversation on the canvas",
        |session| {
            let places = arrangement(session).places;
            let held = channels(&chat_record(session));
            places.into_iter().find_map(|place| {
                let id = place.record.rsplit('/').next()?;
                (held.iter().any(|(held, _)| held == id) && !before.contains(&id.to_owned()))
                    .then_some(place)
            })
        },
    );
    let channel = placed
        .record
        .rsplit('/')
        .next()
        .expect("the place shows a channel");
    let chat = chat_record(&session);
    let (_, participants) = channels(&chat)
        .into_iter()
        .find(|(id, _)| id == channel)
        .expect("the channel is Chat's");
    assert!(
        participants.contains(&Participant::agent(AGENT).key()),
        "the agent takes part in the conversation it is asked in: {participants:?}"
    );
    assert!(
        participants.contains(&Participant::person(PERSON).key()),
        "the person it is with is one of its participants: {participants:?}"
    );

    // The conversation holds the person's message, as they typed it: the turn
    // the broker delivers to that agent is this message.
    let written = until(&mut session, "the question in the conversation", |_| {
        let chat = chat_record(&session);
        let records = records(PERSON, &workspace(&session).0);
        let part_id = chat::parts::part(0);
        let part = records
            .open_record_at(
                "so.tilo.chat",
                part_id.as_str(),
                chat::parts::ChatPart::KIND,
                &|doc| chat::parts::ChatPart::initialize(doc).map_err(|error| error.to_string()),
            )
            .ok()?;
        let reader = part
            .read(tilo_resource::Reader::<chat::parts::ChatPart>::read)
            .ok()??
            .state_owned();
        let _ = &chat;
        reader
            .messages
            .iter()
            .find(|(_, message)| {
                message.channel.as_str() == channel
                    && message.text.trim() == "what is on my task list?"
            })
            .map(|(_, message)| message.text.clone())
    });
    assert_eq!(written, "what is on my task list?");
}

/// The place the ask conversation stands at is the caret where the person
/// typed, as any placed text is: one arranging step of theirs.
#[test]
fn the_ask_conversation_stands_where_the_person_typed() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    keep_an_agent(&mut session);
    common::press(&mut session, CANVAS_ENTRY);
    ask_on_the_canvas(&mut session, "what is on my task list?");

    let (who, verb) = until(&mut session, "the arranging step", |session| {
        session.arrangement().borrow_mut().last_step()
    });
    assert_eq!(who, PERSON, "the step is the person's own");
    assert_eq!(verb, "place", "asking is one arranging step");
}

/// A second ask in the same conversation brings the place that is already
/// there forward instead of standing a second one (`shell.md` §8.1).
#[test]
fn a_second_ask_brings_the_conversation_forward() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    keep_an_agent(&mut session);
    common::press(&mut session, CANVAS_ENTRY);
    ask_on_the_canvas(&mut session, "what is on my task list?");
    let first = until(&mut session, "the conversation placed", |session| {
        let places = arrangement(session).places;
        places
            .into_iter()
            .find(|place| place.record.contains("/channels/"))
    });
    let count = arrangement(&session).places.len();

    common::press(&mut session, CANVAS_ENTRY);
    ask_on_the_canvas(&mut session, "and what is due today?");
    // The conversation's own messages changed; the arrangement's places did
    // not, because the place that is there is the one brought forward.
    until(
        &mut session,
        "the second question in the conversation",
        |_| {
            let records = records(PERSON, &workspace(&session).0);
            let part_id = chat::parts::part(0);
            let part = records
                .open_record_at(
                    "so.tilo.chat",
                    part_id.as_str(),
                    chat::parts::ChatPart::KIND,
                    &|doc| {
                        chat::parts::ChatPart::initialize(doc).map_err(|error| error.to_string())
                    },
                )
                .ok()?;
            let reader = part
                .read(tilo_resource::Reader::<chat::parts::ChatPart>::read)
                .ok()??
                .state_owned();
            reader
                .messages
                .iter()
                .any(|(_, message)| message.text.trim() == "and what is due today?")
                .then_some(())
        },
    );
    let places = arrangement(&session).places;
    assert_eq!(
        places.len(),
        count,
        "the second ask added no place: {places:?}"
    );
    assert!(
        places.iter().any(|place| place.key == first.key),
        "the conversation's own place is still the one it stands in"
    );
}
