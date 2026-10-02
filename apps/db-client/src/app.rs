//! Root widget for the database client. Owns the [`Session`] (connections,
//! engine, schema, results) and the shared `ui::CodeEditor` (CRDT-backed
//! SQL buffer), lays
//! out the two regions (connections+schema sidebar, main panel) itself, and
//! draws everything directly into the Vello scene. Status is surfaced as a
//! transient floating bubble rather than a permanent strip, so the content
//! spans to the window bottom.
//!
//! The fixed pane split is computed from the viewport rect within one
//! full-bleed Taffy leaf. Event routing hit-tests the schema tree, tab bar,
//! editor, and results grid.
//!
//! The connections + schema sidebar is responsive. On a wide window it stays
//! docked beside the main panel (the desktop layout above). When the window
//! is narrower than `BREAKPOINT` the sidebar collapses into an overlay flap
//! that slides over the main panel behind a dimming scrim — closed by default
//! and summoned with the hamburger toggle in the tab bar. The eased slide and
//! the breakpoint default-reconciliation use the shared [`FlapState`] kernel.

use std::collections::HashMap;
use std::io::Write;
use std::ops::{Deref, DerefMut};
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use serde::{Deserialize, Serialize};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use uuid::Uuid;

use tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Button, ButtonFace, CodeEditor, ContextMenu, EmptyState, FlapState, Language, MenuItem,
    SplitLayout, TaffyTreeExt, Widget,
};

use crate::connect_modal::ConnectModal;
use crate::doc::{ConnectionsDoc, EditorDoc};
use crate::grid::GridView;
use crate::model::Engine;
use crate::schema_nav::{self, Activation, SchemaActivations, SchemaBrowser};
use crate::session::{MainTab, RunStatus, Session};
use crate::theme_ext::ThemeExt;

const SIDEBAR_W: f64 = 268.0;
const TABBAR_H: f64 = 38.0;
const TOOLBAR_H: f64 = 40.0;
const QUERY_DIVIDER_H: f64 = 10.0;
const QUERY_RESULT_STRIP_H: f64 = 26.0;
const QUERY_GRID_MIN_H: f64 = 56.0;
const QUERY_BOTTOM_PAD: f64 = 8.0;
/// Below this allocated width (logical px) the sidebar stops being a docked
/// pane and becomes an overlay flap. Chosen so the 268px sidebar plus a
/// usable editor/results column stop fitting comfortably side by side.
const BREAKPOINT: f64 = 820.0;
/// Hamburger toggle box drawn at the left of the tab bar when the sidebar is
/// not fully docked-open, so a collapsed sidebar can be summoned.
const TOGGLE_SIZE: f64 = 26.0;
/// Horizontal inset shared by the sidebar's connection rows and its
/// "New connection" button so they read as one column.
const SIDEBAR_INSET: f64 = 8.0;

/// Main-panel band heights measured from the renderer the panel paints with.
///
/// Refreshed at the top of every draw (and re-laid out in the same frame when
/// they change), so the tab bar, toolbar, result strip and sidebar toggle grow
/// with their labels at larger text scales instead of the 26 px "Query" pill
/// touching the edges of a fixed 38 px bar. At 1× every value equals its
/// constant: a 13 px label measures 16.25 px, so a tab pill is 16.25 + 2·4 ≤ 26
/// inside the 38 px bar (6 px inset either side), a toolbar button
/// 16.25 + 2·5 ≤ 28 inside the 40 px toolbar, and the 12 px result-strip line
/// (15 px) sits in 15 + 2·5 ≤ 26. Tab bar and toolbar controls are inset 6 px
/// from their band (`inset`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Bands {
    tabbar_h: f64,
    toolbar_h: f64,
    strip_h: f64,
    /// Sidebar toggle button side; square, centred in the tab bar.
    toggle: f64,
    /// Vertical inset of a tab pill / toolbar button inside its band.
    inset: f64,
}

impl Default for Bands {
    fn default() -> Self {
        Self {
            tabbar_h: TABBAR_H,
            toolbar_h: TOOLBAR_H,
            strip_h: QUERY_RESULT_STRIP_H,
            toggle: TOGGLE_SIZE,
            inset: 6.0,
        }
    }
}

impl Bands {
    fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let one = Self::default();
        let tab_h = f64::from(tr.measure("Ag", 13.0, None).1);
        let toolbar_label_h = f64::from(tr.measure("Ag", 13.0, None).1);
        let strip_label_h = f64::from(tr.measure("Ag", 12.0, None).1);
        // Label padding: one spacing step inside a tab pill; 5 px (the
        // toolbar's own off-grid value) inside a button and the strip.
        let pill_pad = f64::from(theme.tokens.spacing.s1);
        let tabbar_h = (tab_h + 2.0 * pill_pad + 2.0 * one.inset).max(TABBAR_H);
        let toolbar_h = (toolbar_label_h + 2.0 * 5.0 + 2.0 * one.inset).max(TOOLBAR_H);
        let strip_h = (strip_label_h + 2.0 * 5.0).max(QUERY_RESULT_STRIP_H);
        Self {
            tabbar_h,
            toolbar_h,
            strip_h,
            toggle: (tabbar_h - 2.0 * one.inset).max(TOGGLE_SIZE),
            inset: one.inset,
        }
    }
}
/// Minimum toolbar control widths; measured labels widen them when needed.
const RUN_MIN_W: f64 = 150.0;
const SNIPPETS_MIN_W: f64 = 110.0;
/// The single externally-invokable effect protocol for live database work.
/// Unlike document actions, these payloads contain only stable object ids and
/// SQL/table names — never passwords or live driver handles.
pub(crate) const DB_OPERATION_TYPE: &str = "db_client.operation";

/// Encode arbitrary catalog text into a path component without the ambiguity
/// of delimiter-joined ids (`a:b` vs `a`, `b`). Labels remain human-readable;
/// only machine identities use this representation.
fn resource_component(value: &str) -> String {
    hex::encode(value)
}

pub(crate) fn catalog_resource_id(kind: &str, components: &[&str]) -> String {
    std::iter::once(kind.to_string())
        .chain(
            components
                .iter()
                .map(|component| resource_component(component)),
        )
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct QueryLayout {
    toolbar: Rect,
    editor: Rect,
    divider: Rect,
    result_strip: Rect,
    grid: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QueryChildSig {
    empty: bool,
    tab: MainTab,
    resize_menu: bool,
}

/// Stable retained identity for the immediate-mode embedded editor. The app
/// still owns its rectangle and paint pass, while focus/accessibility actions
/// see the real `CodeEditor` as a sibling of the query divider. Its absolute
/// measuring leaf shapes at the embedded allocation without joining the app's
/// visual flow.
struct QueryEditorChild {
    editor: CodeEditor,
    measure_width: f32,
}

impl QueryEditorChild {
    fn new(editor: CodeEditor) -> Self {
        Self {
            editor,
            measure_width: 1.0,
        }
    }

    fn set_measure_width(&mut self, width: f64) -> bool {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the editor width is a bounded logical window dimension"
        )]
        let width = width.max(1.0) as f32;
        let changed = (self.measure_width - width).abs() > 0.5;
        if changed {
            self.measure_width = width;
        }
        changed
    }
}

impl Deref for QueryEditorChild {
    type Target = CodeEditor;

    fn deref(&self) -> &Self::Target {
        &self.editor
    }
}

impl DerefMut for QueryEditorChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.editor
    }
}

impl Widget for QueryEditorChild {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Let the shared editor capture theme scale, then keep its measuring
        // leaf out of the app's visual flow. Its width is the real embedded
        // editor allocation learned by the preceding composition pass.
        let node = self.editor.build_layout(taffy, theme, node_map);
        let _ = taffy.set_style(
            node,
            Style {
                position: Position::Absolute,
                size: Size {
                    width: length(self.measure_width),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        self.editor.measure(known, available, text_renderer)
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.editor.handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "QueryEditorChild"
    }

    fn test_id(&self) -> Option<&str> {
        self.editor.test_id()
    }

    fn get_rect(&self) -> Rect {
        self.editor.get_rect()
    }

    fn is_focusable(&self) -> bool {
        self.editor.is_focusable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.editor.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.editor.is_focused()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.editor.cursor_shape(point)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.editor.accessibility()
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        self.editor.perform_action(action, state, path)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.editor.wants_pointer_capture()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.editor.ime_cursor_rect()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.editor.ime_purpose()
    }

    fn text_review(&self) -> Option<(usize, String)> {
        self.editor.text_review()
    }

    fn layout_needs_measure(&self) -> bool {
        self.editor.layout_needs_measure()
    }
}

impl ui::undo::UndoPoints for QueryEditorChild {
    fn needs_commit(&self) -> bool {
        self.editor.needs_commit()
    }
    fn on_commit(&mut self) {
        self.editor.on_commit();
    }
    fn get_state(&self) -> Option<String> {
        self.editor.get_state()
    }
    fn set_state(&mut self, state: &str) {
        self.editor.set_state(state);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum QueryResizeCommand {
    Adjust(f64),
    Reset,
    OpenMenu,
}

struct QueryDivider {
    rect: Rect,
    focused: bool,
    automatic: bool,
    effective_height: f64,
    pending: Option<QueryResizeCommand>,
}

impl QueryDivider {
    fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            focused: false,
            automatic: true,
            effective_height: 0.0,
            pending: None,
        }
    }

    fn set_geometry(&mut self, rect: Rect, effective_height: f64, automatic: bool) {
        self.rect = rect;
        self.effective_height = effective_height;
        self.automatic = automatic;
    }

    fn take_command(&mut self) -> Option<QueryResizeCommand> {
        self.pending.take()
    }

    fn handle_key(&mut self, key: &Key) -> bool {
        if !self.focused {
            return false;
        }
        self.pending = match key {
            Key::Named(NamedKey::ArrowDown) => Some(QueryResizeCommand::Adjust(24.0)),
            Key::Named(NamedKey::ArrowUp) => Some(QueryResizeCommand::Adjust(-24.0)),
            Key::Named(NamedKey::Home) => Some(QueryResizeCommand::Reset),
            Key::Named(NamedKey::Enter | NamedKey::ContextMenu) => {
                Some(QueryResizeCommand::OpenMenu)
            }
            Key::Character(text) if text.as_str() == " " => Some(QueryResizeCommand::OpenMenu),
            _ => None,
        };
        self.pending.is_some()
    }
}

impl Widget for QueryDivider {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(QUERY_DIVIDER_H as f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let center = self.rect.center();
        let track = Rect::new(
            self.rect.x0 + 8.0,
            center.y - 0.5,
            self.rect.x1 - 8.0,
            center.y + 0.5,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.border_standard,
            None,
            &track,
        );
        let grip = Rect::new(
            center.x - 24.0,
            center.y - 1.5,
            center.x + 24.0,
            center.y + 1.5,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            if self.focused {
                theme.accent_primary
            } else {
                theme.text_dim
            },
            None,
            &grip.to_rounded_rect(1.5),
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && self.handle_key(logical_key)
        {
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "QueryDivider"
    }

    fn test_id(&self) -> Option<&str> {
        Some("db.query-divider")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::RowResize)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Slider)
                .with_name("Query editor height")
                .with_description(
                    "Arrow keys resize. Home resets automatic height. Enter opens resize actions.",
                )
                .with_value(if self.automatic {
                    format!("Automatic, {:.0} pixels", self.effective_height)
                } else {
                    format!("Manual, {:.0} pixels", self.effective_height)
                })
                .with_state(
                    ui::a11y::AccessState::new()
                        .focusable(true)
                        .focused(self.focused),
                ),
        )
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        self.pending = match action {
            ui::a11y::Action::Increment => Some(QueryResizeCommand::Adjust(24.0)),
            ui::a11y::Action::Decrement => Some(QueryResizeCommand::Adjust(-24.0)),
            ui::a11y::Action::Activate | ui::a11y::Action::ShowContextMenu => {
                Some(QueryResizeCommand::OpenMenu)
            }
            _ => None,
        };
        self.pending.is_some()
    }
}

struct QueryResizeMenu {
    menu: ContextMenu,
}

impl QueryResizeMenu {
    fn new() -> Self {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Increase query height", || {}));
        menu.push(MenuItem::action("Decrease query height", || {}));
        menu.push(MenuItem::action("Reset automatic query height", || {}));
        Self { menu }
    }

    fn open(&mut self, anchor: Rect, viewport: Rect) {
        self.menu.open_anchored(anchor, viewport);
        self.menu.focus_first_action();
    }

    fn reanchor(&mut self, anchor: Rect, viewport: Rect) {
        self.menu.reanchor(anchor, viewport);
    }

    fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    fn close(&mut self) {
        self.menu.close();
    }

    fn take_selection(&mut self) -> Option<usize> {
        self.menu.take_selection()
    }
}

impl Widget for QueryResizeMenu {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style::default());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.menu
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.menu.handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "QueryResizeMenu"
    }

    fn test_id(&self) -> Option<&str> {
        Some("db.query-resize-menu")
    }

    fn get_rect(&self) -> Rect {
        self.menu.get_rect()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut node = self.menu.accessibility()?;
        for (index, child) in node.children.iter_mut().enumerate() {
            let target = match index {
                0 => "db.increase-query-height",
                1 => "db.decrease-query-height",
                2 => "db.reset-query-height",
                _ => continue,
            };
            *child = child
                .clone()
                .with_action_target(target)
                .with_bounds(self.menu.item_rect(index)?);
        }
        Some(node)
    }

    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if matches!(action, ui::a11y::Action::Activate)
            && let Some(index) = key_path.first().and_then(|key| usize::try_from(*key).ok())
            && index < 3
        {
            self.menu.click_item(index);
            return true;
        }
        false
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let index = match target {
            "db.increase-query-height" => 0,
            "db.decrease-query-height" => 1,
            "db.reset-query-height" => 2,
            _ => return false,
        };
        self.menu.click_item(index);
        true
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        [
            "db.increase-query-height",
            "db.decrease-query-height",
            "db.reset-query-height",
        ]
        .into_iter()
        .enumerate()
        .filter_map(|(index, id)| self.menu.item_rect(index).map(|rect| (id.into(), rect)))
        .collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum DbClientOperation {
    /// Reveal a catalog location in the local navigator. It never touches the
    /// database or shared document, but it is a real view transition rather
    /// than a decorative/no-op search target.
    FocusCatalog {
        connection_id: String,
        schema: Option<String>,
    },
    /// Bring the current result back into view after resolving its generation.
    ShowResult {
        connection_id: String,
        result_generation: usize,
    },
    /// Re-run a bounded, per-device history entry. History itself remains
    /// private to a peer and is re-resolved before the external effect starts.
    RunHistory {
        connection_id: String,
        generation: usize,
    },
    RunQuery {
        connection_id: String,
        sql: String,
    },
    Connect {
        connection_id: String,
    },
    BrowseTable {
        connection_id: String,
        schema: String,
        table: String,
    },
    ExportCsv {
        connection_id: String,
        result_generation: usize,
    },
}

#[allow(
    clippy::expect_used,
    reason = "DbClientOperation contains only serializable strings, so this cannot fail without a broken serializer"
)]
pub(crate) fn encode_operation(operation: &DbClientOperation) -> EncodedAction {
    EncodedAction::encode_operation(DB_OPERATION_TYPE, operation)
        .expect("database operation payload is serializable")
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent piece of widget state (seed/demo bookkeeping, hover/open toggles, shots-mode override), always set by name rather than a confusing positional constructor"
)]
pub struct DbClientApp {
    session: Session,
    editor: QueryEditorChild,
    editor_node: Option<NodeId>,
    /// Honest first-run surface. Kept as a real child widget so its primary
    /// action participates in focus navigation and the accessibility tree.
    empty_state: EmptyState,
    empty_state_node: Option<NodeId>,
    query_divider: QueryDivider,
    query_divider_node: Option<NodeId>,
    query_resize_menu: QueryResizeMenu,
    query_resize_menu_node: Option<NodeId>,
    last_child_sig: Option<QueryChildSig>,
    grid: GridView,
    data_grid: GridView,
    /// The schema browser: a toolkit `TreeView` hosted as a real child widget
    /// (focus, keyboard and semantics included) and placed in the sidebar's
    /// lower region each layout pass.
    tree: SchemaBrowser,
    tree_node: Option<NodeId>,
    schema_activations: SchemaActivations,
    /// Catalog fingerprint the browser's model was last built from.
    schema_signature: u64,
    modal: ConnectModal,
    /// Dispatches saved-connection CRUD through the shared action model, so the
    /// connection list is collaborative, undoable document state.
    actions: crate::resource::ActionSink,
    /// Id of a connection the local user just added and wants opened as soon as
    /// it lands back through the document reconcile (peer-added connections do
    /// not set this, so they are listed but not auto-connected).
    pending_connect_id: Option<Uuid>,
    /// Active resource invocation plus whether its effect can be retried
    /// safely without duplicating a database write.
    resource_operation: Option<(tilo_actions::InvocationId, bool)>,
    export_rx: Option<ui::portal::PortalChannel>,
    export_snapshot: Option<crate::model::QueryResult>,

    rect: Rect,
    node: Option<NodeId>,
    /// Whether the editor buffer in the CRDT has been seeded yet.
    seeded: bool,
    /// When true this is the hermetic `--shots`/demo build: it seeds the
    /// bundled sample database and a starter query. The live app leaves this
    /// false, starts on the user's saved connections and never fabricates.
    demo: bool,
    /// Last cursor position (logical).
    cursor: Point,
    /// Cached sub-rects, recomputed each `apply_layout`.
    sidebar_rect: Rect,
    main_rect: Rect,
    query_layout: QueryLayout,
    /// Main-panel band heights from the last draw (see [`Bands`]).
    bands: Bands,
    /// User-requested editor height. Relayout clamps only the effective
    /// rectangle so a temporary short window cannot erase this preference.
    query_height_request: Option<f64>,
    /// Pointer origin plus effective height captured when a divider drag starts.
    query_drag: Option<(f64, f64)>,
    /// Requests one bounded composition rebuild after the embedded editor's
    /// real available width changes, so its shared measure pass can reshape.
    query_measure_dirty: bool,
    /// Adaptive sidebar flap: docked beside the content on a wide window,
    /// an overlay flap over a scrim on a narrow one. Drives the eased slide
    /// and the breakpoint default-reconciliation.
    flap: FlapState,
    /// Wall-clock of the last animation tick, for frame-rate-independent
    /// easing of the flap reveal.
    last_tick: Instant,
    /// Full-content scrim rect while the overlay flap is open (else `ZERO`).
    scrim_rect: Rect,
    /// Hamburger toggle box (summons / dismisses the sidebar); `ZERO` when
    /// the sidebar is fully docked-open and the toggle is hidden.
    toggle_btn: Rect,
    /// Hovered "Run" button.
    run_hover: bool,
    /// Toolbar snippet-dropdown open?
    snippets_open: bool,
    /// Potentially destructive SQL awaiting an explicit second confirmation
    /// from the editor chrome.
    pending_query_confirmation: Option<String>,
    confirm_run_btn: Rect,
    confirm_cancel_btn: Rect,
    /// Hovered connection row index in the sidebar.
    conn_hover: Option<usize>,
    /// Rects of the editor toolbar run button + snippet button (for hits).
    run_btn: Rect,
    snippet_btn: Rect,
    new_conn_btn: Rect,
    tab_rects: Vec<(MainTab, Rect)>,
    conn_rects: Vec<(usize, Rect)>,
    /// Per-row delete ("×") hit rects in the sidebar, shown on hover.
    conn_delete_rects: Vec<(usize, Rect)>,
    /// Whether the grid bound the current result (avoids rebinding each frame).
    /// Compared against the session's result generations, which the worker
    /// bumps when a fresh result lands.
    bound_query_gen: usize,
    bound_data_gen: usize,
    /// Drives a one-shot redraw request flag.
    dirty: bool,
    /// Transient floating status bubble, in place of the old permanent bottom
    /// status strip. Poked when the summary label changes (a connect, a query
    /// completing, a table-count tick) and revealed while the pointer hovers
    /// the bottom-right corner, then fades — so the editor/results region can
    /// span all the way to the window bottom.
    status_bubble: ui::StatusBubble,
    /// Retained, revisioned global-search projection. It is rebuilt only when
    /// the live catalog or collaborative query collection changes.
    search_index: crate::search::DbSearchIndex,
    /// The last summary the bubble showed, so it is poked only on a real change
    /// rather than every frame (draw recomputes the label unconditionally).
    last_status_label: Option<String>,
    /// `--shots` forces the bubble fully visible so a still frame captures it.
    shots_status: bool,
}

