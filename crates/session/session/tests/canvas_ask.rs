//! Text typed on the canvas that asks (`docs/designs/app-language/shell.md` §8.1,
//! "Asking from the canvas").
//!
//! The canvas entry reads "Find something, or ask". A line that finds something
//! places it, as it always did (`arrangement_placing.rs`). A line that is a
//! question, or that names nothing, is the person's send to the agent they keep
//! in the workspace: `session:send-to#send`, the one send a send-to box, an agent
//! and a script run, so it lands in their conversation with the agent exactly as
//! a box's message does, and Chat opens it.
//!
//! Every step here is the production path: the canvas entry's own `place-found`
//! action on `shell:arrangement`, typed and submitted with the keyboard, and the
//! send as the session carries it out, read back from `session:send-to`'s own
//! reading. Chat's process is not part of this session, so the message landing in
//! the conversation is proven where Chat runs (the nested agent story).

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use std::time::Duration;

use tilo_compositor::InputEvent;
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};

use common::{CANVAS_ENTRY, PERSON, arrangement, on_screen, private_home, show_preset, until};

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

/// Type `text` in the canvas entry and press Enter: the person asking, through the shell's own
/// action.
fn ask(session: &mut Session, text: &str) {
    common::press(session, CANVAS_ENTRY);
    common::type_text(session, text);
    // Pressed and let go within a frame, as `type_text` types: held across one, Enter would repeat.
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key {
                keycode: KEY_ENTER,
                pressed,
            })
            .expect("a key");
    }
    session.step(Duration::from_millis(1)).expect("a frame");
}

/// The `n`th send the session carried out, as `session:send-to` reads, once it is there.
fn sent(session: &mut Session, n: usize) -> serde_json::Value {
    common::wait_for(session, format!("send {n} to be carried out"), |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|sent| sent.as_array()?.get(n).cloned())
    })
}

/// A question typed into the canvas entry is the person's send of those words to the workspace's
/// agent: one where they keep none yet, named as the workspace names its agent. Nothing stands on
/// the canvas for it — the conversation it lands in is Chat's to open.
#[test]
fn a_question_typed_on_the_canvas_is_sent_to_the_workspaces_agent() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    let places = arrangement(&session).places.len();

    ask(&mut session, "what is on my task list?");

    let first = sent(&mut session, 0);
    assert_eq!(first["text"], "what is on my task list?", "{first}");
    let to = first["to"].as_array().expect("whom it went to");
    assert_eq!(to.len(), 1, "one agent is asked: {first}");
    assert_eq!(
        to[0]["kind"], "new-agent",
        "the person kept no agent yet: {first}"
    );
    assert_eq!(to[0]["name"], "Assistant", "{first}");
    assert_eq!(
        arrangement(&session).places.len(),
        places,
        "the ask stands no place of its own on the canvas"
    );
}

/// The agent the first ask made is the one every later ask goes to: the workspace's record names it
/// as the person's, so a second question continues the same conversation rather than making a
/// second agent.
#[test]
fn a_second_ask_goes_to_the_agent_the_first_made() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    sent(&mut session, 0);

    ask(&mut session, "and what is due today?");

    let second = sent(&mut session, 1);
    assert_eq!(second["text"], "and what is due today?", "{second}");
    let to = &second["to"][0];
    assert_eq!(to["kind"], "agent", "the agent kept now: {second}");
    assert_eq!(to["name"], "Assistant", "{second}");
    assert!(
        to["id"].as_str().is_some_and(|id| !id.is_empty()),
        "named by its id: {second}"
    );
}
