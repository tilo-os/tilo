//! The editable table's model: the document layout a
//! [`Table`](crate::widget::table::Table) shows, and the operations its owner
//! applies inside its own action ([`apply`] with the widget's
//! [`TableEdit`]s). The widget itself only reads the document.

use automerge::{
    AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value, transaction::Transactable,
};

/// A column's width before anyone resizes it.
pub(crate) const DEFAULT_COL_W: f64 = 152.0;
/// Columns never shrink below this when dragged.
pub(crate) const MIN_COL_W: f64 = 56.0;

// ---------------------------------------------------------------------------
// CRDT keys
// ---------------------------------------------------------------------------

const K_ROWS: &str = "rows";
const K_COLS: &str = "cols";
const K_CELLS: &str = "cells";
const K_WIDTHS: &str = "widths";
const K_HEADER: &str = "header";
const K_SEQ: &str = "seq";

// ===========================================================================
// Model operations — the production path. These are pure functions over the
// document, so tests exercise exactly what the widget's event handler runs.
// ===========================================================================

/// Cell key for `(row_id, col_id)`. Row/col ids are `"{actor}.{n}"`, which
/// never contain `|`, so this concatenation is unambiguous.
pub(crate) fn cell_key(rid: &str, cid: &str) -> String {
    format!("{rid}|{cid}")
}

pub(crate) fn child_obj(doc: &AutoCommit, parent: &ObjId, key: &str, ty: ObjType) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(t), id))) if t == ty => Some(id),
        _ => None,
    }
}

fn rows_list(doc: &AutoCommit, t: &ObjId) -> Option<ObjId> {
    child_obj(doc, t, K_ROWS, ObjType::List)
}
fn cols_list(doc: &AutoCommit, t: &ObjId) -> Option<ObjId> {
    child_obj(doc, t, K_COLS, ObjType::List)
}
pub(crate) fn cells_map(doc: &AutoCommit, t: &ObjId) -> Option<ObjId> {
    child_obj(doc, t, K_CELLS, ObjType::Map)
}
fn widths_map(doc: &AutoCommit, t: &ObjId) -> Option<ObjId> {
    child_obj(doc, t, K_WIDTHS, ObjType::Map)
}

/// Create the `rows` / `cols` / `cells` / `widths` containers if the table has
/// never been initialised. Idempotent — safe to call every `sync`.
pub fn ensure(doc: &mut AutoCommit, t: &ObjId) {
    if rows_list(doc, t).is_none() {
        let _ = doc.put_object(t, K_ROWS, ObjType::List);
    }
    if cols_list(doc, t).is_none() {
        let _ = doc.put_object(t, K_COLS, ObjType::List);
    }
    if cells_map(doc, t).is_none() {
        let _ = doc.put_object(t, K_CELLS, ObjType::Map);
    }
    if widths_map(doc, t).is_none() {
        let _ = doc.put_object(t, K_WIDTHS, ObjType::Map);
    }
}

fn list_ids(doc: &AutoCommit, list: &ObjId) -> Vec<String> {
    let n = doc.length(list);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        if let Ok(Some((Value::Scalar(s), _))) = doc.get(list, i)
            && let ScalarValue::Str(v) = s.as_ref()
        {
            out.push(v.to_string());
        }
    }
    out
}

/// Row ids in display order.
pub fn row_ids(doc: &AutoCommit, t: &ObjId) -> Vec<String> {
    rows_list(doc, t)
        .map(|l| list_ids(doc, &l))
        .unwrap_or_default()
}

/// Column ids in display order.
pub fn col_ids(doc: &AutoCommit, t: &ObjId) -> Vec<String> {
    cols_list(doc, t)
        .map(|l| list_ids(doc, &l))
        .unwrap_or_default()
}

/// `(rows, cols)` dimensions.
pub fn dims(doc: &AutoCommit, t: &ObjId) -> (usize, usize) {
    (
        rows_list(doc, t).map_or(0, |l| doc.length(&l)),
        cols_list(doc, t).map_or(0, |l| doc.length(&l)),
    )
}

