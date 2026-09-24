//! Closed aggregate reconstruction. Memory custody is not native authority.
mod guard;
#[path = "mutation.rs"]
mod mutation;
mod parser;
use super::{AggregateBytes, CodecBudget, Count, ResourceError, StateOutput, private::PrivateWire};
use crate::{ActionReceipt, Bound, State, StoredPublication};
use automerge::resource_limits::NativeReservation;
pub(crate) use mutation::MetadataChange;
use qos_relay::delivery_v3::commit::{
    MAX_REFERENCES, PublicationRef,
    resources::{HeldDeliveryLedger, HeldPublicationPool, LedgerCustody, PoolMapCustody},
};
use serde::{Deserialize, Serialize, ser::SerializeStruct};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(crate) struct DecodeLimits {
    pub(crate) maximum_bytes: usize,
    pub(crate) allocation_bytes: usize,
    pub(crate) collection_entries: usize,
}
struct DeliveryCustody {
    ledger: LedgerCustody,
    pool: PoolMapCustody,
}
// Both success and failure holders declare owned storage before its credits.
struct DeliveryParts {
    state: super::super::DeliveryState,
    custody: DeliveryCustody,
}
pub(crate) struct HeldCloudState {
    state: State,
    _fields: NativeReservation,
    delivery: Option<DeliveryCustody>,
    // One closed metadata operation per candidate; no allocating guard list.
    mutation: Option<NativeReservation>,
}

