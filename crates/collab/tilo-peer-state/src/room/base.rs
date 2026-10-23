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
    /// It builds on the receiver's base, or neither has one: admitted onto the
    /// document as it is, without changes that start another history.
    Same(Vec<Vec<u8>>),
    /// It carries a newer base, which replaces the receiver's document.
    Newer,
}

/// Sort `changes` against `before`. A change with no dependencies starts a
/// history: the base of a newer generation replaces `before`; any other is
/// dropped once `before` has a base, since applied beside it, an older
/// history would merge back what the base left behind.
pub(crate) fn lineage(before: Option<&AutoCommit>, changes: &[Vec<u8>]) -> Lineage {
    let held = before.map_or(0, generation);
    let mut kept = Vec::with_capacity(changes.len());
    for bytes in changes {
        let Ok(change) = automerge::Change::try_from(bytes.as_slice()) else {
            kept.push(bytes.clone());
            continue;
        };
        if !change.deps().is_empty() {
            kept.push(bytes.clone());
            continue;
        }
        let started =
            super::candidate(None, std::slice::from_ref(bytes)).map_or(0, |doc| generation(&doc));
        if started > held {
            return Lineage::Newer;
        }
        let known = before
            .is_some_and(|before| ReadDoc::get_change_by_hash(before, &change.hash()).is_some());
        if held == 0 || known {
            kept.push(bytes.clone());
        }
    }
    Lineage::Same(kept)
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

        assert!(matches!(lineage(Some(&old), &based), Lineage::Newer));
        assert!(matches!(lineage(None, &based), Lineage::Newer));
        assert!(matches!(
            lineage(Some(&base), &history),
            Lineage::Same(kept) if kept.is_empty()
        ));
        assert!(matches!(
            lineage(Some(&base), &based),
            Lineage::Same(kept) if kept == based
        ));
        let heads = base.get_heads();
        base.put(ROOT, "name", "Crew").unwrap();
        base.commit();
        let after = changes(&mut base, &heads);
        assert!(matches!(
            lineage(Some(&old), &after),
            Lineage::Same(kept) if kept == after
        ));
    }
}
