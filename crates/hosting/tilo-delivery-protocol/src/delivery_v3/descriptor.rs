//! Canonical signed transport bootstrap under the current governed workspace.
//!
//! Callers retain the real committed-owner guard while supplying `StatePeer`.
//! A signed endpoint cannot appoint its own governor, host, generation floor or
//! TLS trust. This module does not install or advertise any live ingress.

use super::{auth, framing::VERSION};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_peer_state::{StatePeer, join::TransportHint};
use tilo_user_identity::{DeviceCert, RevocationList, verify_device_cert_at};
use web_time::SystemTime;

/// Distinct kind covered by the existing transport-pin signature domain.
pub const KIND: &str = "delivery-v3-workspace-host-v1";
/// Exact reliable WebTransport route.
pub const WEBTRANSPORT_PATH: &str = "/peer/v3/connect";
/// Exact HTTPS request base.
pub const HTTPS_BASE_PATH: &str = "/peer/v3";
/// Signed voice service advertisement; installation must be established by its owner.
pub const MEDIA_VOICE_V1: u64 = 1;
/// Every media-capability bit a descriptor may advertise (voice, live).
pub const MEDIA_CAPABILITIES: u64 = MEDIA_VOICE_V1 | super::live_wire::MEDIA_LIVE_V1;
/// Maximum canonical descriptor payload bytes.
pub const MAX_PAYLOAD_BYTES: usize = 8 * 1024;
/// Maximum canonical complete signed transport hint bytes.
pub const MAX_HINT_BYTES: usize = 24 * 1024;
const MAX_OPAQUE_BYTES: usize = 2 * MAX_PAYLOAD_BYTES + 2;
const MAX_CERT_BYTES: usize = 2048;
const LIFETIME_NS: u64 = 900_000_000_000;
const DOMAIN: &[u8] = b"tilo-delivery-v3-descriptor\0";

/// Independent revision expectations supplied by existing owner persistence.
#[derive(Clone, Copy)]
pub enum GenerationExpectation {
    /// First acceptance still needs an independently authenticated nonzero floor.
    FirstAccepted {
        /// Minimum revision supplied by enrollment/installation policy.
        minimum_generation: u64,
    },
    /// Persisted signed-descriptor revision; equal generations cannot equivocate.
    Current {
        /// Last durably accepted revision.
        generation: u64,
        /// Exact digest accepted at that revision.
        descriptor_digest: [u8; 32],
    },
}

/// Trusted owner inputs, not wire claims or an authorization snapshot DTO.
/// Hold the actual committed native publication guard through each operation;
/// an arbitrary cloned or stale `StatePeer` cannot substitute for that owner.
pub struct DescriptorAuthority<'a> {
    /// Actual current committed governed workspace.
    pub peer: &'a StatePeer,
    /// Independently pinned exact governor certificate.
    pub pinned_governor: &'a DeviceCert,
    /// Independently appointed exact service host; never taken from this hint.
    pub pinned_host: &'a DeviceCert,
    /// Exact workspace identifier from the authenticated owner scope.
    pub workspace: u64,
    /// Existing owner-persisted generation expectation.
    pub generation: GenerationExpectation,
    /// Current native policy ceiling for delivery features.
    pub allowed_delivery_capabilities: u64,
    /// Current trusted owner time.
    pub now: SystemTime,
}