impl Default for DbClientApp {
    fn default() -> Self {
        Self::new()
    }
}

impl DbClientApp {
    fn perform_navigator_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
    ) -> bool {
        use ui::a11y::Action;

        if let Some(id) = target
            .strip_prefix("db.connection.remove:")
            .filter(|_| matches!(action, Action::Activate))
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            let Some(index) = self
                .session
                .connections
                .iter()
                .position(|item| item.id == id)
            else {
                return false;
            };
            self.remove_connection(index, state);
            return true;
        }
        if let Some(id) = target
            .strip_prefix("db.connection:")
            .filter(|_| matches!(action, Action::Activate | Action::Focus))
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            return self
                .execute_operation(DbClientOperation::Connect {
                    connection_id: id.to_string(),
                })
                .is_ok();
        }
        false
    }

    /// The live app: starts on the user's saved connections (empty on a fresh
    /// machine) and never fabricates a sample database or profiles.
    pub fn new() -> Self {
        Self::build(Session::new(), false)
    }

    /// The hermetic `--shots`/demo build: the sole source of the bundled
    /// sample database and connection profiles.
    pub fn new_demo() -> Self {
        let mut session = Session::with_sample();
        // Optional startup hooks used by the offscreen screenshot harness to
        // land directly on a given tab/table. Harmless in normal use (the env
        // vars are simply unset), and never referenced by user-facing code.
        if let Ok(tbl) = std::env::var("DB_CLIENT_TABLE") {
            session.select_table(&tbl);
        }
        if let Ok(tab) = std::env::var("DB_CLIENT_TAB") {
            session.tab = match tab.as_str() {
                "data" => MainTab::Data,
                "structure" => MainTab::Structure,
                _ => MainTab::Query,
            };
            if session.tab == MainTab::Data {
                session.load_table_data();
            }
        }
        // Await any table page requested by the screenshot hooks so the single
        // offscreen frame is fully populated.
        session.block_until_idle();
        let mut app = Self::build(session, true);
        // Screenshot hook: open the new-connection dialog for a dialog shot.
        if std::env::var("DB_CLIENT_MODAL").is_ok() {
            app.modal.open_new();
        }
        // Deterministic state hooks for visual regression captures. They are
        // intentionally environment-only: the live UI never exposes fake
        // errors or a pre-approved destructive statement.
        if std::env::var("DB_CLIENT_ERROR").is_ok() {
            app.session.status =
                RunStatus::Error("Connection refused — retry when the server is reachable".into());
        }
        if std::env::var("DB_CLIENT_CONFIRM").is_ok() {
            app.pending_query_confirmation = Some("DELETE FROM tracks".into());
        }
        app
    }

    fn build(session: Session, demo: bool) -> Self {
        let mut editor = CodeEditor::local()
            .with_language(Language::Sql)
            .with_font_size(13.5)
            .with_tab_width(2)
            .with_id("sql-editor");
        // Preserve the database client's type-to-query startup policy. The
        // shared widget itself remains conservatively unfocused by default.
        editor.set_focused(true);
        let schema_activations = SchemaActivations::default();
        let tree = schema_nav::browser(&session.schema, &schema_activations);
        let schema_signature = schema_nav::signature(&session.schema);
        let mut app = Self {
            session,
            demo,
            editor: QueryEditorChild::new(editor),
            editor_node: None,
            empty_state: EmptyState::new("No connection yet")
                .with_icon(ui::icons::Icon::HardDrive)
                .with_message(
                    "Connect to a PostgreSQL, MySQL or SQLite database to browse its schema and run queries.",
                )
                .with_action("New connection")
                .with_action_id("db.new-connection"),
            empty_state_node: None,
            query_divider: QueryDivider::new(),
            query_divider_node: None,
            query_resize_menu: QueryResizeMenu::new(),
            query_resize_menu_node: None,
            last_child_sig: None,
            grid: GridView::new(),
            data_grid: GridView::new(),
            tree,
            tree_node: None,
            schema_activations,
            schema_signature,
            modal: ConnectModal::new(),
            actions: crate::resource::ActionSink::default(),
            pending_connect_id: None,
            resource_operation: None,
            export_rx: None,
            export_snapshot: None,
            rect: Rect::ZERO,
            node: None,
            seeded: false,
            cursor: Point::ZERO,
            sidebar_rect: Rect::ZERO,
            main_rect: Rect::ZERO,
            query_layout: QueryLayout::default(),
            bands: Bands::default(),
            query_height_request: None,
            query_drag: None,
            query_measure_dirty: false,
            flap: FlapState::open_when_docked(),
            last_tick: ui::time::now(),
            scrim_rect: Rect::ZERO,
            toggle_btn: Rect::ZERO,
            run_hover: false,
            snippets_open: false,
            pending_query_confirmation: None,
            confirm_run_btn: Rect::ZERO,
            confirm_cancel_btn: Rect::ZERO,
            conn_hover: None,
            run_btn: Rect::ZERO,
            snippet_btn: Rect::ZERO,
            new_conn_btn: Rect::ZERO,
            tab_rects: Vec::new(),
            conn_rects: Vec::new(),
            conn_delete_rects: Vec::new(),
            bound_query_gen: usize::MAX,
            bound_data_gen: usize::MAX,
            dirty: true,
            status_bubble: ui::StatusBubble::new(),
            search_index: crate::search::DbSearchIndex::default(),
            last_status_label: None,
            shots_status: false,
        };
        // Seed the last-shown summary so a fresh app doesn't flash the bubble on
        // its first frame; it surfaces only when the status actually changes.
        app.last_status_label = Some(app.status_bubble_label());
        app
    }

    fn run_current_query(&mut self) {
        let sql = self.editor.value().to_string();
        if sql_is_potentially_destructive(&sql) {
            self.pending_query_confirmation = Some(sql);
            self.dirty = true;
            return;
        }
        self.run_sql_effect(sql);
    }

    fn run_sql_effect(&mut self, sql: String) {
        let Some(connection_id) = self.session.active_connection_id() else {
            self.session.status =
                RunStatus::Error("Connect to a database before running SQL".into());
            self.dirty = true;
            return;
        };
        // Keep the toolbar shortcut on the very same effect path exposed to
        // the command palette/agents. It carries a stable connection identity
        // and never smuggles a password into the action envelope.
        if let Err(error) = self.execute_operation(DbClientOperation::RunQuery {
            connection_id: connection_id.to_string(),
            sql,
        }) {
            self.session.status = RunStatus::Error(error);
        }
        self.dirty = true;
    }

    /// Execute a transient database effect after re-resolving every target
    /// against the current session. Resource descriptors are only a helpful
    /// snapshot: a collaborator can remove/rename a connection before an
    /// invocation arrives, so their payload is never authority by itself.
    fn execute_operation(&mut self, operation: DbClientOperation) -> Result<(), String> {
        match operation {
            DbClientOperation::FocusCatalog {
                connection_id,
                schema,
            } => self.focus_catalog(&connection_id, schema.as_deref())?,
            DbClientOperation::ShowResult {
                connection_id,
                result_generation,
            } => self.show_result(&connection_id, result_generation)?,
            DbClientOperation::RunHistory {
                connection_id,
                generation,
            } => self.run_history(&connection_id, generation)?,
            DbClientOperation::RunQuery { connection_id, sql } => {
                // Empty id means "active connection" — the framework resource
                // publishes Run SQL without a live session snapshot.
                let id = if connection_id.is_empty() {
                    self.session
                        .active_connection_id()
                        .ok_or_else(|| "connect to a database before running SQL".to_string())?
                } else {
                    Uuid::parse_str(&connection_id)
                        .map_err(|_| "invalid database connection id".to_string())?
                };
                if self.session.active_connection_id() != Some(id) {
                    return Err("the requested connection is no longer active".to_string());
                }
                if !self.session.connected {
                    return Err("connect to this database before running SQL".to_string());
                }
                self.session.run_query(&sql);
            }
            DbClientOperation::Connect { connection_id } => {
                let id = Uuid::parse_str(&connection_id)
                    .map_err(|_| "invalid database connection id".to_string())?;
                let idx = self
                    .session
                    .connections
                    .iter()
                    .position(|connection| connection.id == id)
                    .ok_or_else(|| "connection is no longer available".to_string())?;
                // Reconnect even when the requested resource is already the
                // selected row; the old `select_connection` no-op would make
                // an active-but-disconnected profile impossible to retry.
                self.session.active = idx;
                self.session.connect_active();
            }
            DbClientOperation::BrowseTable {
                connection_id,
                schema,
                table,
            } => {
                let id = Uuid::parse_str(&connection_id)
                    .map_err(|_| "invalid database connection id".to_string())?;
                if self.session.active_connection_id() != Some(id) || !self.session.connected {
                    return Err("the table's connection is no longer open".to_string());
                }
                if self.session.schema.find_relation(&schema, &table).is_none() {
                    return Err("table is no longer available in the active schema".to_string());
                }
                self.session.select_relation(crate::model::RelationRef {
                    schema,
                    name: table,
                });
            }
            DbClientOperation::ExportCsv {
                connection_id,
                result_generation,
            } => {
                let id = Uuid::parse_str(&connection_id)
                    .map_err(|_| "invalid database connection id".to_string())?;
                if self.session.active_connection_id() != Some(id)
                    || self.session.query_gen != result_generation
                {
                    return Err("the query result is no longer current".to_string());
                }
                let result = self
                    .session
                    .query_result
                    .clone()
                    .ok_or_else(|| "there is no query result to export".to_string())?;
                if !result.is_select {
                    return Err(
                        "only row-returning query results can be exported as CSV".to_string()
                    );
                }
                if self.export_rx.is_some() {
                    return Err("an export destination is already being selected".to_string());
                }
                self.export_snapshot = Some(result);
                self.export_rx = Some(
                    ui::portal::PickRequest::save(
                        "Export query results",
                        "Export CSV",
                        "query-results.csv",
                    )
                    .filter(
                        ui::portal::FileFilter::new("CSV (comma-separated values)").glob("*.csv"),
                    )
                    .thread_name("db-client-export-csv")
                    .send(),
                );
                self.session.status = RunStatus::Busy("Choose where to save the CSV…".into());
            }
        }
        self.dirty = true;
        Ok(())
    }

    fn focus_catalog(&mut self, connection_id: &str, schema: Option<&str>) -> Result<(), String> {
        let id = Uuid::parse_str(connection_id)
            .map_err(|_| "invalid database connection id".to_string())?;
        if self.session.active_connection_id() != Some(id) {
            return Err("the catalog connection is no longer active".to_string());
        }
        if let Some(schema) = schema {
            if !self
                .session
                .schema
                .schemas
                .iter()
                .any(|candidate| candidate.name == schema)
            {
                return Err("schema is no longer available in the active catalog".into());
            }
            self.tree
                .set_expanded(schema_nav::schema_row_id(schema), true);
        }
        self.flap.open();
        Ok(())
    }

    fn show_result(&mut self, connection_id: &str, generation: usize) -> Result<(), String> {
        let id = Uuid::parse_str(connection_id)
            .map_err(|_| "invalid database connection id".to_string())?;
        if self.session.active_connection_id() != Some(id)
            || self.session.query_gen != generation
            || self.session.query_result.is_none()
        {
            return Err("the query result is no longer current".into());
        }
        self.session.tab = MainTab::Query;
        Ok(())
    }

    fn run_history(&mut self, connection_id: &str, generation: usize) -> Result<(), String> {
        let id = Uuid::parse_str(connection_id)
            .map_err(|_| "invalid database connection id".to_string())?;
        let sql = self
            .session
            .query_history
            .iter()
            .find(|entry| entry.connection_id == id && entry.generation == generation)
            .map(|entry| entry.sql.clone())
            .ok_or_else(|| "the local history entry is no longer available".to_string())?;
        if self.session.active_connection_id() != Some(id) || !self.session.connected {
            return Err("the history entry's connection is no longer open".into());
        }
        self.session.run_query(&sql);
        Ok(())
    }

    fn drain_export_portal(&mut self) {
        let Some(rx) = self.export_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(ui::portal::PortalOutcome::Opened(path)) => {
                self.export_rx = None;
                let Some(result) = self.export_snapshot.take() else {
                    return;
                };
                match write_csv(&path, &result) {
                    Ok(()) => {
                        self.session.status = RunStatus::Ok(format!(
                            "Exported {} rows to {}",
                            result.rows.len(),
                            path.display()
                        ));
                    }
                    Err(error) => {
                        self.session.status =
                            RunStatus::Error(format!("CSV export failed: {error}"));
                    }
                }
            }
            Ok(ui::portal::PortalOutcome::Cancelled) => {
                self.export_rx = None;
                self.export_snapshot = None;
                self.session.status = RunStatus::Idle;
            }
            Ok(ui::portal::PortalOutcome::Failed(error)) => {
                self.export_rx = None;
                self.export_snapshot = None;
                self.session.status =
                    RunStatus::Error(format!("Couldn't open the save dialog: {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.export_rx = None;
                self.export_snapshot = None;
                self.session.status =
                    RunStatus::Error("CSV save dialog closed unexpectedly".into());
            }
        }
        self.dirty = true;
    }

    fn load_snippet(&mut self, sql: &str, doc: &mut AutoCommit) {
        self.replace_editor_sql(doc, sql);
        self.editor.pull(doc);
        self.snippets_open = false;
        self.dirty = true;
    }

    /// Replace the shared SQL buffer through
    /// [`DbAction::ReplaceEditorSql`](crate::actions::DbAction::ReplaceEditorSql).
    /// Hosted sessions use the framework dispatcher; standalone uses the local host.
    fn replace_editor_sql(&mut self, doc: &mut AutoCommit, sql: &str) {
        let action = crate::actions::DbAction::ReplaceEditorSql {
            sql: sql.to_string(),
        };
        self.actions.dispatch_with_context(
            doc,
            crate::actions::DB_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.db-client", "Database", &action),
        );
    }

    /// Show the saved connections the framework resource holds.
    /// An empty projection does not wipe a roster that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::DbState) {
        if self.demo {
            return;
        }
        if state.connections.is_empty() && !self.session.connections.is_empty() {
            return;
        }
        let same =
            state.connections.len() == self.session.connections.len()
                && state.connections.iter().zip(&self.session.connections).all(
                    |(incoming, local)| {
                        incoming.id == local.id
                            && incoming.name == local.name
                            && incoming.host == local.host
                            && incoming.database == local.database
                    },
                );
        if !same {
            self.session.set_connections(state.connections.clone());
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::DbClient>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    pub(crate) fn session_connected(&self) -> bool {
        self.session.connected
    }

    /// In-flight connect / query / export job label for hosted live extras.
    pub(crate) fn live_job_label(&self) -> Option<String> {
        match &self.session.status {
            RunStatus::Busy(label) => Some(label.clone()),
            _ if self.resource_operation.is_some() => Some("Database operation…".into()),
            _ => None,
        }
    }

    /// Current query-result resource when one is live (not document state).
    pub(crate) fn live_query_result_resource(&self) -> Option<Resource> {
        let connection_id = self.session.active_connection_id()?.to_string();
        let result = self.session.query_result.as_ref()?;
        let result_id = catalog_resource_id(
            "query-result",
            &[&connection_id, &self.session.query_gen.to_string()],
        );
        let mut resource = Resource::new(&result_id, "database.query-result", result.message());
        if let Ok(action) = ActionSpec::operation(
            "show",
            "Show query result",
            OperationKind::View,
            DB_OPERATION_TYPE,
            &DbClientOperation::ShowResult {
                connection_id: connection_id.clone(),
                result_generation: self.session.query_gen,
            },
        ) {
            resource = resource.with_action(action);
        }
        if result.is_select
            && let Ok(action) = ActionSpec::operation(
                "export_csv",
                "Export CSV",
                OperationKind::Effect,
                DB_OPERATION_TYPE,
                &DbClientOperation::ExportCsv {
                    connection_id,
                    result_generation: self.session.query_gen,
                },
            )
        {
            resource = resource.with_action(action);
        }
        Some(resource)
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Save a new connection profile. The live app routes it through the action
    /// model — the metadata lands in `root["connections"]` (collaborative,
    /// undoable) while the password is kept in memory only — and remembers to
    /// open it once it reconciles back. The demo keeps its roster as view state.
    fn save_connection(&mut self, profile: crate::model::ConnectionProfile, doc: &mut AutoCommit) {
        if self.demo {
            self.session.add_connection_blocking(profile);
            return;
        }
        let id = profile.id;
        self.session.remember_password(id, profile.password.clone());
        let action = crate::actions::add_connection_action(&profile);
        self.actions.dispatch_with_context(
            doc,
            crate::actions::DB_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.db-client", "Database", &action),
        );
        self.pending_connect_id = Some(id);
    }

    /// Remove a saved connection by its index in the sidebar list.
    fn remove_connection(&mut self, idx: usize, doc: &mut AutoCommit) {
        let Some(profile) = self.session.connections.get(idx).cloned() else {
            return;
        };
        if self.demo {
            self.session.connections.remove(idx);
            if self.session.active >= self.session.connections.len() {
                self.session.active = self.session.connections.len().saturating_sub(1);
            }
            return;
        }
        let action = crate::actions::remove_connection_action(profile.id);
        self.actions.dispatch_with_context(
            doc,
            crate::actions::DB_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.db-client", "Database", &action),
        );
    }

    /// Resolve the sidebar layout (docked vs overlay) from the *allocated*
    /// width against the breakpoint.
    fn split_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), BREAKPOINT)
    }

    /// Snap the sidebar flap to the layout default for a given width, with NO
    /// animation. Used by the `--shots` offscreen renderer so a single frame
    /// is deterministic: wide ⇒ docked + open, narrow ⇒ overlay + collapsed.
    pub fn snap_flap_for_width(&mut self, width: f64) {
        match SplitLayout::from_width(width, BREAKPOINT) {
            SplitLayout::Docked => self.flap.snap(true),
            SplitLayout::Overlay => self.flap.snap(false),
        }
    }

    /// `--shots`: force the floating status bubble fully visible so a single
    /// headless frame captures the chip that replaced the old status strip.
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(ui::time::now());
    }

    /// `--shots --connect`: open the "New connection" sheet so the modal can
    /// be reviewed from a single headless frame.
    pub fn open_connect_modal_for_shots(&mut self) {
        self.modal.open_new();
    }

    /// `--shots --confirm`: arm the destructive-statement confirmation with
    /// a representative statement so the dialog can be reviewed headlessly.
    pub fn arm_query_confirmation_for_shots(&mut self) {
        self.pending_query_confirmation = Some("DROP TABLE albums;".to_string());
    }

    /// Recompute every region rect from the root rect, honouring the flap's
    /// resolved layout and eased reveal.
    ///
    /// * Docked (wide): the sidebar occupies a left column whose width is the
    ///   sidebar width scaled by the reveal, and the main panel is inset by
    ///   the same amount — so collapsing/expanding the sidebar slides the
    ///   split without overlap.
    /// * Overlay (narrow): the main panel spans the full width; the sidebar
    ///   slides in from the left edge over a scrim by the eased reveal.
    fn relayout(&mut self) {
        let r = self.rect;
        // The content spans to the window bottom: the status now floats as a
        // transient bubble (see `draw_status_bubble`) rather than a permanent
        // full-width strip, so no vertical space is reserved for it.
        let body_bottom = r.y1;
        let layout = self.split_layout();
        let sw = SIDEBAR_W.min(r.width());

        match layout {
            SplitLayout::Docked => {
                let inset = sw * self.flap.reveal();
                self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + inset, body_bottom);
                self.main_rect = Rect::new(r.x0 + inset, r.y0, r.x1, body_bottom);
                self.scrim_rect = Rect::ZERO;
            }
            SplitLayout::Overlay => {
                // Main panel spans the whole body; sidebar floats over it.
                self.main_rect = Rect::new(r.x0, r.y0, r.x1, body_bottom);
                let slide = self.flap.slide();
                let x0 = r.x0 - sw * (1.0 - slide);
                self.sidebar_rect = Rect::new(x0, r.y0, x0 + sw, body_bottom);
                self.scrim_rect = if self.flap.reveal() > 0.001 {
                    Rect::new(r.x0, r.y0, r.x1, body_bottom)
                } else {
                    Rect::ZERO
                };
            }
        }

        // Schema tree fills the lower portion of the sidebar; the
        // connections list takes the top.
        let conn_h = self.connections_panel_height();
        let tree_rect = Rect::new(
            self.sidebar_rect.x0,
            self.sidebar_rect.y0 + conn_h,
            self.sidebar_rect.x1,
            self.sidebar_rect.y1,
        );
        self.tree.place_at(tree_rect);

        // Main panel: tab bar, then tab content. Band heights are the ones the
        // last draw measured (the 1× constants until text grows past them).
        let bands = self.bands;
        let content_top = self.main_rect.y0 + bands.tabbar_h;
        match self.session.tab {
            MainTab::Query => {
                let toolbar = Rect::new(
                    self.main_rect.x0,
                    content_top,
                    self.main_rect.x1,
                    content_top + bands.toolbar_h,
                );
                let editor_top = toolbar.y1;
                let editor_min = self.editor.line_height() * 3.0;
                let editor_max = (self.main_rect.y1
                    - editor_top
                    - QUERY_DIVIDER_H
                    - bands.strip_h
                    - QUERY_GRID_MIN_H
                    - QUERY_BOTTOM_PAD)
                    .max(0.0);
                let automatic =
                    self.editor.inner().content_height() + self.editor.line_height() * 2.0;
                let requested = self.query_height_request.unwrap_or(automatic);
                let editor_h = requested.clamp(editor_min.min(editor_max), editor_max);
                let editor = Rect::new(
                    self.main_rect.x0 + 8.0,
                    editor_top,
                    self.main_rect.x1 - 8.0,
                    editor_top + editor_h,
                );
                let divider =
                    Rect::new(editor.x0, editor.y1, editor.x1, editor.y1 + QUERY_DIVIDER_H);
                let result_strip =
                    Rect::new(editor.x0, divider.y1, editor.x1, divider.y1 + bands.strip_h);
                let grid = Rect::new(
                    self.main_rect.x0 + 8.0,
                    result_strip.y1,
                    self.main_rect.x1 - 8.0,
                    self.main_rect.y1 - 8.0,
                );
                self.query_layout = QueryLayout {
                    toolbar,
                    editor,
                    divider,
                    result_strip,
                    grid,
                };
                if self.query_resize_menu.is_open() {
                    self.query_resize_menu.reanchor(divider, self.main_rect);
                }
                self.query_measure_dirty |= self.editor.set_measure_width(editor.width());
                self.query_divider.set_geometry(
                    divider,
                    editor.height(),
                    self.query_height_request.is_none(),
                );
                self.editor.set_rect(editor);
                self.grid.set_rect(grid);
            }
            MainTab::Data => {
                let grid_rect = Rect::new(
                    self.main_rect.x0 + 8.0,
                    content_top + bands.toolbar_h,
                    self.main_rect.x1 - 8.0,
                    self.main_rect.y1 - 8.0,
                );
                self.data_grid.set_rect(grid_rect);
            }
            MainTab::Structure => {}
        }
    }

    fn connections_panel_height(&self) -> f64 {
        // Matches the sidebar layout in `draw_sidebar`: the connection rows
        // begin 32px below the sidebar top (just under the CONNECTIONS label),
        // advance 44px each, then the "New connection" button (30px) is drawn
        // directly below, with a 16px gap before the schema browser begins.
        let rows = self.session.connections.len() as f64;
        // When there are no connections the sidebar shows a two-line "No
        // connections yet" hint (52px) in place of the rows.
        let empty_hint = if self.session.connections.is_empty() {
            52.0
        } else {
            0.0
        };
        32.0 + rows * 44.0 + empty_hint + 30.0 + 16.0
    }

    fn request_query_height(&mut self, height: Option<f64>) {
        self.query_height_request = height.map(|height| height.max(0.0));
        self.relayout();
        self.dirty = true;
    }

    fn adjust_query_height(&mut self, delta: f64) {
        let base = self
            .query_height_request
            .unwrap_or_else(|| self.query_layout.editor.height());
        self.request_query_height(Some(base + delta));
    }

    fn open_query_resize_menu(&mut self) {
        self.query_resize_menu
            .open(self.query_layout.divider, self.main_rect);
        self.dirty = true;
    }

    fn apply_query_resize_command(&mut self, command: QueryResizeCommand) {
        match command {
            QueryResizeCommand::Adjust(delta) => self.adjust_query_height(delta),
            QueryResizeCommand::Reset => self.request_query_height(None),
            QueryResizeCommand::OpenMenu => self.open_query_resize_menu(),
        }
    }

    fn drain_query_resize_actions(&mut self) {
        if let Some(command) = self.query_divider.take_command() {
            self.apply_query_resize_command(command);
        }
        if let Some(index) = self.query_resize_menu.take_selection() {
            match index {
                0 => self.adjust_query_height(24.0),
                1 => self.adjust_query_height(-24.0),
                2 => self.request_query_height(None),
                _ => {}
            }
        }
    }

    fn child_sig(&self) -> QueryChildSig {
        QueryChildSig {
            empty: self.session.connections.is_empty(),
            tab: self.session.tab,
            resize_menu: self.session.tab == MainTab::Query && self.query_resize_menu.is_open(),
        }
    }

    /// Whether the sidebar pane should be painted this frame. Once the overlay
    /// flap has fully collapsed it contributes nothing and is skipped.
    fn sidebar_visible(&self) -> bool {
        self.flap.reveal() > 0.001
    }

    /// Whether the hamburger toggle is shown: any time the sidebar is not
    /// fully docked-open (overlay at any reveal, or a collapsing docked pane).
    fn show_toggle(&self) -> bool {
        self.split_layout() == SplitLayout::Overlay || self.flap.reveal() < 0.999
    }

    /// `true` while the flap reveal hasn't settled on its target, so the host
    /// keeps pumping frames for the slide.
    fn flap_animating(&self) -> bool {
        self.flap.is_animating(self.split_layout())
    }

    /// A stable, complete semantic sidebar tree for the app's custom-painted
    /// connection roster and schema browser.
    fn sidebar_accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let connection_count = self.session.connections.len();
        let connections = self
            .session
            .connections
            .iter()
            .enumerate()
            .map(|(index, connection)| {
                let mut children = vec![
                    AccessNode::new(Role::Button)
                        .with_name(&connection.name)
                        .with_description(connection.subtitle())
                        .with_action_target(format!("db.connection:{}", connection.id))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .selected(index == self.session.active && self.session.connected)
                                .busy(index == self.session.active && self.session.busy()),
                        ),
                ];
                if !connection.is_sample {
                    children.push(
                        AccessNode::new(Role::Button)
                            .with_name(format!("Remove {}", connection.name))
                            .with_action_target(format!("db.connection.remove:{}", connection.id))
                            .with_state(AccessState::new().focusable(true)),
                    );
                }
                AccessNode::new(Role::ListItem)
                    .with_hashed_key(connection.id)
                    .with_position(index + 1, connection_count)
                    .with_children(children)
            })
            .collect();
        AccessNode::new(Role::Group)
            .with_name("Database navigator")
            .with_hashed_key("db.sidebar")
            .with_children(vec![
                AccessNode::new(Role::List)
                    .with_name("Connections")
                    .with_hashed_key("db.connections")
                    .with_children(connections),
                AccessNode::new(Role::Button)
                    .with_name("New connection")
                    .with_description("Shortcut: Control N")
                    .with_action_target("db.new-connection")
                    .with_state(AccessState::new().focusable(true)),
            ])
    }

    fn workspace_accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};

        let tabs = MainTab::all()
            .into_iter()
            .map(|tab| {
                AccessNode::new(Role::Tab)
                    .with_name(tab.label())
                    .with_action_target(format!("db.tab:{}", tab.label().to_ascii_lowercase()))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(tab == self.session.tab),
                    )
            })
            .collect();
        let mut panel_children = vec![
            AccessNode::new(Role::Toolbar)
                .with_name("Query tools")
                .with_hashed_key("db.query-toolbar")
                .with_children(vec![
                    AccessNode::new(Role::Button)
                        .with_name("Run query")
                        .with_description("Shortcut: Control Enter")
                        .with_action_target("db.query.run")
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .disabled(!self.session.connected || self.session.busy()),
                        ),
                    AccessNode::new(Role::Button)
                        .with_name("SQL snippets")
                        .with_action_target("db.query.snippets")
                        .with_state(AccessState::new().focusable(true)),
                ]),
            AccessNode::new(Role::MultilineTextInput)
                .with_name("SQL query")
                .with_value(self.editor.value())
                .with_action_target("db.editor.sql")
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.editor.is_focused())
                        .multiline(true),
                ),
        ];
        if let Some(result) = &self.session.query_result {
            panel_children.push(
                AccessNode::new(Role::Table)
                    .with_name("Query results")
                    .with_value(result.message())
                    .with_hashed_key("db.query-results"),
            );
        }
        panel_children.push(
            AccessNode::new(Role::StaticText)
                .with_name(self.status_bubble_label())
                .with_hashed_key("db.status")
                .with_live(Politeness::Polite),
        );
        AccessNode::new(Role::Group)
            .with_name("Database workspace")
            .with_hashed_key("db.workspace")
            .with_children(vec![
                AccessNode::new(Role::TabBar)
                    .with_name("Workspace views")
                    .with_hashed_key("db.tabs")
                    .with_children(tabs),
                AccessNode::new(Role::TabPanel)
                    .with_name(self.session.tab.label())
                    .with_hashed_key("db.tab-panel")
                    .with_children(panel_children),
            ])
    }

    fn full_accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut children = vec![self.sidebar_accessibility(), self.workspace_accessibility()];
        if self.show_toggle() {
            children.push(
                AccessNode::new(Role::ToggleButton)
                    .with_name("Toggle database navigator")
                    .with_action_target("db.sidebar.toggle")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(self.flap.is_open(self.split_layout())),
                    ),
            );
        }
        AccessNode::new(Role::Window)
            .with_name("Database Client")
            .with_description("Collaborative database connections and SQL workspace")
            .with_bounds(self.rect)
            .with_children(children)
    }

    fn select_accessible_tab(&mut self, tab: MainTab) {
        self.session.tab = tab;
        if tab == MainTab::Data {
            self.session.load_table_data();
        }
        self.relayout();
        self.dirty = true;
    }
}

