//! A device certificate refused only because of the wall clock's reading.
//!
//! A certificate that verifies but is "not yet valid" or "expired" says
//! nothing about the identity it certifies: the same bytes verify again once
//! the clock reads a time inside their validity interval. Callers tell that
//! apart from a broken or missing identity with [`CertificateClockError`],
//! and wait for the clock through `tilo-device-identity`'s wall-clock wait.

use std::io;

use crate::cert::{DeviceCert, DeviceCertError};

/// A certificate whose signature verified and which was refused only
/// because the wall clock read a time outside its validity interval.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct CertificateClockError {
    /// [`DeviceCertError::NotYetValid`] or [`DeviceCertError::Expired`].
    pub reason: DeviceCertError,
    /// The certificate's issuance instant, Unix nanoseconds.
    pub valid_from_ns: u64,
    /// The certificate's expiry, Unix nanoseconds, when it has one.
    pub valid_until_ns: Option<u64>,
    /// What the wall clock read when the certificate was refused.
    pub clock_ns: u64,
}

impl CertificateClockError {
    /// The clock refusal `error` stands for, when `error` is what
    /// [`crate::verify_device_cert_at`] returned for `cert` at `clock_ns`
    /// and the refusal was about time alone.
    #[must_use]
    pub fn of(cert: &DeviceCert, clock_ns: u64, error: &DeviceCertError) -> Option<Self> {
        matches!(
            error,
            DeviceCertError::NotYetValid | DeviceCertError::Expired
        )
        .then(|| Self {
            reason: error.clone(),
            valid_from_ns: cert.issued_at_ns,
            valid_until_ns: cert.expires_at_ns,
            clock_ns,
        })
    }

    /// The instant the certificate becomes valid if the clock only runs
    /// forward: its issuance when the clock is behind it. An expired
    /// certificate has none; only a clock moved back can admit it again.
    #[must_use]
    pub fn valid_at_ns(&self) -> Option<u64> {
        (self.reason == DeviceCertError::NotYetValid).then_some(self.valid_from_ns)
    }

    /// This refusal as an [`io::Error`] that [`Self::in_io`] recognises
    /// through any caller that passes the error on unchanged.
    #[must_use]
    pub fn into_io(self) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, self)
    }

    /// The clock refusal carried by `error`, when it is one.
    #[must_use]
    pub fn in_io(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref::<Self>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert::{DeviceCapabilities, sign_device_cert, verify_device_cert_at};

    fn cert(issued_at_ns: u64, expires_at_ns: Option<u64>) -> DeviceCert {
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    }

    #[test]
    fn a_certificate_refused_for_the_clock_alone_is_told_apart_through_io_errors() {
        let cert = cert(1_000, Some(2_000));

        let behind = verify_device_cert_at(&cert, 500).unwrap_err();
        let refusal = CertificateClockError::of(&cert, 500, &behind).expect("a clock refusal");
        assert_eq!(refusal.valid_at_ns(), Some(1_000));
        let carried = refusal.clone().into_io();
        assert_eq!(carried.kind(), io::ErrorKind::InvalidData);
        assert_eq!(CertificateClockError::in_io(&carried), Some(&refusal));
        assert_eq!(carried.to_string(), "device cert is not yet valid");

        let ahead = verify_device_cert_at(&cert, 2_000).unwrap_err();
        let expired = CertificateClockError::of(&cert, 2_000, &ahead).expect("a clock refusal");
        assert_eq!(
            expired.valid_at_ns(),
            None,
            "the clock running on never admits it"
        );

        assert_eq!(
            CertificateClockError::of(&cert, 500, &DeviceCertError::BadSignature),
            None
        );
        let other = io::Error::new(
            io::ErrorKind::InvalidData,
            "device-cert signature did not verify",
        );
        assert!(CertificateClockError::in_io(&other).is_none());
    }
}
