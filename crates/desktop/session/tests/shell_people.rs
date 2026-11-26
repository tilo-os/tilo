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
        sync: Some(crate::common::untethered(PERSON)),
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
    set_members(session, here);
}

/// Who is here, as the peer network lists them, software among them.
fn set_members(session: &Session, here: Vec<tilo_session_core::desktop::here::LiveMember>) {
    session
        .sync()
        .expect("the session takes part in the app wire")
        .here()
        .borrow_mut()
        .set_members(None, Some(here));
}

/// An agent here, as the workspace admitted it, at what it says it is doing.
fn an_agent(at: usize, name: &str, doing: &str) -> tilo_session_core::desktop::here::LiveMember {
    tilo_session_core::desktop::here::LiveMember {
        publisher: format!("{:064x}", at + 1),
        kind: "agent".to_owned(),
        label: name.to_owned(),
        member: Some(collab_types::MemberId::agent(name.to_lowercase(), "ada")),
        activity: Some(doing.to_owned()),
        ..tilo_session_core::desktop::here::LiveMember::default()
    }
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
        .map_or_else(
            || {
                let names: Vec<String> = session
                    .a11y_windows()
                    .into_iter()
                    .map(|(_, name, _)| name)
                    .collect();
                panic!("the shell draws a window named {SHELL:?}; it draws {names:?}")
            },
            |(_, _, tree)| tree,
        )
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

/// Whether who is here says this person may invite someone: the one fact the roster's invite and
/// the bar's are offered on.
fn may_invite(session: &Session) -> bool {
    session
        .read("session:people")
        .and_then(|reading| reading["may_invite"].as_bool())
        .expect("who is here reads whether this person may invite")
}

/// The list row named `label`, as a screen reader finds the row itself: a person's name is also
/// said by the face of them on the bar and the controls their row offers, which are no rows.
fn row_named<'t>(tree: &'t TreeUpdate, label: &str) -> &'t Node {
    tree.nodes
        .iter()
        .find(|(_, node)| node.role() == Role::ListItem && node.label() == Some(label))
        .map_or_else(
            || {
                let names: Vec<&str> = tree
                    .nodes
                    .iter()
                    .filter(|(_, node)| node.role() == Role::ListItem)
                    .filter_map(|(_, node)| node.label())
                    .collect();
                panic!("no row named {label:?}; the list says {names:?}")
            },
            |(_, node)| node,
        )
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
    let mia = row_named(&tree, "Mia Clayton");
    assert!(
        mia.description()
            .is_some_and(|said| said.contains("Editing the release plan")),
        "the row says what they are doing: {:?}",
        mia.description()
    );
    let sam = row_named(&tree, "Sam Okafor");
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
    let inviting = may_invite(&session);
    let mut panel_controls = vec!["Close", "People here now", "You"];
    if inviting {
        panel_controls.push("Invite someone");
    }
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
    assert!(
        reached.iter().any(|label| label.starts_with("Close")),
        "Close is reached by Tab: {reached:?}"
    );
    assert_eq!(
        reached
            .iter()
            .any(|label| label.starts_with("Invite someone")),
        inviting,
        "the invite is reached by Tab where the workspace lets this person invite, and nowhere \
         else: {reached:?}"
    );

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
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session_over(WIDTH, HEIGHT);
    set_here(&session, &[("Mia Clayton", None, None)]);
    open_who_is_here(&mut session);

    // A press on the empty shell off the panel puts it away.
    click(
        &mut session,
        (f64::from(WIDTH) * 0.2, f64::from(HEIGHT) * 0.7),
    );
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
    let _settled = tilo_ui_base::motion::testing::settled();
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

