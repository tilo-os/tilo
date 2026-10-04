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
//! The framework owns the shared byte resource and its edit history. File
//! paths and chooser state remain local to this view.

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

use crate::actions::{HexEditorAction, HexEffect, HexPanel};
use crate::model::{Column, EditMode, Endian, HexModel};
use crate::resource::{HexEditor, HexState};
use goto::GotoForm;
use layout::{CellMetrics, ENDIAN_LABELS, Geo, MODE_LABELS};
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectReply, EffectStatus, Effected, Live, ResourceSurface,
    SurfaceCx,
};
use ui::tilo_resource::ActionDecl;
use ui::widget::toggle_group::Segment;

/// How long a failure toast stays up.
const FLASH_MS: u128 = 3500;
const CANCELLED: &str = "cancelled";

enum FileIo {
    Open(PortalChannel),
    /// A chooser delivering contents, where files have no path (the web).
    OpenContents(ContentsChannel),
    Save(PortalChannel),
    SaveContents(SaveChannel),
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
    search_needle: Option<Vec<u8>>,
    search_rules: crate::search::MatchRules,
    matches_truncated: bool,
    match_count: usize,
    current_match_start: Option<usize>,
    /// The match the bar is on.
    match_idx: Option<usize>,

    // --- go to ---
    /// The go-to-offset form, in a popover at the status bar.
    goto: Popover<GotoForm>,

    // --- chrome ---
    /// The inspector: docked when wide, a floating drawer when narrow.
    drawer: SidebarDrawer,
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
    saved: Vec<u8>,
    opened: u64,
    expect_open: bool,
    io: Option<(FileIo, EffectReply)>,
    /// A failed open or save: its title, the reason and its birth time. Shown
    /// as a toast until it expires.
    problem: Option<(String, String, Instant)>,

    /// "Saved" and "Opened": a change worth noticing once, floating at the
    /// content's bottom-trailing corner and fading by itself.
    status_bubble: ui::StatusBubble,
    /// The canvas is keyboard-focusable even though it is custom drawn.
    focused: bool,
    dispatch: Option<Dispatcher<HexEditor>>,
    live: Option<Live<HexEditor>>,
}

impl HexView {
    pub fn new(bytes: Vec<u8>, file_name: impl Into<String>) -> Self {
        let mode_pick = Rc::new(Cell::new(None));
        let endian_pick = Rc::new(Cell::new(None));
        Self {
            saved: bytes.clone(),
            model: HexModel::new(bytes),
            file_name: file_name.into(),
            rect: Rect::ZERO,
            dirty: true,
            find_bar: find::new_find_bar(),
            find_open: false,
            matches: Vec::new(),
            search_needle: None,
            search_rules: crate::search::MatchRules::EXACT,
            matches_truncated: false,
            match_count: 0,
            current_match_start: None,
            match_idx: None,
            goto: Popover::with_content(GotoForm::new())
                .side(PopoverSide::Above)
                .with_label("Go to offset"),
            drawer: SidebarDrawer::new()
                .right()
                .docked_width(f64::from(ui::tokens::Metrics::BASE.inspector_width)),
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
            opened: 0,
            expect_open: false,
            io: None,
            problem: None,
            status_bubble: ui::StatusBubble::new(),
            focused: false,
            dispatch: None,
            live: None,
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
    #[must_use]
    pub fn with_path(mut self, path: Option<PathBuf>) -> Self {
        self.path = path;
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

    /// Record bounded inverse edits for a completed local input. Neither bytes
    /// nor paths enter the collaboration document.
    fn commit_edits(&mut self, doc: &mut AutoCommit) {
        let edits = self.model.take_edits();
        self.dirty = true;
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        if edits.is_empty() {
            dispatch.settle();
            return;
        }
        for edit in edits {
            if dispatch
                .apply_now(doc, &HexEditorAction::from_edit(edit))
                .is_err()
            {
                if let Some(live) = &self.live {
                    let state = live.get();
                    self.model.show_bytes(state.bytes.clone(), &self.saved);
                }
                return;
            }
        }
    }

    fn moved(&mut self) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.settle();
        }
        self.dirty = true;
    }

    fn invoke(&self, effect: &HexEffect) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.effect(effect);
        }
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

