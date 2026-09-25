//! Moving and splitting tabs edits the tree and reports exactly what moved.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use tilo_desktop_model::{
    DesktopTree, DropTarget, IdMinter, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId,
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
fn a_tab_dropped_in_a_gap_becomes_its_own_group_and_empties_its_source() {
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

#[test]
fn a_tab_dropped_into_a_group_lands_at_its_index_and_becomes_active() {
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

#[test]
fn a_stale_target_or_same_strip_move_changes_nothing() {
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

#[test]
fn splitting_mints_a_group_and_refuses_a_lone_tab() {
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
