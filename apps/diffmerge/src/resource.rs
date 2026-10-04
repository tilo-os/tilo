//! Diff & Merge as a [`tilo_resource::Resource`]: the merged buffer and shared
//! three-way inputs sync; local two-way comparisons and in-flight opens stay
//! live extras when hosted.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_actions::{ActionSpec, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::{Element, NavShell};

use crate::actions::{
    DIFFMERGE_ACTION_TYPE, DiffmergeAction, document_bound_actions, document_children,
    local_comparison_resource, register, root_local_operations,
};
use crate::app::DiffMergeApp;
use crate::chrome::Chrome;
use crate::doc::MergeState;

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

    fn bound_actions(state: &MergeState) -> Vec<ActionSpec> {
        document_bound_actions(state)
    }

    fn children(state: &MergeState) -> Vec<PublishedResource> {
        document_children(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // View navigation and open-files / load-sample Effects. Conflict
        // children ride [`children`]; local two-way comparisons stay live.
        root_local_operations()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Hosted-only extras that are not document state: a local two-way comparison
/// and an in-flight open-files job. The workspace door stays on
/// [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &DiffMergeApp) -> Vec<PublishedResource> {
    let mut extras = Vec::new();
    if let Some(ctx) = app.diff_resource_ctx() {
        extras.push(local_comparison_resource(&ctx));
    }
    if let Some(label) = app.open_job_label() {
        extras.push(PublishedResource::new(
            "diffmerge.open-job",
            "diffmerge.job",
            label,
        ));
    }
    extras
}

/// Framework app: the diff/merge widget over [`Diffmerge`].
pub struct DiffmergeShell;

impl ResourceApp for DiffmergeShell {
    type Resource = Diffmerge;
    const ID: &'static str = "so.tilo.diffmerge";
    const TITLE: &'static str = "Diff & Merge";
    const ICON: Option<&'static str> = Some("diffmerge");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn label(state: &MergeState) -> String {
        if state.title.is_empty() {
            Self::TITLE.to_string()
        } else {
            state.title.clone()
        }
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
        &["empty", "diff", "merge", "error"]
    }
}

impl ResourceSurface<Diffmerge> for DiffMergeApp {
    fn present(&mut self, state: &std::rc::Rc<MergeState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Diffmerge>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Diffmerge>>,
    hosted: bool,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Diffmerge>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    /// Test-only: mark hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &DiffmergeAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}

/// Live launch through the framework host, with History mode registered.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<DiffmergeShell>().history(&crate::framework_history::HISTORY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::provide_resources;
    use crate::doc::SharedMerge;
    use crate::model::MergeModel;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn resource_publish_owns_workspace_door_and_hosted_keeps_only_live_extras() {
        let model = MergeModel::compute("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
        let shared = SharedMerge {
            base: "a\nb\nc\n".into(),
            ours: "a\nOURS\nc\n".into(),
            theirs: "a\nTHEIRS\nc\n".into(),
            base_label: "base".into(),
            ours_label: "ours".into(),
            theirs_label: "theirs".into(),
        };
        let mut doc = AutoCommit::new();
        SharedMerge::write(&mut doc, &shared);
        crate::doc::MergeDoc::set_merged(&mut doc, &model.merged_text());
        doc.commit();

        let state = Diffmerge::read(&doc).expect("seeded merge state");
        let published =
            ui::tilo_resource::publish::<Diffmerge>("diffmerge.workspace", &state.title, &state);
        assert_eq!(published.kind, "diffmerge.workspace");
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "edit-merged"),
            "edit-merged bound_action must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "show-diff"),
            "show-diff View must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "open-files"),
            "open-files Effect must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "diffmerge.conflict"),
            "conflict children must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .all(|child| child.kind != "diffmerge.local-comparison"),
            "shared projection must not carry local two-way comparison"
        );

        let mut app = DiffMergeApp::new_demo();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone[0].kind, "diffmerge.workspace");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "diffmerge.conflict"),
            "standalone publishes conflict children"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        // Demo opens on merge mode; switch to diff so a local comparison is live.
        app.set_mode_for_test(crate::app::Mode::Diff);
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "diffmerge.workspace"),
            "hosted must not republish diffmerge.workspace beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "diffmerge.local-comparison"),
            "hosted still publishes local comparison: {:?}",
            extras.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(
            &crate::actions::MergeResourceCtx {
                title: shared.title(),
                model,
            },
            &seeded,
        );
        assert_eq!(door[0].kind, "diffmerge.workspace");
    }

    /// The header's controls are the shared widgets, reached the way a person
    /// reaches them: the shell's header holds the mode switch, change
    /// navigation and file actions, and pressing the mode switch changes
    /// what the comparison shows.
    #[test]
    fn the_header_controls_drive_the_comparison_through_the_shell() {
        use ui::framework::testing::HeadlessApp;
        use ui::headless::{Control, ControlInput};

        let mut app = HeadlessApp::<DiffmergeShell>::new().expect("the app opens");
        app.stage(Some("diff")).expect("the diff stages");
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