    /// Start the host file chooser to open a file, answering `reply` when
    /// it closes.
    fn begin_open(&mut self, reply: EffectReply) {
        let io = if crate::file::native_paths() {
            FileIo::Open(file_open_request().send())
        } else {
            FileIo::OpenContents(file_open_request().send_contents())
        };
        self.io = Some((io, reply));
    }

    /// Start the host Save chooser for a buffer with no file on this
    /// device, answering `reply` when it closes.
    fn begin_save_as(&mut self, reply: EffectReply) {
        let name = self.suggested_name();
        let io = if crate::file::native_paths() {
            FileIo::Save(file_save_request(&name).send())
        } else {
            FileIo::SaveContents(
                file_save_request(&name).send_save_contents(self.model.bytes().to_vec()),
            )
        };
        self.io = Some((io, reply));
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

    /// Write the current bytes to `path`, clearing the changed-byte marks on
    /// success and flashing the outcome either way.
    fn write_to(&mut self, path: &std::path::Path) -> Result<(), String> {
        match crate::file::write(path, self.model.bytes()) {
            Ok(()) => {
                self.saved_now();
                self.path = Some(path.to_path_buf());
                self.flash(
                    format!(
                        "Saved {}",
                        format_bytes(u64::try_from(self.model.len()).unwrap_or(u64::MAX))
                    ),
                    false,
                );
                Ok(())
            }
            Err(e) => {
                self.flash(format!("Save failed: {e}"), true);
                Err(e)
            }
        }
    }

    /// The buffer now matches the file it was saved to.
    fn saved_now(&mut self) {
        self.saved = self.model.bytes().to_vec();
        self.model.clear_dirty();
        self.dirty = true;
    }

    /// Open the file at `path` into the resource. The path stays here.
    fn open_path(&mut self, path: PathBuf) -> Result<(), String> {
        match crate::file::read(&path) {
            Ok(bytes) => {
                let name = path.file_name().map_or_else(
                    || "untitled".to_string(),
                    |s| s.to_string_lossy().into_owned(),
                );
                self.open_bytes(name, bytes);
                self.path = Some(path);
                Ok(())
            }
            Err(e) => {
                self.flash(format!("Open failed: {e}"), true);
                Err(e)
            }
        }
    }

    /// Submit `bytes`, read from the file `name`, as the resource's buffer.
    fn open_bytes(&mut self, name: String, bytes: Vec<u8>) {
        let n = bytes.len();
        if let Some(dispatch) = &self.dispatch {
            dispatch.apply(&HexEditorAction::Open { name, bytes });
            self.expect_open = true;
        }
        self.path = None;
        self.flash(
            format!(
                "Opened {}",
                format_bytes(u64::try_from(n).unwrap_or(u64::MAX))
            ),
            false,
        );
    }

    /// A file chosen by contents.
    fn open_contents(&mut self, outcome: ContentsOutcome) -> Result<(), String> {
        match outcome {
            ContentsOutcome::Picked(files) => {
                let Some(file) = files.into_iter().next() else {
                    return Err("no file was chosen".into());
                };
                self.open_bytes(file.name, file.bytes);
                Ok(())
            }
            ContentsOutcome::Cancelled => Err(CANCELLED.into()),
            ContentsOutcome::Failed(reason) => {
                self.flash(&reason, true);
                Err(reason)
            }
        }
    }

    /// What a closed chooser means for the buffer; `None` while it is open.
    fn take_chooser(&mut self, io: &FileIo) -> Option<Result<(), String>> {
        let disconnected = || Some(Err("file picker disconnected".to_owned()));
        match io {
            FileIo::OpenContents(rx) => match rx.try_recv() {
                Ok(outcome) => Some(self.open_contents(outcome)),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => disconnected(),
            },
            FileIo::SaveContents(rx) => match rx.try_recv() {
                Ok(SaveOutcome::Saved) => {
                    self.saved_now();
                    self.flash("Saved", false);
                    Some(Ok(()))
                }
                Ok(SaveOutcome::Cancelled) => Some(Err(CANCELLED.into())),
                Ok(SaveOutcome::Failed(reason)) => {
                    self.flash(&reason, true);
                    Some(Err(reason))
                }
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => disconnected(),
            },
            FileIo::Open(rx) | FileIo::Save(rx) => match rx.try_recv() {
                Ok(PortalOutcome::Opened(path)) => Some(match io {
                    FileIo::Open(_) => self.open_path(path),
                    _ => self.write_to(&path),
                }),
                Ok(PortalOutcome::Cancelled) => Some(Err(CANCELLED.into())),
                Ok(PortalOutcome::Failed(reason)) => {
                    self.flash(&reason, true);
                    Some(Err(reason))
                }
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => disconnected(),
            },
        }
    }

    /// Answer the open or save effect whose chooser closed.
    fn drain_chooser(&mut self) {
        let Some((io, reply)) = self.io.take() else {
            return;
        };
        if reply.is_cancelled() {
            return;
        }
        match self.take_chooser(&io) {
            None => self.io = Some((io, reply)),
            Some(result) => {
                reply.finish(result.map(|()| ui::tilo_actions::ActionOutput::default()));
            }
        }
    }

    /// Post a transient status message.
    fn flash(&mut self, msg: impl Into<String>, error: bool) {
        let message = msg.into();
        if error {
            self.problem("File operation failed", message);
        } else {
            self.notice(&message);
        }
    }

    /// True while a chooser is open or a status flash is live — the app
    /// keeps pumping frames so the pick drains and the flash expires.
    fn is_busy(&self) -> bool {
        self.io.is_some() || self.problem.is_some()
    }

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
            self.moved();
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
        self.mode_group.handle_event(event, ctx);
        if self.geo.inspector_visible {
            self.endian_group.handle_event(event, ctx);
        }
        let took = *ctx.handled;
        self.apply_strip_picks();
        // The strips repaint themselves under the pointer; the view paints
        // them, so it has to know when they changed.
        if Widget::needs_redraw(&self.mode_group) || Widget::needs_redraw(&self.endian_group) {
            self.dirty = true;
        }
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
        self.dirty = true;
        if self.empty.take_action() {
            self.invoke(&HexEffect::Open { path: None });
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
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::HEX_EDITOR_VIEW_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        _doc: &mut AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        let Some(panel) = crate::actions::decode_view(request.type_id, request.payload)? else {
            return Ok(None);
        };
        if !self.has_file {
            return Err("open a file first".into());
        }
        self.open_panel(panel);
        Ok(Some(ui::widget::ResourceOperationStart::Completed(
            ui::tilo_actions::ActionOutput::default(),
        )))
    }

    fn sync(&mut self, _doc: &mut AppModel, _path: &ModelPath) {
        self.drain_chooser();
        self.apply_strip_picks();
        if self.find_open {
            self.drive_find();
        }
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
                let (doc, _) = ui::doc::document(ctx.state, ctx.path);
                self.handle_key_event(ke, ctrl, shift, doc, ctx.handled);
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
        // The strips and FindBar are hand-positioned parts of this mapped
        // leaf's synthetic tree. Exposing them again as unmapped widgets
        // creates duplicate, unaddressable accessibility controls.
        if self.goto.is_open() {
            vec![&self.goto]
        } else {
            Vec::new()
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in self.children() {
            visitor(child);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.goto.is_open() {
            vec![&mut self.goto]
        } else {
            Vec::new()
        }
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
                    "Keyboard: Ctrl+O open, Ctrl+S save, Ctrl+F find, Ctrl+G go to offset, Ctrl+I show or hide the inspector, Ctrl+Z undo, Ctrl+Y redo, Tab switches hex and ASCII, Insert switches overwrite and insert.",
                ).with_hashed_key("hex.keyboard-help"),
            );
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(if self.drawer.is_open() {
                        "Hide inspector"
                    } else {
                        "Show inspector"
                    })
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(self.geo.toggle)
                    .with_action_target("hex.inspector"),
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
                .with_description(
                    "File bytes are shared through the document; paths stay on this device.",
                )
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
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        // These hand-positioned controls are emitted in this leaf's synthetic
        // tree, so this mapped owner must route their semantic targets too.
        if self.has_file && !self.goto.is_open() {
            let handled = self
                .mode_group
                .perform_semantic_action(target, action, state, path)
                || (self.geo.inspector_visible
                    && self
                        .endian_group
                        .perform_semantic_action(target, action, state, path))
                || (self.find_open
                    && self
                        .find_bar
                        .perform_semantic_action(target, action, state, path));
            if handled {
                self.apply_strip_picks();
                if self.find_open {
                    self.drive_find();
                }
                self.dirty = true;
                return true;
            }
        }
        if matches!(action, ui::a11y::Action::Activate) {
            match target {
                "hex.open" => self.invoke(&HexEffect::Open { path: None }),
                "hex.inspector" if self.has_file => self.toggle_inspector(),
                _ => return false,
            }
            self.dirty = true;
            return true;
        }
        false
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || (self.has_file && self.drawer.is_animating())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        Widget::mark_drawn(&mut self.mode_group);
        Widget::mark_drawn(&mut self.endian_group);
    }
}

impl ResourceSurface<HexEditor, HexEffect> for HexView {
    /// Show the buffer the resource holds now. A newly opened file loads
    /// fresh; any other change (an undo, a collaborator, the palette) is
    /// shown in place, keeping the cursor and the view settings.
    fn present(&mut self, state: &Rc<HexState>) {
        let fresh = state.opened != self.opened || (!self.has_file && state.has_file());
        let name = state.name.clone().unwrap_or_else(|| "No file".to_owned());
        if fresh {
            // A file a collaborator opened is not the one at our path.
            if self.has_file && name != self.file_name && !self.expect_open {
                self.path = None;
            }
            self.expect_open = false;
            self.opened = state.opened;
            self.saved.clone_from(&state.bytes);
            self.model.load(state.bytes.clone());
            self.close_find();
            self.goto.close();
            self.matches.clear();
            self.match_idx = None;
        } else {
            self.model.show_bytes(state.bytes.clone(), &self.saved);
            if self.find_open {
                self.refind();
            }
        }
        self.has_file = state.has_file();
        self.file_name = name;
        self.dirty = true;
    }

