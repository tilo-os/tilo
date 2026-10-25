//! A member admitted later must not receive what was withdrawn before, and
//! an Automerge change keeps every value it ever wrote. So a room document
//! starts over at each admission from a base, its state in one change with no
//! history; members adopt it and build on it, and keep what came before.
use automerge::transaction::Transactable as _;
use automerge::{ActorId, AutoCommit, ChangeHash, ROOT, ReadDoc, ScalarValue, Value};

/// The root key holding a document's base generation; a document that never
/// had a base reads as generation `0`.
pub const BASE_KEY: &str = "base_v1";

/// `doc`'s base generation.
#[must_use]
pub fn generation(doc: &AutoCommit) -> u64 {
    match doc.get(ROOT, BASE_KEY) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Uint(generation) => *generation,
            ScalarValue::Int(generation) => u64::try_from(*generation).unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    }
}

/// A document holding `doc`'s state as it stands, in one change by `actor`
/// with no dependencies, at the next base generation.
///
/// # Errors
///
/// The document does not read, or the copy cannot be written.
pub fn rebase(doc: &AutoCommit, actor: ActorId) -> Result<AutoCommit, automerge::AutomergeError> {
    let next = generation(doc).saturating_add(1);
    let automerge::hydrate::Value::Map(state) = doc.hydrate(ROOT, None)? else {
        return Err(automerge::AutomergeError::InvalidOp(
            automerge::ObjType::Map,
        ));
    };
    let mut base = AutoCommit::new().with_actor(actor);
    base.init_root_from_hydrate(&state)?;
    base.put(ROOT, BASE_KEY, ScalarValue::Uint(next))?;
    base.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
    Ok(base)
}

/// The hash of `doc`'s base change, when it has a base.
#[must_use]
pub fn root(doc: &mut AutoCommit) -> Option<ChangeHash> {
    if generation(doc) == 0 {
        return None;
    }
    doc.get_changes(&[])
        .into_iter()
        .find(|change| change.deps().is_empty())
        .map(|change| change.hash())
}

/// How a batch relates to the base of the document it is admitted to.
pub(crate) enum Lineage {
    /// It builds on the receiver's base, or neither has one: `changes` are
    /// admitted onto the document as it is, once the changes waiting in it
    /// that `replaced` names are dropped.
    Same {
        changes: Vec<Vec<u8>>,
        replaced: Vec<ChangeHash>,
    },
    /// It carries a newer base, which replaces the receiver's document: the
    /// base and the changes of the batch not shown to belong to another
    /// history.
    Newer(Vec<Vec<u8>>),
}

/// Sort `changes` against `before`. A change with no dependencies starts a
/// history: the base of a newer generation replaces `before`; any other is
/// dropped once `before` has a base, since applied beside it, an older
/// history would merge back what the base left behind.
///
/// A governor writes each base's history afresh under its own actor, so the
/// history a base replaced numbers its changes as the new one does. A change
/// of the replaced history waiting for what it builds on holds its actor's
/// sequence number, and the change of the current history at that number
/// would be refused as a duplicate, with everything after it. A change still
/// waiting for its parent may as well be of the current history, and must
/// then wait and apply, so a change is dropped only where it is shown to
/// belong to another history:
///
/// - it starts another history, or builds on a change that does;
/// - or a change built on the base the receiver holds (applied, or taken
///   with this batch on what is applied) holds its actor's sequence number:
///   one history numbers each actor's changes once, so the other change is
///   of another history. This applies to the batch and to the changes
///   already waiting in `before`, and to what builds on such a change.
///
/// A document that never had a base takes the batch as it comes.
pub(crate) fn lineage(before: Option<&AutoCommit>, changes: &[Vec<u8>]) -> Lineage {
    let held = before.map_or(0, generation);
    let decoded: Vec<Option<automerge::Change>> = changes
        .iter()
        .map(|bytes| automerge::Change::try_from(bytes.as_slice()).ok())
        .collect();
    let mut newest: Option<(u64, usize)> = None;
    for (index, (change, bytes)) in decoded.iter().zip(changes).enumerate() {
        let Some(change) = change else {
            continue;
        };
        if !change.deps().is_empty() {
            continue;
        }
        let started =
            super::candidate(None, std::slice::from_ref(bytes)).map_or(0, |doc| generation(&doc));
        if started > held && newest.is_none_or(|(generation, _)| started > generation) {
            newest = Some((started, index));
        }
    }
    if let Some((_, base)) = newest {
        let (changes, _) = sorted(changes, &decoded, Some(base), None);
        return Lineage::Newer(changes);
    }
    let Some(before) = before.filter(|_| held > 0) else {
        return Lineage::Same {
            changes: changes.to_vec(),
            replaced: Vec::new(),
        };
    };
    let (changes, replaced) = sorted(changes, &decoded, None, Some(before.committed_document()));
    Lineage::Same { changes, replaced }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    /// Built on the base: on `base`, or on what `before` holds applied.
    Built,
    /// Not shown to belong to either history yet: it waits.
    Waiting,
    /// Shown to belong to another history.
    Other,
}

