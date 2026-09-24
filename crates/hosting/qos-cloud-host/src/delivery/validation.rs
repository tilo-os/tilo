//! Shared retained delivery checks; no owner or admission witness.
use super::{DeliveryState, VERSION, replay_digest};
use crate::State;
use qos_relay::delivery_v3::commit::{CommitError, Digest, MAX_REFERENCES};
use std::collections::BTreeSet;

/// Fixed structural failures stay allocation-free on the paid caller path.
#[derive(Debug)]
pub(super) enum CheckError {
    Invalid(&'static str),
    Ledger(CommitError),
    Replay(replay_digest::ReplayDigestError),
}
impl From<CommitError> for CheckError {
    fn from(error: CommitError) -> Self {
        Self::Ledger(error)
    }
}
impl CheckError {
    // Preserve the legacy diagnostic and concrete downcast source. Only legacy
    // callers invoke this allocating conversion; bounded callers use fixed errors.
    pub(super) fn into_legacy(self) -> anyhow::Error {
        match self {
            Self::Invalid(message) => anyhow::Error::msg(message),
            Self::Ledger(error) => error.into(),
            Self::Replay(replay_digest::ReplayDigestError::Room(error)) => error.into(),
            Self::Replay(replay_digest::ReplayDigestError::Encoding(error)) => error.into(),
        }
    }
}
pub(super) fn selected(state: &State) -> Result<Option<&DeliveryState>, CheckError> {
    let Some(delivery) = &state.delivery else {
        if state.version == VERSION {
            return Err(CheckError::Invalid("delivery state missing"));
        }
        return Ok(None);
    };
    if state.version != VERSION || !state.outbox.is_empty() {
        return Err(CheckError::Invalid("mixed delivery payload owners"));
    }
    Ok(Some(delivery))
}
pub(super) fn structure(delivery: &DeliveryState) -> Result<(), CheckError> {
    if !matches!(delivery.prior_private_schema, 4 | 5) {
        return Err(CheckError::Invalid("unknown private schema selection"));
    }
    if delivery.policy_lineage == 0
        || delivery.policy_revision.len() != 64
        || !delivery
            .policy_revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(CheckError::Invalid("invalid delivery policy lineage"));
    }
    delivery.ledger.validate()?;
    if delivery.baseline_generation == 0
        || delivery.baseline_generation > delivery.ledger.resolved_through
    {
        return Err(CheckError::Invalid("invalid baseline generation"));
    }
    let baseline_cut = delivery
        .ledger
        .replay
        .get(&delivery.baseline_generation)
        .ok_or(CheckError::Invalid("original baseline cut is not retained"))?;
    if baseline_cut.owner_generation != delivery.baseline_generation
        || baseline_cut.publications != delivery.baseline
    {
        return Err(CheckError::Invalid(
            "baseline generation does not name its exact retained roots",
        ));
    }
    if !delivery
        .baseline
        .len()
        .checked_add(delivery.tail.len())
        .is_some_and(|n| n <= MAX_REFERENCES)
        || delivery.queue.len() > MAX_REFERENCES
        || delivery.baseline.is_empty()
    {
        return Err(CheckError::Invalid("delivery roots exceed bound"));
    }
    for reference in delivery.baseline.iter().chain(&delivery.tail) {
        delivery.pool.resolve(&reference.payload)?;
    }
    for reference in &delivery.queue {
        delivery.pool.resolve(&reference.payload)?;
    }
    for replay in delivery.ledger.replay.values() {
        for reference in &replay.publications {
            delivery.pool.resolve(&reference.payload)?;
        }
    }
    for pin in delivery.ledger.pins.values() {
        for reference in &pin.publications {
            delivery.pool.resolve(&reference.payload)?;
        }
    }
    Ok(())
}
pub(super) fn barrier(state: &State, delivery: &DeliveryState) -> Result<(), CheckError> {
    if delivery.ledger.resolved_through != state.generation {
        return Err(CheckError::Invalid("delivery barrier mismatch"));
    }
    Ok(())
}
pub(super) fn selected_v5(selected: bool) -> Result<(), CheckError> {
    if !selected {
        return Err(CheckError::Invalid("native admission v5 required"));
    }
    Ok(())
}
pub(super) fn revision(delivery: &DeliveryState, actual: &str) -> Result<(), CheckError> {
    if actual != delivery.policy_revision {
        return Err(CheckError::Invalid("delivery policy revision mismatch"));
    }
    Ok(())
}
/// Concrete retained index views only. Neither variant is a receipt or an
/// owner credential, and no caller-defined membership function is accepted.
pub(super) enum Admissions<'index, 'peer, 'work> {
    Legacy(&'index BTreeSet<Digest>),
    Bounded(&'index qos_peer_state::PeerRead<'peer, 'work>),
}
impl Admissions<'_, '_, '_> {
    fn contains(&self, operation: &Digest) -> bool {
        match self {
            Self::Legacy(index) => index.contains(operation),
            Self::Bounded(read) => read.contains_admitted_operation(operation),
        }
    }
}
pub(super) fn replay(
    state: &State,
    delivery: &DeliveryState,
    admissions: Admissions<'_, '_, '_>,
) -> Result<(), CheckError> {
    for entry in delivery.ledger.replay.values() {
        let expected = replay_digest::exact(state, delivery.ledger.store_generation, entry)
            .map_err(CheckError::Replay)?;
        if entry.digest != expected
            || !entry
                .admitted_operations
                .iter()
                .all(|op| admissions.contains(op))
        {
            return Err(CheckError::Invalid("invalid Cloud replay evidence"));
        }
    }
    Ok(())
}
#[cfg(test)]
#[path = "resources/delivery_validation/tests.rs"]
mod tests;
