//! CRDT storage for Calculator's shared artifacts.
//!
//! Window presentation (mode, selected row, calculator input, angle mode and
//! graph cameras) deliberately never enters this document. Histories and graph
//! formulas are keyed maps; a `deleted` tombstone is delete-wins.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::doc::{put_if_changed, put_text_diffed, read_bool, read_scalar, read_str_or_text};

use crate::model::{CalcModel, HistoryEntry, PlotFn};

pub const HISTORY: &str = "history";
pub const FUNCS: &str = "funcs";
pub const SURFACES: &str = "surfaces";
const SCHEMA: &str = "calculator_resources_v2";

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
pub(crate) fn ensure_map(doc: &mut AutoCommit, key: &str) -> Option<ObjId> {
    map_at(doc, key).or_else(|| doc.put_object(&ROOT, key, ObjType::Map).ok())
}

pub fn is_seeded(doc: &AutoCommit) -> bool {
    doc.get(&ROOT, SCHEMA).ok().flatten().is_some()
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
pub fn row_id_at(doc: &AutoCommit, collection: &str, index: usize) -> Option<String> {
    visible_ids(doc, collection).get(index).cloned()
}

pub(crate) fn load_history(doc: &AutoCommit) -> Vec<HistoryEntry> {
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
pub(crate) fn load_graph(doc: &AutoCommit, collection: &str) -> Vec<PlotFn> {
    let Some(rows) = map_at(doc, collection) else {
        return Vec::new();
    };
    visible_ids(doc, collection).into_iter().filter_map(|id| {
        let Some((Value::Object(ObjType::Map), row)) = doc.get(&rows, &id).ok().flatten() else { return None };
        Some(PlotFn { expr: read_str_or_text(doc, &row, "expr").unwrap_or_default(), visible: !matches!(doc.get(&row, "visible").ok().flatten(), Some((Value::Scalar(v), _)) if matches!(v.as_ref(), ScalarValue::Boolean(false))) })
    }).collect()
}
fn fixture_id(collection: &str, rank: usize) -> String {
    format!("fixture-{collection}-{rank}")
}

/// Fixture/bootstrap storage only. Live mutations use `CalculatorAction`.
pub fn store(doc: &mut AutoCommit, model: &CalcModel) {
    if !is_seeded(doc) {
        let _ = doc.put(&ROOT, SCHEMA, true);
    }
    for (collection, graph) in [(FUNCS, &model.funcs), (SURFACES, &model.surfaces)] {
        let Some(rows) = ensure_map(doc, collection) else {
            continue;
        };
        for (rank, f) in graph.iter().enumerate() {
            put_graph_fixture(doc, &rows, collection, rank, f);
        }
    }
    let Some(rows) = ensure_map(doc, HISTORY) else {
        return;
    };
    for (rank, h) in model.history.iter().enumerate() {
        let id = fixture_id(HISTORY, rank);
        let row = match doc.get(&rows, &id).ok().flatten() {
            Some((Value::Object(ObjType::Map), row)) => row,
            _ => match doc.put_object(&rows, &id, ObjType::Map) {
                Ok(row) => row,
                Err(_) => continue,
            },
        };
        put_text_diffed(doc, &row, "expr", &h.expr);
        put_text_diffed(doc, &row, "result", &h.result);
        put_if_changed(doc, &row, "rank", i64::try_from(rank).unwrap_or(i64::MAX));
        put_if_changed(doc, &row, "deleted", false);
    }
}

/// Initialize one checked New record with the live empty Calculator model.
/// Screenshot/demo fixtures never enter this path.
///
/// # Errors
///
/// Returns the history-schema validation error for the written document.
pub fn initialize_empty(doc: &mut AutoCommit) -> Result<(), String> {
    initialize(doc, &CalcModel::default())
}

/// Initialize the Calculator schema from an explicit model. Native live roots
/// pass the empty model; screenshot/demo roots may pass their explicit fixture.
///
/// # Errors
///
/// Returns the history-schema validation error for the written document.
pub fn initialize(doc: &mut AutoCommit, model: &CalcModel) -> Result<(), String> {
    store(doc, model);
    crate::framework_history::validate(doc)
}
fn put_graph_fixture(
    doc: &mut AutoCommit,
    rows: &ObjId,
    collection: &str,
    rank: usize,
    f: &PlotFn,
) {
    let id = fixture_id(collection, rank);
    let row = match doc.get(rows, &id).ok().flatten() {
        Some((Value::Object(ObjType::Map), row)) => row,
        _ => match doc.put_object(rows, &id, ObjType::Map) {
            Ok(row) => row,
            Err(_) => return,
        },
    };
    put_text_diffed(doc, &row, "expr", &f.expr);
    put_if_changed(doc, &row, "visible", f.visible);
    put_if_changed(doc, &row, "rank", i64::try_from(rank).unwrap_or(i64::MAX));
    put_if_changed(doc, &row, "deleted", false);
}

/// Overlay shared resources and retain all local window state.
pub fn load_into(doc: &AutoCommit, model: &mut CalcModel) -> bool {
    if !is_seeded(doc) {
        return false;
    }
    model.history = load_history(doc);
    model.funcs = load_graph(doc, FUNCS);
    model.surfaces = load_graph(doc, SURFACES);
    model.active_fn = model.active_fn.min(model.funcs.len().saturating_sub(1));
    model.active_surface = model
        .active_surface
        .min(model.surfaces.len().saturating_sub(1));
    true
}
pub fn load(doc: &AutoCommit) -> Option<CalcModel> {
    let mut m = CalcModel::default();
    load_into(doc, &mut m).then_some(m)
}

/// Read a checked Existing record without seeding or repairing it. It is
/// projected by the same history schema used by native version restoration.
///
/// # Errors
///
/// Returns a description of the first schema violation when the record is
/// missing its marker or holds an invalid row or field.
pub fn load_existing(doc: &AutoCommit) -> Result<CalcModel, String> {
    if !is_seeded(doc) {
        return Err("Calculator record is missing its schema marker".into());
    }
    // Validate before the lossy UI projection can skip an invalid row or
    // default an invalid field.
    crate::framework_history::validate(doc)?;
    crate::framework_history::project(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document schema: loading keeps presentation state (view, mode)
    /// local, formulas are stored as CRDT Text, and graph rows are ordered by
    /// rank with a stable-id tiebreaker. Fresh document per phase.
    #[test]
    fn document_schema_keeps_presentation_local_text_formulas_and_ranked_rows() {
        // phase presentation_is_local
        {
            let mut d = AutoCommit::new();
            store(&mut d, &CalcModel::seeded());
            let mut m = CalcModel::default();
            m.view2d.scale = 99.;
            m.mode = crate::model::Mode::Graph3d;
            assert!(load_into(&d, &mut m), "presentation: load succeeds");
            assert_eq!(m.view2d.scale, 99., "presentation: view2d scale kept");
            assert_eq!(
                m.mode,
                crate::model::Mode::Graph3d,
                "presentation: mode kept"
            );
        }
        // phase formulas_are_text
        {
            let mut d = AutoCommit::new();
            store(&mut d, &CalcModel::seeded());
            let rows = map_at(&d, FUNCS).unwrap();
            let row = d.get(&rows, "fixture-funcs-0").unwrap().unwrap().1;
            assert!(
                matches!(
                    d.get(&row, "expr"),
                    Ok(Some((Value::Object(ObjType::Text), _)))
                ),
                "formulas_are_text: expr is a Text object"
            );
        }
        // phase graph_rows_follow_rank_with_a_stable_id_tiebreaker
        {
            let mut doc = AutoCommit::new();
            let rows = ensure_map(&mut doc, FUNCS).expect("function resource map");
            for (id, rank) in [("curve-z", 2_i64), ("curve-b", 1), ("curve-a", 1)] {
                let row = doc
                    .put_object(&rows, id, ObjType::Map)
                    .expect("fixture graph row");
                put_if_changed(&mut doc, &row, "rank", rank);
                put_if_changed(&mut doc, &row, "deleted", false);
            }
            assert_eq!(
                visible_ids(&doc, FUNCS),
                vec!["curve-a", "curve-b", "curve-z"],
                "rank orders additions; id makes concurrent equal ranks deterministic"
            );
        }
    }
}
