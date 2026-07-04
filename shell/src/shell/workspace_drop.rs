//! Workspace-drop handling — port of the concept's `DesktopView.tsx`
//! `handleDrop` path (lines 187–205).
//!
//! When a file or folder is dragged from the file explorer (or any
//! source that emits `application/x-os-item`) and released onto the
//! workspace area, the concept opens an appropriate app in the active
//! strip:
//!
//! - Folders → Files app.
//! - Files whose extension is in `{js,ts,tsx,json,md,css,html}` → Editor.
//! - Anything else → Files.
//!
//! This module exposes the two pieces needed for parity: a pure
//! `decide_app_for_drop` selector and an `apply_workspace_drop` helper
//! that splices a new `Tab` into the active workspace's first strip's
//! first window group. Both are unit-testable without a winit event
//! pump, so the behaviour can be locked in before the
//! `WindowEvent::DroppedFile` wiring lands in `main.rs`.

use std::path::Path;

use crate::shell::state::{
    AppKind, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree, next_id,
};
use crate::shell::tile::{
    Strip as RuntimeStrip, Tab as RuntimeTab, TileContent as RuntimeTile,
    WindowGroup as RuntimeGroup, WorkspaceTree as RuntimeTree,
};
use ui::widget::AppKind as UiAppKind;

/// Payload carried by a workspace drop. The concept's drag data payload
/// has two fields (`name`, `type`); `is_folder` collapses the discrete
/// `'folder' | 'file'` value to a bool because the shell only branches
/// on that one bit.
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

    /// Convenience for the `WindowEvent::DroppedFile` path — pulls the
    /// filename segment out of the dropped `PathBuf` and classifies
    /// folder-vs-file via `Path::is_dir`. Returns `None` if the path
    /// has no filename (e.g. the root).
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        Some(Self {
            name,
            is_folder: path.is_dir(),
        })
    }
}

/// Extensions the concept's `DesktopView.tsx` routes to the Editor
/// (line 199). Kept as a sorted `&[&str]` because the list is small and
/// exhaustive — a lookup-table or regex would be overkill.
pub const EDITOR_EXTENSIONS: &[&str] = &["css", "html", "js", "json", "md", "ts", "tsx"];