/// Mint a globally-unique, stable id: `"{actor_hex}.{seq}"`. The actor prefix
/// keeps ids distinct across replicas; the counter keeps them distinct within
/// one.
fn next_id(doc: &mut AutoCommit, t: &ObjId) -> String {
    let seq = match doc.get(t, K_SEQ) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Int(n) => *n,
            ScalarValue::Uint(n) => i64::try_from(*n).unwrap_or(i64::MAX),
            _ => 0,
        },
        _ => 0,
    };
    // A monotonic per-document counter; saturate rather than wrap on the
    // practically-unreachable chance it ever hits `i64::MAX`.
    let _ = doc.put(t, K_SEQ, seq.saturating_add(1));
    let actor = doc.get_actor().to_hex_string();
    format!("{actor}.{seq}")
}

/// Resolve the `Text` object backing cell `(r, c)` in display coordinates.
pub fn cell_obj(doc: &AutoCommit, t: &ObjId, r: usize, c: usize) -> Option<ObjId> {
    let rid = row_ids(doc, t).into_iter().nth(r)?;
    let cid = col_ids(doc, t).into_iter().nth(c)?;
    child_obj(
        doc,
        &cells_map(doc, t)?,
        &cell_key(&rid, &cid),
        ObjType::Text,
    )
}

/// Current text of cell `(r, c)`; empty string when the cell has no content.
pub fn cell_text(doc: &AutoCommit, t: &ObjId, r: usize, c: usize) -> String {
    match cell_obj(doc, t, r, c) {
        Some(obj) => doc.text(&obj).unwrap_or_default(),
        None => String::new(),
    }
}

/// Replace the whole content of cell `(row, col)`. Creates the backing
/// `Text` if it does not exist yet.
pub fn set_cell_text(doc: &mut AutoCommit, t: &ObjId, row: usize, col: usize, text: &str) {
    ensure(doc, t);
    let (Some(rid), Some(cid)) = (
        row_ids(doc, t).into_iter().nth(row),
        col_ids(doc, t).into_iter().nth(col),
    ) else {
        return;
    };
    let Some(cells) = cells_map(doc, t) else {
        return;
    };
    let key = cell_key(&rid, &cid);
    let obj = match child_obj(doc, &cells, &key, ObjType::Text) {
        Some(obj) => obj,
        None => match doc.put_object(&cells, key.as_str(), ObjType::Text) {
            Ok(obj) => obj,
            Err(_) => return,
        },
    };
    crate::doc::splice_text_diffed(doc, &obj, text);
}

/// Insert a fresh empty row at display index `at` (clamped to `0..=rows`).
/// Every current column gets an empty backing cell.
pub fn insert_row(doc: &mut AutoCommit, t: &ObjId, at: usize) {
    ensure(doc, t);
    let rid = next_id(doc, t);
    let cols = col_ids(doc, t);
    let (Some(rows), Some(cells)) = (rows_list(doc, t), cells_map(doc, t)) else {
        return;
    };
    let at = at.min(doc.length(&rows));
    let _ = doc.insert(&rows, at, rid.as_str());
    for cid in &cols {
        let _ = doc.put_object(&cells, cell_key(&rid, cid).as_str(), ObjType::Text);
    }
}

/// Insert a fresh empty column at display index `at` (clamped to `0..=cols`).
pub fn insert_column(doc: &mut AutoCommit, t: &ObjId, at: usize) {
    ensure(doc, t);
    let cid = next_id(doc, t);
    let rows = row_ids(doc, t);
    let (Some(cols), Some(cells)) = (cols_list(doc, t), cells_map(doc, t)) else {
        return;
    };
    let at = at.min(doc.length(&cols));
    let _ = doc.insert(&cols, at, cid.as_str());
    for rid in &rows {
        let _ = doc.put_object(&cells, cell_key(rid, &cid).as_str(), ObjType::Text);
    }
    if let Some(w) = widths_map(doc, t) {
        let _ = doc.put(&w, cid.as_str(), DEFAULT_COL_W);
    }
}

