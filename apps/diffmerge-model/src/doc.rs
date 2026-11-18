//! The session's shared data: the inputs of what is open and the merged result everyone edits,
//! so a member, a reload and history see the same session. Inputs are written as
//! atoms, so two people opening different files end with one of the two sets. `resolutions`
//! and `written` hold how each hunk was taken, and a new merge clears both.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_resource::{self, Atom, JsonSchema, Model};

use crate::model::{DiffModel, MergeModel, Resolution};

/// Which side a change takes: only the outcomes a person chooses, never the internal
/// conflict or automatic states.
#[derive(Model, Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HunkChoice {
    #[default]
    Ours,
    Theirs,
    Both,
}

impl HunkChoice {
    pub fn to_resolution(self) -> Resolution {
        match self {
            HunkChoice::Ours => Resolution::TookOurs,
            HunkChoice::Theirs => Resolution::TookTheirs,
            HunkChoice::Both => Resolution::TookBoth,
        }
    }
}

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
    pub fn model(&self) -> std::sync::Arc<DiffModel> {
        crate::model::comparison(&self.left.0, &self.right.0)
    }

    pub fn title(&self) -> String {
        if self.left_label.0.is_empty() && self.right_label.0.is_empty() {
            "Comparison".into()
        } else {
            format!("{} ↔ {}", self.left_label.0, self.right_label.0)
        }
    }
}

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

/// The file a side's label names: a label may carry what it was chosen from in
/// parentheses, and the file is what reads.
pub fn split_label(label: &str) -> (&str, Option<&str>) {
    let label = label.trim();
    match label.rsplit_once(" (") {
        Some((name, rest)) if rest.ends_with(')') => {
            (name.trim_end(), rest.strip_suffix(')').map(str::trim))
        }
        _ => (label, None),
    }
}

/// The file name a side's label shows, without what it was chosen from.
pub fn file_name(label: &str) -> &str {
    split_label(label).0
}

/// The merge's title: the sides it merges, named by their files. Every device
/// derives the same label from the same resource.
pub fn merge_title(ours: &str, theirs: &str) -> String {
    let (ours_name, theirs_name) = (file_name(ours), file_name(theirs));
    if ours.is_empty() && theirs.is_empty() {
        "Merged result".to_string()
    } else if ours_name == theirs_name {
        format!("Merge of {ours_name}")
    } else {
        format!("Merge of {ours_name} and {theirs_name}")
    }
}

impl MergeInputs {
    pub fn model(&self) -> MergeModel {
        MergeModel::compute(&self.base.0, &self.ours.0, &self.theirs.0)
    }

    pub fn title(&self) -> String {
        merge_title(&self.ours_label.0, &self.theirs_label.0)
    }
}

#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Session {
    /// Comparison
    #[model(readonly)]
    pub comparison: Option<Comparison>,
    /// Merge
    #[model(readonly)]
    pub merge: Option<MergeInputs>,
    /// Merged result
    pub merged: String,
    /// Sides taken
    #[model(readonly)]
    pub resolutions: BTreeMap<String, HunkChoice>,
    /// Texts written for changes
    #[model(readonly)]
    pub written: BTreeMap<String, String>,
}

impl Session {
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
        for (hunk, text) in &self.written {
            if let Some(h) = hunk
                .parse::<usize>()
                .ok()
                .and_then(|i| model.hunks.get_mut(i))
                && h.is_change
            {
                h.resolution = Resolution::Written;
                h.written = text.lines().map(str::to_owned).collect();
            }
        }
        model
    }

    /// Whether the merged result is exactly what the recorded sides compose. Once someone
    /// edits it by hand, taking a side would overwrite the edit, so hunks are resolved in
    /// the text from then on.
    pub fn composed(&self, model: &MergeModel) -> bool {
        self.merge.is_some() && model.merged_text() == self.merged
    }

    pub fn resolutions(&self) -> Vec<(usize, HunkChoice)> {
        let mut out: Vec<(usize, HunkChoice)> = self
            .resolutions
            .iter()
            .filter_map(|(hunk, choice)| Some((hunk.parse().ok()?, *choice)))
            .collect();
        out.sort_by_key(|(hunk, _)| *hunk);
        out
    }

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

    tilo_test_env::scenario! {
        fn a_session_shares_its_alignment_and_applies_recorded_resolutions;

        phase the_alignment_of_two_texts_is_shared_and_replacing_one_recomputes_it;
        {
            let first = Comparison {
                left: Atom("before\n".into()),
                right: Atom("after\n".into()),
                ..Comparison::default()
            };
            assert_eq!(first.model().blocks.len(), 1);
            let changed = Comparison {
                right: Atom("before\n".into()),
                ..first
            };
            assert!(changed.model().blocks.is_empty());
        }

        phase resolutions_apply_to_the_merge_and_composed_follows_the_result;
        {
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
}
