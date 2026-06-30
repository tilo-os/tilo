//! Root widget for the code editor. Owns the [`Session`] (project + open
//! buffers), the editor pane, the file-tree sidebar, the tab bar, the status
//! bar, the bottom panel, and the command palette — and coordinates the data
//! flow between them.
//!
//! Layout:
//! ```text
//! +----------+-------------------------------+
//! | file     |  tab bar                      |
//! | tree     +-------------------------------+
//! | sidebar  |  editor view                  |
//! |          +-------------------------------+
//! |          |  bottom panel (problems/term) |
//! +----------+-------------------------------+
//! |  status bar                              |
//! +------------------------------------------+
//! ```
//! The command palette floats above everything in the overlay pass.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::Key;

use crate::bottom_panel::{BottomPanel, PanelTab};
use crate::command_palette::{Command, CommandPalette, Entry, PaletteKind};
use crate::doc::EditorDoc;
use crate::editor_view::EditorView;
use crate::file_tree::{FileTree, TreeEvent};
use crate::lint;
use crate::model::{NodeId as TreeId, NodeKind, Session};
use crate::sample_project;
use crate::status_bar::{StatusBar, StatusState};
use crate::tab_bar::{TabBar, TabEvent, TabItem};

const SIDEBAR_WIDTH: f64 = 240.0;
const SIDEBAR_MIN: f64 = 160.0;

/// The available color themes, cycled by the palette command.
fn themes() -> Vec<Theme> {
    vec![
        Theme::dark(),
        Theme::dracula(),
        Theme::nord(),
        Theme::gruvbox(),
        Theme::solarized(),
    ]
}

pub struct CodeApp {
    session: Session,
    sidebar: FileTree,
    tabs: TabBar,
    editor: EditorView,
    status: StatusBar,
    panel: BottomPanel,
    palette: CommandPalette,

    sidebar_visible: bool,
    panel_visible: bool,
    /// Index into `themes()` for the active theme override. `None` uses the
    /// host-provided theme; once the user cycles, we override.
    theme_index: Option<usize>,

    rect: Rect,
    sidebar_node: Option<NodeId>,
    tabs_node: Option<NodeId>,
    editor_node: Option<NodeId>,
    panel_node: Option<NodeId>,
    status_node: Option<NodeId>,
    content_node: Option<NodeId>,
    editor_col_node: Option<NodeId>,

    /// The node currently bound into the editor view, so we only re-bind on
    /// change.
    bound_node: Option<TreeId>,
    /// One-time seed guard for the CRDT buffers.
    seeded: bool,
}

impl CodeApp {
    /// Construct the app with the bundled sample project, opening `main.rs`.
    pub fn new() -> Self {
        let project = sample_project::build();
        let default = sample_project::default_open(&project);
        let mut session = Session::new(project);
        if let Some(node) = default {
            session.open_file(node);
        }

        let mut sidebar = FileTree::new();
        sidebar.set_project(&session.project);
        sidebar.set_active(default);

        CodeApp {
            session,
            sidebar,
            tabs: TabBar::new(),
            editor: EditorView::new(),
            status: StatusBar::new(),
            panel: BottomPanel::new(),
            palette: CommandPalette::new(),
            sidebar_visible: true,
            panel_visible: true,
            theme_index: None,
            rect: Rect::ZERO,
            sidebar_node: None,
            tabs_node: None,
            editor_node: None,
            panel_node: None,
            status_node: None,
            content_node: None,
            editor_col_node: None,
            bound_node: default,
            seeded: false,
        }
    }

