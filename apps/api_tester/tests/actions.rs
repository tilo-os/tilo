//! Action-model proof for the retained API-tester tree.
//!
//! The companion to `a11y.rs`: where that proves the controls are enumerable
//! and invokable *by a screen reader*, this proves every interaction is a
//! dispatched **action** on a published **resource tree** — invokable by id
//! (a command palette / AT / test), routed through one executor, and recorded
//! for undo. Same reusable harness (`ui::headless::Driver`), same real event
//! and CRDT paths, no display.

use api_tester::ApiTesterApp;
use ui::a11y::Role;
use ui::headless::Driver;
use ui::State;

fn driver() -> Driver {
    Driver::new(State::new(), Box::new(ApiTesterApp::new_demo()), (1280, 800))
}

/// The app publishes its whole "what can I do here" surface as a resource
/// tree of stable action ids + human labels — the source of truth a Ctrl+K
/// palette, an AT, and the controls all share.
#[test]
fn resources_publishes_the_action_tree() {
    let d = driver();
    let tree = d.resources().expect("the app publishes a resource tree");
    assert_eq!(tree.role, Role::Window, "the workspace is the root resource");

    let ids: Vec<&str> = tree.actions_deep().iter().map(|a| a.id.as_str()).collect();
    assert!(ids.contains(&"request.send"), "Send is published: {ids:?}");
    assert!(ids.iter().any(|id| id.starts_with("request.tab.")), "the tabs are actions");
    assert!(ids.iter().any(|id| id.starts_with("response.view.")), "the response views are actions");
    assert!(ids.contains(&"collection.toggle.0"), "a collection publishes a fold action");
    assert!(ids.contains(&"request.load.0.0"), "a saved request publishes a load action");
    assert!(ids.iter().any(|id| id.starts_with("history.replay.")), "history entries publish a replay action");

    // View changes are not undoable; state changes are.
    let send = tree.find_action("request.send").expect("send action");
    assert!(!send.undoable, "Send is a side-effect, not a persisted change");
    assert_eq!(send.label, "Send request");
    let tab = tree.find_action("request.tab.1").expect("tab action");
    assert!(!tab.undoable, "switching tabs is a view change, not undoable");
    let load = tree.find_action("request.load.0.0").expect("load action");
    assert!(load.undoable, "loading a request is an undoable state change");
}

/// Invoking an action by id runs the one executor — the exact path a command
/// palette or an assistive technology takes — and the effect lands in the app.
#[test]
fn invoking_an_action_by_id_runs_the_executor() {
    let mut d = driver();
    assert!(
        d.focus_access_nodes().iter().any(|n| n.name.as_deref() == Some("GET List users")),
        "the collection starts expanded"
    );

    let ran = d.invoke_action("collection.toggle.0");
    assert!(ran, "the executor recognised and ran the action");
    assert!(
        !d.focus_access_nodes().iter().any(|n| n.name.as_deref() == Some("GET List users")),
        "invoking the fold action collapsed the collection"
    );

    // An unknown id is rejected, not silently swallowed.
    assert!(!d.invoke_action("does.not.exist"), "unknown actions are rejected");
}

/// A state-changing action records an undo boundary, so Ctrl+Z reverts it and
/// Ctrl+Y reapplies it — the whole point of routing interactions through the
/// action executor.
#[test]
fn a_state_changing_action_is_undoable() {
    let mut d = driver();

    // Collapse the collection (a persisted, undoable action).
    d.invoke_action("collection.toggle.0");
    assert!(
        !d.focus_access_nodes().iter().any(|n| n.name.as_deref() == Some("GET List users")),
        "the collection is collapsed"
    );

    // Undo restores it — the model re-derives from the reverted CRDT.
    assert!(d.undo(), "an undo boundary was recorded for the action");
    assert!(
        d.focus_access_nodes().iter().any(|n| n.name.as_deref() == Some("GET List users")),
        "undo re-expanded the collection"
    );

    // Redo reapplies it.
    assert!(d.redo(), "redo is available");
    assert!(
        !d.focus_access_nodes().iter().any(|n| n.name.as_deref() == Some("GET List users")),
        "redo re-collapsed the collection"
    );
}
