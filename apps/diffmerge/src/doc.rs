//! Automerge document model for the diff/merge session.
//!
//! The collaboratively-editable surface is the *merged buffer* — the running
//! result of a three-way merge that the user keeps tuning. It lives as a single
//! Automerge `Text` so two peers resolving the same conflict converge, and so
//! the toolkit's undo/redo (full-CRDT snapshots) covers merge edits for free.
//!
//! ```text
//! root
//! ├── "merged"        (Text)   # the editable merged result
//! ├── "resolutions"   (Map)    # "<hunk index>" -> "ours" | "theirs" | "both"
//! ├── "session"       (Str)    # which sample/session is loaded (for late joiners)
//! └── "shared_merge"  (Str)    # JSON projection of the three-way inputs
//! ```
//!
//! The `resolutions` map records the per-hunk take-a-side choice a user made so
//! the chip state converges across peers and survives a reload; it is cleared
//! when a fresh set of inputs is loaded (the hunk indices no longer correspond).
//!
//! The shared merge projection carries the base/ours/theirs texts collaborators
//! need to rebuild conflict children for [`tilo_resource::publish`]. Two-way
//! local comparisons stay device-local and are never written here.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::fmt::Write as _;

use ui::doc::read_str;

use crate::actions::HunkChoice;
use crate::model::MergeModel;

/// Document keys beside the merged buffer.
pub mod keys {
    pub const SHARED_MERGE: &str = "shared_merge";
}

/// Framework resource state: merged buffer, session label, and the shared
/// three-way projection (with resolutions applied onto the model).
#[derive(Clone, Debug, Default)]
pub struct MergeState {
    pub merged: String,
    pub session: String,
    pub title: String,
    pub model: MergeModel,
}

impl MergeState {
    pub fn from_doc(doc: &AutoCommit) -> Self {
        let shared = SharedMerge::read(doc);
        let title = shared
            .as_ref()
            .map_or_else(|| "Diff & Merge".into(), SharedMerge::title);
        let mut model = shared
            .as_ref()
            .map(SharedMerge::into_model)
            .unwrap_or_default();
        apply_resolutions(&mut model, doc);
        Self {
            merged: MergeDoc::merged_text(doc),
            session: MergeDoc::session(doc).unwrap_or_default(),
            title,
            model,
        }
    }
}

/// Three-way inputs collaborators share so conflict children can be published
/// from document state. Local two-way comparisons are omitted.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedMerge {
    pub base: String,
    pub ours: String,
    pub theirs: String,
    pub base_label: String,
    pub ours_label: String,
    pub theirs_label: String,
}

/// What a merge of `ours` and `theirs` is called: the file when both sides
/// are the same file, else both names.
pub fn merge_title(ours: &str, theirs: &str) -> String {
    use crate::i18n::{t, t_args, text};
    let (ours_name, theirs_name) = (
        crate::highlight::file_name(ours),
        crate::highlight::file_name(theirs),
    );
    if ours.is_empty() && theirs.is_empty() {
        t("merge-title-empty")
    } else if ours_name == theirs_name {
        t_args("merge-title", &[("name", text(ours_name))])
    } else {
        t_args(
            "merge-title-pair",
            &[("ours", text(ours_name)), ("theirs", text(theirs_name))],
        )
    }
}

impl SharedMerge {
    pub fn title(&self) -> String {
        merge_title(&self.ours_label, &self.theirs_label)
    }

    pub fn into_model(&self) -> MergeModel {
        MergeModel::compute(&self.base, &self.ours, &self.theirs)
    }

    pub fn write(doc: &mut AutoCommit, shared: &Self) {
        let json = serde_json::to_string(shared).unwrap_or_else(|_| "{}".into());
        Self::write_json(doc, &json);
    }

    pub fn write_json(doc: &mut AutoCommit, json: &str) {
        let _ = doc.put(&ROOT, keys::SHARED_MERGE, json);
    }