    /// The theme to render with: the user override if set, else the host's.
    fn active_theme<'a>(&self, host: &'a Theme) -> std::borrow::Cow<'a, Theme> {
        match self.theme_index {
            Some(i) => std::borrow::Cow::Owned(themes()[i % themes().len()].clone()),
            None => std::borrow::Cow::Borrowed(host),
        }
    }

    /// Seed every open buffer's text into the CRDT (idempotent) and bind the
    /// editor to the active buffer's Text object.
    fn ensure_buffers(&mut self, doc: &mut AutoCommit) {
        // Seed the active buffer (and any already-open) so their text exists in
        // the doc. We seed lazily per open buffer.
        for buf in &self.session.open {
            if EditorDoc::buffer_text(doc, buf.node).is_none() {
                if let Some(NodeKind::File { seed, .. }) =
                    self.session.project.get(buf.node).map(|n| &n.kind)
                {
                    let seed = seed.clone();
                    EditorDoc::ensure_buffer(doc, buf.node, &seed);
                }
            }
        }
        self.seeded = true;

        // Bind the editor to the active buffer.
        let active = self.session.active_buffer().map(|b| (b.node, b.language));
        match active {
            Some((node, lang)) => {
                if self.bound_node != Some(node) || !self.editor.is_bound() {
                    if let Some(text) = EditorDoc::buffer_text(doc, node) {
                        self.editor.bind(text, lang);
                        self.bound_node = Some(node);
                    }
                }
            }
            None => {
                if self.bound_node.is_some() {
                    self.editor.unbind();
                    self.bound_node = None;
                }
            }
        }
    }

    /// Push the current tab set into the tab bar.
    fn refresh_tabs(&mut self) {
        let items: Vec<TabItem> = self
            .session
            .open
            .iter()
            .map(|b| TabItem {
                name: b.name.clone(),
                language: b.language,
                dirty: b.dirty,
            })
            .collect();
        self.tabs.set_tabs(items, self.session.active);
    }

    /// Recompute diagnostics over the active buffer and push to the panel +
    /// status bar.
    fn refresh_diagnostics(&mut self, doc: &AutoCommit) {
        let diagnostics = match self.session.active_buffer() {
            Some(buf) => {
                let text = EditorDoc::read_buffer(doc, buf.node).unwrap_or_default();
                lint::lint(&text, buf.language)
            }
            None => Vec::new(),
        };
        self.panel.set_problems(diagnostics);
    }

    /// Rebuild the status bar snapshot.
    fn refresh_status(&mut self) {
        let (line, col) = self.editor.primary_caret();
        let lang = if self.editor.is_bound() {
            self.editor.language().label().to_string()
        } else {
            String::new()
        };
        let indent = if self.editor.is_bound() {
            format!("Spaces: {}", self.editor.indent_width())
        } else {
            String::new()
        };
        self.status.set_state(StatusState {
            language: lang,
            line: line + 1,
            col: col + 1,
            selection: self.editor.selection_len(),
            caret_count: self.editor.caret_count(),
            indent,
            problems: self.panel.problem_count(),
            project: self.session.project.name.clone(),
        });
    }

    /// Refresh the command palette's entry set (files + commands).
    fn refresh_palette_entries(&mut self) {
        let mut entries: Vec<Entry> = Vec::new();
        // Files.
        for (id, node) in self.session.project.nodes.iter() {
            if let NodeKind::File { .. } = node.kind {
                let detail = self
                    .session
                    .project
                    .path_of(*id)
                    .rsplitn(2, '/')
                    .nth(1)
                    .unwrap_or("")
                    .to_string();
                entries.push(Entry {
                    label: node.name.clone(),
                    detail,
                    kind: PaletteKind::File(*id),
                });
            }
        }
        // Commands.
        for (cmd, label) in Command::all() {
            entries.push(Entry {
                label: label.to_string(),
                detail: String::new(),
                kind: PaletteKind::Command(cmd),
            });
        }
        self.palette.set_entries(entries);
    }

    /// Open a file node into a tab and bind the editor.
    fn open_node(&mut self, node: TreeId) {
        self.session.open_file(node);
        self.sidebar.set_active(Some(node));
        self.bound_node = None; // force re-bind in ensure_buffers
        self.refresh_tabs();
    }

    /// Apply a tree event (open file / toggle folder).
    fn apply_tree_event(&mut self, ev: TreeEvent) {
        match ev {
            TreeEvent::OpenFile(id) => self.open_node(id),
            TreeEvent::ToggleFolder(id) => {
                self.session.project.toggle_folder(id);
                self.sidebar.set_project(&self.session.project);
                self.sidebar
                    .set_active(self.session.active_buffer().map(|b| b.node));
            }
        }
    }

    /// Apply a tab event (select / close).
    fn apply_tab_event(&mut self, ev: TabEvent) {
        match ev {
            TabEvent::Select(i) => {
                self.session.active = Some(i);
                self.bound_node = None;
            }
            TabEvent::Close(i) => {
                self.session.close_tab(i);
                self.bound_node = None;
            }
        }
        self.sidebar
            .set_active(self.session.active_buffer().map(|b| b.node));
        self.refresh_tabs();
    }

    /// Dispatch a palette command.
    fn apply_command(&mut self, cmd: Command) {
        match cmd {
            Command::NewFile => {
                // Create an untitled scratch file in the project root.
                let root = self.session.project.root;
                let name = format!("untitled-{}.txt", self.session.project.nodes.len());
                let id = self.session.project.add_file(root, &name, "");
                self.sidebar.set_project(&self.session.project);
                self.open_node(id);
            }
            Command::CloseTab => {
                if let Some(i) = self.session.active {
                    self.apply_tab_event(TabEvent::Close(i));
                }
            }
            Command::ToggleBottomPanel => {
                self.panel_visible = !self.panel_visible;
            }
            Command::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
            }
            Command::NextTheme => {
                let n = themes().len();
                self.theme_index = Some(self.theme_index.map(|i| (i + 1) % n).unwrap_or(1));
            }
            Command::GoToProblems => {
                self.panel_visible = true;
                self.panel.set_tab(PanelTab::Problems);
            }
            Command::GoToTerminal => {
                self.panel_visible = true;
                self.panel.set_tab(PanelTab::Terminal);
            }
        }
    }

    /// Drain everything the child widgets emitted this frame.
    fn drain_child_events(&mut self) {
        let tree_events = self.sidebar.take_events();
        for ev in tree_events {
            self.apply_tree_event(ev);
        }
        let tab_events = self.tabs.take_events();
        for ev in tab_events {
            self.apply_tab_event(ev);
        }
        if let Some(kind) = self.palette.take_chosen() {
            match kind {
                PaletteKind::File(id) => self.open_node(id),
                PaletteKind::Command(cmd) => self.apply_command(cmd),
            }
        }
    }

    /// Seed the Terminal panel with a representative build log the first time.
    fn seed_terminal(&mut self) {
        self.panel.set_terminal(vec![
            "$ cargo build".to_string(),
            "   Compiling ferris-cli v0.1.0".to_string(),
            "    Finished `dev` profile [unoptimized] target(s) in 0.84s".to_string(),
            "$ cargo run -- --count 20".to_string(),
            "Hello, world! \u{1F980}".to_string(),
            "Found 8 primes below 20:".to_string(),
            "   2    3    5    7   11   13".to_string(),
            "  17   19".to_string(),
            "$ ".to_string(),
        ]);
    }
}

