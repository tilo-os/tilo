//! Installed source output through the same signed original worker and verifier.
//! The constructor takes an actual certified source owner; expectations are never authority.
use super::{
    InstalledCopyScratch, OriginalCopyReadAdmission,
    original_worker::{self, OriginalWorkerOwner, OriginalWorkerRun},
    runtime::{PACKAGE, TrustedCaptureService},
    store::{InstalledCopyAttempt, InstalledCopyRecovery, Store},
    worker,
};
use crate::setup::{
    flatpak_transfer::portable::Expectation as Applications,
    installed_retained_actor::ReviewedInstalledOriginalCopy,
    migration::{PinnedDirectory, check_cancel},
    migration_durability,
    portable_home_media::Expectation as Home,
};
use launcher::owned_service::{
    InstalledCopyOutputAdmission, InstalledCopyOutputBinding, InstalledCopyOutputCustody,
    InstalledCopyOutputFiles, ServiceAuthority, ServiceCancellation, ServiceEndpoint,
};
use runtime_portal::process_binding::ExecutingPeer;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use zeroize::Zeroizing;
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
pub struct InstalledCopyTransaction {
    owner: Arc<ReviewedInstalledOriginalCopy>,
    source: Arc<OriginalCopyReadAdmission>,
    attempt: Arc<InstalledCopyAttempt>,
    scratch: Arc<InstalledCopyScratch>,
    scope: super::original_copy_read::OriginalCopyAttemptScope,
    digest: [u8; 32],
    cancelled: AtomicBool,
    cancellation: ServiceCancellation,
    recovered: Option<RecoveredExport>,
}
// Only prepare_recovered can create this from the actual protected guard. Its
// retained source/files remain custody; facts alone never mint a Verified token.
struct RecoveredExport {
    _custody: InstalledCopyRecovery,
    facts: InstalledVerifiedArtifactFacts,
    sealed_parent: Option<Arc<PinnedDirectory>>,
}
impl InstalledCopyTransaction {
    /// Select from the genuine current Store journal, never an error fallback.
    pub fn prepare_or_recover(owner: Arc<ReviewedInstalledOriginalCopy>) -> io::Result<Arc<Self>> {
        owner.check()?;
        let cancelled = AtomicBool::new(false);
        let source = owner.source_admission(&cancelled)?;
        let scope = source.output_attempt_scope(&cancelled)?;
        let store = Store::system(&scope.operation)?;
        let existing = InstalledCopyAttempt::exists(&store)?;
        source.revalidate(&cancelled)?;
        owner.check()?;
        // Release THIS selector's operation lock before either typed constructor
        // takes its own same-operation lease. The constructors recheck existence.
        drop(store);
        if existing {
            Self::prepare_recovered(owner)
        } else {
            let mut secret = Zeroizing::new(vec![0; 32]);
            tilo_seal::fill_random(&mut secret).map_err(io::Error::other)?;
            Self::prepare(owner, secret)
        }
    }
    pub fn prepare(
        owner: Arc<ReviewedInstalledOriginalCopy>,
        secret: Zeroizing<Vec<u8>>,
    ) -> io::Result<Arc<Self>> {
        owner.check()?;
        let cancelled = AtomicBool::new(false);
        let source = owner.source_admission(&cancelled)?;
        let scope = source.output_attempt_scope(&cancelled)?;
        let target = owner.target_peer(&cancelled)?;
        let store = Arc::new(Mutex::new(Store::system(&scope.operation)?));
        let attempt = Arc::new(InstalledCopyAttempt::prepare(
            store,
            source.clone(),
            target,
            secret,
            &cancelled,
        )?);
        let scratch = Arc::new(InstalledCopyScratch::create(&attempt, &cancelled)?);
        attempt.bind_scratch(&scratch, &cancelled)?;
        let digest = attempt.mark_launching(&scratch, &cancelled)?;
        let value = Arc::new(Self {
            owner,
            source,
            attempt,
            scratch,
            scope,
            digest,
            cancelled,
            cancellation: ServiceCancellation::default(),
            recovered: None,
        });
        value.check()?;
        Ok(value)
    }
    /// Existing durable ciphertext is input only; a new signed verifier is mandatory.
    pub fn prepare_recovered(owner: Arc<ReviewedInstalledOriginalCopy>) -> io::Result<Arc<Self>> {
        owner.check()?;
        let cancelled = AtomicBool::new(false);
        let source = owner.source_admission(&cancelled)?;
        let scope = source.output_attempt_scope(&cancelled)?;
        let target = owner.target_peer(&cancelled)?;
        let store = Arc::new(Mutex::new(Store::system(&scope.operation)?));
        let mut recovery = None;
        let mut scratch = None;
        let result = (|| {
            recovery = Some(InstalledCopyAttempt::prepare_recovery(
                store.clone(),
                source.clone(),
                target,
                &cancelled,
            )?);
            let guard = recovery
                .as_mut()
                .ok_or_else(|| refused("Installed recovery guard absent"))?;
            guard.restore_for_verifier(&cancelled)?;
            let facts = guard.prior_export_facts(&cancelled)?;
            let sealed_parent = guard.empty_sealed_parent(&cancelled)?;
            scratch = Some(Arc::new(InstalledCopyScratch::review_recovered(
                guard, &cancelled,
            )?));
            let scratch = scratch
                .as_ref()
                .ok_or_else(|| refused("Installed recovered scratch absent"))?
                .clone();
            let attempt = guard.attempt();
            let digest = attempt.mark_recovery_verifying(&scratch, &cancelled)?;
            let value = Arc::new(Self {
                owner: owner.clone(),
                source: source.clone(),
                attempt,
                scratch,
                scope,
                digest,
                cancelled,
                cancellation: ServiceCancellation::default(),
                recovered: Some(RecoveredExport {
                    _custody: recovery
                        .take()
                        .ok_or_else(|| refused("Installed recovery custody absent"))?,
                    facts,
                    sealed_parent,
                }),
            });
            if let Err(error) = value.check() {
                // A durable verifier intent already exists. Never drop the real
                // source/output ownership after an uncertain post-intent failure.
                std::mem::forget(value);
                return Err(error);
            }
            Ok(value)
        })();
        if result.is_err() {
            // Partial native restoration is deliberately quarantined. No rollback,
            // namespace guess, secret renewal or descriptor-only reconstruction.
            std::mem::forget((owner, source, store, recovery, scratch));
        }
        result
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancellation.cancel();
    }
    pub fn operation(&self) -> &str {
        &self.scope.operation
    }
    pub fn attempt(&self) -> &InstalledCopyAttempt {
        &self.attempt
    }
    pub fn scratch(&self) -> &InstalledCopyScratch {
        &self.scratch
    }
    fn command(
        self: &Arc<Self>,
        expected: Option<(Option<Home>, Option<Applications>)>,
        runs: &mut Vec<OriginalWorkerRun<Self>>,
    ) -> io::Result<()> {
        self.check()?;
        let secret = self.attempt.secret(&self.cancelled)?;
        let output = InstalledCopyOutputAdmission::admit(self.clone())?;
        let (_, inventory, allowance) = self.scratch.files(&self.cancelled)?;
        self.source
            .with_original_reader(&self.cancelled, |admission, request, digest| {
                if digest != self.digest {
                    return Err(refused("Installed original read claim changed"));
                }
                let service = TrustedCaptureService::native_for_protocol(request.schema)?;
                let request = match expected {
                    None => request.into_export(secret)?,
                    Some((Some(home), apps)) => {
                        request.into_validate_selected(home, apps, secret)?
                    }
                    Some((None, Some(apps))) => request.into_validate_applications(apps, secret)?,
                    Some((None, None)) => {
                        return Err(refused("Installed verifier selection empty"));
                    }
                };
                let admission = admission
                    .with_inventory_scratch(inventory, allowance)?
                    .retain_inventory_scratch_custody(self.clone())?
                    .with_installed_copy_output(output)?;
                // Strong actual run custody is stored before the final source recheck.
                runs.push(original_worker::launch(
                    self.clone(),
                    admission,
                    request,
                    service,
                )?);
                Ok(())
            })
    }
    pub fn export_and_verify(
        self: &Arc<Self>,
        progress: std::sync::mpsc::Sender<launcher::retained_view::installed_copy::CopyOwnerEvent>,
    ) -> InstalledCopyOutcome {
        let mut runs = Vec::new();
        let result = (|| {
            let expected = if let Some(recovered) = &self.recovered {
                self.validate_expectations(
                    recovered.facts.home.clone(),
                    recovered.facts.applications.clone(),
                )?
            } else {
                self.command(None, &mut runs)?;
                let exported = runs
                    .last_mut()
                    .ok_or_else(|| refused("Installed export absent"))?;
                if !exported.observe_exchange_stopped() {
                    return Err(refused("Installed export exchange live"));
                }
                let expected = self.expectations(exported.quiescent_response()?)?;
                self.attempt
                    .record_exported(&self.scratch, self, exported, &self.cancelled)?;
                if self
                    .attempt
                    .mark_verifying(&self.scratch, &self.cancelled)?
                    != self.digest
                {
                    return Err(refused("Installed verifier current read differs"));
                }
                expected
            };
            progress
                .send(
                    launcher::retained_view::installed_copy::CopyOwnerEvent::Verifying {
                        operation: self.scope.operation.clone(),
                    },
                )
                .map_err(|_| refused("Installed copy controller stopped"))?;
            self.command(Some(expected), &mut runs)?;
            for run in &mut runs {
                if !run.observe_exchange_stopped() {
                    return Err(refused("Installed verifier exchange live"));
                }
            }
            self.check_runs(&runs)?;
            Ok(())
        })();
        InstalledCopyOutcome {
            transaction: self.clone(),
            runs,
            result,
        }
    }
    fn expectations(
        &self,
        response: &worker::Response,
    ) -> io::Result<(Option<Home>, Option<Applications>)> {
        self.check()?;
        let (home, apps) = match response {
            worker::Response::ExportOriginal { home, applications } => {
                (Some(home.clone()), applications.clone())
            }
            worker::Response::ExportOriginalApplications { applications } => {
                (None, Some(applications.clone()))
            }
            _ => return Err(refused("Installed export response differs")),
        };
        self.validate_expectations(home, apps)
    }
    fn validate_expectations(
        &self,
        home: Option<Home>,
        apps: Option<Applications>,
    ) -> io::Result<(Option<Home>, Option<Applications>)> {
        self.check()?;
        self.source
            .validate_output_expectations(home.as_ref(), apps.as_ref(), &self.cancelled)?;
        let mut bytes = 0u64;
        if let Some(home) = &home {
            home.validate()?;
            if home.operation != self.scope.operation
                || home.setup_binding != self.scope.setup_binding
            {
                return Err(refused("Installed Home export protected binding differs"));
            }
            bytes = home.sealed_bytes;
        }
        if let Some(apps) = &apps {
            apps.validate()?;
            if apps.setup_binding != self.scope.setup_binding
                || home.as_ref().is_some_and(|h| h.operation == apps.operation)
            {
                return Err(refused("Installed apps protected binding differs"));
            }
            bytes = bytes
                .checked_add(apps.sealed_bytes)
                .ok_or_else(|| refused("Installed export size overflow"))?;
        }
        if bytes == 0 || bytes > self.scope.reservation {
            return Err(refused("Installed export exceeds source reservation"));
        }
        // A fresh canonical ValidateOriginal request checks the exact Home/apps
        // selection and derived application operation against the protected claim.
        Ok((home, apps))
    }
    fn check_runs(
        self: &Arc<Self>,
        runs: &[OriginalWorkerRun<Self>],
    ) -> io::Result<(Option<Home>, Option<Applications>)> {
        self.check()?;
        let verify = if self.recovered.is_some() {
            let [verify] = runs else {
                return Err(refused("Installed recovery verifier sequence differs"));
            };
            verify
        } else {
            let [_, verify] = runs else {
                return Err(refused("Installed verifier sequence differs"));
            };
            verify
        };
        for run in runs {
            if !run.belongs_to(self)
                || !run.supervised.shutdown.as_ref().is_some_and(|proof| {
                    run.working_path()
                        .is_ok_and(|path| proof.matches(&run.key, path, PACKAGE))
                })
            {
                return Err(refused(
                    "Installed original namespace retirement unobserved",
                ));
            }
            run.quiescent_response()?;
        }
        if !matches!(verify.quiescent_response()?, worker::Response::Validated) {
            return Err(refused("Installed whole stream was not verified"));
        }
        self.attempt.require_namespaces_retired()?;
        if let Some(recovered) = &self.recovered {
            self.validate_expectations(
                recovered.facts.home.clone(),
                recovered.facts.applications.clone(),
            )
        } else {
            self.expectations(runs[0].quiescent_response()?)
        }
    }
}
impl ServiceAuthority for InstalledCopyTransaction {
    fn check(&self) -> io::Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        check_cancel(&self.cancelled)?;
        self.owner.check()?;
        self.source.revalidate(&self.cancelled)?;
        if self.source.output_attempt_scope(&self.cancelled)? != self.scope
            || self.source.output_read_claim_digest(&self.cancelled)? != self.digest
        {
            return Err(refused("Installed original protected scope changed"));
        }
        self.attempt.check(&self.cancelled)?;
        self.scratch.check(&self.cancelled)
    }
}
impl OriginalWorkerOwner for InstalledCopyTransaction {
    fn operation(&self) -> &str {
        &self.scope.operation
    }
    fn read_claim_digest(&self) -> [u8; 32] {
        self.digest
    }
    fn cancellation(&self) -> &ServiceCancellation {
        &self.cancellation
    }
    fn record_namespace(&self, endpoint: &ServiceEndpoint) -> io::Result<()> {
        self.attempt
            .record_namespace(&self.scratch, endpoint, &self.cancelled)
    }
}
impl InstalledCopyOutputCustody for InstalledCopyTransaction {
    fn binding(&self) -> io::Result<InstalledCopyOutputBinding> {
        self.check()?;
        let facts = self.scratch.facts(&self.cancelled)?;
        let source = self
            .source
            .output_original_home(&self.cancelled)?
            .metadata()?;
        let filesystem = facts.filesystem;
        let filesystem_uuid =
            uuid::Uuid::parse_str(&filesystem.volume.filesystem_uuid).map_err(io::Error::other)?;
        Ok(InstalledCopyOutputBinding {
            operation: self.scope.operation.clone(),
            attempt: self.attempt.attempt().into(),
            read_claim_digest: self.digest,
            setup_digest: self.scope.declaration,
            person: self.scope.person,
            source: (source.dev(), source.ino()),
            output: facts.output,
            private_parent: facts.stage,
            original_uid: self.scope.caller.0,
            target_uid: self.attempt.target().uid(),
            reserve_bytes: self.scope.reservation,
            output_mount_id: filesystem.mount_id,
            namespace: filesystem.namespace,
            partition_rdev: filesystem.partition_rdev,
            disk_rdev: filesystem.disk_rdev,
            filesystem_uuid: *filesystem_uuid.as_bytes(),
        })
    }
    fn files(&self) -> io::Result<InstalledCopyOutputFiles> {
        self.check()?;
        let (output, _, _) = self.scratch.files(&self.cancelled)?;
        let stage = self.scratch.private_stage(&self.cancelled)?;
        let namespace = File::open("/proc/self/ns/mnt")?;
        let value = InstalledCopyOutputFiles {
            source: self.source.output_original_home(&self.cancelled)?,
            output,
            private_parent: migration_durability::readable_directory(&stage.file)?,
            namespace,
        };
        self.check()?;
        Ok(value)
    }
    fn target_peer(&self) -> io::Result<Arc<ExecutingPeer>> {
        self.owner.target_peer(&self.cancelled)
    }
    fn additional_output_reservation(&self) -> io::Result<u64> {
        self.check()?;
        self.attempt.additional_output_reservation(&self.cancelled)
    }
    fn check_physical_and_operation_custody(&self) -> io::Result<()> {
        self.check()
    }
}
pub struct InstalledCopyOutcome {
    transaction: Arc<InstalledCopyTransaction>,
    runs: Vec<OriginalWorkerRun<InstalledCopyTransaction>>,
    result: io::Result<()>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::setup::home_capture) struct InstalledVerifiedFileFacts {
    pub(super) name: String,
    pub(super) dev: u64,
    pub(super) ino: u64,
    pub(super) len: u64,
    pub(super) ciphertext_digest: [u8; 32],
}
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::setup::home_capture) struct InstalledVerifiedArtifactFacts {
    pub(super) home: Option<Home>,
    pub(super) applications: Option<Applications>,
    pub(super) files: Vec<InstalledVerifiedFileFacts>,
    pub(super) expectations_digest: [u8; 32],
}
// Constructor remains private to this module; no path, DTO, phase or expectation mints this token.
pub struct InstalledVerifiedArtifacts {
    outcome: InstalledCopyOutcome,
    directory: Arc<PinnedDirectory>,
    files: Vec<(File, InstalledVerifiedFileFacts)>,
    stamps: Vec<[i64; 4]>,
    home: Option<Home>,
    applications: Option<Applications>,
}
impl InstalledVerifiedArtifacts {
    pub(in crate::setup::home_capture) fn journal_facts(
        &self,
        attempt: &InstalledCopyAttempt,
        scratch: &InstalledCopyScratch,
        cancel: &AtomicBool,
    ) -> io::Result<InstalledVerifiedArtifactFacts> {
        check_cancel(cancel)?;
        let tx = &self.outcome.transaction;
        if !std::ptr::eq(attempt, tx.attempt.as_ref())
            || !std::ptr::eq(scratch, tx.scratch.as_ref())
        {
            return Err(refused("Installed verified artifact allocation differs"));
        }
        self.outcome
            .result
            .as_ref()
            .map_err(|_| refused("Installed export failed"))?;
        let expected = tx.check_runs(&self.outcome.runs)?;
        if serde_json::to_vec(&expected)?
            != serde_json::to_vec(&(self.home.clone(), self.applications.clone()))?
        {
            return Err(refused("Installed verified expectations substituted"));
        }
        self.directory.validate_destination()?;
        let m = self.directory.file.metadata()?;
        if m.uid() != 0 || m.gid() != 0 || m.mode() & 0o7777 != 0o700 {
            return Err(refused("Installed artifact parent changed"));
        }
        let mut facts = Vec::new();
        if self.files.len() != self.stamps.len() {
            return Err(refused("Installed sealed stamp count differs"));
        }
        for ((file, expected), stamp) in self.files.iter().zip(&self.stamps) {
            let metadata = file.metadata()?;
            if [
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ] != *stamp
            {
                return Err(refused("Installed sealed archive stamp changed"));
            }
            let leaf = expected
                .name
                .strip_prefix("artifacts/")
                .ok_or_else(|| refused("Installed artifact name differs"))?;
            let actual = measure(file, leaf, &self.directory.file, 0, "artifacts", cancel)?;
            if &actual != expected {
                return Err(refused("Installed verified artifact changed"));
            }
            facts.push(actual);
        }
        Ok(InstalledVerifiedArtifactFacts {
            home: self.home.clone(),
            applications: self.applications.clone(),
            files: facts,
            expectations_digest: Sha256::digest(serde_json::to_vec(&expected)?).into(),
        })
    }
}
pub(in crate::setup::home_capture) fn measure(
    file: &File,
    leaf: &str,
    directory: &File,
    uid: u32,
    prefix: &str,
    cancel: &AtomicBool,
) -> io::Result<InstalledVerifiedFileFacts> {
    let before = file.metadata()?;
    let access = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if access < 0
        || access & (libc::O_ACCMODE | libc::O_PATH) != libc::O_RDONLY
        || !before.is_file()
        || before.uid() != uid
        || (uid == 0 && before.gid() != 0)
        || before.mode() & 0o7777 != 0o600
        || before.nlink() != 1
    {
        return Err(refused(
            "Installed archive is not root retained readonly data",
        ));
    }
    let named = crate::setup::nofollow::metadata_nofollow(directory, Path::new(leaf))?;
    if (
        cap_fs_ext::MetadataExt::dev(&named),
        cap_fs_ext::MetadataExt::ino(&named),
    ) != (before.dev(), before.ino())
    {
        return Err(refused("Installed artifact name changed"));
    }
    use std::os::unix::fs::FileExt;
    let mut buffer = [0u8; 64 * 1024];
    let mut offset = 0u64;
    let mut hash = Sha256::new();
    while offset < before.len() {
        check_cancel(cancel)?;
        let limit = usize::try_from((before.len() - offset).min(buffer.len() as u64))
            .map_err(io::Error::other)?;
        let count = file.read_at(&mut buffer[..limit], offset)?;
        if count == 0 {
            return Err(refused("Installed archive truncated"));
        }
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    let after = file.metadata()?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) {
        return Err(refused("Installed archive changed during verification"));
    }
    let named_after = crate::setup::nofollow::metadata_nofollow(directory, Path::new(leaf))?;
    if (
        cap_fs_ext::MetadataExt::dev(&named_after),
        cap_fs_ext::MetadataExt::ino(&named_after),
    ) != (after.dev(), after.ino())
    {
        return Err(refused("Installed archive name changed during hashing"));
    }
    Ok(InstalledVerifiedFileFacts {
        name: format!("{prefix}/{leaf}"),
        dev: before.dev(),
        ino: before.ino(),
        len: before.len(),
        ciphertext_digest: hash.finalize().into(),
    })
}

