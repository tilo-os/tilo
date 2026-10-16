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
//! `decide_app_for_drop` is a pure selector, unit-testable without a winit
//! event pump; `apply_workspace_drop_at_placement_from_path` opens the
//! chosen app at the drop's placement.

use std::path::Path;

use crate::shell::state::{AppKind, WorkspaceTree};
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
    fn dropped_folders_and_code_files_are_labelled_by_kind() {
        let folder = DraggedItem::folder("Documents");
        assert_eq!(
            tab_label_for(&folder, &decide_app_for_drop(&folder)),
            "Files — Documents"
        );
        let code = DraggedItem::file("main.tsx");
        assert_eq!(
            tab_label_for(&code, &decide_app_for_drop(&code)),
            "main.tsx"
        );
    }
}
