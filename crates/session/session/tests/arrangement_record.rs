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

use tilo_compositor::InputEvent;
use tilo_session::{Options, Session};

/// The person at this device, by their root identity (hex).
const ADA: &str = "abababababababababababababababababababababababababababababababab";
/// Another member of the same workspace.
const BO: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

fn session() -> Session {
    session_in(None)
}

/// Options for a session on this device, whose workspaces it holds.
fn options(state: Option<std::path::PathBuf>) -> Options {
    Options {
        person: ADA.to_owned(),
        state: Some(state.unwrap_or_else(crate::common::seated_state)),
        collab: false,
        ..Options::default()
    }
}

/// The person at this device, as the session's own seat reads it.
fn person() -> String {
    tilo_session::identity::seated_person(&options(None))
}

/// A session of `person` 's: the device's desk is a workspace's layout, so it is one that holds
/// its workspaces.
fn session_as(seat: &str) -> Session {
    crate::common::headless(Options {
        person: seat.to_owned(),
        state: None,
        collab: false,
        ..Options::default()
    })
}

/// A session that keeps its state in `state` when it is given, else in this test's own device.
fn session_in(state: Option<std::path::PathBuf>) -> Session {
    crate::common::headless(options(state))
}

fn step(session: &mut Session) {
    session.step(Duration::from_millis(1)).expect("a frame");
}

/// Take back the person's last step, and run frames until `done`: a step is taken once the write
/// of the record it is in has landed, off the loop.
fn undone(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    session.undo_arrangement().unwrap();
    crate::common::wait_for(session, "the step to be taken back", |session| {
        done(session).then_some(())
    });
}

/// The arrangement the session's desk holds now, as a shell reads it.
fn arrangement(session: &Session) -> tilo_session::arrangement::Reading {
    session.arrangement().borrow().reading()
}

/// An id as the arrangement reads it out: its whole number.
fn number(key: &str) -> u64 {
    key.parse().expect("an id reads as its number")
}

/// The strip `tab` stands in, by its id.
fn strip_of(session: &Session, tab: u64) -> Option<u64> {
    let key = tab.to_string();
    arrangement(session)
        .places
        .iter()
        .find(|place| place.key == key)
        .map(|place| number(&place.strip))
}

/// The place `tab` is, as the arrangement reads it.
fn place_of(session: &Session, tab: u64) -> Option<tilo_session::arrangement::PlaceReading> {
    let key = tab.to_string();
    arrangement(session)
        .places
        .into_iter()
        .find(|place| place.key == key)
}

/// The strip `strip` is, as the arrangement reads it.
fn strip_at(session: &Session, strip: u64) -> Option<tilo_session::arrangement::StripReading> {
    let key = strip.to_string();
    arrangement(session)
        .strips
        .into_iter()
        .find(|held| held.key == key)
}

/// A window of `app` on the desk, its tab's id.
fn open_window(session: &mut Session, app: &str) -> u64 {
    open_window_called(session, app, app)
}

/// A window of `app` on the desk calling itself `title` , its tab's id.
fn open_window_called(session: &mut Session, app: &str, title: &str) -> u64 {
    // A table's window, opened the way the launcher opens one: a tab of the strip in front.
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
            view: String::new(),
            pinned: false,
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
    number(
        &strip_at(session, strip)
            .expect("the strip is in the record")
            .row,
    )
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
    assert_eq!(
        strip_of(&session, window).map(|strip| row_of(&session, strip)),
        Some(row),
        "the record holds the move"
    );
    let who = crate::common::last_step(&mut session, "move");
    assert_eq!(who, person(), "attributed to the person");

    // Undo puts it back.
    undone(&mut session, |session| {
        strip_of(session, window).map(|strip| row_of(session, strip)) == Some(from)
    });
}

/// The action that moves `place` to the row `row` .
fn move_to(place: u64, row: u64) -> tilo_session::arrangement::ArrangementAction {
    tilo_session::arrangement::ArrangementAction::Move {
        place,
        row,
        before: None,
        strip: None,
    }
}

/// The action that floats `place` where the person let it go.
fn float_at(
    place: u64,
    float: tilo_desktop_model::FloatPlace,
) -> tilo_session::arrangement::ArrangementAction {
    tilo_session::arrangement::ArrangementAction::Float { place, float }
}

