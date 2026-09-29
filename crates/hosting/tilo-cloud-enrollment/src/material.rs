use super::{HostedAttemptError as Error, Result, identity};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_room::join::{self, JoinTransportRequest as Request};
use tilo_room::resources::dto_guard::{self as json_guard, GuardLimits};
mod phase;
use std::io::Write;

pub(super) const MAX_MATERIAL: usize = 1024 * 1024;
pub(super) const MAX_REQUEST: usize = 12 * 1024;
pub(super) const MAX_RESULT: usize = 20 * 1024;
pub(super) const FIRST_HOST_LABEL: &str = "Tilo Cloud";
pub(super) const MAX_JOIN: usize =
    tilo_delivery_protocol::delivery_v3::onboarding_wire::MAX_REQUEST_BYTES;
/// Bounded tolerance between the browser clock and the Cloud host clock.
/// Expiry stays strict (`now < expires`); only freshness bounds and
/// not-yet-valid checks accept a clock this far behind or ahead.
pub(super) const CLOCK_TOLERANCE_NS: u64 = 120_000_000_000;
/// A refused Store.apply may restart from Inspect with the same keys at most
/// this many times per durable identity (each with a fresh operation key).
pub(super) const MAX_APPLY_RETRIES: u32 = 64;
const CHALLENGE_TTL_NS: u64 = 300_000_000_000;
const MEMBERSHIP_TTL_NS: u64 = 60_000_000_000;
fn not_future(at: u64, now: u64) -> bool {
    at <= now.saturating_add(CLOCK_TOLERANCE_NS)
}
/// Signature and validity of a certificate whose issuance may be up to the
/// clock tolerance ahead of this browser. Expiry is still checked no earlier
/// than now.
pub(super) fn verify_certificate(certificate: &identity::DeviceCert, now: u64) -> Result<()> {
    if !not_future(certificate.issued_at_ns, now) {
        return Err(Error::Expired);
    }
    identity::verify_device_cert_at(certificate, now.max(certificate.issued_at_ns))
        .map_err(|_| Error::Signature)
}

/// Zero-filled backing of exactly `length` bytes; callers bound `length` first.
fn zeroed(length: usize) -> Result<Vec<u8>> {
    let mut value = Vec::new();
    value
        .try_reserve_exact(length)
        .map_err(|_| Error::Capacity)?;
    value.resize(length, 0);
    Ok(value)
}
// These are enforced visitor ceilings, not input-size estimates. Every
// allocating visit is debited before invocation; exceeding a ceiling refuses.
pub(super) trait JsonSchema: serde::de::DeserializeOwned {
    const MAXIMUM: usize;
    const ALLOCATION: usize;
}
impl JsonSchema for MaterialV4 {
    const MAXIMUM: usize = MAX_MATERIAL;
    const ALLOCATION: usize = 4 * 1024 * 1024;
}
impl JsonSchema for identity::cloud_enrollment::Challenge {
    const MAXIMUM: usize = MAX_REQUEST;
    const ALLOCATION: usize = 128 * 1024;
}
impl JsonSchema for identity::cloud_enrollment::EnrollmentRequest {
    const MAXIMUM: usize = MAX_REQUEST;
    const ALLOCATION: usize = 128 * 1024;
}
impl JsonSchema for super::EnrollmentReply {
    const MAXIMUM: usize = MAX_RESULT;
    const ALLOCATION: usize = 1024 * 1024;
}
pub(super) fn read_json<T: JsonSchema>(bytes: &[u8]) -> Result<T> {
    if bytes.is_empty() || bytes.len() > T::MAXIMUM {
        return Err(Error::Material);
    }
    json_guard::json(bytes, GuardLimits::closed(T::ALLOCATION, T::MAXIMUM)).map_err(|error| {
        match error {
            json_guard::GuardError::Capacity => Error::Capacity,
            json_guard::GuardError::Malformed => Error::Material,
        }
    })
}
pub(super) fn read_request(bytes: &[u8]) -> Result<Request> {
    if bytes.is_empty() || bytes.len() > MAX_JOIN {
        return Err(Error::Material);
    }
    // Existing Postcard discriminants only; no other request schema is decoded.
    let (variant, _) = postcard::take_from_bytes::<u32>(bytes).map_err(|_| Error::Material)?;
    if !matches!(variant, 3 | 4) {
        return Err(Error::Material);
    }
    const LIMIT: usize = 1024 * 1024;
    let value =
        json_guard::postcard(bytes, GuardLimits::closed(LIMIT, MAX_JOIN)).map_err(|error| {
            match error {
                json_guard::GuardError::Capacity => Error::Capacity,
                json_guard::GuardError::Malformed => Error::Material,
            }
        })?;
    let canonical = encode_postcard(&value, MAX_JOIN)?;
    if canonical != bytes {
        return Err(Error::Material);
    }
    Ok(value)
}

