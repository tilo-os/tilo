//! Root widget for the code editor. Owns the [`Session`] (project + open
//! buffers), the editor pane, the file-tree sidebar, the file header, the status
//! model and the bottom panel — and coordinates the data flow between them.
//!
//! Layout:
//! ```text
//! +----------+-------------------------------+
//! | file     |  file header                      |
//! | tree     +-------------------------------+
//! | sidebar  |  find bar (when open)         |
//! |          +-------------------------------+
//! |          |  editor view                  |
//! |          +-------------------------------+
//! |          |  bottom panel (problems/term) |
//! |          +-------------------------------+
//! |          |  status bar                   |
//! +----------+-------------------------------+
//! ```
//! The caret and diagnostics readout is a real row: the status bar reserves its
//! band under the editor, so nothing is ever drawn over code. Searching the
//! project and running a command belong to the platform search, which this root
//! opens with Ctrl+P, Ctrl+Shift+P and Ctrl+Shift+F.
//!
//! ## Responsive sidebar
//!
//! The file-tree sidebar is a toolkit [`SidebarDrawer`]: **docked** beside the
//! editor when the window is wide, a floating drawer over it (behind a scrim)
//! when it is narrow. The toolkit draws the sidebar toggle, at the top-leading
//! corner of the sidebar and, when the sidebar is not docked open, at the
//! leading edge of the file header. The scrim, Escape, or the toggle again
//! dismiss the drawer.
//!
//! The editor is not a master-detail list, so there is no push navigation —
//! the single primary sidebar simply docks or floats per the toolkit's
//! breakpoint.

mod continuity;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

mod chrome;
mod dialogs;
mod file_workflow;
mod find_workflow;
mod lsp_workflow;
mod surface;
#[cfg(test)]
mod tests;
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::Effect as _;
use ui::widget::{Button, EmptyState, FindBar, SidebarDrawer, SplitLayout, Text, Widget};
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
use crate::explorer::{self, Explorer, ExplorerEvent, ExplorerEvents};
use crate::lint;
use crate::model::{Language, NodeId as TreeId, NodeKind, Project, Session};
use crate::sample_project;
use crate::search;
use crate::status_bar::{StatusModel, StatusState};
use ui::time::Instant;

/// Retained toggle for the bottom dock. [`Button`] owns pointer, keyboard,
/// focus, painting, and the canonical action emitter; this thin wrapper adds
/// the disclosure state that a generic momentary button cannot infer for
/// accessibility, and swaps the button's icon and name with it.
struct BottomPanelToggle {
    button: Button,
    expanded: bool,
    dirty: bool,
}

impl BottomPanelToggle {
    const ID: &'static str = "code.bottom-panel-toggle";

    fn new(binding: tilo_actions::ActionBinding<()>) -> Self {
        Self {
            button: Self::button(binding, false),
            expanded: false,
            dirty: true,
        }
    }

    fn name(expanded: bool) -> &'static str {
        if expanded { "Hide panel" } else { "Show panel" }
    }

    fn button(binding: tilo_actions::ActionBinding<()>, expanded: bool) -> Button {
        let icon = if expanded {
            ui::icons::Icon::ChevronDown
        } else {
            ui::icons::Icon::ChevronUp
        };
        Button::icon_only(Self::ID, icon, Self::name(expanded)).on_action(binding)
    }

    /// Captures and past versions have no executor for the panel effect.
    /// Publish the actual control as unavailable rather than retaining an
    /// action binding to a door that this host does not offer.
    fn suspend(&mut self) {
        let icon = if self.expanded {
            ui::icons::Icon::ChevronDown
        } else {
            ui::icons::Icon::ChevronUp
        };
        self.button =
            Button::icon_only(Self::ID, icon, Self::name(self.expanded)).with_enabled(false);
        self.dirty = true;
    }

    fn set_expanded(&mut self, expanded: bool) {
        if self.expanded == expanded {
            return;
        }
        self.expanded = expanded;
        let icon = if expanded {
            ui::icons::Icon::ChevronDown
        } else {
            ui::icons::Icon::ChevronUp
        };
        let name = Self::name(expanded);
        // Keep the retained emitter: rebuilding an action-bound button here
        // discards the host's custody and availability, reviving a door that
        // a capture or past version has already refused.
        let button = std::mem::replace(&mut self.button, Button::icon_only(Self::ID, icon, name));
        self.button = button
            .with_icon(icon)
            .with_accessible_name(name)
            .tooltip(name);
        self.dirty = true;
    }
}

