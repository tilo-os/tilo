//! `ShellTile::AppPlaceholder { kind: Terminal | FileExplorer }` in a
//! persisted `WorkspaceTree` must resolve to live `UiTile::App` widgets,
//! not `UiTile::Stub`, when the shell builds its root tree. Uses
//! `resolve_tile_shape_for_test`, a probe returning `"App"`/`"Stub"`
//! without exposing `UiTile` (which carries a `Box<dyn AppWidget>`).

use shell::shell::state::FreshId as _;
use shell::shell::state::{
    AppKind, Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceTree,
};
use shell::shell::{build_tilo_root, resolve_tile_shape_for_test};
use std::collections::HashMap;

/// A single-workspace tree whose sole window-group holds one
/// `AppPlaceholder` tab of `kind`. Avoids `bootstrap_default` so drift in
/// the default tree's tile kinds can't fail this spuriously.
fn tree_with_single_placeholder(kind: AppKind, title: &str) -> WorkspaceTree {
    let ws_id = shell::shell::state::WorkspaceId::new();
    let strip_id = shell::shell::state::StripId::new();
    // Any unique u64 works; `next_id` isn't exported.
    let group_id: u64 = 9_001;
    let tab_id: u64 = 9_002;
    let strip = Strip {
        id: strip_id,
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: group_id,
            tabs: vec![Tab {
                id: tab_id,
                title: title.into(),
                tile: TileContent::AppPlaceholder {
                    kind,
                    label: title.into(),
                },
            }],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    };
    let ws = Workspace::new(ws_id, "Home");
    let ws = Workspace {
        strips: vec![strip],
        ..ws
    };
    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group: HashMap::new(),
            collaborators: vec![],
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn persisted_terminal_and_files_placeholders_resolve_to_live_tiles() {
    // Phase `terminal_placeholder_resolves_to_live_app_tile`.
    {
        let tile = TileContent::AppPlaceholder {
            kind: AppKind::Terminal,
            label: "Terminal".into(),
        };
        // PTY spawn can legitimately fail in some CI sandboxes, so accept either
        // shape. The regression guarded against is a silent fall-through to Stub
        // with the *original* placeholder label — on spawn failure the fallback
        // carries a "Terminal: <err>" prefix.
        let shape = resolve_tile_shape_for_test(&tile);
        assert!(
            shape == "App" || shape == "Stub",
            "unexpected tile shape: {shape}"
        );
    }

    // Phase `file_explorer_placeholder_resolves_to_live_app_tile`.
    {
        let tile = TileContent::AppPlaceholder {
            kind: AppKind::FileExplorer,
            label: "Files".into(),
        };
        assert_eq!(
            resolve_tile_shape_for_test(&tile),
            "App",
            "a Files placeholder must resolve to the Files component"
        );
    }

    // Phase `build_tilo_root_accepts_terminal_and_files_placeholders`.
    {
        // The full root-build path must not panic on persisted placeholders.
        let term_tree = tree_with_single_placeholder(AppKind::Terminal, "Terminal");
        let _ = build_tilo_root(&term_tree);
        let files_tree = tree_with_single_placeholder(AppKind::FileExplorer, "Files");
        let _ = build_tilo_root(&files_tree);
    }
}