/// The action that snaps the floating `place` back where it stands.
fn snap_back(place: u64) -> tilo_session::arrangement::ArrangementAction {
    tilo_session::arrangement::ArrangementAction::Snap {
        place,
        row: None,
        before: None,
    }
}

/// The strip holding `place` , as the arrangement reads it.
fn strip_reading_of(session: &Session, place: u64) -> tilo_session::arrangement::StripReading {
    let strip = strip_of(session, place).expect("it has a strip");
    strip_at(session, strip).expect("the strip is in the record")
}

// (6) A window let go outside its strip floats above the desk, where every member of the workspace
// sees it: one step of the person's record, taken back by undo, and snapping it back with no target
// returns it exactly where it was.
#[test]
fn a_float_is_one_step_and_snapping_back_returns_it_exactly() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let other = open_window(&mut session, "org.example.Files");
    step(&mut session);
    let before = strip_reading_of(&session, window);
    assert!(
        !before.floating,
        "it stands among the windows to begin with"
    );

    let float = tilo_desktop_model::FloatPlace::new(
        tilo_desktop_model::FloatCorner::BottomRight,
        40,
        24,
        640,
        480,
    );
    session
        .arrangement()
        .borrow_mut()
        .ask_person(&float_at(window, float))
        .unwrap();
    step(&mut session);

    let floating = strip_reading_of(&session, window);
    assert!(floating.floating, "the window floats");
    assert_eq!(floating.float_corner, "bottom-right");
    assert_eq!((floating.float_dx, floating.float_dy), (40, 24));
    assert_eq!((floating.float_width, floating.float_height), (640, 480));
    assert!(
        !strip_reading_of(&session, other).floating,
        "only the window the person let go floats"
    );
    let who = crate::common::last_step(&mut session, "float");
    assert_eq!(who, person(), "attributed to the person");

    // Undo takes the one step back, and redo floats it again.
    undone(&mut session, |session| {
        !strip_reading_of(session, window).floating
    });
    session
        .invoke_with_actor(
            &person(),
            tilo_session::arrangement::RECORD_RESOURCE,
            "redo",
            &serde_json::json!({}),
        )
        .unwrap();
    crate::common::wait_for(&mut session, "the step to be brought back", |session| {
        strip_reading_of(session, window).floating.then_some(())
    });

    // Snapping it back with no target returns it exactly where it was.
    session
        .arrangement()
        .borrow_mut()
        .ask_person(&snap_back(window))
        .unwrap();
    step(&mut session);
    let snapped = strip_reading_of(&session, window);
    assert!(!snapped.floating);
    assert_eq!(
        snapped.key, before.key,
        "the same window, in the same strip"
    );
    assert_eq!(snapped.row, before.row, "in the same row");
    assert_eq!(
        (snapped.placed, snapped.canvas_x, snapped.canvas_y),
        (before.placed, before.canvas_x, before.canvas_y),
        "exactly where it was"
    );
    crate::common::last_step(&mut session, "snap-back");
    undone(&mut session, |session| {
        strip_reading_of(session, window).floating
    });
}

// (7) A float is the workspace's, not the device's: it is written in the workspace's layout, so the
// next start shows it floating.
#[test]
fn a_float_is_in_the_workspaces_layout_so_the_next_start_shows_it() {
    let _settled = ui::motion::testing::settled();
    // A device of this machine's own, holding its workspaces: the identity is minted in its home,
    // so its engine opens and its layout is the workspace's.
    let state = crate::common::seated_state();
    let (window, strip) = {
        let mut session = session_in(Some(state.clone()));
        step(&mut session);
        let window = open_window(&mut session, "org.example.Notes");
        step(&mut session);
        let strip = strip_of(&session, window).expect("it has a strip");
        let float = tilo_desktop_model::FloatPlace::new(
            tilo_desktop_model::FloatCorner::TopLeft,
            10,
            10,
            600,
            400,
        );
        session
            .arrangement()
            .borrow_mut()
            .ask_person(&float_at(window, float))
            .unwrap();
        step(&mut session);
        (window, strip)
    };

    let mut next = session_in(Some(state));
    step(&mut next);
    let back = strip_reading_of(&next, window);
    assert!(back.floating, "the next start shows it floating");
    assert_eq!(back.float_corner, "top-left");
    assert_eq!(
        (
            back.float_dx,
            back.float_dy,
            back.float_width,
            back.float_height
        ),
        (10, 10, 600, 400)
    );
    assert_eq!(strip_of(&next, window), Some(strip));
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

// (2) An agent moves a window through the published action, attributed to the agent and undoable.
#[test]
fn an_agents_move_through_the_action_is_attributed_and_undone() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let second = add_strip(&session, "Second");
    step(&mut session);
    let from = strip_of(&session, window).map(|strip| row_of(&session, strip));
    let from = from.expect("it has a strip");

    // The place's own resource publishes the actions a press names it by, so the catalogue,
    // tilo-control and an agent see them.
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
        strip_of(&session, window).map(|strip| row_of(&session, strip)),
        Some(second),
        "the agent's move is in the record"
    );
    let who = crate::common::last_step(&mut session, "move");
    assert_eq!(who, "agent-task:reading", "attributed to the agent");

    undone(&mut session, |session| {
        strip_of(session, window).map(|strip| row_of(session, strip)) == Some(from)
    });
}

