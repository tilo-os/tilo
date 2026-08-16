//! File-system mutations behind the file explorer's edit commands:
//! new folder, rename, delete-to-trash, copy and move.
//!
//! Each operation is a small, independently-testable free function that
//! works on real paths and returns a `Result<_, String>` (the string is a
//! user-facing error the app surfaces as a toast). Destination-collision
//! handling is centralised in [`unique_destination`] so copy/move never
//! silently clobber an existing file.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

// --- trash ----------------------------------------------------------------

/// The XDG "home trash" files directory (`~/.local/share/Trash/files`), where
/// trashed files are kept. `None` when there is no data dir.
pub fn trash_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("Trash").join("files"))
}

/// Whether `path` is the home-trash files directory.
pub fn is_trash_dir(path: &Path) -> bool {
    trash_dir().map(|t| path == t).unwrap_or(false)
}

/// Ensure the trash files directory exists, so opening an empty trash shows an
/// empty listing rather than a "no such directory" error. Returns the dir.
pub fn ensure_trash_dir() -> Option<PathBuf> {
    let d = trash_dir()?;
    let _ = std::fs::create_dir_all(&d);
    Some(d)
}

/// Permanently remove everything in the trash. Returns how many items were purged.
pub fn empty_trash() -> Result<usize, String> {
    let items = trash::os_limited::list().map_err(|e| e.to_string())?;
    let n = items.len();
    trash::os_limited::purge_all(items).map_err(|e| e.to_string())?;
    Ok(n)
}

/// Whether trash `item` backs the in-trash file named `files_name` (the name
/// shown in `Trash/files`). The item's `.trashinfo` file is
/// `Trash/info/<files_name>.trashinfo`, so match on that.
fn trash_item_backs(item: &trash::TrashItem, files_name: &OsStr) -> bool {
    Path::new(&item.id)
        .file_name()
        .and_then(|f| f.to_str())
        .and_then(|f| f.strip_suffix(".trashinfo"))
        .map(|stem| OsStr::new(stem) == files_name)
        .unwrap_or(false)
}

/// The trash items backing the given in-trash file paths (matched by their
/// `.trashinfo` file name, which is robust to on-trash name deduplication).
fn trash_items_for(entries: &[PathBuf]) -> Result<Vec<trash::TrashItem>, String> {
    let wanted: Vec<_> = entries.iter().filter_map(|p| p.file_name().map(|n| n.to_owned())).collect();
    Ok(trash::os_limited::list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|it| wanted.iter().any(|w| trash_item_backs(it, w)))
        .collect())
}

/// Restore the trashed items backing `entries` to their original locations.
/// Returns how many were restored.
pub fn restore_entries(entries: &[PathBuf]) -> Result<usize, String> {
    let items = trash_items_for(entries)?;
    let n = items.len();
    trash::os_limited::restore_all(items).map_err(|e| e.to_string())?;
    Ok(n)
}

/// Permanently delete the trashed items backing `entries` (file + info).
/// Returns how many were purged.
pub fn purge_entries(entries: &[PathBuf]) -> Result<usize, String> {
    let items = trash_items_for(entries)?;
    let n = items.len();
    trash::os_limited::purge_all(items).map_err(|e| e.to_string())?;
    Ok(n)
}

/// Pick a non-colliding path for `name` inside `dir`. If `dir/name` is
/// free it's returned as-is; otherwise a numeric suffix is inserted
/// before the extension: `report.txt` → `report 2.txt` → `report 3.txt`.
pub fn unique_destination(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 2..10_000 {
        let trial_name = match &ext {
            Some(ext) => format!("{stem} {n}.{ext}"),
            None => format!("{stem} {n}"),
        };
        let trial = dir.join(trial_name);
        if !trial.exists() {
            return trial;
        }
    }
    // Pathological fallback — extremely unlikely.
    dir.join(name)
}

/// Create a new folder named `name` (deduplicated) inside `parent`.
pub fn new_folder(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Folder name cannot be empty".to_string());
    }
    if name.contains('/') {
        return Err("Folder name cannot contain '/'".to_string());
    }
    let dest = unique_destination(parent, name);
    std::fs::create_dir(&dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Rename `path` to `new_name` within the same directory.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("Name cannot be empty".to_string());
    }
    if new_name.contains('/') {
        return Err("Name cannot contain '/'".to_string());
    }
    let parent = path.parent().ok_or_else(|| "No parent directory".to_string())?;
    let dest = parent.join(new_name);
    if dest == path {
        return Ok(dest);
    }
    if dest.exists() {
        return Err(format!("\"{new_name}\" already exists"));
    }
    std::fs::rename(path, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Move `paths` to the trash. Falls back to a clear message if the
/// platform trash isn't available.
pub fn delete_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    trash::delete_all(paths).map_err(|e| e.to_string())
}

