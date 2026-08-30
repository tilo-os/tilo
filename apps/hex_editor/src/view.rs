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
//!              data inspector panel                              (right)
//!              (the grid + inspector span to the window bottom)
//!   * status — a transient floating bubble (cursor offset, selection
//!              length, dirty count, save/error flash) that surfaces on a
//!              change or a bottom-corner hover, then fades; no permanent strip
//!   * overlay — search bar and goto-offset prompt (modal-ish, on top)
//!
//! State lives in the CRDT: every edit is written straight back into the
//! Automerge document via [`HexDoc`], so undo/redo and live collaboration
//! come for free. The view keeps the decoded [`HexModel`] as a render
//! cache, rebuilt from the document in `sync`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::time::Instant;

use automerge::{AutoCommit, ChangeHash, ObjId};
use qos_actions::EncodedAction;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::TextInput;

use crate::doc::HexDoc;
use crate::inspector::{self, Interpretation};
use crate::model::{Column, EditMode, HexModel};
use crate::portal::{self, PortalChannel, PortalOutcome};
use crate::search::{self, SearchKind};

/// How long a transient status message (Saved / open error) stays up.
const FLASH_MS: u128 = 3500;

/// Which transient prompt, if any, is capturing keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overlay {
    None,
    Search,
    Goto,
}

pub struct HexView {
    /// Decoded render cache of the document. Rebuilt in `sync`.
    model: HexModel,
    /// File name shown in the header (cosmetic; the buffer is the doc).
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
    /// Index into `matches` of the currently-focused hit.
    match_idx: Option<usize>,

    // --- goto state ---
    /// Shared local text field for decimal / hexadecimal offsets.
    goto_input: TextInput,

    /// Whether the user has seeded the doc yet (so the first sync writes
    /// the model in rather than reading an empty document back out).
    seeded: bool,
    /// Rows currently fitting in the grid viewport — updated each draw,
    /// read by keyboard paging so PageUp/Down move a screenful.
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
    /// An in-flight XDG-portal save-destination request.
    save_rx: Option<PortalChannel>,
    /// A transient status message (e.g. "Saved 4.2 kB") with its birth time.
    flash: Option<(String, bool, Instant)>,
    /// Hit rect of the empty-state "Open file…" button, recorded during draw.
    open_button: Rect,