impl Widget for BottomPanelToggle {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.button.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.button.build_layout(taffy, theme, node_map);
        // The wrapper, not the button, answers for the node, so events and
        // layout reach the disclosure state too.
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.button.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.button
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.is_focused()
            && matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        logical_key: Key::Named(
                            winit::keyboard::NamedKey::Enter | winit::keyboard::NamedKey::Space
                        ),
                        state: ElementState::Pressed,
                        ..
                    },
                    ..
                }
            )
            && self
                .button
                .perform_action(&ui::a11y::Action::Activate, &mut ctx.state, ctx.path)
        {
            *ctx.handled = true;
            return;
        }
        self.button.handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "BottomPanelToggle"
    }

    fn test_id(&self) -> Option<&str> {
        Some(Self::ID)
    }

    fn get_rect(&self) -> Rect {
        self.button.get_rect()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.button.cursor_shape(point)
    }

    fn tooltip(&self) -> Option<&str> {
        <Button as Widget>::tooltip(&self.button)
    }

    fn is_focusable(&self) -> bool {
        self.button.is_focusable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.button.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.button.is_focused()
    }

    fn is_enabled(&self) -> bool {
        self.button.is_enabled()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        Some(
            AccessNode::new(Role::ToggleButton)
                .with_semantic_key(Self::ID)
                .with_name(Self::name(self.expanded))
                .with_description("Problems and terminal")
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    disabled: !self.is_enabled(),
                    checked: Some(self.expanded),
                    selected: self.expanded,
                    expanded: Some(self.expanded),
                    ..Default::default()
                }),
        )
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.button.perform_action(action, state, path)
    }

    fn set_interaction_environment(
        &mut self,
        environment: ui::interaction::InteractionEnvironment,
    ) {
        self.button.set_interaction_environment(environment);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.button.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.button.mark_drawn();
    }
}

impl ui::widget::controls::ActionControls for BottomPanelToggle {
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        self.button.install_control_custody(domain, attachment);
    }

    fn has_pending_control_invocation(&self) -> bool {
        self.button.has_pending_control_invocation()
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.button.take_control_invocation()
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        self.button.control_action_bindings()
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        self.button.refresh_control_action_availability(allowed);
    }
}

/// A control's binding to one of Code's effects, published on the project
/// resource beside its document actions.
#[allow(
    clippy::expect_used,
    reason = "every effect of the vocabulary is published with no parameters left to supply"
)]
pub(crate) fn effect_binding(
    effect: &crate::effects::CodeEditorEffect,
) -> tilo_actions::ActionBinding<()> {
    let resource = tilo_actions::Resource::new(
        <crate::project::Project as ui::tilo_resource::Resource>::KIND,
        <crate::project::Project as ui::tilo_resource::Resource>::KIND,
        "Code",
    )
    .with_action(effect.bound());
    tilo_actions::ActionBinding::unit_for(&resource, effect.id())
        .expect("a bound effect takes no arguments")
}

/// The editor header's file-name text, and the label it reads.
fn file_header(session: &Session) -> (Text, std::rc::Rc<std::cell::RefCell<String>>) {
    let label = std::rc::Rc::new(std::cell::RefCell::new(
        session
            .active_buffer()
            .map_or_else(String::new, |buffer| buffer.name.clone()),
    ));
    let source = label.clone();
    (
        Text::dynamic(move || source.borrow().clone())
            .with_role(ui::tokens::TextRole::Heading)
            .with_kind(ui::TextKind::Identifier)
            .with_id("code-editor.file"),
        label,
    )
}

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
    reason = "each bool is an independent, unrelated flag (panel visibility, one-time seed guard, terminal focus, find bar visibility) on the app's top-level state, not a set of options that combine — a bitflag/enum would not read better"
)]
pub struct CodeApp {
    session: Session,
    /// The project explorer (a toolkit `TreeView`); its activation and
    /// expansion signals land in `explorer_events`.
    sidebar: Explorer,
    explorer_events: ExplorerEvents,
    /// The editor header's file name. A window shows one file; other files
    /// open beside it as desktop tabs, so there is no file header here.
    header: Text,
    header_label: std::rc::Rc<std::cell::RefCell<String>>,
    editor: EditorView,
    /// The caret / language / diagnostics readout, drawn in the status bar
    /// band under the editor.
    status: StatusModel,
    panel: BottomPanel,
    panel_toggle: BottomPanelToggle,
    /// Opens the platform search over this app: the one search affordance in
    /// the chrome.
    search_button: Button,

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

    /// The file-tree sidebar: docked when the window is wide, a floating
    /// drawer when it is narrow.
    drawer: SidebarDrawer,
    /// Last known allocated width, cached so `build_layout` (which runs before
    /// `apply_layout` has measured the window) can resolve docked vs. drawer
    /// on the very first frame. Seeded wide so a fresh desktop window docks.
    last_width: f64,
    /// Where the sidebar toggle is drawn (the sidebar's header or the tab
    /// strip's leading edge), and whether the pointer is over it.
    /// The sidebar's no-project state, with its one "Open folder" action.
    sidebar_empty: EmptyState,
    /// The header row's height and the width the file header gives up to the
    /// sidebar toggle, from the theme at layout time.
    header_h: f64,
    toggle_slot_w: f64,
    /// Last cursor position, since `MouseInput` carries no coordinates.
    last_cursor: Point,
    panel_visible: bool,

    rect: Rect,
    sidebar_node: Option<NodeId>,
    header_node: Option<NodeId>,
    editor_header_node: Option<NodeId>,
    panel_toggle_node: Option<NodeId>,
    search_button_node: Option<NodeId>,
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

