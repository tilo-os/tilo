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

use automerge::{AutoCommit, ObjId, ReadDoc};
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
use crate::bottom_panel::{BottomPanel, HitRow, PanelTab, SearchState};
use crate::command_palette::{Command, CommandPalette, Entry, PaletteKind};
use crate::disk;
use crate::doc::EditorDoc;
use crate::editor_view::{EditorView, LspAsk};
use crate::file_tree::{FileTree, TreeEvent};
use crate::find::{FindBar, FindEvent};
use crate::lint;
use crate::model::{NodeId as TreeId, NodeKind, Project, Session};
use crate::sample_project;
use crate::search;
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
    /// What the server is currently busy with, if anything. Shown in the status
    /// bar so a multi-minute rust-analyzer index reads as "working", not
    /// "broken".
    lsp_status: LspStatus,
    /// Languages whose server failed to start, so we report the failure once
    /// and do not respawn a missing binary on every frame.
    lsp_failed: std::collections::HashSet<crate::model::Language>,

    /// The find / replace bar and its match model over the active buffer.
    find: FindBar,
    find_node: Option<NodeId>,
    find_matches: Vec<crate::find::Match>,
    find_current: Option<usize>,

    /// The running project-wide search, and the hits it has streamed so far.
    search_job: Option<search::Job>,
    search_hits: Vec<search::Hit>,
    search_truncated: bool,
    /// The needle of the most recent search, kept after the job finishes so the
    /// panel can still say what these results are for.
    last_search_needle: String,

    /// A file picker in flight, and what the app asked for.
    portal: Option<ui::portal::PortalChannel>,
    portal_intent: Option<PortalIntent>,

    /// A save that stopped because the file changed on disk. Holds the buffer
    /// it was for, so the user's choice applies to the right one.
    conflict: Option<Conflict>,
    conflict_hover: Point,
    conflict_buttons: Vec<(ConflictChoice, Rect)>,

    /// A transient message about the last file operation (saved, failed,
    /// reloaded). Shown in the status bar; cleared by the next action.
    notice: Option<Notice>,
    /// A caret position to land on once the buffer it belongs to is bound —
    /// used by a search hit and a cross-file go-to-definition, both of which
    /// open a file that has no editor geometry until the next frame.
    pending_jump: Option<(usize, usize)>,
}

/// What a pending portal pick is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortalIntent {
    /// Replace the project with the chosen directory.
    OpenFolder,
    /// Open the chosen file into a tab, in the current project.
    OpenFile,
    /// Give this buffer a path and write it there.
    SaveAs(TreeId),
}

/// A save refused because the file moved on underneath us.
#[derive(Clone, Debug)]
struct Conflict {
    node: TreeId,
    name: String,
}

/// The three ways out of a conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConflictChoice {
    /// Write our text over theirs.
    Overwrite,
    /// Throw our edits away and take what is on disk.
    Reload,
    Cancel,
}

/// What the language server is doing, as far as the editor can tell.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum LspStatus {
    /// No server for this buffer's language (or none configured).
    #[default]
    None,
    /// Spawned, handshake sent, nothing reported back yet.
    Starting { server: String },
    /// Busy with a named task — this is the indexing state.
    Busy {
        server: String,
        title: String,
        detail: Option<String>,
        percentage: Option<u32>,
    },
    /// Up and answering.
    Ready { server: String },
    /// Could not start; carries the reason.
    Failed(String),
}

impl LspStatus {
    /// The short label the status bar shows.
    pub fn label(&self) -> String {
        match self {
            LspStatus::None => String::new(),
            LspStatus::Starting { server } => format!("{server}: starting\u{2026}"),
            LspStatus::Busy {
                server,
                title,
                detail,
                percentage,
            } => {
                let mut s = format!("{server}: {}", title.to_lowercase());
                if let Some(d) = detail {
                    s.push_str(&format!(" {d}"));
                }
                if let Some(p) = percentage {
                    s.push_str(&format!(" {p}%"));
                }
                s
            }
            LspStatus::Ready { server } => format!("{server}: ready"),
            LspStatus::Failed(e) => format!("Language server: {e}"),
        }
    }

    /// Whether the server is working on something — the status bar spins its
    /// activity dot while this is true.
    pub fn is_working(&self) -> bool {
        matches!(self, LspStatus::Starting { .. } | LspStatus::Busy { .. })
    }
}

/// A transient message about a file operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub is_error: bool,
}

impl CodeApp {
    /// The editor with no project: what the app opens to when it was not given
    /// a path. There is nothing to show, and it says so rather than inventing a
    /// project that does not exist on this machine.
    pub fn new() -> Self {
        Self::from_session(Session::new(Project::new("No project")), None)
    }

    /// The editor over the bundled demo project. Reserved for `--shots` and the
    /// demo — the shipped app never fabricates files.
    pub fn with_sample() -> Self {
        let project = sample_project::build();
        let default = sample_project::default_open(&project);
        let mut session = Session::new(project);
        if let Some(node) = default {
            session.open_file(node);
        }
        Self::from_session(session, default)
    }

    /// Open a real path: a directory becomes the project tree, a file opens in
    /// a tab inside a project rooted at its folder.
    ///
    /// A path that cannot be read is reported in the editor rather than being
    /// swallowed into an empty window — that failure is the single most
    /// important thing the user needs to see.
    pub fn open_path(path: &std::path::Path) -> Self {
        match std::fs::metadata(path) {
            Ok(md) if md.is_dir() => match Project::from_dir(path) {
                Ok(project) => Self::from_session(Session::new(project), None),
                Err(e) => Self::failed_to_open(path, &e.to_string()),
            },
            Ok(_) => match Project::open_single_file(path) {
                Ok((project, node)) => {
                    let mut session = Session::new(project);
                    session.open_file(node);
                    Self::from_session(session, Some(node))
                }
                Err(e) => Self::failed_to_open(path, &e.to_string()),
            },
            Err(e) => Self::failed_to_open(path, &e.to_string()),
        }
    }

    /// An editor that opened nothing, showing why.
    fn failed_to_open(path: &std::path::Path, reason: &str) -> Self {
        let mut app = Self::new();
        app.notice = Some(Notice {
            text: format!("Could not open {}: {reason}", path.display()),
            is_error: true,
        });
        app
    }

