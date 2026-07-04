//! Persistent UI settings, stored in the shared Automerge document.
//!
//! Selection and transient view state live in the widget (they are
//! ephemeral and per-instance), but durable preferences — which diff
//! layout the user prefers, whether whitespace is shown, the default
//! history author filter — belong in the CRDT so they survive restarts
//! and replicate to collaborators. This module is the typed accessor in
//! front of the raw `root.settings` map.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

/// How the diff pane lays out old vs. new content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLayout {
    /// Single column with +/- gutter markers.
    Unified,
    /// Two columns: removed on the left, added on the right.
    SideBySide,
}

impl DiffLayout {
    fn as_str(self) -> &'static str {
        match self {
            DiffLayout::Unified => "unified",
            DiffLayout::SideBySide => "split",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "split" => DiffLayout::SideBySide,
            _ => DiffLayout::Unified,
        }
    }
}

/// Typed snapshot of the durable settings, read from / written to the doc.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub diff_layout: DiffLayout,
    pub show_whitespace: bool,
    pub syntax_highlight: bool,
    pub wrap_lines: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            diff_layout: DiffLayout::Unified,
            show_whitespace: false,
            syntax_highlight: true,
            wrap_lines: false,
        }
    }
}

const KEY: &str = "settings";

fn settings_obj(doc: &mut AutoCommit) -> ObjId {
    match doc.get(&automerge::ROOT, KEY) {
        Ok(Some((Value::Object(ObjType::Map), id))) => id,
        _ => doc
            .put_object(&automerge::ROOT, KEY, ObjType::Map)
            .expect("put settings map"),
    }
}

fn get_bool(doc: &AutoCommit, obj: &ObjId, key: &str, default: bool) -> bool {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Boolean(b) => *b,
            _ => default,
        },
        _ => default,
    }
}

fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    }
}

impl Settings {
    /// Read the current settings out of the document, falling back to the
    /// defaults for any unset key. Creating the map if absent so the first
    /// read also seeds it.
    pub fn read(doc: &mut AutoCommit) -> Self {
        let obj = settings_obj(doc);
        let def = Settings::default();
        let layout = get_str(doc, &obj, "diff_layout")
            .map(|s| DiffLayout::from_str(&s))
            .unwrap_or(def.diff_layout);
        Self {
            diff_layout: layout,
            show_whitespace: get_bool(doc, &obj, "show_whitespace", def.show_whitespace),
            syntax_highlight: get_bool(doc, &obj, "syntax_highlight", def.syntax_highlight),
            wrap_lines: get_bool(doc, &obj, "wrap_lines", def.wrap_lines),
        }
    }

    /// Persist this snapshot into the document.
    pub fn write(&self, doc: &mut AutoCommit) {
        let obj = settings_obj(doc);
        let _ = doc.put(&obj, "diff_layout", self.diff_layout.as_str());
        let _ = doc.put(&obj, "show_whitespace", self.show_whitespace);
        let _ = doc.put(&obj, "syntax_highlight", self.syntax_highlight);
        let _ = doc.put(&obj, "wrap_lines", self.wrap_lines);
    }

    pub fn toggle_layout(&mut self) {
        self.diff_layout = match self.diff_layout {
            DiffLayout::Unified => DiffLayout::SideBySide,
            DiffLayout::SideBySide => DiffLayout::Unified,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_doc() {
        let mut doc = AutoCommit::new();
        let mut s = Settings::default();
        s.diff_layout = DiffLayout::SideBySide;
        s.show_whitespace = true;
        s.syntax_highlight = false;
        s.write(&mut doc);

        let back = Settings::read(&mut doc);
        assert_eq!(back.diff_layout, DiffLayout::SideBySide);
        assert!(back.show_whitespace);
        assert!(!back.syntax_highlight);
    }

    #[test]
    fn defaults_when_unset() {
        let mut doc = AutoCommit::new();
        let s = Settings::read(&mut doc);
        assert_eq!(s.diff_layout, DiffLayout::Unified);
        assert!(s.syntax_highlight);
    }
}