/// The changes of `changes` to admit, each in the order it came, and the
/// changes waiting in `before` to drop first. The base is `changes[base]`
/// when the batch carries a newer one, else the base `before` holds. A change
/// that does not decode is kept, for the admission to refuse as malformed.
fn sorted(
    changes: &[Vec<u8>],
    decoded: &[Option<automerge::Change>],
    base: Option<usize>,
    before: Option<&automerge::Automerge>,
) -> (Vec<Vec<u8>>, Vec<ChangeHash>) {
    let mut sort = started(decoded, base, before);
    build_on_base(decoded, &mut sort, before);
    let queued: Vec<&automerge::Change> = before
        .map(|doc| doc.queued_changes().collect())
        .unwrap_or_default();
    let mut other = Other::default();
    renumbered(decoded, &mut sort, &queued, before, &mut other);
    built_on_other(decoded, &mut sort, &queued, &mut other);
    let kept = changes
        .iter()
        .zip(&sort)
        .filter(|&(_, sort)| *sort != Sort::Other)
        .map(|(bytes, _)| bytes.clone())
        .collect();
    (kept, other.replaced)
}

/// Whether `before` holds the change `hash` applied.
fn applied(before: Option<&automerge::Automerge>, hash: &ChangeHash) -> bool {
    before.is_some_and(|doc| doc.get_change_meta_by_hash(hash).is_some())
}

/// How each change stands before anything it builds on is looked at: the
/// base and what does not decode are built, a change starting a history other
/// than the base is of another, and the rest wait.
fn started(
    decoded: &[Option<automerge::Change>],
    base: Option<usize>,
    before: Option<&automerge::Automerge>,
) -> Vec<Sort> {
    decoded
        .iter()
        .enumerate()
        .map(|(index, change)| match change {
            None => Sort::Built,
            Some(_) if Some(index) == base => Sort::Built,
            Some(change) if change.deps().is_empty() => {
                if applied(before, &change.hash()) {
                    Sort::Built
                } else {
                    Sort::Other
                }
            }
            Some(_) => Sort::Waiting,
        })
        .collect()
}

/// Marks what builds on the base. A batch is mostly in causal order; a change
/// ahead of what it builds on is taken on a later pass.
fn build_on_base(
    decoded: &[Option<automerge::Change>],
    sort: &mut [Sort],
    before: Option<&automerge::Automerge>,
) {
    let mut built: std::collections::HashSet<ChangeHash> = std::collections::HashSet::new();
    let mut grew = true;
    while grew {
        grew = false;
        for (change, sort) in decoded.iter().zip(sort.iter_mut()) {
            let Some(change) = change else { continue };
            if *sort == Sort::Built {
                grew |= built.insert(change.hash());
            } else if *sort == Sort::Waiting
                && change
                    .deps()
                    .iter()
                    .all(|dep| built.contains(dep) || applied(before, dep))
            {
                *sort = Sort::Built;
                built.insert(change.hash());
                grew = true;
            }
        }
    }
}

/// The changes shown to belong to another history, and those of them waiting
/// in the receiver's document.
#[derive(Default)]
struct Other {
    hashes: std::collections::HashSet<ChangeHash>,
    replaced: Vec<ChangeHash>,
}

/// Marks as of another history each waiting change, in the batch or in
/// `queued`, whose actor's sequence number the base's history holds.
fn renumbered(
    decoded: &[Option<automerge::Change>],
    sort: &mut [Sort],
    queued: &[&automerge::Change],
    before: Option<&automerge::Automerge>,
    other: &mut Other,
) {
    let claimed: std::collections::HashMap<(&automerge::ActorId, u64), ChangeHash> = decoded
        .iter()
        .zip(sort.iter())
        .filter(|&(_, sort)| *sort == Sort::Built)
        .filter_map(|(change, _)| change.as_ref())
        .map(|change| ((change.actor_id(), change.seq()), change.hash()))
        .collect();
    let held_elsewhere = |change: &automerge::Change| {
        claimed
            .get(&(change.actor_id(), change.seq()))
            .is_some_and(|hash| *hash != change.hash())
    };
    for (change, sort) in decoded.iter().zip(sort.iter_mut()) {
        let Some(change) = change else { continue };
        let numbered = before.is_some_and(|doc| {
            doc.applied_seq(change.actor_id()) >= change.seq() && !applied(before, &change.hash())
        });
        if *sort == Sort::Waiting && (held_elsewhere(change) || numbered) {
            *sort = Sort::Other;
        }
        if *sort == Sort::Other {
            other.hashes.insert(change.hash());
        }
    }
    for change in queued {
        if held_elsewhere(change) {
            other.hashes.insert(change.hash());
            other.replaced.push(change.hash());
        }
    }
}

