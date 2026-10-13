//! Explicit browser data-root routing. Defaults preserve existing normal paths.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
static ROOT: OnceLock<PathBuf> = OnceLock::new();
static PRIVATE_ROOT: OnceLock<PathBuf> = OnceLock::new();
use ui::webview::chromium::{
    PRIVATE_ROOT_HELPER, PRIVATE_ROOT_LOCK, PRIVATE_ROOT_MARKER, PRIVATE_ROOT_MARKER_CONTENT,
    PRIVATE_ROOT_PREFIX, is_owned_private_root,
};
const PRIVATE_ROOT_OWNER: &str = ".tilo-private-owner-pid";
/// Configure the optional browser data root once for this process.
///
/// # Errors
/// Returns an error for a relative root or a conflicting previous configuration.
pub fn configure(root: Option<&str>) -> Result<(), String> {
    let root = root
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TILO_BROWSER_APP_ROOT").map(PathBuf::from));
    if let Some(root) = root {
        if !root.is_absolute() {
            return Err("Browser app root must be an absolute path".into());
        }
        if let Some(existing) = ROOT.get() {
            if existing != &root {
                return Err("Browser app root is already configured".into());
            }
        } else {
            if is_owned_private_root(&root) {
                let _ = PRIVATE_ROOT.set(root.clone());
            }
            let _ = ROOT.set(root);
        }
    }
    Ok(())
}
/// Route this process's browser data for the window it opens: a private
/// window gets an isolated private root, removed when it is done; any other
/// window the root `TILO_BROWSER_APP_ROOT` names, else the platform's.
///
/// # Errors
/// Returns an error when the private root cannot be allocated or the data
/// root is invalid.
pub fn prepare_window(private: bool) -> Result<(), String> {
    static PREPARED: OnceLock<Result<(), String>> = OnceLock::new();
    PREPARED
        .get_or_init(|| {
            if !private {
                return configure(None);
            }
            let root = allocate_private_root()?;
            let text = root
                .to_str()
                .ok_or_else(|| "Private browser root is not valid UTF-8".to_string())?;
            configure(Some(text)).inspect_err(|_| {
                if is_owned_private_root(&root) {
                    let _ = std::fs::remove_dir_all(&root);
                }
            })?;
            tracing::info!(private_root = %root.display(), "Private browser root allocated");
            Ok(())
        })
        .clone()
}

pub fn explicit_root() -> Option<&'static Path> {
    ROOT.get().map(PathBuf::as_path)
}
pub fn data_dir() -> Option<PathBuf> {
    ROOT.get()
        .cloned()
        .or_else(|| tilo_paths::data_dir().map(|p| p.join("tilo-browser")))
}
pub fn cache_dir() -> Option<PathBuf> {
    ROOT.get()
        .map(|p| p.join("cache"))
        .or_else(|| tilo_paths::cache_dir().map(|p| p.join("tilo-browser")))
}

/// Create and mark an isolated private-storage directory.
///
/// # Errors
/// Returns an error if a unique directory cannot be created or initialized securely.
pub fn allocate_private_root() -> Result<PathBuf, String> {
    sweep_orphaned_private_roots();
    for _ in 0..16 {
        let root =
            std::env::temp_dir().join(format!("{PRIVATE_ROOT_PREFIX}{}", uuid::Uuid::new_v4()));
        match std::fs::create_dir(&root) {
            Ok(()) => {
                tilo_files::native::private::restrict_directory(&root)
                    .map_err(|error| error.to_string())?;
                let initialized = write_private_root_file(
                    &root.join(PRIVATE_ROOT_MARKER),
                    PRIVATE_ROOT_MARKER_CONTENT.as_bytes(),
                )
                .and_then(|()| {
                    write_private_root_file(
                        &root.join(PRIVATE_ROOT_OWNER),
                        std::process::id().to_string().as_bytes(),
                    )
                })
                .and_then(|()| {
                    write_private_root_file(&root.join(PRIVATE_ROOT_LOCK), b"private-root-lock\n")
                });
                if let Err(error) = initialized {
                    let _ = std::fs::remove_dir_all(&root);
                    return Err(format!("Cannot initialize private browser root: {error}"));
                }
                return root.canonicalize().map_err(|error| error.to_string());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("Cannot allocate private browser root: {error}")),
        }
    }
    Err("Could not allocate a unique private browser root".into())
}