    fn attach(&mut self, cx: SurfaceCx<HexEditor>) {
        self.dispatch = Some(cx.dispatcher());
        self.live = Some(cx.live());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: HexEffect, cx: &mut EffectCx<HexEditor, Self>) -> EffectAnswer {
        if self.io.is_some() {
            return Err("another file operation is already running".into());
        }
        match effect {
            HexEffect::Open { path: Some(path) } => {
                self.open_path(PathBuf::from(path))?;
                Ok(Effected::done())
            }
            HexEffect::Open { path: None } => {
                self.begin_open(cx.reply());
                Ok(Effected::Pending)
            }
            HexEffect::Save | HexEffect::SaveAs { .. } if !self.has_file => {
                Err("open a file first".into())
            }
            HexEffect::SaveAs { path } => {
                self.write_to(std::path::Path::new(&path))?;
                Ok(Effected::done())
            }
            HexEffect::Save => {
                if let Some(path) = self.path.clone() {
                    self.write_to(&path)?;
                    Ok(Effected::done())
                } else {
                    self.begin_save_as(cx.reply());
                    Ok(Effected::Pending)
                }
            }
        }
    }

    fn effects(&self) -> Vec<EffectStatus> {
        if self.has_file {
            Vec::new()
        } else {
            vec![
                EffectStatus::unavailable(HexEffect::SAVE, "no_file", "Open a file first"),
                EffectStatus::unavailable(HexEffect::SAVE_AS, "no_file", "Open a file first"),
            ]
        }
    }

