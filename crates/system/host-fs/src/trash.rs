//! The user's trash, where the host lets apps manage it.
//!
//! Enumerating, restoring and purging trashed items is available on Linux
//! and other freedesktop hosts (the XDG home trash); elsewhere those
//! operations report that they are unavailable. Moving items to the trash is
//! available everywhere through the `trash` crate.

use std::path::{Path, PathBuf};

/// Prepare the home Trash without making its contents readable by other users.
/// Existing nonprivate directories and symlinks are refused, never repaired by
/// changing their permissions or following them into another directory.
///
/// # Errors
///
/// Returns an error when Trash is unavailable, a directory cannot be created,
/// or an existing Trash directory is not private and owned by the current user.
pub fn ensure_trash_dir() -> std::io::Result<PathBuf> {
    let files = trash_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "home Trash is unavailable")
    })?;
    let trash = files.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Trash root is unavailable",
        )
    })?;
    let data = trash.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "data directory is unavailable",
        )
    })?;
    std::fs::create_dir_all(data)?;
    private_trash_directory(trash)?;
    private_trash_directory(&files)?;
    private_trash_directory(&trash.join("info"))?;
    Ok(files)
}

fn private_trash_directory(path: &Path) -> std::io::Result<()> {
    match crate::create_private_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path)?;
            let private = metadata.is_dir();
            #[cfg(unix)]
            let private = {
                use std::os::unix::fs::MetadataExt as _;
                // SAFETY: geteuid only reads this process's effective user.
                private
                    && metadata.uid() == unsafe { libc::geteuid() }
                    // The six group/other permission bits must all be clear.
                    && metadata.mode().trailing_zeros() >= 6
            };
            if private {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Trash directory is not a private directory owned by this user",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

/// The XDG "home trash" files directory (`~/.local/share/Trash/files`), where
/// trashed files are kept. `None` when there is no data dir, or on Windows.
#[must_use]
pub fn trash_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        None
    }
    #[cfg(not(windows))]
    tilo_paths::data_dir().map(|directory| directory.join("Trash/files"))
}

/// The user's Trash accessible through an existing launcher grant. This is
/// separate from a confined app's private-data Trash; callers can present
/// both without redirecting legacy operations into another namespace.
#[must_use]
pub fn user_trash_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        None
    }
    #[cfg(not(windows))]
    {
        let data = if std::env::var_os("TILO_APP_DIR").is_some() {
            std::env::var_os("TILO_USER_DATA_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        } else {
            tilo_paths::data_dir()
        };
        data.map(|directory| directory.join("Trash/files"))
    }
}

/// Permanently remove everything in the trash. Returns how many items were purged.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable, or listing or
/// purging its items fails.
pub fn empty() -> Result<usize, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let items = trash::os_limited::list().map_err(|e| e.to_string())?;
        let n = items.len();
        trash::os_limited::purge_all(items).map_err(|e| e.to_string())?;
        Ok(n)
    }
    #[cfg(target_os = "macos")]
    {
        Err("Trash management is unavailable on this host".into())
    }
}

/// Restore the trashed items backing `entries` (paths inside [`trash_dir`])
/// to their original locations. Returns how many were restored.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable, or listing or
/// restoring the matched items fails (e.g. the original location no longer
/// exists).
pub fn restore(entries: &[PathBuf]) -> Result<usize, String> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let items = items_for(entries)?;
        let n = items.len();
        trash::os_limited::restore_all(items).map_err(|e| e.to_string())?;
        Ok(n)
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        let _ = entries;
        Err("Trash restore is unavailable on this host".into())
    }
}

/// Permanently delete the trashed items backing `entries` (file + info).
/// Returns how many were purged.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable, or listing or
/// purging the matched items fails.
pub fn purge(entries: &[PathBuf]) -> Result<usize, String> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let items = items_for(entries)?;
        let n = items.len();
        trash::os_limited::purge_all(items).map_err(|e| e.to_string())?;
        Ok(n)
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        let _ = entries;
        Err("Trash purge is unavailable on this host".into())
    }
}

/// Whether trash `item` backs the in-trash file named `files_name` (the name
/// shown in `Trash/files`). The item's `.trashinfo` file is
/// `Trash/info/<files_name>.trashinfo`, so match on that.
#[cfg(all(unix, not(target_os = "macos")))]
fn item_backs(item: &trash::TrashItem, files_name: &std::ffi::OsStr) -> bool {
    Path::new(&item.id)
        .file_name()
        .and_then(|f| f.to_str())
        .and_then(|f| f.strip_suffix(".trashinfo"))
        .is_some_and(|stem| std::ffi::OsStr::new(stem) == files_name)
}

/// The trash items backing the given in-trash file paths (matched by their
/// `.trashinfo` file name, which is robust to on-trash name deduplication).
#[cfg(all(unix, not(target_os = "macos")))]
fn items_for(entries: &[PathBuf]) -> Result<Vec<trash::TrashItem>, String> {
    let wanted: Vec<_> = entries
        .iter()
        .filter_map(|p| p.file_name().map(std::borrow::ToOwned::to_owned))
        .collect();
    Ok(trash::os_limited::list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|it| wanted.iter().any(|w| item_backs(it, w)))
        .collect())
}

/// Whether `path` is the home-trash files directory.
#[must_use]
pub fn is_trash_dir(path: &Path) -> bool {
    trash_dir().is_some_and(|t| path == t)
}
