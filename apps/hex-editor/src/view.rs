//! The hex view widget — the whole editor surface in one `Widget`.
//!
//! A hex editor is fundamentally a dense, fixed-pitch grid with a cursor
//! synced across two parallel columns; that is a canvas, not a composition of
//! stock form widgets, so the view paints the grid itself (`paint`) and
//! reads every colour and metric from the `Theme`. Everything around the
//! grid is the toolkit's:
//!
//!   * window bar — the file name as the window's location, Overwrite and
//!     Insert as its views, the inspector's toggle as its action
//!     (`window_bar`); the window's search finds and replaces in the bytes
//!     (`find`) and its History control opens the record's History
//!   * body      — offset gutter | hex byte columns | ASCII column
//!   * inspector — a [`SidebarDrawer`] at the trailing edge: docked beside
//!     the content when there is room, a floating drawer when there is not,
//!     holding the endianness strip and a property list of interpretations
//!   * status    — the persistent status bar (cursor offset, selection,
//!     edited bytes, size); "Opened" and failures are transient notices
//!   * go to     — a popover with one field, anchored at the status bar
//!
//! The bytes are the [`HexEditor`] resource's: the view shows the state it
//! is presented, edits its own copy as a key is pressed so typing stays
//! immediate, and submits each edit as the resource's action through the
//! framework's dispatcher. Opening and saving a file are the
//! [`HexEffect`]s it answers. The path a save writes back to stays on this
//! device, in the view.

mod clipboard;
mod find;
mod layout;
mod paint;
mod window_bar;

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::TryRecvError;
use ui::time::Instant;
use ui::widget::TaffyTreeExt;
use ui::widget::toast::{ToastKind, Transient};
use ui::widget::{AppModel, ModelPath};
use ui::window_toolbar::ToolbarNotice;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::format::memory as format_bytes;
use ui::icons::Icon;
use ui::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, PickRequest, PortalChannel, PortalOutcome,
    SaveChannel, SaveOutcome,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    ControlSize, EmptyState, FindMatch, Popover, PopoverPrompt, PopoverSide, SegmentedStyle,
    SidebarDrawer, ToggleGroup,
};
use ui::window_toolbar::SearchOpening;

use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectReply, EffectStatus, Effected, Live, ResourceSurface,
    SurfaceCx,
};
use ui::tilo_resource::ActionDecl;

use crate::actions::{HexEffect, HexPanel};
use crate::model::{BufferEdit, Column, Endian, HexModel};
use crate::resource::HexEditor;
use layout::{CellMetrics, ENDIAN_LABELS, Geo};
use ui::widget::toggle_group::Segment;

/// What a cancelled chooser answers.
const CANCELLED: &str = "cancelled";

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

/// A file chooser in flight, and the effect waiting for it.
enum FileIo {
    Open(PortalChannel),
    /// A chooser delivering contents, where files have no path (the web).
    OpenContents(ContentsChannel),
    Save(PortalChannel),
    SaveContents(SaveChannel),
}

/// What a buffer started from nothing is called until it is saved.
const NEW_FILE_NAME: &str = "Untitled.bin";

/// The no-file zero-state: what the editor is for, opening a file, and
/// (beside it, [`HexView::new_empty_file`]) starting an empty one.
fn empty_state() -> EmptyState {
    EmptyState::new("No file open")
        .with_icon(Icon::File)
        .with_message(
            "Inspect and edit any file byte by byte, or start an empty one and type bytes in.",
        )
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

// The bools below are independent view flags (redraw-pending,
// buffer-present, focus), not a combinatorial option set, so folding them
// into an enum would only add indirection.
#[allow(
    clippy::struct_excessive_bools,
    reason = "unrelated view flags (dirty/has_file/focused/expect_open), not a combinatorial option set"
)]
pub struct HexView {
    /// The view's copy of the buffer and its editor state.
    model: HexModel,
    /// The file's name, the window's location.
    file_name: String,
    rect: Rect,
    dirty: bool,

