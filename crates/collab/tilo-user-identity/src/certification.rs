//! Remote device certification: a new device that holds only its own key
//! asks a root-holding device of the same user for a [`DeviceCert`].
//!
//! The requester signs a [`CertificationRequest`] with the device key it
//! wants certified. That proves possession, so a relay carrying the request
//! cannot substitute another key. The approver checks the request, shows its
//! [`CertificationRequest::pairing_code`] beside the one the requester
//! displays, and issues a certificate clamped to a [`CertificationPolicy`].
//! Either side, or the relay in between, checks the result with
//! [`verify_issued_cert`].
//!
//! The requester never sees the root key and never self-certifies; issuance
//! authority stays with whoever holds the root. The transport is outside
//! this module.
//!
//! The signing preimage constant [`CERTIFICATION_REQUEST_TAG`] is frozen wire
//! ABI — see `signing_preimage_is_frozen_abi` in `lib.rs`.

use ed25519_dalek::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};
use crate::cert::{DeviceCapabilities, DeviceCert, sign_device_cert, verify_device_cert_at};
use crate::revocation::RevocationList;

/// Signing-domain tag for [`CertificationRequest`]. Frozen wire ABI.
pub const CERTIFICATION_REQUEST_TAG: &[u8] = b"tilo-device-certification-request-v1\0";
/// Wire schema of [`CertificationRequest`] understood by this build: 2
/// carries the pairing invite a request answers.
pub const CERTIFICATION_SCHEMA_VERSION: u16 = 2;
/// Longest a request stays answerable.
pub const MAX_CERTIFICATION_REQUEST_LIFETIME_NS: u64 = 10 * 60 * 1_000_000_000;
/// Longest device label a request may carry, in bytes.
pub const MAX_CERTIFICATION_LABEL_BYTES: usize = 80;

/// A device's self-signed request to be certified into `user_public_key`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificationRequest {
    /// Wire schema version.
    pub version: u16,
    /// The user identity the device asks to join.
    pub user_public_key: [u8; 32],
    /// The device key to certify; it signs this request.
    pub device_public_key: [u8; 32],
    /// Label the approver shows and the certificate carries.
    pub device_label: String,
    /// Capabilities the device asks for. The approver may grant fewer.
    pub requested: DeviceCapabilities,
    /// Random single-use request identifier.
    pub nonce: [u8; 16],
    /// The pairing invite this request answers, as the link the person's
    /// inviting device made names it, so that device knows the request is
    /// the one it invited; zeros when no invite asked for it.
    pub invite: [u8; 16],
    /// Earliest time the request is valid, Unix-epoch nanoseconds.
    pub issued_at_ns: u64,
    /// Exclusive expiry, Unix-epoch nanoseconds.
    pub expires_at_ns: u64,
    /// Device-key Ed25519 signature over [`Self::signing_digest`].
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct CertificationRequestView<'a> {
    version: u16,
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    device_label: &'a str,
    requested: &'a DeviceCapabilities,
    nonce: &'a [u8; 16],
    invite: &'a [u8; 16],
    issued_at_ns: u64,
    expires_at_ns: u64,
}

