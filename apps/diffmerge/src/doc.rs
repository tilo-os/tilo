//! The diff/merge session's document.
//!
//! What is open — the two files of a comparison, the three inputs of a merge
//! — and the merge result everyone edits live in one document, so a
//! collaborator, a reload and history see the same session:
//!
//! ```text
//! root
//! ├── "comparison"   (Map)    # left_label, left, right_label, right
//! ├── "merge"        (Map)    # base, ours, theirs and their labels
//! ├── "merged"       (Text)   # the editable merged result
//! └── "resolutions"  (Map)    # "<hunk index>" -> "ours" | "theirs" | "both"
//! ```
//!
//! Opening inputs replaces their map whole, so two people opening different
//! files at once end with one of the two sets, never a mix. The merged result
//! is a text two people type into character by character. `resolutions`
//! records the side taken for each hunk; opening a new merge clears it, since
//! the old hunk indices no longer correspond.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use ui::doc::read_str;

use crate::actions::HunkChoice;
use crate::model::{DiffModel, MergeModel};

/// Document keys.
pub mod keys {
    pub const COMPARISON: &str = "comparison";
    pub const MERGE: &str = "merge";
    pub const MERGED: &str = "merged";
    pub const RESOLUTIONS: &str = "resolutions";
}

/// The two files of a two-way comparison.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    pub left_label: String,
    pub left: String,
    pub right_label: String,
    pub right: String,
}

impl Comparison {
    const FIELDS: [&'static str; 4] = ["left_label", "left", "right_label", "right"];

    fn values(&self) -> [&str; 4] {
        [&self.left_label, &self.left, &self.right_label, &self.right]
    }

    fn read(doc: &AutoCommit) -> Option<Self> {
        let [left_label, left, right_label, right] =
            read_fields(doc, keys::COMPARISON, Self::FIELDS)?;
        Some(Self {
            left_label,
            left,
            right_label,
            right,
        })
    }

    pub(crate) fn write(&self, doc: &mut AutoCommit) -> Result<(), automerge::AutomergeError> {
        write_fields(doc, keys::COMPARISON, Self::FIELDS, self.values())
    }

    /// The comparison's line-aligned diff.
    pub fn model(&self) -> DiffModel {
        DiffModel::compute(&self.left, &self.right)
    }

    /// `left ↔ right`, as the palette names the comparison.
    pub fn title(&self) -> String {
        if self.left_label.is_empty() && self.right_label.is_empty() {
            "Comparison".into()
        } else {
            format!("{} ↔ {}", self.left_label, self.right_label)
        }
    }
}

/// The three inputs of a three-way merge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeInputs {
    pub base_label: String,
    pub base: String,
    pub ours_label: String,
    pub ours: String,
    pub theirs_label: String,
    pub theirs: String,
}

impl MergeInputs {
    const FIELDS: [&'static str; 6] = [
        "base_label",
        "base",
        "ours_label",
        "ours",
        "theirs_label",
        "theirs",
    ];

    fn values(&self) -> [&str; 6] {
        [
            &self.base_label,
            &self.base,
            &self.ours_label,
            &self.ours,
            &self.theirs_label,
            &self.theirs,
        ]
    }

    fn read(doc: &AutoCommit) -> Option<Self> {
        let [base_label, base, ours_label, ours, theirs_label, theirs] =
            read_fields(doc, keys::MERGE, Self::FIELDS)?;
        Some(Self {
            base_label,
            base,
            ours_label,
            ours,
            theirs_label,
            theirs,
        })
    }

    pub(crate) fn write(&self, doc: &mut AutoCommit) -> Result<(), automerge::AutomergeError> {
        write_fields(doc, keys::MERGE, Self::FIELDS, self.values())
    }

    /// The merge of the inputs, before any side is taken.
    pub fn model(&self) -> MergeModel {
        MergeModel::compute(&self.base, &self.ours, &self.theirs)
    }

    /// `Merge · ours ↔ theirs`, as the palette and history name the merge.
    pub fn title(&self) -> String {
        if self.ours_label.is_empty() && self.theirs_label.is_empty() {
            "Merge result".into()
        } else {
            format!("Merge · {} ↔ {}", self.ours_label, self.theirs_label)
        }
    }
}

