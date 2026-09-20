use tilo_user_identity::{DeviceCert, DeviceCertError, verify_device_cert_at};
use std::sync::Arc;

/// Immutable signature-verified public certificate. This is not an authorization
/// result: expiry, revocation and governed membership remain live checks.
#[derive(Clone)]
pub(crate) struct VerifiedCertificate(Arc<DeviceCert>);

impl VerifiedCertificate {
    pub fn new(certificate: DeviceCert, now_ns: u64) -> Result<Self, String> {
        #[cfg(test)]
        crate::state::CERTIFICATE_VERIFICATIONS.with(|count| count.set(count.get() + 1));
        verify_device_cert_at(&certificate, now_ns).map_err(|error| error.to_string())?;
        Ok(Self(Arc::new(certificate)))
    }

    pub fn current(&self, now_ns: u64) -> Result<&DeviceCert, String> {
        if self.is_expired_at(now_ns) {
            return Err(DeviceCertError::Expired.to_string());
        }
        Ok(self)
    }
}

impl std::ops::Deref for VerifiedCertificate {
    type Target = DeviceCert;
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl AsRef<DeviceCert> for VerifiedCertificate {
    fn as_ref(&self) -> &DeviceCert { self }
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
            device_label: "verified".into(), capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1, expires_at_ns: Some(10), signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    }

    #[test]
    fn immutable_clones_check_the_current_expiry_boundary() {
        let verified = VerifiedCertificate::new(certificate(), 9).unwrap();
        crate::state::CERTIFICATE_VERIFICATIONS.with(|count| count.set(0));
        assert!(verified.clone().current(9).is_ok());
        assert!(verified.clone().current(10).unwrap_err().contains("expired"));
        assert!(verified.current(11).is_err());
        assert!(verified.current(9).is_ok(), "time-dependent validity is never cached");
        assert_eq!(crate::state::CERTIFICATE_VERIFICATIONS.with(|count| count.get()), 0);
        assert!(VerifiedCertificate::new(certificate(), 10).is_err());
    }

    #[test]
    fn every_new_value_requires_a_valid_signature() {
        let valid = certificate();
        assert!(VerifiedCertificate::new(valid.clone(), 9).is_ok());
        let mut bad_signature = valid.clone(); bad_signature.signature[0] ^= 1;
        let mut bad_label = valid.clone(); bad_label.device_label.push('!');
        let mut bad_expiry = valid.clone(); bad_expiry.expires_at_ns = None;
        let mut bad_key = valid.clone(); bad_key.device_public_key[0] ^= 1;
        for forged in [bad_signature, bad_label, bad_expiry, bad_key] {
            assert!(VerifiedCertificate::new(forged, 9).is_err());
        }
    }
}
