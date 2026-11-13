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

use std::time::Duration;

use tilo_compositor::InputEvent;
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};

use crate::common::{
    CANVAS_ENTRY, PERSON, arrangement, on_screen, private_home, show_preset, until,
};

/// Enter, the key that submits the canvas entry.
const KEY_ENTER: u32 = 28;

/// A headless session with the shell as the whole screen and the apps a development session seeds,
/// the framed canvas in front.
fn session(root: &std::path::Path) -> Session {
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: false,
        services: vec![Box::new(crate::common::seeded_apps())],
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
    crate::common::press(session, CANVAS_ENTRY);
    crate::common::type_text(session, text);
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
    crate::common::wait_for(session, format!("send {n} to be carried out"), |session| {
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

/// The agent the steps below are attributed to, as an agent's turn names it.
const AGENT: &str = "agent:assistant";
/// What the agent's steps stand beside: the inbox, standing on the canvas at a point of its own.
const INBOX: &str = "tl:so.tilo.lang.mail:inbox";
/// What the agent shows.
const NOTES: &str = "tl:so.tilo.lang.notes:root";

/// The canvas point of the strip `place` stands in, and its workspace.
fn canvas_point(
    session: &Session,
    place: &tilo_session::arrangement::PlaceReading,
) -> (i64, i64, String) {
    let strip = crate::common::strip(session, crate::common::number(&place.strip))
        .expect("the place has its strip");
    (
        strip.canvas_x.into(),
        strip.canvas_y.into(),
        strip.workspace,
    )
}

/// `arguments` placed on the canvas as the agent, and the place showing `record` once it stands.
fn placed_as_agent(
    session: &mut Session,
    record: &str,
    arguments: serde_json::Value,
) -> tilo_session::arrangement::PlaceReading {
    session
        .invoke_with_actor(AGENT, "shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("the place runs: {error}"));
    crate::common::wait_for(session, format!("{record} placed"), |session| {
        crate::common::places(session)
            .into_iter()
            .find(|place| place.record == record)
    })
}

/// An agent's `place` that names no point but something to stand beside stands what it shows in
/// the free space to that place's right, at its line — an agent's turn names the conversation it
/// was asked in, so what it places stands next to that conversation (`shell.md` §8.1). Placing it
/// again brings the place showing it forward rather than standing a second one.
#[test]
fn an_agents_place_without_a_point_stands_beside_what_it_names() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    let inbox = placed_as_agent(
        &mut session,
        INBOX,
        serde_json::json!({"resource": INBOX, "x": 240, "y": 160}),
    );
    let beside = serde_json::json!({"resource": NOTES, "beside": INBOX});
    let notes = placed_as_agent(&mut session, NOTES, beside.clone());

    let (x, y, workspace) = canvas_point(&session, &notes);
    let (ix, iy, inbox_in) = canvas_point(&session, &inbox);
    assert_eq!(workspace, inbox_in, "it stands in the anchor's workspace");
    assert_eq!(y, iy, "it stands at the anchor's line");
    assert!(x > ix, "it stands to the anchor's right: {x} against {ix}");
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("a step of the history");
    assert_eq!(who, AGENT, "attributed to the agent");
    assert_eq!(verb, "place");

    session
        .invoke_with_actor(AGENT, "shell:arrangement", "place", &beside)
        .unwrap_or_else(|error| panic!("the second place runs: {error}"));
    session.step(Duration::from_millis(1)).expect("a frame");
    let shown = crate::common::places(&session)
        .into_iter()
        .filter(|place| place.record == NOTES)
        .count();
    assert_eq!(shown, 1, "one place shows it");
}

/// A place moved beside another (`position` with `beside`, no point) stands in the free space next
/// to it: "put my notes next to the inbox". One that names neither a point nor anything to stand
/// beside is refused rather than sent to the canvas' origin.
#[test]
fn a_place_positioned_beside_another_stands_next_to_it() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    let inbox = placed_as_agent(
        &mut session,
        INBOX,
        serde_json::json!({"resource": INBOX, "x": 240, "y": 160}),
    );
    let note = placed_as_agent(
        &mut session,
        NOTES,
        serde_json::json!({"resource": NOTES, "x": 6000, "y": 4000}),
    );
    let (ix, iy, _) = canvas_point(&session, &inbox);
    let (nx, ny, _) = canvas_point(&session, &note);
    assert!(
        nx >= 6000 && ny >= 4000,
        "the note stands far off first: {nx},{ny}"
    );

    let place = format!("shell:place:{}", note.key);
    let refused = session.invoke_with_actor(AGENT, &place, "position", &serde_json::json!({}));
    assert!(refused.is_err(), "no point and nothing beside is refused");

    session
        .invoke_with_actor(
            AGENT,
            &place,
            "position",
            &serde_json::json!({"beside": format!("shell:place:{}", inbox.key)}),
        )
        .unwrap_or_else(|error| panic!("the position runs: {error}"));
    let moved = crate::common::wait_for(&mut session, "the note moved", |session| {
        let place = crate::common::places(session)
            .into_iter()
            .find(|place| place.record == NOTES)?;
        let (x, y, _) = canvas_point(session, &place);
        (x < 6000).then_some((x, y))
    });
    assert_eq!(moved.1, iy, "it stands at the inbox's line: {moved:?}");
    assert!(moved.0 > ix, "to the inbox's right: {moved:?} against {ix}");
}
