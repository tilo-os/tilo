//! A private, helper-attested ciphertext inbox; never a filesystem-root grant.
use super::*;
use std::fs::File;
pub struct ProtectedInboxGrant<'a> {
    pin: Arc<PinnedDirectory>,
    proof: &'a super::super::read_media::VerifiedInboxLease<'a>,
}
impl<'a> ProtectedInboxGrant<'a> {
    pub(in crate::setup) fn from_admitted(
        file: File,
        proof: &'a super::super::read_media::VerifiedInboxLease<'a>,
    ) -> io::Result<Self> {
        proof.validate_exact(&file)?;
        let pin = PinnedDirectory::from_admitted_directory(file)?;
        proof.validate_exact(&pin.file)?;
        let grant = Self { pin, proof };
        grant.validate()?;
        Ok(grant)
    }
    pub fn validate(&self) -> io::Result<()> {
        self.proof.validate_exact(&self.pin.file)?;
        self.pin.validate_destination()?;
        crate::prepared_media::filesystem::check_writable_directory(&self.pin.file)?;
        Ok(())
    }
    pub fn validate_pending(
        &self,
        medium: &crate::media_layout::MediaPartitionBinding,
        setup: &crate::imager_model::PreparedSetupExpectation,
    ) -> io::Result<()> {
        self.validate()?;
        self.proof.validate_declaration(medium, setup)
    }
    pub fn portable_home_grant(&self) -> io::Result<super::super::portable_home_media::MediaGrant> {
        self.validate()?;
        super::super::portable_home_media::MediaGrant::from_reviewed_pin(self.pin.clone())
    }
    pub(super) fn pin(&self) -> &Arc<PinnedDirectory> {
        &self.pin
    }
}
