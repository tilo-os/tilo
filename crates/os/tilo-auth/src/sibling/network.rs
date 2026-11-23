//! The network early boot needs to reach a sibling.

use super::material;
use anyhow::{Context as _, Result, bail};
use std::path::{Path, PathBuf};
use tilo_rollback::argv;
use tilo_rollback::exec::{Runner, require};

pub const NETWORK_UNIT: &str = "tilo-unlock-network.service";

pub const KEYFILE_DIR: &str = "/run/NetworkManager/system-connections";

pub struct Network<'a> {
    runner: &'a (dyn Runner + Sync),
    keyfiles: Vec<PathBuf>,
    started: bool,
}

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
        tilo_files::native::atomic::write_regenerable_with_mode(
            &path,
            network.keyfile.as_bytes(),
            0o600,
        )
        .with_context(|| format!("could not write {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

pub fn remove_profiles(keyfiles: &[PathBuf]) {
    for path in keyfiles {
        if let Ok(meta) = std::fs::metadata(path) {
            let zeros = vec![0_u8; usize::try_from(meta.len()).unwrap_or(0)];
            let _ = std::fs::write(path, zeros);
        }
        let _ = std::fs::remove_file(path);
    }
}

impl<'a> Network<'a> {
    pub fn up(runner: &'a (dyn Runner + Sync), networks: &[material::Network]) -> Result<Self> {
        let keyfiles = write_profiles(Path::new(KEYFILE_DIR), networks)?;
        let mut network = Self {
            runner,
            keyfiles,
            started: false,
        };
        require(
            runner,
            "systemctl",
            &argv!["start", "--no-block", NETWORK_UNIT],
            "starting the unlock network",
        )?;
        network.started = true;
        Ok(network)
    }

    pub fn down(&mut self) {
        if self.started {
            let _ = require(
                self.runner,
                "systemctl",
                &argv!["stop", NETWORK_UNIT],
                "stopping the unlock network",
            );
            self.started = false;
            flush_links(self.runner);
        }
        remove_profiles(&self.keyfiles);
        self.keyfiles.clear();
    }
}

impl Drop for Network<'_> {
    fn drop(&mut self) {
        self.down();
    }
}

/// Removes the addresses `NetworkManager` configured and sets every link but
/// loopback down.
fn flush_links(runner: &dyn Runner) {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "lo" {
            continue;
        }
        let _ = require(
            runner,
            "ip",
            &argv!["address", "flush", "dev", name],
            "flushing a link",
        );
        let _ = require(
            runner,
            "ip",
            &argv!["link", "set", "dev", name, "down"],
            "setting a link down",
        );
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
