//! Inventory spill storage allocated below an acknowledged preparation operation.
use super::super::home_capture::BackingWitness;
use crate::{
    migration::PinnedDirectory,
    migration_durability,
    migration_review::{InventoryScratch, InventoryScratchCustody},
};
use std::{io, os::fd::AsRawFd, path::Path, sync::Arc};
const RESERVE: u64 = 256 * 1024 * 1024;
struct StorageCustody {
    preparation: Arc<dyn InventoryScratchCustody>,
    backing: BackingWitness,
    directory: Arc<PinnedDirectory>,
}
impl InventoryScratchCustody for StorageCustody {
    fn check(&self) -> io::Result<()> {
        self.preparation.check()?;
        self.directory.validate_destination()?;
        self.backing.validate_directory(&self.directory.file)
    }
}
/// The caller retains the preparation's current-operation guard during review.
/// The supplied custody is rechecked on every use, including after retirement.
pub fn review(
    operation: &Path,
    preparation: Arc<dyn InventoryScratchCustody>,
    current: crate::prepared_identity::DraftLock,
) -> io::Result<Arc<InventoryScratch>> {
    let parent = PinnedDirectory::open_absolute(&operation)?;
    parent.validate_destination()?;
    crate::destination_security::check_private_file(&parent.file)?;
    // Fresh exclusive child, held below the already acknowledged operation.
    // A failed review preserves this private custody rather than deleting data.
    let parent_readable = migration_durability::readable_directory(&parent.file)?;
    let parent_backing = BackingWitness::review(&parent_readable)?;
    parent_backing.validate_directory(&parent.file)?;
    let name = format!("inventory-{}", uuid::Uuid::new_v4());
    let directory = parent.create_child(std::ffi::OsStr::new(&name))?;
    let readable = migration_durability::readable_directory(&directory.file)?;
    let backing = BackingWitness::review(&readable)?;
    let mut capacity: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: writes the complete statvfs for this retained directory descriptor.
    if unsafe { libc::fstatvfs(readable.as_raw_fd(), &raw mut capacity) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let allowance = capacity
        .f_bavail
        .checked_mul(capacity.f_frsize)
        .and_then(|bytes| bytes.checked_sub(RESERVE))
        .filter(|bytes| *bytes > 0)
        .ok_or(io::ErrorKind::StorageFull)?;
    drop(current);
    InventoryScratch::review_with_custody(
        directory.clone(),
        allowance,
        Arc::new(StorageCustody {
            preparation,
            backing,
            directory,
        }),
    )
}
