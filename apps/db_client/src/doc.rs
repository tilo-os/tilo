//! CRDT document model for the SQL editor.
//!
//! The editor buffer lives in the shared Automerge document as a `Text`
//! object so edits splice in-place, undo/redo snapshots the buffer, and a
//! second peer editing the same query converges for free:
//!
//! ```text
//! root
//! └── "editor" (Map)
//!     └── "sql" (Text)   # the live query buffer
//! ```
//!
//! "Which result tab is active", "which table is selected", the caret
//! position — those are per-peer view state and stay in the widget, not the
//! document.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value, ROOT};

/// Stateless helpers over the shared editor document.
pub struct EditorDoc;

impl EditorDoc {
    /// Get-or-create the `editor` Map under the root.
    fn editor_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "editor") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "editor", ObjType::Map)
                .expect("put_object editor on root never fails"),
        }
    }

    /// Get-or-create the `sql` Text object holding the query buffer.
    pub fn sql_text(doc: &mut AutoCommit) -> ObjId {
        let editor = Self::editor_map(doc);
        match doc.get(&editor, "sql") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc
                .put_object(&editor, "sql", ObjType::Text)
                .expect("put_object sql text never fails"),
        }
    }

    /// Read the current buffer contents (empty if not yet seeded).
    pub fn read_sql(doc: &AutoCommit) -> String {
        let editor = match doc.get(&ROOT, "editor") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return String::new(),
        };
        match doc.get(&editor, "sql") {
            Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Replace the entire buffer with `sql`. Used when seeding the default
    /// query or loading a snippet; splices the whole range so it remains a
    /// single CRDT object (no put_object churn).
    pub fn set_sql(doc: &mut AutoCommit, sql: &str) {
        let tid = Self::sql_text(doc);
        ui::doc::splice_text_diffed(doc, &tid, sql);
    }

    /// Insert `s` at char position `at`. Returns the new caret position
    /// (just past the inserted text).
    pub fn insert(doc: &mut AutoCommit, at: usize, s: &str) -> usize {
        let tid = Self::sql_text(doc);
        let len = doc.text(&tid).unwrap_or_default().chars().count();
        let at = at.min(len);
        let _ = doc.splice_text(&tid, at, 0, s);
        at + s.chars().count()
    }

    /// Delete `count` chars starting at `at`. Returns the new caret
    /// position. Clamps so out-of-range requests are harmless.
    pub fn delete(doc: &mut AutoCommit, at: usize, count: usize) -> usize {
        let tid = Self::sql_text(doc);
        let len = doc.text(&tid).unwrap_or_default().chars().count();
        if at >= len || count == 0 {
            return at.min(len);
        }
        let count = count.min(len - at);
        let _ = doc.splice_text(&tid, at, count as isize, "");
        at
    }

    /// Current buffer length in chars.
    pub fn len(doc: &AutoCommit) -> usize {
        Self::read_sql(doc).chars().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_read_round_trip() {
        let mut doc = AutoCommit::new();
        EditorDoc::set_sql(&mut doc, "SELECT 1");
        assert_eq!(EditorDoc::read_sql(&doc), "SELECT 1");
        // Replacing keeps a single object and reflects new content.
        EditorDoc::set_sql(&mut doc, "SELECT 2");
        assert_eq!(EditorDoc::read_sql(&doc), "SELECT 2");
    }

    #[test]
    fn insert_returns_advanced_caret() {
        let mut doc = AutoCommit::new();
        EditorDoc::set_sql(&mut doc, "AC");
        let caret = EditorDoc::insert(&mut doc, 1, "B");
        assert_eq!(EditorDoc::read_sql(&doc), "ABC");
        assert_eq!(caret, 2);
    }

    #[test]
    fn delete_clamps_out_of_range() {
        let mut doc = AutoCommit::new();
        EditorDoc::set_sql(&mut doc, "ABC");
        let caret = EditorDoc::delete(&mut doc, 1, 99);
        assert_eq!(EditorDoc::read_sql(&doc), "A");
        assert_eq!(caret, 1);
    }

    #[test]
    fn merges_between_peers() {
        let mut a = AutoCommit::new();
        EditorDoc::set_sql(&mut a, "SELECT");
        let mut b = AutoCommit::load(&a.save()).unwrap();
        EditorDoc::insert(&mut b, 6, " 1");
        let mut b2 = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b2).unwrap();
        assert_eq!(EditorDoc::read_sql(&a), "SELECT 1");
    }

    #[test]
    fn appending_one_char_is_a_bounded_splice() {
        // Anti-churn: growing a long buffer by one char must splice only the
        // new char, not rewrite the whole SQL.
        let mut doc = AutoCommit::new();
        let long = "S".repeat(300);
        EditorDoc::set_sql(&mut doc, &long);
        let before = doc.save().len();
        let mut longer = long.clone();
        longer.push('X');
        EditorDoc::set_sql(&mut doc, &longer);
        let grew = doc.save().len() - before;
        assert_eq!(EditorDoc::read_sql(&doc), longer, "value round-trips");
        assert!(grew < 40, "appending one char grew the doc by {grew} bytes");
    }

    #[test]
    fn repeated_set_sql_keeps_doc_bounded() {
        // set_sql previously spliced the whole buffer unconditionally; the
        // diff-splice makes an unchanged re-store a no-op.
        let mut doc = AutoCommit::new();
        let long = "Q".repeat(300);
        EditorDoc::set_sql(&mut doc, &long);
        let a = doc.save().len();
        for _ in 0..50 {
            EditorDoc::set_sql(&mut doc, &long);
        }
        let b = doc.save().len();
        assert_eq!(a, b, "re-storing the same SQL grew the document {a} -> {b}");
    }
}
