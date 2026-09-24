use super::{HostedAttemptError as Error, Result, identity, relay};
use automerge::resource_limits::{NativeAllowance, NativeReservation};
use ed25519_dalek::SigningKey;
use qos_peer_state::join::{self, JoinTransportRequest as Request};
use serde::{Deserialize, Serialize};
#[path = "allocation.rs"]
pub(super) mod allocation;
#[path = "json_guard.rs"]
mod json_guard;
#[path = "phase.rs"]
mod phase;
use std::io::Write;

pub(super) const MAX_MATERIAL: usize = 1024 * 1024;
pub(super) const MAX_REQUEST: usize = 12 * 1024;
pub(super) const MAX_RESULT: usize = 20 * 1024;
pub(super) const FIRST_HOST_LABEL: &str = "Tilo Cloud";
pub(super) const MAX_JOIN: usize = relay::delivery_v3::onboarding_wire::MAX_REQUEST_BYTES;

// Each owner drops the backing before its charge. The same original allowance
// funds decoded material, retained bytes, and simultaneous staged checkpoints.
pub(super) struct Held<T> {
    pub value: T,
    _credit: NativeReservation,
}
pub(super) struct Bytes {
    pub value: Vec<u8>,
    _credit: NativeReservation,
}
impl Bytes {
    pub fn zeroed(length: usize, budget: &NativeAllowance) -> Result<Self> {
        let credit = budget.reserve(length).map_err(|_| Error::Capacity)?;
        let mut value = Vec::new();
        value
            .try_reserve_exact(length)
            .map_err(|_| Error::Capacity)?;
        if value.capacity() > length {
            return Err(Error::Capacity);
        }
        value.resize(length, 0);
        Ok(Self {
            value,
            _credit: credit,
        })
    }
    pub fn copy(bytes: &[u8], budget: &NativeAllowance) -> Result<Self> {
        let mut held = Self::zeroed(bytes.len(), budget)?;
        held.value.copy_from_slice(bytes);
        Ok(held)
    }
}
// These are enforced visitor ceilings, not input-size estimates. Every
// allocating visit is debited before invocation; exceeding a ceiling refuses.
#[derive(Clone, Copy)]
struct JsonLimits {
    allocation_bytes: usize,
    collection_entries: usize,
}
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
pub(super) fn read_json<T: JsonSchema>(bytes: &[u8], budget: &NativeAllowance) -> Result<Held<T>> {
    if bytes.is_empty() || bytes.len() > T::MAXIMUM {
        return Err(Error::Material);
    }
    let _parser = allocation::parser(bytes, budget)?;
    let credit = allocation::reserve(budget, T::ALLOCATION)?;
    let value = json_guard::decode(
        bytes,
        JsonLimits {
            allocation_bytes: T::ALLOCATION,
            collection_entries: T::MAXIMUM,
        },
    )
    .map_err(|error| match error {
        json_guard::GuardError::Capacity => Error::Capacity,
        json_guard::GuardError::Malformed => Error::Material,
    })?;
    Ok(Held {
        value,
        _credit: credit,
    })
}
pub(super) fn read_request(bytes: &[u8], budget: &NativeAllowance) -> Result<Held<Request>> {
    if bytes.is_empty() || bytes.len() > MAX_JOIN {
        return Err(Error::Material);
    }
    // Existing Postcard discriminants only; no other request schema is decoded.
    let (variant, _) = postcard::take_from_bytes::<u32>(bytes).map_err(|_| Error::Material)?;
    if !matches!(variant, 3 | 4) {
        return Err(Error::Material);
    }
    const LIMIT: usize = 1024 * 1024;
    let _diagnostic = allocation::reserve(budget, allocation::signature_diagnostic())?;
    let credit = allocation::reserve(budget, LIMIT)?;
    let value = json_guard::request(
        bytes,
        JsonLimits {
            allocation_bytes: LIMIT,
            collection_entries: MAX_JOIN,
        },
    )
    .map_err(|error| match error {
        json_guard::GuardError::Capacity => Error::Capacity,
        json_guard::GuardError::Malformed => Error::Material,
    })?;
    let canonical = encode_postcard(&value, MAX_JOIN, budget)?;
    if canonical.value != bytes {
        return Err(Error::Material);
    }
    Ok(Held {
        value,
        _credit: credit,
    })
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
pub(super) fn encode_json<T: Serialize>(
    value: &T,
    maximum: usize,
    budget: &NativeAllowance,
) -> Result<Bytes> {
    let _errors = allocation::reserve(budget, allocation::error_boxes())?;
    let mut count = Count { size: 0, maximum };
    serde_json::to_writer(&mut count, value).map_err(|_| Error::Capacity)?;
    let mut bytes = Bytes::zeroed(count.size, budget)?;
    let mut destination = bytes.value.as_mut_slice();
    serde_json::to_writer(&mut destination, value).map_err(|_| Error::Capacity)?;
    if !destination.is_empty() {
        return Err(Error::Capacity);
    }
    Ok(bytes)
}
pub(super) fn encode_postcard<T: Serialize>(
    value: &T,
    maximum: usize,
    budget: &NativeAllowance,
) -> Result<Bytes> {
    let size = postcard::experimental::serialized_size(value).map_err(|_| Error::Capacity)?;
    if size == 0 || size > maximum {
        return Err(Error::Capacity);
    }
    let mut bytes = Bytes::zeroed(size, budget)?;
    if postcard::to_slice(value, &mut bytes.value)
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
    /// may root-sign the challenge's host key. Absent (legacy) means false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub first_claim: bool,
}
fn is_false(value: &bool) -> bool {
    !*value
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

// Serialization-only views borrow every dynamic backing. No request/candidate
// clone can grow a decoded Held under its original visitor reservation.
#[derive(Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub(super) enum PhaseView<'a> {
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
        budget: &NativeAllowance,
    ) -> Result<identity::DeviceCert> {
        if !self.first_claim
            || challenge.root_public_key.is_some()
            || challenge.host_certificate.is_some()
        {
            return Err(Error::Scope);
        }
        let _errors = allocation::helper_errors(budget)?;
        let _label = allocation::reserve(budget, FIRST_HOST_LABEL.len())?;
        let mut certificate = identity::DeviceCert {
            user_public_key: self.certificate.user_public_key,
            device_public_key: challenge.host_public_key,
            device_label: FIRST_HOST_LABEL.into(),
            capabilities: identity::cloud_enrollment::host_capabilities(),
            issued_at_ns: self.created_at_ns,
            expires_at_ns: None,
            signature: [0; 64],
        };
        let _transcript = allocation::transcript(&certificate, budget)?;
        identity::sign_device_cert(&mut certificate, &SigningKey::from_bytes(&self.root_secret))
            .map_err(|_| Error::Signature)?;
        Ok(certificate)
    }
    pub fn request_id(&self, budget: &NativeAllowance) -> Result<[u8; 32]> {
        let _errors = allocation::helper_errors(budget)?;
        identity::canonical::signing_digest(
            b"qos-cloud-browser-membership-v4\0",
            &(&self.attempt_id, &self.machine_id, &self.certificate),
        )
        .map_err(|_| Error::Signature)
    }
    pub fn operation_id(&self, budget: &NativeAllowance) -> Result<uuid::Uuid> {
        let _errors = allocation::helper_errors(budget)?;
        let digest = identity::canonical::signing_digest(
            b"qos-cloud-browser-apply-v4\0",
            &(&self.attempt_id, &self.machine_id, &self.certificate),
        )
        .map_err(|_| Error::Signature)?;
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest[..16]);
        Ok(uuid::Uuid::from_bytes(bytes))
    }
    pub fn validate(&self, now: u64, budget: &NativeAllowance) -> Result<()> {
        let _errors = allocation::helper_errors(budget)?;
        if self.version != 4
            || self.kind != "cloud-owner"
            || self.created_at_ns == 0
            || self.created_at_ns > now
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
        identity::verify_device_cert_at(&self.certificate, now).map_err(|_| Error::Signature)?;
        let Some((bytes, signed)) = self.phase.enrollment() else {
            return Ok(());
        };
        if signed < self.created_at_ns || signed > now {
            return Err(Error::Material);
        }
        let enrollment: Held<identity::cloud_enrollment::EnrollmentRequest> =
            read_json(bytes, budget)?;
        self.check_challenge(&enrollment.value.challenge, signed, budget)?;
        self.check_host(
            &enrollment.value.host_certificate,
            &enrollment.value.challenge,
            signed,
            budget,
        )?;
        let _transcript = allocation::enrollment_transcript(&enrollment.value, budget)?;
        if enrollment.value.browser_certificate != self.certificate {
            return Err(Error::Scope);
        }
        identity::cloud_enrollment::verify_request(
            &enrollment.value,
            &enrollment.value.challenge,
            signed,
        )
        .map_err(|_| Error::Signature)?;
        let request = match &self.phase {
            Phase::MembershipChallenge { request, .. }
            | Phase::MembershipComplete { request, .. } => request,
            _ => return Ok(()),
        };
        let held = read_request(request, budget)?;
        let Request::MembershipChallenge(request) = &held.value else {
            return Err(Error::Material);
        };
        if request.room != enrollment.value.challenge.room
            || request.workspace_id
                != enrollment
                    .value
                    .challenge
                    .workspace_id
                    .parse::<u64>()
                    .map_err(|_| Error::Scope)?
            || request.device_cert != self.certificate
            || request.request_id != self.request_id(budget)?
        {
            return Err(Error::Scope);
        }
        if let Phase::MembershipComplete {
            proof, proof_at_ns, ..
        } = &self.phase
        {
            if *proof_at_ns < signed || *proof_at_ns > now {
                return Err(Error::Material);
            }
            let held = read_request(proof, budget)?;
            let Request::MembershipComplete(proof) = &held.value else {
                return Err(Error::Material);
            };
            self.check_membership(
                &proof.proof.challenge,
                &enrollment.value.host_certificate,
                &enrollment.value.challenge,
                *proof_at_ns,
                budget,
            )?;
            let _returned = allocation::reserve(
                budget,
                allocation::sum(&[
                    proof.proof.challenge.room.len(),
                    self.certificate.device_label.len(),
                ])?,
            )?;
            let _transcript = allocation::transcript(&proof.proof.challenge, budget)?;
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
        budget: &NativeAllowance,
    ) -> Result<()> {
        let _errors = allocation::helper_errors(budget)?;
        let _text = allocation::reserve(budget, 36 + 20)?;
        if now < self.created_at_ns
            || now >= challenge.expires_at_ns
            || challenge.expires_at_ns - now > 300_000_000_000
        {
            return Err(Error::Expired);
        }
        // A Store bound to this root presents its stable host certificate. An
        // unbound Store presents neither; only explicit first-claim custody may
        // then root-sign the host key. Any mixed or foreign binding refuses.
        let host = match (&challenge.root_public_key, &challenge.host_certificate) {
            (Some(root), Some(host)) if *root == self.certificate.user_public_key => Some(host),
            (None, None) if self.first_claim => None,
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
            || relay::room_workspace_id(&challenge.room)
                .map_err(|_| Error::Scope)?
                .to_string()
                != challenge.workspace_id
        {
            return Err(Error::Scope);
        }
        if let Some(host) = host {
            self.check_host(host, challenge, now, budget)?;
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
        budget: &NativeAllowance,
    ) -> Result<()> {
        let _errors = allocation::helper_errors(budget)?;
        if host.user_public_key != self.certificate.user_public_key
            || host.device_public_key != challenge.host_public_key
            || host.capabilities != identity::cloud_enrollment::host_capabilities()
            || host.expires_at_ns.is_some()
        {
            return Err(Error::Scope);
        }
        identity::verify_device_cert_at(host, now).map_err(|_| Error::Signature)?;
        Ok(())
    }
    pub fn check_membership(
        &self,
        challenge: &join::MembershipChallenge,
        host: &identity::DeviceCert,
        enrolled: &identity::cloud_enrollment::Challenge,
        now: u64,
        budget: &NativeAllowance,
    ) -> Result<()> {
        let request_id = self.request_id(budget)?;
        self.check_membership_for(challenge, host, enrolled, request_id, now, budget)
    }
    /// The same exact challenge check for a fresh refresh request identifier.
    pub fn check_membership_for(
        &self,
        challenge: &join::MembershipChallenge,
        host: &identity::DeviceCert,
        enrolled: &identity::cloud_enrollment::Challenge,
        request_id: [u8; 32],
        now: u64,
        budget: &NativeAllowance,
    ) -> Result<()> {
        if request_id == [0; 32] {
            return Err(Error::Scope);
        }
        let _errors = allocation::helper_errors(budget)?;
        let _expected = allocation::reserve(budget, 36)?;
        if challenge.nonce == [0; 32]
            || now >= challenge.expires_at_ns
            || challenge.expires_at_ns - now > 60_000_000_000
        {
            return Err(Error::Expired);
        }
        let expected = join::MembershipChallenge::new(
            &enrolled.room,
            enrolled.workspace_id.parse().map_err(|_| Error::Scope)?,
            host,
            &self.certificate,
            request_id,
            challenge.nonce,
            challenge.expires_at_ns,
            now,
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
