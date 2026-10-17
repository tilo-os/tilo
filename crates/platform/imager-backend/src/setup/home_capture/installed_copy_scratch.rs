//! Local inventory/output custody on the canonical installed Store filesystem.
//! No launch, adoption, completion, medium or automatic deletion authority.
use super::{
    inventory_scratch::InventoryScratch,
    original_copy_read::{OriginalCopyReadAdmission, OriginalCopyScratchScope},
    store::{InstalledCopyAttempt, InstalledCopyRecovery, Store},
};
use crate::setup::{
    migration::PinnedDirectory,
    migration_durability,
    migration_handoff::{InstalledScratchFacts, InstalledScratchIdentity},
    nofollow,
};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::Path,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
/// Native observations for the controller journal, not a launch grant.
pub struct InstalledCopyScratchFacts {
    pub stage_name: String,
    pub operation: String,
    pub caller: (u32, u32),
    pub reservation: u64,
    pub allowance: u64,
    pub stage: (u64, u64),
    pub output: (u64, u64),
    pub inventory: (u64, u64),
    pub filesystem: InstalledScratchFacts,
}
pub struct InstalledCopyScratch {
    store: Arc<Mutex<Store>>,
    source: Arc<OriginalCopyReadAdmission>,
    scope: OriginalCopyScratchScope,
    parent: Arc<PinnedDirectory>,
    stage: Arc<PinnedDirectory>,
    stage_name: String,
    physical: InstalledScratchIdentity,
    output: File,
    output_identity: (u64, u64),
    inventory: InventoryScratch,
}
impl InstalledCopyScratch {
    pub(super) fn matches_source(&self, source: &Arc<OriginalCopyReadAdmission>) -> bool {
        Arc::ptr_eq(&self.source, source)
    }
    /// The durable controller records this canonical attempt BEFORE construction;
    /// it must journal the returned native identities BEFORE exporting any FD.
    /// Existing names are refused. Errors leave evidence; never adopt or repair.
    pub fn create(journal: &InstalledCopyAttempt, cancel: &AtomicBool) -> io::Result<Self> {
        journal.check_prepared(cancel)?;
        let store = journal.store();
        let source = journal.source();
        let attempt = journal.attempt();
        if !uuid::Uuid::parse_str(attempt).is_ok_and(|id| !id.is_nil() && id.to_string() == attempt)
        {
            return Err(refused("Installed scratch attempt is not canonical"));
        }
        let scope = source.output_scratch_scope(cancel)?;
        let parent = store
            .lock()
            .map_err(|_| refused("Installed Store lock poisoned"))?
            .installed_copy_snapshots(&scope.operation)?;
        let physical = InstalledScratchIdentity::review(
            migration_durability::readable_directory(&parent.file)?,
            source.clone(),
            cancel,
        )?;
        // Full source geometry and canonical Store custody precede any mkdir.
        physical.revalidate(cancel)?;
        store
            .lock()
            .map_err(|_| refused("Installed Store lock poisoned"))?
            .installed_copy_snapshots(&scope.operation)?;
        let stage_name = format!("installed-copy-{}-{attempt}", scope.operation);
        let stage = parent.create_child(OsStr::new(&stage_name))?;
        let stage_file = migration_durability::readable_directory(&stage.file)?;
        physical.validate_child(&stage_file, cancel)?;
        let child = stage.create_child(OsStr::new("output"))?;
        let output = migration_durability::readable_directory(&child.file)?;
        physical.validate_child(&output, cancel)?;
        let m = output.metadata()?;
        let output_identity = (m.dev(), m.ino());
        if unsafe { libc::fchown(output.as_raw_fd(), scope.caller.0, scope.caller.1) } < 0 {
            return Err(io::Error::last_os_error());
        }
        output.sync_all()?;
        migration_durability::sync_directory(&stage.file)?;
        let inventory = InventoryScratch::create(
            stage.clone(),
            format!("inventory-{attempt}"),
            scope.caller,
            scope.reservation,
            |file| physical.validate_child(file, cancel),
        )?;
        migration_durability::sync_directory(&parent.file)?;
        let result = Self {
            store,
            source,
            scope,
            parent,
            stage,
            stage_name,
            physical,
            output,
            output_identity,
            inventory,
        };
        result.check(cancel)?;
        journal.check_prepared(cancel)?;
        Ok(result)
    }
    pub fn review_recovered(
        proof: &InstalledCopyRecovery,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let journal = proof.attempt();
        let source = journal.source();
        let store = journal.store();
        let scope = source.output_scratch_scope(cancel)?;
        let (parent, stage, output, _, _) = proof.scratch_parts(cancel)?;
        let m = output.metadata()?;
        let output_identity = (m.dev(), m.ino());
        let physical = InstalledScratchIdentity::review(
            migration_durability::readable_directory(&parent.file)?,
            source.clone(),
            cancel,
        )?;
        let inventory = InventoryScratch::review_recovered(proof, cancel)?;
        let stage_name = format!("installed-copy-{}-{}", scope.operation, journal.attempt());
        let value = Self {
            store,
            source,
            scope,
            parent,
            stage,
            stage_name,
            physical,
            output,
            output_identity,
            inventory,
        };
        value.check(cancel)?;
        proof.check(cancel)?;
        Ok(value)
    }
    pub fn check(&self, cancel: &AtomicBool) -> io::Result<()> {
        if self.source.output_scratch_scope(cancel)? != self.scope {
            return Err(refused("Installed scratch protected scope changed"));
        }
        let current = self
            .store
            .lock()
            .map_err(|_| refused("Installed Store lock poisoned"))?
            .installed_copy_snapshots(&self.scope.operation)?;
        let held = self.parent.file.metadata()?;
        let named = current.file.metadata()?;
        if (held.dev(), held.ino()) != (named.dev(), named.ino()) {
            return Err(refused("Installed scratch Store parent changed"));
        }
        self.parent.validate_destination()?;
        self.stage.validate_destination()?;
        let named = nofollow::metadata_nofollow(&self.parent.file, Path::new(&self.stage_name))?;
        let held = self.stage.file.metadata()?;
        if !held.is_dir()
            || !named.is_dir()
            || (
                cap_fs_ext::MetadataExt::dev(&named),
                cap_fs_ext::MetadataExt::ino(&named),
            ) != (held.dev(), held.ino())
            || held.uid() != 0
            || held.gid() != 0
            || held.mode() & 0o7777 != 0o700
        {
            return Err(refused("Installed scratch private stage changed"));
        }
        self.physical.validate_child(
            &migration_durability::readable_directory(&self.stage.file)?,
            cancel,
        )?;
        let named = nofollow::metadata_nofollow(&self.stage.file, Path::new("output"))?;
        let held = self.output.metadata()?;
        if !named.is_dir()
            || !held.is_dir()
            || (
                cap_fs_ext::MetadataExt::dev(&named),
                cap_fs_ext::MetadataExt::ino(&named),
            ) != self.output_identity
            || (held.dev(), held.ino()) != self.output_identity
            || held.uid() != self.scope.caller.0
            || held.gid() != self.scope.caller.1
            || held.mode() & 0o7777 != 0o700
        {
            return Err(refused("Installed scratch caller output changed"));
        }
        self.physical.validate_child(&self.output, cancel)?;
        let (inventory, _) = self.inventory.files()?;
        self.physical.validate_child(&inventory, cancel)?;
        self.source.revalidate(cancel)?;
        self.store
            .lock()
            .map_err(|_| refused("Installed Store lock poisoned"))?
            .check_original_output_operation(&self.scope.operation)
    }
    /// The controller retains Self across every original/receiver use. Cloned
    /// descriptors alone are never admission or retirement evidence.
    pub fn files(&self, cancel: &AtomicBool) -> io::Result<(File, File, u64)> {
        self.check(cancel)?;
        let output = self.output.try_clone()?;
        let (inventory, allowance) = self.inventory.files()?;
        self.check(cancel)?;
        Ok((output, inventory, allowance))
    }
    pub fn private_stage(&self, cancel: &AtomicBool) -> io::Result<Arc<PinnedDirectory>> {
        self.check(cancel)?;
        let stage = self.stage.clone();
        self.check(cancel)?;
        Ok(stage)
    }
    pub fn filesystem(&self, cancel: &AtomicBool) -> io::Result<InstalledScratchFacts> {
        self.check(cancel)?;
        self.physical.facts(cancel)
    }
    pub fn facts(&self, cancel: &AtomicBool) -> io::Result<InstalledCopyScratchFacts> {
        self.check(cancel)?;
        let stage = self.stage.file.metadata()?;
        let (inventory, allowance) = self.inventory.files()?;
        let inventory = inventory.metadata()?;
        let facts = InstalledCopyScratchFacts {
            stage_name: self.stage_name.clone(),
            operation: self.scope.operation.clone(),
            caller: self.scope.caller,
            reservation: self.scope.reservation,
            allowance,
            stage: (stage.dev(), stage.ino()),
            output: self.output_identity,
            inventory: (inventory.dev(), inventory.ino()),
            filesystem: self.filesystem(cancel)?,
        };
        self.check(cancel)?;
        Ok(facts)
    }
}
