//! Who is here, as the person drives it: the keyboard and a reader through the roster, the press
//! that puts it away, its sheet on a phone, and a workspace of thousands.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use serde_json::Map;
use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};
use ui::accesskit::{Node, Role, TreeUpdate};

use crate::common::{HEIGHT, KEY_ENTER, KEY_ESCAPE, PERSON, WIDTH, click, key, until};

/// The Tab key, as the compositor's keyboard reads it.
const KEY_TAB: u32 = 15;
/// The Down arrow.
const KEY_DOWN: u32 = 108;
/// The End key.
const KEY_END: u32 = 107;

/// The window the shell draws on its display, as assistive technology names it.
const SHELL: &str = "Desktop shell";

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        collab: true,
        ..Options::default()
    }
}

/// A session on `width` by `height` that takes part in the app wire, for what presence arrives
/// over it.
fn session_over(width: i32, height: i32) -> Session {
    let mut session =
        crate::common::headless_on(&[OutputConfig::new("HEADLESS-1", width, height)], options());
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// Who is here, as the peer network lists them: each of `people` as a live member, with what
/// they say they are doing.
fn set_here(session: &Session, people: &[(&str, Option<&str>, Option<&str>)]) {
    let here: Vec<tilo_session_core::desktop::here::LiveMember> = people
        .iter()
        .enumerate()
        .map(
            |(at, (label, activity, availability))| tilo_session_core::desktop::here::LiveMember {
                publisher: format!("{:064x}", at + 1),
                kind: "person".to_owned(),
                label: (*label).to_owned(),
                activity: activity.map(str::to_owned),
                availability: availability.map(str::to_owned),
                ..tilo_session_core::desktop::here::LiveMember::default()
            },
        )
        .collect();
    session
        .sync()
        .expect("the session takes part in the app wire")
        .here()
        .borrow_mut()
        .set_members(None, Some(here));
}

/// Open who is here, through the session's own action, and wait for its panel.
fn open_who_is_here(session: &mut Session) {
    session
        .invoke("session:people", "show", &Map::new())
        .expect("who is here opens");
    until(session, "the roster's panel", |session| {
        session.shell_control("Close").is_some()
    });
}

/// The shell's accessibility tree, as a screen reader reads the window the shell draws.
fn shell_tree(session: &Session) -> TreeUpdate {
    session
        .a11y_windows()
        .into_iter()
        .find(|(_, name, _)| name == SHELL)
        .map(|(_, _, tree)| tree)
        .unwrap_or_else(|| {
            let names: Vec<String> = session
                .a11y_windows()
                .into_iter()
                .map(|(_, name, _)| name)
                .collect();
            panic!("the shell draws a window named {SHELL:?}; it draws {names:?}")
        })
}

/// The label of the control that holds the keyboard, as a reader finds it: nothing while the
/// window itself is what has it.
fn focused(session: &Session) -> Option<String> {
    let tree = shell_tree(session);
    let root = tree.tree.as_ref()?.root;
    if tree.focus == root {
        return None;
    }
    tree.nodes
        .into_iter()
        .find(|(id, _)| *id == tree.focus)
        .and_then(|(_, node)| node.label().map(str::to_owned))
}

/// The node named `label`, as a screen reader finds it.
fn labelled<'t>(tree: &'t TreeUpdate, label: &str) -> &'t Node {
    tree.nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label))
        .map(|(_, node)| node)
        .unwrap_or_else(|| {
            let names: Vec<&str> = tree
                .nodes
                .iter()
                .filter_map(|(_, node)| node.label())
                .collect();
            panic!("no node named {label:?}; the shell says {names:?}")
        })
}

/// Whether the shell publishes a row named `label`.
fn has_row(session: &Session, label: &str) -> bool {
    shell_tree(session)
        .nodes
        .into_iter()
        .any(|(_, node)| node.role() == Role::ListItem && node.label() == Some(label))
}

/// How many rows the shell publishes whose name starts with `prefix`.
fn rows_named_with(session: &Session, prefix: &str) -> usize {
    shell_tree(session)
        .nodes
        .into_iter()
        .filter(|(_, node)| {
            node.role() == Role::ListItem
                && node.label().is_some_and(|label| label.starts_with(prefix))
        })
        .count()
}

