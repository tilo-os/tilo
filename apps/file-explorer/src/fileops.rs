//! File-system mutations behind the file explorer's edit commands:
//! new folder, rename, delete-to-trash, copy and move.
//!
//! Each operation is a small, independently-testable free function that
//! works on real paths and returns a `Result<_, String>` (the string is a
//! user-facing error the app surfaces as a toast). Destination-collision
//! handling is centralised in [`unique_destination`] so copy/move never
//! silently clobber an existing file.

#[cfg(all(unix, not(target_os = "macos")))]
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

const MAX_DESTINATION_SUFFIX: u32 = 10_000;

/// Accept one portable filesystem leaf name, never a path expression.
///
/// Both action prompts and inline rename reach these operations. In
/// particular, `.` and `..` must not be accepted as names: either would make
/// a supposedly local operation address an ancestor directory instead.
fn leaf_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(crate::i18n::t("files-name-empty"));
    }
    #[cfg(windows)]
    if name.contains(':') || name.ends_with('.') {
        return Err(crate::i18n::t("files-name-invalid"));
    }
    let path = Path::new(name);
    if name.contains('/')
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || path.components().count() != 1
    {
        return Err(crate::i18n::t("files-name-slash"));
    }
    Ok(name)
}

/// Refuse a recursive directory copy or move before any destination exists.
/// Resolving the destination catches a symlink that points back into `src`.
fn ensure_destination_is_outside_source(src: &Path, dest_dir: &Path) -> Result<(), String> {
    if !std::fs::symlink_metadata(src)
        .map_err(|error| error.to_string())?
        .is_dir()
    {
        return Ok(());
    }
    let source = std::fs::canonicalize(src).map_err(|error| error.to_string())?;
    let destination = std::fs::canonicalize(dest_dir).map_err(|error| error.to_string())?;
    if destination.starts_with(&source) {
        return Err("cannot copy or move a folder into itself".into());
    }
    Ok(())
}

// --- trash ----------------------------------------------------------------

/// The XDG "home trash" files directory (`~/.local/share/Trash/files`), where
/// trashed files are kept. `None` when there is no data dir.
pub fn trash_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        None
    }
    #[cfg(not(windows))]
    dirs::data_dir().map(|d| d.join("Trash").join("files"))
}

/// Whether `path` is the home-trash files directory.
pub fn is_trash_dir(path: &Path) -> bool {
    trash_dir().is_some_and(|t| path == t)
}

/// Ensure the trash files directory exists, so opening an empty trash shows an
/// empty listing rather than a "no such directory" error. Returns the dir.
pub fn ensure_trash_dir() -> Option<PathBuf> {
    let d = trash_dir()?;
    let _ = std::fs::create_dir_all(&d);
    Some(d)
}

/// Snapshot the names currently visible in the home Trash.
///
/// Files uses this only to verify that a just-completed deletion produced an
/// exact, local restore target. Some filesystems use a different Trash or do
/// not expose one at all; in those cases the caller must simply not offer an
/// optimistic Undo control.
pub fn trash_entries_snapshot() -> Option<Vec<PathBuf>> {
    let dir = trash_dir()?;
    let entries = std::fs::read_dir(dir).ok()?;
    Some(
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect(),
    )
}

/// Permanently remove everything in the trash. Returns how many items were purged.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable, or listing or
/// purging its items fails.
#[cfg(not(target_os = "macos"))]
pub fn empty_trash() -> Result<usize, String> {
    let items = trash::os_limited::list().map_err(|e| e.to_string())?;
    let n = items.len();
    trash::os_limited::purge_all(items).map_err(|e| e.to_string())?;
    Ok(n)
}

/// Whether trash `item` backs the in-trash file named `files_name` (the name
/// shown in `Trash/files`). The item's `.trashinfo` file is
/// `Trash/info/<files_name>.trashinfo`, so match on that.
#[cfg(all(unix, not(target_os = "macos")))]
fn trash_item_backs(item: &trash::TrashItem, files_name: &OsStr) -> bool {
    Path::new(&item.id)
        .file_name()
        .and_then(|f| f.to_str())
        .and_then(|f| f.strip_suffix(".trashinfo"))
        .is_some_and(|stem| OsStr::new(stem) == files_name)
}

