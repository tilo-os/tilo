//! CRDT (Automerge) persistence for the Themes catalog.
//!
//! All app state lives in the shared `ui::State::doc` so undo/redo and
//! live collaboration work for free. The layout is intentionally simple
//! and collab-friendly:
//!
//! ```text
//! root
//! ├── "catalog"    (Text)  # JSON array of ThemeSpec — the saved themes
//! ├── "selected"   (Str)   # id of the focused theme
//! ├── "edit_token" (Str)   # which colour slot the picker edits
//! ├── "command"    (Str)   # one-shot command bus (consumed each frame)
//! ├── "ed_radius"  (F64)   # radius slider scratch (0..1)
//! ├── "ed_spacing" (F64)   # spacing slider scratch (0..1)
//! ├── "ed_font"    (Str)   # font dropdown scratch
//! └── "export"     (Str)   # last exported theme JSON (shown in a toast)
//! ```
//!
//! Storing the theme list as a single JSON `Text` blob mirrors the notes
//! app's Phase-1 approach: concurrent *structural* edits to the list are
//! rare for a settings app, while the per-theme scalar controls (which DO
//! get rapid concurrent edits) ride dedicated scalar fields.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::model::ThemeSpec;

pub const KEY_CATALOG: &str = "catalog";
pub const KEY_SELECTED: &str = "selected";
pub const KEY_EDIT_TOKEN: &str = "edit_token";
pub const KEY_COMMAND: &str = "command";
pub const KEY_ED_RADIUS: &str = "ed_radius";
pub const KEY_ED_SPACING: &str = "ed_spacing";
pub const KEY_ED_FONT: &str = "ed_font";
pub const KEY_EXPORT: &str = "export";

/// Read a string scalar at `key`, or `None`.
pub fn get_str(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match doc.get(root, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Read the text of a `Text` object at `key`, or `None`.
pub fn get_text(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match doc.get(root, key) {
        Ok(Some((Value::Object(ObjType::Text), id))) => doc.text(&id).ok(),
        _ => None,
    }
}

pub fn get_f64(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<f64> {
    match doc.get(root, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::F64(f) => Some(*f),
            ScalarValue::Int(i) => Some(*i as f64),
            ScalarValue::Uint(u) => Some(*u as f64),
            _ => None,
        },
        _ => None,
    }
}

pub fn put_str(doc: &mut AutoCommit, root: &ObjId, key: &str, val: &str) {
    let _ = doc.put(root, key, val);
}

pub fn put_f64(doc: &mut AutoCommit, root: &ObjId, key: &str, val: f64) {
    let _ = doc.put(root, key, val);
}

/// Replace the `Text` blob at `key` with `val`, creating it if needed.
pub fn put_text(doc: &mut AutoCommit, root: &ObjId, key: &str, val: &str) {
    let id = match doc.get(root, key) {
        Ok(Some((Value::Object(ObjType::Text), id))) => id,
        _ => doc
            .put_object(root, key, ObjType::Text)
            .expect("put_object text never fails"),
    };
    let cur = doc.text(&id).unwrap_or_default();
    if cur != val {
        let n = cur.chars().count() as isize;
        let _ = doc.splice_text(&id, 0, n, val);
    }
}

/// Decode the catalog JSON blob into a `Vec<ThemeSpec>`.
pub fn load_catalog(doc: &AutoCommit, root: &ObjId) -> Option<Vec<ThemeSpec>> {
    let json = get_text(doc, root, KEY_CATALOG)?;
    serde_json::from_str(&json).ok()
}

/// Encode and persist the whole catalog.
pub fn save_catalog(doc: &mut AutoCommit, root: &ObjId, catalog: &[ThemeSpec]) {
    let json = serde_json::to_string(catalog).unwrap_or_else(|_| "[]".into());
    put_text(doc, root, KEY_CATALOG, &json);
}

/// Read and clear the one-shot command bus. Returns the pending command
/// (empty string means "none").
pub fn take_command(doc: &mut AutoCommit, root: &ObjId) -> String {
    let cmd = get_str(doc, root, KEY_COMMAND).unwrap_or_default();
    if !cmd.is_empty() {
        let _ = doc.put(root, KEY_COMMAND, "");
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::default_catalog;
    use automerge::ROOT;

    #[test]
    fn catalog_round_trips_through_crdt() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        let cat = default_catalog();
        save_catalog(&mut doc, &root, &cat);
        let back = load_catalog(&doc, &root).unwrap();
        assert_eq!(back.len(), cat.len());
        assert_eq!(back[0].name, cat[0].name);
        assert_eq!(back.last().unwrap().builtin, false);
    }

    #[test]
    fn command_bus_consumes_once() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        put_str(&mut doc, &root, KEY_COMMAND, "save");
        assert_eq!(take_command(&mut doc, &root), "save");
        assert_eq!(take_command(&mut doc, &root), "");
    }

    #[test]
    fn scalar_helpers_round_trip() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        put_f64(&mut doc, &root, KEY_ED_RADIUS, 0.42);
        assert!((get_f64(&doc, &root, KEY_ED_RADIUS).unwrap() - 0.42).abs() < 1e-9);
        put_str(&mut doc, &root, KEY_ED_FONT, "Inter");
        assert_eq!(get_str(&doc, &root, KEY_ED_FONT).as_deref(), Some("Inter"));
    }

    #[test]
    fn two_peers_converge_on_catalog_blob() {
        let mut a = AutoCommit::new();
        save_catalog(&mut a, &ROOT, &default_catalog());
        let mut b = AutoCommit::load(&a.save()).unwrap();
        put_str(&mut b, &ROOT, KEY_SELECTED, "dracula");
        let mut b2 = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b2).unwrap();
        assert_eq!(get_str(&a, &ROOT, KEY_SELECTED).as_deref(), Some("dracula"));
    }
}
