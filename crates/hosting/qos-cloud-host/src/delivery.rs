//! Delivery storage shares the existing Cloud Store transaction and barrier.
//! Storage activation alone never qualifies a native allocation path or ingress.
use crate::{PublicationKind, State, StatePeer, Store, StoredPublication, now_time};
use anyhow::{Context, Result, ensure};
use qos_relay::delivery_v3::commit::*;
use rand::RngCore;
pub(crate) mod access;
pub(crate) mod admission;
pub(crate) mod commit_wake;
pub(crate) mod early_forward;
pub(crate) mod relay;
pub(crate) mod replay_digest;
pub(crate) mod resources;
pub(crate) mod retention;
mod schema;
mod security;
#[cfg(test)]
mod security_tests;
mod validation;
pub(crate) use schema::{DeliveryState, VERSION};

pub(crate) fn committed_peer(state: &State, now: u64) -> Result<StatePeer> {
    let bound = state
        .bound
        .as_ref()
        .context("delivery requires enrolled Store")?;
    // The same restore as the Store's current transition (its `before`
    // half), through the exact memo. A transition that cannot be computed
    // falls back to this plain restore, unchanged and unretained.
    if let Ok(peers) = crate::shared_transition(state, now) {
        return Ok(peers.0.clone());
    }
    StatePeer::restore(
        &bound.peer,
        &state.room,
        bound.authority.clone(),
        crate::active_roster(
            &bound.roster,
            bound.root,
            &crate::revocations(&bound.revocations)?,
            now,
        )?,
        crate::revocations(&bound.revocations)?,
        now_time(now),
    )
    .map_err(anyhow::Error::msg)
}
fn revision(peer: &StatePeer) -> Result<String> {
    Ok(
        qos_action_broker::peer_actions::PrivatePolicyBasis::from_peer(peer)
            .map_err(anyhow::Error::msg)?
            .revision()
            .to_owned(),
    )
}
fn replay_digest(state: &State, store: StoreGeneration, replay: &ReplayCommit) -> Result<Digest> {
    replay_digest::exact(state, store, replay).map_err(|error| match error {
        replay_digest::ReplayDigestError::Room(error) => error.into(),
        replay_digest::ReplayDigestError::Encoding(error) => error.into(),
    })
}

fn intern(
    delivery: &mut DeliveryState,
    publications: &[StoredPublication],
) -> Result<Vec<PublicationRef>> {
    let mut retained = Vec::new();
    for publication in publications {
        publication.validate()?;
        let bytes = postcard::to_stdvec(&publication.message)?;
        let payload = delivery.pool.intern(&bytes)?;
        delivery.queue.push(schema::QueuedPublication {
            payload,
            kind: publication.kind,
        });
        let kind = match publication.kind {
            PublicationKind::Checkpoint => qos_peer_state::transport::Retention::Checkpoint,
            PublicationKind::Delta => qos_peer_state::transport::Retention::Delta,
            PublicationKind::Presence => continue,
        };
        retained.push(PublicationRef { payload, kind });
    }
    Ok(retained)
}
pub(crate) fn validate(state: &State, now: u64) -> Result<()> {
    use validation::CheckError;
    let Some(delivery) = validation::selected(state).map_err(CheckError::into_legacy)? else {
        return Ok(());
    };
    delivery.validate()?;
    validation::barrier(state, delivery).map_err(CheckError::into_legacy)?;
    let peer = committed_peer(state, now)?;
    validation::selected_v5(peer.admission_v5_selected()).map_err(CheckError::into_legacy)?;
    validation::revision(delivery, &revision(&peer)?).map_err(CheckError::into_legacy)?;
    let empty = StatePeer::new(
        &state.room,
        peer.authority().clone(),
        vec![peer.authority().clone()],
        qos_user_identity::RevocationList::new(),
        now_time(now),
    )
    .map_err(anyhow::Error::msg)?;
    let operations = peer
        .admissions_since(&empty)?
        .into_iter()
        .map(|op| op.operation_id())
        .collect::<std::collections::BTreeSet<_>>();
    validation::replay(state, delivery, validation::Admissions::Legacy(&operations))
        .map_err(CheckError::into_legacy)
}