// (3) One record answers what the arrangement is: the desk's own, which the workspace shares.
// Nothing is kept beside it, and another member of the workspace reads none of it.
#[test]
fn the_arrangement_is_the_desks_own_and_no_record_beside_it() {
    let _settled = ui::motion::testing::settled();
    // A device of this machine's own, holding its workspaces: the identity is minted in its home,
    // so its engine opens and its layout is the workspace's.
    let state = crate::common::seated_state();
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

    // The workspace's layout record keeps the arrangement; what this device keeps is what it
    // shows, and no record of the arrangement's own is written beside either.
    assert!(
        state.join("tilo").join("shown.json").exists(),
        "the device keeps what it shows"
    );
    assert!(
        !state.join("tilo").join("arrangement.automerge").exists(),
        "no second record of the arrangement is kept"
    );

    // Another member of the workspace runs a desk of their own: nothing of this one's arrangement
    // is theirs.
    let mut theirs = session_as(BO);
    step(&mut theirs);
    assert!(
        place_of(&theirs, window).is_none(),
        "nothing of this person's arrangement is theirs"
    );
}

// (5) What a place shows and whether it is pinned are the window's own, so they survive a restart
// with the desk — and nothing is kept beside it.
#[test]
fn a_places_view_and_pin_are_kept_with_the_desk_across_a_restart() {
    let _settled = ui::motion::testing::settled();
    // A device of this machine's own, holding its workspaces: the identity is minted in its home,
    // so its engine opens and its layout is the workspace's.
    let state = crate::common::seated_state();
    let place = {
        let mut session = session_in(Some(state.clone()));
        step(&mut session);
        let window = open_window(&mut session, "org.example.Notes");
        step(&mut session);
        // A shell showing the window through a view sets it on the window.
        let handle = session.handle().clone();
        let mut tree = handle.read(Clone::clone);
        for tab in tree
            .workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.strips)
            .flat_map(|strip| &mut strip.window_groups)
            .flat_map(|group| &mut group.tabs)
            .filter(|tab| tab.id == window)
        {
            tab.view = "editor".into();
        }
        handle.replace_tree(tree);
        invoke_as(
            &mut session,
            &place_resource(window),
            "pin",
            &serde_json::json!({"pinned": true}),
        );
        let held = place_of(&session, window).expect("the place is in the arrangement");
        assert_eq!(
            held.shown_as, "editor",
            "the arrangement holds the view it shows"
        );
        assert!(held.pinned, "and the pin on it");
        window
    };

    let mut next = session_in(Some(state));
    step(&mut next);
    let back = place_of(&next, place).expect("the place came back");
    assert_eq!(back.shown_as, "editor", "the view the place shows");
    assert!(back.pinned, "the place is still pinned");
}

