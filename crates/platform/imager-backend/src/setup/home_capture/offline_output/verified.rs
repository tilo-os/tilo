//! Non-serializable authority minted from the actual independent verifier.
//! No public expectation, path or checkpoint constructs this token.
use super::*;
use crate::setup::{
    migration::PinnedDirectory,
    portable_home_media::Expectation,
    prepared_setup::{OfflineHomePlan, PreparedSetupReview},
};
use cap_fs_ext::OpenOptionsSyncExt as _;
use std::ffi::OsStr;
use std::io::Read;

pub struct VerifiedOriginalHome {
    transaction: Arc<OfflineOutputTransaction>,
    expected: Expectation,
    declaration: OfflineHomePlan,
    applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
}
pub struct CommittedOriginalSelection {
    declaration: OfflineHomePlan,
    attempt: String,
    pending: crate::imager_model::PreparedSetupExpectation,
    completed: crate::imager_model::PreparedSetupExpectation,
    home: Expectation,
    applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
    home_inode: u64,
    completed_inode: u64,
    marker_sha256: [u8; 32],
}
impl CommittedOriginalSelection {
    pub fn declaration(&self) -> &OfflineHomePlan {
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
    pub fn home(&self) -> &Expectation {
        &self.home
    }
    pub fn applications(&self) -> Option<&crate::setup::flatpak_transfer::portable::Expectation> {
        self.applications.as_ref()
    }
    pub fn home_inode(&self) -> u64 {
        self.home_inode
    }
    pub fn completed_inode(&self) -> u64 {
        self.completed_inode
    }
    pub fn marker_sha256(&self) -> [u8; 32] {
        self.marker_sha256
    }
}
impl OfflineOutputTransaction {
    pub fn validate_verified_outcome(
        self: &Arc<Self>,
        outcome: &OriginalExportOutcome,
    ) -> io::Result<VerifiedOriginalHome> {
        self.check()?;
        let [exported, verified] = outcome.runs.as_slice() else {
            return Err(refused("Original verifier sequence differs"));
        };
        if !exported.belongs_to(self) || !verified.belongs_to(self) {
            return Err(refused("Original outcome belongs to another transaction"));
        }
        let (expected, applications) = match exported.quiescent_response()? {
            super::super::worker::Response::ExportOriginal { home, applications } => {
                (home.clone(), applications.clone())
            }
            _ => return Err(refused("Original export response differs")),
        };
        if !matches!(
            verified.quiescent_response()?,
            super::super::worker::Response::Validated
        ) {
            return Err(refused(
                "Original verifier did not validate the complete stream",
            ));
        }
        // Successful quiescence, all namespace shutdowns and the final response
        // are mandatory; an outcome DTO or worker response alone cannot mint.
        outcome.cleanup_receipt()?;
        for run in &outcome.runs {
            if !run.supervised.shutdown.as_ref().is_some_and(|proof| {
                run.working_path()
                    .is_ok_and(|work| proof.matches(&run.key, work, super::super::runtime::PACKAGE))
            }) {
                return Err(refused("Original verifier namespace shutdown differs"));
            }
        }
        let actual = outcome
            .result
            .as_ref()
            .map_err(|_| refused("Original export failed"))?;
        if serde_json::to_vec(actual)? != serde_json::to_vec(&expected)? {
            return Err(refused("Original verified expectation differs"));
        }
        if serde_json::to_vec(&applications)? != serde_json::to_vec(&outcome.applications)? {
            return Err(refused("Original verified application expectation differs"));
        }
        let declaration = self.read.output_declaration(&self.cancelled)?;
        let token = VerifiedOriginalHome {
            transaction: self.clone(),
            expected,
            declaration,
            applications,
        };
        token.validate()?;
        Ok(token)
    }
}
impl VerifiedOriginalHome {
    pub fn commit_completed_setup(
        &self,
        expected: &crate::imager_model::PreparedSetupExpectation,
        marker_sha256: [u8; 32],
    ) -> io::Result<()> {
        self.validate()?;
        if !expected.valid() || marker_sha256 == [0; 32] {
            return Err(refused("Completed setup expectation invalid"));
        }
        let directory = self.completed_directory()?;
        directory.validate_destination()?;
        let storage = tilo_files::native::atomic::StorageDirectory::from_file(
            crate::setup::migration_durability::readable_directory(&directory.file)?,
        )?;
        let marker = storage.open_read(
            OsStr::new("completion.json"),
            tilo_files::native::atomic::Privacy::OwnerOnly,
        )?;
        if marker.metadata()?.len() > 16 * 1024 {
            return Err(refused("Completed setup marker exceeds bounds"));
        }
        let mut bytes = Vec::new();
        marker.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
        use sha2::Digest;
        let actual: [u8; 32] = sha2::Sha256::digest(&bytes).into();
        if bytes.len() > 16 * 1024 || actual != marker_sha256 {
            return Err(refused("Completed setup marker hash differs"));
        }
        // The closed marker schema and same-origin package are authenticated by
        // the canonical carrier; this seam proves the bytes and journal inode.
        self.transaction
            .store
            .lock()
            .map_err(|_| refused("Verified output Store lock failed"))?
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
        callback: impl FnOnce(&Arc<PinnedDirectory>, &CommittedOriginalSelection) -> io::Result<T>,
    ) -> io::Result<T> {
        self.validate()?;
        let completed = self.completed_directory()?;
        let home = self.transaction.medium.output_directory()?;
        self.check_directory(&home)?;
        let (expected, marker_sha256) = self
            .transaction
            .store
            .lock()
            .map_err(|_| refused("Committed selection Store lock failed"))?
            .committed_original_setup(self.attempt(), &completed.file)?;
        // Rechecks actual marker bytes against the already committed journal;
        // exact repeated commit is idempotent and cannot adopt another marker.
        self.commit_completed_setup(&expected, marker_sha256)?;
        let facts = CommittedOriginalSelection {
            declaration: self.declaration.clone(),
            attempt: self.attempt().into(),
            pending: self.pending_setup_expectation()?.clone(),
            completed: expected,
            home: self.expected.clone(),
            applications: self.applications.clone(),
            home_inode: home.file.metadata()?.ino(),
            completed_inode: completed.file.metadata()?.ino(),
            marker_sha256,
        };
        let private = self.transaction.medium.committed_selection_directory()?;
        let before = private.file.metadata()?;
        let result = callback(&private, &facts);
        private.validate_destination()?;
        let current = self.transaction.medium.committed_selection_directory()?;
        let after = current.file.metadata()?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
            return Err(refused("Committed selection root changed"));
        }
        let current_completed = self.completed_directory()?;
        let current_home = self.transaction.medium.output_directory()?;
        if current_completed.file.metadata()?.ino() != facts.completed_inode
            || current_home.file.metadata()?.ino() != facts.home_inode
        {
            return Err(refused("Committed selection stages changed"));
        }
        let (expected, marker) = self
            .transaction
            .store
            .lock()
            .map_err(|_| refused("Committed selection Store lock failed"))?
            .committed_original_setup(self.attempt(), &current_completed.file)?;
        if expected != facts.completed || marker != facts.marker_sha256 {
            return Err(refused("Committed selection journal changed"));
        }
        self.commit_completed_setup(&expected, marker)?;
        self.validate()?;
        result
    }
    /// Retain the same non-serializable authority, after current revalidation.
    pub fn retain(&self) -> io::Result<Self> {
        self.validate()?;
        Ok(Self {
            transaction: self.transaction.clone(),
            expected: self.expected.clone(),
            declaration: self.declaration.clone(),
            applications: self.applications.clone(),
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
            || self.attempt() != other.attempt()
            || self.declaration != other.declaration
            || serde_json::to_vec(&self.applications)? != serde_json::to_vec(&other.applications)?
            || serde_json::to_vec(&self.expected)? != serde_json::to_vec(&other.expected)?
            || serde_json::to_vec(self.pending_setup_expectation()?)?
                != serde_json::to_vec(other.pending_setup_expectation()?)?
        {
            return Err(refused(
                "Completed setup package belongs to another original transaction",
            ));
        }
        Ok(())
    }
    pub fn applications(&self) -> Option<&crate::setup::flatpak_transfer::portable::Expectation> {
        self.applications.as_ref()
    }
    pub fn expected(&self) -> &Expectation {
        &self.expected
    }
    pub fn declaration(&self) -> &OfflineHomePlan {
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
                "Verified home belongs to another authenticated setup",
            ));
        }
        Ok(())
    }
    pub fn validate(&self) -> io::Result<()> {
        self.transaction.check()?;
        self.expected.validate()?;
        let declaration = self
            .transaction
            .read
            .output_declaration(&self.transaction.cancelled)?;
        if declaration != self.declaration
            || self.expected.operation != self.transaction.binding.operation
            || self.expected.sealed_bytes > self.transaction.binding.reserve_bytes
            || self.expected.setup_binding
                != self
                    .transaction
                    .read
                    .authenticated_setup()
                    .manifest()
                    .setup_binding
        {
            return Err(refused("Verified home authenticated binding differs"));
        }
        let required = self
            .transaction
            .read
            .output_flatpak_required(&self.transaction.cancelled)?;
        let operation = self
            .transaction
            .read
            .output_flatpak_operation(&self.transaction.cancelled)?;
        if required != self.applications.is_some() {
            return Err(refused("Verified application selection differs"));
        }
        if let Some(apps) = &self.applications {
            apps.validate()?;
            if operation.as_deref() != Some(apps.operation.as_str())
                || apps.setup_binding != self.expected.setup_binding
                || apps.operation == self.expected.operation
                || self
                    .expected
                    .sealed_bytes
                    .checked_add(apps.sealed_bytes)
                    .is_none_or(|size| size > self.transaction.binding.reserve_bytes)
            {
                return Err(refused("Verified application binding or budget differs"));
            }
        }
        let directory = self.transaction.medium.output_directory()?;
        self.check_directory(&directory)?;
        self.transaction.check()
    }
    pub fn with_completed_directory<T>(
        &self,
        callback: impl FnOnce(&Arc<PinnedDirectory>) -> io::Result<T>,
    ) -> io::Result<T> {
        self.validate()?;
        let directory = self.completed_directory()?;
        let before = directory.file.metadata()?;
        let answer = callback(&directory);
        directory.validate_destination()?;
        let current = self.completed_directory()?;
        let after = current.file.metadata()?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
            return Err(refused("Completed setup generation inode changed"));
        }
        self.validate()?;
        answer
    }
    fn completed_directory(&self) -> io::Result<Arc<PinnedDirectory>> {
        let store = self
            .transaction
            .store
            .lock()
            .map_err(|_| refused("Verified output Store lock failed"))?;
        self.transaction.medium.completed_directory_paired(
            &store,
            &super::super::store::OriginalOutputExpectations {
                home: &self.expected,
                applications: self.applications.as_ref(),
                reserve_bytes: self.transaction.binding.reserve_bytes,
            },
        )
    }
    fn check_directory(&self, directory: &Arc<PinnedDirectory>) -> io::Result<()> {
        directory.validate_destination()?;
        let actual = directory.file.metadata()?;
        if (actual.dev(), actual.ino()) != self.transaction.binding.output {
            return Err(refused("Verified home output inode differs"));
        }
        use sha2::Digest;
        let binding = super::super::store::StageBinding {
            declaration: sha2::Sha256::digest(serde_json::to_vec(&self.declaration)?).into(),
            filesystem: self.transaction.binding.filesystem_uuid,
            uid: self.declaration.requested_caller().uid(),
            gid: self.declaration.requested_caller().primary_gid(),
        };
        self.transaction
            .store
            .lock()
            .map_err(|_| refused("Verified output Store lock failed"))?
            .check_verified_original_output_paired(
                self.attempt(),
                &directory.file,
                &super::super::store::OriginalOutputExpectations {
                    home: &self.expected,
                    applications: self.applications.as_ref(),
                    reserve_bytes: self.transaction.binding.reserve_bytes,
                },
                &binding,
            )?;
        let storage = tilo_files::native::atomic::StorageDirectory::from_file(
            crate::setup::migration_durability::readable_directory(&directory.file)?,
        )?;
        let receipt = storage.open_read(
            OsStr::new(&format!("{}.receipt.json", self.expected.operation)),
            tilo_files::native::atomic::Privacy::OwnerOnly,
        )?;
        if receipt.metadata()?.len() > 16 * 1024 {
            return Err(refused("Verified home receipt exceeds bounds"));
        }
        let mut bytes = Vec::new();
        receipt.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes != serde_json::to_vec(&self.expected)? {
            return Err(refused("Verified home canonical receipt differs"));
        }
        // The independent verifier authenticated these bytes before the whole
        // provider namespace died. Here only fixed no-follow file shape is read;
        // no claimed metadata is substituted for that actual verifier evidence.
        let mut options = cap_primitives::fs::OpenOptions::new();
        use cap_fs_ext::OpenOptionsFollowExt;
        options
            .read(true)
            .follow(cap_primitives::fs::FollowSymlinks::No)
            .nonblock(true);
        let package = cap_primitives::fs::open(
            &directory.file,
            Path::new(&self.expected.filename),
            &options,
        )?;
        let metadata = package.metadata()?;
        if !metadata.is_file()
            || metadata.len() != self.expected.sealed_bytes
            || metadata.nlink() != 1
        {
            return Err(refused("Verified home ciphertext file differs"));
        }
        if let Some(apps) = &self.applications {
            let receipt = storage.open_read(
                OsStr::new(&format!("{}.receipt.json", apps.operation)),
                tilo_files::native::atomic::Privacy::OwnerOnly,
            )?;
            if receipt.metadata()?.len() > 18 * 1024 {
                return Err(refused("Application receipt exceeds bounds"));
            }
            let mut bytes = Vec::new();
            receipt.take(18 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes != serde_json::to_vec(apps)? {
                return Err(refused("Verified application receipt differs"));
            }
            let grant = crate::setup::portable_home_media::MediaGrant::from_reviewed_pin(
                directory.clone(),
            )?;
            if grant.application_allocation(apps, &self.transaction.cancelled)? != 0 {
                return Err(refused("Verified application ciphertext is missing"));
            }
        }
        directory.validate_destination()
    }
}