// The original Store distinguishes absence from a present null before decoding
// State. Missing fields use the default below; present fields must decode their
// nonnullable concrete wire, so no presence evidence is lost in the holder.
fn present_nonnull<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    decoder: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(decoder).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ShadowState {
    version: u32,
    #[serde(default, deserialize_with = "present_nonnull")]
    delivery: Option<DeliveryWire>,
    #[serde(default, deserialize_with = "present_nonnull")]
    private_agent_ledger: Option<PrivateWire>,
    machine_id: String,
    room: String,
    host_secret: [u8; 32],
    challenge: Option<qos_user_identity::cloud_enrollment::Challenge>,
    bound: Option<Bound>,
    consumed: BTreeMap<String, (String, serde_json::Value)>,
    outbox: Vec<StoredPublication>,
    receipts: BTreeMap<String, (String, ActionReceipt)>,
    pending_models: BTreeMap<String, crate::model::PendingModel>,
    #[serde(default)]
    mesh: crate::mesh::MeshState,
    #[serde(default)]
    join_challenges: BTreeMap<String, crate::collaboration::StoredJoinChallenge>,
    #[serde(default)]
    join_responses: BTreeMap<String, crate::collaboration::StoredJoinResponse>,
    #[serde(default)]
    membership_challenges: BTreeMap<String, crate::collaboration::StoredMembershipChallenge>,
    #[serde(default)]
    membership_responses: BTreeMap<String, crate::collaboration::StoredJoinResponse>,
    #[serde(default)]
    invitations: BTreeMap<String, crate::collaboration::StoredInvitation>,
    #[serde(default)]
    canonical_intents: BTreeMap<String, crate::broker::StoredCanonicalIntent>,
    #[serde(default = "crate::initial_generation")]
    generation: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryWire {
    version: u8,
    prior_private_schema: u32,
    ledger: Vec<u8>,
    pool: Vec<u8>,
    baseline_generation: u64,
    baseline: Vec<PublicationRef>,
    tail: Vec<PublicationRef>,
    queue: Vec<super::super::schema::QueuedPublication>,
    policy_lineage: u64,
    policy_revision: String,
    transport_hint: Option<qos_peer_state::join::TransportHint>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryMeta {
    prior_private_schema: u32,
    baseline_generation: u64,
    baseline: Vec<PublicationRef>,
    tail: Vec<PublicationRef>,
    queue: Vec<super::super::schema::QueuedPublication>,
    policy_lineage: u64,
    policy_revision: String,
    transport_hint: Option<qos_peer_state::join::TransportHint>,
}
impl DeliveryWire {
    fn decode(self, budget: &CodecBudget<'_, '_>) -> Result<DeliveryParts, ResourceError> {
        if self.version != 2 {
            return Err(ResourceError::UnsupportedSchema);
        }
        let ledger = budget.decode_ledger(&self.ledger).map_err(nested)?;
        let pool = budget
            .decode_pool(&self.pool, super::super::schema::POOL_LIMIT)
            .map_err(nested)?;
        let meta = DeliveryMeta {
            prior_private_schema: self.prior_private_schema,
            baseline_generation: self.baseline_generation,
            baseline: self.baseline,
            tail: self.tail,
            queue: self.queue,
            policy_lineage: self.policy_lineage,
            policy_revision: self.policy_revision,
            transport_hint: self.transport_hint,
        };
        meta.attach(ledger, pool)
    }
}
impl DeliveryMeta {
    fn attach(
        self,
        ledger: HeldDeliveryLedger,
        pool: HeldPublicationPool,
    ) -> Result<DeliveryParts, ResourceError> {
        if !matches!(self.prior_private_schema, 4 | 5)
            || self.policy_lineage == 0
            || self.policy_revision.len() != 64
            || !self
                .policy_revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.baseline.is_empty()
        {
            return Err(ResourceError::Malformed);
        }
        if self.baseline_generation == 0
            || self.baseline_generation > ledger.ledger().resolved_through
            || !ledger
                .ledger()
                .replay
                .get(&self.baseline_generation)
                .is_some_and(|entry| {
                    entry.owner_generation == self.baseline_generation
                        && entry.publications == self.baseline
                })
        {
            return Err(ResourceError::Malformed);
        }
        if self
            .baseline
            .len()
            .checked_add(self.tail.len())
            .is_none_or(|n| n > MAX_REFERENCES)
            || self.queue.len() > MAX_REFERENCES
        {
            return Err(ResourceError::Capacity);
        }
        for p in self.baseline.iter().chain(&self.tail) {
            pool.pool().resolve(&p.payload).map_err(super::codec)?;
        }
        for p in &self.queue {
            pool.pool().resolve(&p.payload).map_err(super::codec)?;
        }
        for replay in ledger.ledger().replay.values() {
            for p in &replay.publications {
                pool.pool().resolve(&p.payload).map_err(super::codec)?;
            }
        }
        for pin in ledger.ledger().pins.values() {
            for p in &pin.publications {
                pool.pool().resolve(&p.payload).map_err(super::codec)?;
            }
        }
        // All fallible work is complete. No detached credit can drop before
        // its DTO on an error path. The enclosing holder has value-first fields.
        let (ledger, ledger_custody) = ledger.into_parts();
        let (pool, pool_custody) = pool.into_parts();
        Ok(DeliveryParts {
            state: super::super::DeliveryState {
                prior_private_schema: self.prior_private_schema,
                ledger,
                pool,
                baseline_generation: self.baseline_generation,
                baseline: self.baseline,
                tail: self.tail,
                queue: self.queue,
                policy_lineage: self.policy_lineage,
                policy_revision: self.policy_revision,
                transport_hint: self.transport_hint,
            },
            custody: DeliveryCustody {
                ledger: ledger_custody,
                pool: pool_custody,
            },
        })
    }
}
impl ShadowState {
    fn finish(self, fields: NativeReservation, delivery: Option<DeliveryParts>) -> HeldCloudState {
        // Infallible moves only after paired custody transfer.
        let (delivery, delivery_custody) = match delivery {
            Some(DeliveryParts { state, custody }) => (Some(state), Some(custody)),
            None => (None, None),
        };
        HeldCloudState {
            state: State {
                version: self.version,
                delivery,
                private_agent_ledger: self.private_agent_ledger.map(|p| p.0),
                machine_id: self.machine_id,
                room: self.room,
                host_secret: self.host_secret,
                challenge: self.challenge,
                bound: self.bound,
                consumed: self.consumed,
                outbox: self.outbox,
                receipts: self.receipts,
                pending_models: self.pending_models,
                mesh: self.mesh,
                join_challenges: self.join_challenges,
                join_responses: self.join_responses,
                membership_challenges: self.membership_challenges,
                membership_responses: self.membership_responses,
                invitations: self.invitations,
                canonical_intents: self.canonical_intents,
                generation: self.generation,
                delivery_binding: None,
            },
            _fields: fields,
            delivery: delivery_custody,
            mutation: None,
        }
    }
}
fn nested(error: qos_relay::delivery_v3::commit::resources::ResourceError) -> ResourceError {
    use qos_relay::delivery_v3::commit::resources::ResourceError as E;
    match error {
        E::Capacity => ResourceError::Capacity,
        E::Commit(e) => super::codec(e),
        E::Unaccounted => ResourceError::Malformed,
    }
}
fn guarded(error: guard::GuardError) -> ResourceError {
    match error {
        guard::GuardError::Capacity => ResourceError::Capacity,
        guard::GuardError::Malformed => ResourceError::Malformed,
    }
}
fn reserve(budget: &CodecBudget<'_, '_>, n: usize) -> Result<NativeReservation, ResourceError> {
    budget.reserve(n).map_err(|_| ResourceError::Capacity)
}
fn error_bytes() -> usize {
    2 * (size_of::<Box<str>>() + size_of::<std::io::Error>() + 6 * size_of::<usize>())
}
impl HeldCloudState {
    pub(crate) fn state(&self) -> &State {
        &self.state
    }
    pub(crate) fn decode(
        input: &AggregateBytes,
        limits: DecodeLimits,
        budget: &CodecBudget<'_, '_>,
    ) -> Result<Self, ResourceError> {
        if input.as_bytes().len() > limits.maximum_bytes {
            return Err(ResourceError::Capacity);
        }
        let _parser = reserve(budget, parser::bytes(input.as_bytes())?)?;
        let fields = reserve(budget, limits.allocation_bytes)?;
        let mut shadow = guard::state(input.as_bytes(), limits).map_err(guarded)?;
        if !matches!(
            (shadow.version, shadow.delivery.is_some()),
            (4 | 5, false) | (6, true)
        ) {
            return Err(ResourceError::UnsupportedSchema);
        }
        let delivery = shadow
            .delivery
            .take()
            .map(|d| d.decode(budget))
            .transpose()?;
        Ok(shadow.finish(fields, delivery))
    }
    pub(crate) fn fork(
        &self,
        limits: DecodeLimits,
        budget: &CodecBudget<'_, '_>,
    ) -> Result<Self, ResourceError> {
        // Scratch excludes publication bytes/ledger; nested concrete forks keep
        // original shared payload custody. No State::clone is called.
        let scratch = encode_fork(&self.state, limits.maximum_bytes, budget)?;
        let _parser = reserve(budget, parser::bytes(scratch.as_bytes())?)?;
        let fields = reserve(budget, limits.allocation_bytes)?;
        let fork = guard::fork(scratch.as_bytes(), limits).map_err(guarded)?;
        if fork.state.delivery.is_some() {
            return Err(ResourceError::Malformed);
        }
        let delivery = match (&self.state.delivery, &self.delivery, fork.delivery) {
            (Some(original), Some(custody), Some(meta)) => {
                let ledger = budget
                    .fork_ledger(&custody.ledger, &original.ledger)
                    .map_err(nested)?;
                let pool = budget
                    .fork_pool(&custody.pool, &original.pool)
                    .map_err(nested)?;
                Some(meta.attach(ledger, pool)?)
            }
            (None, None, None) => None,
            _ => return Err(ResourceError::Malformed),
        };
        Ok(fork.state.finish(fields, delivery))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForkWire {
    state: ShadowState,
    delivery: Option<DeliveryMeta>,
}
#[derive(Serialize)]
struct ForkOutput<'a> {
    state: StateOutput<'a>,
    delivery: Option<MetaOutput<'a>>,
}
struct MetaOutput<'a>(&'a super::super::DeliveryState);
impl Serialize for MetaOutput<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let d = self.0;
        let mut s = s.serialize_struct("DeliveryFork", 8)?;
        s.serialize_field("prior_private_schema", &d.prior_private_schema)?;
        s.serialize_field("baseline_generation", &d.baseline_generation)?;
        s.serialize_field("baseline", &d.baseline)?;
        s.serialize_field("tail", &d.tail)?;
        s.serialize_field("queue", &d.queue)?;
        s.serialize_field("policy_lineage", &d.policy_lineage)?;
        s.serialize_field("policy_revision", &d.policy_revision)?;
        s.serialize_field("transport_hint", &d.transport_hint)?;
        s.end()
    }
}
fn encode_fork(
    state: &State,
    maximum: usize,
    budget: &CodecBudget<'_, '_>,
) -> Result<AggregateBytes, ResourceError> {
    let _errors = reserve(budget, error_bytes())?;
    let view = ForkOutput {
        state: StateOutput {
            state,
            delivery: None,
        },
        delivery: state.delivery.as_ref().map(MetaOutput),
    };
    let mut count = Count { length: 0, maximum };
    serde_json::to_writer(&mut count, &view).map_err(|_| ResourceError::Capacity)?;
    let mut bytes = AggregateBytes::zeroed(count.length, budget)?;
    let mut sink = bytes.bytes.as_mut_slice();
    serde_json::to_writer(&mut sink, &view).map_err(|_| ResourceError::Malformed)?;
    if !sink.is_empty() {
        return Err(ResourceError::Malformed);
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests;
