//! Root widget for the API tester.
//!
//! The window is one [`NavShell`]: the sidebar lists saved requests by
//! collection and the history of what was sent; the content is the request
//! builder over the response, split by a divider the person can drag.
//!
//! The app composes the toolkit's widgets and paints nothing of its own. Its
//! controls tell the app what happened by pushing a [`Cmd`] onto a queue; the
//! app applies the queue where it has the shared document, keeps the model,
//! and replaces the parts of the view that depend on it ([`Swap`] slots), so
//! the controls around them keep their focus.
//!
//! The method, the URL and the body of the request are collaborative text in
//! the document and their controls bind to it directly; the rest of the
//! request is the draft this person is composing until they save it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};

use automerge::{AutoCommit, ObjId};
use ui::widget::resizable_split::SplitOrientation;
use ui::widget::{
    AppModel, Column, Dropdown, Element, ModelPath, NavShell, ResizableSplit, Spacer, Stack, Widget,
};
use winit::event::WindowEvent;

use crate::actions::{self, API_TESTER_ACTION_TYPE, ApiTesterAction};
use crate::commands::{AuthField, Cmd, FormMode, FormTarget, RowEdit, Sink, Table};
use crate::dialog::{self, Fields};
use crate::doc::ApiDoc;
use crate::http::{self, Outgoing};
use crate::kv::{KvChange, KvQueue};
use crate::model::{
    Auth, BodyMode, Collection, Environment, HistoryEntry, HttpMethod, KeyValue, Request,
    RequestTab, ResponseData, ResponseView,
};
use crate::request::{self, BlankRows, METHOD_PATH};
use crate::response;
use crate::sidebar::{self, SidebarView};
use crate::swap::{Following, Swap, SwapHandle};
use crate::vars;

/// CRDT key the environment picker's value lives at (the env name, or
/// [`NO_ENV`]).
const ENV_PATH: &str = "sel_env";
/// What the picker reads when no environment is chosen.
const NO_ENV: &str = "No environment";

/// What stands where a sheet goes while none is open.
fn no_dialog() -> Element {
    Box::new(Spacer::token(ui::tokens::SpacingStep::S0))
}

/// The send action of the request being composed.
#[allow(
    clippy::expect_used,
    reason = "the static active-request resource always publishes its nullary send operation"
)]
pub(crate) fn active_send_binding() -> tilo_actions::ActionBinding<()> {
    let resource = actions::active_request_resource();
    tilo_actions::ActionBinding::unit_for(&resource, "send")
        .expect("active request resource publishes a nullary send action")
}

/// Outcome of the background send worker.
enum SendOutcome {
    Ok {
        response: ResponseData,
        method: HttpMethod,
        url: String,
    },
    Err(String),
}

/// What to rebuild at the next synchronization.
#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag names an independent part of the view"
)]
struct Stale {
    sidebar: bool,
    header: bool,
    environments: bool,
    request: bool,
    tab_panel: bool,
    body_inner: bool,
    auth_inner: bool,
    response: bool,
    response_body: bool,
}

/// The slots of the view the app replaces in place.
struct Slots {
    sidebar: SwapHandle,
    request: SwapHandle,
    tab_panel: Option<SwapHandle>,
    panel_inner: Option<SwapHandle>,
    response: SwapHandle,
    response_body: Option<SwapHandle>,
    environments: SwapHandle,
    dialog: SwapHandle,
}

/// A state of the shell that exists only once the window has been laid out.
#[derive(Clone, Copy)]
enum Pose {
    SidebarOpen,
    SidebarClosed,
}

/// An open sheet and what it collects.
struct OpenForm {
    mode: FormMode,
    target: FormTarget,
    fields: Fields,
}

/// The model half of a freshly built [`ApiTesterApp`].
struct BuiltModel {
    collections: Vec<Collection>,
    environments: Vec<Environment>,
    active_env: Option<uuid::Uuid>,
    request: Request,
    history: Vec<HistoryEntry>,
}

/// Root widget: the whole API tester.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (an in-flight send, one-time seed/adopt \
              bookkeeping, and repaint/persist dirty flags), so grouping them would need a real \
              design pass rather than a mechanical lint fix"
)]
pub struct ApiTesterApp {
    nav: NavShell,

    // --- Model ---
    collections: Vec<Collection>,
    environments: Vec<Environment>,
    active_env: Option<uuid::Uuid>,
    request: Request,
    history: Vec<HistoryEntry>,
    response: Option<ResponseData>,
    sending: bool,
    error: Option<String>,

    // --- View state (local, per-peer) ---
    active_tab: RequestTab,
    response_view: ResponseView,
    selected: Option<(usize, usize)>,
    form: Option<OpenForm>,
    blanks: BlankRows,
    table: Option<(Table, KvQueue)>,
    stale: Stale,
    /// A shell state to take at the next paint, for `--shots`.
    pose: Option<Pose>,
    slots: Slots,
    /// The active environment's variables, for the URL's variable hint.
    env_vars: Rc<RefCell<HashMap<String, String>>>,

    /// Commands pushed by widget callbacks, applied after event routing.
    sink: Sink,

    /// Framework-owned action dispatch: every collaborative doc mutation (create
    /// / rename / delete a collection or saved request, add / edit / delete an
    /// environment or variable, select the active environment) is encoded and
    /// applied through this host, which brackets the undo boundary and remembers
    /// the last action — the same path a Ctrl+K palette dispatch takes.
    actions: crate::resource::ActionSink,
    search_provider: crate::search_provider::ApiSearchProvider,
    search_error: Option<ui::search::SearchCatalogError>,

    // --- Worker plumbing ---
    tx: Sender<SendOutcome>,
    rx: Receiver<SendOutcome>,
    resource_send_invocation: Option<tilo_actions::InvocationId>,
    resource_send_result: Option<tilo_actions::InvocationUpdate>,
    pending_history: Vec<HistoryEntry>,

    seeded: bool,
    adopted: bool,
    /// Mounted on a record another participant created: its content is
    /// theirs to seed, so an empty document is left untouched.
    joined: bool,
    dirty: bool,
    persisted_once: bool,
}