/// Conservative editor-side safety check. It deliberately asks for review on
/// the common data-changing verbs, while harmless `SELECT`/`EXPLAIN` keeps the
/// one-keystroke run flow. The worker remains the source of truth for actual
/// SQL validity and permissions.
#[allow(
    clippy::while_let_on_iterator,
    reason = "the lexer must peek and consume paired delimiters"
)]
fn sql_is_potentially_destructive(sql: &str) -> bool {
    let mut code = String::new();
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '-' && chars.peek() == Some(&'-') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '\n' {
                    code.push(' ');
                    break;
                }
            }
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    code.push(' ');
                    break;
                }
            }
        } else if ch == '\'' || ch == '"' || ch == '`' {
            let quote = ch;
            while let Some(c) = chars.next() {
                if c == quote {
                    if chars.peek() == Some(&quote) {
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            code.push(' ');
        } else {
            code.push(ch);
        }
    }
    let statements = code
        .split(';')
        .filter(|part| !part.trim().is_empty())
        .count();
    if statements != 1 {
        return true;
    }
    let words = code
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>();
    let Some(first) = words.first() else {
        return true;
    };
    if first == "WITH" {
        return true;
    }
    words.iter().any(|word| {
        matches!(
            word.as_str(),
            "ALTER"
                | "CREATE"
                | "DELETE"
                | "DROP"
                | "INSERT"
                | "REPLACE"
                | "TRUNCATE"
                | "UPDATE"
                | "MERGE"
                | "GRANT"
                | "REVOKE"
        )
    })
}

fn write_csv(path: &std::path::Path, result: &crate::model::QueryResult) -> Result<(), String> {
    // The portal owns naming/overwrite UX, but never rely on that as write
    // authority: refuse a raced or pre-existing destination rather than
    // silently replacing somebody else's export.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    let mut write_row = |cells: &[String]| -> Result<(), String> {
        let row = cells
            .iter()
            .map(|cell| csv_field(cell))
            .collect::<Vec<_>>()
            .join(",");
        writeln!(file, "{row}").map_err(|error| error.to_string())
    };
    write_row(&result.columns)?;
    for row in &result.rows {
        let cells = row
            .iter()
            .map(crate::model::Cell::display)
            .collect::<Vec<_>>();
        write_row(&cells)?;
    }
    file.flush().map_err(|error| error.to_string())
}

/// Make a CSV field safe to open in spreadsheet software. A leading formula
/// sigil remains data (prefixed with an apostrophe), including when hidden
/// behind whitespace/control characters; the CSV itself still quotes every
/// field so commas/newlines/quotes remain lossless.
fn csv_field(value: &str) -> String {
    let formula = value
        .trim_start_matches(|c: char| c.is_ascii_whitespace() || c.is_ascii_control())
        .starts_with(['=', '+', '-', '@']);
    let safe = if formula {
        format!("'{value}")
    } else {
        value.to_string()
    };
    format!("\"{}\"", safe.replace('"', "\"\""))
}

