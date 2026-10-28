//! Target-user portable home continuation. Authenticated Home data is distinct
//! from prepared identity/account custody and is adopted only by an explicit action.
use super::{
    destination_security,
    home_transfer::promotion::{PromotionEvent, PromotionReview, PromotionWorker},
    migration::{PinnedDirectory, changed},
    portable_home_media::Expectation,
    portable_transfer::{ImportReview, PackageInfo, Purpose},
    prepared_setup::PreparedSetupReview,
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct HomeImportGrants {
    pub package: PathBuf,
    pub home: PathBuf,
    pub workspace: PathBuf,
}
pub struct PortableHomeReview {
    archive: ImportReview,
    grants: HomeImportGrants,
    promotion: Option<PromotionReview>,
    recovered: bool,
    applications: bool,
}
#[derive(Debug, serde::Serialize)]
pub struct AdoptionPreview {
    pub install_items: usize,
    pub collisions: usize,
    pub inactive_custody: usize,
    pub runtime_sockets: u64,
    pub recovered: bool,
}
#[derive(Debug, serde::Serialize)]
pub struct AdoptionResult {
    pub complete: bool,
    pub cancelled: bool,
    pub installed: usize,
    pub collisions: usize,
    pub inactive_custody: usize,
    pub failures: usize,
    pub verified: bool,
}
pub struct AdoptionWorker(PromotionWorker);
impl AdoptionWorker {
    pub fn cancel(&self) {
        self.0.cancel();
    }
    pub fn try_next(&mut self) -> Option<Result<AdoptionResult, String>> {
        self.0.try_next().map(|event| match event {
            PromotionEvent::Failed(error) => Err(error),
            PromotionEvent::Finished(report) => Ok(AdoptionResult {
                complete: report.complete(),
                cancelled: report.cancelled,
                installed: report.installed.len(),
                collisions: report.collisions.len(),
                inactive_custody: report.inactive_custody.len(),
                failures: report.failed.len(),
                verified: report.verified,
            }),
        })
    }
}
impl PortableHomeReview {
    fn recover_promotion(&self, root: &Path, journal: &Path) -> io::Result<PromotionReview> {
        #[cfg(target_os = "linux")]
        if let Some(scratch) = self.archive.working_storage() {
            return PromotionReview::recover_with_storage(
                root,
                &self.grants.home,
                journal,
                scratch,
            )
            .map_err(io::Error::other);
        }
        PromotionReview::recover(root, &self.grants.home, journal).map_err(io::Error::other)
    }

    /// Authenticate the declared full home using the separately authenticated
    /// prepared-setup grant. A mutable media receipt alone cannot declare it.
    pub fn review(
        grants: HomeImportGrants,
        setup: &PreparedSetupReview,
        expectation: &Expectation,
        password: &[u8],
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        setup.require_completed_export()?;
        expectation.validate()?;
        if setup
            .manifest()
            .completed_home
            .as_ref()
            .is_some_and(|home| home != expectation)
            || setup.manifest().portable_home.is_none()
            || expectation.setup_binding != setup.manifest().setup_binding
            || grants.package.file_name() != Some(std::ffi::OsStr::new(&expectation.filename))
        {
            return Err(changed(
                "Portable home does not belong to this prepared setup",
            ));
        }
        let applications = setup
            .manifest()
            .portable_home
            .as_ref()
            .is_some_and(|plan| plan.applications);
        Self::review_archive(grants, expectation, password, cancel, None, applications)
    }
    /// Receive a Home descriptor after the installed controller authenticates
    /// its operation, target owner and expected archive. This authenticates all
    /// bytes independently; it does not mint operation or installation authority.
    pub fn review_received(
        grants: HomeImportGrants,
        expectation: &Expectation,
        file: std::fs::File,
        password: &[u8],
        cancel: &AtomicBool,
        applications: bool,
    ) -> io::Result<Self> {
        expectation.validate()?;
        Self::review_archive(
            grants,
            expectation,
            password,
            cancel,
            Some(file),
            applications,
        )
    }
    fn review_archive(
        grants: HomeImportGrants,
        expectation: &Expectation,
        password: &[u8],
        cancel: &AtomicBool,
        received: Option<std::fs::File>,
        applications: bool,
    ) -> io::Result<Self> {
        for path in [&grants.home, &grants.workspace] {
            let pin = PinnedDirectory::open_absolute(path)?;
            pin.validate_destination()?;
            destination_security::check_private_file(&pin.file)?;
        }
        if !grants
            .workspace
            .starts_with(grants.home.join(".local/state/tilo/portable-home"))
            || grants.workspace == grants.home
            || grants
                .package
                .starts_with(grants.workspace.join("imported"))
        {
            return Err(changed(
                "Portable home import grants overlap or lack private custody",
            ));
        }
        #[cfg(target_os = "linux")]
        let scratch = {
            let existing = grants.workspace.join("imported/home");
            match std::fs::symlink_metadata(&existing) {
                Ok(_) => {
                    let output = PinnedDirectory::open_absolute(&existing)?;
                    let lease = super::migration_review::InventoryScratch::review_for_restore(
                        PinnedDirectory::open_absolute(&grants.workspace)?,
                        0,
                        0,
                        expectation.limits.metadata_bytes,
                        super::home_transfer::promotion::JOURNAL_RESERVE,
                    )?;
                    super::portable_transfer::archive::Scratch::verification_only(lease, output)?
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    super::portable_transfer::archive::Scratch::retained(
                        super::migration_review::InventoryScratch::review_for_restore(
                            PinnedDirectory::open_absolute(&grants.workspace)?,
                            expectation.plaintext_bytes,
                            expectation.entries,
                            expectation.limits.metadata_bytes,
                            super::home_transfer::promotion::JOURNAL_RESERVE,
                        )?,
                    )
                }
                Err(error) => return Err(error),
            }
        };
        #[cfg(not(target_os = "linux"))]
        let scratch = super::portable_transfer::archive::Scratch::default();
        let archive = match received {
            Some(file) => ImportReview::review_file_in(
                file,
                password,
                Purpose::Home,
                expectation.limits,
                cancel,
                scratch,
            )?,
            None => ImportReview::review_in(
                &grants.package,
                password,
                Purpose::Home,
                expectation.limits,
                cancel,
                scratch,
            )?,
        };
        let info = archive.info();
        if archive.ciphertext_digest() != expectation.ciphertext_sha256
            || info.setup_binding.as_deref() != Some(expectation.setup_binding.as_str())
            || info.sealed_bytes != expectation.sealed_bytes
            || info.plaintext_bytes != expectation.plaintext_bytes
            || info.entries != expectation.entries
            || info.runtime_sockets != expectation.runtime_sockets
        {
            return Err(changed(
                "Portable home ciphertext or authenticated declaration changed",
            ));
        }
        Ok(Self {
            archive,
            grants,
            promotion: None,
            recovered: false,
            applications,
        })
    }
    pub fn info(&self) -> &PackageInfo {
        self.archive.info()
    }
    /// Independently verify completed adoption and authenticated relocated bytes.
    /// This does not execute pending adoption work or trust a UI completion flag.
    /// # Errors
    /// Refuses absent journals, pending moves, collisions or changed archive/data.
    pub fn verify_completed_adoption(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        let promotion = self.recover_promotion(
            &self.grants.workspace.join("imported/home"),
            &self.grants.workspace.join("adoption.journal"),
        )?;
        promotion.verify_completed().map_err(io::Error::other)?;
        self.archive
            .verify_adoption(&promotion.witness().map_err(io::Error::other)?, cancel)
    }
    pub fn verify_completed_after_applications(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        let promotion = self.recover_promotion(
            &self.grants.workspace.join("imported/home"),
            &self.grants.workspace.join("adoption.journal"),
        )?;
        let witness = promotion
            .completed_application_witness()
            .map_err(io::Error::other)?;
        self.archive
            .verify_adoption_after_applications(&witness, cancel)
    }
    pub fn verify_original_application_config(&self, bytes: &[u8]) -> io::Result<()> {
        self.archive.verify_original_application_config(bytes)
    }
    pub fn ciphertext_digest(&self) -> [u8; 32] {
        self.archive.ciphertext_digest()
    }
    /// Extract into its final private quarantine, or independently verify the
    /// existing tree/move witnesses after a restart. There is no second home copy.
    ///
    /// # Errors
    /// Refuses authentication/fidelity failures and unsupported existing output;
    /// existing custody, collisions and partial imports are preserved.
    pub fn prepare(
        &mut self,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<AdoptionPreview> {
        let container = self.grants.workspace.join("imported");
        let root = container.join("home");
        let journal = self.grants.workspace.join("adoption.journal");
        let promotion = match std::fs::symlink_metadata(&journal) {
            Ok(_) => {
                let promotion = self.recover_promotion(&root, &journal)?;
                self.archive
                    .verify_adoption(&promotion.witness().map_err(io::Error::other)?, cancel)?;
                self.recovered = true;
                promotion
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(&container) {
                    Ok(_) => self.archive.verify_existing_tree(&root, cancel)?,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        self.archive.extract_held(password, &container, cancel)?;
                    }
                    Err(error) => return Err(error),
                }
                self.archive.verify_existing_tree(&root, cancel)?;
                #[cfg(target_os = "linux")]
                self.archive.release_verified_restoration()?;
                #[cfg(target_os = "linux")]
                let promotion = match self.archive.working_storage() {
                    Some(scratch) => PromotionReview::review_with_storage(
                        &root,
                        &self.grants.home,
                        &journal,
                        scratch,
                        cancel,
                    ),
                    None => PromotionReview::review(&root, &self.grants.home, &journal),
                };
                #[cfg(not(target_os = "linux"))]
                let promotion = PromotionReview::review(&root, &self.grants.home, &journal);
                promotion.map_err(io::Error::other)?
            }
            Err(error) => return Err(error),
        };
        let promotion = promotion
            .with_home_root_acl()
            .map_err(io::Error::other)?
            .with_application_consent(self.applications)
            .map_err(io::Error::other)?;
        let preview = promotion.preview().map_err(io::Error::other)?;
        self.promotion = Some(promotion);
        Ok(AdoptionPreview {
            install_items: preview.installed.len(),
            collisions: preview.collisions.len(),
            inactive_custody: preview.inactive_custody.len(),
            runtime_sockets: self.archive.info().runtime_sockets,
            recovered: self.recovered,
        })
    }
    /// Invoke the same adoption worker after a separate reviewed user action.
    ///
    /// # Errors
    /// Refuses missing review or changed archive/tree/move receipts before moving.
    pub fn start_adoption(&mut self, cancel: &AtomicBool) -> io::Result<AdoptionWorker> {
        let promotion = self
            .promotion
            .as_ref()
            .ok_or_else(|| changed("Portable home adoption has not been reviewed"))?;
        if self.recovered {
            self.archive
                .verify_adoption(&promotion.witness().map_err(io::Error::other)?, cancel)?;
        } else {
            self.archive
                .verify_existing_tree(&self.grants.workspace.join("imported/home"), cancel)?;
        }
        Ok(AdoptionWorker(
            promotion.start(self.recovered).map_err(io::Error::other)?,
        ))
    }
}
#[cfg(test)]
mod tests;
