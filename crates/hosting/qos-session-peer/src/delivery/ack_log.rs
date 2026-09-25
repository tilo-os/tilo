//! Consumer ACK progress as small signed side records.
//!
//! An ACK changes one consumer's `acknowledged` cursor and nothing else a
//! reader or relay consumes. Instead of cloning, re-encoding, signing and
//! committing the whole aggregate (and minting a new owner generation and
//! committed view), the owner appends one signed record to the store's side
//! log (`store/side_log.rs`), barriers it, and only then advances the cursor
//! in its in-memory aggregate. Every later aggregate commit is built from that
//! aggregate and therefore folds the record.
//!
//! Recovery applies each genuine record to the recovered ledger only if it is
//! still exactly current: same room, same store generation, the consumer still
//! exists with the same id and authorization view, the reference is resolved
//! and names the ledger's replay entry with the same digest, and it advances
//! the cursor. That is the same predicate `DeliveryPolicyBasis::acknowledge`
//! enforces, and it makes records idempotent and monotone: a stale, folded,
//! duplicated or superseded (rotated store, changed policy, replaced
//! consumer) record is inert, so the log needs no binding to a checkpoint.
use super::*;
use serde::{Deserialize, Serialize};

const DOMAIN: &str = "qOS native session consumer ACK v1";

#[derive(Serialize, Deserialize)]
struct AckRecordV1 {
    version: u8,
    room: String,
    store_generation: StoreGeneration,
    device: [u8; 32],
    consumer: ConsumerId,
    authorization_view: Digest,
    committed: CommitRef,
}
#[derive(Serialize, Deserialize)]
struct SignedAckRecord {
    payload: Vec<u8>,
    signature: Vec<u8>,
}

fn digest(payload: &[u8]) -> blake3::Hash {
    blake3::Hasher::new_derive_key(DOMAIN)
        .update(payload)
        .finalize()
}

impl SessionOwner {
    /// Durably record an advanced ACK without an aggregate commit. Returns
    /// `Ok(false)` when the side log is unavailable or full (nothing written);
    /// the caller then commits the aggregate, which also folds the log.
    pub(super) fn record_ack_side(
        &mut self,
        device: [u8; 32],
        committed: &CommitRef,
    ) -> Result<bool> {
        if !self.store.side_log_enabled() {
            return Ok(false);
        }
        let ledger = self.delivery_ledger()?;
        let consumer = ledger
            .consumers
            .get(&device)
            .context("consumer disappeared")?;
        let record = AckRecordV1 {
            version: 1,
            room: self.aggregate.room.clone(),
            store_generation: ledger.store_generation,
            device,
            consumer: consumer.id,
            authorization_view: consumer.authorization_view,
            committed: committed.clone(),
        };
        let payload = postcard::to_stdvec(&record)?;
        let signature = self
            .identity
            .native
            .sign(digest(&payload).as_bytes())
            .to_bytes()
            .to_vec();
        let signed = postcard::to_stdvec(&SignedAckRecord { payload, signature })?;
        match self.store.append_side_record(&signed) {
            Ok(()) => {}
            Err(error) if error.is::<crate::store::SideLogFull>() => return Ok(false),
            Err(error) => return Err(error),
        }
        // Durable: advance the in-memory aggregate that later commits encode.
        // No owner generation or committed view changes.
        let PublicationStorage::V5 { delivery, .. } = &mut self.aggregate.publication else {
            unreachable!("delivery ledger checked above")
        };
        delivery
            .consumers
            .get_mut(&device)
            .context("consumer disappeared")?
            .acknowledged = Some(committed.clone());
        Ok(true)
    }
}

/// Apply recovered side records onto a freshly restored aggregate. Returns the
/// number of records that advanced a cursor.
pub(in crate::owner) fn apply_recovered(
    aggregate: &mut Aggregate,
    identity: &Identity,
    records: &[Vec<u8>],
) -> Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }
    let room = aggregate.room.clone();
    let PublicationStorage::V5 { delivery, .. } = &mut aggregate.publication else {
        // Only V5 owners write ACK records; a legacy aggregate cannot use them.
        return Ok(0);
    };
    let key = identity.native.verifying_key();
    let mut applied = 0;
    for bytes in records {
        let signed: SignedAckRecord =
            postcard::from_bytes(bytes).context("corrupt session ACK record")?;
        key.verify(
            digest(&signed.payload).as_bytes(),
            &Signature::from_slice(&signed.signature)?,
        )
        .context("session ACK record failed authentication")?;
        let record: AckRecordV1 =
            postcard::from_bytes(&signed.payload).context("corrupt session ACK record")?;
        ensure!(record.version == 1, "unknown session ACK record version");
        if !still_current(delivery, &room, &record) {
            continue;
        }
        let consumer = delivery.consumers.get_mut(&record.device).expect("checked");
        consumer.acknowledged = Some(record.committed);
        applied += 1;
    }
    delivery.validate()?;
    Ok(applied)
}

fn still_current(delivery: &DeliveryLedgerV1, room: &str, record: &AckRecordV1) -> bool {
    let Some(consumer) = delivery.consumers.get(&record.device) else {
        return false;
    };
    record.room == room
        && record.store_generation == delivery.store_generation
        && consumer.id == record.consumer
        && consumer.authorization_view == record.authorization_view
        && record.committed.sequence != 0
        && record.committed.sequence <= delivery.resolved_through
        && delivery
            .replay
            .get(&record.committed.sequence)
            .is_some_and(|entry| entry.digest == record.committed.digest)
        && consumer
            .acknowledged
            .as_ref()
            .is_none_or(|ack| ack.sequence < record.committed.sequence)
}

#[cfg(test)]
#[path = "ack_log_tests.rs"]
mod tests;
