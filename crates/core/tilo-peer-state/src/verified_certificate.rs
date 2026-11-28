use std::sync::Arc;
use tilo_user_identity::{DeviceCert, DeviceCertError, verify_device_cert, verify_device_cert_at};

/// Immutable signature-verified public certificate. This is not an authorization
/// result: expiry, revocation and the member list remain live checks.
#[derive(Clone)]
pub(crate) struct VerifiedCertificate(Arc<DeviceCert>);

impl VerifiedCertificate {
    pub fn new(certificate: DeviceCert, now_ns: u64) -> Result<Self, String> {
        Self::new_typed(certificate, now_ns).map_err(|error| error.to_string())
    }

    pub(crate) fn new_typed(certificate: DeviceCert, now_ns: u64) -> Result<Self, DeviceCertError> {
        verify_device_cert_at(&certificate, now_ns)?;
        Ok(Self(Arc::new(certificate)))
    }

    pub(crate) fn historical_typed(certificate: DeviceCert) -> Result<Self, DeviceCertError> {
        verify_device_cert(&certificate)?;
        Ok(Self(Arc::new(certificate)))
    }

    /// The same live validity checks without allocating an error diagnostic.
    pub fn current_typed(&self, now_ns: u64) -> Result<&DeviceCert, DeviceCertError> {
        if self
            .expires_at_ns
            .is_some_and(|expires_at_ns| expires_at_ns <= self.issued_at_ns)
        {
            return Err(DeviceCertError::InvalidValidity);
        }
        if now_ns < self.issued_at_ns {
            return Err(DeviceCertError::NotYetValid);
        }
        if self.is_expired_at(now_ns) {
            return Err(DeviceCertError::Expired);
        }
        Ok(self)
    }
}

impl std::ops::Deref for VerifiedCertificate {
    type Target = DeviceCert;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<DeviceCert> for VerifiedCertificate {
    fn as_ref(&self) -> &DeviceCert {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    use crate::fixture::member_cert_with;
    fn certificate() -> DeviceCert {
        let root = SigningKey::from_bytes(&[31; 32]);
        member_cert_with(
            &root,
            &SigningKey::from_bytes(&[32; 32]),
            "verified",
            1,
            Some(10),
        )
    }

    #[test]
    #[expect(
        clippy::redundant_clone,
        reason = "the clones are the subject: each must re-check validity without re-verifying"
    )]
    fn immutable_clones_check_the_current_expiry_boundary() {
        let verified = VerifiedCertificate::new(certificate(), 9).unwrap();
        assert!(verified.clone().current_typed(9).is_ok());
        assert_eq!(
            verified.clone().current_typed(10).unwrap_err(),
            DeviceCertError::Expired
        );
        assert!(verified.current_typed(11).is_err());
        assert!(
            verified.current_typed(9).is_ok(),
            "time-dependent validity is never cached"
        );
        assert_eq!(
            verified.current_typed(9).unwrap(),
            verified.as_ref(),
            "a current certificate is answered as the one that was verified"
        );
        assert!(
            matches!(verified.current_typed(0), Err(DeviceCertError::NotYetValid)),
            "before its issue time"
        );
        assert!(verified.current_typed(1).is_ok(), "at its issue time");
        assert!(VerifiedCertificate::new(certificate(), 10).is_err());
        assert!(
            VerifiedCertificate::new(certificate(), 0).is_err(),
            "a certificate is not taken before its issue time"
        );
        assert!(
            VerifiedCertificate::new(certificate(), 1).is_ok(),
            "a certificate is taken at its issue time"
        );
        // Read as history, only the signature is verified: whether the
        // certificate is current stays a check at each use.
        let historical = VerifiedCertificate::historical_typed(certificate()).unwrap();
        assert_eq!(
            historical.current_typed(10).unwrap_err(),
            DeviceCertError::Expired
        );
        assert_eq!(
            historical.current_typed(0).unwrap_err(),
            DeviceCertError::NotYetValid
        );
        assert_eq!(historical.current_typed(9).unwrap(), verified.as_ref());
    }

    #[test]
    fn every_new_value_requires_a_valid_signature() {
        let valid = certificate();
        assert!(VerifiedCertificate::new(valid.clone(), 9).is_ok());
        let mut bad_signature = valid.clone();
        bad_signature.signature[0] ^= 1;
        let mut bad_label = valid.clone();
        bad_label.device_label.push('!');
        let mut bad_expiry = valid.clone();
        bad_expiry.expires_at_ns = None;
        let mut bad_key = valid;
        bad_key.device_public_key[0] ^= 1;
        for (case, forged) in [
            ("a changed signature", bad_signature),
            ("a changed label", bad_label),
            ("an expiry taken away", bad_expiry),
            ("another device key", bad_key),
        ] {
            assert!(
                VerifiedCertificate::new(forged.clone(), 9).is_err(),
                "{case}"
            );
            assert!(
                VerifiedCertificate::historical_typed(forged).is_err(),
                "{case}, read as history"
            );
        }
    }
}