    fn bound_declarations(&self) -> Vec<ActionDecl<HexEditor>> {
        if !self.has_file {
            return Vec::new();
        }
        crate::actions::cursor_declarations(
            self.model.cursor(),
            self.model.len(),
            self.model.selection(),
        )
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
            self.moved();
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
        doc: &mut AutoCommit,
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
            self.commit_edits(doc);
            self.refind();
            *handled = true;
        }
    }

    /// The only meaningful key action while no file is open. Part of
    /// [`Self::handle_key_event`].
    fn handle_empty_state_key(&self, key: &Key, ctrl: bool, handled: &mut bool) {
        if ctrl
            && let Key::Character(c) = key
            && c.eq_ignore_ascii_case("o")
        {
            self.invoke(&HexEffect::Open { path: None });
            *handled = true;
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
            "o" | "O" => self.invoke(&HexEffect::Open { path: None }),
            "s" | "S" => self.invoke(&HexEffect::Save),
            "f" | "F" => self.open_panel(HexPanel::Search),
            "g" | "G" => self.open_panel(HexPanel::GotoOffset),
            "i" | "I" => self.toggle_inspector(),
            "a" | "A" => {
                self.model.select_all();
                self.moved();
            }
            _ => return false,
        }
        *handled = true;
        true
    }

