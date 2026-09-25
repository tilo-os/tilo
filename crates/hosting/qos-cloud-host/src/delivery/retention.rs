//! Delivery retention and compaction for the Cloud Store.
//!
//! The replay ledger, publication pool and receipt index grow on the commit
//! path and have hard caps (`MAX_REFERENCES` replay entries/roots,
//! `POOL_LIMIT` payload bytes, `MAX_ADMISSIONS` receipts). Without retention
//! every workspace eventually refuses all commits.
//!
//! The frozen ledger contract keeps the replay index one contiguous chain from
//! its incarnation's first entry (`previous == None`); it has no certified
//! prefix anchor, so no prefix of one incarnation can be deleted. Retention
//! therefore uses the contract's explicit reset workflow:
//!
//! * **Receipts** (delivery-ID bindings) are released once the sending
//!   device's own consumer durably acknowledged a cut at or after the
//!   receipt's commit: that device installed the commit, so its journal entry
//!   is retired and cannot be retransmitted under that ID. Receipts of devices
//!   without such an acknowledgement (offline journals) are retained; at the
//!   cap the commit path keeps applying backpressure instead of evicting them.
//!   Retransmission after release is still deduplicated by the native
//!   operation identity (a certified duplicate), never re-admitted as an edit.
//! * **Snapshot + suffix rotation.** When the pool, roots, replay chain or
//!   relay queue reach half of a budget (history beyond the baseline is
//!   budgeted at `HISTORY_BYTES`/`HISTORY_COMMITS`, far below the hard caps,
//!   because every commit re-saves the whole state), the owner certifies a fresh
//!   checkpoint of the committed peer (`durable_bootstrap_messages`) and
//!   commits it as the first entry of a new store incarnation. Canonical
//!   state is exactly that baseline plus the suffix committed after it.
//!   Consumers keep their registration; their cursors name the previous
//!   incarnation and are answered `SnapshotRequired(StoreRotated)`, after
//!   which they install the snapshot and continue with the suffix. Browser
//!   journals are unaffected. Live snapshot pins defer rotation until a budget
//!   is critical (90%), since rotation ends those transfers.
//!
//! Both are ordinary atomic Store publications validated like every commit.
use super::{DeliveryState, schema};
use crate::{StatePeer, Store, baseline_publications, durable_bootstrap_messages, now_time};
use anyhow::{Context, Result, anyhow, ensure};
use qos_relay::delivery_v3::commit::{
    ConsumerState, DeliveryLedgerV1, MAX_ADMISSIONS, MAX_REFERENCES, PublicationPool, ReceiptKey,
    ReplayCommit, StoreGeneration,
};
use rand::RngCore;
use std::collections::{BTreeMap, BTreeSet};

/// Rotate once any budget is half used, so the fresh checkpoint still fits.
const DUE: (usize, usize) = (1, 2);
/// Retained publication history beyond the baseline that makes a rotation
/// due (lane T). The whole delivery state is re-saved on every Store commit
/// (as a JSON number array, ~3.3 bytes per payload byte), so history is kept
/// near the size of one checkpoint instead of half the 64 MiB pool limit:
/// a rotation is due once history reaches max(this, the baseline itself).
pub(crate) const HISTORY_BYTES: usize = 1024 * 1024;
/// Replay entries, roots and queued relay payloads that make a rotation due.
pub(crate) const HISTORY_COMMITS: usize = 1024;
/// Live snapshot pins defer rotation until this fraction of a budget.
const CRITICAL: (usize, usize) = (9, 10);

