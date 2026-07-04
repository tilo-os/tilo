//! Cursor interpolation: given `(0,0)@0` and `(100,100)@100ms`, sampling
//! at `50ms` must return `~(50,50)` within a 5-unit tolerance.
//!
//! Also asserts fade-out, remove, and pubkey → color stability.

use std::time::{Duration, Instant};

use shell::shell::cursor_interp::{
    CursorInterpolator, FADE_AFTER, FADE_DURATION, color_from_pubkey,
};

#[test]
fn interpolates_midpoint_within_tolerance() {
    let t0 = Instant::now();
    let t100 = t0 + Duration::from_millis(100);
    let t50 = t0 + Duration::from_millis(50);
    let key = vec![7u8; 32];

    let mut i = CursorInterpolator::new();
    i.push_sample("alice", &key, "Alice", [0.0, 0.0], t0);
    i.push_sample("alice", &key, "Alice", [100.0, 100.0], t100);

    let cs = i.sample_at(t50);
    assert_eq!(cs.len(), 1);
    let x = cs[0].position[0];
    let y = cs[0].position[1];
    assert!(
        (x - 50.0).abs() <= 5.0 && (y - 50.0).abs() <= 5.0,
        "expected ~(50±5, 50±5), got ({x}, {y})",
    );
}

#[test]
fn holds_latest_position_past_the_window() {
    let t0 = Instant::now();
    let t100 = t0 + Duration::from_millis(100);
    let key = vec![7u8; 32];

    let mut i = CursorInterpolator::new();
    i.push_sample("alice", &key, "Alice", [0.0, 0.0], t0);
    i.push_sample("alice", &key, "Alice", [100.0, 100.0], t100);

    // Sampling well past t100 (but before fade) sticks to latest.
    let cs = i.sample_at(t100 + Duration::from_millis(500));
    assert_eq!(cs[0].position, [100.0, 100.0]);
}

#[test]
fn cursor_fades_out_after_inactivity() {
    let t0 = Instant::now();
    let key = vec![7u8; 32];
    let mut i = CursorInterpolator::new();
    i.push_sample("alice", &key, "Alice", [50.0, 50.0], t0);

    // Just before fade begins.
    let cs = i.sample_at(t0 + FADE_AFTER);
    assert_eq!(cs.len(), 1);
    assert!(cs[0].alpha > 0.99);

    // Midway through fade.
    let cs = i.sample_at(t0 + FADE_AFTER + FADE_DURATION / 2);
    assert!(cs[0].alpha > 0.0 && cs[0].alpha < 1.0);

    // Fully faded — cursor drops out.
    let cs = i.sample_at(t0 + FADE_AFTER + FADE_DURATION * 2);
    assert!(cs.is_empty(), "cursor must vanish after fade");
}

#[test]
fn same_pubkey_yields_same_color_across_calls() {
    let a = color_from_pubkey(&[1u8; 32]);
    let b = color_from_pubkey(&[1u8; 32]);
    assert_eq!(a, b);
    let c = color_from_pubkey(&[2u8; 32]);
    assert_ne!(a, c);
}

#[test]
fn stream_of_samples_smoothly_interpolates_across_bracket() {
    // Given a stream of RemoteCursorMoved events (0,0)@0 and (100,100)@100ms,
    // sample at [0, 25, 50, 75, 100] and assert monotonic increase
    // and midpoint accuracy.
    let t0 = Instant::now();
    let t100 = t0 + Duration::from_millis(100);
    let key = vec![7u8; 32];

    let mut i = CursorInterpolator::new();
    i.push_sample("alice", &key, "", [0.0, 0.0], t0);
    i.push_sample("alice", &key, "", [100.0, 100.0], t100);

    let mut xs = vec![];
    for ms in [0u64, 25, 50, 75, 100] {
        let cs = i.sample_at(t0 + Duration::from_millis(ms));
        xs.push(cs[0].position[0]);
    }
    for w in xs.windows(2) {
        assert!(
            w[1] >= w[0] - 0.01,
            "position must not go backwards across samples: {xs:?}",
        );
    }
    // Midpoint within tolerance.
    assert!((xs[2] - 50.0).abs() <= 5.0);
}
