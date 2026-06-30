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

/// Rewrite a `Text` field, splicing only the diff to keep churn down.
fn put_text(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: &str) {
    let tid = match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Text), id))) => id,
        _ => doc
            .put_object(obj, key, ObjType::Text)
            .expect("put_object text"),
    };
    let cur = doc.text(&tid).unwrap_or_default();
    if cur != value {
        let len = cur.chars().count() as isize;
        let _ = doc.splice_text(&tid, 0, len, value);
    }
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

/// Flush the whole model into the document (idempotent diff for `expr`).
pub fn store(doc: &mut AutoCommit, m: &CalcModel) {
    let _ = doc.put(&ROOT, "mode", mode_str(m.mode));
    let _ = doc.put(&ROOT, "deg", m.deg);
    put_text(doc, &ROOT, "expr", &m.expr);
    let _ = doc.put(&ROOT, "ans", m.ans);
    let _ = doc.put(&ROOT, "memory", m.memory);
    let _ = doc.put(&ROOT, "surface", m.surface.as_str());
    let _ = doc.put(&ROOT, "active", m.active_fn as i64);

    // history (rewrite wholesale — newest-first, bounded list)
    let _ = doc.delete(&ROOT, "history");
    let hist = doc.put_object(&ROOT, "history", ObjType::List).unwrap();
    for (i, e) in m.history.iter().enumerate() {
        let row = doc.insert_object(&hist, i, ObjType::Map).unwrap();
        let _ = doc.put(&row, "expr", e.expr.as_str());
        let _ = doc.put(&row, "result", e.result.as_str());
    }

    // funcs (rewrite wholesale)
    let _ = doc.delete(&ROOT, "funcs");
    let funcs = doc.put_object(&ROOT, "funcs", ObjType::List).unwrap();
    for (i, f) in m.funcs.iter().enumerate() {
        let row = doc.insert_object(&funcs, i, ObjType::Map).unwrap();
        let _ = doc.put(&row, "expr", f.expr.as_str());
        let _ = doc.put(&row, "visible", f.visible);
    }

    let v2 = put_map(doc, &ROOT, "view2d");
    let _ = doc.put(&v2, "cx", m.view2d.cx);
    let _ = doc.put(&v2, "cy", m.view2d.cy);
    let _ = doc.put(&v2, "scale", m.view2d.scale);

    let v3 = put_map(doc, &ROOT, "view3d");
    let _ = doc.put(&v3, "yaw", m.view3d.yaw);
    let _ = doc.put(&v3, "pitch", m.view3d.pitch);
    let _ = doc.put(&v3, "zoom", m.view3d.zoom);
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
}