/// Observed budget use of one delivery state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pressure {
    pub(crate) pool_bytes: usize,
    /// Payload bytes of the current baseline roots alone.
    pub(crate) baseline_bytes: usize,
    pub(crate) roots: usize,
    pub(crate) replay: usize,
    pub(crate) receipts: usize,
    pub(crate) queue: usize,
    pub(crate) live_pins: usize,
}
fn over(value: usize, limit: usize, (numerator, denominator): (usize, usize)) -> bool {
    value.saturating_mul(denominator) >= limit.saturating_mul(numerator)
}
impl Pressure {
    pub(crate) fn of(delivery: &DeliveryState, now: u64) -> Result<Self> {
        let mut seen = BTreeSet::new();
        let mut pool_bytes = 0usize;
        let references = delivery
            .baseline
            .iter()
            .chain(&delivery.tail)
            .chain(
                delivery
                    .ledger
                    .replay
                    .values()
                    .flat_map(|e| e.publications.iter()),
            )
            .chain(
                delivery
                    .ledger
                    .pins
                    .values()
                    .flat_map(|p| p.publications.iter()),
            )
            .map(|reference| reference.payload)
            .chain(delivery.queue.iter().map(|queued| queued.payload));
        for digest in references {
            if seen.insert(digest) {
                pool_bytes = pool_bytes
                    .checked_add(delivery.pool.resolve(&digest)?.len())
                    .context("pool size overflow")?;
            }
        }
        let mut baseline = BTreeSet::new();
        let mut baseline_bytes = 0usize;
        for reference in &delivery.baseline {
            if baseline.insert(reference.payload) {
                baseline_bytes = baseline_bytes
                    .checked_add(delivery.pool.resolve(&reference.payload)?.len())
                    .context("pool size overflow")?;
            }
        }
        Ok(Self {
            pool_bytes,
            baseline_bytes,
            roots: delivery.baseline.len() + delivery.tail.len(),
            replay: delivery.ledger.replay.len(),
            receipts: delivery.ledger.admissions.len(),
            queue: delivery.queue.len(),
            live_pins: delivery
                .ledger
                .pins
                .values()
                .filter(|pin| pin.expires_at_ns > now)
                .count(),
        })
    }
    fn budgets(&self, fraction: (usize, usize)) -> bool {
        // Pool pressure counts only when history beyond the baseline holds at
        // least half of it: a rotation would otherwise not free space and a
        // large workspace would rotate (resetting every consumer) repeatedly.
        // The history budget is likewise never below the baseline itself.
        let history = self.pool_bytes.saturating_sub(self.baseline_bytes);
        let history_budget = HISTORY_BYTES.max(self.baseline_bytes).saturating_mul(2);
        let commits = MAX_REFERENCES.min(HISTORY_COMMITS * 2);
        (over(self.pool_bytes, schema::POOL_LIMIT, fraction)
            && self.pool_bytes >= self.baseline_bytes.saturating_mul(2))
            || over(history, history_budget, fraction)
            || over(self.roots, commits, fraction)
            || over(self.replay, commits, fraction)
            || over(self.queue, commits, fraction)
    }
    /// Whether a snapshot + suffix rotation is due now.
    pub(crate) fn rotation_due(&self) -> bool {
        if self.live_pins > 0 {
            self.budgets(CRITICAL)
        } else {
            self.budgets(DUE)
        }
    }
    pub(crate) fn receipts_due(&self) -> bool {
        over(self.receipts, MAX_ADMISSIONS, DUE)
    }
}
fn acknowledged(consumers: &BTreeMap<[u8; 32], ConsumerState>, device: &[u8; 32]) -> Option<u64> {
    consumers
        .get(device)
        .and_then(|consumer| consumer.acknowledged.as_ref())
        .map(|cut| cut.sequence)
}
/// Release receipts whose sending device durably acknowledged their commit.
fn prune_receipts(ledger: &mut DeliveryLedgerV1) -> usize {
    let consumers = &ledger.consumers;
    let before = ledger.admissions.len();
    ledger
        .admissions
        .retain(|ReceiptKey { device, .. }, record| {
            acknowledged(consumers, device)
                .is_none_or(|sequence| sequence < record.committed.sequence)
        });
    before - ledger.admissions.len()
}
fn prunable_receipts(ledger: &DeliveryLedgerV1) -> usize {
    ledger
        .admissions
        .iter()
        .filter(|(key, record)| {
            acknowledged(&ledger.consumers, &key.device)
                .is_some_and(|sequence| sequence >= record.committed.sequence)
        })
        .count()
}

/// What one retention commit changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RetentionReport {
    pub(crate) rotated: Option<StoreGeneration>,
    pub(crate) receipts_released: usize,
    pub(crate) before: Pressure,
    pub(crate) after: Pressure,
}

