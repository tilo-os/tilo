//! Device-signed HTTPS request admission with bounded per-session replay evidence.
//!
//! Parsing and signing grant no authority. The native ingress supplies the exact
//! raw target before rewriting, bounded immutable body, and current authority;
//! this module neither routes requests nor admits durable business effects.

use super::auth::{self, Binding, Carrier, VerificationContext, VerifiedSession};
use super::framing::{MAX_AUTH_BYTES, VERSION};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Maximum encoded proof bytes, independent of request-target size.
pub const MAX_PROOF_BYTES: usize = 256;
/// Maximum exact origin-form target bytes.
pub const MAX_TARGET_BYTES: usize = 16 * 1024;
/// Maximum body bytes; route-specific bounds can be smaller.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Maximum live replay entries for one authenticated session.
pub const MAX_NONCES: usize = 16_384;
const MAX_PATH_BYTES: usize = 1024;
const MAX_VALUE_BYTES: usize = 12 * 1024;
const LIFETIME_NS: u64 = 60_000_000_000;
const TOLERANCE_NS: u64 = 30_000_000_000;
const REQUEST_DOMAIN: &[u8] = b"tilo-delivery-v3-http-request\0";
const SESSION_DOMAIN: &[u8] = b"tilo-delivery-v3-http-session\0";

/// Closed authenticated-method registry, encoded as Postcard indices 0–3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Method {
    /// GET, including subscriptions and authorized reads.
    Get,
    /// POST, including canonical state and private commands.
    Post,
    /// PUT, including negotiated object chunks.
    Put,
    /// DELETE, including explicit session detach.
    Delete,
}

