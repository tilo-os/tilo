//! Which storage units a change writes, read without the app's code.
//!
//! A record with storage units beside itself (`#[model(unit)]` fields and
//! the items of unit collections) keeps their storage patterns in its
//! document under [`UNIT_INDEX`] (`comments/*`, `body`), as
//! `tilo_resource::footprint` writes them. A receiver applies a change to
//! the record as it was, lists the operations the change made, and maps
//! each to the unit whose pattern it falls under: the record itself (the
//! empty path) when it falls under none. The author lists the same units in
//! the change's claim; a receiver refuses a change that wrote a unit its
//! claim leaves out.

use automerge::{AutoCommit, Change, ObjType, Prop, ROOT, ReadDoc, Value};
use tilo_actions::PathStep;
use tilo_automerge::ApplyIncoming;

use crate::claim::UnitPath;

/// The document key of a record's unit index.
pub const UNIT_INDEX: &str = "_units";

/// The key a collection item keeps its value under, inside its entry.
const ITEM_VALUE: &str = "v";

/// Whether `doc` keeps a unit index: whether it has units beside itself.
pub fn has_units(doc: &impl ReadDoc) -> bool {
    matches!(
        doc.get(ROOT, UNIT_INDEX),
        Ok(Some((Value::Object(ObjType::List), _)))
    )
}

/// The unit patterns `doc` keeps, split into keys (`*` for any item).
fn patterns(doc: &AutoCommit) -> Vec<Vec<String>> {
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(ROOT, UNIT_INDEX) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|at| match doc.get(&list, at).ok()?? {
            (Value::Scalar(scalar), _) => scalar.as_str().map(str::to_owned),
            _ => None,
        })
        .map(|pattern| pattern.split('/').map(str::to_owned).collect())
        .collect()
}

/// The unit the storage keys `keys` fall under, by `patterns`, and how far
/// below the unit's own key they lie: 0 at it (the unit made), 1 for an
/// entry directly in it (an item put into an outbox), more for what is
/// inside such an entry (its settlement).
fn unit_of(patterns: &[Vec<String>], keys: &[String]) -> (UnitPath, usize) {
    let Some(pattern) = patterns
        .iter()
        .filter(|pattern| {
            pattern.len() <= keys.len()
                && pattern
                    .iter()
                    .zip(keys)
                    .all(|(want, key)| want == "*" || want == key)
        })
        .max_by_key(|pattern| pattern.len())
    else {
        return (Vec::new(), 0);
    };
    let mut unit = Vec::new();
    let mut after_item = false;
    for (want, key) in pattern.iter().zip(keys) {
        if want == "*" {
            unit.push(PathStep::Item(key.clone()));
            after_item = true;
        } else if after_item && want == ITEM_VALUE {
            after_item = false;
        } else {
            unit.push(PathStep::Field(key.clone()));
            after_item = false;
        }
    }
    (unit, keys.len().saturating_sub(pattern.len()))
}

fn key_of(prop: &Prop) -> String {
    match prop {
        Prop::Map(key) => key.clone(),
        Prop::Seq(index) => index.to_string(),
    }
}

/// What one change writes: the units its operations fall under, and of
/// those the units it put an entry into: what it queued in an outbox. An
/// outbox made empty, an entry settled or taken out, queues nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Written {
    pub units: Vec<UnitPath>,
    pub added: Vec<UnitPath>,
}

/// The units `change` writes when applied to `before`, the record at the
/// change's dependencies (or later, as long as it holds them). A record
/// without a unit index has only itself.
///
/// # Errors
/// A change that does not apply to `before`.
pub fn written_units(
    before: &AutoCommit,
    change: &Change,
) -> Result<Vec<UnitPath>, automerge::AutomergeError> {
    applying(&mut before.clone(), change).map(|written| written.units)
}

/// Apply `change` to `doc` and return what it wrote: the diff of `doc`
/// between its heads before and after, read in place.
fn applying(doc: &mut AutoCommit, change: &Change) -> Result<Written, automerge::AutomergeError> {
    let before = doc.get_heads();
    doc.apply_incoming([change.clone()])?;
    let patterns = patterns(doc);
    if patterns.is_empty() {
        return Ok(Written {
            units: vec![Vec::new()],
            added: Vec::new(),
        });
    }
    let mut written = Written::default();
    let after = doc.get_heads();
    for patch in doc.diff(&before, &after) {
        use automerge::PatchAction;
        let mut keys: Vec<String> = patch.path.iter().map(|(_, prop)| key_of(prop)).collect();
        let put = matches!(&patch.action, PatchAction::PutMap { .. });
        match &patch.action {
            PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                keys.push(key.clone());
            }
            PatchAction::Increment { prop, .. } | PatchAction::Conflict { prop } => {
                keys.push(key_of(prop));
            }
            _ => {}
        }
        let (unit, depth) = unit_of(&patterns, &keys);
        if put && depth == 1 && !unit.is_empty() && !written.added.contains(&unit) {
            written.added.push(unit.clone());
        }
        if !written.units.contains(&unit) {
            written.units.push(unit);
        }
    }
    Ok(written)
}

/// What each of a received op's changes writes, and the units the record
/// keeps an index of.
#[derive(Clone, Debug, Default)]
pub struct ByChange {
    /// By change hash.
    pub written: std::collections::BTreeMap<[u8; 32], Written>,
    patterns: Vec<Vec<String>>,
}