impl Widget for DbClientApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::undo::UndoPoints
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[DB_OPERATION_TYPE]
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
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let (pending, retryable) = self.resource_operation?;
        if pending != invocation {
            return None;
        }
        if cancellation_requested {
            if !self.session.cancel_in_flight_query() {
                return Some(tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "db_client.cancel_not_active".into(),
                        message:
                            "The requested query already completed; no newer query was cancelled"
                                .into(),
                        details: None,
                        retryable: false,
                    },
                });
            }
            self.resource_operation = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("Cancellation requested for this query".into()),
            });
        }
        self.session.drain();
        self.drain_export_portal();
        if self.session.busy() || self.export_rx.is_some() {
            return None;
        }
        self.resource_operation = None;
        Some(match &self.session.status {
            RunStatus::Error(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "db_client.operation_failed".into(),
                    message: message.clone(),
                    details: None,
                    retryable,
                },
            },
            _ => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
        })
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        self.drain_query_resize_actions();
        // Accessibility activation is delivered directly to the focused child;
        // drain its semantic action here and enter the same modal flow as a
        // pointer press.
        if self.empty_state.take_action() {
            self.modal.open_new();
            self.dirty = true;
        }
        crate::doc::adopt_shared_schema(state);
        if !self.seeded {
            // Only the demo build seeds a starter query; the live editor
            // opens empty so nothing references a database that isn't there.
            if self.demo && EditorDoc::read_sql(state).is_empty() {
                EditorDoc::set_sql(state, crate::sample::DEFAULT_QUERY);
            }
            self.seeded = true;
        }

        // Fold in any results the background worker has finished (connect
        // outcomes, query result sets, table pages). This is what makes a
        // remote query land on screen without further user input.
        self.session.drain();

        // The live app's saved-connection list is collaborative document
        // state: reconcile the session from `root["connections"]` each frame.
        // The demo keeps its seeded sample roster as view state instead.
        if !self.demo {
            let doc_conns = ConnectionsDoc::read(state);
            self.session.set_connections(doc_conns);
            // Open a connection the local user just added, once it has landed
            // back through the reconcile.
            if let Some(id) = self.pending_connect_id
                && let Some(idx) = self.session.connections.iter().position(|c| c.id == id)
            {
                self.session.active = idx;
                self.session.connect_active();
                self.pending_connect_id = None;
                self.dirty = true;
            }
        }

        self.sync_schema_tree();
        let sql_text = EditorDoc::sql_text(state);
        self.editor.bind_text(sql_text);
        self.editor.pull(state);
        if let Err(error) = self
            .search_index
            .sync_for_app(&self.session, state, !self.demo)
        {
            // Preserve the last known-good catalogue for existing palette
            // entries, but make a rejected rebuild visible rather than
            // silently advertising stale discovery as current.
            self.session.status = RunStatus::Error(error);
            self.dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.empty_state_node = None;
        self.editor_node = None;
        self.query_divider_node = None;
        self.query_resize_menu_node = None;
        let mut children = Vec::new();
        self.tree_node = None;
        if self.schema_in_tree() {
            // Registered for focus, semantics and inspection; its rect is the
            // sidebar region `relayout` computes, so it takes no flow space.
            let child = self.tree.build_layout(taffy, theme, node_map);
            let _ = taffy.set_style(
                child,
                Style {
                    position: Position::Absolute,
                    size: Size {
                        width: length(0.0_f32),
                        height: length(0.0_f32),
                    },
                    ..Default::default()
                },
            );
            self.tree_node = Some(child);
            children.push(child);
        }
        if self.session.connections.is_empty() {
            let child = self.empty_state.build_layout(taffy, theme, node_map);
            self.empty_state_node = Some(child);
            children.push(child);
        } else if self.session.tab == MainTab::Query {
            let child = self.editor.build_layout(taffy, theme, node_map);
            self.editor_node = Some(child);
            children.push(child);
            let child = self.query_divider.build_layout(taffy, theme, node_map);
            self.query_divider_node = Some(child);
            children.push(child);
            if self.query_resize_menu.is_open() {
                let child = self.query_resize_menu.build_layout(taffy, theme, node_map);
                self.query_resize_menu_node = Some(child);
                children.push(child);
            }
        }
        let node = match taffy.new_with_children(
            Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                flex_grow: 1.0,
                ..Default::default()
            },
            &children,
        ) {
            Ok(node) => node,
            // `TaffyTree::new_leaf` only ever returns `Err` when the tree is
            // at capacity; this arm exists so a future upstream change
            // degrades the app instead of aborting the whole session.
            Err(_) => NodeId::from(0u64),
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let w = known.width.unwrap_or(match available.width {
            AvailableSpace::Definite(v) => v,
            _ => 1280.0,
        });
        let h = known.height.unwrap_or(match available.height {
            AvailableSpace::Definite(v) => v,
            _ => 800.0,
        });
        taffy::Size {
            width: w,
            height: h,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // Drop a stale user preference when the window crosses the breakpoint
        // (a resize returns the sidebar to the new layout's default), then
        // advance the eased reveal toward the resolved open/closed target.
        let split = self.split_layout();
        self.flap.reconcile(split);
        let now = ui::time::now();
        let dt = (now - self.last_tick).as_secs_f64();
        self.last_tick = now;
        self.flap.tick(split, dt);
        self.relayout();
        if let Some(empty_state_node) = self.empty_state_node {
            self.empty_state
                .apply_layout(taffy, empty_state_node, origin);
            self.empty_state.apply_layout_external(Rect::new(
                self.main_rect.x0,
                self.main_rect.y0 + self.bands.tabbar_h,
                self.main_rect.x1,
                self.main_rect.y1,
            ));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let p = theme.db();
        // App background.
        scene.fill(Fill::NonZero, transform, p.panel_bg, None, &self.rect);

        // Band heights from the renderer this frame paints with. When they
        // change (first frame, text-scale change) re-lay out now, so the
        // frame paints and hit-tests the same geometry.
        let bands = Bands::measure(tr, theme);
        if bands != self.bands {
            self.bands = bands;
            self.relayout();
        }
        self.grid.measure_bands(tr);
        self.data_grid.measure_bands(tr);

        // Bind result sets to grids when the worker has delivered a fresh one
        // (the session bumps these generations as replies land).
        if self.session.query_gen != self.bound_query_gen {
            if let Some(res) = &self.session.query_result {
                self.grid.bind(res, tr);
            }
            self.bound_query_gen = self.session.query_gen;
        }
        if self.session.data_gen != self.bound_data_gen {
            if let Some(res) = &self.session.data_result {
                self.data_grid.bind(res, tr);
            }
            self.bound_data_gen = self.session.data_gen;
        }

        // Main panel first (it sits behind the overlay flap), then the scrim
        // and the sidebar on top. In docked mode the sidebar occupies its own
        // column and the scrim is empty, so the order is harmless there.
        self.draw_main(scene, transform, tr, theme, presence);

        if self.scrim_rect.width() > 0.0 {
            // Scrim opacity tracks the reveal so it fades in with the flap.
            let alpha = (self.flap.reveal() * 128.0).round().clamp(0.0, 255.0) as u8;
            let scrim = theme
                .tokens
                .colors
                .surfaces
                .scrim
                .with_alpha(f32::from(alpha) / 255.0);
            scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);
        }
        if self.sidebar_visible() {
            self.draw_sidebar(scene, transform, tr, theme);
        }

        // Hamburger toggle over the tab bar whenever the sidebar isn't fully
        // docked-open (collapsed-docked or overlay), so it can be summoned.
        self.draw_sidebar_toggle(scene, transform, tr, theme);

        self.draw_status_bubble(scene, transform, tr, theme);

        if self.session.tab == MainTab::Query && self.query_resize_menu.is_open() {
            self.query_resize_menu
                .draw(scene, transform, tr, theme, presence);
        }

        if self.pending_query_confirmation.is_some() {
            self.draw_query_confirmation_modal(scene, transform, tr, theme);
        }

        // Modal overlay drawn last (top layer).
        if self.modal.is_open() {
            self.modal.draw(scene, transform, tr, theme, self.rect);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one window-event dispatcher; each arm is a short hand-off to the owning pane"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Modal swallows events while open.
        if self.modal.is_open() {
            let mut act = None;
            self.modal.handle_event(event, ctx, &mut act);
            if let Some(profile) = act {
                self.save_connection(profile, ctx.state);
                self.dirty = true;
            }
            return;
        }
        let state = &mut *ctx.state;

        // The first-run action is a real child widget. Give it first refusal
        // while it is visible, then translate its drained semantic action into
        // the existing connection-modal flow.
        if self.session.connections.is_empty()
            && let Some(empty_state_node) = self.empty_state_node
        {
            self.empty_state.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: ctx.modifiers,
                    state,
                    path: ctx.path,
                    handled: ctx.handled,
                    focus_target: ctx.focus_target,
                    node_id: empty_state_node,
                    taffy: ctx.taffy,
                },
            );
            if self.empty_state.take_action() {
                self.modal.open_new();
                self.dirty = true;
                return;
            }
        }

        if self.query_resize_menu.is_open()
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            && self.handle_reserved_shortcut(logical_key, *ctx.modifiers)
        {
            *ctx.handled = true;
            return;
        }

        if self.query_resize_menu.is_open() {
            self.query_resize_menu.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: ctx.modifiers,
                    state,
                    path: ctx.path,
                    handled: ctx.handled,
                    focus_target: ctx.focus_target,
                    node_id: self.query_resize_menu_node.unwrap_or(ctx.node_id),
                    taffy: ctx.taffy,
                },
            );
            self.drain_query_resize_actions();
            if *ctx.handled {
                return;
            }
        }

        if let WindowEvent::CursorMoved { position, .. } = event {
            self.on_cursor_moved(position.x, position.y, ctx.scale_factor);
        }
        if self.route_to_schema_tree(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers,
                state,
                path: ctx.path,
                handled: &mut *ctx.handled,
                focus_target: &mut *ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
        ) {
            return;
        }
        if self.handle_query_divider_pointer(event, ctx.handled, ctx.focus_target) {
            return;
        }

        match event {
            WindowEvent::MouseWheel { delta, .. } => {
                self.on_mouse_wheel(delta, *ctx.modifiers, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_click(state, ctx.handled);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.on_key_pressed(logical_key, *ctx.modifiers, ctx.handled);
            }
            _ => {}
        }

        // Route remaining events to the editor (it checks focus internally).
        // Skip anything the app already consumed — e.g. a sidebar / scrim /
        // toggle click, or an Esc that dismissed the overlay flap — so an open
        // sidebar doesn't leak clicks into the editor behind it.
        if !self.session.connections.is_empty()
            && self.session.tab == MainTab::Query
            && !*ctx.handled
        {
            self.editor.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: ctx.modifiers,
                    state,
                    path: ctx.path,
                    handled: ctx.handled,
                    focus_target: ctx.focus_target,
                    node_id: self.editor_node.unwrap_or(ctx.node_id),
                    taffy: ctx.taffy,
                },
            );
            if self.editor.take_dirty().is_some() {
                self.dirty = true;
            }
        }
    }

    fn name(&self) -> &'static str {
        "DbClientApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        if self.session.tab == MainTab::Query && self.query_resize_menu.is_open() {
            self.query_resize_menu.test_rects()
        } else {
            Vec::new()
        }
    }

    fn is_focusable(&self) -> bool {
        false
    }

    fn is_focused(&self) -> bool {
        !self.session.connections.is_empty()
            && self.session.tab == MainTab::Query
            && self.editor.is_focused()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.session.connections.is_empty() {
            return self.empty_state.cursor_shape(point);
        }
        if self.session.tab == MainTab::Query && self.query_layout.divider.contains(point) {
            return Some(winit::window::CursorIcon::RowResize);
        }
        (self.session.tab == MainTab::Query)
            .then(|| self.editor.cursor_shape(point))
            .flatten()
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }

    fn take_structure_dirty(&mut self) -> bool {
        let measurement_changed = std::mem::take(&mut self.query_measure_dirty);
        let mut child_changed = false;
        for child in self.children_mut() {
            child_changed |= child.take_structure_dirty();
        }
        let sig = self.child_sig();
        let self_changed = self.last_child_sig != Some(sig);
        self.last_child_sig = Some(sig);
        measurement_changed || child_changed || self_changed
    }

    /// The schema browser eases wheel scrolling and row transitions on its
    /// own clock; it is the one child whose per-frame update this root runs.
    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.schema_in_tree() && self.tree.update(state, path)
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming while the sidebar flap is sliding, the worker has
        // a connect/query in flight (so its result lands without further input),
        // OR the floating status bubble is still lingering/fading (so it hides
        // itself without a stray pointer event).
        self.flap_animating()
            || (self.schema_in_tree() && self.tree.needs_animation_frame())
            || self.session.busy()
            || self.status_bubble.needs_frame(ui::time::now())
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.session.connections.is_empty() && self.session.tab == MainTab::Query {
            self.editor
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        (!self.session.connections.is_empty()
            && self.session.tab == MainTab::Query
            && self.editor.is_focused())
        .then(|| self.editor.ime_cursor_rect())
        .flatten()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        (!self.session.connections.is_empty()
            && self.session.tab == MainTab::Query
            && self.editor.is_focused())
        .then(|| self.editor.ime_purpose())
        .flatten()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        if let Some(dialog) = self.modal.accessibility() {
            return Some(dialog);
        }
        if self.session.connections.is_empty() {
            // The retained EmptyState child owns the Group + Button semantics;
            // returning them here too would duplicate the subtree.
            return None;
        }
        Some(self.full_accessibility())
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action;
        let (state, _path) = ui::doc::document(state, path);

        if self.modal.is_open() {
            if !target.starts_with("db.modal.") {
                return false;
            }
            if let Some(profile) = self.modal.perform_semantic_action(target, action) {
                self.save_connection(profile, state);
            }
            self.dirty = true;
            return true;
        }
        if target.starts_with("db.connection") {
            let handled = self.perform_navigator_semantic_action(target, action, state);
            self.dirty |= handled;
            return handled;
        }
        match target {
            "db.new-connection" if matches!(action, Action::Activate | Action::Focus) => {
                self.modal.open_new();
            }
            "db.sidebar.toggle" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.flap.toggle(self.split_layout());
            }
            "db.query.run" if matches!(action, Action::Activate) => self.run_current_query(),
            "db.query.snippets" if matches!(action, Action::Activate) => {
                self.snippets_open = !self.snippets_open;
            }
            "db.editor.sql" if let Action::SetValue(value) = action => {
                self.replace_editor_sql(state, value);
                self.editor.pull(state);
            }
            "db.editor.sql" if matches!(action, Action::Focus | Action::Activate) => {
                self.editor.set_focused(true);
            }
            "db.tab:query" if matches!(action, Action::Activate) => {
                self.select_accessible_tab(MainTab::Query);
            }
            "db.tab:data" if matches!(action, Action::Activate) => {
                self.select_accessible_tab(MainTab::Data);
            }
            "db.tab:structure" if matches!(action, Action::Activate) => {
                self.select_accessible_tab(MainTab::Structure);
            }
            _ => return false,
        }
        self.dirty = true;
        true
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = ui::doc::document(state, path);
        let _ = (action, state, path);
        false
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::new();
        if self.schema_in_tree() {
            children.push(&self.tree);
        }
        if self.session.connections.is_empty() {
            children.push(&self.empty_state);
        } else if self.session.tab == MainTab::Query {
            children.push(&self.editor);
            children.push(&self.query_divider);
            if self.query_resize_menu.is_open() {
                children.push(&self.query_resize_menu);
            }
        }
        children
    }

    /// The schema browser's own `Tree` belongs inside the "Database
    /// navigator" group, the first child of the window node.
    fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
        (child_index == 0 && self.schema_in_tree() && !self.modal.is_open()).then(|| vec![0])
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let in_tree = self.schema_in_tree();
        let mut children: Vec<&mut dyn Widget> = Vec::new();
        if in_tree {
            children.push(&mut self.tree);
        }
        if self.session.connections.is_empty() {
            children.push(&mut self.empty_state);
        } else if self.session.tab == MainTab::Query {
            children.push(&mut self.editor);
            children.push(&mut self.query_divider);
            if self.query_resize_menu.is_open() {
                children.push(&mut self.query_resize_menu);
            }
        }
        children
    }

    fn wants_pointer_capture(&self) -> bool {
        self.query_drag.is_some()
            || self.tree.wants_pointer_capture()
            || (!self.session.connections.is_empty()
                && self.session.tab == MainTab::Query
                && self.editor.wants_pointer_capture())
    }

    fn text_review(&self) -> Option<(usize, String)> {
        (self.session.tab == MainTab::Query)
            .then(|| self.editor.text_review())
            .flatten()
    }
}

impl ui::undo::UndoPoints for DbClientApp {
    fn needs_commit(&self) -> bool {
        self.editor.needs_commit()
    }
    fn on_commit(&mut self) {
        self.editor.on_commit();
    }
    fn get_state(&self) -> Option<String> {
        (self.session.tab == MainTab::Query)
            .then(|| self.editor.get_state())
            .flatten()
    }
    fn set_state(&mut self, state: &str) {
        self.editor.set_state(state);
    }
}

impl ui::search::capability::SearchSource for DbClientApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
    }
}

impl ui::resources::PublishesResources for DbClientApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the workspace door — keep only live query results and in-flight jobs.
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        // Remote directory enumeration never walks a live schema. Deeper
        // catalog entries are reached through the retained SearchCatalog and
        // resolved by their canonical resource ids.
        let resources = if cursor.is_empty() && max_results > 0 {
            self.resources(doc)
        } else {
            Vec::new()
        };
        Some(ui::widget::ActionDirectoryResourcePage {
            resources,
            next_cursor: Vec::new(),
            exhausted: true,
        })
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if self.is_hosted() && id == "database" {
            return None;
        }
        self.publish_resource(doc, id)
            .or_else(|| {
                self.is_hosted()
                    .then(|| {
                        crate::resource::live_extras(self)
                            .into_iter()
                            .find(|resource| resource.id.as_str() == id)
                    })
                    .flatten()
            })
            .or_else(|| self.search_index.resource(id))
    }
}

impl ui::undo::CommittedActions for DbClientApp {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        // Hand the action model's undo bracket to the host so saved-connection
        // CRUD becomes a single undo boundary and replicates to peers.
        self.actions.take_committed()
    }
}

impl DbClientApp {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if !matches!(kind, OperationKind::Effect | OperationKind::View)
            || type_id != DB_OPERATION_TYPE
        {
            return Ok(false);
        }
        let operation: DbClientOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        self.execute_operation(operation)?;
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if !matches!(kind, OperationKind::Effect | OperationKind::View)
            || type_id != DB_OPERATION_TYPE
        {
            return Ok(None);
        }
        if kind == OperationKind::View {
            return self
                .answer_resource_operation(kind, type_id, payload, doc, path)
                .map(|handled| {
                    handled.then(|| {
                        ui::widget::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )
                    })
                });
        }
        if self.resource_operation.is_some() || self.session.busy() {
            return Err("another database operation is already running".into());
        }
        let operation: DbClientOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        // Reconnecting and browsing are idempotent reads/handle opens. An
        // arbitrary SQL statement is deliberately *not* retryable: repeating
        // an INSERT/UPDATE after an uncertain network outcome could duplicate
        // a real side effect.
        let retryable = matches!(
            operation,
            DbClientOperation::Connect { .. } | DbClientOperation::BrowseTable { .. }
        );
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        self.resource_operation = Some((invocation, retryable));
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

