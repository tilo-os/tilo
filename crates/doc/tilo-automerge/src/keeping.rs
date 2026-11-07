//! Take changes in keeping the first change of a writer's sequence.
//!
//! Automerge gives every change of one actor a sequence number, and a
//! document holds at most one change under each: applying a second change of
//! the same actor and sequence is [`AutomergeError::DuplicateSeqNumber`], and
//! it refuses the whole call, so the changes before and after it are lost
//! too. That happens when one writer wrote a sequence twice — a device that
//! restarted a writer's sequence (a stored actor continued by a second
//! process, a restored copy of a replica) — and both changes reach a document.
//!
//! A replica keeps the change that reached it first, as Automerge itself
//! does when the first was already applied, and leaves out the other one,
//! naming its writer and sequence. The two changes are two different changes
//! under one sequence, not two versions of one change, so one branch of that
//! writer is what a reader has: the rest of the record is read as it is, and
//! what builds on the change left out waits in the document's queue for it.
//!
//! This is the one place that rule is written: the peer that reads a
//! workspace's records, a page's replication and an app's own merge all load
//! incoming changes through [`apply`].

use automerge::{ActorId, AutoCommit, AutomergeError, Change, ChangeHash};

use crate::ApplyIncoming as _;

/// A change left out because the document already holds a change of its
/// writer under the same sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeftOut {
    /// The writer, as the change names it, hex.
    pub actor: String,
    pub seq: u64,
}

impl std::fmt::Display for LeftOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.actor, self.seq)
    }
}

/// What taking incoming changes did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keeping {
    /// Whether the document's heads moved.
    pub changed: bool,
    /// The changes left out because a change of their writer's sequence is
    /// already here, in the order they came.
    pub left_out: Vec<LeftOut>,
}

/// Apply `data`, Automerge changes as a peer sends them, to `doc`. A saved
/// document parses to the changes it carries and applies as they do.
///
/// # Errors
///
/// `data` does not parse as changes, or the changes it carries do not apply;
/// `doc` is unchanged then.
pub fn apply_bytes(doc: &mut AutoCommit, data: &[u8]) -> Result<Keeping, AutomergeError> {
    let changes = automerge::Automerge::parse_changes(data)?;
    apply(doc, changes)
}

/// Apply `changes` to `doc`, leaving out each change whose writer's sequence
/// it already holds (see the module documentation).
///
/// # Errors
///
/// The changes that are taken do not apply; `doc` is unchanged then.
pub fn apply(
    doc: &mut AutoCommit,
    changes: impl IntoIterator<Item = Change>,
) -> Result<Keeping, AutomergeError> {
    let (kept, left_out) = take(doc, changes);
    let before = doc.get_heads();
    doc.apply_incoming(kept)?;
    Ok(Keeping {
        changed: doc.get_heads() != before,
        left_out,
    })
}

/// What a document makes of one change of an incoming stream, among the
/// changes `taken` before it: `None` when it takes it, and whether it already
/// holds exactly that change (`true`) or another change of its writer's
/// sequence (`false`, a fork).
fn held_by(doc: &AutoCommit, change: &Change, taken: &[(ActorId, u64)]) -> Option<bool> {
    let document = doc.committed_document();
    let held = document.applied_seq(change.actor_id()) >= change.seq()
        || taken
            .iter()
            .any(|(actor, seq)| actor == change.actor_id() && *seq == change.seq());
    held.then(|| document.change_hashes().contains(&change.hash()))
}

/// Split `changes` into those `doc` takes and the ones it leaves out, in the
/// order they came (see the module documentation).
fn take(
    doc: &AutoCommit,
    changes: impl IntoIterator<Item = Change>,
) -> (Vec<Change>, Vec<LeftOut>) {
    let mut kept = Vec::new();
    let mut left_out = Vec::new();
    let mut taken: Vec<(ActorId, u64)> = Vec::new();
    let mut hashes: Vec<ChangeHash> = Vec::new();
    for change in changes {
        // A change this stream already carried, as `apply_changes` skips it.
        if hashes.contains(&change.hash()) {
            continue;
        }
        hashes.push(change.hash());
        match held_by(doc, &change, &taken) {
            Some(false) => left_out.push(LeftOut {
                actor: change.actor_id().to_hex_string(),
                seq: change.seq(),
            }),
            Some(true) => {}
            None => {
                taken.push((change.actor_id().clone(), change.seq()));
                kept.push(change);
            }
        }
    }
    (kept, left_out)
}

/// The first change of `data` whose writer's sequence `doc` already holds
/// under another change: a fork, which Automerge cannot hold, and which
/// [`apply`] leaves out. `None` when every change of `data` is one `doc`
/// would take or one it already holds. A reader whose document is decided by
/// its changes as a whole refuses the fork rather than reading one branch.
#[must_use]
pub fn fork(doc: &AutoCommit, data: &[u8]) -> Option<LeftOut> {
    let changes = automerge::Automerge::parse_changes(data).ok()?;
    take(doc, changes).1.into_iter().next()
}

