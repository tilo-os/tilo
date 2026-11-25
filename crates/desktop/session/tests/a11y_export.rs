//! What assistive technology reads of the session's own surfaces.
//! The shell draws this device's whole screen — its bar, its windows and the cards over them — so
//! no application window carries it: the session exports it itself, over the semantic tree the
//! toolkit already builds, one window per display named as the shell names it. These tests read
//! that export the way a screen reader gets it — a named window, a node per control, an action
//! that acts — and drive a control through it.

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

use settings_store::hardware::{
    DisplayConfig, DisplaySet, DisplayTransaction, DisplayTransactionState, OutputConfig,
};
use tilo_compositor::OutputConfig as CompositorOutput;
use tilo_session::{Options, Session};
use ui::a11y::Action;
use ui::accesskit::{Node, NodeId, Role, TreeUpdate};

const REFRESH: Duration = Duration::from_millis(16);

/// The window a display's part of the shell publishes as, as the shell names it.
const SHELL_WINDOW: &str = "Desktop shell";

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
    named_window(windows, name).unwrap_or_else(|| {
        let names: Vec<&str> = windows
            .iter()
            .map(|(_, window, _)| window.as_str())
            .collect();
        panic!("no window named {name:?}; the session publishes {names:?}")
    })
}

/// [`window`] when the session publishes it, for a caller that waits for it.
fn named_window(
    windows: &[(usize, String, TreeUpdate)],
    name: &str,
) -> Option<(usize, TreeUpdate)> {
    windows
        .iter()
        .find(|(_, window, _)| window == name)
        .map(|(place, _, tree)| (*place, tree.clone()))
}

/// The shell's window and where it publishes, once it draws.
fn shell_window(windows: &[(usize, String, TreeUpdate)]) -> Option<(usize, TreeUpdate)> {
    named_window(windows, SHELL_WINDOW)
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

/// The node labelled `label` a screen reader reaches: not one of the copies a mirror hides.
fn reached(tree: &TreeUpdate, label: &str) -> (NodeId, Node) {
    tree.nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label) && !node.is_hidden())
        .cloned()
        .unwrap_or_else(|| {
            let names: Vec<&str> = tree
                .nodes
                .iter()
                .filter_map(|(_, node)| node.label())
                .collect();
            panic!("no node named {label:?} that a reader reaches; the window says {names:?}")
        })
}

#[test]
fn the_shell_is_one_window_whose_bar_a_screen_reader_reads() {
    let _settled = ui::motion::testing::settled();
    let (_home, mut session) = session();
    // The bar stands, its bell among the nodes, so the shell has drawn itself.
    crate::common::until(&mut session, "the shell's export", |session| {
        shell_window(&session.a11y_windows()).is_some_and(|(_, tree)| {
            tree.nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Notifications"))
        })
    });

    let (_, shell) = window(&session.a11y_windows(), SHELL_WINDOW);
    let root = shell.tree.as_ref().expect("a window names its root").root;
    let (_, node) = shell
        .nodes
        .iter()
        .find(|(id, _)| *id == root)
        .unwrap_or_else(|| panic!("the shell exports its root"));
    assert_eq!(node.role(), Role::Window, "the shell is a window");
    assert_eq!(
        node.label(),
        Some(SHELL_WINDOW),
        "the window is named as the shell names it"
    );
    assert!(
        shell.nodes.len() > 1,
        "the window carries the screen it draws, not just its window"
    );
    assert!(
        shell.nodes.iter().any(|(_, node)| node.label().is_some()),
        "the shell's controls are named"
    );

    // The top bar is inside this window: its bell is found by the name it says, and the button a
    // screen reader reads is one it can press.
    let (_, bell) = labelled(&shell, "Notifications");
    assert_eq!(bell.role(), Role::Button, "the bell is a button");
    assert!(
        bell.supports_action(ui::accesskit::Action::Click),
        "the bell a screen reader reads is one it can press"
    );
    // The empty shell offers the way into the app palette, named as the external AT-SPI client of
    // the installed image looks for it.
    let (_, opener) = labelled(&shell, "Open an app");
    assert_eq!(opener.role(), Role::Button);
    assert!(
        opener.supports_action(ui::accesskit::Action::Click),
        "the button a screen reader reads is one it can press"
    );
}

