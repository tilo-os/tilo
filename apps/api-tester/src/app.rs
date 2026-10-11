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
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use ui::LocalState;
use ui::tokens::Spacing;
use ui::widget::resizable_split::SplitOrientation;
use ui::widget::{
    AppModel, Column, Dropdown, Element, ModelPath, NavShell, ResizableSplit, Spacer, Stack, Widget,
};
use winit::event::WindowEvent;

use crate::actions::{self, ApiTesterAction, ApiTesterEffect, RowSection};
use crate::commands::{AuthField, Cmd, FormMode, FormTarget, RowEdit, Sink, Table};
use crate::dialog::{self, Fields};
use crate::http::{self, Outgoing};
use crate::kv::{KvChange, KvQueue};
use crate::model::{
    Auth, BodyMode, Collection, Environment, HistoryEntry, HttpMethod, KeyValue, Request,
    RequestTab, ResponseData, ResponseView,
};
use crate::request::{self, BlankRows};
use crate::resource::{ApiTester, Workspace};
use ui::framework::{Actions, Dispatcher, EffectAnswer, EffectCx, Effected, SurfaceCx};
use ui::secrets::SecretStore;
const CREDENTIAL: &str = "request-credential";
const SECRET_VARIABLE: &str = "variable-value";
use crate::response;
use crate::sidebar::{self, SidebarView};
use crate::swap::{Swap, SwapHandle};
use crate::vars;

/// What the picker reads when no environment is chosen.
const NO_ENV: &str = "No environment";

/// What the environment picker offers besides the environments themselves,
/// and the sheet each opens. The first needs no active environment.
const ENVIRONMENT_COMMANDS: [(&str, FormMode); 5] = [
    ("New environment\u{2026}", FormMode::NewEnvironment),
    ("Rename environment\u{2026}", FormMode::RenameEnvironment),
    ("Add variable\u{2026}", FormMode::NewVariable),
    ("Delete variable\u{2026}", FormMode::DeleteVariable),
    ("Delete environment\u{2026}", FormMode::DeleteEnvironment),
];

/// A pane of the split: `slot` with room between it and the divider, which
/// is painted over the edge of both panes.
fn pane(slot: Swap, below_divider: bool) -> Column {
    let gap = Spacing::BASE.s3;
    let column = Column::new().with_no_padding();
    let column = if below_divider {
        column.with_extra_top_padding(gap)
    } else {
        column
            .with_extra_bottom_padding(gap)
            .with_intrinsic_height()
    };
    let body = Column::new().with_no_padding();
    let body = if below_divider {
        body.with_fill_remaining_height()
    } else {
        body.with_intrinsic_height()
    };
    column.push(Box::new(body.push(Box::new(slot))))
}

/// Preserve the shared divider across request-view replacements, with an
/// explicit local reset requested by the published header control.
struct RequestSplit {
    split: ResizableSplit<Column, Column>,
    reset: LocalState<bool>,
}
impl ui::widget::Decorator for RequestSplit {
    type Inner = ResizableSplit<Column, Column>;
    fn inner(&self) -> &Self::Inner {
        &self.split
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.split
    }
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.reset.get() {
            self.reset.set(false);
            self.split.reset_to_content_size();
        }
        self.split.sync(state, path);
    }
}

