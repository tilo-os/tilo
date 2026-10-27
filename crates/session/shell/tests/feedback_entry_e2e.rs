//! Feedback opened through the control socket's semantic actions, on the real
//! headless compositor: `feedback.open` brings up the private composer beside
//! the window in front, asking again brings back the same one, and the
//! composer's own actions are offered to this device's automation. This
//! desktop has no feedback destination, so its review is refused visibly.

use serde_json::{Value, json};
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

    let field_edit = device_local(&desktop, "field-edit");
    let edited = desktop.call(&json!({
        "op": "actions_invoke", "provider": "device-local", "id": field_edit, "wait_ms": 8000,
        "args": {"edit": {"path": [{"field": "issue"}], "op": "replace", "text": "The note lost my edit"}},
    }));
    assert!(edited.get("err").is_none(), "{edited}");
    let review = device_local(&desktop, "review");
    let refused = desktop.call(&json!({
        "op": "actions_invoke", "provider": "device-local", "id": review, "wait_ms": 8000,
    }));
    assert!(
        // The refusal says the composer's sentence, not its message key.
        refused
            .to_string()
            .contains("Feedback is not configured for this workspace."),
        "a desktop without a destination refuses review: {refused}"
    );
    desktop.check();
}

/// The id of the composer's `action` among this device's private-window actions.
fn device_local(desktop: &Desktop, action: &str) -> String {
    let suffix = format!("\u{1f}{action}");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let listed = desktop.call(&json!({
            "op": "actions_enumerate", "query": "device-local", "limit": 2000, "wait_ms": 2000,
        }));
        let found = listed["answer"]["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["provider"] == "device-local")
            .find_map(|row| row["id"].as_str().filter(|id| id.ends_with(&suffix)))
            .map(str::to_owned);
        if let Some(found) = found {
            return found;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the composer never offered {action} on this device: {listed}"
        );
        desktop.pause(std::time::Duration::from_millis(100));
    }
}
