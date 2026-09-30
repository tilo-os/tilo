//! The hex view widget — the whole editor surface in one custom-drawn
//! `Widget`. A hex editor is fundamentally a dense, fixed-pitch grid with
//! a cursor synced across two parallel columns; that is a canvas, not a
//! composition of stock form widgets, so the view paints everything
//! itself into the Vello scene and reads all of its colours and metrics
//! from the `Theme`.
//!
//! Regions, top to bottom:
//!   * header — file name, byte count, edit-mode / endianness pills
//!   * body   — offset gutter | hex byte columns | ASCII column   (left)
//!     data inspector panel (right)
//!     (the grid + inspector span to the window bottom)
//!   * status — a transient floating bubble (cursor offset, selection
//!     length, dirty count, save/error flash) that surfaces on a
//!     change or a bottom-corner hover, then fades; no permanent strip
//!   * overlay — search bar and goto-offset prompt (modal-ish, on top)
//!
//! File paths and byte contents stay in this device-local view model. The
//! canonical resource operations remain palette/agent-driveable, but never
//! become collaborative document changes.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::format::bytes as format_bytes;
use ui::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, PickRequest, PortalChannel, PortalOutcome,
    SaveChannel, SaveOutcome,
};
use ui::presence::PresenceCtx;
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, TextInputKind};

use crate::actions::HexOperation;
use crate::inspector::{self, Interpretation};
use crate::model::{Column, EditMode, HexModel, LocalEdit};
use crate::search::{self, SearchKind};
use ui::paint::with_alpha;

/// How long a transient status message (Saved / open error) stays up.
const FLASH_MS: u128 = 3500;
const LOCAL_HISTORY_LIMIT: usize = 256;

/// Theme type sizes already carry the accessibility text scale, and the
/// size-based renderer APIs (`measure*`, `draw_text*`, `pill_size`) apply
/// that scale once more. Every token the chrome paints goes through here so
/// the multiplier lands exactly once — 2× glyphs at 200 %, not 4×. The byte
/// grid is unaffected: its `CELL_FONT` is a literal base size by design.
fn ts(tr: &TextRenderer, resolved: f32) -> f32 {
    tr.renderer_input_style(&TextStyle::new(resolved)).size
}

/// Measured line box of a renderer-relative `size` — what stacked chrome
/// lines step by and what a band centres, never the bare font size.
fn line_box(tr: &mut TextRenderer, size: f32) -> f64 {
    let (_, h) = tr.measure("0", size, None);
    f64::from(h)
}

/// How much one line of `size` has grown over its 1× box at the current
/// accessibility scale — what a 1×-designed band adds to keep seating it.
fn line_growth(tr: &mut TextRenderer, size: f32) -> f64 {
    let line = line_box(tr, size);
    let scale = f64::from(tr.text_scale()).max(f32::EPSILON.into());
    (line - line / scale).max(0.0)
}

/// A side panel's base surface with its separating left edge.
fn paint_side_panel(scene: &mut Scene, t: Affine, theme: &Theme, panel: Rect) {
    let base = theme.tokens.colors.surfaces.base;
    scene.fill(Fill::NonZero, t, base, None, &panel);
    scene.stroke(
        &Stroke::new(1.0),
        t,
        ui::surface::edge_on(base),
        None,
        &Line::new((panel.x0, panel.y0), (panel.x0, panel.y1)),
    );
}

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

/// Which transient prompt, if any, is capturing keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overlay {
    None,
    Search,
    Goto,
}

/// The taffy/focus triple [`HexView::handle_overlay_event`] forwards on to
/// the active field's own `Widget::handle_event` — bundled so the helper
/// itself stays under the argument-count limit without reshaping the
/// `Widget` trait signature it forwards into.
struct OverlayLayoutCtx<'a> {
    focus_target: &'a mut Option<NodeId>,
    node_id: NodeId,
    taffy: &'a TaffyTree<()>,
}

// The three bools below are independent view flags (redraw-pending,
// buffer-present, shots-forced-visible), not a combinatorial option set, so
// folding them into an enum would only add indirection.
#[allow(
    clippy::struct_excessive_bools,
    reason = "three unrelated view flags (dirty/has_file/shots_status), not a combinatorial option set"
)]
pub struct HexView {
    /// Decoded local buffer and editor state. Refreshed from local I/O only.
    model: HexModel,
    /// File name shown in the header (cosmetic; the buffer is local).
    file_name: String,
    rect: Rect,
    dirty: bool,

    // --- search state (per-peer; not in the shared doc) ---
    overlay: Overlay,
    /// Shared local text field: owns caret, selection, clipboard, IME and
    /// accessibility while the byte-search policy remains app-owned.
    search_input: TextInput,
    search_kind: SearchKind,
    /// Cached match offsets for the current query, recomputed on demand.
    matches: Vec<usize>,
    /// Parsed search bytes paired with `matches`; keeping this cache avoids
    /// reparsing the query for every painted cell.
    search_needle: Option<Vec<u8>>,
    /// True when matching continued past the retained offset budget.
    matches_truncated: bool,
    /// Index into `matches` of the currently-focused hit.
    match_idx: Option<usize>,

    // --- goto state ---
    /// Shared local text field for decimal / hexadecimal offsets.
    goto_input: TextInput,

    /// Rows currently fitting in the grid viewport — updated each draw,
    /// read by keyboard paging so PageUp/Down move a screenful.
    visible_rows: usize,
    /// The monospace advance + line height measured from the renderer that
    /// paints the grid, refreshed every draw. Layout, paint and hit-testing
    /// all derive their pitches from this one measurement (see
    /// [`CellMetrics`]), so a byte's painted cell is its click target.
    cell_metrics: CellMetrics,
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
    /// A transient status message (e.g. "Saved 4.2 kB") with its birth time.
    flash: Option<(String, bool, Instant)>,
    /// Hit rect of the empty-state "Open file…" button, recorded during draw.
    open_button: Rect,

    /// Bounded, device-local undo/redo journal. Resource operations and
    /// keyboard shortcuts use the same inverse edit representation.
    undo: Vec<LocalEdit>,
    redo: Vec<LocalEdit>,

    /// The cursor read-out / edit status, surfaced as a transient floating
    /// bubble (see `draw_status_bubble`) instead of a permanent bottom strip,
    /// so the byte grid + inspector span to the window bottom. Poked on a real
    /// status change (cursor move, selection, dirty count, a fresh save/error
    /// flash) and shown while the pointer hovers the bottom-right corner, then
    /// it fades.
    status_bubble: ui::StatusBubble,
    /// The composed bubble label from the previous frame, so a poke fires only
    /// when the status genuinely changes rather than every frame.
    last_status_label: Option<String>,
    /// `--shots` forces the bubble fully visible so a single headless frame
    /// captures the floating chip.
    shots_status: bool,
    /// Headless shots render one base layer only; this explicitly includes a
    /// focused overlay in that base layer for visual review.
    shots_overlay: bool,
    /// The canvas is keyboard-focusable even though it is custom drawn.
    focused: bool,
    /// Set when the framework ResourceApp hosts this surface. Hosted
    /// [`PublishesResources`] keeps only live cursor extras — never a second
    /// `hex_editor.document` beside ResourceRoot.
    hosted: bool,
}

impl HexView {
    pub fn new(bytes: Vec<u8>, file_name: impl Into<String>) -> Self {
        Self {
            model: HexModel::new(bytes),
            file_name: file_name.into(),
            rect: Rect::ZERO,
            dirty: true,
            overlay: Overlay::None,
            search_input: TextInput::local()
                .with_kind(TextInputKind::Code)
                .with_id("hex.search")
                .with_placeholder("search…")
                .with_plain_chrome()
                .with_mono(),
            search_kind: SearchKind::Hex,
            matches: Vec::new(),
            search_needle: None,
            matches_truncated: false,
            match_idx: None,
            goto_input: TextInput::local()
                .with_kind(TextInputKind::Number)
                .with_id("hex.goto")
                .with_placeholder("0x… or decimal")
                .with_plain_chrome()
                .with_mono(),
            visible_rows: 1,
            cell_metrics: CellMetrics::FALLBACK,
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
            flash: None,
            open_button: Rect::ZERO,
            undo: Vec::new(),
            redo: Vec::new(),
            status_bubble: ui::StatusBubble::new(),
            last_status_label: None,
            shots_status: false,
            shots_overlay: false,
            focused: false,
            hosted: false,
        }
    }

    /// Framework [`ResourceSurface::attach`] marks the surface hosted so
    /// palette publication does not duplicate ResourceRoot's publish door.
    pub(crate) fn mark_framework_hosted(&mut self) {
        self.hosted = true;
    }

