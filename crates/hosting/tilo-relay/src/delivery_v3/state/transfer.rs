//! Bounded inert staging and pinned-transfer wire values. No value grants authority.
use super::{DeliveryId, Digest, StateError};
use serde::{Deserialize, Serialize};
/// Largest canonical envelope admitted by this transport profile.
pub const MAX_CANONICAL_ITEM_BYTES: u64 = 64 * 1024 * 1024;
/// Fixed stream chunk size before the bounded wire wrapper.
pub const SNAPSHOT_CHUNK_BYTES: usize = 240 * 1024;
/// Maximum retained publication count in a transfer.
pub const MAX_TRANSFER_ITEMS: u32 = 65536;
/// Maximum chunks for256MiB of canonical bytes plus fixed record headers.
pub const MAX_SNAPSHOT_CHUNKS: u32 = 1105;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Durable staging-prefix claim, never a native receipt.
pub struct FlowCredit {
    version: u8,
    delivery: DeliveryId,
    digest: Digest,
    total: u64,
    next: u64,
}
impl FlowCredit {
    /// Construct after checking the bounded inert fields.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the inputs violate this operation's bounds or invariants.
    pub fn new(
        delivery: DeliveryId,
        digest: Digest,
        total: u64,
        next: u64,
    ) -> Result<Self, StateError> {
        let value = Self {
            version: 1,
            delivery,
            digest,
            total,
            next,
        };
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<(), StateError> {
        if self.version != 1
            || self.delivery == [0; 16]
            || self.digest == [0; 32]
            || self.total == 0
            || self.total > MAX_CANONICAL_ITEM_BYTES
            || self.next > self.total
        {
            return Err(StateError::Invalid);
        }
        Ok(())
    }
    /// Encode the exact canonical wire representation.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, StateError> {
        self.validate()?;
        super::encode(self)
    }
    /// Decode exact bounded canonical bytes; no authority is granted.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, StateError> {
        let value: Self = super::decode(bytes)?;
        value.validate()?;
        Ok(value)
    }
    /// Return the stable upload identity.
    pub fn delivery(&self) -> DeliveryId {
        self.delivery
    }
    /// Return the complete canonical input digest.
    pub fn input_digest(&self) -> Digest {
        self.digest
    }
    /// Return the complete canonical item size.
    pub fn canonical_size(&self) -> u64 {
        self.total
    }
    /// Return the durable staging prefix, never native acceptance.
    pub fn next_offset(&self) -> u64 {
        self.next
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Borrowed transport bytes, requiring manifest/current-authority validation.
pub struct SnapshotChunk<'a> {
    version: u8,
    pin: [u8; 16],
    manifest: Digest,
    index: u32,
    #[serde(borrow, with = "serde_bytes")]
    bytes: &'a [u8],
}
impl<'a> SnapshotChunk<'a> {
    /// Construct after checking the bounded inert fields.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the inputs violate this operation's bounds or invariants.
    pub fn new(
        pin: [u8; 16],
        manifest: Digest,
        index: u32,
        bytes: &'a [u8],
    ) -> Result<Self, StateError> {
        let value = Self {
            version: 1,
            pin,
            manifest,
            index,
            bytes,
        };
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<(), StateError> {
        if self.version != 1
            || self.pin == [0; 16]
            || self.manifest == [0; 32]
            || self.index >= MAX_SNAPSHOT_CHUNKS
            || self.bytes.is_empty()
        {
            return Err(StateError::Invalid);
        }
        if self.bytes.len() > SNAPSHOT_CHUNK_BYTES {
            return Err(StateError::Oversized);
        }
        Ok(())
    }
    /// Encode the exact canonical wire representation.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, StateError> {
        self.validate()?;
        super::encode(self)
    }
    /// Decode exact bounded canonical bytes; no authority is granted.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, StateError> {
        if bytes.len() > super::MAX_FRAME_PAYLOAD_BYTES {
            return Err(StateError::Oversized);
        }
        let (value, trailing): (Self, _) =
            postcard::take_from_bytes(bytes).map_err(|_| StateError::Codec)?;
        if !trailing.is_empty() {
            return Err(StateError::Codec);
        }
        value.validate()?;
        postcard::serialize_with_flavor(&value, super::ExactPayload(bytes))
            .map_err(|_| StateError::Codec)?;
        Ok(value)
    }
    /// Return the opaque transfer identifier.
    pub fn pin(&self) -> [u8; 16] {
        self.pin
    }
    /// Return the exact signed manifest digest.
    pub fn manifest_digest(&self) -> Digest {
        self.manifest
    }
    /// Return the private transport chunk index.
    pub fn index(&self) -> u32 {
        self.index
    }
    /// Borrow the original chunk bytes without copying.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Inert family tag; actual native admission still validates canonical bytes.
