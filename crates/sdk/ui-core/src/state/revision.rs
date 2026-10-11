//! Persistent, branch-preserving history derived from an Automerge document.
//!
//! Unlike the session-local [`super::UndoLog`], this graph needs no sidecar
//! file and no app-defined schema: Automerge already persists every change and
//! its causal parents. Rebuilding the graph after a restart or merge therefore
//! recovers the same branch points, merge points, and revision identifiers.

use crate::time::{Duration, SystemTime, UNIX_EPOCH};
use automerge::{AutoCommit, ChangeHash};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

/// Stable, content-addressed identifier for one document revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RevisionId(ChangeHash);

impl RevisionId {
    #[must_use]
    pub const fn as_hash(self) -> ChangeHash {
        self.0
    }
}

impl std::fmt::Display for RevisionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for RevisionId {
    type Err = automerge::ParseChangeHashError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ChangeHash::from_str(value).map(Self)
    }
}

/// One node in a document's causal revision graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub id: RevisionId,
    /// Immediate causal predecessors. More than one means this revision
    /// merged concurrent branches.
    pub parents: Vec<RevisionId>,
    /// Immediate successors. More than one means work branched here.
    pub children: Vec<RevisionId>,
    /// Automerge actor id, encoded as stable hexadecimal.
    pub actor: String,
    /// Advisory commit time. `None` for an unstamped change.
    pub timestamp: Option<SystemTime>,
    pub message: Option<String>,
    pub operations: usize,
    /// Whether this revision is one of the live document's current heads.
    pub is_head: bool,
}

impl Revision {
    #[must_use]
    pub fn is_branch_point(&self) -> bool {
        self.children.len() > 1
    }

    #[must_use]
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

/// A causal-order snapshot of all revisions retained by a document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RevisionHistory {
    revisions: Vec<Revision>,
    positions: HashMap<RevisionId, usize>,
    heads: Vec<RevisionId>,
}

impl RevisionHistory {
    /// Reconstruct history directly from the document's persisted change DAG.
    pub fn from_document(doc: &mut AutoCommit) -> Self {
        let heads: Vec<_> = doc.get_heads().into_iter().map(RevisionId).collect();
        let head_set: HashSet<_> = heads.iter().copied().collect();
        let changes = doc.get_changes(&[]);
        let mut revisions: Vec<_> = changes
            .iter()
            .map(|change| Revision {
                id: RevisionId(change.hash()),
                parents: change.deps().iter().copied().map(RevisionId).collect(),
                children: Vec::new(),
                actor: change.actor_id().to_hex_string(),
                timestamp: timestamp(change.timestamp()),
                message: change.message().map(str::to_owned),
                operations: change.len(),
                is_head: head_set.contains(&RevisionId(change.hash())),
            })
            .collect();
        let positions: HashMap<_, _> = revisions
            .iter()
            .enumerate()
            .map(|(position, revision)| (revision.id, position))
            .collect();
        let edges: Vec<_> = revisions
            .iter()
            .flat_map(|revision| {
                revision
                    .parents
                    .iter()
                    .copied()
                    .map(move |parent| (parent, revision.id))
            })
            .collect();
        for (parent, child) in edges {
            if let Some(position) = positions.get(&parent).copied()
                && let Some(revision) = revisions.get_mut(position)
            {
                revision.children.push(child);
            }
        }
        Self {
            revisions,
            positions,
            heads,
        }
    }

    /// Revisions in Automerge's dependency-closed causal order.
    #[must_use]
    pub fn revisions(&self) -> &[Revision] {
        &self.revisions
    }

    #[must_use]
    pub fn get(&self, id: RevisionId) -> Option<&Revision> {
        self.positions
            .get(&id)
            .and_then(|position| self.revisions.get(*position))
    }

    #[must_use]
    pub fn heads(&self) -> &[RevisionId] {
        &self.heads
    }

    pub fn roots(&self) -> impl Iterator<Item = &Revision> {
        self.revisions
            .iter()
            .filter(|revision| revision.parents.is_empty())
    }

    pub fn merges(&self) -> impl Iterator<Item = &Revision> {
        self.revisions.iter().filter(|revision| revision.is_merge())
    }
}

fn timestamp(seconds: i64) -> Option<SystemTime> {
    let seconds = u64::try_from(seconds).ok().filter(|seconds| *seconds > 0)?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

/// A revision id from another document or from discarded/corrupt input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownRevision(pub RevisionId);

impl std::fmt::Display for UnknownRevision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "revision {} is not present in this document",
            self.0
        )
    }
}

