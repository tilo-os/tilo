//! Root widget for the code editor. Owns the [`Session`] (project + open
//! buffers), the editor pane, the status model and the bottom panel, and
//! coordinates the data flow between them and the file tree in the shell's
//! sidebar.
//!
//! Layout, in the shell's content column:
//! ```text
//! +-------------------------------+
//! |  editor view                  |
//! +-------------------------------+
//! |  bottom panel (problems/term) |
//! +-------------------------------+
//! |  status bar                   |
//! +-------------------------------+
//! ```
//! The caret and diagnostics readout is a real row: the status bar reserves its
//! band under the editor, so nothing is ever drawn over code. The shown file's
//! name, the bottom panel toggle, and find and replace in the buffer are the
//! window bar's ([`window_bar`], [`find_workflow`]). Searching the project and
//! running a command belong to the platform search, which this root opens with
//! Ctrl+P, Ctrl+Shift+P and Ctrl+Shift+F.
//!
//! ## The sidebar
//!
//! The file tree is the shell's sidebar ([`ui::widget::NavShell`]): docked
//! beside the editor when the window is wide, a drawer over it when it is
//! narrow, with its toggle in the window's bar. The editor shows the project
//! through an [`ExplorerFeed`] and reads the sidebar through its
//! [`ui::widget::WindowBarLink`].

mod continuity;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

mod chrome;
mod code_help;
mod dialogs;
mod disk_work;
mod file_workflow;
mod find_workflow;
mod format_workflow;
mod lsp_workflow;
mod surface;
#[cfg(test)]
mod tests;
mod window_bar;
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Widget, WindowBarLink};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::Key;

use crate::actions;
use crate::bottom_panel::{BottomPanel, HitRow, PanelTab, SearchState};
use crate::commands::Command;
use crate::disk;
use crate::doc::EditorDoc;
use crate::editor_view::{EditorView, LspAsk};
use crate::explorer::{ExplorerEvent, ExplorerEvents, ExplorerFeed};
use crate::lint;
use crate::model::{Language, NodeId as TreeId, NodeKind, Project, Session};
use crate::search;
use crate::status_bar::{StatusModel, StatusState};
use ui::time::Instant;

