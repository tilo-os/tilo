//! Native delivery retention: incremental publication, certified checkpoints,
//! receipt release and snapshot + suffix store rotation.
//!
//! Modeled on the Cloud Store (`qos-cloud-host/src/delivery/retention.rs`):
//!
//! * **Incremental commits.** An uploaded document item is admitted by the
//!   exact native path and then published as its author's original signed
//!   bytes (plus the governed room delta, if admission changed the room).
//!   Consumers admit it under that original signer through the retained
//!   native publication carrier. No host copy of the document is minted.
//! * **Certified checkpoints.** Coverage (the governed receipt that retires a
//!   client outbox) still comes only from a host-signed snapshot. A checkpoint
//!   replaces the publication baseline when the tail reaches a count/byte
//!   budget, after a short idle period, or when the oldest uncovered commit
//!   reaches a maximum age (well below the client's 300 s freshness window).
//! * **Receipt release.** Delivery-ID bindings of a device whose own consumer
//!   durably acknowledged a cut at or after the receipt's commit are released
//!   (that device installed the commit). Receipts of devices without such an
//!   acknowledgement (offline journals) are retained. A released retransmission
//!   is still deduplicated by native operation identity.
//! * **Snapshot + suffix rotation.** The frozen ledger contract keeps one
//!   contiguous chain per store incarnation (no certified prefix anchor), so
//!   history is released by committing a fresh certified checkpoint as the
//!   first entry of a new incarnation. Consumers keep their registration;
//!   cursors naming the old incarnation are answered
//!   `SnapshotRequired(StoreRotated)` and recover with snapshot + suffix.
//!   A checkpoint larger than one replay page is installed as a rotation:
//!   every consumer would have to snapshot it anyway, and the rotation frees
//!   all earlier history in the same commit.
//!
//! Every retention step is an ordinary signed owner commit, crash consistent
//! through the store journal/checkpoint like any other commit. ACK side
//! records name their store incarnation and become inert after a rotation.
use super::delivery::error::{self, OwnerDataFailureKind as DataFailure};
use super::*;
use std::collections::BTreeSet;
use tilo_relay::delivery_v3::commit::{
    DeliveryLedgerV1, Digest, MAX_ADMISSIONS, MAX_REFERENCES, PublicationPool, PublicationRef,
    ReceiptKey,
};
use ui::automerge::transaction::Transactable as _;

/// Tail publications after which a checkpoint is due (inline cap is 64).
pub(crate) const CHECKPOINT_TAIL_ENTRIES: usize = 32;
/// Tail payload bytes after which a checkpoint is due (inline cap is 4 MiB).
pub(crate) const CHECKPOINT_TAIL_BYTES: usize = 256 * 1024;
/// While a snapshot download is active, checkpoints wait up to these bounds.
const DEFERRED_TAIL_ENTRIES: usize = 48;
const DEFERRED_TAIL_BYTES: usize = 2 * 1024 * 1024;
/// Quiet period after the last commit before uncovered commits are covered.
pub(crate) const CHECKPOINT_IDLE_NS: u64 = 3_000_000_000;
/// Oldest uncovered commit age that forces a checkpoint during activity.
pub(crate) const CHECKPOINT_MAX_AGE_NS: u64 = 30_000_000_000;
/// A checkpoint whose publications exceed this cannot be one replay page.
pub(crate) const ROTATE_CHECKPOINT_BYTES: usize =
    tilo_relay::delivery_v3::capabilities::PAGE_BYTES - 64 * 1024;
/// History (retained replay payloads outside the current roots) that makes a
/// rotation due, unless twice the current roots is larger. Kept small: the
/// aggregate is re-encoded on every commit and scales the per-turn byte
/// reservations, while a rotation only costs each consumer one snapshot of
/// the (certified) roots.
pub(crate) const HISTORY_BYTES: usize = 1024 * 1024;
/// Payload budget of one owner pool (the aggregate cap).
const POOL_LIMIT: usize = crate::store::MAX_AGGREGATE_BYTES;
/// Rotation receipts list every admitted operation up to this bound.
const ROTATION_OPERATIONS: usize = MAX_REFERENCES / 2;

