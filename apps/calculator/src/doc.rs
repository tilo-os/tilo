//! Automerge bridge for the calculator state. The in-memory [`CalcModel`]
//! is the author of truth during a frame; `store` flushes it into the CRDT
//! and `load` reads it back. Keeping the state here is what gives the app
//! undo/redo and live collaboration for free.
//!
//! Layout under `root`:
//! ```text
//! root
//! ├── "mode"     (Str)   calc | graph2d | graph3d
//! ├── "deg"      (Bool)
//! ├── "expr"     (Text)  collaborative expression line
//! ├── "ans"      (F64)
//! ├── "memory"   (F64)
//! ├── "surface"  (Str)   z = f(x, y)
//! ├── "active"   (Int)
//! ├── "history"  (List<Map { expr, result }>)
//! ├── "funcs"    (List<Map { expr, visible }>)
//! ├── "view2d"   (Map { cx, cy, scale })
//! └── "view3d"   (Map { yaw, pitch, zoom })
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{CalcModel, HistoryEntry, Mode, PlotFn, View2d, View3d};

fn get_f64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<f64> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::F64(f) => Some(*f),
            ScalarValue::Int(i) => Some(*i as f64),
            ScalarValue::Uint(u) => Some(*u as f64),
            _ => None,
        },
        _ => None,
    }
}

fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).ok(),
        _ => None,
    }
}

fn get_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<bool> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

fn map_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

/// Whether the document already holds calculator state.
pub fn is_seeded(doc: &AutoCommit) -> bool {
    doc.get(&ROOT, "mode").ok().flatten().is_some()
}

/// Rewrite a `Text` field, splicing **only the changed span**.
///
/// The naive "delete everything, reinsert everything" approach turns a single
/// keystroke into O(len) text operations and — because Automerge keeps every
/// inserted element as a tombstone forever — accumulates O(len²) ops over a
/// typing session. That makes every subsequent `store` progressively slower
/// until the app appears to freeze. Here we diff `cur` against `value` down to
/// the common prefix + suffix and splice just the middle, so appending one
/// character costs one op, not `len`.
fn put_text(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: &str) {
    let tid = match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Text), id))) => id,
        _ => doc
            .put_object(obj, key, ObjType::Text)
            .expect("put_object text"),
    };
    let cur = doc.text(&tid).unwrap_or_default();
    if cur == value {
        return;
    }
    let cur_chars: Vec<char> = cur.chars().collect();
    let new_chars: Vec<char> = value.chars().collect();
    // Longest common prefix.
    let mut p = 0;
    while p < cur_chars.len() && p < new_chars.len() && cur_chars[p] == new_chars[p] {
        p += 1;
    }
    // Longest common suffix that doesn't overlap the prefix.
    let mut s = 0;
    while s < cur_chars.len() - p
        && s < new_chars.len() - p
        && cur_chars[cur_chars.len() - 1 - s] == new_chars[new_chars.len() - 1 - s]
    {
        s += 1;
    }
    let del = (cur_chars.len() - p - s) as isize;
    let ins: String = new_chars[p..new_chars.len() - s].iter().collect();
    let _ = doc.splice_text(&tid, p, del, &ins);
}

// -- change-guarded scalar writes ----------------------------------------
//
// The old `store` blindly re-`put` every field on every call, so a single
// digit press (which only changes `expr`) still emitted ~20 ops and permanently
// grew the document. These helpers emit an op only when the value actually
// differs from what the document already holds, so a keystroke writes just the
// one span that changed.

fn put_str_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: &str) {
    if get_str(doc, obj, key).as_deref() != Some(val) {
        let _ = doc.put(obj, key, val);
    }
}

fn put_bool_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: bool) {
    if get_bool(doc, obj, key) != Some(val) {
        let _ = doc.put(obj, key, val);
    }
}

fn put_f64_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: f64) {
    // Bit-exact compare: the model is the author of truth, so only a genuine
    // change (not a re-store of the same value) should emit an op.
    if get_f64(doc, obj, key).map(f64::to_bits) != Some(val.to_bits()) {
        let _ = doc.put(obj, key, val);
    }
}

fn put_int_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: i64) {
    if get_f64(doc, obj, key) != Some(val as f64) {
        let _ = doc.put(obj, key, val);
    }
}

/// Whether the document's `history` list already equals `hist`.
fn history_matches(doc: &AutoCommit, hist: &[HistoryEntry]) -> bool {
    let Ok(Some((Value::Object(ObjType::List), lid))) = doc.get(&ROOT, "history") else {
        return false;
    };
    if doc.length(&lid) != hist.len() {
        return false;
    }
    for (i, e) in hist.iter().enumerate() {
        let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&lid, i) else {
            return false;
        };
        if get_str(doc, &row, "expr").as_deref() != Some(e.expr.as_str())
            || get_str(doc, &row, "result").as_deref() != Some(e.result.as_str())
        {
            return false;
        }
    }
    true
}

