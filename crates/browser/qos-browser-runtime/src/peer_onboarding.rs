//! Closed HTTPS onboarding over existing identity and incoming-byte custody.
use crate::peer_delivery::incoming::IncomingBudget;
use crate::peer_invitation::InvitedIdentity;
use automerge::resource_limits::NativeReservation;
use ed25519_dalek::{Signer, SigningKey};
use qos_peer_state::join::JoinTransportRequest;
use serde::Serialize;
use tilo_relay::delivery_v3::{onboarding_wire as wire, state::canonical_item_digest};
mod handoff;
mod local;
mod preflight;
pub(crate) use local::{BoundLocal, GovernanceReply, LocalContext, bind_local};
pub(crate) use preflight::preflight_completion;
#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub(crate) use local::local_governance;
#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::exchange_invited;

pub(crate) struct OnboardingReply {
    held: qos_peer_state::resources::BoundedJoinResponse,
}
impl OnboardingReply {
    pub(crate) fn response(&self) -> &qos_peer_state::join::JoinTransportResponse {
        self.held.response()
    }
    fn decode(bytes: &[u8], budget: &IncomingBudget) -> Result<Self, String> {
        if bytes.len() > wire::MAX_RESPONSE_BYTES {
            return Err("onboarding nested reply exceeds bound".into());
        }
        let held = qos_peer_state::resources::BoundedJoinResponse::decode(
            bytes,
            qos_peer_state::resources::OuterLimits {
                allocation_bytes: 32 * 1024 * 1024,
                // Legacy Vec<u8> is a Serde sequence too. The separate
                // allocation guard limits concrete backing before growth.
                collection_entries: wire::MAX_RESPONSE_BYTES,
            },
            budget.allowance(),
        )
        .map_err(|_| "storage-pressure: bounded onboarding response refused")?;
        Ok(Self { held })
    }
}

pub(crate) struct OnboardingCancel {
    closed: std::cell::Cell<bool>,
    #[cfg(target_arch = "wasm32")]
    controller: std::cell::RefCell<Option<wasm_bindgen::JsValue>>,
}
impl OnboardingCancel {
    pub(crate) fn new() -> Self {
        Self {
            closed: std::cell::Cell::new(false),
            #[cfg(target_arch = "wasm32")]
            controller: std::cell::RefCell::new(None),
        }
    }
    pub(crate) fn cancel(&self) {
        self.closed.set(true);
        #[cfg(target_arch = "wasm32")]
        if let Some(controller) = self.controller.borrow().as_ref() {
            wasm::abort(controller);
        }
    }
    fn check(&self) -> Result<(), String> {
        if self.closed.get() {
            Err("onboarding attempt cancelled".into())
        } else {
            Ok(())
        }
    }
}

struct ChargedBytes {
    bytes: Vec<u8>,
    _credit: NativeReservation,
}

impl ChargedBytes {
    fn zeroed(length: usize, budget: &IncomingBudget) -> Result<Self, String> {
        let credit = budget.reserve(length)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| "onboarding allocation unavailable")?;
        if bytes.capacity() > length {
            return Err("onboarding allocation exceeds reserved capacity".into());
        }
        bytes.resize(length, 0);
        Ok(Self {
            bytes,
            _credit: credit,
        })
    }

    fn copy(source: &[u8], budget: &IncomingBudget) -> Result<Self, String> {
        let mut value = Self::zeroed(source.len(), budget)?;
        value.bytes.copy_from_slice(source);
        Ok(value)
    }
}

fn encode<T: Serialize>(
    value: &T,
    maximum: usize,
    budget: &IncomingBudget,
) -> Result<ChargedBytes, String> {
    let length = postcard::experimental::serialized_size(value).map_err(|e| e.to_string())?;
    if length == 0 || length > maximum {
        return Err("onboarding encoding exceeds bound".into());
    }
    let credit = budget.reserve(length)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| "onboarding allocation unavailable")?;
    if bytes.capacity() > length {
        return Err("onboarding allocation exceeds reserved capacity".into());
    }
    bytes.resize(length, 0);
    let encoded = postcard::to_slice(value, &mut bytes).map_err(|e| e.to_string())?;
    if encoded.len() != length {
        return Err("onboarding encoding size changed".into());
    }
    Ok(ChargedBytes {
        bytes,
        _credit: credit,
    })
}

pub(crate) struct PreparedJoin {
    encoded: ChargedBytes,
    key: SigningKey,
    budget: IncomingBudget,
}

