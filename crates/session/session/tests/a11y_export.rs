//! What assistive technology reads of the session's own surfaces.
//! The desk, the bar and every sheet and panel the session hosts are painted by the session's own
//! loop, so no application window carries them: the session exports each one itself, over the
//! semantic tree the toolkit already builds. These tests read that export the way a screen reader
//! gets it — a named window per surface, a node per control, an action that acts — and drive a
//! control through it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

mod common;

use std::time::Duration;

use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use tilo_sheets::portal::{Body, Dialog, PortalEffect};
use ui::a11y::Action;
use ui::accesskit::{Node, NodeId, Role, TreeUpdate};

const REFRESH: Duration = Duration::from_millis(16);

/// A headless session on its own home, with the sheets it hosts mounted.
fn session() -> (tilo_test_env::TestHome, Session) {
    let home = tilo_test_env::TestHome::new().unwrap();
    let mut session = crate::common::headless(Options {
        collab: false,
        state: Some(home.state().to_path_buf()),
        ..Options::default()
    });
    session.step(REFRESH).expect("a turn of the session");
    (home, session)
}

/// The window `name` and the place it publishes at, as the exporter hands them over.
fn window(windows: &[(usize, String, TreeUpdate)], name: &str) -> (usize, TreeUpdate) {
    windows
        .iter()
        .find(|(_, window, _)| window == name)
        .map_or_else(
            || {
                let names: Vec<&str> = windows
                    .iter()
                    .map(|(_, window, _)| window.as_str())
                    .collect();
                panic!("no window named {name:?}; the session publishes {names:?}")
            },
            |(place, _, tree)| (*place, tree.clone()),
        )
}

/// The node `label` , as a screen reader finds it: by the name it says.
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
            panic!("no node named {label:?}; the window says {names:?}")
        })
}

#[test]
fn the_bar_and_the_desk_are_windows_a_screen_reader_reads() {
    let _settled = ui::motion::testing::settled();
    let (_home, session) = session();
    let windows = session.a11y_windows();

    let (_, bar) = window(&windows, "Top bar");
    let (_, desk) = window(&windows, "Desktop");
    for (what, tree) in [("Top bar", &bar), ("Desktop", &desk)] {
        let root = tree.tree.as_ref().expect("a window names its root").root;
        let (_, node) = tree
            .nodes
            .iter()
            .find(|(id, _)| *id == root)
            .unwrap_or_else(|| panic!("{what} exports its root"));
        assert_eq!(node.role(), Role::Window, "{what} is a window");
        assert_eq!(
            node.label(),
            Some(what),
            "{what} is the name its window carries"
        );
        assert!(
            tree.nodes.len() > 1,
            "{what} carries what it draws, not just its window"
        );
    }
    assert!(
        desk.nodes.iter().any(|(_, node)| node.label().is_some()),
        "the desk's controls are named"
    );
    // The empty desk offers the way into the app palette, named as the external AT-SPI client of
    // the installed image looks for it.
    let (_, node) = labelled(&desk, "Open an app");
    assert_eq!(node.role(), Role::Button);
    assert!(
        node.supports_action(ui::accesskit::Action::Click),
        "the button a screen reader reads is one it can press"
    );
}

#[test]
fn activating_a_button_through_the_export_performs_its_action() {
    let _settled = ui::motion::testing::settled();
    let (_home, mut session) = session();
    let (place, desk) = window(&session.a11y_windows(), "Desktop");
    let (opener, _) = labelled(&desk, "Open an app");
    session.step(REFRESH).expect("a turn of the session");
    let (_, again) = window(&session.a11y_windows(), "Desktop");
    let (opener_now, _) = labelled(&again, "Open an app");
    assert_eq!(
        opener, opener_now,
        "the id a client holds of the desk's button survives a frame"
    );
    assert!(
        !session.handle().search_open(),
        "the palette is closed to begin with"
    );

    assert!(
        session.a11y_perform(place, opener, &Action::Activate),
        "the desk took the press its window offers"
    );
    common::wait_for(
        &mut session,
        "the palette to open from the button",
        |session| {
            session.step(REFRESH).expect("a turn of the session");
            session.handle().search_open().then_some(())
        },
    );
}

#[test]
fn an_app_the_session_hosts_is_exported_with_working_controls() {
    let _settled = ui::motion::testing::settled();
    let (_home, mut session) = session();
    // A question on the portal sheet: an app of its own, drawn by the session's loop, so only the
    // export a screen reader gets carries it.
    let seat = session
        .portal()
        .expect("the session mounts the portal sheet")
        .clone();
    seat.presentation().open(false);
    seat.show(Some(Dialog {
        handle: "a11y-export-1".to_owned(),
        title: "Share your screen?".to_owned(),
        heading: "Meet".to_owned(),
        detail: "Meet asks to record your screen".to_owned(),
        body: Body::Choices(vec!["Whole screen".to_owned(), "One window".to_owned()]),
        confirm_label: "Allow".to_owned(),
        cancel_label: "Cancel".to_owned(),
        ..Dialog::default()
    }));
    session.step(REFRESH).expect("a turn of the session");

    let (place, sheet) = window(&session.a11y_windows(), "System question");
    let (allow, node) = labelled(&sheet, "Allow");
    assert_eq!(node.role(), Role::Button, "a button, as the app draws it");
    assert!(
        node.supports_action(ui::accesskit::Action::Click),
        "the button a screen reader reads is one it can press"
    );

    assert!(
        session.a11y_perform(place, allow, &Action::Activate),
        "the app took the press on its button"
    );
    let mut asked = Vec::new();
    let confirmed = common::wait_maybe(&mut session, |session| {
        session.step(REFRESH).expect("a turn of the session");
        asked.extend(seat.take_asked());
        asked
            .iter()
            .any(|effect| matches!(effect, PortalEffect::Confirm { .. }))
            .then_some(())
    });
    if confirmed.is_some() {
        return;
    }
    let (_, sheet) = window(&session.a11y_windows(), "System question");
    let names: Vec<&str> = sheet
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label())
        .collect();
    panic!(
        "the app did not answer from the button assistive technology pressed: {asked:?}, showing {names:?}"
    );
}