/// Issuance configuration, which conveys neither authority nor installation.
/// The ordinary issuance function always emits zero media advertisement.
pub struct DescriptorSettings {
    /// Exact configured HTTPS origin; TLS still requires separate verification.
    pub origin: String,
    /// Configured delivery bits, within the current owner's allowed policy.
    pub delivery_capabilities: u64,
    /// Absolute issue time in nanoseconds.
    pub issued_at_ns: u64,
    /// Expiry within 15 minutes and both current certificate validity windows.
    pub expires_at_ns: u64,
    /// Revision reserved by the existing owner before publication.
    pub generation: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct DescriptorPayload {
    protocol: u16,
    workspace: u64,
    workspace_root: [u8; 32],
    host_root: [u8; 32],
    host_device: [u8; 32],
    host_certificate_digest: [u8; 32],
    #[serde(deserialize_with = "bounded_origin")]
    origin: String,
    #[serde(deserialize_with = "bounded_route")]
    webtransport_path: String,
    #[serde(deserialize_with = "bounded_route")]
    https_base_path: String,
    #[serde(deserialize_with = "bounded_endpoint")]
    native_endpoint: String,
    delivery_capabilities: u64,
    media_capabilities: u64,
    issued_at_ns: u64,
    expires_at_ns: u64,
    generation: u64,
}

/// Issue under the actual current governed host, using the existing hint signer.
/// The caller must persist its revision and gate actual route installation before
/// publishing this hint; creating signed wire bytes does not install a service.
///
/// # Errors
///
/// Returns `DescriptorError` if the inputs violate this operation's bounds or invariants.
pub fn issue_transport_hint(
    settings: &DescriptorSettings,
    current: &DescriptorAuthority<'_>,
    host_key: &SigningKey,
) -> Result<TransportHint, DescriptorError> {
    issue_transport_hint_with_media(settings, current, host_key, 0)
}

/// Sign an explicit known media mask. This is only a wire signing utility, not
/// proof of installation. A native publisher must retain its private live service
/// witness through the owner barrier before requesting a nonzero mask.
///
/// # Errors
///
/// Returns `DescriptorError` if the inputs violate this operation's bounds or invariants.
pub fn issue_transport_hint_with_media(
    settings: &DescriptorSettings,
    current: &DescriptorAuthority<'_>,
    host_key: &SigningKey,
    media_capabilities: u64,
) -> Result<TransportHint, DescriptorError> {
    validate_authority(current)?;
    if host_key.verifying_key().to_bytes() != current.pinned_host.device_public_key {
        return Err(DescriptorError::Host);
    }
    validate_origin(&settings.origin)?;
    let payload = DescriptorPayload {
        protocol: VERSION,
        workspace: current.workspace,
        workspace_root: current.pinned_governor.user_public_key,
        host_root: current.pinned_host.user_public_key,
        host_device: current.pinned_host.device_public_key,
        host_certificate_digest: certificate_digest(current.pinned_host)?,
        origin: settings.origin.clone(),
        webtransport_path: WEBTRANSPORT_PATH.into(),
        https_base_path: HTTPS_BASE_PATH.into(),
        native_endpoint: format!("{}{WEBTRANSPORT_PATH}", settings.origin),
        delivery_capabilities: settings.delivery_capabilities,
        media_capabilities,
        issued_at_ns: settings.issued_at_ns,
        expires_at_ns: settings.expires_at_ns,
        generation: settings.generation,
    };
    validate_payload(&payload, current)?;
    let opaque_json = serde_json::to_string(&hex::encode(encode_payload(&payload)?))
        .map_err(|_| DescriptorError::Codec)?;
    let hint = tilo_peer_state::join::sign_transport_hint(
        KIND,
        opaque_json,
        current.pinned_host.clone(),
        current.pinned_host.device_public_key,
        host_key,
    )
    .map_err(|_| DescriptorError::Signature)?;
    check_generation(payload.generation, hint_digest(&hint)?, current.generation)?;
    Ok(hint)
}

/// Inert historical metadata for an already owner-persisted signed hint.
/// This is not current authorization, a verified descriptor or a client trust pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersistedHintMetadata {
    /// Signed historical revision, usable only as a stored renewal floor.
    pub generation: u64,
    /// Signed historical issue time.
    pub issued_at_ns: u64,
    /// Signed historical expiry; current time was not checked.
    pub expires_at_ns: u64,
    /// Digest of the exact canonical persisted hint, not an authenticated client pin.
    pub descriptor_digest: [u8; 32],
}

/// Inert routing scope authenticated only against an independently pinned host.
/// This does not establish current membership, authority, capability or freshness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedHintScope {
    /// Canonical signed origin, not an independently established audience.
    pub origin: String,
    /// Signed workspace to compare with independently enrolled custody.
    pub workspace: u64,
    /// Signed workspace root to compare with independently enrolled custody.
    pub workspace_root: [u8; 32],
    /// Historical signed lifetime and revision; caller must check current freshness.
    pub metadata: PersistedHintMetadata,
}