impl ByChange {
    /// Whether the record's index names `unit` itself, so what a change
    /// writes there is told apart from the rest of the record. A record
    /// indexed before `unit` was one does not.
    #[must_use]
    pub fn indexes(&self, unit: &UnitPath) -> bool {
        let keys: Option<Vec<&str>> = unit
            .iter()
            .map(|step| match step {
                PathStep::Field(key) => Some(key.as_str()),
                _ => None,
            })
            .collect();
        keys.is_some_and(|keys| {
            self.patterns
                .iter()
                .any(|pattern| pattern.iter().map(String::as_str).eq(keys.iter().copied()))
        })
    }
}

/// Whether one of `changes` makes the record's unit index: what a record
/// that kept none gains from the first action applied to it once its app
/// has units, and what the changes after it are read by.
#[must_use]
pub fn brings_index(changes: &[Change]) -> bool {
    use automerge::legacy::{Key, ObjectId};
    changes.iter().any(|change| {
        change.decode().operations.iter().any(|op| {
            matches!(op.obj, ObjectId::Root)
                && matches!(&op.key, Key::Map(key) if key == UNIT_INDEX)
        })
    })
}

/// What each of `changes` writes, applied in order to `held`, the
/// receiver's own copy of the record (empty for a record it does not hold
/// yet). A change that does not apply is left out: it is refused for that
/// before its units matter. `None` when the record keeps no unit index even
/// after the changes: it has no units to tell apart.
#[must_use]
pub fn units_by_change(held: AutoCommit, changes: &[Change]) -> Option<ByChange> {
    let mut current = held;
    let mut written = std::collections::BTreeMap::new();
    for change in changes {
        if let Ok(units) = applying(&mut current, change) {
            written.insert(change.hash().0, units);
        }
    }
    has_units(&current).then(|| ByChange {
        written,
        patterns: patterns(&current),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;

    /// A record whose comments are units: a change to one comment writes
    /// that comment's unit, a change elsewhere the record.
    #[test]
    fn a_change_writes_the_units_its_operations_fall_under() {
        let mut doc = automerge::AutoCommit::new();
        let index = doc.put_object(ROOT, UNIT_INDEX, ObjType::List).unwrap();
        doc.insert(&index, 0, "comments/*").unwrap();
        let comments = doc.put_object(ROOT, "comments", ObjType::Map).unwrap();
        let entry = doc.put_object(&comments, "c1", ObjType::Map).unwrap();
        let value = doc.put_object(&entry, ITEM_VALUE, ObjType::Map).unwrap();
        doc.put(&value, "text", "first").unwrap();
        doc.put(ROOT, "body", "hello").unwrap();
        doc.commit();
        let before = doc.clone();

        doc.put(&value, "text", "edited").unwrap();
        let comment = doc.commit().unwrap();
        doc.put(ROOT, "body", "changed").unwrap();
        let body = doc.commit().unwrap();

        let after = doc.document().clone();
        let change = |hash| after.get_change_by_hash(&hash).unwrap();
        assert_eq!(
            written_units(&before, &change(comment)).unwrap(),
            vec![vec![
                PathStep::Field("comments".into()),
                PathStep::Item("c1".into())
            ]]
        );
        let mut at_comment = before;
        at_comment.apply_incoming([change(comment)]).unwrap();
        assert_eq!(
            written_units(&at_comment, &change(body)).unwrap(),
            vec![Vec::<PathStep>::new()]
        );
    }

    /// A record the receiver does not hold yet is read from its first
    /// changes, which carry its unit index; one that never keeps an index
    /// has no units to tell apart.
    #[test]
    fn a_new_records_units_are_read_from_its_first_changes() {
        let mut doc = automerge::AutoCommit::new();
        let index = doc.put_object(ROOT, UNIT_INDEX, ObjType::List).unwrap();
        doc.insert(&index, 0, "uploads").unwrap();
        doc.put(ROOT, "title", "Lounge").unwrap();
        let first = doc.commit().unwrap();
        let after = doc.document().clone();
        let changes = vec![after.get_change_by_hash(&first).unwrap()];
        let units = units_by_change(AutoCommit::new(), &changes).expect("an indexed record");
        assert_eq!(
            units.written.get(&first.0).map(|written| &written.units),
            Some(&vec![Vec::<PathStep>::new()])
        );
        assert!(units.indexes(&vec![PathStep::Field("uploads".into())]));
        assert!(!units.indexes(&vec![PathStep::Field("title".into())]));

        let mut plain = automerge::AutoCommit::new();
        plain.put(ROOT, "title", "Lounge").unwrap();
        let only = plain.commit().unwrap();
        let after = plain.document().clone();
        let changes = vec![after.get_change_by_hash(&only).unwrap()];
        assert!(units_by_change(AutoCommit::new(), &changes).is_none());
    }

    /// An item kept as its order key alone is the scalar under its id; it
    /// falls under its item's unit like an entry map does.
    #[test]
    fn an_item_stored_as_its_order_key_writes_its_own_unit() {
        let mut doc = automerge::AutoCommit::new();
        let index = doc.put_object(ROOT, UNIT_INDEX, ObjType::List).unwrap();
        doc.insert(&index, 0, "rows/*").unwrap();
        let rows = doc.put_object(ROOT, "rows", ObjType::Map).unwrap();
        doc.commit();
        let before = doc.clone();
        doc.put(&rows, "r1", "a0").unwrap();
        let inserted = doc.commit().unwrap();
        let change = doc.document().get_change_by_hash(&inserted).unwrap();
        assert_eq!(
            written_units(&before, &change).unwrap(),
            vec![vec![
                PathStep::Field("rows".into()),
                PathStep::Item("r1".into())
            ]]
        );
    }
}