pub(crate) fn language_from_label(label: &str) -> Option<Language> {
    [
        Language::Rust,
        Language::Toml,
        Language::Markdown,
        Language::Json,
        Language::Yaml,
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Html,
        Language::Css,
        Language::Shell,
        Language::C,
        Language::Cpp,
        Language::Go,
        Language::Verilog,
        Language::SystemVerilog,
        Language::PlainText,
    ]
    .into_iter()
    .find(|language| language.label().eq_ignore_ascii_case(label))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiagnosticsRevision {
    heads: Vec<ChangeHash>,
    buffer: String,
    language: Language,
    scope: lint::Scope,
}

struct DiagnosticsCache {
    revision: DiagnosticsRevision,
    local: Vec<crate::bottom_panel::Diagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SymbolProjectionRevision {
    heads: Vec<ChangeHash>,
    /// Canonical resource identity and displayed path, sorted independently of
    /// the local project tree's allocation and iteration order.
    files: Vec<(String, String)>,
}

#[derive(Default)]
struct ReopenPhases {
    demand: u128,
    symbols: u128,
    search_entries: u128,
    diagnostics: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchProjectionRevision {
    /// Collaborative text and durable file metadata are authoritative in the
    /// CRDT, so its heads cheaply cover every remote/local document edit.
    document_heads: Vec<ChangeHash>,
    pending_ops: usize,
    /// Filesystem/project and streamed-result changes are local projections,
    /// not necessarily CRDT mutations.
    /// `None` means this local revision counter is exhausted. In that state we
    /// deliberately rebuild rather than reusing a cursor that could describe a
    /// different tree after a wrapped identity.
    local_generation: Option<u64>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SearchRefreshStats {
    rebuilds: usize,
    skipped: usize,
    buffer_reads: usize,
    indexed_records: usize,
}

/// The files an opener may show, and the ones asked for since the last
/// sync.
#[derive(Default)]
struct Opened {
    known: HashSet<String>,
    requests: Vec<String>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (panel visibility, one-time seed guard, terminal focus, liveness) on the app's top-level state, not a set of options that combine — a bitflag/enum would not read better"
)]
pub struct CodeApp {
    session: Session,
    /// What the shell's sidebar lists: the project and the file on screen.
    /// What the person does there lands in `explorer_events`.
    explorer: ExplorerFeed,
    explorer_events: ExplorerEvents,
    /// The shell around the editor: its sidebar, open or put away.
    bar: Option<WindowBarLink>,
    editor: EditorView,
    /// The caret / language / diagnostics readout, drawn in the status bar
    /// band under the editor.
    status: StatusModel,
    panel: BottomPanel,

    /// What a test runs with the document at the start of the next sync.
    #[cfg(test)]
    pub(crate) test_hook: Option<TestHook>,
    /// Where every edit of the project goes: the framework's door, which
    /// checks, applies, records and replicates it. `None` until the
    /// framework mounts the editor; an editor that is not mounted edits
    /// nothing.
    dispatch: Option<ui::framework::Dispatcher<crate::project::Project>>,
    /// The project as it was last presented, and the live handle that asks
    /// for the text of the file the window shows.
    presented: std::rc::Rc<crate::project::Project>,
    /// Files a search hit or an agent's `open` asked to show, taken up in
    /// the next sync, and the files it may name.
    opened: std::rc::Rc<std::cell::RefCell<Opened>>,
    live_state: Option<ui::framework::Live<crate::project::Project>>,
    /// The file ids whose text the window demanded last.
    demanded: Vec<String>,
    /// A command invoked through its published effect. An effect handler has
    /// no document, so the next sync runs the command with it.
    pending_command: Option<Command>,
    /// The file a New File command created, opened once its record arrives.
    pending_open: Option<String>,

    /// Last cursor position, since `MouseInput` carries no coordinates.
    last_cursor: Point,
    panel_visible: bool,

    rect: Rect,
    editor_node: Option<NodeId>,
    panel_node: Option<NodeId>,
    status_node: Option<NodeId>,
    status_rect: Rect,
    content_node: Option<NodeId>,
    editor_col_node: Option<NodeId>,

    /// The node currently bound into the editor view, so we only re-bind on
    /// change.
    bound_node: Option<TreeId>,
    /// One-time seed guard for the CRDT buffers.
    seeded: bool,
    /// The file whose disk text the document refused last, and when: the
    /// recording is tried again until the document holds the text.
    text_seed: Option<(String, std::time::Instant)>,

    /// The embedded system terminal, spawned lazily the first time the Terminal
    /// dock tab is shown. `None` until then (so `CodeApp::new` — used across the
    /// test suite — never spawns a shell) and if the shell fails to launch.
    ///
    /// This reuses `terminal::TerminalWidget` through a direct dependency on the
    /// `terminal` app crate. That is a stop-gap: an app depending on another app
    /// is the wrong shape, and the same widget should back both this panel and
    /// the standalone terminal. The right home is a shared terminal-widget crate
    /// that both binaries depend on; until the terminal is extracted there, this
    /// panel keeps embedding the widget from the app crate rather than shipping a
    /// second, weaker emulator.
    terminal: Option<ui::widget::Embedded<terminal::TerminalView>>,
    /// The command lines asked of the terminal's session, handed from the
    /// project's outbox to the terminal that types them.
    terminal_runs: terminal::runs::RunQueue,
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
    diagnostics_cache: Option<DiagnosticsCache>,
    /// At most two inactive exact-input lint snapshots and 16,384 records.
    inactive_diagnostics: Vec<DiagnosticsCache>,
    #[cfg(test)]
    diagnostics_buffer_reads: usize,
    /// What the server is currently busy with, if anything. Shown in the status
    /// bar so a multi-minute rust-analyzer index reads as "working", not
    /// "broken".
    lsp_status: LspStatus,
    /// Languages whose server failed to start, so we report the failure once
    /// and do not respawn a missing binary on every frame.
    lsp_failed: std::collections::HashSet<crate::model::Language>,
    /// Where language servers are looked for: the person's tool folders
    /// ([`tilo_process::tools::ToolDirs::current`]) unless set.
    lsp_tools: Option<tilo_process::tools::ToolDirs>,
    /// A language server is being launched on a background thread.
    lsp_launching: bool,

    /// The matches of the window search's query in the active buffer, and
    /// which of them is current ([`find_workflow`]).
    find_matches: Vec<crate::find::Match>,
    find_current: Option<usize>,

    /// The project text search the platform palette drives, and the latch that
    /// asks the platform to open its search.
    search_job: Option<search::Job>,
    search_hits: Vec<search::Hit>,
    search_truncated: bool,
    search_files_scanned: usize,
    last_search_needle: String,
    search_opener: ui::search::SearchOpener,
    /// Declaration index rebuilt from collaborative buffers during sync. It is
    /// presentation-only derived data; edits themselves remain native CRDT
    /// text splices.
    search_symbols: Vec<actions::SymbolRef>,
    /// The symbols and project-search results as search entries. They are
    /// updated from current collaborative state during `sync`; a search only
    /// reads them and never rebuilds files, symbols, result previews, or an
    /// LSP index.
    search_entries: Vec<ui::tilo_resource::SearchEntry>,
    /// Revision gate for the expensive complete projection above. An idle
    /// `sync` compares this tiny value and does no project/body traversal.
    search_projection_revision: Option<SearchProjectionRevision>,
    search_projection_local_generation: Option<u64>,
    /// Last actual phase costs, exposed only by the explicit native observer.
    reopen_phase_micros: ReopenPhases,
    symbol_projection_revision: Option<SymbolProjectionRevision>,
    projected_search_results: Vec<actions::SearchResultRef>,
    #[cfg(test)]
    search_refresh_stats: SearchRefreshStats,

    /// Whether the editor serves a window someone uses, and may start work
    /// that reaches outside the process (a language server).
    live: bool,
    /// The person's preferences, once the framework mounted the editor.
    settings: Option<ui::framework::Setting<crate::settings::CodeSettings>>,
    /// The chrome laid out last (dock shown, its tab, the find bar), so a
    /// change made outside an event (an effect, a staged scenario) rebuilds
    /// the layout before it is drawn.
    laid_out: (bool, PanelTab),
    structure_dirty: bool,
    /// A file picker in flight, and what the app asked for.
    portal: Option<ui::portal::PortalChannel>,
    portal_intent: Option<PortalIntent>,
    /// The answer owed to the effect that is opening or saving a file, given
    /// once the picker or the write settles.
    file_reply: Option<ui::framework::EffectReply>,
    /// Disk work under way where the window must not wait for it.
    disk_work: disk_work::DiskWork,
    /// Whether the shown file is being looked at for changes on disk.
    staleness_looking: bool,
    /// A save an effect asked for, written on the next sync, where the
    /// buffer's text is.
    pending_save: Option<(TreeId, Option<std::path::PathBuf>)>,
    /// The file the language server is formatting, and what follows once it
    /// answered.
    formatting: Option<format_workflow::PendingFormat>,
    /// The server's answer to `formatting`, applied on the next settlement.
    formatted: Option<Result<Vec<crate::lsp::TextEdit>, String>>,
    /// The screenshot scenario still being staged: the view state it shows is
    /// set once the file it is about is open and bound.
    staging: Option<Staging>,
    /// Until when a staged scenario waits for a live source (the terminal's
    /// shell) to draw.
    staging_until: Option<Instant>,

    /// A save that stopped because the file changed on disk. Holds the buffer
    /// it was for, so the user's choice applies to the right one.
    conflict: Option<Conflict>,
    /// A tab close waiting on the user's "save your changes?" answer.
    close_prompt: Option<ClosePrompt>,
    /// The sheet a question is asked in: it keeps where its answers landed.
    question_sheet: ui::widget::ModalSheet,
    code_help: code_help::CodeHelpReadout,

    /// A transient message about the last file operation (saved, failed,
    /// reloaded). Shown in the status bar; cleared by the next action.
    notice: Option<Notice>,
    /// A caret position to land on once the buffer it belongs to is bound —
    /// used by a search hit and a cross-file go-to-definition, both of which
    /// open a file that has no editor geometry until the next frame.
    pending_jump: Option<(usize, usize)>,
    pending_continuation: Option<ui::text_position::TextPosition>,
}

/// What a test runs with the editor and its document.
#[cfg(test)]
pub(crate) type TestHook = Box<dyn FnOnce(&mut CodeApp, &mut ui::widget::Edits<'_>)>;

/// A screenshot scenario being staged: `None` for a plain `--shots`.
#[derive(Clone, Debug)]
struct Staging {
    scenario: Option<String>,
}

/// What a pending portal pick is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// A tab close held back because the buffer has unsaved edits. Closing it would
/// silently discard them, so the user answers first.
#[derive(Clone, Debug)]
struct ClosePrompt {
    node: TreeId,
    name: String,
    /// The file to show once this one is saved or discarded.
    then_open: Option<TreeId>,
}

/// The three ways out of an unsaved-close prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseChoice {
    Save,
    /// Close the tab, discarding the unsaved edits.
    Discard,
    /// Keep the tab open.
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
    /// Not answering, and why.
    Failed(crate::lsp::LspFailure),
}

impl LspStatus {
    /// What the status bar says while the project is still being read, in
    /// the person's words: nothing once it is ready.
    pub fn label(&self) -> String {
        match self {
            LspStatus::None | LspStatus::Ready { .. } | LspStatus::Failed(_) => String::new(),
            LspStatus::Starting { .. }
            | LspStatus::Busy {
                percentage: None, ..
            } => "Loading project\u{2026}".to_owned(),
            LspStatus::Busy {
                percentage: Some(percentage),
                ..
            } => format!("Loading project {percentage}%"),
        }
    }

    /// Whether the server is working on something — the status bar spins its
    /// activity dot while this is true.
    pub fn is_working(&self) -> bool {
        matches!(self, LspStatus::Starting { .. } | LspStatus::Busy { .. })
    }

    /// What the Problems panel should title its zero-state with while the
    /// server is still working, or `None` when an empty list really is a
    /// finding.
    ///
    /// A cold `rust-analyzer` on a workspace this size spends minutes on
    /// `cargo metadata`, proc-macro loading and indexing before it says
    /// anything. During that window the editor looks exactly like an editor
    /// that has analysed the file and found it perfect. Naming the task is the
    /// difference between "this is working" and "this is broken", and it is the
    /// honest reading of an empty list.
    pub fn analysis_note(&self) -> Option<String> {
        let task = match self {
            LspStatus::Starting { .. } => "starting".to_string(),
            LspStatus::Busy {
                title,
                detail,
                percentage,
                ..
            } => {
                let mut s = title.to_lowercase();
                if let Some(d) = detail {
                    let _ = write!(s, " {d}");
                }
                if let Some(p) = percentage {
                    let _ = write!(s, " {p}%");
                }
                s
            }
            LspStatus::None | LspStatus::Ready { .. } | LspStatus::Failed(_) => return None,
        };
        let server = match self {
            LspStatus::Starting { server } | LspStatus::Busy { server, .. } => server.as_str(),
            _ => return None,
        };
        Some(format!("{server} is still {task}"))
    }
}

/// A transient message about a file operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub is_error: bool,
}

impl Default for CodeApp {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeApp {
    /// The editor with no project: what the app opens to when it was not given
    /// a path. There is nothing to show, and it says so rather than inventing a
    /// project that does not exist on this machine. Files made before a
    /// folder is opened belong to an untitled project, named as one.
    pub fn new() -> Self {
        Self::from_session(Session::new(Project::new("Untitled")), None)
    }

    /// The editor over the bundled sample project, held in memory. Shots and
    /// the demo write the sample to disk and open it like any folder; tests
    /// that only need a populated editor start here.
    #[cfg(test)]
    pub(crate) fn with_sample() -> Self {
        let project = crate::sample_project::build();
        let default = crate::sample_project::default_open(&project);
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

    #[allow(
        clippy::too_many_lines,
        reason = "one constructor listing every field of the editor once"
    )]
    fn from_session(session: Session, default: Option<TreeId>) -> Self {
        let explorer = ExplorerFeed::default();
        explorer.show(&session.project);
        explorer.set_current(default);

        CodeApp {
            session,
            explorer,
            explorer_events: ExplorerEvents::default(),
            bar: None,
            editor: EditorView::new(),
            status: StatusModel::new(),
            panel: BottomPanel::new(),
            #[cfg(test)]
            test_hook: None,
            dispatch: None,
            presented: std::rc::Rc::default(),
            opened: std::rc::Rc::default(),
            live_state: None,
            demanded: Vec::new(),
            pending_command: None,
            pending_open: None,
            last_cursor: Point::ZERO,
            panel_visible: false,
            rect: Rect::ZERO,
            editor_node: None,
            panel_node: None,
            status_node: None,
            status_rect: Rect::ZERO,
            content_node: None,
            editor_col_node: None,
            bound_node: default,
            seeded: false,
            text_seed: None,
            terminal: None,
            terminal_runs: terminal::runs::RunQueue::default(),
            terminal_focused: false,
            lsp: None,
            lsp_language: None,
            lsp_uri: None,
            lsp_version: 0,
            lsp_diagnostics: std::collections::HashMap::new(),
            lsp_sent_text: String::new(),
            diagnostics_cache: None,
            inactive_diagnostics: Vec::new(),
            #[cfg(test)]
            diagnostics_buffer_reads: 0,
            lsp_status: LspStatus::None,
            lsp_failed: std::collections::HashSet::new(),
            lsp_tools: None,
            lsp_launching: false,
            find_matches: Vec::new(),
            find_current: None,
            search_job: None,
            search_hits: Vec::new(),
            search_truncated: false,
            search_files_scanned: 0,
            last_search_needle: String::new(),
            search_opener: ui::search::SearchOpener::new(),
            search_symbols: Vec::new(),
            search_entries: Vec::new(),
            reopen_phase_micros: ReopenPhases::default(),
            symbol_projection_revision: None,
            projected_search_results: Vec::new(),
            search_projection_revision: None,
            search_projection_local_generation: Some(0),
            #[cfg(test)]
            search_refresh_stats: SearchRefreshStats::default(),
            portal: None,
            portal_intent: None,
            live: false,
            settings: None,
            laid_out: (false, PanelTab::Problems),
            structure_dirty: false,
            file_reply: None,
            disk_work: disk_work::DiskWork::default(),
            staleness_looking: false,
            pending_save: None,
            formatting: None,
            formatted: None,
            staging: None,
            staging_until: None,
            conflict: None,
            close_prompt: None,
            question_sheet: ui::widget::ModalSheet::new(),
            code_help: code_help::CodeHelpReadout::default(),
            notice: None,
            pending_jump: None,
            pending_continuation: None,
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

    /// Stand in the shell: list the project in its sidebar through
    /// `explorer`, hear the tree through `events`, and read the sidebar
    /// through `bar`.
    #[must_use]
    pub(crate) fn in_shell(
        mut self,
        explorer: &ExplorerFeed,
        events: &ExplorerEvents,
        bar: WindowBarLink,
    ) -> Self {
        explorer.show(&self.session.project);
        explorer.set_current(self.session.active_buffer().map(|buffer| buffer.node));
        self.explorer = explorer.clone();
        self.explorer_events = std::rc::Rc::clone(events);
        self.bar = Some(bar);
        self
    }

    /// Show or hide the sidebar (Ctrl+B, the palette command), as the
    /// window bar's toggle does.
    fn toggle_sidebar(&self) {
        if let Some(bar) = &self.bar {
            bar.event(&ui::window_toolbar::ToolbarEvent::ToggleSidebar);
        }
    }

    pub fn shots_snap_sidebar(&mut self, open: bool) {
        if let Some(bar) = &self.bar {
            bar.stage_sidebar(open);
        }
    }

    fn put_drawer_away(&self) {
        if let Some(bar) = &self.bar {
            bar.put_drawer_away();
        }
    }

    /// Apply `action` to `doc` now, through the framework's door: within the
    /// event, so the editor sees its own edit before the next keystroke.
    fn submit_now(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        action: &actions::Action,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.dispatch {
            Some(dispatch) => dispatch.apply_now(doc, action),
            None => Err(tilo_actions::ActionError::Refused(
                "the editor is not open on a project".into(),
            )),
        }
    }

    /// Keep the open buffers' saved-on-disk comparison current, and bind the
    /// editor to the active buffer's text. Reads only: a buffer's text is
    /// seeded when its file is opened ([`Self::open_node`]).
    fn ensure_buffers(&mut self, doc: &AutoCommit) {
        // A New File command opens the file it created once its record is in
        // the project.
        if let Some(id) = self.pending_open.clone()
            && let Some(node) = self.session.project.node_for_resource(&id)
        {
            self.pending_open = None;
            self.open_node(node);
        }
        if let Some(buffer) = self.session.buffer.as_mut() {
            // A buffer with no file on disk (a scratch file, a collaborator's
            // file) is saved when its text is first here: an untouched one is
            // clean and closes without a prompt.
            if buffer.stamp.is_none() && buffer.path.is_none() && buffer.saved_text.is_empty() {
                let key = self
                    .session
                    .project
                    .get(buffer.node)
                    .map(|node| node.resource_id.clone());
                if let Some(text) = key.and_then(|key| EditorDoc::read_buffer_key(doc, &key)) {
                    buffer.mark_saved(text, None);
                }
            }
        }
        self.seeded = true;

        // Dirty is a comparison against the bytes on disk, recomputed here, so
        // undoing back to the original clears the indicator rather than leaving
        // it stuck on.
        if let Some(node) = self.session.active_buffer().map(|b| b.node) {
            let key = self.buffer_key(node);
            if let Some(current) = self.presented.text(&key)
                && let Some(buf) = self.session.buffer.as_mut()
            {
                buf.dirty = buf.is_dirty(current);
            }
        }

        // Bind the editor to the active buffer.
        let active = self.session.active_buffer().map(|b| (b.node, b.language));
        match active {
            Some((node, lang)) => {
                if (self.bound_node != Some(node) || !self.editor.is_bound())
                    && let Some(text) = EditorDoc::buffer_text_key(doc, &self.buffer_key(node))
                {
                    self.editor.bind(text, lang);
                    self.bound_node = Some(node);
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

    /// Record the disk text of the file the window shows when the document
    /// does not hold it yet: the first time the file is opened in the
    /// project, once its entry is recorded. Applied in the settlement as
    /// the person's own edit, like their typing. Opening used to queue this
    /// recording from the open itself, and a launch that hands the editor its
    /// file queued it before the record would take it, which left the file
    /// open over an empty "No file open" editor with nothing to select or
    /// edit. Text already there — a collaborator's or an earlier session's
    /// unsaved edits — is kept. A refused recording is tried again at most
    /// once a second.
    fn record_missing_text(&mut self, doc: &mut ui::widget::Edits<'_>) {
        const RETRY: std::time::Duration = std::time::Duration::from_secs(1);
        let Some(buffer) = self.session.active_buffer() else {
            return;
        };
        if buffer.stamp.is_none() || buffer.path.is_none() {
            return;
        }
        let key = self.buffer_key(buffer.node);
        if EditorDoc::buffer_text_key(doc, &key).is_some()
            || self
                .live_state
                .as_ref()
                .is_some_and(|live| live.with(|state| state.buffers.contains_key(&key)))
        {
            return;
        }
        // The file's own entry is recorded first (`record_disk_entries`):
        // text for a file the project does not hold yet is not taken.
        if EditorDoc::file(doc, &key).is_none() {
            return;
        }
        let now = std::time::Instant::now();
        if self
            .text_seed
            .as_ref()
            .is_some_and(|(refused, at)| *refused == key && now.duration_since(*at) < RETRY)
        {
            return;
        }
        let text = buffer.saved_text.clone();
        if let Err(error) = self.submit_now(doc, &actions::record_text_action(key.clone(), text)) {
            tracing::warn!(%error, file = %key, "the shown file's text was not recorded");
            self.text_seed = Some((key, now));
        } else {
            self.text_seed = None;
        }
    }

    /// Ask for the text of the file the window shows, and only that.
    fn demand_open_buffers(&mut self, doc: &AutoCommit) {
        let open: Vec<String> = self
            .session
            .buffer
            .iter()
            .map(|buffer| self.buffer_key(buffer.node))
            .collect();
        if open == self.demanded {
            return;
        }
        let started = std::time::Instant::now();
        self.demanded.clone_from(&open);
        if let Some(live) = &self.live_state
            && let Err(error) = live.demand(doc, ui::tilo_resource::Demand::none().with_keys(open))
        {
            tracing::warn!(%error, "open files not read");
        }
        self.reopen_phase_micros.demand = started.elapsed().as_micros();
    }

    /// Record the files and folders the editor read from disk since it last
    /// looked — the folder it opened, a folder expanded, the way to a search
    /// hit — so each is a file of the project its collaborators, the palette
    /// and agents address. One step of content from outside, nobody's undo.
    fn record_disk_entries(&mut self) {
        let fresh = self.session.project.take_read_from_disk();
        if fresh.is_empty() || self.dispatch.is_none() {
            return;
        }
        let entries: Vec<actions::Action> = fresh
            .into_iter()
            .filter_map(|id| {
                let node = self.session.project.get(id)?;
                if self
                    .presented
                    .files
                    .contains(&ui::tilo_resource::Id::from(node.resource_id.as_str()))
                {
                    return None;
                }
                let parent = node.parent.and_then(|parent| {
                    self.session
                        .project
                        .get(parent)
                        .map(|parent| parent.resource_id.clone())
                });
                let (kind, language) = match &node.kind {
                    NodeKind::File { language, .. } => (
                        crate::project::EntryKind::File,
                        language.label().to_string(),
                    ),
                    NodeKind::Folder { .. } => (crate::project::EntryKind::Folder, String::new()),
                };
                Some(actions::record_entry_action(
                    &node.resource_id,
                    kind,
                    node.name.clone(),
                    parent.as_deref(),
                    language,
                ))
            })
            .collect();
        if let Some(dispatch) = &self.dispatch {
            dispatch.group(|| {
                for entry in &entries {
                    dispatch.apply(entry);
                }
            });
        }
    }

    /// Project durable file records into the local tree. An empty list does not
    /// wipe a project that has not been published yet.
    /// Adopt the project as last presented again: after a folder or file
    /// was opened, its tree meets the records the project already holds.
    fn adopt_presented(&mut self) {
        let state = std::rc::Rc::clone(&self.presented);
        self.adopt_resource_state(&state);
    }

    pub(crate) fn adopt_resource_state(&mut self, state: &std::rc::Rc<crate::project::Project>) {
        let previous = std::mem::replace(&mut self.presented, std::rc::Rc::clone(state));
        let mut files = crate::doc::CollaborativeFile::all(state);
        self.opened.borrow_mut().known = files.iter().map(|file| file.id.clone()).collect();
        // A file a collaborator deleted leaves the tree too.
        files.extend(
            crate::doc::CollaborativeFile::all(&previous)
                .into_iter()
                .filter(|file| {
                    !state
                        .files
                        .contains(&ui::tilo_resource::Id::from(file.id.as_str()))
                })
                .map(|file| crate::doc::CollaborativeFile {
                    deleted: true,
                    ..file
                }),
        );
        if files.is_empty() {
            return;
        }
        files.sort_by_key(|file| file.path.matches('/').count());
        self.project_files(files);
    }

    /// The queue the terminal takes the project's asked command lines from.
    pub fn terminal_runs(&self) -> terminal::runs::RunQueue {
        self.terminal_runs.clone()
    }

    /// What shows file `id` when a search hit or an agent's `open` names
    /// it; a file the project does not hold is refused.
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&ui::tilo_resource::Id) -> Result<(), ui::tilo_resource::Refusal> + 'static
    {
        let opened = std::rc::Rc::clone(&self.opened);
        move |id| {
            let mut opened = opened.borrow_mut();
            if !opened.known.contains(id.as_str()) {
                return Err(ui::tilo_resource::Refusal::new(
                    "the project has no such file",
                ));
            }
            opened.requests.push(id.to_string());
            Ok(())
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::project::Project>,
        live: bool,
        settings: ui::framework::Setting<crate::settings::CodeSettings>,
        state: ui::framework::Live<crate::project::Project>,
    ) {
        self.dispatch = Some(dispatcher);
        self.live_state = Some(state);
        self.live = live;
        self.editor.set_text_zoom(settings.get().editor_zoom);
        self.settings = Some(settings);
    }

    /// Change the editor's text size one step, store it for this person, and
    /// answer the factor the code now shows. The window bar says the level
    /// through the host's content-zoom indicator, as for any content that
    /// zooms itself.
    pub(crate) fn zoom_editor_text(&mut self, step: ui::content_zoom::ZoomStep) -> f32 {
        let factor = self.editor.text_zoom();
        let next = step.apply(factor);
        self.editor.set_text_zoom(next);
        if let Some(settings) = &self.settings {
            let _ = settings.update(|stored| stored.editor_zoom = self.editor.text_zoom());
        }
        self.editor.text_zoom()
    }

    pub(crate) fn set_highlight_waker(&mut self, wake: ui::WakeHandle) {
        self.editor.set_highlight_waker(wake);
    }

    fn project_files(&mut self, files: Vec<crate::doc::CollaborativeFile>) {
        let ordering: Vec<_> = files
            .iter()
            .filter(|file| !file.deleted)
            .map(|file| (file.id.clone(), file.order))
            .collect();
        for file in files {
            if file.deleted {
                if let Some(node) = self.session.project.remove_collaborative_file(&file.id) {
                    self.session.close_file(node);
                }
                continue;
            }
            if file.kind == "folder" {
                self.session.project.adopt_collaborative_folder(
                    &file.id,
                    file.parent.as_deref(),
                    &file.name,
                );
            } else {
                // An explicitly selected language is durable metadata and wins
                // over extension inference; the extension is only a fallback
                // for legacy imported records.
                let language = language_from_label(&file.language)
                    .unwrap_or_else(|| Language::from_path(&file.name));
                self.session.project.adopt_collaborative_file(
                    &file.id,
                    file.parent.as_deref(),
                    &file.name,
                    language,
                );
            }
        }
        self.session.project.order_collaborative_children(&ordering);
        self.explorer.show(&self.session.project);
    }

    fn buffer_key(&self, node: TreeId) -> String {
        self.session.project.get(node).map_or_else(
            || format!("legacy-node-{node}"),
            |file| file.resource_id.clone(),
        )
    }

    /// The only UI-to-document route for editor text. `EditorView` merely
    /// prepares a before/after request; this boundary derives the compact
    /// character splice, dispatches the public action, and reconciles the
    /// optimistic paint cache if validation refuses it.
    fn dispatch_editor_text_edits(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(edit) = self.editor.take_text_edit() else {
            return;
        };
        let Some(buffer) = self.session.active_buffer() else {
            self.editor.reconcile_from_doc(doc);
            self.note("Text edit was refused: no active file", true);
            return;
        };
        let file_id = self.buffer_key(buffer.node);
        if let Err(error) = self.dispatch_text_action(doc, file_id, &edit.base, &edit.next) {
            self.editor.reconcile_from_doc(doc);
            self.note(format!("Text edit was refused: {error}"), true);
        }
    }

    fn dispatch_text_action(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        file_id: String,
        base: &str,
        next: &str,
    ) -> Result<(), tilo_actions::ActionError> {
        self.submit_now(doc, &actions::text_edit_action(file_id, base, next))
    }

    /// Recompute diagnostics over the active buffer and push to the panel +
    /// status bar.
    ///
    /// Two sources feed the Problems panel, and which parts of the local linter
    /// run depends on whether the language server has answered yet. Before it
    /// has — no server for this language, or one still indexing — the linter is
    /// the only feedback there is, so it runs in full. Once the server has
    /// published for this buffer, its structural scan steps aside
    /// ([`lint::Scope::TextualOnly`]): a bracket-counter with no type
    /// information cannot usefully disagree with a compiler, and when it does
    /// the user is the one who has to work out which to believe.
    fn refresh_diagnostics(&mut self, doc: &AutoCommit) {
        use automerge::transaction::Transactable;
        // Heads identify committed CRDT content, including remote/history changes.
        // Even zero pending operations may hide an open empty transaction.
        // Read the committed witness without cloning or closing the caller's
        // transaction or changing its patch/history cursors. Pending edits bypass
        // caching; an equal pending-operation count is not a content version.
        let heads = (doc.pending_ops() == 0).then(|| doc.committed_heads());
        self.refresh_diagnostics_at(doc, heads);
    }

    fn retain_diagnostics(&mut self, cache: DiagnosticsCache) {
        const RECORD_BUDGET: usize = 16_384;
        if cache.local.len() > RECORD_BUDGET {
            return;
        }
        self.inactive_diagnostics
            .retain(|old| old.revision != cache.revision);
        self.inactive_diagnostics.push(cache);
        while self.inactive_diagnostics.len() > 2
            || self
                .inactive_diagnostics
                .iter()
                .map(|cache| cache.local.len())
                .sum::<usize>()
                > RECORD_BUDGET
        {
            self.inactive_diagnostics.remove(0);
        }
    }

    fn restore_diagnostics(&mut self, revision: Option<&DiagnosticsRevision>) {
        let Some(revision) = revision else {
            // Pending observations remain completely uncached.
            self.inactive_diagnostics.clear();
            self.diagnostics_cache = None;
            return;
        };
        if self
            .diagnostics_cache
            .as_ref()
            .is_some_and(|cache| &cache.revision == revision)
        {
            return;
        }
        if let Some(index) = self
            .inactive_diagnostics
            .iter()
            .position(|cache| &cache.revision == revision)
        {
            let restored = self.inactive_diagnostics.remove(index);
            if let Some(previous) = self.diagnostics_cache.replace(restored) {
                self.retain_diagnostics(previous);
            }
        }
    }

    fn refresh_diagnostics_at(&mut self, doc: &AutoCommit, heads: Option<Vec<ChangeHash>>) {
        let lsp_for_buffer = self
            .lsp_uri
            .as_ref()
            .is_some_and(|uri| self.lsp_diagnostics.contains_key(uri));
        // An empty publish still counts as an answer: it is the server saying
        // "this file is clean", which is exactly when a stale bracket warning
        // would be most confusing.
        let scope = if lsp_for_buffer {
            lint::Scope::TextualOnly
        } else {
            lint::Scope::Full
        };

        let active = self
            .session
            .active_buffer()
            .map(|buf| (self.buffer_key(buf.node), buf.language));
        let revision = heads.and_then(|heads| {
            active
                .as_ref()
                .map(|(buffer, language)| DiagnosticsRevision {
                    heads,
                    buffer: buffer.clone(),
                    language: *language,
                    scope,
                })
        });
        self.restore_diagnostics(revision.as_ref());
        let cached = revision.as_ref().and_then(|revision| {
            self.diagnostics_cache
                .as_ref()
                .filter(|cache| &cache.revision == revision)
        });
        let mut diagnostics = if let Some(cache) = cached {
            self.reopen_phase_micros.diagnostics = 0;
            cache.local.clone()
        } else {
            let started = std::time::Instant::now();
            let local = match active {
                Some((key, language)) => {
                    #[cfg(test)]
                    {
                        self.diagnostics_buffer_reads += 1;
                    }
                    // The editor's optimistic text is not authoritative here.
                    let text = EditorDoc::read_buffer_key(doc, &key).unwrap_or_default();
                    lint::lint(&text, language, scope)
                }
                None => Vec::new(),
            };
            self.reopen_phase_micros.diagnostics = started.elapsed().as_micros();
            if let Some(previous) = self.diagnostics_cache.take() {
                self.retain_diagnostics(previous);
            }
            self.diagnostics_cache = revision.map(|revision| DiagnosticsCache {
                revision,
                local: local.clone(),
            });
            local
        };
        if let Some(lsp) = self
            .lsp_uri
            .as_ref()
            .and_then(|uri| self.lsp_diagnostics.get(uri))
        {
            diagnostics.extend(lsp.iter().map(super::lsp::LspDiagnostic::to_panel));
        }
        diagnostics.sort_by_key(|a| (a.line, a.col));
        self.panel.set_problems(diagnostics);
        self.panel.set_analysis(self.lsp_status.analysis_note());
    }

    /// Rebuild the status bar snapshot.
    fn refresh_status(&mut self) {
        let (line, col) = self.editor.primary_caret();
        let lang = if self.editor.is_bound() {
            self.editor.language().label().to_string()
        } else {
            String::new()
        };
        let notice = self.notice.clone().unwrap_or(Notice {
            text: String::new(),
            is_error: false,
        });
        // With no buffer open there is no caret to report.
        let (line, col) = if self.editor.is_bound() {
            (line + 1, col + 1)
        } else {
            (0, 0)
        };
        self.status.set_state(StatusState {
            language: lang,
            line,
            col,
            selection: self.editor.selection_len(),
            caret_count: self.editor.caret_count(),
            problems: self.panel.problem_count(),
            lsp: self.lsp_status.label(),
            lsp_working: self.lsp_status.is_working(),
            notice: notice.text,
            dirty: self.session.active_buffer().is_some_and(|b| b.dirty),
            code_help: self.code_help_readout(),
        });
    }

    /// The presentation context the Ctrl+K resource tree is built from: the
    /// project name, the UUID the next created file will take, and every
    /// materialised project file/folder. Everything here mirrors state the app
    /// already holds; local arena IDs are never exported as identities.
    fn search_file_entries(&self) -> Vec<crate::search_entries::FileEntry> {
        self.session
            .project
            .nodes
            .iter()
            .filter(|(_, item)| matches!(item.kind, NodeKind::File { .. }))
            .map(|(node, item)| crate::search_entries::FileEntry {
                node: *node,
                resource_id: item.resource_id.clone(),
                name: item.name.clone(),
                path: self.session.project.path_of(*node),
            })
            .collect()
    }

    /// The text hits the platform search currently lists, as the resources
    /// that reveal them.
    fn search_result_refs(&self) -> Vec<actions::SearchResultRef> {
        self.search_hits
            .iter()
            .map(|hit| actions::SearchResultRef {
                file_id: self.session.project.nodes.iter().find_map(|(node, item)| {
                    (self.session.project.disk_path_of(*node).as_deref()
                        == Some(hit.path.as_path())
                        || self.session.project.path_of(*node) == hit.rel)
                        .then_some(item.resource_id.clone())
                }),
                path: hit.rel.clone(),
                disk_path: Some(hit.path.display().to_string()),
                preview: hit.preview.clone(),
                line: hit.line.saturating_sub(1),
                col: hit.col,
            })
            .collect()
    }

    fn refresh_search_symbols(&mut self, doc: &AutoCommit) -> bool {
        use automerge::transaction::Transactable;
        let mut files = self.search_file_entries();
        files.sort_by(|a, b| a.resource_id.cmp(&b.resource_id).then(a.path.cmp(&b.path)));
        // Pending transaction counts do not identify their content. Rebuild
        // for every pending projection, including equal-count unrelated forks.
        let revision = (doc.pending_ops() == 0).then(|| SymbolProjectionRevision {
            heads: doc.committed_heads(),
            files: files
                .iter()
                .map(|file| (file.resource_id.clone(), file.path.clone()))
                .collect(),
        });
        if revision.is_some() && revision == self.symbol_projection_revision {
            self.reopen_phase_micros.symbols = 0;
            return false;
        }
        let started = std::time::Instant::now();
        #[cfg(test)]
        {
            self.search_refresh_stats.buffer_reads = self
                .search_refresh_stats
                .buffer_reads
                .saturating_add(files.len());
        }
        self.search_symbols = files
            .into_iter()
            .flat_map(|file| {
                EditorDoc::read_buffer_key(doc, &self.buffer_key(file.node))
                    .map_or_else(Vec::new, |text| {
                        crate::search_entries::symbols_in(&file.resource_id, &file.path, &text)
                    })
            })
            .collect();
        self.symbol_projection_revision = revision;
        self.reopen_phase_micros.symbols = started.elapsed().as_micros();
        true
    }

    fn invalidate_search_projection(&mut self) {
        self.search_projection_local_generation = self
            .search_projection_local_generation
            .and_then(|generation| generation.checked_add(1));
    }

    /// Refresh the durable search projection only when the authoritative CRDT
    /// document or a local project/result generation changed. This is
    /// intentionally separate from `search_providers`: opening the palette,
    /// typing, and unchanged frames do no filesystem, document, or LSP work.
    fn refresh_search_entries(&mut self, doc: &AutoCommit) {
        let revision = SearchProjectionRevision {
            document_heads: doc.committed_heads(),
            pending_ops: doc.pending_ops(),
            local_generation: self.search_projection_local_generation,
        };
        // Once the local identity space is exhausted there can be no safe
        // equality shortcut: reusing the old projection would let an obsolete
        // action directory masquerade as the current project.
        if revision.pending_ops == 0
            && revision.local_generation.is_some()
            && self.search_projection_revision.as_ref() == Some(&revision)
        {
            #[cfg(test)]
            {
                self.search_refresh_stats.skipped =
                    self.search_refresh_stats.skipped.saturating_add(1);
            }
            return;
        }
        let changed = self.refresh_search_symbols(doc);
        let started = std::time::Instant::now();
        let results = self.search_result_refs();
        if changed
            || results != self.projected_search_results
            || revision.local_generation.is_none()
        {
            self.search_entries =
                crate::search_entries::code_entries(&self.search_symbols, &results);
            self.projected_search_results = results;
        }
        self.reopen_phase_micros.search_entries = started.elapsed().as_micros();
        self.search_projection_revision = Some(revision);
        #[cfg(test)]
        {
            self.search_refresh_stats.rebuilds =
                self.search_refresh_stats.rebuilds.saturating_add(1);
            self.search_refresh_stats.indexed_records = self
                .search_refresh_stats
                .indexed_records
                .saturating_add(self.search_entries.len());
        }
    }

    /// The symbols and project-search results as search entries.
    pub(crate) fn code_search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.search_entries.clone()
    }

    #[cfg(test)]
    fn search_refresh_stats(&self) -> SearchRefreshStats {
        self.search_refresh_stats
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

    /// Show the file `node` in this window and bind the editor. A shown file
    /// with unsaved edits is not dropped: the person decides first, and the
    /// new file opens once it was saved or discarded.
    fn open_node(&mut self, node: TreeId) {
        if let Some(shown) = self.session.active_buffer()
            && shown.node != node
            && shown.dirty
        {
            self.close_prompt = Some(ClosePrompt {
                node: shown.node,
                name: shown.name.clone(),
                then_open: Some(node),
            });
            return;
        }
        self.show_node(node);
    }

    fn show_node(&mut self, node: TreeId) {
        if !self.session.open_file(node) {
            return;
        }
        self.seed_buffer(node);
        self.invalidate_search_projection();
        self.explorer.set_current(Some(node));
        self.bound_node = None; // force re-bind in ensure_buffers
    }

    /// The node of the file in the active tab, if any. Lets a test observe
    /// where a Ctrl+K activation navigated.
    pub fn active_file_node(&self) -> Option<crate::model::NodeId> {
        self.session.active_buffer().map(|b| b.node)
    }

    /// Read access to the editing session — the open buffers + project tree a
    /// search provider snapshots. Exposed for tests.
    pub fn session_for_test(&self) -> &crate::model::Session {
        &self.session
    }

    /// Apply an explorer signal: activating a file opens it, activating a
    /// folder toggles it, and an expansion reads and records the folder.
    fn apply_tree_event(&mut self, ev: ExplorerEvent) {
        match ev {
            ExplorerEvent::Activate(id) => {
                if self
                    .session
                    .project
                    .get(id)
                    .is_some_and(crate::model::TreeNode::is_folder)
                {
                    let open = !self.session.project.get(id).is_some_and(|node| {
                        matches!(node.kind, NodeKind::Folder { expanded: true, .. })
                    });
                    self.apply_tree_event(ExplorerEvent::Expand(id, open));
                    return;
                }
                self.open_node(id);
                // The file lands on screen: a drawer over it goes away, a
                // docked sidebar stays.
                self.put_drawer_away();
            }
            ExplorerEvent::Expand(id, open) => {
                // Reading the folder's children happens here, on first
                // expansion, rather than when the project was opened — one
                // directory level at a time is what makes opening a large
                // repository instant.
                if open && let Some(folder) = self.session.project.folder_to_read(id) {
                    self.on_disk(
                        "code-read-folder",
                        move || crate::model::read_folder(&folder),
                        move |app, listing| app.read_folder(id, listing),
                    );
                }
                self.session.project.set_folder_expanded(id, open);
                self.invalidate_search_projection();
                self.explorer.show(&self.session.project);
            }
            ExplorerEvent::OpenFolder => self.request_open_folder(),
        }
    }

    /// Apply every explorer signal queued since the last frame, including
    /// the expansions a folder activation echoes back.
    fn drain_explorer_events(&mut self) {
        loop {
            let events = std::mem::take(&mut *self.explorer_events.borrow_mut());
            if events.is_empty() {
                break;
            }
            for ev in events {
                self.apply_tree_event(ev);
            }
        }
    }

    /// Stop showing `node` once that is decided — already clean, a discarded
    /// prompt, or a save that landed — and show the file the person asked
    /// for next, if any.
    fn close_node(&mut self, node: TreeId, then_open: Option<TreeId>) {
        if self.session.buffer_of(node).is_some() {
            self.session.close_file(node);
            self.invalidate_search_projection();
            self.bound_node = None;
            self.explorer.set_current(None);
        }
        if let Some(next) = then_open {
            self.show_node(next);
        }
    }

    /// Answer an unsaved-close prompt.
    fn resolve_close_prompt(&mut self, doc: &AutoCommit, choice: CloseChoice) {
        let Some(prompt) = self.close_prompt.take() else {
            return;
        };
        match choice {
            CloseChoice::Save => {
                let has_path = self
                    .session
                    .buffer_of(prompt.node)
                    .is_some_and(|b| b.path.is_some());
                if has_path {
                    // Close only once the save landed. A
                    // save that hits a disk conflict leaves the tab open so the
                    // conflict prompt (now showing) gets answered first.
                    let snapshot = doc.clone();
                    self.save_buffer_then(
                        &snapshot,
                        prompt.node,
                        false,
                        file_workflow::AfterSave::Close(prompt.then_open),
                    );
                } else {
                    // No file yet: the destination has to be chosen first. The
                    // Save As runs async through the portal, so the tab stays
                    // open — once written, it is clean and closes without a
                    // prompt.
                    self.request_save_as(prompt.node);
                }
            }
            CloseChoice::Discard => self.close_node(prompt.node, prompt.then_open),
            CloseChoice::Cancel => {}
        }
    }

    /// Run a command. `doc` is threaded in because the one doc-mutating
    /// command (New file) submits the project's `new-file` verb through the action
    /// host; the rest are pure presentation.
    ///
    fn apply_command(&mut self, cmd: Command, doc: &mut ui::widget::Edits<'_>) {
        match cmd {
            Command::NewFile => {
                // An untitled file at the project root, created through the
                // project's own action and opened once it is there.
                let name = format!("untitled-{}.txt", self.session.project.nodes.len());
                let before = self.presented.files.ids().cloned().collect::<HashSet<_>>();
                let action = actions::new_file_action(name.clone(), None, "");
                match self.submit_now(doc, &action) {
                    Ok(()) => {
                        let created =
                            <crate::project::Project as ui::tilo_resource::Resource>::read(doc)
                                .ok()
                                .and_then(|project| {
                                    project
                                        .files
                                        .ids()
                                        .find(|id| !before.contains(*id))
                                        .map(ToString::to_string)
                                });
                        self.pending_open = created;
                    }
                    Err(error) => self.note(format!("Could not create {name}: {error}"), true),
                }
            }
            Command::OpenFolder => self.request_open_folder(),
            Command::OpenFile => self.request_open_file(),
            Command::Save => {
                if let Some(node) = self.session.active_buffer().map(|b| b.node)
                    && let Some(after) =
                        self.format_before_saving(doc, node, format_workflow::AfterFormat::Save)
                {
                    self.after_format(doc, node, after);
                }
            }
            Command::FormatFile => {
                if let Some(node) = self.session.active_buffer().map(|b| b.node)
                    && !self.request_format(doc, node, format_workflow::AfterFormat::Nothing)
                {
                    self.note("No formatter is running for this file", true);
                }
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
            Command::NextTheme => self.note(
                "Choose appearance in the operating system's settings",
                false,
            ),
            Command::GoToSearch => {
                self.panel_visible = true;
                self.panel.set_tab(PanelTab::Search);
            }
            Command::Find => Self::open_find(ui::window_toolbar::SearchOpening::Find),
            Command::Replace => Self::open_find(ui::window_toolbar::SearchOpening::Replace),
            Command::SearchProject => {
                // The platform search, seeded from the selection the way
                // every editor does.
                let seed = self
                    .editor
                    .selected_text_public()
                    .filter(|s| !s.is_empty() && !s.contains('\n'))
                    .unwrap_or_default();
                self.search_opener
                    .open(ui::search::SearchRequest::this_app().with_query(seed));
            }
            Command::GoToLine => Self::ask_for_line(),
            Command::DuplicateLine => self.editor.duplicate_lines(doc, true),
            Command::MoveLineUp => self.editor.move_lines(doc, true),
            Command::MoveLineDown => self.editor.move_lines(doc, false),
            Command::DeleteLine => self.editor.delete_lines(doc),
            Command::ToggleComment => self.editor.toggle_line_comment(doc),
            Command::ToggleBottomPanel => self.toggle_bottom_panel(),
            Command::ToggleSidebar => {
                self.toggle_sidebar();
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
        self.dispatch_editor_text_edits(doc);
    }

    /// Show the bottom panel, or close it (Ctrl+J, the window bar's toggle).
    fn toggle_bottom_panel(&mut self) {
        if self.panel_visible {
            self.close_bottom_panel();
        } else {
            self.panel_visible = true;
        }
    }

    /// Close the retained dock without destroying its selected tab or live
    /// Terminal. Every close route uses this transition so hidden Terminal
    /// content cannot retain keyboard ownership.
    fn close_bottom_panel(&mut self) {
        self.panel_visible = false;
        self.terminal_focused = false;
    }

    /// Drain everything the child widgets emitted this frame. `doc` is threaded
    /// through so a chosen palette command that mutates the document (New file)
    /// can dispatch its action.
    fn drain_child_events(&mut self) {
        self.run_window_toolbar();
        self.drain_explorer_events();
        // Closing a tab of the dock closes the dock; choosing the Terminal
        // tab starts the shell it shows.
        if self.panel.take_close_request() {
            self.close_bottom_panel();
            self.panel.reset_tabs();
        }
        if self.panel.take_tab_changed() && self.panel.active_tab() == PanelTab::Terminal {
            self.ensure_terminal();
        }
        if let Some(index) = self.panel.take_search_jump()
            && let Some(hit) = self.search_hits.get(index).cloned()
        {
            self.reveal_on_disk(hit.path.clone(), move |app, node| {
                if let Some(node) = node {
                    app.explorer.show(&app.session.project);
                    app.open_node(node);
                    app.pending_jump = Some((hit.line.saturating_sub(1), hit.col));
                }
            });
        }
        // Choosing a problem takes the caret to where it was reported.
        if let Some((line, col)) = self.panel.take_jump()
            && self.editor.is_bound()
        {
            self.editor
                .move_caret_to(line.saturating_sub(1), col.saturating_sub(1));
        }
    }

    /// Move the caret to a 1-based line number (the go-to-line target). Clamped
    /// to the buffer; a no-op with a clear note when nothing is open.
    pub fn go_to_line(&mut self, line_1based: usize) {
        if !self.editor.is_bound() {
            self.note("Open a file to jump to a line", true);
            return;
        }
        let target = line_1based
            .saturating_sub(1)
            .min(self.editor.line_count().saturating_sub(1));
        self.editor.move_caret_to(target, 0);
    }

    /// Spawn the embedded system terminal on first use. Idempotent. On success
    /// the panel hands the Terminal tab body to the live widget; on failure it
    /// shows a clear note instead of the static log.
    fn ensure_terminal(&mut self) {
        if self.terminal.is_some() {
            return;
        }
        match terminal::TerminalView::spawn_default() {
            Ok(mut w) => {
                // The session is the project's terminal child: what it shows
                // is recorded through the project's own dispatcher.
                if let Some(dispatch) = &self.dispatch {
                    w.bind(
                        dispatch.child(crate::resource::TERMINAL_RESOURCE_ID, actions::terminal),
                    );
                }
                w.run_through(self.terminal_runs.clone());
                if let Some(session) = self
                    .live_state
                    .as_ref()
                    .and_then(|live| live.with(|state| state.terminal_panel.clone()))
                {
                    ui::framework::ResourceSurface::present(&mut w, &std::rc::Rc::new(session));
                }
                self.terminal = Some(ui::widget::Embedded::new(w).filling());
                self.panel.set_has_live_terminal(true);
                self.panel.set_terminal_note(None);
            }
            Err(e) => {
                self.panel.set_has_live_terminal(false);
                self.panel
                    .set_terminal_note(Some(format!("The terminal could not start: {e}")));
            }
        }
    }

    /// Show the embedded terminal's session as the project records it: a
    /// collaborator's, a past version's, or this one's own before its shell
    /// draws.
    pub(crate) fn present_terminal(&mut self, session: Option<&terminal::resource::Terminal>) {
        if let (Some(terminal), Some(session)) = (self.terminal.as_mut(), session) {
            ui::framework::ResourceSurface::present(
                terminal.widget_mut(),
                &std::rc::Rc::new(session.clone()),
            );
        }
    }

    /// Whether the Terminal dock tab is currently visible with a live terminal.
    fn terminal_visible(&self) -> bool {
        self.panel_visible
            && self.panel.active_tab() == PanelTab::Terminal
            && self.terminal.is_some()
    }

    /// Give the Terminal panel an honest empty state until a real shell is
    /// spawned. Opening the Terminal tab spawns the live PTY (`ensure_terminal`,
    /// which clears this note); we never pre-fill a fabricated build log.
    fn seed_terminal(&mut self) {
        self.panel
            .set_terminal_note(Some("No terminal is running".to_string()));
    }

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
            t.widget_mut().write_input(s.as_bytes());
        }
    }

    /// The embedded terminal's current grid text, if a live terminal is
    /// present. `None` before the Terminal tab is opened (or if the shell
    /// failed to spawn). Used by `--shots` staging and the embed tests to read
    /// the live shell session without a GPU scene.
    pub fn shots_terminal_grid_text(&self) -> Option<String> {
        self.terminal
            .as_ref()
            .map(|terminal| terminal.widget().grid_text())
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

    /// Ask for a line to go to in the host palette, on the project's
    /// go-to-line effect.
    pub(crate) fn ask_for_line() {
        ui::host_palette::action(
            crate::effects::CodeEditorEffect::GO_TO_LINE
                .on(<crate::project::Project as ui::tilo_resource::Resource>::KIND),
        );
    }

    /// Stage the "save changes before closing?" prompt for the active buffer.
    ///
    /// The prompt only appears for a buffer with unsaved edits, so the file's
    /// saved text is staged one line shorter than the buffer.
    pub fn shots_close_prompt(&mut self) {
        if let Some(buf) = self.session.active_buffer() {
            self.close_prompt = Some(ClosePrompt {
                node: buf.node,
                name: buf.name.clone(),
                then_open: None,
            });
        }
    }

    /// Stage a populated status bar for a screenshot: a live multi-word
    /// selection and a recent notice.
    pub fn shots_status(&mut self) {
        self.shots_select_words();
        self.note("Reloaded main.rs from disk", false);
        self.refresh_status();
    }

    /// Stage the language-server indexing readout.
    pub fn shots_indexing(&mut self) {
        self.lsp_status = LspStatus::Busy {
            server: "rust-analyzer".to_string(),
            title: "Indexing".to_string(),
            detail: Some("ferris-cli".to_string()),
            percentage: Some(37),
        };
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
        let text = EditorDoc::read_buffer_key(doc, &self.buffer_key(node)).unwrap_or_default();
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
                "message": "Magic number 20, name it with a constant"
            }));
        }
        if let Some((l, c0, c1)) = find_char_span(&text, "&args") {
            diag_json.push(serde_json::json!({
                "range": { "start": {"line": l, "character": c0}, "end": {"line": l, "character": c1} },
                "severity": 1,
                "source": "rustc",
                "message": "Expected `&[String]`, found `&Vec<String>`"
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
        self.refresh_diagnostics_at(doc, None);

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
        self.refresh_status();
    }

    // ---- build_layout helpers (split out of `Widget::build_layout`, which
    // assembles their results into the content row and outer column) ----

    /// Lay out the editor column: the editor, (when visible) the bottom panel,
    /// and the status bar, stacked into one flex column.
    ///
    /// # Panics
    /// Panics if taffy's internal tree storage is corrupted — this only
    /// inserts freshly built child nodes into a tree this same call already
    /// holds a mutable reference to, so there is no data-dependent failure
    /// path to recover from.
    fn build_editor_column(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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

        let mut editor_col_children = vec![editor_node];
        if self.panel_visible {
            let panel_node = self.panel.build_layout(taffy, theme, node_map);
            self.panel_node = Some(panel_node);
            editor_col_children.push(panel_node);
        } else {
            self.panel_node = None;
        }
        // The status bar reserves its own band under everything, so nothing
        // is ever painted beneath it.
        #[allow(
            clippy::unwrap_used,
            reason = "inserting one fixed leaf into the app-owned layout tree is infallible unless the taffy arena is already corrupted"
        )]
        let status_node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(ui::chrome::status_bar::height(theme) as f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        self.status_node = Some(status_node);
        editor_col_children.push(status_node);

        #[allow(
            clippy::unwrap_used,
            reason = "Widget::build_layout is infallible by trait contract; inserting a node with a fixed Style and freshly-created children cannot fail short of an already-corrupted taffy arena, which nothing here could recover from"
        )]
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
        editor_col
    }

    // ---- handle_event helpers (split out of `Widget::handle_event`, which
    // dispatches to them in the same order they used to run inline) ----

    /// Track the pointer for the controls this root draws itself.
    fn track_pointer(&mut self, event: &WindowEvent, scale_factor: f64) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
    }

    /// Global shortcuts: Ctrl/Cmd+P and Ctrl/Cmd+Shift+P (the platform
    /// search), Ctrl/Cmd+B (sidebar), Ctrl/Cmd+J (panel), and the rest of the
    /// single-letter command bindings. Returns `true` when the event was
    /// consumed.
    fn handle_global_shortcuts(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let mods = modifiers.state();
        if !(mods.control_key() || mods.super_key()) {
            return false;
        }
        let shift = mods.shift_key();
        let alt = mods.alt_key();
        let command = match c.to_lowercase().as_str() {
            // Quick open and the command list are the host palette: the
            // project's files are its search entries, its commands its
            // published effects.
            "p" => {
                // Files and commands are platform search rows: both shortcuts
                // open the same search over this app.
                self.search_opener
                    .open(ui::search::SearchRequest::this_app());
                *handled = true;
                return true;
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
            "g" => Some(Command::GoToLine),
            _ => None,
        };
        if let Some(command) = command {
            let closes_bottom_panel = command == Command::ToggleBottomPanel && self.panel_visible;
            self.apply_command(command, state);
            if closes_bottom_panel {
                // The panel's toggle is in the window's bar: the keyboard
                // goes back to the code.
                *focus_target = self.editor_node;
            }
            *handled = true;
            return true;
        }
        false
    }

    /// While the Terminal dock tab shows a live terminal, a click in its
    /// content region focuses the shell and routes keyboard + pointer input
    /// to it; a press elsewhere returns focus to the editor. Returns `true`
    /// when the focused terminal owns this event (the caller should stop
    /// routing it to anything else).
    fn handle_terminal_input(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
        _taffy: &TaffyTree<()>,
        handled: &mut bool,
    ) -> bool {
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
        if !(term_active && self.terminal_focused) {
            return false;
        }
        if let Some(t) = self.terminal.as_mut() {
            let mut th = false;
            t.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                &automerge::ROOT,
                &mut th,
            );
            if th {
                *handled = true;
            }
        }
        // The focused terminal owns keyboard + pointer; consume those so the
        // editor underneath doesn't also react.
        matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::CursorMoved { .. }
        )
    }

    /// Route the event to the bottom panel and the editor.
    fn route_to_children(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.panel_visible
            && let Some(n) = self.panel_node
        {
            self.panel
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
        if let Some(n) = self.editor_node {
            self.editor
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            self.dispatch_editor_text_edits(&mut ctx.state);
        }
    }
}

/// The flat character index of `(line, col)` in `text`.
///
/// Automerge's `Text` is character-addressed and so is the editor's caret, so
/// every splice has to be converted here rather than from a byte offset — a
/// buffer containing any non-ASCII character would otherwise splice in the
/// wrong place.
pub(crate) fn flat_index(text: &str, line: usize, col: usize) -> Option<usize> {
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

/// Character-addressed replacement used to prepare a text action from editor
/// UI state. It intentionally operates on an ordinary temporary `String`;
/// the following action dispatch is the only CRDT mutation path.
pub(crate) fn replace_chars(text: &mut String, start: usize, delete: usize, replacement: &str) {
    let total = text.chars().count();
    let start = start.min(total);
    let end = start.saturating_add(delete).min(total);
    let byte_at = |index: usize| {
        text.char_indices()
            .nth(index)
            .map_or_else(|| text.len(), |(byte, _)| byte)
    };
    let first = byte_at(start);
    let last = byte_at(end);
    text.replace_range(first..last, replacement);
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
    let prefix = text.get(..byte).unwrap_or("");
    let line = prefix.matches('\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
    let start_col = text.get(line_start..byte).unwrap_or("").chars().count();
    let end_col = start_col + needle.chars().count();
    Some((line, start_col, end_col))
}

impl ui::presence::capability::PublishesPresence for CodeApp {
    /// The editor's selection in the open file, placed by the file's id in
    /// the project, so an agent's `replace-range` edits exactly that text.
    fn shared_selection(&self) -> Option<ui::shared_selection::SharedSelection> {
        let node = self.bound_node?;
        Some(
            self.editor
                .shared_selection_in(&self.buffer_key(node))?
                .in_kind("code_editor.project"),
        )
    }
}

impl ui::semantic_document_read::SemanticReaders for CodeApp {
    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::text_operations::SNAPSHOT_READERS
    }
}

/// The editor's text zooms by itself: Ctrl+Plus, Ctrl+Minus, Ctrl+0, a
/// Ctrl+scroll or a pinch change the code and its line numbers, not the
/// window's chrome. The chosen size is kept in the person's preferences, so
/// it follows them to the next window and device.
impl ui::content_zoom::ContentZoom for CodeApp {
    fn zoom_content(&mut self, step: ui::content_zoom::ZoomStep) -> Option<f32> {
        Some(self.zoom_editor_text(step))
    }
}

impl Widget for CodeApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::widget::Continuable,
        dyn ui::content_zoom::ContentZoom,
        dyn ui::widget::Findable,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::semantic_document_read::SemanticReaders
    );

    fn native_input_snapshot(&self, doc: &AppModel, path: &ModelPath) -> Option<serde_json::Value> {
        fn rect(rect: Rect) -> [f64; 4] {
            [rect.x0, rect.y0, rect.x1, rect.y1]
        }
        let (doc, _) = ui::doc::document_ref(doc, path);
        // Read authoritative text, never the editor's optimistic render cache.
        // Only the opt-in native input observer invokes this O(buffer length) read.
        let buffer_id = self.active_file_node().map(|node| self.buffer_key(node));
        let body = buffer_id
            .as_ref()
            .and_then(|key| EditorDoc::read_buffer_key(doc, key));
        let editor_rect = self.editor.get_rect();
        let (offset_x, offset_y) = self.editor.native_scroll_offsets();
        let (caret_line, caret_column) = self.editor.primary_caret();
        let (highlighted_rows, cached_buffers, inactive_source_bytes) =
            self.editor.native_projection_stats();
        let (read_texts, read_text_bytes) = self
            .live_state
            .as_ref()
            .map(|live| live.with(crate::project::Project::read_texts))
            .unwrap_or_default();
        Some(serde_json::json!({
            "app": "so.tilo.code-editor",
            "editor_id": "editor_view",
            "hash_algorithm": "sha256",
            "buffer_id": buffer_id,
            "body_hash": body.as_ref().map(|text| crate::native_fixture::hash(text.as_bytes())),
            "body_bytes": body.as_ref().map(String::len),
            "highlighted_rows": highlighted_rows,
            "cached_buffers": cached_buffers,
            "read_texts": read_texts,
            "read_text_bytes": read_text_bytes,
            "cached_diagnostic_buffers": self.inactive_diagnostics.len(),
            "inactive_diagnostic_records": self.inactive_diagnostics.iter().map(|cache| cache.local.len()).sum::<usize>(),
            "reopen_phase_micros": {
                "demand": self.reopen_phase_micros.demand,
                "symbols": self.reopen_phase_micros.symbols,
                "search_entries": self.reopen_phase_micros.search_entries,
                "diagnostics": self.reopen_phase_micros.diagnostics,
            },
            "inactive_source_bytes": inactive_source_bytes,
            "editor_focused": self.editor.is_focused(),
            "caret_line": caret_line,
            "caret_column": caret_column,
            "caret_count": self.editor.caret_count(),
            "selection_chars": self.editor.selection_len(),
            "offset_x": offset_x,
            "offset_y": offset_y,
            "offset_x_unit": "pixels",
            "offset_y_unit": "lines",
            "root_rect": rect(self.rect),
            "editor_rect": rect(editor_rect),
            "ready": body.is_some() && self.editor.is_bound()
                && self.rect.width() > 0.0 && self.rect.height() > 0.0
                && editor_rect.width() > 0.0 && editor_rect.height() > 0.0,
        }))
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        if let Some(settings) = &self.settings {
            self.editor.set_text_zoom(settings.get().editor_zoom);
        }
        if !self.seeded {
            self.seed_terminal();
        }
        let opened = std::mem::take(&mut self.opened.borrow_mut().requests);
        for file in opened {
            if let Some(node) = self.session.project.node_for_resource(&file) {
                self.open_node(node);
            }
        }
        // A file picker in flight is drained here, once a frame, off a channel
        // the worker thread fills — the UI never waits on D-Bus.
        self.poll_portal(state);
        self.poll_search();
        self.record_disk_entries();
        self.ensure_buffers(state);
        self.demand_open_buffers(state);
        self.continue_staging(state);
        self.refresh_search_entries(state);
        self.editor.sync(state, path);
        self.run_window_toolbar();
        self.restore_pending_continuation(state);

        // A jump queued by a search hit or a cross-file definition lands once
        // the target buffer is actually bound.
        if let Some((line, col)) = self.pending_jump
            && self.editor.is_bound()
        {
            self.editor.move_caret_to(line, col);
            self.pending_jump = None;
        }

        // Show the terminal record; update drains its PTY and settlement
        // records the resulting screen through the terminal's action door.
        if let Some(t) = self.terminal.as_mut() {
            t.sync(state, path);
        }

        // Language server: start on demand (opt-in), stream edits out as
        // `didChange`, and drain diagnostics/completions/hover back in.
        self.maybe_start_lsp(state);
        if self.lsp.is_some()
            && let Some((node, uri)) = self
                .session
                .active_buffer()
                .map(|b| b.node)
                .zip(self.lsp_uri.clone())
        {
            let text =
                EditorDoc::read_buffer_key(state, &self.buffer_key(node)).unwrap_or_default();
            if text != self.lsp_sent_text {
                self.lsp_version += 1;
                let version = self.lsp_version;
                if let Some(c) = self.lsp.as_mut() {
                    c.did_change(&uri, version, &text);
                }
                self.lsp_sent_text = text;
            }
        }
        self.poll_lsp();

        self.check_disk_staleness();
        self.refresh_diagnostics(state);
        self.refresh_status();
        let chrome = (self.panel_visible, self.panel.active_tab());
        if chrome != self.laid_out {
            self.laid_out = chrome;
            self.structure_dirty = true;
        }
    }

    fn settle(&mut self, state: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        #[cfg(test)]
        if let Some(hook) = self.test_hook.take() {
            hook(self, state);
        }
        self.apply_disk_work(state);
        if let Some(command) = self.pending_command.take() {
            self.apply_command(command, state);
        }
        if let Some((node, target)) = self.pending_save.take() {
            self.notice = None;
            let after = format_workflow::AfterFormat::SaveEffect(target);
            if let Some(after) = self.format_before_saving(state, node, after) {
                self.after_format(state, node, after);
            }
        }
        self.apply_formatted(state);
        self.record_missing_text(state);
        self.dispatch_editor_text_edits(state);
        for child in self.children_mut() {
            child.settle(&mut state.reborrow(), path);
        }
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.widget_mut().settle(&mut state.reborrow(), path);
        }
        // Canonical row View callbacks run after pointer dispatch. Consume
        // their navigation requests in this settlement, without waiting for
        // another physical input event.
        self.drain_child_events();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // --- Editor column: file header + editor + (optional) bottom panel ---
        let editor_col = self.build_editor_column(taffy, theme, node_map);

        // --- Content row: the editor column ---
        #[allow(
            clippy::unwrap_used,
            reason = "Widget::build_layout is infallible by trait contract; inserting a node with a fixed Style and freshly-created children cannot fail short of an already-corrupted taffy arena, which nothing here could recover from"
        )]
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
                    ..Default::default()
                },
                &[editor_col],
            )
            .unwrap();
        self.content_node = Some(content_node);

        // --- Outer column ---
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[content_node], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let new_origin = self.rect.origin();

        if let Some(content_node) = self.content_node {
            let content_origin = ui::widget::layout_rect(taffy, content_node, new_origin).origin();
            // Editor column origin within the content row.
            if let Some(col) = self.editor_col_node {
                let col_origin = ui::widget::layout_rect(taffy, col, content_origin).origin();
                if let Some(n) = self.editor_node {
                    self.editor.apply_layout(taffy, n, col_origin);
                }
                if let Some(n) = self.panel_node {
                    self.panel.apply_layout(taffy, n, col_origin);
                }
                if let Some(n) = self.status_node {
                    self.status_rect = ui::widget::layout_rect(taffy, n, col_origin);
                }
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The framework painted the window ground; each region stands on its
        // own tier.
        self.draw_content(scene, transform, text_renderer, theme, presence);

        // The embedded terminal renders its live grid over the panel's content
        // region whenever the Terminal dock tab is showing.
        if self.terminal_visible() {
            let content = self.panel.content_rect();
            if let Some(t) = self.terminal.as_mut() {
                t.layout(content, theme, text_renderer);
                t.draw(scene, transform, text_renderer, theme, presence);
            }
        }
    }

    /// The top layer: the open question, over the whole window.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let window = presence.viewport.get().unwrap_or(self.rect);
        self.draw_question(scene, transform, text_renderer, theme, window);
        // The completion list and the other floating pieces draw themselves
        // in `draw`; nothing else lives in the top layer.
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.track_pointer(event, ctx.scale_factor);

        // A question (a save conflict, unsaved changes on close) is modal:
        // while it is up it owns all input, because answering it decides
        // which of two versions of the file survives.
        if let Some(question) = self.question() {
            self.handle_question(
                question,
                event,
                ctx.scale_factor,
                &mut ctx.state,
                ctx.handled,
            );
            return;
        }
        if self.code_help_pointer(event) {
            *ctx.handled = true;
            return;
        }

        if self.handle_global_shortcuts(
            event,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.handled,
            ctx.focus_target,
        ) {
            return;
        }

        // The bottom dock is a retained disclosure. Escape dismisses it after
        // higher-priority modal/find surfaces have had their own Escape, then
        // returns keyboard focus to the visible trigger that can reopen it.
        if self.panel_visible
            && matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        logical_key: Key::Named(winit::keyboard::NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                    ..
                }
            )
        {
            self.close_bottom_panel();
            *ctx.focus_target = self.editor_node;
            *ctx.handled = true;
            return;
        }

        // Terminal input capture: while the Terminal dock tab shows a live
        // terminal, a click in its content region focuses the shell and routes
        // keyboard + pointer input to it (like clicking into any terminal); a
        // press elsewhere returns focus to the editor.
        if self.handle_terminal_input(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.taffy,
            ctx.handled,
        ) {
            return;
        }

        // Route to child widgets.
        self.route_to_children(event, ctx);

        // If the editor asked for a language-server action (Ctrl+Space, F1,
        // F12), issue it now with the caret position.
        if let Some(ask) = self.editor.take_lsp_ask() {
            self.dispatch_lsp_ask(ask);
        }

        // The zero-state's "Open file" button routes to the same portal picker
        // as the Open file command.
        if self.editor.take_empty_action() {
            self.request_open_file();
        }

        self.drain_child_events();
        self.refresh_diagnostics(ctx.state.doc());
        self.refresh_status();
    }

    fn name(&self) -> &'static str {
        "CodeApp"
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        self.code_help_action(target, action)
    }

    fn test_id(&self) -> Option<&str> {
        Some("code-editor")
    }

    /// The editor is a custom-painted multi-pane surface. Its retained child
    /// widgets expose their own controls, while this node gives a screen reader
    /// the missing document-level context: project, active buffer, open-tab
    /// count, and live analysis/file-operation state. Without it an AT user
    /// lands in a generic canvas before discovering the editor's actual
    /// document.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};

        let active = self
            .session
            .active_buffer()
            .map_or("No file open", |buffer| buffer.name.as_str());
        let mut children = vec![
            AccessNode::new(Role::Heading { level: 1 })
                .with_key(ui::a11y::hash_key(&"code.project-heading"))
                .with_name(self.session.project.name.clone()),
            AccessNode::new(Role::StaticText)
                .with_key(ui::a11y::hash_key(&"code.active-buffer"))
                .with_name(active.to_string()),
        ];
        if let Some(notice) = &self.notice {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_key(ui::a11y::hash_key(&"code.notice"))
                    .with_name(notice.text.clone())
                    .with_live(if notice.is_error {
                        Politeness::Assertive
                    } else {
                        Politeness::Polite
                    }),
            );
        }
        children.extend(self.code_help_access());
        let analysis = self.lsp_status.label();
        if !analysis.is_empty() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_key(ui::a11y::hash_key(&"code.analysis-status"))
                    .with_name(analysis)
                    .with_live(Politeness::Polite),
            );
        }
        Some(
            AccessNode::new(Role::Document)
                .with_name(format!("Code: {}", self.session.project.name))
                .with_description(
                    "Code editor. Use the file tree, file header, editor document, and bottom panel to navigate the project.",
                )
                .with_value(active)
                .with_state(AccessState::new().focusable(true))
                .with_children(children),
        )
    }

    /// Expose the panes so headless driving can address them by name. Without
    /// this the whole editor is one opaque rect and `--headless-drive` can only
    /// click at raw coordinates.
    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = vec![&self.editor];
        if self.panel_visible {
            out.push(&self.panel);
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut out: Vec<&mut (dyn Widget + '_)> = vec![&mut self.editor];
        if self.panel_visible {
            out.push(&mut self.panel);
        }
        out
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// A capture waits while a scenario is still staging, a project search
    /// is still streaming its hits.
    fn has_pending_visual_work(&self) -> bool {
        self.staging.is_some() || self.search_job.is_some()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.editor.needs_redraw()
            || self.panel.needs_redraw()
            || self.question().is_some()
            || self.code_help_needs_redraw()
            // A live terminal repaints continuously while visible.
            || self.terminal_visible()
    }

    /// Advance the editor and drain the retained terminal before settlement.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = self.editor.update(state, path);
        if let Some(terminal) = self.terminal.as_mut() {
            changed |= terminal.widget_mut().update(state, path);
        }
        changed
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the event loop pumping frames while a live terminal is
        // visible: its PTY is drained during update, which the App repeats
        // while an animation frame is requested.
        //
        // A running search or an in-flight file picker also needs frames: both
        // deliver over a channel drained in `sync`, so without a frame their
        // results would sit unread until the user happened to move the mouse.
        // A language server that is still working needs them too, so the
        // indexing readout actually advances.
        self.panel.needs_animation_frame()
            || self.terminal_visible()
            || self.search_job.is_some()
            || self.portal.is_some()
            || self.lsp_status.is_working()
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }

    fn mark_drawn(&mut self) {
        self.editor.mark_drawn();
        self.panel.mark_drawn();
    }
}

