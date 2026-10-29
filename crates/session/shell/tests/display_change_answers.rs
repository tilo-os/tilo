//! Display layouts and the Keep/Revert answers to them, invoked over the
//! control socket the way an agent drives a desktop: every invocation
//! answers, with or without `wait_ms`, and the compositor's loop keeps
//! turning throughout. With nothing to keep, Keep and Revert say so at once
//! instead of leaving the caller waiting.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use settings_store::SettingsSource as _;
use settings_store::hardware::{
    DISPLAY_TRANSACTION_SECTION, DisplayTransaction, DisplayTransactionState,
};
use shell::shell::state::WorkspaceTree;

use crate::support::desktop::Desktop;

/// Far below the longest a control reply may be held (30 s): an answer
/// that has nothing to wait for comes at once, so this only absorbs a
/// loaded machine.
const PROMPT: Duration = Duration::from_secs(10);

/// Invoke `actions:<id>`, with `wait_ms` or without, and return the reply,
/// which must come promptly.
#[track_caller]
fn invoke(desktop: &Desktop, id: &str, wait_ms: Option<u64>) -> Value {
    let mut request = json!({"op": "actions_invoke", "provider": "actions", "id": id});
    if let Some(wait_ms) = wait_ms {
        request["wait_ms"] = json!(wait_ms);
    }
    let started = Instant::now();
    let reply = desktop.call(&request);
    assert!(
        started.elapsed() < PROMPT,
        "{id} (wait_ms {wait_ms:?}) took {:?} to answer: {reply}",
        started.elapsed()
    );
    desktop.check();
    reply
}

#[track_caller]
fn assert_nothing_waiting(reply: &Value, id: &str) {
    let err = reply
        .get("err")
        .unwrap_or_else(|| panic!("{id} with nothing pending must be refused: {reply}"));
    assert!(
        err.to_string().contains("no display change is waiting")
            || err.to_string().contains("not available"),
        "{id} says why it did nothing: {reply}"
    );
}

/// The display change the compositor would be counting down on after a
/// layout that can leave a screen dark, written through the compositor as
/// Settings writes it.
fn stage_awaiting_change(desktop: &Desktop) -> u64 {
    let store = settings_store::Store::follow(&desktop.settings_root())
        .expect("follow the compositor's settings");
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_millis(),
    )
    .expect("milliseconds fit");
    let transaction = DisplayTransaction {
        id: now,
        state: DisplayTransactionState::AwaitingConfirmation,
        // Never expires within the test; the headless backend does not
        // count it down anyway.
        deadline_unix_ms: Some(now.saturating_add(600_000)),
        ..DisplayTransaction::default()
    };
    let section = serde_json::to_value(&transaction)
        .expect("encode the transaction")
        .as_object()
        .cloned()
        .expect("a section");
    store
        .update_section(DISPLAY_TRANSACTION_SECTION, section)
        .expect("the compositor takes the display change");
    transaction.id
}

#[test]
fn display_layout_then_keep_and_revert_always_answer_and_never_stall_the_loop() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    // An untouched demo is recognised as sample data and replaced on load.
    tree.desktop.workspaces[0].name = "Display answers".into();
    let desktop = Desktop::start((1280, 800), &tree);

    // The sequence that froze a laptop: a layout, then Keep. Headless
    // publishes no displays, so the layout has nothing to arrange and Keep
    // nothing to keep; each says so, with a wait and without.
    for wait_ms in [None, Some(8000)] {
        invoke(&desktop, "display.layout.extend", wait_ms);
        let kept = invoke(&desktop, "display.change.keep", wait_ms);
        assert_nothing_waiting(&kept, "display.change.keep");
        let reverted = invoke(&desktop, "display.change.revert", wait_ms);
        assert_nothing_waiting(&reverted, "display.change.revert");
    }

    // A change waiting for its answer: Keep is recorded and answers, with
    // a wait and without; a second Keep finds nothing left to keep.
    for wait_ms in [None, Some(8000)] {
        stage_awaiting_change(&desktop);
        let kept = invoke(&desktop, "display.change.keep", wait_ms);
        assert!(kept.get("err").is_none(), "Keep is taken: {kept}");
        let again = invoke(&desktop, "display.change.keep", wait_ms);
        assert_nothing_waiting(&again, "a second display.change.keep");
    }

    // Revert answers the same way.
    stage_awaiting_change(&desktop);
    let reverted = invoke(&desktop, "display.change.revert", Some(8000));
    assert!(reverted.get("err").is_none(), "Revert is taken: {reverted}");

    // The loop is still serving after all of it.
    desktop.chrome();
    desktop.check();
}