/// Inspect bounded signed routing fields before current governance is available.
/// The caller must match its independent enrollment scope and origin, and check
/// freshness before using this only for the signed governance handoff. Ordinary
/// requests still require `verify_transport_hint` against actual current state.
///
/// # Errors
///
/// Returns `DescriptorError` if the value fails validation or verification.
pub fn inspect_pinned_hint_scope(
    hint: &TransportHint,
    pinned_host: &DeviceCert,
) -> Result<PinnedHintScope, DescriptorError> {
    let (payload, metadata) = inspect_pinned_payload(hint, pinned_host)?;
    Ok(PinnedHintScope {
        origin: payload.origin,
        workspace: payload.workspace,
        workspace_root: payload.workspace_root,
        metadata,
    })
}

/// Inspect canonical owner-stored bytes under an independently pinned host cert.
/// Verifies the signature at its signed issue time, even after descriptor expiry.
/// Revocation, current roster/policy, current expiry and client pin admission are
/// deliberately NOT established: use `verify_transport_hint` for every current use.
/// Callers may only use this to retain/advance the stored descriptor revision.
///
/// # Errors
///
/// Returns `DescriptorError` if the value fails validation or verification.
pub fn inspect_persisted_hint(
    hint: &TransportHint,
    pinned_host: &DeviceCert,
) -> Result<PersistedHintMetadata, DescriptorError> {
    Ok(inspect_pinned_payload(hint, pinned_host)?.1)
}
fn inspect_pinned_payload(
    hint: &TransportHint,
    pinned_host: &DeviceCert,
) -> Result<(DescriptorPayload, PersistedHintMetadata), DescriptorError> {
    if hint.kind != KIND {
        return Err(DescriptorError::Kind);
    }
    if &hint.host != pinned_host
        || hint.service_public_key != pinned_host.device_public_key
        || !pinned_host.capabilities.can_act_as_host
    {
        return Err(DescriptorError::Host);
    }
    let certificate = certificate_digest(pinned_host)?;
    let payload = decode_payload(&hint.opaque_json)?;
    let descriptor_digest = hint_digest(hint)?;
    if payload.protocol != VERSION {
        return Err(DescriptorError::Version);
    }
    if payload.workspace == 0
        || payload.workspace_root == [0; 32]
        || payload.host_root == [0; 32]
        || payload.host_root != pinned_host.user_public_key
        || payload.host_device != pinned_host.device_public_key
        || payload.host_certificate_digest != certificate
    {
        return Err(DescriptorError::Scope);
    }
    validate_origin(&payload.origin)?;
    if payload.webtransport_path != WEBTRANSPORT_PATH
        || payload.https_base_path != HTTPS_BASE_PATH
        || payload.native_endpoint != format!("{}{WEBTRANSPORT_PATH}", payload.origin)
    {
        return Err(DescriptorError::Endpoint);
    }
    if payload.media_capabilities & !MEDIA_CAPABILITIES != 0 {
        return Err(DescriptorError::Capabilities);
    }
    if payload.generation == 0
        || payload
            .expires_at_ns
            .checked_sub(payload.issued_at_ns)
            .as_ref()
            .is_none_or(|n| !(*n > 0 && *n <= LIFETIME_NS))
        || pinned_host
            .expires_at_ns
            .is_some_and(|end| payload.expires_at_ns > end)
    {
        return Err(DescriptorError::Lifetime);
    }
    tilo_peer_state::join::verify_transport_hint_at(
        hint,
        &RevocationList::new(),
        payload.issued_at_ns,
    )
    .map_err(|_| DescriptorError::Signature)?;
    let metadata = PersistedHintMetadata {
        generation: payload.generation,
        issued_at_ns: payload.issued_at_ns,
        expires_at_ns: payload.expires_at_ns,
        descriptor_digest,
    };
    Ok((payload, metadata))
}

/// Verify exact signed bytes against independently pinned current native state.
/// A valid self-signed URL alone cannot construct this opaque result.
///
/// # Errors
///
/// Returns `DescriptorError` if the value fails validation or verification.
pub fn verify_transport_hint(
    hint: &TransportHint,
    current: &DescriptorAuthority<'_>,
) -> Result<VerifiedDescriptor, DescriptorError> {
    let payload = check_hint(hint, current)?;
    let digest = hint_digest(hint)?;
    Ok(VerifiedDescriptor {
        hint: hint.clone(),
        payload,
        digest,
    })
}

