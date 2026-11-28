//! The layout is the layout's own record (shell.md §2, workspace-model.md
//! §9): what is open where, kept with the layout the workspace shares, with
//! history and undo. No second record of it exists, and what the layout's windows
//! do not carry — the view a place shows and whether it is pinned — is kept
//! with the layout too.
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
use tilo_session_core::layout;

use tilo_compositor::InputEvent;
use tilo_session::{Options, Session};

/// The person a session of a plain device, one no identity names, starts as.
const ADA: &str = "abababababababababababababababababababababababababababababababab";

thread_local! {
    /// This computer: its device daemon, holding its workspaces, and the home it runs on. A layout
    /// is a workspace's own record, which the daemon keeps and signs, so a session reaches its
    /// layout only as its daemon's shell; two sessions of one of these are the same computer
    /// started twice.
    static DEVICE: crate::common::Device =
        crate::common::device_daemon(crate::common::device_home(), None);
    /// Where a session of this computer that is handed no state keeps its own services' files.
    static STATE: tempfile::TempDir = tempfile::tempdir().expect("a state directory");
}

fn session() -> Session {
    session_in(None)
}

/// The person at this device, as the session's own identity reads it: the daemon's person.
fn person() -> String {
    DEVICE.with(|device| device.person.clone())
}

/// A session on a computer of someone else's: its layouts are the layouts of the workspaces it
/// holds, and it holds none of this one's.
fn another_computer() -> Session {
    crate::common::on_its_daemon(
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
        Options {
            collab: false,
            ..Options::default()
        },
    )
}

/// A session on this computer that keeps its state in `state` when it is given, else in this
/// computer's own.
fn session_in(state: Option<std::path::PathBuf>) -> Session {
    let state = state.unwrap_or_else(|| STATE.with(|state| state.path().to_path_buf()));
    DEVICE.with(|device| device.session(&state))
}

fn step(session: &mut Session) {
    session.step(Duration::from_millis(1)).expect("a frame");
}

/// Take back the person's last step, and run frames until `done`: a step is taken once the write
/// of the record it is in has landed, off the loop.
fn undone(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    session.undo_layout().unwrap();
    crate::common::wait_for(session, "the step to be taken back", |session| {
        done(session).then_some(())
    });
}

/// The layout the session's layout holds now, as a shell reads it.
fn layout(session: &Session) -> layout::Reading {
    session.layout().borrow().reading()
}

/// An id as the layout reads it out: its whole number.
fn number(key: &str) -> u64 {
    key.parse().expect("an id reads as its number")
}

/// The strip `tab` stands in, by its id.
fn strip_of(session: &Session, tab: u64) -> Option<u64> {
    let key = tab.to_string();
    layout(session)
        .places
        .iter()
        .find(|place| place.key == key)
        .map(|place| number(&place.strip))
}

/// The place `tab` is, as the layout reads it.
fn place_of(session: &Session, tab: u64) -> Option<layout::PlaceReading> {
    let key = tab.to_string();
    layout(session)
        .places
        .into_iter()
        .find(|place| place.key == key)
}

/// The strip `strip` is, as the layout reads it.
fn strip_at(session: &Session, strip: u64) -> Option<layout::StripReading> {
    let key = strip.to_string();
    layout(session)
        .strips
        .into_iter()
        .find(|held| held.key == key)
}

/// A window of `app` on the shell, its tab's id.
fn open_window(session: &mut Session, app: &str) -> u64 {
    open_window_called(session, app, app)
}

/// A window of `app` on the shell calling itself `title` , its tab's id.
fn open_window_called(session: &mut Session, app: &str, title: &str) -> u64 {
    // A table's window, opened the way the launcher opens one: a tab of the strip in front.
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    let tab = handle.mint_id();
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id() == tree.active)
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

