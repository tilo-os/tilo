//! The narrow host I/O surface the staging helper needs, implemented by Linux and Windows and faked
//! in memory for tests. Nothing here decides policy: plan checks live in `plan`, sequencing and
//! recording in `helper`.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// `id` is a platform handle valid only for this boot (Windows disk number, Linux `/dev/...`
/// path); `guid` is durable.
/// this boot (Windows disk number, Linux `/dev/...` path); `guid` is durable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    pub id: String,
    pub guid: String,
    pub size_bytes: u64,
    pub sector_bytes: u64,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub partitions: Vec<Partition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partition {
    pub number: u32,
    pub guid: String,
    pub type_guid: String,
    pub first_lba: u64,
    pub last_lba: u64,
}

impl Partition {
    /// The partition's inclusive block range.
    pub fn extent(&self) -> staging_handoff_model::Extent {
        staging_handoff_model::Extent {
            first_lba: self.first_lba,
            last_lba: self.last_lba,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemVolume {
    pub partition_guid: String,
    pub size_bytes: u64,
    pub supported_min_bytes: u64,
    pub used_bytes: u64,
}

/// Attested shrink facts for one directly mounted GPT Btrfs device on Linux.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxBtrfsVolume {
    pub partition_guid: String,
    pub filesystem_uuid: String,
    pub device_id: u64,
    pub mount_id: u64,
    pub first_lba: u64,
    pub sector_bytes: u64,
    pub size_bytes: u64,
    pub filesystem_bytes: u64,
    pub kernel_partition_bytes: u64,
    pub supported_min_bytes: u64,
    pub reserve_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPartition {
    pub first_lba: u64,
    pub last_lba: u64,
    pub type_guid: String,
    pub name: String,
    pub guid: Option<String>,
}

pub type IoResult<T> = Result<T, String>;

/// Every mutating call must be followed by the caller re-reading state; implementations must not
/// retry silently.
pub trait Platform {
    fn system_disk(&self) -> IoResult<Disk>;
    /// Windows only; `Ok(None)` on Linux.
    fn system_volume(&self) -> IoResult<Option<SystemVolume>>;
    /// Unsupported layouts report no eligible volume; the helper binds these facts into the reviewed
    /// plan.
    fn linux_btrfs_volume(&self) -> IoResult<Option<LinuxBtrfsVolume>> {
        Ok(None)
    }
    fn resize_linux_btrfs_filesystem(
        &mut self,
        _partition_guid: &str,
        _bytes: u64,
    ) -> IoResult<()> {
        Err("Linux Btrfs filesystem resize is unavailable".into())
    }
    fn resize_linux_btrfs_partition(&mut self, _partition_guid: &str, _bytes: u64) -> IoResult<()> {
        Err("Linux Btrfs partition resize is unavailable".into())
    }
    fn shrink_system_volume(&mut self, partition_guid: &str, to_bytes: u64) -> IoResult<()>;
    fn grow_system_volume(&mut self, partition_guid: &str, to_bytes: u64) -> IoResult<()>;
    fn create_partition(&mut self, disk: &Disk, spec: &NewPartition) -> IoResult<String>;
    fn delete_partition(&mut self, disk: &Disk, partition_guid: &str) -> IoResult<()>;
    /// Write `data` at `offset` bytes from the start of the partition. It is durable once
    /// [`Self::sync_disk`] returns, which a step calls before it records itself done.
    fn write_partition(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        data: &[u8],
    ) -> IoResult<()>;
    fn read_partition(
        &self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        buf: &mut [u8],
    ) -> IoResult<()>;
    fn sync_disk(&mut self, disk: &Disk) -> IoResult<()>;
    /// Place a root-only file at `path` inside the filesystem the given staged
    /// partition holds: the single-use ticket a plan's machine spends must
    /// never travel in the ESP handoff the firmware can read.
    ///
    /// Unsupported on a platform that cannot mount the staged filesystem; the
    /// helper then refuses rather than staging a plan without its ticket.
    fn write_staging_file(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> IoResult<()> {
        let _ = (disk, partition_guid, path, contents, mode);
        Err("writing a file into a staging root is unavailable".into())
    }
    /// Read back what [`Self::write_staging_file`] placed, to prove a resumed
    /// staging still holds it.
    fn read_staging_file(
        &self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
    ) -> IoResult<Option<Vec<u8>>> {
        let _ = (disk, partition_guid, path);
        Err("reading a file from a staging root is unavailable".into())
    }
    /// Place the regular file `source` at `path` inside the filesystem the
    /// given staged partition holds, streamed: the prebuilt layers a
    /// development server applies at its first boot are far larger than
    /// memory. Reports the bytes copied.
    ///
    /// Unsupported on a platform that cannot mount the staged filesystem; the
    /// helper then refuses rather than staging a plan without its layers.
    fn write_staging_file_from(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
        source: &Path,
        mode: u32,
    ) -> IoResult<u64> {
        let _ = (disk, partition_guid, path, source, mode);
        Err("streaming a file into a staging root is unavailable".into())
    }
    /// Whether the file `path` inside the staged partition is exactly the
    /// regular file `source`, compared streamed for the same reason.
    fn staging_file_matches(
        &self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
        source: &Path,
    ) -> IoResult<bool> {
        let _ = (disk, partition_guid, path, source);
        Err("comparing a file in a staging root is unavailable".into())
    }
    /// Existing EFI adapters implement this surface; Linux also observes a missing efivarfs when
    /// reviewing geometry before boot eligibility.
    fn has_uefi(&self) -> bool {
        true
    }
    /// `Ok(None)` when it does not exist. Returns the payload without attributes.
    fn get_efi_var(&self, name: &str) -> IoResult<Option<Vec<u8>>>;
    fn set_efi_var(&mut self, name: &str, data: &[u8]) -> IoResult<()>;
    fn delete_efi_var(&mut self, name: &str) -> IoResult<()>;
}
