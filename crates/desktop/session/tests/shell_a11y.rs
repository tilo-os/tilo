//! What assistive technology reads of the tilolang shell: the screen's own window, its bar's
//! controls and its windows as nodes with the actions a screen reader presses — the same export a
//! Rust-drawn shell carried, read the way a screen reader gets it and driven through it.
//!
//! The shell draws this device's whole screen, so it is the session's own surface and carries no
//! application window: the session exports it over the semantic tree the toolkit builds, one
//! window per display. These tests read that export, never a parallel description of it.

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

use serde_json::{Map, Value, json};
use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};
use ui::accesskit::{Node, NodeId, Role, TreeUpdate};

use crate::common::{HEIGHT, PERSON, WIDTH, notebook as notes, until};

/// The window a display's part of the shell publishes as, as the shell names it.
const SHELL_WINDOW: &str = "Desktop shell";

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        collab: false,
        ..Options::default()
    }
}

/// A session on the displays `places` name, each [`WIDTH`] by [`HEIGHT`] .
fn session(places: &[(i32, i32)]) -> Session {
    let outputs: Vec<OutputConfig> = places
        .iter()
        .enumerate()
        .map(|(index, place)| {
            OutputConfig::new(format!("HEADLESS-{}", index + 1), WIDTH, HEIGHT).at(place.0, place.1)
        })
        .collect();
    let mut session = crate::common::headless_on(&outputs, options());
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// A session that takes part in the app wire too, for what presence arrives over it.
fn collaborating(places: &[(i32, i32)]) -> Session {
    let outputs: Vec<OutputConfig> = places
        .iter()
        .enumerate()
        .map(|(index, place)| {
            OutputConfig::new(format!("HEADLESS-{}", index + 1), WIDTH, HEIGHT).at(place.0, place.1)
        })
        .collect();
    let mut session = crate::common::headless_on(
        &outputs,
        Options {
            collab: true,
            ..options()
        },
    );
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// Place `resource` as a strip of its own on the workspace in front, through the layout's
/// published `place` .
fn place_note(session: &mut Session, resource: &str) -> u64 {
    let id = session.handle().mint_id();
    let (workspace, row, strip) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.id().to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
        )
    };
    let arguments: Map<String, Value> = json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": id,
        "resource": resource,
        "x": 80,
        "y": 60,
        "width": 480,
        "height": 360,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:layout", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    id
}

/// Every window the session exports, by the place it publishes at: its name and its tree.
fn windows(session: &Session) -> Vec<(usize, String, TreeUpdate)> {
    session.a11y_windows()
}

/// The shell's windows, one per display that draws it.
fn shell_windows(session: &Session) -> Vec<(usize, TreeUpdate)> {
    windows(session)
        .into_iter()
        .filter(|(_, name, _)| name == SHELL_WINDOW)
        .map(|(place, _, tree)| (place, tree))
        .collect()
}

/// The root node of `tree` , as a screen reader finds the window's own.
fn root(tree: &TreeUpdate) -> &Node {
    let root = tree.tree.as_ref().expect("a window names its root").root;
    tree.nodes
        .iter()
        .find(|(id, _)| *id == root)
        .map_or_else(|| panic!("the window exports its root"), |(_, node)| node)
}

/// The node labelled `label` , as a screen reader finds it: by the name it says.
fn labelled(tree: &TreeUpdate, label: &str) -> (NodeId, Node) {
    tree.nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label))
        .cloned()
        .unwrap_or_else(|| {
            let names: Vec<&str> = tree
                .nodes
                .iter()
                .filter_map(|(_, node)| node.label())
                .collect();
            panic!("no node named {label:?}; the shell says {names:?}")
        })
}

/// The node whose label holds `words` , with the node itself.
fn holding(tree: &TreeUpdate, words: &str) -> Option<(NodeId, Node)> {
    tree.nodes
        .iter()
        .find(|(_, node)| node.label().is_some_and(|label| label.contains(words)))
        .cloned()
}

#[test]
fn the_shell_is_a_window_whose_bar_and_windows_a_screen_reader_reads() {
    let _settled = ui::motion::testing::settled();
    let (notebook, titles) = notes(&crate::common::TITLES[..1]);
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            services: vec![Box::new(notebook)],
            ..options()
        },
    );
    // The bar stands: its bell is up, so the shell has drawn itself.
    until(&mut session, "the bar's bell", |session| {
        session.shell_control("Notifications").is_some()
    });

    let shell = shell_windows(&session);
    assert_eq!(
        shell.len(),
        1,
        "one display draws one window of the shell: {:?}",
        windows(&session)
            .into_iter()
            .map(|(_, name, _)| name)
            .collect::<Vec<_>>()
    );
    let (_, tree) = shell.into_iter().next().expect("the shell's window");
    assert_eq!(root(&tree).role(), Role::Window, "the shell is a window");
    assert_eq!(
        root(&tree).label(),
        Some(SHELL_WINDOW),
        "the window is named as the shell names it"
    );
    assert!(
        tree.nodes.len() > 1,
        "the window carries the screen it draws, not the window alone"
    );
    // The bar's controls are nodes a screen reader reads and presses: the bell, and the person's
    // own face.
    let (_, bell) = labelled(&tree, "Notifications");
    assert_eq!(bell.role(), Role::Button, "the bell is a button");
    assert!(
        bell.supports_action(ui::accesskit::Action::Click),
        "the button a screen reader reads is one it can press"
    );
    assert!(
        tree.nodes
            .iter()
            .any(|(_, node)| node.label() == Some(PERSON)),
        "the person's own face is named for assistive technology"
    );

    // A window placed on the shell reads as a node of it, by what it shows.
    let resource = &titles[0];
    place_note(&mut session, resource);
    until(&mut session, "the placed window in the layout", |session| {
        crate::common::places(session)
            .iter()
            .any(|place| place.resource == *resource)
    });
    let shown = crate::common::drawn_name(&session, resource);
    until(
        &mut session,
        "the window's name in the shell's tree",
        |session| {
            shell_windows(session)
                .first()
                .is_some_and(|(_, tree)| holding(tree, &shown).is_some())
        },
    );
    let (_, tree) = shell_windows(&session).remove(0);
    let (_, node) = holding(&tree, &shown).expect("the window is named in the shell's tree");
    assert!(
        !node.is_hidden(),
        "the window the shell draws is a node a screen reader reaches"
    );
}

