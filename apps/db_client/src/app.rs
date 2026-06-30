//! Root widget for the database client. Owns the [`Session`] (connections,
//! engine, schema, results) and the [`SqlEditor`] (CRDT-backed buffer), lays
//! out the three regions (connections+schema sidebar, main panel, status
//! bar) itself, and draws everything directly into the Vello scene.
//!
//! Layout is hand-rolled rather than delegated to Taffy because the panes are
//! a fixed split that responds to the viewport rect — a single full-bleed
//! leaf node is handed to Taffy and every sub-rect is computed from it. Event
//! routing dispatches to the schema tree, the tab bar, the editor and the
//! results grid by hit-testing their rects.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{Fill, Color};
use vello::Scene;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::connect_modal::ConnectModal;
use crate::doc::EditorDoc;
use crate::editor::SqlEditor;
use crate::grid::GridView;
use crate::model::Engine;
use crate::schema_tree::{SchemaTree, TreeClick};
use crate::session::{MainTab, RunStatus, Session};
use crate::theme_ext::ThemeExt;

const SIDEBAR_W: f64 = 268.0;
const STATUS_H: f64 = 28.0;
const TABBAR_H: f64 = 38.0;
const TOOLBAR_H: f64 = 40.0;

pub struct DbClientApp {
    session: Session,
    editor: SqlEditor,
    grid: GridView,
    data_grid: GridView,
    tree: SchemaTree,
    modal: ConnectModal,

    rect: Rect,
    node: Option<NodeId>,
    /// Whether the editor buffer in the CRDT has been seeded yet.
    seeded: bool,
    /// Last cursor position (logical).
    cursor: Point,
    /// Cached sub-rects, recomputed each `apply_layout`.
    sidebar_rect: Rect,
    main_rect: Rect,
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
    tab_rects: Vec<(MainTab, Rect)>,
    conn_rects: Vec<(usize, Rect)>,
    /// Whether the grid bound the current result (avoids rebinding each frame).
    bound_query_gen: usize,
    bound_data_gen: usize,
    query_gen: usize,
    data_gen: usize,
    /// Drives a one-shot redraw request flag.
    dirty: bool,
}

impl DbClientApp {
    pub fn new() -> Self {
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
        Self {
            session,
            editor: SqlEditor::new(),
            grid: GridView::new(),
            data_grid: GridView::new(),
            tree: SchemaTree::new(),
            modal: ConnectModal::new(),
            rect: Rect::ZERO,
            node: None,
            seeded: false,
            cursor: Point::ZERO,
            sidebar_rect: Rect::ZERO,
            main_rect: Rect::ZERO,
            run_hover: false,
            snippets_open: false,
            conn_hover: None,
            run_btn: Rect::ZERO,
            snippet_btn: Rect::ZERO,
            new_conn_btn: Rect::ZERO,
            tab_rects: Vec::new(),
            conn_rects: Vec::new(),
            bound_query_gen: usize::MAX,
            bound_data_gen: usize::MAX,
            query_gen: 0,
            data_gen: 0,
            dirty: true,
        }
    }

    fn run_current_query(&mut self) {
        let sql = self.editor.buffer().to_string();
        self.session.run_query(&sql);
        self.query_gen += 1;
        self.dirty = true;
    }

    fn load_snippet(&mut self, sql: &str, doc: &mut AutoCommit) {
        EditorDoc::set_sql(doc, sql);
        self.editor.sync(doc);
        self.snippets_open = false;
        self.dirty = true;
    }

    /// Recompute every region rect from the root rect.
    fn relayout(&mut self) {
        let r = self.rect;
        self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + SIDEBAR_W, r.y1 - STATUS_H);
        self.main_rect = Rect::new(r.x0 + SIDEBAR_W, r.y0, r.x1, r.y1 - STATUS_H);

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
        // begin 56px below the sidebar top, advance 44px each, then the
        // "New connection" button (30px) is drawn directly below, with a
        // 16px gap before the schema browser begins.
        let rows = self.session.connections.len() as f64;
        56.0 + rows * 44.0 + 30.0 + 16.0
    }
}