fn over(value: usize, limit: usize, (numerator, denominator): (usize, usize)) -> bool {
    value.saturating_mul(denominator) >= limit.saturating_mul(numerator)
}
const DUE: (usize, usize) = (1, 2);
const CRITICAL: (usize, usize) = (9, 10);

/// Room governance deltas are `SyncResp` (postcard variant 1); every other
/// retained `Delta` publication is an author's incremental document item.
fn is_author_delta(bytes: &[u8], kind: Retention) -> bool {
    kind == Retention::Delta && bytes.first() != Some(&1)
}

/// Observed budget use of the committed delivery storage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pressure {
    /// All payload bytes owned by the pool, referenced or not.
    pub(crate) pool_bytes: usize,
    pub(crate) pool_entries: usize,
    /// Unique payload bytes of the current baseline plus tail.
    pub(crate) root_bytes: usize,
    /// Unique replay payload bytes outside the current roots.
    pub(crate) history_bytes: usize,
    pub(crate) tail_entries: usize,
    pub(crate) tail_bytes: usize,
    /// Tail publications that no certified checkpoint covers yet.
    pub(crate) author_deltas: usize,
    pub(crate) replay: usize,
    pub(crate) receipts: usize,
    pub(crate) prunable_receipts: usize,
    pub(crate) last_commit_ns: u64,
    pub(crate) first_uncovered_ns: Option<u64>,
}
impl Pressure {
    fn rotation(&self, fraction: (usize, usize)) -> bool {
        // Pool pressure counts only when history holds at least as much as
        // the roots: a rotation would otherwise not free space.
        (over(self.pool_bytes, POOL_LIMIT, fraction)
            && self.pool_bytes >= self.root_bytes.saturating_mul(2))
            || over(self.pool_entries, MAX_REFERENCES, fraction)
            || over(self.replay, MAX_REFERENCES, fraction)
    }
    /// Whether a snapshot + suffix rotation is due now.
    pub(crate) fn rotation_due(&self, busy: bool) -> bool {
        let history = self.history_bytes >= HISTORY_BYTES.max(self.root_bytes.saturating_mul(2));
        self.rotation(CRITICAL) || (!busy && (history || self.rotation(DUE)))
    }
    /// Whether a certified checkpoint of the tail is due now.
    pub(crate) fn checkpoint_due(&self, now_ns: u64, busy: bool) -> bool {
        if self.tail_entries == 0 {
            return false;
        }
        if self.tail_entries >= DEFERRED_TAIL_ENTRIES || self.tail_bytes >= DEFERRED_TAIL_BYTES {
            return true;
        }
        if busy {
            return false;
        }
        if self.tail_entries >= CHECKPOINT_TAIL_ENTRIES || self.tail_bytes >= CHECKPOINT_TAIL_BYTES
        {
            return true;
        }
        self.author_deltas > 0
            && (now_ns.saturating_sub(self.last_commit_ns) >= CHECKPOINT_IDLE_NS
                || self
                    .first_uncovered_ns
                    .is_some_and(|first| now_ns.saturating_sub(first) >= CHECKPOINT_MAX_AGE_NS))
    }
    pub(crate) fn receipts_due(&self) -> bool {
        over(self.receipts, MAX_ADMISSIONS, DUE) && self.prunable_receipts > 0
    }
}

/// The retention commit a maintenance turn should make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetentionAction {
    Rotate,
    Checkpoint,
    ReleaseReceipts,
}
/// What one retention commit changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RetentionReport {
    pub(crate) action: RetentionAction,
    /// Whether the commit rotated the store incarnation.
    pub(crate) rotated: bool,
    pub(crate) receipts_released: usize,
    pub(crate) before: Pressure,
    pub(crate) after: Pressure,
}

