//! First-contact transport bootstrap: an enrolled device asks the workspace
//! governor for the current signed transport hint. Bounded inert wire values;
//! decoding never grants authority.
//!
//! Host/current-custody checks belong to the native owner and browser Peer.
//! Domains, field order and the split signature tuples are wire contract:
//! joined rooms persist and re-verify what this exchange returned.
use serde::{Deserialize, Deserializer, Serialize, de};
use tilo_user_identity::canonical::signing_digest;

/// Maximum canonical host `DeviceCert` bytes in a bootstrap response.
pub const MAX_CERTIFICATE_BYTES: usize = 4096;

/// Dedicated first-contact request domain; this proof creates no session authority.
pub const BOOTSTRAP_REQUEST_DOMAIN: &[u8] = b"tilo-delivery-v3-bootstrap-request\0";
/// Dedicated governor attestation for independently appointed host and descriptor floor.
pub const BOOTSTRAP_RESPONSE_DOMAIN: &[u8] = b"tilo-delivery-v3-bootstrap-response\0";
/// Bootstrap request ceiling.
pub const MAX_BOOTSTRAP_REQUEST_BYTES: usize = 4096;
/// Bootstrap response ceiling, including the exact canonical transport hint.
pub const MAX_BOOTSTRAP_RESPONSE_BYTES: usize = 32 * 1024;
/// Inert enrolled-device request; native verifies the current enrolled signer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapRequest {
    /// Exactly one.
    pub version: u8,
    /// Enrolled device signing key.
    pub device: [u8; 32],
    /// Fresh Rust-generated nonce retained until response consumption.
    pub nonce: [u8; 32],
    /// Exact admitted workspace.
    pub workspace: u64,
    /// Independently admitted workspace root.
    pub root: [u8; 32],
    /// Exact admitted room.
    #[serde(deserialize_with = "text128")]
    pub room: String,
    /// Digest of the retained candidate descriptor, not an authority pin.
    pub candidate_digest: [u8; 32],
    /// Enrolled device signature over the dedicated request digest.
    pub signature: [[u8; 32]; 2],
}
/// Inert governor response. Browser must verify against its independently admitted governor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapResponse {
    /// Exactly one.
    pub version: u8,
    /// Exact retained signed request, including nonce.
    pub request: BootstrapRequest,
    /// Canonical `DeviceCert` postcard bytes, at most 4096 bytes.
    #[serde(with = "certificate_bytes")]
    pub host_certificate: Vec<u8>,
    /// Exact canonical `TransportHint` postcard bytes, at most 24 KiB.
    #[serde(with = "hint_bytes")]
    pub hint: Vec<u8>,
    /// Owner-stored descriptor generation, never a store-generation substitute.
    pub generation: u64,
    /// Owner-stored descriptor digest, equal to the retained candidate.
    pub descriptor_digest: [u8; 32],
    /// Absolute issue time.
    pub issued_at_ns: u64,
    /// Absolute expiry, at most thirty seconds after issue.
    pub expires_at_ns: u64,
    /// Current governor signature over the dedicated response digest.
    pub signature: [[u8; 32]; 2],
}
impl BootstrapRequest {
    /// Validate structural bounds only.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        if self.version != 1 {
            return Err(WireError::Invalid);
        }
        nonzero(self.device)?;
        nonzero(self.nonce)?;
        nonzero(self.root)?;
        nonzero(self.candidate_digest)?;
        text(&self.room, 128)
    }
    /// Exact typed bootstrap signing digest; creates no verified authority.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(
            BOOTSTRAP_REQUEST_DOMAIN,
            &(
                self.version,
                self.device,
                self.nonce,
                self.workspace,
                self.root,
                &self.room,
                self.candidate_digest,
            ),
        )
    }
}
impl BootstrapResponse {
    /// Validate structural bounds only.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        self.request.validate()?;
        if self.version != 1
            || self.generation == 0
            || self.descriptor_digest != self.request.candidate_digest
            || self.hint.is_empty()
            || self.hint.len() > 24 * 1024
            || self
                .expires_at_ns
                .checked_sub(self.issued_at_ns)
                .is_none_or(|n| n == 0 || n > 30_000_000_000)
        {
            return Err(WireError::Invalid);
        }
        certificate_limit(&self.host_certificate)
    }
    /// Exact typed governor response digest; creates no verified authority.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(
            BOOTSTRAP_RESPONSE_DOMAIN,
            &(
                self.version,
                &self.request,
                serde_bytes::Bytes::new(&self.host_certificate),
                serde_bytes::Bytes::new(&self.hint),
                self.generation,
                self.descriptor_digest,
                self.issued_at_ns,
                self.expires_at_ns,
            ),
        )
    }
}
mod hint_bytes {
    use super::{Deserializer, Serialize, de};
    pub fn serialize<S: serde::Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(v).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        struct V;
        impl de::Visitor<'_> for V {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("1 to 24576 canonical hint bytes")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                if v.is_empty() || v.len() > 24 * 1024 {
                    return Err(E::custom("bootstrap hint exceeds bounds"));
                }
                Ok(v.to_vec())
            }
        }
        d.deserialize_bytes(V)
    }
}