impl DbClientApp {
    #[allow(
        clippy::too_many_lines,
        clippy::manual_midpoint,
        reason = "the confirmation is one atomic modal paint stack"
    )]
    fn draw_query_confirmation_modal(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        scene.fill(
            Fill::NonZero,
            t,
            theme.tokens.colors.surfaces.scrim,
            None,
            &self.rect,
        );
        let width = self.rect.width().min(520.0);
        let panel = Rect::new(
            self.rect.x0 + (self.rect.width() - width) / 2.0,
            self.rect.y0 + (self.rect.height() - 238.0) / 2.0,
            self.rect.x0 + (self.rect.width() + width) * 0.5,
            self.rect.y0 + (self.rect.height() + 238.0) * 0.5,
        );
        scene.fill(
            Fill::NonZero,
            t,
            ui::chrome::panel_ground(theme),
            None,
            &panel.to_rounded_rect(10.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(panel, 10.0),
        );
        tr.draw_text(
            scene,
            "Run destructive statement?",
            18.0,
            theme.text_primary,
            t * Affine::translate((panel.x0 + 24.0, panel.y0 + 22.0)),
            None,
        );
        let target = self.session.active_connection().map_or_else(
            || "No active connection".to_string(),
            |c| format!("Connection: {} · {}", c.name, c.subtitle()),
        );
        tr.draw_text(
            scene,
            &target,
            12.0,
            theme.text_dim,
            t * Affine::translate((panel.x0 + 24.0, panel.y0 + 52.0)),
            Some((panel.width() - 48.0) as f32),
        );
        let preview = self
            .pending_query_confirmation
            .as_deref()
            .unwrap_or_default();
        let preview = preview.chars().take(220).collect::<String>();
        let code = Rect::new(
            panel.x0 + 24.0,
            panel.y0 + 74.0,
            panel.x1 - 24.0,
            panel.y0 + 144.0,
        );
        scene.fill(
            Fill::NonZero,
            t,
            theme.bg_app,
            None,
            &code.to_rounded_rect(6.0),
        );
        tr.draw_text(
            scene,
            &preview,
            12.0,
            theme.text_primary,
            t * Affine::translate((code.x0 + 10.0, code.y0 + 10.0)),
            Some((code.width() - 20.0) as f32),
        );
        tr.draw_text(
            scene,
            "This cannot be undone by the database client.",
            11.5,
            theme.tokens.colors.semantic.danger,
            t * Affine::translate((panel.x0 + 24.0, panel.y0 + 162.0)),
            None,
        );
        self.confirm_cancel_btn = Rect::new(
            panel.x1 - 194.0,
            panel.y1 - 48.0,
            panel.x1 - 106.0,
            panel.y1 - 16.0,
        );
        self.confirm_run_btn = Rect::new(
            panel.x1 - 96.0,
            panel.y1 - 48.0,
            panel.x1 - 20.0,
            panel.y1 - 16.0,
        );
        // Both actions paint through the shared face: the neutral one gets
        // its rest edge, the destructive one the danger palette, and the
        // labels centre on their measured line instead of a fixed offset.
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(self.confirm_cancel_btn, "Cancel")
                .radius(6.0)
                .text_size(12.0)
                .hovered(self.confirm_cancel_btn.contains(self.cursor)),
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(self.confirm_run_btn, "Run")
                .danger()
                .radius(6.0)
                .text_size(12.0)
                .hovered(self.confirm_run_btn.contains(self.cursor)),
        );
    }

    fn on_cursor_moved(&mut self, x: f64, y: f64, scale_factor: f64) {
        self.cursor = Point::new(x / scale_factor, y / scale_factor);
        self.run_hover = self.run_btn.contains(self.cursor);
        // Sidebar connection hover.
        self.conn_hover = self
            .conn_rects
            .iter()
            .find(|(_, r)| r.contains(self.cursor))
            .map(|(i, _)| *i);
        // Grid hover.
        if self.session.tab == MainTab::Query {
            self.grid.hover_row = self.grid.row_hit(self.cursor);
        } else if self.session.tab == MainTab::Data {
            self.data_grid.hover_row = self.data_grid.row_hit(self.cursor);
        }
    }

    /// Whether the schema browser is part of the widget tree this frame.
    fn schema_in_tree(&self) -> bool {
        !self.session.connections.is_empty() && self.sidebar_visible()
    }

    /// Offer an event to the schema browser. Pointer input reaches it only
    /// while the sidebar is interactive (so a closed overlay flap lets taps
    /// through); keys reach it only while it holds focus. Returns whether it
    /// consumed the event, after applying any rows it activated.
    fn route_to_schema_tree(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let Some(node) = self.tree_node else {
            return false;
        };
        let pointer = matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. }
        );
        if pointer && !self.sidebar_interactive() && !self.tree.wants_pointer_capture() {
            return false;
        }
        self.tree
            .handle_event(event, &mut ctx.reborrow(ctx.path, node));
        if *ctx.focus_target == Some(node) {
            self.editor.set_focused(false);
            self.query_divider.set_focus(false);
        }
        self.drain_schema_activations();
        if *ctx.handled {
            self.dirty = true;
        }
        *ctx.handled
    }

    /// Apply the rows the browser activated: open a table (a column opens its
    /// table) or toggle a schema.
    fn drain_schema_activations(&mut self) {
        let activated = std::mem::take(&mut *self.schema_activations.borrow_mut());
        for id in activated {
            match schema_nav::resolve(self.tree.model(), id) {
                Some(Activation::Open(relation)) => {
                    self.session.select_relation(relation.clone());
                    self.show_relation(&relation);
                    // On a narrow window the sidebar is an overlay; pick a
                    // table and the flap steps aside to reveal its rows.
                    if self.split_layout() == SplitLayout::Overlay {
                        self.flap.close();
                    }
                    self.relayout();
                }
                Some(Activation::Toggle(id)) => self.tree.toggle_expand(id),
                None => {}
            }
        }
    }

    /// Mark the relation on screen in the browser: the current-row rail, and
    /// the selection so keyboard browsing starts there.
    fn show_relation(&mut self, relation: &crate::model::RelationRef) {
        let id = schema_nav::relation_id(relation);
        self.tree.set_current(Some(id));
        self.tree.selection_mut().select_only(id);
    }

    /// Rebuild the browser's model when the catalog changed shape.
    fn sync_schema_tree(&mut self) {
        let signature = schema_nav::signature(&self.session.schema);
        if signature != self.schema_signature {
            self.schema_signature = signature;
            schema_nav::refresh(&mut self.tree, &self.session.schema);
        }
        let current = self
            .session
            .selected_relation
            .as_ref()
            .map(schema_nav::relation_id);
        if self.tree.current() != current {
            match self.session.selected_relation.clone() {
                Some(relation) => self.show_relation(&relation),
                None => self.tree.set_current(None),
            }
        }
    }

    fn handle_query_divider_pointer(
        &mut self,
        event: &WindowEvent,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        if matches!(event, WindowEvent::Focused(false)) {
            self.query_drag = None;
            return false;
        }
        if self.session.connections.is_empty() || self.session.tab != MainTab::Query {
            return false;
        }
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.query_layout.divider.contains(self.cursor) => {
                self.query_drag = Some((self.cursor.y, self.query_layout.editor.height()));
                self.editor.set_focused(false);
                self.query_divider.set_focus(true);
                *focus_target = self.query_divider_node;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.query_layout.divider.contains(self.cursor) => {
                self.editor.set_focused(false);
                self.query_divider.set_focus(true);
                *focus_target = self.query_divider_node;
                self.open_query_resize_menu();
            }
            WindowEvent::CursorMoved { .. } if self.query_drag.is_some() => {
                let (origin_y, origin_height) = self.query_drag.unwrap_or_default();
                self.request_query_height(Some(origin_height + self.cursor.y - origin_y));
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.query_drag.is_some() => {
                self.query_drag = None;
            }
            _ => return false,
        }
        *handled = true;
        true
    }

    fn on_mouse_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        modifiers: Modifiers,
        handled: &mut bool,
    ) {
        let (dx_px, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * 40.0, f64::from(*y)),
            MouseScrollDelta::PixelDelta(pos) => (pos.x, pos.y / 24.0),
        };
        let step = (-dy * 3.0) as isize;
        // Shift+wheel scrolls horizontally, as does a native x-delta.
        let shift = modifiers.state().shift_key();
        if self.session.tab == MainTab::Query && self.grid.rect().contains(self.cursor) {
            if shift {
                self.grid.scroll_h(-dy * 40.0);
            } else {
                let total = self
                    .session
                    .query_result
                    .as_ref()
                    .map_or(0, |r| r.rows.len());
                self.grid.scroll(step, total);
                if dx_px != 0.0 {
                    self.grid.scroll_h(dx_px);
                }
            }
            *handled = true;
        } else if self.session.tab == MainTab::Data && self.data_grid.rect().contains(self.cursor) {
            if shift {
                self.data_grid.scroll_h(-dy * 40.0);
            } else {
                let total = self
                    .session
                    .data_result
                    .as_ref()
                    .map_or(0, |r| r.rows.len());
                self.data_grid.scroll(step, total);
                if dx_px != 0.0 {
                    self.data_grid.scroll_h(dx_px);
                }
            }
            *handled = true;
        }
    }

    fn on_key_pressed(&mut self, logical_key: &Key, modifiers: Modifiers, handled: &mut bool) {
        if self.handle_reserved_shortcut(logical_key, modifiers) {
            *handled = true;
            return;
        }
        if self.session.tab == MainTab::Query && self.query_divider.handle_key(logical_key) {
            self.drain_query_resize_actions();
            *handled = true;
            return;
        }
        // Esc closes the snippet menu, then dismisses an open overlay flap
        // (in docked mode Esc leaves the sidebar untouched).
        if let Key::Named(NamedKey::Escape) = logical_key {
            if self.pending_query_confirmation.take().is_some() {
                self.dirty = true;
                *handled = true;
                return;
            }
            if self.snippets_open {
                self.snippets_open = false;
                *handled = true;
                return;
            }
            if self.split_layout() == SplitLayout::Overlay
                && self.flap.is_open(SplitLayout::Overlay)
            {
                self.flap.close();
                self.dirty = true;
                *handled = true;
            }
        }
    }

    fn handle_reserved_shortcut(&mut self, logical_key: &Key, modifiers: Modifiers) -> bool {
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        if !ctrl {
            return false;
        }
        if matches!(logical_key, Key::Named(NamedKey::Enter)) {
            if let Some(sql) = self.pending_query_confirmation.take() {
                self.run_sql_effect(sql);
            } else {
                self.run_current_query();
            }
            return true;
        }
        if let Key::Character(c) = logical_key
            && c.eq_ignore_ascii_case("n")
        {
            self.modal.open_new();
            return true;
        }
        false
    }

    /// Whether sidebar widgets (connection rows, schema tree, new-connection
    /// button) should accept clicks: always when docked, only while the flap
    /// is open when it's an overlay (so taps fall through to the content
    /// behind a closed/collapsed flap).
    fn sidebar_interactive(&self) -> bool {
        match self.split_layout() {
            SplitLayout::Docked => self.flap.reveal() > 0.5,
            SplitLayout::Overlay => self.flap.is_open(SplitLayout::Overlay),
        }
    }

    fn on_click(&mut self, state: &mut AutoCommit, handled: &mut bool) {
        let c = self.cursor;
        if self.click_chrome(c, handled)
            || self.click_sidebar(c, state, handled)
            || self.click_tab_bar(c, handled)
            || self.click_query_toolbar(c, state, handled)
            || self.click_data_tab(c, handled)
        {
            return;
        }

        // Click into the editor body focuses it (editor handles caret).
        if self.session.tab == MainTab::Query && self.editor.rect().contains(c) {
            self.query_divider.set_focus(false);
            self.editor.set_focused(true);
        } else {
            self.editor.set_focused(false);
        }
    }

    /// Hamburger toggle and scrim dismiss — chrome that isn't gated on which
    /// main tab is active.
    fn click_chrome(&mut self, c: Point, handled: &mut bool) -> bool {
        // Hamburger toggle (summon / dismiss the sidebar).
        if self.show_toggle() && self.toggle_btn.contains(c) {
            self.flap.toggle(self.split_layout());
            self.dirty = true;
            *handled = true;
            return true;
        }

        // Scrim click while the overlay flap is open dismisses it (the click
        // is consumed so it doesn't also fall through to the content).
        if self.scrim_rect.width() > 0.0
            && self.scrim_rect.contains(c)
            && !self.sidebar_rect.contains(c)
        {
            self.flap.close();
            self.dirty = true;
            *handled = true;
            return true;
        }

        false
    }

    /// "New connection", per-row delete and connection-row selection —
    /// only when the sidebar is interactive.
    fn click_sidebar(&mut self, c: Point, state: &mut AutoCommit, handled: &mut bool) -> bool {
        if !self.sidebar_interactive() {
            return false;
        }

        // "New connection" button.
        if self.new_conn_btn.contains(c) {
            self.modal.open_new();
            *handled = true;
            return true;
        }

        // Per-row delete ("×") button, shown on hover — takes priority over
        // selecting the row it sits on.
        for (idx, dr) in self.conn_delete_rects.clone() {
            if dr.contains(c) {
                self.remove_connection(idx, state);
                self.dirty = true;
                *handled = true;
                return true;
            }
        }

        // Connection rows.
        for (idx, r) in self.conn_rects.clone() {
            if r.contains(c) {
                if let Some(connection) = self.session.connections.get(idx) {
                    // Use the shared effect implementation for both selecting
                    // and retrying an already-active disconnected profile.
                    let _ = self.execute_operation(DbClientOperation::Connect {
                        connection_id: connection.id.to_string(),
                    });
                }
                self.dirty = true;
                *handled = true;
                return true;
            }
        }

        false
    }

    fn click_tab_bar(&mut self, c: Point, handled: &mut bool) -> bool {
        for (tab, r) in self.tab_rects.clone() {
            if r.contains(c) {
                self.session.tab = tab;
                self.query_divider.set_focus(false);
                self.query_resize_menu.close();
                if tab == MainTab::Data {
                    self.session.load_table_data();
                }
                self.relayout();
                self.dirty = true;
                *handled = true;
                return true;
            }
        }
        false
    }

    /// Run/snippet toolbar buttons, the snippet menu, and header-click sort —
    /// all Query-tab only.
    fn click_query_toolbar(
        &mut self,
        c: Point,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        if self.session.tab != MainTab::Query {
            return false;
        }

        if self.pending_query_confirmation.is_some() {
            if self.confirm_cancel_btn.contains(c) {
                self.pending_query_confirmation = None;
                self.dirty = true;
                *handled = true;
                return true;
            }
            if self.confirm_run_btn.contains(c) {
                if let Some(sql) = self.pending_query_confirmation.take() {
                    self.run_sql_effect(sql);
                }
                *handled = true;
                return true;
            }
        }

        if self.run_btn.contains(c) {
            self.run_current_query();
            *handled = true;
            return true;
        }
        if self.snippet_btn.contains(c) {
            self.snippets_open = !self.snippets_open;
            *handled = true;
            return true;
        }
        // Snippet menu items.
        if self.snippets_open {
            let items = crate::sample::starter_queries();
            let mut y = self.snippet_btn.y1 + 2.0;
            for (_, sql) in items {
                let item = Rect::new(
                    self.snippet_btn.x0,
                    y,
                    self.snippet_btn.x0 + 240.0,
                    y + 28.0,
                );
                if item.contains(c) {
                    self.load_snippet(sql, state);
                    *handled = true;
                    return true;
                }
                y += 28.0;
            }
            // Click elsewhere closes the menu.
            self.snippets_open = false;
        }

        // Header click → sort.
        if let Some(col) = self.grid.header_hit(c)
            && let Some(res) = &self.session.query_result
        {
            self.grid.sort_by(col, res);
            *handled = true;
            return true;
        }

        false
    }

    /// Data tab grid header-click sort.
    fn click_data_tab(&mut self, c: Point, handled: &mut bool) -> bool {
        if self.session.tab == MainTab::Data
            && let Some(col) = self.data_grid.header_hit(c)
            && let Some(res) = &self.session.data_result
        {
            self.data_grid.sort_by(col, res);
            *handled = true;
            return true;
        }
        false
    }
}

// ---- drawing helpers ----

impl DbClientApp {
    fn draw_sidebar(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let p = theme.db();
        let r = self.sidebar_rect;
        scene.fill(Fill::NonZero, t, p.sidebar_bg, None, &r);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::surface::edge_on(p.sidebar_bg),
            None,
            &Line::new(Point::new(r.x1, r.y0), Point::new(r.x1, r.y1)),
        );

        // Section label. The OS draws the window/app chrome, so the sidebar no
        // longer repeats an app-name wordmark here — the connection list sits at
        // the top behind this functional heading.
        tr.draw_text(
            scene,
            "CONNECTIONS",
            10.0,
            theme.text_dim,
            t * Affine::translate((r.x0 + 14.0, r.y0 + 14.0)),
            None,
        );

        // Connection rows.
        self.conn_rects.clear();
        self.conn_delete_rects.clear();
        let mut y = r.y0 + 32.0;
        let connections = self.session.connections.clone();
        for (idx, conn) in connections.iter().enumerate() {
            self.draw_connection_row(&mut PaintCtx::new(scene, t, tr, theme), idx, conn, y);
            y += 44.0;
        }

        self.draw_connections_footer(scene, t, tr, theme, y);