/// What stands where a sheet goes while none is open.
fn no_dialog() -> Element {
    Box::new(Spacer::token(ui::tokens::SpacingStep::S0))
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
    sidebar: sidebar::NavHandle,
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
    /// What the environment picker shows.
    env_state: LocalState<String>,

    /// Commands pushed by widget callbacks, applied after event routing.
    sink: Sink,

    /// Framework-owned action dispatch: every collaborative doc mutation (create
    /// / rename / delete a collection or saved request, add / edit / delete an
    /// environment or variable, select the active environment) is encoded and
    /// applied through this host, which brackets the undo boundary and remembers
    /// the last action — the same path a Ctrl+K palette dispatch takes.
    dispatcher: Option<Dispatcher<ApiTester>>,
    secrets: Option<SecretStore>,
    credentials: HashMap<uuid::Uuid, String>,
    read_only: bool,
    adopted: bool,
    /// Mounted on a record another participant created: its content is
    /// theirs to seed, so an empty document is left untouched.
    dirty: bool,
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
        Self::build_with_actions(demo, None)
    }
    pub(crate) fn new_with_actions(actions: &Actions<ApiTester>) -> Self {
        Self::build_with_actions(false, Some(actions.clone()))
    }
    fn build_with_actions(demo: bool, actions: Option<Actions<ApiTester>>) -> Self {
        crate::locales::install();
        let model = Self::build_model(demo);
        let sink = Sink::default();
        *sink.actions.borrow_mut() = actions;
        sink.method.set(model.request.method.as_str().to_owned());
        let blanks = BlankRows::new();
        let env_vars = Rc::new(RefCell::new(HashMap::new()));
        let response = demo.then(http::sample_response);
        let selected = demo.then_some((0, 0));

        let builtin = request::section(&model.request, RequestTab::Params, &blanks, &sink);
        let (request_slot, request) = Swap::new(builtin.element);
        let (response_slot, response_handle, response_body) =
            Self::response_slot(response.as_ref(), &sink);
        let (dialog_slot, dialog) = Swap::new(no_dialog());
        let env_state = LocalState::new(
            Self::environment_name(&model.environments, model.active_env).to_owned(),
        );
        let (env_slot, environments) = Swap::new(Self::environment_picker(
            &model.environments,
            model.active_env,
            &env_state,
            &sink,
        ));
        let (lists_slot, sidebar_lists) = sidebar::lists(
            &SidebarView {
                collections: &model.collections,
                history: &model.history,
                selected,
            },
            &sink,
        );

        let content = Self::content(request_slot, response_slot, dialog_slot, &env_vars, &sink);
        let reset = sink.reset_area.clone();
        let reset_area = ui::widget::Button::icon_only(
            "api.reset-request-area",
            ui::icons::Icon::RotateCcw,
            "Reset request area",
        )
        .on_view(move || reset.set(true));
        let nav = NavShell::new()
            .with_id("api.shell")
            .sidebar(sidebar::frame(Box::new(env_slot), Box::new(lists_slot)))
            .sidebar_header("", vec![sidebar::new_collection(&sink)])
            .content(content)
            .content_header(
                model.request.name.clone(),
                vec![
                    Box::new(ui::search::search_button(
                        ui::search::SearchRequest::this_app(),
                    )),
                    Box::new(reset_area),
                ],
            );

        let app = Self {
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
            env_state,
            sink,
            dispatcher: None,
            secrets: None,
            credentials: HashMap::new(),
            read_only: false,
            adopted: false,
            dirty: true,
        };
        app.refresh_env_vars();
        app
    }

    /// The content column: the URL bar over the request and the response,
    /// which a divider splits, with the sheet's slot above them.
    fn content(
        request: Swap,
        response: Swap,
        dialog: Swap,
        env_vars: &Rc<RefCell<HashMap<String, String>>>,
        sink: &Sink,
    ) -> Element {
        // The split relays out its panes as its divider moves.
        let split = ResizableSplit::new(
            pane(request, false),
            pane(response, true),
            SplitOrientation::Vertical,
        )
        .with_content_sized_first()
        .with_divider_id("api.request-divider")
        .with_divider_label("Resize the request and the response");
        Box::new(
            Stack::new()
                .push(Box::new(
                    // The URL and its variable-resolution caption belong
                    // together; keep the caption's tight text rhythm so the
                    // content-sized request leaves room for the response.
                    Column::new()
                        .with_rhythm(ui::tokens::Rhythm::Tight)
                        .push(request::url_bar(sink))
                        .push(request::variable_hint(Rc::clone(env_vars)))
                        .push(Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_fill_remaining_height()
                                .push(Box::new(RequestSplit {
                                    split,
                                    reset: sink.reset_area.clone(),
                                })),
                        )),
                ))
                .push(Box::new(dialog)),
        )
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
            let (slot, handle) = Swap::new(response::empty(sink));
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
                self.response = None;
                self.sending = false;
                self.error = None;
                self.stale.response = true;
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

    /// The name the picker shows for `active`.
    fn environment_name(environments: &[Environment], active: Option<uuid::Uuid>) -> &str {
        environments
            .iter()
            .find(|environment| Some(environment.id) == active)
            .map_or(NO_ENV, |environment| environment.name.as_str())
    }

    /// The environment picker: the environments, no environment, and the
    /// commands that manage them.
    fn environment_picker(
        environments: &[Environment],
        active: Option<uuid::Uuid>,
        state: &LocalState<String>,
        sink: &Sink,
    ) -> Element {
        let mut options: Vec<String> = environments.iter().map(|e| e.name.clone()).collect();
        options.push(NO_ENV.to_owned());
        let commands = if active.is_some() {
            &ENVIRONMENT_COMMANDS[..]
        } else {
            &ENVIRONMENT_COMMANDS[..1]
        };
        options.extend(commands.iter().map(|(label, _)| (*label).to_owned()));
        let picker = Dropdown::from_state(options, state)
            .with_id("api.env")
            .caption("Environment");
        Box::new(match sink.actions.borrow().as_ref() {
            Some(actions) => actions.bind_effect(picker, ApiTesterEffect::PICK_ENVIRONMENT),
            None => picker.with_enabled(false),
        })
    }

    fn refresh_env_vars(&self) {
        *self.env_vars.borrow_mut() = self.env_vars();
    }

    /// Replace the parts of the view whose model changed.
    fn refresh_views(&mut self) {
        if std::mem::take(&mut self.stale.sidebar) {
            self.slots.sidebar.set(&SidebarView {
                collections: &self.collections,
                history: &self.history,
                selected: self.selected,
            });
        }
        if std::mem::take(&mut self.stale.header) {
            self.nav.set_content_title(self.request.name.clone());
        }
        if std::mem::take(&mut self.stale.environments) {
            self.slots.environments.set(Self::environment_picker(
                &self.environments,
                self.active_env,
                &self.env_state,
                &self.sink,
            ));
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
                self.slots.response.set(response::failed(error, &self.sink));
                self.slots.response_body = None;
            }
            (None, None) => {
                self.slots.response.set(response::empty(&self.sink));
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
            Some(env) => {
                let variables: Vec<_> = env
                    .variables
                    .iter()
                    .map(|variable| {
                        let mut variable = variable.clone();
                        if variable.secret {
                            variable.value = self
                                .secret(SECRET_VARIABLE, &variable.id)
                                .unwrap_or_default();
                        }
                        variable
                    })
                    .collect();
                vars::var_map(&variables)
            }
            None => HashMap::new(),
        }
    }

    /// Resolve the request being composed against the active environment.
    fn outgoing(&self) -> Outgoing {
        http::build_outgoing(&self.request, &self.env_vars())
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
    fn load_request(&mut self, _state: &AutoCommit, _path: &ObjId, ci: usize, ri: usize) {
        if let Some(request) = self
            .collections
            .get(ci)
            .and_then(|c| c.requests.get(ri))
            .cloned()
        {
            self.submit(&ApiTesterAction::OpenRequest { request });
        }
        self.dismiss_drawer();
    }

    /// Choosing from a floating sidebar is done with it: show the request.
    fn dismiss_drawer(&mut self) {
        if self.nav.geometry().sidebar == ui::widget::SidebarPlacement::Drawer {
            self.nav.close_sidebar();
        }
    }

    /// The collections, environments or history changed: rebuild what shows
    /// them.
    fn workspace_changed(&mut self) {
        self.stale.sidebar = true;
        self.stale.environments = true;
        self.stale.header = true;
        self.show_active_environment();
        self.refresh_env_vars();
    }

    /// Submit a canonical action; the resource host publishes its authoritative result.
    fn perform(&self, _state: &AutoCommit, _path: &ObjId, action: &ApiTesterAction) {
        self.submit(action);
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

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    /// Drain the command sink and apply each command, persisting when a
    /// command changes shared state. Returns `true` if anything was applied.
    fn drain_commands(&mut self, state: &AutoCommit, path: &ObjId) -> bool {
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
                        self.submit(&ApiTesterAction::SetBodyMode { mode });
                        self.stale.body_inner = true;
                    }
                }
                Cmd::SetAuthKind(i) => self.set_auth_kind(i),
                Cmd::LoadRequest(ci, ri) => self.load_request(state, path, ci, ri),
                Cmd::ReplayHistory(hi) => self.replay_history(state, path, hi),
                Cmd::Edit { table, row, edit } => self.edit_rows(table, &row, &edit),
                Cmd::PickEnvironment(choice) => self.pick_environment(state, path, &choice),
                Cmd::OpenForm(mode, target) => self.open_form(mode, target),
                Cmd::CancelForm => self.close_form(),
                Cmd::SubmitForm => self.submit_form(state, path),
            }
        }
        self.dirty = true;
        true
    }

    fn replay_history(&mut self, _state: &AutoCommit, _path: &ObjId, index: usize) {
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
            self.submit(&ApiTesterAction::OpenRequest {
                request: self.request.clone(),
            });
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
            self.submit(&ApiTesterAction::SetAuth {
                auth: self.request.auth.without_secret(),
            });
            self.stale.auth_inner = true;
        }
    }

    fn edit_auth(&mut self, field: AuthField, text: String) {
        let secret = matches!(
            field,
            AuthField::Token | AuthField::Password | AuthField::Value
        );
        match (&mut self.request.auth, field) {
            (Auth::Bearer { token }, AuthField::Token) => token.clone_from(&text),
            (Auth::Basic { username, .. }, AuthField::Username) => username.clone_from(&text),
            (Auth::Basic { password, .. }, AuthField::Password) => password.clone_from(&text),
            (Auth::ApiKey { header, .. }, AuthField::Header) => header.clone_from(&text),
            (Auth::ApiKey { value, .. }, AuthField::Value) => value.clone_from(&text),
            _ => return,
        }
        if secret {
            if let Some(dispatcher) = &self.dispatcher {
                dispatcher.effect(&ApiTesterEffect::SetCredential { secret: text });
            }
        } else {
            self.submit(&ApiTesterAction::SetAuth {
                auth: self.request.auth.without_secret(),
            });
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
    fn edit_rows(&mut self, table: Table, row: &str, edit: &RowEdit) {
        if row == self.blanks.of(table) {
            return;
        }
        self.submit(&ApiTesterAction::RemoveRow {
            section: row_section(table),
            row_id: row.to_owned(),
        });
        self.rows_mut(table).retain(|pair| pair.id != row);
        if let Some((shown, queue)) = &self.table
            && *shown == table
        {
            queue.push(KvChange::Remove(row.to_owned()));
        }
        let RowEdit::Remove = edit;
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
        self.nav.show(ui::widget::NavPane::Content);
        // In drawer mode, choosing a sidebar command must uncover the
        // content-owned form; selecting a push pane alone cannot close it.
        self.dismiss_drawer();
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
    fn submit_form(&mut self, state: &AutoCommit, path: &ObjId) {
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
                    request.url.clone_from(&self.request.url);
                    request.body_text.clone_from(&self.request.body_text);
                    request.method = self.request.method;
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
                    let secret = fields.secret.get();
                    if secret
                        && let Err(error) = self.keep_secret(SECRET_VARIABLE, &stable_id, &value)
                    {
                        self.error =
                            Some(format!("Couldn't keep the value on this device: {error}"));
                        self.stale.response = true;
                        return;
                    }
                    self.submit(&actions::set_variable_action(
                        env_id, stable_id, &key, &value, secret,
                    ));
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

    /// The person chose from the environment picker: an environment, or one
    /// of the commands that manage them. Selecting the active environment is
    /// collaborative workspace state persisted in the shared document, so it
    /// is routed through the action host, the same
    /// [`SetActiveEnvironment`](ApiTesterAction::SetActiveEnvironment) a
    /// Ctrl+K palette dispatch would apply.
    fn pick_environment(&mut self, state: &AutoCommit, path: &ObjId, choice: &str) {
        if let Some((_, mode)) = ENVIRONMENT_COMMANDS
            .iter()
            .find(|(label, _)| *label == choice)
        {
            self.open_form(*mode, FormTarget::default());
        } else {
            let next = if choice == NO_ENV {
                Some(None)
            } else {
                self.environments
                    .iter()
                    .find(|e| e.name == choice)
                    .map(|e| Some(e.id))
            };
            if let Some(next) = next
                && next != self.active_env
            {
                self.perform(state, path, &actions::set_active_environment_action(next));
            }
        }
        // A command is not a value the picker keeps.
        self.show_active_environment();
    }

    /// Make the environment picker read the active environment, whichever way
    /// it changed (a pick, a rename, a deletion, the palette).
    fn show_active_environment(&self) {
        self.env_state
            .set(Self::environment_name(&self.environments, self.active_env).to_owned());
    }

    // Method selection uses the typed request dispatcher.

    // -----------------------------------------------------------------------
    // Resource operations
    // -----------------------------------------------------------------------
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

    fn inner(&self) -> &NavShell {
        &self.nav
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.nav
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.refresh_views();
        self.nav.sync(state, path);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.nav.handle_event(event, ctx);
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
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
        self.nav
            .draw(scene, transform, text_renderer, theme, presence);
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
            || self.sink.reset_area.get()
            || !self.sink.borrow().is_empty()
            || self.nav.needs_idle_sync()
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        self.nav.settle(edits, path);
        self.drain_commands(edits.doc(), path);
        self.refresh_views();
    }
    fn name(&self) -> &'static str {
        "ApiTesterApp"
    }
}

#[cfg(test)]
mod tests;

impl ApiTesterApp {
    fn secret(&self, purpose: &'static str, item: &str) -> Option<String> {
        let secret = self
            .secrets
            .as_ref()?
            .item(purpose, item)
            .get_blocking()
            .ok()??;
        String::from_utf8(secret.expose().to_vec()).ok()
    }
}

impl ApiTesterApp {
    fn keep_secret(&self, purpose: &'static str, item: &str, value: &str) -> Result<(), String> {
        let secrets = self
            .secrets
            .as_ref()
            .ok_or_else(|| "this window keeps no secrets".to_string())?;
        secrets
            .item(purpose, item)
            .put_blocking(value.as_bytes())
            .map_err(|error| error.to_string())
    }
}

impl ApiTesterApp {
    fn credential(&mut self, id: uuid::Uuid) -> String {
        if let Some(known) = self.credentials.get(&id) {
            return known.clone();
        }
        let found = self.secret(CREDENTIAL, &id.to_string()).unwrap_or_default();
        self.credentials.insert(id, found.clone());
        found
    }
}

impl ApiTesterApp {
    fn submit(&self, action: &ApiTesterAction) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.apply(action);
        }
    }
}

