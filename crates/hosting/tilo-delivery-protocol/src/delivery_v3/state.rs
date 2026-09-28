//! Canonical bounded inert delivery-v3 state and replay payloads.
use crate::delivery_v3::{
    commit::{CommitRef, ConsumerId, DeliveryId, Digest, StoreGeneration},
    cursor::{CursorToken, MAX_CURSOR_BYTES},
    framing::MAX_PAYLOAD_BYTES,
};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeSet;
/// Existing encoded frame-payload ceiling, including metadata.
pub const MAX_FRAME_PAYLOAD_BYTES: usize = MAX_PAYLOAD_BYTES;
/// Maximum operation identities in one admission result.
pub const MAX_ADMITTED_OPERATIONS: usize = 4096;
const VERSION: u8 = 1;
mod transfer;
pub use transfer::{
    FlowCredit, MAX_CANONICAL_ITEM_BYTES, MAX_SNAPSHOT_CHUNKS, MAX_TRANSFER_ITEMS, ManifestIssue,
    SNAPSHOT_CHUNK_BYTES, SignedSnapshotManifest, SnapshotChunk, SnapshotKind, SnapshotManifest,
    SnapshotOperation, SnapshotRequest, TransferKind, TransferRecordHeader, VisibleOrder,
};

/// Canonical lowercase-hex cursor envelope ceiling.
pub const MAX_CURSOR_URL_BYTES: usize = 2 * MAX_CURSOR_BYTES;
/// Borrowed inert upload byte range; admission remains the owner's decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateItemChunk<'a> {
    version: u8,
    delivery: DeliveryId,
    input_digest: Digest,
    canonical_size: u64,
    offset: u64,
    #[serde(borrow, with = "serde_bytes")]
    bytes: &'a [u8],
}
impl<'a> StateItemChunk<'a> {
    /// Construct a bounded upload range.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the inputs violate this operation's bounds or invariants.
    pub fn new(
        delivery: DeliveryId,
        input_digest: Digest,
        canonical_size: u64,
        offset: u64,
        bytes: &'a [u8],
    ) -> Result<Self, StateError> {
        let value = Self {
            version: VERSION,
            delivery,
            input_digest,
            canonical_size,
            offset,
            bytes,
        };
        value.encoded_len()?;
        Ok(value)
    }
    /// Decode a canonical upload range borrowing the original payload.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, StateError> {
        if bytes.len() > MAX_FRAME_PAYLOAD_BYTES {
            return Err(StateError::Oversized);
        }
        let (value, trailing): (Self, _) =
            postcard::take_from_bytes(bytes).map_err(|_| StateError::Codec)?;
        if !trailing.is_empty() {
            return Err(StateError::Codec);
        }
        value.encoded_len()?;
        postcard::serialize_with_flavor(&value, ExactPayload(bytes))
            .map_err(|_| StateError::Codec)?;
        Ok(value)
    }
    /// Return the checked payload size without allocating.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
    pub fn encoded_len(&self) -> Result<usize, StateError> {
        if self.bytes.len() > MAX_FRAME_PAYLOAD_BYTES {
            return Err(StateError::Oversized);
        }
        if self.version != VERSION
            || self.delivery == [0; 16]
            || self.bytes.is_empty()
            || self
                .offset
                .checked_add(self.bytes.len() as u64)
                .is_none_or(|end| end > self.canonical_size)
        {
            return Err(StateError::Invalid);
        }
        let length =
            postcard::experimental::serialized_size(self).map_err(|_| StateError::Codec)?;
        if length > MAX_FRAME_PAYLOAD_BYTES {
            return Err(StateError::Oversized);
        }
        Ok(length)
    }
    /// Encode a validated upload payload.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, StateError> {
        self.encoded_len()?;
        postcard::to_stdvec(self).map_err(|_| StateError::Codec)
    }
    /// Return the stable delivery identity.
    pub fn delivery(&self) -> DeliveryId {
        self.delivery
    }
    /// Return the digest of the complete canonical item.
    pub fn input_digest(&self) -> Digest {
        self.input_digest
    }
    /// Return the complete item size.
    pub fn canonical_size(&self) -> u64 {
        self.canonical_size
    }
    /// Return this range's offset.
    pub fn offset(&self) -> u64 {
        self.offset
    }
    /// Return the original borrowed range bytes.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}
