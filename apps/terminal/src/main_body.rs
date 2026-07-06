use ui::ColorCompat;
use ui::{Theme, widget::Widget, text_render::{TextRenderer, TextWeight}};
use automerge::{AutoCommit, ObjId, Value, transaction::Transactable, ObjType, ReadDoc};
use vello::Scene;
use vello::kurbo::{Affine, Rect, Point};
use vello::peniko::Color;
use winit::event::{WindowEvent, ElementState, KeyEvent, MouseButton, MouseScrollDelta};
use winit::keyboard::{Key, NamedKey};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use portable_pty::{native_pty_system, CommandBuilder, PtySize, MasterPty};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, OnceLock};
use vte::{Parser, Perform};
use crossbeam_channel::Receiver;
use unicode_width::UnicodeWidthChar;

/// Cell coordinate within the rendered terminal grid. (row, col) are
/// indexed relative to the visible viewport (the slice of `grid` that's
/// currently on screen — see `start_y` in `draw`). The selection model
/// stores these as viewport-local so highlight rendering can use them
/// without re-translating through the scrollback offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GridPos {
    pub row: usize,
    pub col: usize,
}

/// Flow-selection state, matching xterm / GNOME-Terminal / kitty / alacritty
/// semantics: `anchor` is where the drag started, `cursor` is where the
/// pointer currently is, and the selected region walks the buffer in
/// reading order from `min(anchor, cursor)` to `max(anchor, cursor)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: GridPos,
    pub cursor: GridPos,
}

impl Selection {
    /// Returns the (start, end) endpoints in reading order.
    pub fn ordered(&self) -> (GridPos, GridPos) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    /// True if anchor and cursor coincide — a "collapsed" selection.
    /// Used after mouse-release to drop selections that were really just
    /// idle clicks.
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.cursor
    }
}

/// Process-wide clipboard handle, lazily initialised on first use. Mirrors
/// `ui::widget::text_input::clipboard()` — once a parallel subagent extracts
/// a shared `ui::clipboard_copy` helper, this fallback can be removed.
fn clipboard_handle() -> Option<&'static Mutex<arboard::Clipboard>> {
    static CB: OnceLock<Option<Mutex<arboard::Clipboard>>> = OnceLock::new();
    CB.get_or_init(|| arboard::Clipboard::new().ok().map(Mutex::new))
        .as_ref()
}

/// Best-effort clipboard write. Silent on failure — clipboard absence
/// shouldn't crash the terminal.
pub(crate) fn clipboard_copy(text: &str) {
    if let Some(cb) = clipboard_handle() {
        if let Ok(mut handle) = cb.lock() {
            let _ = handle.set_text(text.to_string());
        }
    }
}

/// Best-effort clipboard read for paste. Returns `None` when no clipboard
/// backend is available (e.g. headless CI) — paste is then a no-op rather
/// than a crash.
pub(crate) fn clipboard_paste() -> Option<String> {
    let cb = clipboard_handle()?;
    let mut handle = cb.lock().ok()?;
    handle.get_text().ok()
}

/// A single foreground/background color, as produced by SGR. `Default`
/// tracks the theme (fg → `palette.fg_default`, bg → transparent /
/// `palette.bg_default`); `Indexed` is an ANSI-256 palette slot; `Rgb`
/// is a 24-bit truecolor triple.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CellColor {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl CellColor {
    /// Fixed 4-byte encoding: `[kind, a, b, c]`.
    fn pack(self) -> [u8; 4] {
        match self {
            CellColor::Default => [0, 0, 0, 0],
            CellColor::Indexed(n) => [1, n, 0, 0],
            CellColor::Rgb(r, g, b) => [2, r, g, b],
        }
    }
    fn unpack(b: &[u8]) -> Self {
        match b.first().copied().unwrap_or(0) {
            1 => CellColor::Indexed(b[1]),
            2 => CellColor::Rgb(b[1], b[2], b[3]),
            _ => CellColor::Default,
        }
    }
}

/// Per-cell rendering attributes. Colors are `CellColor` (default / ANSI-256
/// / truecolor); the four SGR style bits plus the wide-glyph continuation
/// marker are packed into a single flags byte.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Attr {
    pub fg: CellColor,
    pub bg: CellColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub is_wide_cont: bool,
}

/// Bytes per cell in the packed `attrs` blob: fg(4) + bg(4) + flags(1).
const ATTR_BYTES: usize = 9;

impl Attr {
    pub fn pack(self) -> [u8; ATTR_BYTES] {
        let f = self.fg.pack();
        let b = self.bg.pack();
        let mut flags = 0u8;
        if self.bold { flags |= 0b0_0001; }
        if self.italic { flags |= 0b0_0010; }
        if self.underline { flags |= 0b0_0100; }
        if self.reverse { flags |= 0b0_1000; }
        if self.is_wide_cont { flags |= 0b1_0000; }
        [f[0], f[1], f[2], f[3], b[0], b[1], b[2], b[3], flags]
    }
    pub fn unpack(bytes: &[u8]) -> Self {
        if bytes.len() < ATTR_BYTES { return Self::default(); }
        let flags = bytes[8];
        Self {
            fg: CellColor::unpack(&bytes[0..4]),
            bg: CellColor::unpack(&bytes[4..8]),
            bold: flags & 0b0_0001 != 0,
            italic: flags & 0b0_0010 != 0,
            underline: flags & 0b0_0100 != 0,
            reverse: flags & 0b0_1000 != 0,
            is_wide_cont: flags & 0b1_0000 != 0,
        }
    }
}

impl Default for Attr {
    fn default() -> Self {
        Self {
            fg: CellColor::Default,
            bg: CellColor::Default,
            bold: false,
            italic: false,
            underline: false,
            reverse: false,
            is_wide_cont: false,
        }
    }
}

/// Terminal mode flags toggled by DEC private set/reset (`CSI ? Ps h/l`).
/// Owned by the widget and passed by mutable reference into the vte
/// performer so parsed modes (bracketed paste, cursor visibility, mouse
/// reporting, application cursor keys) survive across PTY reads.
#[derive(Clone, Copy, Debug)]
pub struct TermModes {
    /// `?25` — block cursor drawn when true (DECTCEM).
    pub cursor_visible: bool,
    /// `?2004` — wrap pasted text in `ESC[200~`…`ESC[201~`.
    pub bracketed_paste: bool,
    /// `?1` — arrow keys send `ESC O A` instead of `ESC [ A` (DECCKM).
    pub app_cursor_keys: bool,
    /// `?1000`/`?1002`/`?1003` — some form of mouse reporting requested.
    /// Tracked so wheel scrolling can defer to the app; event forwarding
    /// itself is intentionally a stub.
    pub mouse_report: bool,
    /// `?1006` — SGR-encoded mouse coordinates requested.
    pub mouse_sgr: bool,
}

impl Default for TermModes {
    fn default() -> Self {
        Self {
            cursor_visible: true,
            bracketed_paste: false,
            app_cursor_keys: false,
            mouse_report: false,
            mouse_sgr: false,
        }
    }
}

pub struct TerminalWidget {
    rect: Rect,
    pty_write: Arc<Mutex<Box<dyn Write + Send>>>,
    pty_master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    output_rx: Receiver<Vec<u8>>,
    parser: Parser,

    grid: Vec<String>,
    grid_attrs: Vec<Vec<Attr>>,
    cursor_x: usize,
    cursor_y: usize,
    width: usize,
    height: usize,

    char_w: f64,
    char_h: f64,
    /// Cached font size, refreshed from `theme.tokens.font_size_base` on every
    /// `draw`. `measure` runs before `draw` and does not receive a theme, so
    /// it reads this cached value instead of hard-coding a size. Seeded with
    /// 0.0 and updated before the first frame is drawn.
    font_size: f32,

    grid_id: Option<ObjId>,
    /// Alternate-screen buffer (its own CRDT list). Full-screen apps
    /// (`vim`, `less`, `htop`) switch to it via `CSI ? 1049 h` so their
    /// redraw never pollutes the scrollback, then restore the main buffer
    /// on exit. Created lazily alongside the main grid in `init_crdt`.
    alt_grid_id: Option<ObjId>,
    cursor_id: Option<ObjId>,
    line_ids: Vec<ObjId>,
    alt_line_ids: Vec<ObjId>,
    last_sync_heads: Vec<automerge::ChangeHash>,
    /// Set when the active buffer flips (main ↔ alt) so `sync` re-reads the
    /// grid even if the CRDT heads did not change (leaving the alt screen
    /// only restores the cursor, which is not a doc edit).
    force_resync: bool,

    current_attr: Attr,
    pending_wrap: bool,

    /// True while the alternate screen is the active buffer.
    alt_active: bool,
    /// Cursor + attribute saved on alt-screen entry / DECSC, restored on
    /// exit / DECRC.
    saved_cursor: Option<(usize, usize, Attr)>,
    /// DEC scrolling region (0-based, inclusive). Defaults to the full
    /// screen; honoured for the alternate buffer's scroll/insert/delete.
    scroll_top: usize,
    scroll_bottom: usize,

    /// Terminal mode flags (cursor visibility, bracketed paste, …).
    modes: TermModes,

    /// Rows scrolled up from the bottom of the buffer. 0 == pinned to the
    /// live output; a new keystroke or fresh PTY output snaps it back to 0.
    scroll_offset: usize,

    /// Last palette-derived cursor color, captured on every `draw`. Exposed
    /// for behavioural tests that want to verify palette swaps actually
    /// propagate into rendered state without spinning up a GPU scene.
    last_cursor_color: Color,
    /// Last default-fg color drawn, same rationale as `last_cursor_color`.
    last_fg_default: Color,
    /// Last default-bg color drawn, same rationale as `last_cursor_color`.
    last_bg_default: Color,