    // --- find (per-peer; not in the shared doc) ---
    /// The query the window's search last ran.
    query: find::FindQuery,
    /// Every match of the search's query, as byte ranges, in order.
    matches: Vec<FindMatch>,
    match_idx: Option<usize>,
    /// Moves whenever the bytes change, so an open search finds again.
    revision: u64,

    // --- go to ---
    goto: Popover<PopoverPrompt>,
    /// The offset readout in the status bar, which opens the prompt.
    offset_readout: Rect,

    // --- chrome ---
    /// The inspector: docked when wide, a floating drawer when narrow.
    drawer: SidebarDrawer,
    /// The endianness strip in the inspector, and the pick.
    endian_group: ToggleGroup,
    endian_pick: Rc<Cell<Option<usize>>>,
    /// The zero-state, shown while no file is open.
    empty: EmptyState,
    /// How far the inspector's properties are scrolled.
    inspector_scroll: f64,
    /// The grid's scroll bar.
    scrollbar: ui::widget::scrollbar::Scrollbar,
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

    // --- file state ---
    /// False until the resource holds a file — drives the composed empty
    /// state (no fabricated buffer in the live app).
    has_file: bool,
    /// Where the empty state's "New empty file" button sits, once painted.
    new_file: Rect,
    /// The bytes as last opened or saved, which the changed-byte marks
    /// compare against.
    saved: Vec<u8>,
    /// How many files the resource had opened when last presented.
    opened: u64,
    /// This view submitted the open the resource will present next.
    expect_open: bool,
    /// The file on this device the buffer was opened from or last saved
    /// to, if any. `Ctrl+S` writes straight back here; absent, it opens a
    /// Save chooser.
    path: Option<PathBuf>,
    /// A chooser in flight, drained once per frame in `sync`, and the open
    /// or save effect it answers.
    io: Option<(FileIo, EffectReply)>,
    /// A failed open or save: its title and the reason, shown as the bar's
    /// notice for a notice's lifetime.
    /// What the window's bar says for a while: a result or a failure.
    said: Transient<(String, ToastKind)>,

    /// "Opened": a change worth noticing once, floating at the
    /// content's bottom-trailing corner and fading by itself.
    /// The canvas is keyboard-focusable even though it is custom drawn.
    focused: bool,
    /// Where edits and effects go, once the framework attached the view.
    dispatch: Option<Dispatcher<HexEditor>>,
    /// The resource's state, to show again when an edit is refused.
    live: Option<Live<HexEditor>>,
}

