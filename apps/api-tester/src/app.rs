//! Root widget for the API tester.
//!
//! The window is one [`NavShell`]: the sidebar lists saved requests by
//! collection and the history of what was sent; the content is the request
//! builder over the response, split by a divider the person can drag.
//!
//! The app composes the toolkit's widgets and paints nothing of its own. Its
//! controls tell the app what happened by pushing a [`Cmd`] onto a queue; the
//! app applies the queue where it has the shared document, keeps the model,
//! and replaces the parts of the view that depend on it ([`Slot`]s), so
//! the controls around them keep their focus.
//!
//! The request in the builder is part of the shared record: its controls
//! bind to their fields' paths, and saving copies it into a collection.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::LocalState;
use ui::tokens::Spacing;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::resizable_split::SplitOrientation;
use ui::widget::{
    AppModel, Column, Dropdown, Element, ModelPath, NavShell, ResizableSplit, Spacer, Stack, Widget,
};
use winit::event::WindowEvent;

use crate::actions::ApiTesterEffect;
use crate::commands::{Cmd, FormMode, FormTarget, RowEdit, Sink, Table};
use crate::dialog::{self, Fields};
use crate::http;
use crate::kv::{KvChange, KvQueue};
use crate::model::{
    Auth, BodyMode, Collection, Environment, HistoryEntry, Id, KeyValue, Request, RequestTab,
    ResponseData, ResponseView,
};
use crate::palette::short_path;
use crate::record::{Credentials, Variable};
use crate::request::{self, BlankRows};
use crate::resource::{Workspace, workspace as verbs};
use crate::response;
use crate::sidebar::{self, SidebarView, parse_history, parse_request};
use ui::framework::{Actions, Dispatcher, Live, Submit, ViewCx};
use ui::widget::{Slot, SlotHandle};

mod store;
pub(crate) use store::window;
use store::{SECRET_VARIABLE, Shared, Store};

/// What the picker reads when no environment is chosen.
const NO_ENV: &str = "No environment";

/// What the environment picker offers besides the environments themselves,
/// and the sheet each opens; deleting the environment opens none, the host
/// asks. The first needs no active environment.
const ENVIRONMENT_COMMANDS: [(&str, Option<FormMode>); 5] = [
    ("New environment\u{2026}", Some(FormMode::NewEnvironment)),
    (
        "Rename environment\u{2026}",
        Some(FormMode::RenameEnvironment),
    ),
    ("Add variable\u{2026}", Some(FormMode::NewVariable)),
    ("Delete variable\u{2026}", Some(FormMode::DeleteVariable)),
    ("Delete environment\u{2026}", None),
];

