//! Inventory spill storage allocated below an acknowledged preparation operation.
use super::super::home_capture::BackingProof;
use crate::{
    migration::PinnedDirectory,
    migration_durability,
    migration_review::{InventoryScratch, InventoryScratchOwnership},
};
use std::{io, path::Path, sync::Arc};
const RESERVE: u64 = 256 * 1024 * 1024;
struct StorageOwnership {
    preparation: Arc<dyn InventoryScratchOwnership>,
    backing: BackingProof,
    directory: Arc<PinnedDirectory>,
}
impl InventoryScratchOwnership for StorageOwnership {
    fn check(&self) -> io::Result<()> {
        self.preparation.check()?;
        self.directory.validate_destination()?;
        self.backing.validate_directory(&self.directory.file)
    }
}
/// The caller retains the preparation's current-operation guard during review.
/// The supplied ownership is rechecked on every use, including after retirement.
pub fn review(
    operation: &Path,
    preparation: Arc<dyn InventoryScratchOwnership>,
    current: crate::prepared_identity::DraftLock,
) -> io::Result<Arc<InventoryScratch>> {
    let parent = PinnedDirectory::open_absolute(operation)?;
    parent.validate_destination()?;
    crate::destination_security::check_private_file(&parent.file)?;
    // Fresh exclusive child, held below the already acknowledged operation.
    // A failed review preserves this private ownership rather than deleting data.
    let parent_readable = migration_durability::readable_directory(&parent.file)?;
    let parent_backing = BackingProof::review(&parent_readable)?;
    parent_backing.validate_directory(&parent.file)?;
    let name = format!("inventory-{}", uuid::Uuid::new_v4());
    let directory = parent.create_child(std::ffi::OsStr::new(&name))?;
    let readable = migration_durability::readable_directory(&directory.file)?;
    let backing = BackingProof::review(&readable)?;
    let capacity = rustix::fs::fstatvfs(&readable)?;
    let allowance = capacity
        .f_bavail
        .checked_mul(capacity.f_frsize)
        .and_then(|bytes| bytes.checked_sub(RESERVE))
        .filter(|bytes| *bytes > 0)
        .ok_or(io::ErrorKind::StorageFull)?;
    drop(current);
    InventoryScratch::review_with_ownership(
        directory.clone(),
        allowance,
        Arc::new(StorageOwnership {
            preparation,
            backing,
            directory,
        }),
    )
}
