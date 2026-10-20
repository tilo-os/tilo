//! Exact installed document routing. No transport, signing or action log lives here.
use super::HistoryOwner;
use crate::{
    collaboration::{PrimaryRouteWitness, RouteSpec},
    widget::Widget,
};
use automerge::{AutoCommit, ChangeHash, transaction::Transactable};
use collab_types::DocumentMutation;
use std::collections::{HashSet, VecDeque};
use tilo_automerge::ApplyIncoming;

const MAX_PENDING_ITEMS: usize = 64;
/// A single remote change can be as large as the transport carries, so a
/// deferred queue must hold at least one of them.
const MAX_PENDING_BYTES: usize = collab_types::MAX_DOCUMENT_TRANSFER_BYTES;

/// Only invalid mutation data may be quarantined. Owner/capacity failures keep
/// their admitted queue entry for the existing lifetime/recovery policy.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RemoteApplyError {
    #[error("{0}")]
    InvalidMutation(String),
    #[error("{0}")]
    Unavailable(String),
}
impl RemoteApplyError {
    pub(crate) fn is_invalid_mutation(&self) -> bool {
        matches!(self, Self::InvalidMutation(_))
    }
}

/// Apply a mutation the host has already committed. Bytes from peers reach an
/// app only after the host admitted them through `tilo_automerge_isolate`, so
/// this applies in the app's own process.
pub(crate) fn merge_committed(
    doc: &mut AutoCommit,
    mutation: &DocumentMutation,
) -> Result<bool, RemoteApplyError> {
    ready(doc)?;
    // Decode into a clone first so malformed incremental data cannot partly
    // mutate the real owner. Merge preserves actor, causal history and identity.
    let mut incoming = match mutation {
        DocumentMutation::Snapshot(bytes) => load(bytes)?,
        DocumentMutation::Incremental(bytes) => {
            let mut incoming = doc.clone();
            load_incremental(&mut incoming, bytes)?;
            incoming
        }
    };
    let before = super::sorted_heads(doc.base_heads());
    doc.merge_incoming(&mut incoming)
        .map_err(|error| RemoteApplyError::InvalidMutation(error.to_string()))?;
    Ok(before != super::sorted_heads(doc.base_heads()))
}

/// [`merge_committed`] into `candidate`, a copy its caller keeps only when
/// the mutation applied and validated: the changes load straight into it,
/// with no second copy of the document and no merge of a whole one.
pub(crate) fn merge_committed_into_copy(
    candidate: &mut AutoCommit,
    mutation: &DocumentMutation,
) -> Result<bool, RemoteApplyError> {
    ready(candidate)?;
    let before = super::sorted_heads(candidate.base_heads());
    match mutation {
        DocumentMutation::Snapshot(bytes) => {
            candidate
                .merge_incoming(&mut load(bytes)?)
                .map_err(|error| RemoteApplyError::InvalidMutation(error.to_string()))?;
        }
        DocumentMutation::Incremental(bytes) => load_incremental(candidate, bytes)?,
    }
    Ok(before != super::sorted_heads(candidate.base_heads()))
}

fn ready(doc: &AutoCommit) -> Result<(), RemoteApplyError> {
    if doc.pending_ops() == 0 {
        Ok(())
    } else {
        Err(RemoteApplyError::Unavailable(
            crate::i18n::I18nManager::lookup_plain("history-sync-finish-edit"),
        ))
    }
}

fn load(bytes: &[u8]) -> Result<AutoCommit, RemoteApplyError> {
    AutoCommit::load(bytes).map_err(|error| RemoteApplyError::InvalidMutation(error.to_string()))
}

