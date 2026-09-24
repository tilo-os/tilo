//! Complete incoming state items admitted through the actual Cloud Store owner.
//! The wire receipt is written in the same atomic Store commit as the peer
//! change and its replay group; nothing here is a receipt before that barrier.
use super::VERSION;
use crate::{
    Store, baseline_publications, durable_bootstrap_messages, durable_mutation_publications,
    now_time,
};
use qos_peer_state::{InputDisposition, NeighbourMsg, TransportOrigin};
use qos_relay::delivery_v3::commit::{
    AdmissionRecord, CommitRef, DeliveryId, Digest, DurableAdmissionKind, MAX_ADMISSIONS,
    ReceiptKey,
};

/// Transient commit instruction; never serialized with the Store aggregate.
/// One item, or every item of one multi-item group (one shared replay group).
#[derive(Clone)]
pub(crate) struct DeliveryBinding {
    pub(super) items: Vec<BindingItem>,
}
#[derive(Clone)]
pub(crate) struct BindingItem {
    pub(super) key: ReceiptKey,
    pub(super) digest: Digest,
    pub(super) operation: Digest,
    pub(super) result: DurableAdmissionKind,
    pub(super) existing: Option<CommitRef>,
}
/// Response data only; the durable variant was read back after the commit.
#[derive(Debug)]
pub(crate) enum StateOutcome {
    Durable(AdmissionRecord),
    RetryLater,
    MetadataOnly,
}
#[derive(Debug)]
pub(crate) enum StateRefusal {
    /// Canonical bytes, bounds or native semantics rejected this item.
    Invalid,
    /// The delivery identity already names different canonical bytes.
    Conflict,
    /// A family this owner does not durably deliver.
    Unsupported,
    /// Retained receipt capacity is exhausted.
    Capacity,
    /// The Store commit or owner state failed; the caller must stop using it.
    Owner(anyhow::Error),
}

impl Store {
    /// Existing durable receipt for a lost final reply; never admits input.
    pub(crate) fn delivery_receipt(
        &self,
        device: [u8; 32],
        delivery: DeliveryId,
        digest: Digest,
    ) -> Result<Option<AdmissionRecord>, StateRefusal> {
        let ledger = self
            .state
            .delivery
            .as_ref()
            .ok_or_else(|| StateRefusal::Owner(anyhow::anyhow!("delivery not selected")))?
            .ledger();
        match ledger.admissions.get(&ReceiptKey { device, delivery }) {
            Some(record) if record.digest == digest => Ok(Some(record.clone())),
            Some(_) => Err(StateRefusal::Conflict),
            None => Ok(None),
        }
    }