fn write_private_root_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    tilo_files::native::private::owner_only(&mut options);
    let mut file = options.open(path)?;
    std::io::Write::write_all(&mut file, bytes)
}

/// Serialize private cache writes against native-helper root cleanup.
///
/// Cleanup takes the same file lock exclusively before removing the root.
/// Opening without `create` ensures a writer arriving after cleanup cannot
/// recreate any part of the path.
pub(crate) fn private_root_write_guard(
    directory: &Path,
) -> Result<Option<advisory_lock::FileLock>, String> {
    let Some(root) = ROOT.get() else {
        return Ok(None);
    };
    if PRIVATE_ROOT.get() != Some(root) {
        return Ok(None);
    }
    if !directory.starts_with(root) {
        return Err("Private cache path escaped its owned root".into());
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(PRIVATE_ROOT_LOCK))
        .map_err(|_| "Private browser cache is closing".to_string())?;
    let lock = advisory_lock::FileLock::shared(file)
        .map_err(|_| "Private browser cache lock is unavailable".to_string())?;
    if !is_owned_private_root(root) {
        return Err("Private browser cache is closing".into());
    }
    Ok(Some(lock))
}

fn sweep_orphaned_private_roots() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static RUNNING: AtomicBool = AtomicBool::new(false);
    // Orphans are recognised by their dead owner process.
    if !host_info::process::CAN_CHECK_LIVENESS {
        return;
    }
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    // Directory enumeration can be large. Keep it off the UI thread and admit
    // only one cleanup worker per process; explicit window cleanup is unchanged.
    if std::thread::Builder::new()
        .name("browser-private-orphans".into())
        .spawn(|| {
            sweep_orphaned_private_roots_in(&std::env::temp_dir());
            RUNNING.store(false, Ordering::Release);
        })
        .is_err()
    {
        RUNNING.store(false, Ordering::Release);
    }
}

fn sweep_orphaned_private_roots_in(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    sweep_orphaned_private_entries(entries.flatten().map(|entry| entry.path()));
}

