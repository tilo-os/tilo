//! The narrow host I/O surface the staging helper needs. Linux and Windows each
//! implement [`Platform`]; tests use an in-memory fake. Nothing here decides
//! policy: plan checks live in `plan`, sequencing and recording in `helper`.

use serde::{Deserialize, Serialize};

/// A GPT disk as the host reports it. `id` is a platform handle valid only for
/// this boot (Windows disk number, Linux `/dev/...` path); `guid` is durable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    pub id: String,
    pub guid: String,
    pub size_bytes: u64,
    pub sector_bytes: u64,
    /// First and last LBA usable for partitions (GPT header bounds).
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub partitions: Vec<Partition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partition {
    /// 1-based GPT entry number (needed for the EFI HD device path).
    pub number: u32,
    pub guid: String,
    pub type_guid: String,
    pub first_lba: u64,
    pub last_lba: u64,
}

/// The system volume Windows booted from, with its shrink bounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemVolume {
    pub partition_guid: String,
    pub size_bytes: u64,
    /// `Get-PartitionSupportedSize` minimum.
    pub supported_min_bytes: u64,
    pub used_bytes: u64,
}

/// Attested shrink facts for one directly mounted GPT Btrfs device on Linux.
/// Multi-device, encrypted, mapped and otherwise indirect layouts are refused
/// by the canonical probe rather than represented as an eligible volume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxBtrfsVolume {
    pub partition_guid: String,
    pub filesystem_uuid: String,
    /// Btrfs device ID, not a pathname or a Linux major/minor number.
    pub device_id: u64,
    /// Actual retained mount ID in the helper's mount namespace.
    pub mount_id: u64,
    pub first_lba: u64,
    pub sector_bytes: u64,
    /// Original partition and filesystem size, reattested before resizing.
    pub size_bytes: u64,
    pub filesystem_bytes: u64,
    pub kernel_partition_bytes: u64,
    /// Actual supported Btrfs shrink minimum; never derived from free bytes.
    pub supported_min_bytes: u64,
    /// Extra retained allocation headroom above the supported minimum.
    pub reserve_bytes: u64,
}

/// A partition to create at an exact place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPartition {
    pub first_lba: u64,
    pub last_lba: u64,
    pub type_guid: String,
    pub name: String,
    /// Pre-generated where the platform can set it (Linux); `None` on Windows.
    pub guid: Option<String>,
}

pub type IoResult<T> = Result<T, String>;

/// Host disk + firmware operations. Every mutating call must be followed by
/// the caller re-reading state; implementations must not retry silently.
pub trait Platform {
    /// The disk holding the running OS's boot volume, with its partition map.
    /// # Errors
    /// Returns an error if the disk cannot be probed.
    fn system_disk(&self) -> IoResult<Disk>;
    /// Windows only; `Ok(None)` on Linux.
    /// # Errors
    /// Returns an error if the system volume cannot be probed.
    fn system_volume(&self) -> IoResult<Option<SystemVolume>>;
    /// Linux direct Btrfs shrink facts; unsupported layouts report no eligible
    /// volume. The helper binds these facts into the reviewed plan.
    /// # Errors
    /// Returns an error if retained filesystem facts cannot be attested.
    fn linux_btrfs_volume(&self) -> IoResult<Option<LinuxBtrfsVolume>> {
        Ok(None)
    }
    /// Resize only the retained, reattested eligible Btrfs filesystem.
    /// # Errors
    /// Returns an error if the filesystem is unsupported, its identity changed or resize fails.
    fn resize_linux_btrfs_filesystem(
        &mut self,
        _partition_guid: &str,
        _bytes: u64,
    ) -> IoResult<()> {
        Err("Linux Btrfs filesystem resize is unavailable".into())
    }
    /// Resize GPT and its kernel partition view, with exact readback.
    /// # Errors
    /// Returns an error if the geometry is invalid, identity changed or resize readback fails.
    fn resize_linux_btrfs_partition(&mut self, _partition_guid: &str, _bytes: u64) -> IoResult<()> {
        Err("Linux Btrfs partition resize is unavailable".into())
    }
    /// Shrink the system volume to exactly `to_bytes` (Windows only).
    /// # Errors
    /// Returns an error if the shrink is unsupported, unsafe or fails.
    fn shrink_system_volume(&mut self, partition_guid: &str, to_bytes: u64) -> IoResult<()>;
    /// Grow the system volume back to exactly `to_bytes` (Windows only).
    /// # Errors
    /// Returns an error if the growth is unsupported or fails.
    fn grow_system_volume(&mut self, partition_guid: &str, to_bytes: u64) -> IoResult<()>;
    /// Create one partition; returns its GUID as read back.
    /// # Errors
    /// Returns an error if the geometry is invalid or partition creation fails.
    fn create_partition(&mut self, disk: &Disk, spec: &NewPartition) -> IoResult<String>;
    /// # Errors
    /// Returns an error if the partition is in use, missing or cannot be deleted.
    fn delete_partition(&mut self, disk: &Disk, partition_guid: &str) -> IoResult<()>;
    /// Write `data` at `offset` bytes from the start of the partition.
    /// `offset` and `data.len()` are multiples of `disk.sector_bytes`.
    /// # Errors
    /// Returns an error if the partition identity, alignment or write fails.
    fn write_partition(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        data: &[u8],
    ) -> IoResult<()>;
    /// # Errors
    /// Returns an error if the partition identity, alignment or read fails.
    fn read_partition(
        &self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        buf: &mut [u8],
    ) -> IoResult<()>;
    /// Flush every write to stable storage.
    /// # Errors
    /// Returns an error if the disk cannot be flushed.
    fn sync_disk(&mut self, disk: &Disk) -> IoResult<()>;
    /// Whether this adapter has access to the running host's UEFI firmware.
    /// Existing EFI adapters implement this surface; Linux also observes the
    /// absence of efivarfs when reviewing geometry before boot eligibility.
    fn has_uefi(&self) -> bool {
        true
    }
    /// Read an EFI global variable (`8be4df61-93ca-11d2-aa0d-00e098032b8c`).
    /// `Ok(None)` when it does not exist. Returns the payload without attributes.
    /// # Errors
    /// Returns an error if the variable cannot be read or decoded.
    fn get_efi_var(&self, name: &str) -> IoResult<Option<Vec<u8>>>;
    /// Create or replace a non-volatile, boot + runtime EFI global variable.
    /// # Errors
    /// Returns an error if the name is invalid or the firmware write fails.
    fn set_efi_var(&mut self, name: &str, data: &[u8]) -> IoResult<()>;
    /// # Errors
    /// Returns an error if the name is invalid or the firmware deletion fails.
    fn delete_efi_var(&mut self, name: &str) -> IoResult<()>;
}
