//! A change of a workspace this device's service does not hold is nothing this session can read.
//!
//! The changes stream is what search, the activity and the person's mentions follow: a change that
//! reached it is read again by each of them, on their own threads. A workspace the service holds
//! nothing of answers none of those reads, so a change of one is not noted at all: no worker wakes,
//! nothing is said of what cannot be read, and the session stays idle until the service holds the
//! workspace.

#![cfg(target_os = "linux")]

use std::time::Duration;

use tilo_session::platform::records::Changes;
use tilo_session::{Options, Records, SearchOptions};

use crate::common::private_home;

/// A workspace this session's service holds nothing of: what a joined workspace is while the
/// device's service keeps no copy of it.
const UNHELD: &str = "0ba58f66-488d-4af0-a2fb-6a8a28c42c08";

/// How long the session is left alone before its wakes are counted, and over what window: the
/// changes arrive for longer than both, so what they cost is counted as they come.
const SETTLE: Duration = Duration::from_millis(200);
const WINDOW: Duration = Duration::from_millis(1_200);
const NOTING: Duration = Duration::from_millis(2_000);

/// The threads that follow the changes stream: search's reading of the workspaces' indexes, and
/// the indexer of their content, as the kernel names them: a name is cut at fifteen characters.
const FOLLOWERS: [&str; 2] = ["tilo-search-rec", "tilo-record-ind"];

/// How many times each follower woke over `settle` then `window`, and what the session's threads
/// took of a core over it.
fn wakes() -> (Vec<(String, u64)>, f64) {
    for thread in FOLLOWERS {
        assert!(
            tilo_test_env::idle::live_named(thread).is_some(),
            "the session follows the stream on {thread}"
        );
    }
    let counted = tilo_test_env::idle::idle_wakeups(std::process::id(), SETTLE, WINDOW)
        .expect("the session's threads are sampled");
    let woke = FOLLOWERS
        .into_iter()
        .map(|thread| {
            let woke = counted
                .rows
                .iter()
                .filter(|row| row.thread == thread)
                .map(tilo_test_env::idle::WakeupRow::wakeups)
                .sum();
            (thread.to_owned(), woke)
        })
        .collect();
    (woke, counted.total_cpu_percent())
}

#[test]
fn a_change_of_a_workspace_this_device_does_not_hold_wakes_nothing() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let found = Records::of(root.path().join("data"));
    let changes: Changes = found.changes();
    let mut session = crate::common::headless(Options {
        // No shell and no app: what is under test is the stream the roles follow, not a window.
        shell: false,
        search: SearchOptions {
            records: Some(found),
            ..SearchOptions::default()
        },
        ..crate::common::api_options(root.path())
    });
    // The session's own look at what it holds ran: the threads that follow the stream stand.
    tilo_test_env::wait_until("the session follows the stream", || {
        FOLLOWERS
            .into_iter()
            .all(|thread| tilo_test_env::idle::live_named(thread).is_some())
    });
    session.step(Duration::from_millis(1)).expect("a frame");
    let before = wakes();

    // Changes of the workspace arrive as another device's writes do, while the session's wakes are
    // counted: none is news here, so no follower reads the workspace and nothing of it is indexed.
    let arriving = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + NOTING;
        let mut noted = 0_u64;
        while std::time::Instant::now() < deadline {
            for route in ["desktop", "chat.workspace", "messages"] {
                changes.note(UNHELD, "so.tilo.browser-desktop", route, false);
                noted = noted.saturating_add(1);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        noted
    });
    let after = wakes();
    let noted = arriving.join().expect("the changes arrived");

    let woke = |counted: &[(String, u64)]| -> u64 { counted.iter().map(|(_, woke)| woke).sum() };
    assert_eq!(
        woke(&after.0),
        0,
        "no thread following the stream woke for the unheld workspace while {noted} changes \
         arrived: before {before:?}, after {after:?}"
    );
}
