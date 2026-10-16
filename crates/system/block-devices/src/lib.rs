//! Linux block-device inventory shared by the Disks app, the media imager and
//! the installer.
//!
//! [`lsblk`] runs `lsblk -J -b` with the caller's columns and parses the JSON
//! into a typed [`BlockDevice`] tree, tolerating the value shapes older
//! util-linux releases emit. [`udisks`] maps kernel device names to `UDisks2`
//! object paths and, with the `udisks` feature, wraps the `UDisks2` calls the
//! tools share. What each tool *does* with a device (which ones it offers,
//! what counts as the system disk) stays with the tool.

use std::path::Path;

pub mod installed_root;
pub mod lsblk;
#[cfg_attr(not(unix), path = "root_customization_unsupported.rs")]
pub mod root_customization;
pub mod udisks;

pub use lsblk::{BlockDevice, Lsblk};

/// Resolve a system tool to an absolute path, preferring the canonical system
/// locations over whatever a user's `PATH` surfaces first. A Homebrew, Nix or
/// linuxbrew `lsblk` earlier on `PATH` can silently omit `FSTYPE`, `LABEL` and
/// `UUID` (it has no udev access), which would make every partition read as an
/// unlabelled blob. Falls back to the bare name so a non-standard install
/// still works.
pub fn system_tool(name: &str) -> String {
    for dir in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        let path = format!("{dir}/{name}");
        if Path::new(&path).exists() {
            return path;
        }
    }
    name.to_string()
}
