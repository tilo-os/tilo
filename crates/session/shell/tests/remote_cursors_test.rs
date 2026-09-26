//! Tests for the shell's remote-cursor mapping/overlay helpers:
//! `cursor_in_bounds` and the clipping `RemoteCursorOverlay` performs
//! against the active tile.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::remote_cursors::{RemoteCursorOverlay, cursor_in_bounds};
use ui::presence::{PointerOwner, PresenceCtx, RemoteCursor};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn remote_cursors_are_confined_to_the_active_tile_without_redrawing_the_desktop_pointer() {
    // Phase `remote_cursor_is_clipped_to_active_tile_bounds`.
    {
        let mut ctx = PresenceCtx::default();
        ctx.remote_cursors.push(RemoteCursor {
            user_id: "u".into(),
            color: [0.0; 4],
            position: [50.0, 50.0],
            label: "Ada".into(),
            owner: PointerOwner::Window,
        });
        ctx.remote_cursors.push(RemoteCursor {
            user_id: "v".into(),
            color: [0.0; 4],
            position: [999.0, 999.0],
            label: "Bo".into(),
            owner: PointerOwner::Window,
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

    // Phase `remote_cursor_overlay_exposes_active_tile_bounds`.
    {
        let ov = RemoteCursorOverlay::new();
        assert!(
            ov.active_tile_bounds()
                .iter()
                .all(|component| component.abs() <= f32::EPSILON)
        );
    }

    // Phase `remote_cursor_overlay_filters_to_active_tile`.
    {
        use shell::shell::remote_cursors::RemoteCursorOverlay;
        use ui::presence::{PointerOwner, PresenceCtx, RemoteCursor};
        let mut ctx = PresenceCtx::default();
        ctx.remote_cursors = vec![
            RemoteCursor {
                user_id: "in".into(),
                color: [0.0; 4],
                position: [50.0, 50.0],
                label: "I".into(),
                owner: PointerOwner::Window,
            },
            RemoteCursor {
                user_id: "out".into(),
                color: [0.0; 4],
                position: [500.0, 500.0],
                label: "O".into(),
                owner: PointerOwner::Window,
            },
        ];
        let mut ov = RemoteCursorOverlay::new();
        ov.set_active_tile_bounds([0.0, 0.0, 200.0, 200.0]);
        let drawn = ov.cursors_to_draw(&ctx);
        assert_eq!(drawn.len(), 1);
        assert_eq!(drawn[0].user_id, "in");
    }

    // Phase `a_desktop_pointer_is_not_redrawn_inside_the_tile`.
    // A pointer the compositor overlay paints must not be painted a second
    // time by a widget inside the tree. Not currently mounted — asserted
    // anyway since mounting it is a one-line change, and the store it reads
    // is the one a peer used to come out of twice.
    {
        let mut ctx = PresenceCtx::default();
        ctx.remote_cursors = vec![
            RemoteCursor {
                user_id: "desktop".into(),
                color: [0.0; 4],
                position: [50.0, 50.0],
                label: "D".into(),
                owner: PointerOwner::Desktop,
            },
            RemoteCursor {
                user_id: "window".into(),
                color: [0.0; 4],
                position: [60.0, 60.0],
                label: "W".into(),
                owner: PointerOwner::Window,
            },
        ];
        let mut ov = RemoteCursorOverlay::new();
        ov.set_active_tile_bounds([0.0, 0.0, 200.0, 200.0]);

        let drawn = ov.cursors_to_draw(&ctx);

        // Both are well inside the tile, so bounds are not what decides it.
        assert_eq!(
            drawn.len(),
            1,
            "the compositor's pointer must not be redrawn here"
        );
        assert_eq!(
            drawn[0].user_id, "window",
            "and the one nothing else paints must still be drawn",
        );
    }
}