    /// Active flow-selection. `None` when nothing is selected (idle, or
    /// just after a lone click). Coordinates are viewport-local rows /
    /// grid columns; conversion to grid pixels uses `char_w`/`char_h`
    /// and `self.rect`.
    selection: Option<Selection>,
    /// True between left-mouse-press and left-mouse-release. While set,
    /// `CursorMoved` events update `selection.cursor` to track the drag.
    dragging: bool,
    /// Cursor position cached on every `CursorMoved` so the press arm
    /// can hit-test without receiving a fresh position. Logical coords
    /// (post scale-factor divide), matching `text_input`'s convention.
    last_cursor_pos: Point,
    /// Viewport scrollback offset captured on the most recent `draw`.
    /// Stored so `handle_event` can translate logical pixel coordinates
    /// into viewport-local grid rows (the selection model lives in
    /// viewport space). Defaults to 0 until the first frame paints.
    viewport_start_y: usize,
}

impl TerminalWidget {
    pub fn new(writer: Arc<Mutex<Box<dyn Write + Send>>>, master: Arc<Mutex<Box<dyn MasterPty + Send>>>, output_rx: Receiver<Vec<u8>>) -> Self {
        let width = 80;
        let height = 24;
        let mut grid = Vec::new();
        let mut grid_attrs = Vec::new();
        for _ in 0..height {
            grid.push(" ".repeat(width));
            grid_attrs.push(vec![Attr::default(); width]);
        }

        Self {
            rect: Rect::ZERO,
            pty_write: writer,
            pty_master: master,
            output_rx,
            parser: Parser::new(),
            grid,
            grid_attrs,
            cursor_x: 0,
            cursor_y: 0,
            width,
            height,
            // Seed values only — `draw` overwrites char_w/char_h from the
            // theme-derived `font_size` on the first frame, and `measure`
            // uses the cached `font_size` (seeded to 0.0 so it falls back to
            // these dimensions until the first draw completes).
            char_w: 9.0,
            char_h: 18.0,
            font_size: 0.0,
            grid_id: None,
            alt_grid_id: None,
            cursor_id: None,
            line_ids: Vec::new(),
            alt_line_ids: Vec::new(),
            last_sync_heads: Vec::new(),
            force_resync: false,
            current_attr: Attr::default(),
            pending_wrap: false,
            alt_active: false,
            saved_cursor: None,
            scroll_top: 0,
            scroll_bottom: height - 1,
            modes: TermModes::default(),
            scroll_offset: 0,
            last_cursor_color: Color::from_rgba8(0, 0, 0, 0),
            last_fg_default: Color::from_rgba8(0, 0, 0, 0),
            last_bg_default: Color::from_rgba8(0, 0, 0, 0),
            selection: None,
            dragging: false,
            last_cursor_pos: Point::ZERO,
            viewport_start_y: 0,
        }
    }

    /// Color used for the block cursor on the last `draw`. Test-only accessor
    /// so palette-swap tests can assert the terminal picks up a new theme.
    pub fn last_cursor_color(&self) -> Color { self.last_cursor_color }

    /// Active selection, if any. Test-only — production code reads from
    /// the widget directly inside `draw` / `handle_event`.
    pub fn selection(&self) -> Option<Selection> { self.selection }
    /// True between left-mouse-press and left-mouse-release. Test-only.
    pub fn is_dragging(&self) -> bool { self.dragging }
    /// True while the alternate screen buffer is active. Test/host accessor.
    pub fn is_alt_screen(&self) -> bool { self.alt_active }
    /// Current terminal mode flags. Test/host accessor.
    pub fn modes(&self) -> TermModes { self.modes }
    /// Rows currently scrolled back from the live bottom. Test/host accessor.
    pub fn scroll_offset(&self) -> usize { self.scroll_offset }
    /// Force the widget into a known geometry for tests. Replicates the
    /// effect of `apply_layout` without needing a Taffy tree.
    #[cfg(test)]
    pub fn force_geometry(&mut self, rect: Rect, char_w: f64, char_h: f64) {
        self.rect = rect;
        self.char_w = char_w;
        self.char_h = char_h;
    }
    /// Force a viewport scrollback offset for tests that exercise
    /// `selected_text` over a `grid` larger than the visible viewport.
    #[cfg(test)]
    pub fn force_viewport_start(&mut self, start: usize) {
        self.viewport_start_y = start;
    }
    /// Inject a selection from a test. Avoids needing to simulate a
    /// full press/drag/release sequence when the test is about
    /// downstream behaviour (highlight spans, clipboard text).
    #[cfg(test)]
    pub fn force_selection(&mut self, sel: Option<Selection>) {
        self.selection = sel;
    }
    /// Replace `grid` from a test. Bypasses the PTY + CRDT path so
    /// selected-text tests can assert against a known buffer.
    #[cfg(test)]
    pub fn force_grid(&mut self, grid: Vec<String>) {
        self.grid = grid;
    }
    /// The rows the viewport currently shows, top-to-bottom, honouring the
    /// scrollback offset. Test accessor: lets a scrollback test assert that
    /// scrolling up reveals older lines without a GPU scene.
    #[cfg(test)]
    pub fn visible_lines(&self) -> Vec<String> {
        let vis = self.current_visible_rows();
        let max_top = self.grid.len().saturating_sub(vis);
        let scroll = if self.alt_active { 0 } else { self.scroll_offset };
        let start = max_top.saturating_sub(scroll);
        self.grid.iter().skip(start).take(vis).cloned().collect()
    }
    /// The parsed attribute for the mirrored grid cell at `(row, col)`.
    /// Test accessor for SGR-parsing assertions.
    #[cfg(test)]
    pub fn cell_attr(&self, row: usize, col: usize) -> Attr {
        self.grid_attrs
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or_default()
    }
    /// The mirrored grid row text at `row`. Test accessor.
    #[cfg(test)]
    pub fn grid_row(&self, row: usize) -> String {
        self.grid.get(row).cloned().unwrap_or_default()
    }
    /// Compute the per-row spans for the current selection, exposed for
    /// tests. Returns an empty vec when no selection is active.
    #[cfg(test)]
    pub fn current_row_spans(&self) -> Vec<(usize, usize, usize)> {
        self.selection
            .as_ref()
            .map(|sel| selection_row_spans(sel, self.width))
            .unwrap_or_default()
    }
    /// Test-only access to the selected-text helper.
    #[cfg(test)]
    pub fn test_selected_text(&self) -> Option<String> { self.selected_text() }
    /// Default-fg color last drawn with (sampled from the theme's terminal
    /// palette). Test-only.
    pub fn last_fg_default(&self) -> Color { self.last_fg_default }
    /// Default-bg color last drawn with (sampled from the theme's terminal
    /// palette). Test-only.
    pub fn last_bg_default(&self) -> Color { self.last_bg_default }

    /// Number of grid rows currently visible, from the paint geometry.
    fn current_visible_rows(&self) -> usize {
        (self.rect.height() / self.char_h.max(1.0)).floor().max(1.0) as usize
    }

    /// Furthest the viewport can scroll up: the number of rows above the
    /// live viewport.
    fn max_scroll(&self) -> usize {
        self.grid.len().saturating_sub(self.current_visible_rows())
    }

