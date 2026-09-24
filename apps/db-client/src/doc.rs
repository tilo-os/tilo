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
use ui::doc::{read_bool, read_scalar, read_str};

use crate::model::{ConnectionProfile, Engine};
use uuid::Uuid;

/// Key of the saved-connection list under the document root.
pub const CONNECTIONS_KEY: &str = "connections";
/// Collaborative saved-query collection. Query history and result contents stay
/// local because they reflect a live server and can expose data a peer cannot
/// access; saved SQL itself is intentional workspace content.
pub const SAVED_QUERIES_KEY: &str = "saved_queries";

/// A named, collaborative SQL snippet. Its id survives removal as a tombstone
/// in the Automerge list so concurrent peers do not create duplicate snippets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedQuery {
    pub id: Uuid,
    pub name: String,
    pub sql: String,
}

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
            // Removal is a tombstone, not a list deletion: stable object ids
            // let concurrent peer actions converge without resurrecting a
            // second profile. Tombstones are intentionally invisible to the
            // ordinary connection picker.
            if read_bool(doc, &row, "removed").unwrap_or(false) {
                continue;
            }
            let id = read_str(doc, &row, "id")
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_else(Uuid::new_v4);
            let engine = Engine::from_key(&read_str(doc, &row, "engine").unwrap_or_default());
            out.push(ConnectionProfile {
                id,
                name: read_str(doc, &row, "name").unwrap_or_default(),
                engine,
                host: read_str(doc, &row, "host").unwrap_or_default(),
                port: int_field(doc, &row, "port").unwrap_or(0) as u16,
                database: read_str(doc, &row, "database").unwrap_or_default(),
                user: read_str(doc, &row, "user").unwrap_or_default(),
                password: String::new(),
                file_path: read_str(doc, &row, "file_path").unwrap_or_default(),
                connected: false,
                is_sample: read_bool(doc, &row, "is_sample").unwrap_or(false),
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
                && read_str(doc, &row, "id").as_deref() == Some(target.as_str())
            {
                return Some((i, row));
            }
        }
        None
    }
}

/// Stateless view of collaborative saved queries. Only non-tombstoned entries
/// appear in ordinary UI/search results; the hidden rows retain stable ids for
/// deterministic multi-peer action replay.
pub struct SavedQueriesDoc;

impl SavedQueriesDoc {
    pub fn list(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, SAVED_QUERIES_KEY) {
            Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
            _ => None,
        }
    }

    pub fn read(doc: &AutoCommit) -> Vec<SavedQuery> {
        let Some(list) = Self::list(doc) else {
            return Vec::new();
        };
        (0..doc.length(&list))
            .filter_map(|index| match doc.get(&list, index) {
                Ok(Some((Value::Object(ObjType::Map), row)))
                    if !read_bool(doc, &row, "removed").unwrap_or(false) =>
                {
                    let id = read_str(doc, &row, "id").and_then(|id| Uuid::parse_str(&id).ok())?;
                    Some(SavedQuery {
                        id,
                        name: read_str(doc, &row, "name").unwrap_or_default(),
                        sql: read_str(doc, &row, "sql").unwrap_or_default(),
                    })
                }
                _ => None,
            })
            .collect()
    }
}

/// An `Int`, or a `Uint` reinterpreted as `i64`.
fn int_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
    match read_scalar(doc, obj, key)? {
        ScalarValue::Int(i) => Some(i),
        ScalarValue::Uint(u) => Some(u as i64),
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
    fn merges_between_peers() {
        let mut a = AutoCommit::new();
        EditorDoc::set_sql(&mut a, "SELECT");
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let sql = EditorDoc::sql_text(&mut b);
        b.splice_text(&sql, 6, 0, " 1").unwrap();
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
