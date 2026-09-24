//! Closed, allocation-free shape decoding. Only fixed native codec fields and
//! borrowed bytes are admitted here; arbitrary custom Deserialize is excluded.
use super::*;
use layout::{Vectors, add, tree};

trait Shape {
    fn vectors(&self) -> Result<Vectors, ResourceError>;
}
impl Shape for ConsumerState {
    fn vectors(&self) -> Result<Vectors, ResourceError> {
        Ok(Vectors::default())
    }
}
impl Shape for &[u8] {
    fn vectors(&self) -> Result<Vectors, ResourceError> {
        Ok(Vectors::default())
    }
}
struct Sequence<T> {
    vectors: Vectors,
    _element: PhantomData<T>,
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Sequence<T> {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Values<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> de::Visitor<'de> for Values<T> {
            type Value = Sequence<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded fixed-element sequence")
            }
            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut input: A,
            ) -> Result<Self::Value, A::Error> {
                if input.size_hint().is_some_and(|n| n > MAX_REFERENCES) {
                    return Err(de::Error::custom("collection exceeds bound"));
                }
                let mut len = 0;
                while len < MAX_REFERENCES {
                    match input.next_element::<T>()? {
                        Some(_) => len += 1,
                        None => {
                            return Ok(Sequence {
                                vectors: Vectors::pushed::<T>(len)
                                    .map_err(|_| de::Error::custom("allocation overflow"))?,
                                _element: PhantomData,
                            });
                        }
                    }
                }
                if input.next_element::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::custom("collection exceeds bound"));
                }
                Ok(Sequence {
                    vectors: Vectors::pushed::<T>(len)
                        .map_err(|_| de::Error::custom("allocation overflow"))?,
                    _element: PhantomData,
                })
            }
        }
        decoder.deserialize_seq(Values::<T>(PhantomData))
    }
}
struct Map<K, S, const MAX: usize> {
    len: usize,
    vectors: Vectors,
    _types: PhantomData<(K, S)>,
}
impl<'de, K: Deserialize<'de> + Ord, S: Deserialize<'de> + Shape, const MAX: usize> Deserialize<'de>
    for Map<K, S, MAX>
{
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Values<K, S, const MAX: usize>(PhantomData<(K, S)>);
        impl<'de, K: Deserialize<'de> + Ord, S: Deserialize<'de> + Shape, const MAX: usize>
            de::Visitor<'de> for Values<K, S, MAX>
        {
            type Value = Map<K, S, MAX>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded ordered map")
            }
            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut input: A,
            ) -> Result<Self::Value, A::Error> {
                if input.size_hint().is_some_and(|n| n > MAX) {
                    return Err(de::Error::custom("map exceeds bound"));
                }
                let mut result = Map {
                    len: 0,
                    vectors: Vectors::default(),
                    _types: PhantomData,
                };
                let mut previous = None;
                while let Some(key) = input.next_key::<K>()? {
                    if result.len >= MAX {
                        return Err(de::Error::custom("map exceeds bound"));
                    }
                    if previous.as_ref().is_some_and(|prior| prior >= &key) {
                        return Err(de::Error::custom("map keys are not canonical"));
                    }
                    let value = input.next_value::<S>()?;
                    result.vectors = result
                        .vectors
                        .add(
                            value
                                .vectors()
                                .map_err(|_| de::Error::custom("allocation overflow"))?,
                        )
                        .map_err(|_| de::Error::custom("allocation overflow"))?;
                    result.len += 1;
                    previous = Some(key);
                }
                Ok(result)
            }
        }
        decoder.deserialize_map(Values::<K, S, MAX>(PhantomData))
    }
}
// Fields deliberately have the exact Postcard sequence order of the original
// concrete types. Scalar values need no heap; original validation remains the
// authority on structural errors after paid decode and canonical comparison.
#[allow(dead_code, reason = "fields are validated by deserialization only")]
#[derive(Deserialize)]
struct Replay {
    owner_generation: u64,
    previous: Option<CommitRef>,
    digest: Digest,
    publications: Sequence<PublicationRef>,
    admitted_operations: Sequence<Digest>,
    committed_at_ns: u64,
}
impl Shape for Replay {
    fn vectors(&self) -> Result<Vectors, ResourceError> {
        self.publications
            .vectors
            .add(self.admitted_operations.vectors)
    }
}
#[allow(dead_code, reason = "fields are validated by deserialization only")]
#[derive(Deserialize)]
struct Admission {
    digest: Digest,
    operations: Sequence<Digest>,
    committed: CommitRef,
    result: DurableAdmissionKind,
}
impl Shape for Admission {
    fn vectors(&self) -> Result<Vectors, ResourceError> {
        Ok(self.operations.vectors)
    }
}
#[allow(dead_code, reason = "fields are validated by deserialization only")]
#[derive(Deserialize)]
struct Pin {
    consumer: ConsumerId,
    authorization_view: Digest,
    watermark: CommitRef,
    publications: Sequence<PublicationRef>,
    expires_at_ns: u64,
}
impl Shape for Pin {
    fn vectors(&self) -> Result<Vectors, ResourceError> {
        Ok(self.publications.vectors)
    }
}
#[allow(dead_code, reason = "fields are validated by deserialization only")]
#[derive(Deserialize)]
struct Ledger {
    version: u8,
    store_generation: StoreGeneration,
    resolved_through: u64,
    replay: Map<u64, Replay, MAX_REFERENCES>,
    admissions: Map<ReceiptKey, Admission, MAX_ADMISSIONS>,
    consumers: Map<[u8; 32], ConsumerState, MAX_CONSUMERS>,
    pins: Map<[u8; 16], Pin, MAX_PINS>,
}
#[allow(dead_code, reason = "fields are validated by deserialization only")]
#[derive(Deserialize)]
struct Pool<'a> {
    version: u8,
    #[serde(borrow)]
    payloads: Map<Digest, &'a [u8], MAX_REFERENCES>,
}
fn read<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, ResourceError> {
    let (value, trailing) = postcard::take_from_bytes(bytes).map_err(|_| CommitError::Codec)?;
    if !trailing.is_empty() {
        return Err(CommitError::Codec.into());
    }
    Ok(value)
}
pub(super) fn ledger(bytes: &[u8]) -> Result<(usize, usize), ResourceError> {
    if bytes.len() > MAX_LEDGER_BYTES {
        return Err(CommitError::Quota.into());
    }
    let shape: Ledger = read(bytes)?;
    let vectors = shape
        .replay
        .vectors
        .add(shape.admissions.vectors)?
        .add(shape.pins.vectors)?;
    let maps = map_bytes(
        shape.replay.len,
        shape.admissions.len,
        shape.consumers.len,
        shape.pins.len,
    )?;
    Ok((
        add(maps, vectors.retained)?,
        add(
            vectors.scratch,
            tree::<ConsumerId, ()>(shape.consumers.len)?,
        )?,
    ))
}
fn map_bytes(
    replay: usize,
    admissions: usize,
    consumers: usize,
    pins: usize,
) -> Result<usize, ResourceError> {
    add(
        add(
            tree::<u64, ReplayCommit>(replay)?,
            tree::<ReceiptKey, AdmissionRecord>(admissions)?,
        )?,
        add(
            tree::<[u8; 32], ConsumerState>(consumers)?,
            tree::<[u8; 16], SnapshotPin>(pins)?,
        )?,
    )
}
pub(super) fn ledger_fork(ledger: &DeliveryLedgerV1) -> Result<usize, ResourceError> {
    let mut total = map_bytes(
        ledger.replay.len(),
        ledger.admissions.len(),
        ledger.consumers.len(),
        ledger.pins.len(),
    )?;
    for entry in ledger.replay.values() {
        total = add(
            total,
            Vectors::cloned::<PublicationRef>(entry.publications.len())?.retained,
        )?;
        total = add(
            total,
            Vectors::cloned::<Digest>(entry.admitted_operations.len())?.retained,
        )?;
    }
    for entry in ledger.admissions.values() {
        total = add(
            total,
            Vectors::cloned::<Digest>(entry.operations.len())?.retained,
        )?;
    }
    for entry in ledger.pins.values() {
        total = add(
            total,
            Vectors::cloned::<PublicationRef>(entry.publications.len())?.retained,
        )?;
    }
    Ok(total)
}
pub(super) fn pool(bytes: &[u8], limit: usize) -> Result<usize, ResourceError> {
    // Preserve the existing outer policy and its error before any codec work.
    PublicationPool::new(limit)?;
    if bytes.len()
        > limit
            .saturating_add(MAX_REFERENCES * 42 + 16)
            .min(MAX_HOST_BYTES)
    {
        return Err(CommitError::Quota.into());
    }
    let shape: Pool<'_> = read(bytes)?;
    Ok(shape.payloads.len)
}