    /// The authenticated session device is the transport origin; no item field
    /// selects authority. Every primary result comes from StatePeer admission.
    pub(crate) fn admit_delivery_item(
        &mut self,
        device: [u8; 32],
        delivery: DeliveryId,
        canonical: &[u8],
        maximum: usize,
    ) -> Result<StateOutcome, StateRefusal> {
        let owner = StateRefusal::Owner;
        self.ensure_reconciled().map_err(owner)?;
        if self.state.version != VERSION || self.state.delivery.is_none() {
            return Err(StateRefusal::Owner(anyhow::anyhow!(
                "delivery not selected"
            )));
        }
        if delivery == [0; 16] || canonical.is_empty() || canonical.len() > maximum {
            return Err(StateRefusal::Invalid);
        }
        let digest = *blake3::hash(canonical).as_bytes();
        let prior = self.delivery_receipt(device, delivery, digest)?;
        let message = decode_canonical(canonical)?;
        let now = self.policy_now_ns().map_err(StateRefusal::Owner)?;
        let origin = TransportOrigin::RelayDelta(device);
        let (before, peer) = self.peer_transition_at(now).map_err(StateRefusal::Owner)?;
        let mut candidate = peer;
        let report = candidate
            .receive_from_report(origin.clone(), message, now_time(now))
            .map_err(|_| StateRefusal::Invalid)?;
        let (operation, result) = match report.primary() {
            InputDisposition::Accepted(op) => (op.operation_id(), DurableAdmissionKind::Accepted),
            InputDisposition::Duplicate(op) => (op.operation_id(), DurableAdmissionKind::Duplicate),
            InputDisposition::UnsupportedFamily => return Err(StateRefusal::Unsupported),
            disposition @ (InputDisposition::MetadataOnly | InputDisposition::RetryLater(_)) => {
                let retry = matches!(disposition, InputDisposition::RetryLater(_));
                // Metadata, governance and deferred prerequisites keep the one
                // existing receive transaction; it commits whatever changed.
                match self.receive(origin, decode_canonical(canonical)?, now) {
                    Ok(()) => {}
                    Err(crate::ReceiveError::Rejected(_)) => return Err(StateRefusal::Invalid),
                    Err(crate::ReceiveError::Owner(error)) => {
                        return Err(StateRefusal::Owner(error));
                    }
                }
                return Ok(if retry {
                    StateOutcome::RetryLater
                } else {
                    StateOutcome::MetadataOnly
                });
            }
        };
        if let Some(prior) = prior {
            if prior.operations != [operation] {
                return Err(StateRefusal::Owner(anyhow::anyhow!(
                    "delivery receipt disagrees with native exact admission"
                )));
            }
            return Ok(StateOutcome::Durable(prior));
        }
        let ledger = self.state.delivery.as_ref().expect("selected").ledger();
        if ledger.admissions.len() >= MAX_ADMISSIONS {
            return Err(StateRefusal::Capacity);
        }
        let existing = ledger.replay.iter().find_map(|(sequence, entry)| {
            entry
                .admitted_operations
                .contains(&operation)
                .then_some(CommitRef {
                    sequence: *sequence,
                    digest: entry.digest,
                })
        });
        let generation = self.next_generation().map_err(StateRefusal::Owner)?;
        let key = self.key();
        let mut state = self.state.clone();
        if result == DurableAdmissionKind::Duplicate && existing.is_some() {
            // Receipt only: the exact original group already names this operation.
        } else {
            if result == DurableAdmissionKind::Accepted && candidate.incomplete_reason().is_some() {
                // A document cannot be receipted without its own replay group.
                return Ok(StateOutcome::RetryLater);
            }
            let publications = (if result == DurableAdmissionKind::Duplicate {
                // Exact legacy evidence has no transport association. Certify a
                // real current group rather than inventing its historical commit.
                durable_bootstrap_messages(&mut candidate, &key, now_time(now), generation)
                    .and_then(baseline_publications)
            } else {
                durable_mutation_publications(
                    &mut candidate,
                    &before,
                    &key,
                    now_time(now),
                    generation,
                )
            })
            .map_err(StateRefusal::Owner)?;
            if publications.is_empty() {
                return Err(StateRefusal::Owner(anyhow::anyhow!(
                    "exact admission produced no durable publication"
                )));
            }
            state.outbox.extend(publications);
            state
                .bound
                .as_mut()
                .ok_or_else(|| StateRefusal::Owner(anyhow::anyhow!("not enrolled")))?
                .peer = candidate
                .durable()
                .map_err(|error| StateRefusal::Owner(anyhow::Error::msg(error)))?;
        }
        let accepted = result == DurableAdmissionKind::Accepted;
        state.delivery_binding = Some(DeliveryBinding {
            items: vec![BindingItem {
                key: ReceiptKey { device, delivery },
                digest,
                operation,
                result,
                existing,
            }],
        });
        // LIVE_V1: forwarded from inside the commit, after its last
        // non-durability check and before the Store barrier.
        let _disarm = accepted.then(|| super::early_forward::arm(device, canonical));
        self.commit(state).map_err(StateRefusal::Owner)?;
        let record = self
            .state
            .delivery
            .as_ref()
            .expect("selected")
            .ledger()
            .admissions
            .get(&ReceiptKey { device, delivery })
            .cloned()
            .ok_or_else(|| StateRefusal::Owner(anyhow::anyhow!("committed receipt missing")))?;
        Ok(StateOutcome::Durable(record))
    }
}

