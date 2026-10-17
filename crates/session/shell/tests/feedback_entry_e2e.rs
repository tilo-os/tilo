//! Feedback opened through the control socket's semantic actions, on the real
//! headless compositor: `feedback.open` brings up the private composer beside
//! the window in front, and asking again brings back the same one.

use serde_json::Value;
use shell::shell::state::WorkspaceTree;

use crate::support::desktop::Desktop;

fn composers(windows: &[Value]) -> Vec<&Value> {
    windows
        .iter()
        .filter(|row| row.get("title").and_then(Value::as_str) == Some("Feedback"))
        .collect()
}

#[test]
fn feedback_opens_once_beside_the_window_in_front_through_its_action() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    // An untouched demo is recognised as sample data and replaced on load.
    tree.desktop.workspaces[0].name = "Feedback desktop".into();
    let desktop = Desktop::start((1280, 800), &tree);
    assert!(composers(&desktop.windows()).is_empty());

    desktop.invoke_action("actions", "feedback.open");
    let windows = desktop.windows();
    let opened = composers(&windows);
    assert_eq!(opened.len(), 1, "{windows:?}");
    assert_eq!(
        opened[0]["active"], true,
        "the composer is in front: {windows:?}"
    );
    let tab = opened[0]["tab"].clone();

    desktop.invoke_action("actions", "feedback.open");
    let windows = desktop.windows();
    let again = composers(&windows);
    assert_eq!(
        again.len(),
        1,
        "one private composer at a time: {windows:?}"
    );
    assert_eq!(again[0]["tab"], tab);
    assert_eq!(again[0]["active"], true);
    desktop.check();
}
