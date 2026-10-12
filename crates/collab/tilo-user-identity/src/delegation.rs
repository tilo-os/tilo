//! Signing as a person from a device that does not hold their root key.
//!
//! The identity root key stays on the devices that can admit others. Every
//! other device acts for the person with its own device key, presenting the
//! [`DeviceCert`] the root issued when it enrolled. A verifier accepts such a
//! signature through one chain:
//!
//! ```text
//! user root ──signs──▶ DeviceCert(device key, capabilities, validity)
//!                         └─ device key ──signs──▶ the operation
//! ```
//!
//! [`verify_chain`] checks the certificate half of that chain against a
//! [`ChainPolicy`]: the expected root, the [`Authority`] the operation needs,
//! the root's revocations, and the current time. [`verify_delegated`] adds the
//! operation signature. Both are the only place these rules are spelled out;
//! verifiers across the mesh call them instead of re-deriving the checks.
//!
//! A revocation is permanent. It applies to every signature the device key
//! makes, whatever time the signer claims, because a stolen key can back-date
//! what it signs. Only a revocation the same root signed counts: another
//! identity can neither revoke this person's device nor, by publishing a
//! revocation of its own, hide one.
//!
//! Some operations need the root itself — admitting and revoking devices,
//! issuing certificates, rotating or recovering the identity. They are never
//! delegated; [`RootOnly`] names them for a clear refusal on devices without
//! the root.

use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};
use crate::cert::{DeviceCapabilities, DeviceCert, DeviceCertError, verify_device_cert_at};
use crate::revocation::{RevocationEntry, RevocationList};

/// Signing-domain tag for an operation a device signs as its person.
pub const DELEGATED_SIGNATURE_TAG: &[u8] = b"tilo-delegated-signature-v1\0";

/// Longest operation domain a delegated signature accepts.
pub const MAX_DELEGATION_DOMAIN_BYTES: usize = 128;

/// What an operation needs the signing device's certificate to grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Authority {
    /// Be one of the person's certified devices, whatever it may do. For
    /// transport admission, where each operation then decides further; it
    /// never lets a device sign anything as the person.
    Device,
    /// Act for the person: edit their documents, sign their app actions,
    /// join their rooms. Every personal device grants it; a dedicated host
    /// (certified to host, not to own) does not speak as the person.
    Person,
    /// Create, govern, hand over or delete the person's workspaces.
    OwnWorkspaces,
    /// Approve another device into the person's fleet.
    AdmitDevices,
    /// Serve as an always-on host for the person's workspaces.
    Host,
    /// Receive every one of the person's workspaces, not only the ones it
    /// was given.
    ReadAllWorkspaces,
}

impl Authority {
    /// Whether `capabilities` grant this authority.
    #[must_use]
    pub fn granted_by(self, capabilities: &DeviceCapabilities) -> bool {
        match self {
            Self::Device => true,
            Self::Person => {
                capabilities.can_own_workspaces
                    || (!capabilities.can_act_as_host && !capabilities.can_administer_devices)
            }
            Self::OwnWorkspaces => capabilities.can_own_workspaces,
            Self::AdmitDevices => capabilities.can_admit_devices,
            Self::Host => capabilities.can_act_as_host,
            Self::ReadAllWorkspaces => capabilities.can_read_all_workspaces,
        }
    }

    /// The capability this authority names, for refusal messages.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Device => "be one of the person's devices",
            Self::Person => "act for the person",
            Self::OwnWorkspaces => "own workspaces",
            Self::AdmitDevices => "admit devices",
            Self::Host => "act as a host",
            Self::ReadAllWorkspaces => "read every workspace",
        }
    }
}