/// Structurally valid actual request bytes; this is not authorization.
pub struct Request<'a> {
    method: Method,
    target: &'a str,
    body: &'a [u8],
}
impl<'a> Request<'a> {
    /// Validate exact uppercase method and canonical raw target without rewriting.
    /// The same immutable body must be passed onward to the business owner.
    ///
    /// # Errors
    ///
    /// Returns `ProofError` if the inputs violate this operation's bounds or invariants.
    pub fn new(method: &str, exact_target: &'a str, body: &'a [u8]) -> Result<Self, ProofError> {
        let method = match method {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            _ => return Err(ProofError::Method),
        };
        validate_target(exact_target)?;
        if body.len() > MAX_BODY_BYTES {
            return Err(ProofError::Body);
        }
        Ok(Self {
            method,
            target: exact_target,
            body,
        })
    }
    /// Exact authenticated HTTP method.
    pub fn method(&self) -> Method {
        self.method
    }
    /// Exact raw origin-form target; no normalization was performed.
    pub fn target(&self) -> &str {
        self.target
    }
    /// Exact immutable body whose digest participates in the signature.
    pub fn body(&self) -> &[u8] {
        self.body
    }
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}
fn upper_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => byte.checked_sub(b'0'),
        b'A'..=b'F' => byte.checked_sub(b'A' - 10),
        _ => None,
    }
}
fn validate_target(target: &str) -> Result<(), ProofError> {
    if target.len() > MAX_TARGET_BYTES || !target.is_ascii() {
        return Err(ProofError::Target);
    }
    let (path, query) = target
        .split_once('?')
        .map_or((target, None), |(p, q)| (p, Some(q)));
    if path.len() > MAX_PATH_BYTES {
        return Err(ProofError::Target);
    }
    let Some(segments) = path.strip_prefix('/') else {
        return Err(ProofError::Target);
    };
    for segment in segments.split('/') {
        if segment.is_empty() || matches!(segment, "." | "..") || !segment.bytes().all(unreserved) {
            return Err(ProofError::Target);
        }
    }
    if let Some(query) = query {
        let mut previous = None;
        for (index, pair) in query.split('&').enumerate() {
            if index >= 32 {
                return Err(ProofError::Target);
            }
            let (name, value) = pair.split_once('=').ok_or(ProofError::Target)?;
            if name.is_empty()
                || name.len() > 64
                || !name.bytes().all(unreserved)
                || previous.is_some_and(|p| p >= name)
                || value.len() > MAX_VALUE_BYTES
            {
                return Err(ProofError::Target);
            }
            previous = Some(name);
            let mut rest = value.as_bytes();
            while let Some((&first, tail)) = rest.split_first() {
                if unreserved(first) {
                    rest = tail;
                    continue;
                }
                let [b'%', high, low, tail @ ..] = rest else {
                    return Err(ProofError::Target);
                };
                let byte = (upper_hex(*high).ok_or(ProofError::Target)? << 4)
                    | upper_hex(*low).ok_or(ProofError::Target)?;
                if unreserved(byte) || byte == b'%' || byte.is_ascii_control() {
                    return Err(ProofError::Target);
                }
                rest = tail;
            }
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct ProofWire {
    protocol: u16,
    session_id: [u8; 16],
    nonce: [u8; 32],
    issued_at_ns: u64,
    expires_at_ns: u64,
    #[serde(with = "signature_bytes")]
    signature: [u8; 64],
}

/// Untrusted proof header value. Intentionally has no Debug implementation.
pub struct RequestProof {
    wire: ProofWire,
}
impl RequestProof {
    /// Sign with the existing custody key and a fresh nonce. Caller-supplied
    /// session parameters are inert; admission derives them independently.
    ///
    /// # Errors
    ///
    /// Returns `ProofError` if the value cannot be canonically encoded.
    pub fn sign(
        session_id: [u8; 16],
        session_digest: [u8; 32],
        request: &Request<'_>,
        issued_at_ns: u64,
        expires_at_ns: u64,
        device_key: &SigningKey,
    ) -> Result<Self, ProofError> {
        let mut nonce = [0; 32];
        rand::rng().fill_bytes(&mut nonce);
        let mut wire = ProofWire {
            protocol: VERSION,
            session_id,
            nonce,
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        validate_wire(&wire)?;
        wire.signature = device_key
            .sign(&request_digest(&wire, session_digest, request)?)
            .to_bytes();
        Ok(Self { wire })
    }
    /// Inert lookup hint; never creates or authorizes a session.
    pub fn session_id(&self) -> [u8; 16] {
        self.wire.session_id
    }
    /// Encode canonical bounded proof bytes. Do not log these bytes.
    ///
    /// # Errors
    ///
    /// Returns `ProofError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, ProofError> {
        validate_wire(&self.wire)?;
        let mut buffer = [0; MAX_PROOF_BYTES];
        postcard::to_slice(&self.wire, &mut buffer)
            .map(|b| b.to_vec())
            .map_err(|_| ProofError::Codec)
    }
    /// Decode a complete canonical bounded proof without admission or cache effects.
    ///
    /// # Errors
    ///
    /// Returns `ProofError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProofError> {
        if bytes.len() > MAX_PROOF_BYTES {
            return Err(ProofError::Codec);
        }
        let (wire, rest) = postcard::take_from_bytes(bytes).map_err(|_| ProofError::Codec)?;
        let proof = Self { wire };
        if !rest.is_empty() || proof.encode()? != bytes {
            return Err(ProofError::Codec);
        }
        Ok(proof)
    }
}
fn validate_wire(wire: &ProofWire) -> Result<(), ProofError> {
    if wire.protocol != VERSION {
        return Err(ProofError::Version);
    }
    if wire.session_id == [0; 16] || wire.nonce == [0; 32] {
        return Err(ProofError::Session);
    }
    if wire
        .expires_at_ns
        .checked_sub(wire.issued_at_ns)
        .as_ref()
        .is_none_or(|n| !(*n > 0 && *n <= LIFETIME_NS))
        || wire.expires_at_ns.checked_add(TOLERANCE_NS).is_none()
    {
        return Err(ProofError::Lifetime);
    }
    Ok(())
}

/// Pure digest helper for the custody owner's already verified handshake inputs.
/// Arbitrary inputs yield an inert digest, never verified session evidence.
///
/// # Errors
///
/// Returns `ProofError` if the value cannot be canonically encoded.
pub fn session_binding_digest(
    binding: &Binding,
    session_id: [u8; 16],
    capabilities: u64,
    session_expires_at_ns: u64,
) -> Result<[u8; 32], ProofError> {
    let mut buffer = [0; MAX_AUTH_BYTES];
    let bytes = postcard::to_slice(
        &(binding, session_id, capabilities, session_expires_at_ns),
        &mut buffer,
    )
    .map_err(|_| ProofError::Codec)?;
    let mut hash = blake3::Hasher::new();
    hash.update(SESSION_DOMAIN);
    hash.update(bytes);
    Ok(*hash.finalize().as_bytes())
}
fn request_digest(
    wire: &ProofWire,
    session_digest: [u8; 32],
    request: &Request<'_>,
) -> Result<[u8; 32], ProofError> {
    let body_digest = *blake3::hash(request.body).as_bytes();
    let bytes = postcard::to_allocvec(&(
        wire.protocol,
        wire.session_id,
        session_digest,
        request.method,
        request.target,
        body_digest,
        wire.nonce,
        wire.issued_at_ns,
        wire.expires_at_ns,
    ))
    .map_err(|_| ProofError::Codec)?;
    if bytes.len() > MAX_TARGET_BYTES + 256 {
        return Err(ProofError::Codec);
    }
    let mut hash = blake3::Hasher::new();
    hash.update(REQUEST_DOMAIN);
    hash.update(&bytes);
    Ok(*hash.finalize().as_bytes())
}

/// Single owner of one consumed authenticated HTTPS session and its replay cache.
/// No clone, reset, serialization, or session-extraction API is provided.
pub struct HttpSessionAdmission {
    session: VerifiedSession,
    digest: [u8; 32],
    nonces: HashMap<[u8; 32], u64>,
    next_deadline: Option<u64>,
    high_water_ns: u64,
}
impl HttpSessionAdmission {
    /// Consume the verified session and require current native HTTPS authority.
    ///
    /// # Errors
    ///
    /// Returns `ProofError` if the inputs violate this operation's bounds or invariants.
    pub fn new(
        session: VerifiedSession,
        current: &VerificationContext<'_>,
    ) -> Result<Self, ProofError> {
        session.revalidate(current)?;
        if session.binding().carrier != Carrier::Https {
            return Err(ProofError::Carrier);
        }
        let digest = session_binding_digest(
            session.binding(),
            session.session_id(),
            session.capabilities(),
            session.expires_at_ns(),
        )?;
        Ok(Self {
            session,
            digest,
            nonces: HashMap::new(),
            next_deadline: None,
            high_water_ns: current.now_ns,
        })
    }
    /// Revalidate this session against fresh native authority and trusted time.
    /// Advances the clock high-water mark even when authority validation fails;
    /// it never consumes, prunes, resets or allocates replay-cache entries.
    ///
    /// The returned session borrow is tied to this admission owner. It is not
    /// request or route authorization and cannot construct a `VerifiedRequest`.
    /// A future private held-poll registry must retain the original authenticated
    /// immutable request, cursor, source, session/carrier generation, deadline
    /// and cancellation state, and recheck current native authorization and
    /// filtering on every page. This check alone authorizes no continuation.
    ///
    /// # Errors
    ///
    /// Returns `ProofError` if the value fails validation or verification.
    pub fn revalidate(
        &mut self,
        current: &VerificationContext<'_>,
    ) -> Result<&VerifiedSession, ProofError> {
        let now = current.now_ns;
        if now < self.high_water_ns {
            return Err(ProofError::ClockRollback);
        }
        self.high_water_ns = now;
        self.session.revalidate(current)?;
        if self.session.binding().carrier != Carrier::Https {
            return Err(ProofError::Carrier);
        }
        Ok(&self.session)
    }
    /// Atomically revalidate, verify and record one proof before returning a witness.
    /// Shared handlers must lock this same owner for the complete synchronous call.
    /// Native authorization/commit scheduling must not bypass its own authority.
    ///
    /// # Errors
    ///
    /// Returns `ProofError` if the resource budget refuses the allocation or the input is invalid.
    pub fn admit<'a>(
        &'a mut self,
        proof: &RequestProof,
        request: &'a Request<'a>,
        current: &VerificationContext<'_>,
    ) -> Result<VerifiedRequest<'a>, ProofError> {
        self.revalidate(current)?;
        let now = current.now_ns;
        let wire = &proof.wire;
        validate_wire(wire)?;
        if wire.session_id != self.session.session_id() {
            return Err(ProofError::Session);
        }
        let deadline = wire
            .expires_at_ns
            .checked_add(TOLERANCE_NS)
            .ok_or(ProofError::Lifetime)?
            .min(self.session.expires_at_ns());
        if wire.expires_at_ns > self.session.expires_at_ns()
            || now >= deadline
            || wire.issued_at_ns > now.checked_add(TOLERANCE_NS).ok_or(ProofError::Lifetime)?
        {
            return Err(ProofError::Lifetime);
        }
        let key = VerifyingKey::from_bytes(&self.session.device_certificate().device_public_key)
            .map_err(|_| ProofError::Signature)?;
        key.verify_strict(
            &request_digest(wire, self.digest, request)?,
            &ed25519_dalek::Signature::from_bytes(&wire.signature),
        )
        .map_err(|_| ProofError::Signature)?;
        if self.next_deadline.is_some_and(|next| now >= next) {
            self.nonces.retain(|_, until| *until > now);
            self.next_deadline = self.nonces.values().copied().min();
        }
        if self.nonces.contains_key(&wire.nonce) {
            return Err(ProofError::Replay);
        }
        if self.nonces.len() >= MAX_NONCES {
            return Err(ProofError::ReplayCapacity);
        }
        self.nonces
            .try_reserve(1)
            .map_err(|_| ProofError::Allocation)?;
        self.nonces.insert(wire.nonce, deadline);
        self.next_deadline = Some(self.next_deadline.map_or(deadline, |old| old.min(deadline)));
        Ok(VerifiedRequest {
            session: &self.session,
            request,
        })
    }
}