macro_rules! codec {
    ($ty:ty,$cap:expr) => {
        impl $ty {
            /// Canonical encoding after checking all structural bounds.
            ///
            /// # Errors
            ///
            /// Returns `WireError` if the value violates its wire bounds or cannot be serialized.
            pub fn encode(&self) -> Result<Vec<u8>, WireError> {
                self.validate()?;
                let out = postcard::to_allocvec(self).map_err(|_| WireError::Codec)?;
                if out.len() > $cap {
                    return Err(WireError::Codec);
                }
                Ok(out)
            }
            /// Reject outer size before parsing, bounded fields before copies, and noncanonical tails.
            ///
            /// # Errors
            ///
            /// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
            pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
                if bytes.is_empty() || bytes.len() > $cap {
                    return Err(WireError::Codec);
                }
                let value: Self = postcard::from_bytes(bytes).map_err(|_| WireError::Codec)?;
                if value.encode()? != bytes {
                    return Err(WireError::Codec);
                }
                Ok(value)
            }
        }
    };
}
codec!(BootstrapRequest, MAX_BOOTSTRAP_REQUEST_BYTES);
codec!(BootstrapResponse, MAX_BOOTSTRAP_RESPONSE_BYTES);
/// Generate an ephemeral nonce using the existing secure entropy provider.
/// This creates no signing identity and conveys no authority.
///
/// # Errors
///
/// Returns `WireError` if the inputs violate this operation's bounds or invariants.
pub fn fresh_nonce() -> Result<[u8; 32], WireError> {
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).map_err(|_| WireError::Entropy)?;
    nonzero(nonce)?;
    Ok(nonce)
}
/// Convert a fixed signature into its existing Postcard array shape.
pub fn split_signature(signature: [u8; 64]) -> [[u8; 32]; 2] {
    let mut v = [[0; 32]; 2];
    v[0].copy_from_slice(&signature[..32]);
    v[1].copy_from_slice(&signature[32..]);
    v
}
/// Restore a fixed signature without allocation.
pub fn join_signature(v: [[u8; 32]; 2]) -> [u8; 64] {
    let mut s = [0; 64];
    s[..32].copy_from_slice(&v[0]);
    s[32..].copy_from_slice(&v[1]);
    s
}
fn digest<T: Serialize>(domain: &[u8], v: &T) -> Result<[u8; 32], WireError> {
    signing_digest(domain, v).map_err(|_| WireError::Codec)
}
fn nonzero<const N: usize>(v: [u8; N]) -> Result<(), WireError> {
    if v == [0; N] {
        Err(WireError::Invalid)
    } else {
        Ok(())
    }
}
fn text(v: &str, max: usize) -> Result<(), WireError> {
    if v.is_empty() || v.len() > max || v.chars().any(char::is_control) {
        Err(WireError::Invalid)
    } else {
        Ok(())
    }
}
fn certificate_limit(v: &[u8]) -> Result<(), WireError> {
    if v.is_empty() || v.len() > MAX_CERTIFICATE_BYTES {
        Err(WireError::Invalid)
    } else {
        Ok(())
    }
}
fn bounded_text<'de, D: Deserializer<'de>, const MAX: usize>(d: D) -> Result<String, D::Error> {
    struct V<const N: usize>;
    impl<const N: usize> de::Visitor<'_> for V<N> {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "nonempty text of at most {N} bytes")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            text(v, N).map_err(E::custom)?;
            Ok(v.to_owned())
        }
    }
    d.deserialize_str(V::<MAX>)
}
fn text128<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_text::<D, 128>(d)
}
mod certificate_bytes {
    use super::{Deserializer, Serialize, certificate_limit, de};
    pub fn serialize<S: serde::Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(v).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        struct V;
        impl de::Visitor<'_> for V {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("1 to 4096 certificate bytes")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                certificate_limit(v).map_err(E::custom)?;
                Ok(v.to_vec())
            }
        }
        d.deserialize_bytes(V)
    }
}
/// Closed codec/shape failures; no authority claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// Existing entropy provider failed.
    #[error("bootstrap entropy unavailable")]
    Entropy,
    /// Malformed, oversized, truncated, or noncanonical encoding.
    #[error("invalid bootstrap wire encoding")]
    Codec,
    /// Invalid bounded inert field.
    #[error("invalid bootstrap wire field")]
    Invalid,
}
#[cfg(test)]
mod tests;
