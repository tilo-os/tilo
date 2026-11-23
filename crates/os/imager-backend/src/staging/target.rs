//! The disk a staging operation names, by the identity a machine plan uses: a
//! drive's serial or WWN, never a `/dev` name that another boot may give a
//! different drive.
//!
//! Resolution reads the kernel's own block devices below `/sys/class/block`;
//! it does not depend on udev's `/dev/disk/by-id` links, which a privileged
//! container often lacks. The roots are parameters so tests resolve a
//! temporary tree.

use std::path::{Path, PathBuf};

/// The identity kinds a machine plan names a drive by.
pub const KINDS: [&str; 2] = ["serial", "wwid"];

/// A disk named by the identity it keeps across boots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetDisk {
    /// [`KINDS`]: the drive's serial, or its WWN.
    pub kind: String,
    /// The serial or WWN itself, as the drive reports it.
    pub identity: String,
}

impl TargetDisk {
    /// `serial:<serial>` or `wwid:<wwn>`, the way a machine plan names a disk.
    ///
    /// # Errors
    /// Any other kind, an empty identity, or one that cannot be a drive
    /// identity (whitespace, a path separator).
    pub fn parse(text: &str) -> Result<Self, String> {
        let (kind, identity) = text
            .split_once(':')
            .ok_or_else(|| format!("{text:?} is not <serial|wwid>:<identity>"))?;
        let kind = kind.trim();
        let identity = identity.trim();
        if !KINDS.contains(&kind) {
            return Err(format!(
                "{text:?}: a disk is named serial:<serial> or wwid:<wwn>"
            ));
        }
        if identity.is_empty()
            || identity.contains('/')
            || identity.chars().any(char::is_whitespace)
            || identity.chars().any(char::is_control)
        {
            return Err(format!("{text:?}: {identity:?} is not a drive identity"));
        }
        Ok(Self {
            kind: kind.to_owned(),
            identity: identity.to_owned(),
        })
    }
}

impl std::fmt::Display for TargetDisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind, self.identity)
    }
}

/// Where resolution looks: the kernel's block devices and their device nodes.
#[derive(Clone, Debug)]
pub struct Roots {
    /// `/sys/class/block`, which holds whole disks and their partitions.
    pub sys_block: PathBuf,
    /// `/dev`.
    pub dev: PathBuf,
}

impl Default for Roots {
    fn default() -> Self {
        Self {
            sys_block: PathBuf::from("/sys/class/block"),
            dev: PathBuf::from("/dev"),
        }
    }
}

/// The serial and WWN a whole block device reports, from its sysfs directory:
/// where `lsblk` reads them, so a disk resolves by the identity the person and
/// the installer see. NVMe and SCSI disks report their serial on the device, a
/// virtio disk on the disk itself.
fn stable_ids(sys: &Path) -> Vec<(String, String)> {
    let read = |path: PathBuf| -> Option<String> {
        let value = std::fs::read_to_string(path).ok()?;
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    };
    let device = sys.join("device");
    [
        ("serial", device.join("serial")),
        ("serial", sys.join("serial")),
        ("wwid", device.join("wwid")),
        ("wwid", sys.join("wwid")),
    ]
    .into_iter()
    .filter_map(|(kind, path)| read(path).map(|value| (kind.to_owned(), value)))
    .collect()
}

/// The whole-disk device nodes that report `target`'s identity.
///
/// # Errors
/// The block devices cannot be listed, or none, or more than one, reports the
/// identity.
pub fn resolve(target: &TargetDisk, roots: &Roots) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(&roots.sys_block).map_err(|error| {
        format!(
            "{}: {error}; a staging container mounts this machine's /sys",
            roots.sys_block.display()
        )
    })?;
    let mut found: Vec<PathBuf> = Vec::new();
    let mut ambiguous: Vec<String> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let sys = entry.path();
        // A partition is never the disk the plan names.
        if sys.join("partition").exists() || !sys.join("dev").exists() {
            continue;
        }
        if stable_ids(&sys)
            .iter()
            .any(|(kind, value)| *kind == target.kind && *value == target.identity)
        {
            let name = entry.file_name();
            let dev = roots.dev.join(&name);
            if !dev.exists() {
                return Err(format!(
                    "{} reports {} but has no device node at {}",
                    name.to_string_lossy(),
                    target,
                    dev.display()
                ));
            }
            ambiguous.push(name.to_string_lossy().into_owned());
            found.push(dev);
        }
    }
    match found.as_slice() {
        [disk] => Ok(disk.clone()),
        [] => Err(format!("no disk reports the identity {target}")),
        _ => Err(format!(
            "more than one disk reports the identity {target}: {}",
            ambiguous.join(", ")
        )),
    }
}

