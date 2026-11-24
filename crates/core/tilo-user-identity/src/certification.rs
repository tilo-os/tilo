//! Remote device certification: a new device that holds only its own key
//! asks a root-holding device of the same user for a [`DeviceCert`].
//!
//! The requester signs a [`CertificationRequest`] with the device key it
//! wants certified. That proves possession, so a server carrying the request
//! cannot substitute another key. The approver checks the request, shows its
//! [`CertificationRequest::pairing_code`] beside the one the requester
//! displays, and issues a certificate clamped to a [`CertificationPolicy`].
//! Either side, or the server in between, checks the result with
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
use crate::cert::{
    DeviceCapabilities, DeviceCert, sign_device_cert, sign_device_cert_async,
    verify_device_cert_at, verify_stand_in_cert_at,
};
use crate::revocation::RevocationList;
use crate::sign::StatementSigner;

/// Signing-domain tag for [`CertificationRequest`]. Frozen wire ABI.
pub const CERTIFICATION_REQUEST_TAG: &[u8] = b"tilo-device-certification-request-v1\0";
/// Wire schema of [`CertificationRequest`] understood by this build: 2
/// carries the pairing invite a request answers.
pub const CERTIFICATION_SCHEMA_VERSION: u16 = 2;
/// Longest a request stays answerable.
pub const MAX_CERTIFICATION_REQUEST_LIFETIME_NS: u64 = 10 * 60 * 1_000_000_000;
/// How far the clocks of the devices in one certification may differ: 60
/// seconds.
///
/// The new device dates its request by its own clock. The host that queues
/// the request and the device that approves it each check it by theirs, and
/// the host and the new device then read the certificate by theirs again. No
/// two of those clocks agree to the nanosecond: 20 ms between a laptop and
/// the machine beside it turned away half of all pairings. So
///
/// - a request dated up to this far ahead of the clock that checks it is
///   taken ([`CertificationRequest::verify_at`]). Dated further ahead it is
///   refused as [`CertificationError::NotYetValid`], which names the clocks
///   and never reads as a link that expired;
/// - a certificate the person's root issues for a request is valid from this
///   long before the issuer's clock ([`prepare_device_cert`]), so the host
///   and the device it names can use it the moment it arrives. Every
///   verifier of a certificate keeps its exact not-before check.
///
/// It grants nothing. A request still ends at the expiry its device signed,
/// at most [`MAX_CERTIFICATION_REQUEST_LIFETIME_NS`] after its date, so the
/// longest a checker takes one for grows from ten minutes to eleven. A
/// certificate lasts what its policy says, counted from the earlier start.
/// Who may approve, the pairing code and the single use of a pairing link
/// read no clock and are untouched. It is the allowance enrolment and signed
/// peer actions already use.
pub const CERTIFICATION_CLOCK_ALLOWANCE_NS: u64 = 60 * 1_000_000_000;
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

    pub fn sign(self, device_key: &ed25519_dalek::SigningKey) -> Result<Self, CertificationError> {
        if device_key.verifying_key().to_bytes() != self.device_public_key {
            return Err(CertificationError::WrongDevice);
        }
        let signature = device_key.sign(&self.signing_digest()?).to_bytes();
        self.with_signature(signature)
    }

    /// Verify shape, proof of possession and the validity window at
    /// `now_ns`, the checker's own clock. The request may be dated up to
    /// [`CERTIFICATION_CLOCK_ALLOWANCE_NS`] ahead of it; its expiry is exact.
    ///
    /// # Errors
    ///
    /// [`CertificationError::NotYetValid`] when it is dated further ahead
    /// than that, [`CertificationError::Expired`] at or past its expiry, or
    /// what its shape and signature fail with.
    pub fn verify_at(&self, now_ns: u64) -> Result<(), CertificationError> {
        self.verify_signature()?;
        if self.issued_at_ns > now_ns.saturating_add(CERTIFICATION_CLOCK_ALLOWANCE_NS) {
            return Err(CertificationError::NotYetValid);
        }
        if now_ns >= self.expires_at_ns {
            return Err(CertificationError::Expired);
        }
        Ok(())
    }

    /// Stable identifier of this exact signed request.
    pub fn digest(&self) -> Result<[u8; 32], CertificationError> {
        let mut hash = blake3::Hasher::new();
        hash.update(&self.signing_digest()?);
        hash.update(&self.signature);
        Ok(*hash.finalize().as_bytes())
    }

    /// Six decimal digits both the requesting page and the approving device
    /// display. A match tells the person approving that the request they see
    /// is the one their browser sent.
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
    /// A browser device: one of the person's own devices, which may own
    /// workspaces but never admits devices. Certificates last 30 days.
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
            ceiling: DeviceCapabilities::nothing(),
            lifetime_ns: 7 * 24 * 60 * 60 * 1_000_000_000,
        }
    }

    /// `requested` limited to what this policy allows.
    pub fn clamp(&self, requested: &DeviceCapabilities) -> DeviceCapabilities {
        DeviceCapabilities {
            retired: crate::cert::RetiredFlags::default(),
            can_own_workspaces: requested.can_own_workspaces && self.ceiling.can_own_workspaces,
            can_admit_devices: requested.can_admit_devices && self.ceiling.can_admit_devices,
            // Foreign is a restriction: a request or ceiling that says so keeps it.
            foreign: requested.foreign || self.ceiling.foreign,
            // An agent's grant is issued by the person approving the agent,
            // never asked for by a device being enrolled.
            agent: None,
            // A stand-in is certified by its person's root on purpose, never
            // through a device's request.
            stand_in: false,
            server_key: false,
            // Operating web pages in Browser is a key's, given by its person
            // where they make it, never a device enrolled through a request.
            browser_pages: false,
            // A device's workspace service is issued by that device, never
            // asked for by a device being enrolled.
            workspace_service: false,
            // A device confined to one workspace is certified for it by its
            // person's root, never enrolled through a device's request.
            workspace: None,
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
            && (granted.foreign || !ceiling.foreign)
            && !granted.stand_in
            && !granted.browser_pages
    }
}

