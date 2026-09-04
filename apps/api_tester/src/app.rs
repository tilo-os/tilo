//! Root widget for the API tester.
//!
//! # A retained widget tree, not a single painter
//!
//! This app used to be one full-window leaf `Widget` that painted its whole
//! surface and hit-tested a private `Vec<(Rect, Action)>` table. That gave a
//! nice picture but no *structure*: the keyboard could not move between
//! controls, and a screen reader saw a single opaque canvas — the "honest
//! boundary" the hand-rolled-UI audit calls out.
//!
//! It is now a **container** that still paints its own chrome (panels,
//! dividers, section labels, the response body) but composes **real focusable
//! child widgets** for every control:
//!
//! - the HTTP method + environment pickers are [`Dropdown`]s (a11y `ComboBox`),
//! - the URL and body editors are [`TextInput`]s (a11y `TextInput`),
//! - Send is a primary [`Button`] (a11y `Button`),
//! - the request tabs and the response-view switch are [`ToggleGroup`]s
//!   (a11y `Group` of `ToggleButton`s),
//! - each sidebar / history row is a [`NavRow`](crate::navrow::NavRow) (a11y
//!   `Button`).
//!
//! Every one of these is registered in the taffy tree in [`build_layout`], so
//! the host [`ui::App`] walks them into its `focusable_order` (Tab / Shift-Tab
//! / arrow navigation) and the AccessKit bridge emits an AT-SPI node per
//! control. Because the controls are hand-positioned over the painted chrome
//! (via each widget's `apply_layout_external`), the layout — and therefore the
//! look — is preserved while the app becomes fully keyboard- and
//! screen-reader-navigable.
//!
//! ```text
//! ┌──────────┬───────────────────────────────────────────┐
//! │ sidebar  │ method ▾  [ URL bar ............ ] [Send]  │
//! │ collec-  ├───────────────────────────────────────────┤
//! │ tions    │ Params · Headers · Auth · Body            │
//! │ environ- │ <active tab editor>                        │
//! │ ments    ├───────────────────────────────────────────┤
//! │ history  │ 200 OK · 84 ms · 412 B   Pretty Raw Prev   │
//! │          │ <response headers / highlighted body>      │
//! └──────────┴───────────────────────────────────────────┘
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::color_compat::ColorCompat;
use ui::format::bytes as format_bytes;
use ui::icons::Icon;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Button, Checkbox, CheckboxFace, Dropdown, SidebarDrawer, SplitLayout, TextInput, TextInputKind,
    ToggleGroup, Widget,
};

use crate::actions::{self, API_TESTER_ACTION_TYPE, ApiTesterAction};
use crate::doc::ApiDoc;
use crate::http::{self, Outgoing};
use crate::model::{
    Auth, BodyMode, Collection, Environment, HistoryEntry, HttpMethod, KeyValue, Request,
    RequestTab, ResponseData, ResponseView, human_duration,
};
use crate::navrow::{NavKind, NavRow, ROW_H};
use crate::palette::{short_path, status_color, token_color};
use crate::responsive::{RAIL_BREAKPOINT, RAIL_WIDTH};
use crate::vars;

const SIDEBAR_W: f64 = RAIL_WIDTH;
const METHOD_W: f64 = 104.0;
/// The square hamburger toggle that summons the rail in overlay mode.
const HAMBURGER_W: f64 = 36.0;
const SEND_W: f64 = 96.0;
const URLBAR_H: f64 = 56.0;
const TAB_STRIP_H: f64 = 40.0;
const PAD: f64 = 16.0;
const JSON_LINE_H: f64 = 20.0;
const RESPONSE_STATUS_H: f64 = 44.0;
/// A phone gets a second response-chrome row so status metadata and the view
/// switch never compete for the same pixels.
const COMPACT_RESPONSE_STATUS_H: f64 = 76.0;
const COMPACT_RESPONSE_BREAKPOINT: f64 = 520.0;
/// CRDT key the method picker's value lives at (a plain verb string).
const METHOD_PATH: &str = "sel_method";
/// CRDT key the environment picker's value lives at (the env name, or
/// "No Environment").
const ENV_PATH: &str = "sel_env";
/// Label used for the "no environment selected" dropdown option.
const NO_ENV: &str = "No Environment";

/// A status hint may sit beside request tabs only when it has its own space.
/// On a phone the tab strip consumes the full row, so painting the hint at the
/// right edge would overlap the tab labels.
fn tab_strip_has_room_for_hint(layout: &Layout, hint_width: f64) -> bool {
    layout.tabs.x1 + 12.0 + hint_width <= layout.main.x1 - PAD
}

fn response_status_height(width: f64) -> f64 {
    if width < COMPACT_RESPONSE_BREAKPOINT {
        COMPACT_RESPONSE_STATUS_H
    } else {
        RESPONSE_STATUS_H
    }
}

fn response_status_line_center(row: Rect) -> f64 {
    if row.height() > RESPONSE_STATUS_H {
        // Compact response chrome reserves the lower part of the region for
        // the view switcher. Centre status and timing in the first row only.
        row.y0 + RESPONSE_STATUS_H / 2.0
    } else {
        row.center().y
    }
}

/// Outcome of the background send worker.
enum SendOutcome {
    Ok(ResponseData),
    Err(String),
}

/// A user action collected from a child widget's callback, applied after the
/// event has finished routing through the tree. Buttons and rows push these
/// from their `on_click`/`on_activate` closures; the toggle groups push from
/// `on_change`. Draining the sink in one place keeps the borrow of the child
/// widgets and the mutation of the model cleanly separated.
#[derive(Clone, Debug)]
enum Cmd {
    Send,
    SetTab(usize),
    SetView(usize),
    ToggleCollection(usize),
    LoadRequest(usize, usize),
    ReplayHistory(usize),
}

type Sink = Rc<RefCell<Vec<Cmd>>>;

/// Structural signature of the child set. When it changes, the app reports
/// `take_structure_dirty` so the host rebuilds the taffy tree + focus order.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct ChildSig {
    tab: u8,
    body_field: bool,
    resp_view: bool,
    sidebar: bool,
    nav_rows: usize,
}

/// Computed geometry for one frame — every chrome region and every control
/// rect. Produced purely from the widget's rect + the rail state, so
/// `apply_layout` (which positions the child widgets) and `draw` (which paints
/// the chrome behind them) agree without a frame of lag.
#[derive(Default, Clone)]
struct Layout {
    sidebar: Rect,
    scrim: Rect,
    main: Rect,
    // controls
    hamburger: Rect,
    method: Rect,
    url: Rect,
    send: Rect,
    tabs: Rect,
    env: Rect,
    body: Rect,
    resp_view: Rect,
    nav: Vec<Rect>,
    // chrome sub-regions used by draw + hit-tests
    urlbar: Rect,
    tab_strip: Rect,
    editor: Rect,
    response: Rect,
    resp_status: Rect,
    resp_body: Rect,
    collections_label: Point,
    history_label: Point,
    body_region: Rect,
    fold_carets: Vec<(Rect, usize)>,
}

/// Root widget: the whole API tester.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (an in-flight send, one-time seed/adopt \
              bookkeeping, and repaint/persist dirty flags), so grouping them would need a real \
              design pass rather than a mechanical lint fix"
)]
pub struct ApiTesterApp {
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
    collapsed: std::collections::HashSet<usize>,
    body_scroll: f64,
    selected: Option<(usize, usize)>,

    // --- Real child widgets ---
    method_dropdown: Dropdown,
    env_dropdown: Dropdown,
    url_input: TextInput,
    body_input: TextInput,
    send_button: Button,
    request_tabs: ToggleGroup,
    response_view_toggle: ToggleGroup,
    nav_rows: Vec<NavRow>,
    /// Parallel to `nav_rows`: what each row draws + does (rebuilt together).
    nav_meta: Vec<NavMeta>,

    /// Commands pushed by child callbacks, drained after event routing.
    sink: Sink,

    /// Framework-owned action dispatch: every collaborative doc mutation (create
    /// / rename / delete a collection or saved request, add / edit / delete an
    /// environment or variable, select the active environment) is encoded and
    /// applied through this host, which brackets the undo boundary and remembers
    /// the last action — the same path a Ctrl+K palette dispatch takes.
    actions: ui::ActionHost,

    // --- Worker plumbing ---
    tx: Sender<SendOutcome>,
    rx: Receiver<SendOutcome>,
    resource_send_invocation: Option<qos_actions::InvocationId>,
    resource_send_result: Option<qos_actions::InvocationUpdate>,

    // --- Adaptive chrome: the shared collapsible sidebar/drawer controller
    // (docked-vs-overlay decision, eased reveal, and dimming scrim). ---
    rail: SidebarDrawer,

    // --- Layout / interaction caches ---
    rect: Rect,
    layout: Layout,
    last_cursor: Point,
    seeded: bool,
    adopted: bool,
    dirty: bool,
    /// Set when the child set changed; drained by `take_structure_dirty`.
    last_sig: ChildSig,
    persisted_once: bool,
}

/// The data + command for one sidebar row, rebuilt from the model whenever the
/// collections / history / selection change.
#[derive(Clone)]
struct NavMeta {
    kind: NavKind,
    label: String,
    a11y_name: String,
    cmd: Cmd,
}