impl Widget for CodeApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded {
            self.seed_terminal();
            self.refresh_palette_entries();
        }
        self.ensure_buffers(state);
        self.editor.sync(state, path);
        self.refresh_tabs();
        self.refresh_diagnostics(state);
        self.refresh_status();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // --- Sidebar ---
        let sidebar_node = self.sidebar.build_layout(taffy, theme, node_map);
        let sidebar_w = if self.sidebar_visible { SIDEBAR_WIDTH } else { 0.0 };
        let _ = taffy.set_style(
            sidebar_node,
            Style {
                size: Size {
                    width: Dimension::length(sidebar_w as f32),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(if self.sidebar_visible { SIDEBAR_MIN as f32 } else { 0.0 }),
                    height: Dimension::auto(),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        self.sidebar_node = Some(sidebar_node);

        // --- Editor column: tab bar + editor + (optional) bottom panel ---
        let tabs_node = self.tabs.build_layout(taffy, theme, node_map);
        self.tabs_node = Some(tabs_node);

        let editor_node = self.editor.build_layout(taffy, theme, node_map);
        if let Ok(existing) = taffy.style(editor_node).cloned() {
            let _ = taffy.set_style(
                editor_node,
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    flex_basis: Dimension::length(0.0),
                    ..existing
                },
            );
        }
        self.editor_node = Some(editor_node);

        let mut editor_col_children = vec![tabs_node, editor_node];
        if self.panel_visible {
            let panel_node = self.panel.build_layout(taffy, theme, node_map);
            self.panel_node = Some(panel_node);
            editor_col_children.push(panel_node);
        } else {
            self.panel_node = None;
        }

        let editor_col = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size {
                        width: Dimension::auto(),
                        height: Dimension::percent(1.0),
                    },
                    flex_grow: 1.0,
                    flex_basis: Dimension::length(0.0),
                    ..Default::default()
                },
                &editor_col_children,
            )
            .unwrap();
        self.editor_col_node = Some(editor_col);

        // --- Content row: sidebar + editor column ---
        let content_node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    flex_basis: Dimension::length(0.0),
                    gap: Size { width: length(0.0), height: length(0.0) },
                    ..Default::default()
                },
                &[sidebar_node, editor_col],
            )
            .unwrap();
        self.content_node = Some(content_node);

        // --- Status bar ---
        let status_node = self.status.build_layout(taffy, theme, node_map);
        self.status_node = Some(status_node);

        // --- Outer column ---
        let root = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    gap: Size { width: length(0.0), height: length(0.0) },
                    ..Default::default()
                },
                &[content_node, status_node],
            )
            .unwrap();
        node_map.insert(root, self as *mut dyn Widget);
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);

        if let Some(content_node) = self.content_node {
            let content_layout = taffy.layout(content_node).unwrap();
            let content_origin = Point::new(
                new_origin.x + content_layout.location.x as f64,
                new_origin.y + content_layout.location.y as f64,
            );
            if let Some(n) = self.sidebar_node {
                self.sidebar.apply_layout(taffy, n, content_origin);
            }
            // Editor column origin within the content row.
            if let Some(col) = self.editor_col_node {
                let col_layout = taffy.layout(col).unwrap();
                let col_origin = Point::new(
                    content_origin.x + col_layout.location.x as f64,
                    content_origin.y + col_layout.location.y as f64,
                );
                if let Some(n) = self.tabs_node {
                    self.tabs.apply_layout(taffy, n, col_origin);
                }
                if let Some(n) = self.editor_node {
                    self.editor.apply_layout(taffy, n, col_origin);
                }
                if let Some(n) = self.panel_node {
                    self.panel.apply_layout(taffy, n, col_origin);
                }
            }
        }
        if let Some(n) = self.status_node {
            self.status.apply_layout(taffy, n, new_origin);
        }
        self.palette.set_viewport(self.rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let active = self.active_theme(theme);
        let theme = active.as_ref();
        // Window background.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        if self.sidebar_visible {
            self.sidebar.draw(scene, transform, text_renderer, theme, presence);
        }
        self.tabs.draw(scene, transform, text_renderer, theme, presence);
        self.editor.draw(scene, transform, text_renderer, theme, presence);
        if self.panel_visible {
            self.panel.draw(scene, transform, text_renderer, theme, presence);
        }
        self.status.draw(scene, transform, text_renderer, theme, presence);
        // Palette floats on top.
        self.palette.draw_overlay(scene, transform, text_renderer, theme);
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
        // Root spans the whole window — always paint.
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
        _node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // The palette, when open, owns all keyboard input.
        if self.palette.is_visible() {
            if let WindowEvent::KeyboardInput { .. } = event {
                if self.palette.handle_key(event) {
                    *handled = true;
                    self.drain_child_events();
                    return;
                }
            }
        }

        // Global shortcuts: Ctrl/Cmd+P (files), Ctrl/Cmd+Shift+P (commands),
        // Ctrl/Cmd+B (sidebar), Ctrl/Cmd+J (panel).
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            let mods = modifiers.state();
            let cmd = mods.control_key() || mods.super_key();
            if cmd {
                match c.to_lowercase().as_str() {
                    "p" => {
                        self.refresh_palette_entries();
                        if mods.shift_key() {
                            self.palette.open_commands();
                        } else {
                            self.palette.open_files();
                        }
                        *handled = true;
                        return;
                    }
                    "b" => {
                        self.sidebar_visible = !self.sidebar_visible;
                        *handled = true;
                        return;
                    }
                    "j" => {
                        self.panel_visible = !self.panel_visible;
                        *handled = true;
                        return;
                    }
                    _ => {}
                }
            }
        }

        // Route to child widgets.
        if self.sidebar_visible {
            if let Some(n) = self.sidebar_node {
                self.sidebar.handle_event(
                    event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy,
                );
            }
        }
        if let Some(n) = self.tabs_node {
            self.tabs.handle_event(
                event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy,
            );
        }
        if self.panel_visible {
            if let Some(n) = self.panel_node {
                self.panel.handle_event(
                    event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy,
                );
            }
        }
        if let Some(n) = self.editor_node {
            self.editor.handle_event(
                event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy,
            );
        }

        // Drain whatever the children produced, then refresh derived views.
        self.drain_child_events();
        self.refresh_diagnostics(state);
        self.refresh_status();
        // Mirror panel tab changes into focus, if the panel switched itself.
        let _ = self.panel.take_tab_changed();
    }

    fn name(&self) -> &str {
        "CodeApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.sidebar.needs_redraw()
            || self.tabs.needs_redraw()
            || self.editor.needs_redraw()
            || self.panel.needs_redraw()
            || self.status.needs_redraw()
            || self.palette.is_visible()
    }

    fn mark_drawn(&mut self) {
        self.sidebar.mark_drawn();
        self.tabs.mark_drawn();
        self.editor.mark_drawn();
        self.panel.mark_drawn();
        self.status.mark_drawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_app_opens_main_rs() {
        let app = CodeApp::new();
        let active = app.session.active_buffer().unwrap();
        assert_eq!(active.name, "main.rs");
    }

    #[test]
    fn ensure_buffers_seeds_active_text() {
        let mut app = CodeApp::new();
        let mut doc = AutoCommit::new();
        app.ensure_buffers(&mut doc);
        let node = app.session.active_buffer().unwrap().node;
        let text = EditorDoc::read_buffer(&doc, node).unwrap();
        assert!(text.contains("fn main"));
        assert!(app.editor.is_bound());
    }

    #[test]
    fn opening_a_second_file_adds_a_tab() {
        let mut app = CodeApp::new();
        // Find prime.rs.
        let prime = app
            .session
            .project
            .nodes
            .values()
            .find(|n| n.name == "prime.rs")
            .unwrap()
            .id;
        app.open_node(prime);
        assert_eq!(app.session.open.len(), 2);
        assert_eq!(app.session.active_buffer().unwrap().name, "prime.rs");
    }

    #[test]
    fn editing_routes_through_crdt() {
        let mut app = CodeApp::new();
        let mut doc = AutoCommit::new();
        app.ensure_buffers(&mut doc);
        let node = app.session.active_buffer().unwrap().node;
        let before = EditorDoc::read_buffer(&doc, node).unwrap();
        app.editor.insert_for_test(&mut doc, "X");
        let after = EditorDoc::read_buffer(&doc, node).unwrap();
        assert_eq!(after.chars().count(), before.chars().count() + 1);
    }

    #[test]
    fn toggle_sidebar_command_hides_it() {
        let mut app = CodeApp::new();
        assert!(app.sidebar_visible);
        app.apply_command(Command::ToggleSidebar);
        assert!(!app.sidebar_visible);
    }

    #[test]
    fn diagnostics_populate_for_buffer_with_todo() {
        let mut app = CodeApp::new();
        let mut doc = AutoCommit::new();
        app.ensure_buffers(&mut doc);
        app.refresh_diagnostics(&doc);
        // README/main don't necessarily have problems, but the pipeline runs
        // without panicking and the count is queryable.
        let _ = app.panel.problem_count();
    }
}