/// The writer and sequence `data` writes again, when it carries no change
/// `doc` would take: one writer wrote that sequence twice, and `doc` holds
/// the other change under it. `None` for a stream that repeats changes `doc`
/// already holds and writes nothing new, and for one that has something new.
#[must_use]
pub fn repeated_sequence(doc: &mut AutoCommit, data: &[u8]) -> Option<LeftOut> {
    let changes = automerge::Automerge::parse_changes(data).ok()?;
    let (kept, left_out) = take(doc, changes);
    if kept.is_empty() {
        left_out.into_iter().next()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{LeftOut, apply_bytes, repeated_sequence};
    use crate::ApplyIncoming as _;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT, ReadDoc as _};

    /// A record holding its first change, and the second change of the same
    /// writer as two devices wrote it: one continued the writer from the
    /// record it held, the other from a copy that had gone another way.
    fn forked() -> (AutoCommit, Vec<u8>, Vec<u8>) {
        let mut first = AutoCommit::new();
        first.put(ROOT, "a", 1).unwrap();
        first.commit();
        let actor = first.get_actor().clone();
        let base = first.get_heads();
        let base_bytes = first.save_after(&[]);

        let mut kept = first.clone();
        kept.put(ROOT, "kept", "first").unwrap();
        kept.commit();
        let kept_change = kept.save_after(&base);

        let mut fork = AutoCommit::new();
        fork.set_actor(actor);
        fork.load_incoming(&base_bytes).unwrap();
        fork.put(ROOT, "fork", "second").unwrap();
        fork.commit();
        let fork_change = fork.save_after(&base);

        let parsed_kept = automerge::Automerge::parse_changes(&kept_change).unwrap();
        let parsed_fork = automerge::Automerge::parse_changes(&fork_change).unwrap();
        assert_eq!(parsed_kept.len(), 1);
        assert_eq!(parsed_fork.len(), 1);
        assert_eq!(parsed_kept[0].seq(), 2, "sequence 2 of one writer");
        assert_eq!(parsed_fork[0].seq(), 2);
        assert_ne!(parsed_kept[0].hash(), parsed_fork[0].hash(), "a fork");
        (first, kept_change, fork_change)
    }

    #[test]
    fn a_forked_sequence_keeps_the_change_that_reached_the_record_first() {
        let (mut record, kept, fork) = forked();
        let mut stream = kept;
        stream.extend_from_slice(&fork);
        let applied = apply_bytes(&mut record, &stream).unwrap();
        assert!(applied.changed);
        assert_eq!(
            applied.left_out,
            [LeftOut {
                actor: record.get_actor().to_hex_string(),
                seq: 2
            }]
        );
        assert!(
            record.get(ROOT, "kept").unwrap().is_some(),
            "the change the record holds first is read"
        );
        assert!(
            record.get(ROOT, "fork").unwrap().is_none(),
            "the change under the same sequence is left out, not applied"
        );
        let actor = record.get_actor().clone();
        assert_eq!(record.committed_document().applied_seq(&actor), 2);
    }

    #[test]
    fn a_record_that_meets_the_other_branch_first_reads_that_one_whole() {
        let (mut record, kept, fork) = forked();
        let applied = apply_bytes(&mut record, &fork).unwrap();
        assert_eq!(applied.left_out, [] as [LeftOut; 0]);
        assert!(record.get(ROOT, "fork").unwrap().is_some());
        let applied = apply_bytes(&mut record, &kept).unwrap();
        assert!(!applied.changed);
        assert_eq!(
            applied.left_out,
            [LeftOut {
                actor: record.get_actor().to_hex_string(),
                seq: 2
            }],
            "the sequence is named"
        );
        assert!(record.get(ROOT, "kept").unwrap().is_none());
    }

    #[test]
    fn changes_a_record_already_holds_are_a_replay_and_not_a_fork() {
        let (mut record, kept, fork) = forked();
        apply_bytes(&mut record, &kept).unwrap();
        let applied = apply_bytes(&mut record, &kept).unwrap();
        assert_eq!(applied.left_out, [] as [LeftOut; 0]);
        assert!(!applied.changed, "the document holds all of it");
        assert_eq!(repeated_sequence(&mut record, &kept), None, "a replay");
        assert_eq!(
            repeated_sequence(&mut record, &fork),
            Some(LeftOut {
                actor: record.get_actor().to_hex_string(),
                seq: 2
            }),
            "a stream of only the other branch writes the sequence again"
        );
    }

    #[test]
    fn a_stream_with_something_new_is_not_a_repeated_sequence() {
        let (mut record, kept, _fork) = forked();
        apply_bytes(&mut record, &kept).unwrap();
        let mut later = AutoCommit::new();
        later
            .apply_incoming(automerge::Automerge::parse_changes(&kept).unwrap())
            .unwrap();
        later.put(ROOT, "later", true).unwrap();
        later.commit();
        let stream = later.save_after(&[]);
        assert_eq!(
            repeated_sequence(&mut record, &stream),
            None,
            "it carries a change this record would take"
        );
        assert!(apply_bytes(&mut record, &stream).unwrap().changed);
    }

    #[test]
    fn a_payload_that_is_not_changes_is_refused_and_applies_nothing() {
        let mut record = AutoCommit::new();
        assert!(apply_bytes(&mut record, b"not an automerge document").is_err());
        assert_eq!(record.get_heads(), Vec::new());
    }
}
