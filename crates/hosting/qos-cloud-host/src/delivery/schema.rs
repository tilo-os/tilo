//! Inert persisted delivery data. Only the enclosing Store transaction commits it.
use crate::{PublicationKind, State};
use anyhow::Result;
use qos_relay::delivery_v3::commit::*;
use serde::{Deserialize, Serialize, ser::SerializeStruct};

pub(crate) const VERSION: u32 = 6;
pub(super) const POOL_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QueuedPublication {
    pub(super) payload: Digest,
    pub(super) kind: PublicationKind,
}

pub(crate) struct DeliveryState {
    pub(super) prior_private_schema: u32,
    pub(super) ledger: DeliveryLedgerV1,
    pub(super) pool: PublicationPool,
    pub(super) baseline_generation: u64,
    pub(super) baseline: Vec<PublicationRef>,
    pub(super) tail: Vec<PublicationRef>,
    pub(super) queue: Vec<QueuedPublication>,
    pub(super) policy_lineage: u64,
    pub(super) policy_revision: String,
    pub(super) transport_hint: Option<qos_peer_state::join::TransportHint>,
}
impl Clone for DeliveryState {
    fn clone(&self) -> Self {
        Self {
            prior_private_schema: self.prior_private_schema,
            ledger: self.ledger.clone(),
            pool: self.pool.fork(),
            baseline_generation: self.baseline_generation,
            baseline: self.baseline.clone(),
            tail: self.tail.clone(),
            queue: self.queue.clone(),
            policy_lineage: self.policy_lineage,
            policy_revision: self.policy_revision.clone(),
            transport_hint: self.transport_hint.clone(),
        }
    }
}
// The old outer JSON aggregate remains unchanged. Inner codecs have one canonical
// versioned representation; none of these decoded labels creates live authority.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    prior_private_schema: u32,
    ledger: Vec<u8>,
    pool: Vec<u8>,
    baseline_generation: u64,
    baseline: Vec<PublicationRef>,
    tail: Vec<PublicationRef>,
    queue: Vec<QueuedPublication>,
    policy_lineage: u64,
    policy_revision: String,
    transport_hint: Option<qos_peer_state::join::TransportHint>,
}
impl Serialize for DeliveryState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(serde::ser::Error::custom)?;
        let ledger = self.ledger.encode().map_err(serde::ser::Error::custom)?;
        let pool = self
            .pool
            .encode(POOL_LIMIT)
            .map_err(serde::ser::Error::custom)?;
        let mut output = serializer.serialize_struct("CloudDeliveryV2", 11)?;
        output.serialize_field("version", &2u8)?;
        output.serialize_field("prior_private_schema", &self.prior_private_schema)?;
        output.serialize_field("ledger", &ledger)?;
        output.serialize_field("pool", &pool)?;
        output.serialize_field("baseline_generation", &self.baseline_generation)?;
        output.serialize_field("baseline", &self.baseline)?;
        output.serialize_field("tail", &self.tail)?;
        output.serialize_field("queue", &self.queue)?;
        output.serialize_field("policy_lineage", &self.policy_lineage)?;
        output.serialize_field("policy_revision", &self.policy_revision)?;
        output.serialize_field("transport_hint", &self.transport_hint)?;
        output.end()
    }
}
impl<'de> Deserialize<'de> for DeliveryState {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        let wire = Wire::deserialize(decoder)?;
        if wire.version != 2 {
            return Err(serde::de::Error::custom("unknown Cloud delivery version"));
        }
        let value = Self {
            prior_private_schema: wire.prior_private_schema,
            ledger: DeliveryLedgerV1::decode(&wire.ledger).map_err(serde::de::Error::custom)?,
            pool: PublicationPool::decode(&wire.pool, POOL_LIMIT)
                .map_err(serde::de::Error::custom)?,
            baseline_generation: wire.baseline_generation,
            baseline: wire.baseline,
            tail: wire.tail,
            queue: wire.queue,
            policy_lineage: wire.policy_lineage,
            policy_revision: wire.policy_revision,
            transport_hint: wire.transport_hint,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}
impl DeliveryState {
    pub(crate) fn ledger(&self) -> &DeliveryLedgerV1 {
        &self.ledger
    }
    /// Only the enclosing Store candidate may be mutated; commit validates it.
    pub(crate) fn ledger_mut(&mut self) -> &mut DeliveryLedgerV1 {
        &mut self.ledger
    }
    pub(crate) fn pool(&self) -> &PublicationPool {
        &self.pool
    }
    pub(crate) fn roots(&self) -> (u64, &[PublicationRef], &[PublicationRef]) {
        (self.baseline_generation, &self.baseline, &self.tail)
    }
    pub(crate) fn store_generation(&self) -> StoreGeneration {
        self.ledger.store_generation
    }
    pub(crate) fn policy_lineage(&self) -> u64 {
        self.policy_lineage
    }
    /// Owner-persisted signed descriptor; historical until verified against Current.
    pub(crate) fn transport_hint(&self) -> Option<&qos_peer_state::join::TransportHint> {
        self.transport_hint.as_ref()
    }
    /// Candidate-only replacement; the enclosing Store commit is the barrier.
    pub(crate) fn set_transport_hint(&mut self, hint: qos_peer_state::join::TransportHint) {
        self.transport_hint = Some(hint);
    }
    pub(super) fn validate(&self) -> Result<()> {
        super::validation::structure(self).map_err(super::validation::CheckError::into_legacy)
    }
}
impl State {
    pub(crate) fn private_schema_version(&self) -> Result<u32> {
        match (self.version, &self.delivery) {
            (4 | 5, None) => Ok(self.version),
            (VERSION, Some(delivery)) if matches!(delivery.prior_private_schema, 4 | 5) => {
                Ok(delivery.prior_private_schema)
            }
            _ => anyhow::bail!("delivery/private outer schema mismatch"),
        }
    }
    pub(crate) fn select_private_schema(&mut self) -> Result<()> {
        self.private_schema_version()?;
        if self.version == VERSION {
            self.delivery
                .as_mut()
                .expect("validated v6")
                .prior_private_schema = 5;
        } else {
            self.version = 5;
        }
        Ok(())
    }
}