/// Pure app-selector — given a dragged item, pick which app to open.
/// Mirrors `DesktopView.tsx:195-204`.
pub fn decide_app_for_drop(item: &DraggedItem) -> AppKind {
    if item.is_folder {
        return AppKind::FileExplorer;
    }
    let ext = Path::new(&item.name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if EDITOR_EXTENSIONS.iter().any(|e| *e == ext) {
        AppKind::Other("Editor".into())
    } else {
        AppKind::FileExplorer
    }
}

/// Apply a workspace-area drop to the tree. Splices a new `Tab` (with
/// a freshly-minted id) into the active workspace's first strip's first
/// window group, focuses it, and returns the new `Tab::id` so callers
/// can route subsequent presence/focus events to it.
///
/// If the active workspace has no strips or the first strip has no
/// window group, the function creates a new group in the first strip
/// so the drop always lands somewhere — matching the concept's `onOpenApp`
/// which takes the active strip id and has the app decide where inside
/// that strip to live.
///
/// Returns `None` if the tree has no active workspace (shouldn't happen
/// on a sanely-constructed tree, but the caller shouldn't panic).
pub fn apply_workspace_drop(tree: &mut WorkspaceTree, item: DraggedItem) -> Option<u64> {
    let kind = decide_app_for_drop(&item);
    let label = tab_label_for(&item, &kind);
    let tab = Tab {
        id: next_id(),
        title: label.clone(),
        tile: TileContent::AppPlaceholder {
            kind,
            label,
        },
    };
    let tab_id = tab.id;
    // Capture for the doc mirror before the cache `push` consumes the tab.
    let doc_title = tab.title.clone();
    let doc_tile_blob = serde_json::to_vec(&tab.tile).unwrap_or_default();

    let ws = tree.active_workspace_mut()?;
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
    // Unit D: mirror the cache append into the automerge doc so peers
    // observe the new tab on the next sync round. No-op if the active
    // workspace isn't seeded into the doc.
    tree.doc_append_tab_to_first_group_of_active_workspace(
        tab_id,
        doc_title,
        doc_tile_blob,
    );
    Some(tab_id)
}

/// Map a persisted-state [`AppKind`] onto a UI-layer [`UiAppKind`]. The
/// runtime tile uses the UI vocabulary (`UiAppKind::{Files,Terminal,…}`)
/// while the persisted vocabulary carries `FileExplorer` and the
/// open-ended `Other(String)` bucket — so we fold the `"Editor"` variant
/// onto `UiAppKind::Settings` as a temporary parking spot until an
/// `Editor` variant lands on the UI enum (spec §7 follow-up).
fn state_kind_to_ui(kind: &AppKind) -> UiAppKind {
    match kind {
        AppKind::Terminal => UiAppKind::Terminal,
        AppKind::FileExplorer => UiAppKind::Files,
        AppKind::Browser => UiAppKind::Browser,
        AppKind::Settings => UiAppKind::Settings,
        AppKind::Other(s) if s.eq_ignore_ascii_case("Editor") => UiAppKind::Settings,
        AppKind::Other(_) => UiAppKind::Settings,
    }
}

/// Runtime-tree mirror of [`apply_workspace_drop`]. Appends a Stub tile
/// (classified via [`decide_app_for_drop`]) to the first strip's first
/// window group of the active workspace. A Stub — not a live
/// `Box<dyn AppWidget>` — because drops happen outside the app
/// boot-time allocation window: materialising an AppWidget synchronously
/// could spawn subprocesses, and the expected UX is "tab appears
/// immediately; its content hydrates when the user focuses it."
///
/// Returns the new `Tab::id` or `None` if the tree has no active
/// workspace.
pub fn apply_runtime_workspace_drop(
    tree: &mut RuntimeTree,
    item: DraggedItem,
) -> Option<u64> {
    let kind = decide_app_for_drop(&item);
    let label = tab_label_for(&item, &kind);
    let ui_kind = state_kind_to_ui(&kind);
    let tab = RuntimeTab::new(
        &label,
        RuntimeTile::Stub {
            kind: ui_kind,
            label: label.clone(),
        },
    );
    let tab_id = tab.id;

    let ws = tree.workspaces.iter_mut().find(|w| w.id == tree.active)?;
    let strip = ws.strips.first_mut()?;
    if let Some(group) = strip.window_groups.first_mut() {
        group.tabs.push(tab);
        group.active_tab = tab_id;
    } else {
        let group = RuntimeGroup::with_tabs(vec![tab], WindowWidth::Normal);
        strip.window_groups.push(group);
        let group = strip.window_groups.last_mut().unwrap();
        group.active_tab = tab_id;
    }
    Some(tab_id)
}

/// `apply_runtime_workspace_drop` overload that takes a `Path` and does
/// the `DraggedItem::from_path` conversion. Convenience wrapper used by
/// the winit `WindowEvent::DroppedFile` path — the event carries a
/// `PathBuf` directly.
pub fn apply_runtime_workspace_drop_from_path(
    tree: &mut RuntimeTree,
    path: &Path,
) -> Option<u64> {
    let item = DraggedItem::from_path(path)?;
    apply_runtime_workspace_drop(tree, item)
}

/// `apply_workspace_drop` overload that takes a `Path` and does the
/// `DraggedItem::from_path` conversion. Persisted-tree counterpart of
/// [`apply_runtime_workspace_drop_from_path`]; used by
/// [`super::chrome_state::ChromeState::handle_file_drop`] which holds a
/// `WorkspaceTree` (not a runtime one).
pub fn apply_workspace_drop_from_path(
    tree: &mut WorkspaceTree,
    path: &Path,
) -> Option<u64> {
    let item = DraggedItem::from_path(path)?;
    apply_workspace_drop(tree, item)
}

/// Silence the unused-import warning when `RuntimeStrip` is only used
/// in doctests / docs (above). Keeps the symbol available so future
/// reworks of the module don't have to re-add the import.
#[allow(dead_code)]
fn _use_runtime_strip(_s: &RuntimeStrip) {}

/// Pick a tab label for a dropped item. Mirrors the concept's
/// `onOpenApp` defaults: folders get "Files — <name>", code files keep
/// their filename, others get "Files — <name>" too.
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

    #[test]
    fn folder_drop_opens_files() {
        let item = DraggedItem::folder("src");
        assert_eq!(decide_app_for_drop(&item), AppKind::FileExplorer);
    }

    #[test]
    fn code_file_extensions_route_to_editor() {
        for ext in EDITOR_EXTENSIONS {
            let item = DraggedItem::file(format!("example.{ext}"));
            assert_eq!(
                decide_app_for_drop(&item),
                AppKind::Other("Editor".into()),
                "expected .{ext} to open Editor"
            );
        }
    }

    #[test]
    fn code_file_extension_matching_is_case_insensitive() {
        let item = DraggedItem::file("README.MD");
        assert_eq!(decide_app_for_drop(&item), AppKind::Other("Editor".into()));
    }

    #[test]
    fn unknown_file_extension_falls_back_to_files() {
        let item = DraggedItem::file("logo.png");
        assert_eq!(decide_app_for_drop(&item), AppKind::FileExplorer);
    }

    #[test]
    fn file_without_extension_falls_back_to_files() {
        let item = DraggedItem::file("Makefile");
        assert_eq!(decide_app_for_drop(&item), AppKind::FileExplorer);
    }

    #[test]
    fn from_path_extracts_filename_for_existing_file() {
        let tmp = std::env::temp_dir();
        let path = tmp.join("nirios-drop-test.md");
        // Create the file so is_dir() resolves correctly; fail-open if
        // the temp write doesn't work (e.g. in a read-only sandbox).
        let _ = std::fs::write(&path, b"hello");
        let item = DraggedItem::from_path(&path).expect("filename");
        assert_eq!(item.name, "nirios-drop-test.md");
        assert!(!item.is_folder);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn apply_drop_appends_tab_to_first_group_of_active_workspace() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let before = tree.active_workspace().unwrap().strips[0].window_groups[0]
            .tabs
            .len();
        let tab_id =
            apply_workspace_drop(&mut tree, DraggedItem::file("example.ts")).expect("tab_id");
        let group = &tree.active_workspace().unwrap().strips[0].window_groups[0];
        assert_eq!(group.tabs.len(), before + 1);
        let new_tab = group.tabs.last().unwrap();
        assert_eq!(new_tab.id, tab_id);
        assert_eq!(new_tab.title, "example.ts");
        // Editor routes to Other("Editor") per concept.
        match &new_tab.tile {
            TileContent::AppPlaceholder { kind, .. } => {
                assert_eq!(kind, &AppKind::Other("Editor".into()));
            }
            other => panic!("expected AppPlaceholder, got {:?}", other),
        }
    }

    #[test]
    fn apply_drop_focuses_the_new_tab() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let tab_id =
            apply_workspace_drop(&mut tree, DraggedItem::folder("src")).expect("tab_id");
        let group = &tree.active_workspace().unwrap().strips[0].window_groups[0];
        assert_eq!(group.active_tab, tab_id);
    }

    #[test]
    fn apply_drop_creates_group_when_strip_is_empty() {
        let mut tree = WorkspaceTree::bootstrap_default();
        // Empty the first strip so the helper has to create a group.
        tree.active_workspace_mut().unwrap().strips[0]
            .window_groups
            .clear();
        let tab_id = apply_workspace_drop(&mut tree, DraggedItem::file("readme.md"))
            .expect("tab_id");
        let strip = &tree.active_workspace().unwrap().strips[0];
        assert_eq!(strip.window_groups.len(), 1);
        assert_eq!(strip.window_groups[0].tabs.len(), 1);
        assert_eq!(strip.window_groups[0].active_tab, tab_id);
    }

    #[test]
    fn apply_drop_labels_folder_tab_with_files_prefix() {
        let mut tree = WorkspaceTree::bootstrap_default();
        apply_workspace_drop(&mut tree, DraggedItem::folder("Documents")).unwrap();
        let group = &tree.active_workspace().unwrap().strips[0].window_groups[0];
        assert_eq!(group.tabs.last().unwrap().title, "Files — Documents");
    }

    #[test]
    fn apply_drop_labels_code_file_tab_with_filename_only() {
        let mut tree = WorkspaceTree::bootstrap_default();
        apply_workspace_drop(&mut tree, DraggedItem::file("main.tsx")).unwrap();
        let group = &tree.active_workspace().unwrap().strips[0].window_groups[0];
        assert_eq!(group.tabs.last().unwrap().title, "main.tsx");
    }

    fn runtime_tree_with_empty_workspace() -> RuntimeTree {
        let group = RuntimeGroup::with_tabs(vec![], WindowWidth::Normal);
        let strip = RuntimeStrip::new("Main", vec![group]);
        let ws = crate::shell::tile::Workspace::new("Home", vec![strip]);
        RuntimeTree::new(vec![ws])
    }

    #[test]
    fn runtime_drop_appends_stub_tab_to_active_workspace() {
        let mut tree = runtime_tree_with_empty_workspace();
        let tab_id = apply_runtime_workspace_drop(&mut tree, DraggedItem::file("main.ts"))
            .expect("tab_id");
        let ws = tree.workspaces.iter().find(|w| w.id == tree.active).unwrap();
        let group = &ws.strips[0].window_groups[0];
        assert_eq!(group.tabs.len(), 1);
        assert_eq!(group.tabs[0].id, tab_id);
        assert_eq!(group.tabs[0].title, "main.ts");
        // Editor-routed drops map onto UiAppKind::Settings today (see
        // `state_kind_to_ui`'s doc-comment for the rationale).
        match &group.tabs[0].tile {
            RuntimeTile::Stub { kind, .. } => {
                assert_eq!(*kind, UiAppKind::Settings);
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
        let tab_id = apply_runtime_workspace_drop_from_path(&mut tree, &path)
            .expect("tab_id");
        let ws = tree.workspaces.iter().find(|w| w.id == tree.active).unwrap();
        let group = &ws.strips[0].window_groups[0];
        assert_eq!(group.active_tab, tab_id);
        assert_eq!(group.tabs[0].title, "file.md");
    }

    #[test]
    fn runtime_drop_creates_group_when_strip_is_empty() {
        let strip = RuntimeStrip::new("Main", vec![]);
        let ws = crate::shell::tile::Workspace::new("Home", vec![strip]);
        let mut tree = RuntimeTree::new(vec![ws]);
        let tab_id = apply_runtime_workspace_drop(&mut tree, DraggedItem::folder("src"))
            .expect("tab_id");
        let ws = tree.workspaces.iter().find(|w| w.id == tree.active).unwrap();
        let strip = &ws.strips[0];
        assert_eq!(strip.window_groups.len(), 1);
        assert_eq!(strip.window_groups[0].active_tab, tab_id);
        assert_eq!(strip.window_groups[0].tabs[0].title, "Files — src");
    }

    #[test]
    fn apply_drop_preserves_previous_tabs() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let titles_before: Vec<String> = tree.active_workspace().unwrap().strips[0]
            .window_groups[0]
            .tabs
            .iter()
            .map(|t| t.title.clone())
            .collect();
        apply_workspace_drop(&mut tree, DraggedItem::file("app.css")).unwrap();
        let titles_after: Vec<String> = tree.active_workspace().unwrap().strips[0]
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