impl CertificationRequest {
    fn view(&self) -> CertificationRequestView<'_> {
        CertificationRequestView {
            version: self.version,
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            device_label: &self.device_label,
            requested: &self.requested,
            nonce: &self.nonce,
            invite: &self.invite,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        }
    }

    /// The digest the device key signs. A signer that cannot export its key
    /// (a non-extractable `WebCrypto` key) signs these bytes and hands the
    /// signature to [`Self::with_signature`].
    ///
    /// # Errors
    ///
    /// Returns an error when the request violates its shape bounds.
    pub fn signing_digest(&self) -> Result<[u8; 32], CertificationError> {
        self.validate_shape()?;
        Ok(signing_digest(CERTIFICATION_REQUEST_TAG, &self.view())?)
    }

    /// Attach an externally produced signature and check it.
    ///
    /// # Errors
    ///
    /// Returns [`CertificationError::Signature`] when it does not verify
    /// under `device_public_key`.
    pub fn with_signature(mut self, signature: [u8; 64]) -> Result<Self, CertificationError> {
        self.signature = signature;
        self.verify_signature()?;
        Ok(self)
    }

    /// Sign with an in-memory device key.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed request or a key that is not the
    /// requested device.
    pub fn sign(self, device_key: &ed25519_dalek::SigningKey) -> Result<Self, CertificationError> {
        if device_key.verifying_key().to_bytes() != self.device_public_key {
            return Err(CertificationError::WrongDevice);
        }
        let signature = device_key.sign(&self.signing_digest()?).to_bytes();
        self.with_signature(signature)
    }

    /// Verify shape, proof of possession and the validity window.
    ///
    /// # Errors
    ///
    /// Returns an error when any of them fails.
    pub fn verify_at(&self, now_ns: u64) -> Result<(), CertificationError> {
        self.verify_signature()?;
        if now_ns < self.issued_at_ns || now_ns >= self.expires_at_ns {
            return Err(CertificationError::Expired);
        }
        Ok(())
    }

    /// Stable identifier of this exact signed request.
    ///
    /// # Errors
    ///
    /// Returns an error when the request violates its shape bounds.
    pub fn digest(&self) -> Result<[u8; 32], CertificationError> {
        let mut hash = blake3::Hasher::new();
        hash.update(&self.signing_digest()?);
        hash.update(&self.signature);
        Ok(*hash.finalize().as_bytes())
    }

    /// Six decimal digits both the requesting page and the approving device
    /// display. A match tells the person approving that the request they see
    /// is the one their browser sent.
    ///
    /// # Errors
    ///
    /// Returns an error when the request violates its shape bounds.
    pub fn pairing_code(&self) -> Result<String, CertificationError> {
        let digest = blake3::derive_key("tilo-device-certification-pairing-v1", &self.digest()?);
        let value = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
        Ok(format!("{value:06}"))
    }

    fn verify_signature(&self) -> Result<(), CertificationError> {
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.device_public_key)
            .map_err(|_| CertificationError::Signature)?;
        key.verify(
            &self.signing_digest()?,
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| CertificationError::Signature)
    }

    fn validate_shape(&self) -> Result<(), CertificationError> {
        if self.version != CERTIFICATION_SCHEMA_VERSION {
            return Err(CertificationError::UnsupportedSchema(self.version));
        }
        let label = self.device_label.trim();
        if self.nonce == [0; 16]
            || self.user_public_key == self.device_public_key
            || label.is_empty()
            || label.len() != self.device_label.len()
            || self.device_label.len() > MAX_CERTIFICATION_LABEL_BYTES
            || self.device_label.chars().any(char::is_control)
            || self
                .expires_at_ns
                .checked_sub(self.issued_at_ns)
                .is_none_or(|lifetime| {
                    lifetime == 0 || lifetime > MAX_CERTIFICATION_REQUEST_LIFETIME_NS
                })
        {
            return Err(CertificationError::InvalidRequest);
        }
        Ok(())
    }
}

/// What an approver may grant a remotely certified device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificationPolicy {
    /// Upper bound on the granted capabilities; requests are intersected
    /// with it.
    pub ceiling: DeviceCapabilities,
    /// Certificate lifetime. Remote devices always expire; renewal is a
    /// fresh certification.
    pub lifetime_ns: u64,
}

impl CertificationPolicy {
    /// A browser device: an ordinary member that may own workspaces but
    /// never admits devices, hosts, or reads beyond its memberships.
    /// Certificates last 30 days.
    pub fn browser() -> Self {
        Self {
            ceiling: DeviceCapabilities::member(),
            lifetime_ns: 30 * 24 * 60 * 60 * 1_000_000_000,
        }
    }

    /// An agent device: no capabilities at all. What it may touch is the
    /// root-signed delegation that accompanies it, not its certificate.
    /// Certificates last 7 days.
    pub fn agent() -> Self {
        Self {
            ceiling: DeviceCapabilities {
                can_own_workspaces: false,
                can_admit_devices: false,
                can_act_as_host: false,
                can_read_all_workspaces: false,
                foreign: false,
                can_administer_devices: false,
            },
            lifetime_ns: 7 * 24 * 60 * 60 * 1_000_000_000,
        }
    }

    /// `requested` limited to what this policy allows.
    pub fn clamp(&self, requested: &DeviceCapabilities) -> DeviceCapabilities {
        DeviceCapabilities {
            can_own_workspaces: requested.can_own_workspaces && self.ceiling.can_own_workspaces,
            can_admit_devices: requested.can_admit_devices && self.ceiling.can_admit_devices,
            can_act_as_host: requested.can_act_as_host && self.ceiling.can_act_as_host,
            can_read_all_workspaces: requested.can_read_all_workspaces
                && self.ceiling.can_read_all_workspaces,
            // Foreign is a restriction: a request or ceiling that says so keeps it.
            foreign: requested.foreign || self.ceiling.foreign,
            can_administer_devices: requested.can_administer_devices
                && self.ceiling.can_administer_devices,
        }
    }