impl HexView {
    pub fn new(bytes: Vec<u8>, file_name: impl Into<String>) -> Self {
        let endian_pick = Rc::new(Cell::new(None));
        Self {
            model: HexModel::new(bytes),
            file_name: file_name.into(),
            rect: Rect::ZERO,
            dirty: true,
            query: find::FindQuery::default(),
            matches: Vec::new(),
            match_idx: None,
            revision: 0,
            goto: Popover::prompt("Go to offset", "0x1F or 31", "Hex with 0x, or decimal")
                .with_field_chars(16)
                .with_field(|input| {
                    input
                        .with_kind(ui::widget::TextInputKind::Code)
                        .with_id("hex.goto")
                })
                .side(PopoverSide::Above),
            offset_readout: Rect::ZERO,
            drawer: SidebarDrawer::new()
                .right()
                .docked_width(f64::from(ui::tokens::Metrics::BASE.inspector_width)),
            endian_group: strip("hex.endian", &ENDIAN_LABELS, &endian_pick).fill(),
            endian_pick,
            empty: empty_state(),
            inspector_scroll: 0.0,
            scrollbar: ui::widget::scrollbar::Scrollbar::vertical(),
            reveal_inspector: false,
            geo: Geo::default(),
            cell: CellMetrics::FALLBACK,
            layout_theme: Theme::dark(),
            visible_rows: 1,
            last_mouse: Point::ZERO,
            has_file: true,
            new_file: Rect::ZERO,
            saved: Vec::new(),
            opened: 0,
            expect_open: false,
            path: None,
            io: None,
            said: Transient::new(),
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

    /// The file on this device a save writes back to without a chooser.
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
    /// cursor / selection that drive the bound actions.
    #[cfg(test)]
    pub fn model_mut(&mut self) -> &mut HexModel {
        &mut self.model
    }

    /// Whether a real buffer is loaded (false in the empty state).
    pub fn has_file(&self) -> bool {
        self.has_file
    }

    /// The on-disk path currently backing the buffer, if any.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Show the bundled sample as a capture does: the cursor on the IHDR
    /// width field (offset `0x10`) with the inspector in big-endian — PNG's
    /// native byte order — so the data inspector reads the image's
    /// dimensions as clean `u32` values.
    fn stage_sample(&mut self) {
        if self.model.endian() != Endian::Big {
            self.model.toggle_endian();
        }
        self.model.move_to(0x10, false);
        self.dirty = true;
    }

    /// Submit the edits the last input made as the resource's actions, one
    /// undo step per run of typing; an input that edited nothing ends the
    /// run, so the next one is a step of its own. A refused edit shows the
    /// resource's bytes again.
    fn commit_edits(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let edits = self.model.take_edits();
        self.dirty = true;
        if !edits.is_empty() {
            self.content_changed();
        }
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        if edits.is_empty() {
            dispatch.settle();
            return;
        }
        for edit in edits {
            let submitted = byte_edit(&edit)
                .ok_or(())
                .and_then(|edit| dispatch.submit_now(doc, edit).map_err(drop));
            if submitted.is_err() {
                if let Some(live) = &self.live {
                    let state = live.get();
                    self.model.show_bytes(&state.bytes.to_vec(), &self.saved);
                }
                return;
            }
        }
    }

    /// The cursor moved or the view changed without an edit: the next edit
    /// starts an undo step of its own.
    fn moved(&mut self) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.settle();
        }
        self.dirty = true;
    }

