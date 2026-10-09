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

use automerge::{Automerge, Change, ObjType, Prop, ROOT, ReadDoc, Value};
use tilo_actions::PathStep;

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
fn patterns(doc: &Automerge) -> Vec<Vec<String>> {
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

/// The unit the storage keys `keys` fall under, by `patterns`.
fn unit_of(patterns: &[Vec<String>], keys: &[String]) -> UnitPath {
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
        return Vec::new();
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
    unit
}

fn key_of(prop: &Prop) -> String {
    match prop {
        Prop::Map(key) => key.clone(),
        Prop::Seq(index) => index.to_string(),
    }
}

/// The units `change` writes when applied to `before`, the record at the
/// change's dependencies (or later, as long as it holds them). A record
/// without a unit index has only itself.
///
/// # Errors
/// A change that does not apply to `before`.
pub fn written_units(
    before: &Automerge,
    change: &Change,
) -> Result<Vec<UnitPath>, automerge::AutomergeError> {
    let mut after = before.clone();
    after.apply_changes([change.clone()])?;
    let patterns = patterns(&after);
    if patterns.is_empty() {
        return Ok(vec![Vec::new()]);
    }
    let mut units: Vec<UnitPath> = Vec::new();
    for patch in after.diff(&before.get_heads(), &after.get_heads()) {
        use automerge::PatchAction;
        let mut keys: Vec<String> = patch.path.iter().map(|(_, prop)| key_of(prop)).collect();
        match &patch.action {
            PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                keys.push(key.clone());
            }
            PatchAction::Increment { prop, .. } | PatchAction::Conflict { prop } => {
                keys.push(key_of(prop));
            }
            _ => {}
        }
        let unit = unit_of(&patterns, &keys);
        if !units.contains(&unit) {
            units.push(unit);
        }
    }
    Ok(units)
}

/// The units each of `changes` writes, applied in order to `before`, by
/// change hash. A change that does not apply is left out: it is refused
/// for that before its units matter.
#[must_use]
pub fn units_by_change(
    before: &Automerge,
    changes: &[Change],
) -> std::collections::BTreeMap<[u8; 32], Vec<UnitPath>> {
    let mut current = before.clone();
    let mut units = std::collections::BTreeMap::new();
    for change in changes {
        if let Ok(written) = written_units(&current, change) {
            units.insert(change.hash().0, written);
            let _ = current.apply_changes([change.clone()]);
        }
    }
    units
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
        let before = doc.document().clone();

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
        at_comment.apply_changes([change(comment)]).unwrap();
        assert_eq!(
            written_units(&at_comment, &change(body)).unwrap(),
            vec![Vec::<PathStep>::new()]
        );
    }
}