/// A control the export names is one a screen reader presses: activating the bar's bell through
/// the tree opens the notification centre, exactly as a press on it does.
#[test]
fn a_control_pressed_through_the_export_acts_on_the_shell() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0)]);
    until(&mut session, "the bar's bell", |session| {
        session.shell_control("Notifications").is_some()
    });
    until(&mut session, "the centre is down", |session| {
        !session.shell_says("Nothing to catch up on")
    });
    let (place, tree) = shell_windows(&session).remove(0);
    let (bell, _) = labelled(&tree, "Notifications");
    assert!(
        session.a11y_perform(place, bell, &ui::a11y::Action::Activate),
        "the export takes the activation"
    );
    until(&mut session, "the centre up", |session| {
        session.shell_says("Nothing to catch up on")
    });
}

#[test]
fn every_display_draws_a_window_of_the_shell_of_its_own() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0), (WIDTH, 0)]);
    until(&mut session, "two parts of the shell", |session| {
        session.shell_parts().len() == 2
    });
    until(&mut session, "both parts' export", |session| {
        shell_windows(session).len() == 2
    });
    let shell = shell_windows(&session);
    let mut places: Vec<usize> = shell.iter().map(|(place, _)| *place).collect();
    places.sort_unstable();
    places.dedup();
    assert_eq!(
        places.len(),
        2,
        "each display publishes a window of its own"
    );
    for (place, tree) in &shell {
        assert_eq!(
            root(tree).role(),
            Role::Window,
            "the display at {place} exports a window"
        );
        assert!(
            tree.nodes.iter().any(|(_, node)| node.label().is_some()),
            "the display at {place} carries its own controls"
        );
    }
}

/// A notice that arrives is a live region a screen reader announces politely: the banner the
/// shell raises for it carries the words it says, marked polite.
#[test]
fn a_notice_that_arrives_is_announced_politely() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0)]);
    until(&mut session, "the bar's bell", |session| {
        session.shell_control("Notifications").is_some()
    });
    session.notices().post(
        tilo_session_core::desktop::notifications::Notification::new(
            "Backup finished",
            "12 GB safe",
        )
        .with_app_name("Settings"),
    );
    until(&mut session, "the banner's words", |session| {
        shell_windows(session)
            .first()
            .is_some_and(|(_, tree)| holding(tree, "Backup finished").is_some())
    });
    let (_, tree) = shell_windows(&session).remove(0);
    let (_, node) = labelled(&tree, "Backup finished");
    assert_eq!(
        node.live(),
        Some(ui::accesskit::Live::Polite),
        "a notice that passes is announced politely, not interruptively"
    );
    assert!(
        node.description()
            .is_some_and(|said| said.contains("12 GB safe")),
        "the announcement carries what the notice says: {:?}",
        node.description()
    );
}

/// The roster names each person here and says what they are doing: a reader who cannot see the
/// rows is told the same line the row draws under the name.
#[test]
fn the_roster_reads_each_person_and_what_they_are_doing() {
    let _settled = ui::motion::testing::settled();
    let mut session = collaborating(&[(0, 0)]);
    until(&mut session, "the people button", |session| {
        session.shell_control("People").is_some()
    });
    let activity = "Editing the release plan";
    let here: Vec<tilo_session_core::desktop::here::LiveMember> = vec![tilo_session_core::desktop::here::LiveMember {
        publisher: "01".repeat(32),
        kind: "person".to_owned(),
        label: "Mia Clayton".to_owned(),
        activity: Some(activity.to_owned()),
        ..tilo_session_core::desktop::here::LiveMember::default()
    }];
    session
        .sync()
        .expect("the session takes part in the app wire")
        .here()
        .borrow_mut()
        .set_members(None, Some(here));
    until(&mut session, "the face of the person here", |session| {
        session.shell_control("Mia Clayton is here").is_some()
    });
    crate::common::press(&mut session, "Mia Clayton is here");

    // The roster is a window of its own in the export, and each person is a row of it with what
    // they are doing.
    until(&mut session, "the roster's export", |session| {
        windows(session)
            .iter()
            .any(|(_, name, tree)| name == "People" && holding(tree, "Mia Clayton").is_some())
    });
    let (_, _, tree) = windows(&session)
        .into_iter()
        .find(|(_, name, _)| name == "People")
        .expect("the roster's window");
    let (_, node) = labelled(&tree, "Mia Clayton");
    assert_eq!(node.role(), Role::ListItem, "the person is a row");
    assert!(
        node.description()
            .is_some_and(|said| said.contains(activity)),
        "the row says what they are doing, as the row draws it: {:?}",
        node.description()
    );
}
