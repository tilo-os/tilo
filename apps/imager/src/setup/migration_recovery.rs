//! Conservative low-space review and recovery using the existing planner and
//! verified journaled copier. A copy is never permission to erase its source.
//! Remote account access and sync labels are not independent recovery evidence.
mod destination;
mod flow;
mod write_guard;
pub(crate) use flow::Evidence;
pub use flow::Flow;

use super::windows_inventory::{Availability, Inventory};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

/// Re-run the real placement checker, including filesystem shrink floors and
/// retained headroom. A hand-edited geometry or stale size cannot waive it.
/// This admits only the existing alongside path, never full-disk replacement.
pub use imager_backend::recovery::admit_retained;

/// Required local recovery space. No remote content-verification API is wired
/// here: even entries labelled synced remain unverified and retain their source.
#[derive(Debug, Default, Serialize)]
pub struct RecoveryBudget {
    pub local_bytes: u64,
    pub unverified_placeholder_bytes: u64,
    pub unknown_size_entries: u64,
    pub coverage_gaps: usize,
    pub source_must_remain: bool,
}
impl RecoveryBudget {
    pub fn windows(inventory: &Inventory) -> Result<Self, String> {
        let mut result = Self {
            source_must_remain: true,
            ..Self::default()
        };
        for root in &inventory.roots {
            result.coverage_gaps = result
                .coverage_gaps
                .checked_add(root.gaps.len())
                .ok_or("Windows inventory exceeds recovery bounds")?;
            for entry in &root.entries {
                let Some(bytes) = entry.bytes else {
                    result.unknown_size_entries += 1;
                    continue;
                };
                let total = if entry.availability == Availability::Local {
                    &mut result.local_bytes
                } else {
                    &mut result.unverified_placeholder_bytes
                };
                *total = total
                    .checked_add(bytes)
                    .ok_or("Windows recovery byte count overflow")?;
            }
        }
        Ok(result)
    }
}

/// An exact, user-selected local/media destination. Remote URLs are not offered
/// as writable backups: there is no remote write-and-independent-verify adapter.
/// Destination durability and source containment are checked by the real copier.
/// Completed output still confers no disk-write or source-deletion authority.
pub struct RecoveryCopy {
    source: PathBuf,
    destination: PathBuf,
    journal: PathBuf,
    root: Arc<super::migration::PinnedDirectory>,
    inventory: super::migration_review::SourceInventory,
    reviewed: (
        super::migration_durability::Identity,
        super::migration_durability::Identity,
        [u8; 32],
    ),
}
impl RecoveryCopy {
    pub fn review(source: &Path, destination: &Path, journal: &Path) -> Result<Self, String> {
        super::migration_worker::durable_copy_available()?;
        let review = || -> std::io::Result<Self> {
            let root = super::migration::PinnedDirectory::open_absolute(source)?;
            let parent = super::migration::PinnedDirectory::open_absolute(
                destination
                    .parent()
                    .ok_or_else(|| std::io::Error::other("Select a destination folder"))?,
            )?;
            parent.validate_destination()?;
            let (records, _) = super::migration_journal_log::parent(journal)?;
            if records.contains_identity(&root.identity)
                || root.contains_identity(&records.identity)
                || destination.starts_with(
                    journal.parent().ok_or_else(|| {
                        std::io::Error::other("Select a private journal directory")
                    })?,
                )
            {
                return Err(std::io::Error::other(
                    "Recovery journal must be in a separate private directory outside both copied trees",
                ));
            }
            if root.contains_identity(&parent.identity) || parent.contains_identity(&root.identity)
            {
                return Err(std::io::Error::other(
                    "Select an independent destination outside the source",
                ));
            }
            let inventory = super::migration_review::SourceInventory::capture(&root)?;
            // A resumed output may already occupy the required capacity.
            // Existence never establishes verification: the journal engine must
            // rehash it, and any remaining space failure leaves the source intact.
            let needed = if destination.try_exists()? {
                0
            } else {
                inventory.summary()?.bytes
            };
            destination::capacity(&parent, needed)?;
            let reviewed = (
                super::migration_durability::identity(
                    &super::migration_durability::readable_directory(&root.file)?,
                )?,
                super::migration_durability::identity(
                    &super::migration_durability::readable_directory(&parent.file)?,
                )?,
                inventory.revision()?,
            );
            Ok(Self {
                source: source.into(),
                destination: destination.into(),
                journal: journal.into(),
                root,
                inventory,
                reviewed,
            })
        };
        review().map_err(|error| error.to_string())
    }

    pub(crate) fn validate_source(&self, cancelled: &AtomicBool) -> Result<(), String> {
        self.inventory
            .validate_with_cancel(&self.root, cancelled)
            .map_err(|e| e.to_string())
    }

