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
//! ├── "active"   (Int)   active 2D function row
//! ├── "active_surface" (Int)  active 3D surface row
//! ├── "history"  (List<Map { expr, result }>)
//! ├── "funcs"    (List<Map { expr, visible }>)   2D y = f(x) curves
//! ├── "surfaces" (List<Map { expr, visible }>)   3D z = f(x, y) surfaces
//! ├── "view2d"   (Map { cx, cy, scale })
//! └── "view3d"   (Map { yaw, pitch, zoom })
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::doc::{put_if_changed, put_text_diffed};

use crate::model::{CalcModel, HistoryEntry, Mode, PlotFn, View2d, View3d};

// Ints are read back as floats so a document that stored a whole number (or an
// older document that stored an index) still yields a usable value. Above 2^53
// that conversion rounds, which is why row indices are read by `get_index`
// below rather than through here.
#[allow(
    clippy::cast_precision_loss,
    reason = "the only f64 fields are `ans`/`memory`/view geometry, where a \
              rounded magnitude past 2^53 is already the displayed value"
)]
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

/// Read a row index. `store` writes these as `Int`, so this stays in the
/// integer domain: a negative or out-of-range value falls back to the first
/// row (which `load` re-checks against the list length anyway).
fn get_index(doc: &AutoCommit, obj: &ObjId, key: &str) -> usize {
    let Ok(Some((Value::Scalar(v), _))) = doc.get(obj, key) else {
        return 0;
    };
    match v.as_ref() {
        ScalarValue::Int(i) => usize::try_from(*i).unwrap_or(0),
        ScalarValue::Uint(u) => usize::try_from(*u).unwrap_or(0),
        _ => 0,
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

// -- change-guarded scalar writes ----------------------------------------
//
// The old `store` blindly re-`put` every field on every call, so a single
// digit press (which only changes `expr`) still emitted ~20 ops and permanently
// grew the document. The shared `ui::put_if_changed` emits an op only when the
// value actually differs, and `ui::put_text_diffed` splices only the changed
// span of a `Text` — so a keystroke writes just the one span that changed.
// `f64` keeps a bit-exact guard here because `NaN != NaN` would defeat the
// generic scalar guard (a `NaN` result would re-emit an op every frame).

fn put_f64_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: f64) {
    // Bit-exact compare: the model is the author of truth, so only a genuine
    // change (not a re-store of the same value) should emit an op.
    if get_f64(doc, obj, key).map(f64::to_bits) != Some(val.to_bits()) {
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

/// Reconcile the document's `funcs` list against the model **in place**: grow /
/// shrink the list to length, then update each row's fields with a
/// change-guarded write. Each formula is a diff-spliced `Text` object (see
/// [`put_text_diffed`]), so a per-character edit is a genuine CRDT op on a stable
/// object — collaborative editing / multi-cursor become possible and a keystroke
/// no longer rewrites the whole list (the old wholesale delete+reinsert both
/// broke character-level merge and churned the document each keystroke).
fn store_funcs(doc: &mut AutoCommit, key: &str, funcs: &[PlotFn]) {
    let Some(lid) = list_at_root(doc, key) else {
        return; // the document rejected the write; leave it as it was
    };
    let cur = doc.length(&lid);
    // Shrink: drop trailing rows the model no longer has (back-to-front so
    // indices stay valid).
    for i in (funcs.len()..cur).rev() {
        let _ = doc.delete(&lid, i);
    }
    // Grow: append empty rows for newly-added functions. A rejected insert
    // stops the growth — the update loop below skips any row that is missing.
    for i in cur..funcs.len() {
        if doc.insert_object(&lid, i, ObjType::Map).is_err() {
            break;
        }
    }
    // Update every row in place — the `expr` Text is spliced by diff, so an
    // unchanged formula emits no ops and a one-character edit emits one op.
    for (i, f) in funcs.iter().enumerate() {
        let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&lid, i) else {
            continue;
        };
        put_text_diffed(doc, &row, "expr", &f.expr);
        put_if_changed(doc, &row, "visible", f.visible);
    }
}

/// Read a `List<Map { expr, visible }>` (either `funcs` or `surfaces`) back
/// into a `Vec<PlotFn>`.
fn load_fn_list(doc: &AutoCommit, key: &str) -> Vec<PlotFn> {
    let mut out = Vec::new();
    if let Ok(Some((Value::Object(ObjType::List), lid))) = doc.get(&ROOT, key) {
        for i in 0..doc.length(&lid) {
            if let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&lid, i) {
                out.push(PlotFn {
                    expr: get_str(doc, &row, "expr").unwrap_or_default(),
                    visible: get_bool(doc, &row, "visible").unwrap_or(true),
                });
            }
        }
    }
    out
}

/// The list at `root[key]`, created if it is missing. `None` when the document
/// refuses the write, in which case the caller has nothing to store into.
fn list_at_root(doc: &mut AutoCommit, key: &str) -> Option<ObjId> {
    if let Ok(Some((Value::Object(ObjType::List), id))) = doc.get(&ROOT, key) {
        return Some(id);
    }
    doc.put_object(&ROOT, key, ObjType::List).ok()
}

/// The map at `obj[key]`, created if it is missing. `None` when the document
/// refuses the write.
fn put_map(doc: &mut AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    if let Some(id) = map_at(doc, obj, key) {
        return Some(id);
    }
    doc.put_object(obj, key, ObjType::Map).ok()
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
    put_if_changed(doc, &ROOT, "mode", mode_str(m.mode));
    put_if_changed(doc, &ROOT, "deg", m.deg);
    put_text_diffed(doc, &ROOT, "expr", &m.expr);
    put_f64_if_changed(doc, &ROOT, "ans", m.ans);
    put_f64_if_changed(doc, &ROOT, "memory", m.memory);
    // Row indices are bounded by the list they point into, so a value that does
    // not fit an i64 cannot name a real row — store the first row instead.
    put_if_changed(
        doc,
        &ROOT,
        "active",
        i64::try_from(m.active_fn).unwrap_or(0),
    );
    put_if_changed(
        doc,
        &ROOT,
        "active_surface",
        i64::try_from(m.active_surface).unwrap_or(0),
    );

    // history / funcs: only rewrite when the list actually differs. Rewriting a
    // list wholesale on every keystroke (even unchanged) permanently grew the
    // document; the equality guard makes an unchanged history a no-op.
    if !history_matches(doc, &m.history) {
        let _ = doc.delete(&ROOT, "history");
        if let Ok(hist) = doc.put_object(&ROOT, "history", ObjType::List) {
            for (i, e) in m.history.iter().enumerate() {
                let Ok(row) = doc.insert_object(&hist, i, ObjType::Map) else {
                    break; // the document refused the row; stop rather than skew indices
                };
                let _ = doc.put(&row, "expr", e.expr.as_str());
                let _ = doc.put(&row, "result", e.result.as_str());
            }
        }
    }

    // 2D curves and 3D surfaces are both diff-spliced `Text`-per-row lists, so
    // a per-character edit to any formula is a bounded CRDT op (collaborative
    // editing / multi-cursor) rather than a wholesale list rewrite.
    store_funcs(doc, "funcs", &m.funcs);
    store_funcs(doc, "surfaces", &m.surfaces);

    if let Some(v2) = put_map(doc, &ROOT, "view2d") {
        put_f64_if_changed(doc, &v2, "cx", m.view2d.cx);
        put_f64_if_changed(doc, &v2, "cy", m.view2d.cy);
        put_f64_if_changed(doc, &v2, "scale", m.view2d.scale);
    }

    if let Some(v3) = put_map(doc, &ROOT, "view3d") {
        put_f64_if_changed(doc, &v3, "yaw", m.view3d.yaw);
        put_f64_if_changed(doc, &v3, "pitch", m.view3d.pitch);
        put_f64_if_changed(doc, &v3, "zoom", m.view3d.zoom);
    }
}

/// Read the full model from the document, if present.
pub fn load(doc: &AutoCommit) -> Option<CalcModel> {
    if !is_seeded(doc) {
        return None;
    }
    let mut m = CalcModel {
        mode: mode_from(&get_str(doc, &ROOT, "mode").unwrap_or_default()),
        deg: get_bool(doc, &ROOT, "deg").unwrap_or(false),
        expr: get_str(doc, &ROOT, "expr").unwrap_or_default(),
        ans: get_f64(doc, &ROOT, "ans").unwrap_or(0.0),
        memory: get_f64(doc, &ROOT, "memory").unwrap_or(0.0),
        active_fn: get_index(doc, &ROOT, "active"),
        active_surface: get_index(doc, &ROOT, "active_surface"),
        ..CalcModel::default()
    };

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

    m.funcs = load_fn_list(doc, "funcs");
    m.surfaces = load_fn_list(doc, "surfaces");

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
    if m.active_surface >= m.surfaces.len() {
        m.active_surface = 0;
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
        assert_eq!(loaded.surfaces.len(), m.surfaces.len());
        assert_eq!(
            loaded.surfaces, m.surfaces,
            "surfaces round-trip byte-identically"
        );
        assert_eq!(loaded.history.len(), m.history.len());
        // Bit-exact: the memory value must survive the CRDT round-trip
        // unchanged, which an epsilon compare would not prove.
        assert_eq!(loaded.memory.to_bits(), m.memory.to_bits());
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
    fn func_expr_and_surface_are_crdt_text_objects() {
        // Every graph formula — 2D curve OR 3D surface — is a character-level
        // CRDT `Text`, not a plain `Str`, which is what makes collaborative
        // editing / multi-cursor possible.
        let mut doc = AutoCommit::new();
        let m = CalcModel::seeded();
        store(&mut doc, &m);
        for key in ["funcs", "surfaces"] {
            let Ok(Some((Value::Object(ObjType::List), lid))) = doc.get(&ROOT, key) else {
                panic!("{key} list missing");
            };
            let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&lid, 0) else {
                panic!("{key} row 0 missing")
            };
            assert!(
                matches!(
                    doc.get(&row, "expr"),
                    Ok(Some((Value::Object(ObjType::Text), _)))
                ),
                "each {key} row's expr must be a CRDT Text object"
            );
        }
    }

    #[test]
    fn editing_one_func_char_is_a_bounded_splice() {
        // Character-level CRDT + bounded churn: appending one char to a formula
        // grows the doc a little (one spliced op), not by rewriting the whole
        // funcs list — the old wholesale delete+reinsert did the latter.
        let mut doc = AutoCommit::new();
        let mut m = CalcModel {
            funcs: vec![PlotFn {
                expr: "123456789012345678901234567890".into(),
                visible: true,
            }],
            ..CalcModel::default()
        };
        store(&mut doc, &m);
        let before = doc.save().len();
        m.funcs[0].expr.push('7');
        store(&mut doc, &m);
        let grew = doc.save().len() - before;
        assert_eq!(load(&doc).unwrap().funcs[0].expr, m.funcs[0].expr);
        assert!(
            grew < 40,
            "appending one char to a formula grew the doc by {grew} bytes (expected a single-char splice)"
        );
    }

    #[test]
    fn appending_one_char_stores_a_single_char_edit() {
        // Directly prove `put_text` splices only the diff: after seeding a
        // known expression, appending one char must round-trip and the doc must
        // grow only a little — not by the whole (re-inserted) string.
        let mut doc = AutoCommit::new();
        let mut m = CalcModel {
            expr: "123456789012345678901234567890".into(),
            ..CalcModel::default()
        };
        store(&mut doc, &m);
        let before = doc.save().len();
        m.expr.push('5');
        store(&mut doc, &m);
        let grew = doc.save().len() - before;
        assert_eq!(load(&doc).unwrap().expr, m.expr);
        assert!(
            grew < 40,
            "appending one char grew the doc by {grew} bytes (expected a single-char splice)"
        );
    }
}
