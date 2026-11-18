//! The session's verbs, the places derived from it. Typing in the merged
//! result is the field edit at its path; opening inputs and taking a side are
//! verbs. A merge is recomputed inside a verb from the resource's inputs, so
//! every peer composes the same result.

use tilo_resource::{self, Atom, Derived, Refusal, resource};

use crate::actions::{
    CONTEXTUAL_CHILD_LIMIT, DiffmergeEffect, compose_choice, compose_custom,
    diff_block_fingerprint, diff_block_resource_id, hunk_preview, hunk_resource_id,
};
use crate::doc::{Comparison, HunkChoice, MergeInputs, Session};
use crate::model::{BlockKind, MergeModel};

impl Session {
    /// What the resource holds: the inputs compared, which side each change takes, and the
    /// merged result.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "diffmerge.session",
    children = Session::places,
    read_content = Session::read_document
)]
impl Session {
    /// Compare two texts.
    #[action]
    pub fn open_comparison(
        &mut self,
        #[param("Name of the left side")] left_label: String,
        #[param("The left text")] left: String,
        #[param("Name of the right side")] right_label: String,
        #[param("The right text")] right: String,
    ) {
        self.comparison = Some(Comparison {
            left_label: Atom(left_label),
            left: Atom(left),
            right_label: Atom(right_label),
            right: Atom(right),
        });
    }

    /// Merge three texts.
    #[action]
    pub fn open_merge(
        &mut self,
        #[param("Name of the common base")] base_label: String,
        #[param("The common base text")] base: String,
        #[param("Name of our side")] ours_label: String,
        #[param("Our text")] ours: String,
        #[param("Name of their side")] theirs_label: String,
        #[param("Their text")] theirs: String,
    ) {
        let inputs = MergeInputs {
            base_label: Atom(base_label),
            base: Atom(base),
            ours_label: Atom(ours_label),
            ours: Atom(ours),
            theirs_label: Atom(theirs_label),
            theirs: Atom(theirs),
        };
        self.merged = inputs.model().merged_text();
        self.merge = Some(inputs);
        self.resolutions.clear();
        self.written.clear();
    }

    /// Take ours for a change.
    #[action]
    pub fn take_ours(&mut self, #[param("The change's hunk")] hunk: u64) -> Result<(), Refusal> {
        self.take(hunk, HunkChoice::Ours)
    }

    /// Take theirs for a change.
    #[action]
    pub fn take_theirs(&mut self, #[param("The change's hunk")] hunk: u64) -> Result<(), Refusal> {
        self.take(hunk, HunkChoice::Theirs)
    }

    /// Take both sides for a change.
    #[action]
    pub fn take_both(&mut self, #[param("The change's hunk")] hunk: u64) -> Result<(), Refusal> {
        self.take(hunk, HunkChoice::Both)
    }

    /// Resolve a change with custom text.
    #[action]
    pub fn resolve(
        &mut self,
        #[param("The change's hunk")] hunk: u64,
        #[param("The resolved line(s)")] text: String,
    ) -> Result<(), Refusal> {
        let (model, index) = self.change(hunk)?;
        self.merged = compose_custom(&model, index, &text);
        self.resolutions.remove(&index.to_string());
        self.written.insert(index.to_string(), text);
        Ok(())
    }
}

impl Session {
    fn change(&self, hunk: u64) -> Result<(MergeModel, usize), Refusal> {
        if self.merge.is_none() {
            return Err(Refusal::new("no merge is open"));
        }
        let model = self.merge_model();
        let index = usize::try_from(hunk).unwrap_or(usize::MAX);
        if !model.hunks.get(index).is_some_and(|h| h.is_change) {
            return Err(Refusal::new(format!(
                "the merge has no change {}",
                hunk.saturating_add(1)
            )));
        }
        if !self.composed(&model) {
            return Err(Refusal::new(
                "the merged output was edited by hand; resolve the rest in it",
            ));
        }
        Ok((model, index))
    }

    fn take(&mut self, hunk: u64, choice: HunkChoice) -> Result<(), Refusal> {
        let (model, index) = self.change(hunk)?;
        self.merged = compose_choice(&model, index, choice);
        self.written.remove(&index.to_string());
        self.resolutions.insert(index.to_string(), choice);
        Ok(())
    }

    fn places(&self) -> Vec<Derived<Self>> {
        let model = self.merge_model();
        let mut places = Vec::new();
        if self.composed(&model) {
            places.extend(Self::change_places(&model));
        }
        if let Some(comparison) = &self.comparison {
            places.push(Self::comparison_place(comparison));
        }
        places
    }

    /// Each change as a place, under an id derived from its content, so a recomputed merge
    /// never points an old address at a different change.
    fn change_places(model: &MergeModel) -> Vec<Derived<Self>> {
        let mut places = Vec::new();
        let mut ordinal = 0usize;
        let mut identities = std::collections::BTreeMap::<String, usize>::new();
        for (index, h) in model.hunks.iter().enumerate() {
            if !h.is_change {
                continue;
            }
            ordinal += 1;
            let tag = if h.resolution.is_conflict() {
                "Conflict"
            } else {
                "Change"
            };
            let preview = hunk_preview(h);
            let label = if preview.is_empty() {
                format!("{tag} {ordinal}")
            } else {
                format!("{tag} {ordinal}: {preview}")
            };
            let occurrence = identities.entry(hunk_resource_id(h, 0)).or_default();
            let id = hunk_resource_id(h, *occurrence);
            *occurrence = occurrence.saturating_add(1);
            let hunk = u64::try_from(index).unwrap_or(u64::MAX);
            places.push(
                Derived::new(label)
                    .of_kind("change")
                    .identified(id)
                    .with(Self::TAKE_OURS.bind(session::TakeOurs::paths().hunk, hunk))
                    .with(Self::TAKE_THEIRS.bind(session::TakeTheirs::paths().hunk, hunk))
                    .with(Self::TAKE_BOTH.bind(session::TakeBoth::paths().hunk, hunk))
                    .with(Self::RESOLVE.bind(session::Resolve::paths().hunk, hunk)),
            );
        }
        places
    }

    /// The open comparison, one place per change block (bounded).
    fn comparison_place(comparison: &Comparison) -> Derived<Self> {
        let model = comparison.model();
        let mut place = Derived::new(comparison.title())
            .of_kind("comparison")
            .identified("comparison");
        let mut occurrences = std::collections::HashMap::<String, usize>::new();
        for (index, block) in model.blocks.iter().take(CONTEXTUAL_CHILD_LIMIT).enumerate() {
            let kind = match block.kind {
                BlockKind::Added => "Added",
                BlockKind::Removed => "Removed",
                BlockKind::Modified => "Modified",
            };
            let label = format!(
                "{kind} lines {}–{}",
                block.first_row.saturating_add(1),
                block.last_row.saturating_add(1)
            );
            let Some(fingerprint) = diff_block_fingerprint(&model, index) else {
                continue;
            };
            let occurrence = occurrences
                .entry(fingerprint.clone())
                .and_modify(|count| *count += 1)
                .or_insert(0);
            place = place.with_child(
                Derived::new(label)
                    .of_kind("change-block")
                    .identified(diff_block_resource_id(block, &fingerprint, *occurrence))
                    .with_effect(&DiffmergeEffect::Show {
                        block: u64::try_from(index).unwrap_or(u64::MAX),
                        fingerprint,
                    }),
            );
        }
        place
    }
}