/// The unsigned certificate an approver would issue for `request` at
/// `now_ns`. Sign it with [`sign_device_cert`], or have an external root
/// signer sign [`crate::device_cert_signing_digest`] of it.
///
/// It is valid from [`CERTIFICATION_CLOCK_ALLOWANCE_NS`] before `now_ns` and
/// lasts `policy`'s lifetime from there: the host that relays it and the
/// device it names read it by their own clocks, exactly, and either may run
/// behind the approver's.
pub fn prepare_device_cert(
    request: &CertificationRequest,
    user_public_key: [u8; 32],
    policy: &CertificationPolicy,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, CertificationError> {
    prepare_valid_from(
        request,
        user_public_key,
        policy,
        revocations,
        now_ns,
        now_ns.saturating_sub(CERTIFICATION_CLOCK_ALLOWANCE_NS),
    )
}

/// [`prepare_device_cert`] for a certificate whose validity starts at
/// `valid_from_ns`, checked against `request` at `now_ns`.
fn prepare_valid_from(
    request: &CertificationRequest,
    user_public_key: [u8; 32],
    policy: &CertificationPolicy,
    revocations: &RevocationList,
    now_ns: u64,
    valid_from_ns: u64,
) -> Result<DeviceCert, CertificationError> {
    request.verify_at(now_ns)?;
    if request.user_public_key != user_public_key {
        return Err(CertificationError::DifferentIdentity);
    }
    if revocations.is_revoked(&user_public_key, &request.device_public_key) {
        return Err(CertificationError::Revoked);
    }
    Ok(DeviceCert {
        user_public_key,
        device_public_key: request.device_public_key,
        device_label: request.device_label.clone(),
        capabilities: policy.clamp(&request.requested),
        issued_at_ns: valid_from_ns,
        expires_at_ns: Some(valid_from_ns.saturating_add(policy.lifetime_ns)),
        signature: [0; 64],
        issuer: None,
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
    root: &dyn StatementSigner,
    policy: &CertificationPolicy,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, CertificationError> {
    let mut cert = prepare_device_cert(request, root.public_key(), policy, revocations, now_ns)?;
    sign_device_cert(&mut cert, root).map_err(|_| CertificationError::Certificate)?;
    Ok(cert)
}

/// [`issue_device_cert`] with a root that answers only later: the root a page
/// holds, which certifies the device it approved through `WebCrypto`.
///
/// # Errors
///
/// As [`issue_device_cert`].
pub async fn issue_device_cert_async(
    request: &CertificationRequest,
    root: &dyn collab_types::signer::DeviceSigner,
    policy: &CertificationPolicy,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, CertificationError> {
    let mut cert = prepare_device_cert(
        request,
        root.device_public_key(),
        policy,
        revocations,
        now_ns,
    )?;
    sign_device_cert_async(&mut cert, root)
        .await
        .map_err(|_| CertificationError::Certificate)?;
    Ok(cert)
}

/// Have a person's stand-in approver (its key `stand_in_key`, certified by
/// `stand_in_cert`) answer `request`: a certificate of a member device of
/// that person for a browser's lifetime ([`CertificationPolicy::browser`]),
/// signed by the stand-in and carrying its certificate as the issuer. What a
/// host that keeps a person's stand-in calls once a device of the person
/// asked for exactly this request; see [`crate::cert`] for what a stand-in
/// may do.
///
/// # Errors
///
/// [`CertificationError::Certificate`] when `stand_in_cert` is no valid
/// stand-in certificate at `now_ns` or not `stand_in_key`'s,
/// [`CertificationError::Revoked`] when `revocations` refuse the stand-in or
/// the device, or the errors of [`prepare_device_cert`].
pub fn issue_device_cert_as_stand_in(
    request: &CertificationRequest,
    stand_in_key: &dyn StatementSigner,
    stand_in_cert: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, CertificationError> {
    verify_stand_in_cert_at(stand_in_cert, now_ns).map_err(|_| CertificationError::Certificate)?;
    if stand_in_cert.device_public_key != stand_in_key.public_key() {
        return Err(CertificationError::Certificate);
    }
    if revocations.refuses_at(stand_in_cert, now_ns) {
        return Err(CertificationError::Revoked);
    }
    // Dated exactly, unlike what a root issues: the host that keeps the
    // stand-in is the host that reads the certificate first, by this same
    // clock, and the time a stand-in certified at is what tells its
    // certificates from those it could not issue (before its own validity,
    // from its revocation on).
    let mut cert = prepare_valid_from(
        request,
        stand_in_cert.user_public_key,
        &CertificationPolicy::browser(),
        revocations,
        now_ns,
        now_ns,
    )?;
    cert.issuer = Some(Box::new(stand_in_cert.clone()));
    sign_device_cert(&mut cert, stand_in_key).map_err(|_| CertificationError::Certificate)?;
    Ok(cert)
}

/// Have one of the person's devices (its key `device_key`, certified by
/// `device_cert`, which the person's root signed) answer `request`: a
/// certificate of a member device of that person for a browser's lifetime
/// ([`CertificationPolicy::browser`]), signed by the device and carrying its
/// certificate as the issuer. What a computer that is already the person's
/// signs their browser tab in with; see [`crate::cert`].
///
/// # Errors
///
/// [`CertificationError::Certificate`] when `device_cert` is no live
/// certificate of one of the person's devices at `now_ns` or not
/// `device_key`'s, [`CertificationError::Revoked`] when `revocations` refuse
/// the device or the tab, or the errors of [`prepare_valid_from`].
pub fn issue_device_cert_as_device(
    request: &CertificationRequest,
    device_key: &dyn StatementSigner,
    device_cert: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, CertificationError> {
    verify_device_cert_at(device_cert, now_ns).map_err(|_| CertificationError::Certificate)?;
    if !device_cert.is_root_signed_device()
        || device_cert.device_public_key != device_key.public_key()
    {
        return Err(CertificationError::Certificate);
    }
    if revocations.refuses_at(device_cert, now_ns) {
        return Err(CertificationError::Revoked);
    }
    // Dated as one the root issues is, since another machine reads it by its
    // own clock: valid from the allowance before this device's clock, and
    // never from before the device it comes from existed.
    let valid_from_ns = now_ns
        .saturating_sub(CERTIFICATION_CLOCK_ALLOWANCE_NS)
        .max(device_cert.issued_at_ns);
    let mut cert = prepare_valid_from(
        request,
        device_cert.user_public_key,
        &CertificationPolicy::browser(),
        revocations,
        now_ns,
        valid_from_ns,
    )?;
    cert.issuer = Some(Box::new(device_cert.clone()));
    sign_device_cert(&mut cert, device_key).map_err(|_| CertificationError::Certificate)?;
    Ok(cert)
}

/// Check that `cert` is a valid, unrevoked answer to exactly `request`
/// within `policy`.
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
    if revocations.refuses(cert) {
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
    /// The request is at or past its expiry.
    #[error("certification request has expired")]
    Expired,
    /// The request is dated further ahead of the checker's clock than
    /// [`CERTIFICATION_CLOCK_ALLOWANCE_NS`]: the two clocks differ.
    #[error(
        "certification request is dated more than 60 seconds ahead: the clocks of the two devices differ"
    )]
    NotYetValid,
    /// A key does not belong to the requested device.
    #[error("certificate or key belongs to another device")]
    WrongDevice,
    /// The request or certificate names another user identity.
    #[error("certificate or request names another user identity")]
    DifferentIdentity,
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

    tilo_test_env::scenario! {
        fn certification_issues_only_what_root_policy_and_revocation_allow;

        phase root_issues_a_clamped_expiring_certificate;
        {
            let (root, user) = UserIdentity::generate("Ada", 1);
            let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let request = request(user.public_key, &device);
            let policy = CertificationPolicy::browser();
            let revocations = RevocationList::new();
            let cert = issue_device_cert(&request, &root, &policy, &revocations, NOW + 1).unwrap();
            assert_eq!(cert.capabilities, DeviceCapabilities::member());
            assert!(!cert.capabilities.can_admit_devices);
            assert_eq!(cert.issued_at_ns, NOW + 1 - CERTIFICATION_CLOCK_ALLOWANCE_NS);
            assert_eq!(cert.expires_at_ns, Some(cert.issued_at_ns + policy.lifetime_ns));
            verify_issued_cert(&request, &cert, &policy, &revocations, NOW + 2).unwrap();
            assert_eq!(request.pairing_code().unwrap().len(), 6);
        }

        phase external_signer_produces_the_same_request;
        {
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

        phase substituted_key_foreign_root_and_escalation_fail_closed;
        {
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

        phase an_approver_may_grant_less_than_requested;
        {
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

        phase a_stand_in_answers_a_request_as_a_browser_until_it_is_revoked;
        {
            let (root, user) = UserIdentity::generate("Ada", 1);
            let stand_in_key = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let mut stand_in = crate::prepare_stand_in_cert(
                user.public_key,
                stand_in_key.verifying_key().to_bytes(),
                "Stand-in on tilo.example",
                NOW - 1,
                NOW - 1 + crate::STAND_IN_LIFETIME_NS,
            );
            sign_device_cert(&mut stand_in, &root).unwrap();
            let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let request = request(user.public_key, &device);
            let policy = CertificationPolicy::browser();
            let none = RevocationList::new();

            let cert =
                issue_device_cert_as_stand_in(&request, &stand_in_key, &stand_in, &none, NOW + 1)
                    .unwrap();
            assert_eq!(cert.capabilities, DeviceCapabilities::member());
            // A stand-in dates what it certifies exactly: no earlier start.
            assert_eq!(cert.issued_at_ns, NOW + 1);
            assert_eq!(cert.expires_at_ns, Some(NOW + 1 + policy.lifetime_ns));
            assert_eq!(cert.issuer.as_deref(), Some(&stand_in));
            verify_issued_cert(&request, &cert, &policy, &none, NOW + 2).unwrap();

            // Not with another key, nor once the stand-in is gone.
            let other = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            assert_eq!(
                issue_device_cert_as_stand_in(&request, &other, &stand_in, &none, NOW + 1),
                Err(CertificationError::Certificate)
            );
            assert_eq!(
                issue_device_cert_as_stand_in(
                    &request,
                    &stand_in_key,
                    &stand_in,
                    &none,
                    NOW - 1 + crate::STAND_IN_LIFETIME_NS
                ),
                Err(CertificationError::Certificate)
            );
            let mut entry = RevocationEntry {
                user_public_key: user.public_key,
                revoked_device_public_key: stand_in.device_public_key,
                revoked_at_ns: NOW + 2,
                reason: RevocationReason::Retired,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, &root).unwrap();
            let mut revoked = RevocationList::new();
            revoked.insert(entry).unwrap();
            assert_eq!(
                issue_device_cert_as_stand_in(&request, &stand_in_key, &stand_in, &revoked, NOW + 2),
                Err(CertificationError::Revoked)
            );
            // What it issued before stays valid.
            verify_issued_cert(&request, &cert, &policy, &revoked, NOW + 3).unwrap();
            // A stand-in is never what a device's request is answered with.
            assert_eq!(
                verify_issued_cert(&request, &stand_in, &policy, &none, NOW + 1),
                Err(CertificationError::Certificate)
            );
        }

        phase revoked_device_is_neither_issued_nor_accepted;
        {
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

    tilo_test_env::scenario! {
        fn certification_allows_the_clock_difference_real_devices_have;

        phase a_pairing_passes_clocks_twenty_milliseconds_apart;
        {
            // What a laptop and the machine beside it differed by.
            const APART_NS: u64 = 20_000_000;
            let (root, user) = UserIdentity::generate("Ada", 1);
            let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            // Dated NOW, by the new device's clock.
            let request = request(user.public_key, &device);
            let policy = CertificationPolicy::browser();
            let none = RevocationList::new();
            // The host that queues it reads a clock a little behind.
            assert_eq!(request.verify_at(NOW - APART_NS), Ok(()));
            // So does the device that approves it,
            let cert = issue_device_cert(&request, &root, &policy, &none, NOW - APART_NS).unwrap();
            // and the host and the new device read its certificate by clocks
            // behind the approver's in turn.
            assert_eq!(
                verify_issued_cert(&request, &cert, &policy, &none, NOW - 2 * APART_NS),
                Ok(())
            );
        }

        phase a_request_dated_ahead_is_taken_within_the_allowance_and_refused_for_its_date_beyond;
        {
            let (_, user) = UserIdentity::generate("Ada", 1);
            let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let request = request(user.public_key, &device);
            assert_eq!(
                request.verify_at(NOW - CERTIFICATION_CLOCK_ALLOWANCE_NS),
                Ok(())
            );
            // Beyond it the refusal is about the date, and names the clocks.
            assert_eq!(
                request.verify_at(NOW - CERTIFICATION_CLOCK_ALLOWANCE_NS - 1),
                Err(CertificationError::NotYetValid)
            );
            assert_eq!(
                CertificationError::NotYetValid.to_string(),
                "certification request is dated more than 60 seconds ahead: the clocks of the two devices differ"
            );
            assert_eq!(
                CERTIFICATION_CLOCK_ALLOWANCE_NS,
                60 * 1_000_000_000,
                "the sentence names the allowance"
            );
            // The allowance moves no expiry.
            assert_eq!(request.verify_at(NOW + 60_000_000_000 - 1), Ok(()));
            assert_eq!(
                request.verify_at(NOW + 60_000_000_000),
                Err(CertificationError::Expired)
            );
            // Nor does it lengthen what a device may sign: ten minutes from
            // its own date.
            let mut long = request;
            long.expires_at_ns = long.issued_at_ns + MAX_CERTIFICATION_REQUEST_LIFETIME_NS + 1;
            assert_eq!(long.sign(&device), Err(CertificationError::InvalidRequest));
        }

        phase a_root_certificate_starts_before_its_issuers_clock_and_every_reader_stays_exact;
        {
            let (root, user) = UserIdentity::generate("Ada", 1);
            let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let request = request(user.public_key, &device);
            let policy = CertificationPolicy::browser();
            let none = RevocationList::new();
            let cert = issue_device_cert(&request, &root, &policy, &none, NOW + 1).unwrap();
            let starts = NOW + 1 - CERTIFICATION_CLOCK_ALLOWANCE_NS;
            assert_eq!(cert.issued_at_ns, starts);
            // It lasts what the policy says, no longer for the earlier start.
            assert_eq!(cert.expires_at_ns, Some(starts + policy.lifetime_ns));
            // A reader whose clock is the whole allowance behind the
            // issuer's takes it,
            assert_eq!(verify_device_cert_at(&cert, starts), Ok(()));
            assert_eq!(
                verify_issued_cert(&request, &cert, &policy, &none, starts),
                Ok(())
            );
            // and one further behind refuses it, by the exact check every
            // reader of a certificate makes.
            assert_eq!(
                verify_device_cert_at(&cert, starts - 1),
                Err(crate::DeviceCertError::NotYetValid)
            );
            assert_eq!(
                verify_issued_cert(&request, &cert, &policy, &none, starts - 1),
                Err(CertificationError::Certificate)
            );
        }
    }
}