/// The trash items backing the given in-trash file paths (matched by their
/// `.trashinfo` file name, which is robust to on-trash name deduplication).
#[cfg(all(unix, not(target_os = "macos")))]
fn trash_items_for(entries: &[PathBuf]) -> Result<Vec<trash::TrashItem>, String> {
    let wanted: Vec<_> = entries
        .iter()
        .filter_map(|p| p.file_name().map(std::borrow::ToOwned::to_owned))
        .collect();
    Ok(trash::os_limited::list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|it| wanted.iter().any(|w| trash_item_backs(it, w)))
        .collect())
}

/// Restore the trashed items backing `entries` to their original locations.
/// Returns how many were restored.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable, or listing or
/// restoring the matched items fails (e.g. the original location no longer
/// exists).
#[cfg(all(unix, not(target_os = "macos")))]
pub fn restore_entries(entries: &[PathBuf]) -> Result<usize, String> {
    let items = trash_items_for(entries)?;
    let n = items.len();
    trash::os_limited::restore_all(items).map_err(|e| e.to_string())?;
    Ok(n)
}

/// Permanently delete the trashed items backing `entries` (file + info).
/// Returns how many were purged.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable, or listing or
/// purging the matched items fails.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn purge_entries(entries: &[PathBuf]) -> Result<usize, String> {
    let items = trash_items_for(entries)?;
    let n = items.len();
    trash::os_limited::purge_all(items).map_err(|e| e.to_string())?;
    Ok(n)
}

/// Pick a non-colliding path for `name` inside `dir`. If `dir/name` is
/// free it's returned as-is; otherwise a numeric suffix is inserted
/// before the extension: `report.txt` → `report 2.txt` → `report 3.txt`.
///
/// # Errors
/// Returns an error if a candidate cannot be inspected or all supported suffixes
/// are occupied. Never falls back to an occupied directory entry.
pub fn unique_destination(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let is_free = |candidate: &Path| match std::fs::symlink_metadata(candidate) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(crate::i18n::t_args(
            "files-destination-inspect-failed",
            &[
                crate::i18n::arg("path", candidate.to_string_lossy().into_owned()),
                crate::i18n::arg("error", error.to_string()),
            ],
        )),
    };
    // A dangling link still occupies its directory entry. Following it here
    // would let a later copy create its target, or a move replace the link.
    let candidate = dir.join(name);
    if is_free(&candidate)? {
        return Ok(candidate);
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 2..MAX_DESTINATION_SUFFIX {
        let trial = dir.join(suffixed_name(&stem, ext.as_deref(), n));
        if is_free(&trial)? {
            return Ok(trial);
        }
    }
    Err(crate::i18n::t_args(
        "files-destination-names-exhausted",
        &[
            crate::i18n::arg("name", name),
            crate::i18n::arg("path", dir.to_string_lossy().into_owned()),
        ],
    ))
}

fn suffixed_name(stem: &str, extension: Option<&str>, suffix: u32) -> String {
    match extension {
        Some(extension) => format!("{stem} {suffix}.{extension}"),
        None => format!("{stem} {suffix}"),
    }
}

/// Create a new folder named `name` (deduplicated) inside `parent`.
///
/// # Errors
///
/// Returns an error if `name` is empty, a path expression (`/`, `.` or `..`),
/// or if creating the directory fails (e.g. `parent` is not writable).
pub fn new_folder(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let name = match leaf_name(name) {
        Ok(name) => name,
        Err(_) if name.trim().is_empty() => return Err(crate::i18n::t("files-folder-name-empty")),
        Err(error) => return Err(error),
    };
    let dest = unique_destination(parent, name)?;
    std::fs::create_dir(&dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Rename `path` to `new_name` within the same directory.
///
/// # Errors
///
/// Returns an error if `new_name` is empty or a path expression, if `path`
/// has no parent directory, if a file already exists at the destination name,
/// or if the OS rename fails.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let new_name = leaf_name(new_name)?;
    let parent = path
        .parent()
        .ok_or_else(|| crate::i18n::t("files-no-parent"))?;
    let dest = parent.join(new_name);
    if dest == path {
        return Ok(dest);
    }
    if let Ok(destination) = std::fs::symlink_metadata(&dest) {
        // Case-insensitive filesystems resolve a new spelling to the source.
        // Do not mistake that for a collision, or follow links into the source.
        // symlink_metadata also protects dangling destinations, unlike exists().
        let same_source = !destination.file_type().is_symlink()
            && std::fs::symlink_metadata(path).is_ok_and(|source| !source.file_type().is_symlink())
            && std::fs::canonicalize(path)
                .ok()
                .zip(std::fs::canonicalize(&dest).ok())
                .is_some_and(|(source, destination)| source == destination);
        if !same_source {
            return Err(crate::i18n::t_args(
                "files-name-exists",
                &[crate::i18n::arg("name", new_name)],
            ));
        }
    }
    std::fs::rename(path, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Move `paths` to the trash. Falls back to a clear message if the
/// platform trash isn't available.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable or the delete
/// fails (e.g. a path no longer exists, or crosses a filesystem the trash
/// backend can't handle).
pub fn delete_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    trash::delete_all(paths).map_err(|e| e.to_string())
}

/// Recursively copy `src` into the directory `dest_dir`, choosing a
/// non-colliding name. Returns the created destination path.
///
/// # Errors
///
/// Returns an error if `src` has no file name, or the recursive copy fails
/// (a read/write/permission error partway through the tree).
pub fn copy_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    transfer_into(src, dest_dir, false, |_| {})
}

/// Move `src` into `dest_dir` without replacing a concurrent destination.
/// The source is removed only after its complete type-preserving copy exists.
///
/// # Errors
///
/// Returns an error if `src` has no file name, the copy fails, or `src` could
/// not be removed after a successful copy.
pub fn move_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    transfer_into(src, dest_dir, true, |_| {})
}