impl std::error::Error for UnknownRevision {}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::{CommitOptions, Transactable};
    use automerge::{ActorId, ROOT, ReadDoc};

    fn commit(doc: &mut AutoCommit, message: &str) -> RevisionId {
        RevisionId(
            doc.commit_with(
                CommitOptions::default()
                    .with_message(message)
                    .with_time(1_700_000_000),
            )
            .expect("test edit commits"),
        )
    }

    fn forked_document() -> (AutoCommit, RevisionId, RevisionId, RevisionId, RevisionId) {
        let mut base = AutoCommit::new();
        base.set_actor(ActorId::from(b"base".as_slice()));
        base.put(&ROOT, "base", true).unwrap();
        let base_id = commit(&mut base, "base");
        let bytes = base.save();

        let mut alice = AutoCommit::load(&bytes).unwrap();
        alice.set_actor(ActorId::from(b"alice".as_slice()));
        alice.put(&ROOT, "alice", true).unwrap();
        let alice_id = commit(&mut alice, "alice branch");

        let mut bob = AutoCommit::load(&bytes).unwrap();
        bob.set_actor(ActorId::from(b"bob".as_slice()));
        bob.put(&ROOT, "bob", true).unwrap();
        let bob_id = commit(&mut bob, "bob branch");

        alice.merge(&mut bob).unwrap();
        alice.put(&ROOT, "merged", true).unwrap();
        let merge_id = commit(&mut alice, "merge branches");
        (alice, base_id, alice_id, bob_id, merge_id)
    }

    #[test]
    fn the_revision_graph_round_trips_and_restores_branches_undoably() {
        // phase: graph_exposes_branch_and_merge_edges
        {
            let (mut doc, base, alice, bob, merge) = forked_document();
            let graph = RevisionHistory::from_document(&mut doc);
            let branch = graph.get(base).unwrap();
            assert!(branch.is_branch_point());
            assert_eq!(
                branch.children.iter().copied().collect::<HashSet<_>>(),
                HashSet::from([alice, bob])
            );
            let merged = graph.get(merge).unwrap();
            assert!(merged.is_merge());
            assert_eq!(
                merged.parents.iter().copied().collect::<HashSet<_>>(),
                HashSet::from([alice, bob])
            );
            assert_eq!(graph.heads(), &[merge]);
            assert!(merged.is_head);
            assert_eq!(merged.message.as_deref(), Some("merge branches"));
            assert!(merged.timestamp.is_some());
        }
        // phase: graph_round_trips_with_the_document
        {
            let (mut doc, _, _, _, _) = forked_document();
            let before = RevisionHistory::from_document(&mut doc);
            let bytes = doc.save();
            let mut loaded = AutoCommit::load(&bytes).unwrap();
            let after = RevisionHistory::from_document(&mut loaded);
            assert_eq!(after, before);
        }
        // phase: restoring_a_branch_is_forward_history_and_is_undoable
        {
            let (doc, base, alice, _bob, merge) = forked_document();
            let mut state = crate::State::from_doc(doc);
            assert!(
                state
                    .restore_revision(alice, None, Some("Alice branch".into()), None)
                    .unwrap()
            );
            assert!(state.doc.get(&ROOT, "alice").unwrap().is_some());
            assert!(state.doc.get(&ROOT, "bob").unwrap().is_none());
            assert!(state.doc.get(&ROOT, "merged").unwrap().is_none());

            let graph = state.revision_history();
            assert!(graph.get(base).unwrap().is_branch_point());
            assert!(
                graph.get(merge).is_some(),
                "the previous future is retained"
            );

            state
                .undo(None, None)
                .expect("the restore itself is undoable");
            assert!(state.doc.get(&ROOT, "bob").unwrap().is_some());
            assert!(state.doc.get(&ROOT, "merged").unwrap().is_some());
        }
        // phase: restore_rejects_a_revision_from_another_document
        {
            let mut other = AutoCommit::new();
            other.put(&ROOT, "other", true).unwrap();
            let foreign = commit(&mut other, "foreign");
            let mut state = crate::State::new();
            assert_eq!(
                state.restore_revision(foreign, None, None, None),
                Err(UnknownRevision(foreign))
            );
        }
    }
}