fn read_fields<const N: usize>(
    doc: &AutoCommit,
    key: &str,
    fields: [&str; N],
) -> Option<[String; N]> {
    let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(&ROOT, key) else {
        return None;
    };
    Some(fields.map(|field| read_str(doc, &map, field).unwrap_or_default()))
}

fn write_fields<const N: usize>(
    doc: &mut AutoCommit,
    key: &str,
    fields: [&str; N],
    values: [&str; N],
) -> Result<(), automerge::AutomergeError> {
    let map = doc.put_object(&ROOT, key, ObjType::Map)?;
    for (field, value) in fields.into_iter().zip(values) {
        doc.put(&map, field, value)?;
    }
    Ok(())
}

/// The session as the views read it.
#[derive(Clone, Debug, Default)]
pub struct MergeState {
    /// The open two-way comparison.
    pub comparison: Option<Comparison>,
    /// The open three-way merge.
    pub merge: Option<MergeInputs>,
    /// The merged result as it stands.
    pub merged: String,
    /// The merge's hunks, each with the side recorded for it.
    pub model: MergeModel,
    /// Whether the merged result is exactly what the recorded sides compose.
    /// Once someone edits it by hand, taking a side would overwrite the
    /// edit, so hunks are resolved in the text from then on.
    pub composed: bool,
    pub title: String,
}

impl MergeState {
    pub fn from_doc(doc: &AutoCommit) -> Self {
        let comparison = Comparison::read(doc);
        let merge = MergeInputs::read(doc);
        let mut model = merge.as_ref().map(MergeInputs::model).unwrap_or_default();
        apply_resolutions(&mut model, doc);
        let merged = MergeDoc::merged_text(doc);
        let composed = merge.is_some() && model.merged_text() == merged;
        let title = merge
            .as_ref()
            .map(MergeInputs::title)
            .or_else(|| comparison.as_ref().map(Comparison::title))
            .unwrap_or_else(|| "Diff & Merge".into());
        Self {
            comparison,
            merge,
            merged,
            model,
            composed,
            title,
        }
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

/// Stateless helpers over the document.
pub struct MergeDoc;

impl MergeDoc {
    /// The merged result's text object, if the document has one.
    pub fn merged_text_id(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, keys::MERGED) {
            Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
            _ => None,
        }
    }

    /// The merged result's text object, created when the document has none.
    pub(crate) fn merged_text_obj(
        doc: &mut AutoCommit,
    ) -> Result<ObjId, automerge::AutomergeError> {
        match Self::merged_text_id(doc) {
            Some(id) => Ok(id),
            None => doc.put_object(&ROOT, keys::MERGED, ObjType::Text),
        }
    }

    /// Read the merged buffer's current contents.
    pub fn merged_text(doc: &AutoCommit) -> String {
        Self::merged_text_id(doc)
            .and_then(|id| doc.text(&id).ok())
            .unwrap_or_default()
    }

    /// Replace the merged buffer with `text`, as the smallest splice that
    /// turns the current contents into it, so the document records the
    /// edited span rather than a rewrite and a collaborator's typing
    /// elsewhere survives.
    pub(crate) fn set_merged(
        doc: &mut AutoCommit,
        text: &str,
    ) -> Result<(), automerge::AutomergeError> {
        let id = Self::merged_text_obj(doc)?;
        ui::doc::splice_text_diffed(doc, &id, text);
        Ok(())
    }

    /// Record the take-a-side choice for hunk `hunk`.
    pub(crate) fn set_resolution(
        doc: &mut AutoCommit,
        hunk: usize,
        choice: HunkChoice,
    ) -> Result<(), automerge::AutomergeError> {
        let map = match doc.get(&ROOT, keys::RESOLUTIONS) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc.put_object(&ROOT, keys::RESOLUTIONS, ObjType::Map)?,
        };
        doc.put(&map, hunk.to_string(), choice.as_str())
    }