impl ui::widget::controls::ActionControls for CodeApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.search_opener.take().or_else(|| {
            self.children_mut()
                .into_iter()
                .find_map(ui::widget::controls::take_control_invocation)
        })
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        self.children()
            .into_iter()
            .flat_map(ui::widget::controls::control_action_bindings)
            .collect::<Vec<_>>()
    }
}

#[cfg(test)]
#[path = "native_proof_tests.rs"]
mod native_proof_tests;

#[cfg(test)]
#[path = "diagnostics_cache_tests.rs"]
mod diagnostics_cache_tests;

impl CodeApp {
    fn start_project_search(&mut self, query: search::Query) {
        // A previous job is cancelled rather than left to finish: eight
        // abandoned walks of a large repository is eight threads reading it.
        if let Some(job) = self.search_job.take() {
            job.cancel();
        }
        let had_results = !self.search_hits.is_empty();
        self.search_hits.clear();
        if had_results {
            self.invalidate_search_projection();
        }
        self.search_truncated = false;
        self.search_files_scanned = 0;
        let Some(root) = self.session.project.root_path() else {
            self.note("Open a project folder to search it", true);
            return;
        };
        self.last_search_needle.clone_from(&query.needle);
        if query.is_empty() {
            self.push_search_state();
            return;
        }
        self.search_job = Some(search::start(&root, query));
        self.panel_visible = true;
        self.panel.set_tab(PanelTab::Search);
        self.push_search_state();
    }
}