/// Every control the roster shows is reachable from the keyboard, a reader hears what each row
/// says of its person, and the keyboard stays in the roster while it is open.
#[test]
fn who_is_here_is_reachable_by_keyboard_and_says_what_each_row_shows() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_over(WIDTH, HEIGHT);
    set_here(
        &session,
        &[
            ("Mia Clayton", Some("Editing the release plan"), None),
            ("Sam Okafor", None, Some("away")),
        ],
    );
    open_who_is_here(&mut session);
    until(&mut session, "the rows", |session| {
        has_row(session, "Mia Clayton")
    });

    // The panel says what it is, and each row says of its person what the row draws: what they
    // are doing or how available they are.
    let tree = shell_tree(&session);
    let panel = tree
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Dialog && node.label() == Some("People"))
        .map(|(_, node)| node)
        .expect("the roster is a dialog named People");
    assert!(
        panel
            .description()
            .is_some_and(|said| said.contains("where they are and what they are doing")),
        "the panel names itself for a reader: {:?}",
        panel.description()
    );
    let mia = labelled(&tree, "Mia Clayton");
    assert!(
        mia.description()
            .is_some_and(|said| said.contains("Editing the release plan")),
        "the row says what they are doing: {:?}",
        mia.description()
    );
    let sam = labelled(&tree, "Sam Okafor");
    assert!(
        sam.description().is_some_and(|said| said.contains("Away")),
        "the row of someone away says so: {:?}",
        sam.description()
    );
    // What the row offers, named with whose row it is.
    assert!(
        session.shell_control("Wave at Mia Clayton").is_some(),
        "the row offers a wave, named for the person: {:?}",
        session.accessible_labels()
    );
    assert!(
        session.shell_control("Follow Mia Clayton").is_some(),
        "the row offers following them: {:?}",
        session.accessible_labels()
    );

    // Tab reaches every control of the roster, and never a control of the bar behind it.
    let panel_controls = ["Close", "Invite someone", "People here now", "You"];
    let mut reached = Vec::new();
    for _ in 0..24 {
        key(&mut session, KEY_TAB);
        if let Some(label) = focused(&session) {
            reached.push(label);
        }
    }
    assert!(!reached.is_empty(), "the keyboard reaches the roster");
    for label in &reached {
        assert!(
            panel_controls
                .iter()
                .any(|control| label.starts_with(control))
                || session.shell_control(label).is_some(),
            "the keyboard stays in the roster, on a control it shows: {label:?} in {reached:?}"
        );
        assert!(
            !label.contains("is here") && !label.contains("are here"),
            "the faces on the bar behind the roster never take the keyboard: {label:?}"
        );
    }
    for control in ["Close", "Invite someone"] {
        assert!(
            reached.iter().any(|label| label.starts_with(control)),
            "{control} is reached by Tab: {reached:?}"
        );
    }

    // The list holds the rows: moving into it with the arrows and pressing Enter opens the person
    // the keyboard is on, as a press does.
    let mut on_list = focused(&session).is_some_and(|label| label == "People here now");
    for _ in 0..24 {
        if on_list {
            break;
        }
        key(&mut session, KEY_TAB);
        on_list = focused(&session).is_some_and(|label| label == "People here now");
    }
    assert!(on_list, "Tab reaches the list of who is here");
    key(&mut session, KEY_DOWN);
    key(&mut session, KEY_ENTER);
    until(&mut session, "the person's card", |session| {
        session.shell_control("Back to everyone").is_some()
    });

    // Escape puts it away, as a press off it does.
    key(&mut session, KEY_ESCAPE);
    until(&mut session, "the roster gone", |session| {
        crate::common::shell_here(session) != "people"
    });
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// A press off the roster puts it away, and the press that did so does not open it again: the
/// faces are under the pointer it landed on.
#[test]
fn a_press_off_who_is_here_closes_it_and_does_not_reopen_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_over(WIDTH, HEIGHT);
    set_here(&session, &[("Mia Clayton", None, None)]);
    open_who_is_here(&mut session);

    // A press on the empty shell off the panel puts it away.
    click(&mut session, (WIDTH as f64 * 0.2, HEIGHT as f64 * 0.7));
    until(&mut session, "who is here gone", |session| {
        crate::common::shell_here(session) != "people"
    });

    // The press that closed it lands on the faces under it: opening again within the moment is
    // that press, not a new one.
    session
        .invoke("session:people", "show", &Map::new())
        .expect("the same press asks again");
    session.step(Duration::from_millis(32)).unwrap();
    assert_ne!(
        crate::common::shell_here(&session),
        "people",
        "the press that closed the panel does not reopen it"
    );

    // A press after the moment is a new one, and opens it.
    session.step(Duration::from_millis(300)).unwrap();
    session
        .invoke("session:people", "show", &Map::new())
        .expect("a later press opens it");
    until(&mut session, "who is here open", |session| {
        crate::common::shell_here(session) == "people"
    });
}

