//! Imager as a [`tilo_resource::Resource`]: install profile is document state;
//! write/verify jobs are Effects in [`Resource::local_operations`].

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{IMAGER_ACTION_TYPE, ImagerAction, operation_resource, register};
use crate::app::ImagerApp;
use crate::doc::ImagerDoc;
use crate::model::{InstallProfile, Source};

/// Shared image-selection and install-profile projection.
#[derive(Clone, Debug, Default)]
pub struct ImagerState {
    pub source: Option<Source>,
    pub profile: InstallProfile,
}

/// The disk-imager sources / write-plan resource.
pub struct Imager;

impl Resource for Imager {
    const KIND: &'static str = "imager.sources";
    const LABEL: &'static str = "Image sources";
    const ACTION_TYPE: &'static str = IMAGER_ACTION_TYPE;

    type State = ImagerState;
    type Action = ImagerAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<ImagerState, ModelError> {
        Ok(ImagerState {
            source: ImagerDoc::source(doc),
            profile: ImagerDoc::profile(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        Vec::new()
    }

    fn local_operations() -> Vec<ActionSpec> {
        operation_resource(None, false, false, true)
            .actions
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Framework app.
pub struct ImagerShell;

impl ResourceApp for ImagerShell {
    type Resource = Imager;
    const ID: &'static str = "so.tilo.imager";
    const TITLE: &'static str = "Disk Imager";
    const ICON: Option<&'static str> = Some("imager");
    const SHOT_VIEWPORT: (u32, u32) = (1180, 820);

    fn view(cx: &mut ViewCx<Imager>) -> Element {
        cx.surface(ImagerApp::new())
    }
}

impl ResourceSurface<Imager> for ImagerApp {
    fn present(&mut self, state: &Rc<ImagerState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Imager>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher for the selected image and install profile.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Imager>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Imager>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &ImagerAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = EncodedAction::encode(IMAGER_ACTION_TYPE, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => {
                self.local
                    .dispatch_with_context(doc, type_id, action, context);
            }
        }
    }

    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Hosted commits are recorded by the dispatcher. Standalone tests still
    /// read the local host's rising edge.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }
}
