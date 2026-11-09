//! Signing a person in to a web service with a device they already have.
//!
//! A service that is not one of the person's devices (a hosting provider's
//! sign-in page, say) learns who the person is the way a relay does at its
//! handshake: one of their devices signs the service's fresh challenge with
//! its own key, and shows the certificate their identity root gave that key.
//! The service checks the certificate under the root, that the device is not
//! revoked, and the signature; the root is the person. No account, password
//! or passkey assertion is involved, and the root's private key never leaves
//! where it is.
//!
//! The proof names the service (its origin) and its challenge, so it is
//! useless anywhere else and anywhen else: a proof for one service cannot
//! sign the person in to another, and a challenge is the service's own and
//! spent once.

use ed25519_dalek::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::signing_digest;
use crate::{DeviceCert, RevocationList, verify_device_cert_at};

/// Frozen domain tag for sign-in signatures.
pub const SIGN_IN_TAG: &[u8] = b"tilo-sign-in-v1\0";
/// The longest an audience (a service's origin) may be.
pub const MAX_AUDIENCE_BYTES: usize = 256;

/// The audience a device signs for when it opens a session at a workspace's
/// agent endpoint (a host's `tilo-runtime`), which challenges every caller
/// the same way a web service does. It names the workspace, so a proof for
/// one workspace's endpoint, or for any web service (whose audience is an
/// origin), is no proof for another.
#[must_use]
pub fn agent_endpoint_audience(workspace: &str) -> String {
    format!("tilo-agent-endpoint:{workspace}")
}

/// A device's signature over a service's sign-in challenge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignInProof {
    /// The signing device's certificate under the person's root.
    pub cert: DeviceCert,
    /// The service signed in to: its origin, as it names itself.
    pub audience: String,
    /// The service's challenge.
    pub challenge: [u8; 32],
    /// The device key's Ed25519 signature over [`sign_in_digest`].
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct SignInView<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    audience: &'a str,
    challenge: &'a [u8; 32],
}

/// Why a sign-in proof was refused.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SignInError {
    #[error("the proof is for another service or challenge")]
    WrongChallenge,
    #[error("the device's certificate does not hold: {0}")]
    Certificate(String),
    #[error("the device was revoked")]
    Revoked,
    #[error("the signature does not verify")]
    Signature,
    #[error("the audience is empty or too long")]
    Audience,
}

/// What the device signs: the person, the device, the service and its
/// challenge, under [`SIGN_IN_TAG`].
///
/// # Errors
/// The audience is empty or longer than [`MAX_AUDIENCE_BYTES`].
pub fn sign_in_digest(
    cert: &DeviceCert,
    audience: &str,
    challenge: &[u8; 32],
) -> Result<[u8; 32], SignInError> {
    if audience.is_empty() || audience.len() > MAX_AUDIENCE_BYTES {
        return Err(SignInError::Audience);
    }
    signing_digest(
        SIGN_IN_TAG,
        &SignInView {
            user_public_key: &cert.user_public_key,
            device_public_key: &cert.device_public_key,
            audience,
            challenge,
        },
    )
    .map_err(|_| SignInError::Audience)
}

impl SignInProof {
    /// Sign `challenge` from `audience` with the device key `key`, which
    /// `cert` certifies.
    ///
    /// # Errors
    /// `key` is not the certified device's, or the audience is malformed.
    pub fn sign(
        cert: DeviceCert,
        key: &ed25519_dalek::SigningKey,
        audience: &str,
        challenge: [u8; 32],
    ) -> Result<Self, SignInError> {
        if key.verifying_key().to_bytes() != cert.device_public_key {
            return Err(SignInError::Signature);
        }
        let digest = sign_in_digest(&cert, audience, &challenge)?;
        Ok(Self {
            signature: key.sign(&digest).to_bytes(),
            cert,
            audience: audience.to_owned(),
            challenge,
        })
    }

    /// Check this proof answers `challenge` from `audience`, signed by a
    /// device the person's root certified and has not revoked by `now_ns`
    /// (`revocations` is the person's own list). Answers the person: their
    /// identity root key.
    ///
    /// # Errors
    /// [`SignInError`]: which check failed.
    pub fn verify(
        &self,
        audience: &str,
        challenge: &[u8; 32],
        revocations: &RevocationList,
        now_ns: u64,
    ) -> Result<[u8; 32], SignInError> {
        if self.audience != audience || &self.challenge != challenge {
            return Err(SignInError::WrongChallenge);
        }
        verify_device_cert_at(&self.cert, now_ns)
            .map_err(|error| SignInError::Certificate(error.to_string()))?;
        if revocations.is_revoked_at(&self.cert.device_public_key, now_ns) {
            return Err(SignInError::Revoked);
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.cert.device_public_key)
            .map_err(|_| SignInError::Signature)?;
        let digest = sign_in_digest(&self.cert, audience, challenge)?;
        key.verify(
            &digest,
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| SignInError::Signature)?;
        Ok(self.cert.user_public_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceCapabilities, sign_device_cert};

    fn device(root: &ed25519_dalek::SigningKey, key: &ed25519_dalek::SigningKey) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    #[test]
    fn a_certified_device_signs_its_person_in_once_for_one_service() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let cert = device(&root, &key);
        let audience = "https://door.example";
        let proof = SignInProof::sign(cert.clone(), &key, audience, [7; 32]).unwrap();
        let none = RevocationList::new();
        assert_eq!(
            proof.verify(audience, &[7; 32], &none, 10),
            Ok(root.verifying_key().to_bytes())
        );
        assert_eq!(
            proof.verify("https://other.example", &[7; 32], &none, 10),
            Err(SignInError::WrongChallenge)
        );
        assert_eq!(
            proof.verify(audience, &[8; 32], &none, 10),
            Err(SignInError::WrongChallenge)
        );
        // Another device may not borrow the certificate.
        let other = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        assert!(SignInProof::sign(cert.clone(), &other, audience, [7; 32]).is_err());
        let mut forged = proof.clone();
        forged.signature = other
            .sign(&sign_in_digest(&cert, audience, &[7; 32]).unwrap())
            .to_bytes();
        assert_eq!(
            forged.verify(audience, &[7; 32], &none, 10),
            Err(SignInError::Signature)
        );
        // A device certified by another root is another person.
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let mut claimed = proof;
        claimed.cert.user_public_key = stranger.verifying_key().to_bytes();
        assert!(matches!(
            claimed.verify(audience, &[7; 32], &none, 10),
            Err(SignInError::Certificate(_))
        ));
    }

    #[test]
    fn a_revoked_device_signs_nobody_in() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let cert = device(&root, &key);
        let proof = SignInProof::sign(cert, &key, "https://door.example", [7; 32]).unwrap();
        let mut entry = crate::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: key.verifying_key().to_bytes(),
            revoked_at_ns: 5,
            reason: crate::RevocationReason::Lost,
            signature: [0; 64],
        };
        crate::sign_revocation_entry(&mut entry, &root).unwrap();
        let mut revoked = RevocationList::new();
        revoked.insert(entry).unwrap();
        assert_eq!(
            proof.verify("https://door.example", &[7; 32], &revoked, 10),
            Err(SignInError::Revoked)
        );
    }
}