    /// Whether `granted` stays within what was requested and this policy
    /// allows. An approver may grant less than was asked (an agent device
    /// is granted nothing), never more.
    fn allows(&self, granted: &DeviceCapabilities, requested: &DeviceCapabilities) -> bool {
        let ceiling = self.clamp(requested);
        let within = |granted: bool, allowed: bool| !granted || allowed;
        within(granted.can_own_workspaces, ceiling.can_own_workspaces)
            && within(granted.can_admit_devices, ceiling.can_admit_devices)
            && within(granted.can_act_as_host, ceiling.can_act_as_host)
            && within(
                granted.can_read_all_workspaces,
                ceiling.can_read_all_workspaces,
            )
            && within(
                granted.can_administer_devices,
                ceiling.can_administer_devices,
            )
            && (granted.foreign || !ceiling.foreign)
    }
}

/// The unsigned certificate an approver would issue for `request` at
/// `now_ns`. Sign it with [`sign_device_cert`], or have an external root
/// signer sign [`crate::device_cert_signing_digest`] of it.
///
/// # Errors
///
/// Returns an error when the request does not verify, is revoked, or names
/// another user than `user_public_key`.
pub fn prepare_device_cert(
    request: &CertificationRequest,
    user_public_key: [u8; 32],
    policy: &CertificationPolicy,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, CertificationError> {
    request.verify_at(now_ns)?;
    if request.user_public_key != user_public_key {
        return Err(CertificationError::DifferentIdentity);
    }
    if revocations.is_revoked(&request.device_public_key) {
        return Err(CertificationError::Revoked);
    }
    Ok(DeviceCert {
        user_public_key,
        device_public_key: request.device_public_key,
        device_label: request.device_label.clone(),
        capabilities: policy.clamp(&request.requested),
        issued_at_ns: now_ns,
        expires_at_ns: Some(now_ns.saturating_add(policy.lifetime_ns)),
        signature: [0; 64],
    })
}

/// Issue a root-signed certificate for `request`.
///
/// # Errors
///
/// Returns the errors of [`prepare_device_cert`], or
/// [`CertificationError::Certificate`] when `root` does not sign for the
/// requested user.
pub fn issue_device_cert(
    request: &CertificationRequest,
    root: &ed25519_dalek::SigningKey,
    policy: &CertificationPolicy,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, CertificationError> {
    let mut cert = prepare_device_cert(
        request,
        root.verifying_key().to_bytes(),
        policy,
        revocations,
        now_ns,
    )?;
    sign_device_cert(&mut cert, root).map_err(|_| CertificationError::Certificate)?;
    Ok(cert)
}

/// Check that `cert` is a valid, unrevoked answer to exactly `request`
/// within `policy`.
///
/// # Errors
///
/// Returns an error for a bad signature, another device, user or label,
/// capabilities or lifetime beyond the policy, or a revoked device.
pub fn verify_issued_cert(
    request: &CertificationRequest,
    cert: &DeviceCert,
    policy: &CertificationPolicy,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<(), CertificationError> {
    verify_device_cert_at(cert, now_ns).map_err(|_| CertificationError::Certificate)?;
    if cert.user_public_key != request.user_public_key {
        return Err(CertificationError::DifferentIdentity);
    }
    if cert.device_public_key != request.device_public_key
        || cert.device_label != request.device_label
    {
        return Err(CertificationError::WrongDevice);
    }
    if !policy.allows(&cert.capabilities, &request.requested) {
        return Err(CertificationError::Policy);
    }
    match cert.expires_at_ns {
        Some(expiry) if expiry.saturating_sub(cert.issued_at_ns) <= policy.lifetime_ns => {}
        _ => return Err(CertificationError::Policy),
    }
    if revocations.is_revoked(&cert.device_public_key) {
        return Err(CertificationError::Revoked);
    }
    Ok(())
}

/// Certification failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CertificationError {
    /// The wire schema is not understood by this build.
    #[error("unsupported certification schema {0}")]
    UnsupportedSchema(u16),
    /// Nonce, label, keys or validity window violate their bounds.
    #[error("certification request is malformed")]
    InvalidRequest,
    /// The request signature does not verify under its device key.
    #[error("certification request signature is invalid")]
    Signature,
    /// The request is outside its validity window.
    #[error("certification request is expired or not yet valid")]
    Expired,
    /// A key does not belong to the requested device.
    #[error("certificate or key belongs to another device")]
    WrongDevice,
    /// The request or certificate names another user identity.
    #[error("certificate or request names another user identity")]
    DifferentIdentity,
    /// The device key is revoked.
    #[error("device is revoked")]
    Revoked,
    /// The certificate does not verify, or the root cannot sign it.
    #[error("device certificate is invalid")]
    Certificate,
    /// Capabilities or lifetime exceed the certification policy.
    #[error("device certificate exceeds the certification policy")]
    Policy,
    /// Canonical wire encoding failed.
    #[error("certification canonical encoding failed: {0}")]
    Canonical(#[from] CanonicalError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RevocationEntry, RevocationReason, UserIdentity, sign_revocation_entry};
    use getrandom::{SysRng, rand_core::UnwrapErr};

    const NOW: u64 = 1_000_000_000_000;

    fn request(user: [u8; 32], device: &ed25519_dalek::SigningKey) -> CertificationRequest {
        CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: user,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Browser on Cloud".into(),
            requested: DeviceCapabilities::primary(),
            nonce: [7; 16],
            invite: [0; 16],
            issued_at_ns: NOW,
            expires_at_ns: NOW + 60_000_000_000,
            signature: [0; 64],
        }
        .sign(device)
        .unwrap()
    }

    #[test]
    fn root_issues_a_clamped_expiring_certificate() {
        let (root, user) = UserIdentity::generate("Ada", 1);
        let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let request = request(user.public_key, &device);
        let policy = CertificationPolicy::browser();
        let revocations = RevocationList::new();
        let cert = issue_device_cert(&request, &root, &policy, &revocations, NOW + 1).unwrap();
        assert_eq!(cert.capabilities, DeviceCapabilities::member());
        assert!(!cert.capabilities.can_admit_devices);
        assert_eq!(cert.expires_at_ns, Some(NOW + 1 + policy.lifetime_ns));
        verify_issued_cert(&request, &cert, &policy, &revocations, NOW + 2).unwrap();
        assert_eq!(request.pairing_code().unwrap().len(), 6);
    }

    #[test]
    fn external_signer_produces_the_same_request() {
        let (_, user) = UserIdentity::generate("Ada", 1);
        let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let signed = request(user.public_key, &device);
        let mut unsigned = signed.clone();
        unsigned.signature = [0; 64];
        let digest = unsigned.signing_digest().unwrap();
        let external = unsigned
            .with_signature(device.sign(&digest).to_bytes())
            .unwrap();
        assert_eq!(external, signed);
        assert_eq!(external.pairing_code(), signed.pairing_code());
    }

    #[test]
    fn substituted_key_foreign_root_and_escalation_fail_closed() {
        let (root, user) = UserIdentity::generate("Ada", 1);
        let (foreign_root, _) = UserIdentity::generate("Mallory", 1);
        let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let other = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let policy = CertificationPolicy::browser();
        let revocations = RevocationList::new();

        let mut swapped = request(user.public_key, &device);
        swapped.device_public_key = other.verifying_key().to_bytes();
        assert_eq!(
            swapped.verify_at(NOW + 1),
            Err(CertificationError::Signature)
        );

        let request = request(user.public_key, &device);
        assert_eq!(
            issue_device_cert(&request, &foreign_root, &policy, &revocations, NOW + 1),
            Err(CertificationError::DifferentIdentity)
        );
        assert_eq!(
            request.verify_at(NOW + 60_000_000_000),
            Err(CertificationError::Expired)
        );

        let mut escalated =
            prepare_device_cert(&request, user.public_key, &policy, &revocations, NOW + 1).unwrap();
        escalated.capabilities.can_admit_devices = true;
        sign_device_cert(&mut escalated, &root).unwrap();
        assert_eq!(
            verify_issued_cert(&request, &escalated, &policy, &revocations, NOW + 2),
            Err(CertificationError::Policy)
        );
    }

    #[test]
    fn an_approver_may_grant_less_than_requested() {
        let (root, user) = UserIdentity::generate("Ada", 1);
        let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let request = request(user.public_key, &device);
        let revocations = RevocationList::new();
        let cert = issue_device_cert(
            &request,
            &root,
            &CertificationPolicy::agent(),
            &revocations,
            NOW + 1,
        )
        .unwrap();
        assert_eq!(cert.capabilities, CertificationPolicy::agent().ceiling);
        assert_eq!(
            verify_issued_cert(
                &request,
                &cert,
                &CertificationPolicy::browser(),
                &revocations,
                NOW + 2
            ),
            Ok(()),
            "a host checking the browser policy accepts a narrower agent certificate"
        );
    }

    #[test]
    fn revoked_device_is_neither_issued_nor_accepted() {
        let (root, user) = UserIdentity::generate("Ada", 1);
        let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let request = request(user.public_key, &device);
        let policy = CertificationPolicy::browser();
        let cert =
            issue_device_cert(&request, &root, &policy, &RevocationList::new(), NOW + 1).unwrap();
        let mut entry = RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: request.device_public_key,
            revoked_at_ns: NOW,
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).unwrap();
        let mut revocations = RevocationList::new();
        revocations.insert(entry).unwrap();
        assert_eq!(
            issue_device_cert(&request, &root, &policy, &revocations, NOW + 1),
            Err(CertificationError::Revoked)
        );
        assert_eq!(
            verify_issued_cert(&request, &cert, &policy, &revocations, NOW + 2),
            Err(CertificationError::Revoked)
        );
    }
}
