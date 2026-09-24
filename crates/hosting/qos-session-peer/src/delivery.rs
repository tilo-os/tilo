//! Native delivery state shares the signed SessionOwner barrier.
use super::*;
use anyhow::ensure;
use error::{OwnerCapacityKind as DataCapacity, OwnerDataFailureKind as DataFailure};
use qos_peer_state::{InputDisposition, MissingPrerequisite};
use tilo_relay::delivery_v3::commit::*;

/// First local owner slice accepts complete small items; no chunk staging owner exists yet.
pub(crate) const MAX_DELIVERY_ITEM_BYTES: usize = 1024 * 1024;
/// A complete replay group that cannot be one page is answered with the
/// contract's reset: the consumer installs a snapshot (whose chunked transfer
/// has no page bound) and continues with the suffix. Never a denial.
pub(crate) fn oversized_replay_group(diagnostic: &'static str) -> anyhow::Error {
    error::failure(
        DataFailure::Reset(tilo_relay::delivery_v3::state::ResetReason::HistoryUnavailable),
        anyhow::anyhow!(diagnostic),
    )
}
pub(crate) struct CompleteStateItem<'a> {
    pub(crate) delivery_id: DeliveryId,
    pub(crate) canonical: &'a [u8],
}
/// Response data only. Native signed coverage remains the sender-retirement proof.
#[derive(Debug)]
pub(crate) enum AdmissionOutcome {
    Durable(AdmissionRecord),
    RetryLater(MissingPrerequisite),
    MetadataOnly,
}
/// Receipts bound by one commit: a single item, or every item of one
/// multi-item group (all referencing the same replay group).
pub(super) struct DeliveryBinding {
    items: Vec<BindingItem>,
}
struct BindingItem {
    key: ReceiptKey,
    digest: Digest,
    operation: Digest,
    result: DurableAdmissionKind,
    existing: Option<CommitRef>,
}

