//! How a run waits for the workspace's catalogue: on the link's own news, asleep
//! until it comes, never on a timer.

use super::*;

/// A wait for the catalogue sleeps until the link brings its next republication.
/// With nothing published it wakes not at all: one sleep to the outer bound, not an
/// interval a poll runs at.
#[test]
fn a_catalogue_wait_sleeps_until_a_republication_arrives() {
    let mut f = fixture("write a paragraph", World::default(), Vec::new());
    let before = f.session.link.waits();
    let started = std::time::Instant::now();
    let came = f
        .session
        .await_catalogue(Duration::from_millis(400), |_| false);
    let elapsed = started.elapsed();
    let waits = f.session.link.waits() - before;
    assert!(
        !came,
        "nothing was published, so the wait ended at its bound"
    );
    assert_eq!(
        waits, 1,
        "one sleep covers the wait, so it never polls: {waits} waits in {elapsed:?}"
    );
}
