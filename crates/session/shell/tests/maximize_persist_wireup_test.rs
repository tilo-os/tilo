//! Per-frame wire-up coverage for the `WindowGroup::maximized` writeback.
//!
//! The widget toggles `maximized` in memory on a click, but that stays
//! widget-local until the shell harvests it back into the `WorkspaceTree`
//! — without the per-frame writeback the toggle resets to `false` on the
//! next shell restart. These tests prime `bar.pending_max` (what the
//! tab-bar glyph press writes), pump a no-op cursor-move so the drain
//! block flips `maximized`, then tick the same harvest
//! [`Widget::update`] runs in production and assert the persisted value
//! followed (both directions).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::FreshId as _;
use std::collections::HashMap;

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use winit::event::Modifiers;

use crate::support::host::{Layout, dispatch, lay_out};

/// Lays out the strip-0 group on its own so `handle_event`'s
/// `taffy.children(node_id)` has the (bar, content) children it expects.
/// The event is at (9000, 9000), well outside any widget's rect, so only
/// the drain block at the bottom of `handle_event` runs.
fn pump_no_op_event(
    host: &mut ShellOverlayHost,
    strip_id: StripId,
    event: &winit::event::WindowEvent,
) {
    let g = host
        .group_at_for_tests(strip_id, 0)
        .expect("strip-0 group-0 widget");
    let Layout { taffy, child_node } = lay_out(g, 800.0, 600.0);
    dispatch(g, &taffy, child_node, Modifiers::default(), event);
}

