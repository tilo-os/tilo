//! Tests for the shell's remote-cursor mapping/overlay helpers.
//!
//! T24 introduces `cursor_in_bounds` and the `RemoteCursorOverlay` stub so
//! upcoming tasks (T25 SimulatedFeed, T26 overlay draw) can compose against a
//! stable API. T25 adds the `SimulatedFeed` that drives cursors for the demo
//! without a real peer; subsequent tasks append their own cases.

use shell::shell::remote_cursors::{cursor_in_bounds, RemoteCursorOverlay, SimulatedFeed};
use ui::presence::{PresenceCtx, RemoteCursor};
use std::time::Duration;

#[test]
fn remote_cursor_is_clipped_to_active_tile_bounds() {
    let mut ctx = PresenceCtx::default();
    ctx.remote_cursors.push(RemoteCursor {
        user_id: "u".into(),
        color: [0.0; 4],
        position: [50.0, 50.0],
        label: "Ada".into(),
    });
    ctx.remote_cursors.push(RemoteCursor {
        user_id: "v".into(),
        color: [0.0; 4],
        position: [999.0, 999.0],
        label: "Bo".into(),
    });
    let bounds = [0.0_f32, 0.0, 400.0, 300.0];
    let visible: Vec<_> = ctx
        .remote_cursors
        .iter()
        .filter(|c| cursor_in_bounds(c.position, bounds))
        .collect();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].user_id, "u");
}

#[test]
fn remote_cursor_overlay_exposes_active_tile_bounds() {
    // The overlay is a stub in T24; T26 fleshes out draw/filter logic. We
    // still assert its shape so downstream tasks can rely on construction.
    let ov = RemoteCursorOverlay::new();
    assert_eq!(ov.active_tile_bounds(), [0.0, 0.0, 0.0, 0.0]);
}

#[test]
fn simulated_feed_moves_cursor_deterministically() {
    let mut feed = SimulatedFeed::new_with_seed(42);
    let mut ctx = PresenceCtx::default();
    feed.tick(Duration::from_millis(16), &mut ctx);
    assert_eq!(ctx.remote_cursors.len(), 1);
    let p0 = ctx.remote_cursors[0].position;
    feed.tick(Duration::from_millis(100), &mut ctx);
    let p1 = ctx.remote_cursors[0].position;
    assert!(p0 != p1, "cursor should have moved");
}

#[test]
fn remote_cursor_overlay_filters_to_active_tile() {
    use shell::shell::remote_cursors::RemoteCursorOverlay;
    use ui::presence::{PresenceCtx, RemoteCursor};
    let mut ctx = PresenceCtx::default();
    ctx.remote_cursors = vec![
        RemoteCursor {
            user_id: "in".into(),
            color: [0.0; 4],
            position: [50.0, 50.0],
            label: "I".into(),
        },
        RemoteCursor {
            user_id: "out".into(),
            color: [0.0; 4],
            position: [500.0, 500.0],
            label: "O".into(),
        },
    ];
    let mut ov = RemoteCursorOverlay::new();
    ov.set_active_tile_bounds([0.0, 0.0, 200.0, 200.0]);
    let drawn = ov.cursors_to_draw(&ctx);
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].user_id, "in");
}