impl Widget for DbClientApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            // Seed the editor buffer into the CRDT if empty.
            if EditorDoc::read_sql(state).is_empty() {
                EditorDoc::set_sql(state, crate::sample::DEFAULT_QUERY);
            }
            self.seeded = true;
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
        self.relayout();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let p = theme.db();
        // App background.
        scene.fill(Fill::NonZero, transform, p.panel_bg, None, &self.rect);

        // Bind result sets to grids if they changed.
        if self.query_gen != self.bound_query_gen {
            if let Some(res) = &self.session.query_result {
                self.grid.bind(res, tr);
            }
            self.bound_query_gen = self.query_gen;
        }
        if self.data_gen != self.bound_data_gen {
            if let Some(res) = &self.session.data_result {
                self.data_grid.bind(res, tr);
            }
            self.bound_data_gen = self.data_gen;
        }

        self.draw_sidebar(scene, transform, tr, theme);
        self.draw_main(scene, transform, tr, theme);
        self.draw_status(scene, transform, tr, theme);

        // Modal overlay drawn last (top layer).
        if self.modal.is_open() {
            self.modal.draw(scene, transform, tr, theme, self.rect);
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
                let id = self.session.add_connection(profile);
                // Select the freshly-added connection.
                if let Some(idx) = self.session.connections.iter().position(|c| c.id == id) {
                    self.session.select_connection(idx);
                }
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
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64,
                    MouseScrollDelta::PixelDelta(pos) => pos.y / 24.0,
                };
                let step = (-dy * 3.0) as isize;
                if self.session.tab == MainTab::Query && self.grid.rect().contains(self.cursor) {
                    let total = self
                        .session
                        .query_result
                        .as_ref()
                        .map(|r| r.rows.len())
                        .unwrap_or(0);
                    self.grid.scroll(step, total);
                    *handled = true;
                } else if self.session.tab == MainTab::Data
                    && self.data_grid.rect().contains(self.cursor)
                {
                    let total = self
                        .session
                        .data_result
                        .as_ref()
                        .map(|r| r.rows.len())
                        .unwrap_or(0);
                    self.data_grid.scroll(step, total);
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
                // Esc closes the snippet menu.
                if let Key::Named(NamedKey::Escape) = logical_key {
                    if self.snippets_open {
                        self.snippets_open = false;
                        *handled = true;
                        return;
                    }
                }
            }
            _ => {}
        }

        // Route remaining events to the editor (it checks focus internally).
        if self.session.tab == MainTab::Query {
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
}

impl DbClientApp {
    fn on_click(&mut self, state: &mut AutoCommit, handled: &mut bool) {
        let c = self.cursor;

        // "New connection" button.
        if self.new_conn_btn.contains(c) {
            self.modal.open_new();
            *handled = true;
            return;
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

        // Tab bar.
        for (tab, r) in self.tab_rects.clone() {
            if r.contains(c) {
                self.session.tab = tab;
                if tab == MainTab::Data {
                    self.session.load_table_data();
                    self.data_gen += 1;
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

        // Schema tree.
        if let Some(click) = self.tree.hit(c) {
            match click {
                TreeClick::ToggleTable(name) => {
                    self.tree.toggle(&name);
                }
                TreeClick::SelectTable(name) => {
                    self.session.select_table(&name);
                    self.data_gen += 1;
                    // Auto-switch to Data when picking a table from the tree.
                    if self.session.tab == MainTab::Structure {
                        // keep structure
                    }
                    self.tree.set_selected(&name);
                }
                TreeClick::ToggleSchema(name) => self.tree.toggle_schema(&name),
            }
            self.relayout();
            self.dirty = true;
            *handled = true;
            return;
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

        // App title.
        tr.draw_text(
            scene,
            "Database",
            16.0,
            theme.text_primary,
            t * Affine::translate((r.x0 + 14.0, r.y0 + 12.0)),
            None,
        );
        tr.draw_text(
            scene,
            "CONNECTIONS",
            10.0,
            theme.text_dim,
            t * Affine::translate((r.x0 + 14.0, r.y0 + 38.0)),
            None,
        );

        // Connection rows.
        self.conn_rects.clear();
        let mut y = r.y0 + 56.0;
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

            // Connection status dot.
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
            y += 44.0;
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
        let mut x = bar.x0 + 8.0;
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

        match self.session.tab {
            MainTab::Query => self.draw_query_tab(scene, t, tr, theme),
            MainTab::Data => self.draw_data_tab(scene, t, tr, theme),
            MainTab::Structure => self.draw_structure_tab(scene, t, tr, theme),
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

    fn draw_status(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let p = theme.db();
        let bar = Rect::new(self.rect.x0, self.rect.y1 - STATUS_H, self.rect.x1, self.rect.y1);
        scene.fill(Fill::NonZero, t, p.header_bg, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            theme.border_standard,
            None,
            &Line::new(Point::new(bar.x0, bar.y0), Point::new(bar.x1, bar.y0)),
        );

        let conn = self.session.active_connection();
        let left = format!(
            "{}  ·  {}  ·  {} tables",
            conn.engine.label(),
            conn.name,
            self.session.schema.table_count()
        );
        tr.draw_text(
            scene,
            &left,
            11.5,
            theme.text_dim,
            t * Affine::translate((bar.x0 + 12.0, bar.y0 + 7.0)),
            None,
        );

        // Right-aligned status message.
        let (msg, color) = match &self.session.status {
            RunStatus::Ok(m) => (m.clone(), theme.tokens.colors.semantic.success),
            RunStatus::Error(_) => ("error".to_string(), theme.tokens.colors.semantic.danger),
            RunStatus::Idle => ("ready".to_string(), theme.text_dim),
        };
        let (mw, _) = tr.measure(&msg, 11.5, None);
        tr.draw_text(
            scene,
            &msg,
            11.5,
            color,
            t * Affine::translate((bar.x1 - mw as f64 - 12.0, bar.y0 + 7.0)),
            None,
        );
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
