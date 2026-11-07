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
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};

use common::{
    CANVAS_ENTRY, HEIGHT, PERSON, WIDTH, arrangement, on_screen, private_home, show_preset, until,
    wait_for,
};

/// The agent the workspace keeps for [`PERSON`]: the id its actor works as, and
/// the one the session makes first when the workspace keeps none.
const AGENT: &str = "assistant";

/// Enter, the key that submits the canvas entry.
const KEY_ENTER: u32 = 28;

/// A headless session with the shell as the whole screen and the apps a
/// development session seeds, the framed canvas in front.
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

/// The workspace in front, as its id.
fn workspace(session: &Session) -> String {
    session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace is in front")
}

/// Chat's record of the workspace in front, as a window of it reads the record:
/// read from the document on disk, the store every app of the workspace opens,
/// never by holding a writer the session is still writing through. `chat.workspace`
/// is Chat's own primary record, kept under the owner's package store.
fn chat(session: &Session) -> Option<Chat> {
    let data = cli::data_home();
    let workspace = workspace(session);
    std::thread::spawn(move || read_chat(&data, &workspace))
        .join()
        .ok()
        .flatten()
}

fn read_chat(data: &std::path::Path, workspace: &str) -> Option<Chat> {
    let record = ui::record::RecordId::package_primary().storage_key();
    let store = app_state::FsStore::new(data.to_path_buf());
    let stored = durable_store::DurableStore::read(
        &store,
        &durable_store::StorageKey::record("so.tilo.chat", workspace, record),
    )
    .ok()
    .flatten()?;
    // A window appends as it changes, and this read may land on a tail it has
    // not finished writing: what came before it is the record as the last
    // finished write left it, which is what Chat loads.
    let options = automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Ignore);
    let doc = automerge::AutoCommit::load_with_options(&stored.bytes, options).ok()?;
    let state = tilo_app::tilo_resource::Reader::<Chat>::read(&doc).ok()?;
    Some(state.state().as_ref().clone())
}

/// The channels Chat's record holds, as `(id, participant keys)`.
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

/// Type `text` in the canvas entry and press Enter: the person asking, through
/// the shell's own action.
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

/// A question typed into the canvas entry posts into the person's ask
/// conversation with the agent the workspace keeps, and that conversation stands
/// on the canvas where they typed.
#[test]
fn a_question_typed_on_the_canvas_asks_the_kept_agent() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    let before: Vec<String> = chat(&session)
        .map(|chat| channels(&chat).into_iter().map(|(id, _)| id).collect())
        .unwrap_or_default();

    ask(&mut session, "what is on my task list?");

    // A channel Chat holds that was not there before: the ask conversation.
    let placed = wait_for(
        &mut session,
        "the ask conversation on the canvas",
        |session| {
            let told = chat(session)
                .map(|chat| channels(&chat))
                .unwrap_or_default();
            let made: Vec<String> = told
                .iter()
                .map(|(id, _)| id.clone())
                .filter(|id| !before.contains(id))
                .collect();
            arrangement(session).places.into_iter().find(|place| {
                made.iter()
                    .any(|id| place.record.ends_with(&format!("/{id}")))
            })
        },
    );
    let channel = placed
        .record
        .rsplit('/')
        .next()
        .expect("the place shows a channel")
        .to_owned();

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

/// A second ask brings the conversation's place forward instead of standing a
/// second place of it (`shell.md` §8.1).
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