/// A phone's roster offers Follow on software's row: an agent is followed to its run like anyone
/// else, and the control stands inside the panel the person can reach at a phone's width.
#[test]
fn a_phones_who_is_here_offers_follow_on_the_agents_row() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session_over(390, 844);
    until(&mut session, "the shell", |session| {
        session.shell_control("Notifications").is_some()
    });
    set_members(&session, vec![an_agent(0, "Fieldwork", "Reading notes")]);
    open_who_is_here(&mut session);
    until(&mut session, "the agent's row", |session| {
        session.shell_control("Follow Fieldwork").is_some()
    });
    let panel = shell_tree(&session)
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Dialog && node.label() == Some("People"))
        .and_then(|(_, node)| node.bounds())
        .expect("the roster is a dialog named People with bounds");
    let follow = session
        .shell_control("Follow Fieldwork")
        .expect("the agent's row offers Follow");
    // The control is inside the panel the person reads, and on the screen they hold.
    assert!(
        follow.0 >= panel.x0 && follow.0 + follow.2 <= panel.x1,
        "Follow stands inside the sheet: {follow:?} in {panel:?}"
    );
    assert!(
        follow.1 >= panel.y0 && follow.1 + follow.3 <= panel.y1 + 0.5,
        "Follow stands in the sheet's list: {follow:?} in {panel:?}"
    );
    assert!(
        follow.0 >= 0.0 && follow.0 + follow.2 <= 390.0 && follow.1 + follow.3 <= 844.0,
        "Follow is on the phone's screen: {follow:?}"
    );
    // One section takes no label: software alone stands under no group heading, where a second
    // group's band would say "Agents".
    assert!(
        session.shell_control("Agents").is_none(),
        "one group needs no label: {:?}",
        session.accessible_labels()
    );
    // It is the agent's own row: the one that says what it is doing, and what it is.
    assert!(
        session.shell_says("Reading notes"),
        "the agent's row stands in the sheet: {:?}",
        session.accessible_labels()
    );
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session_over(WIDTH, HEIGHT);
    let thousands = 10_000usize;
    // The first of them wears a name no row holds: what a row does with a name longer than it is
    // part of what a workspace of thousands has to keep working. A row says a name as it is
    // written, without the spaces around it, so the name has none.
    let long_name = format!("Ada{}", " Assistance".repeat(24));
    let here: Vec<tilo_session_core::desktop::here::LiveMember> = (0..thousands)
        .map(|at| tilo_session_core::desktop::here::LiveMember {
            publisher: format!("{:064x}", at + 1),
            kind: "person".to_owned(),
            label: if at == 0 {
                long_name.clone()
            } else {
                format!("Person {at}")
            },
            // Each is a person with an account, as in a real workspace: their device is read as
            // theirs, named and coloured as their person.
            user: Some(format!("{:064x}", (1u128 << 100) + at as u128)),
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
    // Everyone here is read once and the rows on screen once: the roster opens in the moment a
    // workspace of three takes, not in one that grows with everyone present.
    let opening = std::time::Instant::now();
    let opened = crate::common::steps_within(&mut session, Duration::from_secs(30), |session| {
        has_row(session, &long_name).then_some(())
    });
    assert!(
        opened.is_some(),
        "a workspace of {thousands} opens its roster within 30 s, not the {:?} it took; the shell's \
         refusals: {:?}",
        opening.elapsed(),
        session.shell_refusals()
    );
    // A name longer than its row leaves the roster's own controls where they stand: its close,
    // and its invite where the workspace lets this person invite.
    assert!(
        session.shell_control("Close").is_some(),
        "a name longer than its row leaves the roster's close where it stands"
    );
    assert_eq!(
        session.shell_control("Invite someone").is_some(),
        may_invite(&session),
        "a name longer than its row leaves the roster's invite where it stands, offered where \
         this person may invite"
    );

    // Only the rows the roster shows are built: the rest of the workspace costs nothing until it
    // is scrolled to.
    let built = rows_named_with(&session, "Person ");
    assert!(
        built > 0 && built < 200,
        "a roster of {thousands} builds the rows it shows, not all of them: {built}"
    );
    assert!(
        !has_row(&session, "Person 9999"),
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
        has_row(session, "Person 9999")
    });
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// On a session whose own workspace lets this person invite, the roster offers the invite and the
/// keyboard reaches it inside the roster, as a press does.
#[test]
fn an_invite_on_who_is_here_is_reached_by_keyboard() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = crate::common::on_its_daemon(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            collab: true,
            ..options()
        },
    );
    until(&mut session, "the shell", |session| {
        session.shell_control("Notifications").is_some()
    });
    open_who_is_here(&mut session);
    assert!(
        may_invite(&session),
        "the workspace this person owns lets them invite"
    );
    let mut reached = Vec::new();
    for _ in 0..24 {
        key(&mut session, KEY_TAB);
        if let Some(label) = focused(&session) {
            reached.push(label);
        }
    }
    assert!(
        reached
            .iter()
            .any(|label| label.starts_with("Invite someone")),
        "the roster's invite is reached by Tab where the person may invite: {reached:?}"
    );
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}
