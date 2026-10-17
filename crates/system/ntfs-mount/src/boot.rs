//! Which disks were attached when this computer started (D5 criterion 4).
//!
//! A unit records the disks once, early in boot, by name and disk sequence
//! number. The kernel gives every newly attached disk a fresh sequence
//! number, so a disk unplugged and plugged back in, or a different disk that
//! takes the same name later, no longer matches the record.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Disk name to disk sequence number, as seen at boot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootDisks {
    pub disks: BTreeMap<String, u64>,
}

/// A disk's current sequence number, if the kernel reports one.
pub fn diskseq(sys_root: &Path, disk: &str) -> Option<u64> {
    fs::read_to_string(sys_root.join("class/block").join(disk).join("diskseq"))
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

impl BootDisks {
    /// Every disk attached now.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the disk list cannot be read.
    pub fn record(sys_root: &Path) -> io::Result<Self> {
        let mut disks = BTreeMap::new();
        for entry in fs::read_dir(sys_root.join("block"))? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Some(seq) = diskseq(sys_root, &name) {
                disks.insert(name, seq);
            }
        }
        Ok(Self { disks })
    }

    /// The record, or an empty one when none was written this boot: then no
    /// disk counts as present at boot, which only ever means asking again.
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Write the record.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file cannot be written.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let staged = path.with_extension("new");
        fs::write(&staged, serde_json::to_vec(self).map_err(io::Error::other)?)?;
        fs::rename(staged, path)
    }

    pub fn contains(&self, disk: &str, seq: Option<u64>) -> bool {
        seq.is_some_and(|seq| self.disks.get(disk) == Some(&seq))
    }
}

/// The physical disks under block device `name`, following device-mapper
/// and RAID members (`slaves/`) down to partitions and their disks.
pub fn physical_disks(sys_root: &Path, name: &str) -> BTreeSet<String> {
    let mut disks = BTreeSet::new();
    let mut pending = vec![name.to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(next) = pending.pop() {
        if !seen.insert(next.clone()) {
            continue;
        }
        let Ok(dir) = fs::canonicalize(sys_root.join("class/block").join(&next)) else {
            continue;
        };
        let slaves: Vec<String> = fs::read_dir(dir.join("slaves"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        if !slaves.is_empty() {
            pending.extend(slaves);
            continue;
        }
        let disk_dir: PathBuf = if dir.join("partition").exists() {
            dir.parent().map_or_else(|| dir.clone(), Path::to_path_buf)
        } else {
            dir
        };
        if let Some(disk) = disk_dir.file_name() {
            disks.insert(disk.to_string_lossy().into_owned());
        }
    }
    disks
}

/// The source device of the mount at `target` in a `mountinfo` listing.
pub fn mount_source<'a>(mountinfo: &'a str, target: &str) -> Option<&'a str> {
    mountinfo.lines().find_map(|line| {
        let mut fields = line.split(' ');
        let mount_point = fields.nth(4)?;
        if mount_point != target {
            return None;
        }
        let mut after = line.split(" - ").nth(1)?.split(' ');
        let _fstype = after.next()?;
        after.next()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::fixture::Sys;
    use std::os::unix::fs::symlink;

    #[test]
    fn a_replugged_disk_no_longer_counts_as_present_at_boot() {
        let dir = tempfile::tempdir().unwrap();
        let sys = Sys::new(dir.path());
        sys.onboard_nvme();
        let boot = BootDisks::record(dir.path()).unwrap();
        assert!(boot.contains("nvme0n1", diskseq(dir.path(), "nvme0n1")));
        assert!(
            !boot.contains("nvme0n1", Some(10)),
            "re-added: new sequence number"
        );
        assert!(!boot.contains("sdb", Some(9)));
        assert!(!boot.contains("nvme0n1", None));

        let path = dir.path().join("run/boot-disks.json");
        boot.save(&path).unwrap();
        assert_eq!(BootDisks::load(&path), boot);
        assert_eq!(
            BootDisks::load(&dir.path().join("missing")),
            BootDisks::default()
        );
    }

    #[test]
    fn the_install_disk_is_found_through_device_mapper() {
        let dir = tempfile::tempdir().unwrap();
        let sys = Sys::new(dir.path());
        sys.onboard_nvme();
        let dm = sys.device("devices/virtual/block/dm-0", None);
        std::fs::create_dir_all(dm.join("slaves")).unwrap();
        let part = std::fs::canonicalize(dir.path().join("class/block/nvme0n1p3")).unwrap();
        symlink(&part, dm.join("slaves/nvme0n1p3")).unwrap();
        symlink(&dm, dir.path().join("class/block/dm-0")).unwrap();
        assert_eq!(
            physical_disks(dir.path(), "dm-0"),
            BTreeSet::from(["nvme0n1".to_owned()])
        );
        assert_eq!(
            physical_disks(dir.path(), "nvme0n1p3"),
            BTreeSet::from(["nvme0n1".to_owned()])
        );
    }

    #[test]
    fn mount_sources_are_read_from_mountinfo() {
        let mountinfo = "\
22 1 0:21 / / ro,relatime shared:1 - overlay composefs ro\n\
23 22 259:3 / /sysroot ro,relatime shared:2 - xfs /dev/mapper/luks-1 rw\n";
        assert_eq!(
            mount_source(mountinfo, "/sysroot"),
            Some("/dev/mapper/luks-1")
        );
        assert_eq!(mount_source(mountinfo, "/"), Some("composefs"));
        assert_eq!(mount_source(mountinfo, "/boot"), None);
    }
}