    pub fn read(doc: &AutoCommit) -> Option<Self> {
        let json = read_str(doc, &ROOT, keys::SHARED_MERGE)?;
        serde_json::from_str(&json).ok()
    }

    pub fn fingerprint(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Apply recorded take-a-side choices onto a freshly computed model.
pub fn apply_resolutions(model: &mut MergeModel, doc: &AutoCommit) {
    for (hunk, choice) in MergeDoc::resolutions(doc) {
        if let (Some(h), Some(c)) = (model.hunks.get_mut(hunk), HunkChoice::from_tag(&choice))
            && h.is_change
        {
            h.resolution = c.to_resolution();
        }
    }
}

/// Stateless helpers over the shared document.
pub struct MergeDoc;

impl MergeDoc {
    /// Get-or-create the top-level `merged` Text object.
    ///
    /// # Panics
    ///
    /// Never in practice: `ROOT` is Automerge's permanent, undeletable root
    /// Map, so `put_object` on it cannot fail short of a corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "ROOT is Automerge's permanent, undeletable root Map, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
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
            Ok(Some((Value::Object(ObjType::Text), id))) => doc.text(&id).unwrap_or_default(),
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
        ui::doc::splice_text_diffed(doc, &id, text);
    }

    /// Get-or-create the top-level `resolutions` map.
    #[allow(
        clippy::expect_used,
        reason = "ROOT is Automerge's permanent, undeletable root Map, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
    fn resolutions_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "resolutions") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "resolutions", ObjType::Map)
                .expect("put_object resolutions Map never fails"),
        }
    }

    /// Record the take-a-side choice for hunk `hunk` (`"ours"`/`"theirs"`/
    /// `"both"`), so peers converge on the same chip state.
    pub fn set_resolution(doc: &mut AutoCommit, hunk: usize, choice: &str) {
        let id = Self::resolutions_map(doc);
        let _ = doc.put(&id, hunk.to_string(), choice);
    }

    /// Every recorded `(hunk index, choice)` pair, in ascending hunk order.
    pub fn resolutions(doc: &AutoCommit) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        if let Ok(Some((Value::Object(ObjType::Map), id))) = doc.get(&ROOT, "resolutions") {
            for key in doc.keys(&id) {
                let Ok(idx) = key.parse::<usize>() else {
                    continue;
                };
                if let Ok(Some((Value::Scalar(v), _))) = doc.get(&id, &key)
                    && let automerge::ScalarValue::Str(s) = v.as_ref()
                {
                    out.push((idx, s.to_string()));
                }
            }
        }
        out.sort_by_key(|(k, _)| *k);
        out
    }

    /// Drop every recorded resolution — called when a fresh set of inputs is
    /// loaded, since the old hunk indices no longer correspond.
    pub fn clear_resolutions(doc: &mut AutoCommit) {
        if let Ok(Some((Value::Object(ObjType::Map), _))) = doc.get(&ROOT, "resolutions") {
            let _ = doc.delete(&ROOT, "resolutions");
        }
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
    use tilo_automerge::ApplyIncoming;

    /// The merged-output CRDT text: round trip, identical sets commit nothing,
    /// resolves splice rather than rewrite, and two peers converge. Every phase
    /// builds its own documents.
    #[test]
    fn merged_buffer_round_trips_edits_minimally_and_converges() {
        merged_round_trip();
        set_merged_is_incremental_and_minimal();
        resolving_hunks_does_not_rewrite_the_whole_buffer();
        two_peers_converge_on_merged_buffer();
    }

    fn merged_round_trip() {
        let mut doc = AutoCommit::new();
        assert!(!MergeDoc::has_merged(&doc));
        MergeDoc::set_merged(&mut doc, "line one\nline two\n");
        assert!(MergeDoc::has_merged(&doc));
        assert_eq!(MergeDoc::merged_text(&doc), "line one\nline two\n");
    }

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

    fn resolving_hunks_does_not_rewrite_the_whole_buffer() {
        // Re-resolving a hunk hands `set_merged` the entire composed output.
        // Rewriting it wholesale costs O(len) ops and leaves O(len) tombstones
        // per resolve; the buffer must grow with the *edited span* instead.
        let mut doc = AutoCommit::new();
        let body: String = (0..400).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "line {i}");
            s
        });
        MergeDoc::set_merged(&mut doc, &body);
        let baseline = doc.save().len();

        // 40 resolves, each flipping one line in the middle of the buffer.
        for i in 0..40 {
            let edited = body.replacen("line 200\n", &format!("line 200 v{i}\n"), 1);
            MergeDoc::set_merged(&mut doc, &edited);
        }
        let grown = doc.save().len() - baseline;

        assert_eq!(
            MergeDoc::merged_text(&doc),
            body.replacen("line 200\n", "line 200 v39\n", 1),
            "the buffer must still hold the last resolution exactly"
        );
        assert!(
            grown < baseline / 4,
            "40 one-line resolves grew the document by {grown} bytes against a \
             {baseline}-byte buffer — set_merged is rewriting the whole text"
        );
    }

    fn two_peers_converge_on_merged_buffer() {
        let mut a = AutoCommit::new();
        MergeDoc::set_merged(&mut a, "shared\n");
        let mut b = AutoCommit::load(&a.save()).unwrap();
        MergeDoc::set_merged(&mut b, "shared\nfrom-b\n");
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge_incoming(&mut b_loaded).unwrap();
        assert!(MergeDoc::merged_text(&a).contains("from-b"));
    }

    /// The sidecar state next to the merged buffer: per-hunk resolutions (sorted,
    /// overwritten, cleared) and the session label. Fresh document per phase.
    #[test]
    fn resolutions_and_session_round_trip() {
        resolutions_round_trip_and_clear();
        session_round_trip();
    }

    fn resolutions_round_trip_and_clear() {
        let mut doc = AutoCommit::new();
        assert!(MergeDoc::resolutions(&doc).is_empty());
        MergeDoc::set_resolution(&mut doc, 2, "theirs");
        MergeDoc::set_resolution(&mut doc, 0, "both");
        assert_eq!(
            MergeDoc::resolutions(&doc),
            vec![(0, "both".to_string()), (2, "theirs".to_string())],
            "sorted by hunk index"
        );
        // Overwriting a hunk's choice replaces, not duplicates.
        MergeDoc::set_resolution(&mut doc, 2, "ours");
        assert_eq!(
            MergeDoc::resolutions(&doc),
            vec![(0, "both".to_string()), (2, "ours".to_string())]
        );
        MergeDoc::clear_resolutions(&mut doc);
        assert!(
            MergeDoc::resolutions(&doc).is_empty(),
            "clear drops every choice"
        );
    }

    fn session_round_trip() {
        let mut doc = AutoCommit::new();
        assert!(MergeDoc::session(&doc).is_none());
        MergeDoc::set_session(&mut doc, "config.toml");
        assert_eq!(MergeDoc::session(&doc).as_deref(), Some("config.toml"));
    }
}

/// Exact authored values used solely to invalidate unsaved external input context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthoredSnapshot {
    merged: Option<String>,
    resolutions: Option<Vec<(usize, String)>>,
    session: Option<String>,
    shared_merge: Option<String>,
}
pub(crate) fn authored_snapshot(doc: &AutoCommit) -> AuthoredSnapshot {
    AuthoredSnapshot {
        merged: MergeDoc::has_merged(doc).then(|| MergeDoc::merged_text(doc)),
        resolutions: doc
            .get(ROOT, "resolutions")
            .ok()
            .flatten()
            .map(|_| MergeDoc::resolutions(doc)),
        session: MergeDoc::session(doc),
        shared_merge: read_str(doc, &ROOT, keys::SHARED_MERGE),
    }
}
