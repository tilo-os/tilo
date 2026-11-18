//! The current role behind a verified membership proof.
use super::{MembershipError, Role, StatePeer, SystemTime};

impl StatePeer {
    /// The role of the device that proved `membership`, as any key holder
    /// reads it: a device its person's root certified, which the workspace
    /// holds and has not revoked, of a member. The key holder that challenged
    /// it need not be the owner's: a member's further device is let in by
    /// any ([`crate::join::VerifiedMembership`]).
    pub(super) fn current_membership_role(
        &self,
        membership: &crate::join::VerifiedMembership,
        now: SystemTime,
    ) -> Result<Role, String> {
        if membership.workspace_id().to_string() != self.core.workspace {
            return Err("membership proof names another workspace".into());
        }
        let cert = membership.device_cert();
        if self
            .core
            .certificates
            .get(&cert.device_public_key)
            .map(AsRef::as_ref)
            != Some(cert)
        {
            return Err("membership certificate is absent from governed state".into());
        }
        self.certificate(&cert.device_public_key, now)
            .map_err(|error| error.to_string())?;
        // A device the workspace admitted and then removed is refused as removed,
        // not as a stranger.
        match self.role(&cert.device_public_key, now) {
            Ok(role) => Ok(role),
            Err(MembershipError::NoMembership) if self.has_governed_admission(cert) => {
                Err("member has been removed".into())
            }
            Err(error) => Err(error.to_string()),
        }
    }
}
#[cfg(test)]
mod tests;
