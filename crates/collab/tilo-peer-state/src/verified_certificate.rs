use std::sync::Arc;
use tilo_user_identity::{DeviceCert, DeviceCertError, verify_device_cert, verify_device_cert_at};

/// Immutable signature-verified public certificate. This is not an authorization
/// result: expiry, revocation and governed membership remain live checks.
#[derive(Clone)]
pub(crate) struct VerifiedCertificate(Arc<DeviceCert>);

impl VerifiedCertificate {
    pub fn new(certificate: DeviceCert, now_ns: u64) -> Result<Self, String> {
        Self::new_typed(certificate, now_ns).map_err(|error| error.to_string())
    }

    pub(crate) fn new_typed(certificate: DeviceCert, now_ns: u64) -> Result<Self, DeviceCertError> {
        #[cfg(test)]
        crate::state::CERTIFICATE_VERIFICATIONS
            .with(|count| count.set(count.get().saturating_add(1)));
        verify_device_cert_at(&certificate, now_ns)?;
        Ok(Self(Arc::new(certificate)))
    }

    pub(crate) fn historical_typed(certificate: DeviceCert) -> Result<Self, DeviceCertError> {
        #[cfg(test)]
        crate::state::CERTIFICATE_VERIFICATIONS
            .with(|count| count.set(count.get().saturating_add(1)));
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
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    fn certificate() -> DeviceCert {
        let root = SigningKey::from_bytes(&[31; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: SigningKey::from_bytes(&[32; 32]).verifying_key().to_bytes(),
            device_label: "verified".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: Some(10),
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    }

    #[test]
    #[expect(
        clippy::redundant_clone,
        reason = "the clones are the subject: each must re-check validity without re-verifying"
    )]
    fn immutable_clones_check_the_current_expiry_boundary() {
        let verified = VerifiedCertificate::new(certificate(), 9).unwrap();
        crate::state::CERTIFICATE_VERIFICATIONS.with(|count| count.set(0));
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
            crate::state::CERTIFICATE_VERIFICATIONS.with(std::cell::Cell::get),
            0
        );
        assert!(VerifiedCertificate::new(certificate(), 10).is_err());
    }

    #[test]
    fn typed_current_certificate_checks_time() {
        let verified = VerifiedCertificate::new(certificate(), 9).unwrap();
        assert_eq!(verified.current_typed(9).unwrap(), verified.as_ref());
        assert!(matches!(
            verified.current_typed(10),
            Err(DeviceCertError::Expired)
        ));
        assert!(matches!(
            verified.current_typed(0),
            Err(DeviceCertError::NotYetValid)
        ));
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
        for forged in [bad_signature, bad_label, bad_expiry, bad_key] {
            assert!(VerifiedCertificate::new(forged, 9).is_err());
        }
    }
}
