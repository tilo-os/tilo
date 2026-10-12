//! The session's verbs, the places derived from it, and the app.
//!
//! Typing in the merged result is the built-in field edit of its path
//! (`Session::paths().merged.splice(..)`); opening inputs and taking a side
//! for a change are verbs. The merge's changes and the comparison's change
//! blocks are derived from the session and published as places that offer
//! the verbs (or the view) that act on them. The merge is recomputed inside a
//! verb from the inputs the record holds, a pure function of them, so every
//! peer composes the same result.

use crate::chrome::Chrome;
use std::rc::Rc;
use ui::widget::NavShell;

use ui::framework::{EffectAnswer, EffectCx, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, Atom, Derived, Invoke, Refusal, resource};
use ui::widget::Element;

use crate::actions::{
    CONTEXTUAL_CHILD_LIMIT, DiffmergeEffect, compose_choice, compose_custom,
    diff_block_fingerprint, diff_block_resource_id, hunk_preview, hunk_resource_id,
};
use crate::app::DiffMergeApp;
use crate::doc::{Comparison, HunkChoice, MergeInputs, Session};
use crate::model::{BlockKind, MergeModel};

#[resource(kind = "diffmerge.session", label = "Diff & Merge", children = Session::places)]
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
    ///
    /// The merged result starts with no side taken, and no side is recorded.
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
        Ok(())
    }
}

impl Session {
    /// The merge and the index of its change `hunk`, refused when no merge
    /// is open, the merge has no such change, or the merged result was edited
    /// by hand (which a recomposition would overwrite).
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

    /// Record `choice` for change `hunk` and recompose the merged result.
    fn take(&mut self, hunk: u64, choice: HunkChoice) -> Result<(), Refusal> {
        let (model, index) = self.change(hunk)?;
        self.merged = compose_choice(&model, index, choice);
        self.resolutions.insert(index.to_string(), choice);
        Ok(())
    }

    /// The places derived from the session: the merge's changes while sides
    /// can still be taken, each offering its sides and a custom resolution,
    /// and the open comparison with its change blocks, each shown on demand.
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

    /// Each change of `model` as a place, under an id derived from its
    /// content so a recomputed merge never points an old address at a
    /// different change.
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

    /// The open comparison, with one place per change block (bounded) that
    /// shows it.
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

/// The bundled sample comparison and merge, as the verbs that open them.
pub fn sample_invocations() -> [Invoke<Session>; 2] {
    let (left, right) = crate::samples::diff_pair();
    let (base, ours, theirs) = crate::samples::merge_triple();
    [
        Session::OPEN_COMPARISON.with(session::OpenComparison {
            left_label: left.label,
            left: left.text,
            right_label: right.label,
            right: right.text,
        }),
        Session::OPEN_MERGE.with(session::OpenMerge {
            base_label: base.label,
            base: base.text,
            ours_label: ours.label,
            ours: ours.text,
            theirs_label: theirs.label,
            theirs: theirs.text,
        }),
    ]
}

/// Framework app: the diff/merge widget over [`Session`].
pub struct DiffmergeShell;

impl ResourceApp for DiffmergeShell {
    type Resource = Session;
    const ID: &'static str = "so.tilo.diffmerge";
    const TITLE: &'static str = "Diff & Merge";
    const ICON: Option<&'static str> = Some("diffmerge");
    fn label(state: &Session) -> String {
        state.title()
    }

    fn view(cx: &mut ViewCx<Session>) -> Element {
        let chrome = Chrome::new();
        let mut comparison = DiffMergeApp::new();
        comparison.attach_chrome(chrome.clone());
        // One column: the shell draws the header row that holds the
        // comparison's controls.
        Box::new(
            NavShell::new()
                .content(cx.surface(comparison))
                .content_header("", chrome.actions()),
        )
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(crate::i18n::catalogs())
    }

    fn scenarios() -> &'static [&'static str] {
        &["empty", "diff", "merge", "error", "status"]
    }

    /// Every scenario but the empty one shows the bundled samples.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Session>> {
        if name == Some("empty") {
            return Vec::new();
        }
        sample_invocations().into()
    }
}

impl ResourceSurface<Session, DiffmergeEffect> for DiffMergeApp {
    fn present(&mut self, state: &Rc<Session>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Session>) {
        self.attach_dispatcher(cx.dispatcher());
        self.attach_waker(cx.waker());
    }

    fn read_only_changed(&mut self, read_only: bool) {
        self.set_read_only(read_only);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: DiffmergeEffect,
        cx: &mut EffectCx<Session, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }
}

#[cfg(test)]
mod tests;
