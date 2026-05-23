//! Per-frame wire-up coverage for the active-group reconciler.
//!
//! [`WorkspaceTree::nudge_active_group_off_closing`] (committed in
//! `f40e43e`) advances the per-strip active-group pointer off any group
//! whose widget tree latched the 180 ms close fade. The reconciler is
//! fully unit-tested at the data layer; this file exercises the *live
//! binary* hookup that calls it each tick from the widget tree's
//! observed `is_closing()` state.
//!
//! Test harness:
//!   1. Build a 2-strip tree with two groups in each strip; record the
//!      first group of each strip as "active" in
//!      [`WorkspaceTree::active_group`].
//!   2. Hand it to [`build_nirios_overlay_host_with_chrome`] so the
//!      shell captures per-strip [`GroupRow`] pointers and wires them
//!      into the host.
//!   3. Mark the active group of strip-0 as closing via the public
//!      [`WindowGroup::start_closing`] entry point (the same call site
//!      `GroupRow::handle_event` reaches when the user closes the last
//!      tab — no test-only escape into private `closing_at` setters).
//!   4. Drive one shell tick by invoking the host's
//!      `tick_active_group_reconcile_for_tests` helper (mirrors the
//!      [`Widget::update`] call the production `App` runs each frame).
//!   5. Assert the strip-0 pointer advanced to the surviving sibling
//!      while strip-1's pointer is untouched.
//!
//! Without the per-frame wire-up the strip-0 pointer would still resolve
//! to the closing group for the duration of the fade, and the assertion
//! at the end would fail.

use std::collections::HashMap;

use shell::shell::build_nirios_overlay_host_with_chrome;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};

/// Build a workspace tree with two strips, two groups per strip, one tab
/// per group. Returns the tree alongside `(strip_id, [group_id; 2])`
/// triples for each strip so callers can drive the reconciler without
/// re-walking the structure.
fn two_strip_two_group_tree() -> (
    WorkspaceTree,
    (StripId, [GroupId; 2]),
    (StripId, [GroupId; 2]),
) {
    let ws_id = WorkspaceId::new();
    let s0_id = StripId::new();
    let s1_id = StripId::new();
    // Use deterministic group ids so the assertion failure messages stay
    // legible. They're plain `u64`s — only widget-tree identity matters
    // here, and the build helper passes them straight through.
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
        }],
        active_tab: id ^ 0xff,
        width: WindowWidth::Normal,
        maximized: false,
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
    let ws = Workspace {
        id: ws_id,
        name: "Test".into(),
        strips: vec![strip0, strip1],
        roles: vec![],
        workspace_files: vec![],
        collaborators: vec![],
        grants: HashMap::new(),
        shared_surfaces: HashMap::new(),
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, s0_id);
    let mut active_group = HashMap::new();
    active_group.insert(s0_id, a0);
    active_group.insert(s1_id, b0);

    let tree = WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
        ..Default::default()
    };

    (tree, (s0_id, [a0, a1]), (s1_id, [b0, b1]))
}

#[test]
fn shell_tick_advances_active_group_off_closing_in_strip_0_only() {
    let (tree, (s0_id, [a0, a1]), (s1_id, [b0, _b1])) = two_strip_two_group_tree();

    // Build the live shell widget tree. The host captures per-strip
    // GroupRow pointers internally and wires them into its per-frame
    // `reconcile_active_group_off_closing` hook.
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);

    // Pre-condition sanity: chrome's data tree carries our seeded
    // pointers (a0 in strip-0, b0 in strip-1). If this fires the
    // builder reset the active_group map somewhere — fix that before
    // chasing the wire-up.
    {
        let st = chrome.borrow();
        assert_eq!(st.tree().active_group_for(s0_id), Some(a0));
        assert_eq!(st.tree().active_group_for(s1_id), Some(b0));
    }

    // Drive a no-op tick: nothing is closing, so the reconciler must
    // leave both pointers untouched. Catches "the wire-up sets
    // pointers it shouldn't" regressions.
    host.tick_active_group_reconcile_for_tests();
    {
        let st = chrome.borrow();
        assert_eq!(st.tree().active_group_for(s0_id), Some(a0));
        assert_eq!(st.tree().active_group_for(s1_id), Some(b0));
    }

    // Latch the close fade on strip-0's active group via the public
    // `WindowGroup::start_closing` entry point — the same call the
    // production GroupRow path reaches when the user closes the last
    // tab. No `pub(crate)` escape needed.
    {
        let g = host
            .group_at_for_tests(s0_id, 0)
            .expect("strip-0 group-0 widget");
        assert_eq!(g.id, a0, "expected the first group of strip-0 to be A0");
        g.start_closing();
        assert!(g.is_closing(), "start_closing must latch is_closing()");
    }

    // One tick — mirrors the per-frame Widget::update call the App
    // runs. The reconciler harvests is_closing() ids per strip and
    // calls `tree.nudge_active_group_off_closing` for each non-empty
    // set.
    host.tick_active_group_reconcile_for_tests();

    // Strip-0's pointer must advance to A1 — the surviving right-hand
    // sibling. Without the wire-up it would still point at A0 (the
    // closing group).
    {
        let st = chrome.borrow();
        assert_eq!(
            st.tree().active_group_for(s0_id),
            Some(a1),
            "active-group pointer must advance to A1 once A0 enters the close fade",
        );
        // Strip-1 is untouched: nothing in it is closing, so the
        // reconciler must not have rewritten its pointer.
        assert_eq!(
            st.tree().active_group_for(s1_id),
            Some(b0),
            "strip-1 pointer must not change when only strip-0 has a closing group",
        );
    }
}

#[test]
fn shell_tick_clears_pointer_when_every_group_in_strip_is_closing() {
    // Edge case the data-layer reconciler explicitly handles
    // ("active_group_clears_when_every_group_is_closing" in
    // `state::active_group_tests`). Verify the wire-up surfaces it
    // end-to-end: when both groups in a strip are closing the pointer
    // is *removed* from the map rather than clamped to a corpse.
    let (tree, (s0_id, [a0, a1]), (s1_id, [b0, _])) = two_strip_two_group_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);

    // Latch close fade on both groups in strip-0.
    host.group_at_for_tests(s0_id, 0)
        .expect("strip-0 group-0")
        .start_closing();
    host.group_at_for_tests(s0_id, 1)
        .expect("strip-0 group-1")
        .start_closing();

    host.tick_active_group_reconcile_for_tests();

    let st = chrome.borrow();
    assert_eq!(
        st.tree().active_group_for(s0_id),
        None,
        "every-group-closing must drop the strip-0 pointer entirely; \
         the data-layer reconciler clears it rather than dangle",
    );
    // Suppress unused-binding warnings on the survivors we don't drive
    // in this test — they keep the tree shape symmetric and act as a
    // regression guard for the strip-1-untouched assertion below.
    let _ = (a0, a1);
    assert_eq!(
        st.tree().active_group_for(s1_id),
        Some(b0),
        "strip-1 must remain unaffected even when strip-0 fully empties",
    );
}