impl Store {
    /// Delivery budget use of the current Store state.
    pub(crate) fn delivery_pressure(&self, now: u64) -> Result<Pressure> {
        let delivery = self
            .state
            .delivery
            .as_ref()
            .ok_or_else(|| anyhow!("delivery not selected"))?;
        Pressure::of(delivery, now)
    }
    /// Commit due retention: a snapshot + suffix rotation when a budget needs
    /// it, otherwise receipt release when the receipt budget needs it.
    /// Returns `None` without committing when nothing is due.
    #[cfg(test)]
    pub(crate) fn retain_delivery(&mut self, now: u64) -> Result<Option<RetentionReport>> {
        self.retain_delivery_with(now, false)
    }
    pub(crate) fn retain_delivery_with(
        &mut self,
        now: u64,
        force_rotation: bool,
    ) -> Result<Option<RetentionReport>> {
        self.ensure_reconciled()?;
        let now = self.policy_now_or(now)?;
        let delivery = self
            .state
            .delivery
            .as_ref()
            .ok_or_else(|| anyhow!("delivery not selected"))?;
        let before = Pressure::of(delivery, now)?;
        if force_rotation || before.rotation_due() {
            let (incarnation, receipts_released) = self.rotate_delivery(now)?;
            let after = self.delivery_pressure(now)?;
            return Ok(Some(RetentionReport {
                rotated: Some(incarnation),
                receipts_released,
                before,
                after,
            }));
        }
        if !before.receipts_due() || prunable_receipts(&delivery.ledger) == 0 {
            return Ok(None);
        }
        let mut next = self.state.clone();
        let released = prune_receipts(&mut next.delivery.as_mut().expect("selected").ledger);
        self.commit(next)?;
        Ok(Some(RetentionReport {
            rotated: None,
            receipts_released: released,
            before,
            after: self.delivery_pressure(now)?,
        }))
    }
    /// Certify a checkpoint of the committed peer as the first entry of a new
    /// store incarnation. Mirrors `activate_delivery_storage`, preserving
    /// consumer registrations, the descriptor, policy lineage and the receipts
    /// of devices that have not acknowledged them.
    fn rotate_delivery(&mut self, now: u64) -> Result<(StoreGeneration, usize)> {
        let old = self
            .state
            .delivery
            .as_ref()
            .ok_or_else(|| anyhow!("delivery not selected"))?;
        let generation = self.next_generation()?;
        let (_before, mut peer) = self.peer_transition_at(now)?;
        let messages =
            durable_bootstrap_messages(&mut peer, &self.key(), now_time(now), generation)?;
        let checkpoint = baseline_publications(messages)?;
        peer.validate_media_governance_for_publication()
            .map_err(anyhow::Error::msg)?;
        let policy = super::revision(&peer)?;
        let mut incarnation = [0; 16];
        rand::rngs::OsRng.try_fill_bytes(&mut incarnation)?;
        ensure!(
            incarnation != [0; 16] && incarnation != old.ledger.store_generation,
            "delivery entropy unavailable"
        );
        let mut ledger = old.ledger.clone();
        let released = prune_receipts(&mut ledger);
        for consumer in ledger.consumers.values_mut() {
            // Acknowledgements name the previous incarnation's chain.
            consumer.acknowledged = None;
        }
        let mut delivery = DeliveryState {
            prior_private_schema: old.prior_private_schema,
            ledger: DeliveryLedgerV1 {
                version: 1,
                store_generation: incarnation,
                resolved_through: generation,
                replay: Default::default(),
                admissions: ledger.admissions,
                consumers: ledger.consumers,
                pins: Default::default(),
            },
            pool: PublicationPool::new(schema::POOL_LIMIT)?,
            baseline_generation: generation,
            baseline: vec![],
            tail: vec![],
            queue: vec![],
            policy_lineage: if policy != old.policy_revision {
                old.policy_lineage
                    .checked_add(1)
                    .context("policy lineage exhausted")?
            } else {
                old.policy_lineage
            },
            policy_revision: policy,
            transport_hint: old.transport_hint.clone(),
        };
        delivery.baseline = super::intern(&mut delivery, &checkpoint)?;
        ensure!(!delivery.baseline.is_empty(), "empty certified checkpoint");
        let empty = StatePeer::new(
            &self.state.room,
            peer.authority().clone(),
            vec![peer.authority().clone()],
            qos_user_identity::RevocationList::new(),
            now_time(now),
        )
        .map_err(anyhow::Error::msg)?;
        let mut replay = ReplayCommit {
            owner_generation: generation,
            previous: None,
            digest: [0; 32],
            publications: delivery.baseline.clone(),
            admitted_operations: peer
                .admissions_since(&empty)?
                .into_iter()
                .map(|op| op.operation_id())
                .collect(),
            committed_at_ns: now,
        };
        replay.digest = super::replay_digest(&self.state, incarnation, &replay)?;
        delivery.ledger.replay.insert(generation, replay);
        let mut next = self.state.clone();
        next.generation = generation;
        next.outbox.clear();
        next.bound.as_mut().context("workspace not enrolled")?.peer = peer
            .durable()
            .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
        next.delivery = Some(delivery);
        super::validate(&next, now)?;
        self.publish_prepared(next)?;
        Ok((incarnation, released))
    }
}
/// Digest set referenced by the delivery state; used by tests.
#[cfg(test)]
fn referenced(delivery: &DeliveryState) -> BTreeSet<qos_relay::delivery_v3::commit::Digest> {
    delivery
        .baseline
        .iter()
        .chain(&delivery.tail)
        .chain(
            delivery
                .ledger
                .replay
                .values()
                .flat_map(|e| e.publications.iter()),
        )
        .chain(
            delivery
                .ledger
                .pins
                .values()
                .flat_map(|p| p.publications.iter()),
        )
        .map(|r| r.payload)
        .chain(delivery.queue.iter().map(|q| q.payload))
        .collect()
}

#[cfg(test)]
#[path = "retention/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "retention/growth_tests.rs"]
mod growth_tests;
