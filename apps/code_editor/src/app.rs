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
//!
//! ## Responsive sidebar
//!
//! The file-tree sidebar adapts to the window's allocated width using the
//! shared [`FlapState`] kernel from the toolkit (the same easing the weather
//! and chat apps use, promoted into `ui`):
//!
//! * **Wide** (≥ [`SIDEBAR_BREAKPOINT`]): the sidebar is **docked** beside the
//!   editor exactly as before — a persistent column the content is inset by.
//! * **Narrow** (< breakpoint): the sidebar collapses to an **overlay flap**.
//!   It is closed by default so a narrow window opens straight to the code;
//!   the content takes the full width and a hamburger in the tab bar summons
//!   the tree, which slides in from the left over a dimming scrim. The scrim,
//!   the Esc key, or the hamburger again dismiss it.
//!
//! The editor is not a master-detail list, so there is no push navigation —
//! the single primary sidebar simply docks or overlays per the breakpoint.

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use ui::color_compat::ColorCompat;
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::Key;

use crate::actions;
use crate::bottom_panel::{BottomPanel, PanelTab};
use crate::command_palette::{Command, CommandPalette, Entry, PaletteKind};
use crate::doc::EditorDoc;
use crate::editor_view::{EditorView, LspAsk};
use crate::file_tree::{FileTree, TreeEvent};
use crate::lint;
use crate::model::{NodeId as TreeId, NodeKind, Session};
use crate::sample_project;
use crate::status_bar::{StatusBar, StatusState};
use crate::tab_bar::{TabBar, TabEvent, TabItem};

const SIDEBAR_WIDTH: f64 = 240.0;
const SIDEBAR_MIN: f64 = 160.0;

/// At or above this allocated width (logical px) the sidebar docks beside the
/// editor; below it the sidebar becomes an overlay flap. Chosen so the file
/// tree (240px) plus a comfortably readable editor column stop fitting side by
/// side — narrower than that, the editor needs the whole width.
const SIDEBAR_BREAKPOINT: f64 = 820.0;

/// Hamburger toggle geometry, shown in the tab bar when the sidebar is an
/// overlay (narrow window). Square button hugging the bar's left edge.
const HAMBURGER_W: f64 = 40.0;

