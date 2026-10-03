//! Inert local persistence metadata. Decoding never proves native admission,
//! authority, durability, or a wire cursor. Existing owners supply those barriers.
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    sync::Arc,
};
use tilo_peer_state::transport::Retention;

/// Maximum canonical encoded ledger size under provisional qualification policy.
pub const MAX_LEDGER_BYTES: usize = 4 * 1024 * 1024;
/// Maximum remembered delivery-ID bindings; never silently evicted.
pub const MAX_ADMISSIONS: usize = 4096;
/// Maximum persisted consumers in one workspace.
pub const MAX_CONSUMERS: usize = 1024;
/// Maximum entries in a publication/replay collection.
pub const MAX_REFERENCES: usize = 65536;
/// Maximum active snapshot pin records.
pub const MAX_PINS: usize = 32;
/// Existing maximum size of one canonical state envelope.
pub const MAX_ENVELOPE_BYTES: usize = 64 * 1024 * 1024;
/// Existing host serialized/replay budget ceiling; caller reservations still apply.
pub const MAX_HOST_BYTES: usize = 256 * 1024 * 1024;
/// Digest of exact bytes; carries no authority.
pub type Digest = [u8; 32];
/// Stable identity of an immutable upload item.
pub type DeliveryId = [u8; 16];
/// Persisted device/workspace consumer identifier.
pub type ConsumerId = [u8; 16];
/// Persisted incarnation; ordinary restart must retain it.
pub type StoreGeneration = [u8; 16];