/// Remove the row at display index `at`, deleting its cells.
pub fn remove_row(doc: &mut AutoCommit, t: &ObjId, at: usize) {
    let rows_v = row_ids(doc, t);
    let Some(rid) = rows_v.get(at).cloned() else {
        return;
    };
    let cols = col_ids(doc, t);
    if let Some(rows) = rows_list(doc, t) {
        let _ = doc.delete(&rows, at);
    }
    if let Some(cells) = cells_map(doc, t) {
        for cid in &cols {
            let _ = doc.delete(&cells, cell_key(&rid, cid).as_str());
        }
    }
}

/// Remove the column at display index `at`, deleting its cells + width.
pub fn remove_column(doc: &mut AutoCommit, t: &ObjId, at: usize) {
    let cols_v = col_ids(doc, t);
    let Some(cid) = cols_v.get(at).cloned() else {
        return;
    };
    let rows = row_ids(doc, t);
    if let Some(cols) = cols_list(doc, t) {
        let _ = doc.delete(&cols, at);
    }
    if let Some(cells) = cells_map(doc, t) {
        for rid in &rows {
            let _ = doc.delete(&cells, cell_key(rid, &cid).as_str());
        }
    }
    if let Some(w) = widths_map(doc, t) {
        let _ = doc.delete(&w, cid.as_str());
    }
}

/// Whether the first row renders as a header band.
pub fn header(doc: &AutoCommit, t: &ObjId) -> bool {
    matches!(
        doc.get(t, K_HEADER),
        Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), ScalarValue::Boolean(true))
    )
}

pub fn set_header(doc: &mut AutoCommit, t: &ObjId, on: bool) {
    ensure(doc, t);
    let _ = doc.put(t, K_HEADER, on);
}

pub(crate) fn col_width(doc: &AutoCommit, t: &ObjId, cid: &str) -> f64 {
    match widths_map(doc, t).and_then(|w| doc.get(&w, cid).ok().flatten()) {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::F64(v) => *v,
            // Widen via `i32` first: that conversion to `f64` is always
            // lossless, unlike a direct `i64 -> f64` cast.
            ScalarValue::Int(v) => {
                f64::from(i32::try_from(*v).unwrap_or(if *v > 0 { i32::MAX } else { i32::MIN }))
            }
            _ => DEFAULT_COL_W,
        },
        _ => DEFAULT_COL_W,
    }
}

fn set_col_width(doc: &mut AutoCommit, t: &ObjId, cid: &str, w: f64) {
    ensure(doc, t);
    if let Some(m) = widths_map(doc, t) {
        let _ = doc.put(&m, cid, w.max(MIN_COL_W));
    }
}

/// A change the user made to a table, in display coordinates, for its owner
/// to submit as an action.
#[derive(Clone, Debug, PartialEq)]
pub enum TableEdit {
    /// Cell `(row, column)` now reads `text`.
    SetCell {
        row: usize,
        column: usize,
        text: String,
    },
    /// A new empty row at `at`.
    InsertRow { at: usize },
    /// A new empty column at `at`.
    InsertColumn { at: usize },
    /// Row `at` removed.
    RemoveRow { at: usize },
    /// Column `at` removed.
    RemoveColumn { at: usize },
    /// Column `column` is `width` logical pixels wide.
    ResizeColumn { column: usize, width: f64 },
}

/// Apply `edit` to the table at `t`: what an owner's action does with a
/// [`TableEdit`] the widget handed it.
pub fn apply(doc: &mut AutoCommit, t: &ObjId, edit: &TableEdit) {
    match edit {
        TableEdit::SetCell { row, column, text } => set_cell_text(doc, t, *row, *column, text),
        TableEdit::InsertRow { at } => insert_row(doc, t, *at),
        TableEdit::InsertColumn { at } => insert_column(doc, t, *at),
        TableEdit::RemoveRow { at } => remove_row(doc, t, *at),
        TableEdit::RemoveColumn { at } => remove_column(doc, t, *at),
        TableEdit::ResizeColumn { column, width } => {
            if let Some(cid) = col_ids(doc, t).into_iter().nth(*column) {
                set_col_width(doc, t, &cid, *width);
            }
        }
    }
}
