//! A desktop nobody is touching republishes nothing.
//!
//! Every listing of actions, and an open palette, reads the action surface
//! apps publish: the broker's published surfaces and directory roots, the
//! invocation statuses, the app actions' directory epoch. Each of those
//! moving makes the next read start over, and a listing paged while they
//! move had its cursor go stale; apps' actions came and went for agents.
//! With nothing being done, nothing has changed, so none of them may move.
//!
//! The compositor traces each move with its cause (`tilo::publications`),
//! and an app republishing a surface it already had. The test waits on
//! those lines as the compositor prints them, for a minute.

use std::time::{Duration, Instant};

use crate::idle_wakeups::Desktop;

/// How long the settled desktop is watched.
const QUIET: Duration = Duration::from_secs(60);

/// What the compositor prints when the action surface moves, or an app
/// republishes what it had.
const MOVES: [&str; 6] = [
    "published surfaces changed",
    "action directory roots changed",
    "invocation statuses changed",
    "app actions directory epoch moved",
    "action surface changed",
    "an unchanged action surface republished",
];

#[test]
fn an_idle_desktop_republishes_nothing_for_a_minute() {
    let desktop = Desktop::start();
    desktop.wait_for("the restored Notes notebook", |targets| {
        targets.iter().any(|name| name == "notes.first_note")
    });
    // Settled: Notes has published, and the listing of every action is
    // complete, with nothing on its way that would add to it. The control
    // socket answers once it is (`wait_ms`), woken by what it waits for.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let reply = desktop.control(&serde_json::json!({
            "op": "actions_enumerate", "query": "", "detail": "compact", "wait_ms": 10000,
        }));
        let answer = &reply["answer"];
        let notes = answer["actions"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["application"] == "so.tilo.notes" && row["operation"] == "new-note")
        });
        if notes && answer["complete"] == true {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the desktop's listing never settled: {reply}\nLog:\n{}",
            desktop.log_tail()
        );
    }
    let quiet_from = Instant::now();
    let moved = desktop.next_line(quiet_from, &MOVES, quiet_from + QUIET);
    assert!(
        moved.is_none(),
        "an idle desktop republished {:?} after it settled: {}\nLog:\n{}",
        quiet_from.elapsed(),
        moved.unwrap_or_default(),
        desktop.log_tail()
    );
}
