//! Exact terminal effects require actual private completion evidence, not JSON.
use super::super::super::installed_copy_output::InstalledCopyCompletionFacts;
use super::*;
use crate::setup::migration::PinnedDirectory;
use std::{
    collections::BTreeSet,
    os::fd::AsRawFd,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
fn denied() -> io::Error {
    failure("Installed completed cleanup custody differs")
}
fn absent(parent: &File, name: &str) -> io::Result<()> {
    match crate::setup::nofollow::metadata_nofollow(parent, Path::new(name)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        _ => Err(denied()),
    }
}
fn empty(pin: &PinnedDirectory) -> io::Result<()> {
    let dir = cap_std::fs::Dir::from_std_file(pin.file.try_clone()?);
    if dir.entries()?.next().transpose()?.is_some() {
        return Err(failure(
            "Installed completed directory is not empty; retained",
        ));
    }
    Ok(())
}
fn child(
    parent: &Arc<PinnedDirectory>,
    name: &str,
    id: (u64, u64),
    owner: (u32, u32),
) -> io::Result<Arc<PinnedDirectory>> {
    let metadata = crate::setup::nofollow::metadata_nofollow(&parent.file, Path::new(name))?;
    let pin = parent.open_child(OsStr::new(name), &metadata)?;
    let m = pin.file.metadata()?;
    if !m.is_dir()
        || (m.dev(), m.ino()) != id
        || (m.uid(), m.gid()) != owner
        || m.mode() & 0o7777 != 0o700
    {
        return Err(denied());
    }
    pin.validate()?;
    Ok(pin)
}
/// Constructor-private permit retains actual completion/source/target custody.
/// Private flags change only after this object's exact owned native syscall.
pub(in crate::setup::home_capture) struct CompletedCleanup {
    attempt: Arc<InstalledCopyAttempt>,
    completion: Arc<InstalledCopyCompletion>,
    facts: InstalledCopyCompletionFacts,
    checkpoint: Vec<u8>,
    verified: InstalledVerifiedArtifactFacts,
    scratch: Scratch,
    parent: Arc<PinnedDirectory>,
    stage: Arc<PinnedDirectory>,
    output: Arc<PinnedDirectory>,
    inventory: Arc<PinnedDirectory>,
    sealed: Arc<PinnedDirectory>,
    removed_files: Mutex<BTreeSet<String>>,
    removed_dirs: Mutex<BTreeSet<String>>,
    stage_removed: AtomicBool,
    secret_removed: AtomicBool,
    artifacts_removed: AtomicBool,
    metadata_quarantined: AtomicBool,
    finished: AtomicBool,
}
impl InstalledCopyAttempt {
    pub(in crate::setup::home_capture) fn begin_completed_cleanup(
        self: &Arc<Self>,
        completion: Arc<InstalledCopyCompletion>,
        cancel: &AtomicBool,
    ) -> io::Result<CompletedCleanup> {
        let facts = completion.retirement_facts(self, cancel)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let record = load(&store)?.ok_or_else(denied)?;
        match_complete(&record, self, &facts)?;
        if record.material_retired {
            return Err(failure("Installed completed material already retired"));
        }
        let scratch = record.scratch.clone().ok_or_else(denied)?;
        let parent = store.installed_copy_snapshots(&self.binding.operation)?;
        let stage = child(&parent, &scratch.stage_name, scratch.stage, (0, 0))?;
        let caller = (self.binding.source_uid, self.binding.source_gid);
        let output = child(&stage, "output", scratch.output, caller)?;
        let inventory = child(
            &stage,
            &format!("inventory-{}", self.attempt),
            scratch.inventory,
            caller,
        )?;
        let actual_sealed = completion.completed_artifact_directory();
        let metadata = actual_sealed.file.metadata()?;
        let sealed = child(
            &stage,
            "artifacts",
            (metadata.dev(), metadata.ino()),
            (0, 0),
        )?;
        if fs_native::snapshot_lease::mount_id(&sealed.file)? != self.target_storage.mount {
            return Err(denied());
        }
        let permit = CompletedCleanup {
            attempt: self.clone(),
            completion,
            facts,
            checkpoint: serde_json::to_vec(&record)?,
            verified: record.verified.clone().ok_or_else(denied)?,
            scratch,
            parent,
            stage,
            output,
            inventory,
            sealed,
            removed_files: Mutex::new(BTreeSet::new()),
            removed_dirs: Mutex::new(BTreeSet::new()),
            stage_removed: AtomicBool::new(false),
            secret_removed: AtomicBool::new(false),
            artifacts_removed: AtomicBool::new(false),
            metadata_quarantined: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        };
        drop(store);
        permit.check(cancel)?;
        // Refuse unknown inventory/stage contents BEFORE any artifact unlink.
        let directory = cap_std::fs::Dir::from_std_file(permit.stage.file.try_clone()?);
        let inventory = format!("inventory-{}", self.attempt);
        let mut count = 0;
        for entry in directory.entries()? {
            count += 1;
            let entry = entry?;
            if count > 3
                || !["output", "artifacts", inventory.as_str()]
                    .iter()
                    .any(|name| entry.file_name() == OsStr::new(name))
            {
                return Err(failure(
                    "Installed completed stage has unknown entries; retained",
                ));
            }
        }
        empty(&permit.output)?;
        permit.check(cancel)?;
        Ok(permit)
    }
}
fn match_complete(
    record: &Record,
    attempt: &InstalledCopyAttempt,
    facts: &InstalledCopyCompletionFacts,
) -> io::Result<()> {
    let receiver = record.receiver.as_ref().ok_or_else(denied)?;
    let verified = record.verified.as_ref().ok_or_else(denied)?;
    if record.phase != Phase::Complete
        || record.attempt != attempt.attempt
        || record.binding != attempt.binding
        || record.secret != attempt.secret_identity
        || record.target_storage != attempt.target_storage
        || (facts.receiver_pid, facts.receiver_start) != (receiver.pid, receiver.start)
        || facts.expectations_digest != verified.expectations_digest
        || facts.home_sha256 != verified.home.as_ref().map(|h| h.ciphertext_sha256)
        || facts.applications_sha256 != verified.applications.as_ref().map(|a| a.ciphertext_sha256)
    {
        return Err(denied());
    }
    receiver.custody.require_stopped()?;
    record
        .export_namespace
        .as_ref()
        .ok_or_else(denied)?
        .custody
        .require_stopped()?;
    record
        .verify_namespace
        .as_ref()
        .ok_or_else(denied)?
        .custody
        .require_stopped()?;
    if let Some(namespace) = &record.recovery_namespace {
        namespace.custody.require_stopped()?;
    }
    Ok(())
}
impl CompletedCleanup {
    pub(in crate::setup::home_capture) fn check(&self, cancel: &AtomicBool) -> io::Result<()> {
        let a = &self.attempt;
        self.completion.retirement_peer_stopped(a, cancel)?;
        if Binding::actual(&a.source.output_attempt_scope(cancel)?, &a.target)? != a.binding
            || TargetStorage::actual(a.physical.facts(cancel)?)? != a.target_storage
        {
            return Err(denied());
        }
        let store = a
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let record = load(&store)?.ok_or_else(denied)?;
        match_complete(&record, a, &self.facts)?;
        if serde_json::to_vec(&record)? != self.checkpoint {
            let mut expected: Record =
                serde_json::from_slice(&self.checkpoint).map_err(|_| denied())?;
            expected.material_retired = true;
            expected.metadata_quarantined = self.metadata_quarantined.load(Ordering::Acquire);
            if !self.secret_removed.load(Ordering::Acquire)
                || !self.artifacts_removed.load(Ordering::Acquire)
                || serde_json::to_vec(&record)? != serde_json::to_vec(&expected)?
            {
                return Err(denied());
            }
            // Same permit observed every native effect. This accepts only its
            // exact terminal receipt if fsync/publication returned an error later.
            self.finished.store(true, Ordering::Release);
        }
        let parent = store.installed_copy_snapshots(&a.binding.operation)?;
        let held = self.parent.file.metadata()?;
        let current = parent.file.metadata()?;
        if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
            return Err(denied());
        }
        self.parent.validate_destination()?;
        if fs_native::snapshot_lease::mount_id(&self.stage.file)? != a.target_storage.mount {
            return Err(denied());
        }
        if self.stage_removed.load(Ordering::Acquire) {
            absent(&self.parent.file, &self.scratch.stage_name)?;
            if self.stage.file.metadata()?.nlink() != 0 {
                return Err(denied());
            }
        } else {
            self.stage.validate_destination()?;
        }
        if self.secret_removed.load(Ordering::Acquire) {
            absent(&store.records.file, &secret_name(&a.binding.operation))?;
            let m = a.secret.metadata()?;
            if m.nlink() != 0
                || !m.is_file()
                || (
                    m.dev(),
                    m.ino(),
                    m.len(),
                    m.uid(),
                    m.gid(),
                    m.mode() & 0o7777,
                ) != (
                    a.secret_identity.dev,
                    a.secret_identity.ino,
                    a.secret_identity.len,
                    0,
                    0,
                    0o600,
                )
            {
                return Err(denied());
            }
            let mut bytes = Zeroizing::new(vec![0; m.len() as usize]);
            a.secret.read_exact_at(&mut bytes, 0)?;
            let digest: [u8; 32] = sha2::Sha256::digest(&*bytes).into();
            if digest != a.secret_identity.sha256 {
                return Err(denied());
            }
        } else {
            let named = storage(&store.records)?.open_read(
                OsStr::new(&secret_name(&a.binding.operation)),
                Privacy::OwnerOnly,
            )?;
            if secret_facts(&named)? != a.secret_identity
                || secret_facts(&a.secret)? != a.secret_identity
            {
                return Err(denied());
            }
        }
        drop(store);
        a.source.revalidate(cancel)?;
        if a.source.output_read_claim_digest(cancel)? != record.read_claim.ok_or_else(denied)? {
            return Err(denied());
        }
        Ok(())
    }
    fn artifact_removed(
        &self,
        file: &File,
        facts: &super::super::super::installed_copy_output::InstalledVerifiedFileFacts,
        cancel: &AtomicBool,
    ) -> io::Result<bool> {
        self.check(cancel)?;
        if !self
            .removed_files
            .lock()
            .map_err(|_| denied())?
            .contains(&facts.name)
        {
            return Ok(false);
        }
        let leaf = facts.name.strip_prefix("artifacts/").ok_or_else(denied)?;
        absent(&self.sealed.file, leaf)?;
        let m = file.metadata()?;
        if m.nlink() != 0
            || !m.is_file()
            || (
                m.dev(),
                m.ino(),
                m.len(),
                m.uid(),
                m.gid(),
                m.mode() & 0o7777,
            ) != (facts.dev, facts.ino, facts.len, 0, 0, 0o600)
        {
            return Err(denied());
        }
        let mut hash = sha2::Sha256::new();
        let mut offset = 0;
        let mut buffer = [0; 65536];
        while offset < m.len() {
            if cancel.load(Ordering::Acquire) {
                return Err(failure("Installed completed cleanup cancelled"));
            }
            let count = (m.len() - offset).min(buffer.len() as u64) as usize;
            file.read_exact_at(&mut buffer[..count], offset)?;
            hash.update(&buffer[..count]);
            offset += count as u64;
        }
        let digest: [u8; 32] = hash.finalize().into();
        if digest != facts.ciphertext_digest {
            return Err(denied());
        }
        self.check(cancel)?;
        Ok(true)
    }
    pub(in crate::setup::home_capture) fn remove_artifact(
        &self,
        file: &File,
        facts: &super::super::super::installed_copy_output::InstalledVerifiedFileFacts,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if !self.verified.files.iter().any(|expected| expected == facts) {
            return Err(denied());
        }
        if self.artifact_removed(file, facts, cancel)? {
            // Native absence proves this permit's unlink, not its durability.
            migration_durability::sync_directory(&self.sealed.file)?;
            self.check(cancel)?;
            return Ok(());
        }
        self.check(cancel)?;
        self.sealed.validate_destination()?;
        let leaf = facts.name.strip_prefix("artifacts/").ok_or_else(denied)?;
        let native = super::super::super::installed_copy_output::measure(
            file,
            leaf,
            &self.sealed.file,
            0,
            "artifacts",
            cancel,
        )?;
        if &native != facts
            || fs_native::snapshot_lease::mount_id(file)? != self.attempt.target_storage.mount
        {
            return Err(denied());
        }
        self.check(cancel)?;
        let name = std::ffi::CString::new(leaf).map_err(|_| denied())?;
        if unsafe { libc::unlinkat(self.sealed.file.as_raw_fd(), name.as_ptr(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // Only THIS permit observes and records actual syscall success. Missing
        // names alone never grant idempotence, and caller flags cannot add it.
        self.removed_files
            .lock()
            .map_err(|_| denied())?
            .insert(facts.name.clone());
        migration_durability::sync_directory(&self.sealed.file)?;
        if !self.artifact_removed(file, facts, cancel)? {
            return Err(denied());
        }
        Ok(())
    }
    fn remove_directory(
        &self,
        name: &str,
        pin: &Arc<PinnedDirectory>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.check(cancel)?;
        if self
            .removed_dirs
            .lock()
            .map_err(|_| denied())?
            .contains(name)
        {
            absent(&self.stage.file, name)?;
            if pin.file.metadata()?.nlink() != 0 {
                return Err(denied());
            }
            migration_durability::sync_directory(&self.stage.file)?;
            self.check(cancel)?;
            return Ok(());
        }
        pin.validate()?;
        empty(pin)?;
        if fs_native::snapshot_lease::mount_id(&pin.file)? != self.attempt.target_storage.mount {
            return Err(denied());
        }
        let cname = std::ffi::CString::new(name).map_err(|_| denied())?;
        if unsafe {
            libc::unlinkat(
                self.stage.file.as_raw_fd(),
                cname.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        self.removed_dirs
            .lock()
            .map_err(|_| denied())?
            .insert(name.to_owned());
        migration_durability::sync_directory(&self.stage.file)?;
        absent(&self.stage.file, name)?;
        if pin.file.metadata()?.nlink() != 0 {
            return Err(denied());
        }
        Ok(())
    }
    pub(in crate::setup::home_capture) fn remove_empty_directories(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.check(cancel)?;
        self.remove_directory("artifacts", &self.sealed, cancel)?;
        self.remove_directory("output", &self.output, cancel)?;
        self.artifacts_removed.store(true, Ordering::Release);
        if self.stage_removed.load(Ordering::Acquire) {
            migration_durability::sync_directory(&self.parent.file)?;
            return self.check(cancel);
        }
        let inventory = format!("inventory-{}", self.attempt.attempt);
        if !self
            .removed_dirs
            .lock()
            .map_err(|_| denied())?
            .contains(&inventory)
        {
            self.inventory.validate()?;
            let metadata = cap_std::fs::Dir::from_std_file(self.inventory.file.try_clone()?);
            if metadata.entries()?.next().transpose()?.is_some() {
                self.metadata_quarantined.store(true, Ordering::Release);
                migration_durability::sync_directory(&self.stage.file)?;
                return self.check(cancel);
            }
        }
        self.remove_directory(&inventory, &self.inventory, cancel)?;
        self.check(cancel)?;
        empty(&self.stage)?;
        let name =
            std::ffi::CString::new(self.scratch.stage_name.as_str()).map_err(|_| denied())?;
        if unsafe {
            libc::unlinkat(
                self.parent.file.as_raw_fd(),
                name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        self.stage_removed.store(true, Ordering::Release);
        migration_durability::sync_directory(&self.parent.file)?;
        self.check(cancel)
    }
    pub(in crate::setup::home_capture) fn metadata_quarantined(&self) -> bool {
        self.metadata_quarantined.load(Ordering::Acquire)
    }
    pub(in crate::setup::home_capture) fn remove_secret_and_finish(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.check(cancel)?;
        if self.finished.load(Ordering::Acquire) {
            // A previous replace may have published before its fsync failed.
            let store = self.attempt.store.lock().map_err(|_| denied())?;
            migration_durability::sync_directory(&store.records.file)?;
            drop(store);
            self.check(cancel)?;
            return Ok(());
        }
        if !self.artifacts_removed.load(Ordering::Acquire)
            || (!self.stage_removed.load(Ordering::Acquire)
                && !self.metadata_quarantined.load(Ordering::Acquire))
        {
            return Err(denied());
        }
        let a = &self.attempt;
        if !self.secret_removed.load(Ordering::Acquire) {
            let store = a
                .store
                .lock()
                .map_err(|_| failure("Installed Store lock poisoned"))?;
            let name =
                std::ffi::CString::new(secret_name(&a.binding.operation)).map_err(|_| denied())?;
            if unsafe { libc::unlinkat(store.records.file.as_raw_fd(), name.as_ptr(), 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
            self.secret_removed.store(true, Ordering::Release);
        }
        {
            let store = a.store.lock().map_err(|_| denied())?;
            migration_durability::sync_directory(&store.records.file)?;
        }
        self.check(cancel)?;
        let store = a
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record = load(&store)?.ok_or_else(denied)?;
        if serde_json::to_vec(&record)? != self.checkpoint {
            return Err(denied());
        }
        record.material_retired = true;
        record.metadata_quarantined = self.metadata_quarantined.load(Ordering::Acquire);
        save(&store, &record, PublishMode::Replace)?;
        self.finished.store(true, Ordering::Release);
        Ok(())
    }
}
