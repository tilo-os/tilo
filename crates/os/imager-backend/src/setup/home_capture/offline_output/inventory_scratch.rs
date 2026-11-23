//! Medium-specific native policy around the shared inventory admission.
use super::{OriginalOutputBinding, refused};
use crate::setup::{home_capture::inventory_scratch::InventoryScratch, migration::PinnedDirectory};
use std::{fs::File, io, os::unix::fs::MetadataExt, sync::Arc};
pub(super) struct MediumScratch(InventoryScratch);
impl MediumScratch {
    pub(super) fn create(
        parent: Arc<PinnedDirectory>,
        binding: &OriginalOutputBinding,
        gid: u32,
    ) -> io::Result<Self> {
        let value = InventoryScratch::create(
            parent,
            format!("inventory-{}", binding.attempt),
            (binding.original_uid, gid),
            binding.reserve_bytes,
            |file| {
                if file.metadata()?.dev() != binding.partition_rdev
                    || fs_native::snapshot_lease::mount_id(file)? != binding.mount_id
                {
                    return Err(refused("Inventory storage is not on the admitted medium"));
                }
                Ok(())
            },
        )?;
        Ok(Self(value))
    }
    pub(super) fn files(&self) -> io::Result<(File, u64)> {
        self.0.files()
    }
}