/// Central hook before the existing Store save. No successful in-memory
/// candidate grants a receipt; the enclosing atomic file barrier still decides.
pub(crate) fn prepare_commit(before: &State, next: &mut State, now: u64) -> Result<()> {
    // A receipt instruction never survives into the candidate or a later commit.
    let binding = next.delivery_binding.take();
    if next.version != VERSION {
        ensure!(
            binding.is_none(),
            "delivery receipt requires selected delivery"
        );
        ensure!(
            before.version != VERSION && next.delivery.is_none(),
            "delivery downgrade refused"
        );
        return Ok(());
    }
    if let Some(old) = &before.delivery {
        let prior_peer = committed_peer(before, now)?;
        let next_peer = committed_peer(next, now)?;
        next_peer
            .validate_media_governance_for_publication()
            .map_err(anyhow::Error::msg)?;
        let policy = revision(&next_peer)?;
        let peer_changed =
            before.bound.as_ref().map(|b| &b.peer) != next.bound.as_ref().map(|b| &b.peer);
        ensure!(
            !peer_changed || !next.outbox.is_empty(),
            "public transition lacks actual publications"
        );
        let publications = std::mem::take(&mut next.outbox);
        let delivery = next.delivery.as_mut().context("delivery state missing")?;
        ensure!(
            delivery.ledger.store_generation == old.ledger.store_generation
                && delivery.policy_lineage == old.policy_lineage,
            "delivery lineage substitution"
        );
        delivery.policy_lineage = if policy != old.policy_revision {
            old.policy_lineage
                .checked_add(1)
                .context("policy lineage exhausted")?
        } else {
            old.policy_lineage
        };
        delivery.policy_revision = policy;
        let references = intern(delivery, &publications)?;
        delivery.ledger.resolved_through = next.generation;
        let mut created = None;
        if !references.is_empty() {
            let previous = delivery
                .ledger
                .replay
                .iter()
                .next_back()
                .map(|(sequence, entry)| CommitRef {
                    sequence: *sequence,
                    digest: entry.digest,
                });
            let mut operations: Vec<_> = next_peer
                .admissions_since(&prior_peer)?
                .into_iter()
                .map(|op| op.operation_id())
                .collect();
            // An exact duplicate certified into a fresh group names its original
            // native operation there; the peer already retains that admission.
            for item in binding
                .iter()
                .flat_map(|binding| &binding.items)
                .filter(|item| item.existing.is_none())
            {
                if !operations.contains(&item.operation) {
                    operations.push(item.operation);
                }
            }
            let mut replay = ReplayCommit {
                owner_generation: next.generation,
                previous,
                digest: [0; 32],
                publications: references.clone(),
                admitted_operations: operations,
                committed_at_ns: now,
            };
            delivery.tail.extend(references);
            let incarnation = delivery.ledger.store_generation;
            replay.digest = replay_digest(next, incarnation, &replay)?;
            created = Some(CommitRef {
                sequence: next.generation,
                digest: replay.digest,
            });
            next.delivery
                .as_mut()
                .expect("retained delivery")
                .ledger
                .replay
                .insert(next.generation, replay);
        }
        if let Some(binding) = binding {
            admission::bind(
                next.delivery.as_mut().expect("retained delivery"),
                binding,
                created,
            )?;
        }
    } else {
        ensure!(
            binding.is_none(),
            "delivery receipt requires selected delivery"
        );
    }
    validate(next, now)
}

impl Store {
    /// Explicit operator selection, never an ordinary open or request side effect.
    pub(crate) fn activate_delivery_storage(&mut self, now: u64) -> Result<()> {
        self.ensure_reconciled()?;
        if self.state.version == VERSION {
            return validate(&self.state, now);
        }
        let prior_private_schema = self.state.private_schema_version()?;
        let mut peer = committed_peer(&self.state, now)?;
        peer.select_admission_v5()?;
        let generation = self.next_generation()?;
        let messages =
            crate::durable_bootstrap_messages(&mut peer, &self.key(), now_time(now), generation)?;
        let baseline = crate::baseline_publications(messages)?;
        let mut incarnation = [0; 16];
        rand::rngs::OsRng.try_fill_bytes(&mut incarnation)?;
        ensure!(incarnation != [0; 16], "delivery entropy unavailable");
        let mut delivery = DeliveryState {
            prior_private_schema,
            ledger: DeliveryLedgerV1 {
                version: 1,
                store_generation: incarnation,
                resolved_through: generation,
                replay: Default::default(),
                admissions: Default::default(),
                consumers: Default::default(),
                pins: Default::default(),
            },
            pool: PublicationPool::new(schema::POOL_LIMIT)?,
            baseline_generation: generation,
            baseline: vec![],
            tail: vec![],
            queue: vec![],
            policy_lineage: 1,
            policy_revision: revision(&peer)?,
            transport_hint: None,
        };
        // Preserve pending relay bytes, but never invent receipts/history for previously drained work.
        intern(&mut delivery, &self.state.outbox)?;
        delivery.baseline = intern(&mut delivery, &baseline)?;
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
        replay.digest = replay_digest(&self.state, incarnation, &replay)?;
        delivery.ledger.replay.insert(generation, replay);
        let mut next = self.state.clone();
        next.version = VERSION;
        next.bound
            .as_mut()
            .context("enrolled Store disappeared")?
            .peer = peer.durable().map_err(anyhow::Error::msg)?;
        next.outbox.clear();
        next.delivery = Some(delivery);
        self.commit(next)
    }
}

#[cfg(test)]
mod tests;
