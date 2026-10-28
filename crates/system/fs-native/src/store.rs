//! The store volume: a dedicated Btrfs filesystem holding the room trees,
//! capture snapshots and retained versions.
//!
//! ```text
//! <root>/live/<room>/   one subvolume per room: the real POSIX tree
//! <root>/.epochs/       read-only capture snapshots (short-lived)
//! ```
//!
//! fanotify filesystem marks are per superblock, and a pre-content mark
//! puts the daemon in front of every access it covers. The store must
//! therefore be its own filesystem, never a subvolume of the root
//! filesystem, which would put every file on the machine behind the daemon.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::btrfs;

/// Why a path cannot serve as a Tier 1 store volume.
#[derive(Debug, PartialEq, Eq)]
pub enum VolumeError {
    NotBtrfs,
    /// The path is on the same Btrfs filesystem as `/` or `/var`.
    SharesSystemFilesystem(PathBuf),
    /// The path is not the top of its filesystem (subvolume id 5), where
    /// filesystem-wide marks must be placed.
    NotFilesystemRoot,
    Io(String),
}

impl std::fmt::Display for VolumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotBtrfs => write!(f, "not on Btrfs"),
            Self::SharesSystemFilesystem(p) => {
                write!(
                    f,
                    "same Btrfs filesystem as {}, not a dedicated volume",
                    p.display()
                )
            }
            Self::NotFilesystemRoot => write!(f, "not the root of its filesystem"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for VolumeError {}

impl From<io::Error> for VolumeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// Checks that `root` is the top of a dedicated Btrfs filesystem.
pub fn check_dedicated(root: &Path) -> Result<(), VolumeError> {
    if !btrfs::is_btrfs(root)? {
        return Err(VolumeError::NotBtrfs);
    }
    let uuid = btrfs::filesystem_uuid(root)?;
    for system in ["/", "/var", "/var/home", "/home"] {
        let p = Path::new(system);
        if btrfs::is_btrfs(p).unwrap_or(false) && btrfs::filesystem_uuid(p).ok() == Some(uuid) {
            return Err(VolumeError::SharesSystemFilesystem(p.to_path_buf()));
        }
    }
    if !is_mount_root(root)? {
        return Err(VolumeError::NotFilesystemRoot);
    }
    Ok(())
}

/// Whether `root` is the mount point of the top-level subvolume.
fn is_mount_root(root: &Path) -> io::Result<bool> {
    let canonical = fs::canonicalize(root)?;
    let info = fs::read_to_string("/proc/self/mountinfo")?;
    Ok(info.lines().any(|line| {
        let fields: Vec<&str> = line.split(' ').collect();
        // mountinfo: id parent major:minor root mount-point ...
        let (Some(fs_root), Some(mount_point)) = (fields.get(3), fields.get(4)) else {
            return false;
        };
        *fs_root == "/" && Path::new(&unescape_mountinfo(mount_point)) == canonical
    }))
}

fn unescape_mountinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('\\') {
        out.push_str(rest.get(..i).unwrap_or_default());
        let code = rest.get(i.saturating_add(1)..i.saturating_add(4));
        if let Some(b) = code.and_then(|c| u8::from_str_radix(c, 8).ok()) {
            out.push(char::from(b));
            rest = rest.get(i.saturating_add(4)..).unwrap_or_default();
        } else {
            out.push('\\');
            rest = rest.get(i.saturating_add(1)..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    out
}

/// Paths of one store volume.
#[derive(Clone, Debug)]
pub struct StoreLayout {
    pub root: PathBuf,
}

impl StoreLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn live(&self) -> PathBuf {
        self.root.join("live")
    }

    pub fn room(&self, room: &str) -> PathBuf {
        self.live().join(room)
    }

    pub fn epochs(&self) -> PathBuf {
        self.root.join(".epochs")
    }

    /// Creates `live/` and `.epochs/`, and the room's subvolume if missing.
    pub fn ensure_room(&self, room: &str) -> io::Result<PathBuf> {
        fs::create_dir_all(self.live())?;
        fs::create_dir_all(self.epochs())?;
        let path = self.room(room);
        if !path.exists() {
            btrfs::create_subvolume(&path)?;
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_volumes_decode_mountinfo_and_exclude_tmp() {
        {
            assert_eq!(unescape_mountinfo("/a\\040b"), "/a b");
            assert_eq!(unescape_mountinfo("/plain"), "/plain");
            assert_eq!(unescape_mountinfo("/x\\"), "/x\\");
        }
        {
            assert!(check_dedicated(Path::new("/proc")).is_err());
        }
    }
}