    fn open_panel(&mut self, panel: HexPanel) {
        match panel {
            HexPanel::Search => self.open_find(),
            HexPanel::GotoOffset => self.open_goto(),
        }
        self.moved();
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
        self.model.move_to(0x10, false);
        if self.model.endian() != Endian::Big {
            self.model.toggle_endian();
        }
        match scenario {
            Some("search") => {
                self.find_bar.set_query("IHDR");
                self.open_find();
            }
            Some("find") => {
                self.find_bar.set_query("e");
                self.open_find();
            }
            Some("goto") => self.open_goto(),
            Some("edited") => {
                self.saved = crate::sample::png_image();
                self.model
                    .show_bytes(self.model.bytes().to_vec(), &self.saved);
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

    /// Shared find controls retain the byte policies and the bounded index.
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
        assert!(view.matches_truncated);
        assert!(view.is_match_offset(0));
        assert!(view.is_match_offset(search::MAX_RETAINED_MATCHES - 1));
        assert!(view.is_match_offset(search::MAX_RETAINED_MATCHES));

        Findable::clear_find(&mut view);
        assert!(view.matches.is_empty() && view.match_idx.is_none());
    }

    fn hosted_bytes(bytes: Vec<u8>) -> ui::prelude::TestApp<crate::resource::HexEditorShell> {
        let mut app = ui::prelude::TestApp::<crate::resource::HexEditorShell>::new().unwrap();
        app.invoke(
            ui::tilo_resource::Invoke::apply(&HexEditorAction::Open {
                name: "matches.bin".into(),
                bytes,
            })
            .unwrap(),
        )
        .unwrap();
        app
    }

    fn activate_named(
        app: &mut ui::prelude::TestApp<crate::resource::HexEditorShell>,
        label: &str,
    ) {
        let node = app
            .harness()
            .a11y_node_named(label)
            .unwrap_or_else(|| panic!("missing {label}"));
        app.harness()
            .perform_a11y_action(node, &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
    }

    #[test]
    fn hosted_strip_and_find_targets_are_owned_and_dispatched_by_the_surface() {
        let mut app = hosted_bytes(b"aa-aa".to_vec());
        app.harness()
            .with_widget_mut::<HexView, _>(|view| {
                view.find_bar.set_query("aa");
                view.open_find();
            })
            .unwrap();
        app.settle();
        for label in ["Insert", "Big endian", "Next match", "Close find"] {
            assert_eq!(
                app.harness()
                    .a11y_nodes()
                    .iter()
                    .filter(|(_, node)| node.label() == Some(label))
                    .count(),
                1,
                "each manual control has exactly one semantic owner: {label}"
            );
        }
        for target in ["hex.mode:segment:1", "hex.endian:segment:1", "find-Next"] {
            app.harness()
                .perform_semantic_named("hex_view", Some(target), &ui::a11y::Action::Activate)
                .unwrap();
            app.settle();
        }
        app.harness()
            .with_widget_mut::<HexView, _>(|view| {
                assert_eq!(view.model.mode(), EditMode::Insert);
                assert_eq!(view.model.endian(), Endian::Big);
                assert_eq!(view.model.cursor(), 3);
            })
            .unwrap();
        app.harness()
            .perform_semantic_named("hex_view", Some("find-Close"), &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
        app.harness()
            .with_widget_mut::<HexView, _>(|view| assert!(!view.find_open))
            .unwrap();
    }

    #[test]
    fn hosted_find_refresh_preserves_navigation_edits_and_peer_selection() {
        let mut app = hosted_bytes(b"aa-aa-aa".to_vec());
        app.harness()
            .with_widget_mut::<HexView, _>(|view| {
                view.find_bar.set_query("aa");
                view.open_find();
                Widget::set_focus(&mut view.find_bar, false);
                view.model.move_to(2, false);
            })
            .unwrap();
        app.harness().set_modifiers(&["Shift".into()]);
        app.harness().key("ArrowRight");
        app.harness().set_modifiers(&[]);
        app.harness()
            .with_widget_mut::<HexView, _>(|view| {
                assert_eq!(view.model.cursor(), 3);
                assert_eq!(view.model.selection(), Some((2, 3)));
                assert_eq!(view.find_bar.query(), "aa");
                assert_eq!(view.find_bar.position(), Some((2, 3)));
                view.model.move_to(4, false);
                view.model.set_column(Column::Ascii);
            })
            .unwrap();
        app.harness().type_text("x");
        app.settle();
        assert_eq!(app.state().bytes, b"aa-ax-aa");
        app.harness()
            .with_widget_mut::<HexView, _>(|view| {
                assert_eq!(view.model.cursor(), 5);
                assert_eq!(view.find_bar.position(), Some((2, 2)));
                view.model.move_to(4, true);
            })
            .unwrap();
        app.merge_peer_edit(|doc| {
            ui::tilo_actions::Action::apply(
                &HexEditorAction::OverwriteBytes {
                    offset: 0,
                    bytes: b"bb".to_vec(),
                },
                doc,
            )
            .unwrap();
        });
        app.harness()
            .with_widget_mut::<HexView, _>(|view| {
                assert_eq!(view.model.cursor(), 4);
                assert_eq!(view.model.selection(), Some((4, 5)));
                assert_eq!(view.find_bar.query(), "aa");
                assert_eq!(view.find_bar.position(), Some((1, 1)));
            })
            .unwrap();
        activate_named(&mut app, "Next match");
        app.harness()
            .with_widget_mut::<HexView, _>(|view| assert_eq!(view.model.cursor(), 6))
            .unwrap();
        activate_named(&mut app, "Insert");
        activate_named(&mut app, "Big endian");
        app.harness()
            .with_widget_mut::<HexView, _>(|view| {
                assert_eq!(view.model.mode(), EditMode::Insert);
                assert_eq!(view.model.endian(), Endian::Big);
            })
            .unwrap();
        activate_named(&mut app, "Close find");
        app.harness()
            .with_widget_mut::<HexView, _>(|view| assert!(!view.find_open))
            .unwrap();
    }

    #[test]
    fn hosted_find_steps_beyond_retained_matches_and_reports_full_count() {
        let total = search::MAX_RETAINED_MATCHES + 3;
        let mut app = hosted_bytes(vec![b'a'; total]);
        app.harness()
            .with_widget_mut::<HexView, _>(|view| {
                view.find_bar.set_query("a");
                view.open_find();
                view.model.move_to(search::MAX_RETAINED_MATCHES - 1, false);
                view.refind();
                assert_eq!(view.matches.len(), search::MAX_RETAINED_MATCHES);
                assert_eq!(
                    view.find_bar.position(),
                    Some((search::MAX_RETAINED_MATCHES, total))
                );
            })
            .unwrap();
        for (label, offset) in [
            ("Next match", total - 3),
            ("Next match", total - 2),
            ("Previous match", total - 3),
            ("Next match", total - 2),
            ("Next match", total - 1),
            ("Next match", 0),
            ("Previous match", total - 1),
        ] {
            activate_named(&mut app, label);
            app.harness()
                .with_widget_mut::<HexView, _>(|view| {
                    assert_eq!(view.model.cursor(), offset, "{label}");
                    assert_eq!(view.find_bar.position(), Some((offset + 1, total)));
                    assert_eq!(view.matches.len(), search::MAX_RETAINED_MATCHES);
                })
                .unwrap();
        }
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
            // Wide at every text scale: the inspector docks beside a grid
            // that keeps a full row.
            view.rect = Rect::new(0.0, 0.0, 1600.0 * f64::from(scale), 1000.0);
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
            let lane = f64::from(theme.tokens.metrics.scrollbar_gutter + theme.tokens.spacing.s1);
            assert!(
                (g.content.x1 - g.ascii_x1 - lane).abs() < 1.0,
                "the ASCII column ends at the scroll bar's lane, no empty band: {g:?}"
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

        // The grid fills every width: whatever the window, the gaps absorb
        // what a whole number of bytes leaves over.
        let lane = f64::from(theme.tokens.metrics.scrollbar_gutter + theme.tokens.spacing.s1);
        for width in (640..=1600).step_by(37) {
            let mut view = HexView::new(vec![0; 4096], "fill.bin");
            view.rect = Rect::new(0.0, 0.0, f64::from(width), 700.0);
            paint_frame(&mut view, &mut tr, &theme);
            let g = &view.geo;
            assert!(
                (g.content.x1 - g.ascii_x1 - lane).abs() < 1.0,
                "width {width}: dead space right of the ASCII column: {g:?}"
            );
        }

        // An 800 px window keeps 16 bytes a row by floating the inspector.
        let mut view = HexView::new(vec![0; 4096], "compact.bin");
        view.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        paint_frame(&mut view, &mut tr, &theme);
        assert!(!view.geo.docked, "too narrow for a docked inspector");
        assert!(view.model.bytes_per_row() >= 16);

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
                .is_some_and(|text| text.contains("bytes"))
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
        let mut view = HexView::new(crate::sample::png_image(), "sample.png");
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
                    let mut view = if name == Some("empty") {
                        HexView::empty()
                    } else {
                        HexView::new(crate::sample::png_image(), "sample.png")
                    };
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
        let mut view = HexView::new(crate::sample::png_image(), "sample.png");
        view.rect = Rect::new(0.0, 0.0, 640.0, 720.0);
        view.stage_scenario(Some("narrow-inspector"));
        paint_frame(&mut view, &mut tr, &Theme::dark());
        assert!(
            view.geo.inspector_visible,
            "the narrow scenario opens the drawer"
        );
        let mut bytes = crate::sample::png_image();
        for (offset, byte) in [(0x1D, 0x01), (0x1E, 0x00), (0x25, 0xFF)] {
            bytes[offset] = byte;
        }
        let mut view = HexView::new(bytes, "sample.png");
        view.stage_scenario(Some("edited"));
        assert_eq!(view.model.dirty_count(), 3);
        assert_eq!(view.model.selection_len(), 4);
    }
}