/// The row `strip` (a column of places) stands in, in the layout.
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
    let _settled = tilo_ui_base::motion::testing::settled();
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

    // The drag's end commits one layout action, as the person's.
    session
        .layout()
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
    assert_eq!(
        who,
        crate::common::as_person(&person()),
        "attributed to the person"
    );

    // Undo puts it back.
    undone(&mut session, |session| {
        strip_of(session, window).map(|strip| row_of(session, strip)) == Some(from)
    });
}

/// The action that moves `place` to the row `row` .
fn move_to(place: u64, row: u64) -> layout::LayoutAction {
    layout::LayoutAction::Move {
        place,
        row,
        before: None,
        strip: None,
        window: None,
    }
}

/// What the shell calls the window of the place `tab`: the name its bar and its pill say — the
/// place's own name, else its title, else what it shows.
fn window_name(session: &Session, tab: u64) -> String {
    let place = place_of(session, tab).expect("the place is in the record");
    if !place.name.is_empty() {
        place.name
    } else if !place.title.is_empty() {
        place.title
    } else if !place.shows.is_empty() {
        place.shows
    } else {
        "Window".to_owned()
    }
}

/// The bar of the window named `name`, as the shell lays it on screen: the widest control across
/// the window's top, never the whole frame the same name also names nor a corner's mark.
fn bar_of(session: &mut Session, name: &str) -> (f64, f64, f64, f64) {
    crate::common::wait_for(session, "the window's bar", |session| {
        session
            .shell_controls(name)
            .into_iter()
            .filter(|rect| rect.3 < 100.0)
            .max_by(|a, b| a.2.total_cmp(&b.2))
    })
}

/// Where a hand takes the bar: its middle across, near its top edge, above the tabs it shows and
/// on no window control.
fn bar_grip((x, y, w, _h): (f64, f64, f64, f64)) -> (f64, f64) {
    (x + w / 2.0, y + 14.0)
}

/// The places standing in the strip `strip`, in the record's order.
fn places_in(session: &Session, strip: u64) -> Vec<u64> {
    let key = strip.to_string();
    layout(session)
        .places
        .into_iter()
        .filter(|place| place.strip == key)
        .map(|place| number(&place.key))
        .collect()
}

// (2) A tab carried out of its window's bar and let go over another window's bar is a tab of that
// window's: one move of the person's record, which one Undo takes back, leaving each window what
// it held.
#[test]
fn a_tab_dragged_onto_another_window_lands_and_one_undo_puts_it_back() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let first = open_window(&mut session, "org.example.Notes");
    let second = open_window(&mut session, "org.example.Files");
    let first_strip = strip_of(&session, first).expect("the first place stands in a strip");
    let second_strip = strip_of(&session, second).expect("the second place stands in a strip");
    assert_ne!(
        first_strip, second_strip,
        "each window starts in one of its own"
    );
    let first_name = window_name(&session, first);
    let second_name = window_name(&session, second);
    // The first window's bar is pressed and dragged onto the second window's bar, where the second
    // window's drop target takes the place it carries.
    let from = bar_grip(bar_of(&mut session, &first_name));
    let to = bar_grip(bar_of(&mut session, &second_name));
    crate::common::drag(&mut session, from, to);
    crate::common::wait_for(&mut session, "the tab landed", |session| {
        (strip_of(session, first) == Some(second_strip)).then_some(())
    });
    assert_eq!(
        places_in(&session, second_strip),
        [second, first],
        "the tab moved in lands after the window's own tab"
    );
    let who = crate::common::last_step(&mut session, "move");
    assert_eq!(
        who,
        crate::common::as_person(&person()),
        "attributed to the person"
    );
    // One Undo takes the drop back: the tab is where it stood, in the window it came from, and the
    // window it was let go over holds its own place alone.
    undone(&mut session, |session| {
        strip_of(session, first) == Some(first_strip)
    });
    assert_eq!(places_in(&session, first_strip), [first]);
    assert_eq!(places_in(&session, second_strip), [second]);
}

