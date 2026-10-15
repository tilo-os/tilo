//! Native capacity checks on the selected directory, not an inferred mount root.
use crate::setup::migration::PinnedDirectory;
use std::io;

#[cfg(unix)]
pub(super) fn capacity(directory: &PinnedDirectory, needed: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: live pinned descriptor and writable native output storage.
    if unsafe { libc::fstatvfs(directory.file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initializes the structure.
    let status = unsafe { status.assume_init() };
    if u128::from(status.f_flag) & u128::from(libc::ST_RDONLY) != 0 {
        return Err(io::Error::other("Selected recovery directory is read-only"));
    }
    let available = u128::from(status.f_bavail) * u128::from(status.f_frsize);
    // Leave room for directory entries and durable journal overhead. Never use
    // privileged reserved blocks as space available to the person copying.
    let required = needed
        .checked_add(16 << 20)
        .ok_or_else(|| io::Error::other("Recovery capacity overflow"))?;
    if available < u128::from(required) {
        return Err(io::Error::other(
            "Selected recovery directory has insufficient free space",
        ));
    }
    directory.validate_destination()?;
    crate::setup::migration_durability::readable_directory(&directory.file)?.sync_all()
}
#[cfg(not(unix))]
pub(super) fn capacity(_: &PinnedDirectory, _: u64) -> io::Result<()> {
    Err(io::Error::other(
        "Native recovery capacity and durability validation is unavailable",
    ))
}

pub(super) struct Hardware {
    topology: std::path::PathBuf,
    stable: Option<(&'static str, String)>,
}
impl Hardware {
    pub(super) fn independent_of(&self, other: &Self) -> bool {
        self.topology != other.topology
            && self.stable.as_ref().zip(other.stable.as_ref()).is_some_and(
                |((kind, id), (other_kind, other_id))| kind == other_kind && id != other_id,
            )
    }
    #[cfg(target_os = "linux")]
    pub(super) fn same_device(&self, other: &Self) -> bool {
        self.topology == other.topology
            || self
                .stable
                .as_ref()
                .zip(other.stable.as_ref())
                .is_some_and(|(id, other_id)| id == other_id)
    }
}

/// Only plain, observable Linux block devices count as physical independence.
/// Distinct partitions, bind mounts and directory names are not distinct disks.
#[cfg(target_os = "linux")]
pub(super) fn physical(path: &std::path::Path) -> Option<Hardware> {
    use std::os::unix::fs::MetadataExt;
    let dev = std::fs::metadata(path).ok()?.dev();
    let mut sys = std::fs::canonicalize(format!(
        "/sys/dev/block/{}:{}",
        libc::major(dev),
        libc::minor(dev)
    ))
    .ok()?;
    if sys.join("partition").exists() {
        sys = sys.parent()?.to_path_buf();
    }
    // Device-mapper, RAID, loop and virtual/unknown mappings must not be
    // mistaken for independent hardware; those require a native extent mapper.
    if !sys.join("device").exists() || std::fs::read_dir(sys.join("slaves")).ok()?.next().is_some()
    {
        return None;
    }
    hardware(&sys)
}

#[cfg(target_os = "linux")]
pub(super) fn hardware(sys: &std::path::Path) -> Option<Hardware> {
    // Namespace/LUN names can differ while the hardware is shared. Collapse
    // NVMe namespaces to their controller and SCSI LUNs to their target.
    let device = std::fs::canonicalize(sys.join("device")).ok()?;
    let stable = [
        ("serial", device.join("serial")),
        ("wwid", device.join("wwid")),
        ("wwid", sys.join("wwid")),
    ]
    .into_iter()
    .find_map(|(kind, path)| {
        let value = std::fs::read_to_string(path).ok()?;
        let value = value.trim();
        (!value.is_empty()).then(|| (kind, value.to_string()))
    });
    let topology = if device
        .parent()?
        .file_name()?
        .to_str()?
        .starts_with("target")
    {
        device.parent()?.to_path_buf()
    } else {
        device
    };
    Some(Hardware { topology, stable })
}
#[cfg(not(target_os = "linux"))]
pub(super) fn physical(_: &std::path::Path) -> Option<Hardware> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespaces_multipath_and_unknown_ids_are_not_independent_disks() {
        let device = |path: &str, id: Option<&str>| Hardware {
            topology: path.into(),
            stable: id.map(|id| ("wwid", id.into())),
        };
        assert!(
            !device("controller-a", Some("namespace-one"))
                .independent_of(&device("controller-a", Some("namespace-two")))
        );
        assert!(
            !device("path-a", Some("disk-one")).independent_of(&device("path-b", Some("disk-one")))
        );
        assert!(!device("path-a", None).independent_of(&device("path-b", None)));
        assert!(
            device("controller-a", Some("disk-one"))
                .independent_of(&device("controller-b", Some("disk-two")))
        );
    }
}
