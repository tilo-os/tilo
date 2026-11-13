//! [`HostWitness`] — a host's answer for one invocation, carried to the
//! device that runs it.
//!
//! A device a host certified for a workspace may hold a window whose actions
//! ask the person first, with nobody in front of it to ask. The host asks
//! them on their approving devices
//! ([`CertificationProtocol::confirm`](crate::CertificationPolicy)), and
//! hands the answer to the device as this: the person's decision for one
//! invocation, over the action, its arguments and the device that asked,
//! signed by the host's own device key — the key its certificate names as
//! the issuer.
//!
//! Only that signature counts. A device accepts a witness it can chain to
//! the key that certified it, for its own person, and nothing else: the
//! counter-signature of a host it does not belong to is refused like any
//! other word.
//!
//! Signing preimage: `blake3(HOST_WITNESS_PREIMAGE_TAG || postcard(view))`,
//! the same pipeline as every other signed type here.

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};

/// Signing-domain tag for [`HostWitness`]. Frozen wire ABI.
pub const HOST_WITNESS_PREIMAGE_TAG: &[u8] = b"tilo-host-witness-v1\0";

/// Longest a witness may be made to last: a person's answer to a question
/// they were shown, not a standing grant.
pub const MAX_LIFETIME_NS: u64 = 10 * 60 * 1_000_000_000;

/// The person's answer for one invocation, as the host that certified the
/// device running it heard it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostWitness {
    /// The person who answered.
    pub person_public_key: [u8; 32],
    /// The host that asked them, and signed this: the key of the
    /// certificate that issued the running device's.
    pub host_device_public_key: [u8; 32],
    /// The device that asked for the action (the agent's).
    pub caller_device_public_key: [u8; 32],
    /// The device the action runs on (the one holding the window).
    pub target_device_public_key: [u8; 32],
    /// The invocation it answers, and nothing else.
    #[serde(with = "serde_bytes")]
    pub invocation: [u8; 16],
    /// The action's provider, resource and action, and its arguments'
    /// digest: what the person was shown.
    pub provider: String,
    pub resource: String,
    pub action: String,
    #[serde(with = "serde_bytes")]
    pub argument: [u8; 32],
    /// Nanoseconds since the Unix epoch.
    pub issued_at_ns: u64,
    pub expires_at_ns: u64,
    /// Ed25519 signature by [`Self::host_device_public_key`] over
    /// [`signing_digest`]`(HOST_WITNESS_PREIMAGE_TAG, view)`.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

/// Signing view — everything except the signature.
#[derive(Serialize)]
struct HostWitnessView<'a> {
    person_public_key: &'a [u8; 32],
    host_device_public_key: &'a [u8; 32],
    caller_device_public_key: &'a [u8; 32],
    target_device_public_key: &'a [u8; 32],
    invocation: &'a [u8; 16],
    provider: &'a str,
    resource: &'a str,
    action: &'a str,
    argument: &'a [u8; 32],
    issued_at_ns: u64,
    expires_at_ns: u64,
}

/// Why a witness does not answer for an invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum HostWitnessError {
    #[error("the witness is not signed")]
    Unsigned,
    #[error("the witness's window is not a valid lifetime")]
    InvalidValidity,
    #[error("the witness is not good yet")]
    NotYetValid,
    #[error("the witness has expired")]
    Expired,
    #[error("the witness is not its host's signature")]
    Signature,
}

/// Whose answer a witness is: the person who gave it, the host that asked
/// them and signs it, and the two devices it stands between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parties {
    pub person: [u8; 32],
    pub host: [u8; 32],
    pub caller: [u8; 32],
    pub target: [u8; 32],
}

/// What the person answered: the invocation, and the action as it was shown
/// to them with its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    pub invocation: [u8; 16],
    pub provider: String,
    pub resource: String,
    pub action: String,
    pub argument: [u8; 32],
}

impl HostWitness {
    /// A witness `host` is about to sign: `parties`' person allowed
    /// `parties`' caller to run `answered` on its target.
    #[must_use]
    pub fn unsigned(
        parties: Parties,
        answered: Answered,
        (issued_at_ns, expires_at_ns): (u64, u64),
    ) -> Self {
        Self {
            person_public_key: parties.person,
            host_device_public_key: parties.host,
            caller_device_public_key: parties.caller,
            target_device_public_key: parties.target,
            invocation: answered.invocation,
            provider: answered.provider,
            resource: answered.resource,
            action: answered.action,
            argument: answered.argument,
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        }
    }