impl PreparedJoin {
    pub(crate) fn request_bytes(&self) -> &[u8] {
        &self.encoded.bytes
    }

    fn verify_reply<'a>(&self, bytes: &'a [u8], now: u64) -> Result<VerifiedReply<'a>, String> {
        if bytes.len() > 1024 * 1024 {
            return Err("initial onboarding reply exceeds bound".into());
        }
        // The shared verifier may decode up to 64 manifest descriptors with
        // Vec growth overlap and the request's bounded owned certificate.
        let credit = self
            .budget
            .reserve(3 * wire::MAX_SEGMENTS * std::mem::size_of::<wire::HandoffSegment>() + 4096)?;
        let request =
            wire::OnboardingRequest::decode(self.request_bytes()).map_err(|e| e.to_string())?;
        if now >= request.expires_at_ns {
            return Err("original onboarding attempt expired".into());
        }
        let reply = wire::OnboardingResponse::decode(bytes).map_err(|e| e.to_string())?;
        reply
            .verify_host_signature(&request, now)
            .map_err(|e| e.to_string())?;
        Ok(VerifiedReply {
            body: reply.body,
            kind: reply.body_kind,
            _credit: credit,
        })
    }
}

struct VerifiedReply<'a> {
    body: &'a [u8],
    kind: wire::ReplyBodyKind,
    _credit: NativeReservation,
}

pub(crate) fn prepare_invited(
    identity: &InvitedIdentity,
    request: JoinTransportRequest,
    budget: &IncomingBudget,
    now_ns: u64,
) -> Result<PreparedJoin, String> {
    // These are the existing verified identity/pin readers. Their legacy
    // identity allocation is not upgraded to native qualification by this
    // transport adapter; all newly encoded request backings below are charged.
    let invitation = identity.retained_invitation()?;
    let grant = invitation.grant();
    let (host, origin) = identity
        .selected_delivery_pin()?
        .ok_or("invitation has no unique signed HTTPS delivery origin")?;
    tilo_user_identity::verify_device_cert_at(&host, now_ns).map_err(|e| e.to_string())?;
    if !host.capabilities.can_act_as_host || now_ns == 0 {
        return Err("invalid onboarding delivery host/time".into());
    }
    let certificate = identity.certificate();
    tilo_user_identity::verify_device_cert_at(certificate, now_ns).map_err(|e| e.to_string())?;
    let operation = match &request {
        JoinTransportRequest::Inspect { signed_invitation }
            if signed_invitation == invitation.signed_invitation() =>
        {
            wire::OnboardingOperation::Inspect
        }
        JoinTransportRequest::Challenge(value)
            if value.signed_invitation == invitation.signed_invitation()
                && value.device_cert == *certificate
                && value.request_id == identity.request_id() =>
        {
            wire::OnboardingOperation::Challenge
        }
        JoinTransportRequest::Complete(_) if identity.completion().is_some() => {
            wire::OnboardingOperation::Complete
        }
        JoinTransportRequest::MembershipChallenge(value)
            if value.room == grant.room
                && value.workspace_id == grant.workspace_id
                && value.device_cert == *certificate
                && value.request_id != [0; 32] =>
        {
            wire::OnboardingOperation::MembershipChallenge
        }
        JoinTransportRequest::MembershipComplete(value)
            if value.proof.challenge.room == grant.room
                && value.proof.challenge.workspace_id == grant.workspace_id
                && value.proof.challenge.authority_device_public_key
                    == grant.authority.device_public_key
                && value.proof.challenge.device_public_key == certificate.device_public_key
                && value.proof.challenge.user_public_key == certificate.user_public_key =>
        {
            wire::OnboardingOperation::MembershipComplete
        }
        _ => return Err("onboarding request differs from retained identity scope".into()),
    };
    let body = encode(&request, wire::MAX_REQUEST_BYTES, budget)?;
    if operation == wire::OnboardingOperation::Complete
        && body.bytes.get(1..) != identity.completion()
    {
        return Err("onboarding completion differs from durable checkpoint".into());
    }
    let scope = wire::OnboardingScope {
        origin: &origin,
        host: host.device_public_key,
        workspace: grant.workspace_id,
        root: grant.authority.user_public_key,
        room: grant.room.parse().map_err(|_| "invalid onboarding room")?,
    };
    wrap(
        operation,
        body,
        scope,
        certificate,
        identity.device_key(),
        budget,
        now_ns,
    )
}