/// Only the actual paired signed run can construct this intermediate token.
/// Sealing intent is persisted from its measured native files before any rename.
pub struct InstalledVerifiedExport {
    outcome: InstalledCopyOutcome,
    output: File,
    files: Vec<(File, InstalledVerifiedFileFacts)>,
    home: Option<Home>,
    applications: Option<Applications>,
}
fn open_leaf(directory: &File, leaf: &str) -> io::Result<File> {
    use cap_fs_ext::OpenOptionsFollowExt;
    let mut options = cap_primitives::fs::OpenOptions::new();
    options
        .read(true)
        .follow(cap_primitives::fs::FollowSymlinks::No);
    cap_primitives::fs::open(directory, Path::new(leaf), &options)
}
impl InstalledCopyOutcome {
    pub fn verified_export(self) -> io::Result<InstalledVerifiedExport> {
        self.result
            .as_ref()
            .map_err(|_| refused("Installed signed export/verifier failed"))?;
        let (home, applications) = self.transaction.check_runs(&self.runs)?;
        let (output, _, _) = self
            .transaction
            .scratch
            .files(&self.transaction.cancelled)?;
        let mut files = Vec::new();
        for (leaf, length, digest) in home
            .iter()
            .map(|h| (&h.filename, h.sealed_bytes, h.ciphertext_sha256))
            .chain(
                applications
                    .iter()
                    .map(|a| (&a.filename, a.sealed_bytes, a.ciphertext_sha256)),
            )
        {
            let file = open_leaf(&output, leaf)?;
            let facts = measure(
                &file,
                leaf,
                &output,
                self.transaction.scope.caller.0,
                "output",
                &self.transaction.cancelled,
            )?;
            if facts.len != length || facts.ciphertext_digest != digest {
                return Err(refused(
                    "Installed actual verified output differs from complete stream",
                ));
            }
            if let Some(recovered) = &self.transaction.recovered {
                let prior = recovered
                    .facts
                    .files
                    .iter()
                    .find(|prior| prior.name == facts.name)
                    .ok_or_else(|| refused("Installed recovery verified file was not declared"))?;
                if prior != &facts {
                    return Err(refused(
                        "Installed recovery verifier substituted native ciphertext",
                    ));
                }
            }
            files.push((file, facts));
        }
        let value = InstalledVerifiedExport {
            outcome: self,
            output,
            files,
            home,
            applications,
        };
        value.journal_facts(
            value.outcome.transaction.attempt(),
            value.outcome.transaction.scratch(),
            &value.outcome.transaction.cancelled,
        )?;
        Ok(value)
    }
}
impl InstalledVerifiedExport {
    pub(in crate::setup::home_capture) fn journal_facts(
        &self,
        attempt: &InstalledCopyAttempt,
        scratch: &InstalledCopyScratch,
        cancel: &AtomicBool,
    ) -> io::Result<InstalledVerifiedArtifactFacts> {
        check_cancel(cancel)?;
        let tx = &self.outcome.transaction;
        if !std::ptr::eq(attempt, tx.attempt.as_ref())
            || !std::ptr::eq(scratch, tx.scratch.as_ref())
        {
            return Err(refused("Installed verifier token allocation differs"));
        }
        self.outcome
            .result
            .as_ref()
            .map_err(|_| refused("Installed signed verification failed"))?;
        let expected = tx.check_runs(&self.outcome.runs)?;
        if serde_json::to_vec(&expected)?
            != serde_json::to_vec(&(self.home.clone(), self.applications.clone()))?
        {
            return Err(refused("Installed verified selection differs"));
        }
        let (current, _, _) = scratch.files(cancel)?;
        let current = current.metadata()?;
        let held = self.output.metadata()?;
        if (current.dev(), current.ino()) != (held.dev(), held.ino()) {
            return Err(refused("Installed verified output directory substituted"));
        }
        let mut files = Vec::new();
        for (file, expected) in &self.files {
            let leaf = expected
                .name
                .strip_prefix("output/")
                .ok_or_else(|| refused("Installed source artifact name differs"))?;
            let actual = measure(
                file,
                leaf,
                &self.output,
                tx.scope.caller.0,
                "output",
                cancel,
            )?;
            if &actual != expected {
                return Err(refused("Installed output changed after full verifier"));
            }
            files.push(actual);
        }
        Ok(InstalledVerifiedArtifactFacts {
            home: self.home.clone(),
            applications: self.applications.clone(),
            files,
            expectations_digest: Sha256::digest(serde_json::to_vec(&expected)?).into(),
        })
    }
    pub fn seal(self) -> io::Result<InstalledVerifiedArtifacts> {
        let tx = &self.outcome.transaction;
        self.journal_facts(&tx.attempt, &tx.scratch, &tx.cancelled)?;
        tx.attempt.mark_sealing(&self, &tx.scratch, &tx.cancelled)?;
        let stage = tx.scratch.private_stage(&tx.cancelled)?;
        let directory = if let Some(existing) = tx
            .recovered
            .as_ref()
            .and_then(|recovered| recovered.sealed_parent.as_ref())
        {
            tx.check()?;
            existing.validate_destination()?;
            let metadata =
                crate::setup::nofollow::metadata_nofollow(&stage.file, Path::new("artifacts"))?;
            let held = existing.file.metadata()?;
            if (
                cap_fs_ext::MetadataExt::dev(&metadata),
                cap_fs_ext::MetadataExt::ino(&metadata),
            ) != (held.dev(), held.ino())
                || held.uid() != 0
                || held.gid() != 0
                || held.mode() & 0o7777 != 0o700
            {
                return Err(refused("Installed recovery sealed parent substituted"));
            }
            let parent = cap_std::fs::Dir::from_std_file(existing.file.try_clone()?);
            if parent.entries()?.next().transpose()?.is_some() {
                return Err(refused("Installed recovery sealed parent contains files"));
            }
            tx.check()?;
            existing.clone()
        } else {
            stage.create_child(std::ffi::OsStr::new("artifacts"))?
        };
        let mut files = Vec::new();
        for (held, original) in &self.files {
            tx.check()?;
            let leaf = original
                .name
                .strip_prefix("output/")
                .ok_or_else(|| refused("Installed output filename differs"))?;
            let before = measure(
                held,
                leaf,
                &self.output,
                tx.scope.caller.0,
                "output",
                &tx.cancelled,
            )?;
            if &before != original {
                return Err(refused("Installed output changed before sealing"));
            }
            let name = std::ffi::CString::new(leaf).map_err(io::Error::other)?;
            // Both dirs are held and the destination is a fresh root-private child.
            // No replacement, copying of unbounded bytes, or path reopening occurs.
            if unsafe {
                libc::renameat2(
                    self.output.as_raw_fd(),
                    name.as_ptr(),
                    directory.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::fchown(held.as_raw_fd(), 0, 0) } < 0
                || unsafe { libc::fchmod(held.as_raw_fd(), 0o600) } < 0
            {
                return Err(io::Error::last_os_error());
            }
            held.sync_all()?;
            migration_durability::sync_directory(&self.output)?;
            migration_durability::sync_directory(&directory.file)?;
            let file = open_leaf(&directory.file, leaf)?;
            let measured = measure(&file, leaf, &directory.file, 0, "artifacts", &tx.cancelled)?;
            if (
                measured.dev,
                measured.ino,
                measured.len,
                measured.ciphertext_digest,
            ) != (
                original.dev,
                original.ino,
                original.len,
                original.ciphertext_digest,
            ) {
                return Err(refused(
                    "Installed sealed artifact differs from verified inode",
                ));
            }
            files.push((file, measured));
        }
        migration_durability::sync_directory(&stage.file)?;
        let stamps = files
            .iter()
            .map(|(file, _)| {
                file.metadata()
                    .map(|m| [m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec()])
            })
            .collect::<io::Result<Vec<_>>>()?;
        let value = InstalledVerifiedArtifacts {
            outcome: self.outcome,
            directory,
            files,
            stamps,
            home: self.home,
            applications: self.applications,
        };
        let tx = &value.outcome.transaction;
        value.journal_facts(&tx.attempt, &tx.scratch, &tx.cancelled)?;
        tx.attempt
            .mark_receiver_pending(&value, &tx.scratch, &tx.cancelled)?;
        Ok(value)
    }
}

impl InstalledVerifiedArtifacts {
    pub fn transaction(&self) -> Arc<InstalledCopyTransaction> {
        self.outcome.transaction.clone()
    }
    pub fn expectations(&self) -> (Option<Home>, Option<Applications>) {
        (self.home.clone(), self.applications.clone())
    }
    pub fn declaration(
        &self,
    ) -> io::Result<launcher::retained_view::installed_copy::CopyDeclaration> {
        self.check()?;
        let tx = &self.outcome.transaction;
        let payload = crate::setup::installed_copy::CopyPayload {
            attempt: tx.attempt.attempt().into(),
            home: self.home.clone(),
            applications: self.applications.clone(),
            applications_selected: self.applications.is_some(),
        };
        Ok(launcher::retained_view::installed_copy::CopyDeclaration {
            operation: tx.scope.operation.clone(),
            setup_binding: tx.scope.setup_binding.clone(),
            person: tx.scope.person,
            read_claim: tx.digest,
            target_uid: tx.attempt.target().uid(),
            payload: serde_json::to_vec(&payload)?,
        })
    }
    pub fn prepare_issue(
        self: &Arc<Self>,
        channel: launcher::retained_view::RootConsumerChannel,
        recipient: launcher::retained_view::installed_copy::CopyRecipient,
        cancellation: ServiceCancellation,
    ) -> io::Result<launcher::retained_view::installed_copy::InstalledCopyIssue> {
        self.check()?;
        let locate = |leaf: Option<&str>| -> io::Result<Option<File>> {
            let Some(leaf) = leaf else {
                return Ok(None);
            };
            let name = format!("artifacts/{leaf}");
            let (file, _) = self
                .files
                .iter()
                .find(|(_, facts)| facts.name == name)
                .ok_or_else(|| refused("Installed sealed file custody missing"))?;
            Ok(Some(file.try_clone()?))
        };
        let files = (
            locate(self.home.as_ref().map(|h| h.filename.as_str()))?,
            locate(self.applications.as_ref().map(|a| a.filename.as_str()))?,
        );
        let tx = &self.outcome.transaction;
        let secret = tx.attempt.secret(&tx.cancelled)?;
        let secret =
            launcher::retained_view::RootConsumerChannel::seal_installed_copy_secret(&secret)?;
        channel.prepare_installed_copy(
            self.declaration()?,
            recipient,
            files,
            secret,
            self.clone(),
            cancellation,
        )
    }
}
impl ServiceAuthority for InstalledVerifiedArtifacts {
    fn check(&self) -> io::Result<()> {
        let tx = &self.outcome.transaction;
        tx.check()?;
        self.directory.validate_destination()?;
        let parent = self.directory.file.metadata()?;
        if parent.uid() != 0 || parent.gid() != 0 || parent.mode() & 0o7777 != 0o700 {
            return Err(refused("Installed sealed artifact custody changed"));
        }
        // Actual crypto verification is performed when minting the token and
        // recording the transition. This private root directory and O_RDONLY
        // descriptors prevent receiver writes; the gate independently pins
        // mtime/ctime for every delivered inode on every operation.
        if self.files.len() != self.stamps.len() {
            return Err(refused("Installed sealed stamp count differs"));
        }
        for ((file, expected), stamp) in self.files.iter().zip(&self.stamps) {
            let metadata = file.metadata()?;
            if [
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ] != *stamp
            {
                return Err(refused("Installed sealed archive stamp changed"));
            }
            let leaf = expected
                .name
                .strip_prefix("artifacts/")
                .ok_or_else(|| refused("Installed sealed name invalid"))?;
            let named =
                crate::setup::nofollow::metadata_nofollow(&self.directory.file, Path::new(leaf))?;
            let held = file.metadata()?;
            if !held.is_file()
                || held.uid() != 0
                || held.gid() != 0
                || held.mode() & 0o7777 != 0o600
                || held.nlink() != 1
                || (held.dev(), held.ino(), held.len())
                    != (expected.dev, expected.ino, expected.len)
                || (
                    cap_fs_ext::MetadataExt::dev(&named),
                    cap_fs_ext::MetadataExt::ino(&named),
                ) != (expected.dev, expected.ino)
            {
                return Err(refused("Installed sealed archive identity changed"));
            }
        }
        tx.check()
    }
}

/// Authenticated Complete plus observed death of that same owned importer.
/// The constructor consumes the private controller observation, never a DTO.
pub struct InstalledCopyCompletion {
    cleanup: Mutex<Option<Arc<super::store::CompletedCleanup>>>,
    received: crate::setup::retained_consumer_driver::original_copy::ActualCopyReceiveComplete,
}
pub(in crate::setup::home_capture) struct InstalledCopyCompletionFacts {
    pub(super) receiver_pid: u32,
    pub(super) receiver_start: u64,
    pub(super) expectations_digest: [u8; 32],
    pub(super) home_sha256: Option<[u8; 32]>,
    pub(super) applications_sha256: Option<[u8; 32]>,
}
impl InstalledCopyCompletion {
    pub fn observed(
        received: crate::setup::retained_consumer_driver::original_copy::ActualCopyReceiveComplete,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let value = Self {
            received,
            cleanup: Mutex::new(None),
        };
        let (artifacts, _, _, _) = value.received.evidence();
        let tx = artifacts.transaction();
        value.journal_facts(&tx.attempt, cancel)?;
        Ok(value)
    }
    pub(in crate::setup::home_capture) fn completed_artifact_directory(
        &self,
    ) -> Arc<PinnedDirectory> {
        self.received.evidence().0.directory.clone()
    }
    pub(in crate::setup::home_capture) fn retirement_peer_stopped(
        &self,
        attempt: &InstalledCopyAttempt,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_cancel(cancel)?;
        let (artifacts, importer, _, _) = self.received.evidence();
        if !std::ptr::eq(attempt, artifacts.transaction().attempt.as_ref())
            || !Arc::ptr_eq(importer, &attempt.importer()?)
        {
            return Err(refused(
                "Installed cleanup actual completion allocation differs",
            ));
        }
        let mut stopped = libc::pollfd {
            fd: importer.pidfd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let count = unsafe { libc::poll(&raw mut stopped, 1, 0) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count != 1
            || stopped.revents & libc::POLLIN == 0
            || stopped.revents & (libc::POLLERR | libc::POLLNVAL) != 0
        {
            return Err(refused("Installed cleanup importer still live"));
        }
        Ok(())
    }
    /// Actual private authenticated completion evidence remains authority after
    /// terminal journal publication; a serialized phase cannot construct Self.
    pub(in crate::setup::home_capture) fn retirement_facts(
        &self,
        attempt: &InstalledCopyAttempt,
        cancel: &AtomicBool,
    ) -> io::Result<InstalledCopyCompletionFacts> {
        check_cancel(cancel)?;
        let (artifacts, importer, home, applications) = self.received.evidence();
        let tx = artifacts.transaction();
        if !std::ptr::eq(attempt, tx.attempt.as_ref())
            || !Arc::ptr_eq(importer, &attempt.importer()?)
        {
            return Err(refused(
                "Installed retirement completion allocation differs",
            ));
        }
        if home != artifacts.home.as_ref().map(|h| h.ciphertext_sha256)
            || applications != artifacts.applications.as_ref().map(|a| a.ciphertext_sha256)
        {
            return Err(refused("Installed retirement completion digest differs"));
        }
        let mut stopped = libc::pollfd {
            fd: importer.pidfd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let count = unsafe { libc::poll(&raw mut stopped, 1, 0) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count != 1
            || stopped.revents & libc::POLLIN == 0
            || stopped.revents & (libc::POLLERR | libc::POLLNVAL) != 0
        {
            return Err(refused("Installed retirement importer still live"));
        }
        Ok(InstalledCopyCompletionFacts {
            receiver_pid: importer.pid(),
            receiver_start: attempt.importer_start()?,
            expectations_digest: Sha256::digest(serde_json::to_vec(&(
                artifacts.home.clone(),
                artifacts.applications.clone(),
            ))?)
            .into(),
            home_sha256: home,
            applications_sha256: applications,
        })
    }
    /// Explicit exact-material cleanup, only after durable actual completion.
    pub fn retire_material(
        self: &Arc<Self>,
        attempt: &InstalledCopyAttempt,
        cancel: &AtomicBool,
    ) -> io::Result<bool> {
        let (artifacts, _, _, _) = self.received.evidence();
        let mut held = self
            .cleanup
            .lock()
            .map_err(|_| refused("Installed completion cleanup custody poisoned"))?;
        if held.is_none() {
            let tx = artifacts.transaction();
            if !std::ptr::eq(attempt, tx.attempt.as_ref()) {
                return Err(refused("Installed cleanup transaction differs"));
            }
            *held = Some(Arc::new(
                tx.attempt.begin_completed_cleanup(self.clone(), cancel)?,
            ));
        }
        let permit = held
            .as_ref()
            .ok_or_else(|| refused("Installed cleanup permit absent"))?
            .clone();
        for (file, expected) in &artifacts.files {
            permit.remove_artifact(file, expected, cancel)?;
        }
        permit.remove_empty_directories(cancel)?;
        permit.remove_secret_and_finish(cancel)?;
        let metadata_quarantined = permit.metadata_quarantined();
        *held = None; // Break intentional custody cycle only after exact retirement.
        Ok(metadata_quarantined)
    }
    pub(in crate::setup::home_capture) fn journal_facts(
        &self,
        attempt: &InstalledCopyAttempt,
        cancel: &AtomicBool,
    ) -> io::Result<InstalledCopyCompletionFacts> {
        check_cancel(cancel)?;
        let (artifacts, importer, home, applications) = self.received.evidence();
        let tx = artifacts.transaction();
        if !std::ptr::eq(attempt, tx.attempt.as_ref()) {
            return Err(refused("Installed Copy completion allocation differs"));
        }
        let registered = attempt.importer()?;
        if !Arc::ptr_eq(importer, &registered) {
            return Err(refused("Installed Copy completion importer differs"));
        }
        let facts = artifacts.journal_facts(attempt, &tx.scratch, cancel)?;
        if home != facts.home.as_ref().map(|h| h.ciphertext_sha256)
            || applications != facts.applications.as_ref().map(|a| a.ciphertext_sha256)
        {
            return Err(refused("Installed Copy completion digest differs"));
        }
        let mut stopped = libc::pollfd {
            fd: importer.pidfd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&raw mut stopped, 1, 0) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if stopped.revents & (libc::POLLERR | libc::POLLNVAL) != 0
            || stopped.revents & libc::POLLIN == 0
        {
            return Err(refused("Installed Copy importer retirement unobserved"));
        }
        check_cancel(cancel)?;
        Ok(InstalledCopyCompletionFacts {
            receiver_pid: importer.pid(),
            receiver_start: attempt.importer_start()?,
            expectations_digest: facts.expectations_digest,
            home_sha256: home,
            applications_sha256: applications,
        })
    }
}