    fn view(&self) -> HostWitnessView<'_> {
        HostWitnessView {
            person_public_key: &self.person_public_key,
            host_device_public_key: &self.host_device_public_key,
            caller_device_public_key: &self.caller_device_public_key,
            target_device_public_key: &self.target_device_public_key,
            invocation: &self.invocation,
            provider: &self.provider,
            resource: &self.resource,
            action: &self.action,
            argument: &self.argument,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        }
    }

    /// The digest the host signs.
    ///
    /// # Errors
    ///
    /// [`CanonicalError`] if the witness cannot be canonically encoded
    /// (effectively unreachable).
    pub fn signing_digest(&self) -> Result<[u8; 32], CanonicalError> {
        signing_digest(HOST_WITNESS_PREIMAGE_TAG, &self.view())
    }

    /// Sign this with `key`, which must be the key
    /// [`Self::host_device_public_key`] names.
    ///
    /// # Errors
    ///
    /// [`HostWitnessError::Signature`] when it is another key, and
    /// [`HostWitnessError::InvalidValidity`] for an empty or over-long
    /// window.
    pub fn sign(&mut self, key: &ed25519_dalek::SigningKey) -> Result<(), HostWitnessError> {
        if key.verifying_key().to_bytes() != self.host_device_public_key {
            return Err(HostWitnessError::Signature);
        }
        let lifetime = self.expires_at_ns.saturating_sub(self.issued_at_ns);
        if lifetime == 0 || lifetime > MAX_LIFETIME_NS {
            return Err(HostWitnessError::InvalidValidity);
        }
        let digest = self
            .signing_digest()
            .map_err(|_| HostWitnessError::Signature)?;
        self.signature = key.sign(&digest).to_bytes();
        Ok(())
    }

    /// Whether this is its host's signature, live at `now_ns`.
    ///
    /// # Errors
    ///
    /// [`HostWitnessError`] naming what is wrong with it.
    pub fn verify(&self, now_ns: u64) -> Result<(), HostWitnessError> {
        if self.signature == [0; 64] {
            return Err(HostWitnessError::Unsigned);
        }
        let lifetime = self.expires_at_ns.saturating_sub(self.issued_at_ns);
        if lifetime == 0 || lifetime > MAX_LIFETIME_NS {
            return Err(HostWitnessError::InvalidValidity);
        }
        if now_ns < self.issued_at_ns {
            return Err(HostWitnessError::NotYetValid);
        }
        if now_ns >= self.expires_at_ns {
            return Err(HostWitnessError::Expired);
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.host_device_public_key)
            .map_err(|_| HostWitnessError::Signature)?;
        let digest = self
            .signing_digest()
            .map_err(|_| HostWitnessError::Signature)?;
        key.verify(
            &digest,
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| HostWitnessError::Signature)
    }

    /// Whether this answers for `certificate`'s device: the person that
    /// device belongs to let it run, the key that issued its certificate
    /// signed the answer, and it reaches the certificate's own device and no
    /// other. A host that certified nothing answers for nothing.
    #[must_use]
    pub fn stands_for(&self, certificate: &crate::DeviceCert, now_ns: u64) -> bool {
        let Some(issuer) = certificate.issuer.as_ref() else {
            return false;
        };
        self.person_public_key == certificate.user_public_key
            && self.host_device_public_key == issuer.device_public_key
            && self.target_device_public_key == certificate.device_public_key
            && self.verify(now_ns).is_ok()
    }

    /// This witness as the token a caller carries back in
    /// `Confirmation::Witnessed`: its fields in hex.
    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_vec(self).map_or_else(|_| String::new(), hex::encode)
    }

    /// The witness a token names, when it names one at all.
    #[must_use]
    pub fn decode(token: &str) -> Option<Self> {
        let bytes = hex::decode(token).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a test's unwrap is its assertion")]

    use super::*;

    fn key(byte: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[byte; 32])
    }

    fn answered() -> Answered {
        Answered {
            invocation: [3; 16],
            provider: "browser@42".into(),
            resource: "browser.page".into(),
            action: "submit-page".into(),
            argument: [4; 32],
        }
    }

    fn witness(host: u8, issued_at_ns: u64) -> HostWitness {
        HostWitness::unsigned(
            Parties {
                person: [9; 32],
                host: key(host).verifying_key().to_bytes(),
                caller: [7; 32],
                target: [8; 32],
            },
            answered(),
            (
                issued_at_ns,
                issued_at_ns.saturating_add(60 * 1_000_000_000),
            ),
        )
    }

    #[test]
    fn a_host_s_witness_verifies_for_its_window_and_arguments() {
        let mut signed = witness(2, 1_000);
        signed.sign(&key(2)).unwrap();
        assert_eq!(signed.verify(2_000), Ok(()));
        let decoded = HostWitness::decode(&signed.encode()).expect("a token names its witness");
        assert_eq!(decoded, signed);
    }

    #[test]
    fn another_key_s_witness_and_a_changed_word_are_refused() {
        let mut signed = witness(2, 1_000);
        signed.sign(&key(2)).unwrap();
        let mut changed = signed.clone();
        changed.action = "fill-page".into();
        assert_eq!(changed.verify(2_000), Err(HostWitnessError::Signature));
        let mut elsewhere = witness(5, 1_000);
        elsewhere.sign(&key(5)).unwrap();
        assert_eq!(elsewhere.verify(2_000), Ok(()), "it is that host's own");
        assert_eq!(HostWitness::decode("not a witness"), None);
    }

    #[test]
    fn a_witness_is_only_good_for_its_window() {
        let mut signed = witness(2, 1_000);
        signed.sign(&key(2)).unwrap();
        assert_eq!(
            signed.verify(1_000 + 61 * 1_000_000_000),
            Err(HostWitnessError::Expired)
        );
        assert_eq!(signed.verify(500), Err(HostWitnessError::NotYetValid));
        let mut long = witness(2, 1_000);
        long.expires_at_ns = long.issued_at_ns + MAX_LIFETIME_NS + 1;
        assert_eq!(long.sign(&key(2)), Err(HostWitnessError::InvalidValidity));
    }

    /// The certificate a host certified a device with: the device's, issued
    /// by the host's own key.
    fn certified_by(host: &ed25519_dalek::SigningKey) -> crate::DeviceCert {
        let root = key(9);
        let device = key(4);
        let now = 500;
        let mut stand_in = crate::prepare_stand_in_cert(
            root.verifying_key().to_bytes(),
            host.verifying_key().to_bytes(),
            "the host",
            now,
            now + 3_600 * 1_000_000_000,
        );
        crate::sign_device_cert(&mut stand_in, &root).unwrap();
        let mut cert = crate::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Browser".into(),
            capabilities: crate::DeviceCapabilities::member(),
            issued_at_ns: now,
            expires_at_ns: Some(now + 600 * 1_000_000_000),
            signature: [0; 64],
            issuer: Some(Box::new(stand_in)),
        };
        crate::sign_device_cert(&mut cert, host).unwrap();
        cert
    }

    #[test]
    fn only_the_host_that_certified_a_device_answers_for_it() {
        let host = key(2);
        let cert = certified_by(&host);
        let mut witness = HostWitness::unsigned(
            Parties {
                person: cert.user_public_key,
                host: host.verifying_key().to_bytes(),
                caller: [7; 32],
                target: cert.device_public_key,
            },
            answered(),
            (1_000, 1_000 + 60 * 1_000_000_000),
        );
        witness.sign(&host).unwrap();
        assert!(witness.stands_for(&cert, 2_000));

        // Another host's word, for this very device, is no answer at all.
        let elsewhere = key(5);
        let mut other = witness.clone();
        other.host_device_public_key = elsewhere.verifying_key().to_bytes();
        other.sign(&elsewhere).unwrap();
        assert!(!other.stands_for(&cert, 2_000));
        // Nor is an answer for another person, or for another device.
        let mut stranger = witness.clone();
        stranger.person_public_key = [0x42; 32];
        stranger.sign(&host).unwrap();
        assert!(!stranger.stands_for(&cert, 2_000));
        let mut elsewhere_device = witness.clone();
        elsewhere_device.target_device_public_key = [0x43; 32];
        elsewhere_device.sign(&host).unwrap();
        assert!(!elsewhere_device.stands_for(&cert, 2_000));
        // And a device no host certified has nobody to answer for it.
        let mut uncertified = cert;
        uncertified.issuer = None;
        assert!(!witness.stands_for(&uncertified, 2_000));
    }

    #[test]
    fn a_host_signs_only_with_the_key_its_certificate_names() {
        let mut signed = witness(2, 1_000);
        assert_eq!(signed.sign(&key(3)), Err(HostWitnessError::Signature));
        assert_eq!(signed.verify(2_000), Err(HostWitnessError::Unsigned));
    }
}