/// Private-constructor verified bootstrap. Deriving a fresh authentication scope
/// always revalidates against the caller's actual current native owner.
pub struct VerifiedDescriptor {
    hint: TransportHint,
    payload: DescriptorPayload,
    digest: [u8; 32],
}
impl VerifiedDescriptor {
    /// Exact HTTPS origin authenticated under the governed host.
    pub fn origin(&self) -> &str {
        &self.payload.origin
    }
    /// Exact reliable WebTransport route.
    pub fn webtransport_path(&self) -> &str {
        &self.payload.webtransport_path
    }
    /// Exact HTTPS base route.
    pub fn https_base_path(&self) -> &str {
        &self.payload.https_base_path
    }
    /// Native connection URL, identical to the origin plus WebTransport route.
    pub fn native_endpoint(&self) -> &str {
        &self.payload.native_endpoint
    }
    /// Exact currently certified host pinned during verification.
    pub fn host_certificate(&self) -> &DeviceCert {
        &self.hint.host
    }
    /// Signed allowed delivery bits, separate from media service advertisement.
    pub fn delivery_capabilities(&self) -> u64 {
        self.payload.delivery_capabilities
    }
    /// Authenticated known media service bits; this does not grant participation.
    pub fn media_capabilities(&self) -> u64 {
        self.payload.media_capabilities
    }
    /// Signed revision, to persist in existing owner metadata before use.
    pub fn generation(&self) -> u64 {
        self.payload.generation
    }
    /// Digest of the complete canonical signed hint for full-transcript auth.
    pub fn descriptor_digest(&self) -> [u8; 32] {
        self.digest
    }
    /// Recheck current authority, expiry, host pins and persisted revision.
    ///
    /// # Errors
    ///
    /// Returns `DescriptorError` if the value fails validation or verification.
    pub fn revalidate(&self, current: &DescriptorAuthority<'_>) -> Result<(), DescriptorError> {
        check_hint(&self.hint, current).map(|_| ())
    }
    /// Derive independently pinned client scope; boot remains freshly host-proved.
    ///
    /// # Errors
    ///
    /// Returns `DescriptorError` if the value fails validation or verification.
    pub fn client_scope(
        &self,
        current: &DescriptorAuthority<'_>,
        device: &DeviceCert,
        carrier: auth::Carrier,
        requested: u64,
        mandatory: u64,
    ) -> Result<auth::ClientScope, DescriptorError> {
        self.revalidate(current)?;
        let revocations = current_revocations(current.peer)?;
        validate_device(current, device, &revocations)?;
        if mandatory & !requested != 0 || mandatory & !self.payload.delivery_capabilities != 0 {
            return Err(DescriptorError::Capabilities);
        }
        Ok(auth::ClientScope {
            workspace: self.payload.workspace,
            workspace_root: self.payload.workspace_root,
            device: device.device_public_key,
            device_root: device.user_public_key,
            host: self.payload.host_device,
            host_root: self.payload.host_root,
            origin: self.payload.origin.clone(),
            descriptor_generation: self.payload.generation,
            descriptor_digest: self.digest,
            carrier,
            requested_capabilities: requested,
            mandatory_capabilities: mandatory,
        })
    }
    /// Derive server scope using only that server's own nonzero current boot.
    ///
    /// # Errors
    ///
    /// Returns `DescriptorError` if the value fails validation or verification.
    pub fn server_binding(
        &self,
        current: &DescriptorAuthority<'_>,
        device: &DeviceCert,
        carrier: auth::Carrier,
        requested: u64,
        mandatory: u64,
        own_boot: [u8; 16],
    ) -> Result<auth::Binding, DescriptorError> {
        if own_boot == [0; 16] {
            return Err(DescriptorError::Scope);
        }
        let scope = self.client_scope(current, device, carrier, requested, mandatory)?;
        Ok(auth::Binding {
            workspace: scope.workspace,
            workspace_root: scope.workspace_root,
            device: scope.device,
            device_root: scope.device_root,
            host: scope.host,
            host_root: scope.host_root,
            origin: scope.origin,
            descriptor_generation: scope.descriptor_generation,
            descriptor_digest: scope.descriptor_digest,
            server_boot_id: own_boot,
            carrier: scope.carrier,
            requested_capabilities: scope.requested_capabilities,
            mandatory_capabilities: scope.mandatory_capabilities,
        })
    }
}

