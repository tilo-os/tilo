//! The hex view widget — the whole editor surface in one `Widget`.
//!
//! A hex editor is fundamentally a dense, fixed-pitch grid with a cursor
//! synced across two parallel columns; that is a canvas, not a composition of
//! stock form widgets, so the view paints the grid itself ([`paint`]) and
//! reads every colour and metric from the `Theme`. Everything around the
//! grid is the toolkit's:
//!
//!   * header    — the file name and an edit-mode strip, on the content with
//!     one hairline under them
//!   * find      — the framework [`FindBar`](ui::widget::FindBar), a band
//!     under the header; the view is its `Findable`
//!   * body      — offset gutter | hex byte columns | ASCII column
//!   * inspector — a [`SidebarDrawer`] at the trailing edge: docked beside
//!     the content when there is room, a floating drawer when there is not,
//!     holding the endianness strip and a property list of interpretations
//!   * status    — the persistent status bar (cursor offset, selection,
//!     edited bytes, size); "Saved" and failures are transient notices
//!   * go to     — a popover with one field, anchored at the status bar
//!
//! File paths and byte contents stay in this device-local view model. The
//! canonical resource operations remain palette/agent-driveable, but never
//! become collaborative document changes.

mod find;
mod goto;
mod layout;
mod paint;

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::TryRecvError;
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::format::bytes as format_bytes;
use ui::icons::Icon;
use ui::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, PickRequest, PortalChannel, PortalOutcome,
    SaveChannel, SaveOutcome,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    ControlSize, EmptyState, FindBar, FindMatch, Popover, PopoverSide, SegmentedStyle,
    SidebarDrawer, ToggleGroup,
};

use crate::actions::HexOperation;
use crate::model::{Column, EditMode, Endian, HexModel, LocalEdit};
use goto::GotoForm;
use layout::{CellMetrics, ENDIAN_LABELS, Geo, MODE_LABELS};
use ui::widget::toggle_group::Segment;

/// How long a failure toast stays up.
const FLASH_MS: u128 = 3500;
const LOCAL_HISTORY_LIMIT: usize = 256;

fn file_open_request() -> PickRequest {
    PickRequest::open("Open file", "Open")
        .thread_name("hex-portal-open")
        .filter(FileFilter::all_files())
}

fn file_save_request(suggested_name: &str) -> PickRequest {
    PickRequest::save("Save file", "Save", suggested_name)
        .thread_name("hex-portal-save")
        .filter(FileFilter::all_files())
}

/// The no-file zero-state: one sentence and the one action.
fn empty_state() -> EmptyState {
    EmptyState::new("No file open")
        .with_icon(Icon::File)
        .with_message("Open a file to inspect and edit its bytes.")
        .with_action("Open file")
        .with_action_id("hex.open")
}

/// A segmented strip over `labels` whose picks land in `pick`.
fn strip(id: &str, labels: &[&str], pick: &Rc<Cell<Option<usize>>>) -> ToggleGroup {
    let pick = Rc::clone(pick);
    ToggleGroup::new(labels.iter().map(|l| Segment::new(*l)).collect(), 0)
        .with_id(id)
        .control_size(ControlSize::Small)
        .with_style(SegmentedStyle::Segmented)
        .on_change(move |index| pick.set(Some(index)))
}

// The three bools below are independent view flags (redraw-pending,
// buffer-present, find band shown), not a combinatorial option set, so
// folding them into an enum would only add indirection.
#[allow(
    clippy::struct_excessive_bools,
    reason = "unrelated view flags (dirty/has_file/find_open/toggle_hovered), not a combinatorial option set"
)]
pub struct HexView {
    /// Decoded local buffer and editor state. Refreshed from local I/O only.
    model: HexModel,
    /// File name shown in the header (cosmetic; the buffer is local).
    file_name: String,
    rect: Rect,
    dirty: bool,

    // --- find (per-peer; not in the shared doc) ---
    /// The framework's find bar, hosted under the header while open.
    find_bar: FindBar,
    find_open: bool,
    /// Every match of the bar's query, as byte ranges, in order.
    matches: Vec<FindMatch>,
    /// The match the bar is on.
    match_idx: Option<usize>,

    // --- go to ---
    /// The go-to-offset form, in a popover at the status bar.
    goto: Popover<GotoForm>,

    // --- chrome ---
    /// The inspector: docked when wide, a floating drawer when narrow.
    drawer: SidebarDrawer,
    /// The window's History button, presented in this header so the window
    /// draws no History bar above the editor.
    history: ui::widget::Button,
    /// The edit-mode strip in the header, and the pick the person made.
    mode_group: ToggleGroup,
    mode_pick: Rc<Cell<Option<usize>>>,
    /// The endianness strip in the inspector, and the pick.
    endian_group: ToggleGroup,
    endian_pick: Rc<Cell<Option<usize>>>,
    /// The zero-state, shown while no file is open.
    empty: EmptyState,
    /// How far the inspector's properties are scrolled.
    inspector_scroll: f64,
    /// The grid's scroll bar.
    scrollbar: ui::widget::scrollbar::Scrollbar,
    toggle_hovered: bool,
    /// A capture asks for the inspector open once the window size is known.
    reveal_inspector: bool,
    /// Where everything is; refreshed by [`HexView::relayout`].
    geo: Geo,
    /// The monospace advance and line height measured from the renderer that
    /// paints the grid. Layout, paint and hit-testing all derive their
    /// pitches from this one measurement, so a byte's painted cell is its
    /// click target.
    cell: CellMetrics,
    /// The theme the last layout pass saw, so the event path resolves the
    /// same geometry as the last paint.
    layout_theme: Theme,
    /// Rows currently fitting in the grid viewport — read by keyboard paging
    /// so PageUp/Down move a screenful.
    visible_rows: usize,
    /// Last pointer position in logical coords, tracked from `CursorMoved`
    /// so a left-click can hit-test the cell under the cursor.
    last_mouse: Point,

    // --- file / portal state ---
    /// False until a real buffer is loaded — drives the composed empty
    /// state (no fabricated buffer in the live app).
    has_file: bool,
    /// The on-disk path this buffer came from / was last saved to, if any.
    /// `Ctrl+S` writes straight back here; absent, it opens a Save chooser.
    path: Option<PathBuf>,
    /// An in-flight XDG-portal open request, drained once per frame in `sync`.
    open_rx: Option<PortalChannel>,
    /// A chooser delivering contents, where files have no path (the web).
    open_contents_rx: Option<ContentsChannel>,
    save_contents_rx: Option<SaveChannel>,
    /// An in-flight XDG-portal save-destination request.
    save_rx: Option<PortalChannel>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    pending_resource_io: Option<(tilo_actions::InvocationId, crate::actions::HexOperation)>,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    /// A failed open or save: its title, the reason and its birth time. Shown
    /// as a toast until it expires.
    problem: Option<(String, String, Instant)>,

    /// Bounded, device-local undo/redo journal. Resource operations and
    /// keyboard shortcuts use the same inverse edit representation.
    undo: Vec<LocalEdit>,
    redo: Vec<LocalEdit>,

    /// "Saved" and "Opened": a change worth noticing once, floating at the
    /// content's bottom-trailing corner and fading by itself.
    status_bubble: ui::StatusBubble,
    /// The canvas is keyboard-focusable even though it is custom drawn.
    focused: bool,
    /// Local host for standalone tests; the framework dispatcher once the
    /// surface is attached. Hosted publication keeps only live cursor extras.
    pub(crate) actions: crate::resource::ActionSink,
}

