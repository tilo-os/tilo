//! Explicit browser data-root routing. Defaults preserve existing normal paths.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
static ROOT: OnceLock<PathBuf> = OnceLock::new();
pub const PRIVATE_ROOT_PREFIX: &str = "qos-browser-private-";
pub const PRIVATE_ROOT_MARKER: &str = ".qos-private-root-v1";
pub const PRIVATE_ROOT_MARKER_CONTENT: &str = "qos-browser-owned-private-root-v1\n";
const PRIVATE_ROOT_OWNER: &str = ".qos-private-owner-pid";
const PRIVATE_ROOT_HELPER: &str = ".qos-private-helper-pid";
pub fn configure(root: Option<&str>) -> Result<(), String> {
    let root = root
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("QOS_BROWSER_APP_ROOT").map(PathBuf::from));
    if let Some(root) = root {
        if !root.is_absolute() {
            return Err("Browser app root must be an absolute path".into());
        }
        if let Some(existing) = ROOT.get() {
            if existing != &root {
                return Err("Browser app root is already configured".into());
            }
        } else {
            let _ = ROOT.set(root);
        }
    }
    Ok(())
}
pub fn explicit_root() -> Option<&'static Path> {
    ROOT.get().map(PathBuf::as_path)
}
pub fn data_dir() -> Option<PathBuf> {
    ROOT.get()
        .cloned()
        .or_else(|| dirs::data_dir().map(|p| p.join("qos-browser")))
}
pub fn config_dir() -> Option<PathBuf> {
    ROOT.get()
        .cloned()
        .or_else(|| dirs::config_dir().map(|p| p.join("qos-browser")))
}
pub fn cache_dir() -> Option<PathBuf> {
    ROOT.get()
        .map(|p| p.join("cache"))
        .or_else(|| dirs::cache_dir().map(|p| p.join("qos-browser")))
}

pub fn allocate_private_root() -> Result<PathBuf, String> {
    sweep_orphaned_private_roots();
    for _ in 0..16 {
        let root =
            std::env::temp_dir().join(format!("{PRIVATE_ROOT_PREFIX}{}", uuid::Uuid::new_v4()));
        match std::fs::create_dir(&root) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                        .map_err(|error| error.to_string())?;
                }
                let initialized = write_private_root_file(
                    &root.join(PRIVATE_ROOT_MARKER),
                    PRIVATE_ROOT_MARKER_CONTENT.as_bytes(),
                )
                .and_then(|()| {
                    write_private_root_file(
                        &root.join(PRIVATE_ROOT_OWNER),
                        std::process::id().to_string().as_bytes(),
                    )
                });
                if let Err(error) = initialized {
                    let _ = std::fs::remove_dir_all(&root);
                    return Err(format!("Cannot initialize private browser root: {error}"));
                }
                return root.canonicalize().map_err(|error| error.to_string());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Cannot allocate private browser root: {error}")),
        }
    }
    Err("Could not allocate a unique private browser root".into())
}

pub fn is_owned_private_root(root: &Path) -> bool {
    std::fs::symlink_metadata(root)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        && root
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| name.starts_with(PRIVATE_ROOT_PREFIX))
        && std::fs::symlink_metadata(root.join(PRIVATE_ROOT_MARKER)).is_ok_and(|metadata| {
            metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 64
        })
        && std::fs::read_to_string(root.join(PRIVATE_ROOT_MARKER))
            .is_ok_and(|value| value == PRIVATE_ROOT_MARKER_CONTENT)
}

fn write_private_root_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    std::io::Write::write_all(&mut file, bytes)
}

#[cfg(unix)]
fn process_is_alive(pid: i32) -> bool {
    // SAFETY: signal 0 does not deliver a signal; it asks the kernel whether
    // the process exists and whether this process may signal it.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn sweep_orphaned_private_roots() {
    sweep_orphaned_private_roots_in(&std::env::temp_dir());
}

#[cfg(unix)]
fn sweep_orphaned_private_roots_in(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten().take(256) {
        let root = entry.path();
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
                    .is_some_and(|pid| pid > 1 && !process_is_alive(pid))
        });
        let helper = root.join(PRIVATE_ROOT_HELPER);
        let helper_is_live_or_untrusted = match std::fs::symlink_metadata(&helper) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => true,
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= 32 =>
            {
                std::fs::read_to_string(&helper)
                    .ok()
                    .and_then(|value| value.parse::<i32>().ok())
                    .is_none_or(|pid| pid <= 1 || process_is_alive(pid))
            }
            Ok(_) => true,
        };
        if owner_is_dead && !helper_is_live_or_untrusted {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[cfg(not(unix))]
fn sweep_orphaned_private_roots() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_roots_are_unique_owned_and_never_reuse_normal_overrides() {
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

    #[test]
    #[cfg(unix)]
    fn allocation_sweeps_only_owned_roots_with_dead_processes() {
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
}
