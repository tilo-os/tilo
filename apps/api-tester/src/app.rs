//! Root widget for the API tester.
//!
//! # Retained controls over painted chrome
//!
//! The app paints its panels, dividers, section labels, and response body, and
//! composes focusable child widgets for every control:
//!
//! - the HTTP method + environment pickers are [`Dropdown`]s (a11y `ComboBox`),
//! - the URL and body editors are [`TextInput`]s (a11y `TextInput`),
//! - Send is a primary [`Button`] (a11y `Button`),
//! - the request tabs and the response-view switch are [`ToggleGroup`]s
//!   (a11y `Group` of `ToggleButton`s),
//! - each sidebar / history row is a [`NavRow`](crate::navrow::NavRow) (a11y
//!   `Button`).
//!
//! [`build_layout`] registers every control in the Taffy tree for keyboard
//! navigation and AccessKit export. Their external layout rects align them with
//! the painted chrome.
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
use ui::widget::{AppModel, ModelPath};

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
use ui::richtext::{RichSpan, RichTextLayout, TextAlign};
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Button, Checkbox, CheckboxFace, Dropdown, EmptyState, SidebarDrawer, SplitLayout, TextInput,
    TextInputKind, ToggleGroup, Widget,
};

use crate::actions::{self, API_TESTER_ACTION_TYPE, ApiTesterAction};
use crate::doc::ApiDoc;
use crate::http::{self, Outgoing};
use crate::model::{
    Auth, BodyMode, Collection, Environment, HistoryEntry, HttpMethod, Request, RequestTab,
    ResponseData, ResponseView, human_duration,
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
/// On a phone, method/send stay on the first row and the URL gets a full
/// second row. Keeping all four controls in one row leaves only a clipped
/// sliver of the endpoint — especially unhelpful when it starts with a
/// `{{variable}}` reference.
const COMPACT_URLBAR_BREAKPOINT: f64 = 480.0;
const TAB_STRIP_H: f64 = 40.0;
const COMPACT_URLBAR_H: f64 = 100.0;
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

#[cfg(test)]
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
    Ok {
        response: ResponseData,
        method: HttpMethod,
        url: String,
    },
    Err(String),
}

/// A user action collected from a child widget's callback, applied after the
/// event has finished routing through the tree. Buttons and rows push these
/// from their `on_click`/`on_activate` closures; the toggle groups push from
/// `on_change`. Draining the sink in one place keeps the borrow of the child
/// widgets and the mutation of the model cleanly separated.
#[derive(Clone, Debug)]
enum Cmd {
    ToggleRail,
    ToggleManagement(usize),
    ResizeRequest(f64),
    ResetRequest,
    ScrollNav(i32),
    SetTab(usize),
    SetView(usize),
    ToggleCollection(usize),
    LoadRequest(usize, usize),
    ReplayHistory(usize),
    OpenForm(FormMode),
    SubmitForm,
    CancelForm,
}

/// The rail never leaves a generic text field lying around.  A field appears
/// only while the user is completing one named, contextual durable action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormMode {
    NewCollection,
    SaveRequest,
    RenameCollection,
    RenameRequest,
    DeleteRequest,
    NewEnvironment,
    RenameEnvironment,
    DeleteEnvironment,
    NewVariable,
    DeleteVariable,
}

impl FormMode {
    fn heading(self) -> &'static str {
        match self {
            Self::NewCollection => "New collection",
            Self::SaveRequest => "Save request",
            Self::RenameCollection => "Rename collection",
            Self::RenameRequest => "Rename request",
            Self::DeleteRequest => "Delete request?",
            Self::NewEnvironment => "New environment",
            Self::RenameEnvironment => "Rename environment",
            Self::DeleteEnvironment => "Delete environment?",
            Self::NewVariable => "Add variable",
            Self::DeleteVariable => "Delete variable",
        }
    }

    fn needs_variable_fields(self) -> bool {
        matches!(self, Self::NewVariable | Self::DeleteVariable)
    }

    fn needs_value(self) -> bool {
        matches!(self, Self::NewVariable)
    }

    fn needs_name(self) -> bool {
        !self.needs_variable_fields()
            && !matches!(self, Self::DeleteRequest | Self::DeleteEnvironment)
    }
}

type Sink = Rc<RefCell<Vec<Cmd>>>;

/// Structural signature of the child set. When it changes, the app reports
/// `take_structure_dirty` so the host rebuilds the taffy tree + focus order.
#[derive(Clone, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag describes an independent retained-child category"
)]
struct ChildSig {
    tab: u8,
    body_field: bool,
    resp_view: bool,
    hamburger: bool,
    sidebar: bool,
    nav_rows: usize,
    nav_start: usize,
    management: Option<usize>,
    form: Option<FormMode>,
    sidebar_items: Vec<SidebarItem>,
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
    rail_close: Rect,
    method: Rect,
    url: Rect,
    send: Rect,
    tabs: Rect,
    save: Rect,
    request_actions: Rect,
    env: Rect,
    form_name: Rect,
    form_key: Rect,
    form_value: Rect,
    form_actions: [Rect; 2],
    sidebar_items: Vec<(SidebarItem, Rect)>,
    nav_viewport: Rect,
    divider: Rect,
    body: Rect,
    resp_view: Rect,

    // chrome sub-regions used by draw + hit-tests
    urlbar: Rect,
    tab_strip: Rect,
    editor: Rect,
    response: Rect,
    resp_status: Rect,
    resp_body: Rect,
    form_heading: Point,
    collections_label: Point,
    body_region: Rect,
    fold_carets: Vec<(Rect, usize)>,
}

/// Shaped cells and relative row geometry are retained together: automatic
/// height and paint consume exactly the same wrapped content and column widths.
struct KvRowLayout {
    rect: Rect,
    key_rect: Rect,
    value_rect: Rect,
    key: RichTextLayout,
    value: RichTextLayout,
    enabled: bool,
    placeholder: bool,
    indicator: bool,
    /// Side of the enable checkbox: the 16px design size scaled with the
    /// text, bounded by the row so it never outgrows the text it toggles.
    indicator_size: f64,
}

impl KvRowLayout {
    fn checkbox(&self) -> Rect {
        let half = self.indicator_size / 2.0;
        Rect::new(
            PAD,
            self.rect.center().y - half,
            PAD + self.indicator_size,
            self.rect.center().y + half,
        )
    }
}

/// Design size of the KV row's enable checkbox at 1× text.
const KV_INDICATOR_SIZE: f64 = 16.0;
/// Gap between the enable checkbox and the key column.
const KV_INDICATOR_GAP: f64 = 14.0;

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
    management: Option<usize>,
    nav_start: usize,
    request_height_override: Option<f64>,
    request_drag: Option<(f64, f64)>,
    row_height: f64,
    kv_rows: Vec<KvRowLayout>,
    kv_height: Option<f64>,
    variable_color: [u8; 3],
    control_height: f64,
    /// Gap between stacked rail controls (environment picker, buttons, the
    /// action disclosures and their rows): one `s2` step for one relationship
    /// (HIG §7b), read from the theme where the rail used to mix 8 and 4 px.
    rail_row_gap: f64,
    /// Gap between the rows inside an expanded action group (`s1`): rows
    /// that read as one group under their disclosure sit closer than the
    /// stacked top-level controls do (HIG §7b), so an open group of five
    /// still fits a short window.
    rail_group_gap: f64,
    /// Rail top inset (`s6`, the page gutter) and the measured line boxes of
    /// the two rail captions ("ENV" at 9 px, "COLLECTIONS" at 10 px), so each
    /// caption sits one `s1` above what it labels instead of in a fixed band.
    rail_inset: f64,
    env_caption_h: f64,
    caption_h: f64,
    text_sm: f32,
    text_scale: f64,
    disclosures: Vec<NavRow>,
    divider: NavRow,
    /// Local scroll offset for the workspace rail. Essential when large text
    /// turns a previously short control list into more than one viewport.
    sidebar_scroll: f64,

    // --- Real child widgets ---
    method_dropdown: Dropdown,
    rail_button: Button,
    rail_close: Button,
    env_dropdown: Dropdown,
    environment_actions: Dropdown,
    url_input: TextInput,
    body_input: TextInput,
    send_button: Button,
    save_button: Button,
    request_actions: Dropdown,
    request_tabs: ToggleGroup,
    response_view_toggle: ToggleGroup,
    /// Shared accessible recovery surfaces for an empty response and a failed
    /// transport. They own their CTA focus/Enter/AT activation.
    response_empty: EmptyState,
    response_error: EmptyState,
    form_name_input: TextInput,
    form_key_input: TextInput,
    form_value_input: TextInput,
    form_submit: Button,
    form_cancel: Button,
    form: Option<FormMode>,
    manage_buttons: Vec<Button>,
    nav_rows: Vec<NavRow>,
    // Host maps may still inspect old focus between an event and build_layout.
    // Moving a Vec retains its backing allocation until that safe rebuild.
    retired_nav_rows: Vec<Vec<NavRow>>,
    nav_rows_replaced: bool,
    /// Parallel to `nav_rows`: what each row draws + does (rebuilt together).
    nav_meta: Vec<NavMeta>,

    /// Commands pushed by child callbacks, drained after event routing.
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

    // --- Adaptive chrome: the shared collapsible sidebar/drawer controller
    // (docked-vs-overlay decision, eased reveal, and dimming scrim). ---
    rail: SidebarDrawer,

    // --- Layout / interaction caches ---
    /// Last theme text scale, used to keep hand-positioned chrome and the
    /// retained controls' scaled glyphs in the same geometry system.
    layout_text_scale: f64,
    rect: Rect,
    layout: Layout,
    last_cursor: Point,
    seeded: bool,
    adopted: bool,
    /// Mounted on a record another participant created: its content is
    /// theirs to seed, so an empty document is left untouched.
    joined: bool,
    dirty: bool,
    /// Set when the child set changed; drained by `take_structure_dirty`.
    last_sig: ChildSig,
    persisted_once: bool,
}

/// Ordered identities used by layout, paint, event routing and focus traversal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SidebarItem {
    Environment,
    Manage(usize),
    Disclosure(usize),
    Nav(usize),
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
    rail_button: Button,
    rail_close: Button,
    env_dropdown: Dropdown,
    environment_actions: Dropdown,
    url_input: TextInput,
    body_input: TextInput,
    send_button: Button,
    save_button: Button,
    request_actions: Dropdown,
    request_tabs: ToggleGroup,
    response_view_toggle: ToggleGroup,
    sink: Sink,
    form_name_input: TextInput,
    form_key_input: TextInput,
    form_value_input: TextInput,
    form_submit: Button,
    form_cancel: Button,
    manage_buttons: Vec<Button>,
}

