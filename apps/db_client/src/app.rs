//! Root widget for the database client. Owns the [`Session`] (connections,
//! engine, schema, results) and the [`SqlEditor`] (CRDT-backed buffer), lays
//! out the two regions (connections+schema sidebar, main panel) itself, and
//! draws everything directly into the Vello scene. Status is surfaced as a
//! transient floating bubble rather than a permanent strip, so the content
//! spans to the window bottom.
//!
//! Layout is hand-rolled rather than delegated to Taffy because the panes are
//! a fixed split that responds to the viewport rect — a single full-bleed
//! leaf node is handed to Taffy and every sub-rect is computed from it. Event
//! routing dispatches to the schema tree, the tab bar, the editor and the
//! results grid by hit-testing their rects.
//!
//! The connections + schema sidebar is responsive. On a wide window it stays
//! docked beside the main panel (the desktop layout above). When the window
//! is narrower than [`BREAKPOINT`] the sidebar collapses into an overlay flap
//! that slides over the main panel behind a dimming scrim — closed by default
//! and summoned with the hamburger toggle in the tab bar. The eased slide and
//! the breakpoint default-reconciliation are handled by the toolkit's shared
//! [`FlapState`] kernel rather than hand-rolled here.

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{Fill, Color};
use vello::Scene;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use uuid::Uuid;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, Widget};

use crate::connect_modal::ConnectModal;
use crate::doc::{ConnectionsDoc, EditorDoc};
use crate::editor::SqlEditor;
use crate::grid::GridView;
use crate::model::Engine;
use crate::schema_tree::{SchemaTree, TreeClick};
use crate::session::{MainTab, RunStatus, Session};
use crate::theme_ext::ThemeExt;

const SIDEBAR_W: f64 = 268.0;
const TABBAR_H: f64 = 38.0;
const TOOLBAR_H: f64 = 40.0;
/// Below this allocated width (logical px) the sidebar stops being a docked
/// pane and becomes an overlay flap. Chosen so the 268px sidebar plus a
/// usable editor/results column stop fitting comfortably side by side.
const BREAKPOINT: f64 = 820.0;
/// Hamburger toggle box drawn at the left of the tab bar when the sidebar is
/// not fully docked-open, so a collapsed sidebar can be summoned.
const TOGGLE_SIZE: f64 = 26.0;

pub struct DbClientApp {
    session: Session,
    editor: SqlEditor,
    grid: GridView,
    data_grid: GridView,
    tree: SchemaTree,
    modal: ConnectModal,
    /// Dispatches saved-connection CRUD through the shared action model, so the
    /// connection list is collaborative, undoable document state.
    actions: ui::ActionHost,
    /// Id of a connection the local user just added and wants opened as soon as
    /// it lands back through the document reconcile (peer-added connections do
    /// not set this, so they are listed but not auto-connected).
    pending_connect_id: Option<Uuid>,

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
    /// Hovered connection row index in the sidebar.
    conn_hover: Option<usize>,
    /// Rects of the editor toolbar run button + snippet button (for hits).
    run_btn: Rect,
    snippet_btn: Rect,
    new_conn_btn: Rect,
    /// Call-to-action button in the main-panel empty state (opens the modal).
    empty_new_conn_btn: Rect,
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
    /// The last summary the bubble showed, so it is poked only on a real change
    /// rather than every frame (draw recomputes the label unconditionally).
    last_status_label: Option<String>,
    /// `--shots` forces the bubble fully visible so a still frame captures it.
    shots_status: bool,
}

