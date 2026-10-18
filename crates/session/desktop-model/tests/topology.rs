//! Moving and splitting tabs edits the tree and reports exactly what moved.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use tilo_desktop_model::{
    DesktopTree, DropTarget, GroupShape, IdMinter, Strip, StripId, Tab, TileContent, WindowGroup,
    WindowWidth, Workspace, WorkspaceId,
};

fn tab(id: u64) -> Tab {
    Tab {
        id,
        title: format!("tab {id}"),
        tile: TileContent::LocalApp {
            app_id: "org.example.App".into(),
            label: "App".into(),
            note: String::new(),
        },
    }
}

fn group(id: u64, tabs: &[u64]) -> WindowGroup {
    WindowGroup {
        id,
        tabs: tabs.iter().copied().map(tab).collect(),
        active_tab: tabs[0],
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// Strip 1 holds groups 10 [100, 101] and 11 [110]; strip 2 holds 20 [200].
fn tree() -> DesktopTree {
    let mut ws = Workspace::new(WorkspaceId(1), "Desk");
    let mut a = Strip::new(StripId(1), "A");
    a.window_groups = vec![group(10, &[100, 101]), group(11, &[110])];
    let mut b = Strip::new(StripId(2), "B");
    b.window_groups = vec![group(20, &[200])];
    ws.strips = vec![a, b];
    DesktopTree {
        workspaces: vec![ws],
        active: WorkspaceId(1),
        ..DesktopTree::default()
    }
}

fn layout(tree: &DesktopTree, strip: u64) -> Vec<(u64, Vec<u64>)> {
    tree.workspaces[0]
        .strips
        .iter()
        .find(|s| s.id == StripId(strip))
        .unwrap()
        .window_groups
        .iter()
        .map(|g| (g.id, g.tabs.iter().map(|t| t.id).collect()))
        .collect()
}

#[test]
fn tab_drops_and_splits_reshape_groups_or_change_nothing() {
    // Phase `a_tab_dropped_in_a_gap_becomes_its_own_group_and_empties_its_source`.
    {
        let mut tree = tree();
        let moved = tree
            .move_tab_across_strips(
                StripId(1),
                StripId(2),
                110,
                99,
                DropTarget::GroupGap { gap_index: 7 },
            )
            .unwrap();
        assert_eq!(moved.source_group, 11);
        assert!(moved.source_group_removed);
        assert_eq!(moved.target, DropTarget::GroupGap { gap_index: 1 });
        assert_eq!(layout(&tree, 1), vec![(10, vec![100, 101])]);
        assert_eq!(layout(&tree, 2), vec![(20, vec![200]), (99, vec![110])]);
    }
    // Phase `a_tab_dropped_into_a_group_lands_at_its_index_and_becomes_active`.
    {
        let mut tree = tree();
        let moved = tree
            .move_tab_across_strips(
                StripId(1),
                StripId(2),
                100,
                99,
                DropTarget::ExistingGroup {
                    group_id: 20,
                    tab_index: 0,
                },
            )
            .unwrap();
        assert!(!moved.source_group_removed);
        assert_eq!(layout(&tree, 2), vec![(20, vec![100, 200])]);
        let source = &tree.workspaces[0].strips[0].window_groups[0];
        assert_eq!(source.active_tab, 101, "the source promotes a sibling");
        assert_eq!(
            tree.workspaces[0].strips[1].window_groups[0].active_tab,
            100
        );
    }
    // Phase `a_stale_target_or_same_strip_move_changes_nothing`.
    {
        let mut tree = tree();
        let stale = DropTarget::ExistingGroup {
            group_id: 404,
            tab_index: 0,
        };
        assert!(
            tree.move_tab_across_strips(StripId(1), StripId(2), 100, 99, stale)
                .is_none()
        );
        let gap = DropTarget::GroupGap { gap_index: 0 };
        assert!(
            tree.move_tab_across_strips(StripId(1), StripId(1), 100, 99, gap)
                .is_none()
        );
        assert!(
            tree.move_tab_across_strips(StripId(1), StripId(2), 999, 99, gap)
                .is_none()
        );
        assert_eq!(layout(&tree, 1), layout(&self::tree(), 1));
        assert_eq!(layout(&tree, 2), layout(&self::tree(), 2));
    }
    // Phase `splitting_mints_a_group_and_refuses_a_lone_tab`.
    {
        let mut tree = tree();
        let mut ids = IdMinter::resume(0, 500);
        let moved = tree
            .split_tab_into_new_group(StripId(1), 100, 1, &mut ids)
            .unwrap();
        assert_eq!(moved.new_group.id, 500);
        assert_eq!(
            layout(&tree, 1),
            vec![(10, vec![101]), (500, vec![100]), (11, vec![110])]
        );
        assert!(
            tree.split_tab_into_new_group(StripId(1), 110, 0, &mut ids)
                .is_none(),
            "a group's only tab has nothing to split from"
        );
    }
}

#[test]
fn a_live_strip_shape_rearranges_tabs_without_touching_their_content() {
    let mut tree = tree();
    tree.workspaces[0].strips[0].window_groups[1].maximized = true;
    let shape = [
        GroupShape {
            id: 11,
            tab_ids: vec![110, 101],
            active_tab: 101,
            width: WindowWidth::Fraction(700),
        },
        GroupShape {
            id: 12,
            tab_ids: vec![100],
            active_tab: 100,
            width: WindowWidth::Normal,
        },
    ];
    let groups = tree.reconcile_strip_topology(StripId(1), &shape).unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(
        layout(&tree, 1),
        vec![(11, vec![110, 101]), (12, vec![100])]
    );
    let first = &tree.workspaces[0].strips[0].window_groups[0];
    assert!(first.maximized, "maximized stays with its group id");
    assert_eq!(first.width, WindowWidth::Fraction(700));
    assert!(
        matches!(&first.tabs[1].tile, TileContent::LocalApp { app_id, .. } if app_id == "org.example.App")
    );
    assert_eq!(tree.active_group_for(StripId(1)), Some(11));
    assert!(
        tree.reconcile_strip_topology(StripId(1), &shape).is_none(),
        "an unchanged shape reports nothing"
    );
}

#[test]
fn removing_a_groups_last_tab_removes_the_group_and_moves_the_active_pointer() {
    let mut tree = tree();
    tree.set_active_group(StripId(1), 10);
    let removed = tree.remove_tab(101).unwrap();
    assert!(!removed.group_removed);
    assert_eq!(
        tree.workspaces[0].strips[0].window_groups[0].active_tab,
        100
    );
    let removed = tree.remove_tab(100).unwrap();
    assert!(removed.group_removed);
    assert_eq!(
        (removed.strip, removed.group, removed.tab.id),
        (StripId(1), 10, 100)
    );
    assert_eq!(layout(&tree, 1), vec![(11, vec![110])]);
    assert_eq!(tree.active_group_for(StripId(1)), Some(11));
    assert!(tree.remove_tab(100).is_none());
}

#[test]
fn a_live_strip_shape_that_loses_or_invents_a_tab_is_refused() {
    let mut tree = tree();
    let lost = [GroupShape {
        id: 10,
        tab_ids: vec![100, 101],
        active_tab: 100,
        width: WindowWidth::Normal,
    }];
    assert!(tree.reconcile_strip_topology(StripId(1), &lost).is_none());
    let invented = [
        GroupShape {
            id: 10,
            tab_ids: vec![100, 101, 999],
            active_tab: 100,
            width: WindowWidth::Normal,
        },
        GroupShape {
            id: 11,
            tab_ids: vec![110],
            active_tab: 110,
            width: WindowWidth::Normal,
        },
    ];
    assert!(
        tree.reconcile_strip_topology(StripId(1), &invented)
            .is_none()
    );
    assert_eq!(
        layout(&tree, 1),
        vec![(10, vec![100, 101]), (11, vec![110])]
    );
}

#[test]
fn closing_the_front_tab_returns_to_the_tab_it_came_from() {
    let mut tree = tree();
    // Group 10 shows 100; a third tab opens beside it and takes the front.
    let group = &mut tree.workspaces[0].strips[0].window_groups[0];
    group.tabs.push(tab(102));
    group.activate_tab(102);
    // 100 was the one the person came from, though 101 sits beside it.
    tree.close_tab(102).unwrap();
    assert_eq!(
        tree.workspaces[0].strips[0].window_groups[0].active_tab, 100,
        "closing returns to the previously shown tab"
    );
}

#[test]
fn history_skips_tabs_that_closed_and_plain_removal_keeps_the_index_rule() {
    let mut tree = tree();
    let group = &mut tree.workspaces[0].strips[0].window_groups[0];
    group.tabs.push(tab(102));
    group.tabs.push(tab(103));
    group.activate_tab(101);
    group.activate_tab(102);
    group.activate_tab(103);
    // 102 closes while behind, then 103 closes: 101 is the most recent
    // survivor before it, not 100.
    tree.close_tab(102).unwrap();
    tree.close_tab(103).unwrap();
    assert_eq!(
        tree.workspaces[0].strips[0].window_groups[0].active_tab,
        101
    );
    // Restructuring removal ignores history: the tab now at the index.
    let mut plain = self::tree();
    let group = &mut plain.workspaces[0].strips[0].window_groups[0];
    group.activate_tab(101);
    group.activate_tab(100);
    plain.remove_tab(100).unwrap();
    assert_eq!(
        plain.workspaces[0].strips[0].window_groups[0].active_tab,
        101
    );
}

#[test]
fn closing_a_window_returns_to_the_window_you_came_from_not_its_right_neighbour() {
    // [10 | 11]: the person works in 10, opens a window between 10 and 11
    // (where a new window lands), looks at it, closes it.
    let mut tree = tree();
    tree.set_active_group(StripId(1), 10);
    let strip = &mut tree.workspaces[0].strips[0];
    strip.window_groups.insert(1, group(12, &[120]));
    tree.set_active_group(StripId(1), 12);
    let removed = tree.close_tab(120).unwrap();
    assert!(removed.group_removed);
    assert_eq!(
        tree.active_group_for(StripId(1)),
        Some(10),
        "back to the window the person came from, not 11, which now sits at the index"
    );
    assert!(
        tree.group_focus_history[&StripId(1)]
            .iter()
            .all(|g| *g != 12),
        "a closed group is never returned to"
    );

    // History skips groups that closed meanwhile: 10 -> 11 -> 12, close 11
    // while behind, then close 12: back to 10.
    let mut tree = self::tree();
    tree.workspaces[0].strips[0]
        .window_groups
        .push(group(12, &[120]));
    tree.set_active_group(StripId(1), 10);
    tree.set_active_group(StripId(1), 11);
    tree.set_active_group(StripId(1), 12);
    tree.close_tab(110).unwrap();
    assert_eq!(tree.active_group_for(StripId(1)), Some(12));
    tree.close_tab(120).unwrap();
    assert_eq!(tree.active_group_for(StripId(1)), Some(10));

    // Restructuring removal keeps the index rule.
    let mut plain = self::tree();
    plain.workspaces[0].strips[0]
        .window_groups
        .insert(1, group(12, &[120]));
    plain.set_active_group(StripId(1), 10);
    plain.set_active_group(StripId(1), 12);
    plain.remove_tab(120).unwrap();
    assert_eq!(plain.active_group_for(StripId(1)), Some(11));
}

#[test]
fn activating_the_front_tab_again_records_nothing() {
    let mut group = group(30, &[300, 301]);
    group.activate_tab(300);
    assert!(group.focus_history.is_empty());
    group.activate_tab(301);
    group.activate_tab(301);
    assert_eq!(group.focus_history, vec![300]);
}