    /// Scroll the viewport back (toward older output) by `n` rows.
    pub fn scroll_up(&mut self, n: usize) {
        if self.alt_active { return; }
        self.scroll_offset = (self.scroll_offset + n).min(self.max_scroll());
    }
    /// Scroll the viewport forward (toward the live bottom) by `n` rows.
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }
    /// Jump to the oldest retained line.
    pub fn scroll_to_top(&mut self) {
        if self.alt_active { return; }
        self.scroll_offset = self.max_scroll();
    }
    /// Pin the viewport back to the live output.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Drain queued PTY bytes and replay them through the vte parser into the
    /// CRDT grid. Shared by both `Widget::update` (for hosts that pump it, e.g.
    /// the compositor's tile host) and `Widget::sync` (for the plain `ui::App`
    /// loop, which never calls `update`). Returns whether any bytes were
    /// processed this call.
    fn pump(&mut self, state: &mut AutoCommit, root: &ObjId) -> bool {
        self.init_crdt(state, root);

        // Collect queued messages first so the `output_rx` borrow is dropped
        // before `feed` takes `&mut self`.
        let mut msgs = Vec::new();
        while let Ok(bytes) = self.output_rx.try_recv() {
            msgs.push(bytes);
        }
        let activity = !msgs.is_empty();
        for bytes in &msgs {
            self.feed(state, root, bytes);
        }
        // Fresh output snaps the viewport back to the live bottom.
        if activity {
            self.scroll_offset = 0;
        }
        activity
    }

    /// Run `bytes` through the vte parser + performer, mutating the active
    /// CRDT buffer. This is the single production entry point for terminal
    /// input processing — the PTY pump feeds real shell output through it,
    /// and tests / headless screenshot scenarios feed canned VT sequences
    /// through the very same path.
    pub fn feed(&mut self, state: &mut AutoCommit, root: &ObjId, bytes: &[u8]) {
        self.init_crdt(state, root);

        // Clamp cursors in case of resize.
        self.cursor_x = self.cursor_x.min(self.width.saturating_sub(1));
        self.cursor_y = self.cursor_y.min(self.height.saturating_sub(1));

        let was_alt = self.alt_active;
        let mut performer = TerminalPerformer {
            state,
            main_grid_id: self.grid_id.clone().unwrap(),
            alt_grid_id: self.alt_grid_id.clone().unwrap(),
            cursor_id: self.cursor_id.clone().unwrap(),
            main_line_ids: &mut self.line_ids,
            alt_line_ids: &mut self.alt_line_ids,
            alt_active: &mut self.alt_active,
            width: self.width,
            height: self.height,
            cursor_x: self.cursor_x,
            cursor_y: self.cursor_y,
            current_attr: self.current_attr,
            pending_wrap: self.pending_wrap,
            modes: &mut self.modes,
            saved_cursor: &mut self.saved_cursor,
            scroll_top: self.scroll_top,
            scroll_bottom: self.scroll_bottom,
        };
        // vte 0.15: `advance` takes `&[u8]` instead of one byte at a time.
        self.parser.advance(&mut performer, bytes);
        self.cursor_x = performer.cursor_x;
        self.cursor_y = performer.cursor_y;
        self.current_attr = performer.current_attr;
        self.pending_wrap = performer.pending_wrap;
        self.scroll_top = performer.scroll_top;
        self.scroll_bottom = performer.scroll_bottom;

        // A buffer flip must force `sync` to re-read even if heads match.
        if self.alt_active != was_alt {
            self.force_resync = true;
        }
    }

    /// Place the terminal at an explicit `rect` and reflow the grid + PTY to
    /// match. This is the embedding entry point: a host widget (e.g. the code
    /// editor's terminal panel) that owns the terminal outside a Taffy leaf
    /// calls this instead of `apply_layout` to give it a region to render in.
    pub fn place_at(&mut self, rect: Rect) {
        self.set_geometry(rect);
    }

    /// Write raw bytes to the PTY's stdin — the same path a keystroke takes,
    /// exposed so an embedding host (or a headless screenshot scenario) can
    /// drive the shell programmatically (e.g. run a command for a demo frame).
    pub fn write_input(&mut self, bytes: &[u8]) {
        if let Ok(mut w) = self.pty_write.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    /// Paste `text` into the PTY. When the running app has requested
    /// bracketed paste (`CSI ? 2004 h`) the text is wrapped in the
    /// `ESC[200~`…`ESC[201~` guards so the app can tell a paste from typed
    /// input; otherwise newlines are normalised to carriage returns (what
    /// the Enter key sends). Pasting snaps the viewport to the live bottom.
    pub fn paste(&mut self, text: &str) {
        if text.is_empty() { return; }
        let mut out = Vec::new();
        if self.modes.bracketed_paste {
            out.extend_from_slice(b"\x1b[200~");
            out.extend_from_slice(text.as_bytes());
            out.extend_from_slice(b"\x1b[201~");
        } else {
            let normalised = text.replace("\r\n", "\r").replace('\n', "\r");
            out.extend_from_slice(normalised.as_bytes());
        }
        self.write_input(&out);
        self.scroll_offset = 0;
    }

    /// Assign `rect` and, if the cell grid dimensions changed, reflow the
    /// CRDT-backed grid and resize the PTY so the shell sees the new size.
    fn set_geometry(&mut self, rect: Rect) {
        self.rect = rect;

        let new_width = (rect.width() / self.char_w.max(1.0)).floor() as usize;
        let new_height = (rect.height() / self.char_h.max(1.0)).floor() as usize;

        // Cap the CRDT-backed grid at a sensible terminal size. The init pass
        // splices `self.height * self.width` characters into a fresh automerge
        // doc; each char is its own column op, and automerge's column-data
        // insert is O(doc_size), so a full-screen embed (186 × 219 ≈ 40 700
        // chars) freezes the process for tens of seconds while the doc
        // compacts. The visible region can be larger than the CRDT grid —
        // overflow rows render empty until the grid is made sparse/lazy.
        let new_width = new_width.min(200);
        let new_height = new_height.min(80);
        if new_width > 0 && new_height > 0 && (new_width != self.width || new_height != self.height) {
            let history_offset = self.line_ids.len().saturating_sub(self.height);
            let absolute_y = history_offset + self.cursor_y;

            let new_history_offset = self.line_ids.len().saturating_sub(new_height);
            let new_cursor_y = absolute_y.saturating_sub(new_history_offset).min(new_height.saturating_sub(1));

            self.cursor_y = new_cursor_y;
            self.cursor_x = self.cursor_x.min(new_width.saturating_sub(1));

            self.width = new_width;
            self.height = new_height;
            // Reset the scrolling region to the full new screen unless an
            // app narrows it again.
            self.scroll_top = 0;
            self.scroll_bottom = new_height.saturating_sub(1);
            let _ = self.pty_master.lock().unwrap().resize(PtySize {
                rows: self.height as u16,
                cols: self.width as u16,
                pixel_width: rect.width() as u16,
                pixel_height: rect.height() as u16,
            });
        }
    }

    pub(crate) fn init_crdt(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.grid_id.is_none() {
            if let Ok(Some((Value::Object(ObjType::List), id))) = state.get(root, "terminal_grid") {
                self.grid_id = Some(id.clone());
            } else {
                let id = state.put_object(root, "terminal_grid", ObjType::List).unwrap();
                self.grid_id = Some(id);
            }
        }
        Self::ensure_lines(state, self.grid_id.as_ref().unwrap(), self.height, self.width, &mut self.line_ids);

        if self.alt_grid_id.is_none() {
            if let Ok(Some((Value::Object(ObjType::List), id))) = state.get(root, "terminal_grid_alt") {
                self.alt_grid_id = Some(id.clone());
            } else {
                let id = state.put_object(root, "terminal_grid_alt", ObjType::List).unwrap();
                self.alt_grid_id = Some(id);
            }
        }
        Self::ensure_lines(state, self.alt_grid_id.as_ref().unwrap(), self.height, self.width, &mut self.alt_line_ids);

        if self.cursor_id.is_none() {
            if let Ok(Some((Value::Object(ObjType::Map), id))) = state.get(root, "cursor") {
                self.cursor_id = Some(id.clone());
                if let Ok(Some((Value::Scalar(v), _))) = state.get(&id, "y") {
                    if let Some(y) = v.to_i64() {
                        let abs_y = y as usize;
                        let history_offset = self.line_ids.len().saturating_sub(self.height);
                        self.cursor_y = abs_y.saturating_sub(history_offset).min(self.height.saturating_sub(1));
                    }
                }
            } else {
                let cursor_id = state.put_object(root, "cursor", ObjType::Map).unwrap();
                state.put(&cursor_id, "x", 0u64).unwrap();
                state.put(&cursor_id, "y", 0u64).unwrap();
                self.cursor_id = Some(cursor_id);
            }
        }
    }

    /// Ensure `grid_id` holds at least `min_len` blank lines and refresh
    /// `ids` to mirror the list. Shared by the main and alternate buffers.
    fn ensure_lines(state: &mut AutoCommit, grid_id: &ObjId, min_len: usize, width: usize, ids: &mut Vec<ObjId>) {
        let current_len = state.length(grid_id);
        if current_len < min_len {
            for y in current_len..min_len {
                let line_id = state.insert_object(grid_id, y, ObjType::Map).unwrap();
                let text_id = state.put_object(&line_id, "text", ObjType::Text).unwrap();
                let _ = state.splice_text(&text_id, 0, 0, &" ".repeat(width));
                let attr_bytes: Vec<u8> = vec![Attr::default().pack(); width].concat();
                let _ = state.put(&line_id, "attrs", attr_bytes);
            }
        }
        let new_len = state.length(grid_id);
        if ids.len() != new_len {
            ids.clear();
            for i in 0..new_len {
                if let Ok(Some((Value::Object(ObjType::Map), line_id))) = state.get(grid_id, i) {
                    ids.push(line_id);
                }
            }
        }
    }
}

const MAX_HISTORY: usize = 1000;

impl Widget for TerminalWidget {
    fn measure(&mut self, _known: taffy::Size<Option<f32>>, _available: taffy::Size<taffy::AvailableSpace>, text_renderer: &mut TextRenderer) -> taffy::Size<f32> {
        // `measure` does not receive a theme; read the cached font size set
        // by the most recent `draw` (which pulls from
        // `theme.tokens.font_size_base`). Before the first draw the cache is
        // 0.0 and we keep the constructor-seeded char_w/char_h so layout still
        // yields a usable grid.
        if self.font_size > 0.0 {
            // Measure with the monospace family so the cell advance matches
            // the mono glyphs `draw` actually paints.
            let (w, _h) = text_renderer.measure_styled(" ", self.font_size, None, TextWeight::Regular, false, true);
            self.char_w = (w as f64).max(1.0);
            self.char_h = ((self.font_size as f64) * 1.25f64).ceil().max(1.0);
        }
        taffy::Size::ZERO
    }

    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Drain any bytes the PTY reader thread has queued and replay them
        // through the vte parser *here*, before reading the grid back out.
        // The plain `ui::App` event loop only ever calls `sync` (never
        // `update`), so if the pty were only pumped from `update` the live
        // terminal would never advance. Pumping from `sync` — combined with
        // `needs_animation_frame` keeping the loop ticking — is what makes a
        // standalone or embedded terminal actually show output.
        self.pump(state, root);

        let current_heads = state.get_heads();
        if !self.force_resync && !self.last_sync_heads.is_empty() && current_heads == self.last_sync_heads {
            return;
        }
        self.force_resync = false;
        self.last_sync_heads = current_heads;

        self.init_crdt(state, root);

        // Mirror whichever buffer is active. The alt buffer has no scrollback,
        // so its list is exactly `height` lines.
        let (grid_id, line_ids) = if self.alt_active {
            (self.alt_grid_id.clone(), &self.alt_line_ids)
        } else {
            (self.grid_id.clone(), &self.line_ids)
        };
        let grid_id = match grid_id {
            Some(id) => id,
            None => return,
        };

        let len = state.length(&grid_id);
        self.grid.resize(len, String::new());
        self.grid_attrs.resize(len, Vec::new());

        for i in 0..len.min(line_ids.len()) {
            if let Ok(Some((Value::Object(ObjType::Text), text_id))) = state.get(&line_ids[i], "text") {
                if let Ok(text) = state.text(&text_id) {
                    self.grid[i] = text;
                }
            }
            if let Ok(Some((Value::Scalar(v), _))) = state.get(&line_ids[i], "attrs") {
                if let automerge::ScalarValue::Bytes(bytes) = v.as_ref() {
                    self.grid_attrs[i] = bytes.chunks_exact(ATTR_BYTES).map(Attr::unpack).collect();
                }
            }
        }

