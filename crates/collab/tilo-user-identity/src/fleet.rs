//! One person's devices: which keys speak for a user identity right now.
//!
//! A [`CertifiedFleet`] holds the [`DeviceCert`]s and [`RevocationEntry`]s
//! one user root signed, each verified as it is added, and answers the one
//! question a verifier asks of a device key: does it stand for this user
//! *now*? That is the cert chain [`DeviceCert`] documents — a certificate
//! signed by the user root, inside its validity window, for a key the root
//! has not revoked — applied in one place.
//!
//! A revocation is permanent and applies to everything the key signed,
//! whenever it claims to have signed it: a lost key can back-date a
//! signature, so a timestamp the signer chose proves nothing.

use thiserror::Error;

use crate::{
    DeviceCert, DeviceCertError, RevocationEntry, RevocationError, RevocationList,
    RevocationReason, verify_device_cert, verify_device_cert_at,
};

/// Where a device key stands with a user identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceStanding {
    /// The user root certified the key, the certificate is valid now, and
    /// the key was never revoked.
    Certified,
    Revoked(RevocationReason),
    /// No certificate of this user's names the key, or none is valid now.
    Uncertified,
}

/// Why a certificate or revocation was not added to a fleet.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum FleetError {
    /// The certificate or revocation was signed by another user root.
    #[error("signed by another identity")]
    OtherIdentity,
    /// The certificate's signature does not verify.
    #[error(transparent)]
    Certificate(#[from] DeviceCertError),
    /// The revocation's signature does not verify.
    #[error(transparent)]
    Revocation(#[from] RevocationError),
}

/// The devices one user identity certified, and the ones it revoked.
#[derive(Clone, Debug)]
pub struct CertifiedFleet {
    user_public_key: [u8; 32],
    certificates: Vec<DeviceCert>,
    revocations: RevocationList,
}

impl CertifiedFleet {
    /// The fleet of `user_public_key`, with no devices yet.
    pub fn new(user_public_key: [u8; 32]) -> Self {
        Self {
            user_public_key,
            certificates: Vec::new(),
            revocations: RevocationList::new(),
        }
    }

    /// The user root the fleet belongs to.
    pub fn user_public_key(&self) -> &[u8; 32] {
        &self.user_public_key
    }

    /// Add a certificate the user root signed. Its validity window is
    /// checked when a key's standing is asked, not here.
    ///
    /// # Errors
    ///
    /// Returns [`FleetError::OtherIdentity`] for another user's certificate
    /// and [`FleetError::Certificate`] when its signature does not verify.
    pub fn certify(&mut self, cert: DeviceCert) -> Result<(), FleetError> {
        if cert.user_public_key != self.user_public_key {
            return Err(FleetError::OtherIdentity);
        }
        verify_device_cert(&cert)?;
        if !self.certificates.contains(&cert) {
            self.certificates.push(cert);
        }
        Ok(())
    }

    /// Add a revocation the user root signed.
    ///
    /// # Errors
    ///
    /// Returns [`FleetError::OtherIdentity`] for another user's revocation
    /// and [`FleetError::Revocation`] when its signature does not verify.
    pub fn revoke(&mut self, entry: RevocationEntry) -> Result<(), FleetError> {
        if entry.user_public_key != self.user_public_key {
            return Err(FleetError::OtherIdentity);
        }
        self.revocations.insert(entry)?;
        Ok(())
    }

    /// Where `device_public_key` stands with this user at `now_ns`.
    pub fn standing(&self, device_public_key: &[u8; 32], now_ns: u64) -> DeviceStanding {
        if let Some(entry) = self
            .revocations
            .iter()
            .find(|entry| &entry.revoked_device_public_key == device_public_key)
        {
            return DeviceStanding::Revoked(entry.reason);
        }
        let certified = self.certificates.iter().any(|cert| {
            &cert.device_public_key == device_public_key
                && verify_device_cert_at(cert, now_ns).is_ok()
        });
        if certified {
            DeviceStanding::Certified
        } else {
            DeviceStanding::Uncertified
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceCapabilities, UserIdentity, sign_device_cert, sign_revocation_entry};

    fn cert(
        root: &ed25519_dalek::SigningKey,
        device: [u8; 32],
        capabilities: DeviceCapabilities,
        expires_at_ns: Option<u64>,
    ) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "device".into(),
            capabilities,
            issued_at_ns: 100,
            expires_at_ns,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    /// A key stands for the user only through a valid certificate of theirs
    /// and only until they revoke it; another user's papers are refused.
    #[test]
    fn standing_follows_the_certificate_chain() {
        let (root, user) = UserIdentity::generate("Ada", 1);
        let (other_root, _) = UserIdentity::generate("Bob", 1);
        let laptop = [1; 32];
        let host = [2; 32];
        let phone = [3; 32];
        let mut fleet = CertifiedFleet::new(user.public_key);
        fleet
            .certify(cert(&root, laptop, DeviceCapabilities::primary(), None))
            .unwrap();
        fleet
            .certify(cert(&root, host, DeviceCapabilities::host(), None))
            .unwrap();
        fleet
            .certify(cert(
                &root,
                phone,
                DeviceCapabilities::member(),
                Some(1_000),
            ))
            .unwrap();

        assert_eq!(fleet.standing(&laptop, 500), DeviceStanding::Certified);
        assert_eq!(fleet.standing(&host, 500), DeviceStanding::Certified);
        assert_eq!(fleet.standing(&phone, 500), DeviceStanding::Certified);
        // Expired, not yet issued, never certified.
        assert_eq!(fleet.standing(&phone, 1_000), DeviceStanding::Uncertified);
        assert_eq!(fleet.standing(&laptop, 50), DeviceStanding::Uncertified);
        assert_eq!(fleet.standing(&[9; 32], 500), DeviceStanding::Uncertified);

        // Another person's certificate, even for a key this fleet knows.
        assert_eq!(
            fleet.certify(cert(
                &other_root,
                [4; 32],
                DeviceCapabilities::primary(),
                None
            )),
            Err(FleetError::OtherIdentity)
        );
        let mut forged = cert(&root, [5; 32], DeviceCapabilities::primary(), None);
        forged.device_public_key = [6; 32];
        assert!(matches!(
            fleet.certify(forged),
            Err(FleetError::Certificate(_))
        ));
        assert_eq!(fleet.standing(&[6; 32], 500), DeviceStanding::Uncertified);

        let mut entry = RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: host,
            revoked_at_ns: 900,
            reason: RevocationReason::Retired,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).unwrap();
        fleet.revoke(entry).unwrap();
        assert_eq!(
            fleet.standing(&host, 500),
            DeviceStanding::Revoked(RevocationReason::Retired)
        );
        assert_eq!(fleet.standing(&laptop, 500), DeviceStanding::Certified);

        // Only the fleet's own root can revoke its devices.
        let mut entry = RevocationEntry {
            user_public_key: other_root.verifying_key().to_bytes(),
            revoked_device_public_key: laptop,
            revoked_at_ns: 900,
            reason: RevocationReason::Stolen,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &other_root).unwrap();
        assert_eq!(fleet.revoke(entry), Err(FleetError::OtherIdentity));
        assert_eq!(fleet.standing(&laptop, 500), DeviceStanding::Certified);
    }
}
