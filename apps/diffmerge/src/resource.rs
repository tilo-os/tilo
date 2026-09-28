//! Diff & Merge as a [`tilo_resource::Resource`]: the merged buffer syncs;
//! opening files and loading samples are Effects / Views.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_actions::{ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{DIFFMERGE_ACTION_TYPE, DiffmergeAction, register, workspace_operations};
use crate::app::DiffMergeApp;
use crate::doc::MergeDoc;

/// Projection of the collaborative merge session.
#[derive(Clone, Debug, Default)]
pub struct MergeState {
    pub merged: String,
    pub session: String,
}

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
        Ok(MergeState {
            merged: MergeDoc::merged_text(doc),
            session: MergeDoc::session(doc).unwrap_or_default(),
        })
    }

    fn local_operations() -> Vec<ActionSpec> {
        workspace_operations()
            .actions
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Framework app: the diff/merge widget over [`Diffmerge`].
pub struct DiffmergeShell;

impl ResourceApp for DiffmergeShell {
    type Resource = Diffmerge;
    const ID: &'static str = "so.tilo.diffmerge";
    const TITLE: &'static str = "Diff & Merge";
    const ICON: Option<&'static str> = Some("diffmerge");
    const SHOT_VIEWPORT: (u32, u32) = (1200, 800);

    fn view(cx: &mut ViewCx<Diffmerge>) -> Element {
        cx.surface(DiffMergeApp::new())
    }
}

impl ResourceSurface<Diffmerge> for DiffMergeApp {
    fn present(&mut self, state: &std::rc::Rc<MergeState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Diffmerge>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Diffmerge>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Diffmerge>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
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
