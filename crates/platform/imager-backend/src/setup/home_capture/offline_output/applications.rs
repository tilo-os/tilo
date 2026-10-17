//! Applications-only original export: no Home archive is manufactured.
use super::{runner::OriginalWorkerRun, *};
use crate::setup::{
    flatpak_transfer::portable::Expectation,
    home_capture::{runtime::TrustedCaptureService, worker},
    migration::PinnedDirectory,
    prepared_setup::{OfflineApplicationsPlan, PreparedSetupReview},
};
use std::{ffi::OsStr, io::Read};
use zeroize::Zeroizing;

pub struct OriginalApplicationsOutcome {
    pub(super) runs: Vec<OriginalWorkerRun>,
    pub result: io::Result<Expectation>,
}
impl OriginalApplicationsOutcome {
    pub fn cleanup_receipt(
        &self,
    ) -> io::Result<(
        &launcher::owned_service::ConfirmedShutdown,
        &launcher::owned_service::ServiceLeaseKey,
        &Path,
    )> {
        if self.runs.is_empty()
            || self
                .runs
                .iter()
                .any(|run| run.supervised.shutdown.is_none())
        {
            return Err(refused("Applications worker shutdown remains unconfirmed"));
        }
        let run = self
            .runs
            .last()
            .ok_or_else(|| refused("Applications cleanup custody missing"))?;
        let shutdown = run
            .supervised
            .shutdown
            .as_ref()
            .ok_or_else(|| refused("Applications shutdown missing"))?;
        Ok((shutdown, &run.key, run.working_path()?))
    }
}
pub struct VerifiedOriginalApplications {
    transaction: Arc<OfflineOutputTransaction>,
    expected: Expectation,
    declaration: OfflineApplicationsPlan,
}
pub struct CommittedOriginalApplicationsSelection {
    declaration: OfflineApplicationsPlan,
    attempt: String,
    pending: crate::imager_model::PreparedSetupExpectation,
    completed: crate::imager_model::PreparedSetupExpectation,
    applications: Expectation,
    applications_inode: u64,
    completed_inode: u64,
    marker_sha256: [u8; 32],
}
impl CommittedOriginalApplicationsSelection {
    pub fn declaration(&self) -> &OfflineApplicationsPlan {
        &self.declaration
    }
    pub fn attempt(&self) -> &str {
        &self.attempt
    }
    pub fn pending_setup(&self) -> &crate::imager_model::PreparedSetupExpectation {
        &self.pending
    }
    pub fn completed_setup(&self) -> &crate::imager_model::PreparedSetupExpectation {
        &self.completed
    }
    pub fn applications(&self) -> &Expectation {
        &self.applications
    }
    pub fn applications_inode(&self) -> u64 {
        self.applications_inode
    }
    pub fn completed_inode(&self) -> u64 {
        self.completed_inode
    }
    pub fn marker_sha256(&self) -> [u8; 32] {
        self.marker_sha256
    }
}
impl OfflineOutputTransaction {
    pub fn export_applications_and_verify(
        self: &Arc<Self>,
        password: Zeroizing<Vec<u8>>,
    ) -> OriginalApplicationsOutcome {
        let mut runs = Vec::new();
        let result = (|| {
            self.check()?;
            // Purpose is authenticated before any durable launch or worker.
            let _ = self.read.output_applications_declaration(&self.cancelled)?;
            self.store
                .lock()
                .map_err(|_| refused("Applications Store lock failed"))?
                .mark_original_output_launching(
                    &self.binding.attempt,
                    &self.medium.files()?.output,
                )?;
            self.applications_command(Zeroizing::new(password.to_vec()), None, &mut runs)?;
            let exported = runs
                .last()
                .ok_or_else(|| refused("Applications export runner missing"))?;
            let expected = match exported.quiescent_response()? {
                worker::Response::ExportOriginalApplications { applications } => {
                    applications.clone()
                }
                _ => return Err(refused("Applications export response differs")),
            };
            self.validate_applications_expectation(&expected)?;
            let exit = exported
                .supervised
                .result
                .as_ref()
                .map_err(|_| refused("Applications export quiescence missing"))?;
            self.check()?;
            {
                let store = self
                    .store
                    .lock()
                    .map_err(|_| refused("Applications Store lock failed"))?;
                let output = self.medium.files()?.output;
                store.mark_original_applications_exported(
                    &self.binding.attempt,
                    &output,
                    &expected,
                    self.binding.reserve_bytes,
                    exit,
                    &exported.key,
                    exported.working_path()?,
                )?;
                store.mark_original_applications_verifying(
                    &self.binding.attempt,
                    &output,
                    &expected,
                    self.binding.reserve_bytes,
                )?;
            }
            self.applications_command(password, Some(expected.clone()), &mut runs)?;
            let verified = runs
                .last()
                .ok_or_else(|| refused("Applications verifier runner missing"))?;
            if !matches!(verified.quiescent_response()?, worker::Response::Validated) {
                return Err(refused("Applications verifier response differs"));
            }
            let exit = verified
                .supervised
                .result
                .as_ref()
                .map_err(|_| refused("Applications verifier quiescence missing"))?;
            self.check()?;
            self.store
                .lock()
                .map_err(|_| refused("Applications Store lock failed"))?
                .mark_original_applications_verified(
                    &self.binding.attempt,
                    &self.medium.files()?.output,
                    &expected,
                    self.binding.reserve_bytes,
                    exit,
                    &verified.key,
                    verified.working_path()?,
                )?;
            self.medium.publish_verified_applications_receipt(
                &expected,
                exit,
                &verified.key,
                verified.working_path()?,
            )?;
            self.check()?;
            Ok(expected)
        })();
        OriginalApplicationsOutcome { runs, result }
    }
    fn validate_applications_expectation(&self, expected: &Expectation) -> io::Result<()> {
        self.check()?;
        expected.validate()?;
        let declaration = self.read.output_applications_declaration(&self.cancelled)?;
        if self
            .read
            .output_flatpak_operation(&self.cancelled)?
            .as_deref()
            != Some(expected.operation.as_str())
            || expected.setup_binding != declaration.setup_binding()
            || expected.sealed_bytes > self.binding.reserve_bytes
            || !self.read.output_flatpak_required(&self.cancelled)?
        {
            return Err(refused("Applications export binding or budget differs"));
        }
        Ok(())
    }
    fn applications_command(
        self: &Arc<Self>,
        password: Zeroizing<Vec<u8>>,
        expected: Option<Expectation>,
        runs: &mut Vec<OriginalWorkerRun>,
    ) -> io::Result<()> {
        self.check()?;
        let _ = self.read.output_applications_declaration(&self.cancelled)?;
        let service = TrustedCaptureService::native_for_protocol(worker::APPLICATIONS_PROTOCOL)?;
        let runtime = service.root.clone();
        let publisher = service.publisher.clone();
        let proof = OriginalOutputAdmission::admit(self.clone())?;
        self.with_inventory_reader(&runtime, &publisher, |admission, request, digest| {
            if digest != self.binding.read_claim_digest {
                return Err(refused("Applications command read binding differs"));
            }
            let request = match expected {
                Some(expected) => request.into_validate_applications(expected, password)?,
                None => request.into_export(password)?,
            };
            let run =
                self.launch_request(admission.with_original_output(proof)?, request, service)?;
            // Retain real shutdown custody before source permission recheck.
            runs.push(run);
            Ok(())
        })
    }
    pub fn validate_verified_applications_outcome(
        self: &Arc<Self>,
        outcome: &OriginalApplicationsOutcome,
    ) -> io::Result<VerifiedOriginalApplications> {
        self.check()?;
        let [exported, verified] = outcome.runs.as_slice() else {
            return Err(refused("Applications verifier sequence differs"));
        };
        if !exported.belongs_to(self) || !verified.belongs_to(self) {
            return Err(refused(
                "Applications outcome belongs to another transaction",
            ));
        }
        let expected = match exported.quiescent_response()? {
            worker::Response::ExportOriginalApplications { applications } => applications.clone(),
            _ => return Err(refused("Applications export response differs")),
        };
        if !matches!(verified.quiescent_response()?, worker::Response::Validated) {
            return Err(refused("Applications complete stream was not verified"));
        }
        outcome.cleanup_receipt()?;
        for run in &outcome.runs {
            if !run.supervised.shutdown.as_ref().is_some_and(|shutdown| {
                run.working_path().is_ok_and(|work| {
                    shutdown.matches(&run.key, work, super::super::runtime::PACKAGE)
                })
            }) {
                return Err(refused("Applications verifier shutdown differs"));
            }
        }
        if outcome
            .result
            .as_ref()
            .map_err(|_| refused("Applications export failed"))?
            != &expected
        {
            return Err(refused("Applications verified expectation differs"));
        }
        let token = VerifiedOriginalApplications {
            transaction: self.clone(),
            expected,
            declaration: self.read.output_applications_declaration(&self.cancelled)?,
        };
        token.validate()?;
        Ok(token)
    }
}
impl VerifiedOriginalApplications {
    pub fn expected(&self) -> &Expectation {
        &self.expected
    }
    pub fn declaration(&self) -> &OfflineApplicationsPlan {
        &self.declaration
    }
    pub fn attempt(&self) -> &str {
        &self.transaction.binding.attempt
    }
    pub fn pending_setup_expectation(
        &self,
    ) -> io::Result<&crate::imager_model::PreparedSetupExpectation> {
        self.validate()?;
        self.transaction.read.pending_setup_expectation()
    }
    pub fn validate_setup(&self, setup: &PreparedSetupReview) -> io::Result<()> {
        self.validate()?;
        if !std::ptr::eq(self.transaction.read.authenticated_setup().as_ref(), setup) {
            return Err(refused(
                "Applications belong to another authenticated setup",
            ));
        }
        Ok(())
    }
    pub fn retain(&self) -> io::Result<Self> {
        self.validate()?;
        Ok(Self {
            transaction: self.transaction.clone(),
            expected: self.expected.clone(),
            declaration: self.declaration.clone(),
        })
    }
    pub fn validate_same_origin(&self, other: &Self) -> io::Result<()> {
        self.validate()?;
        other.validate()?;
        if !Arc::ptr_eq(&self.transaction, &other.transaction)
            || !Arc::ptr_eq(
                self.transaction.read.authenticated_setup(),
                other.transaction.read.authenticated_setup(),
            )
            || self.expected != other.expected
            || self.declaration != other.declaration
            || self.pending_setup_expectation()? != other.pending_setup_expectation()?
        {
            return Err(refused(
                "Applications setup package belongs to another transaction",
            ));
        }
        Ok(())
    }
    pub fn validate(&self) -> io::Result<()> {
        self.transaction
            .validate_applications_expectation(&self.expected)?;
        if self
            .transaction
            .read
            .output_applications_declaration(&self.transaction.cancelled)?
            != self.declaration
        {
            return Err(refused("Verified applications declaration changed"));
        }
        let directory = self.transaction.medium.output_directory()?;
        directory.validate_destination()?;
        let meta = directory.file.metadata()?;
        if (meta.dev(), meta.ino()) != self.transaction.binding.output {
            return Err(refused("Verified applications directory changed"));
        }
        let binding = super::super::store::StageBinding {
            declaration: self.declaration.canonical_digest()?,
            filesystem: self.transaction.binding.filesystem_uuid,
            uid: self.declaration.requested_caller().uid(),
            gid: self.declaration.requested_caller().primary_gid(),
        };
        self.transaction
            .store
            .lock()
            .map_err(|_| refused("Applications Store lock failed"))?
            .check_verified_original_applications(
                self.attempt(),
                &directory.file,
                &self.expected,
                self.transaction.binding.reserve_bytes,
                &binding,
            )?;
        let storage = tilo_files::native::atomic::StorageDirectory::from_file(
            crate::setup::migration_durability::readable_directory(&directory.file)?,
        )?;
        let receipt = storage.open_read(
            OsStr::new(&format!("{}.receipt.json", self.expected.operation)),
            tilo_files::native::atomic::Privacy::OwnerOnly,
        )?;
        let mut bytes = Vec::new();
        receipt.take(18 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 18 * 1024 || bytes != serde_json::to_vec(&self.expected)? {
            return Err(refused("Applications canonical receipt differs"));
        }
        let grant =
            crate::setup::portable_home_media::MediaGrant::from_reviewed_pin(directory.clone())?;
        if grant.application_allocation(&self.expected, &self.transaction.cancelled)? != 0 {
            return Err(refused("Verified applications ciphertext is missing"));
        }
        directory.validate_destination()?;
        self.transaction.check()
    }
    pub fn commit_completed_setup(
        &self,
        expected: &crate::imager_model::PreparedSetupExpectation,
        marker_sha256: [u8; 32],
    ) -> io::Result<()> {
        self.validate()?;
        if !expected.valid() || marker_sha256 == [0; 32] {
            return Err(refused("Applications completed setup expectation invalid"));
        }
        let directory = self.completed_directory()?;
        let storage = tilo_files::native::atomic::StorageDirectory::from_file(
            crate::setup::migration_durability::readable_directory(&directory.file)?,
        )?;
        let marker = storage.open_read(
            OsStr::new("completion.json"),
            tilo_files::native::atomic::Privacy::OwnerOnly,
        )?;
        let mut bytes = Vec::new();
        marker.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
        use sha2::Digest;
        if bytes.len() > 16 * 1024
            || <[u8; 32]>::from(sha2::Sha256::digest(&bytes)) != marker_sha256
        {
            return Err(refused("Applications completed marker hash differs"));
        }
        self.transaction
            .store
            .lock()
            .map_err(|_| refused("Applications Store lock failed"))?
            .commit_completed_original_setup(
                self.attempt(),
                &directory.file,
                expected,
                marker_sha256,
            )?;
        directory.validate_destination()?;
        self.validate()
    }
    pub fn with_committed_selection_directory<T>(
        &self,
        callback: impl FnOnce(
            &Arc<PinnedDirectory>,
            &CommittedOriginalApplicationsSelection,
        ) -> io::Result<T>,
    ) -> io::Result<T> {
        self.validate()?;
        let completed = self.completed_directory()?;
        let applications = self.transaction.medium.output_directory()?;
        let (expected, marker_sha256) = self
            .transaction
            .store
            .lock()
            .map_err(|_| refused("Applications Store lock failed"))?
            .committed_original_setup(self.attempt(), &completed.file)?;
        self.commit_completed_setup(&expected, marker_sha256)?;
        let facts = CommittedOriginalApplicationsSelection {
            declaration: self.declaration.clone(),
            attempt: self.attempt().into(),
            pending: self.pending_setup_expectation()?.clone(),
            completed: expected,
            applications: self.expected.clone(),
            applications_inode: applications.file.metadata()?.ino(),
            completed_inode: completed.file.metadata()?.ino(),
            marker_sha256,
        };
        let private = self.transaction.medium.committed_selection_directory()?;
        let before = private.file.metadata()?;
        let result = callback(&private, &facts);
        private.validate_destination()?;
        let current = self
            .transaction
            .medium
            .committed_selection_directory()?
            .file
            .metadata()?;
        if (before.dev(), before.ino()) != (current.dev(), current.ino())
            || self.completed_directory()?.file.metadata()?.ino() != facts.completed_inode
            || self
                .transaction
                .medium
                .output_directory()?
                .file
                .metadata()?
                .ino()
                != facts.applications_inode
        {
            return Err(refused("Applications committed selection stages changed"));
        }
        let (expected, marker) = self
            .transaction
            .store
            .lock()
            .map_err(|_| refused("Applications Store lock failed"))?
            .committed_original_setup(self.attempt(), &completed.file)?;
        if expected != facts.completed || marker != facts.marker_sha256 {
            return Err(refused("Applications committed journal changed"));
        }
        self.commit_completed_setup(&expected, marker)?;
        self.validate()?;
        result
    }
    pub fn with_completed_directory<T>(
        &self,
        callback: impl FnOnce(&Arc<PinnedDirectory>) -> io::Result<T>,
    ) -> io::Result<T> {
        self.validate()?;
        let directory = self.completed_directory()?;
        let before = directory.file.metadata()?;
        let result = callback(&directory);
        directory.validate_destination()?;
        let current = self.completed_directory()?.file.metadata()?;
        if (before.dev(), before.ino()) != (current.dev(), current.ino()) {
            return Err(refused("Applications completed generation changed"));
        }
        self.validate()?;
        result
    }
    fn completed_directory(&self) -> io::Result<Arc<PinnedDirectory>> {
        let store = self
            .transaction
            .store
            .lock()
            .map_err(|_| refused("Applications Store lock failed"))?;
        self.transaction
            .medium
            .completed_applications_directory(&store, &self.expected)
    }
}