/// Recursively copy `src` into the directory `dest_dir`, choosing a
/// non-colliding name. Returns the created destination path.
pub fn copy_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| "Source has no file name".to_string())?;
    let dest = unique_destination(dest_dir, &name);
    copy_recursive(src, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Move `src` into `dest_dir`. Tries a cheap rename first; on a
/// cross-device error falls back to copy-then-delete.
pub fn move_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| "Source has no file name".to_string())?;
    let dest = unique_destination(dest_dir, &name);
    match std::fs::rename(src, &dest) {
        Ok(()) => Ok(dest),
        Err(_) => {
            // Likely a cross-filesystem move — copy then remove.
            copy_recursive(src, &dest).map_err(|e| e.to_string())?;
            remove_path(src).map_err(|e| e.to_string())?;
            Ok(dest)
        }
    }
}

fn remove_path(p: &Path) -> std::io::Result<()> {
    if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// Copy a file or (recursively) a directory tree to an exact `dest` path.
fn copy_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let child_dest = dest.join(entry.file_name());
            copy_recursive(&entry.path(), &child_dest)?;
        }
        Ok(())
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dest)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn unique_destination_dedups() {
        let d = tmp();
        let root = d.path();
        // Free name is returned verbatim.
        assert_eq!(unique_destination(root, "a.txt"), root.join("a.txt"));
        fs::write(root.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_destination(root, "a.txt"), root.join("a 2.txt"));
        fs::write(root.join("a 2.txt"), b"x").unwrap();
        assert_eq!(unique_destination(root, "a.txt"), root.join("a 3.txt"));
        // Extension-less names.
        fs::create_dir(root.join("folder")).unwrap();
        assert_eq!(unique_destination(root, "folder"), root.join("folder 2"));
    }

    #[test]
    fn new_folder_creates_and_dedups() {
        let d = tmp();
        let p = new_folder(d.path(), "Stuff").unwrap();
        assert!(p.is_dir());
        let p2 = new_folder(d.path(), "Stuff").unwrap();
        assert_eq!(p2.file_name().unwrap().to_string_lossy(), "Stuff 2");
        assert!(new_folder(d.path(), "  ").is_err());
        assert!(new_folder(d.path(), "a/b").is_err());
    }

    #[test]
    fn rename_moves_within_dir() {
        let d = tmp();
        let f = d.path().join("old.txt");
        fs::write(&f, b"x").unwrap();
        let renamed = rename(&f, "new.txt").unwrap();
        assert!(renamed.ends_with("new.txt"));
        assert!(!f.exists());
        assert!(renamed.exists());
        // Collision is refused.
        fs::write(d.path().join("taken.txt"), b"x").unwrap();
        assert!(rename(&renamed, "taken.txt").is_err());
    }

    #[test]
    fn copy_into_recurses_and_dedups() {
        let d = tmp();
        let src = d.path().join("dir");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("inner.txt"), b"hello").unwrap();
        let destdir = d.path().join("dest");
        fs::create_dir(&destdir).unwrap();

        let copied = copy_into(&src, &destdir).unwrap();
        assert!(copied.join("inner.txt").exists());
        assert_eq!(fs::read(copied.join("inner.txt")).unwrap(), b"hello");
        // Original still present (copy, not move).
        assert!(src.join("inner.txt").exists());
        // A second copy into the same dir dedups the folder name.
        let copied2 = copy_into(&src, &destdir).unwrap();
        assert_eq!(copied2.file_name().unwrap().to_string_lossy(), "dir 2");
    }

    #[test]
    fn trash_dir_is_the_xdg_home_trash_files_dir() {
        // The Trash sidebar entry + the "am I in the trash?" check both hinge on
        // this path being the freedesktop home-trash files dir.
        if let Some(t) = trash_dir() {
            assert!(t.ends_with("Trash/files"), "trash dir is <data>/Trash/files: {t:?}");
            assert!(is_trash_dir(&t), "the trash dir recognises itself");
            assert!(!is_trash_dir(Path::new("/tmp/not-trash")), "an unrelated dir is not the trash");
        }
    }

    #[test]
    fn move_into_relocates() {
        let d = tmp();
        let f = d.path().join("move-me.txt");
        fs::write(&f, b"x").unwrap();
        let destdir = d.path().join("dest");
        fs::create_dir(&destdir).unwrap();
        let moved = move_into(&f, &destdir).unwrap();
        assert!(moved.exists());
        assert!(!f.exists());
        assert_eq!(moved.parent().unwrap(), destdir);
    }
}
