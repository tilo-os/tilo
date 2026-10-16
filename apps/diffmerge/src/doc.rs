//! The diff/merge session's record.
//!
//! What is open — the two files of a comparison, the three inputs of a merge
//! — and the merge result everyone edits live in one record, so a
//! collaborator, a reload and history see the same session. Opening inputs
//! replaces them whole (they are written as atoms, never merged by
//! character), so two people opening different files at once end with one of
//! the two sets, never a mix. The merged result is a text two people type
//! into character by character. `resolutions` records the side taken for
//! each hunk; opening a new merge clears it, since the old hunk indices no
//! longer correspond.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Atom, JsonSchema, Model};

use crate::model::{DiffModel, MergeModel, Resolution};

/// Which side a change hunk takes. Only the outcomes a person chooses, so a
/// record can never hold the internal conflict or automatic states.
#[derive(Model, Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HunkChoice {
    /// Take our side (the left / "ours" column).
    #[default]
    Ours,
    /// Take their side (the right / "theirs" column).
    Theirs,
    /// Take both, ours then theirs.
    Both,
}

impl HunkChoice {
    /// The [`Resolution`] this choice sets on a hunk.
    pub fn to_resolution(self) -> Resolution {
        match self {
            HunkChoice::Ours => Resolution::TookOurs,
            HunkChoice::Theirs => Resolution::TookTheirs,
            HunkChoice::Both => Resolution::TookBoth,
        }
    }
}

/// The two files of a two-way comparison.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Left name
    pub left_label: Atom,
    /// Left text
    pub left: Atom,
    /// Right name
    pub right_label: Atom,
    /// Right text
    pub right: Atom,
}

impl Comparison {
    /// The comparison's line-aligned diff, shared with every reader of the
    /// same two texts.
    pub fn model(&self) -> std::sync::Arc<DiffModel> {
        crate::model::comparison(&self.left.0, &self.right.0)
    }

    /// `left ↔ right`, as the palette names the comparison.
    pub fn title(&self) -> String {
        if self.left_label.0.is_empty() && self.right_label.0.is_empty() {
            "Comparison".into()
        } else {
            format!("{} ↔ {}", self.left_label.0, self.right_label.0)
        }
    }
}

/// The three inputs of a three-way merge.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeInputs {
    /// Base name
    pub base_label: Atom,
    /// Base text
    pub base: Atom,
    /// Our name
    pub ours_label: Atom,
    /// Our text
    pub ours: Atom,
    /// Their name
    pub theirs_label: Atom,
    /// Their text
    pub theirs: Atom,
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

impl MergeInputs {
    /// The merge of the inputs, before any side is taken.
    pub fn model(&self) -> MergeModel {
        MergeModel::compute(&self.base.0, &self.ours.0, &self.theirs.0)
    }

    /// `Merge · ours ↔ theirs`, as the palette and history name the merge.
    pub fn title(&self) -> String {
        merge_title(&self.ours_label.0, &self.theirs_label.0)
    }
}

/// The diff/merge session: what is open and the merged result.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Session {
    /// Comparison
    ///
    /// The open two-way comparison. Only opening files changes it.
    #[model(readonly)]
    pub comparison: Option<Comparison>,
    /// Merge
    ///
    /// The open three-way merge. Only opening a merge changes it.
    #[model(readonly)]
    pub merge: Option<MergeInputs>,
    /// Merged result
    pub merged: String,
    /// Sides taken
    ///
    /// The side taken for each change, by its hunk index. Only taking a side
    /// changes it.
    #[model(readonly)]
    pub resolutions: BTreeMap<String, HunkChoice>,
}

impl Session {
    /// The merge's hunks, each with the side recorded for it.
    pub fn merge_model(&self) -> MergeModel {
        let mut model = self
            .merge
            .as_ref()
            .map(MergeInputs::model)
            .unwrap_or_default();
        for (hunk, choice) in self.resolutions() {
            if let Some(h) = model.hunks.get_mut(hunk)
                && h.is_change
            {
                h.resolution = choice.to_resolution();
            }
        }
        model
    }

    /// Whether the merged result is exactly what the recorded sides compose
    /// in `model` ([`Self::merge_model`]). Once someone edits it by hand,
    /// taking a side would overwrite the edit, so hunks are resolved in the
    /// text from then on.
    pub fn composed(&self, model: &MergeModel) -> bool {
        self.merge.is_some() && model.merged_text() == self.merged
    }

    /// Every recorded `(hunk index, choice)` pair, in ascending hunk order.
    pub fn resolutions(&self) -> Vec<(usize, HunkChoice)> {
        let mut out: Vec<(usize, HunkChoice)> = self
            .resolutions
            .iter()
            .filter_map(|(hunk, choice)| Some((hunk.parse().ok()?, *choice)))
            .collect();
        out.sort_by_key(|(hunk, _)| *hunk);
        out
    }

    /// What the session is called: the merge, else the comparison.
    pub fn title(&self) -> String {
        self.merge
            .as_ref()
            .map(MergeInputs::title)
            .or_else(|| self.comparison.as_ref().map(Comparison::title))
            .unwrap_or_else(|| "Diff & Merge".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict_inputs() -> MergeInputs {
        MergeInputs {
            base_label: Atom("base".into()),
            base: Atom("a\nb\nc\n".into()),
            ours_label: Atom("ours".into()),
            ours: Atom("a\nOURS\nc\n".into()),
            theirs_label: Atom("theirs".into()),
            theirs: Atom("a\nTHEIRS\nc\n".into()),
        }
    }

    /// The alignment of two texts is computed once and shared; other texts
    /// get their own.
    #[test]
    fn the_alignment_of_two_texts_is_shared_and_replacing_one_recomputes_it() {
        let first = Comparison {
            left: Atom("before\n".into()),
            right: Atom("after\n".into()),
            ..Comparison::default()
        };
        let model = first.model();
        let again = first.clone();
        assert!(std::sync::Arc::ptr_eq(&model, &again.model()));
        let changed = Comparison {
            right: Atom("before\n".into()),
            ..first
        };
        assert!(changed.model().blocks.is_empty());
        assert!(!std::sync::Arc::ptr_eq(&model, &changed.model()));
    }

    /// Recorded sides apply to the merge's changes, and the session says
    /// whether the merged result is still what they compose.
    #[test]
    fn resolutions_apply_to_the_merge_and_composed_follows_the_result() {
        let mut session = Session {
            merge: Some(conflict_inputs()),
            ..Session::default()
        };
        let conflict = session
            .merge_model()
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .unwrap();
        session
            .resolutions
            .insert(conflict.to_string(), HunkChoice::Theirs);
        session.resolutions.insert("99".into(), HunkChoice::Both);
        let model = session.merge_model();
        assert_eq!(
            model.hunks[conflict].resolution,
            Resolution::TookTheirs,
            "the recorded side, and an index the merge lacks is ignored"
        );
        session.merged = model.merged_text();
        assert!(session.composed(&model));
        assert_eq!(session.title(), "Merge of ours and theirs");
        session.merged = "edited by hand\n".into();
        assert!(!session.composed(&model));
        assert_eq!(
            session.resolutions(),
            vec![(conflict, HunkChoice::Theirs), (99, HunkChoice::Both)],
            "sorted by hunk index"
        );
    }
}