/// Opaque, borrowed evidence for these exact request bytes and verified session.
/// Consume synchronously before releasing the admission owner; do not retain
/// across response streaming. This is never a durable business commit receipt.
pub struct VerifiedRequest<'a> {
    session: &'a VerifiedSession,
    request: &'a Request<'a>,
}
impl<'a> VerifiedRequest<'a> {
    /// Exact immutable request admitted by the device signature.
    pub fn request(&self) -> &Request<'a> {
        self.request
    }
    /// Certified source evidence from the consumed verified session.
    pub fn session(&self) -> &VerifiedSession {
        self.session
    }
}

/// Closed failures without proof/header/body values in diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    /// Truncated, oversized, noncanonical or otherwise malformed encoding.
    #[error("invalid HTTP proof encoding")]
    Codec,
    /// Unsupported request-proof protocol major.
    #[error("unsupported HTTP proof version")]
    Version,
    /// Method is not an exact supported uppercase token.
    #[error("invalid HTTP proof method")]
    Method,
    /// Raw target is noncanonical or exceeds its explicit bounds.
    #[error("invalid HTTP proof target")]
    Target,
    /// Actual request body exceeds the transport bound.
    #[error("HTTP proof body exceeds limit")]
    Body,
    /// Session is not an HTTPS carrier.
    #[error("HTTP proof requires HTTPS session")]
    Carrier,
    /// Session identifier or nonce is invalid for this owner.
    #[error("HTTP proof session mismatch")]
    Session,
    /// Invalid, expired, future or overflowing proof lifetime.
    #[error("invalid HTTP proof lifetime")]
    Lifetime,
    /// Trusted server time moved backward behind replay-cache evidence.
    #[error("HTTP admission clock moved backward")]
    ClockRollback,
    /// Device signature does not authenticate these exact request/session bytes.
    #[error("invalid HTTP request signature")]
    Signature,
    /// This nonce has already been admitted and remains live.
    #[error("HTTP request proof replayed")]
    Replay,
    /// Cache is full of still-live evidence; no evidence was evicted.
    #[error("HTTP request replay cache at capacity")]
    ReplayCapacity,
    /// Bounded replay allocation could not be reserved.
    #[error("HTTP request replay allocation failed")]
    Allocation,
    /// Current native session authority denied admission.
    #[error("HTTP session authority: {0}")]
    Authority(#[from] auth::AuthError),
}

mod signature_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        signature: &[u8; 64],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(signature)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 64], D::Error> {
        let bytes = <&[u8]>::deserialize(deserializer)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("HTTP proof signature must contain 64 bytes"))
    }
}

#[cfg(test)]
mod tests;
