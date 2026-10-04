//! CRDT storage for Calculator's shared artifacts.
//!
//! Window presentation (mode, selected row, calculator input, angle mode and
//! graph cameras) deliberately never enters this document. Histories and graph
//! formulas are keyed maps; a `deleted` tombstone is delete-wins.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::doc::{read_bool, read_scalar, read_str_or_text};

use crate::model::{HistoryEntry, PlotFn};

pub const HISTORY: &str = "history";
pub const FUNCS: &str = "funcs";
pub const SURFACES: &str = "surfaces";

/// An `Int`, or a `Uint` saturated to `i64::MAX`; 0 when absent.
fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> i64 {
    match read_scalar(doc, obj, key) {
        Some(ScalarValue::Int(n)) => n,
        Some(ScalarValue::Uint(n)) => i64::try_from(n).unwrap_or(i64::MAX),
        _ => 0,
    }
}
fn map_at(doc: &AutoCommit, key: &str) -> Option<ObjId> {
    match doc.get(&ROOT, key).ok().flatten() {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

/// Stable IDs in presentation order. The ordinal is only for display; callers
/// must put this returned ID in actions.
pub fn visible_ids(doc: &AutoCommit, collection: &str) -> Vec<String> {
    let Some(rows) = map_at(doc, collection) else {
        return Vec::new();
    };
    let mut ids: Vec<(i64, String)> = doc
        .keys(&rows)
        .filter_map(|id| {
            let Some((Value::Object(ObjType::Map), row)) = doc.get(&rows, &id).ok().flatten()
            else {
                return None;
            };
            (!read_bool(doc, &row, "deleted").unwrap_or(false))
                .then(|| (get_i64(doc, &row, "rank"), id))
        })
        .collect();
    if collection == HISTORY {
        // Newest first. The stable id breaks a rank collision between two
        // independent replicas without relying on map iteration order.
        ids.sort_by(|a, b| b.cmp(a));
    } else {
        // Graph additions are authored as an ordered append (`rank` is part of
        // AddGraph's durable payload). Sorting by the id discarded that intent
        // and could reorder same-tick ids lexically (…-10 before …-9). Keep a
        // deterministic id tie-breaker for concurrent equal ranks.
        ids.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    }
    ids.into_iter().map(|(_, id)| id).collect()
}

pub fn load_history(doc: &AutoCommit) -> Vec<HistoryEntry> {
    let Some(rows) = map_at(doc, HISTORY) else {
        return Vec::new();
    };
    visible_ids(doc, HISTORY)
        .into_iter()
        .filter_map(|id| {
            let Some((Value::Object(ObjType::Map), row)) = doc.get(&rows, &id).ok().flatten()
            else {
                return None;
            };
            Some(HistoryEntry {
                expr: read_str_or_text(doc, &row, "expr").unwrap_or_default(),
                result: read_str_or_text(doc, &row, "result").unwrap_or_default(),
            })
        })
        .collect()
}
pub fn load_graph(doc: &AutoCommit, collection: &str) -> Vec<PlotFn> {
    let Some(rows) = map_at(doc, collection) else {
        return Vec::new();
    };
    visible_ids(doc, collection).into_iter().filter_map(|id| {
        let Some((Value::Object(ObjType::Map), row)) = doc.get(&rows, &id).ok().flatten() else { return None };
        Some(PlotFn { expr: read_str_or_text(doc, &row, "expr").unwrap_or_default(), visible: !matches!(doc.get(&row, "visible").ok().flatten(), Some((Value::Scalar(v), _)) if matches!(v.as_ref(), ScalarValue::Boolean(false))) })
    }).collect()
}
/// Write the empty lists of a new record: the same change on every device.
///
/// # Errors
///
/// Returns the document's write error.
pub fn initialize(doc: &mut AutoCommit) -> Result<(), String> {
    for collection in [HISTORY, FUNCS, SURFACES] {
        if map_at(doc, collection).is_none() {
            doc.put_object(&ROOT, collection, ObjType::Map)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{CalculatorAction, GraphKind};
    use tilo_actions::Action as _;

    fn add(doc: &mut AutoCommit, id: &str, rank: i64, expr: &str) {
        CalculatorAction::AddGraph {
            kind: GraphKind::Curve2d,
            id: id.into(),
            rank,
            expr: expr.into(),
        }
        .apply(doc)
        .unwrap();
    }

    /// Formulas are stored as CRDT Text, and graph rows are ordered by rank
    /// with a stable-id tiebreaker.
    #[test]
    fn formulas_are_text_and_rows_follow_rank_with_a_stable_id_tiebreaker() {
        let mut doc = AutoCommit::new();
        initialize(&mut doc).unwrap();
        for (id, rank) in [("curve-z", 2_i64), ("curve-b", 1), ("curve-a", 1)] {
            add(&mut doc, id, rank, "x^2");
        }
        assert_eq!(
            visible_ids(&doc, FUNCS),
            vec!["curve-a", "curve-b", "curve-z"],
            "rank orders additions; id makes concurrent equal ranks deterministic"
        );
        let rows = map_at(&doc, FUNCS).unwrap();
        let row = doc.get(&rows, "curve-a").unwrap().unwrap().1;
        assert!(
            matches!(
                doc.get(&row, "expr"),
                Ok(Some((Value::Object(ObjType::Text), _)))
            ),
            "the formula is a Text object"
        );
        assert_eq!(load_graph(&doc, FUNCS).len(), 3);
    }
}
