//! Backups as a [`tilo_resource::Resource`]: retention policy is document
//! state; backup/restore runs are Effects in [`Resource::local_operations`].

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{BACKUPS_ACTION_TYPE, BackupsAction, provide_resources, register};
use crate::app::BackupsApp;
use crate::doc::BackupsDoc;
use crate::model::{Catalog, RetentionPolicy};

/// Retention policy projection from the shared document.
#[derive(Clone, Debug, Default)]
pub struct BackupsState {
    pub policy: RetentionPolicy,
}

/// The backups workspace resource.
pub struct Backups;

impl Resource for Backups {
    const KIND: &'static str = "backups.workspace";
    const LABEL: &'static str = "Backups";
    const ACTION_TYPE: &'static str = BACKUPS_ACTION_TYPE;

    type State = BackupsState;
    type Action = BackupsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<BackupsState, ModelError> {
        Ok(BackupsState {
            policy: BackupsDoc::policy(doc, &automerge::ROOT),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        Vec::new()
    }

    fn local_operations() -> Vec<ActionSpec> {
        provide_resources(&RetentionPolicy::default(), &Catalog::empty(), false)
            .into_iter()
            .filter(|resource| resource.kind == "backups.workspace")
            .flat_map(|resource| resource.actions)
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Framework app.
pub struct BackupsShell;

impl ResourceApp for BackupsShell {
    type Resource = Backups;
    const ID: &'static str = "so.tilo.backups";
    const TITLE: &'static str = "Backups";
    const ICON: Option<&'static str> = Some("backups");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Backups>) -> Element {
        cx.surface(BackupsApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "status",
            "progress",
            "failure",
            "restore",
            "sources",
            "destinations",
            "schedules",
            "unavailable",
        ]
    }
}

impl ResourceSurface<Backups> for BackupsApp {
    fn present(&mut self, state: &std::rc::Rc<BackupsState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Backups>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Backups>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Backups>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &BackupsAction,
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
