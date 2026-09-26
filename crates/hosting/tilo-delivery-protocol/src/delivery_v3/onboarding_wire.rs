//! Signed, allocation-preflighted HTTPS join wrappers. All decoded values are inert.
//! Certificate trust, invitation/current membership, replay custody and host appointment
//! remain with the actual owner; cryptographic possession never grants membership.
#![allow(missing_docs)]
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;
use uuid::Uuid;
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
pub const REQUEST_DOMAIN: &[u8] = b"tilo-delivery-v3-onboarding-request\0";
pub const RESPONSE_DOMAIN: &[u8] = b"tilo-delivery-v3-onboarding-response\0";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardingOperation {
    Inspect,
    Challenge,
    Complete,
    MembershipChallenge,
    MembershipComplete,
}
impl OnboardingOperation {
    pub fn path(self) -> &'static str {
        match self {
            Self::Inspect => "/peer/v3/join/inspect",
            Self::Challenge => "/peer/v3/join/challenge",
            Self::Complete => "/peer/v3/join/complete",
            Self::MembershipChallenge => "/peer/v3/join/membership-challenge",
            Self::MembershipComplete => "/peer/v3/join/membership-complete",
        }
    }
    /// # Errors
    ///
    /// Returns `WireError` if the inputs violate this operation's bounds or invariants.
    pub fn from_target(method: &str, target: &str) -> Result<Self, WireError> {
        [
            Self::Inspect,
            Self::Challenge,
            Self::Complete,
            Self::MembershipChallenge,
            Self::MembershipComplete,
        ]
        .into_iter()
        .find(|op| method == "POST" && op.path() == target)
        .ok_or(WireError::Invalid)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingScope<'a> {
    #[serde(borrow)]
    pub origin: &'a str,
    pub host: [u8; 32],
    pub workspace: u64,
    pub root: [u8; 32],
    pub room: Uuid,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingRequest<'a> {
    pub version: u16,
    pub operation: OnboardingOperation,
    #[serde(borrow)]
    pub scope: OnboardingScope<'a>,
    #[serde(borrow, with = "serde_bytes")]
    pub certificate: &'a [u8],
    pub nonce: [u8; 32],
    pub issued_at_ns: u64,
    pub expires_at_ns: u64,
    pub body_digest: [u8; 32],
    #[serde(borrow, with = "serde_bytes")]
    pub body: &'a [u8],
    pub signature: [[u8; 32]; 2],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplyBodyKind {
    InlineLegacy,
    HandoffManifest,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingResponse<'a> {
    pub version: u16,
    pub operation: OnboardingOperation,
    pub body_kind: ReplyBodyKind,
    pub request_digest: [u8; 32],
    pub request_nonce: [u8; 32],
    pub host: [u8; 32],
    pub server_boot_id: [u8; 16],
    pub issued_at_ns: u64,
    pub expires_at_ns: u64,
    pub body_digest: [u8; 32],
    #[serde(borrow, with = "serde_bytes")]
    pub body: &'a [u8],
    pub signature: [[u8; 32]; 2],
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WireError {
    #[error("invalid onboarding encoding")]
    Codec,
    #[error("onboarding exceeds bound")]
    Oversized,
    #[error("invalid onboarding context")]
    Invalid,
    #[error("invalid onboarding signature")]
    Signature,
    #[error("onboarding scope mismatch")]
    Scope,
    #[error("onboarding proof expired")]
    Lifetime,
}
const LIFETIME_NS: u64 = 60_000_000_000;
const TOLERANCE_NS: u64 = 30_000_000_000;
fn lifetime(issued: u64, expires: u64, max: u64) -> Result<(), WireError> {
    if expires
        .checked_sub(issued)
        .is_none_or(|n| n == 0 || n > max)
    {
        Err(WireError::Lifetime)
    } else {
        Ok(())
    }
}
fn current(issued: u64, expires: u64, now: u64) -> Result<(), WireError> {
    if issued > now.saturating_add(TOLERANCE_NS) || now >= expires {
        Err(WireError::Lifetime)
    } else {
        Ok(())
    }
}
impl OnboardingScope<'_> {
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        super::auth::validate_origin(self.origin).map_err(|_| WireError::Invalid)?;
        if self.host == [0; 32] || self.root == [0; 32] || self.room.is_nil() {
            return Err(WireError::Invalid);
        }
        Ok(())
    }
}
fn verify(key: &[u8; 32], digest: &[u8; 32], signature: &[[u8; 32]; 2]) -> Result<(), WireError> {
    let mut bytes = [0; 64];
    bytes[..32].copy_from_slice(&signature[0]);
    bytes[32..].copy_from_slice(&signature[1]);
    ed25519_dalek::VerifyingKey::from_bytes(key)
        .map_err(|_| WireError::Signature)?
        .verify_strict(digest, &ed25519_dalek::Signature::from_bytes(&bytes))
        .map_err(|_| WireError::Signature)
}
// Streaming canonical flavors retain no encoded body/certificate copy.
struct HashFlavor(blake3::Hasher);
impl postcard::ser_flavors::Flavor for HashFlavor {
    type Output = [u8; 32];
    fn try_push(&mut self, b: u8) -> postcard::Result<()> {
        self.0.update(&[b]);
        Ok(())
    }
    fn try_extend(&mut self, b: &[u8]) -> postcard::Result<()> {
        self.0.update(b);
        Ok(())
    }
    fn finalize(self) -> postcard::Result<Self::Output> {
        Ok(*self.0.finalize().as_bytes())
    }
}
fn digest<T: Serialize>(domain: &[u8], value: &T) -> Result<[u8; 32], WireError> {
    let mut h = blake3::Hasher::new();
    h.update(domain);
    postcard::serialize_with_flavor(value, HashFlavor(h)).map_err(|_| WireError::Codec)
}
struct Exact<'a>(&'a [u8]);
impl postcard::ser_flavors::Flavor for Exact<'_> {
    type Output = ();
    fn try_push(&mut self, b: u8) -> postcard::Result<()> {
        self.try_extend(&[b])
    }
    fn try_extend(&mut self, b: &[u8]) -> postcard::Result<()> {
        let Some(rest) = self.0.strip_prefix(b) else {
            return Err(postcard::Error::SerializeBufferFull);
        };
        self.0 = rest;
        Ok(())
    }
    fn finalize(self) -> postcard::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(postcard::Error::SerializeBufferFull)
        }
    }
}
fn canonical<'a, T: Deserialize<'a> + Serialize>(
    bytes: &'a [u8],
    max: usize,
) -> Result<T, WireError> {
    if bytes.len() > max {
        return Err(WireError::Oversized);
    }
    let (value, tail): (T, _) = postcard::take_from_bytes(bytes).map_err(|_| WireError::Codec)?;
    if !tail.is_empty() {
        return Err(WireError::Codec);
    }
    postcard::serialize_with_flavor(&value, Exact(bytes)).map_err(|_| WireError::Codec)?;
    Ok(value)
}
fn encode<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>, WireError> {
    if postcard::experimental::serialized_size(value).map_err(|_| WireError::Codec)? > max {
        return Err(WireError::Oversized);
    }
    postcard::to_stdvec(value).map_err(|_| WireError::Codec)
}
#[derive(Serialize, Deserialize)]
struct BorrowedCertificate<'a> {
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    #[serde(borrow)]
    device_label: &'a str,
    capabilities: tilo_user_identity::DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
    #[serde(borrow, with = "serde_bytes")]
    signature: &'a [u8],
}
fn certificate_preflight(bytes: &[u8]) -> Result<(), WireError> {
    let cert: BorrowedCertificate<'_> = canonical(bytes, 4096)?;
    if cert.signature.len() != 64 {
        return Err(WireError::Invalid);
    }
    Ok(())
}
impl<'a> OnboardingRequest<'a> {
    fn validate(&self) -> Result<(), WireError> {
        self.scope.validate()?;
        lifetime(self.issued_at_ns, self.expires_at_ns, LIFETIME_NS)?;
        certificate_preflight(self.certificate)?;
        if postcard::experimental::serialized_size(self).map_err(|_| WireError::Codec)?
            > MAX_REQUEST_BYTES
        {
            return Err(WireError::Oversized);
        }
        if self.version != 3
            || self.nonce == [0; 32]
            || self.body.is_empty()
            || self.body.len() > MAX_REQUEST_BYTES
            || self.body_digest != *blake3::hash(self.body).as_bytes()
            || self.body.first() != Some(&(self.operation as u8))
        {
            return Err(WireError::Invalid);
        }
        Ok(())
    }
    /// # Errors
    ///
    /// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, WireError> {
        let value: Self = canonical(bytes, MAX_REQUEST_BYTES)?;
        value.validate()?;
        Ok(value)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        self.validate()?;
        encode(self, MAX_REQUEST_BYTES)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(
            REQUEST_DOMAIN,
            &(
                self.version,
                self.operation,
                self.scope,
                *blake3::hash(self.certificate).as_bytes(),
                self.nonce,
                self.issued_at_ns,
                self.expires_at_ns,
                self.body_digest,
            ),
        )
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn request_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(b"tilo-delivery-v3-onboarding-request-id\0", self)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the inputs violate this operation's bounds or invariants.
    pub fn device_certificate(&self) -> Result<DeviceCert, WireError> {
        certificate_preflight(self.certificate)?;
        postcard::from_bytes(self.certificate).map_err(|_| WireError::Codec)
    }
    /// Verifies exact transport possession only. Caller owns certificate trust,
    /// current invitation/membership and non-evicting replay admission.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn verify_device_signature(
        &self,
        expected: OnboardingScope<'_>,
        certificate: &DeviceCert,
        now_ns: u64,
    ) -> Result<(), WireError> {
        self.validate()?;
        current(self.issued_at_ns, self.expires_at_ns, now_ns)?;
        if self.scope != expected {
            return Err(WireError::Scope);
        }
        postcard::serialize_with_flavor(certificate, Exact(self.certificate))
            .map_err(|_| WireError::Scope)?;
        verify(
            &certificate.device_public_key,
            &self.signing_digest()?,
            &self.signature,
        )
    }
}
impl<'a> OnboardingResponse<'a> {
    fn validate(&self) -> Result<(), WireError> {
        lifetime(self.issued_at_ns, self.expires_at_ns, LIFETIME_NS)?;
        if postcard::experimental::serialized_size(self).map_err(|_| WireError::Codec)?
            > MAX_RESPONSE_BYTES
        {
            return Err(WireError::Oversized);
        }
        if self.version != 3
            || self.request_digest == [0; 32]
            || self.request_nonce == [0; 32]
            || self.host == [0; 32]
            || self.server_boot_id == [0; 16]
            || self.body.is_empty()
            || self.body.len() > MAX_RESPONSE_BYTES
            || self.body_digest != *blake3::hash(self.body).as_bytes()
        {
            return Err(WireError::Invalid);
        }
        // JoinTransportResponse discriminants: the operation's own reply, 5 =
        // Error, and 6 = OwnerMembershipComplete (membership completion only).
        if self.body_kind == ReplyBodyKind::InlineLegacy
            && self.body.first() != Some(&(self.operation as u8))
            && self.body.first() != Some(&5)
            && !(self.operation == OnboardingOperation::MembershipComplete
                && self.body.first() == Some(&6))
        {
            return Err(WireError::Invalid);
        }
        Ok(())
    }
    /// # Errors
    ///
    /// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, WireError> {
        let value: Self = canonical(bytes, MAX_RESPONSE_BYTES)?;
        value.validate()?;
        Ok(value)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        self.validate()?;
        encode(self, MAX_RESPONSE_BYTES)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(
            RESPONSE_DOMAIN,
            &(
                self.version,
                self.operation,
                self.body_kind,
                self.request_digest,
                self.request_nonce,
                self.host,
                self.server_boot_id,
                self.issued_at_ns,
                self.expires_at_ns,
                self.body_digest,
            ),
        )
    }
    /// Uses the independently pinned request host. The authenticated boot is
    /// returned as inert data; native appointment and bootstrap admission remain external.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn verify_host_signature(
        &self,
        request: &OnboardingRequest<'_>,
        now_ns: u64,
    ) -> Result<(), WireError> {
        self.validate()?;
        current(self.issued_at_ns, self.expires_at_ns, now_ns)?;
        if self.operation != request.operation
            || self.request_digest != request.request_digest()?
            || self.request_nonce != request.nonce
            || self.host != request.scope.host
        {
            return Err(WireError::Scope);
        }
        verify(
            &request.scope.host,
            &self.signing_digest()?,
            &self.signature,
        )?;
        if self.body_kind == ReplyBodyKind::HandoffManifest {
            let manifest = HandoffManifest::decode(self.body)?;
            if manifest.purpose != HandoffPurpose::JoinResponse
                || manifest.server_boot_id != self.server_boot_id
            {
                return Err(WireError::Scope);
            }
            manifest.verify_host_signature(
                request.scope,
                request.device_certificate()?.device_public_key,
                request.request_digest()?,
                request.nonce,
                now_ns,
            )?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;

mod handoff;
pub use handoff::*;
