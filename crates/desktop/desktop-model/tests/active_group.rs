//! The per-strip active-group pointer and the persisted shape of groups.

use std::collections::HashMap;

use tilo_desktop_model::{
    GroupId, Layout, Strip, StripId, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};

/// A single-workspace, single-strip tree carrying `n` empty window groups
/// with ids `1..=n`.
fn tree_with_n_groups(n: u64) -> (Layout, StripId, Vec<GroupId>) {
    let workspace_id = WorkspaceId::from_u128(100);
    let strip_id = StripId(200);
    let group_ids: Vec<GroupId> = (1..=n).collect();
    let mut strip = Strip::new(strip_id, "Main");
    strip.window_groups = group_ids
        .iter()
        .map(|gid| WindowGroup {
            id: *gid,
            tabs: vec![],
            active_tab: 0,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        })
        .collect();
    let mut ws = Workspace::new(workspace_id, "Test");
    ws.strips.push(strip);
    let mut active_strip = HashMap::new();
    active_strip.insert(workspace_id, strip_id);
    let tree = Layout {
        workspaces: vec![ws],
        active: workspace_id,
        active_strip,
        ..Layout::default()
    };
    (tree, strip_id, group_ids)
}

tilo_test_env::scenario! {
    #[allow(
        clippy::items_after_statements,
        reason = "the nudge phase keeps its original early-return body as a nested fn"
    )]
    fn active_group_follows_closing_groups_and_resolves_neighbours;

    phase a_strip_whose_focused_group_left_it_focuses_the_window_it_still_holds;
    {
        // The person was in B, then carried B to another strip. The strip B
        // left still names B as its focused group; a strip must focus a
        // window it holds, or going back to it leaves no window in front.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, b);
        let elsewhere = StripId(201);
        tree.workspaces[0].strips.push(Strip::new(elsewhere, "Below"));
        let moved = tree.workspaces[0].strips[0].window_groups.remove(1);
        tree.workspaces[0].strips[1].window_groups.push(moved);

        assert_eq!(
            tree.resolve_active_group(strip_id, |_| false),
            Some(a),
            "the strip focuses the window it still holds, not the one that left"
        );
        assert_eq!(
            tree.active_group_for(strip_id),
            None,
            "and the pointer, read straight, never names a window the strip no longer holds"
        );
    }

    phase a_closing_active_group_hands_focus_to_its_left_neighbour_whatever_was_focused_before;
    {
        // [A, B, C, D, E]: the person was in A, then went to E. When E starts
        // closing, focus goes to D beside it, never back to A at the strip's
        // start, which would pan the whole strip there.
        let (mut tree, strip_id, ids) = tree_with_n_groups(5);
        let (first, beside, last) = (ids[0], ids[3], ids[4]);
        tree.set_active_group(strip_id, first);
        tree.set_active_group(strip_id, last);
        assert_eq!(
            tree.nudge_active_group_off_closing(strip_id, &[last]),
            Some(beside)
        );

        // At the strip's start, the neighbour on the right.
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        tree.set_active_group(strip_id, ids[2]);
        tree.set_active_group(strip_id, ids[0]);
        assert_eq!(
            tree.nudge_active_group_off_closing(strip_id, &[ids[0]]),
            Some(ids[1])
        );

        // A neighbour that is itself closing is skipped, still leftwards first.
        let (mut tree, strip_id, ids) = tree_with_n_groups(4);
        let (first, second, third) = (ids[0], ids[1], ids[2]);
        tree.set_active_group(strip_id, third);
        assert_eq!(
            tree.resolve_active_group(strip_id, |id| id == third || id == second),
            Some(first)
        );
    }
}

#[test]
fn active_strip_for_resolves_recorded_or_falls_back_to_first() {
    let work_id = WorkspaceId::from_u128(1);
    let other_id = WorkspaceId::from_u128(2);
    let mut work = Workspace::new(work_id, "Work");
    work.strips = vec![
        Strip::new(StripId(10), "First"),
        Strip::new(StripId(11), "Second"),
    ];
    let mut other = Workspace::new(other_id, "Other");
    other.strips = vec![Strip::new(StripId(20), "Main")];
    let mut tree = Layout {
        workspaces: vec![work, other],
        active: work_id,
        ..Layout::default()
    };

    // Unrecorded: falls back to the workspace's first strip.
    assert_eq!(
        tree.active_strip_for(&tree.workspaces[0]),
        Some(StripId(10))
    );
    assert_eq!(tree.active_strip_id(), Some(StripId(10)));

    // Recorded strip that exists: returns it.
    tree.active_strip.insert(work_id, StripId(11));
    assert_eq!(
        tree.active_strip_for(&tree.workspaces[0]),
        Some(StripId(11))
    );
    assert_eq!(tree.active_strip_id(), Some(StripId(11)));

    // Stale recorded strip: falls back to the first surviving strip.
    tree.active_strip.insert(work_id, StripId(99));
    assert_eq!(
        tree.active_strip_for(&tree.workspaces[0]),
        Some(StripId(10))
    );
    assert_eq!(tree.active_strip_id(), Some(StripId(10)));

    // Non-active workspace resolves its own strip independently of `tree.active`.
    assert_eq!(
        tree.active_strip_for(&tree.workspaces[1]),
        Some(StripId(20))
    );

    // Empty workspace: returns `None` even with a stale recorded strip.
    tree.workspaces[0].strips.clear();
    assert_eq!(tree.active_strip_for(&tree.workspaces[0]), None);
    assert_eq!(tree.active_strip_id(), None);
}
