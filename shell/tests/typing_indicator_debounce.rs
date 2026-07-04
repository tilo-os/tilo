//! Typing indicator debounce: 100 emits within 300ms is one visible
//! indicator; 300ms of silence clears it.

use std::time::{Duration, Instant};

use shell::shell::typing_indicator::{
    EmitDebouncer, SILENCE_TIMEOUT, TypingTracker,
};

#[test]
fn hundred_emits_within_window_is_one_indicator() {
    let t0 = Instant::now();
    let mut tracker = TypingTracker::new();
    for i in 0..100 {
        tracker.note("alice", "typing", t0 + Duration::from_millis(i * 3));
    }
    let ind = tracker.active_at(t0 + Duration::from_millis(300));
    assert_eq!(
        ind.len(),
        1,
        "100 rapid emits must collapse to a single indicator",
    );
    assert_eq!(ind[0].action, "typing");
}

#[test]
fn silence_clears_indicator() {
    let t0 = Instant::now();
    let mut tracker = TypingTracker::new();
    // Feed a small burst then go silent for well over the timeout.
    for i in 0..5 {
        tracker.note("alice", "typing", t0 + Duration::from_millis(i * 20));
    }
    let last_note = t0 + Duration::from_millis(4 * 20);
    let ind = tracker.active_at(last_note + SILENCE_TIMEOUT + Duration::from_millis(50));
    assert!(
        ind.is_empty(),
        "indicator must clear after {}ms silence",
        SILENCE_TIMEOUT.as_millis(),
    );
}

#[test]
fn action_transitions_replace_indicator() {
    let t0 = Instant::now();
    let mut tracker = TypingTracker::new();
    tracker.note("alice", "typing", t0);
    tracker.note("alice", "resizing", t0 + Duration::from_millis(50));
    let ind = tracker.active_at(t0 + Duration::from_millis(60));
    assert_eq!(ind.len(), 1);
    assert_eq!(ind[0].action, "resizing");
}

#[test]
fn per_peer_indicators_are_independent() {
    let t0 = Instant::now();
    let mut tracker = TypingTracker::new();
    tracker.note("alice", "typing", t0);
    tracker.note("bob", "moving", t0);
    let ind = tracker.active_at(t0 + Duration::from_millis(10));
    assert_eq!(ind.len(), 2);
}

#[test]
fn emit_debouncer_swallows_keystroke_flood() {
    // Simulated 300ms burst: 60 keystrokes at ~5ms each. The debouncer
    // should let through exactly 3 (one at t=0, one every ~150ms).
    let t0 = Instant::now();
    let mut d = EmitDebouncer::new();
    let mut sent = 0;
    for i in 0..60 {
        if d.should_send(t0 + Duration::from_millis(i * 5)) {
            sent += 1;
        }
    }
    // 300ms burst / 150ms interval = at most 3 emits.
    assert!(
        sent >= 2 && sent <= 3,
        "expected 2-3 emits for a 300ms burst; got {sent}",
    );
}