impl ApiTesterApp {
    /// The live app: no fabricated saved collections/history and NO canned
    /// response — the response pane is empty until a real Send. It opens on a
    /// single blank starter request.
    pub fn new() -> Self {
        Self::build(false)
    }

    /// The hermetic `--shots` / demo build: a sample collection, environments,
    /// history and a rendered example response so the screenshot is populated.
    pub fn new_demo() -> Self {
        Self::build(true)
    }

    fn build(demo: bool) -> Self {
        crate::locales::install();
        let (tx, rx) = std::sync::mpsc::channel();
        let model = Self::build_model(demo);
        let sink: Sink = Rc::new(RefCell::new(Vec::new()));
        let blanks = BlankRows::new();
        let env_vars = Rc::new(RefCell::new(HashMap::new()));
        let response = demo.then(http::sample_response);
        let selected = demo.then_some((0, 0));

        let builtin = request::section(&model.request, RequestTab::Params, &blanks, &sink);
        let (request_slot, request) = Swap::new(builtin.element);
        let (response_slot, response_handle, response_body) =
            Self::response_slot(response.as_ref(), &sink);
        let (dialog_slot, dialog) = Swap::new(no_dialog());
        let (env_slot, environments) = Swap::new(Self::env_dropdown(&model.environments));
        let (lists_slot, sidebar_lists) = Swap::new(sidebar::lists(
            &SidebarView {
                collections: &model.collections,
                history: &model.history,
                selected,
            },
            &sink,
        ));

        let split = Following::new(
            ResizableSplit::new(request_slot, response_slot, SplitOrientation::Vertical)
                .with_split_position(0.45),
        );
        let content = Stack::new()
            .push(Box::new(
                Column::new()
                    .push(request::url_bar(&sink))
                    .push(request::variable_hint(Rc::clone(&env_vars)))
                    .push(Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_fill_remaining_height()
                            .push(Box::new(split)),
                    )),
            ))
            .push(Box::new(dialog_slot));

        let nav = NavShell::new()
            .with_id("api.shell")
            .sidebar(sidebar::frame(Box::new(env_slot), Box::new(lists_slot)))
            .content(Box::new(content))
            .content_header(
                model.request.name.clone(),
                vec![Box::new(ui::search::search_button(
                    ui::search::SearchRequest::this_app(),
                ))],
            );

