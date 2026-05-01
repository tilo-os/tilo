//! Per-frame wire-up coverage for the `WindowGroup::maximized` writeback.
//!
//! The widget side toggles `WindowGroup::maximized` in memory when the
//! user clicks the tab-bar Maximize2 glyph (or the floating restore /
//! close-while-maximized chrome). Audit task #105 already added the
//! persisted-state field + the `with_maximized` builder, but a runtime
//! click stayed widget-local until the shell harvests it back into the
//! `WorkspaceTree`. This file pins that harvest end-to-end:
//!
//!   1. Build a single-group tree with `maximized = false` and seed the
//!      shell host so the per-strip [`GroupRow`] pointers are captured.
//!   2. Drive the live click pipeline by directly priming
//!      `WindowGroup::bar.pending_max` (the production code path the
//!      tab-bar glyph press writes to) and pumping a no-op cursor-move
//!      through the widget's `handle_event` so the drain block flips
//!      `maximized` *and* sets `pending_maximize_change = Some(true)`.
//!   3. Tick the host's
//!      [`tick_pending_maximize_harvest_for_tests`] helper — the
//!      production [`Widget::update`] runs the same harvest each frame.
//!   4. Assert the persisted state's `WindowGroup::maximized` field
//!      flipped to `true` (+ the inverse round-trip flips it back).
//!
//! Without the per-frame writeback the on-screen toggle is in-memory
//! only and resets to `false` on the next shell restart, which is the
//! exact regression #105 closes.

use std::collections::HashMap;

use quick_desktop::shell::build_nirios_overlay_host_with_chrome;
use quick_desktop::shell::overlay_host::ShellOverlayHost;
use quick_desktop::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
// `handle_event` / `build_layout` / `apply_layout` live on the `Widget`
// trait — must be in scope at the call site for the integration test to
// drive the drain block.
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;
use vello::kurbo::Point;

/// Drive one no-op `CursorMoved` event through the strip-0/group-0
/// widget. Builds a tiny self-contained taffy tree around the group so
/// `handle_event`'s `taffy.children(node_id)` call has the two
/// (bar, content) children it expects. The event itself is at (9000,
/// 9000) — well outside any widget's rect — so the only behaviour
/// exercised is the drain block at the bottom of `handle_event`, which
/// flips `maximized` and sets `pending_maximize_change` from a primed
/// `bar.pending_max`.
fn pump_no_op_event(
    host: &mut ShellOverlayHost,
    strip_id: StripId,
    event: &winit::event::WindowEvent,
) {
    use std::collections::HashMap;
    use taffy::{NodeId, Style, TaffyTree, Size, AvailableSpace};
    use taffy::prelude::TaffyMaxContent;

    let g = host
        .group_at_for_tests(strip_id, 0)
        .expect("strip-0 group-0 widget");
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(Style::default(), &[group_node])
        .unwrap();
    let map_for_measure = node_map.clone();
    let mut tr = quick_ui::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(800.0),
                height: AvailableSpace::Definite(600.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_for_measure.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .or_else(|_| taffy.compute_layout(root, taffy::Size::MAX_CONTENT))
        .expect("layout must compute");
    g.apply_layout(&taffy, group_node, Point::ZERO);

    let mut state = automerge::AutoCommit::new();
    let root_id = automerge::ROOT;
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    g.handle_event(
        event,
        1.0,
        &winit::event::Modifiers::default(),
        &mut state,
        &root_id,
        &mut handled,
        &mut focus_target,
        group_node,
        &taffy,
    );
}

/// Build a one-workspace, one-strip, one-group tree with a single tab.
/// The simplest topology that exercises the harvest — and the one that
/// would silently swallow a missing writeback (no sibling group to mask
/// a misrouted update).
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
        // The whole point of the test: this starts false and must
        // become true after the click + harvest.
        maximized: false,
    };
    let strip = Strip {
        id: strip_id,
        name: "Solo".into(),
        window_groups: vec![group],
    };
    let ws = Workspace {
        id: ws_id,
        name: "Test".into(),
        strips: vec![strip],
        roles: vec![],
        workspace_files: vec![],
        collaborators: vec![],
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, group_id);

    let tree = WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
    };

    (tree, strip_id, group_id)
}