    /// Whether this surface is under the framework ResourceApp host.
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.hosted
    }

    /// Test-only: mark hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
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
        if self.model.endian() != crate::model::Endian::Big {
            self.model.toggle_endian();
        }
        self
    }

    /// `--shots`: force the floating status bubble fully visible so a single
    /// headless frame captures the floating chip.
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(ui::time::now());
    }

    /// Deterministic `--shots` hook for the real search overlay. It is not
    /// reachable in a normal launch and exists only so visual review can cover
    /// focused text input and match highlighting without a live compositor.
    pub fn arm_search_for_shots(&mut self) {
        self.overlay = Overlay::Search;
        self.search_kind = SearchKind::Text;
        self.search_input.set_mono(false);
        self.search_input.set_text("IHDR");
        self.search_input.set_focus(true);
        self.recompute_matches();
        self.shots_overlay = true;
        self.dirty = true;
    }

    /// Deterministic `--shots` hook for a failed local-file operation. Uses
    /// the same transient error presentation as `open_path`/`write_to`.
    pub fn arm_error_for_shots(&mut self) {
        self.flash("Open failed: permission denied", true);
        self.arm_status_bubble_for_shots();
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
        }
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
                self.flash(
                    format!(
                        "Saved {}",
                        format_bytes(u64::try_from(self.model.len()).unwrap_or(u64::MAX))
                    ),
                    false,
                );
                self.dirty = true;
                Ok(())
            }
            Err(e) => {
                self.flash(format!("Save failed: {e}"), true);
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
                self.flash(
                    format!(
                        "Opened {}",
                        format_bytes(u64::try_from(n).unwrap_or(u64::MAX))
                    ),
                    false,
                );
                Ok(())
            }
            Err(e) => {
                self.flash(format!("Open failed: {e}"), true);
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
        self.overlay = Overlay::None;
        self.matches.clear();
        self.search_needle = None;
        self.matches_truncated = false;
        self.match_idx = None;
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
                self.flash(
                    format!(
                        "Opened {}",
                        format_bytes(u64::try_from(n).unwrap_or(u64::MAX))
                    ),
                    false,
                );
                Ok(())
            }
            ContentsOutcome::Cancelled => Err("cancelled".into()),
            ContentsOutcome::Failed(reason) => {
                self.flash(&reason, true);
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
                            self.flash("Saved", false);
                            self.dirty = true;
                            Ok(())
                        }
                        SaveOutcome::Cancelled => Err("cancelled".into()),
                        SaveOutcome::Failed(reason) => {
                            self.flash(&reason, true);
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
                            self.flash(&reason, true);
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
                            self.flash(&reason, true);
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

    /// Post a transient status message.
    fn flash(&mut self, msg: impl Into<String>, error: bool) {
        self.flash = Some((msg.into(), error, ui::time::now()));
        self.dirty = true;
    }

    /// True while any portal request is in flight or a status flash is live —
    /// the app keeps pumping frames so the pick drains and the flash expires.
    fn is_busy(&self) -> bool {
        self.open_rx.is_some() || self.save_rx.is_some() || self.flash.is_some()
    }

    /// The number of byte columns that fills the current width, leaving room
    /// for the offset gutter, the ASCII column, and a right-hand inspector.
    /// Recomputed each draw so the grid reflows on resize (best-in-class hex
    /// editors fill the window rather than stranding a fixed 16 columns in a
    /// sea of empty space). Snapped to a multiple of 8 and clamped to a sane
    /// range.
    fn fit_bytes_per_row(&self) -> usize {
        let cell = self.cell_metrics;
        let inspector_reserve = if self.show_inspector() {
            GridMetrics::INSPECTOR_GAP + GridMetrics::INSPECTOR_MIN
        } else {
            0.0
        };
        let avail = self.rect.width()
            - GridMetrics::LEFT_PAD
            - cell.gutter_w()
            - GridMetrics::GUTTER_GAP
            - GridMetrics::HEX_ASCII_GAP
            - inspector_reserve;
        let per_byte = cell.hex_col_w() + cell.char_w();
        if per_byte <= 0.0 {
            return 16;
        }
        let raw = (avail / per_byte).floor() as i64;
        let snapped = (raw / 8) * 8;
        snapped.clamp(8, 32) as usize
    }

    /// Recompute match offsets from the current query + kind.
    fn recompute_matches(&mut self) {
        self.search_needle = search::needle(self.search_input.value(), self.search_kind);
        let (matches, truncated) = self.search_needle.as_ref().map_or_else(
            || (Vec::new(), false),
            |needle| {
                search::find_all_bounded(self.model.bytes(), needle, search::MAX_RETAINED_MATCHES)
            },
        );
        self.matches = matches;
        self.matches_truncated = truncated;
        // Keep match_idx pointing at the nearest hit to the cursor.
        self.match_idx = if self.matches.is_empty() {
            None
        } else {
            let cur = self.model.cursor();
            Some(self.matches.iter().position(|&m| m >= cur).unwrap_or(0))
        };
    }

    /// Whether `offset` sits inside a match. This tests the local byte slice,
    /// not the retained result index: painting remains correct after the
    /// capped index has dropped older/farther matches.
    fn is_match_offset(&self, offset: usize) -> bool {
        let Some(needle) = self.search_needle.as_ref() else {
            return false;
        };
        let first = offset.saturating_sub(needle.len().saturating_sub(1));
        (first..=offset).any(|start| {
            self.model
                .bytes()
                .get(start..start.saturating_add(needle.len()))
                == Some(needle)
        })
    }

    fn show_inspector(&self) -> bool {
        self.rect.width() >= 860.0
    }

    fn jump_to_match(&mut self, forward: bool) {
        let Some(n) = search::needle(self.search_input.value(), self.search_kind) else {
            return;
        };
        let cur = self.model.cursor();
        let target = if forward {
            search::find_next(self.model.bytes(), &n, cur + 1)
        } else {
            search::find_prev(self.model.bytes(), &n, cur)
        };
        if let Some(off) = target {
            self.model.move_to(off, false);
            // Select the whole match so it reads as a found span.
            self.model.move_to(off + n.len().saturating_sub(1), true);
            self.model.move_to(off, false);
            self.model
                .scroll_into_view(off / self.model.bytes_per_row(), self.visible_rows);
            self.recompute_matches();
        }
    }

    fn commit_goto(&mut self) {
        let q = self.goto_input.value().trim();
        // Accept "0x.." hex or plain decimal.
        let parsed = if let Some(hex) = q.strip_prefix("0x").or_else(|| q.strip_prefix("0X")) {
            usize::from_str_radix(hex, 16).ok()
        } else if q.chars().all(|c| c.is_ascii_hexdigit())
            && q.chars().any(|c| c.is_ascii_alphabetic())
        {
            usize::from_str_radix(q, 16).ok()
        } else {
            q.parse::<usize>().ok()
        };
        if let Some(off) = parsed {
            self.model.move_to(off, false);
            self.model
                .scroll_into_view(off / self.model.bytes_per_row(), self.visible_rows);
        }
        self.overlay = Overlay::None;
        self.goto_input.set_text("");
        self.goto_input.set_focus(false);
    }

    // --- geometry helpers ----------------------------------------------

    /// Metrics for the grid layout at the current width.
    fn metrics(&self) -> GridMetrics {
        GridMetrics::new(self.rect, self.model.bytes_per_row(), self.cell_metrics)
    }

    /// Map a mouse point to a byte offset + column, if it lands on a cell.
    fn hit_test(&self, p: Point) -> Option<(usize, Column)> {
        let m = self.metrics();
        let bpr = self.model.bytes_per_row();
        // Determine the row under the cursor.
        if p.y < m.grid_top || p.y >= m.grid_bottom {
            return None;
        }
        let row_rel = ((p.y - m.grid_top) / m.row_h) as usize;
        let row = self.model.top_row() + row_rel;

        // Hex columns region.
        if p.x >= m.hex_x0 && p.x < m.hex_x1 {
            let rel = p.x - m.hex_x0;
            let col = (rel / m.hex_col_w) as usize;
            if col < bpr {
                let off = row * bpr + col;
                if off < self.model.len() {
                    return Some((off, Column::Hex));
                }
            }
        }
        // ASCII column region.
        if p.x >= m.ascii_x0 && p.x < m.ascii_x1 {
            let rel = p.x - m.ascii_x0;
            let col = (rel / m.char_w) as usize;
            if col < bpr {
                let off = row * bpr + col;
                if off < self.model.len() {
                    return Some((off, Column::Ascii));
                }
            }
        }
        None
    }

    // --- drawing --------------------------------------------------------

    fn draw_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &GridMetrics,
    ) {
        let tokens = &theme.tokens;
        let bar = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, m.header_bottom);
        scene.fill(Fill::NonZero, t, tokens.colors.surfaces.raised, None, &bar);
        // Bar role: one derived hairline under the title bar (HIG §9.5).
        scene.fill(
            Fill::NonZero,
            t,
            ui::chrome::chrome_hairline(theme),
            None,
            &Rect::new(bar.x0, bar.y1 - 1.0, bar.x1, bar.y1),
        );

        let pad = f64::from(tokens.spacing.s4);

        // File name (semibold) + byte count (dim), each centred on its own
        // measured line box so the two sizes share the bar's centre line.
        let (name_w, name_h) = tr.measure_weighted(
            &self.file_name,
            ts(tr, tokens.typography.scale.lg),
            None,
            TextWeight::Semibold,
        );
        tr.draw_text_weighted(
            scene,
            &self.file_name,
            ts(tr, tokens.typography.scale.lg),
            tokens.colors.text_primary,
            t * Affine::translate((
                bar.x0 + pad,
                bar.y0 + (bar.height() - f64::from(name_h)) / 2.0,
            )),
            None,
            TextWeight::Semibold,
        );
        let meta = format!("{} bytes", self.model.len());
        let (_, meta_h) = tr.measure(&meta, ts(tr, tokens.typography.scale.sm), None);
        tr.draw_text(
            scene,
            &meta,
            ts(tr, tokens.typography.scale.sm),
            tokens.colors.text_dim,
            t * Affine::translate((
                bar.x0 + pad + f64::from(name_w) + 12.0,
                bar.y0 + (bar.height() - f64::from(meta_h)) / 2.0,
            )),
            None,
        );

        // Right-aligned pills: mode + endianness + width. Narrow windows keep
        // the interactive state pills and drop the informational width chip.
        let mut x = bar.x1 - pad;
        if self.rect.width() >= 620.0 {
            let width_label = format!("{}/row", self.model.bytes_per_row());
            x = Self::draw_pill(
                scene,
                t,
                tr,
                theme,
                x,
                bar,
                &width_label,
                tokens.colors.neutral[3],
            );
            x -= 8.0;
        }
        let endian_color = tokens.colors.accent[4];
        x = Self::draw_pill(
            scene,
            t,
            tr,
            theme,
            x,
            bar,
            self.model.endian().label(),
            endian_color,
        );
        x -= 8.0;
        let mode_color = match self.model.mode() {
            EditMode::Overwrite => tokens.colors.neutral[3],
            EditMode::Insert => tokens.colors.semantic.warning,
        };
        let _ = Self::draw_pill(
            scene,
            t,
            tr,
            theme,
            x,
            bar,
            self.model.mode().label(),
            mode_color,
        );
    }

    /// Draw a small rounded pill ending at `right_x`; returns its left x.
    fn draw_pill(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        right_x: f64,
        bar: Rect,
        label: &str,
        fill: Color,
    ) -> f64 {
        let size = ts(tr, theme.tokens.typography.scale.xs);
        let (tw, th) = tr.measure_weighted(label, size, None, TextWeight::Bold);
        let pad_x = 8.0;
        let w = f64::from(tw) + pad_x * 2.0;
        // The capsule wraps the measured line box (18 px at 1×) so it keeps
        // seating its label at every text scale.
        let h = f64::from(th) + 5.5;
        let x0 = right_x - w;
        let y0 = bar.y0 + (bar.height() - h) / 2.0;
        // A pill with its derived rest edge: the mode / endianness chips sit
        // on the raised bar and otherwise read as bare washes.
        ui::surface::pill(
            scene,
            t,
            Rect::new(x0, y0, right_x, y0 + h),
            fill,
            Some(ui::surface::edge_on(fill)),
            Some(6.0),
        );
        // Centre the measured line box in the pill (no nudge on the font size).
        tr.draw_text_weighted(
            scene,
            label,
            size,
            theme.tokens.colors.on_surface(fill).ink,
            t * Affine::translate((x0 + pad_x, y0 + (h - f64::from(th)) / 2.0)),
            None,
            TextWeight::Bold,
        );
        x0
    }

    fn draw_grid(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &GridMetrics,
    ) {
        let bpr = self.model.bytes_per_row();
        self.draw_grid_headers(scene, t, tr, theme, m, bpr);

        let selection = self.model.selection();
        let inspector_span = self.inspector_span();
        let cursor = self.model.cursor();

        let visible = m.visible_rows.max(1);
        self.visible_rows = visible;

        for vr in 0..visible {
            let row = self.model.top_row() + vr;
            let row_off = row * bpr;
            if row_off > self.model.len() && !self.model.is_empty() {
                break;
            }
            let y = m.grid_top + vr as f64 * m.row_h;
            // Centre the measured line box in the row, not the nominal font size.
            let text_y = y + (m.row_h - m.line_h) / 2.0;

            // Offset gutter.
            let gutter = format!("{row_off:08X}");
            tr.draw_style(
                scene,
                &gutter,
                theme.tokens.colors.accent[6],
                t * Affine::translate((m.gutter_x0, text_y)),
                None,
                &TextStyle::new(m.cell_font).with_family(FontChoice::Monospace),
            );

            let ctx = GridCellCtx {
                theme,
                m,
                selection,
                inspector_span,
                cursor,
                y,
                text_y,
            };
            for c in 0..bpr {
                self.draw_grid_cell(scene, t, tr, &ctx, row_off + c, c);
            }
        }
    }

    /// The offset/hex-index/ASCII column headers, plus the hairline and
    /// vertical separators framing the grid. Drawn once per frame, ahead of
    /// the per-row cell loop.
    fn draw_grid_headers(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &GridMetrics,
        bpr: usize,
    ) {
        let tokens = &theme.tokens;
        let head_y = m.col_header_y;
        tr.draw_style(
            scene,
            "Offset",
            tokens.colors.text_dim,
            t * Affine::translate((m.gutter_x0, head_y)),
            None,
            &TextStyle::new(m.header_font)
                .with_weight(TextWeight::Medium)
                .with_family(FontChoice::Monospace),
        );
        for c in 0..bpr {
            let label = format!("{c:02X}");
            let x = m.hex_x0 + c as f64 * m.hex_col_w;
            tr.draw_style(
                scene,
                &label,
                tokens.colors.text_dim,
                t * Affine::translate((x, head_y)),
                None,
                &TextStyle::new(m.header_font)
                    .with_weight(TextWeight::Medium)
                    .with_family(FontChoice::Monospace),
            );
        }
        tr.draw_style(
            scene,
            "ASCII",
            tokens.colors.text_dim,
            t * Affine::translate((m.ascii_x0, head_y)),
            None,
            &TextStyle::new(m.header_font)
                .with_weight(TextWeight::Medium)
                .with_family(FontChoice::Monospace),
        );
        // Rules on the grid derive from the surface they cross (HIG §9.5):
        // the declared border is three shades from the dark base and vanishes.
        let rule = ui::surface::edge_on(tokens.colors.surfaces.base);
        // Hairline under the column header.
        scene.stroke(
            &Stroke::new(1.0),
            t,
            rule,
            None,
            &Line::new(
                (self.rect.x0 + 8.0, m.grid_top - 2.0),
                (m.ascii_x1 + 8.0, m.grid_top - 2.0),
            ),
        );
        // Vertical separators framing the hex region.
        for sep_x in [m.hex_x0 - 8.0, m.ascii_x0 - 8.0] {
            scene.stroke(
                &Stroke::new(1.0),
                t,
                rule,
                None,
                &Line::new((sep_x, m.grid_top - 2.0), (sep_x, m.grid_bottom)),
            );
        }
    }

    /// Draw one byte's hex + ASCII cells: background tint layers (inspected /
    /// match / selection), the cursor outline, then the two glyphs. `ctx`
    /// bundles the per-row state (theme, metrics, selection, cursor) shared
    /// across a row's cells.
    fn draw_grid_cell(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        ctx: &GridCellCtx<'_>,
        off: usize,
        c: usize,
    ) {
        let Some(&byte) = self.model.bytes().get(off) else {
            return;
        };
        let m = ctx.m;
        let hex_x = m.hex_x0 + c as f64 * m.hex_col_w;
        let ascii_x = m.ascii_x0 + c as f64 * m.char_w;
        let cells = (
            Rect::new(hex_x - 3.0, ctx.y, hex_x + m.byte_w + 3.0, ctx.y + m.row_h),
            Rect::new(
                ascii_x - 1.0,
                ctx.y,
                ascii_x + m.char_w + 1.0,
                ctx.y + m.row_h,
            ),
        );

        self.paint_cell_background(scene, t, ctx, off, cells);
        if off == ctx.cursor {
            self.paint_cursor_outline(scene, t, ctx, cells);
        }
        self.draw_cell_glyphs(
            scene,
            t,
            tr,
            ctx,
            &CellGeom {
                off,
                byte,
                hex_x,
                ascii_x,
            },
        );
    }

    /// The inspected / search-match / selection tint layers under one cell,
    /// lowest to highest. Split out of [`Self::draw_grid_cell`] to keep that
    /// dispatch under the line-count limit.
    fn paint_cell_background(
        &self,
        scene: &mut Scene,
        t: Affine,
        ctx: &GridCellCtx<'_>,
        off: usize,
        cells: (Rect, Rect),
    ) {
        let (cell_hex, cell_ascii) = cells;
        let tokens = &ctx.theme.tokens;
        let is_cursor = off == ctx.cursor;
        let selected = ctx.selection.is_some_and(|(lo, hi)| off >= lo && off <= hi);
        let inspected = ctx
            .inspector_span
            .is_some_and(|(lo, hi)| off >= lo && off <= hi);
        let is_match = self.is_match_offset(off);

        if inspected && !is_cursor {
            let tint = with_alpha(tokens.colors.accent[5], 40);
            scene.fill(Fill::NonZero, t, tint, None, &cell_hex);
            scene.fill(Fill::NonZero, t, tint, None, &cell_ascii);
        }
        if is_match {
            let tint = with_alpha(tokens.colors.semantic.warning, 70);
            scene.fill(Fill::NonZero, t, tint, None, &cell_hex);
            scene.fill(Fill::NonZero, t, tint, None, &cell_ascii);
        }
        if selected {
            let tint = with_alpha(tokens.colors.accent[5], 90);
            scene.fill(Fill::NonZero, t, tint, None, &cell_hex);
            scene.fill(Fill::NonZero, t, tint, None, &cell_ascii);
        }
    }

    /// The cursor cell's fill on its active column, plus a faint outline on
    /// the mirror column so the synced position is visible in both panes.
    fn paint_cursor_outline(
        &self,
        scene: &mut Scene,
        t: Affine,
        ctx: &GridCellCtx<'_>,
        cells: (Rect, Rect),
    ) {
        let (cell_hex, cell_ascii) = cells;
        let active_hex = self.model.column() == Column::Hex;
        let (cell, other) = if active_hex {
            (cell_hex, cell_ascii)
        } else {
            (cell_ascii, cell_hex)
        };
        let tokens = &ctx.theme.tokens;
        scene.fill(
            Fill::NonZero,
            t,
            tokens.colors.accent[ui_accent_idx()],
            None,
            &cell,
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            tokens.colors.accent[ui_accent_idx()],
            None,
            &RoundedRect::from_rect(other, 2.0),
        );
    }

    /// The hex and ASCII glyphs for one cell, coloured for cursor / dirty /
    /// selection / zero-byte state.
    fn draw_cell_glyphs(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        ctx: &GridCellCtx<'_>,
        geom: &CellGeom,
    ) {
        let tokens = &ctx.theme.tokens;
        let size = ctx.m.cell_font;
        let is_cursor = geom.off == ctx.cursor;
        let is_dirty = self.model.is_dirty(geom.off);
        let selected = ctx
            .selection
            .is_some_and(|(lo, hi)| geom.off >= lo && geom.off <= hi);

        let hex_color = if is_cursor {
            tokens
                .colors
                .on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
        } else if is_dirty {
            tokens.colors.semantic.warning
        } else if selected {
            tokens.colors.text_primary
        } else if geom.byte == 0 {
            // Zero bytes read as background noise — dim them.
            tokens.colors.neutral[5]
        } else {
            tokens.colors.text_primary
        };
        tr.draw_style(
            scene,
            &format!("{:02X}", geom.byte),
            hex_color,
            t * Affine::translate((geom.hex_x, ctx.text_y)),
            None,
            &TextStyle::new(size)
                .with_weight(if is_dirty {
                    TextWeight::Bold
                } else {
                    TextWeight::Regular
                })
                .with_family(FontChoice::Monospace),
        );

        // ASCII glyph.
        let ch = ascii_glyph(geom.byte);
        let ascii_color = if is_cursor && self.model.column() == Column::Ascii {
            tokens
                .colors
                .on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
        } else if is_dirty {
            tokens.colors.semantic.warning
        } else if (0x20..=0x7E).contains(&geom.byte) {
            tokens.colors.text_primary
        } else {
            tokens.colors.neutral[5]
        };
        tr.draw_style(
            scene,
            &ch,
            ascii_color,
            t * Affine::translate((geom.ascii_x, ctx.text_y)),
            None,
            &TextStyle::new(size).with_family(FontChoice::Monospace),
        );
    }

    /// Byte range the current inspector interpretation highlights — the
    /// widest non-trivial interpretation (u64 when 8 bytes remain, else
    /// whatever fits). Drives the inspected-range tint on the grid.
    fn inspector_span(&self) -> Option<(usize, usize)> {
        let cur = self.model.cursor();
        if self.model.is_empty() {
            return None;
        }
        let remaining = self.model.len() - cur;
        let width = remaining.clamp(1, 8);
        Some((cur, cur + width - 1))
    }

    fn draw_inspector(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &GridMetrics,
    ) {
        if !self.show_inspector() {
            return;
        }
        let tokens = &theme.tokens;
        let panel = Rect::new(
            m.inspector_x0,
            m.grid_top - 2.0,
            self.rect.x1,
            m.grid_bottom,
        );
        paint_side_panel(scene, t, theme, panel);

        let pad = f64::from(tokens.spacing.s4);
        let x = panel.x0 + pad;
        let mut y = panel.y0 + pad;

        let title_size = ts(tr, tokens.typography.scale.xs);
        tr.draw_text_weighted(
            scene,
            "DATA INSPECTOR",
            title_size,
            tokens.colors.text_dim,
            t * Affine::translate((x, y)),
            None,
            TextWeight::Bold,
        );
        // Stacked lines step by their measured boxes (22 / 24 px at 1×).
        y += line_box(tr, title_size) + 9.5;

        // Cursor + endianness sub-header.
        let sub = format!(
            "@ 0x{:X}  ·  {}",
            self.model.cursor(),
            self.model.endian().label()
        );
        let value_size = ts(tr, tokens.typography.scale.sm);
        tr.draw_style(
            scene,
            &sub,
            tokens.colors.accent[6],
            t * Affine::translate((x, y)),
            None,
            &TextStyle::new(value_size).with_family(FontChoice::Monospace),
        );
        let value_line = line_box(tr, value_size);
        y += value_line + 9.0;

        let rows: Vec<Interpretation> =
            inspector::inspect(self.model.bytes(), self.model.cursor(), self.model.endian());
        // One measured pitch for every row (22 px at 1×); the label column is
        // as wide as the widest label it holds plus a gutter, so values never
        // start under a label at any text scale.
        // Row pitch: the line box plus one `s1` (the inter-band gap) and a
        // 3 px seat, all from the same scaled token so the band below can
        // never turn negative when spacing scales.
        let band_gap = f64::from(tokens.spacing.s1);
        let row_h = value_line + band_gap + 3.0;
        let label_w = rows
            .iter()
            .map(|r| {
                f64::from(
                    tr.measure_styled(r.label, value_size, None, TextWeight::Medium, false, true)
                        .0,
                )
            })
            .fold(0.0_f64, f64::max)
            + 12.0;
        // The zebra band is the row minus half an `s1` above and below (so
        // consecutive bands keep a token-step gap, HIG §7), centred on the
        // text's measured line box rather than nudged around an assumed height.
        let band_pad = (row_h - value_line) / 2.0 - band_gap / 2.0;

        for r in &rows {
            if y + row_h > panel.y1 {
                break;
            }
            // Zebra striping for legibility.
            let band = Rect::new(panel.x0, y - band_pad, panel.x1, y + value_line + band_pad);
            scene.fill(
                Fill::NonZero,
                t,
                with_alpha(tokens.colors.surfaces.raised, 60),
                None,
                &band,
            );
            tr.draw_style(
                scene,
                r.label,
                tokens.colors.text_dim,
                t * Affine::translate((x, y)),
                None,
                &TextStyle::new(value_size)
                    .with_weight(TextWeight::Medium)
                    .with_family(FontChoice::Monospace),
            );
            let val_color = if r.value == "—" {
                tokens.colors.neutral[4]
            } else {
                tokens.colors.text_primary
            };
            tr.draw_style(
                scene,
                &r.value,
                val_color,
                t * Affine::translate((x + label_w, y)),
                Some((panel.width() - pad * 2.0 - label_w) as f32),
                &TextStyle::new(value_size).with_family(FontChoice::Monospace),
            );
            y += row_h;
        }
    }

    /// The composed status label for the floating bubble: the cursor offset
    /// (hex + decimal), plus the selection length and dirty count when present,
    /// plus a transient save/open flash folded in when one is active. The
    /// decorative keyboard-hint line the old strip carried is gone — it was
    /// always identical and never told the user anything about their document.
    fn status_bubble_label(&self) -> String {
        let mut s = format!("0x{:08X}  ({})", self.model.cursor(), self.model.cursor());
        let sel = self.model.selection_len();
        if sel > 0 {
            // Writing to a `String` via `fmt::Write` never fails.
            let _ = write!(s, "   sel {sel} B");
        }
        if self.model.dirty_count() > 0 {
            let _ = write!(s, "   ●{} edited", self.model.dirty_count());
        }
        if let Some((msg, _error, _)) = &self.flash {
            let _ = write!(s, "   {msg}");
        }
        s
    }

    /// Draw the transient floating status bubble at the bottom-right of the
    /// window — the reclaimed home of the old bottom status strip. It surfaces
    /// on a real status change (the cursor offset, selection, dirty count, or a
    /// fresh save/error flash) or while the pointer hovers the bottom-right
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
        // Poke only on a genuine change — `draw` runs every frame, so poking
        // unconditionally would pin the bubble open forever.
        if self.last_status_label.as_deref() != Some(label.as_str()) {
            self.status_bubble.poke(now);
            self.last_status_label = Some(label.clone());
        }
        let text_size = ts(tr, theme.tokens.typography.scale.sm);
        let (w, h) = ui::status_bubble::pill_size(&label, tr, text_size, 0.0);
        let margin = 14.0;
        let rect = Rect::new(
            self.rect.x1 - margin - w,
            self.rect.y1 - margin - h,
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        // Reveal zone: the bottom-right corner band (plus the pill), so nudging
        // the pointer there re-surfaces the status on demand.
        let zone = Rect::new(rect.x0 - 40.0, rect.y0 - 24.0, self.rect.x1, self.rect.y1);
        let over = self.shots_status || zone.contains(self.last_mouse);
        self.status_bubble.set_hover(over, now);
        let alpha = self.status_bubble.alpha(now);
        if alpha <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            t,
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, t, tr, theme, rect, &label, text_size);
        scene.pop_layer();
    }

    /// The no-file zero-state: a title, a one-line explanation, and a single
    /// primary action (Open), centred. Records the button's hit rect.
    fn draw_empty_state(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let c = &tokens.colors;
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;

        // The stack is anchored on the button (`by0`) and each line above it
        // sits on its measured line box, so the title, subtitle and button
        // keep their 1× gaps (8 / 17 px) without overlapping at 200 %.
        let by0 = cy - 8.0;

        // Title.
        let title = "No file open";
        let tsz = ts(tr, tokens.typography.scale.x2l);
        let (tw, th) = tr.measure_weighted(title, tsz, None, TextWeight::Semibold);
        // Subtitle.
        let sub = "Open a file to inspect and edit its bytes.";
        let ssz = ts(tr, tokens.typography.scale.sm);
        let (sw, sh) = tr.measure(sub, ssz, None);
        let sub_top = by0 - 17.0 - f64::from(sh);
        let title_top = sub_top - 8.0 - f64::from(th);
        tr.draw_text_weighted(
            scene,
            title,
            tsz,
            c.text_primary,
            t * Affine::translate((cx - f64::from(tw) / 2.0, title_top)),
            None,
            TextWeight::Semibold,
        );
        tr.draw_text(
            scene,
            sub,
            ssz,
            c.text_dim,
            t * Affine::translate((cx - f64::from(sw) / 2.0, sub_top)),
            None,
        );

        // Primary action: Open file…
        let label = "Open file…";
        let bsz = ts(tr, tokens.typography.scale.base);
        let (lw, lh) = tr.measure_weighted(label, bsz, None, TextWeight::Semibold);
        let bw = f64::from(lw) + 44.0;
        // 40 px at 1×; grows with the label's measured line box.
        let bh = (f64::from(lh) + 22.5).max(40.0);
        let bx0 = cx - bw / 2.0;
        let btn = Rect::new(bx0, by0, bx0 + bw, by0 + bh);
        let accent = c.accent[ui_accent_idx()];
        scene.fill(
            Fill::NonZero,
            t,
            accent,
            None,
            &RoundedRect::from_rect(btn, 10.0),
        );
        tr.draw_text_weighted(
            scene,
            label,
            bsz,
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            t * Affine::translate((
                bx0 + (bw - f64::from(lw)) / 2.0,
                by0 + (bh - f64::from(lh)) / 2.0,
            )),
            None,
            TextWeight::Semibold,
        );
        self.open_button = btn;

        // Keyboard hint under the button.
        let hint = "Ctrl+O";
        let hsz = ts(tr, tokens.typography.scale.xs);
        let (hw, _) = tr.measure(hint, hsz, None);
        tr.draw_text(
            scene,
            hint,
            hsz,
            c.text_dim,
            t * Affine::translate((cx - f64::from(hw) / 2.0, by0 + bh + 16.0)),
            None,
        );

        // A pending open error (e.g. unreadable path) surfaces here.
        if let Some((msg, error, _)) = &self.flash
            && *error
        {
            let (mw, _) = tr.measure(msg, ssz, None);
            tr.draw_text(
                scene,
                msg,
                ssz,
                c.semantic.danger,
                t * Affine::translate((cx - f64::from(mw) / 2.0, by0 + bh + 44.0)),
                None,
            );
        }
    }

    /// The search sheet's match counter: empty with no query, otherwise the
    /// match count (and current position when the list is complete).
    fn search_count_label(&self) -> String {
        let query_empty = self.search_input.value().is_empty();
        if self.matches.is_empty() {
            if query_empty {
                String::new()
            } else {
                "no matches".to_string()
            }
        } else {
            if self.matches_truncated {
                format!("{}+ matches", self.matches.len())
            } else {
                let pos = self.match_idx.map_or(0, |i| i + 1);
                format!("{pos}/{}", self.matches.len())
            }
        }
    }

    fn draw_search_overlay(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let w = 460.0;
        // 56 px at 1×; the sheet grows with its `sm` field/count line so the
        // shared input and the counter keep seating at every text scale.
        let count_size = ts(tr, tokens.typography.scale.sm);
        let h = 56.0 + line_growth(tr, count_size);
        let x0 = self.rect.x1 - w - 24.0;
        let y0 = self.rect.y0 + 56.0;
        let panel = RoundedRect::new(x0, y0, x0 + w, y0 + h, 10.0);
        scene.fill(
            Fill::NonZero,
            t,
            tokens.colors.surfaces.raised,
            None,
            &panel,
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            tokens.colors.border_focus,
            None,
            &panel,
        );

        let pad = 14.0;
        // Kind toggle chip: sized from its measured label (48 × 22 px at 1×)
        // and the label centred on its measured line box.
        let kind = self.search_kind.label();
        let kind_size = ts(tr, tokens.typography.scale.xs);
        let (kind_w, kind_h) = tr.measure_weighted(kind, kind_size, None, TextWeight::Bold);
        let chip_w = (f64::from(kind_w) + 16.0).max(48.0);
        let chip_h = f64::from(kind_h) + 9.5;
        let chip = RoundedRect::new(
            x0 + pad,
            y0 + 12.0,
            x0 + pad + chip_w,
            y0 + 12.0 + chip_h,
            6.0,
        );
        scene.fill(
            Fill::NonZero,
            t,
            tokens.colors.accent[ui_accent_idx()],
            None,
            &chip,
        );
        tr.draw_text_weighted(
            scene,
            kind,
            kind_size,
            tokens
                .colors
                .on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            t * Affine::translate((
                x0 + pad + (chip_w - f64::from(kind_w)) / 2.0,
                y0 + 12.0 + (chip_h - f64::from(kind_h)) / 2.0,
            )),
            None,
            TextWeight::Bold,
        );

        // Match counter. It stays app-owned because it describes byte-search
        // policy, while the shared field owns all text-editing behaviour.
        let count = self.search_count_label();
        let count_w = if count.is_empty() {
            0.0
        } else {
            f64::from(
                tr.measure(&count, ts(tr, tokens.typography.scale.sm), None)
                    .0,
            )
        };

        // The shared field supplies selection, word/caret navigation,
        // clipboard, context-menu and IME support inside the existing overlay.
        let qx = x0 + pad + chip_w + 12.0;
        let qright = x0 + w
            - pad
            - if count.is_empty() {
                0.0
            } else {
                count_w + 16.0
            };
        self.search_input
            .apply_layout_external(Rect::new(qx, y0 + 8.0, qright, y0 + h - 8.0));
        let presence = PresenceCtx::empty();
        self.search_input.draw(scene, t, tr, theme, &presence);

        if !count.is_empty() {
            let ccolor = if self.matches.is_empty() {
                tokens.colors.semantic.danger
            } else {
                tokens.colors.text_dim
            };
            let count_top = y0 + (h - line_box(tr, count_size)) / 2.0;
            tr.draw_text(
                scene,
                &count,
                count_size,
                ccolor,
                t * Affine::translate((x0 + w - pad - count_w, count_top)),
                None,
            );
        }
    }

    fn draw_goto_overlay(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let w = 340.0;
        // The caption sits on its measured line and the shared input starts
        // just beneath it (19 px at 1×); the sheet grows with both lines.
        let caption_size = ts(tr, tokens.typography.scale.xs);
        let caption_line = line_box(tr, caption_size);
        let input_top = 8.0 + caption_line - 1.5;
        let input_h = 34.0 + line_growth(tr, ts(tr, tokens.typography.scale.sm));
        let h = input_top + input_h + 3.0;
        let x0 = self.rect.x0 + (self.rect.width() - w) / 2.0;
        let y0 = self.rect.y0 + 80.0;
        let panel = RoundedRect::new(x0, y0, x0 + w, y0 + h, 10.0);
        scene.fill(
            Fill::NonZero,
            t,
            tokens.colors.surfaces.raised,
            None,
            &panel,
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            tokens.colors.border_focus,
            None,
            &panel,
        );

        let pad = 14.0;
        tr.draw_text_weighted(
            scene,
            "Go to offset",
            caption_size,
            tokens.colors.text_dim,
            t * Affine::translate((x0 + pad, y0 + 8.0)),
            None,
            TextWeight::Bold,
        );
        self.goto_input.apply_layout_external(Rect::new(
            x0 + pad,
            y0 + input_top,
            x0 + w - pad,
            y0 + h - 3.0,
        ));
        let presence = PresenceCtx::empty();
        self.goto_input.draw(scene, t, tr, theme, &presence);
    }

    // --- keyboard ------------------------------------------------------

    /// Route an event into the active shared field, keeping only byte-search
    /// policy (kind cycling / match navigation / offset commit) in this app.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "OverlayLayoutCtx bundles a `&mut Option<NodeId>` and is built fresh at the \
                  single call site purely to move its fields on into the active field's \
                  Widget::handle_event; it isn't Copy and taking it by reference would still \
                  need to move the mutable reference back out, so by-value is the real shape"
    )]
    fn handle_overlay_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        layout: OverlayLayoutCtx<'_>,
    ) -> bool {
        let shift = modifiers.state().shift_key();
        if let WindowEvent::KeyboardInput { event: ke, .. } = event
            && ke.state == ElementState::Pressed
        {
            match (self.overlay, &ke.logical_key) {
                (Overlay::Search, Key::Named(NamedKey::Escape)) => {
                    self.search_input.set_focus(false);
                    self.overlay = Overlay::None;
                    self.dirty = true;
                    return true;
                }
                (Overlay::Search, Key::Named(NamedKey::Enter)) => {
                    self.jump_to_match(!shift);
                    self.record_local_history();
                    return true;
                }
                (Overlay::Search, Key::Named(NamedKey::Tab)) => {
                    self.search_kind = self.search_kind.toggle();
                    self.search_input
                        .set_mono(self.search_kind == SearchKind::Hex);
                    self.recompute_matches();
                    self.dirty = true;
                    return true;
                }
                (Overlay::Goto, Key::Named(NamedKey::Escape)) => {
                    self.goto_input.set_text("");
                    self.goto_input.set_focus(false);
                    self.overlay = Overlay::None;
                    self.dirty = true;
                    return true;
                }
                (Overlay::Goto, Key::Named(NamedKey::Enter)) => {
                    self.commit_goto();
                    self.record_local_history();
                    return true;
                }
                _ => {}
            }
        }

        let mut field_handled = false;
        let changed = match self.overlay {
            Overlay::Search => {
                self.search_input.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor,
                        modifiers: &modifiers,
                        state: doc,
                        path,
                        handled: &mut field_handled,
                        focus_target: layout.focus_target,
                        node_id: layout.node_id,
                        taffy: layout.taffy,
                    },
                );
                self.search_input.take_dirty().is_some()
            }
            Overlay::Goto => {
                self.goto_input.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor,
                        modifiers: &modifiers,
                        state: doc,
                        path,
                        handled: &mut field_handled,
                        focus_target: layout.focus_target,
                        node_id: layout.node_id,
                        taffy: layout.taffy,
                    },
                );
                self.goto_input.take_dirty().is_some()
            }
            Overlay::None => false,
        };
        if changed {
            if self.overlay == Overlay::Search {
                self.recompute_matches();
            }
            self.dirty = true;
        }
        field_handled || changed
    }
}

