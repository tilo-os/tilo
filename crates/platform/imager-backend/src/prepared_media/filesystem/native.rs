//! Linux filesystem authority for the reviewed removable medium.
use super::root::PinnedRoot;
use crate::media_model::TargetDrive;
use block_devices::{BlockDevice, Lsblk};
use cap_primitives::fs::MetadataExt;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
#[cfg(any(test, feature = "flatpak-media-writer"))]
pub(super) mod automatic;
#[cfg(any(test, feature = "flatpak-media-writer"))]
pub(super) mod home;
const COLUMNS: &str = "NAME,TYPE,SIZE,RO,FSTYPE,UUID,PARTUUID,PARTTYPE,MAJ:MIN,MOUNTPOINTS";
fn changed(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct PartitionBinding {
    name: String,
    filesystem: String,
    uuid: String,
    partition_uuid: String,
    number: String,
}
/// A filesystem grant belongs to the already reviewed media device, and pins
/// its exact mounted root. Arbitrary host paths and stale partition lists fail.
#[derive(Clone)]
pub struct MediaFilesystemGrant {
    target: TargetDrive,
    root: PathBuf,
    pin: Arc<PinnedRoot>,
    partition: PartitionBinding,
}
impl MediaFilesystemGrant {
    /// The binding is only an expectation: verify it against the held writer
    /// descriptor and this actual filesystem grant before delivering anything.
    ///
    /// # Errors
    /// Returns an error if grant revalidation, descriptor attestation or partition
    /// table reading fails, or the stable device or partition binding differs
    /// from the expectation.
    pub fn validate_offline_media(
        &self,
        raw: &std::fs::File,
        expected: &crate::media_layout::MediaPartitionBinding,
    ) -> io::Result<()> {
        self.validate()?;
        crate::write::attest_target_descriptor(&self.target, raw)?;
        if crate::media_layout::binding(raw, self.target.size, &expected.target_stable_id)?
            != *expected
            || self.target.stable_id.as_deref() != Some(expected.target_stable_id.as_str())
            || !self
                .partition
                .partition_uuid
                .eq_ignore_ascii_case(&expected.partition_guid)
        {
            return Err(changed(
                "Offline export medium differs from authenticated selection",
            ));
        }
        self.validate()
    }
    pub fn root_path(&self) -> &Path {
        &self.root
    }
    /// Check an adapter's descriptor against the held canonical directory.
    /// This checks namespace identity; callers must also validate device custody.
    ///
    /// # Errors
    /// Returns an error if the pinned destination changed, descriptor metadata
    /// cannot be read, or the descriptor identifies a different directory.
    pub fn check_directory_identity(&self, file: &std::fs::File) -> io::Result<()> {
        self.pin.validate_destination()?;
        if !crate::nofollow::same_directory_identity(
            &cap_primitives::fs::Metadata::from_file(file)?,
            &self.pin.identity,
        ) {
            return Err(changed(
                "Media directory descriptor differs from held grant",
            ));
        }
        Ok(())
    }
    /// Resolve a pinned root against a supplied block-device fixture inventory.
    ///
    /// # Errors
    /// Returns an error if the root cannot be safely opened or validated, its
    /// filesystem is read-only, or the inventory lacks a unique supported
    /// writable partition with filesystem and partition identities at that root.
    #[cfg(any(test, feature = "test-support"))]
    pub fn resolve_fixture(
        target: &TargetDrive,
        mounted_root: &Path,
        devices: &[BlockDevice],
    ) -> io::Result<Self> {
        let pin = PinnedRoot::open_absolute(mounted_root)?;
        pin.validate_destination()?;
        writable(&pin)?;
        let partition = select_partition(devices, target, mounted_root, pin.identity.dev())?;
        Ok(Self {
            target: target.clone(),
            root: mounted_root.to_owned(),
            pin,
            partition,
        })
    }
    /// Does not mount, format, repartition, or start writing. The existing writer
    /// must first obtain a writable mounted filesystem on its selected medium.
    ///
    /// # Errors
    /// Returns an error if the target is ineligible or changed, the root cannot
    /// be safely opened or validated, the filesystem is read-only, inventory
    /// lookup fails, or no unique supported writable partition with filesystem
    /// and partition identities matches the root on the selected medium.
    pub fn resolve(target: &TargetDrive, mounted_root: &Path) -> io::Result<Self> {
        if !target.is_media_target() {
            return Err(changed(
                "Prepared setup requires the reviewed external medium",
            ));
        }
        crate::write::revalidate_target(target)?;
        let pin = PinnedRoot::open_absolute(mounted_root)?;
        pin.validate_destination()?;
        writable(&pin)?;
        let partition = select_partition(&inventory()?, target, mounted_root, pin.identity.dev())?;
        Ok(Self {
            target: target.clone(),
            root: mounted_root.to_owned(),
            pin,
            partition,
        })
    }
    /// Recheck the device, pinned directory and mounted partition binding.
    ///
    /// # Errors
    /// Returns an error if the device or directory changed, the filesystem is
    /// read-only, inventory lookup fails, or the current partition binding no
    /// longer matches the grant.
    pub fn validate(&self) -> io::Result<()> {
        crate::write::revalidate_target(&self.target)?;
        self.pin.validate_destination()?;
        writable(&self.pin)?;
        if select_partition(
            &inventory()?,
            &self.target,
            &self.root,
            self.pin.identity.dev(),
        )? != self.partition
        {
            return Err(changed("Prepared setup filesystem/device binding changed"));
        }
        Ok(())
    }
}
fn inventory() -> io::Result<Vec<BlockDevice>> {
    Lsblk::new(COLUMNS)
        .full_paths(true)
        .list()
        .ok_or_else(|| changed("Cannot identify the selected media filesystem"))
}
fn select_partition(
    devices: &[BlockDevice],
    target: &TargetDrive,
    root: &Path,
    device: u64,
) -> io::Result<PartitionBinding> {
    let number = format!("{}:{}", libc::major(device), libc::minor(device));
    let mut matches = Vec::new();
    for disk in devices.iter().filter(|disk| {
        disk.name == target.dev_path()
            && disk.kind == "disk"
            && disk.size == target.size
            && !disk.read_only
    }) {
        for partition in &disk.children {
            if partition.kind != "part"
                || partition.read_only
                || !partition.children.is_empty()
                || partition.maj_min.as_deref() != Some(number.as_str())
                || !partition
                    .all_mountpoints()
                    .iter()
                    .any(|point| Path::new(point) == root)
            {
                continue;
            }
            let filesystem = partition
                .fstype
                .as_deref()
                .filter(|kind| matches!(*kind, "vfat" | "exfat" | "ext4" | "xfs" | "btrfs"))
                .ok_or_else(|| {
                    changed("Media has no supported writable customization filesystem")
                })?;
            let uuid = partition
                .uuid
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| changed("Media filesystem identity is missing"))?;
            let partition_uuid = partition
                .partuuid
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| changed("Media partition identity is missing"))?;
            matches.push(PartitionBinding {
                name: partition.name.clone(),
                filesystem: filesystem.into(),
                uuid: uuid.into(),
                partition_uuid: partition_uuid.into(),
                number: number.clone(),
            });
        }
    }
    match matches.as_slice() {
        [only] => Ok(only.clone()),
        _ => Err(changed(
            "Prepared setup needs an explicit mounted writable filesystem on this reviewed medium; no raw-tail fallback is permitted",
        )),
    }
}
fn writable(pin: &PinnedRoot) -> io::Result<()> {
    check_writable_directory(&pin.file)
}
/// Check that the descriptor's filesystem is not mounted read-only.
///
/// # Errors
/// Returns an error if filesystem status cannot be read or the filesystem is
/// mounted read-only. This does not check directory access permissions.
pub fn check_writable_directory(file: &std::fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: valid held descriptor and writable statvfs storage.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful syscall initialized status.
    if unsafe { status.assume_init() }.f_flag & libc::ST_RDONLY != 0 {
        return Err(changed("Granted media filesystem is read-only"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
