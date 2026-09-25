//! Resource-owned metadata candidates; current authority stays with Store.
use super::*;
use qos_relay::delivery_v3::commit::{
    CommitRef, ConsumerId, ConsumerState, DeliveryLedgerV1, MAX_CONSUMERS, MAX_PINS, SnapshotPin,
};
use qos_session_peer::ingress::PublicationCustody;

pub(crate) enum MetadataChange<'a> {
    Consumer(ConsumerState),
    Ack {
        device: [u8; 32],
        consumer: ConsumerId,
        cut: CommitRef,
    },
    PinInsert {
        id: [u8; 16],
        consumer: ConsumerId,
        authorization_view: [u8; 32],
        watermark: CommitRef,
        references: &'a PublicationCustody,
        expires_at_ns: u64,
    },
    PinRenew {
        id: [u8; 16],
        expected: &'a SnapshotPin,
        expires_at_ns: u64,
    },
    PinRelease {
        id: [u8; 16],
        expected: &'a SnapshotPin,
    },
}
fn require(value: bool) -> Result<(), ResourceError> {
    if value {
        Ok(())
    } else {
        Err(ResourceError::Malformed)
    }
}
fn cut(ledger: &DeliveryLedgerV1, reference: &CommitRef) -> Result<(), ResourceError> {
    require(
        reference.sequence != 0
            && reference.sequence <= ledger.resolved_through
            && ledger.replay.get(&reference.sequence).is_some_and(|entry| {
                entry.owner_generation == reference.sequence && entry.digest == reference.digest
            }),
    )
}
// Pinned BTree B=6/CAPACITY=11. Padding each field to the maximum alignment
// bounds repr(Rust) ordering. One insertion can split each ancestor plus a new
// root: binary height+2 is above the actual minimum-six-child tree height.
fn insertion<K, V>(length: usize) -> Result<usize, ResourceError> {
    let align = align_of::<K>()
        .max(align_of::<V>())
        .max(align_of::<usize>());
    let mut node = 0usize;
    for bytes in [
        size_of::<usize>(),
        size_of::<u16>(),
        size_of::<u16>(),
        11usize
            .checked_mul(size_of::<K>())
            .ok_or(ResourceError::Capacity)?,
        11usize
            .checked_mul(size_of::<V>())
            .ok_or(ResourceError::Capacity)?,
        12 * size_of::<usize>(),
    ] {
        let padded = bytes
            .checked_add(align - 1)
            .ok_or(ResourceError::Capacity)?
            & !(align - 1);
        node = node.checked_add(padded).ok_or(ResourceError::Capacity)?;
    }
    let count = length.checked_add(1).ok_or(ResourceError::Capacity)?;
    let path = (usize::BITS - count.leading_zeros()) as usize + 2;
    node.checked_mul(path).ok_or(ResourceError::Capacity)
}
fn same_refs(pin: &SnapshotPin, refs: &PublicationCustody) -> bool {
    pin.publications.len() == refs.len()
        && pin
            .publications
            .iter()
            .enumerate()
            .all(|(i, p)| refs.reference(i) == Some(p))
}
impl HeldCloudState {
    fn preview(&self, change: &MetadataChange<'_>) -> Result<Option<usize>, ResourceError> {
        let delivery = self
            .state
            .delivery
            .as_ref()
            .ok_or(ResourceError::UnsupportedSchema)?;
        let ledger = &delivery.ledger;
        match change {
            MetadataChange::Consumer(value) => {
                require(value.id != [0; 16] && value.acknowledged.is_none())?;
                require(
                    !ledger
                        .consumers
                        .iter()
                        .any(|(device, c)| device != &value.device && c.id == value.id),
                )?;
                if let Some(old) = ledger.consumers.get(&value.device) {
                    require(old.id == value.id && old.root == value.root)?;
                    // Registration at the same policy must not clear a saved ACK.
                    return Ok((old.authorization_view != value.authorization_view).then_some(0));
                }
                if ledger.consumers.len() >= MAX_CONSUMERS {
                    return Err(ResourceError::Capacity);
                }
                Ok(Some(insertion::<[u8; 32], ConsumerState>(
                    ledger.consumers.len(),
                )?))
            }
            MetadataChange::Ack {
                device,
                consumer,
                cut: reference,
            } => {
                cut(ledger, reference)?;
                let old = ledger
                    .consumers
                    .get(device)
                    .ok_or(ResourceError::Malformed)?;
                require(old.id == *consumer)?;
                if let Some(saved) = &old.acknowledged {
                    require(saved.sequence <= reference.sequence)?;
                    if saved == reference {
                        return Ok(None);
                    }
                }
                Ok(Some(0))
            }
            MetadataChange::PinInsert {
                id,
                consumer,
                authorization_view,
                watermark,
                references,
                expires_at_ns,
            } => {
                require(*id != [0; 16] && *expires_at_ns != 0 && !references.is_empty())?;
                if references.len() > MAX_REFERENCES {
                    return Err(ResourceError::Capacity);
                }
                require(
                    ledger
                        .consumers
                        .values()
                        .any(|c| c.id == *consumer && c.authorization_view == *authorization_view),
                )?;
                cut(ledger, watermark)?;
                for i in 0..references.len() {
                    let reference = references.reference(i).ok_or(ResourceError::Malformed)?;
                    delivery
                        .pool
                        .resolve(&reference.payload)
                        .map_err(super::super::codec)?;
                }
                if let Some(old) = ledger.pins.get(id) {
                    require(
                        old.consumer == *consumer
                            && old.authorization_view == *authorization_view
                            && old.watermark == *watermark
                            && old.expires_at_ns == *expires_at_ns
                            && same_refs(old, references),
                    )?;
                    return Ok(None);
                }
                if ledger.pins.len() >= MAX_PINS {
                    return Err(ResourceError::Capacity);
                }
                let vector = references
                    .len()
                    .checked_mul(size_of::<PublicationRef>())
                    .ok_or(ResourceError::Capacity)?;
                Ok(Some(
                    insertion::<[u8; 16], SnapshotPin>(ledger.pins.len())?
                        .checked_add(vector)
                        .ok_or(ResourceError::Capacity)?,
                ))
            }
            MetadataChange::PinRenew {
                id,
                expected,
                expires_at_ns,
            } => {
                let old = ledger.pins.get(id).ok_or(ResourceError::Malformed)?;
                require(
                    std::ptr::eq(old, *expected)
                        && *expires_at_ns >= old.expires_at_ns
                        && *expires_at_ns != 0,
                )?;
                Ok((*expires_at_ns != old.expires_at_ns).then_some(0))
            }
            MetadataChange::PinRelease { id, expected } => {
                let old = ledger.pins.get(id).ok_or(ResourceError::Malformed)?;
                require(std::ptr::eq(old, *expected))?;
                Ok(Some(0))
            }
        }
    }
    pub(crate) fn metadata_candidate(
        &self,
        change: MetadataChange<'_>,
        next_generation: u64,
        limits: DecodeLimits,
        budget: &CodecBudget<'_, '_>,
    ) -> Result<Option<Self>, ResourceError> {
        let source = self
            .state
            .delivery
            .as_ref()
            .ok_or(ResourceError::UnsupportedSchema)?;
        require(
            self.state.version == 6
                && source.ledger.resolved_through == self.state.generation
                && self.state.generation.checked_add(1) == Some(next_generation),
        )?;
        let Some(extra) = self.preview(&change)? else {
            return Ok(None);
        };
        let consumers = source.ledger.consumers.len()
            + usize::from(
                matches!(&change, MetadataChange::Consumer(c) if !source.ledger.consumers.contains_key(&c.device)),
            );
        // Validation's temporary consumer uniqueness set is funded before it
        // exists; no naked encoded_len/validate call may allocate outside credit.
        let scratch = consumers
            .checked_mul(size_of::<super::super::ConsumerNode>())
            .ok_or(ResourceError::Capacity)?;
        let _validation = reserve(budget, scratch)?;
        let extra = if extra == 0 {
            None
        } else {
            Some(reserve(budget, extra)?)
        };
        let mut candidate = self.fork(limits, budget)?;
        candidate.mutation = extra;
        let ledger = &mut candidate
            .state
            .delivery
            .as_mut()
            .ok_or(ResourceError::Malformed)?
            .ledger;
        match change {
            MetadataChange::Consumer(value) => {
                ledger.consumers.insert(value.device, value);
            }
            MetadataChange::Ack {
                device,
                consumer: _,
                cut,
            } => {
                ledger
                    .consumers
                    .get_mut(&device)
                    .ok_or(ResourceError::Malformed)?
                    .acknowledged = Some(cut);
            }
            MetadataChange::PinInsert {
                id,
                consumer,
                authorization_view,
                watermark,
                references,
                expires_at_ns,
            } => {
                let mut publications = Vec::with_capacity(references.len());
                for i in 0..references.len() {
                    publications.push(
                        references
                            .reference(i)
                            .ok_or(ResourceError::Malformed)?
                            .clone(),
                    );
                }
                ledger.pins.insert(
                    id,
                    SnapshotPin {
                        consumer,
                        authorization_view,
                        watermark,
                        publications,
                        expires_at_ns,
                    },
                );
            }
            MetadataChange::PinRenew {
                id,
                expected: _,
                expires_at_ns,
            } => {
                ledger
                    .pins
                    .get_mut(&id)
                    .ok_or(ResourceError::Malformed)?
                    .expires_at_ns = expires_at_ns;
            }
            MetadataChange::PinRelease { id, expected: _ } => {
                ledger.pins.remove(&id);
            }
        }
        ledger.resolved_through = next_generation;
        ledger.encoded_len().map_err(super::super::codec)?;
        candidate.state.generation = next_generation;
        Ok(Some(candidate))
    }
}
#[cfg(test)]
#[path = "mutation/tests.rs"]
mod tests;