/// Operations only the identity root key may sign. A device without the
/// root refuses them with [`RootOnly::refusal`] rather than signing with its
/// device key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootOnly {
    /// Certify a new device.
    AdmitDevice,
    /// Revoke an enrolled device.
    RevokeDevice,
    /// Certify this device's key under an identity it does not yet belong to.
    CertifyDevice,
    /// Export the identity's recovery backup.
    BackupIdentity,
    /// Replace or recover the identity's keys.
    RotateIdentity,
    /// Certify a device's disk-unlock boot key.
    CertifyBootKey,
    /// Approve a device's disk-unlock policy.
    SignUnlockSetting,
    /// Export the identity's secret-sync key.
    ExportSecretSyncKey,
}

impl RootOnly {
    fn action(self) -> &'static str {
        match self {
            Self::AdmitDevice => "Approving a device",
            Self::RevokeDevice => "Revoking a device",
            Self::CertifyDevice => "Certifying this device",
            Self::BackupIdentity => "Backing up the identity",
            Self::RotateIdentity => "Changing the identity's keys",
            Self::CertifyBootKey => "Certifying a boot key",
            Self::SignUnlockSetting => "Changing disk-unlock policy",
            Self::ExportSecretSyncKey => "Reading the secret-sync key",
        }
    }

    /// A refusal that names where the operation can run. `root_holders` are
    /// the labels of the devices whose certificates show they hold the root
    /// (they may admit devices); none known yields a generic pointer.
    #[must_use]
    pub fn refusal<'a>(self, root_holders: impl IntoIterator<Item = &'a str>) -> String {
        let holders: Vec<&str> = root_holders.into_iter().collect();
        let place = match holders.as_slice() {
            [] => "a device that holds the identity (one that can approve devices)".to_string(),
            [one] => format!("'{one}'"),
            many => many
                .iter()
                .map(|label| format!("'{label}'"))
                .collect::<Vec<_>>()
                .join(" or "),
        };
        format!(
            "{} needs the identity's root key, which this device does not hold; do this on {place}",
            self.action()
        )
    }
}

/// Labels of the certified devices that may admit others — the devices that
/// hold the root — skipping revoked or expired ones.
pub fn root_holder_labels<'a>(
    certificates: impl IntoIterator<Item = &'a DeviceCert>,
    revocations: &RevocationList,
    now_ns: u64,
) -> Vec<&'a str> {
    certificates
        .into_iter()
        .filter(|cert| cert.capabilities.can_admit_devices)
        .filter(|cert| verify_device_cert_at(cert, now_ns).is_ok())
        .filter(|cert| {
            revocations
                .revocation_of(&cert.user_public_key, &cert.device_public_key)
                .is_none()
        })
        .map(|cert| cert.device_label.as_str())
        .collect()
}

/// What a verifier requires of a device acting for a person.
#[derive(Clone, Copy, Debug)]
pub struct ChainPolicy<'a> {
    /// The identity the signature must speak for.
    pub user_public_key: &'a [u8; 32],
    /// What the operation needs the certificate to grant.
    pub authority: Authority,
    /// The verified revocations known for that identity.
    pub revocations: &'a RevocationList,
    /// The verifier's clock, nanoseconds since the Unix epoch.
    pub now_ns: u64,
}