impl ApiTesterApp {
    #[allow(
        clippy::expect_used,
        reason = "the static active-request resource always publishes its nullary send operation"
    )]
    fn active_send_binding() -> tilo_actions::ActionBinding<()> {
        let resource = actions::active_request_resource();
        tilo_actions::ActionBinding::unit_for(&resource, "send")
            .expect("active request resource publishes a nullary send action")
    }

    #[allow(
        clippy::expect_used,
        reason = "the private menu resource constructors always export one typed open action"
    )]
    fn menu_binding(resource: &tilo_actions::Resource) -> tilo_actions::ActionBinding<String> {
        let spec = resource
            .actions
            .first()
            .expect("menu resources always publish their typed open action");
        tilo_actions::ActionBinding::new(resource, spec, |action| {
            serde_json::Map::from_iter([("action".to_string(), serde_json::Value::String(action))])
        })
        .expect("menu choices match their exported action schema")
    }

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

    /// Screenshot/test staging only: render the same failure surface a real
    /// transport error uses, without manufacturing a successful response.
    pub fn shot_error(&mut self) {
        self.response = None;
        self.sending = false;
        self.error = Some(
            "Could not reach api.example.com. Check the URL or your network connection.".into(),
        );
        self.dirty = true;
    }

    /// Screenshot/test staging for the native `TextInput` focus treatment.
    pub fn shot_focus_url(&mut self) {
        self.url_input.set_focus(true);
        self.dirty = true;
    }

    /// Screenshot staging for each contextual management form.  This keeps
    /// visual regression coverage honest without making the form state durable.
    pub fn shot_form(&mut self, name: &str) {
        let mode = match name {
            "new-collection" => Some(FormMode::NewCollection),
            "save-request" => Some(FormMode::SaveRequest),
            "rename-collection" => Some(FormMode::RenameCollection),
            "rename-request" => Some(FormMode::RenameRequest),
            "new-environment" => Some(FormMode::NewEnvironment),
            "rename-environment" => Some(FormMode::RenameEnvironment),
            "delete-environment" => Some(FormMode::DeleteEnvironment),
            "new-variable" => Some(FormMode::NewVariable),
            "delete-variable" => Some(FormMode::DeleteVariable),
            _ => None,
        };
        if let Some(mode) = mode {
            self.open_form(mode);
            self.dirty = true;
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "constructs every widget of the app in one declarative initializer"
    )]
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
            management: None,
            nav_start: 0,
            request_height_override: None,
            request_drag: None,
            row_height: ROW_H,
            kv_rows: Vec::new(),
            kv_height: None,
            variable_color: [0, 0, 0],
            control_height: 34.0,
            rail_row_gap: f64::from(ui::tokens::spacing::Spacing::BASE.s2),
            rail_group_gap: f64::from(ui::tokens::spacing::Spacing::BASE.s1),
            rail_inset: f64::from(ui::tokens::spacing::Spacing::BASE.s6),
            env_caption_h: 12.0,
            caption_h: 13.0,
            text_sm: 13.0,
            text_scale: 1.0,
            disclosures: [
                ("api.collection-actions", "Collection actions"),
                ("api.request-actions", "Request actions"),
                ("api.environment-actions", "Environment actions"),
            ]
            .iter()
            .enumerate()
            .map(|(i, (id, label))| {
                let sink = widgets.sink.clone();
                NavRow::new(*id, NavKind::Disclosure { expanded: false }, *label, *label)
                    .on_activate(move || sink.borrow_mut().push(Cmd::ToggleManagement(i)))
            })
            .collect(),
            divider: {
                let sink = widgets.sink.clone();
                NavRow::new(
                    "api.request-divider",
                    NavKind::Divider,
                    "",
                    "Request area divider",
                )
                .on_adjust(move |delta| sink.borrow_mut().push(Cmd::ResizeRequest(delta)))
            },
            sidebar_scroll: 0.0,
            method_dropdown: widgets.method_dropdown,
            rail_button: widgets.rail_button,
            rail_close: widgets.rail_close,
            env_dropdown: widgets.env_dropdown,
            environment_actions: widgets.environment_actions,
            url_input: widgets.url_input,
            body_input: widgets.body_input,
            send_button: widgets.send_button,
            save_button: widgets.save_button,
            request_actions: widgets.request_actions,
            request_tabs: widgets.request_tabs,
            response_view_toggle: widgets.response_view_toggle,
            response_empty: EmptyState::new("No response yet")
                .with_icon(Icon::Globe)
                .with_message("Enter a URL and send a request to inspect its response.")
                .with_action("Send request")
                .with_action_id("api.response.send")
                .on_action(Self::active_send_binding()),
            response_error: EmptyState::new("Request failed")
                .with_icon(Icon::Refresh)
                .with_message("Check the URL or network, then retry.")
                .with_action("Retry request")
                .with_action_id("api.response.retry")
                .on_action(Self::active_send_binding()),
            form_name_input: widgets.form_name_input,
            form_key_input: widgets.form_key_input,
            form_value_input: widgets.form_value_input,
            form_submit: widgets.form_submit,
            form_cancel: widgets.form_cancel,
            form: None,
            manage_buttons: widgets.manage_buttons,
            nav_rows: Vec::new(),
            retired_nav_rows: Vec::new(),
            nav_rows_replaced: false,
            nav_meta: Vec::new(),
            sink: widgets.sink,
            actions: crate::resource::ActionSink::default(),
            search_provider: crate::search_provider::ApiSearchProvider::new(),
            search_error: None,
            tx,
            rx,
            resource_send_invocation: None,
            pending_history: Vec::new(),
            resource_send_result: None,
            rail: SidebarDrawer::new()
                .left()
                .docked_width(SIDEBAR_W)
                .overlay_width(SIDEBAR_W)
                .breakpoint(RAIL_BREAKPOINT),
            layout_text_scale: 1.0,
            rect: Rect::ZERO,
            layout: Layout::default(),
            last_cursor: Point::ZERO,
            seeded: false,
            adopted: false,
            joined: ui::launch::record_launch() == Some(ui::launch::RecordLaunch::Joined),
            dirty: true,
            last_sig: ChildSig::default(),
            persisted_once: false,
        };
        app.rebuild_nav();
        app.refresh_search();
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
    #[allow(
        clippy::too_many_lines,
        reason = "one constructor keeps all retained controls and command bindings together"
    )]
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

        // A real retained menu button replaces the painted-only hamburger in
        // overlay mode, so a keyboard or screen-reader user can open the
        // workspace rail on a narrow display.
        let rail_sink = sink.clone();
        let rail_button = Button::new("api.workspace-menu", "", None)
            .with_icon(Icon::Menu)
            .with_accessible_name("Open workspace")
            .tooltip("Open workspace")
            .with_size(HAMBURGER_W as f32, 36.0)
            .on_effect(move || rail_sink.borrow_mut().push(Cmd::ToggleRail));
        let close_sink = sink.clone();
        let rail_close = Button::new("api.workspace-close", "Close", None)
            .with_icon(Icon::X)
            .with_accessible_name("Close workspace")
            .tooltip("Close workspace")
            .on_effect(move || close_sink.borrow_mut().push(Cmd::ToggleRail));

        // Environment picker — "No Environment" plus each named environment.
        let mut env_options = vec![NO_ENV.to_string()];
        env_options.extend(model.environments.iter().map(|e| e.name.clone()));
        let env_dropdown = Dropdown::new(env_options, ENV_PATH).with_id("api.env");
        let environment_action_state = ui::LocalState::new("Environment actions".to_string());
        let environment_actions = Dropdown::from_state(
            vec![
                "Environment actions".to_string(),
                "Rename environment…".to_string(),
                "Add variable…".to_string(),
                "Delete variable…".to_string(),
            ],
            &environment_action_state,
        )
        .with_id("api.environment-actions")
        .with_accessible_name("Environment actions")
        .on_action(Self::menu_binding(&actions::environment_menu_resource()));

        // Send — the accent primary action.
        let send_button = Button::new("api.send", "Send", None)
            .primary()
            .with_size(SEND_W as f32, 36.0)
            .on_action(Self::active_send_binding());
        let save_sink = sink.clone();
        let save_button = Button::new("api.save-request", "Save", None)
            .with_icon(Icon::Check)
            .with_accessible_name("Save request")
            .tooltip("Save current request")
            .on_effect(move || {
                save_sink
                    .borrow_mut()
                    .push(Cmd::OpenForm(FormMode::SaveRequest));
            });
        let request_action_state = ui::LocalState::new("Request actions".to_string());
        let request_actions = Dropdown::from_state(
            vec![
                "Request actions".to_string(),
                "Save request…".to_string(),
                "Rename request…".to_string(),
                "Delete request…".to_string(),
            ],
            &request_action_state,
        )
        .with_id("api.request-actions")
        .with_accessible_name("Request actions")
        .on_action(Self::menu_binding(&actions::request_menu_resource()));

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

        let form_name_input = TextInput::local()
            .with_placeholder("Name")
            .with_id("api.form-name")
            .with_accessible_name("Name");
        let form_key_input = TextInput::local()
            .with_placeholder("Key")
            .with_id("api.form-variable-key")
            .with_accessible_name("Variable key");
        let form_value_input = TextInput::local()
            .with_placeholder("Value")
            .with_id("api.form-variable-value")
            .with_accessible_name("Variable value");
        let submit_sink = sink.clone();
        let form_submit = Button::new("api.form-confirm", "Confirm", None)
            .primary()
            .on_effect(move || submit_sink.borrow_mut().push(Cmd::SubmitForm));
        let cancel_sink = sink.clone();
        let form_cancel = Button::new("api.form-cancel", "Cancel", None)
            .on_effect(move || cancel_sink.borrow_mut().push(Cmd::CancelForm));
        let management = [
            (
                "api.new-collection",
                "New collection",
                Cmd::OpenForm(FormMode::NewCollection),
            ),
            (
                "api.save-request",
                "Save request",
                Cmd::OpenForm(FormMode::SaveRequest),
            ),
        ];
        let management = management.into_iter().chain([
            (
                "api.rename-collection",
                "Rename collection",
                Cmd::OpenForm(FormMode::RenameCollection),
            ),
            (
                "api.rename-request",
                "Rename request",
                Cmd::OpenForm(FormMode::RenameRequest),
            ),
            (
                "api.delete-request",
                "Delete request",
                Cmd::OpenForm(FormMode::DeleteRequest),
            ),
            (
                "api.new-environment",
                "New environment",
                Cmd::OpenForm(FormMode::NewEnvironment),
            ),
            (
                "api.rename-environment",
                "Rename environment",
                Cmd::OpenForm(FormMode::RenameEnvironment),
            ),
            (
                "api.delete-environment",
                "Delete environment",
                Cmd::OpenForm(FormMode::DeleteEnvironment),
            ),
            (
                "api.new-variable",
                "Add variable",
                Cmd::OpenForm(FormMode::NewVariable),
            ),
            (
                "api.delete-variable",
                "Delete variable",
                Cmd::OpenForm(FormMode::DeleteVariable),
            ),
            (
                "api.increase-request-area",
                "Increase request area",
                Cmd::ResizeRequest(24.0),
            ),
            (
                "api.decrease-request-area",
                "Decrease request area",
                Cmd::ResizeRequest(-24.0),
            ),
            (
                "api.reset-request-area",
                "Reset request area",
                Cmd::ResetRequest,
            ),
            ("api.previous-saved", "↑ Earlier", Cmd::ScrollNav(-1)),
            ("api.next-saved", "↓ More", Cmd::ScrollNav(1)),
        ]);
        let manage_buttons = management
            .into_iter()
            .map(|(id, label, command)| {
                let sink = sink.clone();
                Button::new(id, label, None)
                    .with_accessible_name(label)
                    .tooltip(label)
                    .on_effect(move || sink.borrow_mut().push(command.clone()))
            })
            .collect();

        BuiltWidgets {
            method_dropdown,
            rail_button,
            rail_close,
            env_dropdown,
            environment_actions,
            url_input,
            body_input,
            send_button,
            save_button,
            request_actions,
            request_tabs,
            response_view_toggle,
            sink,
            form_name_input,
            form_key_input,
            form_value_input,
            form_submit,
            form_cancel,
            manage_buttons,
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

    /// Kick off a real request on the app runtime.
    fn send(&mut self) {
        if self.sending {
            return;
        }
        let out = self.outgoing();
        let method = self.request.method;
        self.sending = true;
        self.error = None;
        self.dirty = true;
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
                    self.response_view_toggle.set_selected(0);
                    self.collapsed.clear();
                    self.body_scroll = 0.0;
                    self.rebuild_nav();
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

    /// Seed a previously empty document through the native per-resource
    /// writers. Normal interactions use precise actions and never call this.
    fn seed_workspace(&self, state: &mut AutoCommit, root: &ObjId) {
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
        for (id, name) in &state.collections {
            let Ok(id) = id.parse::<crate::model::Id>() else {
                continue;
            };
            if let Some(collection) = self
                .collections
                .iter_mut()
                .find(|collection| collection.id == id)
            {
                collection.name = name.clone();
            }
        }
        let mut renamed_env = false;
        for (id, name) in &state.environments {
            let Ok(id) = id.parse::<crate::model::Id>() else {
                continue;
            };
            if let Some(environment) = self
                .environments
                .iter_mut()
                .find(|environment| environment.id == id)
            {
                if environment.name != *name {
                    environment.name = name.clone();
                    renamed_env = true;
                }
            }
        }
        if renamed_env {
            self.rebuild_env_dropdown();
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
        self.refresh_search();
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
            hamburger: self.rail.layout_for(self.rect.width()) == SplitLayout::Overlay,
            sidebar: self.sidebar_visible(),
            nav_rows: self.nav_rows.len(),
            nav_start: self.nav_start,
            management: self.management,
            form: self.form,
            sidebar_items: self
                .layout
                .sidebar_items
                .iter()
                .map(|(item, _)| *item)
                .collect(),
        }
    }

    // -------------------------------------------------------------------
    // Nav rows
    // -------------------------------------------------------------------

    /// Rebuild the ordered `nav_meta` list (collections → requests → history)
    /// from the model, then rebuild the `NavRow` widgets to match, each wired
    /// to push its command onto the sink.
    fn rebuild_nav(&mut self) {
        self.layout
            .sidebar_items
            .retain(|(item, _)| !matches!(item, SidebarItem::Nav(_)));
        self.nav_meta = self.compute_nav_meta();
        let rows = self
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
        let previous = std::mem::replace(&mut self.nav_rows, rows);
        if !previous.is_empty() {
            self.retired_nav_rows.push(previous);
        }
        // A same-count replacement still invalidates the host's raw pointers.
        self.nav_rows_replaced = true;
    }

    /// The ordered sidebar rows derived from the model. Kept separate from
    /// widget construction so both `rebuild_nav` and (if needed) tests can
    /// reason about the row set.
    fn compute_nav_meta(&self) -> Vec<NavMeta> {
        let mut v = Vec::new();
        for (ci, col) in self.collections.iter().enumerate() {
            let expanded = !self.collapsed.contains(&ci);
            v.push(NavMeta {
                kind: NavKind::Collection {
                    expanded,
                    count: col.requests.len(),
                },
                label: col.name.clone(),
                a11y_name: format!("{}, collection, {} requests", col.name, col.requests.len()),
                cmd: Cmd::ToggleCollection(ci),
            });
            if expanded {
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
            &self.request_actions,
        ];
        if self.rect.width() > 600.0 {
            v.insert(3, &self.save_button);
        }
        if self.rail.layout_for(self.rect.width()) == SplitLayout::Overlay {
            v.insert(0, &self.rail_button);
            if self.sidebar_visible() {
                v.insert(1, &self.rail_close);
            }
        }
        if self.body_field_visible() {
            v.push(&self.body_input);
        }
        v.push(&self.divider);
        if self.response.is_some() {
            v.push(&self.response_view_toggle);
        } else if self.error.is_some() {
            v.push(&self.response_error);
        } else {
            v.push(&self.response_empty);
        }
        if self.sidebar_visible() {
            if let Some(form) = self.form {
                if form.needs_name() {
                    v.push(&self.form_name_input);
                }
                if form.needs_variable_fields() {
                    v.push(&self.form_key_input);
                    if form.needs_value() {
                        v.push(&self.form_value_input);
                    }
                }
                v.push(&self.form_cancel);
                v.push(&self.form_submit);
            }
            #[expect(
                clippy::indexing_slicing,
                reason = "every SidebarItem index is produced by layout from these widget vectors, so it is in bounds"
            )]
            for (item, _) in &self.layout.sidebar_items {
                v.push(match *item {
                    SidebarItem::Environment => &self.env_dropdown,
                    SidebarItem::Manage(i) => &self.manage_buttons[i],
                    SidebarItem::Disclosure(i) => &self.disclosures[i],
                    SidebarItem::Nav(i) => &self.nav_rows[i],
                });
            }
        }
        v
    }

    fn children_mut_ref(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = Vec::new();
        let body = self.body_field_visible();
        let resp = self.response.is_some();
        let sidebar = self.sidebar_visible();
        let hamburger = self.rail.layout_for(self.rect.width()) == SplitLayout::Overlay;
        if hamburger {
            v.push(&mut self.rail_button);
            if sidebar {
                v.push(&mut self.rail_close);
            }
        }
        v.push(&mut self.method_dropdown);
        v.push(&mut self.url_input);
        v.push(&mut self.send_button);
        if self.rect.width() > 600.0 {
            v.push(&mut self.save_button);
        }
        v.push(&mut self.request_tabs);
        v.push(&mut self.request_actions);
        if body {
            v.push(&mut self.body_input);
        }
        v.push(&mut self.divider);
        if resp {
            v.push(&mut self.response_view_toggle);
        } else if self.error.is_some() {
            v.push(&mut self.response_error);
        } else {
            v.push(&mut self.response_empty);
        }
        if sidebar {
            if let Some(form) = self.form {
                if form.needs_name() {
                    v.push(&mut self.form_name_input);
                }
                if form.needs_variable_fields() {
                    v.push(&mut self.form_key_input);
                    if form.needs_value() {
                        v.push(&mut self.form_value_input);
                    }
                }
                v.push(&mut self.form_cancel);
                v.push(&mut self.form_submit);
            }
            // Gather unique references first, then consume by explicit identity.
            // Filtering never changes the association between a rect and action.
            let mut items: HashMap<SidebarItem, &mut dyn Widget> = HashMap::new();
            items.insert(SidebarItem::Environment, &mut self.env_dropdown);
            for (i, button) in self.manage_buttons.iter_mut().enumerate() {
                items.insert(SidebarItem::Manage(i), button);
            }
            for (i, row) in self.disclosures.iter_mut().enumerate() {
                items.insert(SidebarItem::Disclosure(i), row);
            }
            for (i, row) in self.nav_rows.iter_mut().enumerate() {
                items.insert(SidebarItem::Nav(i), row);
            }
            for (item, _) in &self.layout.sidebar_items {
                if let Some(widget) = items.remove(item) {
                    v.push(widget);
                }
            }
        }
        v
    }

    /// Position every live child widget from the computed layout, in the same
    /// order `children_mut_ref` / `build_layout` use.
    fn position_children(&mut self) {
        let l = self.layout.clone();
        self.divider.apply_layout_external(l.divider);
        // Hidden rows and closed actions must have no stale hit rectangles.
        self.env_dropdown.apply_layout_external(Rect::ZERO);
        for (i, button) in self.manage_buttons.iter_mut().enumerate() {
            if !l
                .sidebar_items
                .iter()
                .any(|(item, _)| *item == SidebarItem::Manage(i))
            {
                button.apply_layout_external(Rect::ZERO);
            }
        }
        for row in self.nav_rows.iter_mut().chain(self.disclosures.iter_mut()) {
            row.apply_layout_external(Rect::ZERO);
        }
        #[expect(
            clippy::indexing_slicing,
            reason = "every SidebarItem index is produced by layout from these widget vectors, so it is in bounds"
        )]
        for (item, rect) in &l.sidebar_items {
            match *item {
                SidebarItem::Environment => self.env_dropdown.apply_layout_external(*rect),
                SidebarItem::Manage(i) => self.manage_buttons[i].apply_layout_external(*rect),
                SidebarItem::Disclosure(i) => self.disclosures[i].apply_layout_external(*rect),
                SidebarItem::Nav(i) => self.nav_rows[i].apply_layout_external(*rect),
            }
        }
        if self.rail.layout_for(self.rect.width()) == SplitLayout::Overlay {
            self.rail_button.apply_layout_external(l.hamburger);
            if self.sidebar_visible() {
                self.rail_close.apply_layout_external(l.rail_close);
            }
        }
        self.method_dropdown.apply_layout_external(l.method);
        self.url_input.apply_layout_external(l.url);
        self.send_button.apply_layout_external(l.send);
        if self.rect.width() > 600.0 {
            self.save_button.apply_layout_external(l.save);
        }
        self.request_tabs.apply_layout_external(l.tabs);
        self.request_actions
            .apply_layout_external(l.request_actions);
        if self.body_field_visible() {
            self.body_input.apply_layout_external(l.body);
        }
        if self.response.is_some() {
            self.response_view_toggle.apply_layout_external(l.resp_view);
        } else if self.error.is_some() {
            self.response_error.apply_layout_external(l.resp_body);
        } else {
            self.response_empty.apply_layout_external(l.resp_body);
        }
        if self.sidebar_visible()
            && let Some(form) = self.form
        {
            if form.needs_name() {
                self.form_name_input.apply_layout_external(l.form_name);
            }
            if form.needs_variable_fields() {
                self.form_key_input.apply_layout_external(l.form_key);
                if form.needs_value() {
                    self.form_value_input.apply_layout_external(l.form_value);
                }
            }
            self.form_cancel.apply_layout_external(l.form_actions[0]);
            self.form_submit.apply_layout_external(l.form_actions[1]);
        }
    }

    fn response_chrome_height(&self, width: f64) -> f64 {
        response_status_height(width / self.text_scale)
            + (self.control_height - 34.0).max(0.0)
                * if width < COMPACT_RESPONSE_BREAKPOINT * self.text_scale {
                    2.0
                } else {
                    1.0
                }
    }

    /// Left edge of the KV key column: the row gutter, the text-scaled enable
    /// checkbox and a gap. Measurement, the `KEY` heading and the rows all
    /// read this one value, so a 200% checkbox never overprints its key.
    fn kv_key_x(&self) -> f64 {
        PAD + KV_INDICATOR_SIZE * self.text_scale + KV_INDICATOR_GAP
    }

    fn automatic_request_height(&self) -> f64 {
        // The provisional pre-measure pass only needs a nonzero editor.
        // Settled layout and paint both consume the retained shaped rows.
        let table_height = self.kv_height.unwrap_or(self.row_height + 42.0);
        match self.active_tab {
            RequestTab::Params | RequestTab::Headers => table_height,
            RequestTab::Body => match self.request.body_mode {
                BodyMode::Form => self.row_height + 16.0 + table_height,
                BodyMode::None => 24.0 + self.row_height * 2.0,
                BodyMode::Json | BodyMode::Raw => 36.0 + self.row_height * 6.0,
            },
            RequestTab::Auth => {
                36.0 + 20.0 * self.text_scale
                    + self.row_height
                    + match self.request.auth {
                        Auth::None => self.row_height,
                        Auth::Bearer { .. } => self.control_height + 16.0 * self.text_scale + 12.0,
                        _ => (self.control_height + 36.0 * self.text_scale) * 2.0,
                    }
            }
        }
    }

    // -------------------------------------------------------------------
    // Layout
    // -------------------------------------------------------------------

    /// Compute all chrome regions + control rects for the current window rect
    /// and rail state. Pure geometry; called from both `apply_layout` and
    /// `draw`.
    #[allow(
        clippy::too_many_lines,
        reason = "one pure geometry pass keeps the hand-positioned controls and painted chrome in lockstep"
    )]
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
        let compact_urlbar = split == SplitLayout::Overlay
            && area.width() < COMPACT_URLBAR_BREAKPOINT * self.text_scale;
        let control_h = self.control_height.max(36.0);
        let row_h = URLBAR_H + control_h - 36.0;
        let urlbar_h = if compact_urlbar {
            COMPACT_URLBAR_H + 2.0 * (control_h - 36.0)
        } else {
            row_h
        };
        l.urlbar = Rect::new(area.x0, area.y0, area.x1, area.y0 + urlbar_h);
        let cy0 = l.urlbar.y0 + 10.0;
        let mut row_x0 = area.x0 + PAD;
        if split == SplitLayout::Overlay {
            // Square toggle: as wide as the row's controls are tall, so it
            // does not become a narrow slab beside a taller picker at 200%.
            let side = HAMBURGER_W.max(control_h);
            l.hamburger = Rect::new(row_x0, cy0, row_x0 + side, cy0 + control_h);
            row_x0 = l.hamburger.x1 + 8.0;
        }
        l.method = Rect::new(
            row_x0,
            cy0,
            row_x0 + METHOD_W.max(72.0 * self.text_scale),
            cy0 + control_h,
        );
        l.send = Rect::new(
            area.x1 - PAD - SEND_W.max(64.0 * self.text_scale),
            cy0,
            area.x1 - PAD,
            cy0 + control_h,
        );
        l.url = if compact_urlbar {
            Rect::new(
                area.x0 + PAD,
                l.urlbar.y0 + row_h,
                area.x1 - PAD,
                l.urlbar.y0 + row_h + control_h,
            )
        } else {
            Rect::new(l.method.x1 + 8.0, cy0, l.send.x0 - 8.0, cy0 + control_h)
        };

        // --- Tab strip ---
        let compact_request_actions = area.width() <= 600.0;
        let tab_strip_h = if compact_request_actions {
            TAB_STRIP_H.max(control_h + 8.0) + control_h + 8.0
        } else {
            TAB_STRIP_H.max(control_h + 8.0)
        };
        let tab_y0 = l.urlbar.y1 + 8.0;
        l.tab_strip = Rect::new(area.x0, tab_y0, area.x1, tab_y0 + tab_strip_h);
        let tabs_w =
            (96.0 * self.text_scale * RequestTab::ALL.len() as f64).min(area.width() - PAD * 2.0);
        let tabs_y1 = if compact_request_actions {
            l.tab_strip.y0 + TAB_STRIP_H.max(control_h + 8.0) - 4.0
        } else {
            l.tab_strip.y1 - 4.0
        };
        l.tabs = Rect::new(
            area.x0 + PAD,
            l.tab_strip.y0 + 4.0,
            area.x0 + PAD + tabs_w,
            tabs_y1,
        );
        l.save = Rect::new(
            l.tabs.x1 + 8.0,
            l.tabs.y0,
            l.tabs.x1 + 8.0 + 84.0,
            l.tabs.y1,
        );
        l.request_actions = if compact_request_actions {
            Rect::new(
                area.x0 + PAD,
                l.tabs.y1 + 4.0,
                area.x1 - PAD,
                l.tab_strip.y1 - 4.0,
            )
        } else {
            Rect::new(l.save.x1 + 8.0, l.tabs.y0, l.save.x1 + 164.0, l.tabs.y1)
        };

        // --- Editor / response split ---
        let available = (area.y1 - l.tab_strip.y1 - 10.0).max(0.0);
        let response_min = self.response_chrome_height(area.width()) + self.row_height * 2.0;
        let maximum = (available - response_min).max(0.0);
        let minimum = (self.row_height + 36.0).min(maximum);
        let editor_h = self
            .request_height_override
            .unwrap_or_else(|| self.automatic_request_height())
            .clamp(minimum, maximum);
        l.editor = Rect::new(area.x0, l.tab_strip.y1, area.x1, l.tab_strip.y1 + editor_h);
        l.divider = Rect::new(area.x0, l.editor.y1, area.x1, l.editor.y1 + 10.0);
        l.response = Rect::new(area.x0, l.divider.y1, area.x1, area.y1);

        // Body field (Body tab, JSON/Raw).
        if self.body_field_visible() {
            let e = l.editor;
            l.body = Rect::new(
                e.x0 + PAD,
                (e.y0 + 24.0 + self.row_height).min(e.y1),
                e.x1 - PAD,
                (e.y1 - 12.0).max((e.y0 + 24.0 + self.row_height).min(e.y1)),
            );
        }

        // Response chrome + view switch.
        let status_h = self.response_chrome_height(l.response.width());
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
            if l.response.width() < COMPACT_RESPONSE_BREAKPOINT * self.text_scale {
                // Keep the switcher a full-width, easy-to-hit control below
                // the status line. Equal segments remain legible at large
                // text sizes and the response body starts below both rows.
                l.resp_view = Rect::new(
                    l.response.x0 + PAD,
                    l.resp_status.y1 - (self.control_height - 10.0) - 6.0,
                    l.response.x1 - PAD,
                    l.resp_status.y1 - 6.0,
                );
            } else {
                let view_w = 210.0 * self.text_scale;
                let cy = l.resp_status.center().y;
                l.resp_view = Rect::new(
                    l.response.x1 - PAD - view_w,
                    cy - (self.control_height - 10.0) / 2.0,
                    l.response.x1 - PAD,
                    cy + (self.control_height - 10.0) / 2.0,
                );
            }
        }

        // --- Sidebar rows ---
        if self.rail.reveal() > 0.001 && l.sidebar.width() > 1.0 {
            let s = l.sidebar;
            let rail_gap = self.rail_row_gap;
            let group_gap = self.rail_group_gap;
            // No app-name wordmark (the OS titlebar shows "API Tester"); the ENV
            // picker leads the sidebar. Keep room above it for the ENV caption.
            // The rail opens with its `s6` gutter, grown only when the ENV
            // caption plus its `s1` gap to the picker needs more.
            let mut y = s.y0 + self.rail_inset.max(self.env_caption_h + 2.0 * group_gap);
            let control_x = s.x0 + PAD;
            let scale = self.text_scale.max(1.0);
            let close_w = 88.0 * scale;
            let close_h = 36.0 * scale;
            l.rail_close = Rect::new(
                s.x1 - PAD - close_w,
                s.y0 + PAD,
                s.x1 - PAD,
                s.y0 + PAD + close_h,
            );
            let control_w = s.width() - PAD * 2.0;
            if let Some(form) = self.form {
                l.form_heading = Point::new(control_x, y);
                y += (20.0 * self.text_scale).max(20.0);
                if form.needs_name() {
                    l.form_name = Rect::new(control_x, y, s.x1 - PAD, y + self.control_height);
                    y += self.control_height + 10.0;
                }
                if form.needs_variable_fields() {
                    l.form_key = Rect::new(control_x, y, s.x1 - PAD, y + self.control_height);
                    y += self.control_height + 8.0;
                    if form.needs_value() {
                        l.form_value = Rect::new(control_x, y, s.x1 - PAD, y + self.control_height);
                        y += self.control_height + 10.0;
                    }
                }
                let half = (control_w - 8.0) / 2.0;
                l.form_actions = [
                    Rect::new(control_x, y, control_x + half, y + self.control_height),
                    Rect::new(
                        control_x + half + 8.0,
                        y,
                        s.x1 - PAD,
                        y + self.control_height,
                    ),
                ];
                y += self.control_height + 16.0;
            } else {
                l.env = Rect::new(control_x, y, s.x1 - PAD, y + self.control_height);
                l.sidebar_items.push((SidebarItem::Environment, l.env));
                y = l.env.y1 + rail_gap;
            }
            let h = self.control_height;
            let row = self.row_height;
            let footer_actions: &[usize] = match self.management {
                Some(0) => &[2],
                Some(1) => &[3, 4, 10, 11, 12],
                Some(2) => &[5, 6, 7, 8, 9],
                _ => &[],
            };
            let footer_h = if self.form.is_none() {
                4.0 * (h + rail_gap) + footer_actions.len() as f64 * (h + group_gap) + rail_gap
            } else {
                0.0
            };
            let mut add = |item, at: f64, height: f64| {
                let rect = Rect::new(control_x, at, s.x1 - PAD, at + height);
                if rect.y0 >= s.y0 && rect.y1 <= s.y1 - 4.0 {
                    l.sidebar_items.push((item, rect));
                }
            };
            if self.form.is_none() {
                add(SidebarItem::Manage(0), y, h);
                y += h + rail_gap;
            }
            // The "COLLECTIONS" caption only labels a list that has room for
            // at least one row above the footer; a height-starved rail (an
            // open action group in a short window) hands that band to the
            // footer instead of reserving it for nothing (HIG §7a: empty
            // docks do not reserve area).
            let label_h = self.caption_h + group_gap;
            let footer_top = s.y1 - PAD - footer_h;
            if footer_top - y >= label_h + row {
                l.collections_label = Point::new(control_x, y);
                y += label_h;
            }
            let bottom = footer_top.max(y);
            let paging = (self.nav_meta.len() as f64 * row) > bottom - y;
            let nav_bottom = if paging {
                (bottom - h - 4.0).max(y)
            } else {
                bottom
            };
            l.nav_viewport = Rect::new(s.x0, y, s.x1, nav_bottom);
            let capacity = ((nav_bottom - y) / row).floor().max(0.0) as usize;
            self.nav_start = self
                .nav_start
                .min(self.nav_meta.len().saturating_sub(capacity.max(1)));
            for i in self.nav_start..(self.nav_start + capacity).min(self.nav_meta.len()) {
                add(SidebarItem::Nav(i), y, row);
                y += row;
            }
            if paging && bottom - l.nav_viewport.y0 >= h {
                // Both directions stay available to keyboard users, including
                // when a disclosure temporarily reduces the list viewport.
                let half = (s.width() - PAD * 2.0 - rail_gap) / 2.0;
                l.sidebar_items.push((
                    SidebarItem::Manage(13),
                    Rect::new(control_x, bottom - h, control_x + half, bottom),
                ));
                l.sidebar_items.push((
                    SidebarItem::Manage(14),
                    Rect::new(control_x + half + rail_gap, bottom - h, s.x1 - PAD, bottom),
                ));
            }
            if self.form.is_none() {
                y = if paging {
                    bottom + rail_gap
                } else {
                    y + rail_gap
                };
                l.sidebar_items.push((
                    SidebarItem::Manage(1),
                    Rect::new(control_x, y, s.x1 - PAD, y + h),
                ));
                y += h + rail_gap;
                for group in 0..3 {
                    l.sidebar_items.push((
                        SidebarItem::Disclosure(group),
                        Rect::new(control_x, y, s.x1 - PAD, y + h),
                    ));
                    y += h + rail_gap;
                    if self.management == Some(group) {
                        for &i in footer_actions {
                            l.sidebar_items.push((
                                SidebarItem::Manage(i),
                                Rect::new(control_x, y, s.x1 - PAD, y + h),
                            ));
                            y += h + group_gap;
                        }
                        // The next top-level control sits a full step below
                        // the group, not a group step.
                        y += rail_gap - group_gap;
                    }
                }
            }
        }

        l
    }

    /// Keep the hand-positioned app chrome in step with the toolkit's text
    /// scale. At large accessibility sizes a narrow overlay becomes a
    /// full-width workspace rather than letting the underlying editor show
    /// through beside clipped drawer controls.
    fn sync_adaptive_metrics(&mut self, theme: &Theme) {
        let scale = f64::from(theme.text_scale).clamp(1.0, 2.0);
        self.layout_text_scale = scale;
        self.rail.set_docked_width(SIDEBAR_W * scale);
        self.rail
            .set_overlay_width((SIDEBAR_W * scale).min(self.rect.width()));
        self.rail.set_breakpoint(RAIL_BREAKPOINT * scale);
    }

    /// Keep keyboard / screen-reader focus visible as it traverses a rail
    /// longer than the viewport. The offset is view-local and bounded, so it
    /// never becomes collaborative document churn.
    fn follow_rail_focus(&mut self) -> bool {
        let sidebar = self.layout.sidebar;
        let Some(row) = self.nav_rows.iter().find(|row| row.is_focused()) else {
            return false;
        };
        let top = sidebar.y0 + 8.0;
        let bottom = sidebar.y1 - 8.0;
        let next = if row.get_rect().y0 < top {
            self.sidebar_scroll - (top - row.get_rect().y0)
        } else if row.get_rect().y1 > bottom {
            self.sidebar_scroll + (row.get_rect().y1 - bottom)
        } else {
            return false;
        };
        self.sidebar_scroll = next.clamp(0.0, 1200.0);
        true
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
        // The rail's edge is derived against the rail itself (HIG §9.5).
        let div = Rect::new(area.x1 - 1.0, area.y0, area.x1, area.y1);
        scene.fill(
            Fill::NonZero,
            t,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &div,
        );

        // ENV label above the picker.
        if self.form.is_none() {
            tr.draw_text_weighted(
                scene,
                "ENV",
                9.0,
                c.text_dim,
                t * Affine::translate((
                    l.env.x0,
                    l.env.y0 - (self.env_caption_h + self.rail_group_gap),
                )),
                None,
                TextWeight::Bold,
            );
        }
        if self.rail.layout() == SplitLayout::Overlay {
            self.rail_close.draw(scene, t, tr, theme, presence);
        }
        Self::draw_section_label(scene, t, tr, theme, l.collections_label, "COLLECTIONS");
        if let Some(form) = self.form {
            Self::draw_section_label(scene, t, tr, theme, l.form_heading, form.heading());
        }

        // The environment plus visible management controls are all retained
        // widgets, so their labels/tooltips/focus state are exposed to AT.
        if let Some(form) = self.form {
            if form.needs_name() {
                self.form_name_input.draw(scene, t, tr, theme, presence);
            }
            if form.needs_variable_fields() {
                self.form_key_input.draw(scene, t, tr, theme, presence);
                if form.needs_value() {
                    self.form_value_input.draw(scene, t, tr, theme, presence);
                }
            }
            self.form_cancel.draw(scene, t, tr, theme, presence);
            self.form_submit.draw(scene, t, tr, theme, presence);
        }
        #[expect(
            clippy::indexing_slicing,
            reason = "every SidebarItem index is produced by layout from these widget vectors, so it is in bounds"
        )]
        for (item, _) in &l.sidebar_items {
            match *item {
                SidebarItem::Environment => self.env_dropdown.draw(scene, t, tr, theme, presence),
                SidebarItem::Manage(i) => {
                    self.manage_buttons[i].draw(scene, t, tr, theme, presence);
                }
                SidebarItem::Disclosure(i) => {
                    self.disclosures[i].draw(scene, t, tr, theme, presence);
                }
                SidebarItem::Nav(i) => self.nav_rows[i].draw(scene, t, tr, theme, presence),
            }
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
        scene.fill(
            Fill::NonZero,
            t,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &bar_div,
        );

        if self.rail.layout() == SplitLayout::Overlay {
            self.rail_button.draw(scene, t, tr, theme, presence);
        }

        // Method picker draws itself; keep the method-tinted rest look via the
        // dropdown's own themed chrome.
        self.method_dropdown.draw(scene, t, tr, theme, presence);

        // URL field box + the plain TextInput inside it.
        // Rest: a base-toned field with its derived edge; focus: the ring.
        if self.url_input.is_focused() {
            let url_shape = RoundedRect::from_rect(l.url, f64::from(theme.tokens.radii.md));
            scene.fill(Fill::NonZero, t, c.surfaces.base, None, &url_shape);
            scene.stroke(&Stroke::new(2.0), t, theme.border_focus, None, &url_shape);
        } else {
            ui::surface::card(
                scene,
                t,
                l.url,
                f64::from(theme.tokens.radii.md),
                c.surfaces.base,
            );
        }
        self.url_input.draw(scene, t, tr, theme, presence);

        // Send button draws itself.
        self.send_button.draw(scene, t, tr, theme, presence);

        // Tab strip background + the ToggleGroup + the var hint.
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &l.tab_strip);
        self.request_tabs.draw(scene, t, tr, theme, presence);
        // Paint Save after the strip surface: its geometry deliberately lives
        // beside the tabs, and drawing it before this fill made it invisible.
        if self.rect.width() > 600.0 {
            self.save_button.draw(scene, t, tr, theme, presence);
        }
        self.request_actions.draw(scene, t, tr, theme, presence);
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
        let size = tr
            .renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
            .size;
        let (hw, hh) = tr.measure(&hint, size, None);
        if !tab_strip_has_room_for_hint(l, f64::from(hw)) {
            return;
        }
        // Centre on the measured line height, not a font-size guess.
        tr.draw_text(
            scene,
            &hint,
            size,
            hint_col,
            t * Affine::translate((
                l.main.x1 - PAD - f64::from(hw),
                l.tab_strip.center().y - f64::from(hh) / 2.0,
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
        self.divider.draw(scene, t, tr, theme, presence);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &area,
        );
        match self.active_tab {
            RequestTab::Params | RequestTab::Headers => {
                self.draw_kv_table(scene, t, tr, theme, area);
            }
            RequestTab::Auth => self.draw_auth(scene, t, tr, theme, area),
            RequestTab::Body => self.draw_body(scene, t, tr, theme, presence, l),
        }
        scene.pop_layer();
    }

    fn measure_kv_table(&mut self, tr: &mut TextRenderer, width: f64) {
        let (rows, empty_msg) = match self.active_tab {
            RequestTab::Params => (&self.request.params, "No query parameters"),
            RequestTab::Headers => (&self.request.headers, "No headers"),
            RequestTab::Body if self.request.body_mode == BodyMode::Form => {
                (&self.request.form, "No form fields")
            }
            _ => {
                self.kv_rows.clear();
                self.kv_height = None;
                return;
            }
        };
        let mut cells: Vec<_> = rows
            .iter()
            .filter(|row| !row.is_blank())
            .map(|row| {
                (
                    row.key.as_str(),
                    row.value.as_str(),
                    row.enabled,
                    false,
                    true,
                )
            })
            .collect();
        if cells.is_empty() {
            cells.push((empty_msg, "", true, false, false));
        } else {
            cells.push(("New key", "New value", false, true, true));
        }
        // RichTextLayout accepts resolved metrics directly; draw_rich paints
        // the same shaped glyphs without another scale or wrapping pass.
        let style = TextStyle::new(self.text_sm);
        let key_x = self.kv_key_x();
        let value_x = width * 0.5;
        let key_width = (value_x - key_x - 12.0).max(1.0) as f32;
        let value_width = (width - PAD - value_x).max(1.0) as f32;
        let header_height =
            f64::from(tr.measure_weighted("KEY", 10.0, None, TextWeight::Bold).1).ceil();
        let mut y = 12.0 + header_height + 9.0;
        self.kv_rows.clear();
        for (key, value, enabled, placeholder, indicator) in cells {
            let key = RichTextLayout::shape_styled(
                &mut tr.font_system,
                &[RichSpan::new(key)],
                &style,
                Some(if indicator {
                    key_width
                } else {
                    (width - PAD * 2.0).max(1.0) as f32
                }),
                TextAlign::Left,
                vec![],
                vec![],
            );
            let spans = Self::value_spans(value, self.variable_color);
            let value = RichTextLayout::shape_styled(
                &mut tr.font_system,
                &spans,
                &style,
                Some(value_width),
                TextAlign::Left,
                vec![],
                vec![],
            );
            let key_h = f64::from(key.height());
            let value_h = f64::from(value.height());
            let height = (key_h.max(value_h).ceil() + 12.0).max(ROW_H);
            let rect = Rect::new(PAD, y, width - PAD, y + height);
            let kx = if indicator { key_x } else { PAD };
            let indicator_size = (KV_INDICATOR_SIZE * self.text_scale).min(height - 4.0);
            self.kv_rows.push(KvRowLayout {
                rect,
                key_rect: Rect::new(
                    kx,
                    rect.center().y - key_h / 2.0,
                    if indicator {
                        key_x + f64::from(key_width)
                    } else {
                        width - PAD
                    },
                    rect.center().y + key_h / 2.0,
                ),
                value_rect: Rect::new(
                    value_x,
                    rect.center().y - value_h / 2.0,
                    value_x + f64::from(value_width),
                    rect.center().y + value_h / 2.0,
                ),
                key,
                value,
                enabled,
                placeholder,
                indicator,
                indicator_size,
            });
            y += height;
        }
        self.kv_height = Some(y + 8.0);
    }

    fn value_spans(value: &str, accent: [u8; 3]) -> Vec<RichSpan> {
        let mut spans = Vec::new();
        let mut rest = value;
        while let Some((before, after_open)) = rest.split_once("{{") {
            let Some((name, after)) = after_open.split_once("}}") else {
                break;
            };
            spans.push(RichSpan::new(before));
            spans.push(
                RichSpan::new(format!("{{{{{name}}}}}"))
                    .bold(true)
                    .color(Some(accent)),
            );
            rest = after;
        }
        spans.push(RichSpan::new(rest));
        spans
    }

    fn draw_kv_table(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        let t = t * Affine::translate((area.x0, area.y0));
        for (label, x) in [("KEY", self.kv_key_x()), ("VALUE", area.width() * 0.5)] {
            tr.draw_text_weighted(
                scene,
                label,
                10.0,
                c.text_dim,
                t * Affine::translate((x, 12.0)),
                None,
                TextWeight::Bold,
            );
        }
        for row in &self.kv_rows {
            if row.rect.y1 > area.height() {
                break;
            }
            Self::draw_kv_row(scene, t, tr, theme, row);
        }
    }

    fn draw_kv_row(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: &KvRowLayout,
    ) {
        let c = &theme.tokens.colors;
        // Clip ink overhang too; text geometry already fits these bounds.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &row.rect,
        );
        if row.indicator {
            if row.placeholder {
                scene.stroke(
                    &Stroke::new(1.0),
                    t,
                    ui::surface::edge_on(c.surfaces.base),
                    None,
                    &RoundedRect::from_rect(row.checkbox(), 4.0),
                );
            } else {
                Checkbox::paint(
                    scene,
                    t,
                    theme,
                    &CheckboxFace::new(row.checkbox(), row.enabled).transparent_unchecked(),
                );
            }
        }
        let color = if row.placeholder {
            c.text_dim.with_alpha_factor(0.6)
        } else if row.enabled {
            c.text_primary
        } else {
            c.text_dim
        };
        for (layout, rect) in [(&row.key, row.key_rect), (&row.value, row.value_rect)] {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                t,
                &rect,
            );
            tr.draw_rich(
                scene,
                layout,
                t * Affine::translate((rect.x0, rect.y0)),
                color,
            );
            scene.pop_layer();
        }
        if !row.placeholder && row.indicator {
            scene.fill(
                Fill::NonZero,
                t,
                ui::surface::edge_on(c.surfaces.base),
                None,
                &Rect::new(row.rect.x0, row.rect.y1 - 1.0, row.rect.x1, row.rect.y1),
            );
        }
        scene.pop_layer();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "declarative drawing of the auth editor that reads top-to-bottom"
    )]
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
        y += 20.0 * self.text_scale;

        // Mode chips (display of the active scheme; the scheme is fixed per
        // loaded request in this app, so these are read-only chrome).
        let mut cx = x0;
        for label in Auth::LABELS {
            let active = self.request.auth.label() == label;
            let (lw, _) = tr.measure(
                label,
                tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                    .size,
                None,
            );
            let chip = Rect::new(cx, y, cx + f64::from(lw) + 24.0, y + self.row_height);
            if active {
                let shape = RoundedRect::from_rect(chip, f64::from(theme.tokens.radii.full));
                scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
            } else {
                ui::surface::card(
                    scene,
                    t,
                    chip,
                    f64::from(theme.tokens.radii.full),
                    c.lift(0.35),
                );
            }
            tr.draw_text_weighted(
                scene,
                label,
                tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                    .size,
                if active {
                    c.on_surface(theme.accent_primary).ink
                } else {
                    c.text_dim
                },
                t * Affine::translate((cx + 12.0, y + 8.0)),
                None,
                TextWeight::Semibold,
            );
            cx = chip.x1 + 8.0;
        }
        y += self.row_height + 20.0;

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
            let field = Rect::new(
                x0,
                y + 16.0 * self.text_scale,
                area.x1 - PAD,
                y + 16.0 * self.text_scale + self.control_height,
            );
            ui::surface::card(
                scene,
                t,
                field,
                f64::from(theme.tokens.radii.md),
                c.surfaces.raised,
            );
            let shown = if secret && !value.is_empty() {
                "\u{2022}".repeat(value.chars().count().min(24))
            } else {
                value.to_string()
            };
            tr.draw_text(
                scene,
                &shown,
                tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                    .size,
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
                    tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                        .size,
                    c.text_dim,
                    t * Affine::translate((x0, y)),
                    None,
                );
            }
            Auth::Bearer { token } => draw_field(scene, tr, y, "TOKEN", token, true),
            Auth::Basic { username, password } => {
                draw_field(scene, tr, y, "USERNAME", username, false);
                draw_field(
                    scene,
                    tr,
                    y + self.control_height + 36.0 * self.text_scale,
                    "PASSWORD",
                    password,
                    true,
                );
            }
            Auth::ApiKey { header, value } => {
                draw_field(scene, tr, y, "HEADER NAME", header, false);
                draw_field(
                    scene,
                    tr,
                    y + self.control_height + 36.0 * self.text_scale,
                    "VALUE",
                    value,
                    true,
                );
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
            let (lw, _) = tr.measure(
                label,
                tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                    .size,
                None,
            );
            let chip = Rect::new(cx, y, cx + f64::from(lw) + 20.0, y + self.row_height);
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
                ui::surface::card(
                    scene,
                    t,
                    chip,
                    f64::from(theme.tokens.radii.md),
                    c.lift(0.35),
                );
            }
            tr.draw_text_weighted(
                scene,
                label,
                tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                    .size,
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
        y += self.row_height + 12.0;

        match self.request.body_mode {
            BodyMode::None => {
                tr.draw_text(
                    scene,
                    "This request does not have a body.",
                    tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                        .size,
                    c.text_dim,
                    t * Affine::translate((x0, y)),
                    None,
                );
            }
            BodyMode::Form => {
                let sub = Rect::new(area.x0, y - 8.0, area.x1, area.y1);
                self.draw_kv_table(scene, t, tr, theme, sub);
            }
            BodyMode::Json | BodyMode::Raw => {
                let field = l.body;
                if self.body_input.is_focused() {
                    let shape = RoundedRect::from_rect(field, f64::from(theme.tokens.radii.md));
                    scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
                    scene.stroke(&Stroke::new(2.0), t, theme.border_focus, None, &shape);
                } else {
                    ui::surface::card(
                        scene,
                        t,
                        field,
                        f64::from(theme.tokens.radii.md),
                        c.surfaces.raised,
                    );
                }
                self.body_input.draw(scene, t, tr, theme, presence);
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "declarative drawing of the response pane that reads top-to-bottom"
    )]
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
        scene.fill(
            Fill::NonZero,
            t,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &top_div,
        );

        let row = l.resp_status;
        let x0 = area.x0 + PAD;
        let cy = row.y0 + f64::midpoint(self.control_height, 10.0);

        if let Some(resp) = self.response.clone() {
            let scol = status_color(theme, resp.status_class());
            let pill_label = format!("{} {}", resp.status, resp.status_text);
            let (pw, ph) = tr.measure_weighted(
                &pill_label,
                tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                    .size,
                None,
                TextWeight::Bold,
            );
            let pill = Rect::new(
                x0,
                cy - self.control_height / 2.0,
                x0 + f64::from(pw) + 24.0,
                cy + self.control_height / 2.0,
            );
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
                tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                    .size,
                scol,
                t * Affine::translate((pill.x0 + 12.0, cy - f64::from(ph) / 2.0)),
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
                tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                    .size,
                c.text_dim,
                t * Affine::translate((pill.x1 + 16.0, cy - f64::from(ph) / 2.0)),
                None,
            );

            // View switch draws itself.
            self.response_view_toggle
                .draw(scene, t, tr, theme, presence);

            let body_area = l.resp_body;
            scene.fill(Fill::NonZero, t, c.surfaces.base, None, &body_area);
            let body_div = Rect::new(area.x0, body_area.y0, area.x1, body_area.y0 + 1.0);
            scene.fill(
                Fill::NonZero,
                t,
                ui::surface::edge_on(c.surfaces.raised),
                None,
                &body_div,
            );

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
            let body_area = l.resp_body;
            scene.fill(Fill::NonZero, t, c.surfaces.base, None, &body_area);
            self.response_error.draw(scene, t, tr, theme, presence);
            // Preserve the transport's actual detail below the shared recovery
            // treatment; the CTA remains visible and keyboard-accessible.
            tr.draw_text(
                scene,
                err,
                tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                    .size,
                c.text_dim,
                t * Affine::translate((body_area.x0 + PAD, body_area.y1 - 48.0)),
                Some((body_area.width() - PAD * 2.0) as f32),
            );
        } else {
            let body_area = l.resp_body;
            scene.fill(Fill::NonZero, t, c.surfaces.base, None, &body_area);
            self.response_empty.draw(scene, t, tr, theme, presence);
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
        let size = tr
            .renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
            .size;
        // Measure the line at the size the lines are drawn with.
        let line_h = (f64::from(tr.measure("Ag", size, None).1) + 4.0).max(JSON_LINE_H);
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
            if screen_y + line_h >= area.y0 && screen_y <= area.y1 {
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
                    let caret_pt = Point::new(area.x0 + gutter_w - 2.0, screen_y + line_h / 2.0);
                    draw_fold_caret(scene, t, caret_pt, c.text_dim, !collapsed);
                    let caret_rect = Rect::new(
                        area.x0 + gutter_w - 10.0,
                        screen_y,
                        area.x0 + gutter_w + 6.0,
                        screen_y + line_h,
                    );
                    self.layout.fold_carets.push((caret_rect, idx));
                }
                let mut cx = text_x0;
                for tok in &line.tokens {
                    let col = token_color(theme, tok.kind);
                    tr.draw_style(
                        scene,
                        &tok.text,
                        col,
                        t * Affine::translate((cx, screen_y + 3.0)),
                        None,
                        &TextStyle::new(size).with_family(FontChoice::Monospace),
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
            screen_y += line_h;
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
        let size = tr
            .renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
            .size;
        // Measure the line at the size the lines are drawn with.
        let line_h = (f64::from(tr.measure("Ag", size, None).1) + 4.0).max(JSON_LINE_H);
        let mut y = area.y0 + 8.0 - self.body_scroll;
        for line in body.lines() {
            if y + line_h >= area.y0 && y <= area.y1 {
                tr.draw_style(
                    scene,
                    line,
                    c.text_primary,
                    t * Affine::translate((area.x0 + PAD, y + 3.0)),
                    None,
                    &TextStyle::new(size).with_family(FontChoice::Monospace),
                );
            }
            y += line_h;
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
        let caption = "RESPONSE HEADERS";
        let (_, caption_h) = tr.measure_weighted(caption, 10.0, None, TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            caption,
            10.0,
            c.text_dim,
            t * Affine::translate((x0, y)),
            None,
            TextWeight::Bold,
        );
        y += f64::from(caption_h) + 9.0;
        // Rows advance by the measured line height of the size they paint
        // with, so a scaled header list never overprints its neighbours.
        let size = tr
            .renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
            .size;
        let line_h = f64::from(tr.measure("Ag", size, None).1);
        let row_h = (line_h + 10.0).max(28.0);
        let key_w = (area.width() * 0.32).min(280.0);
        for h in &resp.headers {
            if y + row_h > area.y1 {
                break;
            }
            let ty = y + (row_h - 1.0 - line_h) / 2.0;
            tr.draw_text_weighted(
                scene,
                &h.key,
                size,
                c.semantic.info,
                t * Affine::translate((x0, ty)),
                Some((key_w - 12.0) as f32),
                TextWeight::Semibold,
            );
            tr.draw_text(
                scene,
                &h.value,
                size,
                c.text_primary,
                t * Affine::translate((x0 + key_w, ty)),
                Some((area.width() - PAD * 2.0 - key_w) as f32),
            );
            let sep = Rect::new(x0, y + row_h - 1.0, area.x1 - PAD, y + row_h);
            scene.fill(
                Fill::NonZero,
                t,
                ui::surface::edge_on(c.surfaces.base),
                None,
                &sep,
            );
            y += row_h;
        }
    }

    // -------------------------------------------------------------------
    // Command application
    // -------------------------------------------------------------------

    /// Drain the command sink and apply each command, persisting when a
    /// command changes shared state. Returns `true` if anything was applied.
    #[allow(
        clippy::too_many_lines,
        reason = "one match is the authoritative mapping from visible commands to canonical actions"
    )]
    fn drain_commands(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let cmds: Vec<Cmd> = self.sink.borrow_mut().drain(..).collect();
        if cmds.is_empty() {
            return false;
        }
        for cmd in cmds {
            match cmd {
                Cmd::ToggleRail => self.rail.toggle(),
                Cmd::ToggleManagement(group) => {
                    self.management = if self.management == Some(group) {
                        None
                    } else {
                        Some(group)
                    };
                    for (i, row) in self.disclosures.iter_mut().enumerate() {
                        row.set_expanded(self.management == Some(i));
                    }
                }
                Cmd::ResizeRequest(delta) => {
                    self.request_height_override =
                        Some((self.layout.editor.height() + delta).max(0.0));
                }
                Cmd::ResetRequest => self.request_height_override = None,
                Cmd::ScrollNav(direction) => {
                    let page = (self.layout.nav_viewport.height() / self.row_height)
                        .floor()
                        .max(1.0) as usize;
                    self.nav_start = if direction < 0 {
                        self.nav_start.saturating_sub(page)
                    } else {
                        (self.nav_start + page).min(self.nav_rows.len().saturating_sub(1))
                    };
                }
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
                    if self.collapsed.contains(&ci) {
                        self.collapsed.remove(&ci);
                    } else {
                        self.collapsed.insert(ci);
                    }
                    self.rebuild_nav();
                }
                Cmd::LoadRequest(ci, ri) => {
                    self.load_request(ci, ri);
                }
                Cmd::ReplayHistory(hi) => {
                    if let Some(h) = self.history.get(hi).cloned() {
                        self.request.method = h.method;
                        self.url_input.set_text(&h.url);
                        self.request.url = h.url;
                    }
                }
                Cmd::OpenForm(mode) => self.open_form(mode),
                Cmd::CancelForm => self.form = None,
                Cmd::SubmitForm => self.submit_form(state, path),
            }
        }
        self.dirty = true;
        true
    }

    fn current_collection_id(&self) -> Option<uuid::Uuid> {
        self.selected
            .and_then(|(index, _)| self.collections.get(index))
            .or_else(|| self.collections.first())
            .map(|collection| collection.id)
    }

    fn open_form(&mut self, mode: FormMode) {
        self.form = Some(mode);
        self.form_name_input.set_text(match mode {
            FormMode::RenameCollection => self
                .current_collection_id()
                .and_then(|id| self.collections.iter().find(|c| c.id == id))
                .map_or("", |c| c.name.as_str()),
            FormMode::RenameRequest => self
                .selected
                .and_then(|(ci, ri)| self.collections.get(ci)?.requests.get(ri))
                .map_or("", |r| r.name.as_str()),
            FormMode::RenameEnvironment => self
                .active_env
                .and_then(|id| self.environments.iter().find(|e| e.id == id))
                .map_or("", |e| e.name.as_str()),
            FormMode::SaveRequest => self.request.name.as_str(),
            _ => "",
        });
        self.form_key_input.set_text("");
        self.form_value_input.set_text("");
    }

    fn open_request_menu_action(&mut self, action: &str) -> Result<(), String> {
        let mode = match action {
            "Save request…" => FormMode::SaveRequest,
            "Rename request…" => FormMode::RenameRequest,
            "Delete request…" => FormMode::DeleteRequest,
            _ => return Err(format!("unknown request action `{action}`")),
        };
        self.open_form(mode);
        self.request_actions.set_selected("Request actions");
        Ok(())
    }

    fn open_environment_menu_action(&mut self, action: &str) -> Result<(), String> {
        let mode = match action {
            "Rename environment…" => FormMode::RenameEnvironment,
            "Add variable…" => FormMode::NewVariable,
            "Delete variable…" => FormMode::DeleteVariable,
            _ => return Err(format!("unknown environment action `{action}`")),
        };
        self.open_form(mode);
        self.environment_actions.set_selected("Environment actions");
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive form-to-canonical-action mapping makes contextual management easy to audit"
    )]
    fn submit_form(&mut self, state: &mut AutoCommit, path: &ObjId) {
        let Some(mode) = self.form else { return };
        let name = self.form_name_input.value().trim().to_string();
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
                if let Some(id) = self.current_collection_id().filter(|_| !name.is_empty()) {
                    self.perform(state, path, &actions::rename_collection_action(id, &name));
                }
            }
            FormMode::SaveRequest => {
                if let Some(collection_id) = self.current_collection_id() {
                    let mut request = self.request.clone();
                    request.name = if name.is_empty() {
                        "Saved request".into()
                    } else {
                        name
                    };
                    request.url = self.url_input.value().to_string();
                    request.body_text = self.body_input.value().to_string();
                    self.perform(
                        state,
                        path,
                        &actions::save_request_action(collection_id, request),
                    );
                }
            }
            FormMode::RenameRequest => {
                if let Some((ci, ri)) = self.selected
                    && let Some(collection) = self.collections.get(ci)
                    && let Some(request) = collection.requests.get(ri)
                    && !name.is_empty()
                {
                    self.perform(
                        state,
                        path,
                        &actions::rename_request_action(collection.id, request.id, &name),
                    );
                }
            }
            FormMode::DeleteRequest => {
                if let Some((ci, ri)) = self.selected
                    && let Some(collection) = self.collections.get(ci)
                    && let Some(request) = collection.requests.get(ri)
                {
                    self.perform(
                        state,
                        path,
                        &actions::delete_request_action(collection.id, request.id),
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
                let key = self.form_key_input.value().trim().to_string();
                let value = self.form_value_input.value().trim().to_string();
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
                let key = self.form_key_input.value().trim();
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
        self.form = None;
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
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::API_TESTER_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        if self.joined && !self.adopted && ApiDoc::is_empty(state, path) {
            return;
        }
        if !self.adopted {
            if ApiDoc::is_empty(state, path) {
                if !self.seeded {
                    self.seed_workspace(state, path);
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
        self.poll_worker();
        if !self.pending_history.is_empty() {
            for entry in std::mem::take(&mut self.pending_history) {
                self.perform(state, path, &ApiTesterAction::RecordHistory { entry });
            }
            self.history = ApiDoc::history(state, path);
            self.rebuild_nav();
            self.refresh_search();
        }

        let collection_name = self
            .current_collection_id()
            .and_then(|id| self.collections.iter().find(|c| c.id == id))
            .map_or("No collection", |c| c.name.as_str());
        let request_name = self
            .selected
            .and_then(|(ci, ri)| self.collections.get(ci)?.requests.get(ri))
            .map_or("Unsaved request", |r| r.name.as_str());
        let environment_name = self
            .environments
            .iter()
            .find(|e| Some(e.id) == self.active_env)
            .map_or(NO_ENV, |e| e.name.as_str());
        #[expect(
            clippy::indexing_slicing,
            reason = "disclosures always holds the three management groups built in build()"
        )]
        self.disclosures[0].set_accessible_name(format!("Collection actions: {collection_name}"));
        #[expect(
            clippy::indexing_slicing,
            reason = "disclosures always holds the three management groups built in build()"
        )]
        self.disclosures[1].set_accessible_name(format!("Request actions: {request_name}"));
        #[expect(
            clippy::indexing_slicing,
            reason = "disclosures always holds the three management groups built in build()"
        )]
        self.disclosures[2].set_accessible_name(format!("Environment actions: {environment_name}"));
        #[expect(
            clippy::indexing_slicing,
            reason = "the listed indices are fixed entries of the management button table built in build_widgets()"
        )]
        for (index, label, target) in [
            (2, "Rename collection", collection_name),
            (3, "Rename request", request_name),
            (4, "Delete request", request_name),
            (6, "Rename environment", environment_name),
            (7, "Delete environment", environment_name),
            (8, "Add variable", environment_name),
            (9, "Delete variable", environment_name),
        ] {
            // Preserve each button's callback while refreshing its contextual
            // screen-reader name.
            let button =
                std::mem::replace(&mut self.manage_buttons[index], Button::new("", "", None));
            self.manage_buttons[index] = button.with_accessible_name(&format!("{label}: {target}"));
        }

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
        // Both App::rebuild_structure/init_layout and FrameHarness::try_relayout
        // clear bindings and node_map before calling root.build_layout. Only
        // here can replaced rows be dropped: focus capture and event undo
        // inspection have finished reading the preceding admitted tree.
        self.retired_nav_rows.clear();
        self.nav_rows_replaced = false;
        // Build a child taffy node per live control, in the SAME order
        // `children`/`children_mut`/`position_children` use, so the host's
        // focus walk (which zips `children()` against `taffy.children()`)
        // lines up. The children are hand-positioned in `apply_layout`, so
        // their taffy geometry is unused — the nodes exist only so each
        // control is registered in `node_map` (a11y + focus + hit-testing).
        self.text_sm = theme.tokens.typography.scale.sm;
        self.rail_row_gap = f64::from(theme.tokens.spacing.s2);
        self.rail_group_gap = f64::from(theme.tokens.spacing.s1);
        self.rail_inset = f64::from(theme.tokens.spacing.s6);
        let accent = theme.accent_primary.to_rgba8();
        self.variable_color = [accent.r, accent.g, accent.b];
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

    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        tr: &mut TextRenderer,
    ) -> bool {
        let style = tr.renderer_input_style(&TextStyle::new(self.text_sm));
        let (_, line) = tr.measure_style("Ag", None, &style);
        let (_, label) = tr.measure("Environment actions", 14.0, None);
        let row = f64::from(line).ceil() + 12.0;
        let control = f64::from(label).ceil() + 14.0;
        let (_, env_caption) = tr.measure_weighted("ENV", 9.0, None, TextWeight::Bold);
        let (_, caption) = tr.measure_weighted("COLLECTIONS", 10.0, None, TextWeight::Bold);
        self.env_caption_h = f64::from(env_caption).ceil();
        self.caption_h = f64::from(caption).ceil();
        let changed = self.row_height.to_bits() != row.max(ROW_H).to_bits()
            || self.control_height.to_bits() != control.max(34.0).to_bits();
        self.row_height = row.max(ROW_H);
        self.control_height = control.max(34.0);
        self.text_scale = f64::from(tr.text_scale());
        let (width, _) = tr.measure("Environment actions", 14.0, None);
        let rail_width = SIDEBAR_W.max(f64::from(width) + 68.0);
        self.rail.set_docked_width(rail_width);
        self.rail.set_overlay_width(rail_width);
        self.rail
            .set_breakpoint(RAIL_BREAKPOINT.max(rail_width + 520.0));
        let before = self.kv_height;
        let rect = taffy.layout(node).map_or(self.rect, |layout| {
            Rect::new(
                0.0,
                0.0,
                f64::from(layout.size.width),
                f64::from(layout.size.height),
            )
        });
        let width = self.rail.resolve(rect).width();
        self.measure_kv_table(tr, width);
        changed || before != self.kv_height
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
        self.sync_adaptive_metrics(theme);
        // Advance the drawer's eased reveal (it self-ticks off wall-clock and
        // honours reduce-motion), then recompute geometry so the controls sit
        // exactly on the chrome this frame.
        self.rail.tick(theme.reduce_motion);
        self.layout = self.compute_layout();
        self.position_children();
        if self.follow_rail_focus() {
            self.layout = self.compute_layout();
            self.position_children();
        }
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

    #[expect(
        clippy::too_many_lines,
        reason = "event routing is ordered by precedence (escape, overlays, sidebar, editors); splitting it would hide that order"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }

        let child_nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        if matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        ) {
            #[expect(
                clippy::indexing_slicing,
                reason = "management only ever holds a group index from ToggleManagement, which is emitted by one of the three disclosures"
            )]
            let target = if let Some(group) = self.management.take() {
                self.disclosures[group].set_expanded(false);
                Some(
                    [
                        "api.collection-actions",
                        "api.request-actions",
                        "api.environment-actions",
                    ][group],
                )
            } else if self.rail.is_overlay() && self.rail.is_open() {
                self.rail.snap(false);
                Some("api.workspace-menu")
            } else {
                None
            };
            if let Some(target) = target {
                if let Some(index) = self
                    .children_ref()
                    .iter()
                    .position(|child| child.test_id() == Some(target))
                {
                    *ctx.focus_target = child_nodes.get(index).copied();
                }
                *ctx.handled = true;
                self.dirty = true;
                return;
            }
        }
        let blocked_by_rail = self.rail.is_overlay()
            && self.sidebar_visible()
            && self.layout.sidebar.contains(self.last_cursor);
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.layout.divider.contains(self.last_cursor) && !blocked_by_rail => {
                self.request_drag = Some((self.last_cursor.y, self.layout.editor.height()));
                if let Some(index) = self
                    .children_ref()
                    .iter()
                    .position(|child| child.test_id() == Some("api.request-divider"))
                {
                    *ctx.focus_target = child_nodes.get(index).copied();
                }
                *ctx.handled = true;
            }
            WindowEvent::CursorMoved { .. } if self.request_drag.is_some() => {
                if let Some((start, height)) = self.request_drag {
                    self.request_height_override =
                        Some((height + self.last_cursor.y - start).max(0.0));
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.request_drag.is_some() => {
                self.request_drag = None;
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.layout.nav_viewport.contains(self.last_cursor) =>
            {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(p) => p.y,
                };
                if dy < 0.0 {
                    self.nav_start =
                        (self.nav_start + 1).min(self.nav_rows.len().saturating_sub(1));
                } else if dy > 0.0 {
                    self.nav_start = self.nav_start.saturating_sub(1);
                }
                *ctx.handled = true;
            }
            _ => {}
        }
        if *ctx.handled {
            self.dirty = true;
            return;
        }

        let pointer_event = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        );
        let overlay_sidebar = self.rail.is_overlay() && self.sidebar_visible() && pointer_event;
        if overlay_sidebar
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            )
            && !self.layout.sidebar.contains(self.last_cursor)
        {
            self.rail.snap(false);
            *ctx.handled = true;
            self.dirty = true;
            return;
        }
        let sidebar_count = self.layout.sidebar_items.len()
            + self.form.map_or(0, |form| {
                2 + usize::from(form.needs_name())
                    + usize::from(form.needs_variable_fields())
                    + usize::from(form.needs_value())
            });
        // Route the event through the child widgets (pointer-capture aware),
        // exactly like a `Column`.
        let child_nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        {
            let mut kids = self.children_mut_ref();
            if let Some(idx) = kids.iter().position(|k| k.wants_pointer_capture()) {
                if let Some(&cnode) = child_nodes.get(idx)
                    && let Some(k) = kids.get_mut(idx)
                {
                    k.handle_event(event, &mut ctx.reborrow(ctx.path, cnode));
                }
            } else {
                let sidebar_start = kids.len().saturating_sub(sidebar_count);
                let cursor_move = matches!(event, WindowEvent::CursorMoved { .. });
                let mut any_handled = false;
                for (index, (k, &cn)) in kids.iter_mut().zip(child_nodes.iter()).enumerate() {
                    if overlay_sidebar && index < sidebar_start {
                        continue;
                    }
                    if cursor_move {
                        *ctx.handled = false;
                    }
                    k.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
                    any_handled |= *ctx.handled;
                    if *ctx.handled && !cursor_move {
                        break;
                    }
                }
                *ctx.handled |= any_handled;
            }
        }

        // Apply any commands the children queued, then reconcile the pickers.
        let did = self.drain_commands(ctx.state, ctx.path);
        self.reconcile_pickers(ctx.state, ctx.path);
        if did || *ctx.handled {
            self.dirty = true;
        }
        if *ctx.handled {
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
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * -40.0,
                    MouseScrollDelta::PixelDelta(p) => -p.y,
                };
                if self.layout.sidebar.contains(self.last_cursor) {
                    // The rail is a local view, not a collaborative document
                    // field. Cap its offset to a generous bounded range so a
                    // runaway wheel event cannot hide it indefinitely.
                    self.sidebar_scroll = (self.sidebar_scroll + dy).clamp(0.0, 1200.0);
                    self.dirty = true;
                    *ctx.handled = true;
                } else if self.layout.body_region.contains(self.last_cursor) {
                    self.body_scroll = (self.body_scroll + dy).max(0.0);
                    self.dirty = true;
                    *ctx.handled = true;
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

    fn wants_pointer_capture(&self) -> bool {
        self.request_drag.is_some()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.layout
            .divider
            .contains(point)
            .then_some(winit::window::CursorIcon::RowResize)
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

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
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
        for child in self.children_mut_ref() {
            if let Some(update) =
                child.poll_resource_operation(invocation, cancellation_requested, doc, path)
            {
                return Some(update);
            }
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
        let rows_replaced = std::mem::take(&mut self.nav_rows_replaced);
        child_changed || self_changed || rows_replaced
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
        self.sending || !self.pending_history.is_empty() || !self.adopted
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

impl ApiTesterApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
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
                self.send();
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
                self.load_request(collection_index, request_index);
                self.send();
            }
            crate::actions::ApiTesterOperation::ReplayHistory { history_id } => {
                if kind != tilo_actions::OperationKind::View {
                    return Ok(false);
                }
                let history_id = uuid::Uuid::parse_str(&history_id)
                    .map_err(|_| "invalid API Tester history id".to_string())?;
                let Some(entry) = self.history.iter().find(|entry| entry.id == history_id) else {
                    return Err("history entry no longer exists".to_string());
                };
                self.request.method = entry.method;
                self.request.url.clone_from(&entry.url);
                self.url_input.set_text(&entry.url);
                self.error = None;
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
        doc: &AutoCommit,
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

impl ApiTesterApp {
    /// Handle a left-press on painted chrome that has no backing widget: the
    /// overlay scrim and JSON fold carets. The menu toggle itself is a real
    /// [`Button`], so it participates in focus and accessibility.
    fn on_chrome_click(&mut self, p: Point) {
        if self.rail.layout() == SplitLayout::Overlay
            && self.rail.is_open()
            && !self.layout.sidebar.contains(p)
            && self.layout.scrim.contains(p)
        {
            self.rail.close();
            self.dirty = true;
            return;
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

    /// The request builder's derived state: placeholder highlighting, active
    /// environment resolution, the outgoing URL, tab counts and loading a saved
    /// request, each on a fresh app.
    #[test]
    fn request_builder_resolves_environment_highlights_placeholders_and_loads_saved_requests() {
        value_spans_highlight_each_closed_placeholder();
        env_vars_resolve_from_active_environment();
        outgoing_substitutes_environment_into_url();
        loading_a_saved_request_swaps_the_builder();
        tab_count_reflects_enabled_rows();
    }

    /// Case of `request_builder_resolves_environment_highlights_placeholders_and_loads_saved_requests`.
    fn value_spans_highlight_each_closed_placeholder() {
        let texts = |value: &str| -> Vec<(String, bool)> {
            ApiTesterApp::value_spans(value, [1, 2, 3])
                .into_iter()
                .map(|span| (span.text, span.bold))
                .collect()
        };
        assert_eq!(texts(""), [(String::new(), false)]);
        assert_eq!(
            texts("a {{host}}/é{{}}b {{open"),
            [
                ("a ".into(), false),
                ("{{host}}".into(), true),
                ("/é".into(), false),
                ("{{}}".into(), true),
                ("b {{open".into(), false),
            ]
        );
        assert_eq!(
            texts("{{x}}"),
            [
                (String::new(), false),
                ("{{x}}".into(), true),
                (String::new(), false)
            ]
        );
    }

    /// The navigation rail: token spacing, one retained row per nav entry, every
    /// row reachable at large text, focus surviving row replacement, and
    /// activation of the named item.
    #[test]
    fn navigation_rail_rows_keep_rhythm_reachability_and_focus() {
        rail_controls_share_one_token_gap();
        demo_builds_nav_rows_for_collections_and_history();
        scaled_rail_keeps_every_users_request_before_history();
        composition_same_count_nav_replacement_keeps_old_focus_alive_until_rebuild();
        composition_long_displayed_navigation_activates_the_named_item();
    }

    /// The rail's stacked controls share one rhythm step (HIG §7b): the
    /// named gap is the `s2` token, and every stacked pair in the computed
    /// layout is separated by exactly that step.
    /// Case of `navigation_rail_rows_keep_rhythm_reachability_and_focus`.
    fn rail_controls_share_one_token_gap() {
        let mut app = ApiTesterApp::new();
        let rail_gap = app.rail_row_gap;
        assert!((rail_gap - f64::from(ui::tokens::spacing::Spacing::BASE.s2)).abs() < 0.01);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.snap_rail(true);
        let l = app.compute_layout();
        let mut stacked: Vec<Rect> = l.sidebar_items.iter().map(|(_, r)| *r).collect();
        stacked.sort_by(|a, b| a.y0.total_cmp(&b.y0));
        let controls: Vec<Rect> = stacked
            .into_iter()
            .filter(|r| (r.height() - app.control_height).abs() < 0.01)
            .collect();
        assert!(controls.len() >= 4, "the rail lays out its controls");
        for pair in controls.windows(2) {
            let gap = pair[1].y0 - pair[0].y1;
            // Consecutive stacked controls: one step; a larger gap is the
            // collections section between the groups, never a smaller one.
            assert!(
                (gap - rail_gap).abs() < 0.01 || gap > rail_gap,
                "stacked rail controls keep the s2 step, got {gap}"
            );
            assert!(gap >= 4.0, "rail controls never touch: gap {gap}");
        }
        // An open action group: its rows sit one `s1` apart and the group
        // still ends a full `s2` above whatever follows.
        let group_gap = app.rail_group_gap;
        assert!((group_gap - f64::from(ui::tokens::spacing::Spacing::BASE.s1)).abs() < 0.01);
        app.management = Some(1);
        let l = app.compute_layout();
        let rows: Vec<Rect> = l
            .sidebar_items
            .iter()
            .filter_map(|(item, r)| match item {
                SidebarItem::Manage(3 | 4 | 10 | 11 | 12) => Some(*r),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows.len(),
            5,
            "the environment group lays out its five rows"
        );
        for pair in rows.windows(2) {
            let gap = pair[1].y0 - pair[0].y1;
            assert!(
                (gap - group_gap).abs() < 0.01,
                "group rows keep the s1 step, got {gap}"
            );
        }
    }
    use crate::model::KeyValue;

    fn app() -> ApiTesterApp {
        ApiTesterApp::new_demo()
    }

    /// The live constructor fabricates nothing, the demo constructor seeds a full
    /// workspace, and a seeded workspace survives a CRDT round trip.
    #[test]
    fn constructors_start_live_empty_demo_seeded_and_persist_through_crdt() {
        demo_app_has_seeded_workspace();
        live_app_starts_empty();
        persist_then_adopt_round_trips_through_crdt();
    }

    /// Case of `constructors_start_live_empty_demo_seeded_and_persist_through_crdt`.
    fn demo_app_has_seeded_workspace() {
        let a = ApiTesterApp::new_demo();
        assert!(!a.collections.is_empty());
        assert!(!a.environments.is_empty());
        assert!(a.active_env.is_some());
        assert!(a.response.is_some());
    }

    /// Case of `constructors_start_live_empty_demo_seeded_and_persist_through_crdt`.
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

    /// Case of `request_builder_resolves_environment_highlights_placeholders_and_loads_saved_requests`.
    fn env_vars_resolve_from_active_environment() {
        let a = app();
        let vars = a.env_vars();
        assert_eq!(
            vars.get("base_url").map(String::as_str),
            Some("https://api.example.com/v2")
        );
    }

    /// Case of `request_builder_resolves_environment_highlights_placeholders_and_loads_saved_requests`.
    fn outgoing_substitutes_environment_into_url() {
        let a = app();
        let out = a.outgoing();
        assert!(out.url.starts_with("https://api.example.com/v2/users"));
        assert!(out.url.contains("page=1"));
        assert!(out.url.contains("per_page=25"));
        assert!(!out.url.contains("sort="));
    }

    /// Case of `request_builder_resolves_environment_highlights_placeholders_and_loads_saved_requests`.
    fn loading_a_saved_request_swaps_the_builder() {
        let mut a = app();
        a.load_request(0, 2);
        assert_eq!(a.request.method, HttpMethod::Post);
        assert_eq!(a.url_input.value(), "{{base_url}}/users");
        assert_eq!(a.request.body_mode, BodyMode::Json);
        assert_eq!(a.selected, Some((0, 2)));
    }

    /// Case of `request_builder_resolves_environment_highlights_placeholders_and_loads_saved_requests`.
    fn tab_count_reflects_enabled_rows() {
        let a = app();
        assert_eq!(a.tab_count(RequestTab::Params), 2);
        assert_eq!(a.tab_count(RequestTab::Headers), 2);
        assert_eq!(a.tab_count(RequestTab::Auth), 1);
    }

    /// Case of `constructors_start_live_empty_demo_seeded_and_persist_through_crdt`.
    fn persist_then_adopt_round_trips_through_crdt() {
        let a = app();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        a.seed_workspace(&mut doc, &root);

        let mut b = ApiTesterApp::new();
        b.collections.clear();
        b.adopt(&doc, &root);
        assert!(!b.collections.is_empty());
        assert_eq!(b.collections.len(), a.collections.len());
    }

    /// Visible commands either dispatch the canonical action (affordance ==
    /// `ActionSpec`) or stay device-local without touching document heads; each
    /// case uses a fresh app and document.
    #[test]
    fn ui_commands_dispatch_canonical_actions_or_stay_device_local() {
        selecting_environment_dispatches_the_set_active_action();
        visible_workspace_management_commands_use_canonical_actions();
        folding_a_collection_is_device_local_and_emits_no_document_change();
        visible_context_menus_open_real_request_and_environment_workflows();
        composition_view_commands_never_change_document_heads_or_leave_hidden_rects();
    }

    /// The environment picker is wired through the action host: selecting a new
    /// environment dispatches exactly the [`SetActiveEnvironment`] the Ctrl+K
    /// "activate" spec would build (affordance == `ActionSpec`), and the choice
    /// lands in the shared document. Drives the real reconcile path.
    /// Case of `ui_commands_dispatch_canonical_actions_or_stay_device_local`.
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

    /// Case of `ui_commands_dispatch_canonical_actions_or_stay_device_local`.
    fn visible_workspace_management_commands_use_canonical_actions() {
        let mut a = ApiTesterApp::new_demo();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        a.seed_workspace(&mut doc, &root);

        a.open_form(FormMode::NewCollection);
        a.form_name_input.set_text("Billing API");
        a.sink.borrow_mut().push(Cmd::SubmitForm);
        assert!(a.drain_commands(&mut doc, &root));
        assert!(
            ApiDoc::collections(&doc, &root)
                .iter()
                .any(|collection| collection.name == "Billing API"),
            "the visible New collection control dispatched its canonical action"
        );
        let create = a
            .actions
            .last_dispatched()
            .expect("action host recorded create");
        assert_eq!(create.type_id, API_TESTER_ACTION_TYPE);

        a.open_form(FormMode::NewVariable);
        a.form_key_input.set_text("region");
        a.form_value_input.set_text("eu-west-1");
        a.sink.borrow_mut().push(Cmd::SubmitForm);
        assert!(a.drain_commands(&mut doc, &root));
        let active = a.active_env.expect("demo has an active environment");
        assert!(
            ApiDoc::environments(&doc, &root)
                .iter()
                .find(|environment| environment.id == active)
                .is_some_and(|environment| environment
                    .variables
                    .iter()
                    .any(|variable| variable.key == "region" && variable.value == "eu-west-1")),
            "the visible Set variable control dispatched its canonical action"
        );
        let set_variable = a
            .actions
            .last_dispatched()
            .expect("action host recorded variable update");
        assert_eq!(set_variable.type_id, API_TESTER_ACTION_TYPE);
    }

    /// Case of `ui_commands_dispatch_canonical_actions_or_stay_device_local`.
    fn folding_a_collection_is_device_local_and_emits_no_document_change() {
        let mut a = ApiTesterApp::new_demo();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        a.seed_workspace(&mut doc, &root);
        let heads = doc.get_heads();
        a.sink.borrow_mut().push(Cmd::ToggleCollection(0));
        assert!(a.drain_commands(&mut doc, &root));
        assert_eq!(doc.get_heads(), heads, "fold state stays off the CRDT wire");
        assert!(a.collapsed.contains(&0));
    }

    /// Case of `navigation_rail_rows_keep_rhythm_reachability_and_focus`.
    fn demo_builds_nav_rows_for_collections_and_history() {
        let a = app();
        // At least the collection headers + the seeded history entries.
        assert!(
            !a.nav_rows.is_empty(),
            "sidebar rows are real focusable widgets"
        );
        assert_eq!(a.nav_rows.len(), a.nav_meta.len());
    }

    /// Case of `navigation_rail_rows_keep_rhythm_reachability_and_focus`.
    fn scaled_rail_keeps_every_users_request_before_history() {
        let mut a = app();
        a.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        let theme = Theme::dark().with_text_scale(2.0);
        a.sync_adaptive_metrics(&theme);
        a.snap_rail(true);
        let layout = a.compute_layout();

        let users = a
            .nav_meta
            .iter()
            .position(|meta| {
                matches!(meta.kind, NavKind::Collection { .. }) && meta.label == "Users API"
            })
            .expect("the demo exposes Users API");
        let history = a
            .nav_meta
            .iter()
            .position(|meta| matches!(meta.kind, NavKind::History { .. }))
            .expect("the demo exposes request history");
        let _ = layout;
        let mut visited = std::collections::BTreeSet::new();
        for first in users..=history {
            a.nav_start = first;
            let page = a.compute_layout();
            let visible: Vec<_> = page
                .sidebar_items
                .iter()
                .filter_map(|(item, rect)| {
                    if let SidebarItem::Nav(index) = item {
                        Some((*index, *rect))
                    } else {
                        None
                    }
                })
                .collect();
            assert!(
                !visible.is_empty(),
                "a narrow large-text rail keeps a usable list viewport"
            );
            for (index, rect) in &visible {
                assert!(
                    rect.y0 >= page.nav_viewport.y0 && rect.y1 <= page.nav_viewport.y1,
                    "each reachable row stays inside the list viewport"
                );
                visited.insert(*index);
            }
            for pair in visible.windows(2) {
                assert!(
                    pair[0].1.y1 <= pair[1].1.y0,
                    "scaled request rows retain their own vertical cadence"
                );
            }
        }
        for index in users..users + 5 {
            assert!(
                visited.contains(&index),
                "the collection and all four requests remain reachable"
            );
        }
        assert!(
            visited.contains(&history),
            "History remains reachable after the collection"
        );
    }

    /// Request and response chrome geometry at phone, overlay and desktop widths.
    #[test]
    fn request_and_response_chrome_adapts_between_phone_and_desktop() {
        compact_tab_strip_reserves_no_overlapping_hint_space();
        desktop_request_controls_keep_save_and_overflow_visible();
        phone_url_bar_gives_the_endpoint_a_full_second_row();
        compact_response_chrome_gets_a_separate_switcher_row();
        wide_response_chrome_keeps_the_inline_switcher();
        overlay_hamburger_is_square_and_kv_indicator_scales_with_text();
    }

    /// Case of `request_and_response_chrome_adapts_between_phone_and_desktop`.
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

    /// Case of `request_and_response_chrome_adapts_between_phone_and_desktop`.
    fn desktop_request_controls_keep_save_and_overflow_visible() {
        let mut a = app();
        a.rect = Rect::new(0.0, 0.0, 1440.0, 900.0);
        let layout = a.compute_layout();
        assert!(
            layout.tabs.x1 + 8.0 <= layout.save.x0,
            "Save clears the tabs"
        );
        assert!(
            layout.save.x1 + 8.0 <= layout.request_actions.x0,
            "the contextual request menu clears Save"
        );
        assert!(
            layout.request_actions.x1 <= layout.main.x1 - PAD,
            "the visible request overflow stays within desktop chrome"
        );
        assert!(
            layout.save.width() >= 80.0 && layout.save.height() >= 32.0,
            "the explicit desktop Save affordance has a usable target: {}×{}",
            layout.save.width(),
            layout.save.height()
        );
    }

    /// Case of `ui_commands_dispatch_canonical_actions_or_stay_device_local`.
    fn visible_context_menus_open_real_request_and_environment_workflows() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        let request = serde_json::to_vec(&crate::actions::ApiTesterOperation::RequestMenu {
            action: "Rename request…".to_string(),
        })
        .expect("operation encodes");
        assert_eq!(
            a.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    crate::actions::API_TESTER_OPERATION_TYPE,
                    &request
                ),
                &mut doc,
                &root
            ),
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                ui::tilo_actions::ActionOutput::default()
            )))
        );
        assert_eq!(a.form, Some(FormMode::RenameRequest));
        assert_eq!(a.request_actions.selected(), "Request actions");

        a.form = None;
        let environment =
            serde_json::to_vec(&crate::actions::ApiTesterOperation::EnvironmentMenu {
                action: "Add variable…".to_string(),
            })
            .expect("operation encodes");
        assert_eq!(
            a.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    crate::actions::API_TESTER_OPERATION_TYPE,
                    &environment
                ),
                &mut doc,
                &root
            ),
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                ui::tilo_actions::ActionOutput::default()
            )))
        );
        assert_eq!(a.form, Some(FormMode::NewVariable));
        assert_eq!(a.environment_actions.selected(), "Environment actions");
    }

    /// Case of `request_and_response_chrome_adapts_between_phone_and_desktop`.
    fn phone_url_bar_gives_the_endpoint_a_full_second_row() {
        let mut a = app();
        a.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        let layout = a.compute_layout();
        assert!((layout.urlbar.height() - COMPACT_URLBAR_H).abs() < 0.01);
        assert!(layout.url.y0 >= layout.method.y1 + 8.0);
        assert!(
            layout.url.width() >= 350.0,
            "URL input is not a clipped sliver"
        );
        assert!(layout.url.x0 >= layout.main.x0 + PAD);
        assert!(layout.url.x1 <= layout.main.x1 - PAD);
        assert!(layout.tab_strip.y0 >= layout.url.y1 + 8.0);
    }

    /// Case of `request_and_response_chrome_adapts_between_phone_and_desktop`.
    fn compact_response_chrome_gets_a_separate_switcher_row() {
        let mut a = app();
        a.response = Some(http::sample_response());
        a.rect = Rect::new(0.0, 0.0, 400.0, 844.0);
        let layout = a.compute_layout();
        assert!(
            (layout.resp_status.height() - COMPACT_RESPONSE_STATUS_H).abs() < 0.01,
            "response width={} status height={}",
            layout.response.width(),
            layout.resp_status.height()
        );
        assert!(layout.resp_view.y0 >= layout.resp_status.y0 + 44.0);
        assert!(layout.resp_view.y1 <= layout.resp_status.y1);
        assert!((layout.resp_body.y0 - layout.resp_status.y1).abs() < f64::EPSILON);
        assert!(layout.resp_view.x0 >= layout.response.x0 + PAD);
        assert!(layout.resp_view.x1 <= layout.response.x1 - PAD);
        assert!(
            response_status_line_center(layout.resp_status) + 12.0 < layout.resp_view.y0,
            "status pill must clear the compact view switcher"
        );
    }

    /// Case of `request_and_response_chrome_adapts_between_phone_and_desktop`.
    fn wide_response_chrome_keeps_the_inline_switcher() {
        let mut a = app();
        a.response = Some(http::sample_response());
        a.rect = Rect::new(0.0, 0.0, 1024.0, 800.0);
        let layout = a.compute_layout();
        assert!((layout.resp_status.height() - RESPONSE_STATUS_H).abs() < f64::EPSILON);
        assert!((layout.resp_view.x1 - (layout.response.x1 - PAD)).abs() < f64::EPSILON);
        assert!(layout.resp_view.y0 < layout.resp_status.y1);
    }

    /// The `` surface: Send performs a real HTTP round trip and
    /// reports worker completion exactly once, and a late record resolves by
    /// one indexed lookup.
    #[test]
    fn resource_provider_sends_for_real_and_resolves_exact_records() {
        resource_send_performs_a_real_http_round_trip();
        resource_send_reports_worker_completion_instead_of_queue_success();
        resource_provider_resolves_a_late_record_without_scanning_the_workspace();
    }

    /// Case of `resource_provider_sends_for_real_and_resolves_exact_records`.
    #[expect(
        clippy::too_many_lines,
        reason = "end-to-end scenario that reads top-to-bottom"
    )]
    fn resource_send_performs_a_real_http_round_trip() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use ui::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = tilo_test_env::hang_deadline();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            ui::time::now() < deadline,
                            "the app's HTTP request never reached the probe server"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept request: {error}"),
                }
            };
            // Windows inherits the listener's nonblocking mode on accept.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            stream
                .set_write_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 8192, "unbounded request headers");
            }
            let request = String::from_utf8(request).unwrap();
            assert!(
                request.starts_with("GET /probe?name=Gr%C3%BCn HTTP/1.1\r\n"),
                "{request}"
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("x-tilo-probe: native\r\n"),
                "{request}"
            );
            let body = "{\"result\":\"Grün\"}";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });

        let mut app = ApiTesterApp::new();
        app.url_input.set_text(&format!("http://{address}/probe"));
        app.request.params.push(KeyValue::new("name", "Grün"));
        app.request
            .headers
            .push(KeyValue::new("X-Tilo-Probe", "native"));
        let invocation = tilo_actions::InvocationId::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(ApiDoc::history(&doc, &automerge::ROOT).is_empty());
        let payload =
            serde_json::to_vec(&crate::actions::ApiTesterOperation::SendActiveRequest).unwrap();
        assert!(matches!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::API_TESTER_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &automerge::ROOT
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Accepted)
        ));
        // The builder remains editable while a request is in flight. History
        // must describe the request sent, not these later edits.
        app.url_input.set_text("http://127.0.0.1/edited-after-send");
        app.request.method = HttpMethod::Post;
        let update = tilo_test_env::wait_for("the HTTP worker to complete the request", || {
            app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
        });
        assert!(matches!(
            update,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ));
        server.join().unwrap();
        let response = app.response.as_ref().expect("the response is displayed");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"result\":\"Grün\"}");
        assert_eq!(app.history.len(), 1);
        assert_eq!(app.history[0].status, 200);
        assert_eq!(app.history[0].method, HttpMethod::Get);
        assert_eq!(
            app.history[0].url,
            format!("http://{address}/probe?name=Gr%C3%BCn")
        );
        assert!(!app.sending);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(ApiDoc::history(&doc, &automerge::ROOT), app.history);
        let mut restarted = ApiTesterApp::new();
        restarted.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            restarted.history, app.history,
            "completed requests survive restart"
        );
        let entry = app.history[0].clone();
        app.perform(
            &mut doc,
            &automerge::ROOT,
            &ApiTesterAction::RecordHistory { entry },
        );
        assert_eq!(
            ApiDoc::history(&doc, &automerge::ROOT).len(),
            1,
            "replay does not duplicate history"
        );
    }

    /// Case of `resource_provider_sends_for_real_and_resolves_exact_records`.
    fn resource_send_reports_worker_completion_instead_of_queue_success() {
        let mut a = app();
        let invocation = tilo_actions::InvocationId::new();
        a.resource_send_invocation = Some(invocation);
        a.tx.send(SendOutcome::Ok {
            response: http::sample_response(),
            method: HttpMethod::Get,
            url: "http://localhost/test".into(),
        })
        .unwrap();
        let doc = AutoCommit::new();
        assert!(matches!(
            a.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert!(
            a.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                .is_none(),
            "terminal delivery is at most once"
        );
    }
    /// Key/value rows size to wrapped content, keep glyphs inside their rows and
    /// drive the automatic editor height without overriding a manual preference.
    #[test]
    fn kv_table_rows_fit_their_content_and_drive_the_editor_height() {
        composition_auto_height_tracks_rows_and_auth_fields_without_changing_manual_preference();
        composition_long_kv_glyphs_fit_allocated_rows();
        composition_long_tables_share_wrapped_height_and_centered_indicators();
    }

    /// Case of `kv_table_rows_fit_their_content_and_drive_the_editor_height`.
    #[expect(
        clippy::float_cmp,
        reason = "layout values are compared for exact reproduction, not approximate equality"
    )]
    fn composition_auto_height_tracks_rows_and_auth_fields_without_changing_manual_preference() {
        let mut a = app();
        a.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);
        let mut tr = TextRenderer::new();
        a.measure_kv_table(&mut tr, 936.0);
        let params = a.compute_layout();
        a.request.params.push(KeyValue::new("extra", "value"));
        a.measure_kv_table(&mut tr, 936.0);
        assert_eq!(
            a.compute_layout().editor.height(),
            params.editor.height() + a.row_height
        );
        a.request.params.push(KeyValue::new("", ""));
        a.measure_kv_table(&mut tr, 936.0);
        assert_eq!(
            a.compute_layout().editor.height(),
            params.editor.height() + a.row_height
        );
        a.active_tab = RequestTab::Auth;
        a.request.auth = Auth::None;
        let none = a.compute_layout().editor.height();
        a.request.auth = Auth::Basic {
            username: "user".into(),
            password: "secret".into(),
        };
        assert!(a.compute_layout().editor.height() > none);
        a.request_height_override = Some(410.0);
        a.rect = Rect::new(0.0, 0.0, 800.0, 480.0);
        let small = a.compute_layout();
        assert!(small.editor.height() < 410.0);
        assert!(small.resp_body.height() >= a.row_height * 2.0);
        assert_eq!(a.request_height_override, Some(410.0));
        a.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);
        assert_eq!(a.compute_layout().editor.height(), 410.0);
    }

    /// Case of `ui_commands_dispatch_canonical_actions_or_stay_device_local`.
    fn composition_view_commands_never_change_document_heads_or_leave_hidden_rects() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        a.seed_workspace(&mut doc, &root);
        let heads = doc.get_heads();
        a.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);
        a.layout = a.compute_layout();
        a.sink
            .borrow_mut()
            .extend([Cmd::ToggleManagement(1), Cmd::ResizeRequest(24.0)]);
        a.drain_commands(&mut doc, &root);
        a.layout = a.compute_layout();
        a.position_children();
        assert!(a.manage_buttons[3].get_rect().area() > 0.0);
        let sig = a.child_sig();
        a.sink
            .borrow_mut()
            .extend([Cmd::ToggleManagement(1), Cmd::ResetRequest]);
        a.drain_commands(&mut doc, &root);
        a.layout = a.compute_layout();
        a.position_children();
        assert!(sig != a.child_sig());
        assert_eq!(a.manage_buttons[3].get_rect(), Rect::ZERO);
        assert!(
            !a.children_ref()
                .iter()
                .any(|child| child.test_id() == Some("api.rename-request"))
        );
        assert_eq!(doc.get_heads(), heads);
    }
    /// Case of `kv_table_rows_fit_their_content_and_drive_the_editor_height`.
    #[expect(
        clippy::float_cmp,
        reason = "layout values are compared for exact reproduction, not approximate equality"
    )]
    fn composition_long_kv_glyphs_fit_allocated_rows() {
        for (width, scale) in [(480.0, 1.0), (800.0, 2.0)] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            let rows = vec![KeyValue::new(
                "a long query header or form field key that must wrap inside its own column",
                "a long value with {{environment_variable}} and more text after the variable",
            )];
            let mut app = app();
            app.text_sm = theme.tokens.typography.scale.sm;
            app.request.params = rows;
            app.measure_kv_table(&mut tr, width);
            let row = &app.kv_rows[0];
            let mut scene = Scene::new();
            ApiTesterApp::draw_kv_row(&mut scene, Affine::IDENTITY, &mut tr, &theme, row);
            let end = row.rect.y1;
            assert_eq!(row.checkbox().center().y, row.rect.center().y);
            assert_eq!(row.key_rect.center().y, row.rect.center().y);
            assert_eq!(row.value_rect.center().y, row.rect.center().y);
            assert!(row.rect.height() > ROW_H);
            assert_eq!(app.automatic_request_height(), app.kv_height.unwrap());
            for run in &scene.encoding().resources.glyph_runs {
                for glyph in &scene.encoding().resources.glyphs[run.glyphs.clone()] {
                    let baseline = f64::from(run.transform.translation[1] + glyph.y);
                    assert!(
                        baseline < end,
                        "{scale}x row ends at {end}, glyph baseline {baseline} escapes"
                    );
                }
            }
        }
    }

    /// The overlay hamburger is as wide as the row's controls are tall at
    /// every text scale (a 36px slab beside a 52px picker read as a broken
    /// button), and the KV enable box scales with the text it toggles.
    /// Case of `request_and_response_chrome_adapts_between_phone_and_desktop`.
    fn overlay_hamburger_is_square_and_kv_indicator_scales_with_text() {
        for scale in [1.0f32, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            let mut a = app();
            a.text_scale = f64::from(scale);
            a.text_sm = theme.tokens.typography.scale.sm;
            a.control_height = 34.0 * f64::from(scale);
            a.rect = Rect::new(0.0, 0.0, 480.0, 900.0);
            a.rail.resolve(a.rect);
            assert_eq!(
                a.rail.layout(),
                SplitLayout::Overlay,
                "480px collapses the rail"
            );
            let l = a.compute_layout();
            assert!(
                (l.hamburger.width() - l.hamburger.height()).abs() < 0.01,
                "{scale}x hamburger {:?} is not square",
                l.hamburger
            );
            assert!(l.hamburger.height() >= a.control_height - 0.01);
            assert!(
                l.method.x0 >= l.hamburger.x1,
                "{scale}x the method picker starts after the hamburger"
            );

            a.measure_kv_table(&mut tr, 448.0);
            let row = &a.kv_rows[0];
            let expected = (KV_INDICATOR_SIZE * f64::from(scale)).min(row.rect.height() - 4.0);
            assert!((row.checkbox().width() - expected).abs() < 0.01);
            assert!((row.checkbox().height() - expected).abs() < 0.01);
            assert!(row.checkbox().y0 >= row.rect.y0 && row.checkbox().y1 <= row.rect.y1);
            assert!(
                row.checkbox().x1 + KV_INDICATOR_GAP <= row.key_rect.x0 + 0.01,
                "{scale}x checkbox {:?} runs into the key column at {}",
                row.checkbox(),
                row.key_rect.x0
            );
        }
    }

    /// Case of `navigation_rail_rows_keep_rhythm_reachability_and_focus`.
    fn composition_same_count_nav_replacement_keeps_old_focus_alive_until_rebuild() {
        let mut app = app();
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        app.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        assert!(app.retired_nav_rows.is_empty());
        app.nav_rows[1].set_focus(true);
        let original = std::ptr::from_ref(&app.nav_rows[1]);
        let original_name = app.nav_rows[1].accessibility().unwrap().name;
        let count = app.nav_rows.len();
        app.load_request(0, 1);
        assert_eq!(app.nav_rows.len(), count);
        assert!(
            app.take_structure_dirty(),
            "same-count replacement invalidates topology"
        );
        assert_eq!(std::ptr::from_ref(&app.retired_nav_rows[0][1]), original);
        assert_eq!(
            app.retired_nav_rows[0][1].accessibility().unwrap().name,
            original_name
        );
        assert!(app.retired_nav_rows[0][1].is_focused());
        app.collections[0].requests[0].name = "renamed without changing row count".into();
        app.rebuild_nav();
        assert!(app.take_structure_dirty());
        assert_eq!(app.retired_nav_rows.len(), 2);
        // Match both real hosts: discard old maps before admitting a new tree.
        nodes.clear();
        tree = TaffyTree::new();
        app.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        assert!(app.retired_nav_rows.is_empty());
        assert!(!app.nav_rows_replaced);
        assert!(
            app.nav_rows[1]
                .accessibility()
                .unwrap()
                .name
                .unwrap()
                .contains("renamed")
        );
    }

    fn long_content_app(tab: RequestTab) -> ApiTesterApp {
        let mut app = ApiTesterApp::new_demo();
        let rows = vec![
            KeyValue::new(
                "X-Client-Requested-Response-Format-For-International-Customer-Service",
                "prefix {{environment_variable}} with a longer suffix",
            ),
            KeyValue {
                key: "disabled optional request metadata field".into(),
                value: "more descriptive content".into(),
                enabled: false,
                ..KeyValue::new("", "")
            },
        ];
        app.request.params = rows.clone();
        app.request.headers = rows.clone();
        app.request.form = rows;
        app.request.body_mode = BodyMode::Form;
        app.active_tab = tab;
        app.collections.truncate(1);
        app.collections[0].name =
            "Long named collection for the international customer service".into();
        app.collections[0].requests.truncate(2);
        app.collections[0].requests[0].name =
            "List international customers with Unicode café and detailed response metadata".into();
        app.collections[0].requests[1].name =
            "Create international customer record with a long descriptive request name".into();
        app.history.truncate(1);
        app.history[0].url =
            "https://example.com/very/long/customer/history/path/with/many/segments/and/details"
                .into();
        app.rebuild_nav();
        app
    }

    /// Case of `kv_table_rows_fit_their_content_and_drive_the_editor_height`.
    #[expect(
        clippy::float_cmp,
        reason = "layout values are compared for exact reproduction, not approximate equality"
    )]
    fn composition_long_tables_share_wrapped_height_and_centered_indicators() {
        for (width, scale) in [(480.0, 1.0), (800.0, 2.0)] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            for tab in [RequestTab::Params, RequestTab::Headers, RequestTab::Body] {
                let mut app = long_content_app(tab);
                app.text_sm = theme.tokens.typography.scale.sm;
                app.text_scale = f64::from(scale);
                app.row_height = (f64::from(
                    tr.measure_style(
                        "Ag",
                        None,
                        &tr.renderer_input_style(&TextStyle::new(app.text_sm)),
                    )
                    .1,
                )
                .ceil()
                    + 12.0)
                    .max(ROW_H);
                app.rect = Rect::new(0.0, 0.0, width, 1200.0);
                app.measure_kv_table(&mut tr, width);
                assert_eq!(app.kv_rows.len(), 3);
                for row in &app.kv_rows {
                    assert_eq!(row.checkbox().center().y, row.key_rect.center().y);
                    assert_eq!(row.checkbox().center().y, row.value_rect.center().y);
                    assert!(row.rect.contains_rect(row.key_rect));
                    assert!(row.rect.contains_rect(row.value_rect));
                    assert!(row.key_rect.x1 < row.value_rect.x0);
                    assert!((row.key_rect.height() - f64::from(row.key.height())).abs() < 0.01);
                    let mut scene = Scene::new();
                    ApiTesterApp::draw_kv_row(&mut scene, Affine::IDENTITY, &mut tr, &theme, row);
                    for run in &scene.encoding().resources.glyph_runs {
                        for glyph in &scene.encoding().resources.glyphs[run.glyphs.clone()] {
                            let x = f64::from(run.transform.translation[0] + glyph.x);
                            let y = f64::from(run.transform.translation[1] + glyph.y);
                            assert!(
                                x >= row.rect.x0
                                    && x < row.rect.x1
                                    && y >= row.rect.y0
                                    && y < row.rect.y1
                            );
                        }
                    }
                }
                for pair in app.kv_rows.windows(2) {
                    assert!(pair[0].rect.y1 <= pair[1].rect.y0);
                }
                let offset = if tab == RequestTab::Body {
                    app.row_height + 16.0
                } else {
                    0.0
                };
                assert_eq!(
                    app.automatic_request_height(),
                    app.kv_height.unwrap() + offset
                );
                assert_eq!(
                    app.compute_layout().editor.height(),
                    app.automatic_request_height()
                );
                let narrow = app.kv_height.unwrap();
                app.measure_kv_table(&mut tr, width * 2.0);
                assert!(narrow > app.kv_height.unwrap());
                app.request_height_override = Some(140.0);
                app.measure_kv_table(&mut tr, width);
                assert_eq!(app.compute_layout().editor.height(), 140.0);
                assert_eq!(app.request_height_override, Some(140.0));
            }
        }
    }

    /// Case of `navigation_rail_rows_keep_rhythm_reachability_and_focus`.
    fn composition_long_displayed_navigation_activates_the_named_item() {
        use ui::headless::Driver;
        for (width, height, scale) in [(480, 900, 1.0), (800, 900, 2.0)] {
            let mut app = long_content_app(RequestTab::Params);
            app.snap_rail(true);
            let selected_name = format!(
                "{} {}",
                app.collections[0].requests[1].method.as_str(),
                app.collections[0].requests[1].name
            );
            let history_name = app.nav_meta.last().unwrap().a11y_name.clone();
            let mut d = Driver::new_with_theme(
                ui::State::new(),
                Box::new(app),
                (width, height),
                Theme::dark().with_text_scale(scale),
            );
            d.click_named("api.nav.2").unwrap();
            assert!(d.focus_access_nodes().iter().any(|node| node.name.as_deref() == Some(&selected_name) && node.state.selected));
            let targets = d.targets();
            assert!(targets["api.nav.2"].y1 <= targets["api.nav.3"].y0);
            assert!(
                d.focus_access_nodes()
                    .iter()
                    .any(|node| node.name.as_deref() == Some(&history_name))
            );
            // Mount the actual generated history row on its own so its
            // callback can be observed without the unrelated draft-sync gap.
            let mut history_app = long_content_app(RequestTab::Params);
            let sink = history_app.sink.clone();
            let row = history_app.nav_rows.pop().unwrap();
            let mut history_driver = Driver::new_with_theme(
                ui::State::new(),
                Box::new(row),
                (width, height),
                Theme::dark().with_text_scale(scale),
            );
            history_driver.click_named("api.nav.3").unwrap();
            assert!(matches!(
                sink.borrow_mut().pop(),
                Some(Cmd::ReplayHistory(0))
            ));
            history_driver.key("Enter");
            assert!(matches!(
                sink.borrow_mut().pop(),
                Some(Cmd::ReplayHistory(0))
            ));
        }
    }

    /// Manual capture tool: writes PNG and target JSON for the named routes and
    /// the long-content routes.
    #[test]
    #[ignore = "writes named route and long-content route captures for review; set API_TESTER_CAPTURE_DIR"]
    fn composition_capture_named_and_long_content_routes() {
        composition_long_content_capture_routes();
        composition_capture_routes();
    }

    /// Case of `composition_capture_named_and_long_content_routes`.
    fn composition_long_content_capture_routes() {
        use ui::headless::Driver;
        let directory = std::path::PathBuf::from(std::env::var("API_TESTER_CAPTURE_DIR").unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        for (width, height, scale) in [(480, 900, 1.0), (800, 900, 2.0)] {
            for (route, tab, long, rail) in [
                ("params-response", RequestTab::Params, false, false),
                ("headers", RequestTab::Headers, false, false),
                ("long-params", RequestTab::Params, true, false),
                ("long-headers", RequestTab::Headers, true, false),
                ("long-form", RequestTab::Body, true, false),
                ("long-navigation", RequestTab::Params, true, true),
            ] {
                let mut app = if long {
                    long_content_app(tab)
                } else {
                    ApiTesterApp::new_demo()
                };
                app.active_tab = tab;
                app.request_tabs.set_selected(
                    RequestTab::ALL
                        .iter()
                        .position(|value| *value == tab)
                        .unwrap(),
                );
                if rail {
                    app.snap_rail(true);
                }
                let d = Driver::new_with_theme(
                    ui::State::new(),
                    Box::new(app),
                    (width, height),
                    Theme::dark().with_text_scale(scale),
                );
                let stem = format!("{route}-{width}x{height}-{scale}x");
                d.render_window_png(&directory.join(format!("{stem}.png")));
                let targets: std::collections::BTreeMap<_, _> = d
                    .targets()
                    .into_iter()
                    .map(|(id, r)| (id, vec![r.x0, r.y0, r.x1, r.y1]))
                    .collect();
                std::fs::write(
                    directory.join(format!("{stem}.json")),
                    serde_json::to_string_pretty(&targets).unwrap(),
                )
                .unwrap();
            }
        }
    }

    /// Case of `composition_capture_named_and_long_content_routes`.
    fn composition_capture_routes() {
        use ui::headless::Driver;
        let directory = std::path::PathBuf::from(std::env::var("API_TESTER_CAPTURE_DIR").unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        for (width, height, scale) in [
            (1200, 800, 1.0),
            (800, 700, 1.0),
            (480, 900, 1.0),
            (800, 480, 1.0),
            (800, 900, 2.0),
        ] {
            for route in ["params-response", "body", "management"] {
                let mut app = ApiTesterApp::new_demo();
                if route != "params-response" {
                    // Fixture staged before the initial document sync. Loading
                    // another saved request into an already bound draft has a
                    // separate pre-existing synchronization defect; these
                    // captures do not claim to verify that transition.
                    app.load_request(0, 2);
                    app.active_tab = RequestTab::Body;
                }
                if route == "management" {
                    app.snap_rail(true);
                }
                let mut d = Driver::new_with_theme(
                    ui::State::new(),
                    Box::new(app),
                    (width, height),
                    Theme::dark().with_text_scale(scale),
                );
                if route != "params-response" {
                    assert!(d.focus_access_nodes().iter().any(|node| {
                        node.name.as_deref() == Some("Request body")
                            && node
                                .value
                                .as_deref()
                                .is_some_and(|body| body.contains("Grace Hopper"))
                    }));
                }
                if route == "management" {
                    d.click_named("api.environment-actions").unwrap();
                    for id in [
                        "api.new-environment",
                        "api.rename-environment",
                        "api.delete-environment",
                        "api.new-variable",
                        "api.delete-variable",
                    ] {
                        let rect = d.targets()[id];
                        assert!(rect.x0 >= 0.0 && rect.y0 >= 0.0 && rect.y1 <= f64::from(height));
                    }
                }
                let stem = format!("{route}-{width}x{height}-{scale}x");
                d.render_window_png(&directory.join(format!("{stem}.png")));
                let targets: std::collections::BTreeMap<_, _> = d
                    .targets()
                    .into_iter()
                    .map(|(id, r)| (id, vec![r.x0, r.y0, r.x1, r.y1]))
                    .collect();
                std::fs::write(
                    directory.join(format!("{stem}.json")),
                    serde_json::to_string_pretty(&targets).unwrap(),
                )
                .unwrap();
            }
        }
    }

    /// Case of `resource_provider_sends_for_real_and_resolves_exact_records`.
    fn resource_provider_resolves_a_late_record_without_scanning_the_workspace() {
        let mut a = app();
        let mut collection = Collection::new("Load test");
        collection.requests = (0..5_000)
            .map(|index| Request::new(format!("Request {index}")))
            .collect();
        let request = collection.requests.last().expect("late request").id;
        a.collections = vec![collection];
        a.environments.clear();
        a.refresh_search();

        let doc = AutoCommit::new();
        let id = format!("api_tester.request:{request}");
        let resource =
            ApiTesterApp::publish_resource(&a, &doc, &id).expect("late exact address resolves");
        assert_eq!(resource.id.0, id);
        assert!(resource.actions.iter().any(|action| action.id == "send"));
        assert_eq!(
            a.search_provider.last_exact_inspected(),
            1,
            "the performs one indexed address lookup"
        );
    }
}
