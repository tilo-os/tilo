//! The network early boot needs to reach a sibling.
//!
//! The image's `NetworkManager` runs in the initramfs (`NETWORK_UNIT`, shipped by
//! the `tilo-sibling-unlock` dracut module). Every wired link comes up with
//! DHCP on its own; the only profiles added are the networks the owner chose
//! for unlocking, written to `NetworkManager`'s runtime keyfile directory for
//! this one attempt. Wired routes keep `NetworkManager`'s lower default metric,
//! so Ethernet is preferred whenever it is there.
//!
//! [`Network::down`] (also run on drop) stops `NetworkManager`, removes the
//! profiles and their secrets, and takes the links down again, so the booted
//! system's own `NetworkManager` starts from clean links.

use crate::exec;
use crate::material;
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// The initramfs unit that runs `NetworkManager` for an unlock.
pub const NETWORK_UNIT: &str = "tilo-sibling-unlock-network.service";

/// `NetworkManager`'s runtime keyfile directory.
pub const KEYFILE_DIR: &str = "/run/NetworkManager/system-connections";

/// A started unlock network.
#[derive(Debug)]
pub struct Network {
    keyfiles: Vec<PathBuf>,
    started: bool,
}

/// Writes the owner's unlock networks into `dir` and returns their paths.
///
/// # Errors
/// When a profile has an unusable name or cannot be written.
pub fn write_profiles(dir: &Path, networks: &[material::Network]) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let mut written = Vec::new();
    for network in networks {
        if !material::valid_network_id(&network.id) {
            bail!("unlock network {:?} has an unusable name", network.id);
        }
        let path = dir.join(format!(
            "tilo-unlock-{}.nmconnection",
            network.id.replace(' ', "_")
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("could not write {}", path.display()))?;
        file.write_all(network.keyfile.as_bytes())?;
        written.push(path);
    }
    Ok(written)
}

/// Overwrites and removes each profile.
pub fn remove_profiles(keyfiles: &[PathBuf]) {
    for path in keyfiles {
        if let Ok(meta) = std::fs::metadata(path) {
            let zeros = vec![0_u8; usize::try_from(meta.len()).unwrap_or(0)];
            let _ = std::fs::write(path, zeros);
        }
        let _ = std::fs::remove_file(path);
    }
}

impl Network {
    /// Writes the profiles and starts `NetworkManager`.
    ///
    /// # Errors
    /// When the profiles cannot be written or the unit cannot be started.
    pub fn up(networks: &[material::Network]) -> Result<Self> {
        let keyfiles = write_profiles(Path::new(KEYFILE_DIR), networks)?;
        let mut network = Self {
            keyfiles,
            started: false,
        };
        exec::check("systemctl", ["start", "--no-block", NETWORK_UNIT], None)
            .context("could not start the unlock network")?;
        network.started = true;
        Ok(network)
    }

    /// Stops `NetworkManager`, wipes the profiles and takes the links down.
    pub fn down(&mut self) {
        if self.started {
            let _ = exec::check("systemctl", ["stop", NETWORK_UNIT], None);
            self.started = false;
            flush_links();
        }
        remove_profiles(&self.keyfiles);
        self.keyfiles.clear();
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        self.down();
    }
}

/// Removes the addresses `NetworkManager` configured and sets every link but
/// loopback down.
fn flush_links() {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "lo" {
            continue;
        }
        let _ = exec::check("ip", ["address", "flush", "dev", name], None);
        let _ = exec::check("ip", ["link", "set", "dev", name, "down"], None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn profiles_are_private_and_wiped() {
        let dir = tempfile::tempdir().unwrap();
        let networks = vec![material::Network {
            id: "Home Wi-Fi".into(),
            keyfile: "[wifi-security]\npsk=hunter2\n".into(),
        }];
        let written = write_profiles(dir.path(), &networks).unwrap();
        assert_eq!(
            written,
            vec![dir.path().join("tilo-unlock-Home_Wi-Fi.nmconnection")]
        );
        let mode = std::fs::metadata(&written[0]).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        remove_profiles(&written);
        assert!(!written[0].exists());
    }
}
