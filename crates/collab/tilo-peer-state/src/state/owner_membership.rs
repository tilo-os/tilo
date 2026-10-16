//! The current role behind a verified membership proof.
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
}
#[cfg(test)]
mod tests;
