//! Narrow integration-test hooks, absent from normal builds.

use crate::copy::{CopyProgress, CopyReport, MigrationStore, OperationRecord};
use crate::review::DirectoryIdentity;
use std::path::Path;
use std::sync::{atomic::AtomicBool, mpsc};

pub fn hold_save(path: &Path, nth: usize) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    crate::migration_store::test_support::hold_save(path, nth)
}

pub fn save_snapshot(store: &MigrationStore, operations: &[OperationRecord]) -> Result<(), String> {
    store.save_snapshot(operations).map(drop)
}

pub fn run_reviewed_copy_for_test(
    source: &Path,
    destination: &Path,
    journal_path: &Path,
    reviewed: &(DirectoryIdentity, DirectoryIdentity, [u8; 32]),
    resume: bool,
    cancelled: &AtomicBool,
    progress: impl FnMut(CopyProgress),
) -> Result<CopyReport, String> {
    crate::migration_journal::copy_reviewed(
        source,
        destination,
        journal_path,
        reviewed,
        resume,
        cancelled,
        progress,
    )
}

pub use crate::migration_journal::resume_folder;