/// Why a device-to-person chain was refused.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ChainError {
    /// The certificate names another identity.
    #[error("the device certificate belongs to another identity")]
    OtherIdentity,
    /// The certificate names another device key than the one that signed.
    #[error("the device certificate names a different device")]
    OtherDevice,
    /// The certificate's root signature or validity window failed.
    #[error("device certificate: {0}")]
    Certificate(#[from] DeviceCertError),
    /// The identity revoked this device.
    #[error("the identity revoked this device")]
    Revoked,
    /// The certificate does not grant what the operation needs.
    #[error("the device certificate does not allow this device to {}", .0.describe())]
    NotPermitted(Authority),
    /// The device key's signature over the operation did not verify.
    #[error("the device signature did not verify")]
    BadSignature,
    /// The operation domain is empty or too long.
    #[error("invalid delegated-signature domain")]
    InvalidDomain,
    /// Canonical encoding failed.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Verify that `cert` lets its device act for `policy.user_public_key` with
/// `policy.authority` at `policy.now_ns`: the root signed it, it is inside
/// its validity window, the root has not revoked the device, and its
/// capabilities grant the authority.
///
/// # Errors
///
/// Returns the first failing link as a [`ChainError`].
pub fn verify_chain(cert: &DeviceCert, policy: &ChainPolicy<'_>) -> Result<(), ChainError> {
    if &cert.user_public_key != policy.user_public_key {
        return Err(ChainError::OtherIdentity);
    }
    verify_device_cert_at(cert, policy.now_ns)?;
    if policy
        .revocations
        .revocation_of(&cert.user_public_key, &cert.device_public_key)
        .is_some()
    {
        return Err(ChainError::Revoked);
    }
    if !policy.authority.granted_by(&cert.capabilities) {
        return Err(ChainError::NotPermitted(policy.authority));
    }
    Ok(())
}

/// A signature a device made for its person, with the certificate that
/// chains the device key to the person's root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegatedSignature {
    /// The root-issued certificate of the signing device.
    pub cert: DeviceCert,
    /// Ed25519 signature by `cert.device_public_key` over
    /// [`delegated_signing_digest`].
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

impl DelegatedSignature {
    /// The identity this signature speaks for.
    #[must_use]
    pub fn user_public_key(&self) -> &[u8; 32] {
        &self.cert.user_public_key
    }

    /// The device key that made it.
    #[must_use]
    pub fn device_public_key(&self) -> &[u8; 32] {
        &self.cert.device_public_key
    }
}

#[derive(Serialize)]
struct DelegatedView<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    domain: &'a str,
    #[serde(with = "serde_bytes")]
    message: &'a [u8],
}

fn valid_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= MAX_DELEGATION_DOMAIN_BYTES
        && domain.bytes().all(|byte| byte.is_ascii_graphic())
}

/// The digest a device signs for `message` in operation `domain` as the
/// person `user_public_key`. Binding the identity and the device key means a
/// signature cannot be re-presented for another person the same key serves,
/// nor under another certificate.
///
/// # Errors
///
/// Returns [`ChainError::InvalidDomain`] for an empty, over-long or
/// non-printable domain.
pub fn delegated_signing_digest(
    user_public_key: &[u8; 32],
    device_public_key: &[u8; 32],
    domain: &str,
    message: &[u8],
) -> Result<[u8; 32], ChainError> {
    if !valid_domain(domain) {
        return Err(ChainError::InvalidDomain);
    }
    Ok(signing_digest(
        DELEGATED_SIGNATURE_TAG,
        &DelegatedView {
            user_public_key,
            device_public_key,
            domain,
            message,
        },
    )?)
}

/// Verify a delegated signature over `message` in `domain`: the chain under
/// `policy` ([`verify_chain`]) and the device key's signature.
///
/// # Errors
///
/// Returns the first failing check as a [`ChainError`].
pub fn verify_delegated(
    signature: &DelegatedSignature,
    domain: &str,
    message: &[u8],
    policy: &ChainPolicy<'_>,
) -> Result<(), ChainError> {
    verify_chain(&signature.cert, policy)?;
    let digest = delegated_signing_digest(
        &signature.cert.user_public_key,
        &signature.cert.device_public_key,
        domain,
        message,
    )?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&signature.cert.device_public_key)
        .map_err(|_| ChainError::BadSignature)?;
    key.verify_strict(
        &digest,
        &ed25519_dalek::Signature::from_bytes(&signature.signature),
    )
    .map_err(|_| ChainError::BadSignature)
}

/// Something that signs operations as a person.
pub trait IdentitySigner {
    /// The identity this signer speaks for.
    fn user_public_key(&self) -> [u8; 32];

    /// Sign `message` in operation `domain` as the person.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError`] when the signer may not or cannot sign.
    fn sign_as_person(
        &self,
        domain: &str,
        message: &[u8],
    ) -> Result<DelegatedSignature, ChainError>;
}

/// A device key with the certificate its person's root issued for it.
///
/// Construction checks that the key is the certified one, so a signer can
/// never present a certificate for a key it does not hold.
pub struct DeviceSigner {
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
}

