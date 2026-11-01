//! The shell's arrangement is the person's own typed record (shell.md §2,
//! workspace-model.md §9): what is open where, kept in their record with
//! history and undo, synced to their devices that use the same shell, and
//! shared with no other member.
//!
//! Each test here fails before the arrangement is a record: the desk keeps
//! its own tree, nothing attributes a move to anyone, nothing undoes one,
//! and an agent's move lands under the person's hands.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

use std::time::Duration;

use tilo_compositor::{InputEvent, OutputConfig};
use tilo_desktop_model::own_arrangement::{self, Strips};
use tilo_session::{Options, Session};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// The person at this device, by their root identity (hex).
const ADA: &str = "abababababababababababababababababababababababababababababababab";
/// Another member of the same workspace.
const BO: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

fn session() -> Session {
    session_in(None)
}

/// A session of `person`'s, on a device with no state of its own.
fn session_as(person: &str) -> Session {
    Session::headless(
        Options {
            person: person.to_owned(),
            presentation: ui::headless::fixture_runner_inputs,
            state: None,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// A session that keeps its state in `state` when it is given.
fn session_in(state: Option<std::path::PathBuf>) -> Session {
    Session::headless(
        Options {
            person: ADA.to_owned(),
            presentation: ui::headless::fixture_runner_inputs,
            state,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

fn step(session: &mut Session) {
    session.step(Duration::from_millis(1)).expect("a frame");
}

/// The arrangement the session's record holds now.
fn arrangement(session: &Session) -> Strips {
    session.arrangement().borrow().strips()
}

/// The strip `tab` stands in, by its id.
fn strip_of(session: &Session, tab: u64) -> Option<u64> {
    arrangement(session).strip_of(tab).map(|strip| strip.id)
}

/// A window of `app` on the desk, its tab's id.
fn open_window(session: &mut Session, app: &str) -> u64 {
    // A table's window, opened the way the launcher opens one: a tab of the
    // strip in front.
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    let tab = handle.mint_id();
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == tree.active)
        .expect("a workspace is in front");
    let strip = workspace.strips.first_mut().expect("a strip is in front");
    let group = handle.mint_id();
    strip.window_groups.push(tilo_desktop_model::WindowGroup {
        id: group,
        tabs: vec![tilo_desktop_model::Tab {
            id: tab,
            title: app.to_owned(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: app.to_owned(),
                label: app.to_owned(),
                note: String::new(),
            },
            app_title: None,
        }],
        active_tab: tab,
        width: tilo_desktop_model::WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    handle.replace_tree(tree);
    step(session);
    tab
}

/// An empty strip called `name` in the workspace in front.
fn add_strip(session: &Session, name: &str) -> u64 {
    let handle = session.handle();
    let mut tree = handle.read(Clone::clone);
    let made = tilo_desktop_model::commands::apply_to_active_workspace(
        &mut tree,
        &tilo_desktop_model::commands::DesktopCommand::CreateStrip { name: name.into() },
        &mut || Ok(handle.mint_id()),
    )
    .expect("a strip is made")
    .expect("the strip has an id");
    handle.replace_tree(tree);
    made.0
}

/// The row `strip` (a column of places) stands in, in the arrangement.
fn row_of(session: &Session, strip: u64) -> u64 {
    arrangement(session)
        .strip(strip)
        .expect("the strip is in the record")
        .row
        .0
}

// (1) The person drags a window to another strip: the record holds the move,
// attributed to them, and undo puts it back.
#[test]
fn the_persons_drag_is_in_their_record_attributed_and_undone() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let second = add_strip(&session, "Second");
    step(&mut session);
    let from = row_of(
        &session,
        strip_of(&session, window).expect("it has a strip"),
    );
    assert_ne!(from, second);
    let row = second;

    // The drag's end commits one arranging action, as the person's.
    session
        .arrangement()
        .borrow_mut()
        .ask_person(&move_to(window, row))
        .unwrap();
    step(&mut session);

    // The record holds the move, attributed to the person.
    let held = arrangement(&session);
    let strip = held.strip_of(window).expect("the place is in the record");
    assert_eq!(strip.row.0, row, "the record holds the move");
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("the move is a step of the record's history");
    assert_eq!(who, ADA, "attributed to the person");
    assert_eq!(verb, "move");

    // Undo puts it back.
    session.undo_arrangement().unwrap();
    step(&mut session);
    assert_eq!(
        arrangement(&session)
            .strip_of(window)
            .map(|strip| strip.row.0),
        Some(from),
        "undo restored the place to its row"
    );
}

/// The action that moves `place` to the row `row`.
fn move_to(place: u64, row: u64) -> tilo_session::arrangement::ArrangementAction {
    tilo_session::arrangement::ArrangementAction::Move {
        place,
        row,
        before: None,
    }
}

// (2) An agent moves a window through the published action, attributed to
// the agent and undoable.
#[test]
fn an_agents_move_through_the_action_is_attributed_and_undone() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let second = add_strip(&session, "Second");
    step(&mut session);
    let from = arrangement(&session)
        .strip_of(window)
        .expect("it has a strip")
        .row
        .0;

    // The action is published, so the catalogue, tilo-control and an agent
    // see it.
    let address = session
        .resources()
        .into_iter()
        .flat_map(|resource| {
            resource
                .actions
                .into_iter()
                .map(move |action| (resource.id.0.clone(), action.id.clone()))
        })
        .any(|(resource, action)| resource == "shell:arrangement" && action == "move-place");
    assert!(address, "the arranging action is published");

    session
        .invoke_with_actor(
            "agent-task:reading",
            "shell:arrangement",
            "move-place",
            &serde_json::json!({"place": window, "row": second}),
        )
        .expect("the action runs");
    step(&mut session);

    assert_eq!(
        arrangement(&session)
            .strip_of(window)
            .map(|strip| strip.row.0),
        Some(second),
        "the agent's move is in the record"
    );
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("it is a step of the record's history");
    assert_eq!(who, "agent-task:reading", "attributed to the agent");
    assert_eq!(verb, "move");

    session.undo_arrangement().unwrap();
    step(&mut session);
    assert_eq!(
        arrangement(&session)
            .strip_of(window)
            .map(|strip| strip.row.0),
        Some(from),
        "undo restored it to its row"
    );
}

// (3) A second device with the same shell gets the arrangement; another
// member does not.
#[test]
fn a_second_device_gets_the_arrangement_and_another_member_does_not() {
    let _settled = ui::motion::testing::settled();
    let mut here = session();
    step(&mut here);
    let window = open_window(&mut here, "org.example.Notes");
    let second = add_strip(&here, "Second");
    step(&mut here);
    here.arrangement()
        .borrow_mut()
        .ask_person(&move_to(window, second))
        .unwrap();
    step(&mut here);

    // A second device of the person's places the same desk from the same
    // record.
    let state = tempfile::tempdir().expect("a state directory");
    let mut other = session_in(Some(state.path().to_path_buf()));
    step(&mut other);
    let shared = here.arrangement().borrow().shared_record().clone();
    other.arrangement().borrow_mut().adopt(&shared);
    step(&mut other);
    assert_eq!(
        other.handle().read(|tree| tree
            .workspaces
            .iter()
            .filter(|w| !w.strips.is_empty())
            .count()),
        1,
        "the other device shows the arrangement"
    );

    // What this person's devices share: the record, on the route only their
    // devices hold the key to.
    let id = own_arrangement::record_id(ADA);
    assert_eq!(
        tilo_action_policy::member_record(&id),
        Some((ADA, own_arrangement::KIND)),
        "the record is the person's own"
    );
    // Another member's record of the same kind is another record, which this
    // arrangement's changes never reach.
    assert_ne!(id, own_arrangement::record_id(BO));

    // The other device reads the arrangement from the same record.
    let brought: Strips = own_arrangement::read(&shared);
    assert_eq!(
        brought.strip_of(window).map(|strip| strip.row.0),
        Some(second),
        "the other device has the arrangement"
    );

    // Another member of the workspace runs the same shell on their own
    // device: their arrangement is their own record, and holds none of this
    // one's places.
    let mut theirs = session_as(BO);
    step(&mut theirs);
    assert_eq!(
        theirs.arrangement().borrow().record_id(),
        own_arrangement::record_id(BO),
        "their record is their own"
    );
    assert!(
        theirs
            .arrangement()
            .borrow()
            .strips()
            .place(window)
            .is_none(),
        "nothing of this person's arrangement is theirs"
    );
    // And what they share of theirs contains none of it.
    let theirs_record = theirs.arrangement().borrow().shared_record().clone();
    assert!(
        own_arrangement::read(&theirs_record)
            .place(window)
            .is_none(),
        "their record, read, holds none of this person's places"
    );
}

// (4) While the person types or drags, an agent's move waits, not drops, and
// applies after the gesture ends (shell.md §1).
#[test]
fn an_agents_move_waits_while_the_person_types_and_applies_after() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let second = add_strip(&session, "Second");
    step(&mut session);
    let from = strip_of(&session, window).expect("it has a strip");

    // The person types: nothing moves under their hands.
    session
        .compositor()
        .input(InputEvent::Key {
            keycode: 30,
            pressed: true,
        })
        .unwrap();
    step(&mut session);
    assert!(session.compositor().underway().typing, "they are typing");

    session
        .invoke_with_actor(
            "agent-task:reading",
            "shell:arrangement",
            "move-place",
            &serde_json::json!({"place": window, "row": second}),
        )
        .expect("the action is taken");
    step(&mut session);
    assert_eq!(
        strip_of(&session, window),
        Some(from),
        "it did not move under the person's typing"
    );
    assert_eq!(
        session.arrangement().borrow().waiting(),
        1,
        "and it waits rather than dropping"
    );

    // The typing ends; what waited runs by itself.
    session
        .compositor()
        .input(InputEvent::Key {
            keycode: 30,
            pressed: false,
        })
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while session.arrangement().borrow().waiting() > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the waiting move never ran"
        );
        std::thread::sleep(Duration::from_millis(1100));
        step(&mut session);
    }
    assert_eq!(
        arrangement(&session)
            .strip_of(window)
            .map(|strip| strip.row.0),
        Some(second),
        "it applies once the person is done"
    );
    let (who, _) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("it is a step of the record's history");
    assert_eq!(who, "agent-task:reading", "still attributed to the agent");
}

// An agent's arrangement leaves a place the person pinned where it is
// (shell.md §5's default).
#[test]
fn a_pinned_place_stays_where_the_person_put_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let second = add_strip(&session, "Second");
    step(&mut session);
    let from = strip_of(&session, window).expect("it has a strip");
    // The person pins it, through the published action.
    invoke_as(
        &mut session,
        "shell:arrangement",
        "pin-place",
        &serde_json::json!({"place": window, "pinned": true}),
    );
    assert!(arrangement(&session).place(window).unwrap().pinned);

    // The agent's move is refused, and the place stands.
    let refused = session.invoke_with_actor(
        "agent-task:reading",
        "shell:arrangement",
        "move-place",
        &serde_json::json!({"place": window, "row": second}),
    );
    assert!(refused.is_err(), "the agent may not move a pinned place");
    assert_eq!(strip_of(&session, window), Some(from));
}

/// Invoke `action` of `resource` with `arguments` as the person.
fn invoke_as(session: &mut Session, resource: &str, action: &str, arguments: &serde_json::Value) {
    let empty = serde_json::Map::new();
    let arguments = arguments.as_object().unwrap_or(&empty);
    session
        .invoke(resource, action, arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
    step(session);
}

// The arrangement as the shell draws it: every workspace, row, strip and place, by key, as the
// session publishes it for the shell's views. Which way it is shown is not the arrangement's.
#[test]
fn the_arrangement_reads_out_by_key_for_the_shells_views() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    step(&mut session);
    let reading = session.arrangement().borrow().reading();
    let key = window.to_string();
    let place = reading
        .places
        .iter()
        .find(|place| place.key == key)
        .expect("the place reads out by its key");
    let strip = reading
        .strips
        .iter()
        .find(|strip| strip.key == place.strip)
        .expect("its strip reads out, named by the place");
    assert!(
        reading.rows.iter().any(|row| row.key == strip.row),
        "the strip stands in a row the reading names"
    );
    assert!(
        reading
            .workspaces
            .iter()
            .any(|workspace| workspace.key == strip.workspace),
        "the row is of a workspace the reading names"
    );
    assert!(
        !reading.workspaces.is_empty(),
        "the desk shows its workspaces"
    );
}

// Moving a place to another row through the published action is a place the reading then shows
// in that row, and undo puts it back — the same both ways. Moving a place into another row's
// empty strip makes a strip there; undo takes the place back to the strip it came from.
#[test]
fn a_move_the_reading_shows_is_undone_to_where_it_was() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    step(&mut session);
    let from = strip_of(&session, window).expect("it has a strip");
    let was = row_of(&session, from);
    let second = add_strip(&session, "Second");
    step(&mut session);

    invoke_as(
        &mut session,
        "shell:arrangement",
        "move-place",
        &serde_json::json!({"place": window, "row": second}),
    );
    let key = window.to_string();
    let reading = session.arrangement().borrow().reading();
    let strip = reading
        .places
        .iter()
        .find(|place| place.key == key.as_str())
        .and_then(|place| reading.strips.iter().find(|strip| strip.key == place.strip))
        .expect("the place's strip reads out");
    assert_eq!(strip.row, second.to_string(), "the reading holds the move");

    session.undo_arrangement().unwrap();
    step(&mut session);
    let reading = session.arrangement().borrow().reading();
    let place = reading
        .places
        .iter()
        .find(|place| place.key == key.as_str())
        .expect("the place reads out");
    let strip = reading
        .strips
        .iter()
        .find(|strip| strip.key == place.strip)
        .expect("the place's strip reads out");
    assert_eq!(
        strip.row,
        was.to_string(),
        "undo restores the row the reading shows"
    );
}

// A strip's width is resized through the published action, one step the person undoes.
#[test]
fn resizing_a_strip_is_one_step_and_undone() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    step(&mut session);
    let strip = strip_of(&session, window).expect("it has a strip");
    let was = arrangement(&session)
        .strip(strip)
        .map(|strip| strip.width.permille())
        .expect("the strip is in the record");

    invoke_as(
        &mut session,
        "shell:arrangement",
        "resize-strip",
        &serde_json::json!({"strip": strip, "permille": 333}),
    );
    let now = arrangement(&session)
        .strip(strip)
        .map(|strip| strip.width.permille())
        .expect("the strip is in the record");
    assert_ne!(now, was.max(333), "the width changed");
    assert_eq!(now, 333, "the strip stands narrow");

    session.undo_arrangement().unwrap();
    step(&mut session);
    let back = arrangement(&session)
        .strip(strip)
        .map(|strip| strip.width.permille())
        .expect("the strip is in the record");
    assert_eq!(back, was, "undo restores the width");
}