/// The model half of a freshly built [`ApiTesterApp`] — see
/// [`ApiTesterApp::build_model`].
struct BuiltModel {
    collections: Vec<Collection>,
    environments: Vec<Environment>,
    active_env: Option<uuid::Uuid>,
    request: Request,
    history: Vec<HistoryEntry>,
}

/// The real child widgets of a freshly built [`ApiTesterApp`] — see
/// [`ApiTesterApp::build_widgets`].
struct BuiltWidgets {
    method_dropdown: Dropdown,
    env_dropdown: Dropdown,
    url_input: TextInput,
    body_input: TextInput,
    send_button: Button,
    request_tabs: ToggleGroup,
    response_view_toggle: ToggleGroup,
    sink: Sink,
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
        let (tx, rx) = std::sync::mpsc::channel();
        let model = Self::build_model(demo);
        let widgets = Self::build_widgets(&model);

        let mut app = Self {
            collections: model.collections,
            environments: model.environments,
            active_env: model.active_env,
            request: model.request,
            history: model.history,
            response: if demo {
                Some(http::sample_response())
            } else {
                None
            },
            sending: false,
            error: None,
            active_tab: RequestTab::Params,
            response_view: ResponseView::Pretty,
            collapsed: std::collections::HashSet::new(),
            body_scroll: 0.0,
            selected: if demo { Some((0, 0)) } else { None },
            method_dropdown: widgets.method_dropdown,
            env_dropdown: widgets.env_dropdown,
            url_input: widgets.url_input,
            body_input: widgets.body_input,
            send_button: widgets.send_button,
            request_tabs: widgets.request_tabs,
            response_view_toggle: widgets.response_view_toggle,
            nav_rows: Vec::new(),
            nav_meta: Vec::new(),
            sink: widgets.sink,
            actions: ui::ActionHost::new(),
            tx,
            rx,
            resource_send_invocation: None,
            resource_send_result: None,
            rail: SidebarDrawer::new()
                .left()
                .docked_width(SIDEBAR_W)
                .overlay_width(SIDEBAR_W)
                .breakpoint(RAIL_BREAKPOINT),
            rect: Rect::ZERO,
            layout: Layout::default(),
            last_cursor: Point::ZERO,
            seeded: false,
            adopted: false,
            dirty: true,
            last_sig: ChildSig::default(),
            persisted_once: false,
        };
        app.rebuild_nav();
        app
    }

    /// The model half of a fresh app: sample data when `demo`, otherwise
    /// empty/blank. Split out of [`Self::build`] to keep it under the line
    /// budget.
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

    /// The real child widgets for a fresh app, wired to `model`'s starting
    /// values. Split out of [`Self::build`] to keep it under the line budget.
    fn build_widgets(model: &BuiltModel) -> BuiltWidgets {
        let mut url_input = TextInput::local().with_kind(TextInputKind::Url);
        url_input.set_plain_chrome(true);
        url_input.set_text(&model.request.url);
        let url_input = url_input
            .with_id("api.url")
            .with_accessible_name("Request URL");
        let mut body_input = TextInput::local();
        body_input.set_plain_chrome(true);
        body_input.set_mono(true);
        body_input.set_text(&model.request.body_text);
        let body_input = body_input
            .multiline(true)
            .with_id("api.body")
            .with_accessible_name("Request body");

        let sink: Sink = Rc::new(RefCell::new(Vec::new()));

        // Method picker — a ComboBox bound to a dedicated CRDT string.
        let method_dropdown = Dropdown::new(
            HttpMethod::ALL
                .iter()
                .map(|m| m.as_str().to_string())
                .collect(),
            METHOD_PATH,
        )
        .with_id("api.method");

        // Environment picker — "No Environment" plus each named environment.
        let mut env_options = vec![NO_ENV.to_string()];
        env_options.extend(model.environments.iter().map(|e| e.name.clone()));
        let env_dropdown = Dropdown::new(env_options, ENV_PATH).with_id("api.env");

        // Send — the accent primary action.
        let send_sink = sink.clone();
        let send_button = Button::new("api.send", "Send", None)
            .primary()
            .with_size(SEND_W as f32, 36.0)
            .on_effect(move || send_sink.borrow_mut().push(Cmd::Send));

        // Request-builder tabs.
        let tab_sink = sink.clone();
        let request_tabs = ToggleGroup::new(
            RequestTab::ALL
                .iter()
                .map(|t| Segment::new(t.label()))
                .collect(),
            0,
        )
        .on_view("api.request-tabs", move |i| {
            tab_sink.borrow_mut().push(Cmd::SetTab(i));
        });

        // Response-view switch.
        let view_sink = sink.clone();
        let response_view_toggle = ToggleGroup::new(
            ResponseView::ALL
                .iter()
                .map(|v| Segment::new(v.label()))
                .collect(),
            0,
        )
        .on_view("api.response-view", move |i| {
            view_sink.borrow_mut().push(Cmd::SetView(i));
        });

        BuiltWidgets {
            method_dropdown,
            env_dropdown,
            url_input,
            body_input,
            send_button,
            request_tabs,
            response_view_toggle,
            sink,
        }
    }

    /// Snap the rail to a settled open/closed state with no animation.
    pub fn snap_rail(&mut self, open: bool) {
        self.rail.snap(open);
        self.dirty = true;
    }

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

    /// Resolve the active request against the active environment.
    fn outgoing(&self) -> Outgoing {
        let mut req = self.request.clone();
        req.url = self.url_input.value().to_string();
        req.body_text = self.body_input.value().to_string();
        http::build_outgoing(&req, &self.env_vars())
    }

    /// Kick off a real request on a worker thread.
    fn send(&mut self) {
        if self.sending {
            return;
        }
        let out = self.outgoing();
        self.sending = true;
        self.error = None;
        self.dirty = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = match http::send_blocking(&out) {
                Ok(r) => SendOutcome::Ok(r),
                Err(e) => SendOutcome::Err(e),
            };
            let _ = tx.send(outcome);
        });
    }

    /// Drain any finished send result from the worker.
    fn poll_worker(&mut self) {
        while let Ok(outcome) = self.rx.try_recv() {
            self.sending = false;
            self.dirty = true;
            match outcome {
                SendOutcome::Ok(resp) => {
                    if let Some(invocation) = self.resource_send_invocation.take() {
                        self.resource_send_result =
                            Some(qos_actions::InvocationUpdate::Succeeded {
                                invocation,
                                output: qos_actions::ActionOutput::default(),
                            });
                    }
                    let out = self.outgoing();
                    self.history.insert(
                        0,
                        HistoryEntry {
                            id: uuid::Uuid::new_v4(),
                            method: self.request.method,
                            url: out.url.clone(),
                            status: resp.status,
                            duration_ms: resp.duration_ms,
                            when: "just now".into(),
                        },
                    );
                    self.history.truncate(50);
                    self.response = Some(resp);
                    self.response_view = ResponseView::Pretty;
                    self.response_view_toggle.set_selected(0);
                    self.collapsed.clear();
                    self.body_scroll = 0.0;
                    self.rebuild_nav();
                }
                SendOutcome::Err(e) => {
                    if let Some(invocation) = self.resource_send_invocation.take() {
                        self.resource_send_result = Some(qos_actions::InvocationUpdate::Failed {
                            invocation,
                            error: qos_actions::ActionFailure {
                                code: "api_tester.request_failed".into(),
                                message: e.clone(),
                                details: None,
                                retryable: false,
                            },
                        });
                    }
                    self.error = Some(e);
                }
            }
        }
    }

    /// Load a saved request from a collection into the builder.
    fn load_request(&mut self, ci: usize, ri: usize) {
        if let Some(req) = self
            .collections
            .get(ci)
            .and_then(|c| c.requests.get(ri))
            .cloned()
        {
            self.url_input.set_text(&req.url);
            self.body_input.set_text(&req.body_text);
            self.request = req;
            self.selected = Some((ci, ri));
            self.active_tab = RequestTab::Params;
            self.request_tabs.set_selected(0);
            self.dirty = true;
            self.rebuild_nav();
        }
    }

    /// Push the whole workspace into the shared CRDT.
    fn persist(&self, state: &mut AutoCommit, root: &ObjId) {
        let mut snap = self.request.clone();
        snap.url = self.url_input.value().to_string();
        snap.body_text = self.body_input.value().to_string();
        ApiDoc::set_request(state, root, &snap);
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
            self.rebuild_env_dropdown();
        }
        if let Some(id) = ApiDoc::active_env(state, root) {
            self.active_env = id.parse().ok();
        }
        if let Some(req) = ApiDoc::request(state, root) {
            self.url_input.set_text(&req.url);
            self.body_input.set_text(&req.body_text);
            self.request = req;
        }
        let hist = ApiDoc::history(state, root);
        if !hist.is_empty() {
            self.history = hist;
        }
        self.adopted = true;
        self.dirty = true;
        self.rebuild_nav();
    }

    /// Dispatch one collaborative [`ApiTesterAction`] against the shared document.
    ///
    /// The API tester holds its workspace as an in-memory model mirrored into
    /// per-key JSON snapshots, so this first flushes the live model to `doc`
    /// (usually a no-op — the app already persisted — but it guarantees the
    /// action mutates current state and that the
    /// [`ActionHost`](ui::ActionHost) brackets *only* the action). The host
    /// encodes + applies it via the executor (one undo boundary, byte-identical
    /// replay on peers); we then re-adopt the mutated snapshots so the UI and the
    /// document agree on a single source of truth.
    fn perform(&mut self, state: &mut AutoCommit, path: &ObjId, action: &ApiTesterAction) {
        self.persist(state, path);
        self.actions.dispatch_with_context(
            state,
            API_TESTER_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.ApiTester", "API Tester", action),
        );
        self.adopt_workspace(state, path);
        self.dirty = true;
    }

    /// Re-read the collaborative workspace snapshots (collections, environments,
    /// the selected environment) from the document after an action mutated them.
    /// Deliberately leaves the active request buffer, its live URL/body `Text`,
    /// and per-peer view state untouched — those are not what an action changed.
    fn adopt_workspace(&mut self, state: &AutoCommit, path: &ObjId) {
        self.collections = ApiDoc::collections(state, path);
        self.environments = ApiDoc::environments(state, path);
        self.active_env = ApiDoc::active_env(state, path).and_then(|id| id.parse().ok());
        self.rebuild_env_dropdown();
        self.rebuild_nav();
    }

    /// Number shown on a request-builder tab badge (kept for the tests + the
    /// var hint; the tab strip itself no longer paints badges).
    #[cfg(test)]
    fn tab_count(&self, tab: RequestTab) -> usize {
        match tab {
            RequestTab::Params => Request::enabled_count(&self.request.params),
            RequestTab::Headers => Request::enabled_count(&self.request.headers),
            RequestTab::Auth => usize::from(!matches!(self.request.auth, Auth::None)),
            RequestTab::Body => usize::from(!matches!(self.request.body_mode, BodyMode::None)),
        }
    }

    /// Whether the body editor field is on screen (Body tab, JSON/Raw mode).
    fn body_field_visible(&self) -> bool {
        self.active_tab == RequestTab::Body
            && matches!(self.request.body_mode, BodyMode::Json | BodyMode::Raw)
    }

    /// Whether the sidebar (and therefore the env picker + nav rows) is part
    /// of the tree this frame.
    fn sidebar_visible(&self) -> bool {
        self.rail.is_open()
    }

    fn tab_index(&self) -> usize {
        RequestTab::ALL
            .iter()
            .position(|t| *t == self.active_tab)
            .unwrap_or(0)
    }

    /// The 0-based index of a saved request across the (expanded) collection
    /// tree — matches the nav-row ordering so an a11y "n of m" reads sensibly.
    fn child_sig(&self) -> ChildSig {
        ChildSig {
            tab: self.tab_index() as u8,
            body_field: self.body_field_visible(),
            resp_view: self.response.is_some(),
            sidebar: self.sidebar_visible(),
            nav_rows: self.nav_rows.len(),
        }
    }

    // -------------------------------------------------------------------
    // Nav rows
    // -------------------------------------------------------------------

    /// Rebuild the ordered `nav_meta` list (collections → requests → history)
    /// from the model, then rebuild the `NavRow` widgets to match, each wired
    /// to push its command onto the sink.
    fn rebuild_nav(&mut self) {
        self.nav_meta = self.compute_nav_meta();
        self.nav_rows = self
            .nav_meta
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let sink = self.sink.clone();
                let cmd = m.cmd.clone();
                NavRow::new(
                    format!("api.nav.{i}"),
                    m.kind.clone(),
                    m.label.clone(),
                    m.a11y_name.clone(),
                )
                .on_activate(move || sink.borrow_mut().push(cmd.clone()))
            })
            .collect();
    }

    /// The ordered sidebar rows derived from the model. Kept separate from
    /// widget construction so both `rebuild_nav` and (if needed) tests can
    /// reason about the row set.
    fn compute_nav_meta(&self) -> Vec<NavMeta> {
        let mut v = Vec::new();
        for (ci, col) in self.collections.iter().enumerate() {
            v.push(NavMeta {
                kind: NavKind::Collection {
                    expanded: col.expanded,
                    count: col.requests.len(),
                },
                label: col.name.clone(),
                a11y_name: format!("{}, collection, {} requests", col.name, col.requests.len()),
                cmd: Cmd::ToggleCollection(ci),
            });
            if col.expanded {
                for (ri, req) in col.requests.iter().enumerate() {
                    v.push(NavMeta {
                        kind: NavKind::Request {
                            method: req.method,
                            selected: self.selected == Some((ci, ri)),
                        },
                        label: req.name.clone(),
                        a11y_name: format!("{} {}", req.method.as_str(), req.name),
                        cmd: Cmd::LoadRequest(ci, ri),
                    });
                }
            }
        }
        for (hi, h) in self.history.iter().enumerate() {
            v.push(NavMeta {
                kind: NavKind::History {
                    method: h.method,
                    status: h.status,
                },
                label: short_path(&h.url),
                a11y_name: format!("{} {} {}", h.method.as_str(), short_path(&h.url), h.status),
                cmd: Cmd::ReplayHistory(hi),
            });
        }
        v
    }

    /// Rebuild the env dropdown after the environment list changes (seed /
    /// adopt), preserving its CRDT binding.
    fn rebuild_env_dropdown(&mut self) {
        let mut opts = vec![NO_ENV.to_string()];
        opts.extend(self.environments.iter().map(|e| e.name.clone()));
        self.env_dropdown = Dropdown::new(opts, ENV_PATH).with_id("api.env");
    }

    // -------------------------------------------------------------------
    // Ordered child access (build_layout / children / children_mut all agree)
    // -------------------------------------------------------------------

    fn children_ref(&self) -> Vec<&dyn Widget> {
        let mut v: Vec<&dyn Widget> = vec![
            &self.method_dropdown,
            &self.url_input,
            &self.send_button,
            &self.request_tabs,
        ];
        if self.body_field_visible() {
            v.push(&self.body_input);
        }
        if self.response.is_some() {
            v.push(&self.response_view_toggle);
        }
        if self.sidebar_visible() {
            v.push(&self.env_dropdown);
            for r in &self.nav_rows {
                v.push(r);
            }
        }
        v
    }

    fn children_mut_ref(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = Vec::new();
        let body = self.body_field_visible();
        let resp = self.response.is_some();
        let sidebar = self.sidebar_visible();
        v.push(&mut self.method_dropdown);
        v.push(&mut self.url_input);
        v.push(&mut self.send_button);
        v.push(&mut self.request_tabs);
        if body {
            v.push(&mut self.body_input);
        }
        if resp {
            v.push(&mut self.response_view_toggle);
        }
        if sidebar {
            v.push(&mut self.env_dropdown);
            for r in &mut self.nav_rows {
                v.push(r);
            }
        }
        v
    }

    /// Position every live child widget from the computed layout, in the same
    /// order `children_mut_ref` / `build_layout` use.
    fn position_children(&mut self) {
        let l = self.layout.clone();
        self.method_dropdown.apply_layout_external(l.method);
        self.url_input.apply_layout_external(l.url);
        self.send_button.apply_layout_external(l.send);
        self.request_tabs.apply_layout_external(l.tabs);
        if self.body_field_visible() {
            self.body_input.apply_layout_external(l.body);
        }
        if self.response.is_some() {
            self.response_view_toggle.apply_layout_external(l.resp_view);
        }
        if self.sidebar_visible() {
            self.env_dropdown.apply_layout_external(l.env);
            for (row, rect) in self.nav_rows.iter_mut().zip(l.nav.iter()) {
                row.apply_layout_external(*rect);
            }
        }
    }

    // -------------------------------------------------------------------
    // Layout
    // -------------------------------------------------------------------

    /// Compute all chrome regions + control rects for the current window rect
    /// and rail state. Pure geometry; called from both `apply_layout` and
    /// `draw`.
    fn compute_layout(&mut self) -> Layout {
        // The shared drawer resolves docked-vs-overlay from the rect and hands
        // back the sidebar / content / scrim rects: a docked column with inset
        // content when wide, or a slid-in overlay panel over a scrim when narrow.
        let mut l = Layout {
            main: self.rail.resolve(self.rect),
            ..Layout::default()
        };
        let split = self.rail.layout();
        l.sidebar = self.rail.sidebar_rect();
        l.scrim = self.rail.scrim_rect();

        // --- URL bar row ---
        let area = l.main;
        l.urlbar = Rect::new(area.x0, area.y0, area.x1, area.y0 + URLBAR_H);
        let cy0 = l.urlbar.y0 + (URLBAR_H - 36.0) / 2.0;
        let mut row_x0 = area.x0 + PAD;
        if split == SplitLayout::Overlay {
            l.hamburger = Rect::new(row_x0, cy0, row_x0 + HAMBURGER_W, cy0 + 36.0);
            row_x0 = l.hamburger.x1 + 8.0;
        }
        l.method = Rect::new(row_x0, cy0, row_x0 + METHOD_W, cy0 + 36.0);
        l.send = Rect::new(area.x1 - PAD - SEND_W, cy0, area.x1 - PAD, cy0 + 36.0);
        l.url = Rect::new(l.method.x1 + 8.0, cy0, l.send.x0 - 8.0, cy0 + 36.0);

        // --- Tab strip ---
        l.tab_strip = Rect::new(area.x0, l.urlbar.y1, area.x1, l.urlbar.y1 + TAB_STRIP_H);
        let tabs_w = 96.0 * RequestTab::ALL.len() as f64;
        l.tabs = Rect::new(
            area.x0 + PAD,
            l.tab_strip.y0 + 4.0,
            area.x0 + PAD + tabs_w,
            l.tab_strip.y1 - 4.0,
        );

        // --- Editor / response split ---
        let editor_h = ((area.height() - URLBAR_H - TAB_STRIP_H) * 0.42).max(150.0);
        l.editor = Rect::new(area.x0, l.tab_strip.y1, area.x1, l.tab_strip.y1 + editor_h);
        l.response = Rect::new(area.x0, l.editor.y1, area.x1, area.y1);

        // Body field (Body tab, JSON/Raw).
        if self.body_field_visible() {
            let e = l.editor;
            l.body = Rect::new(e.x0 + PAD, e.y0 + 52.0, e.x1 - PAD, e.y1 - 12.0);
        }

        // Response chrome + view switch.
        let status_h = response_status_height(l.response.width());
        l.resp_status = Rect::new(
            l.response.x0,
            l.response.y0,
            l.response.x1,
            l.response.y0 + status_h,
        );
        l.resp_body = Rect::new(
            l.response.x0,
            l.resp_status.y1,
            l.response.x1,
            l.response.y1,
        );
        l.body_region = l.resp_body;
        if self.response.is_some() {
            if status_h == COMPACT_RESPONSE_STATUS_H {
                // Keep the switcher a full-width, easy-to-hit control below
                // the status line. Equal segments remain legible at large
                // text sizes and the response body starts below both rows.
                l.resp_view = Rect::new(
                    l.response.x0 + PAD,
                    l.resp_status.y1 - 30.0,
                    l.response.x1 - PAD,
                    l.resp_status.y1 - 6.0,
                );
            } else {
                let view_w = 210.0;
                let cy = l.resp_status.center().y;
                l.resp_view = Rect::new(
                    l.response.x1 - PAD - view_w,
                    cy - 12.0,
                    l.response.x1 - PAD,
                    cy + 12.0,
                );
            }
        }

        // --- Sidebar rows ---
        if self.rail.reveal() > 0.001 && l.sidebar.width() > 1.0 {
            let s = l.sidebar;
            // No app-name wordmark (the OS titlebar shows "API Tester"); the ENV
            // picker leads the sidebar. Keep room above it for the ENV caption.
            let mut y = s.y0 + PAD + 16.0;
            l.env = Rect::new(s.x0 + PAD, y, s.x1 - PAD, y + 40.0);
            y += 54.0;
            l.collections_label = Point::new(s.x0 + PAD, y);
            y += 24.0;
            // Rows follow the same order as `compute_nav_meta`.
            let history_start = self
                .nav_meta
                .iter()
                .position(|m| matches!(m.kind, NavKind::History { .. }));
            for (i, _m) in self.nav_meta.iter().enumerate() {
                if Some(i) == history_start {
                    y += 12.0;
                    l.history_label = Point::new(s.x0 + PAD, y);
                    y += 24.0;
                }
                if y + ROW_H > s.y1 - PAD {
                    // Off the bottom — park the row off-screen so it still has a
                    // valid (empty) rect.
                    l.nav.push(Rect::new(s.x0, s.y1, s.x0, s.y1));
                    continue;
                }
                l.nav.push(Rect::new(s.x0 + 8.0, y, s.x1 - 8.0, y + ROW_H));
                y += ROW_H;
            }
            if history_start.is_none() {
                // No history yet — still place the HISTORY label under the
                // collections for parity with the original chrome.
                y += 12.0;
                l.history_label = Point::new(s.x0 + PAD, y);
            }
        }

        l
    }
}

