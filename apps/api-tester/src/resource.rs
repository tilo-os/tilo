//! API Tester as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path. HTTP send is an Effect in [`Resource::local_operations`].

use std::rc::Rc;

use automerge::{AutoCommit, ROOT};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{API_TESTER_ACTION_TYPE, ApiTesterAction, active_request_resource, register};
use crate::app::ApiTesterApp;
use crate::doc::ApiDoc;

/// Lightweight workspace projection for search and the published tree.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceState {
    pub collections: Vec<(String, String)>,
    pub environments: Vec<(String, String)>,
}

/// The API workspace resource.
pub struct ApiTester;

impl Resource for ApiTester {
    const KIND: &'static str = "api_tester.workspace";
    const LABEL: &'static str = "API workspace";
    const ACTION_TYPE: &'static str = API_TESTER_ACTION_TYPE;

    type State = WorkspaceState;
    type Action = ApiTesterAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<WorkspaceState, ModelError> {
        let collections = ApiDoc::collections(doc, &ROOT)
            .into_iter()
            .map(|collection| (collection.id.to_string(), collection.name))
            .collect();
        let environments = ApiDoc::environments(doc, &ROOT)
            .into_iter()
            .map(|environment| (environment.id.to_string(), environment.name))
            .collect();
        Ok(WorkspaceState {
            collections,
            environments,
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Prompted new-collection / new-environment stay on the published tree
        // (param `value`, continuation identity). Stub declarations here would
        // shadow those prompts.
        Vec::new()
    }

    fn local_operations() -> Vec<ActionSpec> {
        active_request_resource()
            .actions
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &WorkspaceState) -> Vec<SearchEntry> {
        let collections = state.collections.iter().map(|(id, name)| SearchEntry {
            anchor: Some(format!("api_tester.collection:{id}")),
            title: name.clone(),
            text: String::new(),
        });
        let environments = state.environments.iter().map(|(id, name)| SearchEntry {
            anchor: Some(format!("api_tester.environment:{id}")),
            title: name.clone(),
            text: String::new(),
        });
        collections.chain(environments).collect()
    }
}

/// Framework app.
pub struct ApiTesterShell;

impl ResourceApp for ApiTesterShell {
    type Resource = ApiTester;
    const ID: &'static str = "so.tilo.api-tester";
    const TITLE: &'static str = "API Tester";
    const ICON: Option<&'static str> = Some("api-tester");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<ApiTester>) -> Element {
        cx.surface(ApiTesterApp::new())
    }
}

impl ResourceSurface<ApiTester> for ApiTesterApp {
    fn present(&mut self, state: &Rc<WorkspaceState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<ApiTester>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher for collections, environments, and saved requests.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<ApiTester>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<ApiTester>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &ApiTesterAction,
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