        let active_len = line_ids.len();
        if let Some(cursor_id) = self.cursor_id.as_ref() {
            if let Ok(Some((Value::Scalar(v), _))) = state.get(cursor_id, "x") {
                if let Some(x) = v.to_i64() { self.cursor_x = x as usize; }
            }
            if let Ok(Some((Value::Scalar(v), _))) = state.get(cursor_id, "y") {
                if let Some(y) = v.to_i64() {
                    let abs_y = y as usize;
                    let history_offset = active_len.saturating_sub(self.height);
                    self.cursor_y = abs_y.saturating_sub(history_offset).min(self.height.saturating_sub(1));
                }
            }
        }
    }

    fn update(&mut self, state: &mut AutoCommit, root: &ObjId) -> bool {
        self.pump(state, root)
    }

    /// A live PTY may produce output at any time, but the reader thread has no
    /// handle to the winit event loop to wake it. Reporting that we always want
    /// another frame keeps `ui::App` ticking at ~60 Hz (its `about_to_wait`
    /// bounded `WaitUntil`) so `sync` re-runs and drains the PTY promptly. A
    /// host that embeds the terminal gates this behind its own visibility check.
    fn needs_animation_frame(&self) -> bool {
        true
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
            ..Default::default()
        }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        self.set_geometry(rect);
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &ui::presence::PresenceCtx) {
        // Pull every visual parameter from design tokens. Nothing in this
        // method may hard-code a color, font size, or padding — the OS owns
        // the theme and swapping it must reflect in the rendered output.
        let font_size = theme.tokens.font_size_base;
        let palette = &theme.tokens.terminal_palette;
        // Horizontal gutter between the terminal's left edge and the first
        // glyph column. Sourced from the shared spacing scale (`s1` == 4px
        // in the default tokens) so the OS can tune it centrally.
        let padding_x = theme.tokens.spacing.s1 as f64;

        // Cache the theme-derived font size so `measure` (which runs before
        // `draw` each frame and does not receive a theme) can size the grid
        // from the same value.
        self.font_size = font_size;

        scene.fill(vello::peniko::Fill::NonZero, transform, palette.bg_default, None, &self.rect);

        // Measure the space glyph in the monospace family — that's the cell
        // advance the mono glyph runs will use.
        let (w, _h) = text_renderer.measure_styled(" ", font_size, None, TextWeight::Regular, false, true);
        self.char_w = (w as f64).max(1.0);
        self.char_h = ((font_size as f64) * 1.25f64).ceil().max(1.0);

        let visible_rows = (self.rect.height() / self.char_h).floor() as usize;
        let max_top = self.grid.len().saturating_sub(visible_rows);
        // Alt screen has no scrollback; clamp the offset for the main buffer.
        let scroll = if self.alt_active { 0 } else { self.scroll_offset.min(max_top) };
        self.scroll_offset = scroll;
        let start_y = max_top.saturating_sub(scroll);

        // Cache for `handle_event`: pointer hit-tests translate logical y
        // into viewport-local rows by subtracting `self.rect.y0` and dividing
        // by `char_h`. The selection-rendering pass below uses the offset
        // directly to find the row in `self.grid`.
        self.viewport_start_y = start_y;

        // Paint the selection BEFORE the cell text so glyphs read on top
        // of the highlight. The selection model is viewport-local — row 0
        // is the first visible row, regardless of scrollback.
        if let Some(sel) = self.selection {
            let sel_color = theme.accent_primary.with_alpha_factor(0.30);
            for (row, start_col, end_col) in selection_row_spans(&sel, self.width) {
                if row >= visible_rows { continue; }
                let py = (self.rect.y0 + row as f64 * self.char_h).floor();
                let x0 = self.rect.x0 + padding_x + start_col as f64 * self.char_w;
                let x1 = self.rect.x0 + padding_x + end_col as f64 * self.char_w;
                let band = Rect::new(x0, py, x1, py + self.char_h);
                scene.fill(vello::peniko::Fill::NonZero, transform, sel_color, None, &band);
            }
        }

        for (y, line_text) in self.grid.iter().skip(start_y).take(visible_rows).enumerate() {
            let py = (self.rect.y0 + y as f64 * self.char_h).floor();
            let attrs_idx = y + start_y;
            if attrs_idx >= self.grid_attrs.len() { continue; }
            let attrs = &self.grid_attrs[attrs_idx];

            let mut current_run = String::new();
            let mut current_attr: Option<Attr> = None;
            let mut run_start_cell_idx = 0;

            let mut cell_idx = 0;
            for c in line_text.chars() {
                let c_w = c.width().unwrap_or(1);
                if cell_idx >= self.width { break; }
                let attr = if cell_idx < attrs.len() { attrs[cell_idx] } else { Attr::default() };

                if Some(attr) != current_attr {
                    if !current_run.is_empty() {
                        self.draw_run(scene, &current_run, current_attr.unwrap(), py, run_start_cell_idx as f64 * self.char_w + padding_x, text_renderer, transform, font_size, palette);
                    }
                    current_run.clear();
                    current_attr = Some(attr);
                    run_start_cell_idx = cell_idx;
                }
                current_run.push(c);
                cell_idx += c_w;
            }
            if !current_run.is_empty() {
                self.draw_run(scene, &current_run, current_attr.unwrap(), py, run_start_cell_idx as f64 * self.char_w + padding_x, text_renderer, transform, font_size, palette);
            }
        }

        // Cursor color is derived from the palette's default foreground at a
        // reduced opacity so it reads as a translucent block. Using the
        // palette makes the cursor follow theme swaps automatically. These
        // caches update every frame regardless of whether the cursor is
        // actually painted (hidden by `?25l` or scrolled off screen).
        let cursor_color = cursor_from_palette(palette);
        self.last_cursor_color = cursor_color;
        self.last_fg_default = palette.fg_default;
        self.last_bg_default = palette.bg_default;

        // Absolute grid row of the cursor → visible row (may be scrolled off).
        let cursor_abs = self.grid.len().saturating_sub(self.height) + self.cursor_y;
        if self.modes.cursor_visible
            && cursor_abs >= start_y
            && cursor_abs < start_y + visible_rows.max(1)
        {
            let cy_local = cursor_abs - start_y;
            let cx = self.rect.x0 + padding_x + self.cursor_x as f64 * self.char_w;
            let cy = (self.rect.y0 + cy_local as f64 * self.char_h).floor();
            let cursor_rect = Rect::new(cx, cy, cx + self.char_w, cy + self.char_h);
            scene.fill(vello::peniko::Fill::NonZero, transform, cursor_color, None, &cursor_rect);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &winit::event::Modifiers, state: &mut AutoCommit, root: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        self.init_crdt(state, root);

        match event {
            WindowEvent::MouseWheel { delta, .. } => {
                if !self.rect.contains(self.last_cursor_pos) { return; }
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (*y * 3.0).round() as i32,
                    MouseScrollDelta::PixelDelta(pos) => (pos.y / self.char_h.max(1.0)).round() as i32,
                };
                if lines == 0 { return; }
                if self.alt_active && !self.modes.mouse_report {
                    // In the alternate screen (pager / editor) forward wheel
                    // notches as arrow keys so `less`/`man` scroll naturally.
                    let seq: &[u8] = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
                    let reps = lines.unsigned_abs() as usize;
                    let mut out = Vec::new();
                    for _ in 0..reps { out.extend_from_slice(seq); }
                    self.write_input(&out);
                } else if !self.alt_active {
                    if lines > 0 { self.scroll_up(lines as usize); } else { self.scroll_down((-lines) as usize); }
                }
                *handled = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Always cache pointer position so the next MouseInput::Pressed
                // event can hit-test without needing the position from
                // winit (Pressed arrives without coordinates).
                let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor_pos = logical;
                if self.dragging && self.selection.is_some() {
                    // Compute the new cursor first (only needs `&self`),
                    // then take a mutable borrow to write it.
                    let pos = self.pos_from_logical(logical);
                    let sel = self.selection.as_mut().expect("just checked is_some");
                    if pos != sel.cursor {
                        sel.cursor = pos;
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
                // Mouse reporting (ESC[?1000h family) is parsed into `modes`
                // but click forwarding is intentionally a stub — v1 always
                // captures the left button for text selection.
                match element_state {
                    ElementState::Pressed if self.rect.contains(self.last_cursor_pos) => {
                        let pos = self.pos_from_logical(self.last_cursor_pos);
                        self.selection = Some(Selection { anchor: pos, cursor: pos });
                        self.dragging = true;
                        *handled = true;
                    }
                    ElementState::Released => {
                        if self.dragging {
                            self.dragging = false;
                            // Drop collapsed selections (a lone click should
                            // not paint a one-cell highlight).
                            if let Some(sel) = self.selection {
                                if sel.is_collapsed() {
                                    self.selection = None;
                                }
                            }
                            *handled = true;
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, text, state: element_state, .. }, .. } => {
                if *element_state == ElementState::Pressed {
                    let mods = modifiers.state();
                    // Ctrl+Shift+C → clipboard copy; Ctrl+Shift+V → paste.
                    // Both must be matched BEFORE the regular Ctrl+C → SIGINT
                    // path or the user would lose the ability to interrupt.
                    if mods.control_key() && mods.shift_key() {
                        if let Key::Character(c) = logical_key {
                            if c.as_str().eq_ignore_ascii_case("c") {
                                if let Some(text) = self.selected_text() {
                                    if !text.is_empty() {
                                        clipboard_copy(&text);
                                    }
                                }
                                *handled = true;
                                return;
                            }
                            if c.as_str().eq_ignore_ascii_case("v") {
                                if let Some(text) = clipboard_paste() {
                                    self.paste(&text);
                                }
                                *handled = true;
                                return;
                            }
                        }
                    }

                    // Scrollback navigation — only intercepted for the main
                    // buffer; the alternate screen (pager / editor) gets the
                    // keys forwarded to the app instead.
                    if !self.alt_active {
                        if let Key::Named(named) = logical_key {
                            let page = self.current_visible_rows().saturating_sub(1).max(1);
                            match named {
                                NamedKey::PageUp => { self.scroll_up(page); *handled = true; return; }
                                NamedKey::PageDown => { self.scroll_down(page); *handled = true; return; }
                                NamedKey::Home if mods.shift_key() => { self.scroll_to_top(); *handled = true; return; }
                                NamedKey::End if mods.shift_key() => { self.scroll_to_bottom(); *handled = true; return; }
                                _ => {}
                            }
                        }
                    }

                    let mut bytes = self.map_key(logical_key, modifiers);
                    if bytes.is_none() {
                        if let Some(t) = text {
                            if !t.as_str().chars().any(|c| c as u32 == 0x23CE || c as u32 == 0x21B5) {
                                bytes = Some(t.as_str().as_bytes().to_vec());
                            }
                        }
                    }
                    if let Some(b) = bytes {
                        {
                            let mut w = self.pty_write.lock().unwrap();
                            let _ = w.write_all(&b);
                            let _ = w.flush();
                        }
                        // Typing snaps the viewport back to the live bottom.
                        self.scroll_offset = 0;
                        *handled = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "Terminal" }
    fn get_rect(&self) -> Rect { self.rect }
    fn is_focusable(&self) -> bool { true }
}

impl TerminalWidget {
    #[allow(clippy::too_many_arguments)]
    fn draw_run(
        &self,
        scene: &mut Scene,
        text: &str,
        attr: Attr,
        py: f64,
        rx: f64,
        text_renderer: &mut TextRenderer,
        transform: Affine,
        font_size: f32,
        palette: &ui::tokens::TerminalPalette,
    ) {
        let (fg, bg_opt) = resolve_pair(attr, palette);
        let tx = self.rect.x0 + rx;

        let mut visual_width = 0.0;
        for c in text.chars() {
            visual_width += c.width().unwrap_or(1) as f64 * self.char_w;
        }

        if let Some(bg_color) = bg_opt {
            let bg_rect = Rect::new(tx, py, tx + visual_width + 0.5, py + self.char_h + 0.5);
            scene.fill(vello::peniko::Fill::NonZero, transform, bg_color, None, &bg_rect);
        }

        let weight = if attr.bold { TextWeight::Bold } else { TextWeight::Regular };
        text_renderer.draw_text_styled(
            scene,
            text,
            font_size,
            fg,
            transform * Affine::translate((tx, py)),
            None,
            weight,
            attr.italic,
            true, // monospace family — a terminal must render fixed-width
        );

        if attr.underline {
            let uy = py + self.char_h - 1.5;
            let line = Rect::new(tx, uy, tx + visual_width, uy + 1.0);
            scene.fill(vello::peniko::Fill::NonZero, transform, fg, None, &line);
        }
    }

    /// Convert a logical pointer position into a viewport-local
    /// `GridPos`. Coordinates outside the cell grid are clamped to the
    /// nearest valid cell — this matches xterm's behaviour during a
    /// drag-select that leaves the terminal area.
    fn pos_from_logical(&self, p: Point) -> GridPos {
        // Match the padding used in `draw` so a click on column 0 lands
        // on column 0 and not -1 due to the leading gutter.
        let padding_x = self.last_padding_x();
        let local_x = (p.x - self.rect.x0 - padding_x).max(0.0);
        let local_y = (p.y - self.rect.y0).max(0.0);
        let col = (local_x / self.char_w.max(1.0)).floor() as usize;
        let row = (local_y / self.char_h.max(1.0)).floor() as usize;
        // `width` is logical grid width; `viewport` height is computed
        // from `rect.height() / char_h` rather than `self.height` because
        // resize-clamp may have a stale `self.height` between frames.
        let visible_rows = (self.rect.height() / self.char_h.max(1.0)).floor() as usize;
        GridPos {
            row: row.min(visible_rows.saturating_sub(1)),
            col: col.min(self.width.saturating_sub(1)),
        }
    }

    /// Best-effort recovery of the padding used in the last `draw`. The
    /// theme isn't available in `handle_event`, so we approximate with
    /// the same default the OS ships (`spacing.s1` = 4px). If the theme
    /// changes the value, the worst-case is a one-cell hit-test offset
    /// at the leftmost column until the next frame syncs the value
    /// through some shared state — acceptable for v1.
    fn last_padding_x(&self) -> f64 {
        4.0
    }

    /// Walk the selected cells across grid rows and return the joined
    /// text, with trailing whitespace stripped from each row (standard
    /// terminal-copy semantics). Returns `None` if there is no selection
    /// or the selection covers no rows.
    fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        let spans = selection_row_spans(&sel, self.width);
        if spans.is_empty() { return None; }

        let mut out = String::new();
        for (i, (row, start_col, end_col)) in spans.iter().enumerate() {
            if i > 0 { out.push('\n'); }
            // Translate viewport-local row to absolute grid index.
            let abs_row = self.viewport_start_y + row;
            let line = self.grid.get(abs_row).map(String::as_str).unwrap_or("");

            // Walk chars and pick out the cells covered by [start_col, end_col).
            let mut cell_idx = 0usize;
            let mut row_text = String::new();
            for c in line.chars() {
                let w = c.width().unwrap_or(1);
                if cell_idx >= *end_col { break; }
                if cell_idx >= *start_col {
                    row_text.push(c);
                }
                cell_idx += w;
            }
            // Strip trailing whitespace per row — matches xterm / kitty
            // / GNOME-Terminal copy semantics so empty cells at the
            // right edge don't leak into the clipboard.
            let trimmed = row_text.trim_end();
            out.push_str(trimmed);
        }
        Some(out)
    }

    fn map_key(&self, key: &Key, modifiers: &winit::event::Modifiers) -> Option<Vec<u8>> {
        let mods = modifiers.state();
        // DECCKM: application cursor keys send SS3-prefixed sequences.
        let cursor_prefix: &[u8] = if self.modes.app_cursor_keys { b"\x1bO" } else { b"\x1b[" };
        let cursor_seq = |final_byte: u8| {
            let mut v = cursor_prefix.to_vec();
            v.push(final_byte);
            v
        };
        match key {
            Key::Named(named) => match named {
                NamedKey::Enter => Some(vec![b'\r']),
                NamedKey::Backspace => Some(vec![127]),
                NamedKey::Tab => Some(vec![b'\t']),
                NamedKey::Escape => Some(vec![27]),
                NamedKey::ArrowUp => Some(cursor_seq(b'A')),
                NamedKey::ArrowDown => Some(cursor_seq(b'B')),
                NamedKey::ArrowRight => Some(cursor_seq(b'C')),
                NamedKey::ArrowLeft => Some(cursor_seq(b'D')),
                NamedKey::Home => Some(cursor_seq(b'H')),
                NamedKey::End => Some(cursor_seq(b'F')),
                NamedKey::PageUp => Some(vec![27, b'[', b'5', b'~']),
                NamedKey::PageDown => Some(vec![27, b'[', b'6', b'~']),
                NamedKey::Insert => Some(vec![27, b'[', b'2', b'~']),
                NamedKey::Delete => Some(vec![27, b'[', b'3', b'~']),
                _ => None,
            },
            Key::Character(c) if mods.control_key() => {
                // Ctrl+letter → the corresponding C0 control code.
                let s = c.as_str();
                let ch = s.chars().next()?;
                let up = ch.to_ascii_uppercase();
                match up {
                    'A'..='Z' => Some(vec![(up as u8) - b'A' + 1]),
                    // Ctrl+Space → NUL, common shell binding.
                    ' ' => Some(vec![0]),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

pub struct TerminalPerformer<'a> {
    pub state: &'a mut AutoCommit,
    pub main_grid_id: ObjId,
    pub alt_grid_id: ObjId,
    pub cursor_id: ObjId,
    pub main_line_ids: &'a mut Vec<ObjId>,
    pub alt_line_ids: &'a mut Vec<ObjId>,
    pub alt_active: &'a mut bool,
    pub width: usize,
    pub height: usize,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub current_attr: Attr,
    pub pending_wrap: bool,
    pub modes: &'a mut TermModes,
    pub saved_cursor: &'a mut Option<(usize, usize, Attr)>,
    pub scroll_top: usize,
    pub scroll_bottom: usize,
}

impl<'a> Perform for TerminalPerformer<'a> {
    fn print(&mut self, c: char) {
        let char_w = c.width().unwrap_or(1);
        if char_w == 0 { return; }

        if self.pending_wrap {
            self.cursor_x = 0;
            self.newline();
            self.pending_wrap = false;
        }

        let y = self.absolute_y().min(self.active_len().saturating_sub(1));
        let line_obj = match self.active_line_id(y) { Some(o) => o, None => return };
        if let Ok(Some((Value::Object(ObjType::Text), text_id))) = self.state.get(&line_obj, "text") {
            let current_len = self.state.length(&text_id);
            let replacement = format!("{}{}", c, if char_w > 1 { " ".repeat(char_w - 1) } else { "".into() });

            if self.cursor_x < current_len {
                let _ = self.state.splice_text(&text_id, self.cursor_x, char_w as isize, &replacement);
            } else {
                let _ = self.state.splice_text(&text_id, self.cursor_x, 0, &replacement);
            }

            if let Ok(Some((Value::Scalar(v), _))) = self.state.get(&line_obj, "attrs") {
                if let automerge::ScalarValue::Bytes(bytes) = v.as_ref() {
                    let mut attr_bytes = bytes.clone();
                    let start = self.cursor_x * ATTR_BYTES;
                    let end = (self.cursor_x + char_w) * ATTR_BYTES;
                    if attr_bytes.len() < end { attr_bytes.resize(end, 0); }

                    attr_bytes[start..start + ATTR_BYTES].copy_from_slice(&self.current_attr.pack());
                    if char_w > 1 {
                        let mut cont = self.current_attr;
                        cont.is_wide_cont = true;
                        let cont_packed = cont.pack();
                        for i in 1..char_w {
                            let s = (self.cursor_x + i) * ATTR_BYTES;
                            if s + ATTR_BYTES <= attr_bytes.len() { attr_bytes[s..s + ATTR_BYTES].copy_from_slice(&cont_packed); }
                        }
                    }
                    let _ = self.state.put(&line_obj, "attrs", attr_bytes);
                }
            }

            self.cursor_x += char_w;
            if self.cursor_x >= self.width {
                self.pending_wrap = true;
                self.cursor_x = self.width - 1;
            }
        }
        self.update_cursor();
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0B | 0x0C => { self.pending_wrap = false; self.newline(); }
            b'\r' => { self.pending_wrap = false; self.cursor_x = 0; }
            8 => { self.pending_wrap = false; if self.cursor_x > 0 { self.cursor_x -= 1; } }
            b'\t' => {
                self.pending_wrap = false;
                let next = ((self.cursor_x / 8) + 1) * 8;
                self.cursor_x = next.min(self.width.saturating_sub(1));
            }
            7 => {} // BEL — no audible bell in the toolkit yet.
            _ => {}
        }
        self.update_cursor();
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => { *self.saved_cursor = Some((self.cursor_x, self.cursor_y, self.current_attr)); } // DECSC
            b'8' => { // DECRC
                if let Some((x, y, a)) = *self.saved_cursor {
                    self.cursor_x = x.min(self.width.saturating_sub(1));
                    self.cursor_y = y.min(self.height.saturating_sub(1));
                    self.current_attr = a;
                }
            }
            b'D' => { self.pending_wrap = false; self.newline(); } // IND — index (line feed)
            b'E' => { self.pending_wrap = false; self.cursor_x = 0; self.newline(); } // NEL
            b'M' => { // RI — reverse index
                self.pending_wrap = false;
                if self.cursor_y == self.scroll_top {
                    self.scroll_region_down(1);
                } else if self.cursor_y > 0 {
                    self.cursor_y -= 1;
                }
            }
            _ => {}
        }
        self.update_cursor();
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, c: char) {
        self.pending_wrap = false;
        let private = intermediates.first() == Some(&b'?');
        let first = |p: &vte::Params| p.iter().next().and_then(|x| x.first()).copied().unwrap_or(0);
        let first_or1 = |p: &vte::Params| {
            let v = first(p);
            if v == 0 { 1 } else { v as usize }
        };

        if private {
            match c {
                'h' => self.set_dec_modes(params, true),
                'l' => self.set_dec_modes(params, false),
                _ => {}
            }
            self.update_cursor();
            return;
        }

        match c {
            'A' => { let n = first_or1(params); self.cursor_y = self.cursor_y.saturating_sub(n); }
            'B' | 'e' => { let n = first_or1(params); self.cursor_y = (self.cursor_y + n).min(self.height - 1); }
            'C' | 'a' => { let n = first_or1(params); self.cursor_x = (self.cursor_x + n).min(self.width - 1); }
            'D' => { let n = first_or1(params); self.cursor_x = self.cursor_x.saturating_sub(n); }
            'G' | '`' => { let n = first_or1(params); self.cursor_x = (n - 1).min(self.width - 1); } // CHA
            'd' => { let n = first_or1(params); self.cursor_y = (n - 1).min(self.height - 1); } // VPA
            'H' | 'f' => {
                let mut it = params.iter();
                let y = it.next().and_then(|p| p.first()).cloned().unwrap_or(1) as usize;
                let x = it.next().and_then(|p| p.first()).cloned().unwrap_or(1) as usize;
                self.cursor_y = (y.saturating_sub(1)).min(self.height - 1);
                self.cursor_x = (x.saturating_sub(1)).min(self.width - 1);
            }
            'J' => self.erase_display(first(params)),
            'K' => self.erase_line(first(params)),
            'X' => self.erase_chars(first_or1(params)), // ECH
            '@' => self.insert_chars(first_or1(params)), // ICH
            'P' => self.delete_chars(first_or1(params)), // DCH
            'L' => self.insert_lines(first_or1(params)), // IL
            'M' => self.delete_lines(first_or1(params)), // DL
            'S' => self.scroll_region_up(first_or1(params)), // SU
            'T' => self.scroll_region_down(first_or1(params)), // SD
            'r' => { // DECSTBM — set scrolling region
                let mut it = params.iter();
                let top = it.next().and_then(|p| p.first()).cloned().unwrap_or(1) as usize;
                let bot = it.next().and_then(|p| p.first()).cloned().unwrap_or(self.height as u16 as u16) as usize;
                let top0 = top.saturating_sub(1).min(self.height.saturating_sub(1));
                let bot0 = if bot == 0 { self.height - 1 } else { (bot - 1).min(self.height - 1) };
                if top0 < bot0 {
                    self.scroll_top = top0;
                    self.scroll_bottom = bot0;
                }
                self.cursor_x = 0;
                self.cursor_y = 0;
            }
            's' => { *self.saved_cursor = Some((self.cursor_x, self.cursor_y, self.current_attr)); } // SCP
            'u' => { // RCP
                if let Some((x, y, a)) = *self.saved_cursor {
                    self.cursor_x = x.min(self.width - 1);
                    self.cursor_y = y.min(self.height - 1);
                    self.current_attr = a;
                }
            }
            'm' => self.apply_sgr(params),
            _ => {}
        }
        self.update_cursor();
    }
}

impl<'a> TerminalPerformer<'a> {
    fn active_grid_id(&self) -> ObjId {
        if *self.alt_active { self.alt_grid_id.clone() } else { self.main_grid_id.clone() }
    }
    fn active_len(&self) -> usize {
        if *self.alt_active { self.alt_line_ids.len() } else { self.main_line_ids.len() }
    }
    fn active_line_id(&self, idx: usize) -> Option<ObjId> {
        let ids: &Vec<ObjId> = if *self.alt_active { self.alt_line_ids } else { self.main_line_ids };
        ids.get(idx).cloned()
    }
    fn active_line_ids_mut(&mut self) -> &mut Vec<ObjId> {
        if *self.alt_active { self.alt_line_ids } else { self.main_line_ids }
    }

    /// Insert a fresh blank line object into the active grid at `idx`,
    /// returning its id. Does not touch `line_ids`.
    fn make_blank_line(&mut self, idx: usize) -> ObjId {
        let grid_id = self.active_grid_id();
        let line_id = self.state.insert_object(&grid_id, idx, ObjType::Map).unwrap();
        let text_id = self.state.put_object(&line_id, "text", ObjType::Text).unwrap();
        let _ = self.state.splice_text(&text_id, 0, 0, &" ".repeat(self.width));
        let attr_bytes: Vec<u8> = vec![Attr::default().pack(); self.width].concat();
        let _ = self.state.put(&line_id, "attrs", attr_bytes);
        line_id
    }

    fn newline(&mut self) {
        self.cursor_y += 1;
        if self.cursor_y < self.height {
            return;
        }
        self.cursor_y = self.height - 1;

        if *self.alt_active {
            // Fixed-height alternate buffer: scroll the region up in place.
            self.scroll_region_up(1);
        } else {
            // Main buffer accumulates scrollback.
            let grid_id = self.main_grid_id.clone();
            let idx = self.main_line_ids.len();
            let line_id = self.state.insert_object(&grid_id, idx, ObjType::Map).unwrap();
            let text_id = self.state.put_object(&line_id, "text", ObjType::Text).unwrap();
            let _ = self.state.splice_text(&text_id, 0, 0, &" ".repeat(self.width));
            let attr_bytes: Vec<u8> = vec![Attr::default().pack(); self.width].concat();
            let _ = self.state.put(&line_id, "attrs", attr_bytes);
            self.main_line_ids.push(line_id);

            if self.main_line_ids.len() > MAX_HISTORY {
                let overflow = self.main_line_ids.len() - MAX_HISTORY;
                for _ in 0..overflow {
                    let _ = self.state.delete(&grid_id, 0);
                    self.main_line_ids.remove(0);
                }
            }
        }
    }

    /// Scroll the current scrolling region up by `n` lines (content moves up;
    /// blank lines appear at the bottom of the region). Honours the DEC
    /// scrolling margins.
    fn scroll_region_up(&mut self, n: usize) {
        let len = self.active_len();
        if len == 0 { return; }
        let top = self.scroll_top.min(len - 1);
        let bot = self.scroll_bottom.min(len - 1);
        if top > bot { return; }
        let n = n.min(bot - top + 1);
        for _ in 0..n {
            let grid_id = self.active_grid_id();
            let _ = self.state.delete(&grid_id, top);
            self.active_line_ids_mut().remove(top);
            let line = self.make_blank_line(bot);
            self.active_line_ids_mut().insert(bot, line);
        }
    }

    /// Scroll the current scrolling region down by `n` lines (content moves
    /// down; blank lines appear at the top of the region).
    fn scroll_region_down(&mut self, n: usize) {
        let len = self.active_len();
        if len == 0 { return; }
        let top = self.scroll_top.min(len - 1);
        let bot = self.scroll_bottom.min(len - 1);
        if top > bot { return; }
        let n = n.min(bot - top + 1);
        for _ in 0..n {
            let grid_id = self.active_grid_id();
            let _ = self.state.delete(&grid_id, bot);
            self.active_line_ids_mut().remove(bot);
            let line = self.make_blank_line(top);
            self.active_line_ids_mut().insert(top, line);
        }
    }

    /// IL — insert `n` blank lines at the cursor row, within the region.
    fn insert_lines(&mut self, n: usize) {
        let len = self.active_len();
        if len == 0 { return; }
        let at = self.cursor_y.max(self.scroll_top);
        let bot = self.scroll_bottom.min(len - 1);
        if at > bot { return; }
        let n = n.min(bot - at + 1);
        for _ in 0..n {
            let grid_id = self.active_grid_id();
            let _ = self.state.delete(&grid_id, bot);
            self.active_line_ids_mut().remove(bot);
            let line = self.make_blank_line(at);
            self.active_line_ids_mut().insert(at, line);
        }
        self.cursor_x = 0;
    }

    /// DL — delete `n` lines at the cursor row, within the region.
    fn delete_lines(&mut self, n: usize) {
        let len = self.active_len();
        if len == 0 { return; }
        let at = self.cursor_y.max(self.scroll_top);
        let bot = self.scroll_bottom.min(len - 1);
        if at > bot { return; }
        let n = n.min(bot - at + 1);
        for _ in 0..n {
            let grid_id = self.active_grid_id();
            let _ = self.state.delete(&grid_id, at);
            self.active_line_ids_mut().remove(at);
            let line = self.make_blank_line(bot);
            self.active_line_ids_mut().insert(bot, line);
        }
        self.cursor_x = 0;
    }

    fn erase_display(&mut self, mode: u16) {
        let len = self.active_len();
        if len == 0 { return; }
        let screen_top = len.saturating_sub(self.height);
        let range: Vec<usize> = match mode {
            0 => (self.absolute_y()..len).collect(),         // cursor → end
            1 => (screen_top..=self.absolute_y().min(len - 1)).collect(), // start → cursor
            2 | 3 => (screen_top..len).collect(),            // whole screen
            _ => return,
        };
        for abs_y in range {
            if let Some(line_obj) = self.active_line_id(abs_y) {
                if let Ok(Some((Value::Object(ObjType::Text), text_id))) = self.state.get(&line_obj, "text") {
                    let l = self.state.length(&text_id);
                    let _ = self.state.splice_text(&text_id, 0, l as isize, &" ".repeat(self.width));
                }
                let attr_bytes: Vec<u8> = vec![Attr::default().pack(); self.width].concat();
                let _ = self.state.put(&line_obj, "attrs", attr_bytes);
            }
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let y = self.absolute_y().min(self.active_len().saturating_sub(1));
        let line_obj = match self.active_line_id(y) { Some(o) => o, None => return };
        if let Ok(Some((Value::Object(ObjType::Text), text_id))) = self.state.get(&line_obj, "text") {
            let len = self.state.length(&text_id);
            let (from, count): (usize, usize) = match mode {
                0 => (self.cursor_x, len.saturating_sub(self.cursor_x)), // cursor → end
                1 => (0, self.cursor_x.min(len)),                        // start → cursor
                2 => (0, len),                                           // whole line
                _ => return,
            };
            if count > 0 {
                let _ = self.state.splice_text(&text_id, from, count as isize, &" ".repeat(count));
            }
            self.reset_attr_range(&line_obj, from, count);
        }
    }

    fn erase_chars(&mut self, n: usize) {
        let y = self.absolute_y().min(self.active_len().saturating_sub(1));
        let line_obj = match self.active_line_id(y) { Some(o) => o, None => return };
        if let Ok(Some((Value::Object(ObjType::Text), text_id))) = self.state.get(&line_obj, "text") {
            let len = self.state.length(&text_id);
            let count = n.min(len.saturating_sub(self.cursor_x));
            if count > 0 {
                let _ = self.state.splice_text(&text_id, self.cursor_x, count as isize, &" ".repeat(count));
                self.reset_attr_range(&line_obj, self.cursor_x, count);
            }
        }
    }

    fn insert_chars(&mut self, n: usize) {
        let y = self.absolute_y().min(self.active_len().saturating_sub(1));
        let line_obj = match self.active_line_id(y) { Some(o) => o, None => return };
        if let Ok(Some((Value::Object(ObjType::Text), text_id))) = self.state.get(&line_obj, "text") {
            let n = n.min(self.width.saturating_sub(self.cursor_x));
            if n == 0 { return; }
            let _ = self.state.splice_text(&text_id, self.cursor_x, 0, &" ".repeat(n));
            // Truncate back to the grid width.
            let len = self.state.length(&text_id);
            if len > self.width {
                let _ = self.state.splice_text(&text_id, self.width, (len - self.width) as isize, "");
            }
        }
    }

    fn delete_chars(&mut self, n: usize) {
        let y = self.absolute_y().min(self.active_len().saturating_sub(1));
        let line_obj = match self.active_line_id(y) { Some(o) => o, None => return };
        if let Ok(Some((Value::Object(ObjType::Text), text_id))) = self.state.get(&line_obj, "text") {
            let len = self.state.length(&text_id);
            let count = n.min(len.saturating_sub(self.cursor_x));
            if count == 0 { return; }
            let _ = self.state.splice_text(&text_id, self.cursor_x, count as isize, "");
            // Pad blanks at the end to restore the width.
            let new_len = self.state.length(&text_id);
            if new_len < self.width {
                let _ = self.state.splice_text(&text_id, new_len, 0, &" ".repeat(self.width - new_len));
            }
        }
    }

    /// Reset the packed attrs for cells `[from, from+count)` to default.
    fn reset_attr_range(&mut self, line_obj: &ObjId, from: usize, count: usize) {
        if count == 0 { return; }
        if let Ok(Some((Value::Scalar(v), _))) = self.state.get(line_obj, "attrs") {
            if let automerge::ScalarValue::Bytes(bytes) = v.as_ref() {
                let mut attr_bytes = bytes.clone();
                let empty = Attr::default().pack();
                for i in from..(from + count) {
                    let s = i * ATTR_BYTES;
                    if s + ATTR_BYTES <= attr_bytes.len() {
                        attr_bytes[s..s + ATTR_BYTES].copy_from_slice(&empty);
                    }
                }
                let _ = self.state.put(line_obj, "attrs", attr_bytes);
            }
        }
    }

    /// DEC private set/reset — `CSI ? Ps h` / `CSI ? Ps l`.
    fn set_dec_modes(&mut self, params: &vte::Params, enable: bool) {
        for p in params {
            let Some(&n) = p.first() else { continue };
            match n {
                1 => self.modes.app_cursor_keys = enable,
                25 => self.modes.cursor_visible = enable,
                1000 | 1002 | 1003 => self.modes.mouse_report = enable,
                1006 => self.modes.mouse_sgr = enable,
                2004 => self.modes.bracketed_paste = enable,
                1048 => {
                    if enable {
                        *self.saved_cursor = Some((self.cursor_x, self.cursor_y, self.current_attr));
                    } else if let Some((x, y, a)) = *self.saved_cursor {
                        self.cursor_x = x.min(self.width - 1);
                        self.cursor_y = y.min(self.height - 1);
                        self.current_attr = a;
                    }
                }
                47 | 1047 | 1049 => {
                    if enable { self.enter_alt(n == 1049); } else { self.exit_alt(n == 1049); }
                }
                _ => {}
            }
        }
    }

    fn enter_alt(&mut self, save_cursor: bool) {
        if *self.alt_active { return; }
        if save_cursor {
            *self.saved_cursor = Some((self.cursor_x, self.cursor_y, self.current_attr));
        }
        *self.alt_active = true;
        // Present a cleared alternate screen and home the cursor.
        self.clear_alt_buffer();
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.pending_wrap = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.height.saturating_sub(1);
    }

    fn exit_alt(&mut self, restore_cursor: bool) {
        if !*self.alt_active { return; }
        *self.alt_active = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.height.saturating_sub(1);
        if restore_cursor {
            if let Some((x, y, a)) = self.saved_cursor.take() {
                self.cursor_x = x.min(self.width.saturating_sub(1));
                self.cursor_y = y.min(self.height.saturating_sub(1));
                self.current_attr = a;
            }
        }
        self.pending_wrap = false;
    }

    /// Blank every line of the alternate buffer and trim it back to exactly
    /// `height` lines (a previous alt session may have left it longer).
    fn clear_alt_buffer(&mut self) {
        let width = self.width;
        let ids: Vec<ObjId> = self.alt_line_ids.clone();
        for id in &ids {
            if let Ok(Some((Value::Object(ObjType::Text), tid))) = self.state.get(id, "text") {
                let l = self.state.length(&tid);
                let _ = self.state.splice_text(&tid, 0, l as isize, &" ".repeat(width));
            }
            let attr_bytes: Vec<u8> = vec![Attr::default().pack(); width].concat();
            let _ = self.state.put(id, "attrs", attr_bytes);
        }
        while self.alt_line_ids.len() > self.height {
            let idx = self.alt_line_ids.len() - 1;
            let _ = self.state.delete(&self.alt_grid_id, idx);
            self.alt_line_ids.pop();
        }
    }

    /// SGR — `CSI Ps... m`. Handles reset, the four style bits, the 8+8 ANSI
    /// slots, and both truecolor forms (`38;2;r;g;b`, `38:2::r:g:b`) plus
    /// 256-indexed (`38;5;n`).
    fn apply_sgr(&mut self, params: &vte::Params) {
        // Flatten into groups so we can look across `;`-separated params for
        // the extended-color introducers while still handling `:`-subparam
        // colon form within a single group.
        let groups: Vec<Vec<u16>> = params.iter().map(|p| p.to_vec()).collect();
        if groups.is_empty() {
            self.current_attr = Attr::default();
            return;
        }
        let mut i = 0;
        while i < groups.len() {
            let group = &groups[i];
            let code = group.first().copied().unwrap_or(0);
            match code {
                0 => self.current_attr = Attr::default(),
                1 => self.current_attr.bold = true,
                3 => self.current_attr.italic = true,
                4 => self.current_attr.underline = true,
                7 => self.current_attr.reverse = true,
                22 => self.current_attr.bold = false,
                23 => self.current_attr.italic = false,
                24 => self.current_attr.underline = false,
                27 => self.current_attr.reverse = false,
                30..=37 => self.current_attr.fg = CellColor::Indexed((code - 30) as u8),
                39 => self.current_attr.fg = CellColor::Default,
                40..=47 => self.current_attr.bg = CellColor::Indexed((code - 40) as u8),
                49 => self.current_attr.bg = CellColor::Default,
                90..=97 => self.current_attr.fg = CellColor::Indexed((code - 90 + 8) as u8),
                100..=107 => self.current_attr.bg = CellColor::Indexed((code - 100 + 8) as u8),
                38 | 48 => {
                    let is_fg = code == 38;
                    if group.len() >= 2 {
                        // Colon subparam form: `38:2:..:r:g:b` or `38:5:n`.
                        if let Some(col) = color_from_subparams(&group[1..]) {
                            if is_fg { self.current_attr.fg = col; } else { self.current_attr.bg = col; }
                        }
                    } else {
                        // Semicolon form: pull the following groups.
                        let mode = groups.get(i + 1).and_then(|g| g.first()).copied();
                        match mode {
                            Some(2) => {
                                let r = groups.get(i + 2).and_then(|g| g.first()).copied().unwrap_or(0) as u8;
                                let g = groups.get(i + 3).and_then(|g| g.first()).copied().unwrap_or(0) as u8;
                                let b = groups.get(i + 4).and_then(|g| g.first()).copied().unwrap_or(0) as u8;
                                let col = CellColor::Rgb(r, g, b);
                                if is_fg { self.current_attr.fg = col; } else { self.current_attr.bg = col; }
                                i += 4;
                            }
                            Some(5) => {
                                let n = groups.get(i + 2).and_then(|g| g.first()).copied().unwrap_or(0) as u8;
                                let col = CellColor::Indexed(n);
                                if is_fg { self.current_attr.fg = col; } else { self.current_attr.bg = col; }
                                i += 2;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn absolute_y(&self) -> usize {
        let history_offset = self.active_len().saturating_sub(self.height);
        history_offset + self.cursor_y
    }

    fn update_cursor(&mut self) {
        let x = self.cursor_x as u64;
        let y = self.absolute_y() as u64;
        let _ = self.state.put(&self.cursor_id, "x", x);
        let _ = self.state.put(&self.cursor_id, "y", y);
    }
}

/// Decode a truecolor / indexed color from the subparams following a `38`
/// or `48` introducer (colon form). `sub[0]` is the color space id (2 = RGB,
/// 5 = indexed); RGB takes the last three values so an optional colorspace
/// placeholder (`38:2::r:g:b`) is tolerated.
fn color_from_subparams(sub: &[u16]) -> Option<CellColor> {
    match sub.first().copied() {
        Some(2) => {
            let n = sub.len();
            if n >= 4 {
                Some(CellColor::Rgb(sub[n - 3] as u8, sub[n - 2] as u8, sub[n - 1] as u8))
            } else if n >= 2 {
                // `38:2:r` style with too few components — treat what we have.
                Some(CellColor::Rgb(sub.get(1).copied().unwrap_or(0) as u8, 0, 0))
            } else {
                None
            }
        }
        Some(5) => sub.get(1).map(|&n| CellColor::Indexed(n as u8)),
        _ => None,
    }
}

/// Decompose a flow-selection into per-row pixel spans, in display
/// order. Each span is `(viewport_row, start_col, end_col)` with
/// `end_col` exclusive (half-open) so single-row selections of zero
/// length collapse to nothing.
///
/// Layout matches standard terminal-emulator flow semantics:
/// - Single-row: one span from `start_col` to `end_col`.
/// - Multi-row: first row goes `start_col..cols`, middle rows go
///   `0..cols`, last row goes `0..end_col`.
pub(crate) fn selection_row_spans(sel: &Selection, cols: usize) -> Vec<(usize, usize, usize)> {
    let (start, end) = sel.ordered();
    if start == end {
        // Collapsed selection produces no spans — a lone click clears
        // the selection on release, but if a `selection_row_spans` call
        // sneaks in beforehand we should produce nothing.
        return Vec::new();
    }
    let mut spans = Vec::new();
    if start.row == end.row {
        spans.push((start.row, start.col, end.col));
    } else {
        spans.push((start.row, start.col, cols));
        for r in (start.row + 1)..end.row {
            spans.push((r, 0, cols));
        }
        spans.push((end.row, 0, end.col));
    }
    spans
}

pub(crate) fn map_ansi_256(code: u8, palette: &ui::tokens::TerminalPalette) -> Color {
    if code < 16 {
        palette.ansi[code as usize]
    } else if code < 232 {
        // 6x6x6 cube colors are deterministic ANSI math, not a themable
        // surface — the cube is defined by the ANSI 256 spec, not by the OS
        // theme. The 16 "anchor" colors above come from the palette, so the
        // rest of the surface still flexes with the theme.
        let n = code - 16;
        let r = (n / 36) * 51;
        let g = ((n % 36) / 6) * 51;
        let b = (n % 6) * 51;
        Color::from_rgb8(r, g, b)
    } else {
        let gray = (code - 232) * 10 + 8;
        Color::from_rgb8(gray, gray, gray)
    }
}

/// Resolve a `CellColor` to a concrete `Color`. Bold brightens the low 8
/// ANSI foreground slots (xterm behaviour).
pub(crate) fn resolve_color(c: CellColor, palette: &ui::tokens::TerminalPalette, is_fg: bool, bold: bool) -> Color {
    match c {
        CellColor::Default => if is_fg { palette.fg_default } else { palette.bg_default },
        CellColor::Indexed(mut n) => {
            if bold && is_fg && n < 8 { n += 8; }
            map_ansi_256(n, palette)
        }
        CellColor::Rgb(r, g, b) => Color::from_rgb8(r, g, b),
    }
}

/// Resolve a cell's `(foreground, optional background)` colors, applying
/// reverse video. A `None` background means "leave the default terminal
/// background showing" (no fill).
pub(crate) fn resolve_pair(attr: Attr, palette: &ui::tokens::TerminalPalette) -> (Color, Option<Color>) {
    let fg = resolve_color(attr.fg, palette, true, attr.bold);
    let bg = match attr.bg {
        CellColor::Default => None,
        other => Some(resolve_color(other, palette, false, false)),
    };
    if attr.reverse {
        // Swap: the foreground color fills the cell; the (explicit) bg
        // color, or the default terminal bg, paints the text.
        let text = bg.unwrap_or(palette.bg_default);
        (text, Some(fg))
    } else {
        (fg, bg)
    }
}

/// Derive the block-cursor color from the palette's default foreground.
/// Kept as a free function so the rule "the cursor always tracks the theme"
/// is a single obvious call site and tests can invoke it directly.
pub(crate) fn cursor_from_palette(palette: &ui::tokens::TerminalPalette) -> Color {
    // 59% alpha (150/255) preserves the legacy translucent-block feel while
    // sourcing the hue from the theme rather than a hard-coded grey.
    palette.fg_default.with_alpha_factor(150.0 / 255.0)
}

pub struct TerminalBackend { pub writer: Box<dyn Write + Send>, pub reader: Box<dyn Read + Send>, pub master: Box<dyn MasterPty + Send>, pub shell_name: String }
impl TerminalBackend {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(&shell); cmd.env("TERM", "xterm-256color");
        let _child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        Ok(Self { writer, reader, master: pair.master, shell_name: shell })
    }
}

impl TerminalWidget {
    /// Spawn a PTY + reader thread and return a fully wired `TerminalWidget`.
    /// Returns an error if PTY creation fails.
    pub fn spawn_default() -> anyhow::Result<Self> {
        let backend = TerminalBackend::new().map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let writer = std::sync::Arc::new(std::sync::Mutex::new(backend.writer));
        let master = std::sync::Arc::new(std::sync::Mutex::new(backend.master));
        let (output_tx, output_rx) = crossbeam_channel::unbounded();

        let reader = std::sync::Arc::new(std::sync::Mutex::new(backend.reader));
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                let mut r = reader.lock().unwrap();
                match std::io::Read::read(&mut *r, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => { let _ = output_tx.send(buf[..n].to_vec()); }
                    Err(_) => break,
                }
            }
        });

        Ok(TerminalWidget::new(writer, master, output_rx))
    }

    /// A terminal with a null backend: a sink writer, an empty reader, and a
    /// mock PTY master. No shell is spawned, so nothing arrives on the output
    /// channel — used by headless screenshot scenarios that drive the grid
    /// entirely through `feed`, keeping the frame deterministic.
    pub fn spawn_headless() -> anyhow::Result<Self> {
        struct NullMaster;
        impl MasterPty for NullMaster {
            fn get_size(&self) -> Result<PtySize, anyhow::Error> { Ok(PtySize::default()) }
            fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> { Ok(()) }
            fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> { Ok(Box::new(std::io::sink())) }
            fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> { Ok(Box::new(std::io::empty())) }
            fn process_group_leader(&self) -> Option<i32> { None }
            fn as_raw_fd(&self) -> Option<i32> { None }
            fn tty_name(&self) -> Option<std::path::PathBuf> { None }
        }
        let writer: Box<dyn Write + Send> = Box::new(std::io::sink());
        let (_tx, output_rx) = crossbeam_channel::unbounded();
        Ok(TerminalWidget::new(
            std::sync::Arc::new(std::sync::Mutex::new(writer)),
            std::sync::Arc::new(std::sync::Mutex::new(Box::new(NullMaster))),
            output_rx,
        ))
    }
}