#[test]
fn shell_tick_writes_maximize_click_through_to_persisted_state() {
    let (tree, strip_id, group_id) = single_group_tree();

    // Build the live shell widget tree. The host captures per-strip
    // GroupRow pointers internally and wires them into its per-frame
    // `harvest_pending_maximize_changes` hook.
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);

    // Pre-condition: the persisted state starts un-maximized. If this
    // fires the builder lost the seed somewhere — fix that before
    // chasing the harvest.
    {
        let st = chrome.borrow();
        let g = &st.tree.workspaces[0].strips[0].window_groups[0];
        assert_eq!(g.id, group_id);
        assert!(!g.maximized, "seed group must start un-maximized");
    }

    // Drive the live click pipeline by priming the same pending slot
    // the per-tab Maximize2 press arm writes to. We avoid a synthetic
    // pointer-event walk through `apply_layout` here because the bar
    // rect math is already pinned by the unit tests in
    // `quick-ui/src/widget/window_group.rs::tests` — what *this* test
    // pins is the writeback from the widget's drained state to the
    // chrome's `WorkspaceTree`.
    {
        let g = host
            .group_at_for_tests(strip_id, 0)
            .expect("strip-0 group-0 widget");
        assert_eq!(g.id, group_id, "widget id must match the seeded group");
        assert!(!g.maximized, "widget starts un-maximized to mirror state");
        // Same path the live binary hits when the user clicks the
        // glyph: bar publishes pending_max → WindowGroup drains it on
        // the next event. We force the drain by following the same
        // pattern the unit test uses.
        g.publish_maximize_intent_for_tests();
    }
    // The drain happens inside `WindowGroup::handle_event`, which only
    // runs when the widget tree gets an event. Send a no-op
    // CursorMoved through the host to drive it.
    use winit::dpi::PhysicalPosition;
    use winit::event::WindowEvent;
    let moved = WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: PhysicalPosition::new(9000.0, 9000.0),
    };
    pump_no_op_event(&mut host, strip_id, &moved);
    {
        let g = host
            .group_at_for_tests(strip_id, 0)
            .expect("strip-0 group-0 widget");
        assert!(g.maximized, "drain must flip widget.maximized true");
    }

    // Pre-harvest: persisted state still mirrors the *old* (false)
    // value — the writeback hasn't run yet. This is the regression
    // bracket: without the harvest call below the assertion would
    // stay `false` forever even though the widget says `true`.
    {
        let st = chrome.borrow();
        let g = &st.tree.workspaces[0].strips[0].window_groups[0];
        assert!(
            !g.maximized,
            "before the harvest tick the persisted state lags the widget",
        );
    }

    // One tick — mirrors the per-frame Widget::update call the App
    // runs. The harvest drains `take_pending_maximize_change` for
    // every group and writes the new value into the matching tree
    // entry.
    host.tick_pending_maximize_harvest_for_tests();

    // Post-harvest: persisted state now matches the widget — this is
    // the writeback #105 needs.
    {
        let st = chrome.borrow();
        let g = &st.tree.workspaces[0].strips[0].window_groups[0];
        assert!(
            g.maximized,
            "harvest tick must mirror the widget's maximize flip into \
             the persisted WorkspaceTree (#105 writeback)",
        );
    }

    // Round-trip: a second click flips back to false, harvest mirrors
    // the inverse value. Pins that the harvest is bidirectional, not
    // a "latch true once" hack.
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
        let g = &st.tree.workspaces[0].strips[0].window_groups[0];
        assert!(
            !g.maximized,
            "harvest must mirror the inverse flip, not just the rising edge",
        );
    }
}

/// A no-op tick (no clicks pending) must not perturb a group whose
/// persisted `maximized = true` was loaded from disk. Catches
/// "the harvest writes Some(false) when the widget has nothing to
/// report" regressions — the take-pattern accessor must yield `None`
/// when no flip happened, and the harvest must skip groups that yield
/// `None` rather than overwriting their persisted value with the
/// widget's default.
#[test]
fn shell_tick_leaves_persisted_maximize_alone_when_no_click_fired() {
    let (mut tree, strip_id, group_id) = single_group_tree();
    // Pre-set persisted state to maximized. `with_maximized(true)` runs
    // at build time so the widget mirrors disk; the harvest must then
    // *not* overwrite the persisted true with a stale false.
    tree.workspaces[0].strips[0].window_groups[0].maximized = true;

    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);

    // Sanity: the widget got the persisted value via with_maximized.
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
    let g = &st.tree.workspaces[0].strips[0].window_groups[0];
    assert!(
        g.maximized,
        "harvest with no pending change must not overwrite persisted true \
         with the widget's default false — pending_maximize_change is the \
         single source of truth for the writeback",
    );
}