/// The whole disks that back any mount in `mountinfo`, from `backing(name)`'s
/// answer for each mounted device. A disk with a mounted partition is not a
/// staging target: its contents are in use.
///
/// # Errors
/// A mounted device cannot be attributed to its disks.
pub fn mounted_disks(
    mountinfo: &str,
    backing: &mut dyn FnMut(&str) -> Result<Vec<String>, String>,
) -> Result<Vec<String>, String> {
    let mut disks: Vec<String> = Vec::new();
    for entry in crate::linux_parse::mount_entries(mountinfo) {
        let Some((major, minor)) = entry.device.split_once(':') else {
            continue;
        };
        // Pseudo filesystems (cgroup, proc, overlay) have no backing disk.
        if major == "0" {
            continue;
        }
        let name = entry
            .source
            .strip_prefix("/dev/")
            .map_or_else(|| format!("{major}:{minor}"), str::to_owned);
        if name.is_empty() {
            continue;
        }
        for disk in backing(&name)? {
            if !disks.contains(&disk) {
                disks.push(disk);
            }
        }
    }
    Ok(disks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, value: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, value).unwrap();
    }

    fn roots() -> (tempfile::TempDir, Roots) {
        let work = tempfile::tempdir().unwrap();
        let roots = Roots {
            sys_block: work.path().join("sys/block"),
            dev: work.path().join("dev"),
        };
        std::fs::create_dir_all(&roots.sys_block).unwrap();
        std::fs::create_dir_all(&roots.dev).unwrap();
        (work, roots)
    }

    #[test]
    fn a_disk_is_named_by_a_plan_identity_not_a_device_name() {
        assert_eq!(
            TargetDisk::parse("serial:16Q0A07GTM7J").unwrap(),
            TargetDisk {
                kind: "serial".into(),
                identity: "16Q0A07GTM7J".into()
            }
        );
        assert_eq!(
            TargetDisk::parse("wwid:eui.01000000000000008ce38ee30e25a85f")
                .unwrap()
                .to_string(),
            "wwid:eui.01000000000000008ce38ee30e25a85f"
        );
        for bad in [
            "/dev/nvme1n1",
            "/dev/disk/by-id/nvme-eui.01",
            "serial:",
            "largest:any",
            "serial:two words",
            "serial:a/b",
        ] {
            assert!(TargetDisk::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_serial_or_wwid_resolves_to_exactly_one_whole_disk() {
        let (_work, roots) = roots();
        let disk = |name: &str, dev: &str| {
            let sys = roots.sys_block.join(name);
            write(&sys.join("dev"), dev);
            sys
        };
        let serial = disk("nvme0n1", "259:0");
        write(&serial.join("device/serial"), "16Q0A07GTM7J\n");
        let wwid = disk("nvme1n1", "259:1");
        write(&wwid.join("wwid"), "eui.01000000000000008ce38ee30e25a85f\n");
        // A partition carries its own dev and may carry the same serial.
        let partition = disk("nvme1n1p1", "259:2");
        write(&partition.join("partition"), "1\n");
        write(&partition.join("device/serial"), "16Q0A07GTM7J\n");
        for name in ["nvme0n1", "nvme1n1", "nvme1n1p1"] {
            write(&roots.dev.join(name), "");
        }

        assert_eq!(
            resolve(&TargetDisk::parse("serial:16Q0A07GTM7J").unwrap(), &roots).unwrap(),
            roots.dev.join("nvme0n1")
        );
        assert_eq!(
            resolve(
                &TargetDisk::parse("wwid:eui.01000000000000008ce38ee30e25a85f").unwrap(),
                &roots
            )
            .unwrap(),
            roots.dev.join("nvme1n1")
        );
        assert!(
            resolve(&TargetDisk::parse("serial:NOPE").unwrap(), &roots)
                .unwrap_err()
                .contains("no disk reports")
        );
    }

    #[test]
    fn a_virtio_disk_resolves_by_the_serial_it_reports_itself() {
        let (_work, roots) = roots();
        for (name, dev) in [("vda", "252:0"), ("vdb", "252:16")] {
            let sys = roots.sys_block.join(name);
            write(&sys.join("dev"), dev);
            write(&sys.join("serial"), &format!("disk-{name}\n"));
            write(&roots.dev.join(name), "");
        }
        assert_eq!(
            resolve(&TargetDisk::parse("serial:disk-vdb").unwrap(), &roots).unwrap(),
            roots.dev.join("vdb")
        );
    }

    #[test]
    fn two_disks_reporting_one_identity_are_never_guessed() {
        let (_work, roots) = roots();
        for name in ["sda", "sdb"] {
            let sys = roots.sys_block.join(name);
            write(&sys.join("dev"), "8:0");
            write(&sys.join("device/serial"), "SAME");
            write(&roots.dev.join(name), "");
        }
        let error = resolve(&TargetDisk::parse("serial:SAME").unwrap(), &roots).unwrap_err();
        assert!(error.contains("more than one disk"), "{error}");
        assert!(error.contains("sda") && error.contains("sdb"), "{error}");
    }

    #[test]
    fn a_mounted_partition_names_the_disk_that_is_in_use() {
        let mountinfo = "\
22 1 259:0 / / rw - ext4 /dev/nvme0n1p2 rw
23 22 259:1 / /boot rw - vfat /dev/nvme1n1p1 rw
24 22 0:40 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw
25 22 9:3 / /data rw - xfs /dev/sdb1 rw
";
        let mut backing = |name: &str| -> Result<Vec<String>, String> {
            Ok(match name {
                "nvme0n1p2" => vec!["nvme0n1".into()],
                "nvme1n1p1" => vec!["nvme1n1".into()],
                "sdb1" => vec!["sdb".into()],
                other => return Err(format!("unexpected {other}")),
            })
        };
        assert_eq!(
            mounted_disks(mountinfo, &mut backing).unwrap(),
            vec!["nvme0n1", "nvme1n1", "sdb"]
        );
    }
}
