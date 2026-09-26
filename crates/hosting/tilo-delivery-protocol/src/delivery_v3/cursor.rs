//! Opaque bounded host-signed cursors; authentication here grants no authority.
use crate::delivery_v3::{
    commit::{CommitRef, ConsumerId, Digest, StoreGeneration},
    framing::VERSION,
};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, de};
/// Maximum opaque cursor size accepted from an untrusted peer.
pub const MAX_CURSOR_BYTES: usize = 4096;
const DOMAIN: &[u8] = b"tilo-delivery-v3-cursor-v1\0";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Inert fields the host custody owner signs into a cursor.
pub struct CursorIssue {
    /// Canonical room UUID text.
    pub room: String,
    /// Bound workspace identifier.
    pub workspace: u64,
    /// Certified workspace root.
    pub workspace_root: [u8; 32],
    /// Certified host device key.
    pub host_device: [u8; 32],
    /// Certified host root key.
    pub host_root: [u8; 32],
    /// Certified consumer device key.
    pub consumer_device: [u8; 32],
    /// Certified consumer root key.
    pub consumer_root: [u8; 32],
    /// Stable per-device/workspace consumer identity.
    pub consumer: ConsumerId,
    /// Durable store incarnation.
    pub store_generation: StoreGeneration,
    /// Current native visibility-policy projection.
    pub authorization_view: Digest,
    /// Complete committed replay boundary.
    pub committed: CommitRef,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    version: u8,
    protocol: u16,
    issue: CursorIssue,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BorrowedIssue<'a> {
    #[serde(borrow, deserialize_with = "bounded_room")]
    room: &'a str,
    workspace: u64,
    workspace_root: [u8; 32],
    host_device: [u8; 32],
    host_root: [u8; 32],
    consumer_device: [u8; 32],
    consumer_root: [u8; 32],
    consumer: ConsumerId,
    store_generation: StoreGeneration,
    authorization_view: Digest,
    committed: CommitRef,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BorrowedClaims<'a> {
    version: u8,
    protocol: u16,
    #[serde(borrow)]
    issue: BorrowedIssue<'a>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
/// Opaque exact cursor bytes; construction does not authenticate them.
pub struct CursorToken(Vec<u8>);
impl CursorToken {
    /// Retain nonempty bounded bytes without parsing their claims.
    ///
    /// # Errors
    ///
    /// Returns `CursorError` if the input is not a canonical encoding within this type's bounds.
    pub fn from_bytes(b: &[u8]) -> Result<Self, CursorError> {
        if b.is_empty() || b.len() > MAX_CURSOR_BYTES {
            Err(CursorError::Codec)
        } else {
            Ok(Self(b.to_vec()))
        }
    }
    /// Return the exact bytes received or issued.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// Issue a cursor with the existing certified host signing key.
    ///
    /// # Errors
    ///
    /// Returns `CursorError` if the inputs violate this operation's bounds or invariants.
    pub fn issue(issue: CursorIssue, key: &SigningKey) -> Result<Self, CursorError> {
        validate(&issue)?;
        let claims = Claims {
            version: 1,
            protocol: VERSION,
            issue,
        };
        let c = postcard::to_stdvec(&claims).map_err(|_| CursorError::Codec)?;
        let signature = key.sign(&cursor_digest(&c)).to_bytes();
        let mut b = c;
        b.extend_from_slice(&signature);
        Self::from_bytes(&b)
    }
    /// Authenticate canonical bytes; current native authorization remains required.
    ///
    /// # Errors
    ///
    /// Returns `CursorError` if the value fails validation or verification.
    pub fn authenticate(&self, key: &VerifyingKey) -> Result<SignedCursorClaims, CursorError> {
        let split = self.0.len().checked_sub(64).ok_or(CursorError::Codec)?;
        let (claim_bytes, signature_bytes) = self.0.split_at(split);
        let claims: BorrowedClaims<'_> =
            postcard::from_bytes(claim_bytes).map_err(|_| CursorError::Codec)?;
        if postcard::to_stdvec(&claims).map_err(|_| CursorError::Codec)? != claim_bytes
            || claims.version != 1
            || claims.protocol != VERSION
        {
            return Err(CursorError::Codec);
        }
        validate_borrowed(&claims.issue)?;
        let signature: [u8; 64] = signature_bytes.try_into().map_err(|_| CursorError::Codec)?;
        key.verify(
            &cursor_digest(claim_bytes),
            &ed25519_dalek::Signature::from_bytes(&signature),
        )
        .map_err(|_| CursorError::Signature)?;
        #[cfg(test)]
        ROOM_OWNED_COPIES.with(|count| count.set(count.get().saturating_add(1)));
        Ok(SignedCursorClaims(claims.issue.into_owned()))
    }
}
fn validate_borrowed(v: &BorrowedIssue<'_>) -> Result<(), CursorError> {
    validate_fields(
        v.room,
        v.workspace,
        v.consumer,
        v.store_generation,
        v.committed.sequence,
        [
            v.workspace_root,
            v.host_device,
            v.host_root,
            v.consumer_device,
            v.consumer_root,
        ],
    )
}
fn validate(v: &CursorIssue) -> Result<(), CursorError> {
    validate_fields(
        &v.room,
        v.workspace,
        v.consumer,
        v.store_generation,
        v.committed.sequence,
        [
            v.workspace_root,
            v.host_device,
            v.host_root,
            v.consumer_device,
            v.consumer_root,
        ],
    )
}
fn validate_fields(
    room: &str,
    workspace: u64,
    consumer: ConsumerId,
    store_generation: StoreGeneration,
    sequence: u64,
    keys: [[u8; 32]; 5],
) -> Result<(), CursorError> {
    let canonical_room =
        uuid::Uuid::parse_str(room).is_ok_and(|value| value.hyphenated().to_string() == room);
    if !canonical_room
        || room.len() > 128
        || workspace == 0
        || consumer == [0; 16]
        || store_generation == [0; 16]
        || sequence == 0
        || keys.contains(&[0; 32])
    {
        Err(CursorError::Claims)
    } else {
        Ok(())
    }
}
impl BorrowedIssue<'_> {
    fn into_owned(self) -> CursorIssue {
        CursorIssue {
            room: self.room.to_owned(),
            workspace: self.workspace,
            workspace_root: self.workspace_root,
            host_device: self.host_device,
            host_root: self.host_root,
            consumer_device: self.consumer_device,
            consumer_root: self.consumer_root,
            consumer: self.consumer,
            store_generation: self.store_generation,
            authorization_view: self.authorization_view,
            committed: self.committed,
        }
    }
}
fn cursor_digest(claims: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(DOMAIN);
    hasher.update(claims);
    *hasher.finalize().as_bytes()
}
fn bounded_room<'de, D: Deserializer<'de>>(d: D) -> Result<&'de str, D::Error> {
    struct V;
    impl<'de> de::Visitor<'de> for V {
        type Value = &'de str;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a room string of at most 128 bytes")
        }
        fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
            if v.len() > 128 {
                Err(E::custom("room too long"))
            } else {
                Ok(v)
            }
        }
    }
    d.deserialize_str(V)
}
/// Signature-authenticated claims only; callers must check current native authority.
pub struct SignedCursorClaims(CursorIssue);
impl SignedCursorClaims {
    /// Return all signature-authenticated inert claims.
    pub fn issue(&self) -> &CursorIssue {
        &self.0
    }
    /// Return the stable consumer claim.
    pub fn consumer(&self) -> ConsumerId {
        self.0.consumer
    }
    /// Return the committed-position claim.
    pub fn committed(&self) -> &CommitRef {
        &self.0.committed
    }
}
#[derive(Debug, thiserror::Error)]
/// Closed cursor codec/authentication failures.
pub enum CursorError {
    /// Empty, oversized, malformed, noncanonical, or unsupported encoding.
    #[error("invalid cursor encoding")]
    Codec,
    /// Ed25519 signature verification failed.
    #[error("invalid cursor signature")]
    Signature,
    /// A structurally invalid claim was presented.
    #[error("invalid cursor claims")]
    Claims,
}
#[cfg(test)]
thread_local! { static ROOM_OWNED_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
fn reset_room_owned_copies() {
    ROOM_OWNED_COPIES.with(|count| count.set(0));
}
#[cfg(test)]
fn room_owned_copies() -> usize {
    ROOM_OWNED_COPIES.with(std::cell::Cell::get)
}
#[cfg(test)]
fn canonical_claims_for_test(issue: CursorIssue) -> Result<Vec<u8>, CursorError> {
    validate(&issue)?;
    postcard::to_stdvec(&Claims {
        version: 1,
        protocol: VERSION,
        issue,
    })
    .map_err(|_| CursorError::Codec)
}
#[cfg(test)]
fn cursor_digest_for_test(claims: &[u8]) -> [u8; 32] {
    cursor_digest(claims)
}
#[cfg(test)]
mod tests;
