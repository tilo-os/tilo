//! Linux block-device inventory shared by the Disks app, the media imager and the
//! installer.
//!
//! [`lsblk`] runs `lsblk -J -b` with the caller's columns into a typed
//! [`BlockDevice`] tree, tolerating what older util-linux releases emit; [`udisks`]
//! maps kernel device names to `UDisks2` object paths and wraps the calls the tools
//! share. What each tool does with a device stays with the tool.

use std::path::Path;

pub mod installed_root;
pub mod lsblk;
pub mod migration_ext4;
#[cfg_attr(not(unix), path = "root_customization_unsupported.rs")]
pub mod root_customization;
pub mod udisks;

pub use lsblk::{BlockDevice, Lsblk};

/// Resolve a system tool to an absolute path, preferring the canonical system
/// locations over whatever a user's `PATH` surfaces first. A Homebrew, Nix or
/// linuxbrew `lsblk` earlier on `PATH` has no udev access and silently omits
/// `FSTYPE`, `LABEL` and `UUID`. Falls back to the bare name.
pub fn system_tool(name: &str) -> String {
    for dir in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        let path = format!("{dir}/{name}");
        if Path::new(&path).exists() {
            return path;
        }
    }
    name.to_string()
}
