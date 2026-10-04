//! Diff & Merge as a [`tilo_resource::Resource`]: the open comparison, the
//! open merge and its merged result, shown by [`DiffMergeApp`].

use std::rc::Rc;

use automerge::transaction::Transactable;
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_actions::{ActionSpec, Resource as PublishedResource};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, PlainText, Resource};
use ui::widget::Element;

use crate::actions::{
    DIFFMERGE_ACTION_TYPE, DiffmergeAction, DiffmergeEffect, declarations, document_children,
    merged_resource_id, register, sample_actions, view_operations,
};
use crate::app::DiffMergeApp;
use crate::doc::{MergeDoc, MergeState, keys};

/// The diff/merge workspace resource.
pub struct Diffmerge;

impl Resource for Diffmerge {
    const KIND: &'static str = "diffmerge.workspace";
    const LABEL: &'static str = "Diff & Merge";
    const ACTION_TYPE: &'static str = DIFFMERGE_ACTION_TYPE;

    type State = MergeState;
    type Action = DiffmergeAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<MergeState, ModelError> {
        Ok(MergeState::from_doc(doc))
    }

    /// A new workspace starts with the merged result's text, so two people
    /// who open it before syncing type into the same text.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        if MergeDoc::merged_text_id(doc).is_none() {
            doc.put_object(automerge::ROOT, keys::MERGED, automerge::ObjType::Text)
                .map_err(|error| ModelError::invalid(error.to_string()))?;
        }
        Ok(())
    }

    /// Typing in the merged result is one undo step a word at a time, as in
    /// every plain text.
    fn coalesces(previous: &DiffmergeAction, next: &DiffmergeAction) -> bool {
        match (previous, next) {
            (DiffmergeAction::EditMerged(previous), DiffmergeAction::EditMerged(next)) => {
                PlainText::coalesces(previous, next)
            }
            _ => false,
        }
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        declarations()
    }

    fn children(state: &MergeState) -> Vec<PublishedResource> {
        document_children(state)
    }
}

/// Edits of the merged result through the workspace's dispatcher: each one a
/// [`DiffmergeAction::EditMerged`], addressed to the child
/// [`Diffmerge::children`] publishes.
pub(crate) fn merged_dispatcher(dispatcher: &Dispatcher<Diffmerge>) -> Dispatcher<PlainText> {
    dispatcher.child::<PlainText>(&merged_resource_id(), DiffmergeAction::EditMerged)
}

/// Framework app: the diff/merge widget over [`Diffmerge`].
pub struct DiffmergeShell;

impl ResourceApp for DiffmergeShell {
    type Resource = Diffmerge;
    const ID: &'static str = "so.tilo.diffmerge";
    const TITLE: &'static str = "Diff & Merge";
    const ICON: Option<&'static str> = Some("diffmerge");

    fn label(state: &MergeState) -> String {
        state.title.clone()
    }

    fn view(cx: &mut ViewCx<Diffmerge>) -> Element {
        cx.surface(DiffMergeApp::new())
    }

    fn local_operations() -> Vec<ActionSpec> {
        view_operations()
    }

    fn scenarios() -> &'static [&'static str] {
        &["empty", "diff", "merge", "error", "status"]
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1200, 800)
    }

    /// Every scenario but the empty one shows the bundled samples.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Diffmerge>> {
        if name == Some("empty") {
            return Vec::new();
        }
        sample_actions()
            .iter()
            .filter_map(|action| Invoke::<Diffmerge>::apply(action).ok())
            .collect()
    }
}

impl ResourceSurface<Diffmerge, DiffmergeEffect> for DiffMergeApp {
    fn present(&mut self, state: &Rc<MergeState>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Diffmerge>) {
        self.attach_dispatcher(cx.dispatcher());
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
        cx: &mut EffectCx<Diffmerge, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }
}
