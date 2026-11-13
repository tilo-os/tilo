//! The sheet takes the person's input, whatever window is focused: its field is typed into rather
//! than the window behind it, Escape and a click outside close it, a click inside reaches its own
//! controls, and a sheet the session closed stops being drawn.

use std::time::Duration;

use tilo_session::Session;
use tilo_sheets::Seat as _;

use super::{
    CLUB_WORKSPACE, ask_to_join, club_entry, community_entry, community_site, joining_site, now_ms,
    open_directory, page_of, session, site, this_computer,
};
use crate::common::window::{Client, KEY_ESCAPE, click, tap_key};

/// The session with the directory's sheet up, and a client window focused behind it.
fn sheet_over_a_window() -> (Session, Client) {
    let mut env = tilo_test_env::ScopedEnv::new();
    let (responder, page) = site();
    let _config = community_site(&mut env, &responder.url());
    *page.lock().expect("the page") =
        page_of(vec![community_entry(34, now_ms()), club_entry(now_ms())]);
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let mut client = Client::connect(session.compositor());
    client.pump(&mut session, |_, seen| {
        seen.wm_base.is_some() && seen.shm.is_some() && seen.seat.is_some()
    });
    client.map_plain(&mut session, "org.example.Terminal", (640, 400));
    client.pump(&mut session, |_, seen| seen.keyboard_focused);
    open_directory(&mut session);
    (session, client)
}

/// With a client window focused behind it, typing reaches the sheet's search field and never the
/// window: the list narrows to what was typed.
#[test]
fn typing_reaches_the_sheets_field_and_not_the_window_behind_it() {
    let _settled = ui::motion::testing::settled();
    let (mut session, mut client) = sheet_over_a_window();
    let seen_before = client.seen.keys.clone();
    // "salon" is on no listing, so the list says nothing matches.
    for keycode in [31u32, 30, 38, 24, 49] {
        tap_key(&mut session, keycode);
        client.exchange();
    }
    crate::common::until(
        &mut session,
        "the search field to hold the typing",
        |session| {
            client.exchange();
            session.step(Duration::from_millis(2)).expect("step");
            client.exchange();
            session
                .workspace_directory()
                .is_some_and(|seat| seat.query() == "salon")
        },
    );
    assert_eq!(
        client.seen.keys, seen_before,
        "the window behind the sheet saw none of what was typed"
    );
    assert!(
        session
            .accessible_labels()
            .iter()
            .any(|label| label.contains("salon")),
        "the sheet says what was typed finds nothing: {:?}",
        session.accessible_labels()
    );
}

/// Escape closes the sheet while a client window is focused behind it, and the window never sees
/// the key.
#[test]
fn escape_closes_the_sheet_and_never_reaches_the_window() {
    let _settled = ui::motion::testing::settled();
    let (mut session, mut client) = sheet_over_a_window();
    assert!(
        session
            .workspace_directory()
            .is_some_and(|seat| seat.presentation().is_open()),
        "the sheet is up to begin with"
    );
    let seen_before = client.seen.keys.clone();
    tap_key(&mut session, KEY_ESCAPE);
    crate::common::until(&mut session, "the sheet to close on Escape", |session| {
        client.exchange();
        session.step(Duration::from_millis(2)).expect("step");
        client.exchange();
        session
            .workspace_directory()
            .is_some_and(|seat| !seat.presentation().is_open())
    });
    assert_eq!(
        client.seen.keys, seen_before,
        "the window behind the sheet saw none of the Escape"
    );
    assert!(client.seen.keyboard_focused, "the window kept the seat");
}

/// A press outside the sheet's card is the sheet's, not the window's behind it: it closes the sheet
/// and the window sees no button.
#[test]
fn a_click_outside_the_sheet_never_reaches_the_window() {
    let _settled = ui::motion::testing::settled();
    let (mut session, mut client) = sheet_over_a_window();
    let buttons_before = client.seen.buttons.clone();
    // The sheet covers the whole display; a press at its corner is beside the card and closes the
    // sheet.
    click(&mut session, 1.0, 1.0);
    crate::common::until(
        &mut session,
        "the press beside the card to close the sheet",
        |session| {
            client.exchange();
            session.step(Duration::from_millis(2)).expect("step");
            client.exchange();
            session
                .workspace_directory()
                .is_some_and(|seat| !seat.presentation().is_open())
        },
    );
    assert_eq!(
        client.seen.buttons, buttons_before,
        "the window behind the sheet saw none of the press"
    );
}

/// A press inside the sheet's card reaches its controls: the search field takes the caret, and
/// typing after it narrows the list.
#[test]
fn a_click_inside_the_sheet_reaches_its_controls() {
    let _settled = ui::motion::testing::settled();
    let (mut session, mut client) = sheet_over_a_window();
    // The sheet publishes its field as the screen reader reads it: the shared search field, named
    // by the words it stands for.
    let field = crate::common::wait_for(&mut session, "the sheet's search field", |session| {
        session
            .step(Duration::from_millis(16))
            .expect("a turn of the session");
        sheet_control(session, "Search public workspaces, or paste a link or code")
    });
    let buttons_before = client.seen.buttons.clone();
    click(
        &mut session,
        field.0 + field.2 / 2.0,
        field.1 + field.3 / 2.0,
    );
    tap_key(&mut session, 30);
    client.exchange();
    crate::common::until(
        &mut session,
        "the field to take the typed letter",
        |session| {
            client.exchange();
            session.step(Duration::from_millis(2)).expect("step");
            client.exchange();
            session
                .workspace_directory()
                .is_some_and(|seat| seat.query() == "a")
        },
    );
    assert_eq!(
        client.seen.buttons, buttons_before,
        "the press inside the card was the sheet's"
    );
}

/// Where the sheet's control named `name` stands, as the session's own export publishes it: the
/// window the sheet's app is on, and the node it names there, in logical pixels.
fn sheet_control(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    for (_, _, tree) in session.a11y_windows() {
        let found = tree.nodes.iter().find_map(|(_, node)| {
            let node_name = node.label()?;
            (node_name == name).then(|| node.bounds()).flatten()
        });
        if let Some(bounds) = found {
            return Some((
                bounds.x0,
                bounds.y0,
                bounds.x1 - bounds.x0,
                bounds.y1 - bounds.y0,
            ));
        }
    }
    None
}

/// Choosing Join closes the sheet once its action has started, leaving the person where they are
/// rather than a sheet behind.
#[test]
fn choosing_join_closes_the_sheet() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, _person) = this_computer(&mut env);
    let responder = joining_site(vec![community_entry(34, now_ms()), club_entry(now_ms())]);
    let _config = community_site(&mut env, &responder.url());
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    ask_to_join(&mut session, CLUB_WORKSPACE);
    crate::common::until(
        &mut session,
        "the sheet to close once the join started",
        |session| {
            session.step(Duration::from_millis(2)).expect("step");
            session
                .workspace_directory()
                .is_some_and(|seat| !seat.presentation().is_drawn())
        },
    );
}

/// A sheet the session closed stops being drawn: nothing is left on screen taking no input.
#[test]
fn a_sheet_the_session_closed_stops_being_drawn() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let _config = community_site(&mut env, "http://127.0.0.1:1/");
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    let seat = session
        .workspace_directory()
        .expect("the session hosts the workspace directory")
        .clone();
    seat.presentation().close();
    crate::common::until(
        &mut session,
        "the closed sheet to stop being drawn",
        |session| {
            session.step(Duration::from_millis(16)).expect("step");
            !seat.presentation().is_drawn()
        },
    );
}