struct Count {
    size: usize,
    maximum: usize,
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.size = self
            .size
            .checked_add(bytes.len())
            .filter(|n| *n <= self.maximum)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn encode_json<T: Serialize>(value: &T, maximum: usize) -> Result<Vec<u8>> {
    let mut count = Count { size: 0, maximum };
    serde_json::to_writer(&mut count, value).map_err(|_| Error::Capacity)?;
    let mut bytes = zeroed(count.size)?;
    let mut destination = bytes.as_mut_slice();
    serde_json::to_writer(&mut destination, value).map_err(|_| Error::Capacity)?;
    if !destination.is_empty() {
        return Err(Error::Capacity);
    }
    Ok(bytes)
}
pub(super) fn encode_postcard<T: Serialize>(value: &T, maximum: usize) -> Result<Vec<u8>> {
    let size = postcard::experimental::serialized_size(value).map_err(|_| Error::Capacity)?;
    if size == 0 || size > maximum {
        return Err(Error::Capacity);
    }
    let mut bytes = zeroed(size)?;
    if postcard::to_slice(value, &mut bytes)
        .map_err(|_| Error::Capacity)?
        .len()
        != size
    {
        return Err(Error::Capacity);
    }
    Ok(bytes)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MaterialV4 {
    pub version: u32,
    pub kind: String,
    pub account_id: String,
    pub machine_id: String,
    pub root_secret: [u8; 32],
    pub device_secret: [u8; 32],
    pub attempt_id: [u8; 32],
    pub created_at_ns: u64,
    pub certificate: identity::DeviceCert,
    pub phase: Phase,
    /// Explicit first-claim custody: only material created for a fresh Store
    /// may root-sign the challenge's host key. Absent means false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub first_claim: bool,
    /// Count of refused applies that restarted from Inspect with these keys.
    /// Absent means zero; it only varies the apply operation key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub apply_retry: u32,
}
fn is_false(value: &bool) -> bool {
    !*value
}
fn is_zero(value: &u32) -> bool {
    *value == 0
}
#[derive(Serialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Phase {
    Initial,
    Apply {
        enrollment: Vec<u8>,
        signed_at_ns: u64,
    },
    MembershipChallenge {
        enrollment: Vec<u8>,
        signed_at_ns: u64,
        request: Vec<u8>,
    },
    MembershipComplete {
        enrollment: Vec<u8>,
        signed_at_ns: u64,
        request: Vec<u8>,
        proof: Vec<u8>,
        proof_at_ns: u64,
    },
}

// Serialization-only views borrow every dynamic backing, so staging a phase
// never clones the retained request or proof bytes.
#[derive(Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub(super) enum PhaseView<'a> {
    /// Only a refused apply returns to the initial phase (same keys).
    Initial,
    Apply {
        enrollment: &'a [u8],
        signed_at_ns: u64,
    },
    MembershipChallenge {
        enrollment: &'a [u8],
        signed_at_ns: u64,
        request: &'a [u8],
    },
    MembershipComplete {
        enrollment: &'a [u8],
        signed_at_ns: u64,
        request: &'a [u8],
        proof: &'a [u8],
        proof_at_ns: u64,
    },
}
#[derive(Serialize)]
pub(super) struct MaterialView<'a> {
    version: u32,
    kind: &'a str,
    account_id: &'a str,
    machine_id: &'a str,
    root_secret: &'a [u8; 32],
    device_secret: &'a [u8; 32],
    attempt_id: &'a [u8; 32],
    created_at_ns: u64,
    certificate: &'a identity::DeviceCert,
    phase: PhaseView<'a>,
    #[serde(skip_serializing_if = "is_false")]
    first_claim: bool,
    #[serde(skip_serializing_if = "is_zero")]
    apply_retry: u32,
}
impl Phase {
    pub fn enrollment(&self) -> Option<(&[u8], u64)> {
        match self {
            Self::Initial => None,
            Self::Apply {
                enrollment,
                signed_at_ns,
            }
            | Self::MembershipChallenge {
                enrollment,
                signed_at_ns,
                ..
            }
            | Self::MembershipComplete {
                enrollment,
                signed_at_ns,
                ..
            } => Some((enrollment, *signed_at_ns)),
        }
    }
}
impl MaterialV4 {
    pub fn with_phase<'a>(&'a self, phase: PhaseView<'a>) -> MaterialView<'a> {
        self.with_phase_retry(phase, self.apply_retry)
    }
    pub fn with_phase_retry<'a>(
        &'a self,
        phase: PhaseView<'a>,
        apply_retry: u32,
    ) -> MaterialView<'a> {
        MaterialView {
            version: self.version,
            kind: &self.kind,
            account_id: &self.account_id,
            machine_id: &self.machine_id,
            root_secret: &self.root_secret,
            device_secret: &self.device_secret,
            attempt_id: &self.attempt_id,
            created_at_ns: self.created_at_ns,
            certificate: &self.certificate,
            phase,
            first_claim: self.first_claim,
            apply_retry,
        }
    }
    pub fn device(&self) -> SigningKey {
        SigningKey::from_bytes(&self.device_secret)
    }
    /// First claim only: root-sign the unbound Store's challenged host key
    /// with the narrow host delegation and no expiry. Issued at material
    /// creation so repeated signing is deterministic and never future-dated.
    pub fn first_host_certificate(
        &self,
        challenge: &identity::cloud_enrollment::Challenge,
    ) -> Result<identity::DeviceCert> {
        if !self.first_claim
            || challenge.root_public_key.is_some()
            || challenge.host_certificate.is_some()
        {
            return Err(Error::Scope);
        }
        let mut certificate = identity::DeviceCert {
            user_public_key: self.certificate.user_public_key,
            device_public_key: challenge.host_public_key,
            device_label: FIRST_HOST_LABEL.into(),
            capabilities: identity::cloud_enrollment::host_capabilities(),
            issued_at_ns: self.created_at_ns,
            expires_at_ns: None,
            signature: [0; 64],
        };
        identity::sign_device_cert(&mut certificate, &SigningKey::from_bytes(&self.root_secret))
            .map_err(|_| Error::Signature)?;
        Ok(certificate)
    }
    pub fn request_id(&self) -> Result<[u8; 32]> {
        identity::canonical::signing_digest(
            b"tilo-cloud-browser-membership-v4\0",
            &(&self.attempt_id, &self.machine_id, &self.certificate),
        )
        .map_err(|_| Error::Signature)
    }
    pub fn operation_id(&self) -> Result<uuid::Uuid> {
        // The first apply keeps its original key; a restarted apply (fresh
        // challenge after a refusal) must never reuse a refused key.
        let digest = if self.apply_retry == 0 {
            identity::canonical::signing_digest(
                b"tilo-cloud-browser-apply-v4\0",
                &(&self.attempt_id, &self.machine_id, &self.certificate),
            )
        } else {
            identity::canonical::signing_digest(
                b"tilo-cloud-browser-apply-retry-v4\0",
                &(
                    &self.attempt_id,
                    &self.machine_id,
                    &self.certificate,
                    self.apply_retry,
                ),
            )
        }
        .map_err(|_| Error::Signature)?;
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest[..16]);
        Ok(uuid::Uuid::from_bytes(bytes))
    }
    pub fn validate(&self, now: u64) -> Result<()> {
        if self.version != 4
            || self.kind != "cloud-owner"
            || self.created_at_ns == 0
            || !not_future(self.created_at_ns, now)
            || self.apply_retry > MAX_APPLY_RETRIES
            || self.root_secret == [0; 32]
            || self.device_secret == [0; 32]
            || self.attempt_id == [0; 32]
            || !identifier(&self.account_id)
            || !identifier(&self.machine_id)
            || self.certificate.device_label.is_empty()
            || self.certificate.device_label.len() > 128
            || self.certificate.user_public_key
                != SigningKey::from_bytes(&self.root_secret)
                    .verifying_key()
                    .to_bytes()
            || self.certificate.device_public_key != self.device().verifying_key().to_bytes()
            || self.certificate.capabilities != identity::DeviceCapabilities::member()
            || self.certificate.issued_at_ns != self.created_at_ns
        {
            return Err(Error::Material);
        }
        verify_certificate(&self.certificate, now)?;
        let Some((bytes, signed)) = self.phase.enrollment() else {
            return Ok(());
        };
        if signed < self.created_at_ns || !not_future(signed, now) {
            return Err(Error::Material);
        }
        let enrollment: identity::cloud_enrollment::EnrollmentRequest = read_json(bytes)?;
        self.check_challenge(&enrollment.challenge, signed)?;
        self.check_host(&enrollment.host_certificate, &enrollment.challenge, signed)?;
        if enrollment.browser_certificate != self.certificate {
            return Err(Error::Scope);
        }
        identity::cloud_enrollment::verify_request(&enrollment, &enrollment.challenge, signed)
            .map_err(|_| Error::Signature)?;
        let request = match &self.phase {
            Phase::MembershipChallenge { request, .. }
            | Phase::MembershipComplete { request, .. } => request,
            _ => return Ok(()),
        };
        let held = read_request(request)?;
        let Request::MembershipChallenge(request) = &held else {
            return Err(Error::Material);
        };
        if request.room != enrollment.challenge.room
            || request.workspace_id
                != enrollment
                    .challenge
                    .workspace_id
                    .parse::<u64>()
                    .map_err(|_| Error::Scope)?
            || request.device_cert != self.certificate
            || request.request_id != self.request_id()?
        {
            return Err(Error::Scope);
        }
        if let Phase::MembershipComplete {
            proof, proof_at_ns, ..
        } = &self.phase
        {
            if *proof_at_ns < signed || !not_future(*proof_at_ns, now) {
                return Err(Error::Material);
            }
            let held = read_request(proof)?;
            let Request::MembershipComplete(proof) = &held else {
                return Err(Error::Material);
            };
            self.check_membership(
                &proof.proof.challenge,
                &enrollment.host_certificate,
                &enrollment.challenge,
                *proof_at_ns,
            )?;
            join::verify_membership_proof(
                &proof.proof,
                &proof.proof.challenge,
                &self.certificate,
                &identity::RevocationList::new(),
                *proof_at_ns,
            )
            .map_err(|_| Error::Signature)?;
        }
        Ok(())
    }
    pub fn check_challenge(
        &self,
        challenge: &identity::cloud_enrollment::Challenge,
        now: u64,
    ) -> Result<()> {
        // Expiry is strict; the freshness bound and material age tolerate a
        // browser clock that is a little behind or ahead of the Store.
        if !not_future(self.created_at_ns, now)
            || now >= challenge.expires_at_ns
            || challenge.expires_at_ns - now > CHALLENGE_TTL_NS + CLOCK_TOLERANCE_NS
        {
            return Err(Error::Expired);
        }
        // A Store bound to this root presents its stable host certificate. An
        // unbound Store presents neither; only explicit first-claim custody may
        // then root-sign the host key. Any mixed or foreign binding refuses.
        let host = match (&challenge.root_public_key, &challenge.host_certificate) {
            (Some(root), Some(host)) if *root == self.certificate.user_public_key => Some(host),
            (None, None) if self.first_claim => None,
            // The Store is already bound to another root: this identity can
            // never enroll it (first claim lost, or another owner's Store).
            (Some(root), _) if *root != self.certificate.user_public_key => {
                return Err(Error::Claimed);
            }
            _ => return Err(Error::Scope),
        };
        if challenge.version != 1
            || challenge.machine_id != self.machine_id
            || challenge.nonce == [0; 32]
            || challenge.host_public_key == self.certificate.device_public_key
            || uuid::Uuid::parse_str(&challenge.room)
                .map_err(|_| Error::Scope)?
                .to_string()
                != challenge.room
            || tilo_relay_protocol::room_workspace_id(&challenge.room)
                .map_err(|_| Error::Scope)?
                .to_string()
                != challenge.workspace_id
        {
            return Err(Error::Scope);
        }
        if let Some(host) = host {
            self.check_host(host, challenge, now)?;
        }
        Ok(())
    }
    /// The enrolled host certificate: this root, the challenged host key, the
    /// narrow host delegation and no expiry.
    pub fn check_host(
        &self,
        host: &identity::DeviceCert,
        challenge: &identity::cloud_enrollment::Challenge,
        now: u64,
    ) -> Result<()> {
        if host.user_public_key != self.certificate.user_public_key
            || host.device_public_key != challenge.host_public_key
            || host.capabilities != identity::cloud_enrollment::host_capabilities()
            || host.expires_at_ns.is_some()
        {
            return Err(Error::Scope);
        }
        verify_certificate(host, now)?;
        Ok(())
    }
    pub fn check_membership(
        &self,
        challenge: &join::MembershipChallenge,
        host: &identity::DeviceCert,
        enrolled: &identity::cloud_enrollment::Challenge,
        now: u64,
    ) -> Result<()> {
        let request_id = self.request_id()?;
        self.check_membership_for(challenge, host, enrolled, request_id, now)
    }
    /// The same exact challenge check for a fresh refresh request identifier.
    pub fn check_membership_for(
        &self,
        challenge: &join::MembershipChallenge,
        host: &identity::DeviceCert,
        enrolled: &identity::cloud_enrollment::Challenge,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<()> {
        if request_id == [0; 32] {
            return Err(Error::Scope);
        }
        if challenge.nonce == [0; 32]
            || now >= challenge.expires_at_ns
            || challenge.expires_at_ns - now > MEMBERSHIP_TTL_NS + CLOCK_TOLERANCE_NS
        {
            return Err(Error::Expired);
        }
        for certificate in [host, &self.certificate] {
            if !not_future(certificate.issued_at_ns, now) {
                return Err(Error::Expired);
            }
        }
        // Certificates issued slightly ahead of this clock are checked at
        // their issuance; the challenge expiry above is already strict.
        let checked = now
            .max(host.issued_at_ns)
            .max(self.certificate.issued_at_ns);
        let expected = join::MembershipChallenge::new(
            &enrolled.room,
            enrolled.workspace_id.parse().map_err(|_| Error::Scope)?,
            host,
            &self.certificate,
            request_id,
            challenge.nonce,
            challenge.expires_at_ns,
            checked,
        )
        .map_err(|_| Error::Scope)?;
        if &expected != challenge {
            return Err(Error::Scope);
        }
        Ok(())
    }
}
fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
