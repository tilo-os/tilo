//! A compositor whose main loop deadlocks records it and ends itself.
//!
//! A compositor once deadlocked on its own workspace-tree lock right after
//! login. It kept DRM master and the VT, so the screen stayed frozen,
//! Ctrl+Alt+Fn did nothing, and the person had to hold the power button. The
//! watchdog (`shell::compositor::watchdog`) now notices a loop that stopped
//! for good, writes every thread's stack to the session log and to a freeze
//! record in the crash directory (which the next start reads to tell the
//! person), and exits with its own status, which hands the display back and
//! ends the session.
//!
//! This injects the same kind of deadlock into a real headless compositor on
//! a private runtime directory, with the production thresholds, and asserts
//! both halves: the log and the freeze record name the deadlocked frame, and
//! the process ends.

use std::time::Instant;

use serde_json::json;
use shell::compositor::watchdog::{
    HUNG_AFTER, HUNG_EXIT_STATUS, LONGEST_BOUNDED_WAIT, RECORD_WITHIN,
};
use shell::shell::state::WorkspaceTree;

use crate::support::desktop::Desktop;

#[test]
fn a_deadlocked_main_loop_is_recorded_and_the_compositor_ends() {
    let mut desktop = Desktop::start((1280, 800), &WorkspaceTree::bootstrap_demo());
    // The harness's own monitor fails a test on the first stall; here the
    // stall is the subject.
    desktop.stop_monitor();
    // The compositor symbolizes every thread's stack on the way out, and the
    // first dump in a process parses this build's debug information: ~20 s of
    // the 25 s it allows its own report, against 2 s once that is cached.
    // Production ships the symbol table without debug info (`profile.release`),
    // where the same dump is milliseconds, so the parse is the harness's build
    // and not the recovery under test. Pay it here, through the production
    // dump, before the deadline this test measures starts.
    let warm = desktop.thread_dump();
    assert!(
        warm.contains("thread "),
        "the liveness dump names every thread before the hang:\n{warm}"
    );
    let injected = Instant::now();
    desktop.send(&json!({"op": "deadlock_main_loop"}));

    // Noticed one sample past the bound, then at most one bound for the dump
    // and one for the record.
    let expected = HUNG_AFTER
        .saturating_add(LONGEST_BOUNDED_WAIT)
        .saturating_add(RECORD_WITHIN);
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
    tilo_test_env::wait_until("the compositor's last log line", || {
        desktop.recent_log(usize::MAX).contains(last_line)
    });
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

    // The freeze is kept where the next start looks for it, with the stacks.
    let crashes = desktop.cache_home().join("tilo/crashes");
    let records: Vec<_> = std::fs::read_dir(&crashes)
        .unwrap_or_else(|error| panic!("{}: {error}\n{log}", crashes.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.to_string_lossy()
                .ends_with(shell::crash_log::FREEZE_SUFFIX)
        })
        .collect();
    let [record] = records.as_slice() else {
        panic!(
            "one freeze record in {}: {records:?}\n{log}",
            crashes.display()
        );
    };
    let body = std::fs::read_to_string(record).unwrap();
    assert!(
        body.starts_with(shell::crash_log::FREEZE_TITLE),
        "a freeze record:\n{body}"
    );
    assert!(
        body.contains("deadlock_this_thread_for_test"),
        "the freeze record carries the compositor thread's stack:\n{body}"
    );
    assert!(
        log.contains(&record.display().to_string()),
        "the log names the record:\n{}",
        desktop.recent_log(8)
    );
    let name = record.file_name().unwrap().to_string_lossy();
    assert_eq!(
        shell::shell::crash_notice::ended_by(&name, || None),
        Some(shell::shell::crash_notice::Ended::Froze),
        "the next start tells the person the desktop stopped responding"
    );
}
