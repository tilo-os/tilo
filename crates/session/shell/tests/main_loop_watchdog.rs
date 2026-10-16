//! A compositor whose main loop deadlocks records it and ends itself.
//!
//! A compositor once deadlocked on its own workspace-tree lock right after
//! login. It kept DRM master and the VT, so the screen stayed frozen,
//! Ctrl+Alt+Fn did nothing, and the person had to hold the power button. The
//! watchdog (`shell::compositor::watchdog`) now notices a loop that stopped
//! for good, writes every thread's stack to the session record, and exits
//! with its own status, which hands the display back and ends the session.
//!
//! This injects the same kind of deadlock into a real headless compositor on
//! a private runtime directory, with the production thresholds, and asserts
//! both halves: the record names the deadlocked frame, and the process ends.

use std::time::{Duration, Instant};

use serde_json::json;
use shell::compositor::watchdog::{HUNG_AFTER, HUNG_EXIT_STATUS, LONGEST_BOUNDED_WAIT};
use shell::shell::state::WorkspaceTree;

use crate::support::desktop::Desktop;

#[test]
fn a_deadlocked_main_loop_is_recorded_and_the_compositor_ends() {
    let mut desktop = Desktop::start((1280, 800), &WorkspaceTree::bootstrap_demo());
    // The harness's own monitor fails a test on the first stall; here the
    // stall is the subject.
    desktop.stop_monitor();
    let injected = Instant::now();
    desktop.send(&json!({"op": "deadlock_main_loop"}));

    // Noticed one sample past the bound, then at most one bound for the dump.
    let expected = HUNG_AFTER.saturating_add(LONGEST_BOUNDED_WAIT);
    let status = desktop
        .wait_for_exit(expected.saturating_add(tilo_test_env::hang_bound()))
        .unwrap_or_else(|| {
            panic!(
                "the deadlocked compositor was still running {:?} after the deadlock\n{}",
                injected.elapsed(),
                desktop.recent_log(80)
            )
        });
    let ended_after = injected.elapsed();
    assert!(
        ended_after > LONGEST_BOUNDED_WAIT,
        "ended after {ended_after:?}, within a wait the compositor may make"
    );

    // The stderr reader may still be catching up with the last lines.
    let last_line = "ending the hung compositor";
    let read_from = Instant::now();
    while !desktop.recent_log(usize::MAX).contains(last_line)
        && read_from.elapsed() < Duration::from_secs(10)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    let log = desktop.recent_log(usize::MAX);
    assert_eq!(
        status.code(),
        Some(HUNG_EXIT_STATUS),
        "the watchdog ends a hung compositor with its own status ({status})\n{}",
        desktop.recent_log(80)
    );
    assert!(
        log.contains("main loop hung"),
        "the hang is in the record:\n{}",
        desktop.recent_log(80)
    );
    assert!(
        log.contains("deadlock_this_thread_for_test"),
        "the record carries the compositor thread's stack, down to the deadlock:\n{log}"
    );
    assert!(
        log.contains(last_line),
        "the record ends with the recovery ({} lines read, ending):\n{}",
        log.lines().count(),
        desktop.recent_log(8)
    );
}
