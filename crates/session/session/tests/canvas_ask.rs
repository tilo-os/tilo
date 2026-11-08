//! Text typed on the canvas that asks (`docs/designs/app-language/shell.md` §8.1,
//! "Asking from the canvas").
//!
//! The canvas entry reads "Find something, or ask". A line that finds something
//! places it, as it always did (`arrangement_placing.rs`). A line that is a
//! question, or that names nothing, is not refused: the session posts it as the
//! person into their ask conversation with the agent the workspace keeps, and
//! that conversation stands where they typed, so the person sees what they asked
//! in the place that asked it.
//!
//! Every step here is the production path: the canvas entry's own
//! `place-found` action on `shell:arrangement`, the session's Chat record opened
//! through the same store every app of the workspace opens, and Chat's own
//! `create-channel-with-id` and `post` on it. Nothing here writes Chat's state by
//! hand; the record is read back from the store as a window would read it.
//!
//! What is stubbed: the agent itself. This file proves the ask reaches the
//! conversation — the channel exists, it takes the kept agent as a participant,
//! and it holds the person's message — which is exactly the turn
//! `conversations.rs` derives and the broker delivers.

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

use chat::Chat;
use tilo_compositor::InputEvent;
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};

use common::{
    CANVAS_ENTRY, PERSON, arrangement, on_screen, private_home, show_preset, until, wait_for,
};

/// The agent the workspace keeps for [`PERSON`]: the id its actor works as, and the one the session
/// makes first when the workspace keeps none.
const AGENT: &str = "assistant";

/// Enter, the key that submits the canvas entry.
const KEY_ENTER: u32 = 28;

/// A headless session with the shell as the whole screen and the apps a development session seeds,
/// the framed canvas in front.
fn session(root: &std::path::Path) -> Session {
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: false,
        services: vec![Box::new(common::seeded_apps())],
        questions: Some(Questions::new()),
        action_plane: ActionPlane::At(root.join("plane.sock")),
        agent_door: Some(root.join("agent.sock")),
        ..Options::default()
    });
    show_preset(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    session
}

/// The workspace in front, as its id.
fn workspace(session: &Session) -> String {
    session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace is in front")
}

/// Chat's record of the workspace in front, as this session holds it open: the same record the pass
/// derives turns from, so a test reads it without opening a second writer on the store the session
/// writes through.
fn chat(session: &Session) -> Option<Chat> {
    session.chat_of(&workspace(session))
}

/// The channels Chat's record holds, as `(id, participant keys)` .
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

/// Type `text` in the canvas entry and press Enter: the person asking, through the shell's own
/// action.
fn ask(session: &mut Session, text: &str) {
    common::press(session, CANVAS_ENTRY);
    common::type_text(session, text);
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key {
                keycode: KEY_ENTER,
                pressed,
            })
            .expect("a key");
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// A question typed into the canvas entry posts into the person's ask conversation with the agent
/// the workspace keeps, and that conversation stands on the canvas where they typed.
#[test]
fn a_question_typed_on_the_canvas_asks_the_kept_agent() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    let before: Vec<String> = chat(&session)
        .map(|chat| channels(&chat).into_iter().map(|(id, _)| id).collect())
        .unwrap_or_default();

    ask(&mut session, "what is on my task list?");

    // A place stands that was not there, showing a channel of Chat's record: the
    // ask conversation. The arrangement is read from the session's own desk,
    // which is never a store read, so this wait never touches the store the
    // session is writing through.
    until(
        &mut session,
        "the ask conversation on the canvas",
        |session| {
            arrangement(session)
                .places
                .into_iter()
                .any(|place| place.record.contains("/channels/"))
        },
    );
    let placed = arrangement(&session)
        .places
        .into_iter()
        .find(|place| place.record.contains("/channels/"))
        .expect("a place shows a channel");
    let channel = placed
        .record
        .rsplit('/')
        .next()
        .expect("the place shows a channel")
        .to_owned();
    assert!(
        !before.contains(&channel),
        "the conversation was not there before the ask: {before:?}"
    );

    // Chat's record holds it, with the agent a participant: the turn the broker delivers to that
    // agent is the person's message in this channel.
    let (_, participants) = channels(&chat(&session).expect("Chat's record"))
        .into_iter()
        .find(|(id, _)| *id == channel)
        .expect("the channel is Chat's");
    assert!(
        participants.contains(&format!("agent:{AGENT}")),
        "the agent takes part in the conversation it is asked in: {participants:?}"
    );
    assert!(
        participants.contains(&format!("person:{PERSON}")),
        "the person the conversation is with is a participant: {participants:?}"
    );
}

/// The ask is the person's own arranging step, attributed to them.
#[test]
fn the_ask_is_the_persons_own_step() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");

    let (who, verb) = wait_for(&mut session, "the arranging step", |session| {
        session.arrangement().borrow_mut().last_step()
    });
    assert_eq!(who, PERSON, "the step is the person's own");
    assert_eq!(verb, "place", "asking is one arranging step");
}

/// What the person asked gives the agent the workspace keeps a turn: the run the broker hands it,
/// read from the same source the broker reads (shell.md §8.1). The turn is the person's message,
/// with the agent's id and the conversation's address, so the agent answering is answering this ask.
#[test]
fn the_ask_gives_the_kept_agent_a_turn() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    let workspace = workspace(&session);
    let runs = session.conversation_runs();

    let turns = wait_for(&mut session, "the agent's turn", |_session| {
        let turns = runs.runs(&workspace).ok()?;
        (!turns.is_empty()).then_some(turns)
    });
    assert_eq!(turns.len(), 1, "one ask is one turn: {turns:?}");
    let turn = &turns[0];
    assert_eq!(turn.agent, AGENT, "the agent the ask made is the one");
    assert_eq!(
        turn.instruction, "what is on my task list?",
        "the turn is what the person asked"
    );
    let (address, message) = turn
        .issue
        .rsplit_once("/messages/")
        .expect("the issue names the conversation's message");
    assert!(
        address.starts_with("chat.workspace/channels/"),
        "the turn is in the conversation Chat holds: {address}"
    );
    assert_eq!(
        turn.turn.as_ref().map(|turn| turn.message.as_str()),
        Some(message),
        "the turn answers that message"
    );
    assert!(turn.active, "the person's message waits for its answer");
}

/// A second ask brings the conversation's place forward instead of standing a second place of it
/// (`shell.md` §8.1).
#[test]
fn a_second_ask_brings_the_conversation_forward() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    let first = wait_for(&mut session, "the conversation placed", |session| {
        arrangement(session)
            .places
            .into_iter()
            .find(|place| place.record.contains("/channels/"))
    });
    let count = arrangement(&session).places.len();

    ask(&mut session, "and what is due today?");

    let places = arrangement(&session).places;
    assert_eq!(
        places.len(),
        count,
        "the second ask added no place: {places:?}"
    );
    assert!(
        places.iter().any(|place| place.key == first.key),
        "the conversation's own place is the one it stands in"
    );
}
