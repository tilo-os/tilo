//! Per-frame wire-up coverage for the active-group reconciler.
//!
//! [`WorkspaceTree::nudge_active_group_off_closing`] advances the per-strip
//! active-group pointer off any group whose widget tree latched the 180 ms
//! close fade. The data layer is unit-tested elsewhere; this exercises the
//! *live binary* hookup — marking a group closing through the same
//! [`WindowGroup::start_closing`] entry `GroupRow::handle_event` reaches,
//! then one `tick_active_group_reconcile_for_tests` (mirroring production's
//! per-frame [`Widget::update`]).

use shell::shell::state::FreshId as _;
use std::collections::HashMap;

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};

/// A strip and its two group ids, in layout order.
type StripGroups = (StripId, [GroupId; 2]);

/// Two strips, two groups per strip, one tab per group. Returns the tree
/// plus `(strip_id, [group_id; 2])` per strip so callers can drive the
/// reconciler without re-walking the structure.
fn two_strip_two_group_tree() -> (WorkspaceTree, StripGroups, StripGroups) {
    let ws_id = WorkspaceId::new();
    let s0_id = StripId::new();
    let s1_id = StripId::new();
    // Deterministic group ids keep failure messages legible; only widget-tree
    // identity matters here.
    let a0: GroupId = 1001;
    let a1: GroupId = 1002;
    let b0: GroupId = 2001;
    let b1: GroupId = 2002;

    let mk_group = |id: GroupId, label: &str| WindowGroup {
        id,
        tabs: vec![Tab {
            id: id ^ 0xff, // stable derived tab id
            title: label.into(),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: label.into(),
            },
            app_title: None,
        }],
        active_tab: id ^ 0xff,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };

    let strip0 = Strip {
        id: s0_id,
        name: "Strip0".into(),
        window_groups: vec![mk_group(a0, "A0"), mk_group(a1, "A1")],
    };
    let strip1 = Strip {
        id: s1_id,
        name: "Strip1".into(),
        window_groups: vec![mk_group(b0, "B0"), mk_group(b1, "B1")],
    };
    let ws = {
        let mut workspace = Workspace::new(ws_id, "Test");
        workspace.strips = vec![strip0, strip1];
        workspace
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, s0_id);
    let mut active_group = HashMap::new();
    active_group.insert(s0_id, a0);
    active_group.insert(s1_id, b0);

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

    (tree, (s0_id, [a0, a1]), (s1_id, [b0, b1]))
}

#[test]
fn shell_tick_moves_the_active_group_off_closing_groups_or_clears_it() {
    // Phase `shell_tick_advances_active_group_off_closing_in_strip_0_only`.
    {
        let (tree, (s0_id, [a0, a1]), (s1_id, [b0, _b1])) = two_strip_two_group_tree();

        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);

        // If this fires, the builder reset the active_group map — fix that before
        // chasing the wire-up.
        {
            let st = chrome.borrow();
            assert_eq!(st.tree().desktop.active_group_for(s0_id), Some(a0));
            assert_eq!(st.tree().desktop.active_group_for(s1_id), Some(b0));
        }

        // No-op tick: nothing is closing, so the reconciler must leave both
        // pointers untouched.
        host.tick_active_group_reconcile_for_tests();
        {
            let st = chrome.borrow();
            assert_eq!(st.tree().desktop.active_group_for(s0_id), Some(a0));
            assert_eq!(st.tree().desktop.active_group_for(s1_id), Some(b0));
        }

        {
            let g = host
                .group_at_for_tests(s0_id, 0)
                .expect("strip-0 group-0 widget");
            assert_eq!(g.id, a0, "expected the first group of strip-0 to be A0");
            g.start_closing();
            assert!(g.is_closing(), "start_closing must latch is_closing()");
        }

        host.tick_active_group_reconcile_for_tests();

        // Without the wire-up strip-0's pointer would still point at A0.
        {
            let st = chrome.borrow();
            assert_eq!(
                st.tree().desktop.active_group_for(s0_id),
                Some(a1),
                "active-group pointer must advance to A1 once A0 enters the close fade",
            );
            assert_eq!(
                st.tree().desktop.active_group_for(s1_id),
                Some(b0),
                "strip-1 pointer must not change when only strip-0 has a closing group",
            );
        }
    }

    // Phase `shell_tick_clears_pointer_when_every_group_in_strip_is_closing`.
    {
        // With both groups in a strip closing, the pointer must be *removed* from
        // the map rather than clamped to a corpse.
        let (tree, (s0_id, [a0, a1]), (s1_id, [b0, _])) = two_strip_two_group_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);

        host.group_at_for_tests(s0_id, 0)
            .expect("strip-0 group-0")
            .start_closing();
        host.group_at_for_tests(s0_id, 1)
            .expect("strip-0 group-1")
            .start_closing();

        host.tick_active_group_reconcile_for_tests();

        let st = chrome.borrow();
        assert_eq!(
            st.tree().desktop.active_group_for(s0_id),
            None,
            "every-group-closing must drop the strip-0 pointer entirely; \
             the data-layer reconciler clears it rather than dangle",
        );
        // Survivors we don't drive: they keep the tree shape symmetric and guard
        // the strip-1-untouched assertion below.
        let _ = (a0, a1);
        assert_eq!(
            st.tree().desktop.active_group_for(s1_id),
            Some(b0),
            "strip-1 must remain unaffected even when strip-0 fully empties",
        );
    }
}
