//! The workspaces a menu of the shell lists: the one in front is marked, and the others are
//! offered to switch to. A row that only says where the person already is takes no press, so the
//! menu never shows a command that does nothing.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use tilolang_runtime::{UiNode, Value};

use crate::shell_staged::{SHELL, StagedWindow, ada};

/// The shell with its bar's workspaces menu open: three workspaces, Studio in front.
fn shell() -> StagedWindow {
    let mut theme = tilo_ui_base::Theme::light();
    theme.reduce_motion = true;
    let mut shell = StagedWindow::staged("an afternoon of work", (1280, 800), theme);
    seed_bar(&mut shell);
    // The bar's chip opens the workspaces to switch between, in place.
    shell.press("Studio");
    shell
}

/// The bar a new shell starts with: a staged host makes the shell's document without running its
/// initial action, so the test runs the app's own — the one a new shell's bar and dock come from.
fn seed_bar(shell: &mut StagedWindow) {
    {
        let host = shell.instance.host().expect("the window runs a host");
        let meta = &host.package(SHELL).expect("the shell's package").meta;
        let document = meta
            .types
            .iter()
            .find(|ty| ty.name == "Shell")
            .expect("the shell's document type")
            .id
            .clone();
        let root = host
            .root_resource(SHELL, &document)
            .expect("the shell's document");
        let seed = meta
            .initial_of(&document)
            .expect("the shell's initial action")
            .id
            .clone();
        host.runtime()
            .invoke_action(&ada(), (seed.as_str(), vec![Value::resource(&root)]))
            .expect("the bar a new shell starts with");
    }
    shell.exchange();
}

/// What a node is called.
fn label_of(node: &UiNode) -> Option<&str> {
    match node.props.get("label") {
        Some(Value::Text(text)) => Some(text.as_str()),
        _ => None,
    }
}

/// The menu's row named `wanted`.
fn row<'a>(tree: &'a UiNode, wanted: &str) -> &'a UiNode {
    tree.of_kind("ui.MenuItem")
        .into_iter()
        .find(|node| label_of(node) == Some(wanted))
        .unwrap_or_else(|| {
            let rows: Vec<String> = tree
                .of_kind("ui.MenuItem")
                .into_iter()
                .filter_map(|node| label_of(node).map(str::to_owned))
                .collect();
            panic!("the menu lists {wanted:?}: {rows:?}")
        })
}

#[test]
fn the_workspace_in_front_is_marked_and_the_others_are_offered() {
    let shell = shell();
    let tree: UiNode = shell.instance.root_tree().expect("the window's tree");

    // The workspace in front: the row says so and has nothing to run.
    let current = row(&tree, "Studio");
    assert_eq!(
        current.props.get("checked"),
        Some(&Value::Bool(true)),
        "the workspace in front is the checked one: {:?}",
        current.props
    );
    assert!(
        !current.events.contains_key("choose"),
        "the workspace in front takes no press: there is nowhere to switch to"
    );

    // Another workspace: a press switches to it, through the session's own action.
    let other = row(&tree, "Home");
    assert!(
        other.events.contains_key("choose"),
        "another workspace is a command: {:?}",
        other.events
    );
}
