//! Workspace-drop handling.
//!
//! When a file or folder is dragged from the file explorer (or any
//! source that emits `application/x-os-item`) and released onto the
//! workspace area:
//!
//! - Folders → Files app.
//! - Files whose extension is in `{js,ts,tsx,json,md,css,html}` → Editor.
//! - Anything else → Files.
//!
//! `decide_app_for_drop` is a pure selector; `apply_workspace_drop`
//! splices a new `Tab` into the active workspace's first strip's first
//! window group. Both are unit-testable without a winit event pump.

use std::path::Path;

use crate::shell::state::DesktopTree as RuntimeTree;
use crate::shell::state::{
    AppKind, GroupId, StripId, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree, next_id,
};
use crate::shell::tab_binding::Placement;

/// `is_folder` collapses the concept's `'folder' | 'file'` drag data
/// to a bool, since the shell only branches on that one bit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraggedItem {
    pub name: String,
    pub is_folder: bool,
}

impl DraggedItem {
    pub fn folder(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_folder: true,
        }
    }

    pub fn file(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_folder: false,
        }
    }

    /// For `WindowEvent::DroppedFile`. `None` if the path has no
    /// filename (e.g. the root).
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        Some(Self {
            name,
            is_folder: path.is_dir(),
        })
    }
}

/// Small and exhaustive; a lookup-table or regex would be overkill.
pub const EDITOR_EXTENSIONS: &[&str] = &[
    "bash", "c", "cpp", "css", "go", "h", "hpp", "html", "java", "js", "json", "kt", "md", "py",
    "rs", "sh", "toml", "ts", "tsx", "txt", "xml", "yaml", "yml", "zsh",
];

