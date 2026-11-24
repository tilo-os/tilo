//! What assistive technology reads of the session's own surfaces.
//! The shell, the bar and every sheet and panel the session hosts are painted by the session's own
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

use std::time::Duration;

use tilo_session::{Options, Session};
use tilo_sheets::SheetState as _;
use tilo_sheets::display_confirm::{ConfirmEffect, Question};
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
fn the_bar_and_the_shell_are_windows_a_screen_reader_reads() {
    let _settled = ui::motion::testing::settled();
    let (_home, session) = session();
    let windows = session.a11y_windows();

    let (_, bar) = window(&windows, "Top bar");
    let (_, shell) = window(&windows, "Desktop");
    for (what, tree) in [("Top bar", &bar), ("Desktop", &shell)] {
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
        shell.nodes.iter().any(|(_, node)| node.label().is_some()),
        "the shell's controls are named"
    );
    // The empty shell offers the way into the app palette, named as the external AT-SPI client of
    // the installed image looks for it.
    let (_, node) = labelled(&shell, "Open an app");
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
    let (place, shell) = window(&session.a11y_windows(), "Desktop");
    let (opener, _) = labelled(&shell, "Open an app");
    session.step(REFRESH).expect("a turn of the session");
    let (_, again) = window(&session.a11y_windows(), "Desktop");
    let (opener_now, _) = labelled(&again, "Open an app");
    assert_eq!(
        opener, opener_now,
        "the id a client holds of the shell's button survives a frame"
    );
    assert!(
        !session.handle().search_open(),
        "the palette is closed to begin with"
    );

    assert!(
        session.a11y_perform(place, opener, &Action::Activate),
        "the shell took the press its window offers"
    );
    crate::common::wait_for(
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
    // The question about a display change: an app of its own, drawn by the session's loop, so
    // only the export a screen reader gets carries it.
    let state = session
        .display_confirm()
        .expect("the session mounts the display question")
        .clone();
    state.presentation().open(false);
    state.ask(Question::asking(
        Some("wlr-randr".to_owned()),
        "Reverting in 15 s".to_owned(),
        false,
    ));
    session.step(REFRESH).expect("a turn of the session");

    let (place, sheet) = window(&session.a11y_windows(), "Display change");
    let (keep, node) = labelled(&sheet, "Keep");
    assert_eq!(node.role(), Role::Button, "a button, as the app draws it");
    assert!(
        node.supports_action(ui::accesskit::Action::Click),
        "the button a screen reader reads is one it can press"
    );

    assert!(
        session.a11y_perform(place, keep, &Action::Activate),
        "the app took the press on its button"
    );
    let mut asked = Vec::new();
    let kept = crate::common::wait_maybe(&mut session, |session| {
        session.step(REFRESH).expect("a turn of the session");
        asked.extend(state.take_asked());
        asked
            .iter()
            .any(|effect| matches!(effect, ConfirmEffect::Keep))
            .then_some(())
    });
    if kept.is_some() {
        return;
    }
    let (_, sheet) = window(&session.a11y_windows(), "Display change");
    let names: Vec<&str> = sheet
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label())
        .collect();
    panic!(
        "the app did not answer from the button assistive technology pressed: {asked:?}, showing {names:?}"
    );
}
