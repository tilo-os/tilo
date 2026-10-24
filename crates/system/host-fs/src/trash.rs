//! The user's trash, where the host lets apps manage it.
//!
//! On Unix hosts, macOS included, the home Trash (the XDG trash spec's
//! `Trash` in the data directory: `$XDG_DATA_HOME/Trash`, or
//! `~/Library/Application Support/Trash` on macOS) is managed here directly
//! (`freedesktop`): moving an item in, restoring it and purging it need
//! nothing but the item, the Trash directories and a rename. In particular
//! they need no mount table, which a sandboxed app cannot read (`/proc` is
//! closed to it), and every item moved in can be listed, restored and purged
//! again, which the macOS system Trash does not let an app do. Only an item
//! on another filesystem than every Trash this process can reach goes to the
//! `trash` crate, which finds that mount's own Trash (on macOS through
//! `NSFileManager`, without Finder). On Windows moving items to the trash
//! goes through the `trash` crate; restoring and purging single items report
//! that they are unavailable there.

use std::path::{Path, PathBuf};

#[cfg(unix)]
mod freedesktop;

/// Move files to the host Trash ([`trash()`] each).
///
/// # Errors
///
/// Returns an error if home Trash is unsafe or unavailable on a Unix host, or
/// if the host backend cannot move the files to Trash.
pub fn delete(paths: &[PathBuf]) -> Result<(), String> {
    paths.iter().try_for_each(|path| trash(path).map(drop))
}

/// Move `path` to the host Trash, returning the Trash entry it became where
/// the host lets apps see it (`Trash/files/<name>` on a Unix host), for
/// [`restore`] to take it back.
///
/// On a Unix host the item goes to the home Trash, or, when it is on another
/// filesystem, to the person's own Trash the launcher granted
/// ([`user_trash_dir`]) if that one is on its filesystem; only failing both
/// does the `trash` crate look for its mount's Trash (which reports `None`).
///
/// # Errors
///
/// Returns an error if home Trash is unsafe or unavailable on a Unix host, or
/// if the host backend cannot move the item to Trash.
pub fn trash(path: &Path) -> Result<Option<PathBuf>, String> {
    #[cfg(unix)]
    {
        let home = ensure_trash_dir().map_err(|error| error.to_string())?;
        let mut trashes = vec![home];
        if let Some(user) = user_trash_dir().filter(|user| !trashes.contains(user)) {
            trashes.push(user);
        }
        match freedesktop::move_in(path, &trashes, &|files| {
            ensure_private_trash(files).map(drop)
        }) {
            Ok(entry) => Ok(Some(entry)),
            Err(freedesktop::MoveError::OtherFilesystem) => {
                mount_trash(path).map(|()| None).map_err(|error| {
                    format!(
                        "{} is on another filesystem than Trash, and its own Trash could not be \
                         used: {error}",
                        path.display()
                    )
                })
            }
            Err(freedesktop::MoveError::Io(error)) => Err(error.to_string()),
        }
    }
    #[cfg(not(unix))]
    {
        ::trash::delete(path)
            .map(|()| None)
            .map_err(|error| error.to_string())
    }
}

/// Move `path` to the Trash of its own mount, through the `trash` crate.
#[cfg(unix)]
fn mount_trash(path: &Path) -> Result<(), ::trash::Error> {
    #[cfg(target_os = "macos")]
    {
        use ::trash::macos::{DeleteMethod, TrashContextExtMacos as _};
        // The crate's default asks Finder over Apple Events, which needs an
        // Automation grant the user is prompted for and fails outright
        // without a GUI session. NSFileManager moves the item itself.
        let mut context = ::trash::TrashContext::new();
        context.set_delete_method(DeleteMethod::NsFileManager);
        context.delete(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        ::trash::delete(path)
    }
}

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
    ensure_private_trash(&files)
}

/// [`ensure_trash_dir`] for the Trash whose files directory is `files`.
fn ensure_private_trash(files: &Path) -> std::io::Result<PathBuf> {
    let files = files.to_path_buf();
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
                private
                    && metadata.uid() == rustix::process::geteuid().as_raw()
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

/// Permanently remove everything in the home Trash. Returns how many items
/// were purged.
pub fn empty() -> Result<usize, String> {
    #[cfg(unix)]
    {
        let files = trash_dir().ok_or("home Trash is unavailable")?;
        freedesktop::empty(&files).map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        let items = trash::os_limited::list().map_err(|e| e.to_string())?;
        let n = items.len();
        trash::os_limited::purge_all(items).map_err(|e| e.to_string())?;
        Ok(n)
    }
}

/// Restore the trashed items backing `entries` (paths inside a Trash's
/// `files` directory, such as [`trash()`] returns) to their original
/// locations. Returns how many were restored; an entry no longer in Trash is
/// not counted.
pub fn restore(entries: &[PathBuf]) -> Result<usize, String> {
    #[cfg(unix)]
    {
        freedesktop::restore(entries).map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = entries;
        Err("Trash restore is unavailable on this host".into())
    }
}

/// Permanently delete the trashed items backing `entries` (file + info).
/// Returns how many were purged; an entry no longer in Trash is not counted.
pub fn purge(entries: &[PathBuf]) -> Result<usize, String> {
    #[cfg(unix)]
    {
        freedesktop::purge(entries).map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = entries;
        Err("Trash purge is unavailable on this host".into())
    }
}

/// Whether `path` is the home-trash files directory.
#[must_use]
pub fn is_trash_dir(path: &Path) -> bool {
    trash_dir().is_some_and(|t| path == t)
}