        let mut app = Self {
            nav,
            collections: model.collections,
            environments: model.environments,
            active_env: model.active_env,
            request: model.request,
            history: model.history,
            response,
            sending: false,
            error: None,
            active_tab: RequestTab::Params,
            response_view: ResponseView::Pretty,
            selected,
            form: None,
            blanks,
            table: builtin.table,
            stale: Stale::default(),
            pose: None,
            slots: Slots {
                sidebar: sidebar_lists,
                request,
                tab_panel: Some(builtin.slot),
                panel_inner: builtin.inner,
                response: response_handle,
                response_body,
                environments,
                dialog,
            },
            env_vars,
            sink,
            actions: crate::resource::ActionSink::default(),
            search_provider: crate::search_provider::ApiSearchProvider::new(),
            search_error: None,
            tx,
            rx,
            resource_send_invocation: None,
            resource_send_result: None,
            pending_history: Vec::new(),
            seeded: false,
            adopted: false,
            joined: ui::launch::record_launch() == Some(ui::launch::RecordLaunch::Joined),
            dirty: true,
            persisted_once: false,
        };
        app.refresh_env_vars();
        app.refresh_search();
        app
    }

    /// The response pane's slot, showing `response` or the empty state.
    fn response_slot(
        response: Option<&ResponseData>,
        sink: &Sink,
    ) -> (Swap, SwapHandle, Option<SwapHandle>) {
        if let Some(data) = response {
            let built = response::response(data, ResponseView::Pretty, sink);
            let (slot, handle) = Swap::new(built.element);
            (slot, handle, Some(built.body))
        } else {
            let (slot, handle) = Swap::new(response::empty());
            (slot, handle, None)
        }
    }

    /// The model half of a fresh app: sample data when `demo`, otherwise
    /// empty/blank.
    fn build_model(demo: bool) -> BuiltModel {
        let collections = if demo {
            crate::seed::collections()
        } else {
            Vec::new()
        };
        let environments = if demo {
            crate::seed::environments()
        } else {
            Vec::new()
        };
        let active_env = environments.first().map(|e| e.id);
        let request = if demo {
            crate::seed::active_request()
        } else {
            crate::seed::blank_request()
        };
        let history = if demo {
            crate::seed::history()
        } else {
            Vec::new()
        };
        BuiltModel {
            collections,
            environments,
            active_env,
            request,
            history,
        }
    }

    // -----------------------------------------------------------------------
    // Screenshot staging
    // -----------------------------------------------------------------------

    /// Show the state a named `--shots` scenario captures, on top of the demo
    /// workspace. Returns whether the name is known.
    pub fn stage(&mut self, scenario: &str) -> bool {
        let known = match scenario {
            "default" => true,
            "empty" => {
                *self = Self::new();
                true
            }
            "error" => {
                self.response = None;
                self.sending = false;
                self.error = Some(
                    "Could not reach api.example.com. Check the URL or your network connection."
                        .into(),
                );
                self.stale.response = true;
                true
            }
            "headers" => {
                self.active_tab = RequestTab::Headers;
                self.stale.request = true;
                true
            }
            "auth" => {
                self.active_tab = RequestTab::Auth;
                self.stale.request = true;
                true
            }
            "body" => {
                self.load_saved(0, 2);
                self.active_tab = RequestTab::Body;
                self.stale.request = true;
                true
            }
            "form-body" => {
                self.load_saved(1, 0);
                self.active_tab = RequestTab::Body;
                self.stale.request = true;
                true
            }
            "response-raw" => {
                self.response_view = ResponseView::Raw;
                self.stale.response = true;
                true
            }
            "response-headers" => {
                self.response_view = ResponseView::Headers;
                self.stale.response = true;
                true
            }
            "drawer" => {
                self.pose = Some(Pose::SidebarOpen);
                true
            }
            "sidebar-hidden" => {
                self.pose = Some(Pose::SidebarClosed);
                true
            }
            other => other
                .strip_prefix("form-")
                .and_then(Self::form_scenario)
                .map(|mode| self.open_form(mode, FormTarget::default()))
                .is_some(),
        };
        self.dirty = true;
        known
    }

    fn form_scenario(name: &str) -> Option<FormMode> {
        Some(match name {
            "new-collection" => FormMode::NewCollection,
            "save-request" => FormMode::SaveRequest,
            "rename-collection" => FormMode::RenameCollection,
            "rename-request" => FormMode::RenameRequest,
            "delete-request" => FormMode::DeleteRequest,
            "new-environment" => FormMode::NewEnvironment,
            "rename-environment" => FormMode::RenameEnvironment,
            "delete-environment" => FormMode::DeleteEnvironment,
            "new-variable" => FormMode::NewVariable,
            "delete-variable" => FormMode::DeleteVariable,
            _ => return None,
        })
    }

    /// Every scenario name `stage` accepts.
    pub const SCENARIOS: &'static [&'static str] = &[
        "default",
        "empty",
        "error",
        "headers",
        "auth",
        "body",
        "form-body",
        "response-raw",
        "response-headers",
        "drawer",
        "sidebar-hidden",
        "form-new-collection",
        "form-save-request",
        "form-rename-collection",
        "form-rename-request",
        "form-delete-request",
        "form-new-environment",
        "form-rename-environment",
        "form-delete-environment",
        "form-new-variable",
        "form-delete-variable",
    ];

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    fn env_dropdown(environments: &[Environment]) -> Element {
        let mut options = vec![NO_ENV.to_string()];
        options.extend(environments.iter().map(|e| e.name.clone()));
        Box::new(
            Dropdown::new(options, ENV_PATH)
                .with_id("api.env")
                .caption("Environment"),
        )
    }

    fn refresh_env_vars(&self) {
        *self.env_vars.borrow_mut() = self.env_vars();
    }

    /// Replace the parts of the view whose model changed.
    fn refresh_views(&mut self) {
        if std::mem::take(&mut self.stale.sidebar) {
            self.slots.sidebar.set(sidebar::lists(
                &SidebarView {
                    collections: &self.collections,
                    history: &self.history,
                    selected: self.selected,
                },
                &self.sink,
            ));
        }
        if std::mem::take(&mut self.stale.header) {
            self.nav.set_content_title(self.request.name.clone());
        }
        if std::mem::take(&mut self.stale.environments) {
            self.slots
                .environments
                .set(Self::env_dropdown(&self.environments));
        }
        if std::mem::take(&mut self.stale.request) {
            let built = request::section(&self.request, self.active_tab, &self.blanks, &self.sink);
            self.slots.request.set(built.element);
            self.slots.tab_panel = Some(built.slot);
            self.slots.panel_inner = built.inner;
            self.table = built.table;
            self.stale.tab_panel = false;
            self.stale.body_inner = false;
            self.stale.auth_inner = false;
        }
        if std::mem::take(&mut self.stale.tab_panel) {
            let panel = request::panel(&self.request, self.active_tab, &self.blanks, &self.sink);
            if let Some(slot) = &self.slots.tab_panel {
                slot.set(panel.element);
            }
            self.slots.panel_inner = panel.inner;
            self.table = panel.table;
            self.stale.body_inner = false;
            self.stale.auth_inner = false;
        }
        if std::mem::take(&mut self.stale.body_inner) {
            let (element, table) = request::body_inner(&self.request, &self.blanks, &self.sink);
            if let Some(slot) = &self.slots.panel_inner {
                slot.set(element);
            }
            self.table = table;
        }
        if std::mem::take(&mut self.stale.auth_inner)
            && let Some(slot) = &self.slots.panel_inner
        {
            slot.set(request::auth_inner(&self.request.auth, &self.sink));
        }
        if std::mem::take(&mut self.stale.response) {
            self.rebuild_response();
        }
        if std::mem::take(&mut self.stale.response_body)
            && let (Some(slot), Some(data)) = (&self.slots.response_body, &self.response)
        {
            slot.set(response::body(data, self.response_view));
        }
    }

    fn rebuild_response(&mut self) {
        if self.sending {
            self.slots.response.set(response::sending());
            self.slots.response_body = None;
            self.stale.response_body = false;
            return;
        }
        match (&self.response, &self.error) {
            (Some(data), _) => {
                let built = response::response(data, self.response_view, &self.sink);
                self.slots.response.set(built.element);
                self.slots.response_body = Some(built.body);
            }
            (None, Some(error)) => {
                self.slots.response.set(response::failed(error));
                self.slots.response_body = None;
            }
            (None, None) => {
                self.slots.response.set(response::empty());
                self.slots.response_body = None;
            }
        }
        self.stale.response_body = false;
    }

    // -----------------------------------------------------------------------
    // Sending
    // -----------------------------------------------------------------------

    /// The active environment's variable map (for `{{var}}` substitution).
    fn env_vars(&self) -> HashMap<String, String> {
        match self
            .active_env
            .and_then(|id| self.environments.iter().find(|e| e.id == id))
        {
            Some(env) => vars::var_map(&env.variables),
            None => HashMap::new(),
        }
    }

    /// Resolve the request being composed against the active environment.
    fn outgoing(&self, doc: &AutoCommit, root: &ObjId) -> Outgoing {
        let mut req = self.request.clone();
        req.url = ApiDoc::read_text(doc, root, "url");
        req.body_text = ApiDoc::read_text(doc, root, "body");
        http::build_outgoing(&req, &self.env_vars())
    }

    /// Kick off a real request on the app runtime.
    fn send(&mut self, doc: &AutoCommit, root: &ObjId) {
        if self.sending {
            return;
        }
        let out = self.outgoing(doc, root);
        let method = self.request.method;
        self.sending = true;
        self.error = None;
        self.dirty = true;
        self.stale.response = true;
        let tx = self.tx.clone();
        drop(ui::rt::app().spawn(async move {
            let outcome = match http::send(&out).await {
                Ok(response) => SendOutcome::Ok {
                    response,
                    method,
                    url: out.url,
                },
                Err(e) => SendOutcome::Err(e),
            };
            let _ = tx.send(outcome);
        }));
    }

    /// Drain any finished send result from the worker.
    fn poll_worker(&mut self) {
        while let Ok(outcome) = self.rx.try_recv() {
            self.sending = false;
            self.dirty = true;
            match outcome {
                SendOutcome::Ok {
                    response: resp,
                    method,
                    url,
                } => {
                    if let Some(invocation) = self.resource_send_invocation.take() {
                        self.resource_send_result =
                            Some(tilo_actions::InvocationUpdate::Succeeded {
                                invocation,
                                output: tilo_actions::ActionOutput::default(),
                            });
                    }
                    self.history.insert(
                        0,
                        HistoryEntry {
                            id: uuid::Uuid::new_v4(),
                            method,
                            url,
                            status: resp.status,
                            duration_ms: resp.duration_ms,
                            when: "just now".into(),
                        },
                    );
                    self.history.truncate(50);
                    if let Some(entry) = self.history.first() {
                        self.pending_history.push(entry.clone());
                    }
                    self.response = Some(resp);
                    self.response_view = ResponseView::Pretty;
                    self.stale.response = true;
                    self.stale.sidebar = true;
                }
                SendOutcome::Err(e) => {
                    if let Some(invocation) = self.resource_send_invocation.take() {
                        self.resource_send_result = Some(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "api_tester.request_failed".into(),
                                message: e.clone(),
                                details: None,
                                retryable: false,
                            },
                        });
                    }
                    self.error = Some(e);
                    self.response = None;
                    self.stale.response = true;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // The saved workspace
    // -----------------------------------------------------------------------

    /// Show a saved request from a collection in the builder, with the
    /// document untouched. Used before the document is first synchronized.
    fn load_saved(&mut self, ci: usize, ri: usize) {
        if let Some(req) = self
            .collections
            .get(ci)
            .and_then(|c| c.requests.get(ri))
            .cloned()
        {
            self.request = req;
            self.selected = Some((ci, ri));
            self.active_tab = RequestTab::Params;
            self.stale.request = true;
            self.stale.header = true;
            self.stale.sidebar = true;
            self.dirty = true;
        }
    }

    /// Load a saved request from a collection into the builder and the
    /// document.
    fn load_request(&mut self, state: &mut AutoCommit, path: &ObjId, ci: usize, ri: usize) {
        self.load_saved(ci, ri);
        self.write_request_fields(state, path);
        self.dismiss_drawer();
    }

    /// Choosing from a floating sidebar is done with it: show the request.
    fn dismiss_drawer(&mut self) {
        if self.nav.geometry().sidebar == ui::widget::SidebarPlacement::Drawer {
            self.nav.close_sidebar();
        }
    }

    /// Mirror the composed request's method, URL and body into the document.
    fn write_request_fields(&self, state: &mut AutoCommit, path: &ObjId) {
        ApiDoc::set_text(state, path, "url", &self.request.url);
        ApiDoc::set_text(state, path, "body", &self.request.body_text);
        ApiDoc::set_text(state, path, METHOD_PATH, self.request.method.as_str());
    }

    /// Seed a previously empty document through the native per-resource
    /// writers. Normal interactions use precise actions and never call this.
    fn seed_workspace(&self, state: &mut AutoCommit, root: &ObjId) {
        ApiDoc::set_request(state, root, &self.request);
        ApiDoc::set_collections(state, root, &self.collections);
        ApiDoc::set_environments(state, root, &self.environments);
        ApiDoc::set_active_env(
            state,
            root,
            self.active_env.map(|id| id.to_string()).as_deref(),
        );
        ApiDoc::set_history(state, root, &self.history);
    }

    /// Adopt a peer's workspace from the shared CRDT.
    fn adopt(&mut self, state: &AutoCommit, root: &ObjId) {
        let cols = ApiDoc::collections(state, root);
        if !cols.is_empty() {
            self.collections = cols;
        }
        let envs = ApiDoc::environments(state, root);
        if !envs.is_empty() {
            self.environments = envs;
        }
        if let Some(id) = ApiDoc::active_env(state, root) {
            self.active_env = id.parse().ok();
        }
        if let Some(req) = ApiDoc::request(state, root) {
            self.request = req;
            self.stale.request = true;
        }
        let hist = ApiDoc::history(state, root);
        if !hist.is_empty() {
            self.history = hist;
        }
        self.adopted = true;
        self.dirty = true;
        self.workspace_changed();
    }

    /// The collections, environments or history changed: rebuild what shows
    /// them.
    fn workspace_changed(&mut self) {
        self.stale.sidebar = true;
        self.stale.environments = true;
        self.stale.header = true;
        self.refresh_env_vars();
        self.refresh_search();
    }

    /// Show collection and environment names the framework resource holds.
    /// An empty projection does not wipe a workspace that has not been published yet.
    /// Requests stay on the document; `sync` reads those full records.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::WorkspaceState) {
        if state.collections.is_empty()
            && state.environments.is_empty()
            && (!self.collections.is_empty() || !self.environments.is_empty())
        {
            return;
        }
        let mut changed = false;
        for (id, name) in &state.collections {
            let Ok(id) = id.parse::<crate::model::Id>() else {
                continue;
            };
            if let Some(collection) = self
                .collections
                .iter_mut()
                .find(|collection| collection.id == id)
                && collection.name != *name
            {
                collection.name.clone_from(name);
                changed = true;
            }
        }
        for (id, name) in &state.environments {
            let Ok(id) = id.parse::<crate::model::Id>() else {
                continue;
            };
            if let Some(environment) = self
                .environments
                .iter_mut()
                .find(|environment| environment.id == id)
                && environment.name != *name
            {
                environment.name.clone_from(name);
                changed = true;
            }
        }
        if changed {
            self.workspace_changed();
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::ApiTester>,
    ) {
        self.actions.attach(dispatcher);
    }

    #[cfg(test)]
    pub(crate) fn actions_are_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_actions_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Dispatch one canonical action against the document's current projection.
    /// Presentation commands never flush the in-memory workspace first; the
    /// action owns the precise durable record it mutates, then we re-adopt the
    /// authoritative result.
    fn perform(&mut self, state: &mut AutoCommit, path: &ObjId, action: &ApiTesterAction) {
        self.actions.dispatch_with_context(
            state,
            API_TESTER_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.api-tester", "API Tester", action),
        );
        self.adopt_workspace(state, path);
        self.show_active_environment(state, path);
        self.dirty = true;
    }

    /// Re-read the collaborative workspace snapshots (collections, environments,
    /// the selected environment) from the document after an action mutated them.
    /// Deliberately leaves the request being composed and per-peer view state
    /// untouched — those are not what an action changed.
    fn adopt_workspace(&mut self, state: &AutoCommit, path: &ObjId) {
        let kept = self.selected_request().map(|(_, request)| request);
        self.collections = ApiDoc::collections(state, path);
        self.environments = ApiDoc::environments(state, path);
        self.active_env = ApiDoc::active_env(state, path).and_then(|id| id.parse().ok());
        self.selected = kept.and_then(|id| self.position_of(id));
        self.workspace_changed();
    }

    /// Where the saved request `id` sits in the collections.
    fn position_of(&self, id: uuid::Uuid) -> Option<(usize, usize)> {
        self.collections
            .iter()
            .enumerate()
            .find_map(|(ci, collection)| {
                collection
                    .requests
                    .iter()
                    .position(|request| request.id == id)
                    .map(|ri| (ci, ri))
            })
    }

    fn refresh_search(&mut self) {
        self.search_error = self
            .search_provider
            .refresh(&self.collections, &self.environments, &self.history)
            .err();
    }

    pub(crate) fn resource_from_catalog(&self, id: &str) -> Option<tilo_actions::Resource> {
        self.search_provider.resource(id)
    }

    pub(crate) fn search_error_message(&self) -> Option<String> {
        self.search_error.as_ref().map(ToString::to_string)
    }

    #[cfg(test)]
    pub(crate) fn refresh_search_records(
        &mut self,
        collections: &[Collection],
        environments: &[Environment],
    ) {
        self.search_error = self
            .search_provider
            .refresh(collections, environments, &[])
            .err();
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    /// Drain the command sink and apply each command, persisting when a
    /// command changes shared state. Returns `true` if anything was applied.
    fn drain_commands(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let cmds: Vec<Cmd> = self.sink.borrow_mut().drain(..).collect();
        if cmds.is_empty() {
            return false;
        }
        for cmd in cmds {
            match cmd {
                Cmd::SetTab(i) => {
                    if let Some(&tab) = RequestTab::ALL.get(i)
                        && tab != self.active_tab
                    {
                        self.active_tab = tab;
                        self.stale.tab_panel = true;
                    }
                }
                Cmd::SetView(i) => {
                    if let Some(&view) = ResponseView::ALL.get(i)
                        && view != self.response_view
                    {
                        self.response_view = view;
                        self.stale.response_body = true;
                    }
                }
                Cmd::SetBodyMode(i) => {
                    if let Some(&mode) = BodyMode::ALL.get(i)
                        && mode != self.request.body_mode
                    {
                        self.request.body_mode = mode;
                        self.stale.body_inner = true;
                    }
                }
                Cmd::SetAuthKind(i) => self.set_auth_kind(i),
                Cmd::LoadRequest(ci, ri) => self.load_request(state, path, ci, ri),
                Cmd::ReplayHistory(hi) => self.replay_history(state, path, hi),
                Cmd::Edit { table, row, edit } => self.edit_rows(table, &row, edit),
                Cmd::EditAuth(field, text) => self.edit_auth(field, text),
                Cmd::OpenForm(mode, target) => self.open_form(mode, target),
                Cmd::CancelForm => self.close_form(),
                Cmd::SubmitForm => self.submit_form(state, path),
            }
        }
        self.dirty = true;
        true
    }

    fn replay_history(&mut self, state: &mut AutoCommit, path: &ObjId, index: usize) {
        if let Some(entry) = self.history.get(index).cloned() {
            // History keeps what was sent, not the rest of the request: it
            // opens as a new request.
            let mut request = crate::seed::blank_request();
            request.method = entry.method;
            request.url = entry.url;
            self.request = request;
            self.selected = None;
            self.error = None;
            self.active_tab = RequestTab::Params;
            self.write_request_fields(state, path);
            self.stale.request = true;
            self.stale.header = true;
            self.stale.sidebar = true;
            self.dismiss_drawer();
        }
    }

    fn set_auth_kind(&mut self, index: usize) {
        let next = match index {
            0 => Auth::None,
            1 => Auth::Bearer {
                token: String::new(),
            },
            2 => Auth::Basic {
                username: String::new(),
                password: String::new(),
            },
            _ => Auth::ApiKey {
                header: "X-Api-Key".into(),
                value: String::new(),
            },
        };
        if self.request.auth.label() != next.label() {
            self.request.auth = next;
            self.stale.auth_inner = true;
        }
    }

    fn edit_auth(&mut self, field: AuthField, text: String) {
        match (&mut self.request.auth, field) {
            (Auth::Bearer { token }, AuthField::Token) => *token = text,
            (Auth::Basic { username, .. }, AuthField::Username) => *username = text,
            (Auth::Basic { password, .. }, AuthField::Password) => *password = text,
            (Auth::ApiKey { header, .. }, AuthField::Header) => *header = text,
            (Auth::ApiKey { value, .. }, AuthField::Value) => *value = text,
            _ => {}
        }
    }

    fn rows_mut(&mut self, table: Table) -> &mut Vec<KeyValue> {
        match table {
            Table::Params => &mut self.request.params,
            Table::Headers => &mut self.request.headers,
            Table::Form => &mut self.request.form,
        }
    }

    /// Apply one edit to a row of a table. Typing in the blank row makes it a
    /// real pair and gives the table a new blank row.
    fn edit_rows(&mut self, table: Table, row: &str, edit: RowEdit) {
        let blank = self.blanks.of(table).to_owned();
        let queue = self
            .table
            .as_ref()
            .filter(|(shown, _)| *shown == table)
            .map(|(_, queue)| queue.clone());
        if row == blank {
            match edit {
                RowEdit::Remove => {
                    if let Some(queue) = &queue {
                        queue.push(KvChange::Clear(blank));
                    }
                }
                RowEdit::Enabled(_) => {}
                RowEdit::Key(_) | RowEdit::Value(_) => {
                    let mut pair = KeyValue::new("", "");
                    pair.id.clone_from(&blank);
                    apply_edit(&mut pair, edit);
                    if pair.is_blank() {
                        return;
                    }
                    self.rows_mut(table).push(pair);
                    let next = self.blanks.renew(table);
                    if let Some(queue) = &queue {
                        queue.push(KvChange::Activate(blank));
                        queue.push(KvChange::Append(next));
                    }
                }
            }
            return;
        }
        match edit {
            RowEdit::Remove => {
                self.rows_mut(table).retain(|pair| pair.id != row);
                if let Some(queue) = queue {
                    queue.push(KvChange::Remove(row.to_owned()));
                }
            }
            other => {
                if let Some(pair) = self.rows_mut(table).iter_mut().find(|pair| pair.id == row) {
                    apply_edit(pair, other);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Sheets
    // -----------------------------------------------------------------------

    fn current_collection_id(&self) -> Option<uuid::Uuid> {
        self.selected
            .and_then(|(index, _)| self.collections.get(index))
            .or_else(|| self.collections.first())
            .map(|collection| collection.id)
    }

    fn collection_named(&self, id: uuid::Uuid) -> Option<&Collection> {
        self.collections.iter().find(|c| c.id == id)
    }

    fn selected_request(&self) -> Option<(uuid::Uuid, uuid::Uuid)> {
        let (ci, ri) = self.selected?;
        let collection = self.collections.get(ci)?;
        Some((collection.id, collection.requests.get(ri)?.id))
    }

    /// The collection and request a sheet acts on: the row it was opened
    /// from, else the one in the builder.
    fn resolve_target(&self, target: FormTarget) -> FormTarget {
        let selected = self.selected_request();
        FormTarget {
            collection: target
                .collection
                .or_else(|| selected.map(|(collection, _)| collection))
                .or_else(|| self.current_collection_id()),
            request: target
                .request
                .or_else(|| selected.map(|(_, request)| request)),
        }
    }

    fn request_in(&self, target: FormTarget) -> Option<&Request> {
        self.collection_named(target.collection?)?
            .requests
            .iter()
            .find(|request| Some(request.id) == target.request)
    }

    fn active_environment(&self) -> Option<&Environment> {
        self.environments
            .iter()
            .find(|environment| Some(environment.id) == self.active_env)
    }

    fn open_form(&mut self, mode: FormMode, target: FormTarget) {
        let target = self.resolve_target(target);
        let (initial, subject) = match mode {
            FormMode::RenameCollection => {
                let name = target
                    .collection
                    .and_then(|id| self.collection_named(id))
                    .map_or(String::new(), |c| c.name.clone());
                (name.clone(), name)
            }
            FormMode::RenameRequest => {
                let name = self
                    .request_in(target)
                    .map_or(String::new(), |r| r.name.clone());
                (name.clone(), name)
            }
            FormMode::DeleteRequest => (
                String::new(),
                self.request_in(target)
                    .map_or(String::new(), |r| r.name.clone()),
            ),
            FormMode::RenameEnvironment => {
                let name = self
                    .active_environment()
                    .map_or(String::new(), |e| e.name.clone());
                (name.clone(), name)
            }
            FormMode::DeleteEnvironment => (
                String::new(),
                self.active_environment()
                    .map_or(String::new(), |e| e.name.clone()),
            ),
            FormMode::SaveRequest => (self.request.name.clone(), String::new()),
            FormMode::NewCollection
            | FormMode::NewEnvironment
            | FormMode::NewVariable
            | FormMode::DeleteVariable => (String::new(), String::new()),
        };
        let fields = Fields::new(&initial);
        self.slots
            .dialog
            .set(dialog::build(mode, &subject, &fields, &self.sink));
        self.form = Some(OpenForm {
            mode,
            target,
            fields,
        });
        self.dirty = true;
    }

    fn close_form(&mut self) {
        if self.form.take().is_some() {
            self.slots.dialog.set(no_dialog());
        }
        self.dirty = true;
    }

    fn open_request_menu_action(&mut self, action: &str) -> Result<(), String> {
        let mode = match action {
            "Save request…" => FormMode::SaveRequest,
            "Rename request…" => FormMode::RenameRequest,
            "Delete request…" => FormMode::DeleteRequest,
            _ => return Err(format!("unknown request action `{action}`")),
        };
        self.open_form(mode, FormTarget::default());
        Ok(())
    }

    fn open_environment_menu_action(&mut self, action: &str) -> Result<(), String> {
        let mode = match action {
            "Rename environment…" => FormMode::RenameEnvironment,
            "Add variable…" => FormMode::NewVariable,
            "Delete variable…" => FormMode::DeleteVariable,
            _ => return Err(format!("unknown environment action `{action}`")),
        };
        self.open_form(mode, FormTarget::default());
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive form-to-canonical-action mapping makes contextual management easy to audit"
    )]
    fn submit_form(&mut self, state: &mut AutoCommit, path: &ObjId) {
        let Some(form) = self.form.take() else {
            return;
        };
        self.slots.dialog.set(no_dialog());
        let OpenForm {
            mode,
            target,
            fields,
        } = form;
        let name = fields.name.get().trim().to_string();
        match mode {
            FormMode::NewCollection => {
                let name = if name.is_empty() {
                    format!("Collection {}", self.collections.len() + 1)
                } else {
                    name
                };
                self.perform(
                    state,
                    path,
                    &actions::create_collection_action(uuid::Uuid::new_v4(), &name),
                );
            }
            FormMode::RenameCollection => {
                if let Some(id) = target.collection.filter(|_| !name.is_empty()) {
                    self.perform(state, path, &actions::rename_collection_action(id, &name));
                }
            }
            FormMode::SaveRequest => {
                if let Some(collection_id) = target.collection {
                    let mut request = self.request.clone();
                    request.name = if name.is_empty() {
                        "Saved request".into()
                    } else {
                        name
                    };
                    request.url = ApiDoc::read_text(state, path, "url");
                    request.body_text = ApiDoc::read_text(state, path, "body");
                    request.method =
                        HttpMethod::from_str(&ApiDoc::read_text(state, path, METHOD_PATH));
                    self.request = request.clone();
                    self.perform(
                        state,
                        path,
                        &actions::save_request_action(collection_id, request),
                    );
                    self.selected = self.position_of(self.request.id);
                    self.stale.header = true;
                    self.stale.sidebar = true;
                }
            }
            FormMode::RenameRequest => {
                if let (Some(collection), Some(request)) = (target.collection, target.request)
                    && !name.is_empty()
                {
                    if self.request.id == request {
                        self.request.name.clone_from(&name);
                        self.stale.header = true;
                    }
                    self.perform(
                        state,
                        path,
                        &actions::rename_request_action(collection, request, &name),
                    );
                }
            }
            FormMode::DeleteRequest => {
                if let (Some(collection), Some(request)) = (target.collection, target.request) {
                    self.perform(
                        state,
                        path,
                        &actions::delete_request_action(collection, request),
                    );
                }
            }
            FormMode::NewEnvironment => {
                let name = if name.is_empty() {
                    format!("Environment {}", self.environments.len() + 1)
                } else {
                    name
                };
                self.perform(
                    state,
                    path,
                    &actions::create_environment_action(uuid::Uuid::new_v4(), &name),
                );
            }
            FormMode::RenameEnvironment => {
                if let Some(id) = self.active_env.filter(|_| !name.is_empty()) {
                    self.perform(state, path, &actions::rename_environment_action(id, &name));
                }
            }
            FormMode::DeleteEnvironment => {
                if let Some(id) = self.active_env {
                    self.perform(state, path, &actions::delete_environment_action(id));
                }
            }
            FormMode::NewVariable => {
                let key = fields.key.get().trim().to_string();
                let value = fields.value.get().trim().to_string();
                if let Some(env_id) = self.active_env.filter(|_| !key.is_empty()) {
                    let stable_id = self
                        .environments
                        .iter()
                        .find(|env| env.id == env_id)
                        .and_then(|env| env.variables.iter().find(|row| row.key == key))
                        .map_or_else(|| uuid::Uuid::new_v4().to_string(), |row| row.id.clone());
                    self.perform(
                        state,
                        path,
                        &actions::set_variable_action(env_id, stable_id, &key, &value),
                    );
                }
            }
            FormMode::DeleteVariable => {
                let key = fields.key.get().trim().to_string();
                if let Some(env_id) = self.active_env.filter(|_| !key.is_empty())
                    && let Some(variable) = self
                        .environments
                        .iter()
                        .find(|env| env.id == env_id)
                        .and_then(|env| env.variables.iter().find(|row| row.key == key))
                {
                    self.perform(
                        state,
                        path,
                        &actions::delete_variable_action(env_id, &variable.id),
                    );
                }
            }
        }
        self.dirty = true;
    }

    /// Reconcile the model from the CRDT-bound pickers after they may have
    /// changed the document.
    ///
    /// The method picker feeds the *request being composed* (presentation until
    /// a whole-request save), so it updates the model directly. Selecting the
    /// active **environment** is collaborative workspace state persisted in the
    /// shared document, so that change is routed through the action host — the
    /// same [`SetActiveEnvironment`](ApiTesterAction::SetActiveEnvironment) a
    /// Ctrl+K palette dispatch would apply.
    fn reconcile_pickers(&mut self, state: &mut AutoCommit, path: &ObjId) {
        let method = ApiDoc::read_text(state, path, METHOD_PATH);
        if !method.is_empty() {
            let method = HttpMethod::from_str(&method);
            if method != self.request.method {
                self.request.method = method;
                self.dirty = true;
            }
        }
        // A name no environment has any more is a stale picker (the active
        // environment was just renamed or deleted), not a choice.
        let chosen = ApiDoc::read_text(state, path, ENV_PATH);
        let next = if chosen == NO_ENV {
            Some(None)
        } else {
            self.environments
                .iter()
                .find(|e| e.name == chosen)
                .map(|e| Some(e.id))
        };
        if let Some(next) = next
            && next != self.active_env
        {
            self.perform(state, path, &actions::set_active_environment_action(next));
        }
    }

    /// Make the environment picker read the active environment, whichever way
    /// it changed (a rename, a deletion, the palette).
    fn show_active_environment(&self, state: &mut AutoCommit, path: &ObjId) {
        let name = self.active_environment().map_or_else(
            || NO_ENV.to_string(),
            |environment| environment.name.clone(),
        );
        ApiDoc::set_text(state, path, ENV_PATH, &name);
    }

    /// Seed the picker CRDT strings from the model so the dropdowns display
    /// the right value.
    fn seed_pickers(&self, state: &mut AutoCommit, path: &ObjId) {
        ApiDoc::set_text(state, path, METHOD_PATH, self.request.method.as_str());
        let env_name = self.active_environment().map_or_else(
            || NO_ENV.to_string(),
            |environment| environment.name.clone(),
        );
        ApiDoc::set_text(state, path, ENV_PATH, &env_name);
    }

    fn synchronize(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if self.joined && !self.adopted && ApiDoc::is_empty(state, path) {
            return;
        }
        if !self.adopted {
            if ApiDoc::is_empty(state, path) {
                if !self.seeded {
                    self.seed_workspace(state, path);
                    ApiDoc::set_text(state, path, "url", &self.request.url);
                    ApiDoc::set_text(state, path, "body", &self.request.body_text);
                    self.seed_pickers(state, path);
                    self.seeded = true;
                    self.adopted = true;
                    self.persisted_once = true;
                }
            } else {
                self.adopt(state, path);
                // Ensure the pickers have a value even for a doc seeded before
                // this version existed.
                if ApiDoc::read_text(state, path, METHOD_PATH).is_empty() {
                    self.seed_pickers(state, path);
                }
            }
        }

        // Apply any commands an assistive technology queued by activating a
        // control directly through `perform_action` (which bypasses the
        // container's `handle_event`, where pointer/keyboard activations are
        // drained). Draining here too means Orca / the built-in reader can
        // Send, switch tabs, and open rows just like the keyboard.
        self.drain_commands(state, path);
        self.reconcile_pickers(state, path);

        // Keep the widgets in step with app state the app itself drives.
        self.poll_worker();
        if !self.pending_history.is_empty() {
            for entry in std::mem::take(&mut self.pending_history) {
                self.perform(state, path, &ApiTesterAction::RecordHistory { entry });
            }
            self.history = ApiDoc::history(state, path);
            self.workspace_changed();
        }
        self.refresh_views();
    }

    // -----------------------------------------------------------------------
    // Resource operations
    // -----------------------------------------------------------------------

    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<bool, String> {
        if type_id != crate::actions::API_TESTER_OPERATION_TYPE {
            return Ok(false);
        }
        let operation: crate::actions::ApiTesterOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        match operation {
            crate::actions::ApiTesterOperation::SendActiveRequest => {
                if kind != tilo_actions::OperationKind::Effect {
                    return Ok(false);
                }
                self.send(doc, path);
            }
            crate::actions::ApiTesterOperation::SendRequest {
                collection_id,
                request_id,
            } => {
                if kind != tilo_actions::OperationKind::Effect {
                    return Ok(false);
                }
                let collection_id = uuid::Uuid::parse_str(&collection_id)
                    .map_err(|_| "invalid API Tester collection id".to_string())?;
                let request_id = uuid::Uuid::parse_str(&request_id)
                    .map_err(|_| "invalid API Tester request id".to_string())?;
                let Some((collection_index, request_index)) = self
                    .collections
                    .iter()
                    .enumerate()
                    .find_map(|(collection_index, collection)| {
                        (collection.id == collection_id).then(|| {
                            collection
                                .requests
                                .iter()
                                .position(|request| request.id == request_id)
                                .map(|request_index| (collection_index, request_index))
                        })
                    })
                    .flatten()
                else {
                    return Err("saved request no longer exists".to_string());
                };
                self.load_request(doc, path, collection_index, request_index);
                self.send(doc, path);
            }
            crate::actions::ApiTesterOperation::ReplayHistory { history_id } => {
                if kind != tilo_actions::OperationKind::View {
                    return Ok(false);
                }
                let history_id = uuid::Uuid::parse_str(&history_id)
                    .map_err(|_| "invalid API Tester history id".to_string())?;
                let Some(index) = self.history.iter().position(|entry| entry.id == history_id)
                else {
                    return Err("history entry no longer exists".to_string());
                };
                self.replay_history(doc, path, index);
                self.dirty = true;
            }
            crate::actions::ApiTesterOperation::RequestMenu { action } => {
                if kind != tilo_actions::OperationKind::View {
                    return Ok(false);
                }
                self.open_request_menu_action(&action)?;
            }
            crate::actions::ApiTesterOperation::EnvironmentMenu { action } => {
                if kind != tilo_actions::OperationKind::View {
                    return Ok(false);
                }
                self.open_environment_menu_action(&action)?;
            }
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect
            || type_id != crate::actions::API_TESTER_OPERATION_TYPE
        {
            return Ok(None);
        }
        if self.sending || self.resource_send_invocation.is_some() {
            return Err("another API request is already running".into());
        }
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        self.resource_send_invocation = Some(invocation);
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

/// Apply a field edit to one pair.
fn apply_edit(pair: &mut KeyValue, edit: RowEdit) {
    match edit {
        RowEdit::Key(text) => pair.key = text,
        RowEdit::Value(text) => pair.value = text,
        RowEdit::Enabled(enabled) => pair.enabled = enabled,
        RowEdit::Remove => {}
    }
}

impl Default for ApiTesterApp {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Widget impl
// ===========================================================================

impl ui::widget::Decorator for ApiTesterApp {
    type Inner = NavShell;

    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource;
        inner = nav
    );

    fn inner(&self) -> &NavShell {
        &self.nav
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.nav
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::API_TESTER_OPERATION_TYPE]
    }

    fn resource_operation_handlers(&self) -> HashSet<String> {
        let mut handlers = self.nav.resource_operation_handlers();
        handlers.insert(crate::actions::API_TESTER_OPERATION_TYPE.to_owned());
        handlers
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        self.synchronize(state, path);
        self.nav.sync(state, path);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.nav.handle_event(event, ctx);
        // Apply what the controls queued, then reconcile the pickers.
        let did = self.drain_commands(ctx.state, ctx.path);
        self.reconcile_pickers(ctx.state, ctx.path);
        if did || *ctx.handled {
            self.dirty = true;
        }
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return self.nav.start_operation(op, doc, path);
        };
        if let Some(started) = self.nav.start_operation(op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if let Some(update) =
            self.nav
                .poll_resource_operation(invocation, cancellation_requested, doc, path)
        {
            return Some(update);
        }
        self.poll_worker();
        if self
            .resource_send_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_send_result.take()
        } else {
            None
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        match self.pose.take() {
            Some(Pose::SidebarOpen) => {
                self.nav.open_sidebar();
                self.nav.snap();
            }
            Some(Pose::SidebarClosed) => {
                self.nav.close_sidebar();
                self.nav.snap();
            }
            None => {}
        }
        self.nav.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.sending || self.pose.is_some() || self.nav.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.nav.mark_drawn();
    }

    fn needs_idle_sync(&self) -> bool {
        self.sending
            || !self.pending_history.is_empty()
            || !self.adopted
            || !self.sink.borrow().is_empty()
    }

    fn name(&self) -> &'static str {
        "ApiTesterApp"
    }
}

impl ui::search::capability::SearchSource for ApiTesterApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for ApiTesterApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns the
    /// workspace door — keep only the search-catalog diagnostic when present.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.actions.is_hosted() {
            return crate::actions::live_search_status_resources(self.search_error_message());
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.actions.is_hosted() && id == "api_tester.workspace" {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::undo::CommittedActions for ApiTesterApp {
    /// Drain the undo boundary the [`ActionHost`](ui::ActionHost) bracketed for
    /// this event so the shell records exactly one undo point per dispatched
    /// action — the same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

#[cfg(test)]
mod tests;