#[derive(Debug, thiserror::Error)]
/// Structural storage-contract failure; never a native admission decision.
pub enum CommitError {
    #[error("invalid delivery metadata: {0}")]
    /// Malformed or inconsistent local metadata.
    Invalid(&'static str),
    #[error("delivery metadata quota exceeded")]
    /// Configured bytes or cardinality exceeded.
    Quota,
    #[error("malformed or noncanonical delivery metadata")]
    /// Truncated, unsupported, noncanonical or trailing encoding.
    Codec,
    #[error("unresolved immutable publication reference")]
    /// A reference does not resolve in the single payload owner.
    MissingPayload,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Reference to one immutable payload, with its existing retention role.
pub struct PublicationRef {
    /// Immutable payload digest.
    pub payload: Digest,
    /// Existing checkpoint/delta policy, independent of payload ownership.
    pub kind: Retention,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Inert durable-position claim; only the native owner can establish it.
pub struct CommitRef {
    /// Owner commit generation or relay committed sequence.
    pub sequence: u64,
    /// Exact canonical item/commit digest, as defined by the owning contract.
    pub digest: Digest,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Delivery identity scoped to the authenticated device within this workspace.
pub struct ReceiptKey {
    /// Certified source/consumer device key.
    pub device: [u8; 32],
    /// Immutable upload item identity.
    pub delivery: DeliveryId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Stored outcome label, not a proof that an item was actually admitted.
pub enum DurableAdmissionKind {
    /// Owner originally committed this operation.
    Accepted,
    /// Owner established an exact previously admitted operation.
    Duplicate,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Untrusted stored receipt metadata; must share the real native state barrier.
pub struct AdmissionRecord {
    /// Exact canonical item/commit digest, as defined by the owning contract.
    pub digest: Digest,
    /// Exact operation identities established by native admission, never transport booleans.
    #[serde(deserialize_with = "bounded_vec::<_, _, MAX_REFERENCES>")]
    pub operations: Vec<Digest>,
    /// Original durable commit reference claimed by this record.
    pub committed: CommitRef,
    /// Stored outcome label.
    pub result: DurableAdmissionKind,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Ordered references for a complete native publication group.
pub struct ReplayCommit {
    /// Existing native owner generation.
    pub owner_generation: u64,
    /// Previous indexed commit; absent for the initial retained entry.
    pub previous: Option<CommitRef>,
    /// Exact canonical item/commit digest, as defined by the owning contract.
    pub digest: Digest,
    /// Ordered immutable publication references.
    #[serde(deserialize_with = "bounded_vec::<_, _, MAX_REFERENCES>")]
    pub publications: Vec<PublicationRef>,
    /// Original operation identities included in this commit.
    #[serde(deserialize_with = "bounded_vec::<_, _, MAX_REFERENCES>")]
    pub admitted_operations: Vec<Digest>,
    /// Owner commit time used for retention policy.
    pub committed_at_ns: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Persisted replay progress scoped to a certified device and policy view.
pub struct ConsumerState {
    /// Stable consumer identity.
    pub id: ConsumerId,
    /// Certified source/consumer device key.
    pub device: [u8; 32],
    /// Certified consumer root key.
    pub root: [u8; 32],
    /// Native policy projection identity, not owner generation.
    pub authorization_view: Digest,
    /// Latest durable consumer acknowledgement claim.
    pub acknowledged: Option<CommitRef>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Reference ownership only; does not establish certified snapshot coverage.
pub struct SnapshotPin {
    /// Owner of the pinned snapshot view.
    pub consumer: ConsumerId,
    /// Native policy projection identity, not owner generation.
    pub authorization_view: Digest,
    /// Pinned complete commit boundary.
    pub watermark: CommitRef,
    /// Ordered immutable publication references.
    #[serde(deserialize_with = "bounded_vec::<_, _, MAX_REFERENCES>")]
    pub publications: Vec<PublicationRef>,
    /// Snapshot pin expiry; expiration alone does not prove safe payload deletion.
    pub expires_at_ns: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
/// Versioned inert metadata. Use encode/decode at storage boundaries.
/// Constructing this DTO does not prove durability or admission. Whole-ledger
/// deserialization must use the byte-bounded entry point, including when an
/// enclosing native aggregate embeds the encoded ledger.
///
/// ```compile_fail
/// use tilo_delivery_protocol::delivery_v3::commit::DeliveryLedgerV1;
/// let _ = postcard::from_bytes::<DeliveryLedgerV1>(&[]);
/// ```
pub struct DeliveryLedgerV1 {
    /// Local metadata schema version, exactly one.
    pub version: u8,
    /// Durable incarnation, changed only by explicit restore/reset workflow.
    pub store_generation: StoreGeneration,
    /// Highest committed/resolved owner position, including non-event commits.
    pub resolved_through: u64,
    /// Ordered replay metadata, keyed by owner generation.
    pub replay: BTreeMap<u64, ReplayCommit>,
    /// Exact delivery-ID/body bindings; no automatic eviction.
    pub admissions: BTreeMap<ReceiptKey, AdmissionRecord>,
    /// Stable consumers keyed by device.
    pub consumers: BTreeMap<[u8; 32], ConsumerState>,
    /// Snapshot reference sets keyed by pin identity.
    pub pins: BTreeMap<[u8; 16], SnapshotPin>,
}
#[derive(Serialize, Deserialize)]
struct LedgerWire {
    version: u8,
    store_generation: StoreGeneration,
    resolved_through: u64,
    #[serde(deserialize_with = "bounded_map::<_, _, _, MAX_REFERENCES>")]
    replay: BTreeMap<u64, ReplayCommit>,
    #[serde(deserialize_with = "bounded_map::<_, _, _, MAX_ADMISSIONS>")]
    admissions: BTreeMap<ReceiptKey, AdmissionRecord>,
    #[serde(deserialize_with = "bounded_map::<_, _, _, MAX_CONSUMERS>")]
    consumers: BTreeMap<[u8; 32], ConsumerState>,
    #[serde(deserialize_with = "bounded_map::<_, _, _, MAX_PINS>")]
    pins: BTreeMap<[u8; 16], SnapshotPin>,
}
impl DeliveryLedgerV1 {
    /// Encode local metadata after structural validation; does not prove admission.
    ///
    /// ```
    /// use tilo_delivery_protocol::delivery_v3::commit::DeliveryLedgerV1;
    /// let unknown = DeliveryLedgerV1 {
    ///     version: 2, store_generation: [1; 16], resolved_through: 0,
    ///     replay: Default::default(), admissions: Default::default(),
    ///     consumers: Default::default(), pins: Default::default(),
    /// };
    /// assert!(unknown.encode().is_err());
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, CommitError> {
        self.validate()?;
        encode_bounded(self, MAX_LEDGER_BYTES)
    }
    /// Exact canonical encoded byte length, validated and counted without an output allocation.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the value violates its wire bounds or cannot be serialized.
    pub fn encoded_len(&self) -> Result<usize, CommitError> {
        self.validate()?;
        encoded_size(self, MAX_LEDGER_BYTES)
    }
    /// Decode a complete bounded canonical ledger. The result is still untrusted.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, CommitError> {
        if bytes.len() > MAX_LEDGER_BYTES {
            return Err(CommitError::Quota);
        }
        let wire: LedgerWire = decode_exact(bytes)?;
        let value = Self {
            version: wire.version,
            store_generation: wire.store_generation,
            resolved_through: wire.resolved_through,
            replay: wire.replay,
            admissions: wire.admissions,
            consumers: wire.consumers,
            pins: wire.pins,
        };
        value.validate()?;
        Ok(value)
    }
    /// Structural consistency only; native admission, policy and actual file
    /// durability must be established by the existing owning transaction.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), CommitError> {
        if self.version != 1 || self.store_generation == [0; 16] {
            return Err(CommitError::Invalid("version or store incarnation"));
        }
        if self.replay.len() > MAX_REFERENCES
            || self.admissions.len() > MAX_ADMISSIONS
            || self.consumers.len() > MAX_CONSUMERS
            || self.pins.len() > MAX_PINS
        {
            return Err(CommitError::Quota);
        }
        // There is no compaction API: the first replay entry has no predecessor
        // and each later one links to the one before. Deleting a prefix would
        // need a certified anchor and a change to this chain contract.
        let mut previous = None;
        for (sequence, entry) in &self.replay {
            if *sequence == 0
                || *sequence > self.resolved_through
                || *sequence != entry.owner_generation
                || entry.previous != previous
                || entry.publications.is_empty()
            {
                return Err(CommitError::Invalid("replay order or unresolved position"));
            }
            bounded_count(entry.publications.len(), MAX_REFERENCES)?;
            bounded_count(entry.admitted_operations.len(), MAX_REFERENCES)?;
            previous = Some(CommitRef {
                sequence: *sequence,
                digest: entry.digest,
            });
        }
        for (key, record) in &self.admissions {
            if key.delivery == [0; 16] || record.operations.is_empty() {
                return Err(CommitError::Invalid("delivery identity or operation set"));
            }
            bounded_count(record.operations.len(), MAX_REFERENCES)?;
            self.check_reference(&record.committed)?;
        }
        let mut consumers = BTreeSet::new();
        for (device, consumer) in &self.consumers {
            if device != &consumer.device
                || consumer.id == [0; 16]
                || !consumers.insert(consumer.id)
            {
                return Err(CommitError::Invalid("consumer identity"));
            }
            if let Some(reference) = &consumer.acknowledged {
                self.check_reference(reference)?;
            }
        }
        for (id, pin) in &self.pins {
            if *id == [0; 16] || !consumers.contains(&pin.consumer) || pin.publications.is_empty() {
                return Err(CommitError::Invalid("snapshot reference owner"));
            }
            // An old policy view can retain references without authorizing
            // reads. Decoding must not erase it during a policy transition.
            bounded_count(pin.publications.len(), MAX_REFERENCES)?;
            self.check_reference(&pin.watermark)?;
        }
        encoded_size(self, MAX_LEDGER_BYTES)?;
        Ok(())
    }
    fn check_reference(&self, reference: &CommitRef) -> Result<(), CommitError> {
        if reference.sequence == 0 || reference.sequence > self.resolved_through {
            return Err(CommitError::Invalid("unresolved commit reference"));
        }
        if self
            .replay
            .get(&reference.sequence)
            .is_some_and(|entry| entry.digest != reference.digest)
        {
            return Err(CommitError::Invalid("conflicting commit reference"));
        }
        // A metadata-only committed owner generation can have no replay entry.
        // This observation is not proof that an absent entry was ever admitted.
        Ok(())
    }
}
#[derive(Clone, Serialize)]
struct PoolWire {
    version: u8,
    #[serde(serialize_with = "shared_payloads")]
    payloads: BTreeMap<Digest, Arc<Vec<u8>>>,
}
fn shared_payloads<S: serde::Serializer>(
    values: &BTreeMap<Digest, Arc<Vec<u8>>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(Some(values.len()))?;
    for (id, bytes) in values {
        // Postcard encodes a byte string exactly as a `u8` sequence, but
        // sizes and copies it whole rather than one call per byte.
        map.serialize_entry(id, serde_bytes::Bytes::new(bytes.as_slice()))?;
    }
    map.end()
}
#[derive(Serialize, Deserialize)]
struct BorrowedPool<'a> {
    version: u8,
    #[serde(borrow, deserialize_with = "borrowed_payloads")]
    payloads: BTreeMap<Digest, &'a [u8]>,
}
fn borrowed_payloads<'de, D: Deserializer<'de>>(
    decoder: D,
) -> Result<BTreeMap<Digest, &'de [u8]>, D::Error> {
    bounded_map::<D, Digest, &'de [u8], MAX_REFERENCES>(decoder)
}
/// The single immutable publication-byte owner for an enclosing native aggregate.
/// No deletion/GC API: dropping queue references cannot erase replay payloads.
/// Payload limits count owned byte lengths, not BTreeMap/allocator metadata or
/// temporary encode/decode overlap; host byte permits must cover those too.
pub struct PublicationPool {
    wire: PoolWire,
    limit: usize,
    bytes: usize,
}
impl PublicationPool {
    /// Fork candidate map metadata while sharing immutable retained byte allocations.
    /// New insertions affect only the fork. Map/encode overlap needs host accounting.
    #[must_use]
    pub fn fork(&self) -> Self {
        Self {
            wire: self.wire.clone(),
            limit: self.limit,
            bytes: self.bytes,
        }
    }

    /// Set the owner's reserved payload-byte budget, no greater than host policy.
    /// This does not reserve memory or select a native aggregate storage limit.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the inputs violate this operation's bounds or invariants.
    pub fn new(limit: usize) -> Result<Self, CommitError> {
        if limit == 0 || limit > MAX_HOST_BYTES {
            return Err(CommitError::Quota);
        }
        Ok(Self {
            wire: PoolWire {
                version: 1,
                payloads: BTreeMap::new(),
            },
            limit,
            bytes: 0,
        })
    }
    /// Preserve opaque original publication bytes once. The caller must establish
    /// their canonical message semantics/native admission before owner commit.
    /// Pressure and conflicting identities never alter already retained entries.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the resource budget refuses the allocation or the input is invalid.
    pub fn intern(&mut self, bytes: &[u8]) -> Result<Digest, CommitError> {
        if bytes.is_empty() || bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(CommitError::Quota);
        }
        let id = *blake3::hash(bytes).as_bytes();
        if let Some(prior) = self.wire.payloads.get(&id) {
            return if prior.as_slice() == bytes {
                Ok(id)
            } else {
                Err(CommitError::Invalid("conflicting payload digest"))
            };
        }
        let total = self
            .bytes
            .checked_add(bytes.len())
            .ok_or(CommitError::Quota)?;
        if total > self.limit || self.wire.payloads.len() >= MAX_REFERENCES {
            return Err(CommitError::Quota);
        }
        self.wire.payloads.insert(id, Arc::new(bytes.to_vec()));
        self.bytes = total;
        Ok(id)
    }
    /// Resolve the exact original bytes without a queue-position dependency.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the inputs violate this operation's bounds or invariants.
    pub fn resolve(&self, id: &Digest) -> Result<&[u8], CommitError> {
        self.wire
            .payloads
            .get(id)
            .map(|bytes| bytes.as_slice())
            .ok_or(CommitError::MissingPayload)
    }
    /// Number of unique retained payloads, independent of reference multiplicity.
    pub fn len(&self) -> usize {
        self.wire.payloads.len()
    }
    /// Whether this owner retains any payloads.
    pub fn is_empty(&self) -> bool {
        self.wire.payloads.is_empty()
    }
    /// Retained payload backing capacity, excluding map and Arc metadata.
    /// Each backing is counted once within this pool, independent of Arc clones.
    /// The caller must not add this again for a fork sharing the same payloads.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the resource budget refuses the allocation or the input is invalid.
    pub fn retained_payload_capacity(&self) -> Result<usize, CommitError> {
        // Only intern/decode construct entries: intern allocates for an absent
        // content digest; decode verifies unique digest keys and allocates once
        // per key. Fork preserves that one-backing-per-key invariant. Thus this
        // linear scan needs neither a pointer set nor a quadratic alias search.
        self.wire
            .payloads
            .values()
            .try_fold(0usize, |total, bytes| {
                total
                    .checked_add(bytes.capacity())
                    .ok_or(CommitError::Quota)
            })
    }
    /// Encode only after checking the complete output size without allocating it.
    /// The caller reserves this additional encoded buffer separately from the pool.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self, max_encoded_bytes: usize) -> Result<Vec<u8>, CommitError> {
        if max_encoded_bytes > MAX_HOST_BYTES {
            return Err(CommitError::Quota);
        }
        encode_bounded(&self.wire, max_encoded_bytes)
    }
    /// Exact canonical output length without allocating an encoded buffer.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the value violates its wire bounds or cannot be serialized.
    pub fn encoded_len(&self, max_encoded_bytes: usize) -> Result<usize, CommitError> {
        if max_encoded_bytes > MAX_HOST_BYTES {
            return Err(CommitError::Quota);
        }
        encoded_size(&self.wire, max_encoded_bytes)
    }
    /// Validate all borrowed payload digests/lengths and the aggregate payload
    /// budget before allocating any owned payload buffer. Map metadata is bounded.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &[u8], limit: usize) -> Result<Self, CommitError> {
        let mut pool = Self::new(limit)?;
        // At most 32 digest bytes plus a ten-byte length per bounded map entry,
        // and version/map framing. Reject before allocating the borrowed map.
        let encoded_cap = limit
            .saturating_add(MAX_REFERENCES * 42 + 16)
            .min(MAX_HOST_BYTES);
        if bytes.len() > encoded_cap {
            return Err(CommitError::Quota);
        }
        let wire: BorrowedPool<'_> = decode_exact(bytes)?;
        if wire.version != 1 {
            return Err(CommitError::Invalid("pool version"));
        }
        let mut total = 0usize;
        for (id, payload) in &wire.payloads {
            if payload.is_empty() || payload.len() > MAX_ENVELOPE_BYTES {
                return Err(CommitError::Quota);
            }
            total = total.checked_add(payload.len()).ok_or(CommitError::Quota)?;
            if total > limit {
                return Err(CommitError::Quota);
            }
            if id != blake3::hash(payload).as_bytes() {
                return Err(CommitError::Invalid("payload digest"));
            }
        }
        pool.wire.payloads = wire
            .payloads
            .into_iter()
            .map(|(id, bytes)| (id, Arc::new(bytes.to_vec())))
            .collect();
        pool.bytes = total;
        Ok(pool)
    }
    /// Derive reference counts from queue/baseline roots plus replay/snapshot
    /// roots, rejecting every dangling reference. Counts confer no GC authority.
    ///
    /// # Errors
    ///
    /// Returns `CommitError` if the resource budget refuses the allocation or the input is invalid.
    pub fn reference_counts(
        &self,
        roots: &[PublicationRef],
        ledger: &DeliveryLedgerV1,
    ) -> Result<BTreeMap<Digest, u64>, CommitError> {
        bounded_count(roots.len(), MAX_REFERENCES)?;
        ledger.validate()?;
        let mut counts = BTreeMap::<Digest, u64>::new();
        for reference in roots
            .iter()
            .chain(
                ledger
                    .replay
                    .values()
                    .flat_map(|entry| entry.publications.iter()),
            )
            .chain(ledger.pins.values().flat_map(|pin| pin.publications.iter()))
        {
            self.resolve(&reference.payload)?;
            let count = counts.entry(reference.payload).or_default();
            *count = count.checked_add(1).ok_or(CommitError::Quota)?;
        }
        Ok(counts)
    }
}
fn bounded_count(count: usize, max: usize) -> Result<(), CommitError> {
    if count > max {
        Err(CommitError::Quota)
    } else {
        Ok(())
    }
}
fn bounded_vec<'de, D, T, const MAX: usize>(decoder: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Values<T, const MAX: usize>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const MAX: usize> de::Visitor<'de> for Values<T, MAX> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "at most {MAX} values")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
            if input.size_hint().is_some_and(|length| length > MAX) {
                return Err(de::Error::custom("collection exceeds bound"));
            }
            let mut values = Vec::new();
            while values.len() < MAX {
                match input.next_element()? {
                    Some(value) => values.push(value),
                    None => return Ok(values),
                }
            }
            if input.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("collection exceeds bound"));
            }
            Ok(values)
        }
    }
    decoder.deserialize_seq(Values::<T, MAX>(PhantomData))
}
fn bounded_map<'de, D, K, V, const MAX: usize>(decoder: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    struct Values<K, V, const MAX: usize>(PhantomData<(K, V)>);
    impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>, const MAX: usize> de::Visitor<'de>
        for Values<K, V, MAX>
    {
        type Value = BTreeMap<K, V>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "at most {MAX} ordered unique entries")
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
            if input.size_hint().is_some_and(|length| length > MAX) {
                return Err(de::Error::custom("map exceeds bound"));
            }
            let mut values = BTreeMap::new();
            while let Some(key) = input.next_key()? {
                if values.len() >= MAX {
                    return Err(de::Error::custom("map exceeds bound"));
                }
                if values
                    .last_key_value()
                    .is_some_and(|(prior, _)| prior >= &key)
                {
                    return Err(de::Error::custom("map keys are not canonical"));
                }
                let value = input.next_value()?;
                values.insert(key, value);
            }
            Ok(values)
        }
    }
    decoder.deserialize_map(Values::<K, V, MAX>(PhantomData))
}
fn encode_bounded<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>, CommitError> {
    let length = encoded_size(value, max)?;
    let mut output = vec![0; length];
    let encoded = postcard::to_slice(value, &mut output).map_err(|_| CommitError::Codec)?;
    if encoded.len() != length {
        return Err(CommitError::Codec);
    }
    Ok(output)
}
fn encoded_size<T: Serialize>(value: &T, max: usize) -> Result<usize, CommitError> {
    postcard::serialize_with_flavor(
        value,
        Count {
            bytes: 0,
            limit: max,
        },
    )
    .map_err(|_| CommitError::Quota)
}
struct Count {
    bytes: usize,
    limit: usize,
}
impl postcard::ser_flavors::Flavor for Count {
    type Output = usize;
    fn try_push(&mut self, _: u8) -> postcard::Result<()> {
        self.add(1)
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.add(bytes.len())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.bytes)
    }
}
impl Count {
    fn add(&mut self, len: usize) -> postcard::Result<()> {
        let next = self
            .bytes
            .checked_add(len)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        if next > self.limit {
            return Err(postcard::Error::SerializeBufferFull);
        }
        self.bytes = next;
        Ok(())
    }
}
fn decode_exact<'de, T: Deserialize<'de> + Serialize>(bytes: &'de [u8]) -> Result<T, CommitError> {
    let (value, trailing) = postcard::take_from_bytes(bytes).map_err(|_| CommitError::Codec)?;
    if !trailing.is_empty() {
        return Err(CommitError::Codec);
    }
    postcard::serialize_with_flavor(&value, Exact(bytes)).map_err(|_| CommitError::Codec)?;
    Ok(value)
}
struct Exact<'a>(&'a [u8]);
impl postcard::ser_flavors::Flavor for Exact<'_> {
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
#[cfg(test)]
mod tests;

mod pin;
pub use pin::PublicationPayload;