fn transfer_into(
    src: &Path,
    dest_dir: &Path,
    remove_source: bool,
    mut before_claim: impl FnMut(&Path),
) -> Result<PathBuf, String> {
    ensure_destination_is_outside_source(src, dest_dir)?;
    let name = src
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| crate::i18n::t("files-source-no-name"))?;
    let source_parent = src
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok());
    let destination_dir = std::fs::canonicalize(dest_dir).ok();
    if remove_source && source_parent.is_some() && source_parent == destination_dir {
        return Err("the item is already in this folder".into());
    }

    let path = Path::new(&name);
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned());

    for suffix in 1..MAX_DESTINATION_SUFFIX {
        let candidate_name = if suffix == 1 {
            name.clone()
        } else {
            suffixed_name(&stem, extension.as_deref(), suffix)
        };
        let destination = dest_dir.join(candidate_name);
        match std::fs::symlink_metadata(&destination) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(destination_inspection_error(&destination, error)),
        }

        before_claim(&destination);
        match copy_recursive_exclusive(src, &destination) {
            Ok(()) => {
                if remove_source {
                    remove_path(src).map_err(|error| error.to_string())?;
                }
                return Ok(destination);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }

    Err(crate::i18n::t_args(
        "files-destination-names-exhausted",
        &[
            crate::i18n::arg("name", name),
            crate::i18n::arg("path", dest_dir.to_string_lossy().into_owned()),
        ],
    ))
}

fn destination_inspection_error(path: &Path, error: std::io::Error) -> String {
    crate::i18n::t_args(
        "files-destination-inspect-failed",
        &[
            crate::i18n::arg("path", path.to_string_lossy().into_owned()),
            crate::i18n::arg("error", error.to_string()),
        ],
    )
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
    if meta.file_type().is_symlink() {
        // Never silently dereference a link: that could copy an unbounded
        // tree outside the source a person selected. Preserve the link object,
        // including a dangling link, just as a same-filesystem move does.
        copy_symlink(src, dest)
    } else if meta.is_dir() {
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

/// Copy one tree after atomically claiming its root directory entry.
///
/// `create_new`, `create_dir`, and symlink creation fail with
/// `AlreadyExists` instead of replacing a concurrently-created entry. A
/// failed copy can leave its claimed partial destination for diagnosis; it
/// never removes a path that another process may have replaced meanwhile.
fn copy_recursive_exclusive(src: &Path, dest: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(src)?;
    if metadata.file_type().is_symlink() {
        return copy_symlink(src, dest);
    }

    if metadata.is_dir() {
        std::fs::create_dir(dest)?;
        let result = (|| {
            for entry in std::fs::read_dir(src)? {
                let entry = entry?;
                copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
            }
            std::fs::set_permissions(dest, metadata.permissions())?;
            Ok(())
        })();
        return result;
    }

    let mut destination = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    let result = (|| {
        let mut source = std::fs::File::open(src)?;
        std::io::copy(&mut source, &mut destination)?;
        destination.set_permissions(metadata.permissions())?;
        Ok(())
    })();
    result
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dest: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::symlink;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    symlink(std::fs::read_link(src)?, dest)
}

#[cfg(not(unix))]
fn copy_symlink(_src: &Path, _dest: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "copying symbolic links is not supported on this platform",
    ))
}