    /// Invoke `effect` through the framework's door, as the palette does.
    fn invoke(&self, effect: &HexEffect) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.effect(effect);
        }
    }

    /// A change worth noticing once ("Opened"): the bar's notice.
    fn notice(&mut self, message: &str) {
        self.said.show((message.to_owned(), ToastKind::Success));
        self.dirty = true;
    }

    /// A failed open or save: the bar's notice, with the reason.
    fn problem(&mut self, title: &str, reason: impl Into<String>) {
        self.said
            .show((format!("{title}: {}", reason.into()), ToastKind::Error));
        self.dirty = true;
    }

    /// Start the host file chooser to open a file, answering `reply` when
    /// it closes.
    fn begin_open(&mut self, reply: EffectReply) {
        let io = if tilo_files::available(tilo_files::Feature::NativePaths) {
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
        let io = if tilo_files::available(tilo_files::Feature::NativePaths) {
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
    /// success and noticing the outcome either way.
    fn write_to(&mut self, path: &std::path::Path) -> Result<(), String> {
        match crate::file::write(path, self.model.bytes()) {
            Ok(()) => {
                // The changed-byte marks clear; saving where the file lives
                // says nothing more.
                self.saved_now();
                self.path = Some(path.to_path_buf());
                Ok(())
            }
            Err(e) => {
                self.problem("Couldn't save the file", e.clone());
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
                self.problem("Couldn't open the file", e.clone());
                Err(e)
            }
        }
    }

    /// Start an empty buffer to type bytes into, without any file: a way in
    /// where no file chooser can answer, and for writing bytes from scratch.
    /// Saving it asks where.
    fn new_empty_file(&mut self) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit(HexEditor::loading(NEW_FILE_NAME, Vec::new()));
            self.expect_open = true;
        }
        self.path = None;
    }

    /// Submit `bytes`, read from the file `name`, as the resource's buffer.
    fn open_bytes(&mut self, name: String, bytes: Vec<u8>) {
        let n = bytes.len();
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit(HexEditor::loading(name, bytes));
            self.expect_open = true;
        }
        self.path = None;
        self.notice(&format!(
            "Opened {}",
            format_bytes(u64::try_from(n).unwrap_or(u64::MAX))
        ));
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
                self.problem("Couldn't open the file", reason.clone());
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
                    Some(Ok(()))
                }
                Ok(SaveOutcome::Cancelled) => Some(Err(CANCELLED.into())),
                Ok(SaveOutcome::Failed(reason)) => {
                    self.problem("Couldn't save the file", reason.clone());
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
                    let title = if matches!(io, FileIo::Open(_)) {
                        "Couldn't open the file"
                    } else {
                        "Couldn't save the file"
                    };
                    self.problem(title, reason.clone());
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

    /// True while a chooser is open: the app keeps pumping frames so the
    /// pick drains.
    fn is_busy(&self) -> bool {
        self.io.is_some()
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
            self.moved();
        }
        self.dirty = true;
    }

    /// Open the go-to prompt (Ctrl+G, or a click on the offset readout) at
    /// the status bar's offset readout.
    fn open_goto(&mut self) {
        let anchor = self.goto_anchor();
        // The current offset, selected: typing replaces it, Enter keeps it.
        let cursor = self.model.cursor();
        self.goto.open_prompt(anchor, &format!("0x{cursor:X}"));
        self.dirty = true;
    }

    /// Where the go-to prompt hangs from: the offset readout, or the status
    /// bar's content edge before it has been painted.
    fn goto_anchor(&self) -> Rect {
        if self.offset_readout.area() > 0.0 {
            return self.offset_readout;
        }
        let status = self.geo.status;
        let inset = f64::from(self.layout_theme.tokens.spacing.s4);
        Rect::new(status.x0 + inset, status.y0, status.x0 + inset, status.y1)
    }

    /// Apply what the inspector's strip picked since the last event.
    fn apply_strip_picks(&mut self) {
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

    /// Hand a pointer event to the inspector's strip; true when it took it.
    fn route_to_strips(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        if self.geo.inspector_visible {
            self.endian_group.handle_event(event, ctx);
        }
        let took = *ctx.handled;
        self.apply_strip_picks();
        // The strip repaints itself under the pointer; the view paints it,
        // so it has to know when it changed.
        if Widget::needs_redraw(&self.endian_group) {
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

    /// The pointer moved or a button changed: the inspector's strip, the
    /// scroll bar and the drawer's scrim. True when the event was consumed.
    fn handle_chrome_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = ctx.logical(position);
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
                if self.drawer.press_dismisses(p) {
                    self.relayout_now();
                    self.dirty = true;
                    return true;
                }
                if self.offset_readout.contains(p) {
                    self.open_goto();
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

    /// Events while the go-to popover is open: it captures input.
    fn handle_goto_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        Widget::handle_event(&mut self.goto, event, ctx);
        if let Some(query) = self.goto.take_submitted() {
            self.commit_goto(&query);
        }
        self.dirty = true;
    }

    /// Events while no file is open: the empty state's action and Ctrl+O.
    fn handle_empty_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.empty.handle_event(event, ctx);
        self.dirty = true;
        if self.empty.take_action() {
            self.invoke(&HexEffect::OpenFile { path: None });
            *ctx.handled = true;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = ctx.logical(position);
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && !*ctx.handled
            && self.new_file.contains(self.last_mouse)
        {
            self.new_empty_file();
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
        dyn ui::widget::Findable,
        dyn ui::widget::controls::ActionControls
    );

    fn sync(&mut self, _doc: &AppModel, _path: &ModelPath) {
        // Drain a closed file chooser first, so a freshly opened buffer is
        // submitted before this frame lays out.
        self.drain_chooser();

        // A result or a failure says so in the window's bar while it lasts.
        self.said.expire();
        self.said
            .publish(|(text, kind)| ToolbarNotice::new(text.clone(), *kind));
    }

    fn settle(&mut self, _edits: &mut ui::widget::Edits<'_>, _path: &automerge::ObjId) {
        self.run_window_toolbar();
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.said.deadline()
    }

    fn needs_idle_sync(&self) -> bool {
        self.is_busy()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming while a portal pick is in flight (so it drains
        // once the host dialog closes) and while the inspector slides.
        self.is_busy() || (self.has_file && self.drawer.is_animating())
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        // A key (Insert, Ctrl+I) changes what the bar shows without a sync.
        ui::window_toolbar::declare(Some(self.window_toolbar()));
        self.relayout(text_renderer, theme, true);
        if self.goto.is_open() {
            let anchor = self.goto_anchor();
            self.goto.set_anchor(anchor);
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
                self.handle_key_event(ke, ctrl, shift, &mut ctx.state, ctx.handled);
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
        let mut children: Vec<&dyn Widget> = Vec::new();
        if self.goto.is_open() {
            children.push(&self.goto);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.goto.is_open() {
            visitor(&self.goto);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::new();
        if self.goto.is_open() {
            children.push(&mut self.goto);
        }
        children
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.goto.is_open() {
            return Widget::ime_cursor_rect(self.goto.content());
        }
        None
    }

    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        if self.goto.is_open() {
            return Widget::ime_content(self.goto.content());
        }
        None
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.goto.is_open() {
            return Widget::cursor_shape(self.goto.content(), point);
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
                    "Keyboard: Ctrl+O open, Ctrl+S save, Ctrl+F find, Ctrl+H replace, Ctrl+G go to offset, Ctrl+C copy, Ctrl+X cut, Ctrl+V paste, Ctrl+A select all, Ctrl+I show or hide the inspector, Ctrl+Z undo, Ctrl+Shift+Z redo, Tab switches hex and ASCII, Insert switches overwrite and insert.",
                ).with_hashed_key("hex.keyboard-help"),
            );
            if self.geo.inspector_visible {
                children.extend(self.endian_group.accessibility());
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
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("New empty file")
                    .with_description("Start an empty buffer to type bytes into")
                    .with_state(AccessState::default().focusable(true))
                    .with_bounds(self.new_file)
                    .with_action_target("hex.new"),
            );
        }
        Some(
            AccessNode::new(Role::Document)
                .with_name("Hex editor")
                .with_description("The bytes of one file, edited byte by byte.")
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
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if matches!(action, ui::a11y::Action::Activate) {
            match target {
                "hex.open" => self.invoke(&HexEffect::OpenFile { path: None }),
                "hex.new" if !self.has_file => self.new_empty_file(),
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
        Widget::mark_drawn(&mut self.endian_group);
    }
}

impl ResourceSurface<HexEditor, HexEffect> for HexView {
    /// Show the buffer the resource holds now. A newly opened file loads
    /// fresh; any other change (an undo, a collaborator, the palette) is
    /// shown in place, keeping the cursor and the view settings.
    fn present(&mut self, state: &Rc<HexEditor>) {
        let fresh = state.opened != self.opened || (!self.has_file && state.has_file());
        let name = state.name().unwrap_or("No file").to_owned();
        if fresh {
            // A file a collaborator opened is not the one at our path.
            if self.has_file && name != self.file_name && !self.expect_open {
                self.path = None;
            }
            self.expect_open = false;
            self.opened = state.opened;
            self.saved = state.bytes.to_vec();
            self.model.load(self.saved.clone());
            self.matches.clear();
            self.match_idx = None;
            self.inspector_scroll = 0.0;
        } else {
            self.model.show_bytes(&state.bytes.to_vec(), &self.saved);
        }
        self.content_changed();
        self.has_file = state.has_file();
        self.file_name = name;
        self.dirty = true;
    }

    fn attach(&mut self, cx: SurfaceCx<HexEditor>) {
        self.dispatch = Some(cx.dispatcher());
        self.live = Some(cx.live());
    }

    /// A capture shows the bundled sample the scenario opened; the scenario
    /// then stages its view state.
    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: HexEffect, cx: &mut EffectCx<HexEditor, Self>) -> EffectAnswer {
        let panel = match effect {
            HexEffect::Search => Some(HexPanel::Search),
            HexEffect::GotoOffset => Some(HexPanel::GotoOffset),
            _ => None,
        };
        if let Some(panel) = panel {
            if !self.has_file {
                return Err("open a file first".into());
            }
            self.open_panel(panel);
            return Ok(Effected::done());
        }
        if self.io.is_some() {
            return Err("another file operation is already running".into());
        }
        match effect {
            HexEffect::Search | HexEffect::GotoOffset => Ok(Effected::done()),
            HexEffect::OpenFile { path: Some(path) } => {
                self.open_path(PathBuf::from(path))?;
                Ok(Effected::done())
            }
            HexEffect::OpenFile { path: None } => {
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
                EffectStatus::unavailable(HexEffect::SEARCH, "no_file", "Open a file first"),
                EffectStatus::unavailable(HexEffect::GOTO_OFFSET, "no_file", "Open a file first"),
            ]
        }
    }

    fn bound_declarations(&self) -> Vec<ActionDecl<HexEditor>> {
        if !self.has_file {
            return Vec::new();
        }
        HexEditor::at_cursor(
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
        doc: &mut ui::widget::Edits<'_>,
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
            *handled = true;
        }
    }

    /// The key actions while no file is open: open one (Ctrl+O) or start
    /// an empty one (Ctrl+N). Part of [`Self::handle_key_event`].
    fn handle_empty_state_key(&mut self, key: &Key, ctrl: bool, handled: &mut bool) {
        let Key::Character(c) = key else {
            return;
        };
        if !ctrl {
            return;
        }
        if c.eq_ignore_ascii_case("o") {
            self.invoke(&HexEffect::OpenFile { path: None });
            *handled = true;
        } else if c.eq_ignore_ascii_case("n") {
            self.new_empty_file();
            *handled = true;
        }
    }

    /// Open the window's search or the go-to form.
    fn open_panel(&mut self, panel: HexPanel) {
        match panel {
            HexPanel::Search => Self::open_find(SearchOpening::Find),
            HexPanel::GotoOffset => self.open_goto(),
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
            "c" | "C" => {
                self.copy_selection();
            }
            "x" | "X" => {
                self.cut_selection();
            }
            "v" | "V" => {
                self.paste_clipboard();
            }
            "o" | "O" => self.invoke(&HexEffect::OpenFile { path: None }),
            "s" | "S" => self.invoke(&HexEffect::Save),
            "f" | "F" => self.open_panel(HexPanel::Search),
            "h" | "H" => Self::open_find(SearchOpening::Replace),
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
    /// Stage a screenshot scenario (`None` for a plain `--shots`) over the
    /// bundled sample the scenario opened: the view state the scenario
    /// shows. Never runs in a live window.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        if scenario == Some("empty") {
            return;
        }
        self.stage_sample();
        match scenario {
            Some("find") => {
                let hex = ui::widget::FindOptions {
                    mode: ui::widget::FindMode::Hex,
                    ..ui::widget::FindOptions::default()
                };
                ui::widget::Findable::find(self, "49 48 44 52", hex);
                ui::widget::Findable::reveal(self, 0);
            }
            Some("goto") => self.open_goto(),
            Some("edited") => {
                for (off, value) in [(0x1D_usize, 0x01_u8), (0x1E, 0x00), (0x25, 0xFF)] {
                    self.model.set_byte(off, value);
                }
                // The edits are the resource's, as typing them would be.
                for edit in self.model.take_edits() {
                    if let (Some(dispatch), Some(edit)) = (&self.dispatch, byte_edit(&edit)) {
                        dispatch.submit(edit);
                    }
                }
                self.model.move_to(0x10, false);
                self.model.move_to(0x13, true);
            }
            Some("error") => self.problem("Couldn't save the file", "Permission denied"),
            Some("narrow-inspector") => self.reveal_inspector = true,
            _ => {}
        }
        self.dirty = true;
    }
}

/// `edit` as the field edit of the record's bytes it is.
fn byte_edit(edit: &BufferEdit) -> Option<ui::tilo_resource::Edit<HexEditor>> {
    HexEditor::paths().bytes.perform(edit).ok()
}

impl ui::widget::controls::ActionControls for HexView {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search;

    /// Present the bundled sample as the scenario's open does, then stage
    /// `scenario` on it.
    fn staged(scenario: Option<&str>) -> HexView {
        let mut view = HexView::empty();
        if scenario != Some("empty") {
            view.present(&Rc::new(HexEditor {
                name: Some(ui::tilo_resource::Atom("sample.png".into())),
                bytes: crate::buffer::ByteBuffer::of(&crate::sample::png_image()),
                opened: 1,
            }));
        }
        view.stage_scenario(scenario);
        view
    }

    /// The resource's state is what the view shows: an open loads fresh,
    /// any later change keeps the cursor and marks what differs from the
    /// opened bytes.
    #[test]
    fn presented_state_loads_an_open_and_shows_later_changes_in_place() {
        let mut view = HexView::empty();
        assert!(!view.has_file());
        let state = |bytes: &[u8], opened| {
            Rc::new(HexEditor {
                name: Some(ui::tilo_resource::Atom("bytes.bin".into())),
                bytes: crate::buffer::ByteBuffer::of(bytes),
                opened,
            })
        };
        view.present(&state(&[1, 2, 3, 4], 1));
        assert!(view.has_file());
        assert_eq!(view.model.bytes(), &[1, 2, 3, 4]);
        view.model.move_to(3, false);
        view.present(&state(&[1, 9, 3, 4], 1));
        assert_eq!(view.model.cursor(), 3, "a change keeps the cursor");
        assert_eq!(view.model.dirty_count(), 1);
        view.present(&state(&[5, 6], 2));
        assert_eq!(view.model.cursor(), 0, "a new open starts at the top");
        assert_eq!(view.model.dirty_count(), 0);
    }

    #[test]
    fn presented_local_edits_clear_highlights_when_saved_bytes_are_restored() {
        let mut view = HexView::empty();
        let state = |bytes: &[u8]| {
            Rc::new(HexEditor {
                name: Some(ui::tilo_resource::Atom("bytes.bin".into())),
                bytes: crate::buffer::ByteBuffer::of(bytes),
                opened: 1,
            })
        };
        view.present(&state(&[0x12, 0x34]));
        view.model.input_hex_digit(0xa);
        view.model.input_hex_digit(0xb);
        view.present(&state(&[0xab, 0x34]));
        assert!(view.model.is_dirty(0));
        view.model.move_to(0, false);
        view.model.input_hex_digit(1);
        view.model.input_hex_digit(2);
        view.present(&state(&[0x12, 0x34]));
        assert_eq!(view.model.dirty_count(), 0);
        assert_eq!(
            view.model.cursor(),
            1,
            "reconciliation preserves typing state"
        );
        assert_eq!(view.model.bytes(), &[0x12, 0x34]);
    }

    /// A view on `bytes`, with the grid in the hex column (`ascii` false) or
    /// the ASCII one.
    fn editing(bytes: &[u8], ascii: bool) -> HexView {
        let mut view = HexView::empty();
        view.present(&Rc::new(HexEditor {
            name: Some(ui::tilo_resource::Atom("bytes.bin".into())),
            bytes: crate::buffer::ByteBuffer::of(bytes),
            opened: 1,
        }));
        if ascii {
            view.model.toggle_column();
        }
        view
    }

    /// A clipboard this test has to itself: the process clipboard is shared
    /// with every test running beside it.
    fn own_clipboard() -> ui::host::HostScope {
        let (services, _recorder) = ui::host::HostServices::recording();
        ui::host::enter(&services)
    }

    /// Copy takes the selection in the focus mode, and paste reads it back in
    /// the same mode.
    #[test]
    fn copy_carries_the_selection_in_the_focus_mode() {
        let _clipboard = own_clipboard();
        let mut view = editing(b"PNGDATA", false);
        view.model.move_to(0, false);
        view.model.move_to(3, true);
        assert!(view.copy_selection());
        assert_eq!(ui::clipboard_paste().as_deref(), Some("50 4e 47 44"));
        view.model.toggle_column();
        assert!(view.copy_selection(), "the same bytes as text");
        assert_eq!(ui::clipboard_paste().as_deref(), Some("PNGD"));
    }

    /// Nothing selected copies nothing and says why.
    #[test]
    fn copying_with_nothing_selected_refuses_and_leaves_the_clipboard_alone() {
        let _clipboard = own_clipboard();
        ui::clipboard_copy("untouched");
        let mut view = editing(b"PNGDATA", false);
        assert!(!view.copy_selection());
        assert_eq!(ui::clipboard_paste().as_deref(), Some("untouched"));
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
            assert!(
                g.column_head.y0 > view.rect.y0 && g.column_head.y0 < view.rect.y0 + g.row_h,
                "the column labels stand at the top: the view draws no header row: {g:?}"
            );
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
        ui::testing::dispatch(view, event)
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

    /// The pointer drives the view's own controls: the grid moves the
    /// cursor, the inspector's strip switches the endianness, the scroll bar
    /// scrolls.
    #[test]
    fn pointer_drives_the_grid_the_endian_strip_and_the_scroll_bar() {
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let mut view = staged(None);
        view.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        paint_frame(&mut view, &mut tr, &theme);
        assert_eq!(view.model.endian(), Endian::Big);

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

        let left = |strip: Rect| Rect::new(strip.x0, strip.y0, strip.center().x, strip.y1);
        assert!(click(&mut view, left(g.endian)));
        assert_eq!(view.model.endian(), Endian::Little);
        paint_frame(&mut view, &mut tr, &theme);

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
        for (name, found, goto) in [
            (None, false, false),
            (Some("find"), true, false),
            (Some("goto"), false, true),
            (Some("edited"), false, false),
            (Some("error"), false, false),
            (Some("narrow-inspector"), false, false),
            (Some("empty"), false, false),
        ] {
            for theme in [Theme::dark(), Theme::light()] {
                for (w, h) in [(1280.0, 800.0), (640.0, 720.0)] {
                    let mut view = staged(name);
                    view.rect = Rect::new(0.0, 0.0, w, h);
                    assert_eq!(view.has_file(), name != Some("empty"));
                    assert_eq!(!view.matches.is_empty(), found, "{name:?}");
                    assert_eq!(view.goto.is_open(), goto, "{name:?}");
                    paint_frame(&mut view, &mut tr, &theme);
                    paint_frame(&mut view, &mut tr, &theme);
                }
            }
        }
        let mut view = staged(Some("narrow-inspector"));
        view.rect = Rect::new(0.0, 0.0, 640.0, 720.0);
        paint_frame(&mut view, &mut tr, &Theme::dark());
        assert!(
            view.geo.inspector_visible,
            "the narrow scenario opens the drawer"
        );
        let view = staged(Some("edited"));
        assert_eq!(view.model.dirty_count(), 3);
        assert_eq!(view.model.selection_len(), 4);
    }
}