    /// Every recorded `(hunk index, choice)` pair, in ascending hunk order.
    pub fn resolutions(doc: &AutoCommit) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        if let Ok(Some((Value::Object(ObjType::Map), id))) = doc.get(&ROOT, keys::RESOLUTIONS) {
            for key in doc.keys(&id) {
                let Ok(idx) = key.parse::<usize>() else {
                    continue;
                };
                if let Some(choice) = read_str(doc, &id, &key) {
                    out.push((idx, choice));
                }
            }
        }
        out.sort_by_key(|(k, _)| *k);
        out
    }

    /// Drop every recorded resolution.
    pub(crate) fn clear_resolutions(doc: &mut AutoCommit) -> Result<(), automerge::AutomergeError> {
        if doc.get(&ROOT, keys::RESOLUTIONS)?.is_some() {
            doc.delete(&ROOT, keys::RESOLUTIONS)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use tilo_automerge::ApplyIncoming;

    /// The merged-output text: round trip, identical sets commit nothing,
    /// resolves splice rather than rewrite, and two peers converge.
    #[test]
    fn merged_buffer_round_trips_edits_minimally_and_converges() {
        let mut doc = AutoCommit::new();
        assert!(MergeDoc::merged_text_id(&doc).is_none());
        MergeDoc::set_merged(&mut doc, "a\nb\nc\n").unwrap();
        assert_eq!(MergeDoc::merged_text(&doc), "a\nb\nc\n");
        let before = doc.get_heads();
        MergeDoc::set_merged(&mut doc, "a\nb\nc\n").unwrap();
        assert_eq!(before, doc.get_heads(), "identical set must not commit");

        // Re-resolving a hunk hands `set_merged` the entire composed output;
        // the document must grow with the edited span, not the whole text.
        let mut doc = AutoCommit::new();
        let body: String = (0..400).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "line {i}");
            s
        });
        MergeDoc::set_merged(&mut doc, &body).unwrap();
        let baseline = doc.save().len();
        for i in 0..40 {
            let edited = body.replacen("line 200\n", &format!("line 200 v{i}\n"), 1);
            MergeDoc::set_merged(&mut doc, &edited).unwrap();
        }
        let grown = doc.save().len() - baseline;
        assert_eq!(
            MergeDoc::merged_text(&doc),
            body.replacen("line 200\n", "line 200 v39\n", 1)
        );
        assert!(
            grown < baseline / 4,
            "40 one-line resolves grew the document by {grown} bytes against a \
             {baseline}-byte buffer — set_merged is rewriting the whole text"
        );

        let mut a = AutoCommit::new();
        MergeDoc::set_merged(&mut a, "shared\n").unwrap();
        let mut b = AutoCommit::load(&a.save()).unwrap();
        MergeDoc::set_merged(&mut b, "shared\nfrom-b\n").unwrap();
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge_incoming(&mut b_loaded).unwrap();
        assert!(MergeDoc::merged_text(&a).contains("from-b"));
    }

    /// Inputs and resolutions round trip, and the state says whether the
    /// merged result is still what the recorded sides compose.
    #[test]
    fn inputs_and_resolutions_round_trip_into_the_state() {
        let mut doc = AutoCommit::new();
        assert!(MergeState::from_doc(&doc).merge.is_none());
        let inputs = MergeInputs {
            base_label: "base".into(),
            base: "a\nb\nc\n".into(),
            ours_label: "ours".into(),
            ours: "a\nOURS\nc\n".into(),
            theirs_label: "theirs".into(),
            theirs: "a\nTHEIRS\nc\n".into(),
        };
        inputs.write(&mut doc).unwrap();
        let comparison = Comparison {
            left_label: "l".into(),
            left: "x\n".into(),
            right_label: "r".into(),
            right: "y\n".into(),
        };
        comparison.write(&mut doc).unwrap();
        MergeDoc::set_merged(&mut doc, &inputs.model().merged_text()).unwrap();
        MergeDoc::set_resolution(&mut doc, 2, HunkChoice::Theirs).unwrap();
        MergeDoc::set_resolution(&mut doc, 0, HunkChoice::Both).unwrap();
        assert_eq!(
            MergeDoc::resolutions(&doc),
            vec![(0, "both".to_string()), (2, "theirs".to_string())],
            "sorted by hunk index"
        );
        MergeDoc::clear_resolutions(&mut doc).unwrap();
        assert!(MergeDoc::resolutions(&doc).is_empty());

        let state = MergeState::from_doc(&doc);
        assert_eq!(state.merge.as_ref(), Some(&inputs));
        assert_eq!(state.comparison.as_ref(), Some(&comparison));
        assert_eq!(state.title, "Merge · ours ↔ theirs");
        assert!(state.composed);
        MergeDoc::set_merged(&mut doc, "edited by hand\n").unwrap();
        assert!(!MergeState::from_doc(&doc).composed);
    }
}