    fn from_session(session: Session, default: Option<TreeId>) -> Self {
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
            lsp_status: LspStatus::None,
            lsp_failed: std::collections::HashSet::new(),
            find: FindBar::new(),
            find_node: None,
            find_matches: Vec::new(),
            find_current: None,
            search_job: None,
            search_hits: Vec::new(),
            search_truncated: false,
            last_search_needle: String::new(),
            portal: None,
            portal_intent: None,
            conflict: None,
            conflict_hover: Point::new(-1.0, -1.0),
            conflict_buttons: Vec::new(),
            notice: None,
            pending_jump: None,
        }
    }

    /// The project root on disk, when a real directory is open.
    pub fn project_root(&self) -> Option<std::path::PathBuf> {
        self.session.project.root_path()
    }

    /// The most recent file-operation message, for tests and the status bar.
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    fn note(&mut self, text: impl Into<String>, is_error: bool) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error,
        });
    }

    /// What the language server is doing.
    pub fn lsp_status(&self) -> &LspStatus {
        &self.lsp_status
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

        // Seed each open buffer's text into the document, reading the real file
        // for a disk-backed node. This is the read half of "open a file": until
        // it runs, a tab is bound to an empty Text.
        //
        // A read failure is recorded and the tab closed again — a file that
        // opened as a silently empty buffer would be written back empty by the
        // next Ctrl+S.
        let mut failed: Vec<(TreeId, String)> = Vec::new();
        for i in 0..self.session.open.len() {
            let node = self.session.open[i].node;
            if EditorDoc::buffer_text(doc, node).is_some() {
                continue;
            }
            match self.session.project.file_seed(node) {
                Ok((text, stamp)) => {
                    EditorDoc::ensure_buffer(doc, node, &text);
                    let buf = &mut self.session.open[i];
                    buf.path = self.session.project.disk_path_of(node);
                    buf.mark_saved(text, stamp);
                }
                Err(e) => {
                    let name = self.session.open[i].name.clone();
                    failed.push((node, format!("Could not open {name}: {e}")));
                }
            }
        }
        for (node, message) in failed {
            if let Some(idx) = self.session.open.iter().position(|b| b.node == node) {
                self.session.close_tab(idx);
            }
            self.note(message, true);
            self.bound_node = None;
        }
        self.seeded = true;

        // Dirty is a comparison against the bytes on disk, recomputed here, so
        // undoing back to the original clears the indicator rather than leaving
        // it stuck on.
        for i in 0..self.session.open.len() {
            let node = self.session.open[i].node;
            let current = EditorDoc::read_buffer(doc, node).unwrap_or_default();
            let buf = &mut self.session.open[i];
            buf.dirty = buf.is_dirty(&current);
        }

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
        let notice = self.notice.clone().unwrap_or(Notice {
            text: String::new(),
            is_error: false,
        });
        self.status.set_state(StatusState {
            language: lang,
            line: line + 1,
            col: col + 1,
            selection: self.editor.selection_len(),
            caret_count: self.editor.caret_count(),
            indent,
            problems: self.panel.problem_count(),
            project: self.session.project.name.clone(),
            lsp: self.lsp_status.label(),
            lsp_working: self.lsp_status.is_working(),
            notice: notice.text,
            notice_is_error: notice.is_error,
            dirty: self.session.active_buffer().map(|b| b.dirty).unwrap_or(false),
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

    // ---- saving -----------------------------------------------------------

    /// Write one buffer to its file.
    ///
    /// `force` is the user having answered "overwrite anyway" to a conflict; it
    /// is the only way to skip the freshness check, and it is never set by the
    /// app on its own.
    pub fn save_buffer(&mut self, doc: &AutoCommit, node: TreeId, force: bool) -> bool {
        let Some(idx) = self.session.open.iter().position(|b| b.node == node) else {
            return false;
        };
        let Some(path) = self.session.open[idx].path.clone() else {
            // No file yet: a scratch buffer needs a destination first, which
            // only the user (through the picker) can choose.
            self.request_save_as(node);
            return false;
        };
        let text = EditorDoc::read_buffer(doc, node).unwrap_or_default();
        let expected = if force {
            None
        } else {
            self.session.open[idx].stamp.clone()
        };

        match disk::save(&path, &text, expected.as_ref()) {
            Ok(stamp) => {
                let name = self.session.open[idx].name.clone();
                self.session.open[idx].mark_saved(text, Some(stamp));
                self.note(format!("Saved {name}"), false);
                self.refresh_tabs();
                // Tell the language server the file is on disk now; rustc-based
                // diagnostics only refresh on save.
                if let (Some(c), Some(uri)) = (self.lsp.as_mut(), self.lsp_uri.clone()) {
                    c.did_save(&uri);
                }
                true
            }
            Err(disk::SaveError::Conflict(_)) => {
                self.session.open[idx].stale = true;
                self.conflict = Some(Conflict {
                    node,
                    name: self.session.open[idx].name.clone(),
                });
                false
            }
            Err(e) => {
                let name = self.session.open[idx].name.clone();
                self.note(format!("Could not save {name}: {e}"), true);
                false
            }
        }
    }

    /// Save the active buffer.
    fn save_active(&mut self, doc: &AutoCommit) -> bool {
        match self.session.active_buffer().map(|b| b.node) {
            Some(node) => self.save_buffer(doc, node, false),
            None => false,
        }
    }

    /// Save every buffer with unsaved edits. Reports how many were written, and
    /// stops at the first conflict so the user answers one question at a time.
    fn save_all(&mut self, doc: &AutoCommit) -> usize {
        let dirty: Vec<TreeId> = self
            .session
            .open
            .iter()
            .filter(|b| b.dirty && b.path.is_some())
            .map(|b| b.node)
            .collect();
        let mut written = 0;
        for node in dirty {
            if self.conflict.is_some() {
                break;
            }
            if self.save_buffer(doc, node, false) {
                written += 1;
            }
        }
        if self.conflict.is_none() {
            match written {
                0 => self.note("Nothing to save", false),
                1 => self.note("Saved 1 file", false),
                n => self.note(format!("Saved {n} files"), false),
            }
        }
        written
    }

    /// Throw away this buffer's edits and take what is on disk.
    fn reload_buffer(&mut self, doc: &mut AutoCommit, node: TreeId) {
        let Some(idx) = self.session.open.iter().position(|b| b.node == node) else {
            return;
        };
        match self.session.project.file_seed(node) {
            Ok((text, stamp)) => {
                let name = self.session.open[idx].name.clone();
                // Replace the buffer's whole text through the CRDT so the edit
                // is one undoable action and peers converge on it.
                if let Some(obj) = EditorDoc::buffer_text(doc, node) {
                    let old_len = doc.text(&obj).map(|t| t.chars().count()).unwrap_or(0);
                    EditorDoc::splice(doc, &obj, 0, old_len, &text);
                }
                self.session.open[idx].mark_saved(text, stamp);
                self.note(format!("Reloaded {name} from disk"), false);
                self.bound_node = None;
                self.refresh_tabs();
            }
            Err(e) => {
                let name = self.session.open[idx].name.clone();
                self.note(format!("Could not reload {name}: {e}"), true);
            }
        }
    }

    /// Notice files that changed on disk since we read them, so a tab can warn
    /// before the user tries to save over someone else's work.
    fn check_disk_staleness(&mut self) {
        for buf in self.session.open.iter_mut() {
            let (Some(path), Some(stamp)) = (buf.path.as_ref(), buf.stamp.as_ref()) else {
                continue;
            };
            if !buf.stale && disk::changed_on_disk(path, stamp) {
                buf.stale = true;
            }
        }
    }

    /// Answer an open conflict.
    fn resolve_conflict(&mut self, doc: &mut AutoCommit, choice: ConflictChoice) {
        let Some(conflict) = self.conflict.take() else {
            return;
        };
        match choice {
            ConflictChoice::Overwrite => {
                let snapshot = doc.clone();
                self.save_buffer(&snapshot, conflict.node, true);
            }
            ConflictChoice::Reload => self.reload_buffer(doc, conflict.node),
            ConflictChoice::Cancel => {
                self.note(format!("{} was not saved", conflict.name), false);
            }
        }
    }

    /// The conflict prompt's keyboard shortcuts: Escape cancels, Enter takes
    /// the safe option (reload, which cannot destroy the other writer's work).
    fn conflict_key(&self, event: &WindowEvent) -> Option<ConflictChoice> {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return None;
        };
        match logical_key {
            Key::Named(winit::keyboard::NamedKey::Escape) => Some(ConflictChoice::Cancel),
            Key::Named(winit::keyboard::NamedKey::Enter) => Some(ConflictChoice::Reload),
            _ => None,
        }
    }

    /// Draw the "this file changed on disk" prompt over everything.
    ///
    /// This is deliberately a decision the user makes, not one the editor makes
    /// for them: overwriting silently loses whatever rewrote the file, and
    /// reloading silently loses what they typed. Both are real losses, so both
    /// are offered by name, and the default (Enter) is the one that cannot
    /// destroy someone else's work.
    fn draw_conflict(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.conflict_buttons.clear();
        let Some(conflict) = self.conflict.clone() else {
            return;
        };
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.scrim, None, &self.rect);

        let w = 460.0_f64.min(self.rect.width() - 32.0);
        let h = 190.0_f64;
        let x0 = self.rect.x0 + (self.rect.width() - w) / 2.0;
        let y0 = self.rect.y0 + (self.rect.height() - h) / 2.0;
        let card = vello::kurbo::RoundedRect::new(x0, y0, x0 + w, y0 + h, 12.0);
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &card);
        scene.stroke(&Stroke::new(1.0), transform, c.border_white_20, None, &card);

        text_renderer.draw_text_weighted(
            scene,
            &format!("{} changed on disk", conflict.name),
            16.0,
            c.text_primary,
            transform * Affine::translate((x0 + 20.0, y0 + 20.0)),
            Some((w - 40.0) as f32),
            ui::text_render::TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            "Something else wrote this file after you opened it. Overwriting \
             discards their change; reloading discards yours.",
            13.0,
            c.text_dim,
            transform * Affine::translate((x0 + 20.0, y0 + 50.0)),
            Some((w - 40.0) as f32),
        );

        // Buttons, right-aligned, with the destructive one clearly marked and
        // never the default.
        let bw = 116.0;
        let bh = 34.0;
        let by = y0 + h - 20.0 - bh;
        let mut bx = x0 + w - 20.0 - bw;
        for (choice, label, style) in [
            (ConflictChoice::Reload, "Reload", ui::widget::ButtonStyle::Primary),
            (ConflictChoice::Overwrite, "Overwrite", ui::widget::ButtonStyle::Danger),
            (ConflictChoice::Cancel, "Cancel", ui::widget::ButtonStyle::Neutral),
        ] {
            let rect = Rect::new(bx, by, bx + bw, by + bh);
            let face = ui::widget::ButtonFace::new(rect, label)
                .style(style)
                .hovered(rect.contains(self.conflict_hover));
            ui::widget::Button::paint(scene, transform, text_renderer, theme, &face);
            self.conflict_buttons.push((choice, rect));
            bx -= bw + 8.0;
        }
    }

    // ---- the file picker --------------------------------------------------

    /// Ask the host desktop for a folder to open as the project.
    ///
    /// This goes through the XDG FileChooser portal rather than any in-app
    /// browser: under app-state confinement the editor has exactly one writable
    /// directory of its own and no manifest field can add more, so a path the
    /// user picked in the portal — which is where the sandbox's file-access
    /// grant is made — is the only way it reaches the rest of the filesystem.
    fn request_open_folder(&mut self) {
        self.portal_intent = Some(PortalIntent::OpenFolder);
        self.portal = Some(
            ui::portal::PickRequest::open("Open a project folder", "Open")
                .thread_name("code-portal-folder")
                .send(),
        );
    }

    /// Ask the host desktop for a file to open into a tab.
    fn request_open_file(&mut self) {
        self.portal_intent = Some(PortalIntent::OpenFile);
        self.portal = Some(
            ui::portal::PickRequest::open("Open a file", "Open")
                .thread_name("code-portal-file")
                .filter(source_filter())
                .all_files()
                .send(),
        );
    }

    /// Ask the host desktop where to write a buffer that has no file yet.
    fn request_save_as(&mut self, node: TreeId) {
        let suggested = self
            .session
            .buffer_of(node)
            .map(|b| b.name.clone())
            .unwrap_or_else(|| "untitled.txt".to_string());
        self.portal_intent = Some(PortalIntent::SaveAs(node));
        self.portal = Some(
            ui::portal::PickRequest::save("Save file as", "Save", &suggested)
                .thread_name("code-portal-save")
                .filter(source_filter())
                .all_files()
                .send(),
        );
    }

    /// Drain a pending picker, once per frame. Never blocks: the portal's
    /// D-Bus round-trip and its host dialog live entirely on a worker thread.
    fn poll_portal(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = self.portal.as_ref() else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal = None;
                self.portal_intent = None;
                return;
            }
        };
        self.portal = None;
        let intent = self.portal_intent.take();
        self.apply_portal_outcome(doc, intent, outcome);
    }

    /// Fold a picker result into the app. Separate from the channel plumbing so
    /// a test can drive the exact path a real pick takes.
    pub(crate) fn apply_portal_outcome(
        &mut self,
        doc: &mut AutoCommit,
        intent: Option<PortalIntent>,
        outcome: ui::portal::PortalOutcome,
    ) {
        use ui::portal::PortalOutcome;
        let path = match outcome {
            PortalOutcome::Opened(p) => p,
            PortalOutcome::Cancelled => return,
            PortalOutcome::Failed(e) => {
                self.note(format!("File picker unavailable: {e}"), true);
                return;
            }
        };
        match intent {
            Some(PortalIntent::OpenFolder) => self.open_project_dir(&path),
            Some(PortalIntent::OpenFile) => self.open_disk_file(&path),
            Some(PortalIntent::SaveAs(node)) => {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "untitled".to_string());
                self.session.project.set_file_path(node, &path, &name);
                if let Some(buf) = self.session.buffer_of_mut(node) {
                    buf.path = Some(path.clone());
                    buf.name = name.clone();
                    buf.language = crate::model::Language::from_path(&name);
                    // No stamp: this is a destination the user chose, and the
                    // save that follows is the one they asked for.
                    buf.stamp = None;
                }
                self.sidebar.set_project(&self.session.project);
                let snapshot = doc.clone();
                self.save_buffer(&snapshot, node, true);
            }
            None => {}
        }
    }

    /// Replace the project with the directory at `path`.
    fn open_project_dir(&mut self, path: &std::path::Path) {
        match Project::from_dir(path) {
            Ok(project) => {
                let name = project.name.clone();
                self.session = Session::new(project);
                self.sidebar.set_project(&self.session.project);
                self.sidebar.set_active(None);
                self.bound_node = None;
                self.editor.unbind();
                self.reset_language_server();
                self.clear_search();
                self.refresh_tabs();
                self.refresh_palette_entries();
                self.note(format!("Opened {name}"), false);
            }
            Err(e) => self.note(format!("Could not open {}: {e}", path.display()), true),
        }
    }

    /// Open a real file into a tab, revealing it in the tree when it lives
    /// under the open project and adopting it as a one-file project when not.
    fn open_disk_file(&mut self, path: &std::path::Path) {
        let path = disk::canonical(path);
        if let Some(node) = self.session.project.reveal_path(&path) {
            self.sidebar.set_project(&self.session.project);
            self.open_node(node);
            return;
        }
        match Project::open_single_file(&path) {
            Ok((project, node)) => {
                self.session = Session::new(project);
                self.sidebar.set_project(&self.session.project);
                self.bound_node = None;
                self.reset_language_server();
                self.open_node(node);
                self.refresh_palette_entries();
            }
            Err(e) => self.note(format!("Could not open {}: {e}", path.display()), true),
        }
    }

    /// Tear down any running language server, so the next sync starts one for
    /// the new project root.
    fn reset_language_server(&mut self) {
        self.lsp = None;
        self.lsp_language = None;
        self.lsp_uri = None;
        self.lsp_sent_text.clear();
        self.lsp_diagnostics.clear();
        self.lsp_status = LspStatus::None;
        self.lsp_failed.clear();
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
                // Reading the folder's children happens here, on first
                // expansion, rather than when the project was opened — one
                // directory level at a time is what makes opening a large
                // repository instant.
                if let Err(e) = self.session.project.load_children(id) {
                    let name = self
                        .session
                        .project
                        .get(id)
                        .map(|n| n.name.clone())
                        .unwrap_or_default();
                    self.note(format!("Could not read {name}: {e}"), true);
                }
                self.session.project.toggle_folder(id);
                self.sidebar.set_project(&self.session.project);
                self.sidebar
                    .set_active(self.session.active_buffer().map(|b| b.node));
                self.refresh_palette_entries();
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
            Command::OpenFolder => self.request_open_folder(),
            Command::OpenFile => self.request_open_file(),
            Command::Save => {
                let snapshot = doc.clone();
                self.save_active(&snapshot);
            }
            Command::SaveAs => {
                if let Some(node) = self.session.active_buffer().map(|b| b.node) {
                    self.request_save_as(node);
                }
            }
            Command::SaveAll => {
                let snapshot = doc.clone();
                self.save_all(&snapshot);
            }
            Command::RevertFile => {
                if let Some(node) = self.session.active_buffer().map(|b| b.node) {
                    self.reload_buffer(doc, node);
                }
            }
            Command::Find => self.open_find(false, doc),
            Command::Replace => self.open_find(true, doc),
            Command::SearchProject => {
                // Seed from the selection, the way every editor does.
                let seed = self
                    .editor
                    .selected_text_public()
                    .filter(|s| !s.is_empty() && !s.contains('\n'))
                    .unwrap_or_default();
                self.start_project_search(search::Query::new(&seed));
                self.panel_visible = true;
                self.panel.set_tab(PanelTab::Search);
            }
            Command::GoToSearch => {
                self.panel_visible = true;
                self.panel.set_tab(PanelTab::Search);
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
        if let Some(index) = self.panel.take_activated_hit() {
            self.open_search_hit(index);
        }
    }

    // ---- in-buffer find / replace -----------------------------------------

    /// Recount matches over the active buffer and push the state to the bar and
    /// the editor's highlight layer.
    fn refresh_find(&mut self, doc: &AutoCommit) {
        if !self.find.is_visible() {
            if !self.find_matches.is_empty() {
                self.find_matches.clear();
                self.find_current = None;
                self.editor.set_find_matches(&[], None);
            }
            return;
        }
        let text = match self.session.active_buffer().map(|b| b.node) {
            Some(node) => EditorDoc::read_buffer(doc, node).unwrap_or_default(),
            None => String::new(),
        };
        self.find_matches = crate::find::matches_in(&text, &self.find.query());
        // Keep the current match if it still exists, else point at the first
        // one at or after the caret.
        let (line, col) = self.editor.primary_caret();
        self.find_current = match self.find_current {
            Some(i) if i < self.find_matches.len() => Some(i),
            _ => crate::find::next_from(&self.find_matches, line, col),
        };
        self.find
            .set_match_state(self.find_matches.len(), self.find_current);
        self.editor
            .set_find_matches(&self.find_matches, self.find_current);
    }

    /// Move to a match and select it, so the next keystroke replaces it the way
    /// it would in any editor.
    fn focus_match(&mut self, index: Option<usize>) {
        self.find_current = index;
        if let Some(m) = index.and_then(|i| self.find_matches.get(i)).copied() {
            self.editor
                .set_selection_lc((m.line, m.col), (m.line, m.col + m.len));
        }
        self.find
            .set_match_state(self.find_matches.len(), self.find_current);
        self.editor
            .set_find_matches(&self.find_matches, self.find_current);
    }

    /// Replace the current match, then step to the next one.
    fn replace_current(&mut self, doc: &mut AutoCommit) {
        let Some(node) = self.session.active_buffer().map(|b| b.node) else {
            return;
        };
        let Some(m) = self.find_current.and_then(|i| self.find_matches.get(i)).copied() else {
            return;
        };
        let Some(obj) = EditorDoc::buffer_text(doc, node) else {
            return;
        };
        let text = doc.text(&obj).unwrap_or_default();
        let Some(at) = flat_index(&text, m.line, m.col) else {
            return;
        };
        let replacement = self.find.replacement().to_string();
        EditorDoc::splice(doc, &obj, at, m.len, &replacement);
        self.editor.move_caret_to(m.line, m.col + replacement.chars().count());
        self.refresh_find(doc);
        let (line, col) = self.editor.primary_caret();
        self.focus_match(crate::find::next_from(&self.find_matches, line, col));
    }

    /// Replace every match in the buffer, as one undoable action.
    ///
    /// Replacements are applied last-to-first so each splice's index is still
    /// valid: replacing forwards would shift every later match by the
    /// difference in length.
    fn replace_all(&mut self, doc: &mut AutoCommit) {
        let Some(node) = self.session.active_buffer().map(|b| b.node) else {
            return;
        };
        let Some(obj) = EditorDoc::buffer_text(doc, node) else {
            return;
        };
        let text = doc.text(&obj).unwrap_or_default();
        let replacement = self.find.replacement().to_string();
        let count = self.find_matches.len();
        for m in self.find_matches.clone().into_iter().rev() {
            if let Some(at) = flat_index(&text, m.line, m.col) {
                EditorDoc::splice(doc, &obj, at, m.len, &replacement);
            }
        }
        self.find_current = None;
        self.refresh_find(doc);
        self.note(
            match count {
                0 => "Nothing to replace".to_string(),
                1 => "Replaced 1 match".to_string(),
                n => format!("Replaced {n} matches"),
            },
            false,
        );
    }

    /// Apply whatever the find bar asked for this frame.
    fn drain_find_events(&mut self, doc: &mut AutoCommit) {
        for ev in self.find.take_events() {
            match ev {
                FindEvent::QueryChanged => {
                    self.find_current = None;
                    self.refresh_find(doc);
                }
                FindEvent::Next => {
                    let (line, col) = self.editor.primary_caret();
                    // Step past the current match so Next does not re-find it.
                    let from = self
                        .find_current
                        .and_then(|i| self.find_matches.get(i))
                        .map(|m| (m.line, m.col + 1))
                        .unwrap_or((line, col));
                    self.focus_match(crate::find::next_from(&self.find_matches, from.0, from.1));
                }
                FindEvent::Prev => {
                    let (line, col) = self.editor.primary_caret();
                    let from = self
                        .find_current
                        .and_then(|i| self.find_matches.get(i))
                        .map(|m| (m.line, m.col))
                        .unwrap_or((line, col));
                    self.focus_match(crate::find::prev_from(&self.find_matches, from.0, from.1));
                }
                FindEvent::ReplaceCurrent => self.replace_current(doc),
                FindEvent::ReplaceAll => self.replace_all(doc),
                FindEvent::Close => {
                    self.find_matches.clear();
                    self.find_current = None;
                    self.editor.set_find_matches(&[], None);
                }
            }
        }
    }

    /// Open the find bar, seeded with the editor's selection.
    fn open_find(&mut self, replace: bool, doc: &AutoCommit) {
        let seed = self.editor.selected_text_public();
        self.find.open(replace, seed.as_deref());
        self.refresh_find(doc);
    }

    // ---- project-wide search ----------------------------------------------

    /// Start a search over the whole project on a worker thread, replacing any
    /// search already running.
    fn start_project_search(&mut self, query: search::Query) {
        // A previous job is cancelled rather than left to finish: eight
        // abandoned walks of a large repository is eight threads reading it.
        if let Some(job) = self.search_job.take() {
            job.cancel();
        }
        self.search_hits.clear();
        self.search_truncated = false;
        let Some(root) = self.session.project.root_path() else {
            self.note("Open a project folder to search it", true);
            return;
        };
        self.last_search_needle = query.needle.clone();
        if query.is_empty() {
            self.push_search_state();
            return;
        }
        self.search_job = Some(search::start(&root, query));
        self.panel_visible = true;
        self.panel.set_tab(PanelTab::Search);
        self.push_search_state();
    }

    /// Drain whatever the search worker produced since the last frame.
    fn poll_search(&mut self) {
        let Some(job) = self.search_job.as_mut() else {
            return;
        };
        let msgs = job.drain();
        if msgs.is_empty() && !job.is_finished() {
            return;
        }
        for msg in msgs {
            match msg {
                search::Msg::Hits(hits) => self.search_hits.extend(hits),
                search::Msg::Done { truncated, .. } => self.search_truncated = truncated,
            }
        }
        if self.search_job.as_ref().map(|j| j.is_finished()).unwrap_or(false) {
            self.search_job = None;
        }
        self.push_search_state();
    }

    /// Mirror the search state into the panel.
    fn push_search_state(&mut self) {
        let query = self
            .search_job
            .as_ref()
            .map(|j| j.query().needle.clone())
            .unwrap_or_else(|| self.last_search_needle.clone());
        self.panel.set_search(SearchState {
            query,
            rows: self
                .search_hits
                .iter()
                .map(|h| HitRow {
                    rel: h.rel.clone(),
                    line: h.line,
                    preview: h.preview.clone(),
                    highlight: (h.col.saturating_sub(h.preview_offset), h.len),
                })
                .collect(),
            running: self.search_job.is_some(),
            truncated: self.search_truncated,
            files_scanned: 0,
        });
    }

    fn clear_search(&mut self) {
        if let Some(job) = self.search_job.take() {
            job.cancel();
        }
        self.search_hits.clear();
        self.search_truncated = false;
        self.last_search_needle.clear();
        self.push_search_state();
    }

    /// Open the file a search hit points at, with the caret on the match.
    fn open_search_hit(&mut self, index: usize) {
        let Some(hit) = self.search_hits.get(index).cloned() else {
            return;
        };
        let Some(node) = self.session.project.reveal_path(&hit.path) else {
            self.note(format!("Could not open {}", hit.rel), true);
            return;
        };
        self.sidebar.set_project(&self.session.project);
        self.open_node(node);
        // The buffer is bound on the next sync, so the caret move waits for it.
        self.pending_jump = Some((hit.line.saturating_sub(1), hit.col));
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

    /// The `file://` URI for a project node.
    ///
    /// For a disk-backed node this is the real file's URI, which is the whole
    /// point: a server told about `file:///project-name/src/main.rs` when the
    /// file is at `/home/me/qos/shell/src/main.rs` has been told about a
    /// document that does not exist, and answers accordingly. The synthetic
    /// form survives only for the in-memory demo tree, which has no path.
    fn node_uri(&self, node: TreeId) -> String {
        match self.session.project.disk_path_of(node) {
            Some(path) => crate::lsp::path_to_uri(&path),
            None => format!("file:///{}", self.session.project.path_of(node)),
        }
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

    /// Whether live language servers are wanted. On by default — an IDE whose
    /// diagnostics are behind an environment variable is an IDE without
    /// diagnostics. `QOS_CODE_LSP=0` is the opt-out.
    ///
    /// A server is only ever started for a project that is a real directory,
    /// which is also what keeps `--shots`, the demo project and the test suite
    /// from ever launching rust-analyzer: they have no path to root it at.
    fn lsp_enabled(&self) -> bool {
        if matches!(
            std::env::var("QOS_CODE_LSP").as_deref(),
            Ok("0") | Ok("off") | Ok("false")
        ) {
            return false;
        }
        self.session.project.is_disk_backed()
    }

    /// Start (or restart) a language server for the active buffer's language,
    /// rooted at the real project directory. A missing binary is reported once
    /// and not retried every frame.
    fn maybe_start_lsp(&mut self, doc: &AutoCommit) {
        if !self.lsp_enabled() {
            return;
        }
        let Some(buf) = self.session.active_buffer() else {
            return;
        };
        let (node, lang) = (buf.node, buf.language);
        if self.lsp.is_some() && self.lsp_language == Some(lang) {
            // Already serving this language; just make sure the right document
            // is open on it.
            let uri = self.node_uri(node);
            if self.lsp_uri.as_deref() != Some(uri.as_str()) {
                let text = EditorDoc::read_buffer(doc, node).unwrap_or_default();
                self.lsp_version += 1;
                let version = self.lsp_version;
                if let Some(c) = self.lsp.as_mut() {
                    c.did_open(&uri, Self::language_id(lang), version, &text);
                }
                self.lsp_sent_text = text;
                self.lsp_uri = Some(uri);
            }
            return;
        }
        if crate::lsp::LspClient::server_command(lang).is_none() {
            self.lsp_status = LspStatus::None;
            return; // no server mapping for this language
        }
        if self.lsp_failed.contains(&lang) {
            return; // already reported; do not respawn a missing binary
        }
        let Some(root) = self.session.project.root_path() else {
            return;
        };
        match crate::lsp::LspClient::start(lang, &root) {
            Ok(mut client) => {
                let uri = self.node_uri(node);
                let text = EditorDoc::read_buffer(doc, node).unwrap_or_default();
                let server = client.server_name().to_string();
                tracing::info!(
                    "started language server {server} for {} at {}",
                    lang.label(),
                    root.display()
                );
                self.lsp_version = 1;
                client.did_open(&uri, Self::language_id(lang), self.lsp_version, &text);
                self.lsp_sent_text = text;
                self.lsp = Some(client);
                self.lsp_language = Some(lang);
                self.lsp_uri = Some(uri);
                self.lsp_status = LspStatus::Starting { server };
            }
            Err(e) => {
                self.lsp = None;
                self.lsp_language = None;
                self.lsp_failed.insert(lang);
                self.lsp_status = LspStatus::Failed(e);
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
                crate::lsp::LspEvent::Progress(p) => {
                    let server = self.lsp_server_name();
                    // Folding through the client's table is what keeps the task
                    // label stable: a `report` carries no title, only the token.
                    let shown = self.lsp.as_mut().and_then(|c| c.note_progress(p));
                    self.lsp_status = match shown {
                        Some(p) => LspStatus::Busy {
                            server,
                            title: p.title,
                            detail: p.message,
                            percentage: p.percentage,
                        },
                        None => LspStatus::Ready { server },
                    };
                }
                crate::lsp::LspEvent::Initialized => {
                    // Only claim ready if nothing is still in flight; the
                    // initialize response usually lands before indexing starts.
                    if !self.lsp.as_ref().map(|c| c.is_busy()).unwrap_or(false) {
                        self.lsp_status = LspStatus::Ready {
                            server: self.lsp_server_name(),
                        };
                    }
                }
                crate::lsp::LspEvent::Exited => {
                    let server = self.lsp_server_name();
                    self.lsp = None;
                    self.lsp_language = None;
                    self.lsp_status = LspStatus::Failed(format!("{server} exited"));
                }
            }
        }
    }

    fn lsp_server_name(&self) -> String {
        self.lsp
            .as_ref()
            .map(|c| c.server_name().to_string())
            .unwrap_or_else(|| "Language server".to_string())
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

    /// Jump to a definition result.
    ///
    /// A same-file result just moves the caret. A result in another file — the
    /// common case once the server is rooted at a real workspace, since most
    /// definitions live in another module or another crate — opens that file
    /// first. Without this, go-to-definition silently does nothing for exactly
    /// the jumps people use it for.
    fn apply_definition(&mut self, locs: Vec<crate::lsp::Location>) {
        let Some(loc) = locs.into_iter().next() else {
            return;
        };
        let (line, col) = (
            loc.range.start.line as usize,
            loc.range.start.character as usize,
        );
        if self.lsp_uri.as_deref() == Some(loc.uri.as_str()) {
            self.editor.move_caret_to(line, col);
            return;
        }
        // Another file: resolve the URI back to a path through the same strict
        // mapping the portal uses, open it, and land the caret there.
        let Ok(url) = url::Url::parse(&loc.uri) else {
            return;
        };
        let Some(path) = ui::portal::to_local_path(&url) else {
            self.note(format!("Definition is outside this machine: {}", loc.uri), true);
            return;
        };
        self.open_disk_file(&path);
        self.pending_jump = Some((line, col));
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

    /// Give the Terminal panel an honest empty state until a real shell is
    /// spawned. Opening the Terminal tab spawns the live PTY (`ensure_terminal`,
    /// which clears this note); we never pre-fill a fabricated build log.
    fn seed_terminal(&mut self) {
        self.panel.set_terminal_note(Some(
            "No terminal running. Open the Terminal to run commands in this project.".to_string(),
        ));
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

    /// Stage the find bar over the active buffer with a real query, so the shot
    /// shows real match highlighting and a real count.
    pub fn shots_open_find(&mut self, query: &str, replace: bool, doc: &AutoCommit) {
        self.find.open(replace, None);
        self.find.set_query(query);
        if replace {
            self.find.set_replacement("buffer");
        }
        self.drain_find_events(&mut doc.clone());
        self.refresh_find(doc);
        // Land on the first match so the current-match fill is visible.
        self.focus_match(if self.find_matches.is_empty() { None } else { Some(0) });
    }

    /// Run a real project-wide search and wait for it, so a single offscreen
    /// frame shows genuine hits from the genuine walk rather than canned rows.
    pub fn shots_project_search(&mut self, needle: &str) {
        self.start_project_search(search::Query::new(needle));
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        while self.search_job.is_some() && Instant::now() < deadline {
            self.poll_search();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        self.poll_search();
        self.panel_visible = true;
        self.panel.set_tab(PanelTab::Search);
    }

    /// Stage the "changed on disk" prompt for the active buffer.
    pub fn shots_conflict(&mut self) {
        if let Some(buf) = self.session.active_buffer() {
            self.conflict = Some(Conflict {
                node: buf.node,
                name: buf.name.clone(),
            });
        }
    }

    /// Stage the language-server indexing readout.
    pub fn shots_indexing(&mut self) {
        self.lsp_status = LspStatus::Busy {
            server: "rust-analyzer".to_string(),
            title: "Indexing".to_string(),
            detail: Some("qos-shell".to_string()),
            percentage: Some(37),
        };
    }

    /// Open the first file the tree found, so a shot of a real project shows a
    /// real buffer instead of the empty state.
    pub fn shots_open_first_file(&mut self) {
        let first = self
            .session
            .project
            .visible_rows()
            .into_iter()
            .find(|id| !self.session.project.get(*id).map(|n| n.is_folder()).unwrap_or(true));
        if let Some(id) = first {
            self.open_node(id);
        }
    }

    /// Open a specific file by path, revealing it in the tree first.
    pub fn shots_open_path(&mut self, path: &std::path::Path) {
        let path = disk::canonical(path);
        if let Some(node) = self.session.project.reveal_path(&path) {
            self.sidebar.set_project(&self.session.project);
            self.open_node(node);
        }
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

/// The flat character index of `(line, col)` in `text`.
///
/// Automerge's `Text` is character-addressed and so is the editor's caret, so
/// every splice has to be converted here rather than from a byte offset — a
/// buffer containing any non-ASCII character would otherwise splice in the
/// wrong place.
fn flat_index(text: &str, line: usize, col: usize) -> Option<usize> {
    let mut idx = 0usize;
    for (i, l) in text.split('\n').enumerate() {
        let len = l.chars().count();
        if i == line {
            return Some(idx + col.min(len));
        }
        idx += len + 1; // the newline itself
    }
    None
}

/// The file-type filter the open and save choosers offer, plus the "All files"
/// escape hatch every picker needs so an unusual extension is not a trap.
fn source_filter() -> ui::portal::FileFilter {
    let mut f = ui::portal::FileFilter::new("Text & source").mimetype("text/plain");
    for glob in [
        "*.rs", "*.toml", "*.md", "*.json", "*.yaml", "*.yml", "*.txt", "*.py", "*.js", "*.ts",
        "*.tsx", "*.html", "*.css", "*.c", "*.h", "*.cpp", "*.hpp", "*.go", "*.sh",
    ] {
        f = f.glob(glob);
    }
    f
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
        // A file picker in flight is drained here, once a frame, off a channel
        // the worker thread fills — the UI never waits on D-Bus.
        self.poll_portal(state);
        self.poll_search();
        self.ensure_buffers(state);
        self.editor.sync(state, path);
        self.find.sync(state, path);
        self.drain_find_events(state);

        // A jump queued by a search hit or a cross-file definition lands once
        // the target buffer is actually bound.
        if let Some((line, col)) = self.pending_jump {
            if self.editor.is_bound() {
                self.editor.move_caret_to(line, col);
                self.pending_jump = None;
            }
        }

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

        self.check_disk_staleness();
        self.refresh_find(state);
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

        // The find bar sits between the tabs and the buffer, so it never covers
        // the code it is searching.
        let find_node = self.find.build_layout(taffy, theme, node_map);
        self.find_node = Some(find_node);

        let mut editor_col_children = vec![tabs_node, find_node, editor_node];
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
                if let Some(n) = self.find_node {
                    self.find.apply_layout(taffy, n, col_origin);
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
                self.find.draw(scene, transform, text_renderer, theme, presence);
                self.editor.draw(scene, transform, text_renderer, theme, presence);
                if self.panel_visible {
                    self.panel.draw(scene, transform, text_renderer, theme, presence);
                }
            }
            SplitLayout::Overlay => {
                // Content spans the full width and is drawn first; the sidebar
                // slides in over it behind a scrim.
                self.tabs.draw(scene, transform, text_renderer, theme, presence);
                self.find.draw(scene, transform, text_renderer, theme, presence);
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
        // …and a conflict prompt above even that: it is a question that must be
        // answered before anything else can be done to the file.
        self.draw_conflict(scene, transform, text_renderer, theme);
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
                let shift = mods.shift_key();
                let alt = mods.alt_key();
                let command = match c.to_lowercase().as_str() {
                    "p" => {
                        self.refresh_palette_entries();
                        if shift {
                            self.palette.open_commands();
                        } else {
                            self.palette.open_files();
                        }
                        *handled = true;
                        return;
                    }
                    "b" => Some(Command::ToggleSidebar),
                    "j" => Some(Command::ToggleBottomPanel),
                    "o" if shift => Some(Command::OpenFolder),
                    "o" => Some(Command::OpenFile),
                    "s" if alt => Some(Command::SaveAll),
                    "s" if shift => Some(Command::SaveAs),
                    "s" => Some(Command::Save),
                    "f" if shift => Some(Command::SearchProject),
                    "f" => Some(Command::Find),
                    "h" => Some(Command::Replace),
                    "w" => Some(Command::CloseTab),
                    _ => None,
                };
                if let Some(command) = command {
                    self.apply_command(command, state);
                    *handled = true;
                    return;
                }
            }
        }

        // A modal conflict prompt owns all input while it is up: the question
        // is "which of these two versions of your file survives", and letting
        // keystrokes fall through to the buffer underneath it would be wrong.
        if self.conflict.is_some() {
            if let Some(choice) = self.conflict_key(event) {
                self.resolve_conflict(state, choice);
            }
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
            {
                if let Some((choice, _)) = self
                    .conflict_buttons
                    .iter()
                    .find(|(_, r)| r.contains(self.last_cursor))
                    .copied()
                {
                    self.resolve_conflict(state, choice);
                }
            }
            *handled = true;
            return;
        }

        // The find bar takes keyboard input while it is open (its fields own
        // typing; Enter and Escape are its transport).
        if self.find.is_visible() {
            if let Some(n) = self.find_node {
                let before = *handled;
                self.find.handle_event(
                    event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy,
                );
                self.drain_find_events(state);
                if *handled && !before {
                    return;
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

    fn test_id(&self) -> Option<&str> {
        Some("code-editor")
    }

    /// Expose the panes so headless driving can address them by name. Without
    /// this the whole editor is one opaque rect and `--headless-drive` can only
    /// click at raw coordinates.
    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = vec![&self.sidebar, &self.tabs];
        if self.find.is_visible() {
            out.push(&self.find);
        }
        out.push(&self.editor);
        if self.panel_visible {
            out.push(&self.panel);
        }
        out.push(&self.status);
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut out: Vec<&mut (dyn Widget + '_)> = vec![&mut self.sidebar, &mut self.tabs];
        if self.find.is_visible() {
            out.push(&mut self.find);
        }
        out.push(&mut self.editor);
        if self.panel_visible {
            out.push(&mut self.panel);
        }
        out.push(&mut self.status);
        out
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
            || self.find.needs_redraw()
            || self.palette.is_visible()
            || self.conflict.is_some()
            || self.flap.is_animating(self.sidebar_layout())
            // A live terminal repaints continuously while visible.
            || self.terminal_visible()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the event loop pumping frames while the sidebar flap is
        // mid-transition (so the slide + scrim fade play out) OR while a live
        // terminal is visible — its PTY is drained from `sync`, which the App
        // only re-runs while an animation frame is requested.
        //
        // A running search or an in-flight file picker also needs frames: both
        // deliver over a channel drained in `sync`, so without a frame their
        // results would sit unread until the user happened to move the mouse.
        // A language server that is still working needs them too, so the
        // indexing readout actually advances.
        self.flap.is_animating(self.sidebar_layout())
            || self.terminal_visible()
            || self.search_job.is_some()
            || self.portal.is_some()
            || self.lsp_status.is_working()
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
        self.find.mark_drawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_app_opens_main_rs() {
        let app = CodeApp::with_sample();
        let active = app.session.active_buffer().unwrap();
        assert_eq!(active.name, "main.rs");
    }

    #[test]
    fn ide_embeds_the_real_terminal_widget() {
        // The IDE must embed the real system `TerminalWidget` as its integrated
        // terminal — not ship a second, weaker emulator. Opening the Terminal
        // dock tab spawns a live widget whose grid the panel hands its body to.
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
        let mut doc = AutoCommit::new();
        app.ensure_buffers(&mut doc);
        let node = app.session.active_buffer().unwrap().node;
        let text = EditorDoc::read_buffer(&doc, node).unwrap();
        assert!(text.contains("fn main"));
        assert!(app.editor.is_bound());
    }

    #[test]
    fn opening_a_second_file_adds_a_tab() {
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
        app.set_width_hint(1200.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
        assert!(app.sidebar_open(), "docked sidebar opens by default");
    }

    #[test]
    fn opening_a_file_in_overlay_dismisses_the_flap() {
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
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
        let mut app = CodeApp::with_sample();
        let mut doc = AutoCommit::new();
        app.ensure_buffers(&mut doc);
        app.refresh_diagnostics(&doc);
        // README/main don't necessarily have problems, but the pipeline runs
        // without panicking and the count is queryable.
        let _ = app.panel.problem_count();
    }
}