impl ApiTesterApp {
    pub(crate) fn answer_effect(
        &mut self,
        effect: ApiTesterEffect,
        cx: &mut EffectCx<ApiTester, Self>,
    ) -> EffectAnswer {
        match effect {
            ApiTesterEffect::EditAuthField { field, value } => {
                let field = match field.as_str() {
                    "username" => AuthField::Username,
                    "header" => AuthField::Header,
                    _ => return Err("no such authentication metadata field".into()),
                };
                self.edit_auth(field, value);
                self.dirty = true;
                Ok(Effected::done())
            }
            ApiTesterEffect::PickEnvironment { choice } => {
                self.sink.borrow_mut().push(Cmd::PickEnvironment(choice));
                self.dirty = true;
                Ok(Effected::done())
            }
            ApiTesterEffect::Send => {
                let method = self.request.method;
                let out = self.outgoing();
                self.start_send(method, out, cx)
            }
            ApiTesterEffect::SendSaved { request } => {
                let Some(saved) = self
                    .collections
                    .iter()
                    .flat_map(|collection| &collection.requests)
                    .find(|saved| saved.id.to_string() == request)
                    .cloned()
                else {
                    return Err("saved request no longer exists".to_string());
                };
                cx.apply(&ApiTesterAction::OpenRequest {
                    request: saved.clone(),
                });
                let mut outgoing = saved;
                let credential = self.credential(outgoing.id);
                outgoing.auth = outgoing.auth.with_secret(&credential);
                let out = http::build_outgoing(&outgoing, &self.env_vars());
                self.start_send(outgoing.method, out, cx)
            }
            ApiTesterEffect::SetCredential { secret } => {
                let id = self.request.id;
                self.keep_secret(CREDENTIAL, &id.to_string(), &secret)?;
                self.credentials.insert(id, secret.clone());
                self.request.auth = self.request.auth.with_secret(&secret);
                self.dirty = true;
                Ok(Effected::done())
            }
            ApiTesterEffect::SetSecretValue { variable, value } => {
                let secret = self
                    .environments
                    .iter()
                    .flat_map(|environment| &environment.variables)
                    .find(|row| row.id == variable)
                    .map(|row| row.secret);
                match secret {
                    Some(true) => {
                        self.keep_secret(SECRET_VARIABLE, &variable, &value)?;
                        self.dirty = true;
                        Ok(Effected::done())
                    }
                    Some(false) => {
                        Err("this variable's value is shared; set it as a document edit".into())
                    }
                    None => Err("no such variable".into()),
                }
            }
            ApiTesterEffect::RequestMenu { action } => {
                self.open_request_menu_action(&action)?;
                Ok(Effected::done())
            }
            ApiTesterEffect::EnvironmentMenu { action } => {
                self.open_environment_menu_action(&action)?;
                Ok(Effected::done())
            }
        }
    }
}