impl HexView {
    pub fn new(bytes: Vec<u8>, file_name: impl Into<String>) -> Self {
        let mode_pick = Rc::new(Cell::new(None));
        let endian_pick = Rc::new(Cell::new(None));
        Self {
            model: HexModel::new(bytes),
            file_name: file_name.into(),
            rect: Rect::ZERO,
            dirty: true,
            find_bar: find::new_find_bar(),
            find_open: false,
            matches: Vec::new(),
            match_idx: None,
            goto: Popover::with_content(GotoForm::new())
                .side(PopoverSide::Above)
                .with_label("Go to offset"),
            drawer: SidebarDrawer::new()
                .right()
                .docked_width(f64::from(ui::tokens::Metrics::BASE.inspector_width)),
            history: ui::widget::history_button().control_size(ControlSize::Small),
            mode_group: strip("hex.mode", &MODE_LABELS, &mode_pick),
            mode_pick,
            endian_group: strip("hex.endian", &ENDIAN_LABELS, &endian_pick).fill(),
            endian_pick,
            empty: empty_state(),
            inspector_scroll: 0.0,
            scrollbar: ui::widget::scrollbar::Scrollbar::vertical(),
            toggle_hovered: false,
            reveal_inspector: false,
            geo: Geo::default(),
            cell: CellMetrics::FALLBACK,
            layout_theme: Theme::dark(),
            visible_rows: 1,
            last_mouse: Point::ZERO,
            has_file: true,
            path: None,
            open_rx: None,
            open_contents_rx: None,
            save_contents_rx: None,
            save_rx: None,
            resource_invocation: None,
            pending_resource_io: None,
            resource_result: None,
            problem: None,
            undo: Vec::new(),
            redo: Vec::new(),
            status_bubble: ui::StatusBubble::new(),
            focused: false,
            actions: crate::resource::ActionSink::default(),
        }
    }

    /// Whether this surface is under the framework [`ResourceApp`] host.
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test-only: mark hosted without constructing a framework dispatcher.
    /// Standalone tests stay on the local action host.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// The editor with no file loaded — the honest live default. Shows a
    /// composed empty state whose only affordance is opening a file; it never
    /// fabricates a buffer.
    pub fn empty() -> Self {
        let mut v = Self::new(Vec::new(), "No file");
        v.has_file = false;
        // Where files have no path, the file last opened by contents.
        if !crate::web_file::native_paths()
            && let Some(file) = crate::web_file::recall()
        {
            v.load_bytes(file.name, file.bytes, None);
        }
        v
    }

    /// Record the on-disk path this buffer came from, so `Ctrl+S` writes back
    /// to it without a chooser.
    #[must_use]
    pub fn with_path(mut self, path: PathBuf) -> Self {
        self.path = Some(path);
        self
    }

    /// Test/read accessor for the decoded model.
    pub fn model(&self) -> &HexModel {
        &self.model
    }

    /// Mutable accessor for the decoded model — used by tests to position the
    /// cursor / selection that drive the resource tree and the edit journal.
    #[cfg(test)]
    pub fn model_mut(&mut self) -> &mut HexModel {
        &mut self.model
    }

    /// The presentation context Ctrl+K needs to target its byte-edit actions at
    /// the live cursor / selection. Everything here is view state the document
    /// doesn't own.
    pub(crate) fn hex_resource_ctx(&self) -> crate::actions::HexResourceCtx {
        crate::actions::HexResourceCtx {
            label: if self.file_name.is_empty() {
                "Buffer".to_string()
            } else {
                self.file_name.clone()
            },
            cursor: self.model.cursor(),
            len: self.model.len(),
            selection: self.model.selection(),
            has_file: self.has_file,
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
        }
    }

    /// Whether a real buffer is loaded (false in the empty state).
    pub fn has_file(&self) -> bool {
        self.has_file
    }

    /// The on-disk path currently backing the buffer, if any.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Place the initial cursor at `offset`. Useful for opening a file
    /// "at" a field of interest. Clamped to the buffer.
    #[must_use]
    pub fn with_cursor(mut self, offset: usize) -> Self {
        self.model.move_to(offset, false);
        self
    }

    /// Choose the initial inspector endianness. Big-endian suits opening
    /// network / image formats whose multi-byte fields are big-endian.
    #[must_use]
    pub fn with_big_endian(mut self) -> Self {
        if self.model.endian() != Endian::Big {
            self.model.toggle_endian();
        }
        self
    }

    /// Show the bundled sample (a complete PNG) with the cursor on its width
    /// field, so a capture reads real, structured data.
    pub(crate) fn load_sample(&mut self) {
        self.load_bytes("sample.png".to_string(), crate::sample::png_image(), None);
        if self.model.endian() != Endian::Big {
            self.model.toggle_endian();
        }
        self.model.move_to(0x10, false);
    }

    /// Record bounded inverse edits for a completed local input. Neither bytes
    /// nor paths enter the collaboration document.
    fn record_local_history(&mut self) {
        let edits = self.model.take_local_history();
        if !edits.is_empty() {
            self.undo.extend(edits);
            let excess = self.undo.len().saturating_sub(LOCAL_HISTORY_LIMIT);
            if excess > 0 {
                self.undo.drain(..excess);
            }
            self.redo.clear();
            self.refind();
        }
        self.dirty = true;
    }

    /// A change worth noticing once ("Saved", "Opened"): the bubble.
    fn notice(&mut self, message: &str) {
        let now = ui::time::now();
        self.status_bubble.show(message, now);
        self.status_bubble.poke(now);
        self.dirty = true;
    }

    /// A failed open or save: a toast with the reason.
    fn problem(&mut self, title: &str, reason: impl Into<String>) {
        self.problem = Some((title.to_owned(), reason.into(), ui::time::now()));
        self.dirty = true;
    }

    // --- file open / save (through the host portal) --------------------

    /// Start the host file chooser to open a file. No-op if one is already in
    /// flight. The pick arrives over `open_rx`, drained in `sync`.
    fn begin_open(&mut self) {
        if self.open_rx.is_some() || self.open_contents_rx.is_some() {
            return;
        }
        if crate::web_file::native_paths() {
            self.open_rx = Some(file_open_request().send());
        } else {
            self.open_contents_rx = Some(file_open_request().send_contents());
        }
    }

    /// Save the buffer. With a known path this writes straight back; without
    /// one (a never-saved buffer) it opens the host Save chooser.
    fn begin_save(&mut self) {
        if let Some(path) = self.path.clone() {
            let _ = self.write_to(&path);
        } else if !crate::web_file::native_paths() {
            if self.save_contents_rx.is_none() {
                let name = self.suggested_name();
                crate::web_file::remember(&name, self.model.bytes());
                self.save_contents_rx =
                    Some(file_save_request(&name).send_save_contents(self.model.bytes().to_vec()));
            }
        } else if self.save_rx.is_none() {
            self.save_rx = Some(file_save_request(&self.suggested_name()).send());
        }
    }

    /// The filename the Save dialog seeds with — the current name, or a
    /// sensible default for a scratch buffer.
    fn suggested_name(&self) -> String {
        if self.file_name.is_empty() || !self.has_file {
            "untitled.bin".to_string()
        } else {
            self.file_name.clone()
        }
    }

