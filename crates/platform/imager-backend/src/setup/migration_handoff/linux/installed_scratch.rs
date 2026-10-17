//! Native installed scratch measurement; no source, output or completion grant.
//! The caller supplies only the admitted Store directory and actual read lease.
use super::*;
use crate::setup::home_capture::OriginalCopyReadAdmission;
use std::{
    fs::File,
    os::{
        fd::{AsFd, AsRawFd},
        unix::fs::{FileTypeExt, OpenOptionsExt},
    },
    sync::atomic::AtomicBool,
};
use tilo_files::native::atomic::{Privacy, StorageDirectory};

#[derive(Clone, PartialEq, Eq)]
pub struct InstalledScratchFacts {
    pub directory: (u64, u64),
    pub mount_id: u64,
    pub namespace: (u64, u64),
    pub partition_rdev: u64,
    pub disk_rdev: u64,
    pub volume: SourceVolume,
}
struct Blocks {
    partition: File,
    disk: File,
    partition_name: String,
    disk_name: String,
}
pub struct InstalledScratchIdentity {
    directory: File,
    namespace: File,
    blocks: Blocks,
    facts: InstalledScratchFacts,
    source: Arc<OriginalCopyReadAdmission>,
}
fn root() -> io::Result<()> {
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
fn stamp(file: &File) -> io::Result<(u64, u64)> {
    let m = file.metadata()?;
    Ok((m.dev(), m.ino()))
}
fn number(rdev: u64) -> String {
    format!("{}:{}", libc::major(rdev), libc::minor(rdev))
}
fn open_block(name: &str, expected: &str) -> io::Result<File> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(name)?;
    let m = file.metadata()?;
    let named = fs::symlink_metadata(name)?;
    if !m.file_type().is_block_device()
        || !named.file_type().is_block_device()
        || (m.dev(), m.ino(), m.rdev()) != (named.dev(), named.ino(), named.rdev())
        || number(m.rdev()) != expected
    {
        return Err(refusal("Scratch block name/kernel identity differs"));
    }
    Ok(file)
}
fn block_geometry(file: &File) -> io::Result<(u64, u64)> {
    let mut bytes = 0u64;
    let mut sector = 0u32;
    // SAFETY: actual held block descriptor and correctly sized writable outputs.
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x8008_1272, &raw mut bytes) } != 0
        || unsafe { libc::ioctl(file.as_raw_fd(), 0x1268, &raw mut sector) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((bytes, u64::from(sector)))
}
fn extent(volume: &SourceVolume) -> io::Result<(u64, u64)> {
    if volume.first_lba > volume.last_lba || !matches!(volume.sector_bytes, 512 | 4096) {
        return Err(refusal("Scratch/source partition extent is invalid"));
    }
    let start = volume
        .first_lba
        .checked_mul(volume.sector_bytes)
        .ok_or_else(|| refusal("Partition start overflows"))?;
    let end = volume
        .last_lba
        .checked_add(1)
        .and_then(|n| n.checked_mul(volume.sector_bytes))
        .ok_or_else(|| refusal("Partition end overflows"))?;
    if end > volume.disk_bytes {
        return Err(refusal("Partition exceeds held disk"));
    }
    Ok((start, end))
}
fn overlaps(left: (u64, u64), right: (u64, u64)) -> bool {
    left.0 < right.1 && right.0 < left.1
}
fn blocks(facts: &Facts, volume: &SourceVolume) -> io::Result<Blocks> {
    let selected = facts.matching_volume(volume)?;
    let matches: Vec<_> = facts
        .devices
        .iter()
        .flat_map(|disk| {
            disk.children
                .iter()
                .filter(|part| part.name == selected)
                .map(move |part| (disk, part))
        })
        .collect();
    let [(disk, part)] = matches.as_slice() else {
        return Err(refusal("Scratch/source parent disk is ambiguous"));
    };
    let partition = open_block(
        &part.name,
        part.maj_min
            .as_deref()
            .ok_or_else(|| refusal("Partition kernel number missing"))?,
    )?;
    let held_disk = open_block(
        &disk.name,
        disk.maj_min
            .as_deref()
            .ok_or_else(|| refusal("Disk kernel number missing"))?,
    )?;
    let bounds = extent(volume)?;
    if block_geometry(&held_disk)? != (volume.disk_bytes, volume.sector_bytes)
        || block_geometry(&partition)? != (bounds.1 - bounds.0, volume.sector_bytes)
    {
        return Err(refusal("Scratch/source held block geometry differs"));
    }
    Ok(Blocks {
        partition,
        disk: held_disk,
        partition_name: part.name.clone(),
        disk_name: disk.name.clone(),
    })
}
fn mounted_uuid(directory: &File) -> io::Result<String> {
    // Linux UAPI fs.h: struct fsuuid2 { __u8 len; __u8 uuid[16]; },
    // FS_IOC_GETFSUUID = _IOR(0x15, 0, struct fsuuid2). No path/backing inference.
    #[repr(C)]
    struct FsUuid {
        len: u8,
        uuid: [u8; 16],
    }
    let mut value = FsUuid {
        len: 0,
        uuid: [0; 16],
    };
    // SAFETY: live held directory and the exact fixed-size kernel output ABI.
    if unsafe { libc::ioctl(directory.as_raw_fd(), 0x8011_1500, &raw mut value) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if value.len != 16 || value.uuid == [0; 16] {
        return Err(refusal("Scratch mounted filesystem UUID is unavailable"));
    }
    Ok(uuid::Uuid::from_bytes(value.uuid).to_string())
}
fn target_volume(facts: &Facts, directory: &File, mount_id: u64) -> io::Result<SourceVolume> {
    let matches: Vec<_> = facts
        .mounts
        .iter()
        .filter(|mount| mount.id == mount_id)
        .collect();
    let [mount] = matches.as_slice() else {
        return Err(refusal("Scratch mount identity is missing or ambiguous"));
    };
    let mut kernel = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(directory.as_raw_fd(), kernel.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatfs initialized the output; kernel magic is 32 bits.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Linux filesystem magic is a 32-bit value regardless of signed f_type width"
    )]
    let magic = unsafe { kernel.assume_init() }.f_type as u32;
    let filesystem = match (mount.filesystem.as_str(), magic) {
        ("ext4", 0xef53) => Filesystem::Ext4,
        ("xfs", 0x5846_5342) => Filesystem::Xfs,
        ("btrfs", 0x9123_683e) => Filesystem::Btrfs,
        _ => {
            return Err(unsupported_source(
                "Scratch requires a genuine directly backed ext4/XFS/Btrfs partition",
            ));
        }
    };
    let actual_uuid = mounted_uuid(directory)?;
    let physical = if filesystem == Filesystem::Btrfs {
        let device = fs_native::btrfs::single_device_identity(directory.as_fd())?;
        Some((
            device.rdev,
            uuid::Uuid::from_bytes(device.filesystem).to_string(),
        ))
    } else {
        None
    };
    let rdev = physical
        .as_ref()
        .map_or(directory.metadata()?.dev(), |value| value.0);
    if filesystem != Filesystem::Btrfs && mount.number != number(rdev) {
        return Err(refusal("Scratch held mount/device number differs"));
    }
    let mut volumes = Vec::new();
    for disk in &facts.devices {
        for part in &disk.children {
            if part.maj_min.as_deref() == Some(number(rdev).as_str()) {
                let volume = volume(disk, part)?;
                if volume.filesystem != filesystem
                    || volume.filesystem_uuid != actual_uuid
                    || physical
                        .as_ref()
                        .is_some_and(|(_, fsid)| volume.filesystem_uuid != *fsid)
                {
                    return Err(refusal("Scratch native filesystem/member identity differs"));
                }
                volumes.push(volume);
            }
        }
    }
    let [volume] = volumes.as_slice() else {
        return Err(refusal(
            "Scratch physical partition is missing or ambiguous",
        ));
    };
    for id in [
        &volume.disk_guid,
        &volume.partition_guid,
        &volume.filesystem_uuid,
    ] {
        if !uuid::Uuid::parse_str(id).is_ok_and(|id| !id.is_nil()) {
            return Err(refusal("Scratch native storage UUID is invalid"));
        }
    }
    facts.matching_volume(volume)?;
    Ok(volume.clone())
}
impl InstalledScratchIdentity {
    pub fn review(
        directory: File,
        source: Arc<OriginalCopyReadAdmission>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        root()?;
        source.revalidate(cancel)?;
        StorageDirectory::from_file(directory.try_clone()?)?
            .validate_privacy(Privacy::OwnerOnly)?;
        let namespace = File::open("/proc/self/ns/mnt")?;
        let mount_id = fs_native::snapshot_lease::mount_id(&directory)?;
        if mount_id == 0 {
            return Err(refusal("Scratch mount ID is unavailable"));
        }
        let native = Facts::read()?;
        let volume = target_volume(&native, &directory, mount_id)?;
        let blocks = blocks(&native, &volume)?;
        let facts = InstalledScratchFacts {
            directory: stamp(&directory)?,
            mount_id,
            namespace: stamp(&namespace)?,
            partition_rdev: blocks.partition.metadata()?.rdev(),
            disk_rdev: blocks.disk.metadata()?.rdev(),
            volume,
        };
        let result = Self {
            directory,
            namespace,
            blocks,
            facts,
            source,
        };
        result.revalidate(cancel)?;
        Ok(result)
    }
    pub fn revalidate(&self, cancel: &AtomicBool) -> io::Result<()> {
        root()?;
        let plans = self.source.output_source_plans(cancel)?;
        if plans.is_empty()
            || plans.len() > 128
            || plans
                .iter()
                .try_fold(0usize, |n, p| n.checked_add(p.len()))
                .is_none_or(|n| n > 9 * 1024 * 1024)
        {
            return Err(refusal("Scratch protected source geometry exceeds bounds"));
        }
        StorageDirectory::from_file(self.directory.try_clone()?)?
            .validate_privacy(Privacy::OwnerOnly)?;
        if stamp(&self.directory)? != self.facts.directory
            || stamp(&self.namespace)? != self.facts.namespace
            || stamp(&File::open("/proc/self/ns/mnt")?)? != self.facts.namespace
            || fs_native::snapshot_lease::mount_id(&self.directory)? != self.facts.mount_id
        {
            return Err(refusal("Scratch directory/mount namespace changed"));
        }
        let native = Facts::read()?;
        if target_volume(&native, &self.directory, self.facts.mount_id)? != self.facts.volume {
            return Err(refusal("Scratch filesystem geometry changed"));
        }
        let current = blocks(&native, &self.facts.volume)?;
        if current.partition_name != self.blocks.partition_name
            || current.disk_name != self.blocks.disk_name
            || stamp(&current.partition)? != stamp(&self.blocks.partition)?
            || stamp(&current.disk)? != stamp(&self.blocks.disk)?
            || current.partition.metadata()?.rdev() != self.facts.partition_rdev
            || current.disk.metadata()?.rdev() != self.facts.disk_rdev
        {
            return Err(refusal("Scratch held block custody changed"));
        }
        let target_extent = extent(&self.facts.volume)?;
        for bytes in plans {
            let geometry = SourceGeometry::parse(&bytes)?;
            let volume = geometry.volume();
            let original = blocks(&native, volume)?;
            if original.partition.metadata()?.rdev() == self.facts.partition_rdev
                || volume.filesystem_uuid == self.facts.volume.filesystem_uuid
                || (original.disk.metadata()?.rdev() == self.facts.disk_rdev
                    && overlaps(extent(volume)?, target_extent))
            {
                return Err(refusal(
                    "Scratch overlaps a protected original source partition/filesystem",
                ));
            }
        }
        self.source.revalidate(cancel)?;
        if stamp(&File::open("/proc/self/ns/mnt")?)? != self.facts.namespace
            || fs_native::snapshot_lease::mount_id(&self.directory)? != self.facts.mount_id
        {
            return Err(refusal(
                "Scratch mount/namespace changed during measurement",
            ));
        }
        if mounted_uuid(&self.directory)? != self.facts.volume.filesystem_uuid {
            return Err(refusal(
                "Scratch native filesystem UUID changed during measurement",
            ));
        }
        let final_blocks = blocks(&native, &self.facts.volume)?;
        if stamp(&final_blocks.partition)? != stamp(&self.blocks.partition)?
            || stamp(&final_blocks.disk)? != stamp(&self.blocks.disk)?
        {
            return Err(refusal("Scratch named block changed during measurement"));
        }
        Ok(())
    }
    pub fn facts(&self, cancel: &AtomicBool) -> io::Result<InstalledScratchFacts> {
        self.revalidate(cancel)?;
        Ok(self.facts.clone())
    }
    pub fn validate_child(&self, child: &File, cancel: &AtomicBool) -> io::Result<()> {
        self.revalidate(cancel)?;
        if !child.metadata()?.is_dir()
            || child.metadata()?.dev() != self.directory.metadata()?.dev()
            || fs_native::snapshot_lease::mount_id(child)? != self.facts.mount_id
        {
            return Err(refusal(
                "Scratch child escaped its admitted filesystem mount",
            ));
        }
        self.revalidate(cancel)
    }
}
#[cfg(test)]
mod tests {
    use super::{Filesystem, SourceVolume, extent, overlaps};
    #[test]
    fn partition_byte_extent_refuses_overflow_and_outside_disk() {
        let mut volume = SourceVolume {
            disk_guid: String::new(),
            disk_bytes: 16384,
            sector_bytes: 4096,
            partition_guid: String::new(),
            first_lba: 1,
            last_lba: 2,
            filesystem_uuid: String::new(),
            filesystem: Filesystem::Ext4,
        };
        assert_eq!(extent(&volume).unwrap(), (4096, 12288));
        volume.last_lba = 4;
        assert!(extent(&volume).is_err());
        volume.first_lba = u64::MAX;
        volume.last_lba = u64::MAX;
        volume.disk_bytes = u64::MAX;
        assert!(extent(&volume).is_err());
    }
    #[test]
    fn partition_collision_uses_half_open_byte_ranges_not_parent_disk_equality() {
        assert!(!overlaps((4096, 8192), (8192, 12288)));
        assert!(overlaps((4096, 8192), (8191, 12288)));
        assert!(overlaps((4096, 8192), (0, 16384)));
        assert!(!overlaps((4096, 8192), (0, 4096)));
    }
}
