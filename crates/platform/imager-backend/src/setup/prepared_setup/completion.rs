//! Amend only password-authenticated setup after a genuinely verified Home job.
//! Original pending ciphertext and its sealed identity/account artifacts stay intact.
use super::*;
use crate::setup::{
    home_capture::offline_output::{VerifiedOriginalApplications, VerifiedOriginalHome},
    media_setup::PreparedSetupDelivery,
    portable_transfer::ExportReview,
};
use std::os::unix::fs::PermissionsExt as _;
pub mod boot_review;
mod handoff;
mod origin;
mod publication;
use origin::Origin;
mod selection;
mod selection_facts;
mod stream;
pub use handoff::selected_generation;
pub use publication::CompletedSetupReceipt;
pub use stream::run as stream_payload;

/// Held canonical ciphertext; no public metadata constructs completed authority.
/// The plaintext staging lease is erased before this value is returned.
pub struct CompletedSetupPackage {
    _ciphertext: tempfile::TempDir,
    delivery: PreparedSetupDelivery,
    manifest: Manifest,
    origin: Origin,
    password: Zeroizing<Vec<u8>>,
    sealed_artifacts: std::collections::BTreeMap<String, [u8; 32]>,
}
impl CompletedSetupPackage {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn validate_origin(&self, verified: &VerifiedOriginalHome) -> io::Result<()> {
        self.origin
            .validate_same_origin(&Origin::Home(verified.retain()?))
    }
    pub fn publish(
        &mut self,
        verified: &VerifiedOriginalHome,
        cancel: &AtomicBool,
    ) -> io::Result<crate::setup::media_setup::CompletedSetupReceipt> {
        self.validate_origin(verified)?;
        let origin = Origin::Home(verified.retain()?);
        let receipt = publication::publish(self, &origin, cancel)?;
        selection::publish(&origin, &receipt.setup, cancel)?;
        Ok(receipt)
    }
    pub fn publish_applications(
        &mut self,
        verified: &VerifiedOriginalApplications,
        cancel: &AtomicBool,
    ) -> io::Result<crate::setup::media_setup::CompletedSetupReceipt> {
        let origin = Origin::Applications(verified.retain()?);
        let receipt = publication::publish(self, &origin, cancel)?;
        selection::publish(&origin, &receipt.setup, cancel)?;
        Ok(receipt)
    }
    fn validate_completion_origin(&self, origin: &Origin) -> io::Result<()> {
        self.origin.retain()?.validate_same_origin(origin)
    }
}
impl PreparedSetupReview {
    pub fn reseal_completed_home(
        &self,
        verified: &VerifiedOriginalHome,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<CompletedSetupPackage> {
        self.reseal_completed(Origin::Home(verified.retain()?), password, cancel)
    }
    pub fn reseal_completed_applications(
        &self,
        verified: &VerifiedOriginalApplications,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<CompletedSetupPackage> {
        self.reseal_completed(Origin::Applications(verified.retain()?), password, cancel)
    }
    fn reseal_completed(
        &self,
        origin: Origin,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<CompletedSetupPackage> {
        self.validate()?;
        origin.validate_setup(self)?;
        let declaration = self.pending_original_declaration()?;
        if declaration != origin.declaration()
            || declaration.identity_public_key() != self.manifest.identity_public_key
        {
            return Err(invalid());
        }
        if let Some(home) = origin.home() {
            home.validate()?;
            if home.operation != declaration.as_home()?.export_operation
                || home.setup_binding != self.manifest.setup_binding
                || home.sealed_bytes > declaration.sealed_reservation()
            {
                return Err(invalid());
            }
        }
        if declaration.is_applications_only() {
            let apps = origin.applications().ok_or_else(invalid)?;
            apps.validate()?;
            if apps.operation != declaration.as_applications()?.applications_operation()?
                || apps.setup_binding != self.manifest.setup_binding
                || apps.sealed_bytes > declaration.sealed_reservation()
            {
                return Err(invalid());
            }
        }
        let mut manifest = self.manifest.clone();
        manifest.offline_home = None;
        manifest.offline_applications = None;
        manifest.completed_home = origin.home().cloned();
        manifest.completed_applications_only = declaration.is_applications_only();
        manifest.portable_flatpak = origin.applications().cloned();
        manifest.validate()?;
        super::super::migration::check_cancel(cancel)?;
        let stage = tempfile::Builder::new()
            .prefix("tilo-completed-setup-stage-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let pin = PinnedDirectory::open_absolute(stage.path())?;
        destination_security::check_private_file(&pin.file)?;
        let mut sealed_artifacts = std::collections::BTreeMap::new();
        // Copy only the already-password-sealed artifacts. Do not activate,
        // decrypt, remint or rewrite the person's identity, accounts or sessions.
        for name in [IDENTITY_FILE, ACCOUNTS_FILE, SERVICE_SESSIONS_FILE] {
            let selected = name == IDENTITY_FILE
                || (name == ACCOUNTS_FILE && !manifest.account_ids.is_empty())
                || (name == SERVICE_SESSIONS_FILE && manifest.service_sessions.is_some());
            if !selected {
                continue;
            }
            let bytes = Zeroizing::new(bounded_file(&self.pin, name, LIMITS.plaintext_bytes)?);
            sealed_artifacts.insert(name.to_owned(), Sha256::digest(bytes.as_slice()).into());
            let mut options = OpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .mode(0o600)
                .follow(FollowSymlinks::No);
            let mut file = cap_primitives::fs::open(&pin.file, Path::new(name), &options)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            super::super::migration::check_cancel(cancel)?;
        }
        manifest.write(stage.path())?;
        self.validate()?;
        origin.validate()?;
        let ciphertext = tempfile::Builder::new()
            .prefix("tilo-completed-setup-ciphertext-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let package = ciphertext.path().join("prepared-setup.sealed");
        ExportReview::review(stage.path(), &package, Purpose::PreparedSetup, LIMITS)?
            .export(Zeroizing::new(password.to_vec()), cancel)?;
        // Re-open through the actual password-authenticated setup parser, not
        // a copied public receipt. It verifies every inner sealed artifact.
        let quarantine = tempfile::Builder::new()
            .prefix("tilo-completed-setup-review-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let reviewed = PreparedSetupReview::open(
            &package,
            &password,
            &quarantine.path().join("setup"),
            cancel,
        )?;
        reviewed.require_completed_export()?;
        if reviewed.manifest() != &manifest {
            return Err(invalid());
        }
        let delivery = PreparedSetupDelivery::review(&package, &password, cancel)?;
        self.validate()?;
        origin.validate()?;
        super::super::migration::check_cancel(cancel)?;
        drop(reviewed);
        quarantine.close()?;
        stage.close()?;
        Ok(CompletedSetupPackage {
            _ciphertext: ciphertext,
            delivery,
            manifest,
            origin,
            password,
            sealed_artifacts,
        })
    }
}