impl ApiTesterApp {
    fn start_send(
        &mut self,
        method: HttpMethod,
        out: Outgoing,
        cx: &mut EffectCx<ApiTester, Self>,
    ) -> EffectAnswer {
        if self.sending {
            return Err("another API request is already running".into());
        }
        self.sending = true;
        self.error = None;
        self.dirty = true;
        let url = out.url.clone();
        Ok(cx.spawn(
            move |_| async move { http::send(&out).await },
            move |app: &mut Self, sent, cx| {
                app.sending = false;
                app.dirty = true;
                match sent {
                    Ok(response) => {
                        cx.apply(&ApiTesterAction::RecordHistory {
                            entry: HistoryEntry {
                                id: uuid::Uuid::new_v4(),
                                method,
                                url,
                                status: response.status,
                                duration_ms: response.duration_ms,
                                when: "just now".into(),
                            },
                        });
                        app.response = Some(response);
                        app.response_view = ResponseView::Pretty;
                        app.stale.response = true;
                        app.stale.response_body = true;

                        Ok(Effected::done())
                    }
                    Err(error) => {
                        app.error = Some(error.clone());
                        app.stale.response = true;
                        Err(error)
                    }
                }
            },
        ))
    }
}

impl ApiTesterApp {
    pub(crate) fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
        self.dirty = true;
    }
}