fn sweep_orphaned_private_entries(entries: impl Iterator<Item = PathBuf>) {
    let mut attempted_removals = 0;
    for root in entries {
        // Unrelated entries must not consume the bounded deletion budget.
        if !root
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| name.starts_with(PRIVATE_ROOT_PREFIX))
        {
            continue;
        }
        if !is_owned_private_root(&root) {
            continue;
        }
        let owner = root.join(PRIVATE_ROOT_OWNER);
        let owner_is_dead = std::fs::symlink_metadata(&owner).is_ok_and(|metadata| {
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 32
                && std::fs::read_to_string(&owner)
                    .ok()
                    .and_then(|value| value.parse::<i32>().ok())
                    .is_some_and(|pid| pid > 1 && !host_info::process::is_alive(pid))
        });
        let helper = root.join(PRIVATE_ROOT_HELPER);
        let helper_is_live_or_untrusted = match std::fs::symlink_metadata(&helper) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= 32 =>
            {
                std::fs::read_to_string(&helper)
                    .ok()
                    .and_then(|value| value.parse::<i32>().ok())
                    .is_none_or(|pid| pid <= 1 || host_info::process::is_alive(pid))
            }
            Err(_) | Ok(_) => true,
        };
        if owner_is_dead && !helper_is_live_or_untrusted {
            let _ = std::fs::remove_dir_all(root);
            attempted_removals += 1;
            if attempted_removals == 256 {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn root(parent: &Path, owner_pid: u32, helper_pid: Option<u32>) -> PathBuf {
        let root = parent.join(format!("{PRIVATE_ROOT_PREFIX}{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join(PRIVATE_ROOT_MARKER), PRIVATE_ROOT_MARKER_CONTENT).unwrap();
        std::fs::write(root.join(PRIVATE_ROOT_OWNER), owner_pid.to_string()).unwrap();
        if let Some(pid) = helper_pid {
            std::fs::write(root.join(PRIVATE_ROOT_HELPER), pid.to_string()).unwrap();
        }
        root
    }

    /// Private storage roots allocation and cleanup.
    #[test]
    fn private_roots_are_owned_unique_and_swept_within_budget() {
        // Phase `private_roots_are_unique_owned_and_never_reuse_normal_overrides`.
        {
            let normal = tempfile::tempdir().unwrap();
            let first = allocate_private_root().unwrap();
            let second = allocate_private_root().unwrap();
            assert!(first.is_absolute());
            assert_ne!(first, second);
            assert_ne!(first, normal.path());
            assert!(is_owned_private_root(&first));
            assert!(is_owned_private_root(&second));
            assert!(!is_owned_private_root(normal.path()));
            std::fs::remove_dir_all(first).unwrap();
            std::fs::remove_dir_all(second).unwrap();
        }

        // Phase `allocation_sweeps_only_owned_roots_with_dead_processes`.
        #[cfg(unix)]
        {
            let parent = tempfile::tempdir().unwrap();
            let dead = root(parent.path(), i32::MAX as u32, Some(i32::MAX as u32));
            let live_owner = root(parent.path(), std::process::id(), None);
            let live_helper = root(parent.path(), i32::MAX as u32, Some(std::process::id()));
            sweep_orphaned_private_roots_in(parent.path());
            assert!(!dead.exists());
            assert!(live_owner.exists());
            assert!(live_helper.exists());
            std::fs::remove_dir_all(live_owner).unwrap();
            std::fs::remove_dir_all(live_helper).unwrap();
        }

        // Phase `unrelated_temporary_entries_do_not_starve_private_cleanup`.
        #[cfg(unix)]
        {
            let parent = tempfile::tempdir().unwrap();
            let unrelated = (0..300)
                .map(|index| {
                    let path = parent.path().join(format!("unrelated-{index}"));
                    std::fs::create_dir(&path).unwrap();
                    path
                })
                .collect::<Vec<_>>();
            let orphan = parent.path().join(format!("{PRIVATE_ROOT_PREFIX}orphan"));
            std::fs::create_dir(&orphan).unwrap();
            std::fs::write(
                orphan.join(PRIVATE_ROOT_MARKER),
                PRIVATE_ROOT_MARKER_CONTENT,
            )
            .unwrap();
            std::fs::write(orphan.join(PRIVATE_ROOT_OWNER), i32::MAX.to_string()).unwrap();
            sweep_orphaned_private_entries(
                unrelated
                    .iter()
                    .cloned()
                    .chain(std::iter::once(orphan.clone())),
            );
            assert!(
                !orphan.exists(),
                "unrelated temporary entries must not consume the cleanup budget"
            );
            assert!(unrelated.iter().all(|path| path.is_dir()));
        }

        // Phase `cleanup_budget_counts_orphans_and_preserves_live_roots`.
        #[cfg(unix)]
        {
            let parent = tempfile::tempdir().unwrap();
            let live = root(parent.path(), std::process::id(), None);
            let orphans = (0..257)
                .map(|_| root(parent.path(), i32::MAX as u32, None))
                .collect::<Vec<_>>();
            sweep_orphaned_private_entries(
                std::iter::once(live.clone()).chain(orphans.iter().cloned()),
            );
            assert!(live.is_dir());
            assert!(orphans.iter().take(256).all(|path| !path.exists()));
            assert!(
                orphans.last().unwrap().is_dir(),
                "one pass must attempt at most 256 removals"
            );
        }
    }
}