impl DbClientApp {
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
        app
    }

    fn build(session: Session, demo: bool) -> Self {
        let mut app = Self {
            session,
            demo,
            editor: SqlEditor::new(),
            grid: GridView::new(),
            data_grid: GridView::new(),
            tree: SchemaTree::new(),
            modal: ConnectModal::new(),
            actions: ui::ActionHost::new(),
            pending_connect_id: None,
            rect: Rect::ZERO,
            node: None,
            seeded: false,
            cursor: Point::ZERO,
            sidebar_rect: Rect::ZERO,
            main_rect: Rect::ZERO,
            empty_new_conn_btn: Rect::ZERO,
            flap: FlapState::open_when_docked(),
            last_tick: Instant::now(),
            scrim_rect: Rect::ZERO,
            toggle_btn: Rect::ZERO,
            run_hover: false,
            snippets_open: false,
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
            last_status_label: None,
            shots_status: false,
        };
        // Seed the last-shown summary so a fresh app doesn't flash the bubble on
        // its first frame; it surfaces only when the status actually changes.
        app.last_status_label = Some(app.status_bubble_label());
        app
    }

    fn run_current_query(&mut self) {
        let sql = self.editor.buffer().to_string();
        self.session.run_query(&sql);
        self.dirty = true;
    }

    fn load_snippet(&mut self, sql: &str, doc: &mut AutoCommit) {
        EditorDoc::set_sql(doc, sql);
        self.editor.sync(doc);
        self.snippets_open = false;
        self.dirty = true;
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
        self.actions
            .dispatch(doc, crate::actions::DB_ACTION_TYPE, &action);
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
        self.actions
            .dispatch(doc, crate::actions::DB_ACTION_TYPE, &action);
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
        self.status_bubble.poke(Instant::now());
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
        self.tree.set_rect(tree_rect);

        // Main panel: tab bar, then tab content.
        let content_top = self.main_rect.y0 + TABBAR_H;
        match self.session.tab {
            MainTab::Query => {
                // toolbar, editor (top 45%), grid (rest)
                let toolbar_y = content_top;
                let editor_top = toolbar_y + TOOLBAR_H;
                let avail = self.main_rect.y1 - editor_top;
                let editor_h = (avail * 0.42).clamp(120.0, 360.0);
                let editor_rect = Rect::new(
                    self.main_rect.x0 + 8.0,
                    editor_top,
                    self.main_rect.x1 - 8.0,
                    editor_top + editor_h,
                );
                self.editor.set_rect(editor_rect);
                let grid_rect = Rect::new(
                    self.main_rect.x0 + 8.0,
                    editor_top + editor_h + 30.0, // gap for the result message strip
                    self.main_rect.x1 - 8.0,
                    self.main_rect.y1 - 8.0,
                );
                self.grid.set_rect(grid_rect);
            }
            MainTab::Data => {
                let grid_rect = Rect::new(
                    self.main_rect.x0 + 8.0,
                    content_top + TOOLBAR_H,
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
        let empty_hint = if self.session.connections.is_empty() { 52.0 } else { 0.0 };
        32.0 + rows * 44.0 + empty_hint + 30.0 + 16.0
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
}

impl Widget for DbClientApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
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
            if let Some(id) = self.pending_connect_id {
                if let Some(idx) = self.session.connections.iter().position(|c| c.id == id) {
                    self.session.active = idx;
                    self.session.connect_active();
                    self.pending_connect_id = None;
                    self.dirty = true;
                }
            }
        }

        self.editor.sync(state);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
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
        taffy::Size { width: w, height: h }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        // Drop a stale user preference when the window crosses the breakpoint
        // (a resize returns the sidebar to the new layout's default), then
        // advance the eased reveal toward the resolved open/closed target.
        let split = self.split_layout();
        self.flap.reconcile(split);
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64();
        self.last_tick = now;
        self.flap.tick(split, dt);
        self.relayout();
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
        self.draw_main(scene, transform, tr, theme);

        if self.scrim_rect.width() > 0.0 {
            // Scrim opacity tracks the reveal so it fades in with the flap.
            let alpha = (self.flap.reveal() * 128.0).round().clamp(0.0, 255.0) as u8;
            let scrim = Color::from_rgba8(0, 0, 0, alpha);
            scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);
        }
        if self.sidebar_visible() {
            self.draw_sidebar(scene, transform, tr, theme);
        }

        // Hamburger toggle over the tab bar whenever the sidebar isn't fully
        // docked-open (collapsed-docked or overlay), so it can be summoned.
        self.draw_sidebar_toggle(scene, transform, tr, theme);

        self.draw_status_bubble(scene, transform, tr, theme);

        // Modal overlay drawn last (top layer).
        if self.modal.is_open() {
            self.modal.draw(scene, transform, tr, theme, self.rect, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Modal swallows events while open.
        if self.modal.is_open() {
            let mut act = None;
            self.modal
                .handle_event(event, scale_factor, modifiers, handled, &mut act);
            if let Some(profile) = act {
                self.save_connection(profile, state);
                self.dirty = true;
            }
            return;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                self.tree.set_cursor(self.cursor);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx_px, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (*x as f64 * 40.0, *y as f64),
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
                            .map(|r| r.rows.len())
                            .unwrap_or(0);
                        self.grid.scroll(step, total);
                        if dx_px != 0.0 {
                            self.grid.scroll_h(dx_px);
                        }
                    }
                    *handled = true;
                } else if self.session.tab == MainTab::Data
                    && self.data_grid.rect().contains(self.cursor)
                {
                    if shift {
                        self.data_grid.scroll_h(-dy * 40.0);
                    } else {
                        let total = self
                            .session
                            .data_result
                            .as_ref()
                            .map(|r| r.rows.len())
                            .unwrap_or(0);
                        self.data_grid.scroll(step, total);
                        if dx_px != 0.0 {
                            self.data_grid.scroll_h(dx_px);
                        }
                    }
                    *handled = true;
                } else if self.tree.rect().contains(self.cursor) {
                    self.tree.scroll(step);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_click(state, handled);
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
                let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
                // Global shortcut: Ctrl+Enter runs regardless of focus.
                if ctrl {
                    if let Key::Named(NamedKey::Enter) = logical_key {
                        self.run_current_query();
                        *handled = true;
                        return;
                    }
                    if let Key::Character(c) = logical_key {
                        if c.eq_ignore_ascii_case("n") {
                            self.modal.open_new();
                            *handled = true;
                            return;
                        }
                    }
                }
                // Esc closes the snippet menu, then dismisses an open overlay
                // flap (in docked mode Esc leaves the sidebar untouched).
                if let Key::Named(NamedKey::Escape) = logical_key {
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
                        return;
                    }
                }
            }
            _ => {}
        }

        // Route remaining events to the editor (it checks focus internally).
        // Skip anything the app already consumed — e.g. a sidebar / scrim /
        // toggle click, or an Esc that dismissed the overlay flap — so an open
        // sidebar doesn't leak clicks into the editor behind it.
        if self.session.tab == MainTab::Query && !*handled {
            self.editor
                .handle_event(event, scale_factor, modifiers, state, handled);
            if self.editor.take_run() {
                self.run_current_query();
            }
            if self.editor.take_mutated() {
                self.dirty = true;
            }
        }
    }

    fn name(&self) -> &str {
        "DbClientApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming while the sidebar flap is sliding, the worker has
        // a connect/query in flight (so its result lands without further input),
        // OR the floating status bubble is still lingering/fading (so it hides
        // itself without a stray pointer event).
        self.flap_animating()
            || self.session.busy()
            || self.status_bubble.needs_frame(Instant::now())
    }

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        // Hand the action model's undo bracket to the host so saved-connection
        // CRUD becomes a single undo boundary and replicates to peers.
        self.actions.take_committed()
    }
}

