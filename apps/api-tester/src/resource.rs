//! API Tester as a [`tilo_resource::Resource`]: the shared workspace —
//! collections, environments, the request in the builder and the history —
//! shown and edited by [`ApiTesterApp`].

use std::rc::Rc;

use automerge::ROOT;
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_actions::ActionSpec;
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, PlainText, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    API_TESTER_ACTION_TYPE, ApiTesterAction, ApiTesterEffect, body_resource_id, document_children,
    register, resolve_child, search_entries, set_method_declaration, url_resource_id,
    use_environment_declaration, workspace_bound_actions,
};
use crate::app::ApiTesterApp;
use crate::doc::ApiDoc;
use crate::model::{Collection, Environment, HistoryEntry, Request};

/// The shared workspace as the views read it. Credentials are not in it:
/// they stay in the device's secret store.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub collections: Vec<Collection>,
    pub environments: Vec<Environment>,
    pub active_env: Option<uuid::Uuid>,
    /// The request in the builder.
    pub request: Request,
    /// Completed requests, newest first.
    pub history: Vec<HistoryEntry>,
    /// Exact resource addresses, rebuilt once with each authoritative projection.
    pub(crate) index: std::collections::HashMap<String, crate::actions::RecordLocation>,
    #[cfg(test)]
    pub(crate) exact_inspected: std::cell::Cell<usize>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            collections: Vec::new(),
            environments: Vec::new(),
            active_env: None,
            request: blank_request(),
            history: Vec::new(),
            index: std::collections::HashMap::new(),
            #[cfg(test)]
            exact_inspected: std::cell::Cell::new(0),
        }
    }
}

/// The API workspace resource.
pub struct ApiTester;

impl Resource for ApiTester {
    const KIND: &'static str = "api_tester.workspace";
    const LABEL: &'static str = "API workspace";
    const ACTION_TYPE: &'static str = API_TESTER_ACTION_TYPE;

    type State = Workspace;
    type Action = ApiTesterAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<Workspace, ModelError> {
        let mut state = Workspace {
            collections: ApiDoc::collections(doc, &ROOT),
            environments: ApiDoc::environments(doc, &ROOT),
            active_env: ApiDoc::active_env(doc, &ROOT).and_then(|id| id.parse().ok()),
            request: ApiDoc::request(doc, &ROOT).unwrap_or_else(blank_request),
            history: ApiDoc::history(doc, &ROOT),
            index: std::collections::HashMap::new(),
            #[cfg(test)]
            exact_inspected: std::cell::Cell::new(0),
        };
        state.index = crate::actions::resource_index(&state);
        Ok(state)
    }

    /// A new workspace opens on one blank request, the same on every device
    /// (its id is the nil id), with the URL and body texts two people type
    /// into before they have synced.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        ApiDoc::set_request(doc, &ROOT, &blank_request());
        Ok(())
    }

    /// Typing in the URL or the body is one undo step a word at a time.
    fn coalesces(previous: &ApiTesterAction, next: &ApiTesterAction) -> bool {
        match (previous, next) {
            (ApiTesterAction::EditUrl(previous), ApiTesterAction::EditUrl(next))
            | (ApiTesterAction::EditBody(previous), ApiTesterAction::EditBody(next)) => {
                PlainText::coalesces(previous, next)
            }
            _ => false,
        }
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![set_method_declaration()]
    }

    fn bound_declarations(state: &Workspace) -> Vec<ActionDecl<Self>> {
        vec![use_environment_declaration(state)]
    }

    fn bound_actions(_state: &Workspace) -> Vec<ActionSpec> {
        workspace_bound_actions()
    }

    fn children(state: &Workspace) -> Vec<tilo_actions::Resource> {
        document_children(state)
    }

    fn child(state: &Workspace, id: &str) -> Option<tilo_actions::Resource> {
        resolve_child(state, id)
    }

    fn search(state: &Workspace) -> Vec<SearchEntry> {
        search_entries(state)
    }
}

/// The request a new workspace opens on.
pub fn blank_request() -> Request {
    let mut request = crate::seed::blank_request();
    request.id = uuid::Uuid::nil();
    request
}

/// Edits of the builder's URL through the workspace's dispatcher.
pub(crate) fn url_dispatcher(dispatcher: &Dispatcher<ApiTester>) -> Dispatcher<PlainText> {
    dispatcher.child::<PlainText>(&url_resource_id(), ApiTesterAction::EditUrl)
}

/// Edits of the builder's body through the workspace's dispatcher.
pub(crate) fn body_dispatcher(dispatcher: &Dispatcher<ApiTester>) -> Dispatcher<PlainText> {
    dispatcher.child::<PlainText>(&body_resource_id(), ApiTesterAction::EditBody)
}

/// Framework app.
pub struct ApiTesterShell;

impl ApiTesterShell {
    /// The view state a screenshot scenario stages: a contextual form, the
    /// rail, the URL's focus, a failed send.
    pub const SCENARIOS: &'static [&'static str] = &[
        "empty",
        "rail-open",
        "error",
        "focus-url",
        "form-new-collection",
        "form-save-request",
        "form-rename-collection",
        "form-rename-request",
        "form-new-environment",
        "form-rename-environment",
        "form-delete-environment",
        "form-new-variable",
        "form-delete-variable",
    ];
}

impl ResourceApp for ApiTesterShell {
    type Resource = ApiTester;
    const ID: &'static str = "so.tilo.api-tester";
    const TITLE: &'static str = "API Tester";
    const ICON: Option<&'static str> = Some("api-tester");

    fn view(cx: &mut ViewCx<ApiTester>) -> Element {
        cx.surface(ApiTesterApp::new_with_actions(&cx.actions()))
    }

    fn scenarios() -> &'static [&'static str] {
        Self::SCENARIOS
    }

    /// Every scenario but the empty one shows the sample workspace.
    fn scenario(name: Option<&str>) -> Vec<Invoke<ApiTester>> {
        if name == Some("empty") {
            return Vec::new();
        }
        crate::seed::workspace()
            .iter()
            .filter_map(|action| Invoke::<ApiTester>::apply(action).ok())
            .collect()
    }
}

impl ResourceSurface<ApiTester, ApiTesterEffect> for ApiTesterApp {
    fn present(&mut self, state: &Rc<Workspace>) {
        self.present_workspace(state);
    }

    fn attach(&mut self, cx: SurfaceCx<ApiTester>) {
        self.attach_surface(&cx);
    }

    fn read_only_changed(&mut self, read_only: bool) {
        self.set_read_only(read_only);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: ApiTesterEffect,
        cx: &mut EffectCx<ApiTester, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }
}