    /// Resume only on explicit request. The engine independently checks saved
    /// source/output identities, hashes and durable receipts, not file existence.
    /// Partial verified output is retained on cancellation; the source is untouched.
    pub fn copy(
        &self,
        resume: bool,
        cancelled: &AtomicBool,
        progress: impl FnMut(super::migration::CopyProgress),
    ) -> Result<super::migration::CopyReport, String> {
        super::migration::check_cancel(cancelled).map_err(|error| error.to_string())?;
        self.inventory
            .validate_with_cancel(&self.root, cancelled)
            .map_err(|error| error.to_string())?;
        super::migration_journal::copy_reviewed(
            &self.source,
            &self.destination,
            &self.journal,
            &self.reviewed,
            resume,
            cancelled,
            progress,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::windows_inventory::{RootRole, SelectedRoot};
    use super::*;

    #[test]
    fn cloud_membership_and_exclusion_never_reduce_local_recovery_budget() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("save.dat"), b"unique-save").unwrap();
        std::fs::write(temp.path().join("document"), b"document").unwrap();
        let mut inventory = Inventory::discover(&[SelectedRoot {
            path: temp.path().into(),
            role: RootRole::OneDrive,
        }])
        .unwrap();
        let entry = inventory.roots[0]
            .entries
            .iter_mut()
            .find(|entry| entry.relative == Path::new("save.dat"))
            .unwrap();
        entry.sync_verified = true; // Not a verifier receipt.
        assert_eq!(RecoveryBudget::windows(&inventory).unwrap().local_bytes, 19);
        let id = inventory.roots[0].source_id.clone();
        inventory
            .select(
                &id,
                super::super::windows_inventory::Decision::Exclude,
                Some("not importing".into()),
            )
            .unwrap();
        let budget = RecoveryBudget::windows(&inventory).unwrap();
        assert_eq!(budget.local_bytes, 19);
        assert!(budget.source_must_remain);
        inventory.roots[0]
            .entries
            .iter_mut()
            .find(|entry| entry.relative == Path::new("save.dat"))
            .unwrap()
            .availability = Availability::Placeholder;
        let budget = RecoveryBudget::windows(&inventory).unwrap();
        assert_eq!(budget.local_bytes, 8);
        assert_eq!(budget.unverified_placeholder_bytes, 11);
        assert!(budget.source_must_remain);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn interrupted_copy_resumes_through_verified_receipts_and_retains_originals() {
        super::super::test_support::isolated("recovery-resume", || {
            use std::os::unix::fs::PermissionsExt;
            use std::sync::atomic::Ordering;
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let backup = temp.path().join("backup");
            let records = temp.path().join("records");
            for path in [&source, &backup, &records] {
                std::fs::create_dir(path).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            std::fs::write(source.join("save"), b"unique save").unwrap();
            std::fs::write(source.join("archive"), b"unique archive").unwrap();
            let output = backup.join("recovered");
            let copy = RecoveryCopy::review(&source, &output, &records.join("journal")).unwrap();
            let cancelled = AtomicBool::new(false);
            let report = copy
                .copy(false, &cancelled, |progress| {
                    if progress.verified_files > 0 {
                        cancelled.store(true, Ordering::Relaxed);
                    }
                })
                .unwrap();
            assert!(!report.complete());
            assert!(!report.verified_files.is_empty());
            cancelled.store(false, Ordering::Relaxed);
            let report = copy.copy(true, &cancelled, |_| {}).unwrap();
            assert!(report.complete(), "{report:?}");
            for name in ["save", "archive"] {
                assert_eq!(
                    std::fs::read(source.join(name)).unwrap(),
                    std::fs::read(output.join(name)).unwrap()
                );
            }
            // A receipt is not an exemption from output hash verification.
            std::fs::write(output.join("save"), b"corrupt").unwrap();
            let report = copy.copy(true, &cancelled, |_| {}).unwrap();
            assert!(!report.complete());
            assert_eq!(std::fs::read(source.join("save")).unwrap(), b"unique save");
        });
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn reviewed_copy_detects_source_changes_without_removing_anything() {
        super::super::test_support::isolated("recovery-source-change", || {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let parent = temp.path().join("backup");
            std::fs::create_dir(&source).unwrap();
            std::fs::create_dir(&parent).unwrap();
            let file = source.join("save");
            std::fs::write(&file, b"first").unwrap();
            let records = temp.path().join("records");
            std::fs::create_dir(&records).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o700)).unwrap();
            let copy =
                RecoveryCopy::review(&source, &parent.join("copy"), &records.join("journal"))
                    .unwrap();
            std::fs::write(&file, b"new unique save").unwrap();
            assert!(copy.copy(false, &AtomicBool::new(false), |_| {}).is_err());
            assert_eq!(std::fs::read(file).unwrap(), b"new unique save");
            assert!(!parent.join("copy").exists());
        });
    }
}