fn check_hint(
    hint: &TransportHint,
    current: &DescriptorAuthority<'_>,
) -> Result<DescriptorPayload, DescriptorError> {
    if hint.kind != KIND {
        return Err(DescriptorError::Kind);
    }
    if hint.host != *current.pinned_host
        || hint.service_public_key != current.pinned_host.device_public_key
    {
        return Err(DescriptorError::Host);
    }
    certificate_digest(&hint.host)?;
    let payload = decode_payload(&hint.opaque_json)?;
    let digest = hint_digest(hint)?;
    let revocations = validate_authority(current)?;
    validate_payload(&payload, current)?;
    tilo_peer_state::join::verify_transport_hint_at(hint, &revocations, now_ns(current.now)?)
        .map_err(|_| DescriptorError::Signature)?;
    check_generation(payload.generation, digest, current.generation)?;
    Ok(payload)
}
fn current_revocations(peer: &StatePeer) -> Result<RevocationList, DescriptorError> {
    let mut revocations = RevocationList::new();
    for entry in peer.revocations() {
        revocations
            .insert(entry)
            .map_err(|_| DescriptorError::Authority)?;
    }
    Ok(revocations)
}
fn validate_authority(
    current: &DescriptorAuthority<'_>,
) -> Result<RevocationList, DescriptorError> {
    let peer = current.peer;
    if current.workspace == 0
        || current.pinned_governor.user_public_key == [0; 32]
        || current.pinned_host.user_public_key == [0; 32]
        || peer.authority() != current.pinned_governor
        || !tilo_relay_protocol::room_workspace_id_matches(peer.room(), current.workspace)
        || peer.admitted_room().is_none()
        || tilo_peer_state::room::governance_of(peer.admitted_room())
            .0
            .as_deref()
            != Some(hex::encode(current.pinned_governor.user_public_key).as_str())
    {
        return Err(DescriptorError::Authority);
    }
    let revoked = current_revocations(peer)?;
    verify_device_cert_at(current.pinned_governor, now_ns(current.now)?)
        .map_err(|_| DescriptorError::Certificate)?;
    if revoked.is_revoked(&current.pinned_governor.device_public_key) {
        return Err(DescriptorError::Authority);
    }
    if !current.pinned_host.capabilities.can_act_as_host {
        return Err(DescriptorError::Host);
    }
    validate_device(current, current.pinned_host, &revoked)?;
    Ok(revoked)
}
fn validate_device(
    current: &DescriptorAuthority<'_>,
    device: &DeviceCert,
    revoked: &RevocationList,
) -> Result<(), DescriptorError> {
    verify_device_cert_at(device, now_ns(current.now)?)
        .map_err(|_| DescriptorError::Certificate)?;
    if revoked.is_revoked(&device.device_public_key)
        || !current.peer.certificates().contains(device)
        || !current
            .peer
            .governed_access(current.now)
            .map_err(|_| DescriptorError::Authority)?
            .member_devices
            .contains(&device.device_public_key)
        || current
            .peer
            .permissions(device.device_public_key, current.now)
            .is_err()
    {
        return Err(DescriptorError::Authority);
    }
    Ok(())
}
fn validate_payload(
    payload: &DescriptorPayload,
    current: &DescriptorAuthority<'_>,
) -> Result<(), DescriptorError> {
    if payload.protocol != VERSION {
        return Err(DescriptorError::Version);
    }
    if payload.workspace == 0
        || payload.workspace_root == [0; 32]
        || payload.host_root == [0; 32]
        || payload.workspace != current.workspace
        || payload.workspace_root != current.pinned_governor.user_public_key
        || payload.host_root != current.pinned_host.user_public_key
        || payload.host_device != current.pinned_host.device_public_key
        || payload.host_certificate_digest != certificate_digest(current.pinned_host)?
    {
        return Err(DescriptorError::Scope);
    }
    validate_origin(&payload.origin)?;
    if payload.webtransport_path != WEBTRANSPORT_PATH
        || payload.https_base_path != HTTPS_BASE_PATH
        || payload.native_endpoint.len() > 2112
        || payload.native_endpoint != format!("{}{WEBTRANSPORT_PATH}", payload.origin)
    {
        return Err(DescriptorError::Endpoint);
    }
    if payload.media_capabilities & !MEDIA_CAPABILITIES != 0
        || payload.delivery_capabilities & !current.allowed_delivery_capabilities != 0
    {
        return Err(DescriptorError::Capabilities);
    }
    let now = now_ns(current.now)?;
    if payload.generation == 0
        || payload.issued_at_ns > now
        || now >= payload.expires_at_ns
        || payload
            .expires_at_ns
            .checked_sub(payload.issued_at_ns)
            .as_ref()
            .is_none_or(|n| !(*n > 0 && *n <= LIFETIME_NS))
        || [current.pinned_governor, current.pinned_host]
            .iter()
            .any(|cert| {
                cert.expires_at_ns
                    .is_some_and(|end| payload.expires_at_ns > end)
            })
    {
        return Err(DescriptorError::Lifetime);
    }
    Ok(())
}
fn check_generation(
    generation: u64,
    digest: [u8; 32],
    expected: GenerationExpectation,
) -> Result<(), DescriptorError> {
    let invalid = match expected {
        GenerationExpectation::FirstAccepted { minimum_generation } => {
            minimum_generation == 0 || generation < minimum_generation
        }
        GenerationExpectation::Current {
            generation: prior,
            descriptor_digest,
        } => prior == 0 || generation < prior || generation == prior && digest != descriptor_digest,
    };
    if invalid {
        Err(DescriptorError::Generation)
    } else {
        Ok(())
    }
}
fn now_ns(now: SystemTime) -> Result<u64, DescriptorError> {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| DescriptorError::Lifetime)?
        .as_nanos()
        .try_into()
        .map_err(|_| DescriptorError::Lifetime)
}
fn certificate_digest(cert: &DeviceCert) -> Result<[u8; 32], DescriptorError> {
    if cert.device_label.len() > 1024 {
        return Err(DescriptorError::Codec);
    }
    let mut buffer = [0; MAX_CERT_BYTES];
    let bytes = postcard::to_slice(cert, &mut buffer).map_err(|_| DescriptorError::Codec)?;
    Ok(*blake3::hash(bytes).as_bytes())
}
fn encode_payload(payload: &DescriptorPayload) -> Result<Vec<u8>, DescriptorError> {
    let mut buffer = [0; MAX_PAYLOAD_BYTES];
    postcard::to_slice(payload, &mut buffer)
        .map(|bytes| bytes.to_vec())
        .map_err(|_| DescriptorError::Codec)
}
fn decode_payload(opaque: &str) -> Result<DescriptorPayload, DescriptorError> {
    if opaque.len() > MAX_OPAQUE_BYTES {
        return Err(DescriptorError::Codec);
    }
    let hex: &str = serde_json::from_str(opaque).map_err(|_| DescriptorError::Codec)?;
    if hex.len() > 2 * MAX_PAYLOAD_BYTES
        || !hex.len().is_multiple_of(2)
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    {
        return Err(DescriptorError::Codec);
    }
    if serde_json::to_string(hex).map_err(|_| DescriptorError::Codec)? != opaque {
        return Err(DescriptorError::Codec);
    }
    let bytes = hex::decode(hex).map_err(|_| DescriptorError::Codec)?;
    let (payload, rest): (DescriptorPayload, _) =
        postcard::take_from_bytes(&bytes).map_err(|_| DescriptorError::Codec)?;
    if !rest.is_empty() || encode_payload(&payload)? != bytes {
        return Err(DescriptorError::Codec);
    }
    Ok(payload)
}
fn hint_digest(hint: &TransportHint) -> Result<[u8; 32], DescriptorError> {
    if hint.kind != KIND || hint.opaque_json.len() > MAX_OPAQUE_BYTES {
        return Err(DescriptorError::Codec);
    }
    certificate_digest(&hint.host)?;
    let bytes = postcard::to_allocvec(hint).map_err(|_| DescriptorError::Codec)?;
    if bytes.len() > MAX_HINT_BYTES {
        return Err(DescriptorError::Codec);
    }
    let mut hash = blake3::Hasher::new();
    hash.update(DOMAIN);
    hash.update(&bytes);
    Ok(*hash.finalize().as_bytes())
}
fn bounded_origin<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = <&str>::deserialize(d)?;
    validate_origin(value).map_err(serde::de::Error::custom)?;
    Ok(value.to_owned())
}
fn bounded_route<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = <&str>::deserialize(d)?;
    if value.len() > 16 {
        return Err(serde::de::Error::custom("descriptor route too long"));
    }
    Ok(value.to_owned())
}
fn bounded_endpoint<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = <&str>::deserialize(d)?;
    if value.len() > 2112 {
        return Err(serde::de::Error::custom("descriptor endpoint too long"));
    }
    Ok(value.to_owned())
}
pub(crate) fn validate_origin(origin: &str) -> Result<(), DescriptorError> {
    if origin.len() > 2048 || !origin.is_ascii() {
        return Err(DescriptorError::Endpoint);
    }
    let authority = origin
        .strip_prefix("https://")
        .ok_or(DescriptorError::Endpoint)?;
    if authority.is_empty()
        || authority.bytes().any(|b| {
            b.is_ascii_whitespace()
                || b.is_ascii_control()
                || matches!(b, b'/' | b'\\' | b'?' | b'#' | b'@')
        })
    {
        return Err(DescriptorError::Endpoint);
    }
    let (host, port) = if let Some(ipv6) = authority.strip_prefix('[') {
        let (host, suffix) = ipv6.split_once(']').ok_or(DescriptorError::Endpoint)?;
        host.parse::<std::net::Ipv6Addr>()
            .map_err(|_| DescriptorError::Endpoint)?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or(DescriptorError::Endpoint)?)
        };
        (None, port)
    } else {
        let (host, port) = authority
            .rsplit_once(':')
            .map_or((authority, None), |(h, p)| (h, Some(p)));
        (Some(host), port)
    };
    if let Some(host) = host
        && (host.len() > 253
            || host.split('.').any(|part| {
                part.is_empty()
                    || part.len() > 63
                    || part.starts_with('-')
                    || part.ends_with('-')
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            }))
    {
        return Err(DescriptorError::Endpoint);
    }
    if let Some(port) = port
        && (port.is_empty()
            || !port.bytes().all(|b| b.is_ascii_digit())
            || port.parse::<u16>().ok().as_ref().is_none_or(|p| *p == 0))
    {
        return Err(DescriptorError::Endpoint);
    }
    Ok(())
}