/// A drop that names no place the layout holds is refused in words, never a silence that did
/// nothing: the shell's gap drop runs its move whatever it carries, so the case is what the
/// session answers for the address such a drop sends.
#[test]
fn a_move_naming_no_place_the_layout_holds_is_refused_in_words() {
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let strip = strip_of(&session, window).expect("the place stands in a strip");
    let arguments = serde_json::json!({
        "row": serde_json::Value::String(format!("shell:row:{}", row_of(&session, strip))),
        "strip": serde_json::Value::Null,
        "before": serde_json::Value::Null,
        "window": true,
    })
    .as_object()
    .expect("arguments")
    .clone();
    // `the_place("")` is the place a drop sends when the carried thing names none: the session
    // refuses the call, and the refusal names the address it could not reach.
    let refused = session
        .invoke("shell:place:", "move", &arguments)
        .expect_err("a place the layout does not hold is not moved");
    let said = format!("{refused:?}");
    assert!(
        said.contains("shell:place:"),
        "the refusal names what it could not reach: {said}"
    );
}

/// The action that floats `place` where the person let it go.
fn float_at(place: u64, float: tilo_desktop_model::FloatPlace) -> layout::LayoutAction {
    layout::LayoutAction::Float { place, float }
}

/// The action that snaps the floating `place` back where it stands.
fn snap_back(place: u64) -> layout::LayoutAction {
    layout::LayoutAction::Snap {
        place,
        row: None,
        before: None,
    }
}

/// The strip holding `place` , as the layout reads it.
fn strip_reading_of(session: &Session, place: u64) -> layout::StripReading {
    let strip = strip_of(session, place).expect("it has a strip");
    strip_at(session, strip).expect("the strip is in the record")
}

/// Where the window `tab` floats, as the shell's record keeps it; `None` when it stands in its
/// strip.
fn float_of(session: &Session, tab: u64) -> Option<tilo_desktop_model::FloatPlace> {
    session.handle().read(|tree| {
        tree.workspaces.iter().find_map(|workspace| {
            let group = workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .find(|group| group.tabs.iter().any(|shown| shown.id == tab))?;
            workspace.layout.float_of(group.id)
        })
    })
}

// (6) A window let go outside its strip floats above the shell, where every member of the workspace
// sees it: one step of the person's record, taken back by undo, and snapping it back with no target
// returns it exactly where it was.
#[test]
fn a_float_is_one_step_and_snapping_back_returns_it_exactly() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
        .layout()
        .borrow_mut()
        .ask_person(&float_at(window, float))
        .unwrap();
    step(&mut session);

    let floating = strip_reading_of(&session, window);
    assert!(floating.floating, "the window floats");
    let place = float_of(&session, window).expect("the record keeps where it floats");
    assert_eq!(place.corner, tilo_desktop_model::FloatCorner::BottomRight);
    assert_eq!((place.dx, place.dy), (40, 24));
    assert_eq!((place.width, place.height), (640, 480));
    assert!(
        !strip_reading_of(&session, other).floating,
        "only the window the person let go floats"
    );
    let who = crate::common::last_step(&mut session, "float");
    assert_eq!(
        who,
        crate::common::as_person(&person()),
        "attributed to the person"
    );

    // Undo takes the one step back, and redo floats it again.
    undone(&mut session, |session| {
        !strip_reading_of(session, window).floating
    });
    session
        .invoke_with_actor(
            &crate::common::as_person(&person()),
            layout::HISTORY_RESOURCE,
            "redo",
            &serde_json::json!({}),
        )
        .unwrap();
    crate::common::wait_for(&mut session, "the step to be brought back", |session| {
        strip_reading_of(session, window).floating.then_some(())
    });

    // Snapping it back with no target returns it exactly where it was.
    session
        .layout()
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
    let _settled = tilo_ui_base::motion::testing::settled();
    // This computer's device, holding its workspaces: its sessions are its daemon's shells, and
    // the layout they show is the workspace's.
    let state = tempfile::tempdir().expect("a state directory");
    let (window, strip) = {
        let mut session = session_in(Some(state.path().to_path_buf()));
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
            .layout()
            .borrow_mut()
            .ask_person(&float_at(window, float))
            .unwrap();
        step(&mut session);
        (window, strip)
    };

    let mut next = session_in(Some(state.path().to_path_buf()));
    step(&mut next);
    let back = strip_reading_of(&next, window);
    assert!(back.floating, "the next start shows it floating");
    let place = float_of(&next, window).expect("the record keeps where it floats");
    assert_eq!(place.corner, tilo_desktop_model::FloatCorner::TopLeft);
    assert_eq!(
        (place.dx, place.dy, place.width, place.height),
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
    layout::Item::Place.resource(&key.to_string())
}