pub fn decide_app_for_drop(item: &DraggedItem) -> AppKind {
    if item.is_folder {
        return AppKind::FileExplorer;
    }
    let ext = Path::new(&item.name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if EDITOR_EXTENSIONS.iter().any(|e| *e == ext) {
        AppKind::Other("Editor".into())
    } else {
        AppKind::FileExplorer
    }
}

/// Splices a new `Tab` into the active workspace's first strip's first
/// window group (creating one if none exists) and focuses it. `None`
/// if the tree has no active workspace.
pub fn apply_workspace_drop(tree: &mut WorkspaceTree, item: &DraggedItem) -> Option<u64> {
    let kind = decide_app_for_drop(item);
    let label = tab_label_for(item, &kind);
    let tab = Tab {
        id: next_id(),
        title: label.clone(),
        tile: TileContent::AppPlaceholder { kind, label },
    };
    let tab_id = tab.id;
    // Capture before the cache `push` consumes the tab.
    let doc_title = tab.title.clone();
    let doc_tile_blob = serde_json::to_vec(&tab.tile).unwrap_or_default();

    let ws = tree.desktop.active_workspace_mut()?;
    let strip = ws.strips.first_mut()?;
    if let Some(group) = strip.window_groups.first_mut() {
        group.tabs.push(tab);
        group.active_tab = tab_id;
    } else {
        strip.window_groups.push(WindowGroup {
            id: next_id(),
            tabs: vec![tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        });
    }
    // Mirror the cache append into the automerge doc so peers observe the
    // new tab on the next sync round. No-op if the active workspace isn't
    // seeded into the doc.
    tree.doc_append_tab_to_first_group_of_active_workspace(tab_id, doc_title, doc_tile_blob);
    Some(tab_id)
}

/// Splices into the **specific** `(strip_id, group_id)` the pointer was
/// over at drop time. `None` if either id is stale — a defined no-op
/// instead of dumping the file into an arbitrary window.
pub fn apply_workspace_drop_at(
    tree: &mut WorkspaceTree,
    item: &DraggedItem,
    strip_id: StripId,
    group_id: GroupId,
) -> Option<u64> {
    let kind = decide_app_for_drop(item);
    let label = tab_label_for(item, &kind);
    let tab = Tab {
        id: next_id(),
        title: label.clone(),
        tile: TileContent::AppPlaceholder { kind, label },
    };
    let tab_id = tab.id;
    // Capture before the cache `push` consumes the tab.
    let doc_title = tab.title.clone();
    let doc_tile_blob = serde_json::to_vec(&tab.tile).unwrap_or_default();

    let ws = tree.desktop.active_workspace_mut()?;
    let strip = ws.strips.iter_mut().find(|s| s.id == strip_id)?;
    let group = strip.window_groups.iter_mut().find(|g| g.id == group_id)?;
    group.tabs.push(tab);
    group.active_tab = tab_id;

    // Mirror the cache append into the automerge doc, as above.
    tree.doc_append_tab_to_group_of_active_workspace(group_id, tab_id, doc_title, doc_tile_blob);
    Some(tab_id)
}

pub fn apply_workspace_drop_at_from_path(
    tree: &mut WorkspaceTree,
    path: &Path,
    strip_id: StripId,
    group_id: GroupId,
) -> Option<u64> {
    let item = DraggedItem::from_path(path)?;
    apply_workspace_drop_at(tree, &item, strip_id, group_id)
}

/// Appends a Stub tile, not a live `Box<dyn AppWidget>` — materialising
/// one synchronously could spawn subprocesses; content hydrates on focus.
#[allow(
    clippy::needless_pass_by_value,
    reason = "public API called across the shell/compositor boundary; changing the signature \
              would require editing call sites outside this file's scope"
)]
pub fn apply_runtime_workspace_drop(tree: &mut RuntimeTree, item: DraggedItem) -> Option<u64> {
    let kind = decide_app_for_drop(&item);
    let label = tab_label_for(&item, &kind);
    let tab = crate::shell::state::Tab {
        id: crate::shell::state::next_id(),
        title: label.clone(),
        tile: crate::shell::state::TileContent::Stub { kind, label },
    };
    let tab_id = tab.id;

    let ws = tree.workspaces.iter_mut().find(|w| w.id == tree.active)?;
    let strip = ws.strips.first_mut()?;
    if let Some(group) = strip.window_groups.first_mut() {
        group.tabs.push(tab);
        group.active_tab = tab_id;
    } else {
        let group = crate::shell::state::WindowGroup {
            id: crate::shell::state::next_id(),
            tabs: vec![tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        strip.window_groups.push(group);
        let Some(group) = strip.window_groups.last_mut() else {
            // Unreachable — the line above just pushed one — but a
            // dropped tab that silently doesn't focus is a far better
            // failure than panicking this panic=abort process.
            return Some(tab_id);
        };
        group.active_tab = tab_id;
    }
    Some(tab_id)
}

pub fn apply_runtime_workspace_drop_from_path(tree: &mut RuntimeTree, path: &Path) -> Option<u64> {
    let item = DraggedItem::from_path(path)?;
    apply_runtime_workspace_drop(tree, item)
}

pub fn apply_workspace_drop_from_path(tree: &mut WorkspaceTree, path: &Path) -> Option<u64> {
    let item = DraggedItem::from_path(path)?;
    apply_workspace_drop(tree, &item)
}

/// Place an unclaimed file/folder through the same exact destination used by
/// app and tab drags. Claimed document types take `doc_open::open_at`; this is
/// the Files/placeholder fallback and therefore still has to preserve the
/// tab-side or group-seam instruction.
pub fn apply_workspace_drop_at_placement_from_path(
    tree: &mut WorkspaceTree,
    path: &Path,
    placement: Placement,
) -> Option<u64> {
    let item = DraggedItem::from_path(path)?;
    let kind = decide_app_for_drop(&item);
    let label = tab_label_for(&item, &kind);
    Some(crate::shell::tab_binding::open_hosted_app(
        tree, kind, label, placement,
    ))
}

/// Pick a tab label for a dropped item: folders get "Files — <name>",
/// code files keep their filename, others get "Files — <name>" too.
fn tab_label_for(item: &DraggedItem, kind: &AppKind) -> String {
    match kind {
        AppKind::Other(s) if s == "Editor" => item.name.clone(),
        AppKind::FileExplorer if item.is_folder => format!("Files — {}", item.name),
        _ => item.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::FreshId as _;

    #[test]
    fn dropped_paths_route_by_kind_and_case_insensitive_extension() {
        // Phase `folder_drop_opens_files`.
        {
            let item = DraggedItem::folder("src");
            assert_eq!(decide_app_for_drop(&item), AppKind::FileExplorer);
        }

        // Phase `code_file_extensions_route_to_editor`.
        {
            for ext in EDITOR_EXTENSIONS {
                let item = DraggedItem::file(format!("example.{ext}"));
                assert_eq!(
                    decide_app_for_drop(&item),
                    AppKind::Other("Editor".into()),
                    "expected .{ext} to open Editor"
                );
            }
        }

        // Phase `code_file_extension_matching_is_case_insensitive`.
        {
            let item = DraggedItem::file("README.MD");
            assert_eq!(decide_app_for_drop(&item), AppKind::Other("Editor".into()));
        }

        // Phase `unknown_file_extension_falls_back_to_files`.
        {
            let item = DraggedItem::file("logo.png");
            assert_eq!(decide_app_for_drop(&item), AppKind::FileExplorer);
        }

        // Phase `file_without_extension_falls_back_to_files`.
        {
            let item = DraggedItem::file("Makefile");
            assert_eq!(decide_app_for_drop(&item), AppKind::FileExplorer);
        }

        // Phase `from_path_extracts_filename_for_existing_file`.
        {
            let tmp = std::env::temp_dir();
            let path = tmp.join("tilo-drop-test.md");
            // Create the file so is_dir() resolves correctly; fail-open if
            // the temp write doesn't work (e.g. in a read-only sandbox).
            let _ = std::fs::write(&path, b"hello");
            let item = DraggedItem::from_path(&path).expect("filename");
            assert_eq!(item.name, "tilo-drop-test.md");
            assert!(!item.is_folder);
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn apply_drop_adds_a_labelled_focused_tab_to_the_active_workspace() {
        // Phase `apply_drop_appends_tab_to_first_group_of_active_workspace`.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            let before = tree.desktop.active_workspace().unwrap().strips[0].window_groups[0]
                .tabs
                .len();
            let tab_id =
                apply_workspace_drop(&mut tree, &DraggedItem::file("example.ts")).expect("tab_id");
            let group = &tree.desktop.active_workspace().unwrap().strips[0].window_groups[0];
            assert_eq!(group.tabs.len(), before + 1);
            let new_tab = group.tabs.last().unwrap();
            assert_eq!(new_tab.id, tab_id);
            assert_eq!(new_tab.title, "example.ts");
            // Editor routes to Other("Editor") per concept.
            match &new_tab.tile {
                TileContent::AppPlaceholder { kind, .. } => {
                    assert_eq!(kind, &AppKind::Other("Editor".into()));
                }
                other => panic!("expected AppPlaceholder, got {other:?}"),
            }
        }

        // Phase `apply_drop_focuses_the_new_tab`.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            let tab_id =
                apply_workspace_drop(&mut tree, &DraggedItem::folder("src")).expect("tab_id");
            let group = &tree.desktop.active_workspace().unwrap().strips[0].window_groups[0];
            assert_eq!(group.active_tab, tab_id);
        }

        // Phase `apply_drop_creates_group_when_strip_is_empty`.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            // Empty the first strip so the helper has to create a group.
            tree.desktop.active_workspace_mut().unwrap().strips[0]
                .window_groups
                .clear();
            let tab_id =
                apply_workspace_drop(&mut tree, &DraggedItem::file("readme.md")).expect("tab_id");
            let strip = &tree.desktop.active_workspace().unwrap().strips[0];
            assert_eq!(strip.window_groups.len(), 1);
            assert_eq!(strip.window_groups[0].tabs.len(), 1);
            assert_eq!(strip.window_groups[0].active_tab, tab_id);
        }

        // Phase `apply_drop_labels_folder_tab_with_files_prefix`.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            apply_workspace_drop(&mut tree, &DraggedItem::folder("Documents")).unwrap();
            let group = &tree.desktop.active_workspace().unwrap().strips[0].window_groups[0];
            assert_eq!(group.tabs.last().unwrap().title, "Files — Documents");
        }

        // Phase `apply_drop_labels_code_file_tab_with_filename_only`.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            apply_workspace_drop(&mut tree, &DraggedItem::file("main.tsx")).unwrap();
            let group = &tree.desktop.active_workspace().unwrap().strips[0].window_groups[0];
            assert_eq!(group.tabs.last().unwrap().title, "main.tsx");
        }

        // Phase `apply_drop_preserves_previous_tabs`.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            let titles_before: Vec<String> = tree.desktop.active_workspace().unwrap().strips[0]
                .window_groups[0]
                .tabs
                .iter()
                .map(|t| t.title.clone())
                .collect();
            apply_workspace_drop(&mut tree, &DraggedItem::file("app.css")).unwrap();
            let titles_after: Vec<String> = tree.desktop.active_workspace().unwrap().strips[0]
                .window_groups[0]
                .tabs
                .iter()
                .map(|t| t.title.clone())
                .collect();
            // Every pre-existing title must still be present.
            for t in &titles_before {
                assert!(
                    titles_after.contains(t),
                    "tab `{t}` went missing after drop; new list = {titles_after:?}"
                );
            }
        }
    }

    fn runtime_tree_with_empty_workspace() -> RuntimeTree {
        let mut workspace =
            crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId::new(), "Home");
        let mut strip = crate::shell::state::Strip::new(StripId::new(), "Main");
        strip.window_groups.push(WindowGroup {
            id: 1,
            tabs: vec![],
            active_tab: 0,
            width: WindowWidth::Normal,
            maximized: false,
        });
        workspace.strips = vec![strip];
        RuntimeTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..Default::default()
        }
    }

    #[test]
    fn runtime_drop_appends_stub_tab_to_active_workspace() {
        let mut tree = runtime_tree_with_empty_workspace();
        let tab_id =
            apply_runtime_workspace_drop(&mut tree, DraggedItem::file("main.ts")).expect("tab_id");
        let ws = tree
            .workspaces
            .iter()
            .find(|w| w.id == tree.active)
            .unwrap();
        let group = &ws.strips[0].window_groups[0];
        assert_eq!(group.tabs.len(), 1);
        assert_eq!(group.tabs[0].id, tab_id);
        assert_eq!(group.tabs[0].title, "main.ts");
        // Runtime sidecars use the same canonical application identity.
        match &group.tabs[0].tile {
            crate::shell::state::TileContent::Stub { kind, .. } => {
                assert_eq!(*kind, AppKind::Other("Editor".into()));
            }
            _ => panic!("expected Stub tile"),
        }
    }

    #[test]
    fn runtime_drop_from_path_routes_through_the_same_logic() {
        let mut tree = runtime_tree_with_empty_workspace();
        // Use a nonexistent path — is_dir() → false, so it's classified
        // as a file. Extension .md routes to Editor.
        let path = std::path::PathBuf::from("/no/such/file.md");
        let tab_id = apply_runtime_workspace_drop_from_path(&mut tree, &path).expect("tab_id");
        let ws = tree
            .workspaces
            .iter()
            .find(|w| w.id == tree.active)
            .unwrap();
        let group = &ws.strips[0].window_groups[0];
        assert_eq!(group.active_tab, tab_id);
        assert_eq!(group.tabs[0].title, "file.md");
    }

    #[test]
    fn runtime_drop_creates_group_when_strip_is_empty() {
        let mut tree = runtime_tree_with_empty_workspace();
        tree.workspaces[0].strips[0].window_groups.clear();
        let tab_id =
            apply_runtime_workspace_drop(&mut tree, DraggedItem::folder("src")).expect("tab_id");
        let ws = tree
            .workspaces
            .iter()
            .find(|w| w.id == tree.active)
            .unwrap();
        let strip = &ws.strips[0];
        assert_eq!(strip.window_groups.len(), 1);
        assert_eq!(strip.window_groups[0].active_tab, tab_id);
        assert_eq!(strip.window_groups[0].tabs[0].title, "Files — src");
    }

    #[test]
    fn apply_drop_at_targets_the_named_group_or_refuses_unknown_targets() {
        // Phase `apply_drop_at_targets_the_named_group_not_the_first`.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            // Give the active workspace's first strip a SECOND group, so
            // "first group" and "target group" are distinguishable.
            let (strip_id, first_group_id, second_group_id) = {
                let ws = tree.desktop.active_workspace_mut().unwrap();
                let strip = ws.strips.first_mut().unwrap();
                let first_group_id = strip.window_groups[0].id;
                let second = WindowGroup {
                    id: next_id(),
                    tabs: vec![Tab {
                        id: next_id(),
                        title: "existing".into(),
                        tile: TileContent::AppPlaceholder {
                            kind: AppKind::FileExplorer,
                            label: "existing".into(),
                        },
                    }],
                    active_tab: 0,
                    width: WindowWidth::Normal,
                    maximized: false,
                };
                let second_group_id = second.id;
                strip.window_groups.push(second);
                (strip.id, first_group_id, second_group_id)
            };

            let first_before = group_by_id(&tree, first_group_id).tabs.len();
            let second_before = group_by_id(&tree, second_group_id).tabs.len();

            let tab_id = apply_workspace_drop_at(
                &mut tree,
                &DraggedItem::file("main.ts"),
                strip_id,
                second_group_id,
            )
            .expect("tab_id");

            assert_eq!(
                group_by_id(&tree, first_group_id).tabs.len(),
                first_before,
                "the first group must be untouched when the drop targets another group"
            );
            let second = group_by_id(&tree, second_group_id);
            assert_eq!(second.tabs.len(), second_before + 1);
            assert_eq!(second.active_tab, tab_id);
            assert_eq!(second.tabs.last().unwrap().id, tab_id);
            assert_eq!(second.tabs.last().unwrap().title, "main.ts");
        }

        // Phase `apply_drop_at_returns_none_for_unknown_group`.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            let strip_id = tree.desktop.active_workspace().unwrap().strips[0].id;
            // A group id that doesn't exist in the strip → stale target.
            let result =
                apply_workspace_drop_at(&mut tree, &DraggedItem::file("x.ts"), strip_id, 9_999_999);
            assert!(
                result.is_none(),
                "a drop onto a stale/unknown group must resolve to None, not panic \
                 or fall back to the first group"
            );
        }

        // Phase `apply_drop_at_returns_none_for_unknown_strip`.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            let group_id = tree.desktop.active_workspace().unwrap().strips[0].window_groups[0].id;
            let result = apply_workspace_drop_at(
                &mut tree,
                &DraggedItem::file("x.ts"),
                crate::shell::state::StripId::new(),
                group_id,
            );
            assert!(result.is_none(), "an unknown strip must resolve to None");
        }
    }

    /// Locate a group by id anywhere in the active workspace's strips.
    fn group_by_id(tree: &WorkspaceTree, group_id: u64) -> &WindowGroup {
        tree.desktop
            .active_workspace()
            .unwrap()
            .strips
            .iter()
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.id == group_id)
            .expect("group present")
    }
}
