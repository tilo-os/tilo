//! The arrangement is the desk's own record (shell.md §2, workspace-model.md
//! §9): what is open where, kept with the desk the workspace shares, with
//! history and undo. No second record of it exists, and what the desk's windows
//! do not carry — the view a place shows and whether it is pinned — is kept
//! with the desk too.
//!
//! Each test here fails without it: nothing attributes a move to anyone,
//! nothing undoes one, an agent's move lands under the person's hands, and a
//! place's view and pin are gone after a restart.

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
    open_window_called(session, app, app)
}

/// A window of `app` on the desk calling itself `title`, its tab's id.
fn open_window_called(session: &mut Session, app: &str, title: &str) -> u64 {
    // A table's window, opened the way the launcher opens one: a tab of the
    // strip in front.
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    let tab = handle.mint_id();
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.slot == tree.active)
        .expect("a workspace is in front");
    let strip = workspace.strips.first_mut().expect("a strip is in front");
    let group = handle.mint_id();
    strip.window_groups.push(tilo_desktop_model::WindowGroup {
        id: group,
        tabs: vec![tilo_desktop_model::Tab {
            id: tab,
            title: title.to_owned(),
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

/// Every action `resource` and its children publish, by the resource's id and the action's.
fn published_actions(resource: &tilo_actions::ResourceSpec, out: &mut Vec<(String, String)>) {
    out.extend(
        resource
            .actions
            .iter()
            .map(|action| (resource.id.0.clone(), action.id.clone())),
    );
    for child in &resource.children {
        published_actions(child, out);
    }
}

/// The resource the session publishes the place `key` as: what its own actions are pressed on.
fn place_resource(key: u64) -> String {
    tilo_session::arrangement::Item::Place.resource(&key.to_string())
}

/// The resource the session publishes the row `key` as: what a place's move names.
fn row_resource(key: u64) -> String {
    tilo_session::arrangement::Item::Row.resource(&key.to_string())
}

/// The resource the session publishes the strip `key` as: what its own resize is pressed on.
fn strip_resource(key: u64) -> String {
    tilo_session::arrangement::Item::Strip.resource(&key.to_string())
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

    // The place's own resource publishes the actions a press names it by, so the
    // catalogue, tilo-control and an agent see them.
    let mut shown = Vec::new();
    for resource in session.resources() {
        published_actions(&resource, &mut shown);
    }
    let published = shown
        .iter()
        .any(|(resource, action)| *resource == place_resource(window) && action == "move");
    assert!(published, "the place publishes its own actions");

    session
        .invoke_with_actor(
            "agent-task:reading",
            &place_resource(window),
            "move",
            &serde_json::json!({"row": row_resource(second)}),
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

// (3) One record answers what the arrangement is: the desk's own, which the
// workspace shares. Nothing is kept beside it, and another member of the
// workspace reads none of it.
#[test]
fn the_arrangement_is_the_desks_own_and_no_record_beside_it() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let state = home.state().to_path_buf();
    let mut here = session_in(Some(state.clone()));
    step(&mut here);
    let window = open_window(&mut here, "org.example.Notes");
    let second = add_strip(&here, "Second");
    step(&mut here);
    here.arrangement()
        .borrow_mut()
        .ask_person(&move_to(window, second))
        .unwrap();
    step(&mut here);
    assert_eq!(
        strip_of(&here, window).map(|strip| row_of(&here, strip)),
        Some(second),
        "the desk holds the move"
    );
    drop(here);

    // The desk's one document is what keeps it: no record of the arrangement's
    // own is written beside it.
    assert!(
        state.join("tilo").join("desk.automerge").exists(),
        "the desk's document keeps the arrangement"
    );
    assert!(
        !state.join("tilo").join("arrangement.automerge").exists(),
        "no second record of the arrangement is kept"
    );

    // Another member of the workspace runs a desk of their own: nothing of this
    // one's arrangement is theirs.
    let mut theirs = session_as(BO);
    step(&mut theirs);
    assert!(
        theirs
            .arrangement()
            .borrow()
            .strips()
            .place(window)
            .is_none(),
        "nothing of this person's arrangement is theirs"
    );
}

// (5) What the desk's windows do not carry — the view a place shows and the
// pin on it — is kept with the desk, so it survives a restart with the
// arrangement.
#[test]
fn a_places_view_and_pin_are_kept_with_the_desk_across_a_restart() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let state = home.state().to_path_buf();
    let place = {
        let mut session = session_in(Some(state.clone()));
        step(&mut session);
        let window = open_window(&mut session, "org.example.Notes");
        step(&mut session);
        let strip = strip_of(&session, window).expect("the place has a strip");
        let place = session.handle().mint_id();
        session
            .arrangement()
            .borrow_mut()
            .ask_person(&tilo_session::arrangement::ArrangementAction::OpenInto {
                strip,
                after: Some(window),
                place,
                shows: "org.example.Notes".into(),
                view: "editor".into(),
                record: String::new(),
                state: serde_json::json!({
                    "LocalApp": {
                        "app_id": "org.example.Notes",
                        "label": "Notes",
                        "note": "",
                    }
                }),
            })
            .expect("the place opens");
        session
            .arrangement()
            .borrow_mut()
            .ask_person(&tilo_session::arrangement::ArrangementAction::Pin {
                place,
                pinned: true,
            })
            .expect("the place is pinned");
        step(&mut session);
        assert_eq!(
            arrangement(&session)
                .place(place)
                .map(|place| place.view.as_str()),
            Some("editor"),
            "the arrangement holds the view it shows"
        );
        place
    };

    let mut next = session_in(Some(state));
    step(&mut next);
    let held = next.arrangement().borrow().strips();
    let back = held.place(place).expect("the place came back");
    assert_eq!(back.view, "editor", "the view the place shows");
    assert!(back.pinned, "the place is still pinned");
}

// (6) The arrangement a build before this one kept beside the desk is taken
// once: the desk comes up from it, and the file it was kept in is gone.
#[test]
fn the_record_a_build_before_this_one_kept_comes_into_the_desk_once() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let state = home.state().to_path_buf();

    // The record as the build before this one left it: a workspace, a strip
    // and a place, saying what it shows and that it is pinned.
    let (workspace, strip, place) = (1u64, 7u64, 9u64);
    let row = tilo_desktop_model::own_arrangement::Row {
        id: tilo_desktop_model::StripId(strip),
        workspace: tilo_desktop_model::WorkspaceSlot(workspace),
        name: "Writing".into(),
        shell: "so.tilo.shell.strips".into(),
    };
    let held = Strips {
        workspaces: vec![tilo_desktop_model::own_arrangement::PlacedWorkspace {
            id: tilo_desktop_model::WorkspaceSlot(workspace),
            name: "Home".into(),
        }],
        rows: vec![row],
        strips: vec![tilo_desktop_model::own_arrangement::Strip {
            id: strip,
            workspace: tilo_desktop_model::WorkspaceSlot(workspace),
            row: tilo_desktop_model::StripId(strip),
            width: tilo_desktop_model::WindowWidth::Normal,
            maximized: false,
            active: Some(place),
            canvas: None,
            shell: "so.tilo.shell.strips".into(),
            places: vec![tilo_desktop_model::own_arrangement::Place {
                id: place,
                shows: "org.example.Notes".into(),
                view: "editor".into(),
                record: String::new(),
                state: serde_json::json!({
                    "LocalApp": {
                        "app_id": "org.example.Notes",
                        "label": "Notes",
                        "note": "",
                    }
                }),
                title: "Notes".into(),
                pinned: true,
                shell: "so.tilo.shell.strips".into(),
            }],
        }],
        foreign: Vec::new(),
    };
    let mut doc = ui::automerge::AutoCommit::new();
    own_arrangement::write(&mut doc, &held).expect("the record");
    let path = state.join("tilo");
    std::fs::create_dir_all(&path).expect("the state directory");
    let file = path.join("arrangement.automerge");
    std::fs::write(&file, doc.save()).expect("the record is kept");

    // The session starts on a desk the record places, and the record is gone.
    let mut session = session_in(Some(state));
    step(&mut session);
    let held = arrangement(&session);
    let back = held.place(place).expect("the place came into the desk");
    assert_eq!(back.view, "editor", "the view it shows");
    assert!(back.pinned, "and the pin on it");
    assert_eq!(
        session
            .handle()
            .read(|tree| tree.active_workspace().map(|workspace| workspace.slot.0)),
        Some(workspace),
        "the desk comes up on it"
    );
    assert!(!file.exists(), "the record it came from is gone");
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

    // The person types, and the agent asks while they do: the pass that takes the keystroke in is
    // the pass that tells the agent's action the person is busy, so the move waits rather than
    // landing under their hands.
    session
        .compositor()
        .input(InputEvent::Key {
            keycode: 30,
            pressed: true,
        })
        .unwrap();
    step(&mut session);
    session
        .invoke_with_actor(
            "agent-task:reading",
            &place_resource(window),
            "move",
            &serde_json::json!({"row": row_resource(second)}),
        )
        .expect("the action is taken");
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

    // The typing ends; what waited runs by itself, once the product's own pause on typing is over.
    session
        .compositor()
        .input(InputEvent::Key {
            keycode: 30,
            pressed: false,
        })
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while session.arrangement().borrow().waiting() > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the waiting move never ran"
        );
        session.step(Duration::from_millis(50)).expect("a frame");
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
        &place_resource(window),
        "pin",
        &serde_json::json!({"pinned": true}),
    );
    assert!(arrangement(&session).place(window).unwrap().pinned);

    // The agent's move is refused, and the place stands.
    let refused = session.invoke_with_actor(
        "agent-task:reading",
        &place_resource(window),
        "move",
        &serde_json::json!({"row": row_resource(second)}),
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

// A place reads out named the way every surface shows it: the window's own title and its app's
// own name, never an app id, and the app's name alone for a window that has no title.
#[test]
fn a_place_reads_out_named_by_its_window_and_its_app_never_an_id() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window_called(&mut session, "org.example.TextEditor", "Groceries");
    step(&mut session);
    let reading = session.arrangement().borrow().reading();
    let place = reading
        .places
        .iter()
        .find(|place| place.key == window.to_string())
        .expect("the place reads out by its key");
    assert_eq!(
        place.name, "Groceries — Text Editor",
        "the window's own title, then the app's name: never the id it is opened by"
    );
    assert!(
        !place.name.contains("org.example"),
        "no surface ever reads an app id: {}",
        place.name
    );

    let untitled = open_window_called(&mut session, "org.example.TextEditor", "");
    step(&mut session);
    let reading = session.arrangement().borrow().reading();
    let place = reading
        .places
        .iter()
        .find(|place| place.key == untitled.to_string())
        .expect("the untitled window reads out");
    assert_eq!(
        place.name, "Text Editor",
        "a window with no title of its own is its app alone"
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
        &place_resource(window),
        "move",
        &serde_json::json!({"row": row_resource(second)}),
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
        &strip_resource(strip),
        "resize",
        &serde_json::json!({"permille": 333}),
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

// A window is maximized through the published action, so the desk shows the one window it is,
// and one undo brings the others back beside it.
#[test]
fn maximizing_a_window_is_one_step_and_undone() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let other = open_window_called(&mut session, "org.example.Notes", "Other");
    step(&mut session);
    let strip = strip_of(&session, window).expect("it has a strip");
    let other_strip = strip_of(&session, other).expect("the other place is in a strip");
    assert!(
        arrangement(&session)
            .strips
            .iter()
            .all(|strip| !strip.maximized),
        "the desk starts beside itself"
    );

    invoke_as(
        &mut session,
        &strip_resource(strip),
        "maximize-strip",
        &serde_json::json!({}),
    );
    let maximized = |session: &Session, strip: u64| {
        arrangement(session)
            .strip(strip)
            .map(|strip| strip.maximized)
            .expect("the strip is in the record")
    };
    assert!(maximized(&session, strip), "the window fills the desk");
    assert!(
        !maximized(&session, other_strip),
        "and no other window does"
    );

    session.undo_arrangement().unwrap();
    step(&mut session);
    assert!(
        !maximized(&session, strip),
        "undo restores the windows beside each other"
    );
}

/// A session on a device whose salt is the top of an id's high bits: every place, strip and
/// row here is above `i64::MAX`, so no id fits the integer an id-shaped verb would take.
fn salted_session() -> Session {
    Session::headless(
        Options {
            person: ADA.to_owned(),
            presentation: ui::headless::fixture_runner_inputs,
            state: None,
            collab: false,
            salt: 0x8000,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

// Every arranging press names the place, strip or row it presses, so on a device whose ids do
// not fit an integer each of them still reaches the record's own entry: pin, close, move to a
// row and a strip's width all land on the thing they name, and on nothing else.
#[test]
fn every_arranging_press_reaches_the_record_on_a_salted_device() {
    let _settled = ui::motion::testing::settled();
    let mut session = salted_session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let other = open_window_called(&mut session, "org.example.Notes", "Other");
    assert!(
        window > u64::try_from(i64::MAX).unwrap() && other > window,
        "the desk's ids do not fit an integer: {window}, {other}"
    );
    let second = add_strip(&session, "Second");
    step(&mut session);
    let strip = strip_of(&session, window).expect("the place is in a strip");
    let other_strip = strip_of(&session, other).expect("the other place is in a strip");
    assert_ne!(strip, other_strip, "each place has its own strip");

    // Each press names its own resource, by the whole of the id: pin reaches this place, and
    // the other place stands unpinned.
    invoke_as(
        &mut session,
        &place_resource(window),
        "pin",
        &serde_json::json!({"pinned": true}),
    );
    assert_eq!(
        arrangement(&session)
            .place(window)
            .map(|place| place.pinned),
        Some(true),
        "pin reached the place it named"
    );
    assert_eq!(
        arrangement(&session).place(other).map(|place| place.pinned),
        Some(false),
        "and no other place"
    );

    // So does a strip's resize, which names its strip.
    invoke_as(
        &mut session,
        &strip_resource(strip),
        "resize",
        &serde_json::json!({"permille": 333}),
    );
    assert_eq!(
        arrangement(&session)
            .strip(strip)
            .map(|strip| strip.width.permille()),
        Some(333),
        "resize reached the strip it named"
    );

    // And a move names its place and its row, each by its own resource.
    invoke_as(
        &mut session,
        &place_resource(window),
        "move",
        &serde_json::json!({"row": row_resource(second)}),
    );
    assert_eq!(
        arrangement(&session)
            .strip_of(window)
            .map(|strip| strip.row.0),
        Some(second),
        "move reached the place it named, into the row it named"
    );
    assert_eq!(
        strip_of(&session, other),
        Some(other_strip),
        "the other place stayed in its own strip"
    );

    // Close names its place too: only that place goes.
    invoke_as(
        &mut session,
        &place_resource(window),
        "close",
        &serde_json::json!({}),
    );
    assert_eq!(
        strip_of(&session, window),
        None,
        "close reached the place it named"
    );
    assert_eq!(
        strip_of(&session, other),
        Some(other_strip),
        "and closed no other place"
    );
}

// (4) A place whose saved window state this build cannot read: the window
// does not come back, and its tab says why, where the person can close it —
// the place itself stays in the arrangement for a build that reads it.
#[test]
fn a_place_that_does_not_read_back_keeps_its_tab_saying_why() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let state = home.state().to_path_buf();

    // The arrangement a newer build kept for that place, as this one reads it:
    // a live window it cannot open again.
    let tab = 9u64;
    let placed = Strips {
        workspaces: vec![tilo_desktop_model::own_arrangement::PlacedWorkspace {
            id: tilo_desktop_model::WorkspaceSlot(1),
            name: "Home".into(),
        }],
        rows: vec![tilo_desktop_model::own_arrangement::Row {
            id: tilo_desktop_model::StripId(7),
            workspace: tilo_desktop_model::WorkspaceSlot(1),
            name: "Writing".into(),
            shell: "so.tilo.shell.strips".into(),
        }],
        strips: vec![tilo_desktop_model::own_arrangement::Strip {
            id: 7,
            workspace: tilo_desktop_model::WorkspaceSlot(1),
            row: tilo_desktop_model::StripId(7),
            width: tilo_desktop_model::WindowWidth::Normal,
            maximized: false,
            active: Some(tab),
            canvas: None,
            shell: "so.tilo.shell.strips".into(),
            places: vec![tilo_desktop_model::own_arrangement::Place {
                id: tab,
                shows: "org.example.Notes".into(),
                view: String::new(),
                record: String::new(),
                state: serde_json::json!({"Foreign": {"label": "Notes"}}),
                title: "Notes".into(),
                pinned: false,
                shell: "so.tilo.shell.strips".into(),
            }],
        }],
        foreign: Vec::new(),
    };
    let mut doc = ui::automerge::AutoCommit::new();
    own_arrangement::write(&mut doc, &placed).expect("the arrangement");
    let path = state.join("tilo");
    std::fs::create_dir_all(&path).expect("the state directory");
    std::fs::write(path.join("arrangement.automerge"), doc.save()).expect("the record is kept");

    let mut session = session_in(Some(state));
    step(&mut session);

    // The place is still theirs: a build that reads the state opens the window
    // it kept, and closing the tab is the end of it here.
    assert!(
        arrangement(&session).strip_of(tab).is_some(),
        "the place stays in the arrangement"
    );

    // Where the window would be, its tab says why it is not here.
    let tile = session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|found| found.id == tab)
            .map(|found| found.tile.clone())
    });
    let Some(tilo_desktop_model::TileContent::LocalApp { note, .. }) = tile else {
        panic!("the place has its tab: {tile:?}");
    };
    assert_eq!(
        note,
        "Its saved window could not be read, so it is not here. Close this tab \
         when you are done with it; the app opens again next time you sign in."
    );
}