/// The resource the session publishes the row `key` as: what a place's move names.
fn row_resource(key: u64) -> String {
    layout::Item::Row.resource(&key.to_string())
}

/// The resource the session publishes the strip `key` as: what its own resize is pressed on.
fn strip_resource(key: u64) -> String {
    layout::Item::Strip.resource(&key.to_string())
}

// (2) An agent moves a window through the published action, attributed to the agent and undoable.
#[test]
fn an_agents_move_through_the_action_is_attributed_and_undone() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
            &crate::common::agent("reading"),
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
    assert_eq!(
        who,
        crate::common::agent("reading"),
        "attributed to the agent"
    );

    undone(&mut session, |session| {
        strip_of(session, window).map(|strip| row_of(session, strip)) == Some(from)
    });
}

// (3) One record answers what the layout is: the layout's own, which the workspace shares.
// Nothing is kept beside it, and a computer that holds none of those workspaces reads none of it.
#[test]
fn the_layout_is_the_shells_own_and_no_record_beside_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // This computer's device, holding its workspaces: its sessions are its daemon's shells, and
    // the layout they show is the workspace's.
    let state = tempfile::tempdir().expect("a state directory");
    let mut here = session_in(Some(state.path().to_path_buf()));
    step(&mut here);
    let window = open_window(&mut here, "org.example.Notes");
    let second = add_strip(&here, "Second");
    step(&mut here);
    here.layout()
        .borrow_mut()
        .ask_person(&move_to(window, second))
        .unwrap();
    step(&mut here);
    assert_eq!(
        strip_of(&here, window).map(|strip| row_of(&here, strip)),
        Some(second),
        "the layout holds the move"
    );
    // A session keeps the layout as it ends, and its link to the daemon ends with it: the records
    // are read through the session that follows.
    drop(here);
    let after = session_in(Some(state.path().to_path_buf()));
    let records = after
        .device_records()
        .expect("the session keeps the device's records");

    // The workspace's layout record keeps the layout; what this device keeps is what it
    // shows, and no record of the layout's own is written beside either.
    assert!(
        crate::common::kept(&records, "so.tilo.session-shown").is_some(),
        "the device keeps what it shows"
    );
    assert!(
        !state.path().join("tilo").join("layout.automerge").exists(),
        "no second record of the layout is kept"
    );

    // A computer of someone else's runs the layouts of their own workspaces: nothing of this
    // one's layout is theirs.
    let mut theirs = another_computer();
    step(&mut theirs);
    assert!(
        place_of(&theirs, window).is_none(),
        "nothing of this person's layout is theirs"
    );
}

