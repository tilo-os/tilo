//! The per-strip active-group pointer and the persisted shape of groups.

use std::collections::HashMap;

use tilo_desktop_model::{
    DesktopTree, GroupId, RoomId, Strip, StripId, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};

/// A single-workspace, single-strip tree carrying `n` empty window groups
/// with ids `1..=n`.
fn tree_with_n_groups(n: u64) -> (DesktopTree, StripId, Vec<GroupId>) {
    let ws_id = WorkspaceId(100);
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
    let mut ws = Workspace::new(ws_id, "Test");
    ws.set_room(RoomId::new());
    ws.strips.push(strip);
    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let tree = DesktopTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        ..DesktopTree::default()
    };
    (tree, strip_id, group_ids)
}

#[test]
#[allow(
    clippy::items_after_statements,
    reason = "the nudge phase keeps its original early-return body as a nested fn"
)]
fn active_group_follows_closing_groups_and_resolves_neighbours() {
    // Phase `active_group_advances_when_active_group_starts_closing`.
    {
        // Two groups [A, B] with A active. When A starts closing, the
        // pointer must immediately advance to B so focus / keyboard /
        // presence stop targeting the fading-away group.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, a);
        assert_eq!(tree.active_group_for(strip_id), Some(a));

        let landed = tree.nudge_active_group_off_closing(strip_id, &[a]);

        assert_eq!(landed, Some(b));
        assert_eq!(
            tree.active_group_for(strip_id),
            Some(b),
            "active-group pointer must advance to the next surviving sibling at fade-start, not after the 180 ms fade",
        );
    }
    // Phase `active_group_advances_to_previous_when_last_group_starts_closing`.
    {
        // B (rightmost) is closing with no survivor to its right, so
        // the reconciler falls back to A on the left.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, b);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[b]);

        assert_eq!(landed, Some(a));
        assert_eq!(tree.active_group_for(strip_id), Some(a));
    }
    // Phase `active_group_doesnt_advance_when_non_active_group_closes`.
    {
        // B closes while A is active — a stray reconcile pass on a
        // group not itself closing is a no-op, so the shell can call
        // it freely from a per-tick loop.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, a);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[b]);

        assert_eq!(landed, Some(a));
        assert_eq!(tree.active_group_for(strip_id), Some(a));
    }
    // Phase `active_group_clears_when_every_group_is_closing`.
    {
        // Every group in the strip is closing — the pointer can't land
        // anywhere, so the entry is dropped rather than clamped to a
        // closing-group id.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, a);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[a, b]);

        assert_eq!(landed, None);
        assert_eq!(tree.active_group_for(strip_id), None);
    }
    // Phase `nudge_is_noop_when_strip_has_no_recorded_pointer`.
    fn nudge_is_noop_when_strip_has_no_recorded_pointer() {
        // No call to `set_active_group` first. The reconciler must
        // return `None` and not insert a phantom entry after the
        // first close.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, _b) = (ids[0], ids[1]);
        let landed = tree.nudge_active_group_off_closing(strip_id, &[a]);
        assert_eq!(landed, None);
        assert_eq!(tree.active_group_for(strip_id), None);
    }
    nudge_is_noop_when_strip_has_no_recorded_pointer();
    // Phase `resolve_active_group_returns_recorded_when_not_closing`.
    {
        // Pure read path: with the recorded pointer not in the closing
        // set, `resolve_active_group` returns it unchanged and, unlike
        // `nudge_*`, doesn't mutate the map.
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        tree.set_active_group(strip_id, b);
        let resolved = tree.resolve_active_group(strip_id, |id| id == a || id == c);
        assert_eq!(resolved, Some(b));
        // Map is unchanged.
        assert_eq!(tree.active_group_for(strip_id), Some(b));
    }
    // Phase `resolve_active_group_walks_right_then_left`.
    {
        // Three groups [A, B, C] with B active and closing. The
        // reconciler must prefer C (right) over A (left) so focus
        // moves "forward" rather than ping-ponging back to the start.
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        let (_a, b, c) = (ids[0], ids[1], ids[2]);
        tree.set_active_group(strip_id, b);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[b]);

        assert_eq!(landed, Some(c));
    }
}

#[test]
fn window_group_json_round_trips() {
    // Phase `window_group_maximized_round_trips_through_json`.
    {
        // Window state must survive restart: a fresh tree starts
        // un-maximized, and flipping `maximized = true` on one group
        // must serialize and deserialize cleanly.
        let (mut tree, _, _) = tree_with_n_groups(1);
        let ws = tree.workspaces.first_mut().expect("seed has workspaces");
        let strip = ws.strips.first_mut().expect("seed has strips");
        let group = strip.window_groups.first_mut().expect("seed has groups");
        assert!(!group.maximized);
        group.maximized = true;

        let json = serde_json::to_string(&tree).unwrap();
        let round: DesktopTree = serde_json::from_str(&json).unwrap();
        assert!(round.workspaces[0].strips[0].window_groups[0].maximized);
    }
}