        // Schema browser (its own region; database → schema → table → columns).
        self.tree
            .draw(scene, t, tr, theme, &ui::presence::PresenceCtx::empty());
    }

    fn draw_connection_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        idx: usize,
        conn: &crate::model::ConnectionProfile,
        y: f64,
    ) {
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = ctx.reborrow();
        let p = theme.db();
        let r = self.sidebar_rect;
        let row = Rect::new(r.x0 + SIDEBAR_INSET, y, r.x1 - SIDEBAR_INSET, y + 40.0);
        self.conn_rects.push((idx, row));
        let is_active = idx == self.session.active;
        if is_active {
            scene.fill(
                Fill::NonZero,
                t,
                p.selection,
                None,
                &row.to_rounded_rect(6.0),
            );
        } else if self.conn_hover == Some(idx) {
            scene.fill(Fill::NonZero, t, p.hover, None, &row.to_rounded_rect(6.0));
        }

        // Engine chip.
        let chip = Rect::new(row.x0 + 8.0, y + 9.0, row.x0 + 38.0, y + 31.0).to_rounded_rect(5.0);
        let chip_color = engine_color(conn.engine, theme);
        scene.fill(Fill::NonZero, t, chip_color, None, &chip);
        tr.draw_text(
            scene,
            conn.engine.glyph(),
            10.0,
            theme.tokens.colors.on_surface(chip_color).ink,
            t * Affine::translate((row.x0 + 11.0, y + 13.0)),
            None,
        );

        tr.draw_text(
            scene,
            &conn.name,
            13.0,
            theme.text_primary,
            t * Affine::translate((row.x0 + 46.0, y + 6.0)),
            Some((row.width() - 56.0) as f32),
        );
        tr.draw_text(
            scene,
            &conn.subtitle(),
            10.5,
            theme.text_dim,
            t * Affine::translate((row.x0 + 46.0, y + 22.0)),
            Some((row.width() - 56.0) as f32),
        );

        // On hover, offer a delete ("×") button; otherwise show the
        // connection status dot. The sample connection can't be deleted.
        let hovered = self.conn_hover == Some(idx);
        if hovered && !conn.is_sample {
            self.draw_connection_delete_button(scene, t, theme, idx, row, y);
        } else {
            let dot_c = if conn.connected {
                theme.tokens.colors.semantic.success
            } else if conn.is_remote() {
                theme.tokens.colors.semantic.warning
            } else {
                theme.text_dim
            };
            scene.fill(
                Fill::NonZero,
                t,
                dot_c,
                None,
                &vello::kurbo::Circle::new(Point::new(row.x1 - 12.0, y + 12.0), 3.5),
            );
        }
    }

    fn draw_connection_delete_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        idx: usize,
        row: Rect,
        y: f64,
    ) {
        let p = theme.db();
        let del = Rect::new(row.x1 - 26.0, y + 4.0, row.x1 - 6.0, y + 24.0);
        self.conn_delete_rects.push((idx, del));
        let center = del.center();
        let over = del.contains(self.cursor);
        if over {
            scene.fill(Fill::NonZero, t, p.hover, None, &del.to_rounded_rect(5.0));
        }
        let col = if over {
            theme.tokens.colors.semantic.danger
        } else {
            theme.text_dim
        };
        let half = 4.0;
        scene.stroke(
            &Stroke::new(1.5),
            t,
            col,
            None,
            &Line::new(
                Point::new(center.x - half, center.y - half),
                Point::new(center.x + half, center.y + half),
            ),
        );
        scene.stroke(
            &Stroke::new(1.5),
            t,
            col,
            None,
            &Line::new(
                Point::new(center.x + half, center.y - half),
                Point::new(center.x - half, center.y + half),
            ),
        );
    }

    /// Empty-state hint (when no connections are saved) and the "New
    /// connection" button beneath the row list.
    fn draw_connections_footer(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        y: f64,
    ) {
        let r = self.sidebar_rect;
        let mut y = y;
        if self.session.connections.is_empty() {
            tr.draw_text(
                scene,
                "No connections yet",
                12.5,
                theme.text_primary,
                t * Affine::translate((r.x0 + 14.0, y + 2.0)),
                None,
            );
            tr.draw_text(
                scene,
                "Add one to get started.",
                11.0,
                theme.text_dim,
                t * Affine::translate((r.x0 + 14.0, y + 22.0)),
                None,
            );
            y += 52.0;
        }

        // "New connection" button.
        let btn = Rect::new(r.x0 + SIDEBAR_INSET, y, r.x1 - SIDEBAR_INSET, y + 30.0);
        self.new_conn_btn = btn;
        let btn_color = if btn.contains(self.cursor) {
            theme.accent_hover
        } else {
            theme.accent_primary
        };
        scene.fill(Fill::NonZero, t, btn_color, None, &btn.to_rounded_rect(6.0));
        let label = "+  New connection";
        let (_, lh) = tr.measure(label, 12.0, None);
        tr.draw_text(
            scene,
            label,
            12.0,
            theme
                .tokens
                .colors
                .on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            t * Affine::translate((btn.x0 + 12.0, btn.y0 + (btn.height() - f64::from(lh)) / 2.0)),
            None,
        );
    }

    fn draw_main(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let bar = self.draw_tab_bar(scene, t, tr, theme);

        // Empty state: nothing to query until the user adds a connection.
        if self.session.connections.is_empty() {
            self.empty_state.apply_layout_external(Rect::new(
                self.main_rect.x0,
                bar.y1,
                self.main_rect.x1,
                self.main_rect.y1,
            ));
            self.empty_state.draw(scene, t, tr, theme, presence);
            return;
        }

        match self.session.tab {
            MainTab::Query => self.draw_query_tab(scene, t, tr, theme, presence),
            MainTab::Data => self.draw_data_tab(scene, t, tr, theme),
            MainTab::Structure => self.draw_structure_tab(scene, t, tr, theme),
        }
    }

    fn draw_tab_bar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Rect {
        let p = theme.db();
        // Tab bar.
        let bands = self.bands;
        let bar = Rect::new(
            self.main_rect.x0,
            self.main_rect.y0,
            self.main_rect.x1,
            self.main_rect.y0 + bands.tabbar_h,
        );
        scene.fill(Fill::NonZero, t, p.header_bg, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::surface::edge_on(p.header_bg),
            None,
            &Line::new(Point::new(bar.x0, bar.y1), Point::new(bar.x1, bar.y1)),
        );

        self.tab_rects.clear();
        // Reserve space for the hamburger toggle at the left of the tab bar
        // whenever the sidebar isn't fully docked-open.
        let toggle_inset = if self.show_toggle() {
            bands.toggle + 14.0
        } else {
            0.0
        };
        let mut x = bar.x0 + 8.0 + toggle_inset;
        for tab in MainTab::all() {
            let label = tab.label();
            let (lw, _) = tr.measure(label, 13.0, None);
            let w = f64::from(lw) + 28.0;
            let tab_rect = Rect::new(x, bar.y0 + bands.inset, x + w, bar.y1 - bands.inset);
            self.tab_rects.push((tab, tab_rect));
            let active = tab == self.session.tab;
            if active {
                scene.fill(
                    Fill::NonZero,
                    t,
                    p.selection,
                    None,
                    &tab_rect.to_rounded_rect(6.0),
                );
            } else if tab_rect.contains(self.cursor) {
                scene.fill(
                    Fill::NonZero,
                    t,
                    p.hover,
                    None,
                    &tab_rect.to_rounded_rect(6.0),
                );
            }
            let color = if active {
                theme.text_primary
            } else {
                theme.text_dim
            };
            let (_, lh) = tr.measure(label, 13.0, None);
            tr.draw_text(
                scene,
                label,
                13.0,
                color,
                t * Affine::translate((
                    x + 14.0,
                    tab_rect.y0 + (tab_rect.height() - f64::from(lh)) / 2.0,
                )),
                None,
            );
            x += w + 4.0;
        }

        // Selected-table breadcrumb on the right of the tab bar.
        if let Some(relation) = &self.session.selected_relation {
            let crumb = format!("{} · {}", relation.schema, relation.name);
            let (cw, ch) = tr.measure(&crumb, 12.0, None);
            tr.draw_text(
                scene,
                &crumb,
                12.0,
                theme.text_dim,
                t * Affine::translate((
                    bar.x1 - f64::from(cw) - 14.0,
                    bar.y0 + (bar.height() - f64::from(ch)) / 2.0,
                )),
                None,
            );
        }

        bar
    }

    /// Hamburger / close toggle at the left of the tab bar. Shown whenever the
    /// sidebar isn't fully docked-open, so a collapsed sidebar can be summoned
    /// and an overlay sidebar dismissed. Drawn over everything (after the
    /// sidebar) so it stays reachable while the flap is open. Records its rect
    /// in `toggle_btn` for hit-testing; clears it (to `ZERO`) when hidden.
    fn draw_sidebar_toggle(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        _tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.show_toggle() {
            self.toggle_btn = Rect::ZERO;
            return;
        }
        let p = theme.db();
        let bar_y0 = self.main_rect.y0;
        let bx = self.main_rect.x0 + 10.0;
        let size = self.bands.toggle;
        let by = bar_y0 + (self.bands.tabbar_h - size) / 2.0;
        let btn = Rect::new(bx, by, bx + size, by + size);
        self.toggle_btn = btn;

        let bg = if btn.contains(self.cursor) {
            p.hover
        } else {
            p.header_bg
        };
        ui::surface::card(scene, t, btn, 6.0, bg);

        let cx = btn.x0 + size / 2.0;
        let cy = btn.y0 + size / 2.0;
        let open = self.flap.reveal() > 0.5;
        if open {
            // Close "X" while the sidebar is showing.
            let r = 5.0;
            scene.stroke(
                &Stroke::new(1.6),
                t,
                theme.text_primary,
                None,
                &Line::new(Point::new(cx - r, cy - r), Point::new(cx + r, cy + r)),
            );
            scene.stroke(
                &Stroke::new(1.6),
                t,
                theme.text_primary,
                None,
                &Line::new(Point::new(cx + r, cy - r), Point::new(cx - r, cy + r)),
            );
        } else {
            // Three-line hamburger when collapsed.
            let half = 6.0;
            for i in 0..3 {
                let ly = cy - 5.0 + f64::from(i) * 5.0;
                scene.stroke(
                    &Stroke::new(1.6),
                    t,
                    theme.text_primary,
                    None,
                    &Line::new(Point::new(cx - half, ly), Point::new(cx + half, ly)),
                );
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the editor, result status, confirmation and grid need one ordered paint stack"
    )]
    #[expect(
        clippy::similar_names,
        reason = "`*_w`/`*_h` pairs are the measured width and height of the same label"
    )]
    fn draw_query_tab(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let p = theme.db();
        // Toolbar.
        let toolbar = self.query_layout.toolbar;
        scene.fill(Fill::NonZero, t, p.panel_bg, None, &toolbar);

        // Run button. Both toolbar controls size themselves from their
        // measured labels and centre those labels on the measured line
        // height, so "Run" and "Snippets" share one centre line instead of
        // two different hand-tuned offsets.
        let run_label = "▶  Run";
        let run_hint = "⌘↵";
        let (run_lw, run_lh) = tr.measure(run_label, 13.0, None);
        let (hint_w, hint_h) = tr.measure(run_hint, 12.0, None);
        let run_w = (14.0 + f64::from(run_lw) + 16.0 + f64::from(hint_w) + 14.0).max(RUN_MIN_W);
        let run = Rect::new(
            toolbar.x0 + 10.0,
            toolbar.y0 + self.bands.inset,
            toolbar.x0 + 10.0 + run_w,
            toolbar.y1 - self.bands.inset,
        );
        self.run_btn = run;
        let run_color = if self.run_hover {
            theme.accent_hover
        } else {
            theme.accent_primary
        };
        scene.fill(Fill::NonZero, t, run_color, None, &run.to_rounded_rect(6.0));
        tr.draw_text(
            scene,
            run_label,
            13.0,
            theme
                .tokens
                .colors
                .on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            t * Affine::translate((
                run.x0 + 14.0,
                run.y0 + (run.height() - f64::from(run_lh)) / 2.0,
            )),
            None,
        );
        tr.draw_text(
            scene,
            run_hint,
            12.0,
            theme.tokens.colors.on_surface(run_color).ink_dim,
            t * Affine::translate((
                run.x1 - 14.0 - f64::from(hint_w),
                run.y0 + (run.height() - f64::from(hint_h)) / 2.0,
            )),
            None,
        );

        // Snippets button.
        let snip_label = "Snippets ▾";
        let (snip_lw, snip_lh) = tr.measure(snip_label, 12.5, None);
        let snip_w = (12.0 + f64::from(snip_lw) + 12.0).max(SNIPPETS_MIN_W);
        let snip = Rect::new(
            run.x1 + 8.0,
            toolbar.y0 + self.bands.inset,
            run.x1 + 8.0 + snip_w,
            toolbar.y1 - self.bands.inset,
        );
        self.snippet_btn = snip;
        let snip_color = if snip.contains(self.cursor) || self.snippets_open {
            p.hover
        } else {
            theme.bg_primary
        };
        ui::surface::card(scene, t, snip, 6.0, snip_color);
        tr.draw_text(
            scene,
            snip_label,
            12.5,
            theme.text_primary,
            t * Affine::translate((
                snip.x0 + 12.0,
                snip.y0 + (snip.height() - f64::from(snip_lh)) / 2.0,
            )),
            None,
        );

        // Editor.
        self.editor.draw_embedded(scene, t, tr, theme);
        self.query_divider.draw(scene, t, tr, theme, presence);

        // Result message strip between editor and grid.
        let strip_y = self.query_layout.result_strip.y0;
        self.confirm_cancel_btn = Rect::ZERO;
        self.confirm_run_btn = Rect::ZERO;
        if self.pending_query_confirmation.is_none() {
            let (msg, color) = match &self.session.status {
                RunStatus::Ok(m) => (m.clone(), theme.tokens.colors.semantic.success),
                RunStatus::Error(m) if self.session.query_result.is_some() => (
                    format!("✕ Query failed — previous result remains visible: {m}"),
                    theme.tokens.colors.semantic.danger,
                ),
                RunStatus::Error(m) => (format!("✕ {m}"), theme.tokens.colors.semantic.danger),
                RunStatus::Busy(m) => (m.clone(), theme.accent_primary),
                RunStatus::Idle => ("Ready".to_string(), theme.text_dim),
            };
            let strip_h = self.query_layout.result_strip.height();
            let (_, mh) = tr.measure(&msg, 12.0, None);
            tr.draw_text(
                scene,
                &msg,
                12.0,
                color,
                t * Affine::translate((
                    self.main_rect.x0 + 12.0,
                    strip_y + (strip_h - f64::from(mh)) / 2.0,
                )),
                None,
            );
        }

        // Grid.
        if let Some(res) = &self.session.query_result {
            // The status line is real text, not a decoration: reserve its
            // scaled line box so 200% text never paints underneath the result
            // header. Relayout establishes the ordinary geometry; this small
            // draw-time correction follows the active theme scale.
            let grid = self.grid.rect();
            let top = self.editor.rect().y1 + 8.0 + 20.0 * f64::from(theme.text_scale);
            self.grid
                .set_rect(Rect::new(grid.x0, top.min(grid.y1), grid.x1, grid.y1));
            self.grid.draw(scene, t, tr, theme, res);
        }

        // Snippet dropdown (drawn last so it overlays the grid).
        if self.snippets_open {
            self.draw_snippet_menu(scene, t, tr, theme);
        }
    }

    fn draw_snippet_menu(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let p = theme.db();
        let items = crate::sample::starter_queries();
        let menu = Rect::new(
            self.snippet_btn.x0,
            self.snippet_btn.y1 + 2.0,
            self.snippet_btn.x0 + 240.0,
            self.snippet_btn.y1 + 2.0 + items.len() as f64 * 28.0,
        );
        // The snippets menu is a Panel: panel ground plus its derived border.
        scene.fill(
            Fill::NonZero,
            t,
            ui::chrome::panel_ground(theme),
            None,
            &menu.to_rounded_rect(8.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(menu, 8.0),
        );
        let mut y = menu.y0;
        for (name, _) in items {
            let item = Rect::new(menu.x0, y, menu.x1, y + 28.0);
            if item.contains(self.cursor) {
                scene.fill(Fill::NonZero, t, p.hover, None, &item);
            }
            let (_, nh) = tr.measure(name, 12.0, None);
            tr.draw_text(
                scene,
                name,
                12.0,
                theme.text_primary,
                t * Affine::translate((item.x0 + 12.0, y + (28.0 - f64::from(nh)) / 2.0)),
                None,
            );
            y += 28.0;
        }
    }

    fn draw_data_tab(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let p = theme.db();
        let bands = self.bands;
        let toolbar = Rect::new(
            self.main_rect.x0,
            self.main_rect.y0 + bands.tabbar_h,
            self.main_rect.x1,
            self.main_rect.y0 + bands.tabbar_h + bands.toolbar_h,
        );
        scene.fill(Fill::NonZero, t, p.panel_bg, None, &toolbar);

        // Both toolbar messages centre on the toolbar's measured line rather
        // than sitting at fixed 11/12 px offsets that only fit 1× text.
        let centred = |tr: &mut TextRenderer, text: &str, size: f32| {
            let (_, lh) = tr.measure(text, size, None);
            toolbar.y0 + (toolbar.height() - f64::from(lh)) / 2.0
        };
        if let (Some(relation), Some(res)) =
            (&self.session.selected_relation, &self.session.data_result)
        {
            let total = self.session.selected_table_rows();
            let from = self.session.data_offset + 1;
            let to = (self.session.data_offset + res.rows.len()).min(total as usize);
            let info = format!(
                "{}.{}   ·   rows {from}–{to} of {total}",
                relation.schema, relation.name
            );
            let iy = centred(tr, &info, 13.0);
            tr.draw_text(
                scene,
                &info,
                13.0,
                theme.text_primary,
                t * Affine::translate((toolbar.x0 + 12.0, iy)),
                None,
            );
            self.data_grid.draw(scene, t, tr, theme, res);
        } else {
            let hint = "Select a table in the schema browser to view its rows.";
            let hy = centred(tr, hint, 14.0);
            tr.draw_text(
                scene,
                hint,
                14.0,
                theme.text_dim,
                t * Affine::translate((toolbar.x0 + 12.0, hy)),
                None,
            );
        }
    }

    fn draw_structure_tab(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let content = Rect::new(
            self.main_rect.x0 + 8.0,
            self.main_rect.y0 + self.bands.tabbar_h + 8.0,
            self.main_rect.x1 - 8.0,
            self.main_rect.y1 - 8.0,
        );
        let Some(table) = self.session.selected_table_info().cloned() else {
            tr.draw_text(
                scene,
                "Select a table to inspect its structure.",
                14.0,
                theme.text_dim,
                t * Affine::translate((content.x0 + 8.0, content.y0 + 8.0)),
                None,
            );
            return;
        };
        crate::structure::draw_structure(scene, t, tr, theme, content, &table);
    }

    /// Compact one-line summary for the floating status bubble: the active
    /// connection (engine · name · table count, or "No connection") followed by
    /// the current run status ("ready" / "working…" / "error" / the last Ok
    /// message). Combines what the old two-ended status strip showed.
    fn status_bubble_label(&self) -> String {
        let conn = match self.session.active_connection() {
            Some(conn) => format!(
                "{}  ·  {}  ·  {} tables",
                conn.engine.label(),
                conn.name,
                self.session.schema.table_count()
            ),
            None => "No connection".to_string(),
        };
        let run = match &self.session.status {
            RunStatus::Ok(m) => m.clone(),
            RunStatus::Error(_) => "error".to_string(),
            RunStatus::Busy(_) => "working…".to_string(),
            RunStatus::Idle => "ready".to_string(),
        };
        format!("{conn}  ·  {run}")
    }

    /// Draw the transient floating status bubble at the bottom-right of the
    /// window — the reclaimed home of the old permanent bottom status strip. It
    /// shows on a status change or while the pointer hovers the bottom-right
    /// band, then fades. All colours come from the theme (via `draw_pill`).
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        let label = self.status_bubble_label();
        // Poke only when the summary actually changes (a connect, a query
        // completing, a table-count tick) — draw runs every frame, so poking
        // unconditionally would pin the bubble open forever.
        if self.last_status_label.as_deref() != Some(label.as_str()) {
            self.status_bubble.poke(now);
            self.last_status_label = Some(label.clone());
        }
        let text_size = 11.5f32;
        let (w, h) = ui::status_bubble::pill_size(&label, tr, text_size, 0.0);
        let margin = 12.0;
        let rect = Rect::new(
            self.rect.x1 - margin - w,
            self.rect.y1 - margin - h,
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        // Reveal zone: the bottom-right corner band (plus the pill), so nudging
        // the pointer there re-surfaces the status on demand.
        let zone = Rect::new(rect.x0 - 40.0, rect.y0 - 24.0, self.rect.x1, self.rect.y1);
        let over = self.shots_status || zone.contains(self.cursor);
        self.status_bubble.set_hover(over, now);
        let alpha = self.status_bubble.alpha(now);
        if alpha <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            alpha,
            t,
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, t, tr, theme, rect, &label, text_size);
        scene.pop_layer();
    }
}

fn engine_color(engine: Engine, theme: &Theme) -> Color {
    let sem = &theme.tokens.colors.semantic;
    match engine {
        Engine::Sqlite => sem.info,
        Engine::Postgres => theme.accent_primary,
        Engine::MySql => sem.warning,
    }
}

#[cfg(test)]
mod editor_sql_tests {
    use super::*;

    #[test]
    fn a_standalone_sql_edit_is_submitted_through_the_action() {
        let mut app = DbClientApp::new();
        assert!(!app.actions.is_hosted());
        let mut doc = automerge::AutoCommit::new();
        app.replace_editor_sql(&mut doc, "select 1");
        assert_eq!(crate::doc::EditorDoc::read_sql(&doc), "select 1");
        let recorded = app
            .actions
            .local
            .last_dispatched()
            .expect("the SQL edit is submitted through the action sink");
        assert_eq!(recorded.type_id.as_str(), crate::actions::DB_ACTION_TYPE);
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod responsive_tests {
    use super::*;

    /// Drive the real layout passes (`build_layout` → `compute_layout` →
    /// `apply_layout`) so assertions exercise the production geometry path, not
    /// a hand-recomputed copy.
    fn lay_out(app: &mut DbClientApp, w: f64, h: f64) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, Point::ZERO);
    }

    /// The breakpoint resolves from width; wide windows dock the sidebar, narrow ones overlay a flap behind a scrim, and widening restores it.
    #[test]
    fn sidebar_docks_or_overlays_by_width() {
        // --- phase: breakpoint_resolves_from_allocated_width ---
        {
            assert_eq!(
                SplitLayout::from_width(BREAKPOINT, BREAKPOINT),
                SplitLayout::Docked
            );
            assert_eq!(
                SplitLayout::from_width(BREAKPOINT - 1.0, BREAKPOINT),
                SplitLayout::Overlay
            );
        }

        // --- phase: wide_window_docks_the_sidebar_beside_inset_content ---
        {
            let mut app = DbClientApp::new();
            lay_out(&mut app, 1280.0, 800.0);
            assert_eq!(app.split_layout(), SplitLayout::Docked);
            // Sidebar is docked at full width; the main panel is inset past it.
            assert!(
                app.sidebar_rect.width() > SIDEBAR_W - 1.0,
                "docked sidebar is full width"
            );
            assert!(
                app.main_rect.x0 > app.rect.x0 + SIDEBAR_W - 1.0,
                "docked main panel is inset past the sidebar"
            );
            // No scrim while docked.
            assert_eq!(app.scrim_rect.width(), 0.0, "docked layout paints no scrim");
        }

        // --- phase: narrow_window_collapses_sidebar_to_a_closed_overlay ---
        {
            let mut app = DbClientApp::new();
            app.snap_flap_for_width(720.0); // shots/live default for narrow
            lay_out(&mut app, 720.0, 900.0);
            assert_eq!(app.split_layout(), SplitLayout::Overlay);
            assert!(
                !app.flap.is_open(SplitLayout::Overlay),
                "narrow sidebar is closed by default"
            );
            // Main panel spans the full width; sidebar is slid off-screen left.
            assert!(
                (app.main_rect.width() - app.rect.width()).abs() < 1.0,
                "overlay content spans full width while the flap is closed"
            );
            assert!(
                app.sidebar_rect.x1 <= app.rect.x0 + 1.0,
                "closed overlay sidebar is off-screen"
            );
            assert_eq!(
                app.scrim_rect.width(),
                0.0,
                "closed overlay paints no scrim"
            );
            // The hamburger toggle is offered so the sidebar can be summoned.
            assert!(app.show_toggle(), "narrow layout offers the summon toggle");
        }

        // --- phase: opening_the_overlay_flap_slides_it_flush_behind_a_scrim ---
        {
            let mut app = DbClientApp::new();
            lay_out(&mut app, 720.0, 900.0);
            // Summon the sidebar (as the hamburger does) and settle deterministically.
            app.flap.snap(true);
            lay_out(&mut app, 720.0, 900.0);
            assert!(app.flap.is_open(SplitLayout::Overlay));
            // Fully-revealed overlay sidebar sits flush at the left edge.
            assert!(
                (app.sidebar_rect.x0 - app.rect.x0).abs() < 1.0,
                "revealed overlay sidebar is flush at the left edge"
            );
            // A scrim now covers the content body.
            assert!(
                app.scrim_rect.width() > 0.0,
                "open overlay flap dims the content with a scrim"
            );
        }

        // --- phase: widening_past_the_breakpoint_restores_the_docked_sidebar ---
        {
            let mut app = DbClientApp::new();
            // Start narrow + closed, then widen past the breakpoint.
            lay_out(&mut app, 720.0, 900.0);
            assert_eq!(app.split_layout(), SplitLayout::Overlay);
            lay_out(&mut app, 1280.0, 800.0);
            app.flap.snap(true); // settle the reconciled docked-open default
            lay_out(&mut app, 1280.0, 800.0);
            assert_eq!(app.split_layout(), SplitLayout::Docked);
            assert!(
                app.flap.is_open(SplitLayout::Docked),
                "widening returns to the docked-open default"
            );
            assert!(
                app.main_rect.x0 > app.rect.x0 + 1.0,
                "docked content is inset by the restored sidebar"
            );
        }
    }

    /// The query editor's automatic height follows shaped, wrapped content and short layouts keep every region ordered.
    #[test]
    fn query_editor_height_follows_its_content() {
        // --- phase: demo_query_automatic_height_follows_its_content ---
        {
            let mut app = DbClientApp::new_demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            lay_out(&mut app, 1200.0, 800.0);

            assert_eq!(
                app.editor.line_count(),
                4,
                "the demo fixture stays representative"
            );
            assert!(
                app.editor.rect().height() < 180.0,
                "four query lines plus editing allowance should leave substantially more result space"
            );
        }

        // --- phase: shaped_wrapped_query_refines_the_automatic_height ---
        {
            let mut app = DbClientApp::new_demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            app.editor.set_text(
            "SELECT this_is_a_deliberately_long_projection_name, another_long_projection_name, a_third_long_projection_name FROM albums;",
        );
            lay_out(&mut app, 480.0, 900.0);
            let fallback = app.editor.rect().height();

            let theme = Theme::dark();
            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            app.editor
                .draw_embedded(&mut scene, Affine::IDENTITY, &mut renderer, &theme);
            app.relayout();

            assert!(
                app.editor.rect().height() > fallback,
                "the settled editor height counts shaped visual rows, not only newlines"
            );
        }

        // --- phase: short_query_layout_keeps_every_region_ordered_and_usable ---
        {
            let mut app = DbClientApp::new_demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            lay_out(&mut app, 800.0, 480.0);
            let layout = app.query_layout;

            assert_eq!(layout.toolbar.y1, layout.editor.y0);
            assert_eq!(layout.editor.y1, layout.divider.y0);
            assert_eq!(layout.divider.y1, layout.result_strip.y0);
            assert_eq!(layout.result_strip.y1, layout.grid.y0);
            assert!(layout.editor.height() >= app.editor.line_height() * 3.0);
            assert!(layout.grid.height() >= QUERY_GRID_MIN_H);
            assert!(layout.grid.y1 <= app.main_rect.y1);
        }
    }
}

#[cfg(test)]
mod honesty_tests {
    use super::*;
    use automerge::ROOT;

    /// The live app starts empty with a semantic new-connection action opening the existing modal; only the demo seeds the sample.
    #[test]
    fn start_state_is_empty_live_and_seeded_only_in_demo() {
        // --- phase: live_app_starts_empty ---
        {
            // The live constructor path fabricates nothing: no connections, no
            // live engine, no schema.
            let app = DbClientApp::build(Session::new(), false);
            assert!(!app.demo, "live build is not the demo build");
            assert!(
                app.session.connections.is_empty(),
                "no fabricated connections"
            );
            assert!(!app.session.connected, "no live engine on a fresh machine");
            assert_eq!(app.session.schema.table_count(), 0, "no fabricated schema");
        }

        // --- phase: live_empty_state_exposes_the_new_connection_action ---
        {
            use ui::a11y::Role;

            let app = DbClientApp::build(Session::new(), false);
            assert!(
                app.accessibility().is_none(),
                "the root must not duplicate its accessible child"
            );
            let children = app.children();
            assert_eq!(children.len(), 1, "one retained first-run child");
            let node = children[0]
                .accessibility()
                .expect("the first-run child is accessible");

            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name.as_deref(), Some("No connection yet"));
            assert_eq!(node.children.len(), 1);
            assert_eq!(node.children[0].role, Role::Button);
            assert_eq!(node.children[0].name.as_deref(), Some("New connection"));
            assert!(
                children[0].is_focusable(),
                "the shared CTA participates in keyboard focus"
            );
        }

