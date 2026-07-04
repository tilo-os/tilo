//! Automerge document model for the diff/merge session.
//!
//! The collaboratively-editable surface is the *merged buffer* — the running
//! result of a three-way merge that the user keeps tuning. It lives as a single
//! Automerge `Text` so two peers resolving the same conflict converge, and so
//! the toolkit's undo/redo (full-CRDT snapshots) covers merge edits for free.
//!
//! ```text
//! root
//! ├── "merged"   (Text)   # the editable merged result
//! └── "session"  (Str)    # which sample/session is loaded (for late joiners)
//! ```
//!
//! The diff and three-way panels themselves are derived views over plain text
//! the app holds in memory — only the *output* of the merge is CRDT-backed,
//! mirroring how a real editor keeps the conflict markers ephemeral but the
//! buffer you save authoritative.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

/// Stateless helpers over the shared document.
pub struct MergeDoc;

impl MergeDoc {
    /// Get-or-create the top-level `merged` Text object.
    pub fn merged_text_obj(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "merged") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc
                .put_object(&ROOT, "merged", ObjType::Text)
                .expect("put_object merged Text never fails"),
        }
    }

    /// Read the merged buffer's current contents.
    pub fn merged_text(doc: &AutoCommit) -> String {
        match doc.get(&ROOT, "merged") {
            Ok(Some((Value::Object(ObjType::Text), id))) => {
                doc.text(&id).unwrap_or_default()
            }
            _ => String::new(),
        }
    }

    /// Whether the document has been seeded yet.
    pub fn has_merged(doc: &AutoCommit) -> bool {
        matches!(
            doc.get(&ROOT, "merged"),
            Ok(Some((Value::Object(ObjType::Text), _)))
        )
    }

    /// Replace the entire merged buffer with `text`. Used when the user
    /// re-resolves a hunk (take-ours/theirs/both) and the whole composed
    /// output changes. Splices the diff against the current contents so the
    /// CRDT records a minimal edit rather than a full rewrite.
    pub fn set_merged(doc: &mut AutoCommit, text: &str) {
        let id = Self::merged_text_obj(doc);
        let cur = doc.text(&id).unwrap_or_default();
        if cur == text {
            return;
        }
        let cur_chars = cur.chars().count() as isize;
        let _ = doc.splice_text(&id, 0, cur_chars, text);
    }

    /// Read the loaded session label, if any.
    pub fn session(doc: &AutoCommit) -> Option<String> {
        match doc.get(&ROOT, "session") {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                automerge::ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn set_session(doc: &mut AutoCommit, label: &str) {
        let _ = doc.put(&ROOT, "session", label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_round_trip() {
        let mut doc = AutoCommit::new();
        assert!(!MergeDoc::has_merged(&doc));
        MergeDoc::set_merged(&mut doc, "line one\nline two\n");
        assert!(MergeDoc::has_merged(&doc));
        assert_eq!(MergeDoc::merged_text(&doc), "line one\nline two\n");
    }

    #[test]
    fn set_merged_is_incremental_and_minimal() {
        let mut doc = AutoCommit::new();
        MergeDoc::set_merged(&mut doc, "a\nb\nc\n");
        let before = doc.get_heads();
        // Setting the same text is a no-op (no new change).
        MergeDoc::set_merged(&mut doc, "a\nb\nc\n");
        assert_eq!(before, doc.get_heads(), "identical set must not commit");
        MergeDoc::set_merged(&mut doc, "a\nB\nc\n");
        assert_eq!(MergeDoc::merged_text(&doc), "a\nB\nc\n");
    }

    #[test]
    fn two_peers_converge_on_merged_buffer() {
        let mut a = AutoCommit::new();
        MergeDoc::set_merged(&mut a, "shared\n");
        let mut b = AutoCommit::load(&a.save()).unwrap();
        MergeDoc::set_merged(&mut b, "shared\nfrom-b\n");
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        assert!(MergeDoc::merged_text(&a).contains("from-b"));
    }

    #[test]
    fn session_round_trip() {
        let mut doc = AutoCommit::new();
        assert!(MergeDoc::session(&doc).is_none());
        MergeDoc::set_session(&mut doc, "config.toml");
        assert_eq!(MergeDoc::session(&doc).as_deref(), Some("config.toml"));
    }
}