impl ApiTesterApp {
    pub(crate) fn present_workspace(&mut self, state: &Workspace) {
        let workspace_changed = self.collections != state.collections
            || self.environments != state.environments
            || self.active_env != state.active_env
            || self.history != state.history;
        let mut request = state.request.clone();
        request.auth = request.auth.with_secret(&self.credential(request.id));
        let replaced = self.request.id != request.id;
        let body_changed = self.request.body_mode != request.body_mode;
        let auth_changed =
            std::mem::discriminant(&self.request.auth) != std::mem::discriminant(&request.auth);
        let rows_changed = self.request.params != request.params
            || self.request.headers != request.headers
            || self.request.form != request.form;
        self.collections.clone_from(&state.collections);
        self.environments.clone_from(&state.environments);
        self.active_env = state.active_env;
        self.history.clone_from(&state.history);
        self.sink.method.set(request.method.as_str().to_owned());
        self.request = request;
        // Canonical row-field actions turn the trailing blank into a durable
        // row. Its next presentation must expose a new blank identity.
        for table in [Table::Params, Table::Headers, Table::Form] {
            let blank = self.blanks.of(table);
            let consumed = match table {
                Table::Params => &self.request.params,
                Table::Headers => &self.request.headers,
                Table::Form => &self.request.form,
            }
            .iter()
            .any(|row| row.id == blank);
            if consumed {
                self.blanks.renew(table);
            }
        }
        self.selected = self.collections.iter().enumerate().find_map(|(ci, c)| {
            c.requests
                .iter()
                .position(|r| r.id == self.request.id)
                .map(|ri| (ci, ri))
        });
        if workspace_changed {
            self.workspace_changed();
        }
        self.stale.request |= replaced;
        self.stale.tab_panel |= rows_changed;
        self.stale.body_inner |= body_changed;
        self.stale.auth_inner |= auth_changed;
        self.stale.header = true;
        self.adopted = true;
    }
    pub(crate) fn attach_surface(&mut self, cx: &SurfaceCx<ApiTester>) {
        let dispatcher = cx.dispatcher();
        self.read_only = dispatcher.read_only();
        *self.sink.dispatcher.borrow_mut() = Some(dispatcher.clone());
        *self.sink.actions.borrow_mut() = Some(cx.actions());
        self.dispatcher = Some(dispatcher);
        self.secrets = Some(cx.secrets());
        self.credentials.clear();
    }
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.response = (scenario != Some("empty")).then(http::sample_response);
        if let Some(name) = scenario {
            let mapped = if name == "rail-open" { "drawer" } else { name };
            self.stage(mapped);
            if self.form.is_some() {
                self.nav.snap();
            }
        }
        self.stale.response = true;
    }
}

fn row_section(table: Table) -> RowSection {
    match table {
        Table::Params => RowSection::Params,
        Table::Headers => RowSection::Headers,
        Table::Form => RowSection::Form,
    }
}