impl std::fmt::Debug for DeviceSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceSigner")
            .field("cert", &self.cert)
            .finish_non_exhaustive()
    }
}

impl DeviceSigner {
    /// Pair `key` with its certificate.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError::OtherDevice`] when `cert` certifies another key,
    /// or [`ChainError::Certificate`] when its root signature fails.
    pub fn new(key: ed25519_dalek::SigningKey, cert: DeviceCert) -> Result<Self, ChainError> {
        if key.verifying_key().to_bytes() != cert.device_public_key {
            return Err(ChainError::OtherDevice);
        }
        crate::cert::verify_device_cert(&cert)?;
        Ok(Self { key, cert })
    }

    /// The certificate this signer presents.
    #[must_use]
    pub fn cert(&self) -> &DeviceCert {
        &self.cert
    }

    /// The device key, for transport handshakes that prove possession of it
    /// directly.
    #[must_use]
    pub fn device_key(&self) -> &ed25519_dalek::SigningKey {
        &self.key
    }

    /// Check this signer's own chain under `policy` before lending it out:
    /// a device that was revoked or lacks the authority refuses locally
    /// instead of producing signatures every verifier would drop.
    ///
    /// # Errors
    ///
    /// Returns what [`verify_chain`] returns.
    pub fn check(&self, policy: &ChainPolicy<'_>) -> Result<(), ChainError> {
        verify_chain(&self.cert, policy)
    }

    /// Sign `framed` with the device key, for wire formats that frame their
    /// own bytes. `framed` must begin with the format's own NUL-terminated
    /// domain tag and be longer than 32 bytes, so the signature can stand for
    /// nothing else this key signs — a delegated signature or a transport
    /// challenge, which both cover a 32-byte digest, in particular. Never
    /// pass bytes an app chose without framing them first.
    #[must_use]
    pub fn sign_framed_bytes(&self, framed: &[u8]) -> [u8; 64] {
        self.key.sign(framed).to_bytes()
    }
}

impl IdentitySigner for DeviceSigner {
    fn user_public_key(&self) -> [u8; 32] {
        self.cert.user_public_key
    }

    fn sign_as_person(
        &self,
        domain: &str,
        message: &[u8],
    ) -> Result<DelegatedSignature, ChainError> {
        let digest = delegated_signing_digest(
            &self.cert.user_public_key,
            &self.cert.device_public_key,
            domain,
            message,
        )?;
        Ok(DelegatedSignature {
            cert: self.cert.clone(),
            signature: self.key.sign(&digest).to_bytes(),
        })
    }
}

