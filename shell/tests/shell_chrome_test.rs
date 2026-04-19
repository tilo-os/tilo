//! Interaction tests for shell chrome widgets (OverviewView, GlobalSearch,
//! QuickSettings, WorkspaceSettings, SidebarWheel, TopBar, …).
//!
//! Tasks land their chrome pieces one at a time and add their own test here.

use quick_desktop::shell::overview::OverviewView;
use quick_desktop::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

/// Helper that builds a workspace with a caller-supplied id so each test
/// keeps a stable shape without minting fresh monotonic ids.
fn mk_workspace(id: u64, name: &str) -> Workspace {
    Workspace::new(WorkspaceId::from(id), name)
}

#[test]
fn overview_view_click_activates_workspace() {
    let mut tree = WorkspaceTree {
        workspaces: vec![],
        active: WorkspaceId::from(0),
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

#[test]
fn workspace_settings_general_edits_name_and_removes_collaborator() {
    use quick_desktop::shell::state::{Collaborator, Workspace, WorkspaceId};
    use quick_desktop::shell::workspace_settings::{WorkspaceSettings, WsTab};
    let mut ws = Workspace::new(WorkspaceId::new(), "Design");
    ws.collaborators = vec![
        Collaborator { id: "u1".into(), name: "Ada".into() },
        Collaborator { id: "u2".into(), name: "Bo".into() },
    ];
    let mut modal = WorkspaceSettings::new(ws.id);
    modal.set_tab(WsTab::General);
    modal.rename(&mut ws, "Concept Room");
    assert_eq!(ws.name, "Concept Room");
    modal.remove_collaborator(&mut ws, "u1");
    assert_eq!(ws.collaborators.len(), 1);
    assert_eq!(ws.collaborators[0].id, "u2");
}

#[test]
fn workspace_settings_roles_tab_edits_app_permissions() {
    use quick_desktop::shell::state::{AppPerm, Role, RolePermissions, Workspace, WorkspaceId};
    use quick_desktop::shell::workspace_settings::{WorkspaceSettings, WsTab};
    let mut ws = Workspace::new(WorkspaceId::new(), "Design");
    ws.roles.push(Role {
        id: "r1".into(),
        name: "Viewer".into(),
        permissions: RolePermissions::default(),
    });
    let mut modal = WorkspaceSettings::new(ws.id);
    modal.set_tab(WsTab::Roles);
    modal.select_role("r1");
    assert_eq!(modal.selected_role(), Some("r1"));
    modal.set_app_perm(&mut ws, "r1", "Terminal", AppPerm::View);
    modal.set_app_perm(&mut ws, "r1", "Terminal", AppPerm::Edit);
    let perm = ws.roles[0]
        .permissions
        .per_app
        .get("Terminal")
        .copied()
        .unwrap_or(AppPerm::None);
    assert!(matches!(perm, AppPerm::Edit));
}

#[test]
fn workspace_settings_roles_tab_builds_apps_table_with_columns() {
    use quick_desktop::shell::workspace_settings::WorkspaceSettings;
    use quick_desktop::shell::state::WorkspaceId;
    let modal = WorkspaceSettings::new(WorkspaceId::new());
    // Apps table should expose two columns: the app name (flex) and the
    // permission picker (fixed width). Keys are stable so callers can hook
    // sort/resize policies onto them in later tasks.
    let apps_cols: Vec<String> = modal
        .apps_table()
        .columns()
        .iter()
        .map(|c| c.key.clone())
        .collect();
    assert_eq!(apps_cols, vec!["app".to_string(), "permission".to_string()]);
}
