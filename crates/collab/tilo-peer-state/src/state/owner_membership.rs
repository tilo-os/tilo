//! Distinct structural-owner checks; collaborator roles remain unchanged.
use super::{DeviceCert, Role, StatePeer, SystemTime};

impl StatePeer {
    pub(super) fn current_membership_role(
        &self,
        membership: &crate::join::VerifiedMembership,
        now: SystemTime,
    ) -> Result<Role, String> {
        if membership.room() != self.core.room
            || membership.authority_device_public_key() != self.core.authority.device_public_key
        {
            return Err("membership proof is not for this pinned room authority".into());
        }
        self.current_certificate_role(membership.device_cert(), now)
    }

    fn current_certificate_role(&self, cert: &DeviceCert, now: SystemTime) -> Result<Role, String> {
        if self
            .core
            .certificates
            .get(&cert.device_public_key)
            .map(AsRef::as_ref)
            != Some(cert)
        {
            return Err("membership certificate is absent from governed state".into());
        }
        self.certificate(&cert.device_public_key, now)?;
        match self.permissions(cert.device_public_key, now) {
            Ok(member) => Ok(member.role),
            Err(error)
                if error.contains("no room membership") && self.has_governed_admission(cert) =>
            {
                Err("member has been removed".into())
            }
            Err(error) => Err(error),
        }
    }

    /// Check a verified membership refresh of the room owner.
    ///
    /// # Errors
    ///
    /// Returns an error if the refreshed device is not a current owner device.
    pub fn verify_owner_refresh(
        &self,
        membership: &crate::join::VerifiedMembership,
        now: SystemTime,
    ) -> Result<(), String> {
        let role = self.current_membership_role(membership, now)?;
        self.require_structural_owner(membership.device_cert(), role, now)
    }

    /// Rechecks current owner membership for an already committed receipt.
    /// The caller must independently bind the receipt's original scope and proof.
    ///
    /// # Errors
    ///
    /// Returns an error if `certificate` is not a current owner device at `now`.
    pub fn verify_current_owner(
        &self,
        certificate: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        let role = self.current_certificate_role(certificate, now)?;
        self.require_structural_owner(certificate, role, now)
    }

    fn require_structural_owner(
        &self,
        cert: &DeviceCert,
        role: Role,
        now: SystemTime,
    ) -> Result<(), String> {
        if role != Role::Owner || cert.user_public_key != self.core.authority.user_public_key {
            return Err("membership is not the pinned structural owner".into());
        }
        self.certificate(&self.core.authority.device_public_key, now)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