/// The revocations of one identity that name `device`, for callers holding
/// raw entries rather than a [`RevocationList`].
pub fn revoked_in<'a>(
    entries: impl IntoIterator<Item = &'a RevocationEntry>,
    user_public_key: &[u8; 32],
    device_public_key: &[u8; 32],
) -> bool {
    entries.into_iter().any(|entry| {
        &entry.user_public_key == user_public_key
            && &entry.revoked_device_public_key == device_public_key
            && crate::revocation::verify_revocation_entry(entry).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert::sign_device_cert;
    use crate::revocation::{RevocationReason, sign_revocation_entry};
    use crate::{DeviceIdentity, UserIdentity};

    const NOW: u64 = 1_000_000;

    struct Person {
        root: ed25519_dalek::SigningKey,
        user: [u8; 32],
    }

    fn person(label: &str) -> Person {
        let (root, user) = UserIdentity::generate(label, 1);
        Person {
            root,
            user: user.public_key,
        }
    }

    fn device(
        person: &Person,
        capabilities: DeviceCapabilities,
        expires_at_ns: Option<u64>,
    ) -> DeviceSigner {
        let (key, identity) = DeviceIdentity::generate("Board B", 2);
        let mut cert = DeviceCert {
            user_public_key: person.user,
            device_public_key: identity.public_key,
            device_label: identity.label,
            capabilities,
            issued_at_ns: 100,
            expires_at_ns,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &person.root).unwrap();
        DeviceSigner::new(key, cert).unwrap()
    }

    fn revoke(person: &Person, device: &[u8; 32], at: u64) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: person.user,
            revoked_device_public_key: *device,
            revoked_at_ns: at,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &person.root).unwrap();
        entry
    }

    fn policy<'a>(
        user: &'a [u8; 32],
        authority: Authority,
        revocations: &'a RevocationList,
    ) -> ChainPolicy<'a> {
        ChainPolicy {
            user_public_key: user,
            authority,
            revocations,
            now_ns: NOW,
        }
    }

    #[test]
    fn a_joined_device_signs_for_its_person_without_the_root() {
        let ada = person("Ada");
        let board = device(&ada, DeviceCapabilities::joined(), None);
        let none = RevocationList::new();
        let signed = board.sign_as_person("tilo.test.edit", b"hello").unwrap();
        assert_eq!(signed.user_public_key(), &ada.user);
        verify_delegated(
            &signed,
            "tilo.test.edit",
            b"hello",
            &policy(&ada.user, Authority::Person, &none),
        )
        .unwrap();
    }

    #[test]
    fn device_administration_does_not_grant_person_or_enrollment_authority() {
        let ada = person("Ada");
        let administrator = device(&ada, DeviceCapabilities::administrator(), None);
        let joined = device(&ada, DeviceCapabilities::joined(), None);
        let none = RevocationList::new();
        verify_chain(
            administrator.cert(),
            &policy(&ada.user, Authority::Device, &none),
        )
        .unwrap();
        for authority in [
            Authority::Person,
            Authority::OwnWorkspaces,
            Authority::AdmitDevices,
        ] {
            assert_eq!(
                verify_chain(administrator.cert(), &policy(&ada.user, authority, &none)),
                Err(ChainError::NotPermitted(authority))
            );
        }
        verify_chain(joined.cert(), &policy(&ada.user, Authority::Person, &none)).unwrap();
        assert_eq!(
            verify_chain(
                joined.cert(),
                &policy(&ada.user, Authority::AdmitDevices, &none)
            ),
            Err(ChainError::NotPermitted(Authority::AdmitDevices))
        );
    }

    #[test]
    fn every_link_of_the_chain_is_checked() {
        let ada = person("Ada");
        let bob = person("Bob");
        let board = device(&ada, DeviceCapabilities::joined(), None);
        let none = RevocationList::new();
        let ok = policy(&ada.user, Authority::Person, &none);
        let signed = board.sign_as_person("tilo.test.edit", b"hello").unwrap();

        // Another message, another domain.
        assert_eq!(
            verify_delegated(&signed, "tilo.test.edit", b"hellO", &ok),
            Err(ChainError::BadSignature)
        );
        assert_eq!(
            verify_delegated(&signed, "tilo.test.other", b"hello", &ok),
            Err(ChainError::BadSignature)
        );
        // Presented as another person.
        assert_eq!(
            verify_delegated(
                &signed,
                "tilo.test.edit",
                b"hello",
                &policy(&bob.user, Authority::Person, &none)
            ),
            Err(ChainError::OtherIdentity)
        );
        // A forged certificate: Bob's root cannot certify a key for Ada.
        let mut forged = signed.clone();
        forged.cert.capabilities.can_admit_devices = true;
        assert!(matches!(
            verify_delegated(&forged, "tilo.test.edit", b"hello", &ok),
            Err(ChainError::Certificate(DeviceCertError::BadSignature))
        ));
        // A certificate swapped for another of the person's devices.
        let other = device(&ada, DeviceCapabilities::joined(), None);
        let mut swapped = signed;
        swapped.cert = other.cert().clone();
        assert_eq!(
            verify_delegated(&swapped, "tilo.test.edit", b"hello", &ok),
            Err(ChainError::BadSignature)
        );
        // The device key signing the raw message, without the binding.
        let raw = DelegatedSignature {
            cert: board.cert().clone(),
            signature: board.sign_framed_bytes(b"hello"),
        };
        assert_eq!(
            verify_delegated(&raw, "tilo.test.edit", b"hello", &ok),
            Err(ChainError::BadSignature)
        );
    }

    #[test]
    fn capabilities_bound_what_a_device_may_sign() {
        let ada = person("Ada");
        let none = RevocationList::new();
        let joined = device(&ada, DeviceCapabilities::joined(), None);
        let member = device(&ada, DeviceCapabilities::member(), None);
        let host = device(&ada, DeviceCapabilities::host(), None);
        let primary = device(&ada, DeviceCapabilities::primary(), None);

        let check = |signer: &DeviceSigner, authority| {
            let signed = signer.sign_as_person("tilo.test.op", b"m").unwrap();
            verify_delegated(
                &signed,
                "tilo.test.op",
                b"m",
                &policy(&ada.user, authority, &none),
            )
        };
        assert_eq!(check(&joined, Authority::Person), Ok(()));
        assert_eq!(check(&joined, Authority::ReadAllWorkspaces), Ok(()));
        assert_eq!(
            check(&joined, Authority::AdmitDevices),
            Err(ChainError::NotPermitted(Authority::AdmitDevices))
        );
        assert_eq!(
            check(&member, Authority::ReadAllWorkspaces),
            Err(ChainError::NotPermitted(Authority::ReadAllWorkspaces))
        );
        assert_eq!(
            check(&host, Authority::Person),
            Err(ChainError::NotPermitted(Authority::Person))
        );
        assert_eq!(
            check(&host, Authority::OwnWorkspaces),
            Err(ChainError::NotPermitted(Authority::OwnWorkspaces))
        );
        assert_eq!(check(&host, Authority::Host), Ok(()));
        // Every certified device is one of the person's devices, a host too.
        for signer in [&joined, &member, &host, &primary] {
            assert_eq!(check(signer, Authority::Device), Ok(()));
        }
        assert_eq!(check(&primary, Authority::AdmitDevices), Ok(()));
        assert_eq!(check(&primary, Authority::OwnWorkspaces), Ok(()));
    }

    #[test]
    fn revocation_is_permanent_and_only_the_persons_root_revokes() {
        let ada = person("Ada");
        let mallory = person("Mallory");
        let board = device(&ada, DeviceCapabilities::joined(), None);
        let device_key = board.cert().device_public_key;
        let signed = board.sign_as_person("tilo.test.edit", b"m").unwrap();

        // Revoked "in the future" still refuses now: a revocation is not a
        // schedule, and a signer's claimed time proves nothing.
        let mut revocations = RevocationList::new();
        revocations
            .insert(revoke(&ada, &device_key, NOW * 10))
            .unwrap();
        assert_eq!(
            verify_delegated(
                &signed,
                "tilo.test.edit",
                b"m",
                &policy(&ada.user, Authority::Person, &revocations)
            ),
            Err(ChainError::Revoked)
        );
        assert_eq!(
            board.check(&policy(&ada.user, Authority::Person, &revocations)),
            Err(ChainError::Revoked)
        );

        // Another identity's revocation of Ada's device does not apply...
        let mut foreign = RevocationList::new();
        foreign.insert(revoke(&mallory, &device_key, 1)).unwrap();
        verify_delegated(
            &signed,
            "tilo.test.edit",
            b"m",
            &policy(&ada.user, Authority::Person, &foreign),
        )
        .unwrap();
        // ...and cannot displace Ada's own, whatever its timestamp.
        let mut both = RevocationList::new();
        both.insert(revoke(&ada, &device_key, NOW * 10)).unwrap();
        both.insert(revoke(&mallory, &device_key, 1)).unwrap();
        assert_eq!(
            verify_delegated(
                &signed,
                "tilo.test.edit",
                b"m",
                &policy(&ada.user, Authority::Person, &both)
            ),
            Err(ChainError::Revoked)
        );
        let mut reversed = RevocationList::new();
        reversed.insert(revoke(&mallory, &device_key, 1)).unwrap();
        reversed
            .insert(revoke(&ada, &device_key, NOW * 10))
            .unwrap();
        assert_eq!(
            verify_delegated(
                &signed,
                "tilo.test.edit",
                b"m",
                &policy(&ada.user, Authority::Person, &reversed)
            ),
            Err(ChainError::Revoked)
        );
    }

    #[test]
    fn validity_window_is_enforced() {
        let ada = person("Ada");
        let none = RevocationList::new();
        let short = device(&ada, DeviceCapabilities::joined(), Some(NOW));
        let signed = short.sign_as_person("tilo.test.edit", b"m").unwrap();
        assert_eq!(
            verify_delegated(
                &signed,
                "tilo.test.edit",
                b"m",
                &policy(&ada.user, Authority::Person, &none)
            ),
            Err(ChainError::Certificate(DeviceCertError::Expired))
        );
        let early = ChainPolicy {
            now_ns: 50,
            ..policy(&ada.user, Authority::Person, &none)
        };
        assert_eq!(
            verify_delegated(&signed, "tilo.test.edit", b"m", &early),
            Err(ChainError::Certificate(DeviceCertError::NotYetValid))
        );
    }

    #[test]
    fn a_signer_only_presents_its_own_certificate() {
        let ada = person("Ada");
        let board = device(&ada, DeviceCapabilities::joined(), None);
        let (stranger, _) = DeviceIdentity::generate("x", 1);
        assert_eq!(
            DeviceSigner::new(stranger, board.cert().clone()).unwrap_err(),
            ChainError::OtherDevice
        );
        let (key, identity) = DeviceIdentity::generate("x", 1);
        let mut unsigned = board.cert().clone();
        unsigned.device_public_key = identity.public_key;
        assert!(matches!(
            DeviceSigner::new(key, unsigned),
            Err(ChainError::Certificate(_))
        ));
    }

    #[test]
    fn domains_are_bounded_printable_names() {
        let ada = person("Ada");
        let board = device(&ada, DeviceCapabilities::joined(), None);
        for bad in [
            "",
            "has space",
            &"x".repeat(MAX_DELEGATION_DOMAIN_BYTES + 1),
        ] {
            assert_eq!(
                board.sign_as_person(bad, b"m").unwrap_err(),
                ChainError::InvalidDomain
            );
        }
    }

    #[test]
    fn delegated_signatures_round_trip_and_reject_trailing_bytes() {
        let ada = person("Ada");
        let board = device(&ada, DeviceCapabilities::joined(), None);
        let signed = board.sign_as_person("tilo.test.edit", b"m").unwrap();
        let bytes = crate::canonical::canonical_bytes(&signed).unwrap();
        let back: DelegatedSignature = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(back, signed);
        let mut longer = bytes;
        longer.push(0);
        assert!(crate::canonical::from_canonical_bytes::<DelegatedSignature>(&longer).is_err());
        let json = serde_json::to_string(&signed).unwrap();
        assert_eq!(
            serde_json::from_str::<DelegatedSignature>(&json).unwrap(),
            signed
        );
    }

    #[test]
    fn root_only_refusals_name_where_to_go() {
        let ada = person("Ada");
        let primary = device(&ada, DeviceCapabilities::primary(), None);
        let board = device(&ada, DeviceCapabilities::joined(), None);
        let mut revocations = RevocationList::new();
        let labels = root_holder_labels([primary.cert(), board.cert()], &revocations, NOW);
        assert_eq!(labels, vec!["Board B"]);
        let message = RootOnly::RevokeDevice.refusal(labels);
        assert!(message.starts_with("Revoking a device needs the identity's root key"));
        assert!(message.ends_with("do this on 'Board B'"));
        revocations
            .insert(revoke(&ada, &primary.cert().device_public_key, 1))
            .unwrap();
        let labels = root_holder_labels([primary.cert(), board.cert()], &revocations, NOW);
        assert!(labels.is_empty());
        assert!(
            RootOnly::AdmitDevice
                .refusal(labels)
                .contains("one that can approve devices")
        );
    }
}
