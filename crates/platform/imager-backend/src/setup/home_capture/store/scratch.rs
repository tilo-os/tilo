//! Per-command snapshot inventory storage, subordinate to the protected Store.
//! Matching the admitted Btrfs UUID does not exclude a RAM-backed loop device.
mod backing;
mod recovery;
#[cfg(test)]
mod tests;
use super::*;
pub use backing::BackingWitness;
use std::{
    ffi::CString,
    os::{
        fd::{AsFd, AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(in crate::setup::home_capture) struct ScratchReceipt {
    parent: PathBuf,
    parent_device: u64,
    parent_inode: u64,
    name: String,
    device: u64,
    inode: u64,
    caller: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScratchReceiptWire {
    parent: PathBuf,
    parent_device: u64,
    parent_inode: u64,
    name: String,
    device: u64,
    inode: u64,
    caller: u32,
}
impl TryFrom<ScratchReceiptWire> for ScratchReceipt {
    type Error = io::Error;

    fn try_from(wire: ScratchReceiptWire) -> io::Result<Self> {
        let suffix = wire
            .name
            .strip_prefix("inventory-")
            .ok_or(io::ErrorKind::InvalidData)?;
        if uuid::Uuid::parse_str(suffix)
            .ok()
            .map(|id| id.to_string())
            .as_deref()
            != Some(suffix)
            || !wire.parent.is_absolute()
            || wire.parent.components().any(|component| {
                !matches!(
                    component,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(Self {
            parent: wire.parent,
            parent_device: wire.parent_device,
            parent_inode: wire.parent_inode,
            name: wire.name,
            device: wire.device,
            inode: wire.inode,
            caller: wire.caller,
        })
    }
}
impl<'de> Deserialize<'de> for ScratchReceipt {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        ScratchReceiptWire::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}
impl ScratchReceipt {
    pub(in crate::setup::home_capture) fn cleanup(&self) -> io::Result<()> {
        let suffix = self
            .name
            .strip_prefix("inventory-")
            .ok_or(io::ErrorKind::PermissionDenied)?;
        if uuid::Uuid::parse_str(suffix)
            .ok()
            .map(|id| id.to_string())
            .as_deref()
            != Some(suffix)
            || rustix::process::geteuid().as_raw() != 0
            || !self.parent.is_absolute()
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let parent = PinnedDirectory::open_absolute(&self.parent)?;
        storage(&parent)?.validate_privacy(Privacy::OwnerOnly)?;
        let facts = parent.file.metadata()?;
        if facts.uid() != 0 || (facts.dev(), facts.ino()) != (self.parent_device, self.parent_inode)
        {
            return Err(failure("Inventory scratch parent changed"));
        }
        let actual =
            match cap_primitives::fs::open_dir_nofollow(&parent.file, Path::new(&self.name)) {
                Ok(value) => value.metadata()?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
        if !actual.is_dir()
            || actual.mode() & 0o7777 != 0o700
            || actual.uid() != self.caller
            || (actual.dev(), actual.ino()) != (self.device, self.inode)
        {
            return Err(failure("Inventory scratch child changed"));
        }
        let name = CString::new(self.name.as_bytes()).map_err(io::Error::other)?;
        // Exact empty-directory removal only. Unexpected retained data refuses
        // retirement rather than recursively traversing worker-controlled names.
        if unsafe { libc::unlinkat(parent.file.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        migration_durability::sync_directory(&parent.file)
    }
}
pub(in crate::setup::home_capture) struct ScratchLease {
    pub(in crate::setup::home_capture) directory: File,
    pub(in crate::setup::home_capture) quota_bytes: u64,
    pub(in crate::setup::home_capture) receipt: ScratchReceipt,
    _allocation_lease: Option<FileLease>,
    _backing: BackingWitness,
}
impl ScratchLease {
    pub(in crate::setup::home_capture) fn validate_backing(&self) -> io::Result<()> {
        self._backing.validate()
    }
    pub(in crate::setup::home_capture) fn retain(&mut self) {
        if let Some(lease) = self._allocation_lease.take() {
            std::mem::forget(lease);
        }
    }
}
impl Store {
    pub(in crate::setup::home_capture) fn inventory_scratch(
        &self,
        record: &Record,
    ) -> io::Result<ScratchLease> {
        if rustix::process::geteuid().as_raw() != 0 || record.grant.operation != self.operation {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let allocation_lease = self.inventory_allocation_lease()?;
        // File locks do not survive broker death; sibling protected receipts do.
        self.require_inventory_workers_stopped()?;
        let reserve = record
            .capture
            .as_ref()
            .ok_or(io::ErrorKind::InvalidData)?
            .reserve_bytes;
        let parent = migration_durability::readable_directory(&self.records.file)?;
        self.records.validate_destination()?;
        storage(&self.records)?.validate_privacy(Privacy::OwnerOnly)?;
        if btrfs::subvolume_identity(parent.as_fd())?.filesystem != record.grant.filesystem {
            return Err(failure("Inventory scratch Store filesystem differs"));
        }
        let mut capacity: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(parent.as_raw_fd(), &raw mut capacity) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let quota_bytes = capacity
            .f_bavail
            .checked_mul(capacity.f_frsize)
            .and_then(|bytes| bytes.checked_sub(reserve))
            .filter(|bytes| *bytes > 0)
            .ok_or(io::ErrorKind::StorageFull)?;
        let backing = BackingWitness::review(&parent)?;
        backing.validate_directory(&parent)?;
        let parent_path = std::fs::read_link(format!("/proc/self/fd/{}", parent.as_raw_fd()))?;
        let parent_facts = parent.metadata()?;
        let (directory, name, facts) = allocate_child(&parent, record.grant.caller)?;
        Ok(ScratchLease {
            _allocation_lease: Some(allocation_lease),
            _backing: backing,
            directory,
            quota_bytes,
            receipt: ScratchReceipt {
                parent: parent_path,
                parent_device: parent_facts.dev(),
                parent_inode: parent_facts.ino(),
                name,
                device: facts.dev(),
                inode: facts.ino(),
                caller: record.grant.caller,
            },
        })
    }
}

impl Store {
    fn inventory_allocation_lease(&self) -> io::Result<FileLease> {
        let directory = storage(&self.records)?;
        let name = OsStr::new("inventory-allocation.lock");
        match directory.publish(name, b"", PublishMode::IfAbsent, Privacy::OwnerOnly) {
            Ok(_) => {}
            Err(error) if error.io_error().kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into_io_error()),
        }
        FileLease::try_acquire(
            directory.open_append_existing(name, Privacy::OwnerOnly)?,
            LeaseMode::Exclusive,
        )
        .map_err(io::Error::other)
    }
}

fn allocate_child(parent: &File, caller: u32) -> io::Result<(File, String, std::fs::Metadata)> {
    let name = format!("inventory-{}", uuid::Uuid::new_v4());
    let entry = CString::new(name.as_bytes()).map_err(io::Error::other)?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), entry.as_ptr(), 0o700) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            entry.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        let error = io::Error::last_os_error();
        if unsafe { libc::unlinkat(parent.as_raw_fd(), entry.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
            return Err(io::Error::last_os_error());
        }
        return Err(error);
    }
    let directory = unsafe { File::from_raw_fd(raw) };
    let reviewed = (|| {
        if unsafe { libc::fchown(directory.as_raw_fd(), caller, u32::MAX) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let facts = directory.metadata()?;
        if !facts.is_dir() || facts.mode() & 0o7777 != 0o700 || facts.uid() != caller {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        migration_durability::sync_directory(parent)?;
        Ok(facts)
    })();
    match reviewed {
        Ok(facts) => Ok((directory, name, facts)),
        Err(error) => {
            // No worker has received this directory. Even construction failure
            // removes only the exact held empty inode, never scans names.
            cleanup_fresh_child(parent, &entry, &directory)?;
            Err(error)
        }
    }
}

fn cleanup_fresh_child(parent: &File, name: &CString, directory: &File) -> io::Result<()> {
    let held = directory.metadata()?;
    let named = cap_primitives::fs::open_dir_nofollow(
        parent,
        Path::new(std::ffi::OsStr::from_bytes(name.as_bytes())),
    )?;
    let actual = named.metadata()?;
    if !held.is_dir()
        || !actual.is_dir()
        || (held.dev(), held.ino()) != (actual.dev(), actual.ino())
    {
        return Err(failure("Fresh inventory scratch name changed"));
    }
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
        return Err(io::Error::last_os_error());
    }
    migration_durability::sync_directory(parent)
}