/// On a phone the roster is a sheet at the shell's foot, between equal gutters, and a press
/// above it puts it away.
#[test]
fn a_phones_who_is_here_is_a_sheet_a_press_above_puts_away() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_over(390, 844);
    // A phone's bar gives who is here away (the person's own menu opens them), so the shell is
    // up when its bell is.
    until(&mut session, "the shell", |session| {
        session.shell_control("Notifications").is_some()
    });
    set_here(
        &session,
        &[("Mia Clayton", None, None), ("Sam Okafor", None, None)],
    );
    open_who_is_here(&mut session);

    // The panel itself, as a reader finds it: where it stands is what a sheet is.
    let tree = shell_tree(&session);
    let panel = tree
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Dialog && node.label() == Some("People"))
        .and_then(|(_, node)| node.bounds())
        .expect("the sheet is a dialog named People with bounds");
    assert!(
        panel.y1 > 844.0 - 64.0,
        "the sheet stands at the shell's foot on a phone: {panel:?}"
    );
    assert!(
        panel.y0 > 300.0,
        "the sheet is the lower part of the screen, not the whole of it: {panel:?}"
    );
    let left = panel.x0;
    let right = 390.0 - panel.x1;
    assert!(
        left > 0.0 && (left - right).abs() < 1.0,
        "the sheet spans the shell between equal gutters: {left} leading, {right} trailing"
    );
    let above = panel.y0 - 120.0;

    // A press above the sheet puts it away.
    click(&mut session, (195.0, above));
    until(&mut session, "the sheet gone", |session| {
        crate::common::shell_here(session) != "people"
    });
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// A workspace of thousands opens a roster that builds only the rows it shows, and the keyboard
/// reaches the last of them: a long list costs the screenful, not the workspace. (The list's own
/// ten-thousand case is held in the toolkit's `a_list_held_to_its_most_height_builds_only_what_it_shows`.)
#[test]
fn a_workspace_of_thousands_opens_only_the_rows_who_is_here_shows() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_over(WIDTH, HEIGHT);
    let thousands = 2_000usize;
    // The first of them wears a name no row holds: what a row does with a name longer than it is
    // part of what a workspace of thousands has to keep working.
    let long_name = format!("Ada {}", "Assistance ".repeat(24));
    let here: Vec<tilo_session_core::desktop::here::LiveMember> = (0..thousands)
        .map(|at| tilo_session_core::desktop::here::LiveMember {
            publisher: format!("{:064x}", at + 1),
            kind: "person".to_owned(),
            label: if at == 0 {
                long_name.clone()
            } else {
                format!("Person {at}")
            },
            ..tilo_session_core::desktop::here::LiveMember::default()
        })
        .collect();
    session
        .sync()
        .expect("the session takes part in the app wire")
        .here()
        .borrow_mut()
        .set_members(None, Some(here));
    open_who_is_here(&mut session);
    until(&mut session, "the first rows", |session| {
        has_row(session, &long_name)
    });
    assert!(
        session.shell_control("Invite someone").is_some(),
        "a name longer than its row leaves the roster's own controls where they stand"
    );

    // Only the rows the roster shows are built: the rest of the workspace costs nothing until it
    // is scrolled to.
    let built = rows_named_with(&session, "Person ");
    assert!(
        built > 0 && built < 200,
        "a roster of {thousands} builds the rows it shows, not all of them: {built}"
    );
    assert!(
        !has_row(&session, "Person 1999"),
        "the last person is not built until the list reaches them"
    );

    // The keyboard reaches the end of the list, which scrolls there.
    let mut on_list = focused(&session).is_some_and(|label| label == "People here now");
    for _ in 0..30 {
        if on_list {
            break;
        }
        key(&mut session, KEY_TAB);
        on_list = focused(&session).is_some_and(|label| label == "People here now");
    }
    assert!(on_list, "Tab reaches the list of who is here");
    key(&mut session, KEY_END);
    until(&mut session, "the last person", |session| {
        has_row(session, "Person 1999")
    });
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}