fn acknowledged(ledger: &DeliveryLedgerV1, device: &[u8; 32]) -> Option<u64> {
    ledger
        .consumers
        .get(device)
        .and_then(|consumer| consumer.acknowledged.as_ref())
        .map(|cut| cut.sequence)
}
fn releasable(ledger: &DeliveryLedgerV1, key: &ReceiptKey, sequence: u64) -> bool {
    acknowledged(ledger, &key.device).is_some_and(|ack| ack >= sequence)
}
/// Release receipts whose sending device durably acknowledged their commit.
fn prune_receipts(ledger: &mut DeliveryLedgerV1) -> usize {
    let before = ledger.admissions.len();
    let keep = ledger
        .admissions
        .iter()
        .filter(|(key, record)| !releasable(ledger, key, record.committed.sequence))
        .map(|(key, _)| key.clone())
        .collect::<BTreeSet<_>>();
    ledger.admissions.retain(|key, _| keep.contains(key));
    before - ledger.admissions.len()
}
fn now_ns(now: SystemTime) -> Result<u64> {
    Ok(now
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos()
        .try_into()?)
}
/// Every operation the native peer admitted, in canonical order.
pub(super) fn all_admitted_operations(peer: &StatePeer) -> Result<Vec<Digest>> {
    let empty = StatePeer::new(
        peer.room(),
        peer.authority().clone(),
        vec![peer.authority().clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .map_err(anyhow::Error::msg)?;
    let mut operations = peer
        .admissions_since(&empty)?
        .into_iter()
        .map(|op| op.operation_id())
        .collect::<Vec<_>>();
    operations.sort_unstable();
    operations.dedup();
    Ok(operations)
}

impl SessionOwner {
    /// Delivery budget use of the committed storage. Cheap: metadata and
    /// payload lengths only, no payload decode.
    pub(crate) fn delivery_pressure(&self) -> Result<Pressure> {
        let PublicationStorage::V5 {
            pool,
            roots,
            delivery,
            ..
        } = &self.aggregate.publication
        else {
            bail!("delivery storage not selected")
        };
        let mut pressure = Pressure {
            pool_entries: pool.len(),
            replay: delivery.replay.len(),
            receipts: delivery.admissions.len(),
            tail_entries: roots.tail.len(),
            ..Pressure::default()
        };
        pressure.pool_bytes = pool.retained_payload_capacity()?;
        let mut current = BTreeSet::new();
        for reference in roots.baseline.iter().chain(&roots.tail) {
            if current.insert(reference.payload) {
                pressure.root_bytes += pool.resolve(&reference.payload)?.len();
            }
        }
        let mut seen = BTreeSet::new();
        for reference in &roots.tail {
            let bytes = pool.resolve(&reference.payload)?;
            pressure.tail_bytes += bytes.len();
            if is_author_delta(bytes, reference.kind) {
                pressure.author_deltas += 1;
            }
        }
        for reference in delivery.replay.values().flat_map(|e| e.publications.iter()) {
            if !current.contains(&reference.payload) && seen.insert(reference.payload) {
                pressure.history_bytes += pool.resolve(&reference.payload)?.len();
            }
        }
        pressure.prunable_receipts = delivery
            .admissions
            .iter()
            .filter(|(key, record)| releasable(delivery, key, record.committed.sequence))
            .count();
        pressure.last_commit_ns = delivery
            .replay
            .last_key_value()
            .map_or(0, |(_, entry)| entry.committed_at_ns);
        pressure.first_uncovered_ns = delivery
            .replay
            .range(roots.baseline_generation.saturating_add(1)..)
            .next()
            .map(|(_, entry)| entry.committed_at_ns);
        Ok(pressure)
    }
    /// The retention commit due now, if any. `busy` reports an active client
    /// snapshot download: optional checkpoints and rotations wait for it.
    pub(crate) fn delivery_retention_due(
        &self,
        now: SystemTime,
        busy: bool,
    ) -> Result<Option<RetentionAction>> {
        if !matches!(self.aggregate.publication, PublicationStorage::V5 { .. })
            || self.aggregate.is_read_only()
            || !self.peer.admission_v5_selected()
            || self.store.is_poisoned()
            || self.peer.incomplete_reason().is_some()
        {
            return Ok(None);
        }
        let pressure = self.delivery_pressure()?;
        let now = now_ns(now)?;
        Ok(if pressure.rotation_due(busy) {
            Some(RetentionAction::Rotate)
        } else if pressure.checkpoint_due(now, busy) {
            Some(RetentionAction::Checkpoint)
        } else if pressure.receipts_due() {
            Some(RetentionAction::ReleaseReceipts)
        } else {
            None
        })
    }
    /// Commit the retention step due now. Returns `None` without committing
    /// when nothing is due.
    pub(crate) fn maintain_delivery(
        &mut self,
        now: SystemTime,
        busy: bool,
    ) -> Result<Option<RetentionReport>> {
        let Some(action) = self.delivery_retention_due(now, busy)? else {
            return Ok(None);
        };
        self.retain_delivery(action, now).map(Some)
    }
    /// Commit one explicit retention step.
    pub(crate) fn retain_delivery(
        &mut self,
        action: RetentionAction,
        now: SystemTime,
    ) -> Result<RetentionReport> {
        self.require_owner_resource_scope()?;
        self.require_delivery()?;
        let before = self.delivery_pressure()?;
        let receipts = before.receipts;
        let rotated = match action {
            RetentionAction::Rotate => {
                self.rotate_delivery(now)?;
                true
            }
            RetentionAction::Checkpoint => self.checkpoint_delivery(now)?,
            RetentionAction::ReleaseReceipts => {
                let mut next = self.aggregate.clone();
                let PublicationStorage::V5 { delivery, .. } = &mut next.publication else {
                    unreachable!()
                };
                prune_receipts(delivery);
                self.commit(next, self.peer.clone())?;
                false
            }
        };
        let after = self.delivery_pressure()?;
        Ok(RetentionReport {
            action,
            rotated,
            receipts_released: receipts.saturating_sub(after.receipts),
            before,
            after,
        })
    }
    /// Certify the committed peer as a new publication baseline. A checkpoint
    /// too large for one replay page becomes a rotation. Returns whether the
    /// store rotated.
    fn checkpoint_delivery(&mut self, now: SystemTime) -> Result<bool> {
        let generation = self
            .aggregate
            .generation
            .checked_add(1)
            .context("session generation exhausted")?;
        let mut candidate = self.peer.clone();
        let messages = self.prepare_certified_bootstrap_at(&mut candidate, generation, now)?;
        let cap = self.serialized_commit_cap()?;
        let bytes = messages.iter().try_fold(0usize, |sum, message| {
            sum.checked_add(schema::counted(message, cap)?)
                .context("checkpoint bytes overflow")
        })?;
        if bytes > ROTATE_CHECKPOINT_BYTES {
            self.rotate_with(candidate, &messages)?;
            return Ok(true);
        }
        let mut next = self.aggregate.clone();
        self.replace_publication(&mut next, &messages)?;
        self.commit(next, candidate)?;
        Ok(false)
    }
    /// Commit a certified checkpoint as the first entry of a new store
    /// incarnation, preserving consumer registrations and the receipts of
    /// devices that have not acknowledged them.
    pub(crate) fn rotate_delivery(&mut self, now: SystemTime) -> Result<CommitReceipt> {
        let generation = self
            .aggregate
            .generation
            .checked_add(1)
            .context("session generation exhausted")?;
        let mut candidate = self.peer.clone();
        let messages = self.prepare_certified_bootstrap_at(&mut candidate, generation, now)?;
        self.rotate_with(candidate, &messages)
    }
    fn rotate_with(
        &mut self,
        candidate: StatePeer,
        messages: &[NeighbourMsg],
    ) -> Result<CommitReceipt> {
        error::require(
            candidate.incomplete_reason().is_none(),
            DataFailure::Unavailable,
            "incomplete native state cannot rotate delivery",
        )?;
        let PublicationStorage::V5 {
            delivery: old,
            transport_hint,
            ..
        } = &self.aggregate.publication
        else {
            bail!("delivery storage not selected")
        };
        let incarnation = loop {
            let id = *uuid::Uuid::new_v4().as_bytes();
            if id != [0; 16] && id != old.store_generation {
                break id;
            }
        };
        let mut ledger = old.clone();
        prune_receipts(&mut ledger);
        for consumer in ledger.consumers.values_mut() {
            // Acknowledgements name the previous incarnation's chain.
            consumer.acknowledged = None;
        }
        let mut next = self.aggregate.clone();
        next.publication = PublicationStorage::V5 {
            pool: PublicationPool::new(POOL_LIMIT)?,
            roots: PublicationRootsV1 {
                baseline_generation: self
                    .aggregate
                    .generation
                    .checked_add(1)
                    .context("session generation exhausted")?,
                baseline: vec![],
                tail: vec![],
            },
            delivery: DeliveryLedgerV1 {
                version: 1,
                store_generation: incarnation,
                resolved_through: self.aggregate.generation,
                replay: Default::default(),
                admissions: ledger.admissions,
                consumers: ledger.consumers,
                pins: Default::default(),
            },
            transport_hint: transport_hint.clone(),
        };
        self.replace_publication(&mut next, messages)?;
        self.commit_delivery(next, candidate, None, CommitMode::DeliveryRotation)
    }
    /// Publish an exactly admitted upload as its authors' original signed
    /// items (plus the governed room delta). Returns `false`, leaving `next`
    /// untouched, when the caller must use the certified snapshot path: the
    /// admission changed more than these items, the state is incomplete, or
    /// the tail would exceed its inline caps.
    pub(super) fn append_incremental_publication(
        &self,
        next: &mut Aggregate,
        candidate: &mut StatePeer,
        items: &[&[u8]],
        operations: &[Digest],
        now: SystemTime,
    ) -> Result<bool> {
        if items.is_empty() || candidate.incomplete_reason().is_some() {
            return Ok(false);
        }
        let PublicationStorage::V5 { roots, .. } = &next.publication else {
            return Ok(false);
        };
        let tail_len = roots.tail.len();
        let mut expected = operations.to_vec();
        expected.sort_unstable();
        expected.dedup();
        let mut admitted = candidate
            .admissions_since(&self.peer)?
            .iter()
            .map(|op| op.operation_id())
            .collect::<Vec<_>>();
        admitted.sort_unstable();
        admitted.dedup();
        if admitted != expected {
            return Ok(false);
        }
        let mut prepared = candidate.clone();
        media::prepare_media_governance(&mut prepared, &self.identity.native, now)?;
        let before_room = self
            .peer
            .admitted_room()
            .context("missing prior room governance")?;
        let before_heads = before_room.base_heads();
        let room = prepared
            .admitted_room()
            .context("missing room governance")?;
        let mut encoded = Vec::with_capacity(items.len() + 1);
        if room.base_heads() != before_heads {
            let mut room = room.clone();
            if before_heads
                .iter()
                .any(|head| room.get_change_by_hash(head).is_none())
            {
                return Ok(false);
            }
            let changes = room
                .get_changes(&before_heads)
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect::<Vec<_>>();
            if !changes.is_empty() {
                let message = NeighbourMsg::SyncResp {
                    changes,
                    room: prepared.room().to_owned(),
                };
                encoded.push((
                    schema::encode_bounded(&message, self.serialized_commit_cap()?)?,
                    Retention::Delta,
                ));
            }
        }
        for item in items {
            let message: NeighbourMsg = postcard::from_bytes(item)?;
            if !matches!(message, NeighbourMsg::AppDocument { .. }) {
                return Ok(false);
            }
            let Some((_, kind)) = retention(&message, false) else {
                return Ok(false);
            };
            encoded.push((item.to_vec(), kind));
        }
        let tail_bytes = next
            .outgoing()
            .iter()
            .skip(next.baseline().1)
            .map(|p| p.bytes.len())
            .sum::<usize>();
        if tail_len.saturating_add(encoded.len()) >= MAX_PUBLICATION_TAIL_ENTRIES
            || tail_bytes.saturating_add(encoded.iter().map(|(b, _)| b.len()).sum::<usize>())
                >= MAX_PUBLICATION_TAIL_BYTES
        {
            return Ok(false);
        }
        let PublicationStorage::V5 { pool, roots, .. } = &mut next.publication else {
            unreachable!()
        };
        for (bytes, kind) in &encoded {
            roots.tail.push(PublicationRef {
                payload: pool.intern(bytes)?,
                kind: *kind,
            });
        }
        *candidate = prepared;
        Ok(true)
    }
    /// Incremental publication for an exactly admitted upload, or the
    /// certified snapshot path when it does not apply.
    pub(super) fn append_delivery_publication(
        &self,
        next: &mut Aggregate,
        candidate: &mut StatePeer,
        items: &[&[u8]],
        operations: &[Digest],
    ) -> Result<Option<PendingLivePublicationTransition>> {
        let now = publication_time();
        if self.append_incremental_publication(next, candidate, items, operations, now)? {
            return Ok(None);
        }
        self.append_mutation_publication_at(next, candidate, now)
    }
}