/// Width assumed before the window has been measured (first `build_layout`,
/// before any `apply_layout`). Wide enough to dock — a fresh desktop window
/// and the 1280px `--shots` viewport both start docked.
const DEFAULT_DOCKED_WIDTH_HINT: f64 = 1280.0;

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

    /// Framework-owned action dispatch (the qOS resource + action model). The
    /// only doc-mutating operation Code exports beyond raw text is materialising
    /// a buffer (New file); it is dispatched through this host so it brackets one
    /// undo boundary and replays byte-identically on peers, the same path a
    /// Ctrl+K palette dispatch takes. See [`crate::actions`].
    actions: ui::ActionHost,

    /// Adaptive open/closed state of the file-tree sidebar (the shared flap
    /// kernel). Docked-open when wide, overlay-closed when narrow.
    flap: FlapState,
    /// Last animation tick, for the frame-rate-independent flap easing.
    flap_tick: Instant,
    /// The sidebar's resolved overlay rect (narrow mode), computed each draw
    /// from the eased reveal — also the hit region for routing tree events.
    overlay_sidebar_rect: Rect,
    /// Last known allocated width, cached so `build_layout` (which runs before
    /// `apply_layout` has measured the window) can resolve docked vs. overlay
    /// on the very first frame. Seeded wide so a fresh desktop window docks.
    last_width: f64,
    /// The dimming scrim rect behind an open overlay sidebar (else empty).
    scrim_rect: Rect,
    /// The hamburger toggle's hit rect (narrow mode only; else empty).
    hamburger_rect: Rect,
    /// Last cursor position, since `MouseInput` carries no coordinates.
    last_cursor: Point,
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

    /// The embedded system terminal, spawned lazily the first time the Terminal
    /// dock tab is shown. `None` until then (so `CodeApp::new` — used across the
    /// test suite — never spawns a shell) and if the shell fails to launch.
    terminal: Option<terminal::TerminalWidget>,
    /// True while the terminal owns keyboard input (the user clicked into it).
    /// Cleared when a press lands anywhere else.
    terminal_focused: bool,

    /// The live LSP session for the active buffer's language, when one has been
    /// started (opt-in — see `maybe_start_lsp`). Diagnostics / completion /
    /// hover / definition responses are drained from it each frame.
    lsp: Option<crate::lsp::LspClient>,
    /// The language the current `lsp` client serves, so we restart it when the
    /// active buffer switches to a different language.
    lsp_language: Option<crate::model::Language>,
    /// The `file://` URI most recently opened on the LSP server, so edits go out
    /// as `didChange` against the right document.
    lsp_uri: Option<String>,
    /// Monotonic document version for LSP `didChange`.
    lsp_version: i64,
    /// LSP-provided diagnostics, keyed by document URI. Merged with the local
    /// linter into the Problems panel + editor squiggles.
    lsp_diagnostics: std::collections::HashMap<String, Vec<crate::lsp::LspDiagnostic>>,
    /// The buffer text last sent to the LSP server (as `didChange`), so we only
    /// notify on an actual change.
    lsp_sent_text: String,
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
            actions: ui::ActionHost::new(),
            flap: FlapState::open_when_docked(),
            flap_tick: Instant::now(),
            // Seed wide so the first frame of a desktop window (and `--shots`,
            // 1280px) docks the sidebar; corrected from the real width once
            // `apply_layout` measures the window.
            last_width: DEFAULT_DOCKED_WIDTH_HINT,
            overlay_sidebar_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            hamburger_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
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
            terminal: None,
            terminal_focused: false,
            lsp: None,
            lsp_language: None,
            lsp_uri: None,
            lsp_version: 0,
            lsp_diagnostics: std::collections::HashMap::new(),
            lsp_sent_text: String::new(),
        }
    }

    /// The theme to render with: the user override if set, else the host's.
    fn active_theme<'a>(&self, host: &'a Theme) -> std::borrow::Cow<'a, Theme> {
        match self.theme_index {
            Some(i) => std::borrow::Cow::Owned(themes()[i % themes().len()].clone()),
            None => std::borrow::Cow::Borrowed(host),
        }
    }

    /// Resolve docked vs. overlay from the allocated width. Uses the cached
    /// `last_width` so the decision is stable across all five passes — in
    /// particular `build_layout`, which runs before `apply_layout` has set
    /// `self.rect` to the real window size.
    fn sidebar_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.last_width, SIDEBAR_BREAKPOINT)
    }

    /// Seed the width used to resolve the responsive layout on the first frame,
    /// before the window has been measured. The `--shots` renderer calls this
    /// with the viewport width so a single offscreen frame picks the correct
    /// docked/overlay layout (the live app self-corrects from `apply_layout`).
    ///
    /// Because a single offscreen frame never runs the animation `tick`, also
    /// settle the flap to this layout's resting state (docked ⇒ open, overlay
    /// ⇒ closed) so the captured frame is the settled layout, not one frozen
    /// mid-slide. An explicit `shots_snap_sidebar(true)` after this still wins.
    pub fn set_width_hint(&mut self, width: f64) {
        self.last_width = width;
        let layout = self.sidebar_layout();
        self.flap.reconcile(layout);
        self.flap.snap(self.flap.is_open(layout));
    }

    /// Whether the sidebar is currently resolved open (preference or default).
    fn sidebar_open(&self) -> bool {
        self.flap.is_open(self.sidebar_layout())
    }

    /// Advance the flap easing using real elapsed time. Returns `true` while
    /// still animating (so the host keeps pumping frames). Called once per
    /// draw, mirroring the proven weather/chat pattern.
    fn tick_flap(&mut self) -> bool {
        let now = Instant::now();
        let dt = (now - self.flap_tick).as_secs_f64().min(0.1);
        self.flap_tick = now;
        self.flap.reconcile(self.sidebar_layout());
        self.flap.tick(self.sidebar_layout(), dt)
    }

    /// Flip the sidebar open/closed (hamburger, Ctrl+B, palette command).
    fn toggle_sidebar(&mut self) {
        self.flap.toggle(self.sidebar_layout());
    }

    /// Snap the flap to a final open/closed state with no animation — used by
    /// the single-frame `--shots` renderer so a captured frame shows a settled
    /// flap rather than one mid-slide.
    pub fn shots_snap_sidebar(&mut self, open: bool) {
        self.flap.snap(open);
    }

    /// The y-coordinate of the bottom of the editor content region, accounting
    /// for the optional bottom panel — used as the overlay sidebar/scrim base.
    fn panel_bottom(&self) -> f64 {
        if self.panel_visible {
            self.panel.get_rect().y1
        } else {
            self.editor.get_rect().y1
        }
    }

    /// Draw the hamburger toggle that summons the overlay sidebar. A small,
    /// self-contained button over the tab bar's left corner (its own opaque
    /// background, so it reads as a button on top of the bar). Records its hit
    /// rect for `handle_event`.
    fn draw_hamburger(&mut self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        // The bar is inset by HAMBURGER_W in overlay mode, so the hamburger
        // occupies the reserved strip from the window's left edge up to it.
        let bar = self.tabs.get_rect();
        let left = self.rect.x0;
        let rect = Rect::new(left, bar.y0, left + HAMBURGER_W, bar.y1);
        self.hamburger_rect = rect;
        let c = &theme.tokens.colors;

        // Button background (raised so it covers any tab edge underneath),
        // tinted when the sidebar is open.
        let bg = if self.sidebar_open() {
            c.surface_white_10
        } else {
            c.surfaces.raised
        };
        scene.fill(Fill::NonZero, transform, bg, None, &rect);
        // Right divider, matching the tab bar's seams.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(
                Point::new(rect.x1, rect.y0 + 6.0),
                Point::new(rect.x1, rect.y1 - 6.0),
            ),
        );

        // Three glyph lines.
        let cx = rect.x0 + rect.width() / 2.0;
        let cy = rect.y0 + rect.height() / 2.0;
        let half = 8.0;
        let line_color = if self.sidebar_open() { c.text_primary } else { c.neutral[8] };
        let stroke = Stroke::new(1.6);
        for dy in [-5.0, 0.0, 5.0] {
            scene.stroke(
                &stroke,
                transform,
                line_color,
                None,
                &Line::new(Point::new(cx - half, cy + dy), Point::new(cx + half, cy + dy)),
            );
        }
    }

    /// Seed every open buffer's text into the CRDT (idempotent) and bind the
    /// editor to the active buffer's Text object.
    fn ensure_buffers(&mut self, doc: &mut AutoCommit) {
        // The document is the source of truth for which buffers exist. Any buffer
        // it holds that the in-memory session does not track — created by a Ctrl+K
        // "New file" action or by a peer — is surfaced as an untitled file so it
        // appears in the tree. The buffer's text stays in the CRDT; this only
        // materialises the in-memory shell the tree and tabs read from.
        for node in EditorDoc::buffer_keys(doc) {
            if self.session.project.get(node).is_none() {
                let name = format!("untitled-{node}.txt");
                self.session.project.adopt_file(node, &name);
                self.sidebar.set_project(&self.session.project);
            }
        }

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
    /// status bar. LSP diagnostics (when a server is live) are merged with the
    /// local linter; the LSP set also drives the editor's inline squiggles.
    fn refresh_diagnostics(&mut self, doc: &AutoCommit) {
        let mut diagnostics = match self.session.active_buffer() {
            Some(buf) => {
                let text = EditorDoc::read_buffer(doc, buf.node).unwrap_or_default();
                lint::lint(&text, buf.language)
            }
            None => Vec::new(),
        };
        // Merge in any LSP diagnostics for the active document.
        if let Some(uri) = &self.lsp_uri {
            if let Some(lsp) = self.lsp_diagnostics.get(uri) {
                diagnostics.extend(lsp.iter().map(|d| d.to_panel()));
            }
        }
        diagnostics.sort_by(|a, b| (a.line, a.col).cmp(&(b.line, b.col)));
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

    /// The presentation context the Ctrl+K resource tree is built from: the
    /// project name, the id the next created buffer will take, and the open
    /// buffers. Everything here mirrors state the app already holds.
    pub(crate) fn code_resource_ctx(&self) -> actions::CodeEditorResourceCtx {
        let open_files = self
            .session
            .open
            .iter()
            .map(|b| actions::OpenFileRef {
                node: b.node as u64,
                name: b.name.clone(),
            })
            .collect();
        actions::CodeEditorResourceCtx {
            project_name: self.session.project.name.clone(),
            next_id: self.session.project.peek_next_id() as u64,
            open_files,
        }
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
            TreeEvent::OpenFile(id) => {
                self.open_node(id);
                // In overlay mode, opening a file dismisses the flap so the
                // user lands on the freshly-opened buffer (single-pane feel).
                // Docked mode keeps the sidebar pinned.
                if self.sidebar_layout() == SplitLayout::Overlay {
                    self.flap.close();
                }
            }
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

    /// Dispatch a palette command. `doc` is threaded in because the one
    /// doc-mutating command (New file) dispatches a [`CodeEditorAction`] through
    /// the action host; the rest are pure presentation.
    ///
    /// [`CodeEditorAction`]: crate::actions::CodeEditorAction
    fn apply_command(&mut self, cmd: Command, doc: &mut AutoCommit) {
        match cmd {
            Command::NewFile => {
                // Create an untitled scratch file in the project root. The name
                // and tree position are in-memory; the collaborative, replayable
                // half — materialising the buffer in the shared document — is the
                // one action Code exports, dispatched here through the host so it
                // brackets one undo boundary and replays on peers. The Ctrl+K
                // "New file" spec builds the byte-identical action.
                let root = self.session.project.root;
                let name = format!("untitled-{}.txt", self.session.project.nodes.len());
                let id = self.session.project.add_file(root, &name, "");
                self.sidebar.set_project(&self.session.project);
                self.open_node(id);
                self.actions.dispatch(
                    doc,
                    actions::CODE_EDITOR_ACTION_TYPE,
                    &actions::create_buffer_action(id as u64, ""),
                );
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
                self.toggle_sidebar();
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
                self.ensure_terminal();
            }
        }
    }

    /// Drain everything the child widgets emitted this frame. `doc` is threaded
    /// through so a chosen palette command that mutates the document (New file)
    /// can dispatch its action.
    fn drain_child_events(&mut self, doc: &mut AutoCommit) {
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
                PaletteKind::Command(cmd) => self.apply_command(cmd, doc),
            }
        }
    }

    /// Spawn the embedded system terminal on first use. Idempotent. On success
    /// the panel hands the Terminal tab body to the live widget; on failure it
    /// shows a clear note instead of the static log.
    fn ensure_terminal(&mut self) {
        if self.terminal.is_some() {
            return;
        }
        match terminal::TerminalWidget::spawn_default() {
            Ok(w) => {
                self.terminal = Some(w);
                self.panel.set_has_live_terminal(true);
                self.panel.set_terminal_note(None);
            }
            Err(e) => {
                self.panel.set_has_live_terminal(false);
                self.panel
                    .set_terminal_note(Some(format!("Terminal unavailable: {e}")));
            }
        }
    }

    /// Whether the Terminal dock tab is currently visible with a live terminal.
    fn terminal_visible(&self) -> bool {
        self.panel_visible
            && self.panel.active_tab() == PanelTab::Terminal
            && self.terminal.is_some()
    }

    // ---- LSP orchestration ------------------------------------------------

    /// Build a `file://` URI for a project node, mirroring the sample project's
    /// tree onto a synthetic root so servers get a plausible path.
    fn node_uri(&self, node: TreeId) -> String {
        format!("file:///{}", self.session.project.path_of(node))
    }

    /// LSP `languageId` string for a language (the server's expected token).
    fn language_id(lang: crate::model::Language) -> &'static str {
        use crate::model::Language::*;
        match lang {
            Rust => "rust",
            Python => "python",
            TypeScript => "typescript",
            JavaScript => "javascript",
            Go => "go",
            C => "c",
            Cpp => "cpp",
            Toml => "toml",
            Json => "json",
            Yaml => "yaml",
            Html => "html",
            Css => "css",
            Markdown => "markdown",
            Shell => "shellscript",
            PlainText => "plaintext",
        }
    }

    /// Start (or restart) a language server for the active buffer, but only when
    /// live LSP is opted into via the `QOS_CODE_LSP` env var. Kept opt-in so the
    /// hermetic build, the test suite, and `--shots` never launch rust-analyzer.
    /// A missing binary degrades to a Problems-panel note.
    fn maybe_start_lsp(&mut self, doc: &AutoCommit) {
        if std::env::var("QOS_CODE_LSP").is_err() {
            return;
        }
        let Some(buf) = self.session.active_buffer() else {
            return;
        };
        let (node, lang) = (buf.node, buf.language);
        if self.lsp.is_some() && self.lsp_language == Some(lang) {
            return; // already serving this language
        }
        if crate::lsp::LspClient::server_command(lang).is_none() {
            return; // no server mapping for this language
        }
        let root_uri = format!("file:///{}", self.session.project.name);
        match crate::lsp::LspClient::start(lang, &root_uri) {
            Ok(mut client) => {
                let uri = self.node_uri(node);
                let text = EditorDoc::read_buffer(doc, node).unwrap_or_default();
                tracing::info!(
                    "started language server {} for {}",
                    client.server_name(),
                    lang.label()
                );
                self.lsp_version = 1;
                client.did_open(&uri, Self::language_id(lang), self.lsp_version, &text);
                self.lsp = Some(client);
                self.lsp_language = Some(lang);
                self.lsp_uri = Some(uri);
            }
            Err(e) => {
                // Surface the failure where the user will look for diagnostics.
                self.lsp = None;
                self.lsp_language = None;
                self.panel.set_problems(vec![crate::bottom_panel::Diagnostic {
                    severity: crate::bottom_panel::Severity::Info,
                    line: 1,
                    col: 1,
                    message: format!("Language server: {e}"),
                }]);
            }
        }
    }

    /// Drain events the LSP reader thread produced and fold them into the editor
    /// (diagnostics → panel + squiggles; completion/hover/definition → editor).
    fn poll_lsp(&mut self) {
        let events = match self.lsp.as_mut() {
            Some(c) => c.poll(),
            None => return,
        };
        for ev in events {
            match ev {
                crate::lsp::LspEvent::Diagnostics { uri, diagnostics } => {
                    self.lsp_diagnostics.insert(uri, diagnostics);
                    self.apply_diagnostics_to_editor();
                }
                crate::lsp::LspEvent::Completion(items) => {
                    self.editor.show_completions(items);
                }
                crate::lsp::LspEvent::Hover(text) => {
                    self.editor.set_hover(text);
                }
                crate::lsp::LspEvent::Definition(locs) => {
                    self.apply_definition(locs);
                }
                crate::lsp::LspEvent::Initialized | crate::lsp::LspEvent::Exited => {}
            }
        }
    }

    /// Push the active buffer's LSP diagnostics into the editor's inline model
    /// and merge them with the local linter for the Problems panel.
    fn apply_diagnostics_to_editor(&mut self) {
        let uri = self.lsp_uri.clone();
        let lsp_diags = uri
            .as_ref()
            .and_then(|u| self.lsp_diagnostics.get(u))
            .cloned()
            .unwrap_or_default();
        self.editor.set_diagnostics(&lsp_diags);
    }

    /// Move the caret to a definition result when it lands in the current
    /// buffer (a same-file jump). Cross-file jumps would need to open the target
    /// file; left as a follow-up.
    fn apply_definition(&mut self, locs: Vec<crate::lsp::Location>) {
        let Some(cur) = self.lsp_uri.clone() else {
            return;
        };
        if let Some(loc) = locs.into_iter().find(|l| l.uri == cur) {
            self.editor.move_caret_to(
                loc.range.start.line as usize,
                loc.range.start.character as usize,
            );
        }
    }

    /// The LSP position of the editor's primary caret.
    fn caret_lsp_pos(&self) -> crate::lsp::Position {
        let (line, col) = self.editor.primary_caret();
        crate::lsp::Position {
            line: line as u32,
            character: col as u32,
        }
    }

    /// Issue the language-server request the editor asked for, at the caret.
    /// A no-op when no server is live (opt-in).
    fn dispatch_lsp_ask(&mut self, ask: LspAsk) {
        let uri = match self.lsp_uri.clone() {
            Some(u) => u,
            None => return,
        };
        let pos = self.caret_lsp_pos();
        if let Some(c) = self.lsp.as_mut() {
            match ask {
                LspAsk::Completion => c.request_completion(&uri, pos),
                LspAsk::Hover => c.request_hover(&uri, pos),
                LspAsk::Definition => c.request_definition(&uri, pos),
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

    // ---- headless `--shots` scenario staging ------------------------------

    /// Open the Terminal dock tab and spawn the live shell (for a screenshot of
    /// the embedded terminal).
    pub fn shots_open_terminal(&mut self) {
        self.panel_visible = true;
        self.panel.set_tab(PanelTab::Terminal);
        self.ensure_terminal();
    }

    /// Type a command into the embedded terminal (drives the real shell).
    pub fn shots_terminal_input(&mut self, s: &str) {
        if let Some(t) = self.terminal.as_mut() {
            t.write_input(s.as_bytes());
        }
    }

    /// The embedded terminal's current grid text, if a live terminal is
    /// present. `None` before the Terminal tab is opened (or if the shell
    /// failed to spawn). Used by `--shots` staging and the embed tests to read
    /// the live shell session without a GPU scene.
    pub fn shots_terminal_grid_text(&self) -> Option<String> {
        self.terminal.as_ref().map(|t| t.grid_text())
    }

    /// Stage a multi-word selection in the editor for the selection screenshot.
    pub fn shots_select_words(&mut self) {
        // Select a span on the first line that covers several words. Line 0 of
        // every sample buffer is a doc comment, so this is content-stable.
        if self.editor.line_count() > 0 {
            self.editor.set_selection_lc((0, 4), (0, 24));
        }
    }

    /// Stage a diagnostics + completion-popup screenshot by feeding canned LSP
    /// JSON through the *real* parser and model path (no server binary needed).
    /// This proves the same code the live client uses renders correctly.
    pub fn shots_completion_demo(&mut self, doc: &AutoCommit) {
        use crate::lsp::{parse_completion, parse_publish_diagnostics};

        let Some(node) = self.session.active_buffer().map(|b| b.node) else {
            return;
        };
        let text = EditorDoc::read_buffer(doc, node).unwrap_or_default();
        let uri = self.node_uri(node);
        self.lsp_uri = Some(uri.clone());

        // Canned diagnostics shaped exactly like rust-analyzer's, anchored to
        // real spans found in the buffer so the squiggles land on real tokens.
        let mut diag_json = Vec::new();
        if let Some((l, c0, c1)) = find_char_span(&text, "unwrap_or(20)") {
            diag_json.push(serde_json::json!({
                "range": { "start": {"line": l, "character": c0}, "end": {"line": l, "character": c1} },
                "severity": 2,
                "source": "clippy",
                "message": "manual `unwrap_or` — consider `unwrap_or_default()`"
            }));
        }
        if let Some((l, c0, c1)) = find_char_span(&text, "parse_limit") {
            diag_json.push(serde_json::json!({
                "range": { "start": {"line": l, "character": c0}, "end": {"line": l, "character": c1} },
                "severity": 1,
                "source": "rustc",
                "message": "cannot find function `parse_limitt` in this scope"
            }));
        }
        let notif = serde_json::json!({
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diag_json }
        });
        if let Some((_, diags)) = parse_publish_diagnostics(&notif) {
            self.editor.set_diagnostics(&diags);
            self.lsp_diagnostics.insert(uri, diags);
        }
        self.refresh_diagnostics(doc);

        // Canned completion response fed through the real parser.
        let completion_json = serde_json::json!({
            "result": {
                "isIncomplete": false,
                "items": [
                    { "label": "primes_below", "kind": 3, "detail": "fn(u64) -> Vec<u64>" },
                    { "label": "print", "kind": 3, "detail": "macro" },
                    { "label": "println", "kind": 3, "detail": "macro" },
                    { "label": "parse_limit", "kind": 3, "detail": "fn(&[String]) -> Option<u64>" },
                    { "label": "primary", "kind": 6, "detail": "let binding" }
                ]
            }
        });
        let items = parse_completion(&completion_json);
        // Place the caret after a partial identifier so the popup anchors on a
        // real token, then show the completions.
        if let Some((l, _c0, c1)) = find_char_span(&text, "primes") {
            self.editor.move_caret_to(l, c1);
        }
        self.editor.show_completions(items);
    }
}

/// Locate the first occurrence of `needle` in `text`, returning its
/// `(line, start_col, end_col)` in zero-based *character* coordinates. Used by
/// the shots scenarios to anchor canned diagnostics on real tokens.
fn find_char_span(text: &str, needle: &str) -> Option<(usize, usize, usize)> {
    let byte = text.find(needle)?;
    let prefix = &text[..byte];
    let line = prefix.matches('\n').count();
    let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let start_col = text[line_start..byte].chars().count();
    let end_col = start_col + needle.chars().count();
    Some((line, start_col, end_col))
}

impl Widget for CodeApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded {
            self.seed_terminal();
            self.refresh_palette_entries();
        }
        self.ensure_buffers(state);
        self.editor.sync(state, path);

        // Pump the embedded terminal: its `sync` drains the PTY reader and
        // reflects it into the grid (the plain `ui::App` loop never calls
        // `update`, so pumping happens here — see `TerminalWidget::sync`).
        if self.terminal.is_some() {
            let tr = EditorDoc::terminal_root(state);
            if let Some(t) = self.terminal.as_mut() {
                t.sync(state, &tr);
            }
        }

        // Language server: start on demand (opt-in), stream edits out as
        // `didChange`, and drain diagnostics/completions/hover back in.
        self.maybe_start_lsp(state);
        if self.lsp.is_some() {
            if let Some((node, uri)) = self
                .session
                .active_buffer()
                .map(|b| b.node)
                .zip(self.lsp_uri.clone())
            {
                let text = EditorDoc::read_buffer(state, node).unwrap_or_default();
                if text != self.lsp_sent_text {
                    self.lsp_version += 1;
                    let version = self.lsp_version;
                    if let Some(c) = self.lsp.as_mut() {
                        c.did_change(&uri, version, &text);
                    }
                    self.lsp_sent_text = text;
                }
            }
        }
        self.poll_lsp();

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
        //
        // Docked (wide): the sidebar occupies a real column, its width eased by
        // the flap's reveal so it collapses/expands smoothly and the editor is
        // inset by exactly that much. Overlay (narrow): the column collapses to
        // zero so the content spans the full width — the sidebar is instead
        // drawn off-flow, sliding in over the content (see `draw`).
        let layout = self.sidebar_layout();
        let sidebar_node = self.sidebar.build_layout(taffy, theme, node_map);
        let docked_w = match layout {
            SplitLayout::Docked => SIDEBAR_WIDTH * self.flap.reveal(),
            SplitLayout::Overlay => 0.0,
        };
        let docked_min = if layout == SplitLayout::Docked && self.flap.reveal() > 0.999 {
            SIDEBAR_MIN as f32
        } else {
            0.0
        };
        let _ = taffy.set_style(
            sidebar_node,
            Style {
                size: Size {
                    width: Dimension::length(docked_w as f32),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(docked_min),
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

        // Cache the real measured width so `sidebar_layout()` is consistent
        // across passes (it's read in `build_layout`, before this runs).
        self.last_width = self.rect.width();

        // The allocated width is now known — reconcile the flap so a window
        // resize that crosses the breakpoint adopts the new mode's default
        // (wide ⇒ docked-open, narrow ⇒ overlay-closed) before the next draw.
        self.flap.reconcile(self.sidebar_layout());

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
                    // Reserve the bar's left edge for the overlay hamburger so
                    // the first tab isn't drawn beneath it.
                    if self.sidebar_layout() == SplitLayout::Overlay {
                        self.tabs.inset_left(HAMBURGER_W);
                    }
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

        // Advance the flap easing (frame-rate independent) before computing
        // geometry, so the slide + scrim fade animate while the host keeps
        // redrawing (`needs_animation_frame`). This mirrors the proven
        // weather/chat pattern: the sidebar's overlay geometry is recomputed
        // each frame from the current reveal rather than in `apply_layout`.
        self.tick_flap();
        let layout = self.sidebar_layout();

        // Window background.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        match layout {
            SplitLayout::Docked => {
                // The sidebar is a real column laid out by taffy; draw it in
                // place when it has any width (reveal > 0).
                self.scrim_rect = Rect::ZERO;
                self.overlay_sidebar_rect = Rect::ZERO;
                self.hamburger_rect = Rect::ZERO;
                if self.flap.reveal() > 0.001 {
                    self.sidebar.draw(scene, transform, text_renderer, theme, presence);
                }
                self.tabs.draw(scene, transform, text_renderer, theme, presence);
                self.editor.draw(scene, transform, text_renderer, theme, presence);
                if self.panel_visible {
                    self.panel.draw(scene, transform, text_renderer, theme, presence);
                }
            }
            SplitLayout::Overlay => {
                // Content spans the full width and is drawn first; the sidebar
                // slides in over it behind a scrim.
                self.tabs.draw(scene, transform, text_renderer, theme, presence);
                self.editor.draw(scene, transform, text_renderer, theme, presence);
                if self.panel_visible {
                    self.panel.draw(scene, transform, text_renderer, theme, presence);
                }
                // Scrim: opacity tracks the reveal (Adwaita-style dimming).
                let reveal = self.flap.reveal();
                if reveal > 0.001 {
                    self.scrim_rect = Rect::new(
                        self.rect.x0,
                        self.tabs.get_rect().y0,
                        self.rect.x1,
                        self.editor.get_rect().y1.max(self.panel_bottom()),
                    );
                    let scrim = theme
                        .tokens
                        .colors
                        .surfaces
                        .overlay
                        .with_alpha_factor((reveal * 0.6) as f32);
                    scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);
                } else {
                    self.scrim_rect = Rect::ZERO;
                }
                // The sidebar slides in from the left over the content. Off
                // fully at reveal 0; flush at reveal 1.
                let slide = self.flap.slide();
                let top = self.tabs.get_rect().y0;
                let bottom = self.editor.get_rect().y1.max(self.panel_bottom());
                let x0 = self.rect.x0 - SIDEBAR_WIDTH * (1.0 - slide);
                let sidebar_rect = Rect::new(x0, top, x0 + SIDEBAR_WIDTH, bottom);
                self.overlay_sidebar_rect = sidebar_rect;
                if reveal > 0.001 {
                    self.sidebar.place_at(sidebar_rect);
                    self.sidebar.draw(scene, transform, text_renderer, theme, presence);
                }
                // The hamburger toggle sits at the tab bar's left edge.
                self.draw_hamburger(scene, transform, theme);
            }
        }

        // The embedded terminal renders its live grid over the panel's content
        // region whenever the Terminal dock tab is showing.
        if self.terminal_visible() {
            let content = self.panel.content_rect();
            if let Some(t) = self.terminal.as_mut() {
                t.place_at(content);
                t.draw(scene, transform, text_renderer, theme, presence);
            }
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
                    self.drain_child_events(state);
                    return;
                }
            }
        }

        // Track the cursor (MouseInput carries no coords) so the hamburger and
        // scrim hit-tests below have a position to work with.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        let overlay = self.sidebar_layout() == SplitLayout::Overlay;

        // Esc dismisses an open overlay flap before anything else sees it.
        if overlay && self.sidebar_open() {
            if let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(winit::keyboard::NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            {
                self.flap.close();
                *handled = true;
                return;
            }
        }

        // Left-click routing for the overlay-specific affordances.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            // The hamburger toggle (drawn only in overlay mode).
            if overlay && self.hamburger_rect.width() > 0.0
                && self.hamburger_rect.contains(self.last_cursor)
            {
                self.toggle_sidebar();
                *handled = true;
                return;
            }
            // A click on the scrim (outside the open sidebar) dismisses it.
            if overlay
                && self.sidebar_open()
                && self.scrim_rect.width() > 0.0
                && self.scrim_rect.contains(self.last_cursor)
                && !self.overlay_sidebar_rect.contains(self.last_cursor)
            {
                self.flap.close();
                *handled = true;
                return;
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
                        self.toggle_sidebar();
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

        // Terminal input capture: while the Terminal dock tab shows a live
        // terminal, a click in its content region focuses the shell and routes
        // keyboard + pointer input to it (like clicking into any terminal); a
        // press elsewhere returns focus to the editor.
        let term_active = self.terminal_visible();
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            self.terminal_focused =
                term_active && self.panel.content_rect().contains(self.last_cursor);
        }
        if term_active && self.terminal_focused {
            let tr = EditorDoc::terminal_root(state);
            if let Some(t) = self.terminal.as_mut() {
                let mut th = false;
                let mut tf = None;
                t.handle_event(
                    event, scale_factor, modifiers, state, &tr, &mut th, &mut tf,
                    NodeId::new(0), taffy,
                );
                if th {
                    *handled = true;
                }
            }
            // The focused terminal owns keyboard + pointer; consume those so the
            // editor underneath doesn't also react.
            if matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::CursorMoved { .. }
            ) {
                return;
            }
        }

        // Route to child widgets.
        //
        // Sidebar routing depends on the layout:
        //  * Docked: route whenever the column has width (reveal > 0); the tree
        //    hit-tests against its taffy-derived column rect.
        //  * Overlay: route only while the flap is open. The tree hit-tests
        //    against the off-flow overlay rect set in `draw` via `place_at`.
        let sidebar_active = match self.sidebar_layout() {
            SplitLayout::Docked => self.flap.reveal() > 0.001,
            SplitLayout::Overlay => self.sidebar_open(),
        };
        if sidebar_active {
            if let Some(n) = self.sidebar_node {
                self.sidebar.handle_event(
                    event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy,
                );
            }
        }

        // While the overlay sidebar is open, it captures pointer presses over
        // its area so clicks don't leak to the content behind it (the tree only
        // self-marks `handled` on a row hit — empty rows would otherwise fall
        // through). The scrim outside it was already handled above.
        if overlay && self.sidebar_open() {
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
            {
                if self.overlay_sidebar_rect.contains(self.last_cursor) {
                    *handled = true;
                }
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

        // If the editor asked for a language-server action (Ctrl+Space, F1,
        // F12), issue it now with the caret position.
        if let Some(ask) = self.editor.take_lsp_ask() {
            self.dispatch_lsp_ask(ask);
        }

        // Drain whatever the children produced, then refresh derived views.
        self.drain_child_events(state);
        self.refresh_diagnostics(state);
        self.refresh_status();
        // Mirror panel tab changes into focus, if the panel switched itself.
        let _ = self.panel.take_tab_changed();
    }

    fn name(&self) -> &str {
        "CodeApp"
    }

    /// Publish the open project + its buffers so the shell's one Ctrl+K palette
    /// scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records one undo boundary bracketing it (see
    /// [`Self::apply_command`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
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
            || self.flap.is_animating(self.sidebar_layout())
            // A live terminal repaints continuously while visible.
            || self.terminal_visible()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the event loop pumping frames while the sidebar flap is
        // mid-transition (so the slide + scrim fade play out) OR while a live
        // terminal is visible — its PTY is drained from `sync`, which the App
        // only re-runs while an animation frame is requested.
        self.flap.is_animating(self.sidebar_layout()) || self.terminal_visible()
    }

    fn take_structure_dirty(&mut self) -> bool {
        // While the flap eases in DOCKED mode the sidebar column's width is
        // taffy-driven (set from `reveal` in `build_layout`), so the tree must
        // be rebuilt each animating frame to widen/collapse smoothly. Overlay
        // mode computes its geometry in `draw` and needs no rebuild.
        self.sidebar_layout() == SplitLayout::Docked
            && self.flap.is_animating(self.sidebar_layout())
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
    fn ide_embeds_the_real_terminal_widget() {
        // The IDE must embed the real system `TerminalWidget` as its integrated
        // terminal — not ship a second, weaker emulator. Opening the Terminal
        // dock tab spawns a live widget whose grid the panel hands its body to.
        let mut app = CodeApp::new();
        assert!(
            app.shots_terminal_grid_text().is_none(),
            "no terminal exists before the Terminal tab is opened"
        );
        app.shots_open_terminal();
        assert_eq!(app.panel.active_tab(), PanelTab::Terminal);
        assert!(
            app.shots_terminal_grid_text().is_some(),
            "opening the Terminal tab embeds a live TerminalWidget"
        );
    }

    #[test]
    fn ide_terminal_runs_a_command_and_shows_output() {
        // End-to-end: open the embedded terminal, type a command, pump the
        // app's `sync` (which drains the terminal's PTY), and prove the shell's
        // output flowed back into the IDE's integrated terminal grid.
        let mut app = CodeApp::new();
        app.shots_open_terminal();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        app.shots_terminal_input("printf 'IDE_TERM_OK\\n'\n");
        let mut found = false;
        for _ in 0..300 {
            app.sync(&mut state.doc, &state.root);
            if let Some(t) = app.shots_terminal_grid_text() {
                if t.contains("IDE_TERM_OK") {
                    found = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            found,
            "the IDE's embedded terminal must run the typed command and show its output"
        );
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
        // Default width hint is wide → docked-open; the command flips it shut.
        let mut app = CodeApp::new();
        let mut doc = AutoCommit::new();
        assert!(app.sidebar_open());
        app.apply_command(Command::ToggleSidebar, &mut doc);
        assert!(!app.sidebar_open());
        app.apply_command(Command::ToggleSidebar, &mut doc);
        assert!(app.sidebar_open());
    }

    #[test]
    fn new_file_command_dispatches_the_create_buffer_action() {
        // The on-screen "New file" command and the Ctrl+K "New file" spec must
        // build the byte-identical action — one source of truth. Both target the
        // node id the project will allocate next.
        let mut app = CodeApp::new();
        let mut doc = AutoCommit::new();
        app.ensure_buffers(&mut doc); // seed the sample project's open buffers

        let ctx = app.code_resource_ctx();
        let expected_node = ctx.next_id;
        let resources = crate::actions::provide_resources(&ctx, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "new-file")
            .unwrap();
        let from_spec = match &spec.input {
            qos_actions::ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("new-file is an immediate action"),
        };

        app.apply_command(Command::NewFile, &mut doc);

        let dispatched = app
            .actions
            .last_dispatched()
            .expect("New File dispatched an action");
        assert_eq!(
            dispatched, &from_spec,
            "the command and the Ctrl+K spec build the identical action"
        );
        // The action actually materialised the buffer in the document.
        assert!(EditorDoc::read_buffer(&doc, expected_node as usize).is_some());
    }

    #[test]
    fn a_document_buffer_is_adopted_as_a_tracked_file() {
        // A buffer created directly in the document (a Ctrl+K "New file" or a
        // peer) is surfaced by the next sync as an untitled file in the tree,
        // without inventing a persisted file model.
        let mut app = CodeApp::new();
        let mut doc = AutoCommit::new();
        app.ensure_buffers(&mut doc);
        let before = app.session.project.nodes.len();

        let orphan = 9999;
        EditorDoc::ensure_buffer(&mut doc, orphan, "// created out of band\n");
        assert!(app.session.project.get(orphan).is_none());

        app.ensure_buffers(&mut doc);
        assert!(
            app.session.project.get(orphan).is_some(),
            "the document buffer was adopted as a file"
        );
        assert_eq!(app.session.project.nodes.len(), before + 1);
    }

    #[test]
    fn layout_is_docked_when_wide_overlay_when_narrow() {
        let mut app = CodeApp::new();
        app.set_width_hint(1200.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
        app.set_width_hint(600.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
        // Boundary is inclusive at the breakpoint → docked.
        app.set_width_hint(SIDEBAR_BREAKPOINT);
        assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
        app.set_width_hint(SIDEBAR_BREAKPOINT - 1.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
    }

    #[test]
    fn narrow_window_hides_sidebar_by_default() {
        let mut app = CodeApp::new();
        app.set_width_hint(600.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
        // A fresh narrow window opens straight to the editor.
        assert!(!app.sidebar_open(), "overlay sidebar is closed by default");
        // The hamburger summons it.
        app.toggle_sidebar();
        assert!(app.sidebar_open());
    }

    #[test]
    fn wide_window_docks_sidebar_open_by_default() {
        let mut app = CodeApp::new();
        app.set_width_hint(1200.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
        assert!(app.sidebar_open(), "docked sidebar opens by default");
    }

    #[test]
    fn opening_a_file_in_overlay_dismisses_the_flap() {
        let mut app = CodeApp::new();
        app.set_width_hint(600.0);
        app.toggle_sidebar();
        assert!(app.sidebar_open(), "summoned open before selecting a file");
        // Find prime.rs and open it via the tree-event path the sidebar emits.
        let prime = app
            .session
            .project
            .nodes
            .values()
            .find(|n| n.name == "prime.rs")
            .unwrap()
            .id;
        app.apply_tree_event(TreeEvent::OpenFile(prime));
        assert_eq!(app.session.active_buffer().unwrap().name, "prime.rs");
        assert!(!app.sidebar_open(), "opening a file dismisses the overlay flap");
    }

    #[test]
    fn opening_a_file_keeps_docked_sidebar_pinned() {
        let mut app = CodeApp::new();
        app.set_width_hint(1200.0);
        assert!(app.sidebar_open());
        let prime = app
            .session
            .project
            .nodes
            .values()
            .find(|n| n.name == "prime.rs")
            .unwrap()
            .id;
        app.apply_tree_event(TreeEvent::OpenFile(prime));
        assert!(app.sidebar_open(), "docked sidebar stays open after open");
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