impl Store {
    /// Commit consecutive newly accepted document items of one request as ONE
    /// replay group. Each item passes the exact `StatePeer` admission on top of
    /// the items before it; every receipt references the shared group. The
    /// first item that needs another path (governance, duplicate, retry,
    /// metadata, refusal, capacity, receipt conflict) ends the group, which
    /// commits first; the caller then admits that item on the unchanged
    /// single-item path against the committed group, so a refused item never
    /// enters a commit. `None`: fewer than two items group, and the caller
    /// admits the first item alone. Mutates nothing unless it commits.
    pub(crate) fn admit_delivery_group(
        &mut self,
        device: [u8; 32],
        items: &[(DeliveryId, &[u8])],
        maximum: usize,
    ) -> Result<Option<Vec<AdmissionRecord>>, StateRefusal> {
        if items.len() < 2 {
            return Ok(None);
        }
        let owner = StateRefusal::Owner;
        self.ensure_reconciled().map_err(owner)?;
        if self.state.version != VERSION || self.state.delivery.is_none() {
            return Ok(None);
        }
        let now = self.policy_now_ns().map_err(StateRefusal::Owner)?;
        let origin = TransportOrigin::RelayDelta(device);
        let (before, mut candidate) = self.peer_transition_at(now).map_err(StateRefusal::Owner)?;
        let mut group: Vec<BindingItem> = Vec::new();
        let mut forwarded: Vec<&[u8]> = Vec::new();
        {
            let ledger = self.state.delivery.as_ref().expect("selected").ledger();
            for &(delivery, canonical) in items {
                if delivery == [0; 16] || canonical.is_empty() || canonical.len() > maximum {
                    break;
                }
                let key = ReceiptKey { device, delivery };
                if ledger.admissions.len().saturating_add(group.len()) >= MAX_ADMISSIONS
                    || ledger.admissions.contains_key(&key)
                    || group.iter().any(|prior| prior.key == key)
                {
                    break;
                }
                let Ok(message) = decode_canonical(canonical) else {
                    break;
                };
                // Governance may change the uploader's own authority: it
                // commits alone and later items are re-authorized after it.
                if !matches!(message, NeighbourMsg::AppDocument { .. }) {
                    break;
                }
                let mut trial = candidate.clone();
                let Ok(report) = trial.receive_from_report(origin.clone(), message, now_time(now))
                else {
                    break;
                };
                let InputDisposition::Accepted(op) = report.primary() else {
                    break;
                };
                let operation = op.operation_id();
                if trial.incomplete_reason().is_some()
                    || ledger
                        .replay
                        .values()
                        .any(|entry| entry.admitted_operations.contains(&operation))
                    || group.iter().any(|prior| prior.operation == operation)
                {
                    break;
                }
                candidate = trial;
                group.push(BindingItem {
                    key,
                    digest: *blake3::hash(canonical).as_bytes(),
                    operation,
                    result: DurableAdmissionKind::Accepted,
                    existing: None,
                });
                forwarded.push(canonical);
            }
        }
        if group.len() < 2 {
            return Ok(None);
        }
        let generation = self.next_generation().map_err(StateRefusal::Owner)?;
        let key = self.key();
        let publications =
            durable_mutation_publications(&mut candidate, &before, &key, now_time(now), generation)
                .map_err(StateRefusal::Owner)?;
        if publications.is_empty() {
            return Err(StateRefusal::Owner(anyhow::anyhow!(
                "exact group admission produced no durable publication"
            )));
        }
        let mut state = self.state.clone();
        state.outbox.extend(publications);
        state
            .bound
            .as_mut()
            .ok_or_else(|| StateRefusal::Owner(anyhow::anyhow!("not enrolled")))?
            .peer = candidate
            .durable()
            .map_err(|error| StateRefusal::Owner(anyhow::Error::msg(error)))?;
        let keys = group
            .iter()
            .map(|item| item.key.clone())
            .collect::<Vec<_>>();
        state.delivery_binding = Some(DeliveryBinding { items: group });
        // LIVE_V1: every verified item of the group is forwarded from inside
        // the one commit, after its last non-durability check and before the
        // group's Store barrier.
        let _disarm = super::early_forward::arm_group(device, &forwarded);
        self.commit(state).map_err(StateRefusal::Owner)?;
        let ledger = self.state.delivery.as_ref().expect("selected").ledger();
        keys.iter()
            .map(|key| {
                ledger.admissions.get(key).cloned().ok_or_else(|| {
                    StateRefusal::Owner(anyhow::anyhow!("committed group receipt missing"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }
}

fn decode_canonical(canonical: &[u8]) -> Result<NeighbourMsg, StateRefusal> {
    let message: NeighbourMsg =
        postcard::from_bytes(canonical).map_err(|_| StateRefusal::Invalid)?;
    // Only one byte encoding names a delivery item; re-encode exactly.
    let encoded = postcard::to_stdvec(&message).map_err(|_| StateRefusal::Invalid)?;
    if encoded != canonical {
        return Err(StateRefusal::Invalid);
    }
    Ok(message)
}

/// Insert the transient binding into the candidate ledger inside prepare_commit.
pub(super) fn bind(
    delivery: &mut super::DeliveryState,
    binding: DeliveryBinding,
    created: Option<CommitRef>,
) -> anyhow::Result<()> {
    for item in binding.items {
        bind_item(delivery, item, created.clone())?;
    }
    Ok(())
}
fn bind_item(
    delivery: &mut super::DeliveryState,
    binding: BindingItem,
    created: Option<CommitRef>,
) -> anyhow::Result<()> {
    let committed = binding
        .existing
        .or(created)
        .ok_or_else(|| anyhow::anyhow!("delivery lacks actual committed publication group"))?;
    let entry = delivery
        .ledger
        .replay
        .get(&committed.sequence)
        .ok_or_else(|| anyhow::anyhow!("missing original replay"))?;
    anyhow::ensure!(
        entry.digest == committed.digest && entry.admitted_operations.contains(&binding.operation),
        "receipt lacks exact replay association"
    );
    anyhow::ensure!(
        delivery.ledger.admissions.len() < MAX_ADMISSIONS,
        "delivery receipt capacity exhausted"
    );
    anyhow::ensure!(
        delivery
            .ledger
            .admissions
            .insert(
                binding.key,
                AdmissionRecord {
                    digest: binding.digest,
                    operations: vec![binding.operation],
                    committed,
                    result: binding.result,
                },
            )
            .is_none(),
        "delivery receipt replaced"
    );
    Ok(())
}

#[cfg(test)]
#[path = "admission/tests.rs"]
pub(crate) mod tests;