/// # Errors
/// This host has no qualified trash-enumeration and purge provider.
#[cfg(target_os = "macos")]
pub fn empty_trash() -> Result<usize, String> {
    Err("Trash management is unavailable on this host".into())
}
/// # Errors
/// This host has no qualified trash-restore provider.
#[cfg(any(windows, target_os = "macos"))]
pub fn restore_entries(_entries: &[PathBuf]) -> Result<usize, String> {
    Err("Trash restore is unavailable on this host".into())
}
/// # Errors
/// This host has no qualified trash-item purge provider.
#[cfg(any(windows, target_os = "macos"))]
pub fn purge_entries(_entries: &[PathBuf]) -> Result<usize, String> {
    Err("Trash purge is unavailable on this host".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    #[cfg(windows)]
    fn windows_mutations_reject_stream_names_and_do_not_present_an_xdg_trash() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("document.txt");
        fs::write(&source, b"keep this file").unwrap();
        for name in ["document.txt:hidden", "document.txt."] {
            assert!(rename(&source, name).is_err());
            assert!(new_folder(directory.path(), name).is_err());
        }
        assert_eq!(fs::read(&source).unwrap(), b"keep this file");
        assert!(trash_dir().is_none());
        assert!(restore_entries(std::slice::from_ref(&source)).is_err());
        assert!(purge_entries(std::slice::from_ref(&source)).is_err());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    #[cfg(windows)]
    fn copying_a_junction_never_reads_or_copies_the_outside_tree() {
        let directory = tempfile::tempdir().unwrap();
        let outside = directory.path().join("outside");
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        for path in [&outside, &source, &destination] {
            fs::create_dir(path).unwrap();
        }
        fs::write(outside.join("private.txt"), b"outside data").unwrap();
        let junction = source.join("link");
        let result = std::process::Command::new("cmd.exe")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(copy_into(&source, &destination).is_err());
        assert!(!destination.join("source/link/private.txt").exists());
        assert_eq!(
            fs::read(outside.join("private.txt")).unwrap(),
            b"outside data"
        );
        fs::remove_dir(&junction).unwrap();
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn unique_destination_dedups() {
        let d = tmp();
        let root = d.path();
        // Free name is returned verbatim.
        assert_eq!(
            unique_destination(root, "a.txt").unwrap(),
            root.join("a.txt")
        );
        fs::write(root.join("a.txt"), b"x").unwrap();
        assert_eq!(
            unique_destination(root, "a.txt").unwrap(),
            root.join("a 2.txt")
        );
        fs::write(root.join("a 2.txt"), b"x").unwrap();
        assert_eq!(
            unique_destination(root, "a.txt").unwrap(),
            root.join("a 3.txt")
        );
        // Extension-less names.
        fs::create_dir(root.join("folder")).unwrap();
        assert_eq!(
            unique_destination(root, "folder").unwrap(),
            root.join("folder 2")
        );
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
        assert!(new_folder(d.path(), ".").is_err());
        assert!(new_folder(d.path(), "..").is_err());
        let unicode = new_folder(d.path(), "文書 📁").unwrap();
        assert!(unicode.is_dir());
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
        assert!(rename(&renamed, "..").is_err());
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
    fn recursive_copy_and_move_refuse_a_destination_inside_the_source() {
        let d = tmp();
        let source = d.path().join("source");
        let nested_destination = source.join("nested");
        fs::create_dir_all(&nested_destination).unwrap();
        fs::write(source.join("file.txt"), b"x").unwrap();

        assert!(copy_into(&source, &nested_destination).is_err());
        assert!(move_into(&source, &nested_destination).is_err());
        assert!(
            source.join("file.txt").exists(),
            "refusal has no side effect"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_preserves_symlinks_without_reading_their_targets() {
        use std::os::unix::fs::symlink;

        let d = tmp();
        let source = d.path().join("source");
        let destination = d.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        // This deliberately points outside the selected source tree. A copy
        // must reproduce the link rather than dereferencing `/etc/passwd`.
        symlink("/etc/passwd", source.join("outside-link")).unwrap();
        symlink("missing-target", source.join("dangling-link")).unwrap();

        let copied = copy_into(&source, &destination).unwrap();
        let outside = copied.join("outside-link");
        let dangling = copied.join("dangling-link");
        assert!(
            fs::symlink_metadata(&outside)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(&outside).unwrap(),
            PathBuf::from("/etc/passwd")
        );
        assert!(
            fs::symlink_metadata(&dangling)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(&dangling).unwrap(),
            PathBuf::from("missing-target")
        );
    }

    #[test]
    fn trash_dir_is_the_xdg_home_trash_files_dir() {
        // The Trash sidebar entry + the "am I in the trash?" check both hinge on
        // this path being the freedesktop home-trash files dir.
        if let Some(t) = trash_dir() {
            assert!(
                t.ends_with("Trash/files"),
                "trash dir is <data>/Trash/files: {t:?}"
            );
            assert!(is_trash_dir(&t), "the trash dir recognises itself");
            assert!(
                !is_trash_dir(Path::new("/tmp/not-trash")),
                "an unrelated dir is not the trash"
            );
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

    #[cfg(unix)]
    #[test]
    fn copy_and_move_propagate_destination_metadata_failure() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("report.txt");
        let destination = directory.path().join("destination");
        fs::write(&source, b"selected bytes").unwrap();
        symlink("destination", &destination).unwrap();
        let candidate = destination.join("report.txt");
        let metadata_error = fs::symlink_metadata(&candidate).unwrap_err();
        let expected = crate::i18n::t_args(
            "files-destination-inspect-failed",
            &[
                crate::i18n::arg("path", candidate.to_string_lossy().into_owned()),
                crate::i18n::arg("error", metadata_error.to_string()),
            ],
        );
        for result in [
            copy_into(&source, &destination),
            move_into(&source, &destination),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error, expected);
            assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
            assert_eq!(
                fs::read_link(&destination).unwrap(),
                PathBuf::from("destination")
            );
        }
    }

    #[test]
    fn exhausted_destination_names_do_not_overwrite_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("report.txt");
        let destination = directory.path().join("destination");
        fs::write(&source, b"selected bytes").unwrap();
        fs::create_dir(&destination).unwrap();
        let occupied = destination.join("report.txt");
        fs::write(&occupied, b"existing bytes").unwrap();
        for suffix in 2..10_000 {
            fs::write(
                destination.join(format!("report {suffix}.txt")),
                b"suffix bytes",
            )
            .unwrap();
        }
        assert!(copy_into(&source, &destination).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
        assert_eq!(fs::read(&occupied).unwrap(), b"existing bytes");
        assert!(move_into(&source, &destination).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
        assert_eq!(fs::read(&occupied).unwrap(), b"existing bytes");
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 9_999);
        for suffix in 2..10_000 {
            assert_eq!(
                fs::read(destination.join(format!("report {suffix}.txt"))).unwrap(),
                b"suffix bytes"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn copy_and_move_preserve_dangling_destination_entries() {
        use std::os::unix::fs::symlink;

        for move_source in [false, true] {
            for numbered_collision in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let source = directory.path().join("report.txt");
                let destination = directory.path().join("destination");
                let outside = directory.path().join("unselected-target.txt");
                fs::write(&source, b"selected bytes").unwrap();
                fs::create_dir(&destination).unwrap();
                let occupied = destination.join("report.txt");
                let (link, expected) = if numbered_collision {
                    fs::write(&occupied, b"existing bytes").unwrap();
                    (
                        destination.join("report 2.txt"),
                        destination.join("report 3.txt"),
                    )
                } else {
                    (occupied.clone(), destination.join("report 2.txt"))
                };
                symlink(&outside, &link).unwrap();
                let result = if move_source {
                    move_into(&source, &destination)
                } else {
                    copy_into(&source, &destination)
                }
                .unwrap();
                assert_eq!(result, expected);
                assert_eq!(fs::read(&result).unwrap(), b"selected bytes");
                assert_eq!(fs::read_link(&link).unwrap(), outside);
                assert!(
                    !outside.exists(),
                    "copy must not write through the destination link"
                );
                assert_eq!(source.exists(), !move_source);
                if !move_source {
                    assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
                }
                if numbered_collision {
                    assert_eq!(fs::read(&occupied).unwrap(), b"existing bytes");
                }
            }
        }
    }
}