// (5) What a place shows and whether it is pinned are the window's own, so they survive a restart
// with the layout — and nothing is kept beside it.
#[test]
fn a_places_view_and_pin_are_kept_with_the_shell_across_a_restart() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // This computer's device, holding its workspaces: its sessions are its daemon's shells, and
    // the layout they show is the workspace's.
    let state = tempfile::tempdir().expect("a state directory");
    let place = {
        let mut session = session_in(Some(state.path().to_path_buf()));
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
        let held = place_of(&session, window).expect("the place is in the layout");
        assert_eq!(
            held.shown_as, "editor",
            "the layout holds the view it shows"
        );
        assert!(held.pinned, "and the pin on it");
        window
    };

    let mut next = session_in(Some(state.path().to_path_buf()));
    // The layout is the workspace's, read off the loop once the daemon answers: the place comes
    // back when the session that follows has read what the one before left there.
    let back = crate::common::wait_for(&mut next, "the place to come back", |session| {
        place_of(session, place)
    });
    assert_eq!(back.shown_as, "editor", "the view the place shows");
    assert!(back.pinned, "the place is still pinned");
}

// A place a launch left on the shell and no window came for is not a window the person had open: the
// next start does not bring it back, and the layout is exactly what was open.
#[test]
fn a_launch_that_left_no_window_is_gone_after_a_restart() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // This computer's device, holding its workspaces: its sessions are its daemon's shells, and
    // the layout they show is the workspace's.
    let state = tempfile::tempdir().expect("a state directory");
    let (window, place) = {
        let mut session = session_in(Some(state.path().to_path_buf()));
        step(&mut session);
        let window = open_window(&mut session, "org.example.Notes");
        // A launch of Files opened its place, and no window came for it.
        let handle = session.handle().clone();
        let mut tree = handle.read(Clone::clone);
        let (tab, group) = (handle.mint_id(), handle.mint_id());
        let workspace = tree
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id() == tree.active)
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

    let mut next = session_in(Some(state.path().to_path_buf()));
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
        "the place no window came for is not on the layout again: {tabs:?}"
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
            &crate::common::agent("reading"),
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
        session.layout().borrow().waiting(),
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
    while session.layout().borrow().waiting() > 0 {
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
    assert_eq!(
        who,
        crate::common::agent("reading"),
        "still attributed to the agent"
    );
}

// An agent's layout leaves a place the person pinned where it is (shell.md §5's default).
#[test]
fn a_pinned_place_stays_where_the_person_put_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
        &crate::common::agent("reading"),
        &place_resource(window),
        "move",
        &serde_json::json!({"row": row_resource(second)}),
    );
    assert!(refused.is_err(), "the agent may not move a pinned place");
    assert_eq!(strip_of(&session, window), Some(from));
}

// A pin the person puts on a place through its published action is a step of the layout's
// history: undo takes it back.
#[test]
fn a_pin_through_the_published_action_is_undone() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    step(&mut session);
    invoke_as(
        &mut session,
        &place_resource(window),
        "pin",
        &serde_json::json!({"pinned": true}),
    );
    assert!(place_of(&session, window).unwrap().pinned);
    undone(&mut session, |session| {
        place_of(session, window).is_some_and(|place| !place.pinned)
    });
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

// The layout as the shell draws it: every workspace, row, strip and place, by key, as the
// session publishes it for the shell's views. Which way it is shown is not the layout's.
#[test]
fn the_layout_reads_out_by_key_for_the_shells_views() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    step(&mut session);
    let reading = session.layout().borrow().reading();
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
        "the layout shows its workspaces"
    );
}

// A place reads out named the way every surface shows it: the window's own title and its app's own
// name, never an app id, and the app's name alone for a window that has no title.
#[test]
fn a_place_reads_out_named_by_its_window_and_its_app_never_an_id() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window_called(&mut session, "org.example.TextEditor", "Groceries");
    step(&mut session);
    let reading = session.layout().borrow().reading();
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
    let reading = session.layout().borrow().reading();
    let place = reading
        .places
        .iter()
        .find(|place| place.key == untitled.to_string())
        .expect("the untitled window reads out");
    assert_eq!(
        place.name, "Text Editor",
        "a window with no title of its own is its app alone"
    );

    // A window that titles itself with its app's own name under another spelling — the slug an
    // app reports before its content names a place — is the app alone, never the name joined to
    // its slug.
    let slug = open_window_called(&mut session, "org.example.TextEditor", "text editor");
    step(&mut session);
    let reading = session.layout().borrow().reading();
    let place = reading
        .places
        .iter()
        .find(|place| place.key == slug.to_string())
        .expect("the slug-titled window reads out");
    assert_eq!(
        place.name, "Text Editor",
        "a window naming its app says its name once: {}",
        place.name
    );
}