// A place a launch left on the desk and no window came for is not a window the person had open: the
// next start does not bring it back, and the desk is exactly what was open.
#[test]
fn a_launch_that_left_no_window_is_gone_after_a_restart() {
    let _settled = ui::motion::testing::settled();
    // A device of this machine's own, holding its workspaces: the identity is minted in its home,
    // so its engine opens and its layout is the workspace's.
    let state = crate::common::seated_state();
    let (window, place) = {
        let mut session = session_in(Some(state.clone()));
        step(&mut session);
        let window = open_window(&mut session, "org.example.Notes");
        // A launch of Files opened its place, and no window came for it.
        let handle = session.handle().clone();
        let mut tree = handle.read(Clone::clone);
        let (tab, group) = (handle.mint_id(), handle.mint_id());
        let workspace = tree
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.slot == tree.active)
            .expect("a workspace is in front");
        workspace
            .strips
            .first_mut()
            .expect("a strip is in front")
            .window_groups
            .push(tilo_desktop_model::WindowGroup {
                id: group,
                tabs: vec![tilo_desktop_model::Tab {
                    id: tab,
                    title: "Files".into(),
                    tile: tilo_desktop_model::TileContent::AppPlaceholder {
                        kind: tilo_desktop_model::AppKind::Other("org.example.Files".into()),
                        label: "org.example.Files".into(),
                    },
                    app_title: None,
                    view: String::new(),
                    pinned: false,
                }],
                active_tab: tab,
                width: tilo_desktop_model::WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
        handle.replace_tree(tree);
        step(&mut session);
        (window, tab)
    };

    let mut next = session_in(Some(state));
    step(&mut next);
    let tabs: Vec<u64> = next.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect()
    });
    assert!(
        !tabs.contains(&place),
        "the place no window came for is not on the desk again: {tabs:?}"
    );
    assert!(
        tabs.contains(&window),
        "the window that was open is: {tabs:?}"
    );
}

// (4) While the person types or drags, an agent's move waits, not drops, and applies after the
// gesture ends (shell.md §1).
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
        strip_of(&session, window).map(|strip| row_of(&session, strip)),
        Some(second),
        "it applies once the person is done"
    );
    let who = crate::common::last_step(&mut session, "move");
    assert_eq!(who, "agent-task:reading", "still attributed to the agent");
}

// An agent's arrangement leaves a place the person pinned where it is (shell.md §5's default).
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
    assert!(place_of(&session, window).unwrap().pinned);

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

// A place reads out named the way every surface shows it: the window's own title and its app's own
// name, never an app id, and the app's name alone for a window that has no title.
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

// Moving a place to another row through the published action is a place the reading then shows in
// that row, and undo puts it back — the same both ways. Moving a place into another row's empty
// strip makes a strip there; undo takes the place back to the strip it came from.
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

    undone(&mut session, |session| {
        let reading = session.arrangement().borrow().reading();
        reading
            .places
            .iter()
            .find(|place| place.key == key.as_str())
            .and_then(|place| reading.strips.iter().find(|strip| strip.key == place.strip))
            .is_some_and(|strip| strip.row == was.to_string())
    });
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
    let was = strip_at(&session, strip)
        .map(|strip| strip.width)
        .expect("the strip is in the record");

    invoke_as(
        &mut session,
        &strip_resource(strip),
        "resize",
        &serde_json::json!({"permille": 333}),
    );
    let now = strip_at(&session, strip)
        .map(|strip| strip.width)
        .expect("the strip is in the record");
    assert_ne!(now, was.max(333), "the width changed");
    assert_eq!(now, 333, "the strip stands narrow");

    undone(&mut session, |session| {
        strip_at(session, strip).map(|strip| strip.width) == Some(was)
    });
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
        strip_at(session, strip)
            .map(|strip| strip.maximized)
            .expect("the strip is in the record")
    };
    assert!(maximized(&session, strip), "the window fills the desk");
    assert!(
        !maximized(&session, other_strip),
        "and no other window does"
    );

    undone(&mut session, |session| !maximized(session, strip));
}

/// A session on a device whose salt is the top of an id's high bits: every place, strip and row
/// here is above `i64::MAX` , so no id fits the integer an id-shaped verb would take.
fn salted_session() -> Session {
    crate::common::headless(Options {
        person: ADA.to_owned(),
        state: None,
        collab: false,
        salt: 0x8000,
        ..Options::default()
    })
}

// Every arranging press names the place, strip or row it presses, so on a device whose ids do not
// fit an integer each of them still reaches the record's own entry: pin, close, move to a row and a
// strip's width all land on the thing they name, and on nothing else.
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

    // Each press names its own resource, by the whole of the id: pin reaches this place, and the
    // other place stands unpinned.
    invoke_as(
        &mut session,
        &place_resource(window),
        "pin",
        &serde_json::json!({"pinned": true}),
    );
    assert_eq!(
        place_of(&session, window).map(|place| place.pinned),
        Some(true),
        "pin reached the place it named"
    );
    assert_eq!(
        place_of(&session, other).map(|place| place.pinned),
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
        strip_at(&session, strip).map(|strip| strip.width),
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
        strip_of(&session, window).map(|strip| row_of(&session, strip)),
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