/// Closed bootstrap failures; callers cannot downgrade them to legacy trust.
#[derive(Debug, thiserror::Error)]
pub enum DescriptorError {
    /// Noncanonical, malformed, truncated or oversized descriptor encoding.
    #[error("invalid descriptor encoding")]
    Codec,
    /// Another transport kind cannot authenticate this descriptor.
    #[error("wrong descriptor transport kind")]
    Kind,
    /// Unsupported major protocol version.
    #[error("unsupported descriptor version")]
    Version,
    /// Independent workspace or identity scope differs.
    #[error("descriptor scope mismatch")]
    Scope,
    /// Host identity, capability or signing key differs from the appointed host.
    #[error("invalid descriptor host")]
    Host,
    /// Host signature did not authenticate exact transport-pin bytes.
    #[error("invalid descriptor signature")]
    Signature,
    /// Current certificate is invalid or expired.
    #[error("invalid current descriptor certificate")]
    Certificate,
    /// Current committed governor, roster, role or revocations deny access.
    #[error("descriptor workspace authority denied access")]
    Authority,
    /// Origin, route or native endpoint is invalid.
    #[error("invalid descriptor endpoint")]
    Endpoint,
    /// Delivery bits exceed native policy or media is not installed.
    #[error("unsupported descriptor capabilities")]
    Capabilities,
    /// Invalid issue/expiry interval or certificate/time bound.
    #[error("invalid descriptor lifetime")]
    Lifetime,
    /// Revision is stale or equivocates at a previously accepted generation.
    #[error("stale or conflicting descriptor generation")]
    Generation,
}

#[cfg(test)]
mod tests;
