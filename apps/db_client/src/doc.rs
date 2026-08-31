//! CRDT document model for the database client.
//!
//! Two things live in the shared Automerge document, both edited collaboratively:
//!
//! ```text
//! root
//! ├── "editor" (Map)
//! │   └── "sql" (Text)          # the live query buffer (native text path)
//! └── "connections" (List)      # saved connection profiles (via the action model)
//!     └── [i] (Map)             # id, name, engine, host, port, database, user, file_path
//! ```
//!
//! The SQL buffer is a `Text` so edits splice in-place and a second peer
//! editing the same query converges for free. The saved-connection list is a
//! collection of profile maps mutated only through [`crate::actions::DbAction`]
//! (add / remove / rename), so undo and multi-peer replay come for free too.
//! Crucially the connection **password is never written here** — it is held in
//! memory for the session only ([`crate::model::ConnectionProfile::password`]
//! is `#[serde(skip)]`), so the collaborative document carries the shape of a
//! team's databases without ever replicating a secret.
//!
//! "Which connection is active", "which result tab is showing", "which table
//! is selected", the caret position — those are per-peer view state and stay
//! in the widget/session, not the document.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{ConnectionProfile, Engine};
use uuid::Uuid;

/// Key of the saved-connection list under the document root.
pub const CONNECTIONS_KEY: &str = "connections";

/// Stateless helpers over the shared editor document.
pub struct EditorDoc;

impl EditorDoc {
    /// Get-or-create the `editor` Map under the root.
    #[allow(
        clippy::expect_used,
        reason = "ROOT is Automerge's permanent, undeletable root Map, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
    fn editor_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "editor") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "editor", ObjType::Map)
                .expect("put_object editor on root never fails"),
        }
    }

    /// Get-or-create the `sql` Text object holding the query buffer.
    ///
    /// # Panics
    /// Never in practice: `editor` is either freshly created by this call or
    /// already a live Map, so `put_object` on it only fails for an
    /// already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "editor is either freshly created by this call or already a live Map, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
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
        let Ok(Some((Value::Object(ObjType::Map), editor))) = doc.get(&ROOT, "editor") else {
            return String::new();
        };
        match doc.get(&editor, "sql") {
            Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Replace the entire buffer with `sql`. Used when seeding the default
    /// query or loading a snippet; splices the whole range so it remains a
    /// single CRDT object (no `put_object` churn).
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

/// Stateless helpers over the saved-connection list at `root["connections"]`.
///
/// Reads are shared by [`crate::app`]'s `sync` (which reconciles the live
/// session from the document each frame) and the honesty tests. Writes happen
/// only through [`crate::actions::DbAction::apply`] so every mutation is an
/// undoable, replayable action.
pub struct ConnectionsDoc;

impl ConnectionsDoc {
    /// The connection list object, if present as a List.
    pub fn list(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, CONNECTIONS_KEY) {
            Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
            _ => None,
        }
    }

    /// Read every saved connection profile. The password is always empty — it
    /// is never stored in the document — so callers refill it from their
    /// in-memory secret map before opening a live handle.
    pub fn read(doc: &AutoCommit) -> Vec<ConnectionProfile> {
        let Some(list) = Self::list(doc) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&list, i) else {
                continue;
            };
            let id = str_field(doc, &row, "id")
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_else(Uuid::new_v4);
            let engine = Engine::from_key(&str_field(doc, &row, "engine").unwrap_or_default());
            out.push(ConnectionProfile {
                id,
                name: str_field(doc, &row, "name").unwrap_or_default(),
                engine,
                host: str_field(doc, &row, "host").unwrap_or_default(),
                port: int_field(doc, &row, "port").unwrap_or(0) as u16,
                database: str_field(doc, &row, "database").unwrap_or_default(),
                user: str_field(doc, &row, "user").unwrap_or_default(),
                password: String::new(),
                file_path: str_field(doc, &row, "file_path").unwrap_or_default(),
                connected: false,
                is_sample: bool_field(doc, &row, "is_sample").unwrap_or(false),
            });
        }
        out
    }

    /// The row map + index for a profile id, if present.
    pub fn find(doc: &AutoCommit, id: Uuid) -> Option<(usize, ObjId)> {
        let list = Self::list(doc)?;
        let target = id.to_string();
        for i in 0..doc.length(&list) {
            if let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&list, i)
                && str_field(doc, &row, "id").as_deref() == Some(target.as_str())
            {
                return Some((i, row));
            }
        }
        None
    }
}

fn str_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn int_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Int(i) => Some(*i),
            ScalarValue::Uint(u) => Some(*u as i64),
            _ => None,
        },
        _ => None,
    }
}

fn bool_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<bool> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
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