/// Marks what builds on another history, in the batch or in `queued`, as of
/// it.
fn built_on_other(
    decoded: &[Option<automerge::Change>],
    sort: &mut [Sort],
    queued: &[&automerge::Change],
    other: &mut Other,
) {
    let mut grew = true;
    while grew {
        grew = false;
        for (change, sort) in decoded.iter().zip(sort.iter_mut()) {
            let Some(change) = change else { continue };
            if *sort == Sort::Waiting && change.deps().iter().any(|dep| other.hashes.contains(dep))
            {
                *sort = Sort::Other;
                other.hashes.insert(change.hash());
                grew = true;
            }
        }
        for change in queued {
            if !other.hashes.contains(&change.hash())
                && change.deps().iter().any(|dep| other.hashes.contains(dep))
            {
                other.hashes.insert(change.hash());
                other.replaced.push(change.hash());
                grew = true;
            }
        }
    }
}

/// `before` without the changes waiting in it that `replaced` names, when it
/// names any: what [`Lineage::Same`] admits its changes onto.
pub(crate) fn pruned(before: Option<&AutoCommit>, replaced: Vec<ChangeHash>) -> Option<AutoCommit> {
    before.filter(|_| !replaced.is_empty()).map(|before| {
        let mut doc = before.clone();
        doc.discard_queued(replaced);
        doc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(doc: &mut AutoCommit, since: &[ChangeHash]) -> Vec<Vec<u8>> {
        doc.get_changes(since)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    }

    #[test]
    fn a_base_holds_the_state_and_none_of_the_history() {
        let mut doc = AutoCommit::new();
        let strips = doc
            .put_object(ROOT, "strips", automerge::ObjType::Map)
            .unwrap();
        doc.put(&strips, "1", "ssh prod-db").unwrap();
        doc.commit();
        doc.delete(&strips, "1").unwrap();
        doc.put(&strips, "2", "notes").unwrap();
        doc.commit();

        let mut base = rebase(&doc, ActorId::random()).unwrap();
        assert_eq!(generation(&base), 1);
        let history = base.get_changes(&[]);
        assert_eq!(history.len(), 1);
        assert!(!format!("{:?}", history[0].decode()).contains("ssh prod-db"));
        let strips = base.get(ROOT, "strips").unwrap().unwrap().1;
        assert_eq!(
            base.get(&strips, "2").unwrap().unwrap().0.as_str(),
            Some("notes")
        );
        assert_eq!(root(&mut base), Some(history[0].hash()));
        assert_eq!(generation(&rebase(&base, ActorId::random()).unwrap()), 2);
    }

    #[test]
    fn a_newer_base_replaces_and_an_older_history_is_left_out() {
        let mut old = AutoCommit::new();
        old.put(ROOT, "name", "Team").unwrap();
        old.commit();
        let history = changes(&mut old, &[]);
        let mut base = rebase(&old, ActorId::random()).unwrap();
        let based = changes(&mut base, &[]);

        assert!(matches!(lineage(Some(&old), &based), Lineage::Newer(kept) if kept == based));
        assert!(matches!(lineage(None, &based), Lineage::Newer(kept) if kept == based));
        assert!(matches!(
            lineage(Some(&base), &history),
            Lineage::Same { changes: kept, replaced } if kept.is_empty() && replaced.is_empty()
        ));
        assert!(matches!(
            lineage(Some(&base), &based),
            Lineage::Same { changes: kept, replaced } if kept == based && replaced.is_empty()
        ));
        let heads = base.get_heads();
        base.put(ROOT, "name", "Crew").unwrap();
        base.commit();
        let after = changes(&mut base, &heads);
        assert!(matches!(
            lineage(Some(&old), &after),
            Lineage::Same { changes: kept, replaced } if kept == after && replaced.is_empty()
        ));
    }

    /// What a receiver holding `doc` keeps of `batch`, as the admission
    /// takes it.
    fn take(doc: Option<&AutoCommit>, batch: &[Vec<u8>]) -> AutoCommit {
        match lineage(doc, batch) {
            Lineage::Same { changes, replaced } => {
                let pruned = pruned(doc, replaced);
                super::super::candidate(pruned.as_ref().or(doc), &changes).unwrap()
            }
            Lineage::Newer(changes) => super::super::candidate(None, &changes).unwrap(),
        }
    }

    fn change(bytes: &[u8]) -> automerge::Change {
        automerge::Change::try_from(bytes).unwrap()
    }

    /// A governor's room before and after a base, as `GovernedRoom::commit`
    /// makes it: the base is the governor's first change, and its history
    /// goes on under the same actor.
    struct RebasedRoom {
        /// The room before the base.
        old: AutoCommit,
        /// The changes of the history the base replaced.
        history: Vec<Vec<u8>>,
        /// The base, one change.
        base: Vec<Vec<u8>>,
        /// The room after the base and its new history.
        based: AutoCommit,
        /// The new history's changes after the base.
        new_history: Vec<Vec<u8>>,
    }

    fn rebased_room() -> RebasedRoom {
        let governor = ActorId::from(vec![7; 32]);
        let mut old = AutoCommit::new().with_actor(governor.clone());
        for (key, value) in [("name", "Team"), ("invited", "grace"), ("grace", "invited")] {
            old.put(ROOT, key, value).unwrap();
            old.commit();
        }
        let history = changes(&mut old, &[]);
        let mut based = rebase(&old, governor).unwrap();
        let base = changes(&mut based, &[]);
        let heads = based.get_heads();
        for value in ["viewer", "removed", "agent"] {
            based.put(ROOT, "grace", value).unwrap();
            based.commit();
        }
        let new_history = changes(&mut based, &heads);
        RebasedRoom {
            old,
            history,
            base,
            based,
            new_history,
        }
    }

    /// The governor writes each base's history afresh under its own actor,
    /// so the history a base replaced numbers its changes as the new one
    /// does. A change of the old history that reaches a member after the
    /// base, or with it, never holds the governor's change at its sequence
    /// number back: once a change built on the base holds that number, the
    /// old one is dropped, waiting or arriving.
    #[test]
    fn a_change_of_the_history_a_base_replaced_never_holds_back_the_governors_next() {
        let RebasedRoom {
            old,
            history,
            base,
            based,
            new_history,
        } = rebased_room();
        let old_third = vec![history[2].clone()];
        let seq = |bytes: &Vec<u8>| change(bytes).seq();
        assert_eq!(
            (seq(&old_third[0]), seq(&new_history[1])),
            (3, 3),
            "both histories hold the governor's third change"
        );

        // The old change after the base, before the governor's third: it
        // may yet be of the base's history, so it waits. The governor's
        // third change, built on the base, shows it is not, and it goes.
        let member = take(None, &base);
        let member = take(Some(&member), &old_third);
        assert_eq!(member.committed_document().queued_changes().count(), 1);
        let member = take(Some(&member), &new_history);
        assert_eq!(member.committed_heads(), based.committed_heads());
        assert_eq!(member.committed_document().queued_changes().count(), 0);
        assert_eq!(
            member.get(ROOT, "grace").unwrap().unwrap().0.as_str(),
            Some("agent")
        );

        // The old change after the governor's third: left out, the batch
        // is not refused.
        let late = take(Some(&member), &old_third);
        assert_eq!(late.committed_heads(), based.committed_heads());
        assert_eq!(late.committed_document().queued_changes().count(), 0);

        // The old change with what is left of its history, in the same batch
        // as the base: only what builds on the base is applied, and the old
        // changes go once the governor's history claims their numbers.
        let mut batch = history[1..].to_vec();
        batch.extend(base.iter().cloned());
        let member = take(Some(&old), &batch);
        assert_eq!(member.committed_heads(), vec![change(&base[0]).hash()]);
        assert_eq!(member.committed_document().queued_changes().count(), 2);
        let member = take(Some(&member), &new_history);
        assert_eq!(member.committed_heads(), based.committed_heads());
        assert_eq!(member.committed_document().queued_changes().count(), 0);
    }

    /// A change of the base's own history that arrives before what it builds
    /// on waits, and applies once that arrives: nothing is dropped for being
    /// early.
    #[test]
    fn a_change_of_the_bases_history_ahead_of_its_parent_applies_once_the_parent_arrives() {
        let RebasedRoom {
            base,
            based,
            new_history,
            ..
        } = rebased_room();
        let member = take(None, &base);

        let ahead = take(Some(&member), &new_history[1..]);
        assert_eq!(ahead.committed_document().queued_changes().count(), 2);
        let member = take(Some(&ahead), &new_history[..1]);
        assert_eq!(member.committed_heads(), based.committed_heads());
        assert_eq!(member.committed_document().queued_changes().count(), 0);
        assert_eq!(
            member.get(ROOT, "grace").unwrap().unwrap().0.as_str(),
            Some("agent")
        );

        // The same in one batch, last first.
        let reversed: Vec<Vec<u8>> = new_history.iter().rev().cloned().collect();
        let member = take(Some(&take(None, &base)), &reversed);
        assert_eq!(member.committed_heads(), based.committed_heads());
    }
}
