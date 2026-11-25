//! The frontier each destination is known to have of a document, and the writers a replica
//! wrote it as.

use automerge::{ActorId, AutoCommit, ChangeHash};
use std::collections::HashSet;

/// The actors this replica has written one document as, beyond the ones the document
/// itself names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WritingActors {
    actors: HashSet<ActorId>,
}

impl WritingActors {
    /// Note that this replica writes as `actor`.
    pub fn note(&mut self, actor: ActorId) {
        self.actors.insert(actor);
    }

    /// Whether `actor` is a writer of this replica.
    #[must_use]
    pub fn contains(&self, actor: &ActorId) -> bool {
        self.actors.contains(actor)
    }
}

/// An explicit outbound synchronization frontier for one document route. A frontier
/// instead names the exact heads one destination is known to have, so serialization is
/// independent of unrelated callers and can later be advanced from an acknowledgement
/// rather than merely from a send attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentFrontier {
    heads: Vec<ChangeHash>,
    /// Changes the destination delivered. It holds them, and they are never sent back;
    /// they are kept apart from `heads` because a delivered change can build on one this
    /// device has not sent yet, and that one still has to go out.
    observed: Vec<ChangeHash>,
}

impl DocumentFrontier {
    /// Start at the document's current heads.
    pub fn at_document(doc: &mut AutoCommit) -> Self {
        Self {
            heads: doc.get_heads(),
            observed: Vec::new(),
        }
    }

    /// Serialize everything after this frontier and advance it to the heads represented
    /// by that payload.
    pub fn take_delta(&mut self, doc: &mut AutoCommit, own: &WritingActors) -> Vec<u8> {
        let (delta, current) = self.pending_delta(doc, own);
        self.advance(current);
        delta
    }

    /// Everything the document holds that the destination does not, whoever made it: for
    /// a destination that mirrors the record whole, such as a page replicating an open
    /// record, not for a route, which publishes this replica's own changes alone
    /// ([`Self::pending_delta`]).
    pub fn pending_mirror_delta(&mut self, doc: &mut AutoCommit) -> (Vec<u8>, Vec<ChangeHash>) {
        let current = doc.get_heads();
        if current == self.heads {
            return (Vec::new(), current);
        }
        let delta = doc.save_after(&self.heads);
        if delta.is_empty() {
            self.heads.clone_from(&current);
        }
        (delta, current)
    }

    /// Build a delta without advancing the frontier.
    pub fn pending_delta(
        &mut self,
        doc: &mut AutoCommit,
        own: &WritingActors,
    ) -> (Vec<u8>, Vec<ChangeHash>) {
        let current = doc.get_heads();
        if current == self.heads {
            return (Vec::new(), current);
        }
        let delta = self.delta(doc, own);
        if !delta.is_empty() {
            return (delta, current);
        }
        self.advance(current.clone());
        (Vec::new(), current)
    }

    /// What this device holds that the destination does not, as an incremental payload:
    /// this replica's own changes after the frontier, made under the document's current
    /// actor, a writer `own` names, or a handle on this device whose actor carries the
    /// device's own. Another replica's change is never in it, whether or not this
    /// frontier was told it arrived: a change is published by the replica that made it,
    /// and the identity service signs a change for the action that names it.
    pub fn delta(&self, doc: &mut AutoCommit, own: &WritingActors) -> Vec<u8> {
        let actor = doc.get_actor().clone();
        let device = tilo_resource_store::history::authors::DeviceActors::of(&actor);
        let mut bytes = Vec::new();
        for change in doc.get_changes(&self.heads) {
            let mine = change.actor_id() == &actor
                || own.contains(change.actor_id())
                || device.wrote(&change.actor_id().to_hex_string());
            if !mine || self.observed.contains(&change.hash()) {
                continue;
            }
            bytes.extend_from_slice(change.raw_bytes());
        }
        bytes
    }

    /// Advance to heads whose delta a durable destination accepted.
    pub fn advance(&mut self, heads: Vec<ChangeHash>) {
        self.heads = heads;
        self.observed.clear();
    }

    /// The destination holds the document at `heads` too, beside what it held: what is
    /// sent next is what lies past both.
    pub fn hold(&mut self, heads: impl IntoIterator<Item = ChangeHash>) {
        for head in heads {
            if !self.heads.contains(&head) {
                self.heads.push(head);
            }
        }
    }

    /// Treat the document's current state as already observed on this route.
    pub fn rebase(&mut self, doc: &mut AutoCommit) {
        self.heads = doc.get_heads();
        self.observed.clear();
    }

    /// Record changes known to have arrived from this destination without claiming that
    /// it received locally pending changes.
    pub fn observe(&mut self, observed: impl IntoIterator<Item = ChangeHash>) {
        for hash in observed {
            if !self.heads.contains(&hash) && !self.observed.contains(&hash) {
                self.observed.push(hash);
            }
        }
    }

    /// Whether the destination already holds the document at `heads`: nothing arrived
    /// since this device was told what it holds, and every change under those heads was
    /// delivered or accepted then.
    pub fn unchanged(&self, heads: &[ChangeHash]) -> bool {
        self.observed.is_empty()
            && self.heads.len() == heads.len()
            && heads.iter().all(|head| self.heads.contains(head))
    }

    /// Note every change `doc` gained since `before` as one the destination already holds.
    pub fn observe_changes_after(&mut self, doc: &mut AutoCommit, before: &[ChangeHash]) {
        self.observe(
            doc.get_changes(before)
                .into_iter()
                .map(|change| change.hash()),
        );
    }

    pub fn heads(&self) -> &[ChangeHash] {
        &self.heads
    }
}

/// The changes `payload` carries that `doc` does not hold: loading a change whose history
/// is missing keeps it until that history arrives, and it was delivered now all the same.
pub fn changes_beyond(doc: &mut AutoCommit, payload: &[u8]) -> Vec<ChangeHash> {
    let Ok(changes) = automerge::Automerge::parse_changes(payload) else {
        return Vec::new();
    };
    changes
        .into_iter()
        .map(|change| change.hash())
        .filter(|hash| !doc.has_change(hash))
        .collect()
}
