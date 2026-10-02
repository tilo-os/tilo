//! Automerge document model for the editor.
//!
//! Each file has a durable record in `files` and a single Automerge `Text`
//! object keyed by the same UUID under `buffers`:
//!
//! ```text
//! root
//! ├── "files" (Map, keyed by UUID) # name/parent/language/order/tombstone
//! └── "buffers" (Map, keyed by UUID)
//!     └── <file-uuid> (Text)   # the full file contents, character-addressed
//! ```
//!
//! Storing the body as `Text` (rather than a plain string scalar) is what
//! gives the editor collaborative editing and CRDT-backed undo/redo: every
//! insert/delete is a splice, so two peers typing in the same file converge,
//! and `ui::State::commit_action` can snapshot/restore the whole doc.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::doc::{read_bool, read_i64_coerced, read_str};

#[cfg(test)]
use crate::model::NodeId;

/// Stateless namespace for editor-document helpers.
pub struct EditorDoc;

/// The durable, shareable part of a file-tree item.  `local_node` IDs are an
/// implementation detail of one running tree; this record is keyed by a UUID
/// chosen by the action author and is therefore safe to put in URLs, resources,
/// search results, history, and collaboration messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollaborativeFile {
    pub id: String,
    /// `file` or `folder`; the name remains for API compatibility while this
    /// type represents the full durable project hierarchy.
    pub kind: String,
    pub name: String,
    /// Derived from the durable parent chain and `name`; it is deliberately
    /// not persisted, so concurrent rename/move actions cannot carry stale
    /// derived state into a peer's projection.
    pub path: String,
    pub parent: Option<String>,
    pub language: String,
    pub order: i64,
    pub deleted: bool,
    pub name_revision: i64,
    pub structure_revision: i64,
    pub language_revision: i64,
    pub deletion_revision: i64,
}

/// A deliberately sparse update to one durable tree record.  A distinct
/// `parent` field avoids the ambiguous `Option<Option<_>>` convention: callers
/// either leave the parent alone, set it to a resource ID, or explicitly move
/// the item to the project root.
#[derive(Clone, Copy, Default)]
pub struct FilePatch<'a> {
    pub name: Option<&'a str>,
    pub parent: ParentPatch<'a>,
    pub language: Option<&'a str>,
    pub order: Option<i64>,
    pub deleted: Option<bool>,
    pub revision_field: Option<&'a str>,
}

#[derive(Clone, Copy, Default)]
pub enum ParentPatch<'a> {
    #[default]
    Unchanged,
    Root,
    Parent(&'a str),
}