// Moving a place to another row through the published action is a place the reading then shows in
// that row, and undo puts it back — the same both ways. Moving a place into another row's empty
// strip makes a strip there; undo takes the place back to the strip it came from.
#[test]
fn a_move_the_reading_shows_is_undone_to_where_it_was() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
    let reading = session.layout().borrow().reading();
    let strip = reading
        .places
        .iter()
        .find(|place| place.key == key.as_str())
        .and_then(|place| reading.strips.iter().find(|strip| strip.key == place.strip))
        .expect("the place's strip reads out");
    assert_eq!(strip.row, second.to_string(), "the reading holds the move");

    undone(&mut session, |session| {
        let reading = session.layout().borrow().reading();
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
    let _settled = tilo_ui_base::motion::testing::settled();
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

// The person changes a strip's width twice while their view cannot be written: each change is a
// step of its own, so undo takes back the second and then the first.
#[test]
fn two_width_changes_before_the_view_is_written_are_two_steps() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let strip = strip_of(&session, window).expect("it has a strip");
    let was = strip_at(&session, strip)
        .map(|strip| strip.width)
        .expect("the strip is in the layout");
    for permille in [333, 444] {
        session
            .layout()
            .borrow_mut()
            .ask_person(&layout::LayoutAction::Resize { strip, permille })
            .expect("the width changes");
    }
    assert_eq!(
        strip_at(&session, strip).map(|strip| strip.width),
        Some(444),
        "the layout shows the second change"
    );

    undone(&mut session, |session| {
        strip_at(session, strip).map(|strip| strip.width) == Some(333)
    });
    undone(&mut session, |session| {
        strip_at(session, strip).map(|strip| strip.width) == Some(was)
    });
}

// A window is maximized through the published action, so the layout shows the one window it is,
// and one undo brings the others back beside it.
#[test]
fn maximizing_a_window_is_one_step_and_undone() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let other = open_window_called(&mut session, "org.example.Notes", "Other");
    step(&mut session);
    let strip = strip_of(&session, window).expect("it has a strip");
    let other_strip = strip_of(&session, other).expect("the other place is in a strip");
    assert!(
        layout(&session).strips.iter().all(|strip| !strip.maximized),
        "the layout starts beside itself"
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
    assert!(maximized(&session, strip), "the window fills the layout");
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = salted_session();
    step(&mut session);
    let window = open_window(&mut session, "org.example.Notes");
    let other = open_window_called(&mut session, "org.example.Notes", "Other");
    assert!(
        window > u64::try_from(i64::MAX).unwrap() && other > window,
        "the layout's ids do not fit an integer: {window}, {other}"
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

    // Close names its place too: only that place goes. The shell keeps a window that left the
    // layout in its reading while its frame flies back into its control, so the place close named
    // is the one leaving, and the window it did not name reads live: once the departure has
    // played, only the other one is there.
    invoke_as(
        &mut session,
        &place_resource(window),
        "close",
        &serde_json::json!({}),
    );
    assert!(
        place_of(&session, window).is_some_and(|place| place.leaving),
        "close reached the place it named: it is the window on its way out"
    );
    assert!(
        place_of(&session, other).is_some_and(|place| !place.leaving),
        "and closed no other place: the other window stands"
    );
    crate::common::wait_for(&mut session, "the closed place to go", |session| {
        place_of(session, window).is_none().then_some(())
    });
    assert_eq!(
        strip_of(&session, other),
        Some(other_strip),
        "the other place stayed in its own strip"
    );
}