/// Enrolled Cloud owner scope: the page origin, the enrolled host certificate
/// (also the room authority) and this owner device.
pub(crate) struct OwnerOnboarding<'a> {
    pub origin: &'a str,
    pub host: &'a tilo_user_identity::DeviceCert,
    pub certificate: &'a tilo_user_identity::DeviceCert,
    pub room: &'a str,
    pub workspace: u64,
    pub key: SigningKey,
}

/// Only membership refresh operations for the owner's own certificate. The
/// host still verifies the wrapper against its signed descriptor origin.
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "owner onboarding is called only from the wasm32 build"
    )
)]
pub(crate) fn prepare_owner(
    owner: OwnerOnboarding<'_>,
    request: JoinTransportRequest,
    budget: &IncomingBudget,
    now_ns: u64,
) -> Result<PreparedJoin, String> {
    if !canonical_https_origin(owner.origin) || now_ns == 0 {
        return Err("invalid Cloud owner onboarding origin/time".into());
    }
    let host = owner.host;
    let certificate = owner.certificate;
    tilo_user_identity::verify_device_cert_at(host, now_ns).map_err(|e| e.to_string())?;
    tilo_user_identity::verify_device_cert_at(certificate, now_ns).map_err(|e| e.to_string())?;
    if !host.capabilities.can_act_as_host
        || host.user_public_key != certificate.user_public_key
        || owner.key.verifying_key().to_bytes() != certificate.device_public_key
        || !tilo_relay::room_workspace_id_matches(owner.room, owner.workspace)
    {
        return Err("invalid Cloud owner onboarding scope".into());
    }
    let operation = match &request {
        JoinTransportRequest::MembershipChallenge(value)
            if value.room == owner.room
                && value.workspace_id == owner.workspace
                && value.device_cert == *certificate
                && value.request_id != [0; 32] =>
        {
            wire::OnboardingOperation::MembershipChallenge
        }
        JoinTransportRequest::MembershipComplete(value)
            if value.proof.challenge.room == owner.room
                && value.proof.challenge.workspace_id == owner.workspace
                && value.proof.challenge.authority_device_public_key == host.device_public_key
                && value.proof.challenge.device_public_key == certificate.device_public_key
                && value.proof.challenge.user_public_key == certificate.user_public_key =>
        {
            wire::OnboardingOperation::MembershipComplete
        }
        _ => return Err("Cloud owner onboarding request differs from enrolled scope".into()),
    };
    let body = encode(&request, wire::MAX_REQUEST_BYTES, budget)?;
    let scope = wire::OnboardingScope {
        origin: owner.origin,
        host: host.device_public_key,
        workspace: owner.workspace,
        root: host.user_public_key,
        room: owner.room.parse().map_err(|_| "invalid onboarding room")?,
    };
    wrap(
        operation,
        body,
        scope,
        certificate,
        owner.key,
        budget,
        now_ns,
    )
}

/// `https://host[:port]` only: no path, query, fragment or credentials.
fn canonical_https_origin(origin: &str) -> bool {
    let Some(authority) = origin.strip_prefix("https://") else {
        return false;
    };
    !authority.is_empty()
        && origin.len() <= 256
        && authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

fn wrap(
    operation: wire::OnboardingOperation,
    body: ChargedBytes,
    scope: wire::OnboardingScope<'_>,
    certificate: &tilo_user_identity::DeviceCert,
    key: SigningKey,
    budget: &IncomingBudget,
    now_ns: u64,
) -> Result<PreparedJoin, String> {
    let cert = encode(certificate, 4096, budget)?;
    let (nonce, _) = tilo_user_identity::DeviceIdentity::generate("Onboarding request", now_ns);
    let mut wrapper = wire::OnboardingRequest {
        version: 3,
        operation,
        scope,
        certificate: &cert.bytes,
        nonce: nonce.to_bytes(),
        issued_at_ns: now_ns,
        expires_at_ns: now_ns
            .checked_add(60_000_000_000)
            .ok_or("onboarding time overflow")?,
        body_digest: canonical_item_digest(&body.bytes),
        body: &body.bytes,
        signature: [[0; 32]; 2],
    };
    let signature = key
        .sign(&wrapper.signing_digest().map_err(|e| e.to_string())?)
        .to_bytes();
    wrapper.signature[0].copy_from_slice(&signature[..32]);
    wrapper.signature[1].copy_from_slice(&signature[32..]);
    let encoded = encode(&wrapper, wire::MAX_REQUEST_BYTES, budget)?;
    Ok(PreparedJoin {
        encoded,
        key,
        budget: budget.clone(),
    })
}

#[cfg(test)]
mod tests;
