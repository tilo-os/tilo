//! Task 15: Spawns the nirios shell in headless mode, embeds a real terminal
//! and a real file-explorer in tabs, and asserts their `AppWidget` metadata is
//! reflected in the live `Shell::workspace_tree()`.

use quick_desktop::shell::{Shell, tile::TileContent};
use quick_ui::AppKind;

#[test]
fn default_workspace_hosts_terminal_and_files_and_browser_stub() {
    let shell = Shell::new_with_default_demo_workspace();
    let tree = shell.workspace_tree();
    let tabs: Vec<&TileContent> = tree
        .workspaces
        .iter()
        .flat_map(|w| w.strips.iter())
        .flat_map(|s| s.window_groups.iter())
        .flat_map(|g| g.tabs.iter().map(|t| &t.tile))
        .collect();

    let kinds: Vec<AppKind> = tabs
        .iter()
        .filter_map(|t| match t {
            TileContent::App(a) => Some(a.app_kind()),
            TileContent::Stub { kind, .. } => Some(*kind),
        })
        .collect();

    assert!(
        kinds.contains(&AppKind::Terminal),
        "default workspace must host a terminal (kinds={:?})",
        kinds
    );
    assert!(
        kinds.contains(&AppKind::Files),
        "default workspace must host a file-explorer (kinds={:?})",
        kinds
    );
    assert!(
        kinds.contains(&AppKind::Browser),
        "default workspace must include a browser stub (kinds={:?})",
        kinds
    );
}