    /// Framework-owned action dispatch: brackets the undo boundary (the same undo
    /// path a Ctrl+K palette dispatch takes), encodes and dispatches each
    /// buffer-edit action, and remembers the last one so a test can prove a
    /// keystroke and the Ctrl+K [`qos_actions::ActionSpec`] build the
    /// byte-identical action.
    actions: ui::ActionHost,

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
                .with_id("hex.search")
                .with_placeholder("search…")
                .with_plain_chrome()
                .with_mono(),
            search_kind: SearchKind::Hex,
            matches: Vec::new(),
            match_idx: None,
            goto_input: TextInput::local()
                .with_id("hex.goto")
                .with_placeholder("0x… or decimal")
                .with_plain_chrome()
                .with_mono(),
            seeded: false,
            visible_rows: 1,
            last_mouse: Point::ZERO,
            has_file: true,
            path: None,
            open_rx: None,
            save_rx: None,
            flash: None,
            open_button: Rect::ZERO,
            actions: ui::ActionHost::new(),
            status_bubble: ui::StatusBubble::new(),
            last_status_label: None,
            shots_status: false,
        }
    }

    /// The editor with no file loaded — the honest live default. Shows a
    /// composed empty state whose only affordance is opening a file; it never
    /// fabricates a buffer.
    pub fn empty() -> Self {
        let mut v = Self::new(Vec::new(), "No file");
        v.has_file = false;
        v
    }

    /// Record the on-disk path this buffer came from, so `Ctrl+S` writes back
    /// to it without a chooser.
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

    /// Test accessor for the last action a keystroke dispatched, so a test can
    /// prove the keystroke and the palette build the byte-identical action.
    /// Not `#[cfg(test)]` so the integration test crate (`tests/open_save.rs`),
    /// which compiles against the public API only, can read it too.
    pub fn last_dispatched(&self) -> Option<&EncodedAction> {
        self.actions.last_dispatched()
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

    /// Test hook: feed a portal open/save outcome through the *real* drain
    /// path, exactly as the worker thread would deliver it. Lets the e2e suite
    /// exercise the production open flow without a live host file chooser.
    pub fn inject_open_outcome_for_test(&mut self, outcome: PortalOutcome) {
        self.open_rx = Some(portal::inject_for_test(outcome));
    }

    /// Place the initial cursor at `offset`. Useful for opening a file
    /// "at" a field of interest. Clamped to the buffer.
    pub fn with_cursor(mut self, offset: usize) -> Self {
        self.model.move_to(offset, false);
        self
    }

    /// Choose the initial inspector endianness. Big-endian suits opening
    /// network / image formats whose multi-byte fields are big-endian.
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
        self.status_bubble.poke(Instant::now());
    }

    /// Seed / replace the whole shared buffer at once — used when opening a file
    /// or adopting a peer's document, not for incremental edits. Writing the
    /// bytes wholesale (rather than through an edit action) is right here: a file
    /// load is external I/O, not a collaborative mutation.
    fn flush(&mut self, doc: &mut AutoCommit) {
        HexDoc::write_model(doc, &self.model);
        self.dirty = true;
    }

    /// Persist whatever the last input changed. Buffer mutations the model
    /// journaled are dispatched through the qOS action model — each replays
    /// byte-identically on peers and the batch records one undo boundary Ctrl+Z
    /// reverts. Presentation meta (cursor, mode, endianness) is view state, so
    /// it is written straight to the document, never as an action.
    fn persist(&mut self, doc: &mut AutoCommit) {
        for edit in self.model.take_edits() {
            self.perform_action(doc, crate::actions::edit_to_action(&edit));
        }
        HexDoc::write_meta(doc, &self.model);
        self.dirty = true;
    }

    /// Dispatch one [`crate::actions::HexEditorAction`] — the single buffer-edit
    /// path shared with the Ctrl+K palette. The action runs through the qOS
    /// executor (a pinned actor + committed change, so it replays byte-identically
    /// on peers). The heads captured just before the first action of the event
    /// are stashed so the host records **one** undo boundary bracketing it.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: crate::actions::HexEditorAction) {
        self.actions
            .dispatch(doc, crate::actions::HEX_EDITOR_ACTION_TYPE, &action);
    }

    // --- file open / save (through the host portal) --------------------

    /// Start the host file chooser to open a file. No-op if one is already in
    /// flight. The pick arrives over `open_rx`, drained in `sync`.
    fn begin_open(&mut self) {
        if self.open_rx.is_none() {
            self.open_rx = Some(portal::request_open());
        }
    }

    /// Save the buffer. With a known path this writes straight back; without
    /// one (a never-saved buffer) it opens the host Save chooser.
    fn begin_save(&mut self) {
        if let Some(path) = self.path.clone() {
            self.write_to(&path);
        } else if self.save_rx.is_none() {
            self.save_rx = Some(portal::request_save(self.suggested_name()));
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
    fn write_to(&mut self, path: &std::path::Path) {
        match std::fs::write(path, self.model.bytes()) {
            Ok(()) => {
                self.model.clear_dirty();
                self.path = Some(path.to_path_buf());
                self.flash(format!("Saved {}", human_size(self.model.len())), false);
            }
            Err(e) => self.flash(format!("Save failed: {e}"), true),
        }
        self.dirty = true;
    }

    /// Load `path` into the editor, replacing the buffer and persisting it to
    /// the shared document so collaborators converge on the opened file.
    fn open_path(&mut self, path: PathBuf, doc: &mut AutoCommit) {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let name = path.file_name().map_or_else(
                    || "untitled".to_string(),
                    |s| s.to_string_lossy().into_owned(),
                );
                let n = bytes.len();
                self.model = HexModel::new(bytes);
                self.file_name = name;
                self.path = Some(path);
                self.has_file = true;
                self.overlay = Overlay::None;
                self.matches.clear();
                self.match_idx = None;
                self.flush(doc);
                self.flash(format!("Opened {}", human_size(n)), false);
            }
            Err(e) => self.flash(format!("Open failed: {e}"), true),
        }
    }

    /// Drain any completed portal open/save requests. Called from `sync`, so
    /// it has the document to persist a freshly-opened buffer into.
    fn drain_portal(&mut self, doc: &mut AutoCommit) {
        if let Some(rx) = self.open_rx.as_ref() {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.open_rx = None;
                    match outcome {
                        PortalOutcome::Opened(path) => self.open_path(path, doc),
                        PortalOutcome::Cancelled => {}
                        PortalOutcome::Failed(reason) => self.flash(reason, true),
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.open_rx = None,
            }
        }
        if let Some(rx) = self.save_rx.as_ref() {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.save_rx = None;
                    match outcome {
                        PortalOutcome::Opened(dest) => self.write_to(&dest),
                        PortalOutcome::Cancelled => {}
                        PortalOutcome::Failed(reason) => self.flash(reason, true),
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.save_rx = None,
            }
        }
    }

    /// Post a transient status message.
    fn flash(&mut self, msg: impl Into<String>, error: bool) {
        self.flash = Some((msg.into(), error, Instant::now()));
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
        let avail = self.rect.width()
            - GridMetrics::LEFT_PAD
            - GridMetrics::GUTTER_W
            - GridMetrics::GUTTER_GAP
            - GridMetrics::HEX_ASCII_GAP
            - GridMetrics::INSPECTOR_GAP
            - GridMetrics::INSPECTOR_MIN;
        let per_byte = GridMetrics::HEX_COL_W + GridMetrics::CHAR_W;
        if per_byte <= 0.0 {
            return 16;
        }
        let raw = (avail / per_byte).floor() as i64;
        let snapped = (raw / 8) * 8;
        snapped.clamp(8, 32) as usize
    }

    /// Recompute match offsets from the current query + kind.
    fn recompute_matches(&mut self) {
        self.matches = match search::needle(self.search_input.value(), self.search_kind) {
            Some(n) => search::find_all(self.model.bytes(), &n),
            None => Vec::new(),
        };
        // Keep match_idx pointing at the nearest hit to the cursor.
        self.match_idx = if self.matches.is_empty() {
            None
        } else {
            let cur = self.model.cursor();
            Some(self.matches.iter().position(|&m| m >= cur).unwrap_or(0))
        };
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
        GridMetrics::new(self.rect, self.model.bytes_per_row())
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
        scene.fill(
            Fill::NonZero,
            t,
            tokens.colors.border_standard,
            None,
            &Rect::new(bar.x0, bar.y1 - 1.0, bar.x1, bar.y1),
        );

        let pad = f64::from(tokens.spacing.s4);
        let cy = bar.y0 + (bar.height() - f64::from(tokens.typography.scale.lg)) / 2.0;

        // File name (semibold) + byte count (dim).
        tr.draw_text_weighted(
            scene,
            &self.file_name,
            tokens.typography.scale.lg,
            tokens.colors.text_primary,
            t * Affine::translate((bar.x0 + pad, cy)),
            None,
            TextWeight::Semibold,
        );
        let (name_w, _) = tr.measure_weighted(
            &self.file_name,
            tokens.typography.scale.lg,
            None,
            TextWeight::Semibold,
        );
        let meta = format!("{} bytes", self.model.len());
        tr.draw_text(
            scene,
            &meta,
            tokens.typography.scale.sm,
            tokens.colors.text_dim,
            t * Affine::translate((
                bar.x0 + pad + f64::from(name_w) + 12.0,
                bar.y0 + (bar.height() - f64::from(tokens.typography.scale.sm)) / 2.0,
            )),
            None,
        );

        // Right-aligned pills: mode + endianness + width.
        let mut x = bar.x1 - pad;
        let width_label = format!("{}/row", self.model.bytes_per_row());
        x = self.draw_pill(
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
        let endian_color = tokens.colors.accent[4];
        x = self.draw_pill(
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
        let _ = self.draw_pill(
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
    #[allow(clippy::too_many_arguments)]
    fn draw_pill(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        right_x: f64,
        bar: Rect,
        label: &str,
        fill: Color,
    ) -> f64 {
        let size = theme.tokens.typography.scale.xs;
        let (tw, _) = tr.measure_weighted(label, size, None, TextWeight::Bold);
        let pad_x = 8.0;
        let w = f64::from(tw) + pad_x * 2.0;
        let h = 18.0;
        let x0 = right_x - w;
        let y0 = bar.y0 + (bar.height() - h) / 2.0;
        let shape = RoundedRect::new(x0, y0, right_x, y0 + h, 6.0);
        scene.fill(Fill::NonZero, t, fill, None, &shape);
        tr.draw_text_weighted(
            scene,
            label,
            size,
            contrast_on(fill),
            t * Affine::translate((x0 + pad_x, y0 + (h - f64::from(size)) / 2.0 + 0.5)),
            None,
            TextWeight::Bold,
        );
        x0
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_grid(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &GridMetrics,
    ) {
        let tokens = &theme.tokens;
        let bpr = self.model.bytes_per_row();
        let size = m.cell_font;

        // Column header row: offset label + per-column byte index.
        let head_y = m.col_header_y;
        tr.draw_text_styled(
            scene,
            "Offset",
            tokens.typography.scale.xs,
            tokens.colors.text_dim,
            t * Affine::translate((m.gutter_x0, head_y)),
            None,
            TextWeight::Medium,
            false,
            true,
        );
        for c in 0..bpr {
            let label = format!("{c:02X}");
            let x = m.hex_x0 + c as f64 * m.hex_col_w;
            tr.draw_text_styled(
                scene,
                &label,
                tokens.typography.scale.xs,
                tokens.colors.text_dim,
                t * Affine::translate((x, head_y)),
                None,
                TextWeight::Medium,
                false,
                true,
            );
        }
        tr.draw_text_styled(
            scene,
            "ASCII",
            tokens.typography.scale.xs,
            tokens.colors.text_dim,
            t * Affine::translate((m.ascii_x0, head_y)),
            None,
            TextWeight::Medium,
            false,
            true,
        );
        // Hairline under the column header.
        scene.stroke(
            &Stroke::new(1.0),
            t,
            tokens.colors.border_standard,
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
                tokens.colors.border_standard,
                None,
                &Line::new((sep_x, m.grid_top - 2.0), (sep_x, m.grid_bottom)),
            );
        }

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
            let text_y = y + (m.row_h - f64::from(size)) / 2.0;

            // Offset gutter.
            let gutter = format!("{row_off:08X}");
            tr.draw_text_styled(
                scene,
                &gutter,
                size,
                tokens.colors.accent[6],
                t * Affine::translate((m.gutter_x0, text_y)),
                None,
                TextWeight::Regular,
                false,
                true,
            );

            for c in 0..bpr {
                let off = row_off + c;
                let Some(&byte) = self.model.bytes().get(off) else {
                    continue;
                };
                let hex_x = m.hex_x0 + c as f64 * m.hex_col_w;
                let ascii_x = m.ascii_x0 + c as f64 * m.char_w;

                let selected = selection.is_some_and(|(lo, hi)| off >= lo && off <= hi);
                let inspected = inspector_span.is_some_and(|(lo, hi)| off >= lo && off <= hi);
                let is_cursor = off == cursor;
                let is_match = self.matches.iter().any(|&mh| {
                    // Highlight cells inside any match span.
                    if let Some(n) = search::needle(self.search_input.value(), self.search_kind) {
                        off >= mh && off < mh + n.len()
                    } else {
                        false
                    }
                });
                let is_dirty = self.model.is_dirty(off);

                // --- cell background layers (lowest to highest) ---
                let cell_hex = Rect::new(hex_x - 3.0, y, hex_x + m.byte_w + 3.0, y + m.row_h);
                let cell_ascii = Rect::new(ascii_x - 1.0, y, ascii_x + m.char_w + 1.0, y + m.row_h);

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

                // --- cursor cell outline on the active column ---
                if is_cursor {
                    let active_hex = self.model.column() == Column::Hex;
                    let (cell, other) = if active_hex {
                        (cell_hex, cell_ascii)
                    } else {
                        (cell_ascii, cell_hex)
                    };
                    scene.fill(
                        Fill::NonZero,
                        t,
                        tokens.colors.accent[ui_accent_idx()],
                        None,
                        &cell,
                    );
                    // The mirror column gets a faint outline so the synced
                    // position is visible in both panes.
                    scene.stroke(
                        &Stroke::new(1.0),
                        t,
                        tokens.colors.accent[ui_accent_idx()],
                        None,
                        &RoundedRect::from_rect(other, 2.0),
                    );
                }

                // --- glyphs ---
                let hex_color = if is_cursor {
                    contrast_on(tokens.colors.accent[ui_accent_idx()])
                } else if is_dirty {
                    tokens.colors.semantic.warning
                } else if selected {
                    tokens.colors.text_primary
                } else if byte == 0 {
                    // Zero bytes read as background noise — dim them.
                    tokens.colors.neutral[5]
                } else {
                    tokens.colors.text_primary
                };
                tr.draw_text_styled(
                    scene,
                    &format!("{byte:02X}"),
                    size,
                    hex_color,
                    t * Affine::translate((hex_x, text_y)),
                    None,
                    if is_dirty {
                        TextWeight::Bold
                    } else {
                        TextWeight::Regular
                    },
                    false,
                    true,
                );

                // ASCII glyph.
                let ch = ascii_glyph(byte);
                let ascii_color = if is_cursor && self.model.column() == Column::Ascii {
                    contrast_on(tokens.colors.accent[ui_accent_idx()])
                } else if is_dirty {
                    tokens.colors.semantic.warning
                } else if (0x20..=0x7E).contains(&byte) {
                    tokens.colors.text_primary
                } else {
                    tokens.colors.neutral[5]
                };
                tr.draw_text_styled(
                    scene,
                    &ch,
                    size,
                    ascii_color,
                    t * Affine::translate((ascii_x, text_y)),
                    None,
                    TextWeight::Regular,
                    false,
                    true,
                );
            }
        }
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
        let width = remaining.min(8).max(1);
        Some((cur, cur + width - 1))
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_inspector(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &GridMetrics,
    ) {
        let tokens = &theme.tokens;
        let panel = Rect::new(
            m.inspector_x0,
            m.grid_top - 2.0,
            self.rect.x1,
            m.grid_bottom,
        );
        scene.fill(Fill::NonZero, t, tokens.colors.surfaces.base, None, &panel);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            tokens.colors.border_standard,
            None,
            &Line::new((panel.x0, panel.y0), (panel.x0, panel.y1)),
        );

        let pad = f64::from(tokens.spacing.s4);
        let x = panel.x0 + pad;
        let mut y = panel.y0 + pad;

        tr.draw_text_weighted(
            scene,
            "DATA INSPECTOR",
            tokens.typography.scale.xs,
            tokens.colors.text_dim,
            t * Affine::translate((x, y)),
            None,
            TextWeight::Bold,
        );
        y += 22.0;

        // Cursor + endianness sub-header.
        let sub = format!(
            "@ 0x{:X}  ·  {}",
            self.model.cursor(),
            self.model.endian().label()
        );
        tr.draw_text_styled(
            scene,
            &sub,
            tokens.typography.scale.sm,
            tokens.colors.accent[6],
            t * Affine::translate((x, y)),
            None,
            TextWeight::Regular,
            false,
            true,
        );
        y += 24.0;

        let rows: Vec<Interpretation> =
            inspector::inspect(self.model.bytes(), self.model.cursor(), self.model.endian());
        let label_w = 72.0;
        let row_h = 22.0;
        let value_size = tokens.typography.scale.sm;

        for r in &rows {
            if y + row_h > panel.y1 {
                break;
            }
            // Zebra striping for legibility.
            let band = Rect::new(panel.x0, y - 3.0, panel.x1, y + row_h - 5.0);
            scene.fill(
                Fill::NonZero,
                t,
                with_alpha(tokens.colors.surfaces.raised, 60),
                None,
                &band,
            );
            tr.draw_text_styled(
                scene,
                r.label,
                value_size,
                tokens.colors.text_dim,
                t * Affine::translate((x, y)),
                None,
                TextWeight::Medium,
                false,
                true,
            );
            let val_color = if r.value == "—" {
                tokens.colors.neutral[4]
            } else {
                tokens.colors.text_primary
            };
            tr.draw_text_styled(
                scene,
                &r.value,
                value_size,
                val_color,
                t * Affine::translate((x + label_w, y)),
                Some((panel.width() - pad * 2.0 - label_w) as f32),
                TextWeight::Regular,
                false,
                true,
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
            s.push_str(&format!("   sel {sel} B"));
        }
        if self.model.dirty_count() > 0 {
            s.push_str(&format!("   ●{} edited", self.model.dirty_count()));
        }
        if let Some((msg, _error, _)) = &self.flash {
            s.push_str(&format!("   {msg}"));
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
        let now = Instant::now();
        let label = self.status_bubble_label();
        // Poke only on a genuine change — `draw` runs every frame, so poking
        // unconditionally would pin the bubble open forever.
        if self.last_status_label.as_deref() != Some(label.as_str()) {
            self.status_bubble.poke(now);
            self.last_status_label = Some(label.clone());
        }
        let text_size = theme.tokens.typography.scale.sm;
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

        // Title.
        let title = "No file open";
        let tsz = tokens.typography.scale.x2l;
        let (tw, _) = tr.measure_weighted(title, tsz, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            title,
            tsz,
            c.text_primary,
            t * Affine::translate((cx - f64::from(tw) / 2.0, cy - 78.0)),
            None,
            TextWeight::Semibold,
        );

        // Subtitle.
        let sub = "Open a file to inspect and edit its bytes.";
        let ssz = tokens.typography.scale.sm;
        let (sw, _) = tr.measure(sub, ssz, None);
        tr.draw_text(
            scene,
            sub,
            ssz,
            c.text_dim,
            t * Affine::translate((cx - f64::from(sw) / 2.0, cy - 40.0)),
            None,
        );

        // Primary action: Open file…
        let label = "Open file…";
        let bsz = tokens.typography.scale.base;
        let (lw, _) = tr.measure_weighted(label, bsz, None, TextWeight::Semibold);
        let bw = f64::from(lw) + 44.0;
        let bh = 40.0;
        let bx0 = cx - bw / 2.0;
        let by0 = cy - 8.0;
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
            contrast_on(accent),
            t * Affine::translate((
                bx0 + (bw - f64::from(lw)) / 2.0,
                by0 + (bh - f64::from(bsz)) / 2.0,
            )),
            None,
            TextWeight::Semibold,
        );
        self.open_button = btn;

        // Keyboard hint under the button.
        let hint = "Ctrl+O";
        let hsz = tokens.typography.scale.xs;
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

    fn draw_search_overlay(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let w = 460.0;
        let h = 56.0;
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
        // Kind toggle chip.
        let kind = self.search_kind.label();
        let chip_w = 48.0;
        let chip = RoundedRect::new(
            x0 + pad,
            y0 + 12.0,
            x0 + pad + chip_w,
            y0 + 12.0 + 22.0,
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
            tokens.typography.scale.xs,
            contrast_on(tokens.colors.accent[ui_accent_idx()]),
            t * Affine::translate((x0 + pad + 8.0, y0 + 12.0 + 5.0)),
            None,
            TextWeight::Bold,
        );

        // Match counter. It stays app-owned because it describes byte-search
        // policy, while the shared field owns all text-editing behaviour.
        let query_empty = self.search_input.value().is_empty();
        let count = if self.matches.is_empty() {
            if query_empty {
                String::new()
            } else {
                "no matches".to_string()
            }
        } else {
            let pos = self.match_idx.map_or(0, |i| i + 1);
            format!("{}/{}", pos, self.matches.len())
        };
        let count_w = if count.is_empty() {
            0.0
        } else {
            f64::from(tr.measure(&count, tokens.typography.scale.sm, None).0)
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
            tr.draw_text(
                scene,
                &count,
                tokens.typography.scale.sm,
                ccolor,
                t * Affine::translate((
                    x0 + w - pad - count_w,
                    y0 + (h - f64::from(tokens.typography.scale.sm)) / 2.0,
                )),
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
        let h = 56.0;
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
            tokens.typography.scale.xs,
            tokens.colors.text_dim,
            t * Affine::translate((x0 + pad, y0 + 8.0)),
            None,
            TextWeight::Bold,
        );
        self.goto_input.apply_layout_external(Rect::new(
            x0 + pad,
            y0 + 19.0,
            x0 + w - pad,
            y0 + h - 3.0,
        ));
        let presence = PresenceCtx::empty();
        self.goto_input.draw(scene, t, tr, theme, &presence);
    }

    // --- keyboard ------------------------------------------------------

    /// Route an event into the active shared field, keeping only byte-search
    /// policy (kind cycling / match navigation / offset commit) in this app.
    fn handle_overlay_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
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
                    self.persist(doc);
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
                    self.persist(doc);
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
                    scale_factor,
                    modifiers,
                    doc,
                    path,
                    &mut field_handled,
                    focus_target,
                    node_id,
                    taffy,
                );
                self.search_input.take_dirty().is_some()
            }
            Overlay::Goto => {
                self.goto_input.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    doc,
                    path,
                    &mut field_handled,
                    focus_target,
                    node_id,
                    taffy,
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
    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        // Drain any completed file open/save picks first, so a freshly opened
        // buffer is in place before this frame lays out.
        self.drain_portal(doc);

        if !self.seeded {
            if HexDoc::is_seeded(doc) {
                // Adopt an existing peer's document — but only when we opened
                // no file of our own (an explicit `hex FILE` wins over an empty
                // shared doc, and seeds the peers instead).
                if self.has_file {
                    HexDoc::write_model(doc, &self.model);
                } else if let Some(m) = HexDoc::read_model(doc) {
                    self.model = m;
                    self.has_file = true;
                    self.file_name = "shared buffer".to_string();
                }
            } else if self.has_file {
                // Seed ours.
                HexDoc::write_model(doc, &self.model);
            }
            self.seeded = true;
            self.dirty = true;
        }

        // Adopt buffer changes that arrived out-of-band: a peer's edit, a Ctrl+K
        // palette action, or an undo restoring the document. The shared bytes are
        // authoritative, so the render cache re-derives from them whenever they
        // diverge from ours. A local keystroke keeps model and document in step
        // (the edit is applied to both), so this is a no-op on the common path
        // and never clobbers an in-progress edit.
        if self.seeded
            && self.has_file
            && let Some(bytes) = HexDoc::read_bytes(doc)
            && bytes != self.model.bytes()
        {
            self.model.adopt_bytes(bytes);
            if self.overlay == Overlay::Search {
                self.recompute_matches();
            }
            self.dirty = true;
        }

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
        self.is_busy() || self.status_bubble.needs_frame(Instant::now())
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
        let l = taffy.layout(node).unwrap();
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

        // Reflow the byte grid to fill the current width before laying out.
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        let ctrl = modifiers.state().control_key();
        let shift = modifiers.state().shift_key();

        if self.overlay != Overlay::None {
            if self.handle_overlay_event(
                event,
                scale_factor,
                modifiers,
                state,
                _path,
                _focus_target,
                _node_id,
                _taffy,
            ) {
                *handled = true;
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
            } => {
                // Empty state: the only affordance is the centred Open button.
                if !self.has_file {
                    if self.open_button.contains(self.last_mouse) {
                        self.begin_open();
                        *handled = true;
                    }
                    return;
                }
                // Click position is tracked via CursorMoved; recompute on
                // press from the last known position.
                if let Some((off, col)) = self.hit_test(self.last_mouse) {
                    self.model.set_column(col);
                    self.model.move_to(off, shift);
                    self.persist(state);
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y / 20.0,
                };
                self.model.scroll_by(-(lines.round() as isize) * 3);
                self.dirty = true;
                *handled = true;
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                if ke.state != ElementState::Pressed {
                    return;
                }
                let key = &ke.logical_key;

                // Empty state: the only meaningful key action is opening a file.
                if !self.has_file {
                    if ctrl
                        && let Key::Character(c) = key
                        && c.eq_ignore_ascii_case("o")
                    {
                        self.begin_open();
                        *handled = true;
                    }
                    return;
                }

                // Global chords.
                if ctrl && let Key::Character(c) = key {
                    match c.as_str() {
                        "o" | "O" => {
                            self.begin_open();
                            *handled = true;
                            return;
                        }
                        "s" | "S" => {
                            self.begin_save();
                            *handled = true;
                            return;
                        }
                        "f" | "F" => {
                            self.overlay = Overlay::Search;
                            self.search_input.set_focus(true);
                            self.search_input
                                .set_cursor_pos(self.search_input.value().len());
                            self.recompute_matches();
                            self.dirty = true;
                            *handled = true;
                            return;
                        }
                        "g" | "G" => {
                            self.overlay = Overlay::Goto;
                            self.goto_input.set_focus(true);
                            self.goto_input
                                .set_cursor_pos(self.goto_input.value().len());
                            self.dirty = true;
                            *handled = true;
                            return;
                        }
                        "a" | "A" => {
                            self.model.select_all();
                            self.persist(state);
                            *handled = true;
                            return;
                        }
                        _ => {}
                    }
                }

                let mut moved = true;
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
                        moved = self.handle_char(c.as_str());
                    }
                    _ => moved = false,
                }

                if moved {
                    let (row, _) = self.model.cursor_cell();
                    self.model.scroll_into_view(row, self.visible_rows);
                    self.persist(state);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    /// Publish the open buffer + its byte-edit actions so the shell's Ctrl+K
    /// action palette scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before a keystroke dispatched its
    /// buffer-edit action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
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
            Overlay::None => None,
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl HexView {
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

/// Computed layout metrics for one frame. All in logical px.
struct GridMetrics {
    header_bottom: f64,
    col_header_y: f64,
    grid_top: f64,
    grid_bottom: f64,
    row_h: f64,
    cell_font: f32,
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
    const CELL_FONT: f32 = 14.0;
    const ROW_H: f64 = 20.0;
    /// Monospace pitch at [`Self::CELL_FONT`]: roughly 0.6em per glyph.
    const GLYPH: f64 = Self::CELL_FONT as f64 * 0.6;
    /// One ASCII glyph.
    const CHAR_W: f64 = Self::GLYPH;
    /// Two hex digits plus the inter-byte gap — one byte column's pitch.
    const HEX_COL_W: f64 = Self::GLYPH * 2.0 + Self::GLYPH * 0.55;
    /// Eight hex digits of offset.
    const GUTTER_W: f64 = Self::GLYPH * 8.0;
    const LEFT_PAD: f64 = 16.0;
    /// Gap between the offset gutter and the hex columns.
    const GUTTER_GAP: f64 = 22.0;
    /// Gap between the hex columns and the ASCII column.
    const HEX_ASCII_GAP: f64 = 22.0;
    /// Gap between the ASCII column and the inspector panel.
    const INSPECTOR_GAP: f64 = 24.0;
    /// Smallest inspector width the fit calculation keeps room for.
    const INSPECTOR_MIN: f64 = 300.0;

    fn new(rect: Rect, bpr: usize) -> Self {
        let header_bottom = rect.y0 + 44.0;
        let col_header_h = 20.0;
        let col_header_y = header_bottom + 10.0;
        let grid_top = col_header_y + col_header_h;
        // No permanent status strip any more — the cursor read-out floats in a
        // transient bubble — so the byte grid + inspector reclaim that space and
        // span to the window bottom, keeping only a small margin off the edge.
        let grid_bottom = rect.y1 - 8.0;

        let cell_font = Self::CELL_FONT;
        let row_h = Self::ROW_H;
        let byte_w = Self::GLYPH * 2.0;
        let char_w = Self::CHAR_W;
        let hex_col_w = Self::HEX_COL_W;

        let gutter_x0 = rect.x0 + Self::LEFT_PAD;
        let hex_x0 = gutter_x0 + Self::GUTTER_W + Self::GUTTER_GAP;
        let hex_x1 = hex_x0 + bpr as f64 * hex_col_w;
        let ascii_x0 = hex_x1 + Self::HEX_ASCII_GAP;
        let ascii_x1 = ascii_x0 + bpr as f64 * char_w;

        // The inspector sits directly to the right of the ASCII column and
        // fills the remaining width — the byte grid auto-fits so this stays a
        // clean gutter, never a stranded band of empty space.
        let inspector_x0 = (ascii_x1 + Self::INSPECTOR_GAP).min(rect.x1 - Self::INSPECTOR_MIN);

        let visible_rows = ((grid_bottom - grid_top) / row_h).floor().max(1.0) as usize;

        Self {
            header_bottom,
            col_header_y,
            grid_top,
            grid_bottom,
            row_h,
            cell_font,
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

/// A compact human byte count for the save/open flash (`512 B`, `4.2 kB`).
fn human_size(n: usize) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn with_alpha(c: Color, a: u8) -> Color {
    let rgba = c.to_rgba8();
    Color::from_rgba8(rgba.r, rgba.g, rgba.b, a)
}

/// Choose black or white text for legibility on `bg` (relative luminance).
fn contrast_on(bg: Color) -> Color {
    let rgba = bg.to_rgba8();
    let lum = 0.299 * f64::from(rgba.r) + 0.587 * f64::from(rgba.g) + 0.114 * f64::from(rgba.b);
    if lum > 140.0 {
        Color::from_rgb8(10, 12, 20)
    } else {
        Color::WHITE
    }
}

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

    #[test]
    fn shared_search_field_still_drives_byte_match_policy() {
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
    }

    #[test]
    fn shared_goto_field_keeps_hex_and_decimal_offset_parsing() {
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
    }
}