/// Whether the document's `funcs` list already equals `funcs`.
fn funcs_matches(doc: &AutoCommit, funcs: &[PlotFn]) -> bool {
    let Ok(Some((Value::Object(ObjType::List), lid))) = doc.get(&ROOT, "funcs") else {
        return false;
    };
    if doc.length(&lid) != funcs.len() {
        return false;
    }
    for (i, f) in funcs.iter().enumerate() {
        let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&lid, i) else {
            return false;
        };
        if get_str(doc, &row, "expr").as_deref() != Some(f.expr.as_str())
            || get_bool(doc, &row, "visible") != Some(f.visible)
        {
            return false;
        }
    }
    true
}

fn put_map(doc: &mut AutoCommit, obj: &ObjId, key: &str) -> ObjId {
    if let Some(id) = map_at(doc, obj, key) {
        id
    } else {
        doc.put_object(obj, key, ObjType::Map).expect("put_object map")
    }
}

fn mode_str(m: Mode) -> &'static str {
    match m {
        Mode::Calc => "calc",
        Mode::Graph2d => "graph2d",
        Mode::Graph3d => "graph3d",
    }
}

fn mode_from(s: &str) -> Mode {
    match s {
        "graph2d" => Mode::Graph2d,
        "graph3d" => Mode::Graph3d,
        _ => Mode::Calc,
    }
}

/// Flush the whole model into the document, emitting an operation **only for
/// the fields that actually changed**. This keeps a keystroke (which changes
/// just `expr`) from re-writing the entire document every frame — the churn
/// that made every subsequent `store` slower until the app froze.
pub fn store(doc: &mut AutoCommit, m: &CalcModel) {
    put_str_if_changed(doc, &ROOT, "mode", mode_str(m.mode));
    put_bool_if_changed(doc, &ROOT, "deg", m.deg);
    put_text(doc, &ROOT, "expr", &m.expr);
    put_f64_if_changed(doc, &ROOT, "ans", m.ans);
    put_f64_if_changed(doc, &ROOT, "memory", m.memory);
    put_str_if_changed(doc, &ROOT, "surface", m.surface.as_str());
    put_int_if_changed(doc, &ROOT, "active", m.active_fn as i64);

    // history / funcs: only rewrite when the list actually differs. Rewriting a
    // list wholesale on every keystroke (even unchanged) permanently grew the
    // document; the equality guard makes an unchanged history a no-op.
    if !history_matches(doc, &m.history) {
        let _ = doc.delete(&ROOT, "history");
        let hist = doc.put_object(&ROOT, "history", ObjType::List).unwrap();
        for (i, e) in m.history.iter().enumerate() {
            let row = doc.insert_object(&hist, i, ObjType::Map).unwrap();
            let _ = doc.put(&row, "expr", e.expr.as_str());
            let _ = doc.put(&row, "result", e.result.as_str());
        }
    }

    if !funcs_matches(doc, &m.funcs) {
        let _ = doc.delete(&ROOT, "funcs");
        let funcs = doc.put_object(&ROOT, "funcs", ObjType::List).unwrap();
        for (i, f) in m.funcs.iter().enumerate() {
            let row = doc.insert_object(&funcs, i, ObjType::Map).unwrap();
            let _ = doc.put(&row, "expr", f.expr.as_str());
            let _ = doc.put(&row, "visible", f.visible);
        }
    }

    let v2 = put_map(doc, &ROOT, "view2d");
    put_f64_if_changed(doc, &v2, "cx", m.view2d.cx);
    put_f64_if_changed(doc, &v2, "cy", m.view2d.cy);
    put_f64_if_changed(doc, &v2, "scale", m.view2d.scale);

    let v3 = put_map(doc, &ROOT, "view3d");
    put_f64_if_changed(doc, &v3, "yaw", m.view3d.yaw);
    put_f64_if_changed(doc, &v3, "pitch", m.view3d.pitch);
    put_f64_if_changed(doc, &v3, "zoom", m.view3d.zoom);
}