/// Encode exact opaque cursor bytes for the canonical URL envelope.
///
/// # Errors
///
/// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
pub fn encode_cursor_url(raw: &[u8]) -> Result<String, StateError> {
    if raw.len() > MAX_CURSOR_BYTES {
        return Err(StateError::Oversized);
    }
    if raw.is_empty() {
        return Err(StateError::Invalid);
    }
    Ok(hex::encode(raw))
}
/// Decode a canonical URL envelope without authenticating its opaque cursor.
///
/// # Errors
///
/// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
pub fn decode_cursor_url(text: &str) -> Result<CursorToken, StateError> {
    if text.len() > MAX_CURSOR_URL_BYTES {
        return Err(StateError::Oversized);
    }
    if text.is_empty()
        || !text.len().is_multiple_of(2)
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StateError::Invalid);
    }
    // Reject malformed or oversized envelopes before creating owned cursor bytes.
    let mut raw = [0; MAX_CURSOR_BYTES];
    let raw = raw.get_mut(..text.len() / 2).ok_or(StateError::Oversized)?;
    hex::decode_to_slice(text, raw).map_err(|_| StateError::Invalid)?;
    #[cfg(test)]
    TOKEN_OWNED_COPIES.with(|count| count.set(count.get().saturating_add(1)));
    CursorToken::from_bytes(raw).map_err(|_| StateError::Invalid)
}