impl EditorDoc {
    /// Return the durable project identity, minting it once inside the shared
    /// document.
    pub fn ensure_project_id(doc: &mut AutoCommit, proposed: &str) -> String {
        if let Ok(Some((Value::Scalar(value), _))) = doc.get(&ROOT, "project_id")
            && let ScalarValue::Str(id) = value.as_ref()
            && uuid::Uuid::parse_str(id).is_ok()
        {
            return id.to_string();
        }
        let id = if uuid::Uuid::parse_str(proposed).is_ok() {
            proposed.to_string()
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        let _ = doc.put(&ROOT, "project_id", id.as_str());
        id
    }

    /// The durable project id, when one has been written.
    pub fn project_id(doc: &AutoCommit) -> Option<String> {
        match doc.get(&ROOT, "project_id") {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(id) if uuid::Uuid::parse_str(id).is_ok() => Some(id.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Get-or-create the durable file-record namespace. Unlike `buffers`, this
    /// is the collaborative project tree rather than a cache of editor panes.
    pub fn files_map(doc: &mut AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, "files") {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => doc.put_object(&ROOT, "files", ObjType::Map).ok(),
        }
    }

    fn derived_path(doc: &AutoCommit, parent: &str, name: &str) -> String {
        if parent.is_empty() {
            return name.to_string();
        }
        Self::file(doc, parent).map_or_else(
            || name.to_string(),
            |parent| format!("{}/{}", parent.path, name),
        )
    }

    /// Read a durable file record by its stable identity.
    pub fn file(doc: &AutoCommit, id: &str) -> Option<CollaborativeFile> {
        let Ok(Some((Value::Object(ObjType::Map), files))) = doc.get(&ROOT, "files") else {
            return None;
        };
        let Ok(Some((Value::Object(ObjType::Map), item))) = doc.get(&files, id) else {
            return None;
        };
        let parent = read_str(doc, &item, "parent").unwrap_or_default();
        let name = read_str(doc, &item, "name").unwrap_or_default();
        Some(CollaborativeFile {
            id: id.to_string(),
            kind: read_str(doc, &item, "kind").unwrap_or_default(),
            path: Self::derived_path(doc, &parent, &name),
            name,
            parent: (!parent.is_empty()).then_some(parent),
            language: read_str(doc, &item, "language").unwrap_or_default(),
            order: read_i64_coerced(doc, &item, "order").unwrap_or_default(),
            deleted: read_bool(doc, &item, "deleted").unwrap_or(false),
            name_revision: read_i64_coerced(doc, &item, "name_revision").unwrap_or_default(),
            structure_revision: read_i64_coerced(doc, &item, "structure_revision")
                .unwrap_or_default(),
            language_revision: read_i64_coerced(doc, &item, "language_revision")
                .unwrap_or_default(),
            deletion_revision: read_i64_coerced(doc, &item, "deletion_revision")
                .unwrap_or_default(),
        })
    }

    /// Return all durable records, including tombstones so a synchronizer can
    /// remove a local projection after a peer deletes it.
    pub fn files(doc: &AutoCommit) -> Vec<CollaborativeFile> {
        let Ok(Some((Value::Object(ObjType::Map), files))) = doc.get(&ROOT, "files") else {
            return Vec::new();
        };
        let mut result: Vec<_> = doc
            .keys(&files)
            .filter_map(|id| Self::file(doc, &id))
            .collect();
        result.sort_by(|a, b| {
            (a.parent.as_deref(), a.order, &a.id).cmp(&(b.parent.as_deref(), b.order, &b.id))
        });
        result
    }

    /// Create only a durable file record. This is used by the trusted
    /// filesystem-import projection; user/agent file creation must use the
    /// canonical action, which calls [`create_file`](Self::create_file) and
    /// materialises the Text as the same atomic operation.
    pub fn create_file_record(doc: &mut AutoCommit, file: &CollaborativeFile) -> bool {
        let Some(files) = Self::files_map(doc) else {
            return false;
        };
        if doc.get(&files, &file.id).ok().flatten().is_some() {
            return false;
        }
        let Ok(item) = doc.put_object(&files, &file.id, ObjType::Map) else {
            return false;
        };
        let _ = doc.put(&item, "name", file.name.as_str());
        let _ = doc.put(&item, "kind", file.kind.as_str());
        let _ = doc.put(&item, "parent", file.parent.as_deref().unwrap_or_default());
        let _ = doc.put(&item, "language", file.language.as_str());
        let _ = doc.put(&item, "order", file.order);
        let _ = doc.put(&item, "deleted", file.deleted);
        let _ = doc.put(&item, "name_revision", file.name_revision);
        let _ = doc.put(&item, "structure_revision", file.structure_revision);
        let _ = doc.put(&item, "language_revision", file.language_revision);
        let _ = doc.put(&item, "deletion_revision", file.deletion_revision);
        true
    }

    /// Create a file record and its native collaborative text object. Returns
    /// `false` for a duplicate identity; duplicate/reordered actions are then
    /// harmless rather than overwriting a peer's work.
    pub fn create_file(doc: &mut AutoCommit, file: &CollaborativeFile, seed: &str) -> bool {
        if !Self::create_file_record(doc, file) {
            return false;
        }
        Self::ensure_buffer_key(doc, &file.id, seed).is_some()
    }

    /// Update one validated structural record and bump only the supplied
    /// field-scoped revision. Callers verify that field before invoking this.
    pub fn update_file(
        doc: &mut AutoCommit,
        id: &str,
        patch: FilePatch<'_>,
    ) -> Option<CollaborativeFile> {
        Self::file(doc, id)?;
        let files = Self::files_map(doc)?;
        let Ok(Some((Value::Object(ObjType::Map), item))) = doc.get(&files, id) else {
            return None;
        };
        // A revision is an optimistic-concurrency identity, not a decorative
        // counter. Saturating here would let two different mutations carry the
        // same expected revision after exhaustion, so refuse before changing
        // any field instead.
        let next_revision = match patch.revision_field {
            Some(field) => Some(
                read_i64_coerced(doc, &item, field)
                    .unwrap_or_default()
                    .checked_add(1)
                    .filter(|next| *next >= 0)?,
            ),
            None => None,
        };
        if let Some(value) = patch.name {
            let _ = doc.put(&item, "name", value);
        }
        match patch.parent {
            ParentPatch::Unchanged => {}
            ParentPatch::Root => {
                let _ = doc.put(&item, "parent", "");
            }
            ParentPatch::Parent(value) => {
                let _ = doc.put(&item, "parent", value);
            }
        }
        if let Some(value) = patch.language {
            let _ = doc.put(&item, "language", value);
        }
        if let Some(value) = patch.order {
            let _ = doc.put(&item, "order", value);
        }
        if let Some(value) = patch.deleted {
            let _ = doc.put(&item, "deleted", value);
        }
        if let (Some(field), Some(next)) = (patch.revision_field, next_revision) {
            let _ = doc.put(&item, field, next);
        }
        Self::file(doc, id)
    }
    /// Get-or-create the top-level `buffers` Map.
    ///
    pub fn buffers_map(doc: &mut AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, "buffers") {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => doc.put_object(&ROOT, "buffers", ObjType::Map).ok(),
        }
    }

    /// The `Text` `ObjId` for `node`'s buffer, if it has been opened.
    #[cfg(test)]
    pub fn buffer_text(doc: &AutoCommit, node: NodeId) -> Option<ObjId> {
        Self::buffer_text_key(doc, &node.to_string())
    }

    /// The Text object for a durable file identity.
    pub fn buffer_text_key(doc: &AutoCommit, key: &str) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), buffers))) = doc.get(&ROOT, "buffers") else {
            return None;
        };
        match doc.get(&buffers, key) {
            Ok(Some((Value::Object(ObjType::Text), t))) => Some(t),
            _ => None,
        }
    }

    /// Get-or-create the `Text` object for `node`, seeding it with `seed` if it
    /// did not already exist. Returns the Text `ObjId`.
    ///
    #[cfg(test)]
    pub fn ensure_buffer(doc: &mut AutoCommit, node: usize, seed: &str) -> ObjId {
        // `ROOT` is only a defensive sentinel for a corrupted Automerge
        // document; ordinary callers always receive the freshly-created Text.
        Self::ensure_buffer_key(doc, &node.to_string(), seed).unwrap_or(ROOT)
    }

    /// Get-or-create text keyed by a durable resource identity.
    pub fn ensure_buffer_key(doc: &mut AutoCommit, key: &str, seed: &str) -> Option<ObjId> {
        let buffers = Self::buffers_map(doc)?;
        if let Ok(Some((Value::Object(ObjType::Text), t))) = doc.get(&buffers, key) {
            Some(t)
        } else {
            let t = doc.put_object(&buffers, key, ObjType::Text).ok()?;
            if !seed.is_empty() {
                let _ = doc.splice_text(&t, 0, 0, seed);
            }
            Some(t)
        }
    }

    /// Read a buffer's full contents.
    #[cfg(test)]
    pub fn read_buffer(doc: &AutoCommit, node: NodeId) -> Option<String> {
        Self::read_buffer_key(doc, &node.to_string())
    }

    pub fn read_buffer_key(doc: &AutoCommit, key: &str) -> Option<String> {
        Self::buffer_text_key(doc, key).and_then(|t| doc.text(&t).ok())
    }

    /// Get-or-create the Map that scopes the embedded terminal's CRDT state.
    /// The `TerminalWidget` stores its grid + cursor under whatever object it's
    /// handed as "root"; giving it a dedicated sub-map keeps its keys
    /// (`terminal_grid`, `cursor`) out of the editor's own root namespace.
    ///
    /// # Panics
    /// Never in practice: `put_object` on `ROOT` only fails for an invalid or
    /// deleted parent object, and `ROOT` is permanent for the life of the doc.
    #[allow(
        clippy::expect_used,
        reason = "ROOT is Automerge's permanent, undeletable root Map, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
    pub fn terminal_root(doc: &mut AutoCommit) -> ObjId {
        let _ = tilo_actions::Action::apply(
            &crate::actions::CodeEditorAction::EnsureTerminalPanel,
            doc,
        );
        match Self::terminal_panel(doc) {
            Some(id) => id,
            None => doc
                .put_object(&ROOT, "terminal_panel", ObjType::Map)
                .expect("put_object terminal_panel on root never fails"),
        }
    }

    /// The embedded terminal's map, when the document already has one.
    pub fn terminal_panel(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, "terminal_panel") {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    /// Splice into a buffer's Text at a character index. `delete` characters
    /// are removed, then `insert` is inserted. Indices are in *characters*
    /// (Automerge Text is char-addressed), matching the editor's caret model.
    pub fn splice(
        doc: &mut AutoCommit,
        text: &ObjId,
        char_pos: usize,
        delete: usize,
        insert: &str,
    ) {
        let _ = doc.splice_text(text, char_pos, delete as isize, insert);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming;

    #[test]
    fn project_identity_and_exhausted_file_revisions_refuse_to_overwrite() {
        // Project identity persists and does not collide.
        {
            let proposed_a = "00000000-0000-4000-8000-000000000011";
            let proposed_b = "00000000-0000-4000-8000-000000000012";
            let mut a = AutoCommit::new();
            let mut b = AutoCommit::new();
            let id_a = EditorDoc::ensure_project_id(&mut a, proposed_a);
            assert_eq!(EditorDoc::ensure_project_id(&mut a, proposed_b), id_a);
            assert_eq!(EditorDoc::ensure_project_id(&mut b, proposed_b), proposed_b);
            assert_ne!(id_a, EditorDoc::ensure_project_id(&mut b, proposed_b));

            let bytes = a.save();
            let mut peer = AutoCommit::load(&bytes).unwrap_or_else(|_| AutoCommit::new());
            assert_eq!(EditorDoc::ensure_project_id(&mut peer, proposed_b), id_a);
        }

        // An exhausted field revision refuses without partially mutating metadata.
        {
            let mut doc = AutoCommit::new();
            let file = CollaborativeFile {
                id: "00000000-0000-4000-8000-000000000101".into(),
                kind: "file".into(),
                path: "before.rs".into(),
                name: "before.rs".into(),
                parent: None,
                language: "Rust".into(),
                order: 0,
                deleted: false,
                name_revision: i64::MAX,
                structure_revision: 0,
                language_revision: 0,
                deletion_revision: 0,
            };
            assert!(EditorDoc::create_file(&mut doc, &file, "fn main() {}"));
            assert!(
                EditorDoc::update_file(
                    &mut doc,
                    &file.id,
                    FilePatch {
                        name: Some("after.rs"),
                        revision_field: Some("name_revision"),
                        ..FilePatch::default()
                    },
                )
                .is_none()
            );
            let current = EditorDoc::file(&doc, &file.id).expect("file survives refusal");
            assert_eq!(current.name, "before.rs");
            assert_eq!(current.name_revision, i64::MAX);
        }
    }

    #[test]
    fn buffers_round_trip_splice_and_converge_across_peers() {
        // Ensure and read round trip.
        {
            let mut doc = AutoCommit::new();
            let t = EditorDoc::ensure_buffer(&mut doc, 7, "hello\nworld");
            assert_eq!(doc.text(&t).unwrap(), "hello\nworld");
            assert_eq!(EditorDoc::read_buffer(&doc, 7).unwrap(), "hello\nworld");
        }

        // Ensure is idempotent.
        {
            let mut doc = AutoCommit::new();
            let _ = EditorDoc::ensure_buffer(&mut doc, 1, "first");
            // Second call with a different seed must NOT overwrite.
            let _ = EditorDoc::ensure_buffer(&mut doc, 1, "second");
            assert_eq!(EditorDoc::read_buffer(&doc, 1).unwrap(), "first");
        }

        // Splice inserts and deletes.
        {
            let mut doc = AutoCommit::new();
            let t = EditorDoc::ensure_buffer(&mut doc, 0, "abcdef");
            // Delete "cd" (chars 2..4) and insert "XY".
            EditorDoc::splice(&mut doc, &t, 2, 2, "XY");
            assert_eq!(doc.text(&t).unwrap(), "abXYef");
        }

        // Two peers converge on buffer edits.
        {
            let mut a = AutoCommit::new();
            let t = EditorDoc::ensure_buffer(&mut a, 0, "fn main() {}\n");

            // B joins from A's saved state, sharing the same Text object.
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let tb = EditorDoc::buffer_text(&b, 0).unwrap();
            assert_eq!(t, tb, "shared base => same Text ObjId");

            // A appends at the end; B inserts at the start.
            let a_len = a.text(&t).unwrap().chars().count();
            EditorDoc::splice(&mut a, &t, a_len, 0, "// tail\n");
            EditorDoc::splice(&mut b, &tb, 0, 0, "// head\n");

            let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_loaded).unwrap();
            let merged = a.text(&t).unwrap();
            assert!(merged.contains("// head"));
            assert!(merged.contains("// tail"));
            assert!(merged.contains("fn main"));
        }
    }
}