/// Index into the accent ramp used for the active cursor fill. Bound to a
/// helper so the choice stays consistent across the grid and overlays.
fn ui_accent_idx() -> usize {
    5
}

impl Widget for HexView {
    ui::provides!(dyn ui::resources::PublishesResources);

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

        // Expire a status flash so an idle window settles back to no message.
        if let Some((_, _, born)) = &self.flash
            && born.elapsed().as_millis() > FLASH_MS
        {
            self.flash = None;
            self.dirty = true;
        }
    }

    fn needs_idle_sync(&self) -> bool {
        self.is_busy()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming while a portal pick is in flight (so it drains
        // once the host dialog closes), while a flash is counting down, and
        // while the floating status bubble is lingering / fading so it hides
        // itself without needing a stray pointer event.
        self.is_busy() || self.status_bubble.needs_frame(ui::time::now())
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // App background.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        // No file loaded: a composed zero-state, not a fabricated buffer.
        if !self.has_file {
            self.draw_empty_state(scene, transform, text_renderer, theme);
            return;
        }

        // Measure the cell font (and the title bar's line growth) with the
        // renderer that paints it, then reflow the byte grid to fill the
        // current width before laying out. One `GridMetrics` owns every
        // vertical edge on both the paint and the event path.
        self.cell_metrics = CellMetrics::measure(text_renderer);
        let fit = self.fit_bytes_per_row();
        if fit != self.model.bytes_per_row() {
            self.model.set_bytes_per_row(fit);
        }

        let m = self.metrics();
        self.draw_header(scene, transform, text_renderer, theme, &m);
        self.draw_grid(scene, transform, text_renderer, theme, &m);
        self.draw_inspector(scene, transform, text_renderer, theme, &m);
        // The permanent status strip is gone; a transient floating bubble
        // surfaces the cursor read-out over the reclaimed space at the bottom.
        self.draw_status_bubble(scene, transform, text_renderer, theme);
        if self.shots_overlay && self.overlay == Overlay::Search {
            self.draw_search_overlay(scene, transform, text_renderer, theme);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        match self.overlay {
            Overlay::Search => self.draw_search_overlay(scene, transform, text_renderer, theme),
            Overlay::Goto => self.draw_goto_overlay(scene, transform, text_renderer, theme),
            Overlay::None => {}
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let ctrl = ctx.modifiers.state().control_key();
        let shift = ctx.modifiers.state().shift_key();

        if self.overlay != Overlay::None {
            if self.handle_overlay_event(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                ctx.path,
                OverlayLayoutCtx {
                    focus_target: ctx.focus_target,
                    node_id: ctx.node_id,
                    taffy: ctx.taffy,
                },
            ) {
                *ctx.handled = true;
                return;
            }
            // An open prompt captures keys even when the field deliberately
            // ignores the chord, so grid editing never fires behind it.
            if matches!(event, WindowEvent::KeyboardInput { .. }) {
                return;
            }
        }

        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_mouse_press(shift, ctx.handled),
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y / 20.0,
                };
                self.model.scroll_by(-(lines.round() as isize) * 3);
                self.dirty = true;
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

    fn ime_cursor_rect(&self) -> Option<Rect> {
        match self.overlay {
            Overlay::Search => self.search_input.ime_cursor_rect(),
            Overlay::Goto => self.goto_input.ime_cursor_rect(),
            Overlay::None => None,
        }
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        match self.overlay {
            Overlay::Search => self.search_input.ime_purpose(),
            Overlay::Goto => self.goto_input.ime_purpose(),
            Overlay::None => None,
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        match self.overlay {
            Overlay::Search => self.search_input.cursor_shape(point),
            Overlay::Goto => self.goto_input.cursor_shape(point),
            Overlay::None => None,
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        match self.overlay {
            Overlay::Search => self.search_input.accessibility(),
            Overlay::Goto => self.goto_input.accessibility(),
            Overlay::None => {
                use ui::a11y::{AccessNode, AccessState, Politeness, Role};

                let mut children = vec![
                    AccessNode::new(Role::StaticText)
                        .with_name(if self.has_file {
                            format!(
                                "{}; {} bytes; cursor at hexadecimal 0x{:X}; {} mode; {} endian",
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
                            "Keyboard: Ctrl+O open, Ctrl+S save, Ctrl+F search, Ctrl+G go to offset, Ctrl+Z undo, Ctrl+Y redo, Tab switches hex and ASCII, Insert switches overwrite and insert.",
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
                } else {
                    children.push(
                        AccessNode::new(Role::Button)
                            .with_name("Open file")
                            .with_description("Choose a local file to inspect")
                            .with_state(AccessState::default().focusable(true))
                            .with_bounds(self.open_button)
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
        }
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
        if self.hosted {
            return crate::actions::live_buffer_extras(&self.hex_resource_ctx());
        }
        self.publish_resources(doc)
    }
}

impl HexView {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
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
            HexOperation::Search if self.has_file => {
                self.overlay = Overlay::Search;
                self.search_input.set_focus(true);
                self.recompute_matches();
                self.dirty = true;
            }
            HexOperation::GotoOffset if self.has_file => {
                self.overlay = Overlay::Goto;
                self.goto_input.set_focus(true);
                self.dirty = true;
            }
            HexOperation::Edit { edit } if self.has_file => {
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
        doc: &AutoCommit,
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
    /// Left-click handling for the byte grid: the empty-state Open button, or
    /// a grid hit-test that moves the cursor (optionally extending the
    /// selection). Split out of `Widget::handle_event` to keep that dispatch
    /// under the line-count limit.
    fn handle_mouse_press(&mut self, shift: bool, handled: &mut bool) {
        // Empty state: the only affordance is the centred Open button.
        if !self.has_file {
            if self.open_button.contains(self.last_mouse) {
                self.begin_open();
                *handled = true;
            }
            return;
        }
        // Click position is tracked via CursorMoved; recompute on press from
        // the last known position.
        if let Some((off, col)) = self.hit_test(self.last_mouse) {
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
                self.overlay = Overlay::Search;
                self.search_input.set_focus(true);
                self.search_input
                    .set_cursor_pos(self.search_input.value().len());
                self.recompute_matches();
                self.dirty = true;
                *handled = true;
                true
            }
            "g" | "G" => {
                self.overlay = Overlay::Goto;
                self.goto_input.set_focus(true);
                self.goto_input
                    .set_cursor_pos(self.goto_input.value().len());
                self.dirty = true;
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
                self.model.clear_selection();
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

/// Per-row state shared across a row's cells, bundled so
/// [`HexView::draw_grid_cell`] stays under the argument-count limit.
struct GridCellCtx<'a> {
    theme: &'a Theme,
    m: &'a GridMetrics,
    selection: Option<(usize, usize)>,
    inspector_span: Option<(usize, usize)>,
    cursor: usize,
    y: f64,
    text_y: f64,
}

/// Byte position + value for one grid cell, bundled so
/// [`HexView::draw_cell_glyphs`] stays under the argument-count limit.
struct CellGeom {
    off: usize,
    byte: u8,
    hex_x: f64,
    ascii_x: f64,
}

/// The monospace metrics every grid pitch derives from — one glyph advance
/// and one line height, measured with the exact style the cells are painted
/// in. Layout (`fit_bytes_per_row`), paint (`draw_grid`) and hit-testing
/// (`hit_test`) all build their [`GridMetrics`] from this, so a byte's
/// painted cell is its click target at every text scale.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CellMetrics {
    /// Advance of one monospace glyph at [`GridMetrics::CELL_FONT`].
    glyph_w: f64,
    /// Line height of one row of cell text.
    line_h: f64,
    /// How much the title bar's `lg` line has grown over its 1× box at the
    /// current accessibility scale — the bar adds exactly this to its 44 px
    /// design height so the file name and the pills keep seating.
    title_growth: f64,
}

impl CellMetrics {
    /// Breathing room above and below a row's line box.
    const ROW_PAD_Y: f64 = 1.0;
    /// Metrics before the first draw has measured anything: the 0.6 em
    /// advance and 1.25 line height the monospace face resolves to at 1×.
    const FALLBACK: Self = Self {
        glyph_w: GridMetrics::CELL_FONT as f64 * 0.6,
        line_h: GridMetrics::CELL_FONT as f64 * 1.25,
        title_growth: 0.0,
    };

    /// Measure the cell font with the renderer that will paint it. Eight
    /// glyphs — the offset gutter's width — so a fractional advance is read
    /// at the precision the gutter needs. The title bar's line growth is
    /// measured here too, from the same renderer, so the event path's
    /// `metrics()` and the painted frame place the grid at the same y.
    fn measure(tr: &mut TextRenderer) -> Self {
        let (w, h) = tr.measure_styled(
            "00000000",
            GridMetrics::CELL_FONT,
            None,
            TextWeight::Regular,
            false,
            true,
        );
        if w <= 0.0 || h <= 0.0 {
            return Self::FALLBACK;
        }
        // The title's `lg` token in the renderer's input space is the 1× type
        // ramp's `lg` (the renderer applies the accessibility scale on top),
        // so no theme is needed to measure how much the title line grows.
        let title_growth = line_growth(tr, ui::tokens::TypeScale::default().lg);
        Self {
            glyph_w: f64::from(w) / 8.0,
            line_h: f64::from(h),
            title_growth,
        }
    }

    /// One ASCII glyph.
    fn char_w(self) -> f64 {
        self.glyph_w
    }

    /// Two hex digits plus the inter-byte gap — one byte column's pitch.
    fn hex_col_w(self) -> f64 {
        self.glyph_w * 2.55
    }

    /// Eight hex digits of offset.
    fn gutter_w(self) -> f64 {
        self.glyph_w * 8.0
    }

    /// Row pitch: the measured line box plus symmetric padding, rounded up so
    /// rows land on whole pixels (20px at 1× for the 14px cell font).
    fn row_h(self) -> f64 {
        (self.line_h + 2.0 * Self::ROW_PAD_Y).ceil()
    }
}

/// Computed layout metrics for one frame. All in logical px.
#[derive(Clone, Copy, Debug)]
struct GridMetrics {
    header_bottom: f64,
    col_header_y: f64,
    grid_top: f64,
    grid_bottom: f64,
    row_h: f64,
    /// Measured line height of the cell text, centred inside `row_h`.
    line_h: f64,
    cell_font: f32,
    /// Caption-size cell font for the Offset / column-index / ASCII header
    /// labels, so they scale with the byte pitch they annotate.
    header_font: f32,
    byte_w: f64,
    char_w: f64,
    hex_col_w: f64,
    gutter_x0: f64,
    hex_x0: f64,
    hex_x1: f64,
    ascii_x0: f64,
    ascii_x1: f64,
    inspector_x0: f64,
    visible_rows: usize,
}

impl GridMetrics {
    /// Base size of the cell font. A literal base size, not a theme token, so
    /// the renderer's accessibility scale applies to it exactly once — and
    /// [`CellMetrics::measure`] reads the scaled advance back from the same
    /// renderer.
    const CELL_FONT: f32 = 14.0;
    /// The column header's caption size relative to the cell font (10px at
    /// 1×, the `xs` step of the type scale) — a ratio rather than the token so
    /// the header labels and the cells they annotate share one scale path.
    const COL_HEADER_FONT_RATIO: f32 = 10.0 / 14.0;
    const LEFT_PAD: f64 = 16.0;
    /// Gap between the offset gutter and the hex columns.
    const GUTTER_GAP: f64 = 22.0;
    /// Gap between the hex columns and the ASCII column.
    const HEX_ASCII_GAP: f64 = 22.0;
    /// Gap between the ASCII column and the inspector panel.
    const INSPECTOR_GAP: f64 = 24.0;
    /// Smallest inspector width the fit calculation keeps room for.
    const INSPECTOR_MIN: f64 = 300.0;

    fn new(rect: Rect, bpr: usize, cell: CellMetrics) -> Self {
        // The title bar is 44 px at 1× and grows by exactly its `lg` line's
        // measured growth, so the file name and pills stay seated at 200 %.
        let header_bottom = rect.y0 + 44.0 + cell.title_growth;
        // The column header is the cell font at caption size, so its offset
        // labels stay on the byte pitch at every scale; its band grows with
        // the measured line height (20px at 1×, as before).
        let header_font = Self::CELL_FONT * Self::COL_HEADER_FONT_RATIO;
        let col_header_h = (cell.line_h * f64::from(Self::COL_HEADER_FONT_RATIO)
            + 2.0 * CellMetrics::ROW_PAD_Y)
            .ceil()
            .max(20.0);
        let col_header_y = header_bottom + 10.0;
        let grid_top = col_header_y + col_header_h;
        // No permanent status strip any more — the cursor read-out floats in a
        // transient bubble — so the byte grid + inspector reclaim that space and
        // span to the window bottom, keeping only a small margin off the edge.
        let grid_bottom = rect.y1 - 8.0;

        let cell_font = Self::CELL_FONT;
        let row_h = cell.row_h();
        let line_h = cell.line_h;
        let byte_w = cell.glyph_w * 2.0;
        let char_w = cell.char_w();
        let hex_col_w = cell.hex_col_w();

        let gutter_x0 = rect.x0 + Self::LEFT_PAD;
        let hex_x0 = gutter_x0 + cell.gutter_w() + Self::GUTTER_GAP;
        let hex_x1 = hex_x0 + bpr as f64 * hex_col_w;
        let ascii_x0 = hex_x1 + Self::HEX_ASCII_GAP;
        let ascii_x1 = ascii_x0 + bpr as f64 * char_w;

        // The inspector is a desktop side panel. On a narrow surface the grid
        // gets the full width; reserving a permanent 300px inspector would
        // make byte cells too narrow to read or target.
        let inspector_x0 = if rect.width() >= 860.0 {
            (ascii_x1 + Self::INSPECTOR_GAP).min(rect.x1 - Self::INSPECTOR_MIN)
        } else {
            rect.x1
        };

        let visible_rows = ((grid_bottom - grid_top) / row_h).floor().max(1.0) as usize;

        Self {
            header_bottom,
            col_header_y,
            grid_top,
            grid_bottom,
            row_h,
            line_h,
            cell_font,
            header_font,
            byte_w,
            char_w,
            hex_col_w,
            gutter_x0,
            hex_x0,
            hex_x1,
            ascii_x0,
            ascii_x1,
            inspector_x0,
            visible_rows,
        }
    }
}

// --- small drawing utilities ------------------------------------------

/// ASCII glyph for the ASCII column: the printable char, or a centred dot
/// for control / high bytes (the universal hex-editor convention).
fn ascii_glyph(b: u8) -> String {
    if (0x20..=0x7E).contains(&b) {
        (b as char).to_string()
    } else {
        "·".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The shared text fields keep the hex editor's own parsing policies.
    #[test]
    fn shared_search_and_goto_fields_keep_byte_policies() {
        // The shared search field still drives the byte-match policy.
        let mut view = HexView::new(vec![0xde, 0xad, 0x00, 0xde, 0xad], "bytes.bin");
        view.search_input.set_text("de ad");
        view.recompute_matches();

        assert_eq!(view.search_input.value(), "de ad");
        assert_eq!(view.matches, vec![0, 3]);
        assert_eq!(view.match_idx, Some(0));

        view.search_kind = SearchKind::Text;
        view.search_input.set_text("Þ");
        view.recompute_matches();
        assert!(view.matches.is_empty());

        // The shared goto field keeps hex and decimal offset parsing.
        let mut view = HexView::new(vec![0; 64], "bytes.bin");

        view.overlay = Overlay::Goto;
        view.goto_input.set_text("0x1f");
        view.commit_goto();
        assert_eq!(view.model.cursor(), 31);
        assert!(view.goto_input.value().is_empty());

        view.overlay = Overlay::Goto;
        view.goto_input.set_text("12");
        view.commit_goto();
        assert_eq!(view.model.cursor(), 12);

        // A large search retains a bounded result index.
        let mut view = HexView::new(vec![0; search::MAX_RETAINED_MATCHES + 50], "large.bin");
        view.search_input.set_text("00");
        view.recompute_matches();

        assert_eq!(view.matches.len(), search::MAX_RETAINED_MATCHES);
        assert!(view.matches_truncated);
        assert!(view.is_match_offset(0));
        assert!(view.is_match_offset(search::MAX_RETAINED_MATCHES - 1));
        assert!(view.is_match_offset(search::MAX_RETAINED_MATCHES));
    }

    /// Surface geometry and semantics.
    ///
    /// The grid's pitches come from the renderer's measured monospace advance
    /// and line height, so paint and hit-testing share one geometry at every
    /// text scale: a painted cell's centre maps back to its own byte.
    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "layout reproduces authored whole-pixel sizes and their sums exactly in binary floating point; exact equality is what the test checks"
    )]
    fn surface_geometry_follows_measured_metrics_width_and_state() {
        use ui::a11y::Role;

        let mut tr = TextRenderer::new();
        let base = CellMetrics::measure(&mut tr);
        assert!(
            base.glyph_w > 0.0 && base.line_h > 0.0,
            "measured: {base:?}"
        );
        assert_eq!(base.row_h(), 20.0, "1× keeps the 20px row density");
        tr.set_text_scale(2.0);
        let large = CellMetrics::measure(&mut tr);
        assert!(
            (large.glyph_w - base.glyph_w * 2.0).abs() < 0.05
                && (large.line_h - base.line_h * 2.0).abs() < 0.05,
            "pitches follow the renderer scale: {base:?} → {large:?}"
        );

        // The title bar grows with its `lg` line at 2×, and that growth is
        // part of the same measurement the event path reads, so `metrics()`
        // before and after a painted frame agree on where the grid starts.
        assert!(
            base.title_growth.abs() < 1e-6,
            "no title growth at 1×: {base:?}"
        );
        assert!(
            large.title_growth > 4.0,
            "the title line grows at 2×: {large:?}"
        );

        let mut view = HexView::new(vec![0xAB; 96], "scale.bin");
        view.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);
        for cell in [base, large] {
            view.cell_metrics = cell;
            let before_paint = view.metrics();
            // Paint a real frame at this cell's scale and read the metrics the
            // frame was laid out with; the pre-paint event-path metrics must
            // already match them (same `CellMetrics::measure`).
            let scale = if cell == large { 2.0 } else { 1.0 };
            let theme = Theme::dark().with_text_scale(scale);
            tr.set_text_scale(scale);
            let mut scene = Scene::new();
            Widget::draw(
                &mut view,
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::empty(),
            );
            assert_eq!(
                view.cell_metrics, cell,
                "draw measures the same cell metrics"
            );
            let m = view.metrics();
            assert!(
                (m.header_bottom - before_paint.header_bottom).abs() < 1e-6
                    && (m.grid_top - before_paint.grid_top).abs() < 1e-6,
                "event-path metrics match the painted frame: {before_paint:?} vs {m:?}"
            );
            assert!((m.row_h - cell.row_h()).abs() < f64::EPSILON);
            assert!((m.hex_col_w - cell.hex_col_w()).abs() < f64::EPSILON);
            let bpr = view.model.bytes_per_row();
            for off in [0usize, 1, bpr - 1, bpr, 2 * bpr + 3] {
                let (row, col) = (off / bpr, off % bpr);
                let y = m.grid_top + row as f64 * m.row_h + m.row_h / 2.0;
                let hx = m.hex_x0 + col as f64 * m.hex_col_w + m.byte_w / 2.0;
                assert_eq!(
                    view.hit_test(Point::new(hx, y)),
                    Some((off, Column::Hex)),
                    "hex cell {off} hit-tests to itself ({cell:?})"
                );
                let ax = m.ascii_x0 + col as f64 * m.char_w + m.char_w / 2.0;
                assert_eq!(
                    view.hit_test(Point::new(ax, y)),
                    Some((off, Column::Ascii)),
                    "ascii cell {off} hit-tests to itself ({cell:?})"
                );
            }
        }

        // A narrow layout hides the inspector without shrinking the grid.
        let mut view = HexView::new(vec![0; 128], "narrow.bin");
        view.rect = Rect::new(0.0, 0.0, 540.0, 500.0);

        assert!(!view.show_inspector());
        assert!(view.fit_bytes_per_row() >= 8);
        assert!((view.metrics().inspector_x0 - view.rect.x1).abs() < f64::EPSILON);

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
}