impl DbClientApp {
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

        // Hamburger toggle (summon / dismiss the sidebar).
        if self.show_toggle() && self.toggle_btn.contains(c) {
            self.flap.toggle(self.split_layout());
            self.dirty = true;
            *handled = true;
            return;
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
            return;
        }

        // Empty-state call to action in the main panel (opens the modal).
        if self.session.connections.is_empty() && self.empty_new_conn_btn.contains(c) {
            self.modal.open_new();
            *handled = true;
            return;
        }

        // Sidebar widgets only when the sidebar is interactive.
        if self.sidebar_interactive() {
            // "New connection" button.
            if self.new_conn_btn.contains(c) {
                self.modal.open_new();
                *handled = true;
                return;
            }

            // Per-row delete ("×") button, shown on hover — takes priority
            // over selecting the row it sits on.
            for (idx, dr) in self.conn_delete_rects.clone() {
                if dr.contains(c) {
                    self.remove_connection(idx, state);
                    self.dirty = true;
                    *handled = true;
                    return;
                }
            }

            // Connection rows.
            for (idx, r) in self.conn_rects.clone() {
                if r.contains(c) {
                    self.session.select_connection(idx);
                    self.dirty = true;
                    *handled = true;
                    return;
                }
            }
        }

        // Tab bar.
        for (tab, r) in self.tab_rects.clone() {
            if r.contains(c) {
                self.session.tab = tab;
                if tab == MainTab::Data {
                    self.session.load_table_data();
                }
                self.relayout();
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // Toolbar buttons (Query tab).
        if self.session.tab == MainTab::Query {
            if self.run_btn.contains(c) {
                self.run_current_query();
                *handled = true;
                return;
            }
            if self.snippet_btn.contains(c) {
                self.snippets_open = !self.snippets_open;
                *handled = true;
                return;
            }
            // Snippet menu items.
            if self.snippets_open {
                let items = crate::sample::starter_queries();
                let mut y = self.snippet_btn.y1 + 2.0;
                for (_, sql) in items.iter() {
                    let item = Rect::new(self.snippet_btn.x0, y, self.snippet_btn.x0 + 240.0, y + 28.0);
                    if item.contains(c) {
                        self.load_snippet(sql, state);
                        *handled = true;
                        return;
                    }
                    y += 28.0;
                }
                // Click elsewhere closes the menu.
                self.snippets_open = false;
            }

            // Header click → sort.
            if let Some(col) = self.grid.header_hit(c) {
                if let Some(res) = &self.session.query_result {
                    self.grid.sort_by(col, res);
                    *handled = true;
                    return;
                }
            }
        }

        // Data tab grid sorting + paging.
        if self.session.tab == MainTab::Data {
            if let Some(col) = self.data_grid.header_hit(c) {
                if let Some(res) = &self.session.data_result {
                    self.data_grid.sort_by(col, res);
                    *handled = true;
                    return;
                }
            }
        }

        // Schema tree (a sidebar widget — gated like the rest of the sidebar).
        if self.sidebar_interactive() {
            if let Some(click) = self.tree.hit(c) {
                match click {
                    TreeClick::ToggleTable(name) => {
                        self.tree.toggle(&name);
                    }
                    TreeClick::SelectTable(name) => {
                        self.session.select_table(&name);
                        // Auto-switch to Data when picking a table from the tree.
                        if self.session.tab == MainTab::Structure {
                            // keep structure
                        }
                        self.tree.set_selected(&name);
                        // On a narrow window the sidebar is an overlay; pick a
                        // table and the flap steps aside to reveal its rows.
                        if self.split_layout() == SplitLayout::Overlay {
                            self.flap.close();
                        }
                    }
                    TreeClick::ToggleSchema(name) => self.tree.toggle_schema(&name),
                }
                self.relayout();
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // Click into the editor body focuses it (editor handles caret).
        if self.session.tab == MainTab::Query && self.editor.rect().contains(c) {
            self.editor.set_focused(true);
        } else {
            self.editor.set_focused(false);
        }
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
            theme.border_standard,
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
        let active = self.session.active;
        for (idx, conn) in self.session.connections.iter().enumerate() {
            let row = Rect::new(r.x0 + 6.0, y, r.x1 - 6.0, y + 40.0);
            self.conn_rects.push((idx, row));
            let is_active = idx == active;
            if is_active {
                scene.fill(Fill::NonZero, t, p.selection, None, &row.to_rounded_rect(6.0));
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
                Color::WHITE,
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
                let del = Rect::new(row.x1 - 26.0, y + 4.0, row.x1 - 6.0, y + 24.0);
                self.conn_delete_rects.push((idx, del));
                let dc = del.center();
                let over = del.contains(self.cursor);
                if over {
                    scene.fill(Fill::NonZero, t, p.hover, None, &del.to_rounded_rect(5.0));
                }
                let col = if over {
                    theme.tokens.colors.semantic.danger
                } else {
                    theme.text_dim
                };
                let a = 4.0;
                scene.stroke(&Stroke::new(1.5), t, col, None,
                    &Line::new(Point::new(dc.x - a, dc.y - a), Point::new(dc.x + a, dc.y + a)));
                scene.stroke(&Stroke::new(1.5), t, col, None,
                    &Line::new(Point::new(dc.x + a, dc.y - a), Point::new(dc.x - a, dc.y + a)));
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
            y += 44.0;
        }

        // Empty state: no saved connections yet.
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
        let btn = Rect::new(r.x0 + 8.0, y, r.x1 - 8.0, y + 30.0);
        self.new_conn_btn = btn;
        let btn_color = if btn.contains(self.cursor) {
            theme.accent_hover
        } else {
            theme.accent_primary
        };
        scene.fill(Fill::NonZero, t, btn_color, None, &btn.to_rounded_rect(6.0));
        tr.draw_text(
            scene,
            "+  New connection",
            12.0,
            Color::WHITE,
            t * Affine::translate((btn.x0 + 12.0, btn.y0 + 8.0)),
            None,
        );

        // Schema tree (its own region; draws db→schema→table→columns).
        self.tree.draw(scene, t, tr, theme, &self.session);
    }

    fn draw_main(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let p = theme.db();
        // Tab bar.
        let bar = Rect::new(
            self.main_rect.x0,
            self.main_rect.y0,
            self.main_rect.x1,
            self.main_rect.y0 + TABBAR_H,
        );
        scene.fill(Fill::NonZero, t, p.header_bg, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            theme.border_standard,
            None,
            &Line::new(Point::new(bar.x0, bar.y1), Point::new(bar.x1, bar.y1)),
        );

        self.tab_rects.clear();
        // Reserve space for the hamburger toggle at the left of the tab bar
        // whenever the sidebar isn't fully docked-open.
        let toggle_inset = if self.show_toggle() { TOGGLE_SIZE + 14.0 } else { 0.0 };
        let mut x = bar.x0 + 8.0 + toggle_inset;
        for tab in MainTab::all() {
            let label = tab.label();
            let (lw, _) = tr.measure(label, 13.0, None);
            let w = lw as f64 + 28.0;
            let tab_rect = Rect::new(x, bar.y0 + 6.0, x + w, bar.y1 - 6.0);
            self.tab_rects.push((tab, tab_rect));
            let active = tab == self.session.tab;
            if active {
                scene.fill(Fill::NonZero, t, p.selection, None, &tab_rect.to_rounded_rect(6.0));
            } else if tab_rect.contains(self.cursor) {
                scene.fill(Fill::NonZero, t, p.hover, None, &tab_rect.to_rounded_rect(6.0));
            }
            let color = if active { theme.text_primary } else { theme.text_dim };
            tr.draw_text(
                scene,
                label,
                13.0,
                color,
                t * Affine::translate((x + 14.0, bar.y0 + 11.0)),
                None,
            );
            x += w + 4.0;
        }

        // Selected-table breadcrumb on the right of the tab bar.
        if let Some(name) = &self.session.selected_table {
            let crumb = format!("main · {name}");
            let (cw, _) = tr.measure(&crumb, 12.0, None);
            tr.draw_text(
                scene,
                &crumb,
                12.0,
                theme.text_dim,
                t * Affine::translate((bar.x1 - cw as f64 - 14.0, bar.y0 + 12.0)),
                None,
            );
        }

        // Empty state: nothing to query until the user adds a connection.
        // A composed zero-state — a database mark, a clear title + hint, and a
        // single primary action — rather than two lines of bare text.
        if self.session.connections.is_empty() {
            let body = Rect::new(
                self.main_rect.x0,
                bar.y1,
                self.main_rect.x1,
                self.main_rect.y1,
            );
            let cx = body.center().x;
            let card_w = 380.0_f64.min(body.width() - 32.0);
            let card_h = 250.0;
            let card = Rect::new(
                cx - card_w / 2.0,
                body.center().y - card_h / 2.0,
                cx + card_w / 2.0,
                body.center().y + card_h / 2.0,
            );
            scene.fill(Fill::NonZero, t, theme.bg_primary, None, &card.to_rounded_rect(14.0));
            scene.stroke(
                &Stroke::new(1.0),
                t,
                theme.border_standard,
                None,
                &card.to_rounded_rect(14.0),
            );

            // Database-cylinder mark, drawn from the accent so it re-tints.
            let accent = theme.accent_primary;
            let icx = cx;
            let icy = card.y0 + 34.0;
            let rx = 21.0;
            let ry = 6.5;
            let bh = 30.0;
            scene.fill(
                Fill::NonZero,
                t,
                p.selection,
                None,
                &Rect::new(icx - rx, icy, icx + rx, icy + bh),
            );
            scene.fill(
                Fill::NonZero,
                t,
                p.selection,
                None,
                &vello::kurbo::Ellipse::new((icx, icy + bh), (rx, ry), 0.0),
            );
            scene.stroke(
                &Stroke::new(1.3),
                t,
                accent,
                None,
                &Line::new(Point::new(icx - rx, icy), Point::new(icx - rx, icy + bh)),
            );
            scene.stroke(
                &Stroke::new(1.3),
                t,
                accent,
                None,
                &Line::new(Point::new(icx + rx, icy), Point::new(icx + rx, icy + bh)),
            );
            scene.stroke(
                &Stroke::new(1.3),
                t,
                accent,
                None,
                &vello::kurbo::Ellipse::new((icx, icy + bh * 0.5), (rx, ry), 0.0),
            );
            scene.fill(
                Fill::NonZero,
                t,
                accent,
                None,
                &vello::kurbo::Ellipse::new((icx, icy), (rx, ry), 0.0),
            );

            let title = "No connection yet";
            let hint = "Connect to a PostgreSQL, MySQL or SQLite database to browse its schema and run queries.";
            let (tw, _) = tr.measure(title, 17.0, None);
            tr.draw_text(
                scene,
                title,
                17.0,
                theme.text_primary,
                t * Affine::translate((cx - tw as f64 / 2.0, card.y0 + 92.0)),
                None,
            );
            tr.draw_text(
                scene,
                hint,
                12.5,
                theme.text_dim,
                t * Affine::translate((card.x0 + 26.0, card.y0 + 120.0)),
                Some((card_w - 52.0) as f32),
            );

            // Primary action.
            let btn_w = 188.0;
            let btn = Rect::new(
                cx - btn_w / 2.0,
                card.y1 - 54.0,
                cx + btn_w / 2.0,
                card.y1 - 20.0,
            );
            self.empty_new_conn_btn = btn;
            let btn_bg = if btn.contains(self.cursor) {
                theme.accent_hover
            } else {
                theme.accent_primary
            };
            scene.fill(Fill::NonZero, t, btn_bg, None, &btn.to_rounded_rect(7.0));
            let label = "+  New connection";
            let (lw, _) = tr.measure(label, 13.0, None);
            tr.draw_text(
                scene,
                label,
                13.0,
                Color::WHITE,
                t * Affine::translate((cx - lw as f64 / 2.0, btn.y0 + 9.0)),
                None,
            );
            return;
        }

        match self.session.tab {
            MainTab::Query => self.draw_query_tab(scene, t, tr, theme),
            MainTab::Data => self.draw_data_tab(scene, t, tr, theme),
            MainTab::Structure => self.draw_structure_tab(scene, t, tr, theme),
        }
    }

    /// Hamburger / close toggle at the left of the tab bar. Shown whenever the
    /// sidebar isn't fully docked-open, so a collapsed sidebar can be summoned
    /// and an overlay sidebar dismissed. Drawn over everything (after the
    /// sidebar) so it stays reachable while the flap is open. Records its rect
    /// in `toggle_btn` for hit-testing; clears it (to `ZERO`) when hidden.
    fn draw_sidebar_toggle(&mut self, scene: &mut Scene, t: Affine, _tr: &mut TextRenderer, theme: &Theme) {
        if !self.show_toggle() {
            self.toggle_btn = Rect::ZERO;
            return;
        }
        let p = theme.db();
        let bar_y0 = self.main_rect.y0;
        let bx = self.main_rect.x0 + 10.0;
        let by = bar_y0 + (TABBAR_H - TOGGLE_SIZE) / 2.0;
        let btn = Rect::new(bx, by, bx + TOGGLE_SIZE, by + TOGGLE_SIZE);
        self.toggle_btn = btn;

        let bg = if btn.contains(self.cursor) { p.hover } else { p.header_bg };
        scene.fill(Fill::NonZero, t, bg, None, &btn.to_rounded_rect(6.0));
        scene.stroke(&Stroke::new(1.0), t, theme.border_standard, None, &btn.to_rounded_rect(6.0));

        let cx = btn.x0 + TOGGLE_SIZE / 2.0;
        let cy = btn.y0 + TOGGLE_SIZE / 2.0;
        let open = self.flap.reveal() > 0.5;
        if open {
            // Close "X" while the sidebar is showing.
            let r = 5.0;
            scene.stroke(&Stroke::new(1.6), t, theme.text_primary, None,
                &Line::new(Point::new(cx - r, cy - r), Point::new(cx + r, cy + r)));
            scene.stroke(&Stroke::new(1.6), t, theme.text_primary, None,
                &Line::new(Point::new(cx + r, cy - r), Point::new(cx - r, cy + r)));
        } else {
            // Three-line hamburger when collapsed.
            let half = 6.0;
            for i in 0..3 {
                let ly = cy - 5.0 + i as f64 * 5.0;
                scene.stroke(&Stroke::new(1.6), t, theme.text_primary, None,
                    &Line::new(Point::new(cx - half, ly), Point::new(cx + half, ly)));
            }
        }
    }

    fn draw_query_tab(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let p = theme.db();
        // Toolbar.
        let toolbar = Rect::new(
            self.main_rect.x0,
            self.main_rect.y0 + TABBAR_H,
            self.main_rect.x1,
            self.main_rect.y0 + TABBAR_H + TOOLBAR_H,
        );
        scene.fill(Fill::NonZero, t, p.panel_bg, None, &toolbar);

        // Run button.
        let run = Rect::new(toolbar.x0 + 10.0, toolbar.y0 + 6.0, toolbar.x0 + 160.0, toolbar.y1 - 6.0);
        self.run_btn = run;
        let run_color = if self.run_hover { theme.accent_hover } else { theme.accent_primary };
        scene.fill(Fill::NonZero, t, run_color, None, &run.to_rounded_rect(6.0));
        tr.draw_text(
            scene,
            "▶  Run",
            13.0,
            Color::WHITE,
            t * Affine::translate((run.x0 + 14.0, run.y0 + 7.0)),
            None,
        );
        tr.draw_text(
            scene,
            "⌘↵",
            12.0,
            Color::from_rgba8(255, 255, 255, 190),
            t * Affine::translate((run.x1 - 32.0, run.y0 + 7.0)),
            None,
        );

        // Snippets button.
        let snip = Rect::new(run.x1 + 8.0, toolbar.y0 + 6.0, run.x1 + 118.0, toolbar.y1 - 6.0);
        self.snippet_btn = snip;
        let snip_color = if snip.contains(self.cursor) || self.snippets_open {
            p.hover
        } else {
            theme.bg_primary
        };
        scene.fill(Fill::NonZero, t, snip_color, None, &snip.to_rounded_rect(6.0));
        scene.stroke(
            &Stroke::new(1.0),
            t,
            theme.border_standard,
            None,
            &snip.to_rounded_rect(6.0),
        );
        tr.draw_text(
            scene,
            "Snippets ▾",
            12.5,
            theme.text_primary,
            t * Affine::translate((snip.x0 + 12.0, snip.y0 + 8.0)),
            None,
        );

        // Editor.
        self.editor.draw(scene, t, tr, theme);

        // Result message strip between editor and grid.
        let strip_y = self.editor.rect().y1 + 4.0;
        let (msg, color) = match &self.session.status {
            RunStatus::Ok(m) => (m.clone(), theme.tokens.colors.semantic.success),
            RunStatus::Error(m) => (format!("✕ {m}"), theme.tokens.colors.semantic.danger),
            RunStatus::Busy(m) => (m.clone(), theme.accent_primary),
            RunStatus::Idle => ("Ready".to_string(), theme.text_dim),
        };
        tr.draw_text(
            scene,
            &msg,
            12.0,
            color,
            t * Affine::translate((self.main_rect.x0 + 12.0, strip_y + 4.0)),
            None,
        );

        // Grid.
        if let Some(res) = &self.session.query_result {
            self.grid.draw(scene, t, tr, theme, res);
        }

        // Snippet dropdown (drawn last so it overlays the grid).
        if self.snippets_open {
            self.draw_snippet_menu(scene, t, tr, theme);
        }
    }

    fn draw_snippet_menu(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let p = theme.db();
        let items = crate::sample::starter_queries();
        let menu = Rect::new(
            self.snippet_btn.x0,
            self.snippet_btn.y1 + 2.0,
            self.snippet_btn.x0 + 240.0,
            self.snippet_btn.y1 + 2.0 + items.len() as f64 * 28.0,
        );
        scene.fill(Fill::NonZero, t, theme.bg_primary, None, &menu.to_rounded_rect(8.0));
        scene.stroke(&Stroke::new(1.0), t, theme.border_standard, None, &menu.to_rounded_rect(8.0));
        let mut y = menu.y0;
        for (name, _) in items.iter() {
            let item = Rect::new(menu.x0, y, menu.x1, y + 28.0);
            if item.contains(self.cursor) {
                scene.fill(Fill::NonZero, t, p.hover, None, &item);
            }
            tr.draw_text(
                scene,
                name,
                12.0,
                theme.text_primary,
                t * Affine::translate((item.x0 + 12.0, y + 7.0)),
                None,
            );
            y += 28.0;
        }
    }

    fn draw_data_tab(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let p = theme.db();
        let toolbar = Rect::new(
            self.main_rect.x0,
            self.main_rect.y0 + TABBAR_H,
            self.main_rect.x1,
            self.main_rect.y0 + TABBAR_H + TOOLBAR_H,
        );
        scene.fill(Fill::NonZero, t, p.panel_bg, None, &toolbar);

        match (&self.session.selected_table, &self.session.data_result) {
            (Some(name), Some(res)) => {
                let total = self.session.selected_table_rows();
                let from = self.session.data_offset + 1;
                let to = (self.session.data_offset + res.rows.len()).min(total as usize);
                let info = format!(
                    "{name}   ·   rows {from}–{to} of {total}",
                );
                tr.draw_text(
                    scene,
                    &info,
                    13.0,
                    theme.text_primary,
                    t * Affine::translate((toolbar.x0 + 12.0, toolbar.y0 + 11.0)),
                    None,
                );
                self.data_grid.draw(scene, t, tr, theme, res);
            }
            _ => {
                tr.draw_text(
                    scene,
                    "Select a table in the schema browser to view its rows.",
                    14.0,
                    theme.text_dim,
                    t * Affine::translate((toolbar.x0 + 12.0, toolbar.y0 + 12.0)),
                    None,
                );
            }
        }
    }

    fn draw_structure_tab(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let content = Rect::new(
            self.main_rect.x0 + 8.0,
            self.main_rect.y0 + TABBAR_H + 8.0,
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
    fn draw_status_bubble(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let now = Instant::now();
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
mod responsive_tests {
    use super::*;

    /// Drive the real layout passes (build_layout → compute_layout →
    /// apply_layout) so assertions exercise the production geometry path, not
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

    #[test]
    fn breakpoint_resolves_from_allocated_width() {
        assert_eq!(SplitLayout::from_width(BREAKPOINT, BREAKPOINT), SplitLayout::Docked);
        assert_eq!(SplitLayout::from_width(BREAKPOINT - 1.0, BREAKPOINT), SplitLayout::Overlay);
    }

    #[test]
    fn wide_window_docks_the_sidebar_beside_inset_content() {
        let mut app = DbClientApp::new();
        lay_out(&mut app, 1280.0, 800.0);
        assert_eq!(app.split_layout(), SplitLayout::Docked);
        // Sidebar is docked at full width; the main panel is inset past it.
        assert!(app.sidebar_rect.width() > SIDEBAR_W - 1.0, "docked sidebar is full width");
        assert!(
            app.main_rect.x0 > app.rect.x0 + SIDEBAR_W - 1.0,
            "docked main panel is inset past the sidebar"
        );
        // No scrim while docked.
        assert_eq!(app.scrim_rect.width(), 0.0, "docked layout paints no scrim");
    }

    #[test]
    fn narrow_window_collapses_sidebar_to_a_closed_overlay() {
        let mut app = DbClientApp::new();
        app.snap_flap_for_width(720.0); // shots/live default for narrow
        lay_out(&mut app, 720.0, 900.0);
        assert_eq!(app.split_layout(), SplitLayout::Overlay);
        assert!(!app.flap.is_open(SplitLayout::Overlay), "narrow sidebar is closed by default");
        // Main panel spans the full width; sidebar is slid off-screen left.
        assert!(
            (app.main_rect.width() - app.rect.width()).abs() < 1.0,
            "overlay content spans full width while the flap is closed"
        );
        assert!(app.sidebar_rect.x1 <= app.rect.x0 + 1.0, "closed overlay sidebar is off-screen");
        assert_eq!(app.scrim_rect.width(), 0.0, "closed overlay paints no scrim");
        // The hamburger toggle is offered so the sidebar can be summoned.
        assert!(app.show_toggle(), "narrow layout offers the summon toggle");
    }

    #[test]
    fn opening_the_overlay_flap_slides_it_flush_behind_a_scrim() {
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
        assert!(app.scrim_rect.width() > 0.0, "open overlay flap dims the content with a scrim");
    }

    #[test]
    fn widening_past_the_breakpoint_restores_the_docked_sidebar() {
        let mut app = DbClientApp::new();
        // Start narrow + closed, then widen past the breakpoint.
        lay_out(&mut app, 720.0, 900.0);
        assert_eq!(app.split_layout(), SplitLayout::Overlay);
        lay_out(&mut app, 1280.0, 800.0);
        app.flap.snap(true); // settle the reconciled docked-open default
        lay_out(&mut app, 1280.0, 800.0);
        assert_eq!(app.split_layout(), SplitLayout::Docked);
        assert!(app.flap.is_open(SplitLayout::Docked), "widening returns to the docked-open default");
        assert!(app.main_rect.x0 > app.rect.x0 + 1.0, "docked content is inset by the restored sidebar");
    }
}

#[cfg(test)]
mod honesty_tests {
    use super::*;

    #[test]
    fn live_app_starts_empty() {
        // The live constructor path fabricates nothing: no connections, no
        // live engine, no schema.
        let app = DbClientApp::build(Session::new(), false);
        assert!(!app.demo, "live build is not the demo build");
        assert!(app.session.connections.is_empty(), "no fabricated connections");
        assert!(!app.session.connected, "no live engine on a fresh machine");
        assert_eq!(app.session.schema.table_count(), 0, "no fabricated schema");
    }

    #[test]
    fn demo_app_seeds_the_sample() {
        // The demo builder is the sole source of the sample database.
        let app = DbClientApp::new_demo();
        assert!(app.demo, "demo build is flagged");
        assert!(!app.session.connections.is_empty(), "demo seeds sample connections");
        assert!(app.session.connected, "demo opens the sample engine");
        assert!(app.session.schema.table_count() >= 4, "demo has the sample schema");
    }
}

#[cfg(test)]
mod behavior_tests {
    use super::*;
    use automerge::ROOT;
    use qos_actions::Action;
    use winit::event::{DeviceId, KeyEvent};
    use winit::keyboard::{
        KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey, SmolStr,
    };

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
        WindowEvent::KeyboardInput { device_id: device_id(), event: ev, is_synthetic: false }
    }

    fn enter_event() -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Enter);
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput { device_id: device_id(), event: ev, is_synthetic: false }
    }

    fn send(app: &mut DbClientApp, doc: &mut AutoCommit, ev: &WindowEvent, mods: &Modifiers) -> bool {
        let taffy = TaffyTree::<()>::new();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(ev, 1.0, mods, doc, &ROOT, &mut handled, &mut focus, NodeId::new(0), &taffy);
        handled
    }

    fn synced() -> (DbClientApp, AutoCommit) {
        let mut app = DbClientApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    /// A keystroke driven through the production `handle_event` splices into the
    /// shared SQL `Text` and converges byte-identically on a peer — the native
    /// text path, unchanged by the async engine.
    #[test]
    fn typing_replicates_byte_identically_on_a_peer() {
        let (mut app, mut doc) = synced();
        doc.commit();
        let base = doc.save();
        let before = crate::doc::EditorDoc::read_sql(&doc);

        let mut peer = AutoCommit::load(&base).unwrap();
        assert_eq!(doc.get_heads(), peer.get_heads(), "precondition: replicas share a base");

        assert!(send(&mut app, &mut doc, &char_event("Z"), &Modifiers::default()));
        doc.commit();
        let after = crate::doc::EditorDoc::read_sql(&doc);
        assert_eq!(after.chars().count(), before.chars().count() + 1, "one char inserted");

        peer.merge(&mut doc).unwrap();
        assert_eq!(doc.get_heads(), peer.get_heads(), "byte-identical convergence");
    }

    /// Running a query is external: Ctrl+Enter dispatches work to the engine
    /// worker and writes nothing to the shared document.
    #[test]
    fn running_a_query_writes_nothing_to_the_document() {
        let (mut app, mut doc) = synced();
        doc.commit();
        let before = doc.get_heads();

        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        assert!(send(&mut app, &mut doc, &enter_event(), &ctrl), "Ctrl+Enter runs the query");
        assert_eq!(
            doc.get_heads(),
            before,
            "query execution is a driver side effect, never a document mutation"
        );
    }

    /// Saving a connection goes through the action model: the metadata lands in
    /// `root["connections"]`, an undo boundary is recorded, the password never
    /// enters the document, and the app remembers to connect it.
    #[test]
    fn saving_a_connection_dispatches_an_action_into_the_doc() {
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
            app.take_committed_action().is_some(),
            "the action host recorded an undo boundary for the CRUD"
        );
        assert_eq!(app.pending_connect_id, Some(id), "the app will open it on reconcile");

        let bytes = doc.save();
        assert!(
            !bytes.windows(7).any(|w| w == b"hunter2"),
            "the password must never enter the shared document"
        );
    }

    /// A reconcile from the document surfaces a peer-added connection in the
    /// live session without auto-connecting it (no local password).
    #[test]
    fn reconcile_surfaces_document_connections() {
        let mut app = DbClientApp::build(Session::new(), false);
        let mut doc = AutoCommit::new();
        // A peer (or a prior session) added a connection to the document.
        let p = ConnectionProfile::new("Team DB", Engine::MySql);
        crate::actions::add_connection_action(&p).apply(&mut doc).unwrap();

        app.sync(&mut doc, &ROOT);
        assert_eq!(app.session.connections.len(), 1, "the document connection is listed");
        assert_eq!(app.session.connections[0].name, "Team DB");
        assert!(!app.session.connected, "a peer-added connection is not auto-opened");
    }
}