// Compare canonical serialization directly against the borrowed input. Upload
// validation never creates an attacker-sized temporary serialization buffer.
struct ExactPayload<'a>(&'a [u8]);
impl postcard::ser_flavors::Flavor for ExactPayload<'_> {
    type Output = ();
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.try_extend(&[byte])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        let Some(rest) = self.0.strip_prefix(bytes) else {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Durable barrier represented by an admission result.
pub enum CommitScope {
    /// Relay storage/deduplication only; not native semantic admission.
    RelayStored,
    /// Native admission and replay discovery share the owner commit barrier.
    NativeCommitted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Closed permanent rejection codes.
pub enum AdmissionCode {
    /// Current authority denied this input.
    PermissionDenied,
    /// The stable delivery identity was already bound to other bytes.
    ConflictingId,
    /// The canonical input or framing was invalid.
    InvalidFrame,
    /// The owner cannot retain this input within quota.
    QuotaExceeded,
    /// This input family is not implemented by the endpoint.
    Unsupported,
    /// Native semantic admission rejected the input.
    Rejected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Closed retryable rejection codes.
pub enum RetryCode {
    /// Capacity may become available after progress or operator action.
    QuotaExceeded,
    /// The owner is updating or temporarily unavailable.
    TemporarilyUnavailable,
    /// Native admission retained or awaits an unmet prerequisite.
    PrerequisiteMissing,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Structurally separated durable and non-durable outcomes.
pub enum AdmissionOutcome {
    /// This exact item was durably accepted at the stated scope.
    Accepted {
        /// Barrier that established this result.
        scope: CommitScope,
        /// Native operation identities or relay input-digest associations.
        #[serde(deserialize_with = "operations")]
        operations: Vec<Digest>,
        /// Exact durable position.
        committed: CommitRef,
    },
    /// This exact item matches an earlier durable result.
    Duplicate {
        /// Barrier that established the original result.
        scope: CommitScope,
        /// Native operation identities or relay input-digest associations.
        #[serde(deserialize_with = "operations")]
        operations: Vec<Digest>,
        /// Exact original durable position.
        committed: CommitRef,
    },
    /// The item was not admitted and retrying unchanged will not help.
    Rejected {
        /// Closed rejection reason.
        code: AdmissionCode,
    },
    /// The item was not admitted but a later retry may succeed.
    RetryLater {
        /// Closed retry reason.
        code: RetryCode,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Per-delivery bounded admission result.
pub struct StateAdmission {
    version: u8,
    delivery: DeliveryId,
    input_digest: Digest,
    outcome: AdmissionOutcome,
}
impl StateAdmission {
    /// Construct a newly accepted durable result.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value fails validation or verification.
    pub fn accepted(
        delivery: DeliveryId,
        input_digest: Digest,
        scope: CommitScope,
        operations: Vec<Digest>,
        committed: CommitRef,
    ) -> Result<Self, StateError> {
        Self::admitted(
            delivery,
            input_digest,
            AdmissionOutcome::Accepted {
                scope,
                operations,
                committed,
            },
        )
    }
    /// Construct an exact previously accepted durable result.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the resource budget refuses the allocation or the input is invalid.
    pub fn duplicate(
        delivery: DeliveryId,
        input_digest: Digest,
        scope: CommitScope,
        operations: Vec<Digest>,
        committed: CommitRef,
    ) -> Result<Self, StateError> {
        Self::admitted(
            delivery,
            input_digest,
            AdmissionOutcome::Duplicate {
                scope,
                operations,
                committed,
            },
        )
    }
    fn admitted(
        delivery: DeliveryId,
        input_digest: Digest,
        outcome: AdmissionOutcome,
    ) -> Result<Self, StateError> {
        let v = Self {
            version: VERSION,
            delivery,
            input_digest,
            outcome,
        };
        v.validate()?;
        Ok(v)
    }
    /// Construct a permanent non-admission result.
    pub fn rejected(delivery: DeliveryId, input_digest: Digest, code: AdmissionCode) -> Self {
        Self {
            version: VERSION,
            delivery,
            input_digest,
            outcome: AdmissionOutcome::Rejected { code },
        }
    }
    /// Construct a retryable non-admission result.
    pub fn retry_later(delivery: DeliveryId, input_digest: Digest, code: RetryCode) -> Self {
        Self {
            version: VERSION,
            delivery,
            input_digest,
            outcome: AdmissionOutcome::RetryLater { code },
        }
    }
    /// Inspect the closed result variant.
    pub fn outcome(&self) -> &AdmissionOutcome {
        &self.outcome
    }
    /// Return the upload delivery identity.
    pub fn delivery(&self) -> DeliveryId {
        self.delivery
    }
    /// Return the digest of the exact canonical input.
    pub fn input_digest(&self) -> Digest {
        self.input_digest
    }
    fn validate(&self) -> Result<(), StateError> {
        if self.version != VERSION || self.delivery == [0; 16] {
            return Err(StateError::Invalid);
        }
        if let AdmissionOutcome::Accepted {
            operations,
            committed,
            ..
        }
        | AdmissionOutcome::Duplicate {
            operations,
            committed,
            ..
        } = &self.outcome
            && (operations.is_empty()
                || operations.len() > MAX_ADMITTED_OPERATIONS
                || committed.sequence == 0
                || operations.iter().collect::<BTreeSet<_>>().len() != operations.len())
        {
            return Err(StateError::Invalid);
        }
        Ok(())
    }
    /// Encode a validated canonical payload.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, StateError> {
        self.validate()?;
        encode(self)
    }
    /// Decode an exact bounded canonical payload.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(b: &[u8]) -> Result<Self, StateError> {
        let v: Self = decode(b)?;
        v.validate()?;
        Ok(v)
    }
}
fn operations<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Digest>, D::Error> {
    struct V;
    impl<'de> de::Visitor<'de> for V {
        type Value = Vec<Digest>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded operation ids")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
            let cap = a.size_hint().unwrap_or(0);
            if cap > MAX_ADMITTED_OPERATIONS {
                return Err(de::Error::custom("too many operations"));
            }
            let mut out = Vec::new();
            out.try_reserve(cap.min(MAX_ADMITTED_OPERATIONS))
                .map_err(de::Error::custom)?;
            while let Some(v) = a.next_element()? {
                if out.len() == MAX_ADMITTED_OPERATIONS {
                    return Err(de::Error::custom("too many operations"));
                }
                out.push(v);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(V)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Owner that established an event's storage boundary.
pub enum EventSource {
    /// Relay storage established the referenced event.
    Relay,
    /// Native owner admission established the referenced event.
    Native,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Closed semantic class of original canonical event bytes.
pub enum EventKind {
    /// Canonical application state.
    State,
    /// Canonical room governance.
    Governance,
    /// Canonical native coverage evidence.
    Coverage,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// One bounded byte range of an immutable replay event.
pub struct EventChunk {
    version: u8,
    committed: CommitRef,
    ordinal: u32,
    event_digest: Digest,
    source: EventSource,
    kind: EventKind,
    canonical_size: u64,
    offset: u64,
    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>,
}
impl EventChunk {
    /// Construct a range after validating offset and encoded-size bounds.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the inputs violate this operation's bounds or invariants.
    pub fn new(
        committed: CommitRef,
        ordinal: u32,
        event_digest: Digest,
        source: EventSource,
        kind: EventKind,
        canonical_size: u64,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<Self, StateError> {
        let v = Self {
            version: VERSION,
            committed,
            ordinal,
            event_digest,
            source,
            kind,
            canonical_size,
            offset,
            bytes,
        };
        v.validate()?;
        v.encode()?;
        Ok(v)
    }
    fn validate(&self) -> Result<(), StateError> {
        if self.version != VERSION
            || self.committed.sequence == 0
            || self.bytes.is_empty()
            || self
                .offset
                .checked_add(self.bytes.len() as u64)
                .is_none_or(|v| v > self.canonical_size)
        {
            Err(StateError::Invalid)
        } else {
            Ok(())
        }
    }
    /// Encode a validated canonical event chunk.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, StateError> {
        self.validate()?;
        encode(self)
    }
    /// Decode an exact bounded canonical event chunk.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(b: &[u8]) -> Result<Self, StateError> {
        let v: Self = decode(b)?;
        v.validate()?;
        Ok(v)
    }
    /// Return the complete committed boundary containing this event.
    pub fn committed(&self) -> &CommitRef {
        &self.committed
    }
    /// Return this event's order within the visible boundary.
    pub fn ordinal(&self) -> u32 {
        self.ordinal
    }
    /// Return the digest of the full canonical event.
    pub fn event_digest(&self) -> Digest {
        self.event_digest
    }
    /// Return which owner established storage.
    pub fn source(&self) -> EventSource {
        self.source
    }
    /// Return the event's semantic class.
    pub fn kind(&self) -> EventKind {
        self.kind
    }
    /// Return the full canonical event length.
    pub fn canonical_size(&self) -> u64 {
        self.canonical_size
    }
    /// Return this chunk's byte offset.
    pub fn offset(&self) -> u64 {
        self.offset
    }
    /// Return this chunk's exact canonical bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Complete visible replay cut and its opaque resumable cursor.
pub struct CommitBoundary {
    version: u8,
    consumer: ConsumerId,
    authorization_view: Digest,
    store_generation: StoreGeneration,
    committed: CommitRef,
    visible_count: u32,
    visible_order_digest: Digest,
    #[serde(with = "token_bytes")]
    cursor: CursorToken,
}
impl CommitBoundary {
    /// Construct a complete committed boundary.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the inputs violate this operation's bounds or invariants.
    pub fn new(
        consumer: ConsumerId,
        authorization_view: Digest,
        store_generation: StoreGeneration,
        committed: CommitRef,
        visible_count: u32,
        visible_order_digest: Digest,
        cursor: CursorToken,
    ) -> Result<Self, StateError> {
        let v = Self {
            version: VERSION,
            consumer,
            authorization_view,
            store_generation,
            committed,
            visible_count,
            visible_order_digest,
            cursor,
        };
        v.validate()?;
        Ok(v)
    }
    fn validate(&self) -> Result<(), StateError> {
        if self.version != VERSION
            || self.consumer == [0; 16]
            || self.store_generation == [0; 16]
            || self.committed.sequence == 0
        {
            Err(StateError::Invalid)
        } else {
            Ok(())
        }
    }
    /// Encode a validated canonical boundary.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, StateError> {
        self.validate()?;
        encode(self)
    }
    /// Decode an exact bounded canonical boundary.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(b: &[u8]) -> Result<Self, StateError> {
        let v: Self = decode(b)?;
        v.validate()?;
        Ok(v)
    }
    /// Return the stable consumer identity.
    pub fn consumer(&self) -> ConsumerId {
        self.consumer
    }
    /// Return the native policy-view digest.
    pub fn authorization_view(&self) -> Digest {
        self.authorization_view
    }
    /// Return the durable store incarnation.
    pub fn store_generation(&self) -> StoreGeneration {
        self.store_generation
    }
    /// Return the complete committed replay position.
    pub fn committed(&self) -> &CommitRef {
        &self.committed
    }
    /// Return the number of visible events before this boundary.
    pub fn visible_count(&self) -> u32 {
        self.visible_count
    }
    /// Return the ordered visible-event commitment.
    pub fn visible_order_digest(&self) -> Digest {
        self.visible_order_digest
    }
    /// Return the opaque resumable cursor.
    pub fn cursor(&self) -> &CursorToken {
        &self.cursor
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Exact opaque cursor being durably acknowledged.
pub struct CursorAck {
    version: u8,
    #[serde(with = "token_bytes")]
    cursor: CursorToken,
}
impl CursorAck {
    /// Wrap a bounded opaque cursor.
    pub fn new(cursor: CursorToken) -> Self {
        Self {
            version: VERSION,
            cursor,
        }
    }
    /// Return the exact opaque cursor bytes.
    pub fn cursor(&self) -> &CursorToken {
        &self.cursor
    }
    /// Encode a canonical ACK.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, StateError> {
        encode(self)
    }
    /// Decode an exact bounded canonical ACK.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(b: &[u8]) -> Result<Self, StateError> {
        let v: Self = decode(b)?;
        if v.version != VERSION {
            return Err(StateError::Invalid);
        }
        Ok(v)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Closed reasons that require a fresh replay/snapshot start.
pub enum ResetReason {
    /// Explicit restore or rewind changed the store incarnation.
    StoreRotated,
    /// The committed position is absent from this index.
    CursorUnknown,
    /// The cursor claims a position beyond the committed head.
    CursorAhead,
    /// Required replay history is no longer safely available.
    HistoryUnavailable,
    /// Current replay visibility differs from the cursor view.
    AuthorizationChanged,
    /// A sequence exists with a different committed digest.
    CommitMismatch,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Reset instruction bound to the current store incarnation.
pub struct SnapshotRequired {
    version: u8,
    reason: ResetReason,
    store_generation: StoreGeneration,
}
impl SnapshotRequired {
    /// Construct a reset instruction.
    pub fn new(reason: ResetReason, store_generation: StoreGeneration) -> Self {
        Self {
            version: VERSION,
            reason,
            store_generation,
        }
    }
    /// Encode a validated canonical reset instruction.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, StateError> {
        if self.store_generation == [0; 16] {
            return Err(StateError::Invalid);
        }
        encode(self)
    }
    /// Decode an exact bounded canonical reset instruction.
    ///
    /// # Errors
    ///
    /// Returns `StateError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(b: &[u8]) -> Result<Self, StateError> {
        let v: Self = decode(b)?;
        if v.version != VERSION || v.store_generation == [0; 16] {
            return Err(StateError::Invalid);
        }
        Ok(v)
    }
    /// Return the closed reset reason.
    pub fn reason(&self) -> ResetReason {
        self.reason
    }
    /// Return the current durable store incarnation.
    pub fn store_generation(&self) -> StoreGeneration {
        self.store_generation
    }
}
/// Digest exactly the supplied canonical bytes without interpreting or admitting them.
/// Callers retain responsibility for bounds, canonical decoding and authorization.
pub fn canonical_item_digest(bytes: &[u8]) -> Digest {
    *blake3::hash(bytes).as_bytes()
}

/// Commit to the exact ordered visible event identifiers.
///
/// # Errors
///
/// Returns `StateError` if the value cannot be canonically encoded.
pub fn visible_order_digest(events: &[(u32, Digest)]) -> Result<Digest, StateError> {
    if events.len() > MAX_ADMITTED_OPERATIONS {
        return Err(StateError::Oversized);
    }
    let b = postcard::to_stdvec(events).map_err(|_| StateError::Codec)?;
    let mut h = blake3::Hasher::new();
    h.update(b"tilo-delivery-v3-visible-order-v1\0");
    h.update(&b);
    Ok(*h.finalize().as_bytes())
}
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, StateError> {
    let b = postcard::to_stdvec(v).map_err(|_| StateError::Codec)?;
    if b.len() > MAX_FRAME_PAYLOAD_BYTES {
        Err(StateError::Oversized)
    } else {
        Ok(b)
    }
}
fn decode<T: for<'de> Deserialize<'de> + Serialize>(b: &[u8]) -> Result<T, StateError> {
    if b.len() > MAX_FRAME_PAYLOAD_BYTES {
        return Err(StateError::Oversized);
    }
    let v = postcard::from_bytes(b).map_err(|_| StateError::Codec)?;
    if postcard::to_stdvec(&v).map_err(|_| StateError::Codec)? != b {
        return Err(StateError::Codec);
    }
    Ok(v)
}
mod token_bytes {
    #[cfg(test)]
    use super::TOKEN_OWNED_COPIES;
    use super::{CursorToken, Deserializer, MAX_CURSOR_BYTES, Serialize, de};
    pub fn serialize<S: serde::Serializer>(v: &CursorToken, s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(v.as_bytes()).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<CursorToken, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = CursorToken;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an opaque cursor of at most 4096 bytes")
            }
            fn visit_borrowed_bytes<E: de::Error>(self, b: &'de [u8]) -> Result<Self::Value, E> {
                if b.len() > MAX_CURSOR_BYTES {
                    return Err(E::custom("cursor too large"));
                }
                #[cfg(test)]
                TOKEN_OWNED_COPIES.with(|count| count.set(count.get().saturating_add(1)));
                CursorToken::from_bytes(b).map_err(E::custom)
            }
            fn visit_bytes<E: de::Error>(self, b: &[u8]) -> Result<Self::Value, E> {
                if b.len() > MAX_CURSOR_BYTES {
                    return Err(E::custom("cursor too large"));
                }
                #[cfg(test)]
                TOKEN_OWNED_COPIES.with(|count| count.set(count.get().saturating_add(1)));
                CursorToken::from_bytes(b).map_err(E::custom)
            }
        }
        d.deserialize_bytes(Visitor)
    }
}
#[cfg(test)]
thread_local! { static TOKEN_OWNED_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
fn reset_token_owned_copies() {
    TOKEN_OWNED_COPIES.with(|count| count.set(0));
}
#[cfg(test)]
fn token_owned_copies() -> usize {
    TOKEN_OWNED_COPIES.with(std::cell::Cell::get)
}
#[derive(Debug, thiserror::Error)]
/// Closed state-payload codec failures.
pub enum StateError {
    /// Field or cross-field invariant failed.
    #[error("invalid state payload")]
    Invalid,
    /// Encoded bytes or bounded count exceeded policy.
    #[error("state payload too large")]
    Oversized,
    /// Postcard encoding was malformed, truncated, trailing, or noncanonical.
    #[error("invalid state encoding")]
    Codec,
}
#[cfg(test)]
mod tests;