    /// Write the current bytes to `path`, clearing the dirty markers on
    /// success and flashing the outcome either way.
    fn write_to(&mut self, path: &std::path::Path) -> Result<(), String> {
        let saved = tilo_files::file_at(path).and_then(|(dir, name)| {
            tilo_files::blocking::save_document(&dir, &name, self.model.bytes())
        });
        match saved {
            Ok(()) => {
                self.model.clear_dirty();
                self.path = Some(path.to_path_buf());
                self.notice(&format!(
                    "Saved {}",
                    format_bytes(u64::try_from(self.model.len()).unwrap_or(u64::MAX))
                ));
                Ok(())
            }
            Err(e) => {
                self.problem("Couldn't save the file", e.to_string());
                Err(e.to_string())
            }
        }
    }

    /// Load a local path into this editor. The path and bytes stay local.
    fn open_path(&mut self, path: PathBuf) -> Result<(), String> {
        match crate::app::read_file(&path) {
            Ok(bytes) => {
                let name = path.file_name().map_or_else(
                    || "untitled".to_string(),
                    |s| s.to_string_lossy().into_owned(),
                );
                let n = self.load_bytes(name, bytes, Some(path));
                self.notice(&format!(
                    "Opened {}",
                    format_bytes(u64::try_from(n).unwrap_or(u64::MAX))
                ));
                Ok(())
            }
            Err(e) => {
                self.problem("Couldn't open the file", e.to_string());
                Err(e.to_string())
            }
        }
    }

    /// Show `bytes` as the open file; its length.
    fn load_bytes(&mut self, name: String, bytes: Vec<u8>, path: Option<PathBuf>) -> usize {
        let n = bytes.len();
        self.model = HexModel::new(bytes);
        self.file_name = name;
        self.path = path;
        self.has_file = true;
        self.find_open = false;
        self.matches.clear();
        self.match_idx = None;
        self.undo.clear();
        self.redo.clear();
        self.inspector_scroll = 0.0;
        self.dirty = true;
        n
    }

    /// A file chosen by contents: shown, and remembered so it outlives a
    /// reload.
    fn open_contents(&mut self, outcome: ContentsOutcome) -> Result<(), String> {
        match outcome {
            ContentsOutcome::Picked(files) => {
                let Some(file) = files.into_iter().next() else {
                    return Err("no file was chosen".into());
                };
                crate::web_file::remember(&file.name, &file.bytes);
                let n = self.load_bytes(file.name, file.bytes, None);
                self.notice(&format!(
                    "Opened {}",
                    format_bytes(u64::try_from(n).unwrap_or(u64::MAX))
                ));
                Ok(())
            }
            ContentsOutcome::Cancelled => Err("cancelled".into()),
            ContentsOutcome::Failed(reason) => {
                self.problem("Couldn't open the file", reason.clone());
                Err(reason)
            }
        }
    }

    /// Drain completed portal open/save requests into this device-local view.
    fn drain_portal(&mut self) {
        if let Some(rx) = self.open_contents_rx.as_ref() {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.open_contents_rx = None;
                    let result = self.open_contents(outcome);
                    self.finish_resource_io(result);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.open_contents_rx = None;
                    self.finish_resource_io(Err("file picker disconnected".into()));
                }
            }
        }
        if let Some(rx) = self.save_contents_rx.as_ref() {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.save_contents_rx = None;
                    let result = match outcome {
                        SaveOutcome::Saved => {
                            self.model.clear_dirty();
                            self.notice("Saved");
                            Ok(())
                        }
                        SaveOutcome::Cancelled => Err("cancelled".into()),
                        SaveOutcome::Failed(reason) => {
                            self.problem("Couldn't save the file", reason.clone());
                            Err(reason)
                        }
                    };
                    self.finish_resource_io(result);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.save_contents_rx = None,
            }
        }
        if let Some(rx) = self.open_rx.as_ref() {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.open_rx = None;
                    let result = match outcome {
                        PortalOutcome::Opened(path) => self.open_path(path),
                        PortalOutcome::Cancelled => Err("cancelled".into()),
                        PortalOutcome::Failed(reason) => {
                            self.problem("Couldn't open the file", reason.clone());
                            Err(reason)
                        }
                    };
                    self.finish_resource_io(result);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.open_rx = None;
                    self.finish_resource_io(Err("file picker disconnected".into()));
                }
            }
        }
        if let Some(rx) = self.save_rx.as_ref() {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.save_rx = None;
                    let result = match outcome {
                        PortalOutcome::Opened(dest) => self.write_to(&dest),
                        PortalOutcome::Cancelled => Err("cancelled".into()),
                        PortalOutcome::Failed(reason) => {
                            self.problem("Couldn't save the file", reason.clone());
                            Err(reason)
                        }
                    };
                    self.finish_resource_io(result);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.save_rx = None;
                    self.finish_resource_io(Err("file picker disconnected".into()));
                }
            }
        }
    }

    fn finish_resource_io(&mut self, result: Result<(), String>) {
        let Some(invocation) = self.resource_invocation.take() else {
            return;
        };
        self.resource_result = Some(match result {
            Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            Err(message) if message == "cancelled" => tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(message),
            },
            Err(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "hex.io_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        });
    }

    /// True while any portal request is in flight or a failure toast is
    /// counting down — the app keeps pumping frames so the pick drains and the
    /// toast expires.
    fn is_busy(&self) -> bool {
        self.open_rx.is_some()
            || self.open_contents_rx.is_some()
            || self.save_rx.is_some()
            || self.save_contents_rx.is_some()
            || self.problem.is_some()
    }

    /// Parse a go-to offset: `0x1F` hex, bare hex digits that include a
    /// letter, or plain decimal.
    fn parse_offset(query: &str) -> Option<usize> {
        let q = query.trim();
        if let Some(hex) = q.strip_prefix("0x").or_else(|| q.strip_prefix("0X")) {
            usize::from_str_radix(hex, 16).ok()
        } else if q.chars().all(|c| c.is_ascii_hexdigit())
            && q.chars().any(|c| c.is_ascii_alphabetic())
        {
            usize::from_str_radix(q, 16).ok()
        } else {
            q.parse::<usize>().ok()
        }
    }

    /// Move the cursor to the offset the person typed and bring it into view.
    fn commit_goto(&mut self, query: &str) {
        if let Some(off) = Self::parse_offset(query) {
            self.model.move_to(off, false);
            let (row, _) = self.model.cursor_cell();
            self.model.scroll_into_view(row, self.visible_rows);
            self.record_local_history();
        }
        self.dirty = true;
    }

    /// Open the go-to form (Ctrl+G) at the status bar's offset readout.
    fn open_goto(&mut self) {
        self.goto.set_open(true);
        self.goto.content_mut().focus();
        self.dirty = true;
    }

    /// Apply what the header and inspector strips picked since the last
    /// event.
    fn apply_strip_picks(&mut self) {
        if let Some(index) = self.mode_pick.take() {
            let want = if index == 0 {
                EditMode::Overwrite
            } else {
                EditMode::Insert
            };
            if self.model.mode() != want {
                self.model.toggle_mode();
            }
            self.dirty = true;
        }
        if let Some(index) = self.endian_pick.take() {
            let want = if index == 0 {
                Endian::Little
            } else {
                Endian::Big
            };
            if self.model.endian() != want {
                self.model.toggle_endian();
            }
            self.dirty = true;
        }
    }

    /// Hand a pointer event to the two strips; true when one took it.
    fn route_to_strips(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        self.history.handle_event(event, ctx);
        self.mode_group.handle_event(event, ctx);
        if self.geo.inspector_visible {
            self.endian_group.handle_event(event, ctx);
        }
        let took = *ctx.handled;
        self.apply_strip_picks();
        took
    }

    /// The pointer moved: the thumb under it lights up, and a thumb being
    /// dragged scrolls the grid.
    fn follow_scrollbar(&mut self) {
        let (track, metrics) = (self.geo.scroll_track, self.scroll_metrics());
        if !metrics.overflows() {
            return;
        }
        if let Some(offset) = self.scrollbar.drag_to(track, metrics, self.last_mouse) {
            self.model.set_top_row(offset.round() as usize);
            self.dirty = true;
        }
        if self.scrollbar.update_hover(track, metrics, self.last_mouse) {
            self.dirty = true;
        }
    }

    /// A press on the scroll bar: grab the thumb or page toward the press.
    fn press_scrollbar(&mut self, p: Point) -> bool {
        use ui::widget::scrollbar::PressOutcome;
        let (track, metrics) = (self.geo.scroll_track, self.scroll_metrics());
        if !metrics.overflows() || !track.contains(p) {
            return false;
        }
        match self.scrollbar.press(track, metrics, p) {
            PressOutcome::Miss => return false,
            PressOutcome::Thumb => {}
            PressOutcome::Page(offset) => self.model.set_top_row(offset.round() as usize),
        }
        self.dirty = true;
        true
    }

    /// Show or hide the inspector, remembering the choice for this size.
    fn toggle_inspector(&mut self) {
        self.drawer.toggle();
        self.relayout_now();
        self.dirty = true;
    }

    /// Whether `point` is over the inspector drawer.
    fn over_inspector(&self, point: Point) -> bool {
        self.geo.inspector_visible && self.geo.inspector.contains(point)
    }

    /// Scroll the inspector's properties, or the grid.
    fn scroll_wheel(&mut self, delta: &winit::event::MouseScrollDelta) {
        let lines = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
            winit::event::MouseScrollDelta::PixelDelta(p) => p.y / 20.0,
        };
        if self.over_inspector(self.last_mouse) {
            self.inspector_scroll -= lines * self.geo.row_h;
        } else {
            self.model.scroll_by(-(lines.round() as isize) * 3);
        }
        self.dirty = true;
    }

    /// The pointer moved or a button changed: the strips, the inspector
    /// toggle and the drawer's scrim. True when the event was consumed.
    fn handle_chrome_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let hovered = self.geo.toggle.contains(self.last_mouse);
                if hovered != self.toggle_hovered {
                    self.toggle_hovered = hovered;
                    self.dirty = true;
                }
                self.follow_scrollbar();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.scrollbar.release() {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_mouse;
                if self.geo.toggle.contains(p) {
                    self.toggle_inspector();
                    return true;
                }
                if self.drawer.press_dismisses(p) {
                    self.relayout_now();
                    self.dirty = true;
                    return true;
                }
                if self.over_inspector(p) {
                    // A press on the panel never reaches the grid behind it.
                    self.route_to_strips(event, ctx);
                    return true;
                }
                if self.press_scrollbar(p) {
                    return true;
                }
            }
            _ => {}
        }
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        ) {
            self.route_to_strips(event, ctx);
        }
        false
    }

    /// An open find bar owns the keyboard while it has focus, and the
    /// pointer inside its band. True when it took the event.
    fn route_find(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let in_band = self.geo.find.contains(self.last_mouse);
        if matches!(event, WindowEvent::MouseInput { .. }) && in_band {
            Widget::set_focus(&mut self.find_bar, true);
        }
        let took = self.find_bar.route(event, ctx);
        self.drive_find();
        self.dirty = true;
        let typing = Widget::is_focused(&self.find_bar)
            && matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            );
        took || typing || (in_band && matches!(event, WindowEvent::MouseInput { .. }))
    }

    /// Events while the go-to popover is open: it captures input.
    fn handle_goto_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        Widget::handle_event(&mut self.goto, event, ctx);
        if let Some(query) = self.goto.content_mut().take_submit() {
            self.goto.close();
            self.commit_goto(&query);
        }
        if !self.goto.is_open() {
            self.goto.content_mut().reset();
        }
        self.dirty = true;
    }

    /// Events while no file is open: the empty state's action and Ctrl+O.
    fn handle_empty_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.empty.handle_event(event, ctx);
        if self.empty.take_action() {
            self.begin_open();
            *ctx.handled = true;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
        {
            let ctrl = ctx.modifiers.state().control_key();
            self.handle_empty_state_key(&key.logical_key, ctrl, ctx.handled);
        }
    }
}