impl Default for ApiTesterApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiTesterApp {
    // -------------------------------------------------------------------
    // Drawing — chrome only; controls draw themselves at their rects.
    // -------------------------------------------------------------------

    fn paint(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let l = self.layout.clone();
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.rect);

        self.draw_main(scene, t, tr, theme, presence, &l);

        // The shared drawer paints the (subtle) dimming scrim + soft edge shadow
        // behind an open overlay flap; it is a no-op when docked or closed.
        self.rail.draw_scrim(scene, t);
        if self.rail.reveal() > 0.001 && l.sidebar.width() > 1.0 {
            self.draw_sidebar(scene, t, tr, theme, presence, &l);
        }
    }

    fn draw_sidebar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        l: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let area = l.sidebar;
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &area);
        let div = Rect::new(area.x1 - 1.0, area.y0, area.x1, area.y1);
        scene.fill(Fill::NonZero, t, c.border_standard, None, &div);

        // ENV label above the picker.
        tr.draw_text_weighted(
            scene,
            "ENV",
            9.0,
            c.text_dim,
            t * Affine::translate((l.env.x0, l.env.y0 - 14.0)),
            None,
            TextWeight::Bold,
        );
        Self::draw_section_label(scene, t, tr, theme, l.collections_label, "COLLECTIONS");
        Self::draw_section_label(scene, t, tr, theme, l.history_label, "HISTORY");

        // The env dropdown + every nav row draw themselves.
        self.env_dropdown.draw(scene, t, tr, theme, presence);
        for row in &mut self.nav_rows {
            row.draw(scene, t, tr, theme, presence);
        }
    }

    fn draw_section_label(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        at: Point,
        text: &str,
    ) {
        if at == Point::ZERO {
            return;
        }
        tr.draw_text_weighted(
            scene,
            text,
            10.0,
            theme.tokens.colors.text_dim,
            t * Affine::translate((at.x, at.y)),
            None,
            TextWeight::Bold,
        );
    }

    fn draw_main(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        l: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let area = l.main;

        // URL bar background + divider.
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &l.urlbar);
        let bar_div = Rect::new(area.x0, l.urlbar.y1 - 1.0, area.x1, l.urlbar.y1);
        scene.fill(Fill::NonZero, t, c.border_standard, None, &bar_div);

        // Hamburger (overlay only) — a painted chrome affordance.
        if self.rail.layout() == SplitLayout::Overlay {
            let ham = l.hamburger;
            let shape = RoundedRect::from_rect(ham, f64::from(theme.tokens.radii.md));
            scene.fill(Fill::NonZero, t, c.lift(0.35), None, &shape);
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            draw_hamburger(scene, t, ham, c.text_primary);
        }

        // Method picker draws itself; keep the method-tinted rest look via the
        // dropdown's own themed chrome.
        self.method_dropdown.draw(scene, t, tr, theme, presence);

        // URL field box + the plain TextInput inside it.
        let url_shape = RoundedRect::from_rect(l.url, f64::from(theme.tokens.radii.md));
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &url_shape);
        scene.stroke(
            &Stroke::new(if self.url_input.is_focused() {
                2.0
            } else {
                1.0
            }),
            t,
            if self.url_input.is_focused() {
                theme.border_focus
            } else {
                c.border_standard
            },
            None,
            &url_shape,
        );
        self.url_input.draw(scene, t, tr, theme, presence);

        // Send button draws itself.
        self.send_button.draw(scene, t, tr, theme, presence);

        // Tab strip background + the ToggleGroup + the var hint.
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &l.tab_strip);
        self.request_tabs.draw(scene, t, tr, theme, presence);
        self.draw_var_hint(scene, t, tr, theme, l);
        let strip_div = Rect::new(area.x0, l.tab_strip.y1 - 1.0, area.x1, l.tab_strip.y1);
        scene.fill(Fill::NonZero, t, c.border_standard, None, &strip_div);

        // Editor + response.
        self.draw_editor(scene, t, tr, theme, presence, l);
        self.draw_response(scene, t, tr, theme, presence, l);
    }

    fn draw_var_hint(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let url_now = self.url_input.value().to_string();
        let refs = vars::referenced_vars(&url_now);
        if refs.is_empty() {
            return;
        }
        let env = self.env_vars();
        let unresolved: Vec<&String> = refs.iter().filter(|r| !env.contains_key(*r)).collect();
        let (hint, hint_col) = if unresolved.is_empty() {
            (
                format!("{} variable(s) resolved", refs.len()),
                c.semantic.success,
            )
        } else {
            (
                format!(
                    "unresolved: {}",
                    unresolved
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                c.semantic.warning,
            )
        };
        let (hw, _) = tr.measure(&hint, theme.tokens.typography.scale.sm, None);
        if !tab_strip_has_room_for_hint(l, f64::from(hw)) {
            return;
        }
        tr.draw_text(
            scene,
            &hint,
            theme.tokens.typography.scale.sm,
            hint_col,
            t * Affine::translate((
                l.main.x1 - PAD - f64::from(hw),
                l.tab_strip.center().y - 7.0,
            )),
            None,
        );
    }

    fn draw_editor(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        l: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let area = l.editor;
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &area);
        match self.active_tab {
            RequestTab::Params => {
                let rows = self.request.params.clone();
                Self::draw_kv_table(scene, t, tr, theme, area, &rows, "No query parameters");
            }
            RequestTab::Headers => {
                let rows = self.request.headers.clone();
                Self::draw_kv_table(scene, t, tr, theme, area, &rows, "No headers");
            }
            RequestTab::Auth => self.draw_auth(scene, t, tr, theme, area),
            RequestTab::Body => self.draw_body(scene, t, tr, theme, presence, l),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_kv_table(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        rows: &[KeyValue],
        empty_msg: &str,
    ) {
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let y = Self::draw_kv_table_header(scene, t, tr, theme, area);

        if rows.iter().all(super::model::KeyValue::is_blank) {
            tr.draw_text(
                scene,
                empty_msg,
                theme.tokens.typography.scale.sm,
                c.text_dim,
                t * Affine::translate((x0, y + 8.0)),
                None,
            );
            return;
        }

        let y = Self::draw_kv_rows(scene, t, tr, theme, area, rows, y);
        Self::draw_kv_new_row_placeholder(scene, t, tr, theme, area, y);
    }

    /// The KEY / VALUE column headers. Returns the y of the first data row.
    fn draw_kv_table_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let y = area.y0 + 12.0;
        let key_x = x0 + 30.0;
        let val_x = area.x0 + area.width() * 0.5;
        tr.draw_text_weighted(
            scene,
            "KEY",
            10.0,
            c.text_dim,
            t * Affine::translate((key_x, y)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text_weighted(
            scene,
            "VALUE",
            10.0,
            c.text_dim,
            t * Affine::translate((val_x, y)),
            None,
            TextWeight::Bold,
        );
        y + 22.0
    }

    /// One row per key/value pair, stopping once `area` runs out of room.
    /// Returns the y below the last drawn row.
    #[allow(clippy::too_many_arguments)]
    fn draw_kv_rows(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        rows: &[KeyValue],
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let key_x = x0 + 30.0;
        let val_x = area.x0 + area.width() * 0.5;
        for row in rows {
            if y + ROW_H > area.y1 {
                break;
            }
            let cb = Rect::new(x0, y + 4.0, x0 + 16.0, y + 20.0);
            Checkbox::paint(
                scene,
                t,
                theme,
                &CheckboxFace::new(cb, row.enabled).transparent_unchecked(),
            );
            let row_col = if row.enabled {
                c.text_primary
            } else {
                c.text_dim
            };
            tr.draw_text(
                scene,
                &row.key,
                theme.tokens.typography.scale.sm,
                row_col,
                t * Affine::translate((key_x, y + 7.0)),
                Some((val_x - key_x - 12.0) as f32),
            );
            Self::draw_value_with_vars(
                scene,
                t,
                tr,
                theme,
                &row.value,
                val_x,
                y + 7.0,
                area.x1 - PAD - val_x,
                row.enabled,
            );
            let sep = Rect::new(x0, y + ROW_H - 1.0, area.x1 - PAD, y + ROW_H);
            scene.fill(
                Fill::NonZero,
                t,
                c.border_standard.with_alpha_factor(0.4),
                None,
                &sep,
            );
            y += ROW_H;
        }
        y
    }

    /// The trailing "New key" / "New value" placeholder row, when there is
    /// still room for one below `y`.
    fn draw_kv_new_row_placeholder(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        y: f64,
    ) {
        if y + ROW_H > area.y1 {
            return;
        }
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let key_x = x0 + 30.0;
        let val_x = area.x0 + area.width() * 0.5;
        let cb = Rect::new(x0, y + 4.0, x0 + 16.0, y + 20.0);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard.with_alpha_factor(0.5),
            None,
            &RoundedRect::from_rect(cb, 4.0),
        );
        tr.draw_text(
            scene,
            "New key",
            theme.tokens.typography.scale.sm,
            c.text_dim.with_alpha_factor(0.6),
            t * Affine::translate((key_x, y + 7.0)),
            None,
        );
        tr.draw_text(
            scene,
            "New value",
            theme.tokens.typography.scale.sm,
            c.text_dim.with_alpha_factor(0.6),
            t * Affine::translate((val_x, y + 7.0)),
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_value_with_vars(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        value: &str,
        start_x: f64,
        y: f64,
        max_w: f64,
        enabled: bool,
    ) {
        let c = &theme.tokens.colors;
        let base_col = if enabled { c.text_primary } else { c.text_dim };
        let size = theme.tokens.typography.scale.sm;
        let mut cx = start_x;
        let mut rest = value;
        let limit = start_x + max_w;
        while !rest.is_empty() && cx < limit {
            if let Some(open) = rest.find("{{") {
                // `open` and `close` are byte offsets of an ASCII pattern
                // match, always on a UTF-8 boundary — `.get()` still avoids
                // asserting that rather than indexing blindly.
                if open > 0 {
                    let Some(plain) = rest.get(..open) else {
                        break;
                    };
                    tr.draw_text(
                        scene,
                        plain,
                        size,
                        base_col,
                        t * Affine::translate((cx, y)),
                        None,
                    );
                    let (w, _) = tr.measure(plain, size, None);
                    cx += f64::from(w);
                }
                if let Some(after_open) = rest.get(open..)
                    && let Some(close) = after_open.find("}}")
                {
                    let Some(var) = rest.get(open..open + close + 2) else {
                        break;
                    };
                    tr.draw_text_weighted(
                        scene,
                        var,
                        size,
                        theme.accent_primary,
                        t * Affine::translate((cx, y)),
                        None,
                        TextWeight::Semibold,
                    );
                    let (w, _) = tr.measure(var, size, None);
                    cx += f64::from(w);
                    rest = rest.get(open + close + 2..).unwrap_or_default();
                    continue;
                }
            }
            tr.draw_text(
                scene,
                rest,
                size,
                base_col,
                t * Affine::translate((cx, y)),
                Some((limit - cx) as f32),
            );
            break;
        }
    }

    fn draw_auth(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let mut y = area.y0 + 16.0;
        tr.draw_text_weighted(
            scene,
            "AUTH TYPE",
            10.0,
            c.text_dim,
            t * Affine::translate((x0, y)),
            None,
            TextWeight::Bold,
        );
        y += 20.0;

        // Mode chips (display of the active scheme; the scheme is fixed per
        // loaded request in this app, so these are read-only chrome).
        let mut cx = x0;
        for label in Auth::LABELS {
            let active = self.request.auth.label() == label;
            let (lw, _) = tr.measure(label, theme.tokens.typography.scale.sm, None);
            let chip = Rect::new(cx, y, cx + f64::from(lw) + 24.0, y + 30.0);
            let shape = RoundedRect::from_rect(chip, f64::from(theme.tokens.radii.full));
            if active {
                scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
            } else {
                scene.fill(Fill::NonZero, t, c.lift(0.35), None, &shape);
                scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            }
            tr.draw_text_weighted(
                scene,
                label,
                theme.tokens.typography.scale.sm,
                if active { Color::WHITE } else { c.text_dim },
                t * Affine::translate((cx + 12.0, y + 8.0)),
                None,
                TextWeight::Semibold,
            );
            cx = chip.x1 + 8.0;
        }
        y += 50.0;

        let draw_field = |scene: &mut Scene,
                          tr: &mut TextRenderer,
                          y: f64,
                          label: &str,
                          value: &str,
                          secret: bool| {
            tr.draw_text_weighted(
                scene,
                label,
                10.0,
                c.text_dim,
                t * Affine::translate((x0, y)),
                None,
                TextWeight::Bold,
            );
            let field = Rect::new(x0, y + 16.0, area.x1 - PAD, y + 50.0);
            let shape = RoundedRect::from_rect(field, f64::from(theme.tokens.radii.md));
            scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            let shown = if secret && !value.is_empty() {
                "\u{2022}".repeat(value.chars().count().min(24))
            } else {
                value.to_string()
            };
            tr.draw_text(
                scene,
                &shown,
                theme.tokens.typography.scale.sm,
                c.text_primary,
                t * Affine::translate((field.x0 + 12.0, field.center().y - 7.0)),
                Some((field.width() - 24.0) as f32),
            );
        };

        match &self.request.auth {
            Auth::None => {
                tr.draw_text(
                    scene,
                    "This request does not use authentication.",
                    theme.tokens.typography.scale.sm,
                    c.text_dim,
                    t * Affine::translate((x0, y)),
                    None,
                );
            }
            Auth::Bearer { token } => draw_field(scene, tr, y, "TOKEN", token, true),
            Auth::Basic { username, password } => {
                draw_field(scene, tr, y, "USERNAME", username, false);
                draw_field(scene, tr, y + 70.0, "PASSWORD", password, true);
            }
            Auth::ApiKey { header, value } => {
                draw_field(scene, tr, y, "HEADER NAME", header, false);
                draw_field(scene, tr, y + 70.0, "VALUE", value, true);
            }
        }
    }

    fn draw_body(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        l: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let area = l.editor;
        let x0 = area.x0 + PAD;
        let mut y = area.y0 + 12.0;

        // Body-mode chips (read-only chrome — mode is fixed per request).
        let mut cx = x0;
        for mode in BodyMode::ALL {
            let active = self.request.body_mode == mode;
            let label = mode.label();
            let (lw, _) = tr.measure(label, theme.tokens.typography.scale.sm, None);
            let chip = Rect::new(cx, y, cx + f64::from(lw) + 20.0, y + 28.0);
            let shape = RoundedRect::from_rect(chip, f64::from(theme.tokens.radii.md));
            if active {
                scene.fill(
                    Fill::NonZero,
                    t,
                    theme.accent_primary.with_alpha_factor(0.2),
                    None,
                    &shape,
                );
                scene.stroke(&Stroke::new(1.0), t, theme.accent_primary, None, &shape);
            } else {
                scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            }
            tr.draw_text_weighted(
                scene,
                label,
                theme.tokens.typography.scale.sm,
                if active { c.text_primary } else { c.text_dim },
                t * Affine::translate((cx + 10.0, y + 6.0)),
                None,
                if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                },
            );
            cx = chip.x1 + 8.0;
        }
        y += 40.0;

        match self.request.body_mode {
            BodyMode::None => {
                tr.draw_text(
                    scene,
                    "This request does not have a body.",
                    theme.tokens.typography.scale.sm,
                    c.text_dim,
                    t * Affine::translate((x0, y)),
                    None,
                );
            }
            BodyMode::Form => {
                let rows = self.request.form.clone();
                let sub = Rect::new(area.x0, y - 8.0, area.x1, area.y1);
                Self::draw_kv_table(scene, t, tr, theme, sub, &rows, "No form fields");
            }
            BodyMode::Json | BodyMode::Raw => {
                let field = l.body;
                let shape = RoundedRect::from_rect(field, f64::from(theme.tokens.radii.md));
                scene.fill(
                    Fill::NonZero,
                    t,
                    theme.tokens.colors.neutral[1],
                    None,
                    &shape,
                );
                scene.stroke(
                    &Stroke::new(if self.body_input.is_focused() {
                        2.0
                    } else {
                        1.0
                    }),
                    t,
                    if self.body_input.is_focused() {
                        theme.border_focus
                    } else {
                        c.border_standard
                    },
                    None,
                    &shape,
                );
                self.body_input.draw(scene, t, tr, theme, presence);
            }
        }
    }

    fn draw_response(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        l: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let area = l.response;
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &area);
        let top_div = Rect::new(area.x0, area.y0, area.x1, area.y0 + 1.0);
        scene.fill(Fill::NonZero, t, c.border_standard, None, &top_div);

        let row = l.resp_status;
        let x0 = area.x0 + PAD;
        let cy = response_status_line_center(row);

        if let Some(resp) = self.response.clone() {
            let scol = status_color(theme, resp.status_class());
            let pill_label = format!("{} {}", resp.status, resp.status_text);
            let (pw, _) = tr.measure(&pill_label, theme.tokens.typography.scale.sm, None);
            let pill = Rect::new(x0, cy - 12.0, x0 + f64::from(pw) + 24.0, cy + 12.0);
            let pill_shape = RoundedRect::from_rect(pill, f64::from(theme.tokens.radii.full));
            scene.fill(
                Fill::NonZero,
                t,
                scol.with_alpha_factor(0.18),
                None,
                &pill_shape,
            );
            tr.draw_text_weighted(
                scene,
                &pill_label,
                theme.tokens.typography.scale.sm,
                scol,
                t * Affine::translate((pill.x0 + 12.0, cy - 7.0)),
                None,
                TextWeight::Bold,
            );
            let meta = format!(
                "{}   {}",
                human_duration(resp.duration_ms),
                format_bytes(u64::try_from(resp.size_bytes).unwrap_or(u64::MAX))
            );
            tr.draw_text(
                scene,
                &meta,
                theme.tokens.typography.scale.sm,
                c.text_dim,
                t * Affine::translate((pill.x1 + 16.0, cy - 7.0)),
                None,
            );

            // View switch draws itself.
            self.response_view_toggle
                .draw(scene, t, tr, theme, presence);

            let body_area = l.resp_body;
            scene.fill(
                Fill::NonZero,
                t,
                theme.tokens.colors.neutral[1],
                None,
                &body_area,
            );
            let body_div = Rect::new(area.x0, body_area.y0, area.x1, body_area.y0 + 1.0);
            scene.fill(Fill::NonZero, t, c.border_standard, None, &body_div);

            match self.response_view {
                ResponseView::Pretty => {
                    if resp.is_json() {
                        self.draw_json_body(scene, t, tr, theme, &resp, body_area);
                    } else {
                        self.draw_raw_body(scene, t, tr, theme, &resp.body, body_area);
                    }
                }
                ResponseView::Raw => self.draw_raw_body(scene, t, tr, theme, &resp.body, body_area),
                ResponseView::Preview => {
                    Self::draw_response_headers(scene, t, tr, theme, &resp, body_area);
                }
            }
        } else if let Some(err) = &self.error {
            tr.draw_text_weighted(
                scene,
                "Request failed",
                theme.tokens.typography.scale.base,
                c.semantic.danger,
                t * Affine::translate((x0, cy - 7.0)),
                None,
                TextWeight::Bold,
            );
            tr.draw_text(
                scene,
                err,
                theme.tokens.typography.scale.sm,
                c.text_dim,
                t * Affine::translate((x0, row.y1 + 12.0)),
                Some((area.width() - PAD * 2.0) as f32),
            );
        } else {
            tr.draw_text(
                scene,
                "Send a request to see the response here.",
                theme.tokens.typography.scale.base,
                c.text_dim,
                t * Affine::translate((x0, cy - 7.0)),
                None,
            );
        }
    }

    fn draw_json_body(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        resp: &ResponseData,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        let pretty = vars::pretty_json(&resp.body).unwrap_or_else(|| resp.body.clone());
        let lines = vars::tokenize_json(&pretty);
        let size = theme.tokens.typography.scale.sm;
        let gutter_w = 44.0;
        let text_x0 = area.x0 + gutter_w + 8.0;
        self.layout.fold_carets.clear();

        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &area,
        );
        let mut screen_y = area.y0 + 8.0 - self.body_scroll;
        let mut line_no = 0usize;
        let mut hidden_until: Option<usize> = None;

        for (idx, line) in lines.iter().enumerate() {
            if let Some(close) = hidden_until {
                if idx <= close {
                    continue;
                }
                hidden_until = None;
            }
            line_no += 1;
            if screen_y + JSON_LINE_H >= area.y0 && screen_y <= area.y1 {
                let ln = line_no.to_string();
                let (lnw, _) = tr.measure(&ln, 11.0, None);
                tr.draw_text(
                    scene,
                    &ln,
                    11.0,
                    c.text_dim.with_alpha_factor(0.6),
                    t * Affine::translate((
                        area.x0 + gutter_w - f64::from(lnw) - 6.0,
                        screen_y + 3.0,
                    )),
                    None,
                );

                let collapsed = self.collapsed.contains(&idx);
                if line.opens_block && line.close_line.is_some() {
                    let caret_pt =
                        Point::new(area.x0 + gutter_w - 2.0, screen_y + JSON_LINE_H / 2.0);
                    draw_fold_caret(scene, t, caret_pt, c.text_dim, !collapsed);
                    let caret_rect = Rect::new(
                        area.x0 + gutter_w - 10.0,
                        screen_y,
                        area.x0 + gutter_w + 6.0,
                        screen_y + JSON_LINE_H,
                    );
                    self.layout.fold_carets.push((caret_rect, idx));
                }
                let mut cx = text_x0;
                for tok in &line.tokens {
                    let col = token_color(theme, tok.kind);
                    tr.draw_text_styled(
                        scene,
                        &tok.text,
                        size,
                        col,
                        t * Affine::translate((cx, screen_y + 3.0)),
                        None,
                        TextWeight::Regular,
                        false,
                        true,
                    );
                    let (w, _) =
                        tr.measure_styled(&tok.text, size, None, TextWeight::Regular, false, true);
                    cx += f64::from(w);
                }
                if collapsed && line.opens_block {
                    tr.draw_text(
                        scene,
                        " \u{2026} ",
                        size,
                        c.text_dim,
                        t * Affine::translate((cx, screen_y + 3.0)),
                        None,
                    );
                }
            }
            if line.opens_block
                && self.collapsed.contains(&idx)
                && let Some(close) = line.close_line
            {
                hidden_until = Some(close);
            }
            screen_y += JSON_LINE_H;
        }
        scene.pop_layer();
    }

    fn draw_raw_body(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: &str,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &area,
        );
        let size = theme.tokens.typography.scale.sm;
        let mut y = area.y0 + 8.0 - self.body_scroll;
        for line in body.lines() {
            if y + JSON_LINE_H >= area.y0 && y <= area.y1 {
                tr.draw_text_styled(
                    scene,
                    line,
                    size,
                    c.text_primary,
                    t * Affine::translate((area.x0 + PAD, y + 3.0)),
                    None,
                    TextWeight::Regular,
                    false,
                    true,
                );
            }
            y += JSON_LINE_H;
        }
        scene.pop_layer();
    }

    fn draw_response_headers(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        resp: &ResponseData,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let mut y = area.y0 + 12.0;
        tr.draw_text_weighted(
            scene,
            "RESPONSE HEADERS",
            10.0,
            c.text_dim,
            t * Affine::translate((x0, y)),
            None,
            TextWeight::Bold,
        );
        y += 24.0;
        let key_w = (area.width() * 0.32).min(280.0);
        for h in &resp.headers {
            if y + 26.0 > area.y1 {
                break;
            }
            tr.draw_text_weighted(
                scene,
                &h.key,
                theme.tokens.typography.scale.sm,
                c.semantic.info,
                t * Affine::translate((x0, y)),
                Some((key_w - 12.0) as f32),
                TextWeight::Semibold,
            );
            tr.draw_text(
                scene,
                &h.value,
                theme.tokens.typography.scale.sm,
                c.text_primary,
                t * Affine::translate((x0 + key_w, y)),
                Some((area.width() - PAD * 2.0 - key_w) as f32),
            );
            let sep = Rect::new(x0, y + 22.0, area.x1 - PAD, y + 23.0);
            scene.fill(
                Fill::NonZero,
                t,
                c.border_standard.with_alpha_factor(0.4),
                None,
                &sep,
            );
            y += 28.0;
        }
    }

    // -------------------------------------------------------------------
    // Command application
    // -------------------------------------------------------------------

    /// Drain the command sink and apply each command, persisting when a
    /// command changes shared state. Returns `true` if anything was applied.
    fn drain_commands(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let cmds: Vec<Cmd> = self.sink.borrow_mut().drain(..).collect();
        if cmds.is_empty() {
            return false;
        }
        for cmd in cmds {
            match cmd {
                Cmd::Send => self.send(),
                Cmd::SetTab(i) => {
                    if let Some(&tab) = RequestTab::ALL.get(i) {
                        self.active_tab = tab;
                    }
                }
                Cmd::SetView(i) => {
                    if let Some(&v) = ResponseView::ALL.get(i) {
                        self.response_view = v;
                    }
                }
                Cmd::ToggleCollection(ci) => {
                    if let Some(col) = self.collections.get_mut(ci) {
                        col.expanded = !col.expanded;
                    }
                    self.rebuild_nav();
                    self.persist(state, path);
                }
                Cmd::LoadRequest(ci, ri) => {
                    self.load_request(ci, ri);
                    self.seed_method_env(state, path);
                    self.persist(state, path);
                }
                Cmd::ReplayHistory(hi) => {
                    if let Some(h) = self.history.get(hi).cloned() {
                        self.request.method = h.method;
                        self.url_input.set_text(&h.url);
                        self.request.url = h.url;
                        self.seed_method_env(state, path);
                    }
                    self.persist(state, path);
                }
            }
        }
        self.dirty = true;
        true
    }

    /// Reconcile the model from the CRDT-bound pickers after they may have
    /// changed the document.
    ///
    /// The method picker feeds the *active request buffer* (presentation until a
    /// whole-request save), so it updates the model directly. Selecting the
    /// active **environment** is collaborative workspace state persisted in the
    /// shared document, so that change is routed through the action host — the
    /// same [`SetActiveEnvironment`](ApiTesterAction::SetActiveEnvironment) a
    /// Ctrl+K palette dispatch would apply.
    fn reconcile_pickers(&mut self, state: &mut AutoCommit, path: &ObjId) {
        let sel = self.method_dropdown.selected();
        if !sel.is_empty() {
            let m = HttpMethod::from_str(sel);
            if m != self.request.method {
                self.request.method = m;
                self.dirty = true;
            }
        }
        let esel = self.env_dropdown.selected().to_string();
        if !esel.is_empty() {
            let new_env = if esel == NO_ENV {
                None
            } else {
                self.environments
                    .iter()
                    .find(|e| e.name == esel)
                    .map(|e| e.id)
            };
            if new_env != self.active_env {
                self.perform(
                    state,
                    path,
                    &actions::set_active_environment_action(new_env),
                );
            }
        }
    }

    /// Seed the picker CRDT strings from the current model so the dropdowns
    /// display the right value.
    fn seed_method_env(&self, state: &mut AutoCommit, path: &ObjId) {
        ApiDoc::set_text(state, path, METHOD_PATH, self.request.method.as_str());
        let env_name = self
            .active_env
            .and_then(|id| self.environments.iter().find(|e| e.id == id))
            .map_or_else(|| NO_ENV.to_string(), |e| e.name.clone());
        ApiDoc::set_text(state, path, ENV_PATH, &env_name);
    }
}

