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

use quick_desktop::shell::search::GlobalSearch;

#[test]
fn global_search_seeds_commands_and_filters() {
    let mut s = GlobalSearch::new();
    s.seed_default_commands(); // "Create Workspace", "Rename Workspace", "Open Settings", ...
    s.palette_mut().set_query("create");
    assert!(s.palette().visible_entries().iter().any(|e| e.label.contains("Create")));
}

#[test]
fn global_search_context_filter_narrows_results() {
    let mut s = GlobalSearch::new();
    s.seed_default_commands();
    s.set_context_kind(quick_desktop::shell::search::SearchContext::Files);
    let labels: Vec<String> = s.palette().visible_entries().iter().map(|e| e.label.clone()).collect();
    assert!(labels.iter().all(|l| l.to_lowercase().contains("file") || l.to_lowercase().contains("open")));
}

#[test]
fn quick_settings_brightness_and_volume_clamp_and_mutate() {
    use quick_desktop::shell::quick_settings::QuickSettings;
    let mut q = QuickSettings::new();
    q.set_brightness(50.0);
    q.set_volume(80.0);
    assert_eq!(q.brightness(), 50.0);
    assert_eq!(q.volume(), 80.0);
    q.set_brightness(-10.0);
    q.set_volume(999.0);
    assert_eq!(q.brightness(), 0.0);
    assert_eq!(q.volume(), 100.0);
}

#[test]
fn quick_settings_drill_in_and_back() {
    use quick_desktop::shell::quick_settings::{QuickSettings, QsLevel};
    let mut q = QuickSettings::new();
    assert_eq!(q.current_level(), &QsLevel::Root);
    q.push_level(QsLevel::Wifi);
    assert_eq!(q.current_level(), &QsLevel::Wifi);
    q.push_level(QsLevel::WifiDetails("Home-5G".into()));
    assert!(matches!(q.current_level(), QsLevel::WifiDetails(s) if s == "Home-5G"));
    q.pop_level();
    assert_eq!(q.current_level(), &QsLevel::Wifi);
    q.pop_level();
    assert_eq!(q.current_level(), &QsLevel::Root);
    q.pop_level(); // no-op at root
    assert_eq!(q.current_level(), &QsLevel::Root);
}