#[test]
fn activating_a_button_through_the_export_performs_its_action() {
    let _settled = ui::motion::testing::settled();
    let (_home, mut session) = session();
    let (place, opener) = crate::common::wait_for(
        &mut session,
        "the shell's way into the app palette",
        |session| {
            let (place, tree) = shell_window(&session.a11y_windows())?;
            let opener = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("Open an app"))
                .map(|(id, _)| *id)?;
            Some((place, opener))
        },
    );
    session.step(REFRESH).expect("a turn of the session");
    let (_, again) = shell_window(&session.a11y_windows()).expect("the shell's window");
    let opener_now = again
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some("Open an app"))
        .map(|(id, _)| *id)
        .expect("the shell's button stays");
    assert_eq!(
        opener, opener_now,
        "the id a client holds of the shell's button survives a frame"
    );
    assert_ne!(
        crate::common::shell_here(&session),
        "launcher",
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
            (crate::common::shell_here(session) == "launcher").then_some(())
        },
    );
}

#[test]
fn an_app_the_session_hosts_is_exported_with_working_controls() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let mut session = crate::common::headless_on(
        &[
            CompositorOutput::new("HEADLESS-1", 1280, 800),
            CompositorOutput::new("HEADLESS-2", 1280, 800).at(1280, 0),
        ],
        Options {
            collab: false,
            state: Some(home.state().to_path_buf()),
            ..Options::default()
        },
    );
    session.step(REFRESH).expect("a turn of the session");

    // The question about a display change: the session stages a change that could leave the person
    // with a dark screen, and asks about it before keeping it. It is drawn by the session's loop,
    // so only the export a screen reader gets carries it.
    let snapshot = session
        .compositor()
        .display_snapshot()
        .cloned()
        .expect("the displays are published");
    let descriptors: Vec<_> = snapshot
        .outputs
        .iter()
        .map(|output| output.descriptor.clone())
        .collect();
    let set = DisplaySet::of_descriptors(&descriptors);
    let (left, right) = (&descriptors[0].connector, &descriptors[1].connector);
    let mut candidate = DisplayConfig::default();
    candidate.outputs.insert(
        left.clone(),
        OutputConfig {
            x: 0,
            y: 0,
            ..OutputConfig::default()
        },
    );
    candidate.outputs.insert(
        right.clone(),
        OutputConfig {
            enabled: false,
            ..OutputConfig::default()
        },
    );
    DisplayTransaction::request(set, candidate).expect("a change staged");

    let (place, keep) =
        crate::common::wait_for(&mut session, "the display question's export", |session| {
            session.step(REFRESH).expect("a turn of the session");
            let (place, tree) = named_window(&session.a11y_windows(), "Display change")?;
            let keep = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("Keep") && !node.is_hidden())
                .map(|(id, _)| *id)?;
            Some((place, keep))
        });
    let (_, sheet) = window(&session.a11y_windows(), "Display change");
    let (_, node) = reached(&sheet, "Keep");
    assert_eq!(node.role(), Role::Button, "a button, as the app draws it");
    assert!(
        node.supports_action(ui::accesskit::Action::Click),
        "the button a screen reader reads is one it can press"
    );

    assert!(
        session.a11y_perform(place, keep, &Action::Activate),
        "the app took the press on its button"
    );
    // The answer the client pressed is the one the session records: keeping the change, as a press
    // on the sheet with the pointer does.
    let confirmed = crate::common::wait_maybe(&mut session, |session| {
        session.step(REFRESH).expect("a turn of the session");
        (DisplayTransaction::load().map(|transaction| transaction.state)
            == Some(DisplayTransactionState::Confirmed))
        .then_some(())
    });
    if confirmed.is_some() {
        return;
    }
    let (_, sheet) = window(&session.a11y_windows(), "Display change");
    let names: Vec<&str> = sheet
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label())
        .collect();
    panic!(
        "the app did not answer from the button assistive technology pressed; showing {names:?}"
    );
}