        // --- phase: accessibility_activation_opens_the_existing_connection_modal ---
        {
            let mut app = DbClientApp::build(Session::new(), false);
            let mut doc = AutoCommit::new();

            assert!(!app.modal.is_open());
            assert!(
                app.empty_state
                    .perform_action(&ui::a11y::Action::Activate, &mut doc, &ROOT)
            );
            assert!(
                !app.modal.is_open(),
                "the child publishes its action before the parent sync drains it"
            );
            app.sync(&mut doc, &ROOT);
            assert!(
                app.modal.is_open(),
                "semantic activation enters the existing new-connection flow"
            );
        }

        // --- phase: demo_app_seeds_the_sample ---
        {
            // The demo builder is the sole source of the sample database.
            let app = DbClientApp::new_demo();
            assert!(app.demo, "demo build is flagged");
            assert!(
                !app.session.connections.is_empty(),
                "demo seeds sample connections"
            );
            assert!(app.session.connected, "demo opens the sample engine");
            assert!(
                app.session.schema.table_count() >= 4,
                "demo has the sample schema"
            );
        }
    }
}

#[cfg(test)]
mod behavior_tests {
    use super::*;

    use automerge::ROOT;
    use tilo_actions::Action;
    use tilo_automerge::ApplyIncoming;
    use ui::a11y::Role;
    use winit::event::{DeviceId, KeyEvent};
    use winit::keyboard::{KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey, SmolStr};

    use crate::model::ConnectionProfile;