pub enum TransferKind {
    /// Canonical governance `SyncResp` bytes.
    Governance,
    /// Canonical application document bytes.
    State,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Fixed canonical stream record header.
pub struct TransferRecordHeader {
    ordinal: u32,
    kind: TransferKind,
    total: u64,
    digest: Digest,
}
impl TransferRecordHeader {
    /// Construct after checking the bounded inert fields.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the inputs violate this operation's bounds or invariants.
    pub fn new(
        ordinal: u32,
        kind: TransferKind,
        total: u64,
        digest: Digest,
    ) -> Result<Self, StateError> {
        if total == 0 || total > MAX_CANONICAL_ITEM_BYTES {
            return Err(StateError::Invalid);
        }
        Ok(Self {
            ordinal,
            kind,
            total,
            digest,
        })
    }
    /// Encode the exact canonical wire representation.
    pub fn encode(&self) -> [u8; 45] {
        let mut bytes = [0; 45];
        bytes[..4].copy_from_slice(&self.ordinal.to_le_bytes());
        bytes[4] = match self.kind {
            TransferKind::Governance => 0,
            TransferKind::State => 1,
        };
        bytes[5..13].copy_from_slice(&self.total.to_le_bytes());
        bytes[13..].copy_from_slice(&self.digest);
        bytes
    }
    /// Decode exact bounded canonical bytes; no authority is granted.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, StateError> {
        let bytes: &[u8; 45] = bytes.try_into().map_err(|_| StateError::Codec)?;
        let kind = match bytes[4] {
            0 => TransferKind::Governance,
            1 => TransferKind::State,
            _ => return Err(StateError::Invalid),
        };
        Self::new(
            u32::from_le_bytes(bytes[..4].try_into().map_err(|_| StateError::Codec)?),
            kind,
            u64::from_le_bytes(bytes[5..13].try_into().map_err(|_| StateError::Codec)?),
            bytes[13..].try_into().map_err(|_| StateError::Codec)?,
        )
    }
    /// Return the original visible publication ordinal.
    pub fn ordinal(&self) -> u32 {
        self.ordinal
    }
    /// Return the inert canonical family tag.
    pub fn kind(&self) -> TransferKind {
        self.kind
    }
    /// Return the complete canonical item size.
    pub fn canonical_size(&self) -> u64 {
        self.total
    }
    /// Return the original complete canonical item digest.
    pub fn canonical_digest(&self) -> Digest {
        self.digest
    }
}
/// Streaming commitment to exactly one known visible item count.
pub struct VisibleOrder {
    hasher: blake3::Hasher,
    expected: u32,
    seen: u32,
    last: Option<u32>,
    failed: bool,
}
impl VisibleOrder {
    /// Construct after checking the bounded inert fields.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the inputs violate this operation's bounds or invariants.
    pub fn new(expected: u32) -> Result<Self, StateError> {
        if expected > MAX_TRANSFER_ITEMS {
            return Err(StateError::Oversized);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo-delivery-v3-visible-order-v1\0");
        let mut buffer = [0; 10];
        let bytes =
            postcard::to_slice(&u64::from(expected), &mut buffer).map_err(|_| StateError::Codec)?;
        hasher.update(bytes);
        Ok(Self {
            hasher,
            expected,
            seen: 0,
            last: None,
            failed: false,
        })
    }
    /// Append one strictly increasing original ordinal and digest.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the resource budget refuses the allocation or the input is invalid.
    pub fn push(&mut self, ordinal: u32, digest: Digest) -> Result<(), StateError> {
        if self.failed
            || self.seen >= self.expected
            || self.last.is_some_and(|last| ordinal <= last)
        {
            self.failed = true;
            return Err(StateError::Invalid);
        }
        let mut buffer = [0; 5];
        let bytes = postcard::to_slice(&ordinal, &mut buffer).map_err(|_| StateError::Codec)?;
        self.hasher.update(bytes);
        self.hasher.update(&digest);
        self.last = Some(ordinal);
        self.seen = self.seen.saturating_add(1);
        Ok(())
    }
    /// Finish only after exactly the declared item count.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the inputs violate this operation's bounds or invariants.
    pub fn finish(self) -> Result<Digest, StateError> {
        if self.failed || self.seen != self.expected {
            return Err(StateError::Invalid);
        }
        Ok(*self.hasher.finalize().as_bytes())
    }
}
#[cfg(test)]
mod tests;

use crate::delivery_v3::{
    commit::CommitRef,
    cursor::{CursorIssue, CursorToken},
};
/// Closed snapshot request direction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotOperation {
    /// Complete pinned baseline and retained tail.
    Baseline,
    /// Request the next exact commit after this durable cursor.
    ReplayAfter {
        /// Original opaque durable ACK cursor; current owner verifies it.
        #[serde(with = "super::token_bytes")]
        cursor: CursorToken,
    },
    /// Renew the same pin after server-observed progress.
    Renew {
        /// Exact existing transfer identifier.
        pin: [u8; 16],
        /// Exact prior signed manifest digest.
        manifest_digest: Digest,
    },
}
/// Bounded inert request; fresh proof and owner authorization remain mandatory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRequest {
    version: u8,
    operation: SnapshotOperation,
}
impl SnapshotRequest {
    /// Construct after checking the bounded inert fields.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the inputs violate this operation's bounds or invariants.
    pub fn new(operation: SnapshotOperation) -> Result<Self, StateError> {
        let value = Self {
            version: 1,
            operation,
        };
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<(), StateError> {
        if self.version != 1 {
            return Err(StateError::Invalid);
        }
        if let SnapshotOperation::Renew {
            pin,
            manifest_digest,
        } = &self.operation
            && (*pin == [0; 16] || *manifest_digest == [0; 32])
        {
            return Err(StateError::Invalid);
        }
        Ok(())
    }
    /// Decode exact bounded canonical bytes; no authority is granted.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, StateError> {
        if bytes.len() > 8192 {
            return Err(StateError::Oversized);
        }
        let value: Self = super::decode(bytes)?;
        value.validate()?;
        Ok(value)
    }
    /// Encode the exact canonical wire representation.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, StateError> {
        self.validate()?;
        let bytes = super::encode(self)?;
        if bytes.len() > 8192 {
            return Err(StateError::Oversized);
        }
        Ok(bytes)
    }
    /// Borrow the closed request operation.
    pub fn operation(&self) -> &SnapshotOperation {
        &self.operation
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Transfer coverage mode, not an admission decision.
pub enum SnapshotKind {
    /// Complete pinned baseline and retained tail.
    Baseline,
    /// One exact stored replay publication group.
    Replay,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Inert manifest claims supplied only by the real pin owner in production.
pub struct ManifestIssue {
    #[serde(deserialize_with = "bounded_scope")]
    /// Exact consumer, host, workspace, store, policy and watermark claims.
    pub scope: CursorIssue,
    /// Nonzero opaque transfer identity.
    pub pin: [u8; 16],
    /// Baseline or one exact replay group.
    pub kind: SnapshotKind,
    /// Original verified cut for replay; absent for baseline.
    pub predecessor: Option<CommitRef>,
    /// Baseline: original retained roots generation. Replay: the selected exact
    /// replay entry owner generation, even if the current baseline is newer.
    pub baseline_generation: u64,
    /// Current signed pin deadline, at most five minutes ahead.
    pub expires_at_ns: u64,
    /// Exact count of canonical stream records.
    pub item_count: u32,
    /// Exact stream bytes including fixed record headers.
    pub stream_bytes: u64,
    /// `Fixed240KiB` transport chunk width.
    pub chunk_size: u32,
    #[serde(deserialize_with = "bounded_digests")]
    /// Ordered complete chunk digests, bounded to1105.
    pub chunk_digests: Vec<Digest>,
}
#[derive(Serialize, Deserialize)]
struct ManifestClaims {
    version: u8,
    protocol: u16,
    issue: ManifestIssue,
}
const MANIFEST_DOMAIN: &[u8] = b"tilo-delivery-v3-snapshot-manifest-v1\0";
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const PIN_LIFETIME_NS: u64 = 300_000_000_000;
fn manifest_digest(claims: &[u8]) -> Digest {
    let mut h = blake3::Hasher::new();
    h.update(MANIFEST_DOMAIN);
    h.update(claims);
    *h.finalize().as_bytes()
}
impl ManifestIssue {
    fn validate(&self, key: &ed25519_dalek::VerifyingKey, now_ns: u64) -> Result<(), StateError> {
        self.validate_structure(key)?;
        if self.expires_at_ns <= now_ns
            || self
                .expires_at_ns
                .checked_sub(now_ns)
                .is_none_or(|n| n > PIN_LIFETIME_NS)
        {
            return Err(StateError::Invalid);
        }
        Ok(())
    }
    fn validate_structure(&self, key: &ed25519_dalek::VerifyingKey) -> Result<(), StateError> {
        let scope = &self.scope;
        let canonical = uuid::Uuid::parse_str(&scope.room)
            .is_ok_and(|v| v.hyphenated().to_string() == scope.room);
        if !canonical
            || scope.workspace == 0
            || [
                scope.workspace_root,
                scope.host_device,
                scope.host_root,
                scope.consumer_device,
                scope.consumer_root,
            ]
            .contains(&[0; 32])
            || scope.consumer == [0; 16]
            || scope.store_generation == [0; 16]
            || scope.committed.sequence == 0
            || scope.host_device != key.to_bytes()
            || self.pin == [0; 16]
            || self.expires_at_ns == 0
        {
            return Err(StateError::Invalid);
        }
        match (self.kind, &self.predecessor) {
            (SnapshotKind::Baseline, None) => {}
            (SnapshotKind::Replay, Some(previous))
                if previous.sequence > 0 && previous.sequence < scope.committed.sequence => {}
            _ => return Err(StateError::Invalid),
        }
        if self.item_count == 0
            || self.item_count > MAX_TRANSFER_ITEMS
            || self.chunk_size as usize != SNAPSHOT_CHUNK_BYTES
            || self.chunk_digests.len() > MAX_SNAPSHOT_CHUNKS as usize
        {
            return Err(StateError::Oversized);
        }
        let headers = u64::from(self.item_count) * 45;
        let canonical_bytes = self
            .stream_bytes
            .checked_sub(headers)
            .ok_or(StateError::Invalid)?;
        if canonical_bytes < u64::from(self.item_count)
            || canonical_bytes
                > (256 * 1024 * 1024).min(u64::from(self.item_count) * MAX_CANONICAL_ITEM_BYTES)
            || self.stream_bytes.div_ceil(SNAPSHOT_CHUNK_BYTES as u64)
                != self.chunk_digests.len() as u64
        {
            return Err(StateError::Invalid);
        }
        Ok(())
    }
}
/// Bounded signed bytes, not current native authority.
pub struct SnapshotManifest(Vec<u8>);
/// Signature-authenticated inert claims; native scope must be checked afresh.
pub struct SignedSnapshotManifest {
    issue: ManifestIssue,
    digest: Digest,
}
impl SignedSnapshotManifest {
    /// Check only exact chunk integrity; callers still require current native authorization.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value fails validation or verification.
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "the chunk names its signed manifest by digest, which the holder keeps beside the issue"
    )]
    pub fn validate_chunk(&self, chunk: &SnapshotChunk<'_>) -> Result<(), StateError> {
        let issue = &self.issue;
        let offset = u64::from(chunk.index).saturating_mul(u64::from(issue.chunk_size));
        let expected = issue
            .stream_bytes
            .checked_sub(offset)
            .ok_or(StateError::Invalid)?
            .min(u64::from(issue.chunk_size));
        // The chunk names the signed manifest by digest, which this holder keeps.
        if chunk.pin != issue.pin
            || chunk.manifest != self.digest
            || chunk.bytes.len() as u64 != expected
            || issue.chunk_digests.get(chunk.index as usize)
                != Some(blake3::hash(chunk.bytes).as_bytes())
        {
            return Err(StateError::Invalid);
        }
        Ok(())
    }
    /// Compare every immutable claim before retaining chunks across an expiry renewal.
    pub fn same_content(&self, other: &Self) -> bool {
        let a = &self.issue;
        let b = &other.issue;
        a.scope == b.scope
            && a.pin == b.pin
            && a.kind == b.kind
            && a.predecessor == b.predecessor
            && a.baseline_generation == b.baseline_generation
            && a.item_count == b.item_count
            && a.stream_bytes == b.stream_bytes
            && a.chunk_size == b.chunk_size
            && a.chunk_digests == b.chunk_digests
    }
    /// Borrow signature-authenticated claims; this is not a current-authority witness.
    pub fn issue(&self) -> &ManifestIssue {
        &self.issue
    }
}
impl SnapshotManifest {
    /// Sign validated inert manifest claims using the certified host key.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the inputs violate this operation's bounds or invariants.
    pub fn issue(
        issue: ManifestIssue,
        key: &ed25519_dalek::SigningKey,
        now_ns: u64,
    ) -> Result<Self, StateError> {
        use ed25519_dalek::Signer;
        issue.validate(&key.verifying_key(), now_ns)?;
        let claims = ManifestClaims {
            version: 1,
            protocol: 3,
            issue,
        };
        let length =
            postcard::experimental::serialized_size(&claims).map_err(|_| StateError::Codec)?;
        if length > MAX_MANIFEST_BYTES - 64 {
            return Err(StateError::Oversized);
        }
        let mut bytes = vec![0; length.saturating_add(64)];
        let (claim_bytes, signature_bytes) = bytes.split_at_mut(length);
        postcard::to_slice(&claims, claim_bytes).map_err(|_| StateError::Codec)?;
        let signature = key.sign(&manifest_digest(claim_bytes)).to_bytes();
        signature_bytes.copy_from_slice(&signature);
        Ok(Self(bytes))
    }
    /// Retain bounded signed bytes without authenticating or authorizing them.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StateError> {
        if bytes.len() <= 64 || bytes.len() > MAX_MANIFEST_BYTES {
            return Err(StateError::Oversized);
        }
        Ok(Self(bytes.to_vec()))
    }
    /// Verify exact canonical claims, host signature and expiry; current native authority remains required.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value fails validation or verification.
    pub fn authenticate(
        &self,
        key: &ed25519_dalek::VerifyingKey,
        now_ns: u64,
    ) -> Result<SignedSnapshotManifest, StateError> {
        let signed = self.authenticate_retained(key)?;
        signed.issue.validate(key, now_ns)?;
        Ok(signed)
    }
    /// Authenticate retained bytes without asserting current time. Returned signed
    /// claims are timeless inert evidence, never current native authority. A native
    /// owner must separately match its actual live pin and freshly authorize use.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value fails validation or verification.
    pub fn authenticate_retained(
        &self,
        key: &ed25519_dalek::VerifyingKey,
    ) -> Result<SignedSnapshotManifest, StateError> {
        let (bytes, signature) = self.0.split_at(self.0.len().saturating_sub(64));
        let signature = ed25519_dalek::Signature::from_bytes(
            signature.try_into().map_err(|_| StateError::Codec)?,
        );
        key.verify_strict(&manifest_digest(bytes), &signature)
            .map_err(|_| StateError::Invalid)?;
        let (claims, trailing): (ManifestClaims, _) =
            postcard::take_from_bytes(bytes).map_err(|_| StateError::Codec)?;
        if !trailing.is_empty() || claims.version != 1 || claims.protocol != 3 {
            return Err(StateError::Codec);
        }
        postcard::serialize_with_flavor(&claims, super::ExactPayload(bytes))
            .map_err(|_| StateError::Codec)?;
        claims.issue.validate_structure(key)?;
        Ok(SignedSnapshotManifest {
            issue: claims.issue,
            digest: self.digest(),
        })
    }
    /// Borrow the exact signed bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// Digest the exact signed manifest bytes.
    pub fn digest(&self) -> Digest {
        *blake3::hash(&self.0).as_bytes()
    }
}
fn bounded_digests<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Digest>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Vec<Digest>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most1105 chunk digests")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let cap = seq.size_hint().unwrap_or(MAX_SNAPSHOT_CHUNKS as usize);
            if cap > MAX_SNAPSHOT_CHUNKS as usize {
                return Err(serde::de::Error::custom("too many chunk digests"));
            }
            let mut values = Vec::with_capacity(cap);
            while let Some(value) = seq.next_element()? {
                if values.len() == cap {
                    return Err(serde::de::Error::custom("digest count mismatch"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    d.deserialize_seq(Visitor)
}
fn bounded_scope<'de, D: serde::Deserializer<'de>>(d: D) -> Result<CursorIssue, D::Error> {
    #[derive(Deserialize)]
    struct Scope {
        #[serde(deserialize_with = "bounded_room")]
        room: String,
        workspace: u64,
        workspace_root: Digest,
        host_device: Digest,
        host_root: Digest,
        consumer_device: Digest,
        consumer_root: Digest,
        consumer: [u8; 16],
        store_generation: [u8; 16],
        authorization_view: Digest,
        committed: CommitRef,
    }
    let s = Scope::deserialize(d)?;
    Ok(CursorIssue {
        room: s.room,
        workspace: s.workspace,
        workspace_root: s.workspace_root,
        host_device: s.host_device,
        host_root: s.host_root,
        consumer_device: s.consumer_device,
        consumer_root: s.consumer_root,
        consumer: s.consumer,
        store_generation: s.store_generation,
        authorization_view: s.authorization_view,
        committed: s.committed,
    })
}
fn bounded_room<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("canonical36-byte room UUID")
        }
        fn visit_borrowed_str<E: serde::de::Error>(self, value: &'de str) -> Result<String, E> {
            if value.len() != 36 {
                return Err(E::custom("invalid room length"));
            }
            Ok(value.to_owned())
        }
    }
    d.deserialize_str(Visitor)
}
