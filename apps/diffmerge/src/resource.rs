//! Diff & Merge as a [`tilo_resource::Resource`]: the open comparison, the
//! open merge and its merged result, shown by [`DiffMergeApp`].

use crate::chrome::Chrome;
use std::rc::Rc;
use ui::widget::NavShell;

use automerge::transaction::Transactable;
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_actions::Resource as PublishedResource;
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, PlainText, Resource};
use ui::widget::Element;

use crate::actions::{
    DIFFMERGE_ACTION_TYPE, DiffmergeAction, DiffmergeEffect, declarations, document_children,
    merged_resource_id, register, sample_actions,
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

    fn scenarios() -> &'static [&'static str] {
        &["empty", "diff", "merge", "error", "status"]
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
        cx: &mut EffectCx<Diffmerge, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }
}

#[cfg(test)]
mod header_tests {
    use super::*;
    #[test]
    fn twenty_megabyte_sources_open_through_the_actual_resource_and_stay_scrollable() {
        use std::time::Instant;
        let (left, right) = crate::diff_view::performance_tests::unique_sources();
        let bytes = left.len();
        let mut app = ui::framework::testing::HeadlessApp::<DiffmergeShell>::new().unwrap();
        let open =
            ui::tilo_resource::Invoke::<Diffmerge>::apply(&DiffmergeAction::OpenComparison {
                left_label: "large-before.rs".into(),
                left,
                right_label: "large-after.rs".into(),
                right,
            })
            .unwrap();
        let started = Instant::now();
        app.invoke(open.clone()).unwrap();
        let opened = started.elapsed();
        let state = app.state();
        assert_eq!(state.comparison.as_ref().unwrap().left.len(), bytes);
        let started = Instant::now();
        app.effect(&DiffmergeEffect::NextChange).unwrap();
        let jump = started.elapsed();
        let started = Instant::now();
        app.effect(&DiffmergeEffect::ShowMerge).unwrap();
        app.effect(&DiffmergeEffect::ShowDiff).unwrap();
        let switch = started.elapsed();
        let started = Instant::now();
        app.invoke(open).unwrap();
        let reopened = started.elapsed();
        app.harness().send(&winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(480.0, 400.0),
        });
        let digests = crate::model::digest_passes();
        let started = Instant::now();
        for _ in 0..8 {
            app.harness().scroll(0.0, -3.0);
        }
        let scroll = started.elapsed();
        assert_eq!(
            crate::model::digest_passes(),
            digests,
            "ordinary wheel frames do not hash the unchanged 40 MB comparison"
        );
        assert_eq!(app.state().comparison.as_ref().unwrap().left.len(), bytes);
        let audit = app.harness().native_app_audit();
        assert!(
            audit.is_certified(),
            "the real large comparison publishes its controls: {audit:#?}"
        );
        eprintln!(
            "hosted large comparison bytes={bytes} canonical_open_and_frame={opened:?} next_change_and_frame={jump:?} mode_round_trip={switch:?} repeated_open={reopened:?} eight_wheel_frames={scroll:?}"
        );
    }

    /// The header's controls are the shared widgets, reached the way a person
    /// reaches them: the shell's header holds the mode switch, change
    /// navigation and file actions, and pressing the mode switch changes
    /// what the comparison shows.
    #[test]
    fn the_header_controls_drive_the_comparison_through_the_shell() {
        use ui::framework::testing::HeadlessApp;
        use ui::headless::{Control, ControlInput};

        let mut app =
            HeadlessApp::<DiffmergeShell>::capture(Some("diff")).expect("the diff stages");
        let find = |app: &mut HeadlessApp<DiffmergeShell>, name: &str| -> Option<Control> {
            app.harness()
                .controls()
                .into_iter()
                .find(|control| control.name.as_deref().is_some_and(|n| n.starts_with(name)))
        };
        for name in [
            "Diff",
            "3-way merge",
            "Previous change",
            "Next change",
            "Load sample",
            "Open files",
        ] {
            assert!(find(&mut app, name).is_some(), "the header offers {name}");
        }
        assert!(
            find(&mut app, "Take ours for change").is_none(),
            "the diff is showing, not the merge"
        );

        let merge = find(&mut app, "3-way merge").expect("the mode switch");
        app.harness()
            .operate_control(&merge.id, &ControlInput::Press)
            .expect("the mode switch answers a press");
        app.harness().relayout();
        assert!(
            find(&mut app, "Take ours for change").is_some(),
            "choosing the 3-way merge shows its resolutions"
        );
        assert!(find(&mut app, "Diff between").is_none());
    }
}
