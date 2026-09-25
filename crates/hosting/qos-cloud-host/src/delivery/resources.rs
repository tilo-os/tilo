//! Closed Cloud aggregate byte custody. Not a decoded-owner or native qualifier.
use crate::State;
use automerge::resource_limits::NativeReservation;
use qos_relay::delivery_v3::commit::{CommitError, MAX_CONSUMERS, MAX_REFERENCES};
use serde::{Serialize, ser::SerializeStruct};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResourceError {
    Capacity,
    Malformed,
    Io,
    UnsupportedSchema,
}
/// Backing always dies before the real caller's reservation. No naked Vec escape.
pub(crate) struct AggregateBytes {
    bytes: Vec<u8>,
    _reservation: NativeReservation,
}
impl AggregateBytes {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
    fn zeroed(length: usize, budget: &CodecBudget<'_, '_>) -> Result<Self, ResourceError> {
        let reservation = budget
            .reserve(length)
            .map_err(|_| ResourceError::Capacity)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| ResourceError::Capacity)?;
        bytes.resize(length, 0);
        Ok(Self {
            bytes,
            _reservation: reservation,
        })
    }
    pub(crate) fn copy_from(
        bytes: &[u8],
        maximum: usize,
        budget: &CodecBudget<'_, '_>,
    ) -> Result<Self, ResourceError> {
        if bytes.len() > maximum {
            return Err(ResourceError::Capacity);
        }
        if bytes.is_empty() {
            return Err(ResourceError::Malformed);
        }
        let mut value = Self::zeroed(bytes.len(), budget)?;
        value.bytes.copy_from_slice(bytes);
        Ok(value)
    }
    /// The caller already owns the original open descriptor and Store barrier.
    /// No path lookup, second lock, unbounded read_to_end or growing Vec occurs.
    pub(crate) fn read_file(
        file: &mut File,
        maximum: usize,
        budget: &CodecBudget<'_, '_>,
    ) -> Result<Self, ResourceError> {
        let length = usize::try_from(file.metadata().map_err(|_| ResourceError::Io)?.len())
            .map_err(|_| ResourceError::Capacity)?;
        if length > maximum {
            return Err(ResourceError::Capacity);
        }
        if length == 0 {
            return Err(ResourceError::Malformed);
        }
        let mut value = Self::zeroed(length, budget)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| ResourceError::Io)?;
        file.read_exact(&mut value.bytes)
            .map_err(|_| ResourceError::Io)?;
        let mut extra = [0; 1];
        if file.read(&mut extra).map_err(|_| ResourceError::Io)? != 0 {
            return Err(ResourceError::Malformed);
        }
        Ok(value)
    }
}
impl AggregateBytes {
    /// This checkpoint with its non-empty `state.journal` replayed onto it
    /// (see `storage::journal`). The journal copy and the replay transients
    /// are charged before they exist; the result is a charged copy bounded by
    /// `maximum`.
    pub(crate) fn with_journal(
        self,
        journal: &mut File,
        maximum: usize,
        budget: &CodecBudget<'_, '_>,
    ) -> Result<Self, ResourceError> {
        let journal = Self::read_file(journal, maximum.saturating_add(1024 * 1024), budget)?;
        let transient = self
            .bytes
            .len()
            .checked_add(journal.bytes.len())
            .and_then(|bytes| bytes.checked_mul(2))
            .ok_or(ResourceError::Capacity)?;
        let _transient = budget
            .reserve(transient)
            .map_err(|_| ResourceError::Capacity)?;
        let state = crate::storage::reconstruct(&self.bytes, &journal.bytes);
        drop(journal);
        drop(self);
        Self::copy_from(&state, maximum, budget)
    }
}
fn codec(error: CommitError) -> ResourceError {
    match error {
        CommitError::Quota => ResourceError::Capacity,
        _ => ResourceError::Malformed,
    }
}
// Pinned Rust1.97 alloc/btree/node.rs: B=6,11 key slots,12 child edges.
// For the temporary BTreeSet<[u8;16]> in ledger.validate, every live node
// contains a key (except a transient new root during splitting). Paying one
// full internal node per final key covers leaf/internal/split allocations.
// Fixed C accounting layout is >= actual leaf+edge storage on supported targets.
#[repr(C)]
struct ConsumerNode {
    parent: usize,
    parent_index: u16,
    length: u16,
    keys: [[u8; 16]; 11],
    edges: [usize; 12],
}
struct DeliveryBuffers {
    ledger: AggregateBytes,
    pool: AggregateBytes,
}
fn delivery_buffers(
    state: &super::DeliveryState,
    budget: &CodecBudget<'_, '_>,
) -> Result<DeliveryBuffers, ResourceError> {
    if state.ledger.consumers.len() > MAX_CONSUMERS {
        return Err(ResourceError::Capacity);
    }
    let scratch = state
        .ledger
        .consumers
        .len()
        .checked_mul(size_of::<ConsumerNode>())
        .ok_or(ResourceError::Capacity)?;
    let _validation = budget
        .reserve(scratch)
        .map_err(|_| ResourceError::Capacity)?;
    // Same closed structural checks as DeliveryState::validate, without anyhow
    // error/backtrace construction inside the bounded allocation operation.
    if !matches!(state.prior_private_schema, 4 | 5)
        || state.policy_lineage == 0
        || state.policy_revision.len() != 64
        || !state
            .policy_revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || state.baseline.is_empty()
    {
        return Err(ResourceError::Malformed);
    }
    if state.baseline_generation == 0
        || state.baseline_generation > state.ledger.resolved_through
        || !state
            .ledger
            .replay
            .get(&state.baseline_generation)
            .is_some_and(|entry| {
                entry.owner_generation == state.baseline_generation
                    && entry.publications == state.baseline
            })
    {
        return Err(ResourceError::Malformed);
    }
    if state
        .baseline
        .len()
        .checked_add(state.tail.len())
        .is_none_or(|n| n > MAX_REFERENCES)
        || state.queue.len() > MAX_REFERENCES
    {
        return Err(ResourceError::Capacity);
    }
    for reference in state.baseline.iter().chain(&state.tail) {
        state.pool.resolve(&reference.payload).map_err(codec)?;
    }
    for reference in &state.queue {
        state.pool.resolve(&reference.payload).map_err(codec)?;
    }
    for replay in state.ledger.replay.values() {
        for reference in &replay.publications {
            state.pool.resolve(&reference.payload).map_err(codec)?;
        }
    }
    for pin in state.ledger.pins.values() {
        for reference in &pin.publications {
            state.pool.resolve(&reference.payload).map_err(codec)?;
        }
    }
    let length = state.ledger.encoded_len().map_err(codec)?;
    let reservation = budget
        .reserve(length)
        .map_err(|_| ResourceError::Capacity)?;
    // Existing encoder counts then creates exactly vec![0;length]. Its validation
    // temporary is already funded above; no post-hoc heap estimate is used.
    let bytes = state.ledger.encode().map_err(codec)?;
    let ledger = AggregateBytes {
        bytes,
        _reservation: reservation,
    };
    let length = state
        .pool
        .encoded_len(super::schema::POOL_LIMIT)
        .map_err(codec)?;
    let reservation = budget
        .reserve(length)
        .map_err(|_| ResourceError::Capacity)?;
    let bytes = state
        .pool
        .encode(super::schema::POOL_LIMIT)
        .map_err(codec)?;
    Ok(DeliveryBuffers {
        ledger,
        pool: AggregateBytes {
            bytes,
            _reservation: reservation,
        },
    })
}
struct DeliveryOutput<'a> {
    state: &'a super::DeliveryState,
    buffers: &'a DeliveryBuffers,
}
impl Serialize for DeliveryOutput<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut out = s.serialize_struct("CloudDeliveryV2", 11)?;
        out.serialize_field("version", &2u8)?;
        out.serialize_field("prior_private_schema", &self.state.prior_private_schema)?;
        out.serialize_field("ledger", self.buffers.ledger.as_bytes())?;
        out.serialize_field("pool", self.buffers.pool.as_bytes())?;
        out.serialize_field("baseline_generation", &self.state.baseline_generation)?;
        out.serialize_field("baseline", &self.state.baseline)?;
        out.serialize_field("tail", &self.state.tail)?;
        out.serialize_field("queue", &self.state.queue)?;
        out.serialize_field("policy_lineage", &self.state.policy_lineage)?;
        out.serialize_field("policy_revision", &self.state.policy_revision)?;
        out.serialize_field("transport_hint", &self.state.transport_hint)?;
        out.end()
    }
}
struct StateOutput<'a> {
    state: &'a State,
    delivery: Option<DeliveryOutput<'a>>,
}
impl Serialize for StateOutput<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let state = self.state;
        let mut out = s.serialize_struct(
            "State",
            18 + usize::from(self.delivery.is_some())
                + usize::from(state.private_agent_ledger.is_some()),
        )?;
        out.serialize_field("version", &state.version)?;
        if let Some(delivery) = &self.delivery {
            out.serialize_field("delivery", delivery)?;
        }
        // PrivateAgentState's Serialize is a borrowed untagged enum, unlike its
        // allocating Deserialize. No Value conversion/legacy clone is performed.
        if let Some(private) = &state.private_agent_ledger {
            out.serialize_field("private_agent_ledger", private)?;
        }
        out.serialize_field("machine_id", &state.machine_id)?;
        out.serialize_field("room", &state.room)?;
        out.serialize_field("host_secret", &state.host_secret)?;
        out.serialize_field("challenge", &state.challenge)?;
        out.serialize_field("bound", &state.bound)?;
        out.serialize_field("consumed", &state.consumed)?;
        out.serialize_field("outbox", &state.outbox)?;
        out.serialize_field("receipts", &state.receipts)?;
        out.serialize_field("pending_models", &state.pending_models)?;
        out.serialize_field("mesh", &state.mesh)?;
        out.serialize_field("join_challenges", &state.join_challenges)?;
        out.serialize_field("join_responses", &state.join_responses)?;
        out.serialize_field("membership_challenges", &state.membership_challenges)?;
        out.serialize_field("membership_responses", &state.membership_responses)?;
        out.serialize_field("invitations", &state.invitations)?;
        out.serialize_field("canonical_intents", &state.canonical_intents)?;
        out.serialize_field("generation", &state.generation)?;
        out.end()
    }
}
struct Count {
    length: usize,
    maximum: usize,
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.length = self
            .length
            .checked_add(bytes.len())
            .filter(|n| *n <= self.maximum)
            .ok_or(std::io::ErrorKind::WriteZero)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn encode_state(
    state: &State,
    maximum: usize,
    budget: &CodecBudget<'_, '_>,
) -> Result<AggregateBytes, ResourceError> {
    if !matches!(
        (state.version, state.delivery.is_some()),
        (4 | 5, false) | (6, true)
    ) {
        return Err(ResourceError::UnsupportedSchema);
    }
    // Pinned serde_json1.0.151 ErrorImpl: ErrorCode payload bounded by Box<str>/
    // io::Error plus positions/discriminant. fix_position may overlap two boxes.
    // All successful known Serialize implementations borrow fields, including
    // Value/map iteration and stack-based UUID/number/escape formatting.
    let _errors = budget
        .reserve(2 * (size_of::<Box<str>>() + size_of::<std::io::Error>() + 6 * size_of::<usize>()))
        .map_err(|_| ResourceError::Capacity)?;
    let buffers = state
        .delivery
        .as_ref()
        .map(|d| delivery_buffers(d, budget))
        .transpose()?;
    let view = StateOutput {
        state,
        delivery: state
            .delivery
            .as_ref()
            .zip(buffers.as_ref())
            .map(|(state, buffers)| DeliveryOutput { state, buffers }),
    };
    let mut count = Count { length: 0, maximum };
    serde_json::to_writer(&mut count, &view).map_err(|_| ResourceError::Capacity)?;
    let mut output = AggregateBytes::zeroed(count.length, budget)?;
    let mut sink = output.bytes.as_mut_slice();
    serde_json::to_writer(&mut sink, &view).map_err(|_| ResourceError::Malformed)?;
    if !sink.is_empty() {
        return Err(ResourceError::Malformed);
    }
    Ok(output)
}
#[cfg(test)]
mod tests;

mod private;

mod decode;
pub(crate) use decode::{DecodeLimits, HeldCloudState, MetadataChange};

mod budget;
pub(crate) use budget::CodecBudget;

/// Test-only observer shared by the actual Store/codec integration fixture.
#[cfg(test)]
pub(crate) fn precharged_test<T>(
    allowance: &automerge::resource_limits::NativeAllowance,
    action: impl FnOnce() -> T,
) -> T {
    tests::precharged(allowance, action)
}

mod private_validation;
pub(crate) use private_validation::PrivateStateFailure;

mod delivery_validation;
pub(crate) use delivery_validation::DeliveryStateFailure;