fn reference(sequence: u64, entry: &ReplayCommit) -> CommitRef {
    CommitRef {
        sequence,
        digest: entry.digest,
    }
}
fn replay_digest(
    store: StoreGeneration,
    room: &str,
    workspace: u64,
    entry: &ReplayCommit,
) -> Result<Digest> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-delivery-v3-native-commit-v1\0");
    Ok(postcard::serialize_with_flavor(
        &(
            store,
            room,
            workspace,
            entry.owner_generation,
            &entry.previous,
            &entry.publications,
            &entry.admitted_operations,
        ),
        ReplayHash(hasher),
    )?)
}
// Preserve the exact postcard digest bytes without an encoded scratch Vec.
struct ReplayHash(blake3::Hasher);
impl postcard::ser_flavors::Flavor for ReplayHash {
    type Output = Digest;
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.0.update(&[byte]);
        Ok(())
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.0.update(bytes);
        Ok(())
    }
    fn finalize(self) -> postcard::Result<Digest> {
        Ok(*self.0.finalize().as_bytes())
    }
}
pub(super) fn validate_replay(
    aggregate: &Aggregate,
    workspace: u64,
    peer: &StatePeer,
) -> Result<()> {
    let PublicationStorage::V5 { delivery, .. } = &aggregate.publication else {
        return Ok(());
    };
    // Reader-only fixtures predate actual native exact admission and are inert.
    if aggregate.is_read_only() {
        return Ok(());
    }
    delivery.validate()?;
    let empty = StatePeer::new(
        peer.room(),
        peer.authority().clone(),
        vec![peer.authority().clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .map_err(anyhow::Error::msg)?;
    let exact = peer
        .admissions_since(&empty)?
        .into_iter()
        .map(|op| op.operation_id())
        .collect::<std::collections::BTreeSet<_>>();
    for entry in delivery.replay.values() {
        ensure!(
            entry
                .admitted_operations
                .iter()
                .all(|id| exact.contains(id)),
            "replay metadata lacks native exact admission"
        );
        ensure!(
            entry.digest
                == replay_digest(delivery.store_generation, &aggregate.room, workspace, entry)?,
            "native replay digest mismatch"
        );
    }
    for record in delivery.admissions.values() {
        let entry = delivery
            .replay
            .get(&record.committed.sequence)
            .context("delivery receipt lacks replay evidence")?;
        ensure!(
            entry.digest == record.committed.digest
                && record
                    .operations
                    .iter()
                    .all(|id| entry.admitted_operations.contains(id)),
            "delivery receipt differs from exact replay evidence"
        );
    }
    Ok(())
}

impl SessionOwner {
    /// Explicit local rollout transaction. Ordinary reads, opens and commands never select V5.
    pub fn activate_delivery(&mut self) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        ensure!(
            !self.store.is_poisoned(),
            "reopen owner after uncertain durability"
        );
        if matches!(self.aggregate.publication, PublicationStorage::V5 { .. })
            && self.peer.admission_v5_selected()
            && !self.aggregate.is_read_only()
        {
            return self.flush();
        }
        ensure!(
            self.peer.incomplete_reason().is_none(),
            "incomplete native state cannot activate delivery"
        );
        let mut candidate = self.peer.clone();
        candidate.select_admission_v5()?;
        let generation = self
            .aggregate
            .generation
            .checked_add(1)
            .context("session generation exhausted")?;
        let messages = self.prepare_certified_bootstrap(&mut candidate, generation)?;
        let mut next = self.aggregate.clone();
        let mut pool = PublicationPool::new(crate::store::MAX_AGGREGATE_BYTES)?;
        // Preserve imported queue bytes, including evidence no longer in the new baseline.
        for p in self.aggregate.outgoing().iter() {
            pool.intern(p.bytes)?;
        }
        if matches!(next.publication, PublicationStorage::V5 { .. }) {
            bail!("reader-only V5 requires a separately reviewed imported-ledger activation");
        }
        next.publication = PublicationStorage::V5 {
            pool,
            roots: PublicationRootsV1 {
                baseline_generation: generation,
                baseline: vec![],
                tail: vec![],
            },
            delivery: DeliveryLedgerV1 {
                version: 1,
                store_generation: *uuid::Uuid::new_v4().as_bytes(),
                resolved_through: self.aggregate.generation,
                replay: Default::default(),
                admissions: Default::default(),
                consumers: Default::default(),
                pins: Default::default(),
            },
            transport_hint: None,
        };
        self.replace_publication(&mut next, &messages)?;
        self.commit_delivery(next, candidate, None, CommitMode::DeliveryActivation)
    }
    /// The operator must fence the old writer before importing/restoring this store.
    /// Rebuild transport digests in the new incarnation, preserving native evidence.
    pub fn rotate_store_generation_for_restore(&mut self) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        self.require_delivery()?;
        ensure!(
            self.peer.incomplete_reason().is_none(),
            "incomplete native restore baseline"
        );
        let mut next = self.aggregate.clone();
        let PublicationStorage::V5 { delivery, .. } = &mut next.publication else {
            unreachable!()
        };
        let old_store = delivery.store_generation;
        loop {
            delivery.store_generation = *uuid::Uuid::new_v4().as_bytes();
            if delivery.store_generation != old_store {
                break;
            }
        }
        let mut previous = None;
        for (sequence, entry) in &mut delivery.replay {
            entry.previous = previous;
            entry.digest = replay_digest(
                delivery.store_generation,
                &next.room,
                self.workspace_id,
                entry,
            )?;
            previous = Some(reference(*sequence, entry));
        }
        for admission in delivery.admissions.values_mut() {
            let entry = delivery
                .replay
                .get(&admission.committed.sequence)
                .context("restore receipt missing replay")?;
            admission.committed.digest = entry.digest;
        }
        for consumer in delivery.consumers.values_mut() {
            consumer.acknowledged = None;
        }
        // Pool bytes remain owned; clearing pins grants no reclamation authority.
        delivery.pins.clear();
        let mut candidate = self.peer.clone();
        let messages = self.prepare_certified_bootstrap(
            &mut candidate,
            self.aggregate
                .generation
                .checked_add(1)
                .context("session generation exhausted")?,
        )?;
        self.replace_publication(&mut next, &messages)?;
        self.commit(next, candidate)
    }
    pub(crate) fn delivery_ledger(&self) -> Result<&DeliveryLedgerV1> {
        let PublicationStorage::V5 { delivery, .. } = &self.aggregate.publication else {
            return Err(error::failure(
                DataFailure::WriterNotSelected,
                anyhow::anyhow!("delivery writer not selected"),
            ));
        };
        Ok(delivery)
    }
    pub(super) fn require_delivery(&self) -> Result<()> {
        error::require(
            !self.store.is_poisoned(),
            DataFailure::DurabilityUncertain,
            "reopen owner after uncertain durability",
        )?;
        error::require(
            self.peer.admission_v5_selected() && !self.aggregate.is_read_only(),
            DataFailure::WriterNotSelected,
            "delivery writer not selected",
        )?;
        self.delivery_ledger()?;
        Ok(())
    }
    /// The scheduler supplies authenticated transport origin, never an item field.
    /// Every primary result comes from StatePeer's exact native admission path.
    pub(crate) fn receive_delivery(
        &mut self,
        origin: TransportOrigin,
        item: CompleteStateItem<'_>,
        now: SystemTime,
    ) -> Result<AdmissionOutcome> {
        self.receive_delivery_with_bound(origin, item, now, MAX_DELIVERY_ITEM_BYTES)
    }
    fn receive_delivery_with_bound(
        &mut self,
        origin: TransportOrigin,
        item: CompleteStateItem<'_>,
        now: SystemTime,
        maximum: usize,
    ) -> Result<AdmissionOutcome> {
        self.require_owner_resource_scope()?;
        self.require_delivery()?;
        error::require(
            item.delivery_id != [0; 16]
                && !item.canonical.is_empty()
                && item.canonical.len() <= maximum,
            DataFailure::InvalidNativeInput,
            "invalid complete delivery item bounds",
        )?;
        let device = match origin {
            TransportOrigin::RelayDelta(key) | TransportOrigin::RetainedRelayDelta(key) => key,
            TransportOrigin::RetainedNativePublication { .. }
            | TransportOrigin::UntrustedPresence(_) => {
                return Err(error::failure(
                    DataFailure::CurrentDenied,
                    anyhow::anyhow!("delivery requires authenticated state source"),
                ));
            }
        };
        let key = ReceiptKey {
            device,
            delivery: item.delivery_id,
        };
        let digest = *blake3::hash(item.canonical).as_bytes();
        let prior = self.delivery_ledger()?.admissions.get(&key).cloned();
        if let Some(prior) = &prior {
            error::require(
                prior.digest == digest,
                DataFailure::ExactIdConflict,
                "delivery identity names different canonical bytes",
            )?;
        }
        let message: NeighbourMsg = postcard::from_bytes(item.canonical)
            .map_err(|e| error::failure(DataFailure::InvalidNativeInput, e))?;
        schema::canonical(&message, item.canonical)
            .map_err(|e| error::failure(DataFailure::InvalidNativeInput, e))?;
        let mut candidate = self.peer.clone();
        let report = candidate
            .receive_from_report(origin, message, now)
            .map_err(error::admission)?;
        let (operation, result) = match report.primary() {
            InputDisposition::Accepted(op) => (op.operation_id(), DurableAdmissionKind::Accepted),
            InputDisposition::Duplicate(op) => (op.operation_id(), DurableAdmissionKind::Duplicate),
            InputDisposition::UnsupportedFamily => {
                return Err(error::failure(
                    DataFailure::UnsupportedNativeFamily,
                    anyhow::anyhow!("unsupported durable delivery family"),
                ));
            }
            InputDisposition::MetadataOnly | InputDisposition::RetryLater(_) => {
                if report.summary().changed || report.summary().metadata_changed {
                    if let Some(proof) = self.prepare_received_media_governance(&mut candidate)? {
                        self.commit_verified_security_metadata_only(candidate, proof)?;
                        return Ok(match report.primary() {
                            InputDisposition::RetryLater(why) => {
                                AdmissionOutcome::RetryLater(why.clone())
                            }
                            _ => AdmissionOutcome::MetadataOnly,
                        });
                    }
                    let mut next = self.aggregate.clone();
                    if candidate.incomplete_reason().is_some() {
                        self.append_incomplete_governance(&mut next, &mut candidate)?;
                    } else {
                        self.append_mutation_publication(&mut next, &mut candidate)?;
                    }
                    self.commit(next, candidate)?;
                } else if report.summary().deferred {
                    // Bounded native prerequisite queue only, no accepted record.
                    self.peer = candidate;
                }
                return Ok(match report.primary() {
                    InputDisposition::RetryLater(why) => AdmissionOutcome::RetryLater(why.clone()),
                    _ => AdmissionOutcome::MetadataOnly,
                });
            }
        };
        if let Some(prior) = prior {
            ensure!(
                prior.operations == [operation],
                "delivery receipt disagrees with native exact admission"
            );
            return Ok(AdmissionOutcome::Durable(prior));
        }
        let existing = self
            .delivery_ledger()?
            .replay
            .iter()
            .find_map(|(sequence, entry)| {
                entry
                    .admitted_operations
                    .contains(&operation)
                    .then(|| reference(*sequence, entry))
            });
        let mut next = self.aggregate.clone();
        let transition = if result == DurableAdmissionKind::Duplicate && existing.is_some() {
            None
        } else if result == DurableAdmissionKind::Duplicate {
            // Exact legacy evidence has no transport association. Certify a real
            // current group; do not invent its original historical commit.
            let messages = self.prepare_certified_bootstrap(
                &mut candidate,
                self.aggregate
                    .generation
                    .checked_add(1)
                    .context("session generation exhausted")?,
            )?;
            self.replace_publication(&mut next, &messages)?;
            None
        } else {
            // Publish the author's original signed item, not a host copy.
            self.append_delivery_publication(
                &mut next,
                &mut candidate,
                &[item.canonical],
                &[operation],
            )?
        };
        let accepted = result == DurableAdmissionKind::Accepted;
        let binding = DeliveryBinding {
            items: vec![BindingItem {
                key: key.clone(),
                digest,
                operation,
                result,
                existing,
            }],
        };
        // LIVE_V1: a newly accepted item is forwarded from inside the commit,
        // after its last non-durability check and before the store barrier.
        let _disarm = accepted.then(|| early_forward::arm(device, item.canonical));
        self.commit_delivery(next, candidate, Some(binding), CommitMode::Mutation)?;
        self.install_live_publication_transition(transition);
        Ok(AdmissionOutcome::Durable(
            self.delivery_ledger()?.admissions[&key].clone(),
        ))
    }
    /// Admit a prefix of `items` (at least one) in request order and return one
    /// outcome per admitted item. Consecutive newly accepted document items are
    /// admitted onto one candidate and committed as ONE replay group: every
    /// receipt references that group, and each item still passes the exact
    /// `StatePeer` admission on top of the items before it. The first item
    /// that needs another path (governance, duplicate, retry, metadata,
    /// refusal, receipt conflict) ends the group; the group commits first and
    /// the caller continues with that item, which then takes the unchanged
    /// single-item path against the committed group. A refused item therefore
    /// never enters a commit, and earlier items stay committed exactly as
    /// before. A leading incompatible item takes the single-item path alone.
    pub(crate) fn receive_delivery_group(
        &mut self,
        origin: TransportOrigin,
        items: &[CompleteStateItem<'_>],
        now: SystemTime,
    ) -> Result<Vec<AdmissionOutcome>> {
        let first = items.first().context("empty delivery group")?;
        let single = |owner: &mut Self| -> Result<Vec<AdmissionOutcome>> {
            Ok(vec![owner.receive_delivery(
                origin.clone(),
                CompleteStateItem {
                    delivery_id: first.delivery_id,
                    canonical: first.canonical,
                },
                now,
            )?])
        };
        let device = match origin {
            TransportOrigin::RelayDelta(key) | TransportOrigin::RetainedRelayDelta(key)
                if items.len() > 1 =>
            {
                key
            }
            _ => return single(self),
        };
        self.require_owner_resource_scope()?;
        self.require_delivery()?;
        let mut candidate = self.peer.clone();
        let mut accepted: Vec<BindingItem> = Vec::new();
        let mut forwarded: Vec<&[u8]> = Vec::new();
        for item in items {
            match self.try_group_item(&candidate, &origin, device, item, &accepted, now) {
                Some((trial, binding)) => {
                    candidate = trial;
                    accepted.push(binding);
                    forwarded.push(item.canonical);
                }
                None => break,
            }
        }
        if accepted.len() < 2 {
            // Nothing to share: the unchanged single-item path owns this item.
            return single(self);
        }
        let keys = accepted
            .iter()
            .map(|item| item.key.clone())
            .collect::<Vec<_>>();
        let mut next = self.aggregate.clone();
        let operations = accepted
            .iter()
            .map(|item| item.operation)
            .collect::<Vec<_>>();
        let transition =
            self.append_delivery_publication(&mut next, &mut candidate, &forwarded, &operations)?;
        // LIVE_V1: every verified item of the group is forwarded from inside
        // the one commit, after its last non-durability check and before the
        // group's store barrier.
        let _disarm = early_forward::arm_group(device, &forwarded);
        self.commit_delivery(
            next,
            candidate,
            Some(DeliveryBinding { items: accepted }),
            CommitMode::Mutation,
        )?;
        self.install_live_publication_transition(transition);
        let ledger = self.delivery_ledger()?;
        keys.iter()
            .map(|key| {
                ledger
                    .admissions
                    .get(key)
                    .cloned()
                    .map(AdmissionOutcome::Durable)
                    .context("committed group receipt missing")
            })
            .collect()
    }
    /// Admit one more item onto a group candidate, or `None` when it must take
    /// the single-item path. Never mutates the owner or the given candidate.
    fn try_group_item(
        &self,
        candidate: &StatePeer,
        origin: &TransportOrigin,
        device: [u8; 32],
        item: &CompleteStateItem<'_>,
        group: &[BindingItem],
        now: SystemTime,
    ) -> Option<(StatePeer, BindingItem)> {
        if item.delivery_id == [0; 16]
            || item.canonical.is_empty()
            || item.canonical.len() > MAX_DELIVERY_ITEM_BYTES
        {
            return None;
        }
        let ledger = self.delivery_ledger().ok()?;
        let key = ReceiptKey {
            device,
            delivery: item.delivery_id,
        };
        if ledger.admissions.len().saturating_add(group.len()) >= MAX_ADMISSIONS
            || ledger.admissions.contains_key(&key)
            || group.iter().any(|prior| prior.key == key)
        {
            return None;
        }
        let message: NeighbourMsg = postcard::from_bytes(item.canonical).ok()?;
        // Governance may change the uploader's own authority: it commits alone,
        // and later items are re-authorized against its committed result.
        if !matches!(message, NeighbourMsg::AppDocument { .. }) {
            return None;
        }
        schema::canonical(&message, item.canonical).ok()?;
        let mut trial = candidate.clone();
        let report = trial
            .receive_from_report(origin.clone(), message, now)
            .ok()?;
        let InputDisposition::Accepted(op) = report.primary() else {
            return None;
        };
        let operation = op.operation_id();
        if ledger
            .replay
            .values()
            .any(|entry| entry.admitted_operations.contains(&operation))
            || group.iter().any(|prior| prior.operation == operation)
        {
            return None;
        }
        Some((
            trial,
            BindingItem {
                key,
                digest: *blake3::hash(item.canonical).as_bytes(),
                operation,
                result: DurableAdmissionKind::Accepted,
                existing: None,
            },
        ))
    }
    pub(super) fn append_incomplete_governance(
        &self,
        next: &mut Aggregate,
        candidate: &mut StatePeer,
    ) -> Result<()> {
        self.prepare_media_governance_for_publication(candidate)?;
        let PublicationStorage::V5 { pool, roots, .. } = &mut next.publication else {
            bail!("V5 governance branch requires delivery storage")
        };
        if candidate.room_message() != self.peer.room_message() {
            let bytes =
                schema::encode_bounded(&candidate.room_message(), self.serialized_commit_cap()?)?;
            roots.tail.push(PublicationRef {
                payload: pool.intern(&bytes)?,
                kind: Retention::Delta,
            });
        }
        Ok(())
    }
    pub(super) fn finalize_delivery(
        &self,
        aggregate: &mut Aggregate,
        candidate: &mut StatePeer,
        binding: Option<DeliveryBinding>,
        policy_at: Option<SystemTime>,
        rotation: bool,
    ) -> Result<()> {
        let selected = matches!(aggregate.publication, PublicationStorage::V5 { .. });
        ensure!(
            selected == candidate.admission_v5_selected(),
            "outer and native delivery writer selection differ"
        );
        if !selected {
            return Ok(());
        }
        let mut operations = candidate
            .admissions_since(&self.peer)?
            .iter()
            .map(|a| a.operation_id())
            .collect::<Vec<_>>();
        let generation = aggregate.generation;
        let previous_roots = match &self.aggregate.publication {
            PublicationStorage::V5 { roots, .. } => Some(roots),
            _ => None,
        };
        let PublicationStorage::V5 { roots, .. } = &aggregate.publication else {
            unreachable!()
        };
        let roots_changed = previous_roots.is_none_or(|old| {
            old.baseline_generation != roots.baseline_generation
                || old.baseline != roots.baseline
                || old.tail != roots.tail
        });
        let state_changed = candidate.room_message() != self.peer.room_message()
            || candidate.rows() != self.peer.rows()
            || !operations.is_empty();
        if !roots_changed && state_changed && candidate.incomplete_reason().is_none() {
            let messages = match policy_at {
                Some(now) => self.prepare_certified_bootstrap_at(candidate, generation, now)?,
                None => self.prepare_certified_bootstrap(candidate, generation)?,
            };
            self.replace_publication(aggregate, &messages)?;
            operations = candidate
                .admissions_since(&self.peer)?
                .iter()
                .map(|a| a.operation_id())
                .collect();
        }
        let PublicationStorage::V5 {
            roots, delivery, ..
        } = &mut aggregate.publication
        else {
            unreachable!()
        };
        let publications = if previous_roots.is_none_or(|old| {
            old.baseline_generation != roots.baseline_generation || old.baseline != roots.baseline
        }) {
            roots
                .baseline
                .iter()
                .chain(&roots.tail)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            let old = previous_roots.unwrap();
            ensure!(
                roots.tail.starts_with(&old.tail),
                "delivery tail replacement requires new baseline"
            );
            roots.tail[old.tail.len()..].to_vec()
        };
        for item in binding.iter().flat_map(|binding| &binding.items) {
            if item.existing.is_none() {
                operations.push(item.operation);
            }
        }
        if rotation {
            // The first entry of a new incarnation certifies the complete
            // committed state: it names every admitted operation (bounded),
            // and always those of retained receipts it re-binds below.
            ensure!(
                delivery.replay.is_empty() && binding.is_none(),
                "rotation must start an empty incarnation without a new receipt"
            );
            let all = super::retention::all_admitted_operations(candidate)?;
            if all.len() <= MAX_REFERENCES / 2 {
                operations.extend(all);
            }
            for record in delivery.admissions.values() {
                operations.extend(record.operations.iter().copied());
            }
        }
        operations.sort_unstable();
        operations.dedup();
        ensure!(
            operations.is_empty() || !publications.is_empty(),
            "exact admissions require native replay publications"
        );
        delivery.resolved_through = generation;
        let mut prepared = None;
        if !publications.is_empty() {
            let mut entry = ReplayCommit {
                owner_generation: generation,
                previous: delivery
                    .replay
                    .last_key_value()
                    .map(|(seq, entry)| reference(*seq, entry)),
                digest: [0; 32],
                publications,
                admitted_operations: operations,
                committed_at_ns: policy_at
                    .unwrap_or_else(SystemTime::now)
                    .duration_since(SystemTime::UNIX_EPOCH)?
                    .as_nanos()
                    .try_into()?,
            };
            entry.digest = replay_digest(
                delivery.store_generation,
                &aggregate.room,
                self.workspace_id,
                &entry,
            )?;
            prepared = Some(reference(generation, &entry));
            ensure!(
                delivery.replay.insert(generation, entry).is_none(),
                "replay generation already exists"
            );
        }
        if rotation {
            // Retained receipts (unacknowledged offline journals) now name the
            // incarnation's certified first entry, which contains their ops.
            let first = prepared
                .clone()
                .context("rotation requires a certified checkpoint group")?;
            for record in delivery.admissions.values_mut() {
                record.committed = first.clone();
            }
        }
        for binding in binding.into_iter().flat_map(|binding| binding.items) {
            let committed = binding
                .existing
                .or_else(|| prepared.clone())
                .context("delivery lacks actual committed publication group")?;
            let entry = delivery
                .replay
                .get(&committed.sequence)
                .context("missing original replay")?;
            ensure!(
                entry.digest == committed.digest
                    && entry.admitted_operations.contains(&binding.operation),
                "receipt lacks exact replay association"
            );
            ensure!(
                delivery
                    .admissions
                    .insert(
                        binding.key,
                        AdmissionRecord {
                            digest: binding.digest,
                            operations: vec![binding.operation],
                            committed,
                            result: binding.result
                        }
                    )
                    .is_none(),
                "delivery receipt replaced"
            );
        }
        Ok(())
    }
}
#[path = "delivery/ack_log.rs"]
pub(crate) mod ack_log;
#[path = "delivery/consumer.rs"]
pub(crate) mod consumer;
#[path = "delivery/early_forward.rs"]
pub(crate) mod early_forward;
#[path = "delivery/error.rs"]
pub(crate) mod error;
#[cfg(test)]
#[path = "delivery/resource_tests.rs"]
mod resource_tests;
pub(crate) use consumer::NativeDeliveryAuthority;

#[cfg(all(test, feature = "media"))]
#[path = "delivery/media_tests.rs"]
mod media_tests;
#[cfg(test)]
#[path = "delivery/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "delivery/staged_tests.rs"]
mod staged_tests;

#[cfg(test)]
#[path = "delivery/snapshot_storage_tests.rs"]
mod snapshot_storage_tests;

#[cfg(test)]
#[path = "delivery/retention_tests.rs"]
mod retention_tests;
