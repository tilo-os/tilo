//! Peer-join toast batching: 3 join events in <1s → single toast.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use shell::shell::peer_join_toast::{BATCH_WINDOW, PeerEventKind, PeerToastQueue};

#[test]
fn three_joins_within_one_second_produce_one_toast() {
    let t0 = Instant::now();
    let mut q = PeerToastQueue::new();
    q.push(PeerEventKind::Joined, "alice", t0);
    q.push(
        PeerEventKind::Joined,
        "bob",
        t0 + Duration::from_millis(200),
    );
    q.push(
        PeerEventKind::Joined,
        "charlie",
        t0 + Duration::from_millis(400),
    );

    let ready = q.drain_ready(t0 + BATCH_WINDOW + Duration::from_millis(1));
    assert_eq!(ready.len(), 1, "3 rapid joins must batch into one toast");
    assert_eq!(ready[0].kind, PeerEventKind::Joined);
    assert_eq!(ready[0].title, "3 peers joined");
    assert!(ready[0].body.contains("alice"));
    assert!(ready[0].body.contains("bob"));
    assert!(ready[0].body.contains("charlie"));
}

#[test]
fn single_join_produces_named_toast() {
    let t0 = Instant::now();
    let mut q = PeerToastQueue::new();
    q.push(PeerEventKind::Joined, "ada", t0);
    let ready = q.drain_ready(t0 + BATCH_WINDOW + Duration::from_millis(1));
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].title, "ada joined");
}

#[test]
fn joined_and_left_split_into_two_separate_toasts() {
    let t0 = Instant::now();
    let mut q = PeerToastQueue::new();
    q.push(PeerEventKind::Joined, "alice", t0);
    q.push(PeerEventKind::Left, "bob", t0 + Duration::from_millis(50));
    let ready = q.drain_ready(t0 + BATCH_WINDOW + Duration::from_millis(1));
    assert_eq!(ready.len(), 2);
    let titles: Vec<&str> = ready.iter().map(|r| r.title.as_str()).collect();
    assert!(titles.contains(&"alice joined"));
    assert!(titles.contains(&"bob left"));
}

#[test]
fn events_after_window_form_a_new_batch() {
    let t0 = Instant::now();
    let mut q = PeerToastQueue::new();
    q.push(PeerEventKind::Joined, "alice", t0);
    q.push(
        PeerEventKind::Joined,
        "bob",
        t0 + BATCH_WINDOW + Duration::from_secs(1),
    );
    let ready_first = q.drain_ready(t0 + BATCH_WINDOW + Duration::from_millis(1));
    assert_eq!(ready_first.len(), 1);
    assert_eq!(ready_first[0].title, "alice joined");
    assert_eq!(q.pending(), 1);
    let ready_second = q.drain_ready(t0 + BATCH_WINDOW * 2 + Duration::from_secs(2));
    assert_eq!(ready_second.len(), 1);
    assert_eq!(ready_second[0].title, "bob joined");
}