/// Read the full model from the document, if present.
pub fn load(doc: &AutoCommit) -> Option<CalcModel> {
    if !is_seeded(doc) {
        return None;
    }
    let mut m = CalcModel::default();
    m.mode = mode_from(&get_str(doc, &ROOT, "mode").unwrap_or_default());
    m.deg = get_bool(doc, &ROOT, "deg").unwrap_or(false);
    m.expr = get_str(doc, &ROOT, "expr").unwrap_or_default();
    m.ans = get_f64(doc, &ROOT, "ans").unwrap_or(0.0);
    m.memory = get_f64(doc, &ROOT, "memory").unwrap_or(0.0);
    m.surface = get_str(doc, &ROOT, "surface").unwrap_or_else(|| m.surface.clone());
    m.active_fn = get_f64(doc, &ROOT, "active").unwrap_or(0.0) as usize;

    if let Ok(Some((Value::Object(ObjType::List), hist))) = doc.get(&ROOT, "history") {
        m.history.clear();
        for i in 0..doc.length(&hist) {
            if let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&hist, i) {
                m.history.push(HistoryEntry {
                    expr: get_str(doc, &row, "expr").unwrap_or_default(),
                    result: get_str(doc, &row, "result").unwrap_or_default(),
                });
            }
        }
    }

    if let Ok(Some((Value::Object(ObjType::List), funcs))) = doc.get(&ROOT, "funcs") {
        m.funcs.clear();
        for i in 0..doc.length(&funcs) {
            if let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&funcs, i) {
                m.funcs.push(PlotFn {
                    expr: get_str(doc, &row, "expr").unwrap_or_default(),
                    visible: get_bool(doc, &row, "visible").unwrap_or(true),
                });
            }
        }
    }

    if let Some(v2) = map_at(doc, &ROOT, "view2d") {
        m.view2d = View2d {
            cx: get_f64(doc, &v2, "cx").unwrap_or(0.0),
            cy: get_f64(doc, &v2, "cy").unwrap_or(0.0),
            scale: get_f64(doc, &v2, "scale").unwrap_or(64.0),
        };
    }
    if let Some(v3) = map_at(doc, &ROOT, "view3d") {
        m.view3d = View3d {
            yaw: get_f64(doc, &v3, "yaw").unwrap_or(0.62),
            pitch: get_f64(doc, &v3, "pitch").unwrap_or(0.52),
            zoom: get_f64(doc, &v3, "zoom").unwrap_or(1.0),
        };
    }

    if m.active_fn >= m.funcs.len() {
        m.active_fn = 0;
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_crdt() {
        let mut doc = AutoCommit::new();
        let m = CalcModel::seeded();
        store(&mut doc, &m);
        let loaded = load(&doc).expect("seeded");
        assert_eq!(loaded.mode, m.mode);
        assert_eq!(loaded.expr, m.expr);
        assert_eq!(loaded.funcs.len(), m.funcs.len());
        assert_eq!(loaded.history.len(), m.history.len());
        assert_eq!(loaded.memory, m.memory);
        assert!((loaded.view2d.scale - m.view2d.scale).abs() < 1e-9);
    }

    #[test]
    fn empty_doc_loads_none() {
        let doc = AutoCommit::new();
        assert!(load(&doc).is_none());
    }

    #[test]
    fn repeated_store_of_unchanged_model_emits_no_ops() {
        // The freeze came from `store` re-writing every field each frame. With
        // change-guarded writes, storing an unchanged model must be a no-op, so
        // the serialized document does not grow at all.
        let mut doc = AutoCommit::new();
        let m = CalcModel::seeded();
        store(&mut doc, &m);
        let a = doc.save().len();
        for _ in 0..50 {
            store(&mut doc, &m);
        }
        let b = doc.save().len();
        assert_eq!(a, b, "unchanged re-store grew the document {a} -> {b}");
    }

    #[test]
    fn typing_a_long_expression_keeps_the_doc_bounded() {
        // Reproduces the freeze mechanism on the real write path: append one
        // character at a time (like a keypress) and store after each. The
        // wholesale delete+reinsert of the whole Text used to accumulate
        // O(len^2) tombstoned ops; the diff-splice keeps growth ~linear.
        let mut doc = AutoCommit::new();
        let mut m = CalcModel::default(); // empty history/funcs → isolate `expr`
        store(&mut doc, &m);
        for i in 0..500 {
            m.expr.push(if i % 5 == 4 { '+' } else { '7' });
            store(&mut doc, &m);
        }
        let size = doc.save().len();
        // Diff-splice keeps each keystroke ~1 op, so the whole document stays a
        // few bytes per typed character. The old wholesale delete+reinsert grew
        // this to tens of KB (O(len^2) tombstoned ops) at this length.
        assert!(
            size < 12 * m.expr.len(),
            "document is {size} bytes for {} typed chars — CRDT churn regressed",
            m.expr.len()
        );
        // And the round-trip value is still exactly what was typed.
        assert_eq!(load(&doc).unwrap().expr, m.expr);
    }

    #[test]
    fn appending_one_char_stores_a_single_char_edit() {
        // Directly prove `put_text` splices only the diff: after seeding a
        // known expression, appending one char must round-trip and the doc must
        // grow only a little — not by the whole (re-inserted) string.
        let mut doc = AutoCommit::new();
        let mut m = CalcModel::default();
        m.expr = "123456789012345678901234567890".into();
        store(&mut doc, &m);
        let before = doc.save().len();
        m.expr.push('5');
        store(&mut doc, &m);
        let grew = doc.save().len() - before;
        assert_eq!(load(&doc).unwrap().expr, m.expr);
        assert!(grew < 40, "appending one char grew the doc by {grew} bytes (expected a single-char splice)");
    }
}