impl CodeApp {
    fn poll_search(&mut self) {
        let Some(job) = self.search_job.as_mut() else {
            return;
        };
        let msgs = job.drain();
        if msgs.is_empty() && !job.is_finished() {
            return;
        }
        let mut results_changed = false;
        for msg in msgs {
            match msg {
                search::Msg::Hits(hits) => {
                    results_changed |= !hits.is_empty();
                    self.search_hits.extend(hits);
                }
                search::Msg::Done { files, truncated } => {
                    self.search_files_scanned = files;
                    self.search_truncated = truncated;
                }
            }
        }
        if results_changed {
            self.invalidate_search_projection();
        }
        if self
            .search_job
            .as_ref()
            .is_some_and(super::search::Job::is_finished)
        {
            self.search_job = None;
        }
        self.push_search_state();
    }
}

impl CodeApp {
    fn push_search_state(&mut self) {
        let query = self.last_search_needle.clone();
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
            files_scanned: self.search_files_scanned,
        });
    }
}

impl CodeApp {
    fn clear_search(&mut self) {
        if let Some(job) = self.search_job.take() {
            job.cancel();
        }
        let had_results = !self.search_hits.is_empty();
        self.search_hits.clear();
        if had_results {
            self.invalidate_search_projection();
        }
        self.search_truncated = false;
        self.search_files_scanned = 0;
        self.last_search_needle.clear();
        self.push_search_state();
    }
}

impl CodeApp {
    pub fn shots_project_search(&mut self, needle: &str) {
        self.start_project_search(search::Query::new(needle));
        let deadline = ui::time::now() + std::time::Duration::from_secs(20);
        while self.search_job.is_some() && ui::time::now() < deadline {
            self.poll_search();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        self.poll_search();
        self.panel_visible = true;
        self.panel.set_tab(PanelTab::Search);
    }
}
