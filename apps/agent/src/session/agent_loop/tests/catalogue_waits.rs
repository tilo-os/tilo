//! How a run waits for the workspace's catalogue: on the link's own news, asleep
//! until it comes, never on a timer.

use super::*;
use tilo_test_env::idle::{context_switches, current_thread_id};

/// A wait for the catalogue sleeps until the link brings its next republication.
/// With nothing published it wakes not at all: the bound is the outer end of one
/// sleep, not an interval a poll runs at.
#[test]
fn a_catalogue_wait_sleeps_until_a_republication_arrives() {
    let mut f = fixture("write a paragraph", World::default(), Vec::new());
    let tid = current_thread_id().expect("a thread id").to_string();
    let before = context_switches(&tid);
    let came = f
        .session
        .await_catalogue(Duration::from_millis(400), |_| false);
    let woke = context_switches(&tid).saturating_sub(before);
    assert!(
        !came,
        "nothing was published, so the wait ended at its bound"
    );
    assert!(
        woke <= 2,
        "the wait woke {woke} times over 400 ms of nothing: it polls instead of sleeping"
    );
}
