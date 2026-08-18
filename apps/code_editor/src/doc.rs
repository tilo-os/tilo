//! Automerge document model for the editor.
//!
//! Each open file's text lives as a single Automerge `Text` object, keyed by
//! the file's node id under a top-level `buffers` Map:
//!
//! ```text
//! root
//! └── "buffers" (Map, keyed by NodeId string)
//!     └── <node-id> (Text)   # the full file contents, character-addressed
//! ```
//!
//! Storing the body as `Text` (rather than a plain string scalar) is what
//! gives the editor collaborative editing and CRDT-backed undo/redo: every
//! insert/delete is a splice, so two peers typing in the same file converge,
//! and `ui::State::commit_action` can snapshot/restore the whole doc.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::model::NodeId;

/// Stateless namespace for editor-document helpers.
pub struct EditorDoc;

impl EditorDoc {
    /// Get-or-create the top-level `buffers` Map.
    pub fn buffers_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "buffers") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "buffers", ObjType::Map)
                .expect("put_object buffers on root never fails"),
        }
    }

    /// The `Text` ObjId for `node`'s buffer, if it has been opened.
    pub fn buffer_text(doc: &AutoCommit, node: NodeId) -> Option<ObjId> {
        let buffers = match doc.get(&ROOT, "buffers") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return None,
        };
        match doc.get(&buffers, node.to_string().as_str()) {
            Ok(Some((Value::Object(ObjType::Text), t))) => Some(t),
            _ => None,
        }
    }

    /// Get-or-create the `Text` object for `node`, seeding it with `seed` if it
    /// did not already exist. Returns the Text ObjId.
    pub fn ensure_buffer(doc: &mut AutoCommit, node: NodeId, seed: &str) -> ObjId {
        let buffers = Self::buffers_map(doc);
        let key = node.to_string();
        match doc.get(&buffers, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Text), t))) => t,
            _ => {
                let t = doc
                    .put_object(&buffers, key.as_str(), ObjType::Text)
                    .expect("put_object buffer text never fails");
                if !seed.is_empty() {
                    let _ = doc.splice_text(&t, 0, 0, seed);
                }
                t
            }
        }
    }

    /// Read a buffer's full contents.
    pub fn read_buffer(doc: &AutoCommit, node: NodeId) -> Option<String> {
        Self::buffer_text(doc, node).and_then(|t| doc.text(&t).ok())
    }

    /// The node ids of every buffer that currently exists in the document. The
    /// document is the source of truth for which buffers exist, so this is how
    /// the app surfaces a buffer created by a Ctrl+K "New file" action or by a
    /// peer that it does not yet track in its in-memory session.
    pub fn buffer_keys(doc: &AutoCommit) -> Vec<NodeId> {
        let buffers = match doc.get(&ROOT, "buffers") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Vec::new(),
        };
        doc.keys(&buffers)
            .filter_map(|k| k.parse::<NodeId>().ok())
            .collect()
    }

    /// Get-or-create the Map that scopes the embedded terminal's CRDT state.
    /// The `TerminalWidget` stores its grid + cursor under whatever object it's
    /// handed as "root"; giving it a dedicated sub-map keeps its keys
    /// (`terminal_grid`, `cursor`) out of the editor's own root namespace.
    pub fn terminal_root(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "terminal_panel") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "terminal_panel", ObjType::Map)
                .expect("put_object terminal_panel on root never fails"),
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

    /// Replace a buffer's whole contents with `new_text`, editing only the span
    /// that actually changed.
    ///
    /// A whole-file reformat rewrites most lines the same and a few differently.
    /// Deleting every character and re-inserting the new body would work, but it
    /// tombstones the entire old buffer on every format — the O(n) churn that
    /// bloats an Automerge document and shows up as a giant, uninformative diff
    /// to a collaborator. Instead this trims the common prefix and suffix and
    /// splices only the differing middle, so formatting a file whose first and
    /// last lines are unchanged touches only the interior. The edit stays on the
    /// native collaborative-text path, so it merges with a peer and is a single
    /// undo step. No-op when the text is already identical.
    pub fn replace_text(doc: &mut AutoCommit, node: NodeId, new_text: &str) {
        let obj = Self::ensure_buffer(doc, node, "");
        let old: Vec<char> = doc.text(&obj).unwrap_or_default().chars().collect();
        let new: Vec<char> = new_text.chars().collect();

        // Longest common prefix (in chars).
        let mut prefix = 0;
        while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
            prefix += 1;
        }
        // Longest common suffix that doesn't overlap the prefix.
        let mut suffix = 0;
        while suffix < old.len() - prefix
            && suffix < new.len() - prefix
            && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
        {
            suffix += 1;
        }

        let delete = old.len() - prefix - suffix;
        let insert: String = new[prefix..new.len() - suffix].iter().collect();
        if delete == 0 && insert.is_empty() {
            return; // identical — nothing to do
        }
        let _ = doc.splice_text(&obj, prefix, delete as isize, &insert);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_and_read_round_trip() {
        let mut doc = AutoCommit::new();
        let t = EditorDoc::ensure_buffer(&mut doc, 7, "hello\nworld");
        assert_eq!(doc.text(&t).unwrap(), "hello\nworld");
        assert_eq!(EditorDoc::read_buffer(&doc, 7).unwrap(), "hello\nworld");
    }

    #[test]
    fn ensure_is_idempotent() {
        let mut doc = AutoCommit::new();
        let _ = EditorDoc::ensure_buffer(&mut doc, 1, "first");
        // Second call with a different seed must NOT overwrite.
        let _ = EditorDoc::ensure_buffer(&mut doc, 1, "second");
        assert_eq!(EditorDoc::read_buffer(&doc, 1).unwrap(), "first");
    }

    #[test]
    fn splice_inserts_and_deletes() {
        let mut doc = AutoCommit::new();
        let t = EditorDoc::ensure_buffer(&mut doc, 0, "abcdef");
        // Delete "cd" (chars 2..4) and insert "XY".
        EditorDoc::splice(&mut doc, &t, 2, 2, "XY");
        assert_eq!(doc.text(&t).unwrap(), "abXYef");
    }

    #[test]
    fn replace_text_edits_only_the_changed_middle() {
        let mut doc = AutoCommit::new();
        let obj = EditorDoc::ensure_buffer(&mut doc, 0, "fn main(){let x=1;}\n");
        // A "reformat" that keeps the first and last lines and rewrites the body.
        EditorDoc::replace_text(&mut doc, 0, "fn main() {\n    let x = 1;\n}\n");
        assert_eq!(
            EditorDoc::read_buffer(&doc, 0).unwrap(),
            "fn main() {\n    let x = 1;\n}\n"
        );

        // The edit was minimal: the shared "fn main(" prefix and trailing "}\n"
        // were untouched, so a peer that only knew the prefix still shares the
        // same Text object.
        let mut peer = AutoCommit::load(&doc.save()).unwrap();
        assert_eq!(EditorDoc::buffer_text(&peer, 0), Some(obj));
        let _ = &mut peer;
    }

    #[test]
    fn replace_text_is_a_noop_when_identical() {
        let mut doc = AutoCommit::new();
        EditorDoc::ensure_buffer(&mut doc, 0, "same\n");
        let before = doc.get_heads();
        EditorDoc::replace_text(&mut doc, 0, "same\n");
        assert_eq!(doc.get_heads(), before, "no change ⇒ no new edit");
    }

    #[test]
    fn two_peers_converge_on_buffer_edits() {
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
        a.merge(&mut b_loaded).unwrap();
        let merged = a.text(&t).unwrap();
        assert!(merged.contains("// head"));
        assert!(merged.contains("// tail"));
        assert!(merged.contains("fn main"));
    }
}