/// One-workspace, one-strip, one-group tree with a single tab — no
/// sibling group to mask a misrouted update.
fn single_group_tree() -> (WorkspaceTree, StripId, GroupId) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 4242;
    let tab_id: u64 = 0xc0;

    let group = WindowGroup {
        id: group_id,
        tabs: vec![Tab {
            id: tab_id,
            title: "Solo".into(),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: "solo".into(),
            },
        }],
        active_tab: tab_id,
        width: WindowWidth::Normal,
        maximized: false,
    };
    let strip = Strip {
        id: strip_id,
        name: "Solo".into(),
        window_groups: vec![group],
    };
    let ws = Workspace {
        strips: vec![strip],
        // Everything else comes from the builder, so a field added to
        // `Workspace` does not break this fixture.
        ..Workspace::new(ws_id, "Test")
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, group_id);

    let tree = WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        },
        ..Default::default()
    };

    (tree, strip_id, group_id)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn shell_tick_persists_maximize_only_when_a_click_fired() {
    // Phase `shell_tick_writes_maximize_click_through_to_persisted_state`.
    {
        let (mut tree, strip_id, group_id) = single_group_tree();
        let state_dir = tempfile::tempdir().expect("state directory");
        let state_path = state_dir.path().join("state.json");
        tree.persist_path = Some(state_path.clone());

        // The host captures per-strip GroupRow pointers internally and wires
        // them into its `harvest_pending_maximize_changes` hook.
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);

        // If this fires the builder lost the seed — fix that before chasing the
        // harvest.
        {
            let st = chrome.borrow();
            let g = &st.tree().desktop.workspaces[0].strips[0].window_groups[0];
            assert_eq!(g.id, group_id);
            assert!(!g.maximized, "seed group must start un-maximized");
        }

        // Prime the same pending slot the per-tab Maximize2 press arm writes to.
        // The bar rect math is pinned by unit tests in
        // `crates/sdk/ui-core/src/widget/window_group.rs::tests`; this pins the writeback from
        // drained widget state to the chrome's `WorkspaceTree`.
        {
            let g = host
                .group_at_for_tests(strip_id, 0)
                .expect("strip-0 group-0 widget");
            assert_eq!(g.id, group_id, "widget id must match the seeded group");
            assert!(!g.maximized, "widget starts un-maximized to mirror state");
            // Same path the live binary hits: bar publishes pending_max, WindowGroup
            // drains it on the next event.
            g.publish_maximize_intent_for_tests();
        }
        // The drain runs inside `WindowGroup::handle_event`, so the widget tree
        // needs an event to drive it.
        let moved = ui::synthetic_input::cursor_moved(9000.0, 9000.0);
        pump_no_op_event(&mut host, strip_id, &moved);
        {
            let g = host
                .group_at_for_tests(strip_id, 0)
                .expect("strip-0 group-0 widget");
            assert!(g.maximized, "drain must flip widget.maximized true");
        }

        // The regression bracket: without the harvest call below this stays
        // false forever.
        {
            let st = chrome.borrow();
            let g = &st.tree().desktop.workspaces[0].strips[0].window_groups[0];
            assert!(
                !g.maximized,
                "before the harvest tick the persisted state lags the widget",
            );
        }

        // One tick, mirroring the per-frame Widget::update call.
        host.tick_pending_maximize_harvest_for_tests();

        {
            let st = chrome.borrow();
            let g = &st.tree().desktop.workspaces[0].strips[0].window_groups[0];
            assert!(
                g.maximized,
                "harvest tick must mirror the widget's maximize flip into \
                 the persisted WorkspaceTree (#105 writeback)",
            );
        }
        let restarted = shell::shell::persist::load_tree_from(&state_path)
            .expect("maximize-on writeback creates valid durable state");
        assert!(
            restarted.desktop.workspaces[0].strips[0].window_groups[0].maximized,
            "maximize-on must survive an immediate restart"
        );

        // Round-trip: pins that the harvest is bidirectional, not a
        // "latch true once" hack.
        {
            let g = host
                .group_at_for_tests(strip_id, 0)
                .expect("strip-0 group-0 widget");
            g.publish_maximize_intent_for_tests();
        }
        pump_no_op_event(&mut host, strip_id, &moved);
        {
            let g = host
                .group_at_for_tests(strip_id, 0)
                .expect("strip-0 group-0 widget");
            assert!(!g.maximized, "second drain must flip widget back to false");
        }
        host.tick_pending_maximize_harvest_for_tests();
        {
            let st = chrome.borrow();
            let g = &st.tree().desktop.workspaces[0].strips[0].window_groups[0];
            assert!(
                !g.maximized,
                "harvest must mirror the inverse flip, not just the rising edge",
            );
        }
        let restarted = shell::shell::persist::load_tree_from(&state_path)
            .expect("maximize-off writeback creates valid durable state");
        assert!(
            !restarted.desktop.workspaces[0].strips[0].window_groups[0].maximized,
            "maximize-off must survive an immediate restart"
        );
    }

    // Phase `shell_tick_leaves_persisted_maximize_alone_when_no_click_fired`.
    // A no-op tick must not perturb a group whose persisted `maximized =
    // true` came from disk: the take-pattern accessor yields `None` when
    // nothing flipped, and the harvest must skip those rather than
    // overwriting with the widget's default.
    {
        let (mut tree, strip_id, group_id) = single_group_tree();
        // `with_maximized(true)` runs at build time so the widget mirrors disk;
        // the harvest must then not overwrite the persisted true with a stale
        // false.
        tree.desktop.workspaces[0].strips[0].window_groups[0].maximized = true;

        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);

        {
            let g = host
                .group_at_for_tests(strip_id, 0)
                .expect("strip-0 group-0 widget");
            assert_eq!(g.id, group_id);
            assert!(
                g.maximized,
                "widget must be built from persisted state via `with_maximized`",
            );
        }

        // No click happened → no pending change → harvest is a no-op.
        host.tick_pending_maximize_harvest_for_tests();

        let st = chrome.borrow();
        let g = &st.tree().desktop.workspaces[0].strips[0].window_groups[0];
        assert!(
            g.maximized,
            "harvest with no pending change must not overwrite persisted true \
             with the widget's default false — pending_maximize_change is the \
             single source of truth for the writeback",
        );
    }
}