/// Load an incremental stream into `doc`. Automerge's incremental loader
/// accepts a valid prefix on partial parse errors, so the whole stream is
/// first loaded strictly on its own. MissingDeps comes only after a complete
/// parse and is expected for deltas that need the receiver's history.
fn load_incremental(doc: &mut AutoCommit, bytes: &[u8]) -> Result<(), RemoteApplyError> {
    match AutoCommit::load(bytes) {
        Ok(_) | Err(automerge::AutomergeError::MissingDeps) => {}
        Err(error) => return Err(RemoteApplyError::InvalidMutation(error.to_string())),
    }
    doc.load_incoming(bytes)
        .map(|_| ())
        .map_err(|error| RemoteApplyError::InvalidMutation(error.to_string()))
}
fn mutation_bytes(mutation: &DocumentMutation) -> usize {
    match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes.len(),
    }
}
#[derive(Default)]
pub(crate) struct PendingRemote {
    queue: VecDeque<DocumentMutation>,
    bytes: usize,
    pub(crate) failure: Option<String>,
    invalid_mutation_pending: bool,
    recovery_snapshot_pending: bool,
}
impl PendingRemote {
    pub(crate) fn admit(&mut self, mutation: DocumentMutation) -> bool {
        // Retaining visible failure during deferred recovery must not discard
        // traffic received after S. Queue it in order behind the admitted S.
        if self.failure.is_some() && !self.recovery_snapshot_pending {
            let accepted = self.recover_snapshot(mutation);
            if !accepted {
                // An older snapshot already behind a quarantined item also
                // cannot cover traffic refused after that snapshot arrived.
                self.invalid_mutation_pending = false;
            }
            accepted
        } else {
            self.push(mutation)
        }
    }
    pub(crate) fn push(&mut self, mutation: DocumentMutation) -> bool {
        let size = mutation_bytes(&mutation);
        if self.failure.is_some() && !self.recovery_snapshot_pending {
            return false;
        }
        if self.queue.len() >= MAX_PENDING_ITEMS
            || size > MAX_PENDING_BYTES.saturating_sub(self.bytes)
        {
            // A refused later update is absent from every older queued snapshot.
            // Neither the original recovery S nor an older ordinary snapshot
            // may clear this new loss obligation when the queue drains.
            self.invalid_mutation_pending = false;
            self.recovery_snapshot_pending = false;
            self.failure = Some(crate::i18n::I18nManager::lookup_plain(
                "history-sync-queue-full",
            ));
            return false;
        }
        self.bytes = self.bytes.saturating_add(size);
        self.queue.push_back(mutation);
        true
    }
    pub(crate) fn recover_snapshot(&mut self, mutation: DocumentMutation) -> bool {
        let DocumentMutation::Snapshot(bytes) = &mutation else {
            return false;
        };
        if bytes.len() > MAX_PENDING_BYTES
            || !self.queue.is_empty()
            || AutoCommit::load(bytes).is_err()
        {
            return false;
        }
        // Retain the failure and publication stop until the valid snapshot is
        // actually merged; an unfinished local edit can defer that boundary.
        self.bytes = bytes.len();
        self.queue.push_back(mutation);
        self.recovery_snapshot_pending = true;
        true
    }
    pub(crate) fn quarantine_invalid_front(&mut self, reason: String) {
        self.pop();
        // A malformed item discovered while an overflow is outstanding must
        // not weaken its requirement for a newly received recovery snapshot.
        if self.failure.is_none() || self.invalid_mutation_pending || self.recovery_snapshot_pending
        {
            self.failure = Some(reason);
            self.invalid_mutation_pending = true;
        }
        self.recovery_snapshot_pending = false;
    }
    pub(crate) fn pop_applied(&mut self) {
        // A snapshot already admitted behind bad bytes can also recover once
        // actually merged. Never clear overflow merely by draining an old item.
        if (self.invalid_mutation_pending || self.recovery_snapshot_pending)
            && matches!(self.front(), Some(DocumentMutation::Snapshot(_)))
        {
            self.failure = None;
            self.invalid_mutation_pending = false;
            self.recovery_snapshot_pending = false;
        }
        self.pop();
    }
    pub(crate) fn front(&self) -> Option<&DocumentMutation> {
        self.queue.front()
    }
    pub(crate) fn pop(&mut self) {
        if let Some(mutation) = self.queue.pop_front() {
            self.bytes = self.bytes.saturating_sub(mutation_bytes(&mutation));
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}
pub(crate) struct OwnerRoute {
    pub(crate) route: RouteSpec,
    pub(crate) owners: Vec<HistoryOwner>,
    /// Heads whose changes the transport accepted. Advanced only after a
    /// successful hand-off, so a refused change is offered again.
    pub(crate) sent_heads: Vec<ChangeHash>,
    pub(crate) needs_snapshot: bool,
    pub(crate) pending: PendingRemote,
    /// Why local changes on this route are not reaching peers.
    pub(crate) outbound_failure: Option<String>,
    /// A record kept beside the shown document ([`Widget::record_owners`]):
    /// no host replays its route, so a snapshot that leaves this replica
    /// ahead of its sender is answered with this replica's own.
    pub(crate) answers_snapshots: bool,
    /// Joined, and not yet open for documents: its first publication
    /// waits for the route instead of failing. A record a running action
    /// created (the next part of a chain) has no route until the action
    /// made it.
    pub(crate) opening: bool,
}
/// A record of a live owner's tree, and the lifetime its owner on the
/// route lives by.
struct TreeRecord {
    store: crate::record::RecordStore,
    checkout: crate::record::RecordCheckout,
    lifetime: std::rc::Rc<super::owner::LegacyHistoryLifetime>,
}

#[derive(Default)]
pub(crate) struct OwnerDocumentRoutes {
    pub(crate) witness: Option<PrimaryRouteWitness>,
    pub(crate) owners: Vec<OwnerRoute>,
    pub(crate) primary: PendingRemote,
    pub(crate) primary_generation: u64,
    pub(crate) status: Option<String>,
    /// Why local changes on the primary or room-index route are not reaching
    /// peers. Cleared by the next accepted change.
    pub(crate) outbound: Option<String>,
    retired: HashSet<RouteSpec>,
    /// The records held as part of a live owner's tree ([`RecordId::tree`])
    /// that no view owns: each keeps its route while its store has it open.
    tree: Vec<TreeRecord>,
    #[cfg(test)]
    pub(crate) sent: Vec<(RouteSpec, crate::collaboration::CollabRequest)>,
    /// Transport outcome the recording test transport reports for owned
    /// routes; `None` accepts every change.
    #[cfg(test)]
    pub(crate) refuse_for_test: Option<crate::collaboration::CollabSendError>,
}
impl OwnerDocumentRoutes {
    /// Give every record of a live owner's tree its own owner: a chat's
    /// message parts, a container's child records. They are records of the
    /// owner's store, synced on their own routes like the owner's record,
    /// whether or not a view shows them. Without this a part only one device
    /// wrote never left it.
    fn hold_trees(&mut self, current: &mut Vec<(HistoryOwner, bool)>) {
        let mut stores: Vec<crate::record::RecordStore> = Vec::new();
        for (owner, _) in current.iter() {
            let (store, _) = owner.store_checkout();
            if !stores.iter().any(|known| known.same_owner(store)) {
                stores.push(store.clone());
            }
        }
        let mut held = Vec::new();
        for store in &stores {
            for checkout in store.open_tree_records() {
                let owned = current.iter().any(|(owner, _)| {
                    let (other, owned) = owner.store_checkout();
                    other.same_owner(store) && owned == &checkout
                });
                if owned {
                    continue;
                }
                let index = self
                    .tree
                    .iter()
                    .position(|entry| entry.store.same_owner(store) && entry.checkout == checkout);
                let entry = match index {
                    Some(index) => self.tree.swap_remove(index),
                    None => TreeRecord {
                        store: store.clone(),
                        checkout,
                        lifetime: super::owner::LegacyHistoryLifetime::new(),
                    },
                };
                held.push(entry);
            }
        }
        // What closed, or a view now owns, gives up its tree owner.
        for gone in self.tree.drain(..) {
            gone.lifetime.active.set(false);
        }
        for entry in &held {
            current.push((
                HistoryOwner::legacy(entry.store.clone(), entry.checkout.clone(), &entry.lifetime),
                true,
            ));
        }
        self.tree = held;
    }

    pub(crate) fn retired_route(&self, route: &RouteSpec) -> bool {
        self.retired.contains(route)
    }
    pub(crate) fn reconcile(
        &mut self,
        root: &dyn Widget,
        witness: Option<PrimaryRouteWitness>,
        generation: u64,
        outer: &AutoCommit,
    ) -> Vec<RouteSpec> {
        fn collect(root: &dyn Widget, owners: &mut Vec<(HistoryOwner, bool)>) {
            let held = root.history_owner().map(|owner| (owner, false));
            let beside = root.record_owners().into_iter().map(|owner| (owner, true));
            for (owner, answers) in held.into_iter().chain(beside) {
                if !owners
                    .iter()
                    .any(|(existing, _)| existing.same_owner(&owner))
                {
                    owners.push((owner, answers));
                }
            }
            for child in root.children() {
                collect(child, owners);
            }
        }
        if self.witness != witness || self.primary_generation != generation {
            if !self.primary.is_empty() || self.owners.iter().any(|route| !route.pending.is_empty())
            {
                self.status = Some(crate::i18n::I18nManager::lookup_plain(
                    "history-sync-owner-changed",
                ));
            }
            self.owners.clear();
            self.retired.clear();
            self.primary = PendingRemote::default();
            self.witness = witness;
            self.primary_generation = generation;
        }
        if self.witness.is_none() {
            return vec![];
        }
        let mut current = vec![];
        collect(root, &mut current);
        self.hold_trees(&mut current);
        // Hidden live subtrees retain their admitted collaboration owners.
        crate::history_mode::capability::visit_framework_history(root, &mut |_, live| {
            collect(live, &mut current)
        });
        self.owners.retain_mut(|entry| {
            entry.owners.retain(|owner| {
                current.iter().any(|(actual, _)| actual.same_owner(owner))
                    && owner.validate().is_ok()
            });
            if entry.owners.is_empty() {
                self.retired.insert(entry.route.clone());
                self.status = Some(crate::i18n::I18nManager::lookup_plain(
                    "history-sync-owner-closed",
                ));
            }
            !entry.owners.is_empty()
        });
        let mut joins = vec![];
        for (owner, answers_snapshots) in current {
            let Some(route) = owner.broker_route() else {
                continue;
            };
            if self.retired.contains(&route) {
                continue;
            }
            if let Some(entry) = self.owners.iter_mut().find(|entry| entry.route == route) {
                if !entry
                    .owners
                    .iter()
                    .any(|existing| existing.same_owner(&owner))
                {
                    entry.owners.push(owner);
                }
            } else {
                let Ok(doc) = owner.document(outer) else {
                    continue;
                };
                joins.push(route.clone());
                self.owners.push(OwnerRoute {
                    route,
                    owners: vec![owner],
                    sent_heads: super::sorted_heads(doc.base_heads()),
                    needs_snapshot: true,
                    pending: PendingRemote::default(),
                    outbound_failure: None,
                    answers_snapshots,
                    opening: true,
                });
            }
        }
        joins
    }
}

#[cfg(test)]
mod round1_tests {
    use super::*;
    #[test]
    fn round1_bad_front_does_not_relax_overflow_recovery_or_lose_queued_bytes() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "value", "snapshot").unwrap();
        doc.commit();
        let snapshot = DocumentMutation::Snapshot(doc.save());
        let mut queue = PendingRemote::default();
        assert!(queue.push(DocumentMutation::Snapshot(vec![1])));
        for _ in 1..MAX_PENDING_ITEMS {
            assert!(queue.push(snapshot.clone()));
        }
        assert!(!queue.push(snapshot.clone()));
        let overflow = queue.failure.clone();
        queue.quarantine_invalid_front("invalid front".into());
        assert_eq!(queue.failure, overflow);
        assert_eq!(queue.queue.len(), MAX_PENDING_ITEMS - 1);
        while !queue.is_empty() {
            queue.pop_applied();
        }
        assert_eq!(queue.bytes, 0);
        assert_eq!(
            queue.failure, overflow,
            "old accepted snapshots cannot satisfy a later overflow"
        );
        assert!(!queue.recover_snapshot(DocumentMutation::Snapshot(vec![1])));
        assert_eq!(queue.failure, overflow);
        assert!(queue.recover_snapshot(snapshot));
        assert_eq!(
            queue.failure, overflow,
            "valid recovery remains pending until successful merge"
        );
        queue.pop_applied();
        assert!(queue.failure.is_none());
        assert_eq!(queue.bytes, 0);
    }
}

#[cfg(test)]
mod round2_tests {
    use super::*;
    #[test]
    fn round2_refused_traffic_also_retires_recovery_by_an_older_queued_snapshot() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "snapshot", "accepted before failure")
            .unwrap();
        doc.commit();
        let heads = doc.base_heads();
        let snapshot = DocumentMutation::Snapshot(doc.save());
        doc.put(automerge::ROOT, "later", "not covered by older snapshot")
            .unwrap();
        doc.commit();
        let delta = DocumentMutation::Incremental(doc.save_after(&heads));
        let fresh = DocumentMutation::Snapshot(doc.save());
        let mut queue = PendingRemote::default();
        assert!(queue.admit(DocumentMutation::Snapshot(vec![1])));
        assert!(queue.admit(snapshot.clone()));
        queue.quarantine_invalid_front("invalid front requires recovery".into());
        // S was admitted before discovering the bad front. A temporarily
        // unavailable owner may still leave it queued when later traffic arrives.
        assert!(!queue.admit(delta));
        queue.pop_applied();
        assert!(
            queue.failure.is_some(),
            "the older queued S cannot cover a later refused message"
        );
        assert!(queue.admit(fresh));
        queue.pop_applied();
        assert!(queue.failure.is_none());
    }
}
