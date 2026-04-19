//! Interaction tests for shell chrome widgets (OverviewView, GlobalSearch,
//! QuickSettings, WorkspaceSettings, SidebarWheel, TopBar, …).
//!
//! Tasks land their chrome pieces one at a time and add their own test here.

use quick_desktop::shell::overview::OverviewView;
use quick_desktop::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

/// Helper mirroring the plan's `Workspace::new` stand-in. We keep the
/// `WorkspaceId = u64` alias from Plan 1's state model and mint monotonic ids
/// locally so tests don't clash when several build a tree.
fn mk_workspace(id: WorkspaceId, name: &str) -> Workspace {
    Workspace {
        id,
        name: name.into(),
        strips: vec![],
        roles: vec![],
        workspace_files: vec![],
    }
}

#[test]
fn overview_view_click_activates_workspace() {
    let mut tree = WorkspaceTree {
        workspaces: vec![],
        active: 0,
        active_strip: Default::default(),
        collaborators: vec![],
    };
    let a = mk_workspace(1, "Alpha");
    let b = mk_workspace(2, "Beta");
    let b_id = b.id;
    tree.workspaces = vec![a, b];
    tree.active = tree.workspaces[0].id;

    let mut ov = OverviewView::new();
    ov.sync(&tree);
    assert_eq!(ov.card_count(), 2);
    ov.click_card(1, &mut tree);
    assert_eq!(tree.active, b_id);
}