    /// The toolkit's find bar over the active buffer, and the matches the
    /// editor answered it with. The bar is a band above the editor while
    /// `find_open`.
    find: FindBar,
    find_open: bool,
    find_node: Option<NodeId>,
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
    laid_out: (bool, PanelTab, bool),
    structure_dirty: bool,
    /// A file picker in flight, and what the app asked for.
    portal: Option<ui::portal::PortalChannel>,
    portal_intent: Option<PortalIntent>,
    /// The answer owed to the effect that is opening or saving a file, given
    /// once the picker or the write settles.
    file_reply: Option<ui::framework::EffectReply>,
    /// A save an effect asked for, written on the next sync, where the
    /// buffer's text is.
    pending_save: Option<(TreeId, Option<std::path::PathBuf>)>,
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
    /// Save the buffer, then close the tab.
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
                    let _ = write!(s, " {d}");
                }
                if let Some(p) = percentage {
                    let _ = write!(s, " {p}%");
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

    #[allow(
        clippy::too_many_lines,
        reason = "one constructor listing every field of the editor once"
    )]
    fn from_session(session: Session, default: Option<TreeId>) -> Self {
        let explorer_events = ExplorerEvents::default();
        let mut sidebar = explorer::explorer(&session.project, &explorer_events);
        explorer::set_current(&mut sidebar, default);

        let (header, header_label) = file_header(&session);

        let panel_toggle = BottomPanelToggle::new(effect_binding(
            &crate::effects::CodeEditorEffect::ToggleBottomPanel,
        ));

        CodeApp {
            session,
            sidebar,
            explorer_events,
            header,
            header_label,
            editor: EditorView::new(),
            status: StatusModel::new(),
            panel: BottomPanel::new(),
            panel_toggle,
            #[cfg(test)]
            test_hook: None,
            dispatch: None,
            presented: std::rc::Rc::default(),
            opened: std::rc::Rc::default(),
            live_state: None,
            demanded: Vec::new(),
            pending_command: None,
            pending_open: None,
            drawer: SidebarDrawer::new(),
            search_button: ui::search::search_button(ui::search::SearchRequest::this_app()),
            // Seed wide so the first frame of a desktop window (and `--shots`,
            // 1280px) docks the sidebar; corrected from the real width once
            // `apply_layout` measures the window.
            last_width: f64::MAX,
            sidebar_empty: EmptyState::new("No folder open")
                .with_icon(ui::icons::Icon::FolderOpen)
                .with_action("Open folder"),
            header_h: 0.0,
            toggle_slot_w: 0.0,
            last_cursor: Point::ZERO,
            panel_visible: false,
            rect: Rect::ZERO,
            sidebar_node: None,
            header_node: None,
            editor_header_node: None,
            panel_toggle_node: None,
            search_button_node: None,
            editor_node: None,
            panel_node: None,
            status_node: None,
            status_rect: Rect::ZERO,
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
            diagnostics_cache: None,
            inactive_diagnostics: Vec::new(),
            #[cfg(test)]
            diagnostics_buffer_reads: 0,
            lsp_status: LspStatus::None,
            lsp_failed: std::collections::HashSet::new(),
            find: FindBar::new().with_replace(),
            find_open: false,
            find_node: None,
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
            laid_out: (false, PanelTab::Problems, false),
            structure_dirty: false,
            file_reply: None,
            pending_save: None,
            staging: None,
            staging_until: None,
            conflict: None,
            close_prompt: None,
            question_sheet: ui::widget::ModalSheet::new(),
            notice: None,
            pending_jump: None,
            pending_continuation: None,
        }
    }

    /// Whether there is a project to list: the root alone is no project.
    fn has_project(&self) -> bool {
        self.session.project.nodes.len() > 1
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

    /// Resolve docked vs. drawer from the allocated width. Uses the cached
    /// `last_width` so the decision is stable across all five passes — in
    /// particular `build_layout`, which runs before `apply_layout` has set
    /// `self.rect` to the real window size.
    fn sidebar_layout(&self) -> SplitLayout {
        self.drawer.layout_for(self.last_width)
    }

    /// Seed the width used to resolve the responsive layout on the first frame,
    /// before the window has been measured. The `--shots` renderer calls this
    /// with the viewport width so a single offscreen frame picks the correct
    /// docked/drawer layout (the live app self-corrects from `apply_layout`).
    ///
    /// Because a single offscreen frame never runs the animation `tick`, also
    /// settle the drawer to this layout's resting state (docked ⇒ open, drawer
    /// ⇒ closed) so the captured frame is the settled layout, not one frozen
    /// mid-slide. An explicit `shots_snap_sidebar(true)` after this still wins.
    pub fn set_width_hint(&mut self, width: f64) {
        self.last_width = width;
        self.drawer.resolve(Rect::new(0.0, 0.0, width, 1.0));
        self.drawer.snap(self.drawer.is_open());
    }

    /// Whether the sidebar is currently resolved open (preference or default).
    fn sidebar_open(&self) -> bool {
        self.drawer.is_open()
    }

    /// Flip the sidebar open/closed (its toggle, Ctrl+B, the palette command).
    fn toggle_sidebar(&mut self) {
        self.drawer.toggle();
    }

    /// Snap the drawer to a final open/closed state with no animation — used by
    /// the single-frame `--shots` renderer so a captured frame shows a settled
    /// sidebar rather than one mid-slide.
    pub fn shots_snap_sidebar(&mut self, open: bool) {
        self.drawer.snap(open);
    }

    /// Whether the sidebar panel is on screen: docked with some width, or the
    /// drawer open or still sliding.
    fn sidebar_visible(&self) -> bool {
        self.drawer.reveal() > 0.001
    }

    /// The sidebar panel's header row and the body under it, where the
    /// explorer lives.
    fn sidebar_header_and_body(&self, theme: &Theme) -> (Rect, Rect) {
        let panel = self.drawer.sidebar_rect();
        let header_h = f64::from(theme.tokens.metrics.header_height).min(panel.height());
        (
            Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + header_h),
            Rect::new(panel.x0, panel.y0 + header_h, panel.x1, panel.y1),
        )
    }

    /// The width the file header gives up to the sidebar toggle when the toggle
    /// sits at the strip's leading edge: the toggle and a gap.
    fn toggle_slot_width(theme: &Theme) -> f64 {
        let inset = f64::from(theme.tokens.spacing.s3);
        let side = f64::from(theme.tokens.metrics.min_pointer_target);
        inset + side + f64::from(theme.tokens.spacing.s2)
    }

    /// Whether the toggle sits in the file header: whenever the sidebar panel
    /// is not itself a docked column with a header to hold it.
    fn toggle_in_tab_strip(&self) -> bool {
        self.sidebar_layout() == SplitLayout::Overlay || self.drawer.reveal() <= 0.5
    }

    /// The window's header row, across the top of the editor: the drawer
    /// lays its toggle out at the leading corner.
    fn window_header_row(&mut self, theme: &Theme) -> Rect {
        let area = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.header_h,
        );
        self.drawer.header(area, theme).toggle
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

    /// Submit `action` through the framework's door; it applies after the
    /// event.
    fn submit(&self, action: &actions::Action) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.apply(action);
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

    /// Read a disk-backed file into its buffer the first time this session
    /// opens it. The document may already hold text for it — a collaborator's
    /// or a previous session's unsaved edits, which are not ours to discard —
    /// so the seed only creates text that is missing; the file's bytes and
    /// stamp become the comparison a save is checked against either way.
    ///
    /// A read failure closes the tab again: a file that opened as a silently
    /// empty buffer would be written back empty by the next Ctrl+S.
    fn seed_buffer(&mut self, node: TreeId) {
        let Some(buffer) = self.session.buffer_of(node) else {
            return;
        };
        if buffer.stamp.is_some() || self.session.project.disk_path_of(node).is_none() {
            return;
        }
        let name = buffer.name.clone();
        match self.session.project.file_seed(node) {
            Ok((text, stamp)) => {
                let key = self.buffer_key(node);
                let already_opened = self
                    .live_state
                    .as_ref()
                    .is_some_and(|live| live.with(|state| state.buffers.contains_key(&key)));
                if !already_opened {
                    self.submit(&actions::record_text_action(key, text.clone()));
                }
                let path = self.session.project.disk_path_of(node);
                if let Some(buffer) = self.session.buffer_of_mut(node) {
                    buffer.path = path;
                    buffer.mark_saved(text, stamp);
                }
            }
            Err(error) => {
                if self.session.buffer_of(node).is_some() {
                    self.session.close_file(node);
                    self.invalidate_search_projection();
                }
                self.note(format!("Could not open {name}: {error}"), true);
                self.bound_node = None;
            }
        }
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
        if !live {
            self.panel_toggle.suspend();
        }
        self.settings = Some(settings);
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
        explorer::refresh(&mut self.sidebar, &self.session.project);
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

    /// Keep the header naming the shown file, with a marker while it has
    /// unsaved edits. Idempotent: safe to call more than once a frame.
    fn refresh_header(&self) {
        let label = self
            .session
            .active_buffer()
            .map_or_else(String::new, |buffer| {
                if buffer.dirty {
                    format!("{} \u{2022}", buffer.name)
                } else {
                    buffer.name.clone()
                }
            });
        if *self.header_label.borrow() != label {
            *self.header_label.borrow_mut() = label;
        }
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
            notice_is_error: notice.is_error,
            dirty: self.session.active_buffer().is_some_and(|b| b.dirty),
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
        explorer::set_current(&mut self.sidebar, Some(node));
        self.bound_node = None; // force re-bind in ensure_buffers
        self.refresh_header();
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
                    // Echoes back as `Expand` through the tree's own
                    // `on_expand`, which is where the folder is read.
                    self.sidebar.toggle_expand(explorer::node_key(id));
                    return;
                }
                self.open_node(id);
                // As a drawer, opening a file dismisses it so the user lands on
                // the freshly-opened buffer (single-pane feel). Docked, the
                // sidebar stays pinned.
                if self.sidebar_layout() == SplitLayout::Overlay {
                    self.drawer.close();
                }
            }
            ExplorerEvent::Expand(id, open) => {
                // Reading the folder's children happens here, on first
                // expansion, rather than when the project was opened — one
                // directory level at a time is what makes opening a large
                // repository instant.
                if open && let Err(e) = self.session.project.load_children(id) {
                    let name = self
                        .session
                        .project
                        .get(id)
                        .map(|n| n.name.clone())
                        .unwrap_or_default();
                    self.note(format!("Could not read {name}: {e}"), true);
                }
                self.session.project.set_folder_expanded(id, open);
                self.invalidate_search_projection();
                explorer::refresh(&mut self.sidebar, &self.session.project);
            }
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
            explorer::set_current(&mut self.sidebar, None);
            self.refresh_header();
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
                    // Save synchronously; close only if it actually landed. A
                    // save that hits a disk conflict leaves the tab open so the
                    // conflict prompt (now showing) gets answered first.
                    let snapshot = doc.clone();
                    if self.save_buffer(&snapshot, prompt.node, false) {
                        self.close_node(prompt.node, prompt.then_open);
                    }
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
            Command::NextTheme => self.note(
                "Choose appearance in the operating system's settings",
                false,
            ),
            Command::GoToSearch => {
                self.panel_visible = true;
                self.panel.set_tab(PanelTab::Search);
            }
            Command::Find => self.open_find(),
            Command::Replace => self.open_replace(),
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
            Command::ToggleBottomPanel => {
                if self.panel_visible {
                    self.close_bottom_panel();
                } else {
                    self.panel_visible = true;
                }
            }
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
        self.panel_toggle.set_expanded(self.panel_visible);
        self.dispatch_editor_text_edits(doc);
    }

    /// Close the retained dock without destroying its selected tab or live
    /// Terminal. Every close route uses this transition so hidden Terminal
    /// content cannot retain keyboard ownership.
    fn close_bottom_panel(&mut self) {
        self.panel_visible = false;
        self.terminal_focused = false;
        self.panel_toggle.set_expanded(false);
    }

    /// Drain everything the child widgets emitted this frame. `doc` is threaded
    /// through so a chosen palette command that mutates the document (New file)
    /// can dispatch its action.
    fn drain_child_events(&mut self) {
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
            && let Some(node) = self.session.project.reveal_path(&hit.path)
        {
            self.open_node(node);
            self.pending_jump = Some((hit.line.saturating_sub(1), hit.col));
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

    /// Stage the find bar over the active buffer with a real query, so the shot
    /// shows real match highlighting and a real count.
    pub fn shots_open_find(&mut self, query: &str, doc: &AutoCommit) {
        self.open_find();
        self.find.set_query(query);
        // The bar runs the query on its first drive and lands on the first
        // match, so the current-match outline is visible.
        self.drive_find(&mut ui::widget::Edits::lend(&mut doc.clone()));
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
        ui::host_palette::action(tilo_actions::ActionAddress::new(
            <crate::project::Project as ui::tilo_resource::Resource>::KIND,
            ui::tilo_resource::Effect::id(&crate::effects::CodeEditorEffect::GoToLine {
                value: None,
            }),
        ));
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
    /// selection and a recent save notice.
    pub fn shots_status(&mut self) {
        self.shots_select_words();
        self.note("Saved main.rs", false);
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

    /// Stage the Problems dock over the diagnostics the completion demo
    /// feeds, with no completion list in front of the code.
    pub fn shots_problems(&mut self, doc: &AutoCommit) {
        self.shots_completion_demo(doc);
        self.editor.hide_completions();
        self.panel_visible = true;
        self.panel.set_tab(PanelTab::Problems);
        self.refresh_status();
    }

    // ---- build_layout helpers (split out of `Widget::build_layout`, which
    // assembles their results into the content row and outer column) ----

    /// Lay out the sidebar node: docked (a real column, eased by the drawer's
    /// reveal) or a drawer (collapsed to zero width — it paints off-flow,
    /// sliding in over the content; see `draw`).
    fn build_sidebar(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.drawer.scale_for(theme);
        let layout = self.sidebar_layout();
        let sidebar_node = self.sidebar.build_layout(taffy, theme, node_map);
        let docked_w = match layout {
            SplitLayout::Docked => self.drawer.resolved_docked_width() * self.drawer.reveal(),
            SplitLayout::Overlay => 0.0,
        };
        let _ = taffy.set_style(
            sidebar_node,
            Style {
                size: Size {
                    width: Dimension::length(docked_w as f32),
                    height: Dimension::percent(1.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        self.sidebar_node = Some(sidebar_node);
        sidebar_node
    }

    /// Lay out the editor column: file header and panel toggle, find bar, editor,
    /// (when visible) the bottom panel, and the status bar, stacked into one
    /// flex column.
    ///
    /// # Panics
    /// Panics if taffy's internal tree storage is corrupted — this only
    /// inserts freshly built child nodes into a tree this same call already
    /// holds a mutable reference to, so there is no data-dependent failure
    /// path to recover from.
    #[allow(
        clippy::too_many_lines,
        reason = "one flex column assembled in one place: each row's style and its node stay adjacent so the stacking order reads top to bottom"
    )]
    fn build_editor_column(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let spacing = &theme.tokens.spacing;
        self.header_h = f64::from(theme.tokens.metrics.header_height);
        self.toggle_slot_w = Self::toggle_slot_width(theme);

        let header_node = self.header.build_layout(taffy, theme, node_map);
        if let Ok(existing) = taffy.style(header_node).cloned() {
            let inset = if self.toggle_in_tab_strip() {
                self.toggle_slot_w
            } else {
                f64::from(spacing.s6)
            };
            let _ = taffy.set_style(
                header_node,
                Style {
                    flex_grow: 1.0,
                    flex_basis: Dimension::length(0.0),
                    min_size: Size {
                        width: Dimension::length(0.0),
                        height: Dimension::auto(),
                    },
                    margin: taffy::Rect {
                        left: taffy::LengthPercentageAuto::length(inset as f32),
                        ..existing.margin
                    },
                    ..existing
                },
            );
        }
        self.header_node = Some(header_node);

        self.panel_toggle.set_expanded(self.panel_visible);
        let panel_toggle_node = self.panel_toggle.build_layout(taffy, theme, node_map);
        if let Ok(existing) = taffy.style(panel_toggle_node).cloned() {
            let _ = taffy.set_style(
                panel_toggle_node,
                Style {
                    margin: taffy::Rect {
                        left: taffy::LengthPercentageAuto::length(spacing.s2),
                        right: taffy::LengthPercentageAuto::length(spacing.s3),
                        top: taffy::LengthPercentageAuto::length(0.0),
                        bottom: taffy::LengthPercentageAuto::length(0.0),
                    },
                    ..existing
                },
            );
        }
        self.panel_toggle_node = Some(panel_toggle_node);

        let search_button_node = self.search_button.build_layout(taffy, theme, node_map);
        if let Ok(existing) = taffy.style(search_button_node).cloned() {
            let _ = taffy.set_style(
                search_button_node,
                Style {
                    margin: taffy::Rect {
                        left: taffy::LengthPercentageAuto::length(spacing.s2),
                        right: taffy::LengthPercentageAuto::length(0.0),
                        top: taffy::LengthPercentageAuto::length(0.0),
                        bottom: taffy::LengthPercentageAuto::length(0.0),
                    },
                    ..existing
                },
            );
        }
        self.search_button_node = Some(search_button_node);
        #[allow(
            clippy::unwrap_used,
            reason = "the editor header inserts two freshly-created children into the same app-owned taffy arena"
        )]
        let editor_header_node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    align_items: Some(taffy::AlignItems::Center),
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(self.header_h as f32),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &[header_node, search_button_node, panel_toggle_node],
            )
            .unwrap();
        self.editor_header_node = Some(editor_header_node);

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

        let mut editor_col_children = vec![editor_header_node];
        // The find bar sits between the tabs and the buffer, so it never covers
        // the code it is searching.
        if self.find_open {
            let find_node = self.find.build_layout(taffy, theme, node_map);
            self.find_node = Some(find_node);
            editor_col_children.push(find_node);
        } else {
            self.find_node = None;
        }
        editor_col_children.push(editor_node);
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

    /// What the sidebar column holds: the explorer, or the no-project state.
    fn sidebar_child(&self) -> &dyn Widget {
        if self.has_project() {
            &self.sidebar
        } else {
            &self.sidebar_empty
        }
    }

    // ---- handle_event helpers (split out of `Widget::handle_event`, which
    // dispatches to them in the same order they used to run inline) ----

    /// Track the pointer for the controls this root draws itself.
    fn track_pointer(&mut self, event: &WindowEvent, scale_factor: f64) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
    }

    /// The drawer's own parts: its toggle, the show/hide shortcut, Escape
    /// and a press on the scrim of an open floating drawer. Returns `true`
    /// when the drawer took the event.
    fn handle_sidebar_chrome(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        self.drawer.resolve(self.rect);
        if self.drawer.handle_event(event, ctx) {
            *ctx.handled = true;
            return true;
        }
        false
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
                *focus_target = self.panel_toggle_node;
            }
            *handled = true;
            return true;
        }
        false
    }

    /// The find bar takes keyboard input while it is open and focused (its
    /// field owns typing; Enter, F3 and Escape are its transport). Returns
    /// `true` when the find bar newly consumed the event (the caller should
    /// stop routing).
    fn handle_find_bar(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        if !self.find_open {
            return false;
        }
        let Some(n) = self.find_node else {
            return false;
        };
        let before = *ctx.handled;
        self.find
            .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        self.drive_find(&mut ctx.state);
        *ctx.handled && !before
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

    /// Route the event to the sidebar (when it is on screen), header, bottom
    /// panel, and editor, plus the drawer's own click-capture.
    ///
    /// Sidebar routing depends on the layout:
    ///  * Docked: route whenever the column has width (reveal > 0); the tree
    ///    hit-tests against the rect `place_at` gave it.
    ///  * Drawer: route only while it is open.
    fn route_to_children(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        overlay: bool,
    ) {
        if self.sidebar_visible()
            && let Some(n) = self.sidebar_node
        {
            if self.has_project() {
                self.sidebar
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            } else {
                // The no-project state offers to open one.
                self.sidebar_empty
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
                if self.sidebar_empty.take_action() {
                    self.request_open_folder();
                }
            }
        }

        // While the drawer is open, it captures pointer presses over its area
        // so clicks don't leak to the content behind it (the tree only
        // self-marks `handled` on a row hit — empty rows would otherwise fall
        // through). The scrim outside it was already handled above.
        if overlay
            && self.sidebar_open()
            && let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
            && self.drawer.sidebar_contains(self.last_cursor)
        {
            *ctx.handled = true;
        }

        // A press in the editor takes keyboard focus off the find bar, whose
        // keys the editor would otherwise lose.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.editor.get_rect().contains(self.last_cursor)
        {
            self.find.set_focus(false);
        }

        if let Some(n) = self.search_button_node {
            self.search_button
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
        if let Some(n) = self.panel_toggle_node {
            self.panel_toggle
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
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

/// Character-addressed replacement used to prepare a text action from editor
/// UI state. It intentionally operates on an ordinary temporary `String`;
/// the following action dispatch is the only CRDT mutation path.
fn replace_chars(text: &mut String, start: usize, delete: usize, replacement: &str) {
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

impl Widget for CodeApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::widget::Continuable
    );

    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::text_operations::SNAPSHOT_READERS
    }

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

    #[allow(
        clippy::too_many_lines,
        reason = "the widget lifecycle deliberately sequences every asynchronous projection in one auditable frame boundary"
    )]
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
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
        self.refresh_find();
        self.refresh_header();
        self.header.sync(state, path);
        self.search_button.sync(state, path);
        self.panel_toggle.sync(state, path);
        self.refresh_diagnostics(state);
        self.refresh_status();
        let chrome = (self.panel_visible, self.panel.active_tab(), self.find_open);
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
        if let Some(command) = self.pending_command.take() {
            self.apply_command(command, state);
        }
        if let Some((node, target)) = self.pending_save.take() {
            self.notice = None;
            let saved = self.save_to(state, node, target);
            self.finish_file_effect(if saved {
                Ok(())
            } else {
                Err("file could not be saved".into())
            });
        }
        self.dispatch_editor_text_edits(state);
        self.drive_find(state);
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
        // --- Sidebar ---
        //
        // Docked (wide): the sidebar occupies a real column, its width eased by
        // the drawer's reveal so it collapses/expands smoothly and the editor
        // is inset by exactly that much. Drawer (narrow): the column collapses
        // to zero so the content spans the full width — the sidebar is instead
        // painted off-flow, sliding in over the content (see `draw`).
        let sidebar_node = self.build_sidebar(taffy, theme, node_map);

        // --- Editor column: file header + editor + (optional) bottom panel ---
        let editor_col = self.build_editor_column(taffy, theme, node_map);

        // --- Content row: sidebar + editor column ---
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
                &[sidebar_node, editor_col],
            )
            .unwrap();
        self.content_node = Some(content_node);

        // --- Outer column ---
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[content_node], node_map, self)
    }

    #[allow(
        clippy::unwrap_used,
        reason = "Widget::apply_layout is infallible by trait contract; every node looked up here was created by this widget's own build_layout in the same tree, so a lookup failure means the taffy tree is already corrupted and there is no meaningful layout to fall back to"
    )]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = ui::widget::placed(layout, origin);
        let new_origin = Point::new(self.rect.x0, self.rect.y0);

        // Cache the real measured width so `sidebar_layout()` is consistent
        // across passes (it's read in `build_layout`, before this runs), and
        // let the drawer adopt the new mode's geometry (wide => docked open,
        // narrow => drawer closed) before the next draw.
        self.last_width = self.rect.width();
        self.drawer.resolve(self.rect);

        if let Some(content_node) = self.content_node {
            let content_layout = taffy.layout(content_node).unwrap();
            let content_origin = Point::new(
                new_origin.x + f64::from(content_layout.location.x),
                new_origin.y + f64::from(content_layout.location.y),
            );
            if let Some(n) = self.sidebar_node {
                self.sidebar.apply_layout(taffy, n, content_origin);
            }
            // Editor column origin within the content row.
            if let Some(col) = self.editor_col_node {
                let col_layout = taffy.layout(col).unwrap();
                let col_origin = Point::new(
                    content_origin.x + f64::from(col_layout.location.x),
                    content_origin.y + f64::from(col_layout.location.y),
                );
                if let Some(header_node) = self.editor_header_node {
                    let header_layout = taffy.layout(header_node).unwrap();
                    let header_origin = Point::new(
                        col_origin.x + f64::from(header_layout.location.x),
                        col_origin.y + f64::from(header_layout.location.y),
                    );
                    if let Some(n) = self.header_node {
                        self.header.apply_layout(taffy, n, header_origin);
                    }
                    if let Some(n) = self.search_button_node {
                        self.search_button.apply_layout(taffy, n, header_origin);
                    }
                    if let Some(n) = self.panel_toggle_node {
                        self.panel_toggle.apply_layout(taffy, n, header_origin);
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
                if let Some(n) = self.status_node {
                    let status = taffy.layout(n).unwrap();
                    self.status_rect = Rect::new(
                        col_origin.x + f64::from(status.location.x),
                        col_origin.y + f64::from(status.location.y),
                        col_origin.x + f64::from(status.location.x) + f64::from(status.size.width),
                        col_origin.y + f64::from(status.location.y) + f64::from(status.size.height),
                    );
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
        // The framework painted the window ground; each region below stands on
        // its own tier. Advance the drawer's easing (frame-rate independent)
        // before computing geometry, so the slide and scrim fade animate while
        // the host keeps redrawing (`needs_animation_frame`).
        self.drawer.scale_for(theme);
        self.drawer.resolve(self.rect);
        self.drawer.tick(theme);
        let layout = self.sidebar_layout();
        self.window_header_row(theme);

        if layout == SplitLayout::Docked && self.sidebar_visible() {
            self.draw_sidebar(scene, transform, text_renderer, theme, presence);
        }
        self.draw_content(scene, transform, text_renderer, theme, presence);
        if layout == SplitLayout::Overlay && self.sidebar_visible() {
            // The scrim dims the content, then the drawer floats over it.
            self.drawer.draw_scrim(scene, transform, theme);
            self.draw_sidebar(scene, transform, text_renderer, theme, presence);
        }
        if self.toggle_in_tab_strip() {
            // With no docked sidebar column to hold it the toggle leads the
            // file header.
            self.drawer
                .paint_toggle(scene, transform, text_renderer, theme);
        }

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

        if self.handle_sidebar_chrome(event, ctx) {
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

        // The find bar takes keyboard input while it is open (its fields own
        // typing; Enter and Escape are its transport).
        if self.handle_find_bar(event, ctx) {
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
            *ctx.focus_target = self.panel_toggle_node;
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
        let overlay = self.sidebar_layout() == SplitLayout::Overlay;
        self.route_to_children(event, ctx, overlay);

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

        // Drain whatever the children produced, then refresh derived views.
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
        if target == "sidebar-toggle" && matches!(action, ui::a11y::Action::Activate) {
            self.toggle_sidebar();
            return true;
        }
        false
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
        // The sidebar toggle is painted by the toolkit, so it is published here.
        children.extend(self.drawer.toggle_accessibility());
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
                .with_name(format!("Code — {}", self.session.project.name))
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
        let mut out: Vec<&dyn Widget> = vec![
            self.sidebar_child(),
            &self.header,
            &self.search_button,
            &self.panel_toggle,
        ];
        if self.find_open {
            out.push(&self.find);
        }
        out.push(&self.editor);
        if self.panel_visible {
            out.push(&self.panel);
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let has_project = self.has_project();
        let mut out: Vec<&mut (dyn Widget + '_)> = vec![
            if has_project {
                &mut self.sidebar
            } else {
                &mut self.sidebar_empty
            },
            &mut self.header,
            &mut self.search_button,
            &mut self.panel_toggle,
        ];
        if self.find_open {
            out.push(&mut self.find);
        }
        out.push(&mut self.editor);
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
    /// is still streaming its hits, or the sidebar is still sliding.
    fn has_pending_visual_work(&self) -> bool {
        self.staging.is_some() || self.search_job.is_some() || self.drawer.is_animating()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.sidebar.needs_redraw()
            || self.header.needs_redraw()
            || self.search_button.needs_redraw()
            || self.panel_toggle.needs_redraw()
            || self.editor.needs_redraw()
            || self.panel.needs_redraw()
            || self.find.needs_redraw()
            || self.question().is_some()
            || self.drawer.is_animating()
            // A live terminal repaints continuously while visible.
            || self.terminal_visible()
    }

    /// Advance the explorer and drain the retained terminal before settlement.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = self.sidebar.update(state, path);
        changed |= self.editor.update(state, path);
        if let Some(terminal) = self.terminal.as_mut() {
            changed |= terminal.widget_mut().update(state, path);
        }
        changed
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the event loop pumping frames while the sidebar drawer is
        // mid-transition (so the slide + scrim fade play out) OR while a live
        // terminal is visible — its PTY is drained during update, which the
        // App repeats while an animation frame is requested.
        //
        // A running search or an in-flight file picker also needs frames: both
        // deliver over a channel drained in `sync`, so without a frame their
        // results would sit unread until the user happened to move the mouse.
        // A language server that is still working needs them too, so the
        // indexing readout actually advances.
        self.drawer.is_animating()
            || self.sidebar.needs_animation_frame()
            || self.panel.needs_animation_frame()
            || self.header.needs_animation_frame()
            || self.terminal_visible()
            || self.search_job.is_some()
            || self.portal.is_some()
            || self.lsp_status.is_working()
    }

    fn take_structure_dirty(&mut self) -> bool {
        // While the drawer eases in DOCKED mode the sidebar column's width is
        // taffy-driven (set from `reveal` in `build_layout`), so the tree must
        // be rebuilt each animating frame to widen/collapse smoothly. A
        // floating drawer computes its geometry in `draw` and needs no rebuild.
        std::mem::take(&mut self.structure_dirty)
            || self.sidebar_layout() == SplitLayout::Docked && self.drawer.is_animating()
    }

    fn mark_drawn(&mut self) {
        self.sidebar.mark_drawn();
        self.header.mark_drawn();
        self.search_button.mark_drawn();
        self.panel_toggle.mark_drawn();
        self.editor.mark_drawn();
        self.panel.mark_drawn();
        self.find.mark_drawn();
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