impl Widget for HexView {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ui::widget::Findable
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::HEX_EDITOR_OPERATION_TYPE]
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
        if cancellation_requested
            && self
                .pending_resource_io
                .as_ref()
                .is_some_and(|(id, _)| *id == invocation)
        {
            self.pending_resource_io = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("cancelled before file I/O".into()),
            });
        }
        if cancellation_requested && self.resource_invocation == Some(invocation) {
            self.resource_invocation = None;
            self.open_rx = None;
            self.save_rx = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("file selection cancelled".into()),
            });
        }
        self.resource_result
            .as_ref()
            .is_some_and(|u| u.invocation() == invocation)
            .then(|| self.resource_result.take())
            .flatten()
    }

    fn sync(&mut self, _doc: &mut AppModel, _path: &ModelPath) {
        if let Some((invocation, operation)) = self.pending_resource_io.take() {
            let result = match operation {
                crate::actions::HexOperation::Open { path: Some(path) } => {
                    self.open_path(path.into())
                }
                crate::actions::HexOperation::Save { path: Some(path) } => {
                    self.write_to(std::path::Path::new(&path))
                }
                _ => Err("missing direct path".into()),
            };
            self.resource_result = Some(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "hex.io_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            });
        }
        // Drain any completed file open/save picks first, so a freshly opened
        // buffer is in place before this frame lays out.
        self.drain_portal();

        // Paths, bytes, selection and cursor are local device state. Never
        // seed or adopt them through Automerge: a collaborator cannot open
        // this machine's path or receive its raw file contents.

        // Expire a failure toast so an idle window settles back to no message.
        if let Some((_, _, born)) = &self.problem
            && born.elapsed().as_millis() > FLASH_MS
        {
            self.problem = None;
            self.dirty = true;
        }
    }

    fn needs_idle_sync(&self) -> bool {
        self.is_busy()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming while a portal pick is in flight (so it drains
        // once the host dialog closes), while a failure toast is counting
        // down, while the bubble is lingering / fading so it hides itself
        // without needing a stray pointer event, and while the inspector
        // slides.
        self.is_busy()
            || self.status_bubble.needs_frame(ui::time::now())
            || (self.has_file && self.drawer.is_animating())
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        // `new_leaf` on a freshly created tree never fails.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh tree cannot fail")]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
        self.relayout_now();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The framework painted the window ground; the geometry is resolved
        // with the renderer that paints it, then the frame follows it.
        self.layout_theme = theme.clone();
        self.relayout(text_renderer, theme, true);
        if self.goto.is_open() {
            let status = self.geo.status;
            let inset = f64::from(theme.tokens.spacing.s4);
            self.goto.set_anchor(Rect::new(
                status.x0 + inset,
                status.y0,
                status.x0 + inset,
                status.y1,
            ));
        }
        self.paint(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let ctrl = ctx.modifiers.state().control_key();
        let shift = ctx.modifiers.state().shift_key();

        if !self.has_file {
            self.handle_empty_event(event, ctx);
            return;
        }
        if self.goto.is_open() {
            self.handle_goto_event(event, ctx);
            return;
        }
        if self.handle_chrome_pointer(event, ctx) {
            *ctx.handled = true;
            return;
        }
        if self.find_open && self.route_find(event, ctx) {
            *ctx.handled = true;
            return;
        }

        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_mouse_press(shift, ctx.handled),
            WindowEvent::MouseWheel { delta, .. } => {
                self.scroll_wheel(delta);
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    ke @ winit::event::KeyEvent {
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.handle_key_event(ke, ctrl, shift, ctx.handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "HexView"
    }

    fn test_id(&self) -> Option<&str> {
        Some("hex_view")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![&self.history];
        if self.goto.is_open() {
            children.push(&self.goto);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.history);
        if self.goto.is_open() {
            visitor(&self.goto);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![&mut self.history];
        if self.goto.is_open() {
            children.push(&mut self.goto);
        }
        children
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.goto.is_open() {
            return self.goto.content().ime_cursor_rect();
        }
        if self.find_open && Widget::is_focused(&self.find_bar) {
            return self.find_bar.ime_cursor_rect();
        }
        None
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if self.goto.is_open() {
            return self.goto.content().ime_purpose();
        }
        if self.find_open && Widget::is_focused(&self.find_bar) {
            return self.find_bar.ime_purpose();
        }
        None
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.goto.is_open() {
            return self.goto.content().cursor_shape(point);
        }
        if self.find_open && self.geo.find.contains(point) {
            return self.find_bar.cursor_shape(point);
        }
        None
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};

        let mut children = vec![
            AccessNode::new(Role::StaticText)
                .with_name(if self.has_file {
                    format!(
                        "{}; {} bytes; cursor at hexadecimal 0x{:X}; {} mode; {}",
                        self.file_name,
                        self.model.len(),
                        self.model.cursor(),
                        self.model.mode().label(),
                        self.model.endian().label(),
                    )
                } else {
                    "No file open. Open a local file to inspect its bytes.".to_string()
                })
                .with_live(Politeness::Polite)
                .with_hashed_key("hex.status"),
        ];
        if self.has_file {
            children.push(
                AccessNode::new(Role::StaticText).with_name(
                    "Keyboard: Ctrl+O open, Ctrl+S save, Ctrl+F find, Ctrl+G go to offset, Ctrl+Z undo, Ctrl+Y redo, Tab switches hex and ASCII, Insert switches overwrite and insert.",
                ).with_hashed_key("hex.keyboard-help"),
            );
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Undo local edit")
                    .with_description("Revert the most recent local byte edit")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .disabled(self.undo.is_empty()),
                    )
                    .with_action_target("hex.undo"),
            );
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Redo local edit")
                    .with_description("Reapply the most recently undone local byte edit")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .disabled(self.redo.is_empty()),
                    )
                    .with_action_target("hex.redo"),
            );
            children.extend(self.mode_group.accessibility());
            if self.geo.inspector_visible {
                children.extend(self.endian_group.accessibility());
            }
            if self.find_open {
                children.extend(self.find_bar.accessibility());
            }
            if self.goto.is_open() {
                children.extend(self.goto.content().accessibility());
            }
        } else {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Open file")
                    .with_description("Choose a local file to inspect")
                    .with_state(AccessState::default().focusable(true))
                    .with_bounds(self.empty.action_rect())
                    .with_action_target("hex.open"),
            );
        }
        Some(
            AccessNode::new(Role::Document)
                .with_name("Hex editor")
                .with_description("Local file bytes are not shared with collaborators.")
                .with_state(AccessState::default().focusable(true).focused(self.focused))
                .with_children(children),
        )
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        self.dirty = true;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if matches!(action, ui::a11y::Action::Activate) {
            match target {
                "hex.open" => self.begin_open(),
                "hex.undo" if self.has_file && !self.undo.is_empty() => {
                    let _ = self.undo_local();
                }
                "hex.redo" if self.has_file && !self.redo.is_empty() => {
                    let _ = self.redo_local();
                }
                _ => return false,
            }
            self.dirty = true;
            return true;
        }
        false
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl ui::resources::PublishesResources for HexView {
    /// Standalone / `--shots` / unit tests publish the document door. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// `hex_editor.document` — keep only live cursor / selection byte extras.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.actions.is_hosted() {
            return crate::actions::live_buffer_extras(&self.hex_resource_ctx());
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.actions.is_hosted() && id == "hex_editor.document" {
            return None;
        }
        find_resource(&self.publish_resources(doc), id)
    }
}

fn find_resource(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_resource(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

impl HexView {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match operation {
            HexOperation::Open { .. } => self.begin_open(),
            HexOperation::Save { .. } if self.has_file => self.begin_save(),
            HexOperation::Search if self.has_file => self.open_find(),
            HexOperation::GotoOffset if self.has_file => self.open_goto(),
            HexOperation::Edit { edit } if self.has_file => {
                // Raw bytes: the sink returns before the framework dispatcher.
                // The buffer stays on this device.
                let action = crate::actions::action_from_wire(&edit);
                self.actions.dispatch_document(
                    doc,
                    &action,
                    ui::tilo_actions::ActionContext::default(),
                );
                self.model.apply_local_edit(&edit.into());
                self.record_local_history();
                self.dirty = true;
            }
            HexOperation::Undo if !self.undo.is_empty() => {
                let _ = self.undo_local();
            }
            HexOperation::Redo if !self.redo.is_empty() => {
                let _ = self.redo_local();
            }
            HexOperation::Undo => return Err("no local edit to undo".into()),
            HexOperation::Redo => return Err("no local edit to redo".into()),
            _ => return Err("open a file first".into()),
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        match &operation {
            crate::actions::HexOperation::Open { .. }
            | crate::actions::HexOperation::Save { .. } => {}
            _ => {
                return self
                    .answer_resource_operation(kind, type_id, payload, doc, path)
                    .map(|handled| {
                        handled.then(|| {
                            ui::widget::ResourceOperationStart::Completed(
                                ui::tilo_actions::ActionOutput::default(),
                            )
                        })
                    });
            }
        }
        if self.resource_invocation.is_some() || self.pending_resource_io.is_some() {
            return Err("another file operation is already running".into());
        }
        match &operation {
            crate::actions::HexOperation::Open { path: None } => {
                self.begin_open();
                self.resource_invocation = Some(invocation);
            }
            crate::actions::HexOperation::Save { path: None } => {
                self.begin_save();
                self.resource_invocation = Some(invocation);
            }
            _ => self.pending_resource_io = Some((invocation, operation)),
        }
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

impl HexView {
    /// Left-click handling for the byte grid: a hit-test that moves the
    /// cursor (optionally extending the selection). Split out of `Widget::handle_event` to keep that dispatch
    /// under the line-count limit.
    fn handle_mouse_press(&mut self, shift: bool, handled: &mut bool) {
        // Click position is tracked via CursorMoved; recompute on press from
        // the last known position.
        if let Some((off, col)) = self.hit_test(self.last_mouse) {
            // The grid takes the keyboard back from the find bar.
            Widget::set_focus(&mut self.find_bar, false);
            self.model.set_column(col);
            self.model.move_to(off, shift);
            self.record_local_history();
            *handled = true;
        }
    }

    /// Keyboard handling for the byte grid: global chords (open / save /
    /// search / goto / select-all), cursor movement, and printable-character
    /// edits. Split out of `Widget::handle_event` to keep that dispatch under
    /// the line-count limit.
    fn handle_key_event(
        &mut self,
        ke: &winit::event::KeyEvent,
        ctrl: bool,
        shift: bool,
        handled: &mut bool,
    ) {
        let key = &ke.logical_key;

        // Empty state: the only meaningful key action is opening a file.
        if !self.has_file {
            self.handle_empty_state_key(key, ctrl, handled);
            return;
        }

        if self.handle_global_chord(key, ctrl, handled) {
            return;
        }

        let moved = self.handle_navigation_key(key, ctrl, shift);
        if moved {
            let (row, _) = self.model.cursor_cell();
            self.model.scroll_into_view(row, self.visible_rows);
            self.record_local_history();
            *handled = true;
        }
    }

    /// The only meaningful key action while no file is open. Part of
    /// [`Self::handle_key_event`].
    fn handle_empty_state_key(&mut self, key: &Key, ctrl: bool, handled: &mut bool) {
        if ctrl
            && let Key::Character(c) = key
            && c.eq_ignore_ascii_case("o")
        {
            self.begin_open();
            *handled = true;
        }
    }

    fn undo_local(&mut self) -> bool {
        if let Some(edit) = self.undo.pop() {
            self.model.apply_history_edit(&edit.undo);
            self.redo.push(edit);
            self.dirty = true;
            true
        } else {
            false
        }
    }

    fn redo_local(&mut self) -> bool {
        if let Some(edit) = self.redo.pop() {
            self.model.apply_history_edit(&edit.redo);
            self.undo.push(edit);
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// Ctrl+letter global chords. Returns `true` if one matched (the caller
    /// should stop dispatching this key further). Part of
    /// [`Self::handle_key_event`].
    fn handle_global_chord(&mut self, key: &Key, ctrl: bool, handled: &mut bool) -> bool {
        let Key::Character(c) = key else {
            return false;
        };
        if !ctrl {
            return false;
        }
        match c.as_str() {
            "z" | "Z" => {
                self.undo_local();
                *handled = true;
                true
            }
            "y" | "Y" => {
                self.redo_local();
                *handled = true;
                true
            }
            "o" | "O" => {
                self.begin_open();
                *handled = true;
                true
            }
            "s" | "S" => {
                self.begin_save();
                *handled = true;
                true
            }
            "f" | "F" => {
                self.open_find();
                *handled = true;
                true
            }
            "g" | "G" => {
                self.open_goto();
                *handled = true;
                true
            }
            "a" | "A" => {
                self.model.select_all();
                self.record_local_history();
                *handled = true;
                true
            }
            _ => false,
        }
    }

    /// Cursor movement / mode keys. Returns whether the cursor moved (or a
    /// byte was edited), so the caller knows to scroll it into view and
    /// persist. Part of [`Self::handle_key_event`].
    fn handle_navigation_key(&mut self, key: &Key, ctrl: bool, shift: bool) -> bool {
        match key {
            Key::Named(NamedKey::ArrowLeft) => self.model.move_left(shift),
            Key::Named(NamedKey::ArrowRight) => self.model.move_right(shift),
            Key::Named(NamedKey::ArrowUp) => self.model.move_up(shift),
            Key::Named(NamedKey::ArrowDown) => self.model.move_down(shift),
            Key::Named(NamedKey::Home) => {
                if ctrl {
                    self.model.move_doc_start(shift);
                } else {
                    self.model.move_row_start(shift);
                }
            }
            Key::Named(NamedKey::End) => {
                if ctrl {
                    self.model.move_doc_end(shift);
                } else {
                    self.model.move_row_end(shift);
                }
            }
            Key::Named(NamedKey::PageUp) => self
                .model
                .page_up(self.visible_rows.saturating_sub(1).max(1), shift),
            Key::Named(NamedKey::PageDown) => self
                .model
                .page_down(self.visible_rows.saturating_sub(1).max(1), shift),
            Key::Named(NamedKey::Tab) => {
                self.model.toggle_column();
            }
            Key::Named(NamedKey::Insert) => {
                self.model.toggle_mode();
            }
            Key::Named(NamedKey::Escape) => {
                if self.drawer.is_overlay() && self.drawer.is_open() {
                    self.drawer.close();
                    self.relayout_now();
                } else if self.find_open {
                    self.close_find();
                } else {
                    self.model.clear_selection();
                }
            }
            Key::Named(NamedKey::Backspace) => {
                self.model.backspace();
            }
            Key::Named(NamedKey::Delete) => {
                self.model.delete_at_cursor();
            }
            Key::Character(c) => {
                return self.handle_char(c.as_str());
            }
            _ => return false,
        }
        true
    }

    /// Dispatch a printable character to the active column / mode. Also
    /// handles the unshifted single-letter shortcuts (e / r) that only
    /// apply outside the ASCII column.
    fn handle_char(&mut self, s: &str) -> bool {
        let Some(ch) = s.chars().next() else {
            return false;
        };
        match self.model.column() {
            Column::Hex => {
                if let Some(d) = ch.to_digit(16) {
                    return self.model.input_hex_digit(d as u8);
                }
                // Non-hex letters act as view shortcuts in the hex column.
                // (Byte-width is auto-fit to the viewport, so there is no
                // manual width toggle.)
                match ch {
                    'e' | 'E' => {
                        self.model.toggle_endian();
                        true
                    }
                    _ => false,
                }
            }
            Column::Ascii => self.model.input_ascii_char(ch),
        }
    }
}

impl HexView {
    /// Stage a screenshot scenario (`None` for a plain `--shots`): the
    /// bundled sample, then the view state the scenario shows. Never runs in
    /// a live window.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        if scenario == Some("empty") {
            return;
        }
        self.load_sample();
        match scenario {
            Some("find") => {
                self.find_bar.set_query("e");
                self.open_find();
            }
            Some("goto") => self.open_goto(),
            Some("edited") => {
                for (off, value) in [(0x1D_usize, 0x01_u8), (0x1E, 0x00), (0x25, 0xFF)] {
                    self.model.set_byte(off, value);
                }
                self.record_local_history();
                self.model.move_to(0x10, false);
                self.model.move_to(0x13, true);
            }
            Some("saved") => {
                let size = format_bytes(u64::try_from(self.model.len()).unwrap_or(u64::MAX));
                self.notice(&format!("Saved {size}"));
                self.status_bubble.set_hover(true, ui::time::now());
            }
            Some("error") => self.problem("Couldn't save the file", "Permission denied"),
            Some("narrow-inspector") => self.reveal_inspector = true,
            _ => {}
        }
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search;

    /// Resource operations drive the local buffer: a direct open is accepted
    /// then succeeds, and canonical edit/undo/redo stay local while updating
    /// the published availability.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the open and edit/undo/redo phases share one resource-operation surface"
    )]
    fn resource_operations_open_edit_undo_and_redo_the_local_buffer() {
        use automerge::ReadDoc as _;

        // Direct open is accepted, then succeeds.
        let dir = std::env::temp_dir().join(format!(
            "hex-direct-{}-{}",
            std::process::id(),
            ui::time::wall()
                .duration_since(ui::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("bytes.bin");
        std::fs::write(&source, [1_u8, 2, 3]).unwrap();
        let mut view = HexView::empty();
        let mut doc = AutoCommit::new();
        let invocation = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::HexOperation::Open {
            path: Some(source.display().to_string()),
        })
        .unwrap();
        let started = Widget::start_operation(
            &mut view,
            &ui::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::HEX_EDITOR_OPERATION_TYPE,
                &payload,
            ),
            &mut doc,
            &automerge::ROOT,
        )
        .unwrap()
        .unwrap();
        assert_eq!(started, ui::widget::ResourceOperationStart::Accepted);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(matches!(
            view.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert_eq!(view.model.bytes(), &[1, 2, 3]);
        std::fs::remove_dir_all(dir).ok();

        // Canonical edit, undo and redo stay local and update availability.
        let mut view = HexView::new(vec![0x10, 0x20], "bytes.bin");
        let mut doc = AutoCommit::new();
        let undo = serde_json::to_vec(&crate::actions::HexOperation::Undo).unwrap();
        assert!(
            Widget::start_operation(
                &mut view,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::HEX_EDITOR_OPERATION_TYPE,
                    &undo
                ),
                &mut doc,
                &automerge::ROOT
            )
            .is_err()
        );
        let edit = serde_json::to_vec(&crate::actions::HexOperation::Edit {
            edit: crate::actions::BufferEditWire::Overwrite {
                offset: 1,
                bytes: vec![0xAB],
            },
        })
        .unwrap();
        assert!(matches!(
            Widget::start_operation(
                &mut view,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::HEX_EDITOR_OPERATION_TYPE,
                    &edit
                ),
                &mut doc,
                &automerge::ROOT
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(view.model.bytes(), &[0x10, 0xAB]);
        assert!(
            ui::resources::resources(&view, &doc)[0]
                .actions
                .iter()
                .find(|action| action.id == "undo")
                .is_some_and(|action| action.contract.availability.is_available())
        );

        assert!(matches!(
            Widget::start_operation(
                &mut view,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::HEX_EDITOR_OPERATION_TYPE,
                    &undo
                ),
                &mut doc,
                &automerge::ROOT
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(view.model.bytes(), &[0x10, 0x20]);

        let redo = serde_json::to_vec(&crate::actions::HexOperation::Redo).unwrap();
        assert!(matches!(
            Widget::start_operation(
                &mut view,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::HEX_EDITOR_OPERATION_TYPE,
                    &redo
                ),
                &mut doc,
                &automerge::ROOT
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(view.model.bytes(), &[0x10, 0xAB]);
        assert!(doc.get(automerge::ROOT, "bytes").unwrap().is_none());
    }

    /// Find runs through the framework's `Findable` contract: hex and text
    /// modes, the bar's case and word rules, a bounded index.
    #[test]
    fn find_matches_bytes_in_hex_and_text_modes() {
        use ui::widget::{FindMatch, FindMode, FindOptions, Findable};

        let mut view = HexView::new(vec![0xde, 0xad, 0x00, 0xde, 0xad], "bytes.bin");
        let hex = FindOptions {
            mode: FindMode::Hex,
            ..FindOptions::default()
        };
        let found = Findable::find(&mut view, "de ad", hex);
        assert_eq!(found, vec![FindMatch::new(0, 2), FindMatch::new(3, 5)]);
        Findable::reveal(&mut view, 1);
        assert_eq!(view.model.cursor(), 3);
        assert_eq!(view.match_idx, Some(1));

        // A text query that is not in the buffer finds nothing.
        assert!(Findable::find(&mut view, "Þ", FindOptions::default()).is_empty());

        // Case and whole-word rules apply to text.
        let mut view = HexView::new(b"IHDR ihdr IHDRx".to_vec(), "text.bin");
        let any = FindOptions::default();
        assert_eq!(Findable::find(&mut view, "ihdr", any).len(), 3);
        let exact = FindOptions {
            match_case: true,
            ..any
        };
        assert_eq!(Findable::find(&mut view, "ihdr", exact).len(), 1);
        let word = FindOptions {
            whole_word: true,
            ..any
        };
        assert_eq!(Findable::find(&mut view, "ihdr", word).len(), 2);

        // A large search retains a bounded result index.
        let mut view = HexView::new(vec![0; search::MAX_RETAINED_MATCHES + 50], "large.bin");
        let found = Findable::find(&mut view, "00", hex);
        assert_eq!(found.len(), search::MAX_RETAINED_MATCHES);

        Findable::clear_find(&mut view);
        assert!(view.matches.is_empty() && view.match_idx.is_none());
    }

    /// The go-to form keeps the editor's own offset policies.
    #[test]
    fn goto_accepts_hex_and_decimal_offsets() {
        let mut view = HexView::new(vec![0; 64], "bytes.bin");
        view.commit_goto("0x1f");
        assert_eq!(view.model.cursor(), 31);
        view.commit_goto(" 12 ");
        assert_eq!(view.model.cursor(), 12);
        view.commit_goto("ff");
        assert_eq!(
            view.model.cursor(),
            63,
            "bare hex with a letter is hex, clamped"
        );
        view.commit_goto("not an offset");
        assert_eq!(
            view.model.cursor(),
            63,
            "an unparsable query leaves the cursor"
        );
    }

    /// Paint a frame of `view` at `theme` into a fresh scene.
    fn paint_frame(view: &mut HexView, tr: &mut TextRenderer, theme: &Theme) {
        let mut scene = Scene::new();
        Widget::draw(
            view,
            &mut scene,
            Affine::IDENTITY,
            tr,
            theme,
            &PresenceCtx::empty(),
        );
        Widget::draw_overlay(
            view,
            &mut scene,
            Affine::IDENTITY,
            tr,
            theme,
            &PresenceCtx::empty(),
        );
    }

    /// Surface geometry and semantics.
    ///
    /// The grid's pitches come from the renderer's measured monospace advance
    /// and line height, so paint and hit-testing share one geometry at every
    /// text scale: a painted cell's centre maps back to its own byte. The
    /// grid fills the content width and the inspector docks beside it.
    #[test]
    fn surface_geometry_follows_measured_metrics_width_and_state() {
        use ui::a11y::Role;

        let mut tr = TextRenderer::new();
        for scale in [1.0_f32, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            tr.set_text_scale(scale);
            let mut view = HexView::new(vec![0xAB; 960], "scale.bin");
            view.rect = Rect::new(0.0, 0.0, 1600.0, 1000.0);
            let before = CellMetrics::measure(&mut tr, &theme);
            paint_frame(&mut view, &mut tr, &theme);
            assert_eq!(view.cell, before, "draw measures the same cell metrics");
            let g = view.geo.clone();
            assert!(g.inspector_visible, "a wide window docks the inspector");
            assert!(
                (g.content.x1 - g.inspector.x0).abs() < 1e-6
                    && (g.inspector.x1 - view.rect.x1).abs() < 1e-6,
                "the inspector is the trailing region: {g:?}"
            );
            assert!(
                (g.status.y1 - view.rect.y1).abs() < 1e-6,
                "status foots the window"
            );
            assert!(g.grid.y0 > g.header.y1, "the grid sits under the header");
            let bpr = view.model.bytes_per_row();
            assert!(
                bpr >= 8 && bpr.is_multiple_of(4),
                "rows hold whole groups: {bpr}"
            );
            assert!(
                g.ascii_x1 <= g.content.x1,
                "the grid fits the content width: {g:?}"
            );
            for off in [0usize, 1, bpr - 1, bpr, 2 * bpr + 3] {
                let (row, col) = (off / bpr, off % bpr);
                let y = g.grid.y0 + row as f64 * g.row_h + g.row_h / 2.0;
                let hx = g.hex_x(col) + g.byte_w / 2.0;
                assert_eq!(
                    view.hit_test(Point::new(hx, y)),
                    Some((off, Column::Hex)),
                    "hex cell {off} hit-tests to itself (scale {scale})"
                );
                let ax = g.ascii_x(col) + g.glyph_w / 2.0;
                assert_eq!(
                    view.hit_test(Point::new(ax, y)),
                    Some((off, Column::Ascii)),
                    "ascii cell {off} hit-tests to itself (scale {scale})"
                );
            }
        }

        // A narrow window floats the inspector instead of squeezing the grid,
        // and its toggle summons it.
        let theme = Theme::dark();
        tr.set_text_scale(1.0);
        let mut view = HexView::new(vec![0; 128], "narrow.bin");
        view.rect = Rect::new(0.0, 0.0, 540.0, 500.0);
        paint_frame(&mut view, &mut tr, &theme);
        assert!(!view.geo.inspector_visible, "closed by default when narrow");
        assert!((view.geo.content.x1 - view.rect.x1).abs() < 1e-6);
        assert!(view.model.bytes_per_row() >= 8);
        view.toggle_inspector();
        view.drawer.snap(true);
        paint_frame(&mut view, &mut tr, &theme);
        assert!(view.geo.inspector_visible, "the toggle opens the drawer");
        assert!(
            (view.geo.content.x1 - view.rect.x1).abs() < 1e-6,
            "a floating drawer leaves the grid its full width"
        );

        // A closed editor exposes a local-document a11y summary.
        let view = HexView::empty();
        let tree = Widget::accessibility(&view).expect("semantic root");
        assert_eq!(tree.role, Role::Document);
        assert!(
            tree.description
                .as_deref()
                .is_some_and(|text| text.contains("not shared"))
        );
        assert!(tree.children.iter().any(|node| node.role == Role::Button));
    }

    /// Send `event` to `view` the way the host does.
    fn send(view: &mut HexView, event: &WindowEvent) -> bool {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let (mut handled, mut focus) = (false, None);
        view.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    /// Move the pointer to the middle of `rect` and click.
    fn click(view: &mut HexView, rect: Rect) -> bool {
        let p = rect.center();
        let device_id = winit::event::DeviceId::dummy();
        send(
            view,
            &WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(p.x, p.y),
            },
        );
        let button = |state| WindowEvent::MouseInput {
            device_id,
            state,
            button: MouseButton::Left,
        };
        let down = send(view, &button(ElementState::Pressed));
        send(view, &button(ElementState::Released));
        down
    }

    /// The pointer drives every control: the grid moves the cursor, the
    /// header's strip switches the edit mode, the inspector's strip the
    /// endianness, and the toggle hides and shows the inspector.
    #[test]
    fn pointer_drives_the_grid_the_strips_and_the_inspector_toggle() {
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let mut view = HexView::empty();
        view.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        view.stage_scenario(None);
        paint_frame(&mut view, &mut tr, &theme);
        assert_eq!(view.model.endian(), Endian::Big);

        // A click on a hex cell puts the cursor there, in the hex column.
        let g = view.geo.clone();
        let cell = Rect::new(
            g.hex_x(3),
            g.grid.y0 + g.row_h,
            g.hex_x(3) + g.byte_w,
            g.grid.y0 + 2.0 * g.row_h,
        );
        assert!(click(&mut view, cell));
        assert_eq!(view.model.cursor(), view.model.bytes_per_row() + 3);
        assert_eq!(view.model.column(), Column::Hex);

        // The strips: the second segment of each.
        let half = |strip: Rect| Rect::new(strip.center().x, strip.y0, strip.x1, strip.y1);
        assert!(click(&mut view, half(g.mode)));
        assert_eq!(view.model.mode(), EditMode::Insert);
        let left = |strip: Rect| Rect::new(strip.x0, strip.y0, strip.center().x, strip.y1);
        assert!(click(&mut view, left(g.endian)));
        assert_eq!(view.model.endian(), Endian::Little);
        paint_frame(&mut view, &mut tr, &theme);

        // The toggle hides the docked inspector and the grid takes the room.
        let before = view.geo.content.width();
        assert!(click(&mut view, g.toggle));
        view.drawer.snap(false);
        paint_frame(&mut view, &mut tr, &theme);
        assert!(!view.geo.inspector_visible);
        assert!(view.geo.content.width() > before, "the content widens");
        let toggle = view.geo.toggle;
        assert!(click(&mut view, toggle));
        view.drawer.snap(true);
        paint_frame(&mut view, &mut tr, &theme);
        assert!(view.geo.inspector_visible, "the toggle brings it back");

        // The sample overflows the grid, so a bar shows its position and a
        // dragged thumb scrolls it.
        let metrics = view.scroll_metrics();
        assert!(metrics.overflows());
        let thumb = view
            .scrollbar
            .thumb_rect(view.geo.scroll_track, metrics)
            .expect("a thumb");
        let device_id = winit::event::DeviceId::dummy();
        let at = |p: Point| WindowEvent::CursorMoved {
            device_id,
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        };
        let button = |state| WindowEvent::MouseInput {
            device_id,
            state,
            button: MouseButton::Left,
        };
        send(&mut view, &at(thumb.center()));
        assert!(send(&mut view, &button(ElementState::Pressed)));
        send(
            &mut view,
            &at(Point::new(thumb.center().x, thumb.center().y + 40.0)),
        );
        assert!(view.model.top_row() > 0, "dragging the thumb scrolls");
        send(&mut view, &button(ElementState::Released));
        assert!(!view.scrollbar.is_dragging());
        let max = view.model.row_count() - view.visible_rows;
        view.model.scroll_by(1000);
        paint_frame(&mut view, &mut tr, &theme);
        assert_eq!(view.model.top_row(), max, "never past the last full page");
    }

    /// Every scenario paints in both polarities at a wide and a narrow size
    /// without panicking, and stages the state it names.
    #[test]
    fn scenarios_stage_their_state_and_paint_everywhere() {
        let mut tr = TextRenderer::new();
        for (name, find, goto) in [
            (None, false, false),
            (Some("find"), true, false),
            (Some("goto"), false, true),
            (Some("edited"), false, false),
            (Some("saved"), false, false),
            (Some("error"), false, false),
            (Some("narrow-inspector"), false, false),
            (Some("empty"), false, false),
        ] {
            for theme in [Theme::dark(), Theme::light()] {
                for (w, h) in [(1280.0, 800.0), (640.0, 720.0)] {
                    let mut view = HexView::empty();
                    view.rect = Rect::new(0.0, 0.0, w, h);
                    view.stage_scenario(name);
                    assert_eq!(view.has_file(), name != Some("empty"));
                    assert_eq!(view.find_open, find, "{name:?}");
                    assert_eq!(view.goto.is_open(), goto, "{name:?}");
                    paint_frame(&mut view, &mut tr, &theme);
                    paint_frame(&mut view, &mut tr, &theme);
                }
            }
        }
        let mut view = HexView::empty();
        view.rect = Rect::new(0.0, 0.0, 640.0, 720.0);
        view.stage_scenario(Some("narrow-inspector"));
        paint_frame(&mut view, &mut tr, &Theme::dark());
        assert!(
            view.geo.inspector_visible,
            "the narrow scenario opens the drawer"
        );
        let mut view = HexView::empty();
        view.stage_scenario(Some("edited"));
        assert_eq!(view.model.dirty_count(), 3);
        assert_eq!(view.model.selection_len(), 4);
    }
}