// ===========================================================================
// Small drawing primitives (chrome)
// ===========================================================================

/// Three stacked bars — the hamburger toggle that summons the overlay rail.
fn draw_hamburger(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    Icon::Menu.draw(scene, t, rect, color);
}

/// A triangle fold caret: pointing down when `expanded`, right when not.
fn draw_fold_caret(scene: &mut Scene, t: Affine, center: Point, color: Color, expanded: bool) {
    let mut p = BezPath::new();
    let (cx, cy) = (center.x, center.y);
    if expanded {
        p.move_to((cx - 3.0, cy - 2.0));
        p.line_to((cx + 3.0, cy - 2.0));
        p.line_to((cx, cy + 3.0));
    } else {
        p.move_to((cx - 2.0, cy - 3.0));
        p.line_to((cx + 3.0, cy));
        p.line_to((cx - 2.0, cy + 3.0));
    }
    p.close_path();
    scene.fill(Fill::NonZero, t, color, None, &p);
}

// ===========================================================================
// Widget impl
// ===========================================================================

impl Widget for ApiTesterApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::API_TESTER_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.adopted {
            if ApiDoc::is_empty(state, path) {
                if !self.seeded {
                    self.persist(state, path);
                    let url = self.url_input.value().to_string();
                    let body = self.body_input.value().to_string();
                    ApiDoc::set_text(state, path, "url", &url);
                    ApiDoc::set_text(state, path, "body", &body);
                    self.seed_method_env(state, path);
                    self.seeded = true;
                    self.adopted = true;
                    self.persisted_once = true;
                }
            } else {
                self.adopt(state, path);
                // Ensure the pickers have a value even for a doc seeded before
                // this version existed.
                if ApiDoc::read_text(state, path, METHOD_PATH).is_empty() {
                    self.seed_method_env(state, path);
                }
            }
        }

        // Bind + sync the live text objects.
        let url_obj = ApiDoc::url_text(state, path);
        self.url_input.bind_text(url_obj);
        let body_obj = ApiDoc::body_text(state, path);
        self.body_input.bind_text(body_obj);
        self.url_input.sync(state, path);
        self.body_input.sync(state, path);

        // Apply any commands an assistive technology queued by activating a
        // control directly through `perform_action` (which bypasses the
        // container's `handle_event`, where pointer/keyboard activations are
        // drained). Draining here too means Orca / the built-in reader can
        // Send, switch tabs, and open rows just like the keyboard.
        self.drain_commands(state, path);

        // Sync + reconcile the CRDT-bound pickers.
        self.method_dropdown.sync(state, path);
        self.env_dropdown.sync(state, path);
        self.reconcile_pickers(state, path);

        // Keep the widgets in step with app state the app itself drives.
        self.send_button.set_enabled(!self.sending);
        self.request_tabs.set_selected(self.tab_index());
        self.response_view_toggle.set_selected(
            ResponseView::ALL
                .iter()
                .position(|v| *v == self.response_view)
                .unwrap_or(0),
        );
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Build a child taffy node per live control, in the SAME order
        // `children`/`children_mut`/`position_children` use, so the host's
        // focus walk (which zips `children()` against `taffy.children()`)
        // lines up. The children are hand-positioned in `apply_layout`, so
        // their taffy geometry is unused — the nodes exist only so each
        // control is registered in `node_map` (a11y + focus + hit-testing).
        let mut child_nodes: Vec<NodeId> = Vec::new();
        {
            let mut kids = self.children_mut_ref();
            for k in &mut kids {
                child_nodes.push(k.build_layout(taffy, theme, node_map));
            }
        }
        // `new_with_children` only fails when a child belongs to a different
        // tree; every entry in `child_nodes` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "every child node was just created in this same tree"
        )]
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &child_nodes,
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.last_sig = self.child_sig();
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.layout = self.compute_layout();
        self.position_children();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.poll_worker();
        // Advance the drawer's eased reveal (it self-ticks off wall-clock and
        // honours reduce-motion), then recompute geometry so the controls sit
        // exactly on the chrome this frame.
        self.rail.tick(theme.reduce_motion);
        self.layout = self.compute_layout();
        self.position_children();
        self.paint(scene, transform, text_renderer, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // Route the event through the child widgets (pointer-capture aware),
        // exactly like a `Column`.
        let child_nodes = taffy.children(node_id).unwrap_or_default();
        {
            let mut kids = self.children_mut_ref();
            if let Some(idx) = kids.iter().position(|k| k.wants_pointer_capture()) {
                if let Some(&cnode) = child_nodes.get(idx)
                    && let Some(k) = kids.get_mut(idx)
                {
                    k.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        cnode,
                        taffy,
                    );
                }
            } else {
                for (k, &cn) in kids.iter_mut().zip(child_nodes.iter()) {
                    k.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        cn,
                        taffy,
                    );
                    if *handled {
                        break;
                    }
                }
            }
        }

        // Apply any commands the children queued, then reconcile the pickers.
        let did = self.drain_commands(state, path);
        self.reconcile_pickers(state, path);
        if did || *handled {
            self.dirty = true;
        }
        if *handled {
            return;
        }

        // Chrome-only interactions (no backing widget).
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.rail.layout() == SplitLayout::Overlay && self.rail.is_open() {
                    self.rail.close();
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.layout.body_region.contains(self.last_cursor) {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * -40.0,
                        MouseScrollDelta::PixelDelta(p) => -p.y,
                    };
                    self.body_scroll = (self.body_scroll + dy).max(0.0);
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_chrome_click(self.last_cursor);
                // A chrome click that hit something marks dirty; either way we
                // don't claim `handled` unless it landed on chrome.
            }
            _ => {}
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.children_ref()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.children_mut_ref()
    }

    fn name(&self) -> &'static str {
        "ApiTesterApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Publish the resource tree the shell's one Ctrl+K palette walks — the
    /// collections, saved requests, environments, and variables currently in the
    /// shared document. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Drain the undo boundary the [`ActionHost`](ui::ActionHost) bracketed for
    /// this event so the shell records exactly one undo point per dispatched
    /// action — the same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind != qos_actions::OperationKind::Effect
            || type_id != crate::actions::API_TESTER_OPERATION_TYPE
        {
            return Ok(false);
        }
        let operation: crate::actions::ApiTesterOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        match operation {
            crate::actions::ApiTesterOperation::SendRequest {
                collection_id,
                request_id,
            } => {
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
                self.load_request(collection_index, request_index);
                self.send();
            }
        }
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != qos_actions::OperationKind::Effect
            || type_id != crate::actions::API_TESTER_OPERATION_TYPE
        {
            return Ok(None);
        }
        if self.sending || self.resource_send_invocation.is_some() {
            return Err("another API request is already running".into());
        }
        if !self.invoke_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        self.resource_send_invocation = Some(invocation);
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
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

    fn take_structure_dirty(&mut self) -> bool {
        // Drain child structure-dirty flags per the trait contract, then add
        // our own child-set changes. A plain loop, not `.any()`: every child
        // must be drained regardless of an earlier `true`, which `.any()`
        // would short-circuit past.
        let mut child_changed = false;
        for c in self.children_mut_ref() {
            child_changed |= c.take_structure_dirty();
        }
        let sig = self.child_sig();
        let self_changed = sig != self.last_sig;
        self.last_sig = sig;
        child_changed || self_changed
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.sending
            || self.rail.is_animating()
            || self.children_ref().iter().any(|c| c.needs_redraw())
    }

    fn needs_animation_frame(&self) -> bool {
        self.rail.is_animating()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        for c in self.children_mut_ref() {
            c.mark_drawn();
        }
    }

    fn needs_idle_sync(&self) -> bool {
        self.sending || !self.adopted
    }
}