/// A pane of the split: `slot` with room between it and the divider, which
/// is painted over the edge of both panes.
fn pane(slot: Slot, below_divider: bool) -> Column {
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
/// explicit local reset requested by the published header control. It
/// reports whether the divider moved, so the reset is offered only then.
struct RequestSplit {
    split: ResizableSplit<Column, Column>,
    reset: LocalState<bool>,
    resized: LocalState<bool>,
}
impl RequestSplit {
    fn report(&self) {
        let resized = !self.split.is_at_content_size();
        if self.resized.get() != resized {
            self.resized.set(resized);
        }
    }
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
        self.report();
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        ui::widget::Widget::handle_event(&mut self.split, event, ctx);
        self.report();
    }
    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let handled =
            ui::widget::Widget::perform_action(&mut self.split, action, key_path, state, path);
        self.report();
        handled
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
    url_bar: bool,
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
    url_bar: SlotHandle,
    /// Save and Send, which the window's bar shows as its actions.
    request_actions: SlotHandle,
    request: SlotHandle,
    tab_panel: Option<SlotHandle>,
    panel_inner: Option<SlotHandle>,
    response: SlotHandle,
    response_body: Option<SlotHandle>,
    environments: SlotHandle,
    dialog: SlotHandle,
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

/// The sidebar row whose actions are open, the menu that offers them and
/// what each row of it does.
struct OpenMenu {
    menu: ContextMenu,
    commands: Vec<Cmd>,
}

/// The model half of a freshly built [`ApiTesterApp`].
struct BuiltModel {
    collections: Vec<Collection>,
    environments: Vec<Environment>,
    active_env: Option<Id>,
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
    active_env: Option<Id>,
    request: Request,
    history: Vec<HistoryEntry>,
    /// The record as last presented.
    workspace: Rc<Workspace>,
    /// What the window shares with the view's effect handler: the response,
    /// the request in flight and the secrets on this device.
    store: Shared,
    /// The workspace as the document holds it, shown as it changes; `None`
    /// for a window outside the framework.
    live: Option<Live<Workspace>>,
    /// The `(revision, loads)` of the workspace last shown.
    presented: Option<(u64, u64)>,

    // --- View state (local, per-peer) ---
    active_tab: RequestTab,
    response_view: ResponseView,
    selected: Option<(usize, usize)>,
    form: Option<OpenForm>,
    /// The open row menu, if one is: it belongs to the window, not to a row.
    menu: Option<OpenMenu>,
    blanks: BlankRows,
    table: Option<(Table, KvQueue)>,
    stale: Stale,
    /// A shell state to take at the next paint, for `--shots`.
    pose: Option<Pose>,
    /// The sidebar row menu to open at the next paint, once the window is
    /// laid out, for `--shots`.
    menu_pending: bool,
    slots: Slots,
    /// The active environment's variables, for the URL's variable hint.
    env_vars: Rc<RefCell<HashMap<String, String>>>,
    /// What the environment picker shows.
    env_state: LocalState<String>,

    /// Commands pushed by widget callbacks, applied after event routing.
    sink: Sink,

    /// Where every change of the shared record goes: field edits and verbs,
    /// submitted as one undo step each through the host, the same path the
    /// palette and agents take.
    dispatcher: Option<Dispatcher<Workspace>>,
    /// Whether this participant may only view, as the view follows it.
    read_only: Option<LocalState<bool>>,
    /// The permission the window was last drawn under.
    shown_read_only: bool,
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
    pub(crate) fn new_with_actions(actions: &Actions<Workspace>) -> Self {
        Self::build_with_actions(false, Some(actions.clone()))
    }
    fn build_with_actions(demo: bool, actions: Option<Actions<Workspace>>) -> Self {
        let model = Self::build_model(demo);
        let sink = Sink::default();
        *sink.actions.borrow_mut() = actions;
        sink.method.set(model.request.method.as_str().to_owned());
        let blanks = BlankRows::new();
        let env_vars = Rc::new(RefCell::new(HashMap::new()));
        let response = demo.then(http::sample_response);
        let selected = demo.then_some((0, 0));

        let builtin = request::section(&model.request, RequestTab::Params, &blanks, &sink);
        let (request_slot, request) = Slot::new(builtin.element);
        let (response_slot, response_handle, response_body) =
            Self::response_slot(response.as_ref(), &sink);
        let (dialog_slot, dialog) = Slot::new(no_dialog());
        let env_state = LocalState::new(
            Self::environment_name(&model.environments, model.active_env.as_ref()).to_owned(),
        );
        let (env_slot, environments) = Slot::new(Self::environment_picker(
            &model.environments,
            model.active_env.as_ref(),
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

        let (url_slot, url_bar) = Slot::new(request::url_bar(&sink));
        let (actions_slot, request_actions) = Slot::new(request::request_actions(&sink, false));
        let content = Self::content(
            url_slot,
            request_slot,
            response_slot,
            dialog_slot,
            &env_vars,
            &sink,
        );
        let nav = NavShell::new()
            .content_kind(ui::widget::ContentKind::Document)
            .with_id("api.shell")
            .sidebar(sidebar::frame(Box::new(env_slot), Box::new(lists_slot)))
            .sidebar_header("", vec![sidebar::new_collection(&sink)])
            .content(content)
            // Save and Send stand in the window bar's row beside New
            // collection; resetting the request area is occasional and
            // follows them, in the bar's menu.
            .content_header(
                model.request.name.clone(),
                vec![Box::new(actions_slot), Box::new(Self::reset_area(&sink))],
            );

        let app = Self {
            nav,
            collections: model.collections,
            environments: model.environments,
            active_env: model.active_env,
            request: model.request,
            history: model.history,
            workspace: Rc::default(),
            store: Rc::new(RefCell::new(Store::new(sink.clone(), response))),
            live: None,
            presented: None,
            active_tab: RequestTab::Params,
            response_view: ResponseView::Pretty,
            selected,
            form: None,
            menu: None,
            blanks,
            table: builtin.table,
            stale: Stale::default(),
            pose: None,
            menu_pending: false,
            slots: Slots {
                sidebar: sidebar_lists,
                url_bar,
                request_actions,
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
            read_only: None,
            shown_read_only: false,
            dirty: true,
        };
        app.refresh_env_vars();
        app
    }

    /// The header control that gives the request and the response their
    /// first sizes again, offered only while the person moved the divider.
    /// Its glyph is the request standing on the divider, not a rotating
    /// arrow, which reads as reloading the request.
    fn reset_area(sink: &Sink) -> ui::widget::Button {
        let reset = sink.reset_area.clone();
        ui::widget::Button::icon_only(
            "api.reset-request-area",
            ui::icons::Icon::AlignEndHorizontal,
            "Reset request area",
        )
        .on_view(move || reset.set(true))
        .enabled_from(&sink.area_resized)
    }

    /// The content column: the URL bar over the request and the response,
    /// which a divider splits, with the sheet's slot above them.
    fn content(
        url_bar: Slot,
        request: Slot,
        response: Slot,
        dialog: Slot,
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
                        .push(Box::new(url_bar))
                        .push(request::variable_hint(
                            Rc::clone(env_vars),
                            Rc::clone(&sink.url),
                        ))
                        .push(Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_fill_remaining_height()
                                .push(Box::new(RequestSplit {
                                    split,
                                    reset: sink.reset_area.clone(),
                                    resized: sink.area_resized.clone(),
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
    ) -> (Slot, SlotHandle, Option<SlotHandle>) {
        if let Some(data) = response {
            let built = response::response(data, ResponseView::Pretty, sink, None);
            let (slot, handle) = Slot::new(built.element);
            (slot, handle, Some(built.body))
        } else {
            let (slot, handle) = Slot::new(response::empty(sink));
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
        let active_env = environments.first().map(|e| e.id.clone());
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
                let mut state = self.state();
                state.response = None;
                state.sending = false;
                state.error = None;
                drop(state);
                self.stale.response = true;
                true
            }
            "error" => {
                let mut state = self.state();
                state.response = None;
                state.sending = false;
                state.error = Some(
                    "Could not reach api.example.com. Check the URL or your network connection."
                        .into(),
                );
                drop(state);
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
            "history-actions" => {
                // The menu of a sent request: replayed, kept as a request,
                // or forgotten (`APP-api-tester-04`).
                self.menu_pending = true;
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
            "new-environment" => FormMode::NewEnvironment,
            "rename-environment" => FormMode::RenameEnvironment,
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
        "history-actions",
        "sidebar-hidden",
        "form-new-collection",
        "form-save-request",
        "form-rename-collection",
        "form-rename-request",
        "form-new-environment",
        "form-rename-environment",
        "form-new-variable",
        "form-delete-variable",
    ];

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    /// The name the picker shows for `active`.
    fn environment_name<'a>(environments: &'a [Environment], active: Option<&Id>) -> &'a str {
        environments
            .iter()
            .find(|environment| Some(&environment.id) == active)
            .map_or(NO_ENV, |environment| environment.name.as_str())
    }

    /// The environment picker: the environments, no environment, and the
    /// commands that manage them.
    fn environment_picker(
        environments: &[Environment],
        active: Option<&Id>,
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
        if std::mem::take(&mut self.stale.url_bar) {
            self.slots.url_bar.set(request::url_bar(&self.sink));
            self.slots
                .request_actions
                .set(request::request_actions(&self.sink, self.sending()));
        }
        if std::mem::take(&mut self.stale.environments) {
            self.slots.environments.set(Self::environment_picker(
                &self.environments,
                self.active_env.as_ref(),
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
        let copy_failed = {
            let mut state = self.state();
            let failed = state.copy.failed();
            if failed {
                state.notice = None;
                state.error = Some("Could not copy to the clipboard.".to_owned());
            }
            failed
        };
        self.stale.response |= copy_failed;
        if std::mem::take(&mut self.stale.response) {
            self.rebuild_response();
        }
        if std::mem::take(&mut self.stale.response_body)
            && let (Some(slot), Some(data)) =
                (&self.slots.response_body, &self.store.borrow().response)
        {
            slot.set(response::body(data, self.response_view));
        }
    }

    fn rebuild_response(&mut self) {
        let store = self.store.borrow();
        if store.sending {
            self.slots.response.set(response::sending(&self.sink));
            self.slots.response_body = None;
            self.stale.response_body = false;
            return;
        }
        match (&store.response, &store.error) {
            (Some(data), _) => {
                let built = response::response(
                    data,
                    self.response_view,
                    &self.sink,
                    store.notice.as_deref(),
                );
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
        self.store
            .borrow()
            .env_vars(&self.environments, self.active_env.as_ref())
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

    /// Open a saved request from a collection in the builder.
    fn load_request(&mut self, ci: usize, ri: usize) {
        if let Some((collection, request)) = self
            .collections
            .get(ci)
            .and_then(|c| Some((c.id.clone(), c.requests.get(ri)?.id.clone())))
        {
            self.submit(Workspace::OPEN_REQUEST.with(verbs::OpenRequest {
                collection,
                request,
            }));
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

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    /// Drain the command sink and apply each command, persisting when a
    /// command changes shared state. Returns `true` if anything was applied.
    fn drain_commands(&mut self) -> bool {
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
                        self.submit(Workspace::paths().request.body_mode.set(mode));
                        self.stale.body_inner = true;
                    }
                }
                Cmd::SetAuthKind(i) => self.set_auth_kind(i),
                Cmd::LoadRequest(ci, ri) => self.load_request(ci, ri),
                Cmd::ReplayHistory(hi) => self.replay_history(hi),
                Cmd::SaveHistory(hi) => self.save_history(hi),
                Cmd::DeleteHistory(hi) => self.delete_history(hi),
                Cmd::RowMenu { id, lift, at } => self.open_row_menu(&id, lift, at),
                Cmd::Edit { table, row, edit } => self.edit_rows(table, &row, &edit),
                Cmd::PickEnvironment(choice) => self.pick_environment(&choice),
                Cmd::OpenForm(mode, target) => self.open_form(mode, target),
                Cmd::DeleteRequest(target) => self.delete_request(target),
                Cmd::CancelForm => self.close_form(),
                Cmd::SubmitForm => self.submit_form(),
            }
        }
        self.dirty = true;
        true
    }

    /// Keep the sent request `target` names as a saved one under `name`.
    fn submit_save_sent(&mut self, target: FormTarget, name: String) {
        if let (Some(collection), Some(sent)) = (target.collection, target.sent) {
            let name = (!name.is_empty()).then_some(name);
            self.submit(Workspace::SAVE_SENT.with(verbs::SaveSent {
                collection,
                sent,
                name,
            }));
            self.stale.sidebar = true;
        }
    }

    fn replay_history(&mut self, index: usize) {
        if let Some(entry) = self.history.get(index).cloned() {
            // The same verb the click runs: the entry's whole request comes
            // back through the record, so a collaborator sees the builder
            // change too. The local copy only saves a frame.
            self.request = if let Some(replay) = entry.replay.as_ref() {
                replay.to_request()
            } else {
                let mut request = crate::seed::blank_request();
                request.method = entry.method;
                request.url = entry.url;
                request
            };
            self.selected = None;
            self.state().error = None;
            self.active_tab = RequestTab::Params;
            self.submit(Workspace::OPEN_SENT.with(entry.id));
            self.stale.request = true;
            self.stale.header = true;
            self.stale.sidebar = true;
            self.dismiss_drawer();
        }
    }

    /// Keep the sent request at `index` as a saved one: the sheet takes its
    /// name and the collection it goes in. The builder keeps what is in it.
    fn save_history(&mut self, index: usize) {
        let Some(entry) = self.history.get(index) else {
            return;
        };
        let route = entry.url.split(['?', '#']).next().unwrap_or_default();
        let suggested = format!("{} {}", entry.method.as_str(), short_path(route));
        let target = FormTarget {
            sent: Some(entry.id.clone()),
            ..self.resolve_target(FormTarget::default())
        };
        self.open_form_with(FormMode::SaveSent, target, &suggested);
    }

    /// Forget the sent request at `index`. Undo takes it back, so the window
    /// asks nothing first.
    fn delete_history(&mut self, index: usize) {
        if let Some(entry) = self.history.get(index) {
            self.submit(Workspace::DELETE_SENT.with(verbs::DeleteSent {
                sent: entry.id.clone(),
            }));
        }
        self.dirty = true;
    }

    /// The actions of the sidebar row `id`, under its `lift`, at `at`: a
    /// saved request is renamed or deleted, a sent one is replayed, kept as a
    /// request, or forgotten (`APP-api-tester-04`).
    fn open_row_menu(&mut self, id: &str, lift: vello::kurbo::Rect, at: vello::kurbo::Point) {
        let mut menu = ContextMenu::new();
        let mut commands: Vec<Cmd> = Vec::new();
        let push = |menu: &mut ContextMenu, commands: &mut Vec<Cmd>, item: MenuItem, cmd: Cmd| {
            menu.push(item);
            commands.push(cmd);
        };
        if let Some((ci, ri)) = parse_request(id) {
            let target = FormTarget {
                collection: self
                    .collections
                    .get(ci)
                    .map(|collection| collection.id.clone()),
                sent: None,
                request: self
                    .collections
                    .get(ci)
                    .and_then(|collection| collection.requests.get(ri))
                    .map(|request| request.id.clone()),
            };
            push(
                &mut menu,
                &mut commands,
                MenuItem::action("Rename request", || {}).with_icon(ui::icons::Icon::Pencil),
                Cmd::OpenForm(FormMode::RenameRequest, target.clone()),
            );
            push(
                &mut menu,
                &mut commands,
                MenuItem::destructive_action("Delete request", || {})
                    .with_icon(ui::icons::Icon::Trash),
                Cmd::DeleteRequest(target),
            );
        } else if let Some(index) = parse_history(id) {
            push(
                &mut menu,
                &mut commands,
                MenuItem::action("Replay request", || {}).with_icon(ui::icons::Icon::RotateCcw),
                Cmd::ReplayHistory(index),
            );
            push(
                &mut menu,
                &mut commands,
                MenuItem::action("Save as request…", || {}).with_icon(ui::icons::Icon::Bookmark),
                Cmd::SaveHistory(index),
            );
            push(
                &mut menu,
                &mut commands,
                MenuItem::destructive_action("Delete from history", || {})
                    .with_icon(ui::icons::Icon::Trash),
                Cmd::DeleteHistory(index),
            );
        }
        if commands.is_empty() {
            return;
        }
        menu.anchor_row(lift);
        let bounds = ui::widget::Widget::get_rect(&self.nav);
        menu.open_anchored(vello::kurbo::Rect::from_points(at, at), bounds);
        self.menu = Some(OpenMenu { menu, commands });
        self.dirty = true;
    }

    /// Route an event to the open menu, and apply what it chose.
    fn route_menu_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if matches!(event, WindowEvent::Focused(false)) {
            self.menu = None;
            self.dirty = true;
            return;
        }
        let Some(open) = self.menu.as_mut() else {
            return;
        };
        open.menu.handle_event(event, ctx);
        let chosen = open
            .menu
            .take_selection()
            .and_then(|index| open.commands.get(index).cloned());
        let closed = !open.menu.is_open();
        if closed {
            self.menu = None;
            self.dirty = true;
        }
        if let Some(command) = chosen {
            self.sink.borrow_mut().push(command);
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
            let before = self.workspace.request.auth.clone();
            self.request.auth = next;
            let after = Credentials::of(&self.request.auth);
            if let Some(dispatcher) = &self.dispatcher {
                let edits = Workspace::paths()
                    .request
                    .auth
                    .path()
                    .edits(&before, &after);
                dispatcher.group(|| {
                    for edit in edits {
                        dispatcher.submit(edit);
                    }
                });
            }
            self.stale.auth_inner = true;
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
        self.submit(table.rows().remove(&Id::from_raw(row)));
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

    fn current_collection_id(&self) -> Option<Id> {
        self.selected
            .and_then(|(index, _)| self.collections.get(index))
            .or_else(|| self.collections.first())
            .map(|collection| collection.id.clone())
    }

    fn collection_named(&self, id: &Id) -> Option<&Collection> {
        self.collections.iter().find(|c| &c.id == id)
    }

    fn selected_request(&self) -> Option<(Id, Id)> {
        let (ci, ri) = self.selected?;
        let collection = self.collections.get(ci)?;
        Some((
            collection.id.clone(),
            collection.requests.get(ri)?.id.clone(),
        ))
    }

    /// The collection and request a sheet acts on: the row it was opened
    /// from, else the one in the builder.
    fn resolve_target(&self, target: FormTarget) -> FormTarget {
        let selected = self.selected_request();
        FormTarget {
            collection: target
                .collection
                .or_else(|| selected.as_ref().map(|(collection, _)| collection.clone()))
                .or_else(|| self.current_collection_id()),
            sent: target.sent,
            request: target
                .request
                .or_else(|| selected.map(|(_, request)| request)),
        }
    }

    fn request_in(&self, target: &FormTarget) -> Option<&Request> {
        self.collection_named(target.collection.as_ref()?)?
            .requests
            .iter()
            .find(|request| Some(&request.id) == target.request.as_ref())
    }

    fn active_environment(&self) -> Option<&Environment> {
        self.environments
            .iter()
            .find(|environment| Some(&environment.id) == self.active_env.as_ref())
    }

    fn open_form(&mut self, mode: FormMode, target: FormTarget) {
        let target = self.resolve_target(target);
        let suggested = match mode {
            FormMode::SaveRequest => self.request.name.clone(),
            _ => String::new(),
        };
        self.open_form_with(mode, target, &suggested);
    }

    /// [`Self::open_form`] with the name its field starts from, for a row
    /// whose subject the window already knows.
    fn open_form_with(&mut self, mode: FormMode, target: FormTarget, suggested: &str) {
        let initial = match mode {
            FormMode::RenameCollection => target
                .collection
                .as_ref()
                .and_then(|id| self.collection_named(id))
                .map_or(String::new(), |c| c.name.clone()),
            FormMode::RenameRequest => self
                .request_in(&target)
                .map_or(String::new(), |r| r.name.clone()),
            FormMode::RenameEnvironment => self
                .active_environment()
                .map_or(String::new(), |e| e.name.clone()),
            FormMode::SaveRequest | FormMode::SaveSent => suggested.to_owned(),
            FormMode::NewCollection
            | FormMode::NewEnvironment
            | FormMode::NewVariable
            | FormMode::DeleteVariable => String::new(),
        };
        self.nav.show(ui::widget::NavPane::Content);
        // In drawer mode, choosing a sidebar command must uncover the
        // content-owned form; selecting a push pane alone cannot close it.
        self.dismiss_drawer();
        let fields = Fields::new(&initial);
        self.slots
            .dialog
            .set(dialog::build(mode, &fields, &self.sink));
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

    /// Delete the saved request `target` names (the one in the builder when
    /// none). Undo takes it back, so nobody is asked first.
    fn delete_request(&mut self, target: FormTarget) {
        let target = self.resolve_target(target);
        if let (Some(collection), Some(request)) = (target.collection, target.request) {
            self.submit(Workspace::DELETE_REQUEST.with(verbs::DeleteRequest {
                collection,
                request,
            }));
        }
        self.dirty = true;
    }

    /// Delete the active environment. Undo takes it back, so nobody is asked
    /// first.
    fn delete_environment(&mut self) {
        if let Some(id) = self.active_env.clone() {
            self.submit(Workspace::DELETE_ENVIRONMENT.with(id));
        }
        self.dirty = true;
    }

    fn submit_form(&mut self) {
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
        let paths = Workspace::paths();
        match mode {
            FormMode::NewCollection => {
                let name = if name.is_empty() {
                    format!("Collection {}", self.collections.len() + 1)
                } else {
                    name
                };
                self.submit(Workspace::NEW_COLLECTION.with(name));
            }
            FormMode::RenameCollection => {
                if let Some(id) = target.collection.filter(|_| !name.is_empty()) {
                    self.submit(paths.collections.item(&id).name.replace(name));
                }
            }
            FormMode::SaveRequest => {
                if let Some(collection) = target.collection {
                    let name = if name.is_empty() {
                        "Saved request".to_owned()
                    } else {
                        name
                    };
                    self.request.name.clone_from(&name);
                    self.submit(Workspace::SAVE_REQUEST.with(verbs::SaveRequest {
                        collection,
                        name: Some(name),
                    }));
                    self.stale.header = true;
                    self.stale.sidebar = true;
                }
            }
            FormMode::SaveSent => self.submit_save_sent(target, name),
            FormMode::RenameRequest => {
                if let (Some(collection), Some(request)) = (target.collection, target.request)
                    && !name.is_empty()
                {
                    if self.request.id == request {
                        self.request.name.clone_from(&name);
                        self.stale.header = true;
                    }
                    self.submit(
                        paths
                            .collections
                            .item(&collection)
                            .requests
                            .item(&request)
                            .name
                            .replace(name),
                    );
                }
            }
            FormMode::NewEnvironment => {
                let name = if name.is_empty() {
                    format!("Environment {}", self.environments.len() + 1)
                } else {
                    name
                };
                self.submit(Workspace::NEW_ENVIRONMENT.with(name));
            }
            FormMode::RenameEnvironment => {
                if let Some(id) = self.active_env.clone().filter(|_| !name.is_empty()) {
                    self.submit(paths.environments.item(&id).name.replace(name));
                }
            }
            FormMode::NewVariable => self.set_variable(&fields),
            FormMode::DeleteVariable => {
                let key = fields.key.get().trim().to_string();
                if let Some(env_id) = self.active_env.clone().filter(|_| !key.is_empty())
                    && let Some(variable) = self
                        .environments
                        .iter()
                        .find(|env| env.id == env_id)
                        .and_then(|env| env.variables.iter().find(|row| row.key == key))
                {
                    self.submit(
                        paths
                            .environments
                            .item(&env_id)
                            .variables
                            .remove(&Id::from_raw(variable.id.clone())),
                    );
                }
            }
        }
        self.dirty = true;
    }

    /// Add the variable the form names to the active environment, or give
    /// the one of that key its new value. A secret's value is kept on this
    /// device.
    fn set_variable(&mut self, fields: &Fields) {
        let key = fields.key.get().trim().to_string();
        let value = fields.value.get().trim().to_string();
        let Some(env_id) = self.active_env.clone().filter(|_| !key.is_empty()) else {
            return;
        };
        let existing = self
            .environments
            .iter()
            .find(|env| env.id == env_id)
            .and_then(|env| env.variables.iter().find(|row| row.key == key))
            .cloned();
        let secret = existing
            .as_ref()
            .map_or_else(|| fields.secret.get(), |variable| variable.secret);
        let Some(variable) = existing else {
            // The window names the new variable, so a secret's value is kept
            // under its id before the variable is added.
            let id = crate::model::fresh_id();
            let kept = secret
                .then(|| {
                    self.store
                        .borrow()
                        .keep_secret(SECRET_VARIABLE, id.as_str(), &value)
                })
                .transpose();
            if let Err(error) = kept {
                self.state().error =
                    Some(format!("Couldn't keep the value on this device: {error}"));
                self.stale.response = true;
                return;
            }
            let variables = Workspace::paths().environments.item(&env_id).variables;
            let at = self
                .environments
                .iter()
                .find(|env| env.id == env_id)
                .map_or(0, |env| env.variables.len());
            self.submit(variables.insert_as(
                &id,
                at,
                &Variable {
                    key,
                    value: if secret { String::new() } else { value },
                    enabled: true,
                    secret,
                },
            ));
            return;
        };
        if secret {
            let kept = self
                .store
                .borrow()
                .keep_secret(SECRET_VARIABLE, &variable.id, &value);
            if let Err(error) = kept {
                self.state().error =
                    Some(format!("Couldn't keep the value on this device: {error}"));
                self.stale.response = true;
            }
            return;
        }
        self.submit(
            Workspace::paths()
                .environments
                .item(&env_id)
                .variables
                .item(&Id::from_raw(variable.id))
                .value
                .replace(value),
        );
    }

    /// The person chose from the environment picker: an environment, or one
    /// of the commands that manage them. Which environment is active is
    /// shared, so a choice is a field edit of the record.
    fn pick_environment(&mut self, choice: &str) {
        if let Some((_, mode)) = ENVIRONMENT_COMMANDS
            .iter()
            .find(|(label, _)| *label == choice)
        {
            match mode {
                Some(mode) => self.open_form(*mode, FormTarget::default()),
                None => self.delete_environment(),
            }
        } else {
            let next = if choice == NO_ENV {
                Some(None)
            } else {
                self.environments
                    .iter()
                    .find(|e| e.name == choice)
                    .map(|e| Some(e.id.clone()))
            };
            if let Some(next) = next
                && next != self.active_env
            {
                self.submit(Workspace::paths().active_environment.set(next));
            }
        }
        // A command is not a value the picker keeps.
        self.show_active_environment();
    }

    /// Make the environment picker read the active environment, whichever way
    /// it changed (a pick, a rename, a deletion, the palette).
    fn show_active_environment(&self) {
        self.env_state
            .set(Self::environment_name(&self.environments, self.active_env.as_ref()).to_owned());
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
        self.follow_document();
        self.follow_store();
        self.refresh_views();
        self.nav.sync(state, path);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // An open row menu takes the event first: it is over the window, and
        // the row it belongs to is still there under it.
        let menu_open = self.menu.is_some();
        let replaces = matches!(
            event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Right,
                ..
            }
        );
        if menu_open && !replaces {
            self.route_menu_event(event, ctx);
            if !matches!(event, WindowEvent::CursorMoved { .. }) {
                *ctx.handled = true;
            }
            return;
        }
        if menu_open {
            self.menu = None;
            self.dirty = true;
        }
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
        if std::mem::take(&mut self.menu_pending) && !self.history.is_empty() {
            // Staged after layout: the menu opens under the newest sent
            // request, where its row is.
            let window = Widget::get_rect(&self.nav);
            let at = vello::kurbo::Point::new(window.x0 + 60.0, (window.y1 - 120.0).max(window.y0));
            self.open_row_menu("history:0", vello::kurbo::Rect::ZERO, at);
        }
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
        if let Some(open) = self.menu.as_mut() {
            open.menu.layout_within(text_renderer, theme);
            Widget::draw_overlay(
                &mut open.menu,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.sending()
            || self.pose.is_some()
            || self.menu_pending
            || self.nav.needs_redraw()
    }

    /// What the window says to assistive technology: the shell's own tree,
    /// and the open row menu over it. The menu's rows are read out where they
    /// are; the arrows and Enter pick one, as they do for a pointer.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut node = self.nav.accessibility()?;
        if let Some(open) = &self.menu {
            let items = open
                .commands
                .iter()
                .enumerate()
                .filter_map(|(index, _)| {
                    let item = open.menu.items().get(index)?;
                    Some(
                        AccessNode::new(Role::MenuItem)
                            .with_name(item.label().unwrap_or_default().to_string())
                            .with_hashed_key(format!("api.row-menu:{index}").as_str())
                            .with_bounds(open.menu.item_rect(index).unwrap_or_default())
                            .with_state(
                                AccessState::new()
                                    .focused(open.menu.hovered() == Some(index))
                                    .disabled(!item.is_enabled()),
                            ),
                    )
                })
                .collect();
            node.children.push(
                AccessNode::new(Role::Menu)
                    .with_name("Row actions")
                    .with_hashed_key("api.row-menu")
                    .with_children(items),
            );
        }
        Some(node)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.nav.mark_drawn();
    }

    fn needs_idle_sync(&self) -> bool {
        self.sending()
            || self.store_changed()
            || self.sink.reset_area.get()
            || !self.sink.borrow().is_empty()
            || self.nav.needs_idle_sync()
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        self.nav.settle(edits, path);
        self.follow_store();
        self.drain_commands();
        self.refresh_views();
    }
    fn name(&self) -> &'static str {
        "ApiTesterApp"
    }
}

#[cfg(test)]
mod tests;

impl ApiTesterApp {
    /// Submit a field edit or a verb of the shared record: the host checks
    /// the participant may, applies it as one undo step and presents the
    /// changed workspace.
    fn submit(&self, submit: impl Submit<Workspace>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(submit);
        }
    }

    /// What the window shares with the view's effect handler.
    pub(crate) fn state(&self) -> std::cell::RefMut<'_, Store> {
        self.store.borrow_mut()
    }

    /// A request is in flight.
    fn sending(&self) -> bool {
        self.store.borrow().sending
    }
}

impl ApiTesterApp {
    /// Take part in the view `cx` builds: edit through its dispatcher, keep
    /// secrets in its storage, and follow the document it holds.
    pub(crate) fn attach(&mut self, cx: &ViewCx<Workspace>) {
        let dispatcher = cx.dispatcher();
        *self.sink.dispatcher.borrow_mut() = Some(dispatcher.clone());
        *self.sink.actions.borrow_mut() = Some(cx.actions());
        self.dispatcher = Some(dispatcher);
        self.state().keep_secrets_in(cx.storage().secrets());
        self.live = Some(cx.live().clone());
        self.read_only = Some(cx.read_only_state());
        // The text controls edit at their paths through the dispatcher now.
        self.stale.url_bar = true;
        self.stale.request = true;
    }

    /// Show the workspace when the document changed since it was last shown,
    /// and draw again when the participant's permission to edit changed.
    fn follow_document(&mut self) {
        if let Some(read_only) = self.read_only.as_ref().map(LocalState::get)
            && read_only != self.shown_read_only
        {
            self.shown_read_only = read_only;
            self.dirty = true;
        }
        let Some(shown) = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented))
        else {
            return;
        };
        self.present_workspace(&shown);
    }

    /// Bring the window in step with what the effects changed and staged.
    fn follow_store(&mut self) {
        let (shown, staged) = {
            let mut state = self.state();
            (std::mem::take(&mut state.shown), state.staged.take())
        };
        if let Some(scenario) = staged {
            self.stage_scenario(scenario.0.as_deref());
        }
        self.stale.response |= shown.response || shown.arrived;
        self.stale.url_bar |= shown.url_bar;
        if shown.arrived {
            self.response_view = ResponseView::Pretty;
            self.stale.response_body = true;
        }
        if shown.response || shown.arrived || shown.url_bar || shown.redraw {
            self.dirty = true;
        }
    }

    /// Whether the effects changed or staged something the window has not
    /// shown yet.
    fn store_changed(&self) -> bool {
        let store = self.store.borrow();
        let shown = &store.shown;
        store.staged.is_some() || shown.response || shown.arrived || shown.url_bar || shown.redraw
    }

    pub(crate) fn present_workspace(&mut self, state: &Rc<Workspace>) {
        self.workspace = Rc::clone(state);
        let collections = state.collections_shown();
        let environments = state.environments_shown();
        let history = state.history_shown();
        let workspace_changed = self.collections != collections
            || self.environments != environments
            || self.active_env != state.active_environment
            || self.history != history;
        let mut request = state.request_shown();
        let credential = self
            .state()
            .credential(&request.id, &state.request.auth.reference);
        match credential {
            Ok(secret) => request.auth = request.auth.with_secret(&secret),
            Err(error) => self.state().error = Some(error),
        }
        let replaced = self.request.id != request.id;
        let body_changed = self.request.body_mode != request.body_mode;
        let auth_changed =
            std::mem::discriminant(&self.request.auth) != std::mem::discriminant(&request.auth);
        let rows_changed = self.request.params != request.params
            || self.request.headers != request.headers
            || self.request.form != request.form;
        self.collections = collections;
        self.environments = environments;
        self.active_env.clone_from(&state.active_environment);
        self.history = history;
        self.sink.method.set(request.method.as_str().to_owned());
        self.sink.url.borrow_mut().clone_from(&request.url);
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
    }

    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.state().response = (scenario != Some("empty")).then(http::sample_response);
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
