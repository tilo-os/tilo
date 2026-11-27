//! What a window's app reads of the actions it shows, over the connection the daemon serves it
//! ([`crate::device::shown`]): each decided with the person's ordinary answer in the workspace
//! keeping its record.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::remote::{ActionProvider as _, Caller};
use tilo_actions::shown::{ActionVerdict, NOT_HERE, ShownAction, ShownAsk};
use tilo_actions::{ActionKind, ActionNeed};

use crate::RuntimeServices as _;
use crate::device::services::Daemon;
use crate::device::services::tests::isolated_root;

const TEST: &str =
    "device::shown_tests::a_window_reads_what_its_device_says_of_each_action_it_shows";

/// A window's app reads, over its own connection, what the device says of the actions it shows:
/// one of a record this device's person owns is allowed, one kept in a workspace this device does
/// not keep is refused in words the person reads.
#[test]
fn a_window_reads_what_its_device_says_of_each_action_it_shows() {
    let Some(root) = isolated_root(TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().expect("the person's workspace");
    let at = crate::seats::AppAt {
        app: "so.tilo.notes".to_owned(),
        workspace: workspace.clone(),
        pid: std::process::id(),
    };
    let line = super::live_speaking::tests::app_connection(&daemon, &at);
    let caller = Caller::launched("so.tilo.notes").in_workspace(&workspace);
    let need = ActionNeed::of(ActionKind::Undoable);
    let here = ShownAction::new(
        Some(&workspace),
        "so.tilo.notes",
        ("test.playback", "person:test.playback", "mute"),
        &need,
    );
    let away = ShownAction::new(
        Some(&uuid::Uuid::new_v4().to_string()),
        "so.tilo.notes",
        ("test.playback", "person:test.playback", "mute"),
        &need,
    );
    let ask = ShownAsk {
        actions: vec![here.clone(), away.clone()],
        after: None,
        wait_ms: None,
    };
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    let decisions = loop {
        let answer = line
            .shown_actions(&caller, ask.clone())
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten();
        match answer {
            Some(Ok(decisions)) => break decisions,
            other => {
                assert!(
                    Instant::now() < deadline,
                    "this device never decided what its window shows: {other:?}"
                );
                std::thread::park_timeout(Duration::from_millis(50));
            }
        }
    };
    // Besides each action shown, the device answers it as declared, which a record the window
    // was not asked about takes.
    let said = |action: &ShownAction| {
        decisions
            .iter()
            .find(|decision| &decision.action == action)
            .map(|decision| decision.verdict.clone())
    };
    assert_eq!(
        said(&here),
        Some(ActionVerdict::Allowed),
        "the owner of the workspace that keeps the record may run its action"
    );
    assert_eq!(
        said(&away),
        Some(ActionVerdict::Refused(NOT_HERE.into())),
        "a workspace this device does not keep is refused in words the person reads"
    );
}