impl ApiTesterApp {
    /// Handle a left-press on painted chrome that has no backing widget:
    /// the hamburger toggle, the overlay scrim, and JSON fold carets.
    fn on_chrome_click(&mut self, p: Point) {
        if self.rail.layout() == SplitLayout::Overlay {
            if self.layout.hamburger.width() > 0.0 && self.layout.hamburger.contains(p) {
                self.rail.toggle();
                self.dirty = true;
                return;
            }
            if self.rail.is_open()
                && !self.layout.sidebar.contains(p)
                && self.layout.scrim.contains(p)
            {
                self.rail.close();
                self.dirty = true;
                return;
            }
        }
        for (rect, idx) in self.layout.fold_carets.clone() {
            if rect.contains(p) {
                if self.collapsed.contains(&idx) {
                    self.collapsed.remove(&idx);
                } else {
                    self.collapsed.insert(idx);
                }
                self.dirty = true;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> ApiTesterApp {
        ApiTesterApp::new_demo()
    }

    #[test]
    fn demo_app_has_seeded_workspace() {
        let a = ApiTesterApp::new_demo();
        assert!(!a.collections.is_empty());
        assert!(!a.environments.is_empty());
        assert!(a.active_env.is_some());
        assert!(a.response.is_some());
    }

    #[test]
    fn live_app_starts_empty() {
        let a = ApiTesterApp::new();
        assert!(a.collections.is_empty(), "no fabricated collections");
        assert!(a.environments.is_empty(), "no fabricated environments");
        assert!(a.history.is_empty(), "no fabricated history");
        assert!(
            a.response.is_none(),
            "no canned response before a real Send"
        );
    }

    #[test]
    fn env_vars_resolve_from_active_environment() {
        let a = app();
        let vars = a.env_vars();
        assert_eq!(
            vars.get("base_url").map(String::as_str),
            Some("https://api.example.com/v2")
        );
    }

    #[test]
    fn outgoing_substitutes_environment_into_url() {
        let a = app();
        let out = a.outgoing();
        assert!(out.url.starts_with("https://api.example.com/v2/users"));
        assert!(out.url.contains("page=1"));
        assert!(out.url.contains("per_page=25"));
        assert!(!out.url.contains("sort="));
    }

    #[test]
    fn loading_a_saved_request_swaps_the_builder() {
        let mut a = app();
        a.load_request(0, 2);
        assert_eq!(a.request.method, HttpMethod::Post);
        assert_eq!(a.url_input.value(), "{{base_url}}/users");
        assert_eq!(a.request.body_mode, BodyMode::Json);
        assert_eq!(a.selected, Some((0, 2)));
    }

    #[test]
    fn tab_count_reflects_enabled_rows() {
        let a = app();
        assert_eq!(a.tab_count(RequestTab::Params), 2);
        assert_eq!(a.tab_count(RequestTab::Headers), 2);
        assert_eq!(a.tab_count(RequestTab::Auth), 1);
    }

    #[test]
    fn persist_then_adopt_round_trips_through_crdt() {
        let a = app();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        a.persist(&mut doc, &root);

        let mut b = ApiTesterApp::new();
        b.collections.clear();
        b.adopt(&doc, &root);
        assert!(!b.collections.is_empty());
        assert_eq!(b.collections.len(), a.collections.len());
    }

    /// The environment picker is wired through the action host: selecting a new
    /// environment dispatches exactly the [`SetActiveEnvironment`] the Ctrl+K
    /// "activate" spec would build (affordance == `ActionSpec`), and the choice
    /// lands in the shared document. Drives the real reconcile path.
    #[test]
    fn selecting_environment_dispatches_the_set_active_action() {
        let mut a = ApiTesterApp::new_demo();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        // First sync seeds the workspace into the doc (active env = Production).
        a.sync(&mut doc, &root);
        assert!(a.environments.len() >= 2, "demo seeds several environments");
        let target = a.environments[1].clone();
        assert_ne!(a.active_env, Some(target.id), "target isn't already active");

        // Drive the picker the real way: its selection lives in the CRDT string
        // the Dropdown binds to. Sync the dropdown, then reconcile.
        ApiDoc::set_text(&mut doc, &root, ENV_PATH, &target.name);
        a.env_dropdown.sync(&mut doc, &root);
        a.reconcile_pickers(&mut doc, &root);

        assert_eq!(
            a.active_env,
            Some(target.id),
            "the active environment switched"
        );
        let dispatched = a
            .actions
            .last_dispatched()
            .expect("the picker dispatched an action");
        let expected = actions::encode(&actions::set_active_environment_action(Some(target.id)));
        assert_eq!(dispatched.type_id, expected.type_id);
        assert_eq!(
            dispatched.payload, expected.payload,
            "affordance == ActionSpec"
        );
        assert_eq!(
            ApiDoc::active_env(&doc, &root),
            Some(target.id.to_string()),
            "the selection is recorded in the shared document"
        );
    }

    #[test]
    fn demo_builds_nav_rows_for_collections_and_history() {
        let a = app();
        // At least the collection headers + the seeded history entries.
        assert!(
            !a.nav_rows.is_empty(),
            "sidebar rows are real focusable widgets"
        );
        assert_eq!(a.nav_rows.len(), a.nav_meta.len());
    }

    #[test]
    fn compact_tab_strip_reserves_no_overlapping_hint_space() {
        let mut a = app();
        a.rect = Rect::new(0.0, 0.0, 400.0, 844.0);
        let layout = a.compute_layout();
        assert!(
            layout.tabs.x1 <= layout.main.x1,
            "tabs stay inside phone width"
        );
        assert!(
            !tab_strip_has_room_for_hint(&layout, 112.0),
            "a status hint would overlap the compact request tabs and must be hidden"
        );
    }

    #[test]
    fn compact_response_chrome_gets_a_separate_switcher_row() {
        let mut a = app();
        a.response = Some(http::sample_response());
        a.rect = Rect::new(0.0, 0.0, 400.0, 844.0);
        let layout = a.compute_layout();
        assert_eq!(layout.resp_status.height(), COMPACT_RESPONSE_STATUS_H);
        assert!(layout.resp_view.y0 >= layout.resp_status.y0 + 44.0);
        assert!(layout.resp_view.y1 <= layout.resp_status.y1);
        assert_eq!(layout.resp_body.y0, layout.resp_status.y1);
        assert!(layout.resp_view.x0 >= layout.response.x0 + PAD);
        assert!(layout.resp_view.x1 <= layout.response.x1 - PAD);
        assert!(
            response_status_line_center(layout.resp_status) + 12.0 < layout.resp_view.y0,
            "status pill must clear the compact view switcher"
        );
    }

    #[test]
    fn wide_response_chrome_keeps_the_inline_switcher() {
        let mut a = app();
        a.response = Some(http::sample_response());
        a.rect = Rect::new(0.0, 0.0, 1024.0, 800.0);
        let layout = a.compute_layout();
        assert_eq!(layout.resp_status.height(), RESPONSE_STATUS_H);
        assert_eq!(layout.resp_view.x1, layout.response.x1 - PAD);
        assert!(layout.resp_view.y0 < layout.resp_status.y1);
    }

    #[test]
    fn resource_send_reports_worker_completion_instead_of_queue_success() {
        let mut a = app();
        let invocation = qos_actions::InvocationId::new();
        a.resource_send_invocation = Some(invocation);
        a.tx.send(SendOutcome::Ok(http::sample_response())).unwrap();
        let doc = AutoCommit::new();
        assert!(matches!(
            a.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
            Some(qos_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert!(
            a.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                .is_none(),
            "terminal delivery is at most once"
        );
    }
}