    fn device_id() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }

    fn char_event(c: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(c));
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(c));
        WindowEvent::KeyboardInput {
            device_id: device_id(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn enter_event() -> WindowEvent {
        named_key_event(NamedKey::Enter)
    }

    fn named_key_event(key: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(key);
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: device_id(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn cursor_event(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: device_id(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn left_button(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: device_id(),
            state,
            button: MouseButton::Left,
        }
    }

    fn right_button() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Right,
        }
    }

    fn lay_out(app: &mut DbClientApp, w: f64, h: f64) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, Point::ZERO);
    }

    fn send(
        app: &mut DbClientApp,
        doc: &mut AutoCommit,
        ev: &WindowEvent,
        mods: Modifiers,
    ) -> bool {
        let taffy = TaffyTree::<()>::new();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: doc,
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: NodeId::new(0),
                taffy: &taffy,
            },
        );
        handled
    }

    fn synced() -> (DbClientApp, AutoCommit) {
        let mut app = DbClientApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    /// The split divider resizes by drag, keyboard and the resize menu, sharing one manual height across hosts and resets.
    #[expect(
        clippy::float_cmp,
        reason = "layout reproduces authored whole-pixel sizes and their sums exactly in binary floating point; exact equality is what the test checks"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "one keyboard-resize scenario whose steps depend on the previous layout, read top to bottom"
    )]
    #[test]
    fn divider_resize_by_pointer_keyboard_and_menu() {
        // --- phase: divider_drag_preserves_requested_height_state_and_query_models ---
        {
            let (mut app, mut doc) = synced();
            lay_out(&mut app, 1200.0, 800.0);
            app.editor.set_selection(2, 9);
            app.grid.sort_col = Some(1);
            app.grid.sort_asc = false;
            app.grid.scroll_row = 3;
            app.grid.scroll_x = 17.0;
            let before = app.query_layout.divider;

            assert!(!send(
                &mut app,
                &mut doc,
                &cursor_event(before.center().x, before.center().y),
                Modifiers::default(),
            ));
            assert!(send(
                &mut app,
                &mut doc,
                &left_button(ElementState::Pressed),
                Modifiers::default(),
            ));
            assert!(app.wants_pointer_capture());
            assert!(send(
                &mut app,
                &mut doc,
                &cursor_event(before.center().x, before.center().y + 300.0),
                Modifiers::default(),
            ));
            assert!(send(
                &mut app,
                &mut doc,
                &left_button(ElementState::Released),
                Modifiers::default(),
            ));
            lay_out(&mut app, 1200.0, 800.0);
            let manual = app.query_layout.divider;
            assert_eq!(manual.y0, before.y0 + 300.0);
            app.grid.hover_row = Some(4);

            app.session.tab = MainTab::Data;
            app.relayout();
            app.session.tab = MainTab::Structure;
            app.relayout();
            app.session.tab = MainTab::Query;
            app.relayout();
            assert_eq!(app.query_layout.divider.y0, manual.y0);
            assert_eq!(app.query_layout.divider.height(), manual.height());

            let ctrl = Modifiers::from(ModifiersState::CONTROL);
            assert!(send(&mut app, &mut doc, &enter_event(), ctrl));
            app.session.block_until_idle();
            app.sync(&mut doc, &ROOT);
            app.relayout();
            assert_eq!(
                app.query_layout.divider.y0, manual.y0,
                "running the query keeps the local requested split"
            );
            assert_eq!(app.query_layout.divider.height(), manual.height());

            lay_out(&mut app, 800.0, 480.0);
            assert!(app.query_layout.divider.y0 < manual.y0);
            lay_out(&mut app, 1200.0, 800.0);
            assert_eq!(app.query_layout.divider.y0, manual.y0);
            assert_eq!(app.query_layout.divider.height(), manual.height());
            assert_eq!(app.editor.selection(), Some((2, 9)));
            assert_eq!(app.grid.sort_col, Some(1));
            assert!(!app.grid.sort_asc);
            assert_eq!(app.grid.scroll_row, 3);
            assert_eq!(app.grid.scroll_x, 17.0);
            assert_eq!(app.grid.hover_row, Some(4));
        }

        // --- phase: divider_keyboard_semantics_and_context_reset_share_the_manual_height ---
        {
            let (mut app, mut doc) = synced();
            lay_out(&mut app, 1200.0, 800.0);
            let automatic = app.query_layout.divider;
            assert!(app.take_structure_dirty());
            assert!(!app.take_structure_dirty());

            let divider = app
                .children_mut()
                .into_iter()
                .find(|child| child.test_id() == Some("db.query-divider"))
                .expect("query layout exposes a retained divider");
            divider.set_focus(true);
            assert!(send(
                &mut app,
                &mut doc,
                &named_key_event(NamedKey::ArrowDown),
                Modifiers::default(),
            ));
            lay_out(&mut app, 1200.0, 800.0);
            assert_eq!(app.query_layout.divider.y0, automatic.y0 + 24.0);

            let divider = app
                .children_mut()
                .into_iter()
                .find(|child| child.test_id() == Some("db.query-divider"))
                .unwrap();
            assert!(divider.perform_action(&ui::a11y::Action::Increment, &mut doc, &ROOT));
            app.sync(&mut doc, &ROOT);
            lay_out(&mut app, 1200.0, 800.0);
            assert_eq!(app.query_layout.divider.y0, automatic.y0 + 48.0);

            let divider_rect = app.query_layout.divider;
            send(
                &mut app,
                &mut doc,
                &cursor_event(divider_rect.center().x, divider_rect.center().y),
                Modifiers::default(),
            );
            assert!(send(
                &mut app,
                &mut doc,
                &right_button(),
                Modifiers::default(),
            ));
            assert!(app.take_structure_dirty());
            let menu = app
                .children()
                .into_iter()
                .find(|child| child.test_id() == Some("db.query-resize-menu"))
                .expect("secondary click opens the bounded resize menu");
            let semantics = menu.accessibility().expect("resize menu is accessible");
            assert!(
                semantics
                    .children
                    .iter()
                    .any(|node| { node.name.as_deref() == Some("Reset automatic query height") })
            );
            let reset = menu
                .test_rects()
                .into_iter()
                .find(|(id, _)| id == "db.reset-query-height")
                .expect("reset row publishes its actual pointer geometry")
                .1;
            send(
                &mut app,
                &mut doc,
                &cursor_event(reset.center().x, reset.center().y),
                Modifiers::default(),
            );
            assert!(send(
                &mut app,
                &mut doc,
                &left_button(ElementState::Pressed),
                Modifiers::default(),
            ));
            assert!(app.take_structure_dirty());
            lay_out(&mut app, 1200.0, 800.0);
            assert_eq!(app.query_layout.divider, automatic);

            let divider = app
                .children_mut()
                .into_iter()
                .find(|child| child.test_id() == Some("db.query-divider"))
                .unwrap();
            divider.set_focus(true);
            assert!(send(
                &mut app,
                &mut doc,
                &named_key_event(NamedKey::ArrowDown),
                Modifiers::default(),
            ));
            let divider = app
                .children_mut()
                .into_iter()
                .find(|child| child.test_id() == Some("db.query-divider"))
                .unwrap();
            assert!(divider.perform_action(&ui::a11y::Action::ShowContextMenu, &mut doc, &ROOT,));
            app.sync(&mut doc, &ROOT);
            let menu = app
                .children_mut()
                .into_iter()
                .find(|child| child.test_id() == Some("db.query-resize-menu"))
                .expect("semantic context action opens the retained resize menu");
            assert!(menu.perform_action_at(&ui::a11y::Action::Activate, &[2], &mut doc, &ROOT,));
            app.sync(&mut doc, &ROOT);
            lay_out(&mut app, 1200.0, 800.0);
            assert_eq!(app.query_layout.divider, automatic);

            let divider = app
                .children_mut()
                .into_iter()
                .find(|child| child.test_id() == Some("db.query-divider"))
                .unwrap();
            divider.set_focus(true);
            assert!(send(
                &mut app,
                &mut doc,
                &named_key_event(NamedKey::ArrowDown),
                Modifiers::default(),
            ));
            assert!(send(
                &mut app,
                &mut doc,
                &named_key_event(NamedKey::Home),
                Modifiers::default(),
            ));
            lay_out(&mut app, 1200.0, 800.0);
            assert_eq!(app.query_layout.divider, automatic);
            assert!(send(
                &mut app,
                &mut doc,
                &named_key_event(NamedKey::Enter),
                Modifiers::default(),
            ));
            assert!(app.query_resize_menu.is_open());
            assert!(send(
                &mut app,
                &mut doc,
                &named_key_event(NamedKey::Escape),
                Modifiers::default(),
            ));
            assert!(!app.query_resize_menu.is_open());
            assert!(app.query_divider.is_focused());
        }

        // --- phase: host_routes_named_divider_focus_keyboard_and_pointer_reset ---
        {
            fn host_routes_named_divider_focus_keyboard_and_pointer_reset() {
                fn reset_rect(snapshot: &ui::headless::InspectionSnapshot) -> Rect {
                    fn find(node: &ui::headless::InspectionNode) -> Option<Rect> {
                        if node.test_id.as_deref() == Some("db.query-resize-menu") {
                            return node.semantics.as_ref()?.children.iter().find_map(|child| {
                                (child.action_target.as_deref() == Some("db.reset-query-height"))
                                    .then(|| child.bounds.map(ui::a11y::AccessBounds::to_rect))
                                    .flatten()
                            });
                        }
                        node.children.iter().find_map(find)
                    }

                    find(&snapshot.root).expect("reset action publishes its pointer bounds")
                }

                let mut driver = ui::headless::Driver::new(
                    ui::State::new(),
                    Box::new(DbClientApp::new_demo()),
                    (1200, 800),
                );
                driver
                    .perform_named("db.query-divider", &ui::a11y::Action::Focus)
                    .unwrap();
                let automatic = driver.targets()["db.query-divider"];
                driver.key("ArrowDown");
                assert_eq!(driver.targets()["db.query-divider"].y0, automatic.y0 + 24.0);

                driver
                    .perform_named("db.query-divider", &ui::a11y::Action::ShowContextMenu)
                    .unwrap();
                let reset = reset_rect(&driver.inspection());
                driver.click_at(reset.center().x, reset.center().y);
                assert_eq!(driver.targets()["db.query-divider"], automatic);
            }
            host_routes_named_divider_focus_keyboard_and_pointer_reset();
        }

        // --- phase: modified_enter_runs_once_ahead_of_the_open_resize_menu ---
        {
            let app = Box::new(DbClientApp::new_demo());
            let app_ptr = mounted_app(&app);
            let mut driver = ui::headless::Driver::new(ui::State::new(), app, (1200, 800));
            driver
                .perform_named("db.query-divider", &ui::a11y::Action::Increment)
                .unwrap();
            driver
                .perform_named("db.query-divider", &ui::a11y::Action::ShowContextMenu)
                .unwrap();

            // SAFETY: `app_ptr` points into the Box now owned by `driver`; the
            // allocation stays stable and no Driver borrow is active while read.
            let (generation, requested, effective) = unsafe {
                let app = &*app_ptr;
                (
                    app.session.query_gen,
                    app.query_height_request,
                    app.query_layout.editor.height(),
                )
            };
            assert!(requested.is_some(), "precondition: resize is manual");
            driver.set_modifiers(&["ctrl".into()]);
            driver.key("Enter");
            driver.set_modifiers(&[]);
            assert!(driver.wait_until(tilo_test_env::hang_bound(), |_| {
                // SAFETY: same stable allocation; the mutable frame borrow has
                // ended before this condition callback runs.
                unsafe { (&*app_ptr).session.query_gen > generation }
            }));
            // SAFETY: same stable allocation and non-overlapping read.
            let app = unsafe { &*app_ptr };
            assert_eq!(app.session.query_gen, generation + 1, "Run executes once");
            assert_eq!(app.query_height_request, requested);
            assert_eq!(app.query_layout.editor.height(), effective);
            assert!(
                app.query_resize_menu.is_open(),
                "the reserved shortcut does not select or dismiss a resize item"
            );
        }

        // --- phase: open_resize_menu_reanchors_and_keeps_every_action_mapped_after_resize ---
        {
            #[expect(
                clippy::float_cmp,
                reason = "the menu is anchored by copying these exact edge coordinates, so equality is the invariant under test"
            )]
            fn assert_bounded_and_anchored(
                driver: &ui::headless::Driver,
                app: &DbClientApp,
                width: f64,
                height: f64,
            ) -> Vec<(String, Rect)> {
                let viewport = Rect::new(0.0, 0.0, width, height);
                let menu = driver.targets()["db.query-resize-menu"];
                assert!(
                    menu.x0 >= viewport.x0
                        && menu.y0 >= viewport.y0
                        && menu.x1 <= viewport.x1
                        && menu.y1 <= viewport.y1
                );
                assert_eq!(menu.x0, app.query_layout.divider.x0);
                assert!(
                    menu.y0 == app.query_layout.divider.y1
                        || menu.y1 == app.query_layout.divider.y0
                        || menu.y0 == viewport.y0
                        || menu.y1 == viewport.y1,
                    "menu remains attached to the recomputed divider"
                );
                let actions = menu_action_rects(&driver.inspection());
                assert_eq!(actions.len(), 3);
                for (_, rect) in &actions {
                    assert!(
                        rect.x0 >= menu.x0
                            && rect.y0 >= menu.y0
                            && rect.x1 <= menu.x1
                            && rect.y1 <= menu.y1
                    );
                }
                actions
            }

            let app = Box::new(DbClientApp::new_demo());
            let app_ptr = mounted_app(&app);
            let mut driver = ui::headless::Driver::new(ui::State::new(), app, (1200, 800));
            driver
                .perform_named("db.query-divider", &ui::a11y::Action::Increment)
                .unwrap();
            let requested = unsafe { (&*app_ptr).query_height_request.unwrap() };
            driver
                .perform_named("db.query-divider", &ui::a11y::Action::ShowContextMenu)
                .unwrap();
            driver.key("ArrowDown");
            driver.resize((480, 900)).unwrap();
            // SAFETY: stable Driver-owned Box; read occurs between Driver calls.
            assert_bounded_and_anchored(&driver, unsafe { &*app_ptr }, 480.0, 900.0);
            driver.resize((800, 480)).unwrap();
            assert_bounded_and_anchored(&driver, unsafe { &*app_ptr }, 800.0, 480.0);
            if let Ok(path) = std::env::var("DB_CLIENT_TASK12_MENU_CAPTURE") {
                driver.render_png(std::path::Path::new(&path));
            }
            assert_eq!(unsafe { (&*app_ptr).query_height_request }, Some(requested));
            driver.key("Enter");
            assert_eq!(
                unsafe { (&*app_ptr).query_height_request },
                Some(requested - 24.0),
                "the highlighted Decrease action survives reanchoring"
            );

            driver
                .perform_named("db.query-divider", &ui::a11y::Action::ShowContextMenu)
                .unwrap();
            driver.resize((480, 900)).unwrap();
            driver.resize((800, 480)).unwrap();
            let actions = assert_bounded_and_anchored(&driver, unsafe { &*app_ptr }, 800.0, 480.0);
            let increase = actions
                .iter()
                .find(|(target, _)| target == "db.increase-query-height")
                .unwrap()
                .1;
            let before_increase = unsafe { (&*app_ptr).query_height_request.unwrap() };
            driver.click_at(increase.center().x, increase.center().y);
            assert_eq!(
                unsafe { (&*app_ptr).query_height_request },
                Some(before_increase + 24.0),
                "the pointer row maps to Increase"
            );

            driver
                .perform_named("db.query-divider", &ui::a11y::Action::ShowContextMenu)
                .unwrap();
            driver.resize((480, 900)).unwrap();
            driver.resize((800, 480)).unwrap();
            assert_bounded_and_anchored(&driver, unsafe { &*app_ptr }, 800.0, 480.0);
            driver
                .perform_semantic_named(
                    "db.query-resize-menu",
                    Some("db.reset-query-height"),
                    &ui::a11y::Action::Activate,
                )
                .unwrap();
            assert_eq!(
                unsafe { (&*app_ptr).query_height_request },
                None,
                "the semantic Reset target maps to automatic height"
            );
        }
    }

    fn menu_action_rects(snapshot: &ui::headless::InspectionSnapshot) -> Vec<(String, Rect)> {
        fn find(node: &ui::headless::InspectionNode) -> Option<Vec<(String, Rect)>> {
            if node.test_id.as_deref() == Some("db.query-resize-menu") {
                return Some(
                    node.semantics
                        .as_ref()?
                        .children
                        .iter()
                        .filter_map(|child| {
                            Some((
                                child.action_target.clone()?,
                                child.bounds.map(ui::a11y::AccessBounds::to_rect)?,
                            ))
                        })
                        .collect(),
                );
            }
            node.children.iter().find_map(find)
        }

        find(&snapshot.root).expect("open resize menu publishes semantic action bounds")
    }

    fn mounted_app(app: &DbClientApp) -> *const DbClientApp {
        std::ptr::from_ref(app)
    }

    #[test]
    fn mounted_clamped_long_query_remains_editable_and_scrollable() {
        let app = Box::new(DbClientApp::new_demo());
        let app_ptr = mounted_app(&app);
        let mut driver = ui::headless::Driver::new(ui::State::new(), app, (800, 480));
        let long_query = (0..60)
            .map(|line| format!("SELECT {line} AS value;"))
            .collect::<Vec<_>>()
            .join("\n");
        driver
            .perform_named(
                "sql-editor",
                &ui::a11y::Action::SetValue(long_query.clone()),
            )
            .unwrap();
        let editor = driver.targets()["sql-editor"];
        driver.click_at(editor.x0 + 64.0, editor.y0 + 12.0);
        driver
            .perform_named(
                "sql-editor",
                &ui::a11y::Action::SetSelection {
                    anchor: long_query.len(),
                    focus: long_query.len(),
                },
            )
            .unwrap();
        driver.type_text("\n-- edited at the end");
        let editor = driver.targets()["sql-editor"];
        driver.send(&cursor_event(editor.center().x, editor.center().y));
        driver.scroll(0.0, -12.0);

        // SAFETY: stable Driver-owned Box; reads occur between Driver calls.
        unsafe {
            let app = &*app_ptr;
            assert!(app.editor.inner().content_height() > app.editor.rect().height());
            assert!(
                app.editor.inner().scroll_y() > 0.0,
                "wheel scroll reaches the clamped editor: scroll={} content={} viewport={} focused={}",
                app.editor.inner().scroll_y(),
                app.editor.inner().content_height(),
                app.editor.rect().height(),
                app.editor.is_focused()
            );
            assert!(app.editor.value().ends_with("-- edited at the end"));
            assert_eq!(app.editor.cursor_pos(), app.editor.value().len());
            assert!(app.query_layout.grid.height() >= QUERY_GRID_MIN_H);
        }
        let shared_sql = EditorDoc::read_sql(&driver.state_mut().doc);
        assert_eq!(shared_sql, unsafe { (&*app_ptr).editor.value() });

        driver
            .perform_named("db.query-divider", &ui::a11y::Action::Increment)
            .unwrap();
        let requested = unsafe { (&*app_ptr).query_height_request };
        driver.resize((800, 700)).unwrap();
        driver.resize((800, 480)).unwrap();
        let app = unsafe { &*app_ptr };
        assert_eq!(app.query_height_request, requested);
        assert!(app.editor.value().ends_with("-- edited at the end"));
        assert_eq!(app.editor.cursor_pos(), app.editor.value().len());
        assert!(app.query_layout.grid.height() >= QUERY_GRID_MIN_H);
    }

    /// Many connections keep the narrow drawer and query actions reachable, and the semantic new-connection dialog is accessible and secret-safe.
    #[test]
    fn narrow_drawer_and_new_connection_dialog_stay_reachable() {
        // --- phase: many_connections_keep_narrow_drawer_and_query_actions_reachable ---
        {
            let mut app = Box::new(DbClientApp::new_demo());
            for index in 3..8 {
                let name = format!("Connection {index}");
                app.session
                    .connections
                    .push(ConnectionProfile::new(&name, Engine::Postgres));
            }
            let app_ptr = mounted_app(&app);
            let mut driver = ui::headless::Driver::new(ui::State::new(), app, (480, 900));
            // SAFETY: stable Driver-owned Box; reads occur between Driver calls.
            let closed_split = unsafe { (&*app_ptr).query_layout };
            let toggle = unsafe { (&*app_ptr).toggle_btn };
            assert!(toggle.width() > 0.0 && toggle.y0 >= 0.0 && toggle.y1 <= 900.0);
            driver.click_at(toggle.center().x, toggle.center().y);
            // Settle the existing flap animation after proving the real trigger.
            unsafe { (&mut *(app_ptr.cast_mut())).flap.snap(true) };
            driver.relayout();
            let app = unsafe { &*app_ptr };
            assert_eq!(app.session.connections.len(), 8);
            assert!(app.flap.is_open(SplitLayout::Overlay));
            assert_eq!(app.query_layout, closed_split);
            assert_eq!(app.conn_rects.len(), 8);
            let seventh = app.conn_rects[6].1;
            assert!(seventh.y0 >= 0.0 && seventh.y1 <= 900.0);
            driver.click_at(seventh.center().x, seventh.center().y);
            assert_eq!(unsafe { (&*app_ptr).session.active }, 6);

            let toggle = unsafe { (&*app_ptr).toggle_btn };
            driver.click_at(toggle.center().x, toggle.center().y);
            unsafe { (&mut *(app_ptr.cast_mut())).flap.snap(false) };
            driver.relayout();
            assert!(!unsafe { (&*app_ptr).flap.is_open(SplitLayout::Overlay) });
            assert!(unsafe { (&*app_ptr).toggle_btn.width() > 0.0 });
        }

        // --- phase: semantic_new_connection_opens_an_accessible_secret_safe_dialog ---
        {
            use ui::a11y::Action as A11yAction;

            let (mut app, mut doc) = synced();
            assert!(app.perform_semantic_action(
                "db.new-connection",
                &A11yAction::Activate,
                &mut doc,
                &ROOT,
            ));
            let dialog = app.accessibility().expect("open modal is accessible");
            assert_eq!(dialog.role, Role::Dialog);
            assert!(dialog.children.iter().any(|node| {
                node.name.as_deref() == Some("Password")
                    && node.state.protected
                    && node.value.is_none()
            }));
        }
    }

    #[test]
    fn host_first_frame_measures_query_content_before_painting_the_split() {
        let wide = ui::headless::Driver::new(
            ui::State::new(),
            Box::new(DbClientApp::new_demo()),
            (1200, 800),
        );
        assert!(
            wide.targets()["sql-editor"].height() >= 100.0,
            "the normal first frame must show the four-line demo query with editing allowance"
        );

        let narrow = ui::headless::Driver::new(
            ui::State::new(),
            Box::new(DbClientApp::new_demo()),
            (480, 900),
        );
        assert!(
            narrow.targets()["sql-editor"].height() >= 120.0,
            "the first narrow frame must include rows introduced by wrapping"
        );
    }

    /// SQL typing replicates byte-identically, replaces selections in the CRDT, pulls collaborator edits and is exported to accessibility.
    #[test]
    fn sql_editor_is_collaborative_crdt_text() {
        // --- phase: typing_replicates_byte_identically_on_a_peer ---
        // A keystroke driven through the production `handle_event` splices into the
        // shared SQL `Text` and converges byte-identically on a peer — the native
        // text path, unchanged by the async engine.
        {
            let (mut app, mut doc) = synced();
            doc.commit();
            let base = doc.save();
            let before = crate::doc::EditorDoc::read_sql(&doc);

            let mut peer = AutoCommit::load(&base).unwrap();
            assert_eq!(
                doc.get_heads(),
                peer.get_heads(),
                "precondition: replicas share a base"
            );

            assert!(send(
                &mut app,
                &mut doc,
                &char_event("Z"),
                Modifiers::default()
            ));
            doc.commit();
            let after = crate::doc::EditorDoc::read_sql(&doc);
            assert_eq!(
                after.chars().count(),
                before.chars().count() + 1,
                "one char inserted"
            );

            peer.merge_incoming(&mut doc).unwrap();
            assert_eq!(
                doc.get_heads(),
                peer.get_heads(),
                "byte-identical convergence"
            );
        }

        // --- phase: typing_replaces_the_selected_sql_in_the_crdt ---
        // The adopted editor's selection is not local-only decoration: replacing
        // it performs one splice on the same shared SQL `Text` object.
        {
            let (mut app, mut doc) = synced();
            assert!(app.editor.value().starts_with("SELECT"));
            app.editor.set_selection(0, "SELECT".len());

            assert!(send(
                &mut app,
                &mut doc,
                &char_event("WITH"),
                Modifiers::default()
            ));

            assert!(app.editor.value().starts_with("WITH"));
            assert_eq!(
                crate::doc::EditorDoc::read_sql(&doc),
                app.editor.value(),
                "selection replacement and CRDT text stay byte-identical"
            );
        }

        // --- phase: sync_pulls_a_collaborators_sql_edit_into_the_shared_editor ---
        {
            let (mut app, mut doc) = synced();
            crate::doc::EditorDoc::set_sql(&mut doc, "SELECT * FROM remote_table");

            app.sync(&mut doc, &ROOT);

            assert_eq!(app.editor.value(), "SELECT * FROM remote_table");
        }

        // --- phase: accessibility_exports_the_full_workspace_and_routes_sql_edits ---
        {
            use ui::a11y::Action as A11yAction;

            let (mut app, mut doc) = synced();
            let a11y = app
                .accessibility()
                .expect("connected app exposes a semantic window");

            assert_eq!(a11y.role, Role::Window);
            assert_eq!(a11y.name.as_deref(), Some("Database Client"));
            assert!(
                a11y.children
                    .iter()
                    .any(|node| node.name.as_deref() == Some("Database navigator"))
            );
            assert!(
                a11y.children
                    .iter()
                    .any(|node| node.name.as_deref() == Some("Database workspace"))
            );
            assert!(app.perform_semantic_action(
                "db.editor.sql",
                &A11yAction::SetValue("SELECT 42".into()),
                &mut doc,
                &ROOT,
            ));
            assert_eq!(crate::doc::EditorDoc::read_sql(&doc), "SELECT 42");
            assert!(app.is_focused(), "startup typing focus reaches the editor");
            assert!(app.perform_semantic_action(
                "db.tab:data",
                &A11yAction::Activate,
                &mut doc,
                &ROOT,
            ));
            assert_eq!(app.session.tab, MainTab::Data);
        }
    }

    /// Running a query writes nothing; saving a connection dispatches into the doc, reconciles into the app and survives reload.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn connections_live_in_the_document_but_queries_do_not() {
        // --- phase: running_a_query_writes_nothing_to_the_document ---
        // Running a query is external: Ctrl+Enter dispatches work to the engine
        // worker and writes nothing to the shared document.
        {
            let (mut app, mut doc) = synced();
            doc.commit();
            let before = doc.get_heads();

            let ctrl = Modifiers::from(ModifiersState::CONTROL);
            assert!(
                send(&mut app, &mut doc, &enter_event(), ctrl),
                "Ctrl+Enter runs the query"
            );
            assert_eq!(
                doc.get_heads(),
                before,
                "query execution is a driver side effect, never a document mutation"
            );
        }

        // --- phase: saving_a_connection_dispatches_an_action_into_the_doc ---
        // Saving a connection goes through the action model: the metadata lands in
        // `root["connections"]`, an undo boundary is recorded, the password never
        // enters the document, and the app remembers to connect it.
        {
            let mut app = DbClientApp::build(Session::new(), false);
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);

            let mut profile = ConnectionProfile::new("Prod", Engine::Postgres);
            profile.host = "db.internal".into();
            profile.password = "hunter2".into();
            let id = profile.id;
            app.save_connection(profile, &mut doc);

            let conns = crate::doc::ConnectionsDoc::read(&doc);
            assert_eq!(conns.len(), 1, "the connection was written to the document");
            assert_eq!(conns[0].name, "Prod");
            assert!(
                ui::undo::take_committed_action(&mut app).is_some(),
                "the action host recorded an undo boundary for the CRUD"
            );
            assert_eq!(
                app.pending_connect_id,
                Some(id),
                "the app will open it on reconcile"
            );

            let bytes = doc.save();
            assert!(
                !bytes.windows(7).any(|w| w == b"hunter2"),
                "the password must never enter the shared document"
            );
        }

        // --- phase: reconcile_surfaces_document_connections ---
        // A reconcile from the document surfaces a peer-added connection in the
        // live session without auto-connecting it (no local password).
        {
            let mut app = DbClientApp::build(Session::new(), false);
            let mut doc = AutoCommit::new();
            // A peer (or a prior session) added a connection to the document.
            let p = ConnectionProfile::new("Team DB", Engine::MySql);
            crate::actions::add_connection_action(&p)
                .apply(&mut doc)
                .unwrap();

            app.sync(&mut doc, &ROOT);
            assert_eq!(
                app.session.connections.len(),
                1,
                "the document connection is listed"
            );
            assert_eq!(app.session.connections[0].name, "Team DB");
            assert!(
                !app.session.connected,
                "a peer-added connection is not auto-opened"
            );
        }

        // --- phase: sqlite_unicode_connection_survives_reload_and_executes_resource_queries ---
        {
            use ui::a11y::Action as A11yAction;

            let directory = std::env::temp_dir().join(format!("tilo-db-{}", Uuid::new_v4()));
            std::fs::create_dir(&directory).unwrap();
            let database = directory.join("Grün 🌍.sqlite");
            {
                let seed = rusqlite::Connection::open(&database).unwrap();
                seed.execute_batch(
                    "CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES ('Grün 🌍');",
                )
                .unwrap();
            }
            let mut app = DbClientApp::build(Session::new(), false);
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);
            for (target, action) in [
                ("db.new-connection", A11yAction::Activate),
                ("db.modal.engine:sqlite", A11yAction::Activate),
                (
                    "db.modal.field:name",
                    A11yAction::SetValue("Local Unicode".into()),
                ),
                (
                    "db.modal.field:file-path",
                    A11yAction::SetValue(database.to_str().unwrap().into()),
                ),
                ("db.modal.save", A11yAction::Activate),
            ] {
                assert!(app.perform_semantic_action(target, &action, &mut doc, &ROOT));
            }
            app.sync(&mut doc, &ROOT);
            app.session.block_until_idle();
            assert!(app.session.connected);
            assert_eq!(ConnectionsDoc::read(&doc).len(), 1);
            let connection_id = app.session.active_connection_id().unwrap().to_string();
            assert!(app.perform_semantic_action(
                "db.editor.sql",
                &A11yAction::SetValue("SELECT value FROM proof".into()),
                &mut doc,
                &ROOT,
            ));
            let saved = doc.save();
            drop(app);

            let mut doc = AutoCommit::load(&saved).unwrap();
            let mut app = DbClientApp::build(Session::new(), false);
            app.sync(&mut doc, &ROOT);
            assert!(
                !app.session.connected,
                "loading metadata must not open external resources"
            );
            let navigator = app.sidebar_accessibility();
            assert!(
                !navigator.children[0].children[0].children[0].state.selected,
                "a disconnected profile must remain selectable for native reconnect"
            );
            assert_eq!(app.editor.value(), "SELECT value FROM proof");
            let heads = doc.get_heads();
            for operation in [
                DbClientOperation::Connect {
                    connection_id: connection_id.clone(),
                },
                DbClientOperation::RunQuery {
                    connection_id,
                    sql: "SELECT value FROM proof".into(),
                },
            ] {
                let invocation = tilo_actions::InvocationId::new();
                assert_eq!(
                    app.start_operation(
                        &ui::widget::ResourceOperation::local(
                            invocation,
                            OperationKind::Effect,
                            DB_OPERATION_TYPE,
                            &serde_json::to_vec(&operation).unwrap()
                        ),
                        &mut doc,
                        &ROOT
                    )
                    .unwrap(),
                    Some(ui::widget::ResourceOperationStart::Accepted)
                );
                app.session.block_until_idle();
                assert!(matches!(
                    app.poll_resource_operation(invocation, false, &doc, &ROOT),
                    Some(tilo_actions::InvocationUpdate::Succeeded { .. })
                ));
            }
            let result = app.session.query_result.as_ref().unwrap();
            assert_eq!(result.rows.len(), 1);
            assert_eq!(result.rows[0][0].display(), "Grün 🌍");
            assert_eq!(
                doc.get_heads(),
                heads,
                "executing SQL must not copy results into the shared document"
            );
            drop(app);
            std::fs::remove_file(&database).unwrap();
            std::fs::remove_dir(&directory).unwrap();
        }
    }

    /// Resource queries stay pending until the worker finishes; the live tree covers schema, results and transactions without secrets and stays schema-qualified.
    #[test]
    fn resource_tree_and_queries_are_schema_qualified_and_secret_free() {
        // --- phase: resource_query_is_pending_until_the_database_worker_finishes ---
        {
            let (mut app, mut doc) = synced();
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&DbClientOperation::RunQuery {
                connection_id: app.session.active_connection_id().unwrap().to_string(),
                sql: "SELECT 1".into(),
            })
            .unwrap();
            assert_eq!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        OperationKind::Effect,
                        DB_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            app.session.block_until_idle();
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &ROOT),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }

        // --- phase: live_resource_tree_covers_schema_results_and_transactions_without_secrets ---
        {
            let (mut app, doc) = synced();
            app.session.connections[0].password = "never-publish-this".into();
            let resources = ui::resources::resources(&app, &doc);
            assert_eq!(resources.len(), 1, "root discovery is bounded");
            assert!(resources[0].children.is_empty());
            let connection = app.session.active_connection().unwrap().id.to_string();
            let schema = &app.session.schema.schemas[0];
            let relation = &schema.tables[0];
            let relation_id =
                catalog_resource_id("relation", &[&connection, &schema.name, &relation.name]);
            let resolved =
                ui::resources::resource(&app, &doc, &relation_id).expect("exact relation resolves");
            assert_eq!(resolved.id.as_str(), relation_id);
            assert!(resolved.actions.iter().any(|action| action.id == "browse"));
            let run_sql = resources[0]
                .actions
                .iter()
                .find(|action| action.id == "query")
                .expect("Run SQL is an exported action");
            assert_eq!(run_sql.contract.risk, tilo_actions::RiskLevel::Destructive);
            assert_eq!(
                run_sql.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm,
                "an arbitrary SQL envelope is never advertised as harmless"
            );
            assert!(
                ui::resources::resource(&app, &doc, "database")
                    .expect("root resolves exactly")
                    .actions
                    .iter()
                    .any(|action| action.id == "query"),
                "directory and exact root resolution publish the same actionable root"
            );
        }

        // --- phase: relation_resources_and_browse_payloads_remain_schema_qualified ---
        {
            let (mut app, mut doc) = synced();
            let original = app.session.schema.schemas[0].tables[0].clone();
            app.session.schema.schemas.push(crate::model::SchemaInfo {
                name: "other:schema".into(),
                tables: vec![original.clone()],
            });
            app.search_index.sync(&app.session, &mut doc).unwrap();
            let connection = app.session.active_connection().unwrap().id.to_string();
            let first_id = catalog_resource_id(
                "relation",
                &[
                    &connection,
                    &app.session.schema.schemas[0].name,
                    &original.name,
                ],
            );
            let second_id =
                catalog_resource_id("relation", &[&connection, "other:schema", &original.name]);
            assert_ne!(first_id, second_id, "encoded ids cannot collide on colons");
            let resolved =
                ui::resources::resource(&app, &doc, &second_id).expect("exact relation resolves");
            let browse = resolved
                .actions
                .iter()
                .find(|action| action.id == "browse")
                .unwrap();
            let encoded = browse.build_json(&serde_json::Map::new()).unwrap();
            let operation: DbClientOperation = serde_json::from_slice(&encoded.payload).unwrap();
            assert!(matches!(
                operation,
                DbClientOperation::BrowseTable { schema, table, .. }
                    if schema == "other:schema" && table == original.name
            ));
        }
    }

    /// Destructive SQL needs explicit confirmation, read queries stay fast, and CSV fields neutralise formulas without losing values.
    #[test]
    fn sql_safety_and_csv_export_guard_destructive_or_formula_input() {
        // --- phase: destructive_sql_requires_an_explicit_editor_confirmation ---
        {
            let (mut app, _doc) = synced();
            app.editor.set_text("DELETE FROM tracks");
            app.run_current_query();
            assert_eq!(
                app.pending_query_confirmation.as_deref(),
                Some("DELETE FROM tracks"),
                "the first run asks for confirmation instead of issuing the effect"
            );
            assert!(!app.session.busy());
        }

        // --- phase: sql_safety_check_leaves_read_queries_fast ---
        {
            assert!(!sql_is_potentially_destructive("SELECT * FROM tracks"));
            assert!(!sql_is_potentially_destructive("EXPLAIN SELECT 1"));
            assert!(sql_is_potentially_destructive(
                "  UPDATE tracks SET name = 'x'"
            ));
            assert!(sql_is_potentially_destructive("DROP TABLE tracks"));
            assert!(sql_is_potentially_destructive(
                "-- note\nDELETE FROM tracks"
            ));
            assert!(sql_is_potentially_destructive(
                "/* note */ WITH gone AS (DELETE FROM tracks RETURNING id) SELECT * FROM gone"
            ));
            assert!(sql_is_potentially_destructive(
                "SELECT 1; DROP TABLE tracks"
            ));
            assert!(!sql_is_potentially_destructive(
                "SELECT 'DELETE FROM tracks'"
            ));
            assert!(!sql_is_potentially_destructive("-- DELETE\nSELECT 1"));
        }

        // --- phase: csv_fields_neutralize_spreadsheet_formulas_without_losing_values ---
        {
            assert_eq!(csv_field("=SUM(A1:A2)"), "\"'=SUM(A1:A2)\"");
            assert_eq!(csv_field(" \t@cmd"), "\"' \t@cmd\"");
            assert_eq!(csv_field("plain, \"text\""), "\"plain, \"\"text\"\"\"");
            assert_eq!(csv_field("NULL"), "\"NULL\"");
            assert_eq!(csv_field("«4 bytes»"), "\"«4 bytes»\"");
        }
    }
}
