//! "New File" **templates**: the files in the user's Templates directory
//! (`$XDG_TEMPLATES_DIR`, else `~/Templates`). Each becomes a "New …" entry in
//! the right-click menu that copies the template into the current directory —
//! the standard Nautilus / Finder "New Document from template" affordance.
//!
//! When the Templates directory has no files the whole submenu is hidden (see
//! [`crate::files_app`]), so a user with no templates never sees an empty menu.

use std::path::{Path, PathBuf};

/// The Templates directory: `$XDG_TEMPLATES_DIR` when set (and non-empty),
/// otherwise `~/Templates`. `None` only when there is no home directory.
pub fn templates_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("XDG_TEMPLATES_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    tilo_paths::home().map(|h| h.join("Templates"))
}

/// The template files inside `dir`: regular files only (directories and
/// dotfiles are skipped), sorted by name for a stable menu order. An absent or
/// unreadable directory yields an empty list.
pub fn list_templates(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_file()) {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

/// The menu label for a template file: `New <stem>` (the extension is dropped
/// from the label but kept when the file is created).
pub fn menu_label(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    crate::i18n::t_args("files-new-from-template", &[crate::i18n::arg("name", stem)])
}

/// Create a new file in `dest_dir` by copying `template` there under a
/// non-colliding name. Returns the created path. Delegates de-duplication to
/// [`crate::fileops::copy_into`] so `Untitled.md` → `Untitled 2.md`.
///
/// # Errors
///
/// Returns an error if `template` has no file name, or the underlying copy
/// fails (see [`crate::fileops::copy_into`]).
pub fn create_from_template(template: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    crate::fileops::copy_into(template, dest_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Templates list sorted files skipping dirs and dotfiles, empty dirs yield nothing, labels drop extensions and creation copies content.
    #[test]
    fn templates_list_label_and_create_files() {
        // --- phase: list_templates_returns_files_sorted_skipping_dirs_and_dotfiles ---
        {
            let d = tempfile::tempdir().unwrap();
            fs::write(d.path().join("Document.md"), b"# ${title}\n").unwrap();
            fs::write(d.path().join("Spreadsheet.csv"), b"a,b\n").unwrap();
            fs::write(d.path().join(".hidden"), b"x").unwrap();
            fs::create_dir(d.path().join("subdir")).unwrap();

            let t = list_templates(d.path());
            let names: Vec<String> = t
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                names,
                ["Document.md", "Spreadsheet.csv"],
                "files only, sorted, no dotfiles/dirs"
            );
        }

        // --- phase: empty_templates_dir_yields_nothing ---
        {
            let d = tempfile::tempdir().unwrap();
            assert!(list_templates(d.path()).is_empty());
            // A missing directory is also empty, not an error.
            assert!(list_templates(&d.path().join("nope")).is_empty());
        }

        // --- phase: menu_label_drops_extension ---
        {
            assert_eq!(menu_label(Path::new("/t/Document.md")), "New Document");
            assert_eq!(menu_label(Path::new("/t/notes")), "New notes");
        }

        // --- phase: create_from_template_copies_content_into_dest ---
        {
            let d = tempfile::tempdir().unwrap();
            let tmpl = d.path().join("Document.md");
            fs::write(&tmpl, b"hello template").unwrap();
            let dest = d.path().join("dest");
            fs::create_dir(&dest).unwrap();

            let made = create_from_template(&tmpl, &dest).unwrap();
            assert_eq!(made.file_name().unwrap().to_string_lossy(), "Document.md");
            assert_eq!(fs::read(&made).unwrap(), b"hello template");
            // A second create de-duplicates the name.
            let made2 = create_from_template(&tmpl, &dest).unwrap();
            assert_eq!(
                made2.file_name().unwrap().to_string_lossy(),
                "Document 2.md"
            );
        }
    }
}
