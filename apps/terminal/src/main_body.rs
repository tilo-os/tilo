use ui::ColorCompat;
use ui::widget::{AppModel, ModelPath};
use ui::{
    Theme,
    paint::PaintCtx,
    text_render::{FontChoice, TextRenderer, TextStyle, TextWeight},
    widget::{PressOutcome, ScrollMetrics, Scrollbar, TextInput, Widget},
};
// Shared text-selection + clipboard primitives from the toolkit — the terminal
// reuses these rather than re-implementing word/line boundary walking, multi-
// click gesture detection, or clipboard access.
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value, transaction::Transactable};
use base64::Engine as _;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Mutex};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use tilo_content::ContentStore;
use ui::app::UserEvent;
use ui::collaboration::PresencePayload;
use ui::icons::{Icon, draw_icon};
use ui::permissions::{AppKind as PermAppKind, PermissionContext};
use ui::presence::{PresenceCtx, PresenceKey};
use ui::tilo_actions::{ActionSpec, OperationKind, Resource, RetryPolicy};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::{
    DragSelect, Granularity, clipboard_copy, clipboard_copy_primary, clipboard_paste,
    clipboard_paste_primary, extend_span_drag, line_boundaries, word_boundaries,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Color, Fill};
use vte::{Parser, Perform};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

use crate::graphics::{
    GraphicsScanner, StreamEvent, TerminalImagePlacement, decode_for_rendering, parse_kitty,
};

/// The PTY reader applies natural backpressure once this many chunks are
/// waiting for the UI. A terminal must never silently discard process output,
/// but it also must not let a noisy process allocate without bound.
const PTY_QUEUE_CHUNKS: usize = 64;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PtyReadDisposition {
    Data(usize),
    Retry,
    Closed,
}

/// Linux PTY masters report `EIO` whenever no slave descriptor is open. For a
/// hosted `ush` session that happens briefly in the hand-off window, after the
/// master reaches the app but before the guest opens its slave, and it also
/// happens for good once the guest exits. `session_open` tells the two apart:
/// the supervisor keeps the session's lifetime socket open exactly while its
/// guest runs, so `EIO` retries only while that socket is still open. A local
/// PTY has no hand-off window, so its `EIO` always means the shell exited.
/// `EINTR` is never closure. EOF and every other error remain terminal.
pub(crate) fn classify_pty_read(
    result: &std::io::Result<usize>,
    session_open: impl FnOnce() -> bool,
) -> PtyReadDisposition {
    match result {
        Ok(0) => PtyReadDisposition::Closed,
        Ok(n) => PtyReadDisposition::Data(*n),
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => PtyReadDisposition::Retry,
        Err(error) if error.raw_os_error() == Some(libc::EIO) && session_open() => {
            PtyReadDisposition::Retry
        }
        Err(_) => PtyReadDisposition::Closed,
    }
}

/// Whether the peer still holds a hosted session's lifetime socket open. The
/// probe peeks without blocking and without changing the descriptor's flags,
/// which it shares with the widget's own handle.
#[cfg(unix)]
pub(crate) fn lifetime_socket_open(socket: &std::os::unix::net::UnixStream) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut byte = 0u8;
    // SAFETY: `byte` is valid writable storage for the one byte requested and
    // the descriptor stays open for the duration of the call.
    let received = unsafe {
        libc::recv(
            socket.as_raw_fd(),
            (&raw mut byte).cast::<libc::c_void>(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if received >= 0 {
        return received > 0;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EAGAIN | libc::EINTR)
    )
}

/// One UI turn may parse at most this much queued PTY output. Anything beyond
/// it stays in the bounded queue for the following frame, preserving ordering
/// while keeping pointer input and accessibility responsive during a build
/// flood.
const MAX_PUMP_BYTES: usize = 256 * 1024;
/// A palette/agent invocation is intentionally much smaller than local paste:
/// it is a reviewed command submission, not a bulk data transport.
pub(crate) const MAX_RESOURCE_SEND_BYTES: usize = 8 * 1024;
/// OSC is process-controlled input. Keep its metadata bounded before it can
/// enter the replicated document, clipboard, or app title.
const MAX_OSC_TITLE_BYTES: usize = 256;
const MAX_OSC_LINK_BYTES: usize = 2 * 1024;
const MAX_OSC_LINKS: usize = 4096;
const MAX_OSC_CLIPBOARD_BYTES: usize = 1024 * 1024;
/// Keep contextual resource publication proportional to what is on screen.
/// Historical OSC links remain lazily resolvable by their stable id, but a
/// malicious process cannot make every palette refresh materialise thousands
/// of children.
const MAX_VISIBLE_RESOURCE_LINKS: usize = 32;
/// Resource labels are navigation chrome, not a second copy of an arbitrarily
/// long OSC URL. The full validated target stays in the operation payload.
const MAX_LINK_RESOURCE_LABEL_CHARS: usize = 120;
/// Find is deliberately bounded separately from retained scrollback. A noisy
/// log may contain thousands of identical hits; keeping a focused review
/// surface responsive matters more than counting every occurrence forever.
const MAX_FIND_MATCHES: usize = 2_048;
/// A find string is local presentation state, but cap it before repeatedly
/// scanning history on each keystroke or pasting an accidental large blob.
const MAX_FIND_QUERY_CHARS: usize = 256;
const MAX_GESTURE_EFFECTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FindMatch {
    row: usize,
    start_col: usize,
    end_col: usize,
}

fn link_resource_label(url: &str) -> String {
    let mut label: String = url.chars().take(MAX_LINK_RESOURCE_LABEL_CHARS).collect();
    if url.chars().count() > MAX_LINK_RESOURCE_LABEL_CHARS {
        label.push('…');
    }
    label
}

/// Cell coordinate within the terminal buffer. `row` is an **absolute** index
/// into the full `grid` (scrollback included), not a viewport-local row — the
/// selection model anchors to the buffer so a selection stays on the same
/// cells when the viewport scrolls. `col` is a grid column. Rendering
/// translates absolute rows into on-screen rows via the current scrollback
/// offset (`viewport_start_y`); pointer hit-testing does the inverse.
///
/// `Ord` is derived and, because `row` is declared first, orders in reading
/// order (row then column) — which is exactly what the span-walker and
/// `extend_span_drag` rely on.
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

/// Open an OSC 8 hyperlink through the toolkit's native opener without waiting
/// for the browser. A launch failure is logged; malformed URLs are ignored.
/// Only plain public `http(s)` targets are launched. In particular, terminal
/// output never gets to turn an OSC 8 link into access to a local `file:` URL,
/// a `mailto:` draft, credentials in an authority, or a query/fragment that
/// might contain a bearer token. A user can still select and copy any text the
/// process printed; the opener is deliberately the narrower, explicit effect.
pub(crate) fn open_url(url: &str) {
    if !is_public_web_link(url) {
        return;
    }
    ui::widget::open_external_url(url);
}

/// An OSC 8 target that is safe to publish to the action directory and hand to
/// the external opener. This intentionally accepts a small URL subset instead
/// of attempting to parse and redact arbitrary process-provided metadata: a
/// redaction mistake would disclose a secret in a resource payload.
pub(crate) fn is_public_web_link(url: &str) -> bool {
    let Some(authority_and_path) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    if authority_and_path.is_empty()
        || authority_and_path.contains(['?', '#', '@', '\\'])
        || authority_and_path
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return false;
    }
    // An authority is required. A slash is optional (for example
    // `https://example.test`), but a path cannot be mistaken for one.
    authority_and_path
        .split_once('/')
        .map_or(!authority_and_path.is_empty(), |(authority, _)| {
            !authority.is_empty()
        })
}

/// Prompted resource input is text, not an escape hatch for arbitrary terminal
/// control bytes. This keeps the typed `send` promise truthful and lets `run`
/// guarantee precisely one appended Enter.
fn is_reviewable_terminal_text(value: &str) -> bool {
    !value.chars().any(|ch| ch.is_control() && ch != '\t')
}

/// Convert an OSC metadata field into a compact, single-line label. OSC is
/// emitted by the process, not entered through a trusted text field: control
/// characters would corrupt window/accessibility labels and an unlimited value
/// would become replicated metadata. Invalid UTF-8, empty, or over-limit
/// fields are deliberately ignored rather than lossy-decoded.
pub(crate) fn safe_osc_text(bytes: &[u8], max_bytes: usize) -> Option<String> {
    if bytes.is_empty() || bytes.len() > max_bytes {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    Some(text.to_string())
}

/// Record the first terminal transport failure. Later failures cannot make a
/// dead session live again, so keeping the first reason makes the persistent
/// status deterministic instead of flapping with each rejected keypress.
fn set_terminal_disconnected(
    disconnected: &AtomicBool,
    message: &Mutex<&'static str>,
    status: &'static str,
) {
    if !disconnected.swap(true, Ordering::AcqRel)
        && let Ok(mut message) = message.lock()
    {
        *message = status;
    }
}

/// Wake an idle app after either fresh PTY bytes *or a lifecycle transition*.
/// The latter is crucial: EOF has no bytes to queue, but it still changes
/// visible text, accessibility, and action availability. Coalescing is shared
/// with the reader's normal output wake path.
fn request_terminal_wake(
    waker: &Arc<Mutex<Option<EventLoopProxy<UserEvent>>>>,
    wake_pending: &AtomicBool,
) {
    if !wake_pending.swap(true, Ordering::AcqRel) {
        let sent = waker
            .lock()
            .ok()
            .and_then(|guard| {
                guard.as_ref().map(|proxy| {
                    proxy
                        .send_event(UserEvent::External("terminal-lifecycle".to_string()))
                        .is_ok()
                })
            })
            .unwrap_or(false);
        if !sent {
            // Embedded/startup callers have no proxy yet. They poll through
            // `needs_animation_frame`; do not strand a future real wake.
            wake_pending.store(false, Ordering::Release);
        }
    }
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
            1 => b
                .get(1)
                .copied()
                .map_or(CellColor::Default, CellColor::Indexed),
            2 => match (b.get(1), b.get(2), b.get(3)) {
                (Some(&r), Some(&g), Some(&blue)) => CellColor::Rgb(r, g, blue),
                _ => CellColor::Default,
            },
            _ => CellColor::Default,
        }
    }
}

/// Per-cell rendering attributes. Colors are `CellColor` (default / ANSI-256
/// / truecolor); the four SGR style bits plus the wide-glyph continuation
/// marker are packed into a single flags byte.
#[derive(Clone, Copy, PartialEq, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent SGR style bit, packed into one flags byte by `pack`; \
              grouping them would not change what they mean"
)]
pub struct Attr {
    pub fg: CellColor,
    pub bg: CellColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    /// SGR 2 — faint / dim: the foreground is drawn at reduced opacity.
    pub dim: bool,
    /// SGR 9 — crossed-out (strikethrough).
    pub strike: bool,
    pub is_wide_cont: bool,
    /// OSC 8 hyperlink id. `0` means "no link"; any other value indexes the
    /// widget's link registry (`TerminalWidget::links`, mirrored from the
    /// CRDT `terminal_links` map) to resolve the target URL for rendering
    /// (underline + link tint) and click-to-open. Packed alongside the cell
    /// so it rides the same CRDT + render-mirror path as every other attr.
    pub link_id: u16,
}

/// Bytes per cell in the packed `attrs` blob: fg(4) + bg(4) + flags(1) +
/// `link_id(2`, little-endian).
const ATTR_BYTES: usize = 11;

impl Attr {
    pub fn pack(self) -> [u8; ATTR_BYTES] {
        let f = self.fg.pack();
        let b = self.bg.pack();
        let mut flags = 0u8;
        if self.bold {
            flags |= 0b000_0001;
        }
        if self.italic {
            flags |= 0b000_0010;
        }
        if self.underline {
            flags |= 0b000_0100;
        }
        if self.reverse {
            flags |= 0b000_1000;
        }
        if self.is_wide_cont {
            flags |= 0b001_0000;
        }
        if self.dim {
            flags |= 0b010_0000;
        }
        if self.strike {
            flags |= 0b100_0000;
        }
        let l = self.link_id.to_le_bytes();
        [
            f[0], f[1], f[2], f[3], b[0], b[1], b[2], b[3], flags, l[0], l[1],
        ]
    }
    pub fn unpack(bytes: &[u8]) -> Self {
        if bytes.len() < ATTR_BYTES {
            return Self::default();
        }
        let flags = bytes.get(8).copied().unwrap_or(0);
        let link_id = match (bytes.get(9), bytes.get(10)) {
            (Some(&lo), Some(&hi)) => u16::from_le_bytes([lo, hi]),
            _ => 0,
        };
        Self {
            fg: CellColor::unpack(bytes.get(0..4).unwrap_or(&[])),
            bg: CellColor::unpack(bytes.get(4..8).unwrap_or(&[])),
            bold: flags & 0b000_0001 != 0,
            italic: flags & 0b000_0010 != 0,
            underline: flags & 0b000_0100 != 0,
            reverse: flags & 0b000_1000 != 0,
            is_wide_cont: flags & 0b001_0000 != 0,
            dim: flags & 0b010_0000 != 0,
            strike: flags & 0b100_0000 != 0,
            link_id,
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
            dim: false,
            strike: false,
            is_wide_cont: false,
            link_id: 0,
        }
    }
}

/// Terminal mode flags toggled by DEC private set/reset (`CSI ? Ps h/l`).
/// Owned by the widget and passed by mutable reference into the vte
/// performer so parsed modes (bracketed paste, cursor visibility, mouse
/// reporting, application cursor keys) survive across PTY reads.
#[derive(Clone, Copy, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent DEC private mode (CSI ? Ps h/l), set and read \
              separately by the vte performer; grouping them would not change what they mean"
)]
pub struct TermModes {
    /// `?25` — block cursor drawn when true (DECTCEM).
    pub cursor_visible: bool,
    /// `?2004` — wrap pasted text in `ESC[200~`…`ESC[201~`.
    pub bracketed_paste: bool,
    /// `?1` — arrow keys send `ESC O A` instead of `ESC [ A` (DECCKM).
    pub app_cursor_keys: bool,
    /// `?1000`/`?1002`/`?1003` — some form of mouse reporting requested.
    pub mouse_report: bool,
    /// The active xterm mouse tracking mode: 0, 1000 (buttons), 1002
    /// (button-motion), or 1003 (all motion). Keeping the exact mode lets us
    /// forward only the events the application requested.
    pub mouse_tracking: u16,
    /// `?1006` — SGR-encoded mouse coordinates requested.
    pub mouse_sgr: bool,
    /// `?1004` — report window focus in/out to the foreground application.
    pub focus_report: bool,
}

impl Default for TermModes {
    fn default() -> Self {
        Self {
            cursor_visible: true,
            bracketed_paste: false,
            app_cursor_keys: false,
            mouse_report: false,
            mouse_tracking: 0,
            mouse_sgr: false,
            focus_report: false,
        }
    }
}

/// A terminal emulator packaged as a reusable `ui` widget.
///
/// It owns a PTY running a shell (or an arbitrary command), a vte parser, and
/// a CRDT-backed cell grid, and implements the toolkit's `Widget` trait so it
/// drops into any layout the same way any other widget does.
///
/// # Embedding
///
/// A host that wants an integrated terminal (e.g. the IDE's terminal panel):
///
/// 1. Spawn one: [`TerminalWidget::spawn_default`] (the user's `$SHELL`) or
///    [`TerminalWidget::spawn_command`] (a given `CommandBuilder`).
/// 2. Give it a region every frame with [`place_at`](Self::place_at) (this
///    reflows the grid **and** resizes the PTY so the shell sees the new size),
///    then call [`Widget::draw`] to paint it there.
/// 3. Pump it: call [`Widget::sync`] each frame — it drains the PTY reader and
///    replays the bytes into the grid. (Gate your host's animation-frame /
///    redraw request on the terminal being visible so it only spins while on
///    screen.)
/// 4. Forward input: route winit events to [`Widget::handle_event`] while the
///    terminal is focused; keystrokes go to the PTY, wheel scrolls scrollback.
///
/// # Programmatic driving
///
/// [`write_input`](Self::write_input) sends raw bytes to the shell's stdin and
/// [`paste`](Self::paste) pastes text (honouring bracketed-paste); the
/// scrollback API ([`scroll_up`](Self::scroll_up) / [`scroll_down`](Self::scroll_down)
/// / [`scroll_to_top`](Self::scroll_to_top) / [`scroll_to_bottom`](Self::scroll_to_bottom)
/// / [`scroll_offset`](Self::scroll_offset)) and the read-back accessors
/// ([`grid_text`](Self::grid_text) / [`grid_size`](Self::grid_size)) let a host
/// inspect and control the session without a GPU.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent piece of terminal/selection state \
              (force_resync, pending_wrap, alt_active, select_to_copy, …) read and \
              set on its own; grouping them would not change what they mean"
)]
pub struct TerminalWidget {
    rect: Rect,
    pty_write: Arc<Mutex<Box<dyn Write + Send>>>,
    pty_master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    #[cfg(unix)]
    service_lifetime: Option<std::os::unix::net::UnixStream>,
    output_rx: Receiver<Vec<u8>>,
    /// At most one reader chunk that did not fit the current bounded pump.
    /// Retaining it locally preserves byte ordering without an unbounded
    /// worklist and lets the next frame resume exactly where parsing stopped.
    /// `needs_animation_frame` also parks the next queued chunk here, since a
    /// channel receiver can only be inspected by taking from it.
    deferred_output: std::cell::Cell<Option<Vec<u8>>>,
    /// Set permanently when the process end of the PTY closes, its reader
    /// fails, or a write fails. A live session cannot be honestly revived by
    /// retrying arbitrary input, so this is local terminal lifecycle state.
    session_disconnected: Arc<AtomicBool>,
    #[cfg(windows)]
    child_killer: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    /// Stable, non-sensitive reason shown in the retained terminal surface and
    /// its accessibility description. Kept local with the transport state.
    session_status_message: Arc<Mutex<&'static str>>,
    parser: Parser,

    grid: Vec<String>,
    grid_attrs: Vec<Vec<Attr>>,
    cursor_x: usize,
    cursor_y: usize,
    width: usize,
    height: usize,

    char_w: f64,
    char_h: f64,
    forced_cell: bool,
    theme_font_size: f32,
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
    /// Sub-row precision retained between high-resolution touchpad events.
    /// Terminal storage is row based, but discarding each fractional delta
    /// made slow two-finger gestures feel sticky and unpredictable.
    wheel_remainder_px: f64,
    /// Shared toolkit interaction model for the terminal history bar.
    scrollbar: Scrollbar,

    /// Local find-in-scrollback state. It is intentionally not mirrored into
    /// the CRDT or resource tree: a find term and its matching output can be
    /// sensitive, while the shared terminal transcript already has its own
    /// carefully bounded collaboration contract.
    conventions: ui::PlatformConventions,
    find_open: bool,
    find_input: TextInput,
    find_matches: Vec<FindMatch>,
    find_current: usize,
    find_query: String,
    find_rect: Rect,
    find_previous_rect: Rect,
    find_next_rect: Rect,
    find_close_rect: Rect,

    /// Last palette-derived cursor color, captured on every `draw`. Exposed
    /// for behavioural tests that want to verify palette swaps actually
    /// propagate into rendered state without spinning up a GPU scene.
    last_cursor_color: Color,
    /// Last default-fg color drawn, same rationale as `last_cursor_color`.
    last_fg_default: Color,
    /// Last default-bg color drawn, same rationale as `last_cursor_color`.
    last_bg_default: Color,

    /// Active flow-selection. `None` when nothing is selected (idle, or
    /// just after a lone click). Coordinates are **buffer-absolute** rows /
    /// grid columns (see [`GridPos`]); conversion to grid pixels subtracts
    /// the scrollback offset and uses `char_w`/`char_h` and `self.rect`.
    selection: Option<Selection>,
    /// Multi-click gesture recognizer from the toolkit — single→char,
    /// double→word, triple→line, with shift-extend. Owns the click-count /
    /// timing / distance thresholds so the terminal doesn't re-derive them.
    drag_select: DragSelect,
    /// Granularity of the in-progress drag (char / word / line), captured on
    /// press so `CursorMoved` extends by whole words or lines, not just cells.
    drag_granularity: Granularity,
    /// The word/line span the pointer first anchored on (buffer-absolute),
    /// used by `extend_span_drag` to keep that original unit inside the
    /// selection while a word-/line-granularity drag grows by whole units.
    drag_anchor_span: Option<(GridPos, GridPos)>,
    /// When true, finishing a (non-empty) selection also copies it to the
    /// system clipboard, not just the primary selection. Off by default;
    /// the primary selection is always set so middle-click paste works.
    select_to_copy: bool,
    /// Right-click context menu (Copy / Paste), reusing the shared toolkit
    /// `ContextMenu` primitive so the terminal never draws its own menu chrome.
    /// The menu's `Arc<dyn Fn>` callbacks can't reach the PTY, so we dispatch on
    /// the chosen row's label ourselves (mirrors `TextInput`).
    context_menu: ContextMenu,
    find_menu_index: Option<usize>,
    menu_paste_index: Option<usize>,
    menu_effects: HashMap<usize, ui::widget::ActionEmitter<()>>,
    allowed_menu_actions: Option<HashSet<ui::tilo_actions::ActionAddress>>,
    /// Keyboard and pointer gestures enqueue the same typed invocations as
    /// menu rows. The app host drains and admits these against the live role
    /// and confirmation policy before any PTY or clipboard effect runs.
    gesture_effects: VecDeque<ui::tilo_actions::ControlInvocation>,
    /// Cursor position cached on every `CursorMoved` so the press arm
    /// can hit-test without receiving a fresh position. Logical coords
    /// (post scale-factor divide), matching `text_input`'s convention.
    last_cursor_pos: Point,
    /// Viewport scrollback offset captured on the most recent `draw`.
    /// Stored so `handle_event` can translate logical pixel coordinates
    /// into viewport-local grid rows (the selection model lives in
    /// viewport space). Defaults to 0 until the first frame paints.
    viewport_start_y: usize,

    /// Horizontal glyph gutter used by the most recent `draw`, cached so
    /// pointer hit-testing in `handle_event` (which has no `Theme`) lands on
    /// the same column the glyphs were painted at even if the theme retunes
    /// the spacing. Seeded to the default `spacing.s1` (4px).
    last_padding_x: f64,

    /// The single source of truth for grid geometry: the widget
    /// [`rect`](Self::rect) inset by a uniform body padding on every side, with
    /// a scrollbar gutter reserved on the right. Every grid read — cell layout,
    /// cursor, selection, pointer hit-testing — is relative to this exact rect,
    /// computed once by [`grid_content_rect`](Self::grid_content_rect) and used
    /// identically by `set_geometry` and `draw`, so a glyph, its cursor, and a
    /// click all resolve to the same cell. The compositor draws the window
    /// decoration (the tab/title chrome); the app draws only the grid.
    grid_rect: Rect,
    /// Cached body geometry (padding, scrollbar gutter), refreshed from the
    /// theme on every `draw` (the same pattern as `last_padding_x`) so
    /// `set_geometry` and pointer hit-testing — neither of which receives a
    /// `Theme` — inset the grid by the exact amounts `draw` painted with.
    /// Seeded to the default token values so the first (pre-draw) layout is
    /// already correct.
    chrome_body_pad: f64,
    chrome_scrollbar_w: f64,
    /// Resolved monospace `FontChoice`, cached against the theme's mono family
    /// name. Recomputed only when that name changes: the theme's named mono
    /// face is used when installed, otherwise the generic monospace family —
    /// never the proportional default cosmic-text would silently substitute,
    /// which would give the cell grid a space advance narrower than its glyphs
    /// and smear every run.
    mono_choice: Option<(String, FontChoice)>,
    /// Latches true the first time the mirrored grid holds any non-blank
    /// content. Gates the honest empty-state watermark so it only shows before
    /// the session has ever produced output — never over a legitimately
    /// cleared screen (`clear`, `CSI 2J`) once the shell has run.
    ever_nonblank: bool,

    /// Number of oldest scrollback lines dropped from the front of the main
    /// buffer since the last `sync` mirrored the grid. `sync` uses it to keep
    /// the render mirror (`grid`/`grid_attrs`) index-aligned with the CRDT
    /// line list without re-reading every retained line each frame.
    trimmed_front: usize,
    /// True once `grid`/`grid_attrs` have been fully populated from the CRDT.
    /// While false (first frame, or after a buffer swap) `sync` does a full
    /// re-read; afterwards it only re-reads the lines the emulator actually
    /// touches (the live screen + freshly appended rows), which is what keeps
    /// heavy output from re-scanning the whole scrollback every frame.
    mirror_valid: bool,
    /// Set by `feed` when the emulator itself mutated the grid. `sync` reads
    /// it to decide between the cheap incremental re-read (our own writes only
    /// touch the live screen + appended tail) and a full re-read (a remote
    /// collaborator may have edited anywhere, so nothing can be assumed).
    pending_local_edit: bool,
    /// CRDT line count captured at the previous `sync`. The incremental
    /// re-read starts from that frame's screen top (`synced_len - height`),
    /// because the rows that scrolled off during a burst of output were the
    /// live screen *at the time they were written* — re-reading only the
    /// current screen would miss them.
    synced_len: usize,

    /// Window title set by OSC 0 / OSC 2. Mirrored from the CRDT
    /// (`terminal_title`) each `sync` so a collaborator sees the same title;
    /// surfaced through `AppWidget::app_title`. Defaults to "Terminal".
    title: String,
    /// CRDT map id for `terminal_links`: `string(link_id)` → URL. Populated by
    /// OSC 8 during `feed`; read back into `links` on `sync`.
    links_id: Option<ObjId>,
    /// Local fast-lookup mirror of the OSC 8 hyperlink registry (id → URL),
    /// rebuilt from the CRDT `terminal_links` map on `sync`. Used to render
    /// link tint/underline and to resolve a click into a URL to open.
    links: HashMap<u16, String>,
    /// Monotonic OSC 8 link-id allocator (host side). `0` is reserved for
    /// "no link"; the first hyperlink gets id 1.
    next_link_id: u16,
    /// The link id currently open (between `OSC 8 ; ; URL` and its close);
    /// every printed cell adopts it so a multi-cell link shares one id.
    /// Survives across `feed` calls so a link spanning PTY reads stays whole.
    current_link_id: u16,
    /// Content-addressed graphics descriptors. Bytes live in `content`; only
    /// immutable hashes and cell placement ride Automerge.
    images_id: Option<ObjId>,
    /// Where graphics bytes are stored and read back (verified) by hash.
    content: ContentStore,
    image_placements: Vec<TerminalImagePlacement>,
    /// Decoded images are deliberately local GPU-side state. The CRDT contains
    /// only immutable content addresses; each peer validates and decodes bytes
    /// on demand when a placement comes into view.
    image_cache: HashMap<String, ui::widget::Image>,
    graphics_scanner: GraphicsScanner,

    /// Stable per-instance id used to scope this terminal's collaboration
    /// presence (`PresenceKey::App { sub }`), so a peer's caret/selection is
    /// attributed to this pane rather than colliding with another terminal.
    pane_id: String,
    /// Set by [`ResourceSurface::attach`]. The framework root already publishes
    /// the session door; this widget then returns only the live transcript.
    framework_hosted: bool,
    /// Dispatcher stored by [`ResourceSurface::attach`]. Document-level session
    /// actions submit through it while this widget is hosted.
    framework_dispatch: Option<ui::framework::Dispatcher<crate::resource::Terminal>>,
    /// Standalone session actions submit through this host.
    session_actions: ui::ActionHost,
    /// Identifies the local PTY whose presentation is mirrored into this
    /// record. A terminal tab is durable, but its process is not: after a
    /// logout the next login spawns a new shell. Without an incarnation
    /// marker that new shell inherited the previous process' persisted grid
    /// and appended a second prompt to it, making a fresh session look as if
    /// several terminals had been restored. Headless/replica widgets leave
    /// this `None` because they intentionally render an existing transcript.
    local_session_id: Option<String>,
    /// The record scope arrives before the first sync. We reset stale
    /// process-owned presentation exactly once at that boundary, never on a
    /// collaborator's ordinary document update.
    local_session_bound: bool,

    /// Wakes the `ui::App` event loop when the PTY reader has new output, so a
    /// standalone terminal can idle (`ControlFlow::Wait`) instead of polling at
    /// ~60 Hz. The binary arms it via `App::run_with_setup` (see
    /// [`waker_handle`](Self::waker_handle)); it stays `None` when the terminal
    /// is embedded in a host that pumps it. Embedded hosts call `sync`/`update`
    /// independently of painting, so they can remain visually idle until the
    /// channel actually contains output.
    waker: Arc<Mutex<Option<EventLoopProxy<UserEvent>>>>,
    /// Coalesces reader wakes: set when a wake is in flight, cleared when the
    /// App delivers the resulting `External` event (`on_external_event`), so a
    /// burst of PTY reads sends at most one wake per processed frame.
    wake_pending: Arc<AtomicBool>,
    /// Buttons currently held while an application has mouse tracking active.
    /// Bits 0/1/2 are left/middle/right. This is local input state, never
    /// replicated with the shared terminal screen.
    mouse_buttons: u8,
    focused: bool,
}

impl TerminalWidget {
    #[allow(
        clippy::too_many_lines,
        reason = "terminal MainBody wires many host services at construction"
    )]
    pub fn new(
        writer: Arc<Mutex<Box<dyn Write + Send>>>,
        master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
        output_rx: Receiver<Vec<u8>>,
        content: ContentStore,
    ) -> Self {
        let width = 80;
        let height = 24;
        let grid = vec![" ".repeat(width); height];
        let grid_attrs = vec![vec![Attr::default(); width]; height];

        Self {
            rect: Rect::ZERO,
            pty_write: writer,
            pty_master: master,
            #[cfg(unix)]
            service_lifetime: None,
            output_rx,
            deferred_output: std::cell::Cell::new(None),
            session_disconnected: Arc::new(AtomicBool::new(false)),
            #[cfg(windows)]
            child_killer: None,
            session_status_message: Arc::new(Mutex::new("Session ended — input unavailable")),
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
            forced_cell: false,
            theme_font_size: 0.0,
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
            wheel_remainder_px: 0.0,
            scrollbar: Scrollbar::vertical(),
            conventions: ui::PlatformConventions::default(),
            find_open: false,
            find_input: TextInput::local().with_id("terminal_find"),
            find_matches: Vec::new(),
            find_current: 0,
            find_query: String::new(),
            find_rect: Rect::ZERO,
            find_previous_rect: Rect::ZERO,
            find_next_rect: Rect::ZERO,
            find_close_rect: Rect::ZERO,
            last_cursor_color: Color::from_rgba8(0, 0, 0, 0),
            last_fg_default: Color::from_rgba8(0, 0, 0, 0),
            last_bg_default: Color::from_rgba8(0, 0, 0, 0),
            selection: None,
            drag_select: DragSelect::default(),
            drag_granularity: Granularity::Char,
            drag_anchor_span: None,
            select_to_copy: false,
            context_menu: ContextMenu::new(),
            find_menu_index: None,
            menu_paste_index: None,
            menu_effects: HashMap::new(),
            allowed_menu_actions: None,
            gesture_effects: VecDeque::new(),
            last_cursor_pos: Point::ZERO,
            viewport_start_y: 0,
            last_padding_x: 4.0,
            grid_rect: Rect::ZERO,
            // Seeded to the default token metrics (body pad 12, scrollbar
            // gutter 8) so the first layout — which runs before the first
            // `draw` refreshes these from the live theme — already insets the
            // grid correctly.
            chrome_body_pad: 12.0,
            chrome_scrollbar_w: 8.0,
            mono_choice: None,
            ever_nonblank: false,
            trimmed_front: 0,
            mirror_valid: false,
            pending_local_edit: false,
            synced_len: 0,
            title: "Terminal".to_string(),
            links_id: None,
            links: HashMap::new(),
            next_link_id: 0,
            current_link_id: 0,
            images_id: None,
            image_placements: Vec::new(),
            image_cache: HashMap::new(),
            content,
            graphics_scanner: GraphicsScanner::default(),
            pane_id: uuid::Uuid::new_v4().to_string(),
            framework_hosted: false,
            framework_dispatch: None,
            session_actions: ui::ActionHost::new(),
            local_session_id: None,
            local_session_bound: false,
            waker: Arc::new(Mutex::new(None)),
            wake_pending: Arc::new(AtomicBool::new(false)),
            mouse_buttons: 0,
            focused: false,
        }
    }

    /// Mark this widget as the presentation of a newly spawned local PTY.
    /// The transcript is collaborative state only while the process that
    /// produced it is alive; a new process must start from a clean screen.
    fn with_local_session(mut self) -> Self {
        self.local_session_id = Some(uuid::Uuid::new_v4().to_string());
        self
    }

    fn reset_stale_local_session(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let Some(session_id) = self.local_session_id.as_deref() else {
            return;
        };
        if self.local_session_bound {
            return;
        }
        self.local_session_bound = true;

        let stored_session = state
            .get(root, "terminal_local_session")
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                Value::Scalar(value) => value.as_str().map(ToOwned::to_owned),
                Value::Object(_) => None,
            });
        if stored_session.as_deref() == Some(session_id) {
            return;
        }

        // These objects describe one live PTY, not the durable identity of
        // the tab. Delete the old objects atomically before `init_crdt`
        // creates blank replacements for the new process. Notes/editor data
        // and the tab itself are separate records and remain untouched.
        let action = crate::resource::TerminalAction::BindLocalSession {
            session_id: session_id.to_string(),
        };
        self.apply_session_action(state, &action);

        self.grid_id = None;
        self.alt_grid_id = None;
        self.cursor_id = None;
        self.links_id = None;
        self.images_id = None;
        self.line_ids.clear();
        self.alt_line_ids.clear();
        self.grid.clear();
        self.grid_attrs.clear();
        self.links.clear();
        self.image_placements.clear();
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.alt_active = false;
        self.saved_cursor = None;
        self.mirror_valid = false;
        self.force_resync = true;
        self.pending_local_edit = true;
        self.synced_len = 0;
        self.trimmed_front = 0;
        self.ever_nonblank = false;
        self.title = "Terminal".to_string();
        self.pane_id = uuid::Uuid::new_v4().to_string();
    }

    /// Handle used to arm the event-loop waker. A binary running the terminal
    /// under `ui::App` calls this before moving the widget into the App, then
    /// stores the `EventLoopProxy` inside `App::run_with_setup`'s closure. Once
    /// armed, the PTY reader thread wakes the loop on new output and the
    /// terminal stops polling at ~60 Hz while idle.
    pub fn waker_handle(&self) -> Arc<Mutex<Option<EventLoopProxy<UserEvent>>>> {
        self.waker.clone()
    }

    /// Color used for the block cursor on the last `draw`. Test-only accessor
    /// so palette-swap tests can assert the terminal picks up a new theme.
    pub fn last_cursor_color(&self) -> Color {
        self.last_cursor_color
    }

    /// Active selection, if any. Test-only — production code reads from
    /// the widget directly inside `draw` / `handle_event`.
    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }
    /// True between left-mouse-press and left-mouse-release. Test-only.
    pub fn is_dragging(&self) -> bool {
        self.drag_select.is_dragging()
    }

    /// True once the terminal process or its PTY transport has ended. This is
    /// intentionally local: it describes this peer's transport, not a shared
    /// claim that a collaborator's separately attached terminal is dead.
    pub fn is_disconnected(&self) -> bool {
        self.session_disconnected.load(Ordering::Acquire)
    }

    fn mark_disconnected(&self, message: &'static str) {
        set_terminal_disconnected(
            &self.session_disconnected,
            &self.session_status_message,
            message,
        );
    }

    fn disconnected_message(&self) -> &'static str {
        self.session_status_message
            .lock()
            .map_or("Session ended — input unavailable", |message| *message)
    }

    /// A compact visual lifecycle state. The detailed transport cause stays in
    /// the accessible description/live region, where it is neither clipped at
    /// 200% text scale nor mistaken for a recoverable command prompt.
    #[cfg(test)]
    pub(crate) const fn disconnected_badge_label() -> &'static str {
        "Session ended"
    }
    /// Current window title (OSC 0/2), defaulting to "Terminal". Host/test
    /// accessor — also what `AppWidget::app_title` returns.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Stable resource identity for this shared pane. `init_crdt` adopts the
    /// same pane id on every replica, so this address survives a collaborator
    /// joining or a retained-widget rebuild without conflating two terminals.
    fn terminal_resource_id(&self) -> String {
        format!("terminal:{}", self.pane_id)
    }

    /// Read the shared pane identity deterministically even when two peers
    /// initialise an otherwise-empty document at the same time. Automerge
    /// preserves concurrent scalar writes rather than silently overwriting
    /// either one, so `get()` alone is not a sufficient identity rule: it can
    /// expose a different conflict winner to different callers. Choosing the
    /// lexicographically first valid UUID makes the resource/presence address
    /// converge without inventing a destructive conflict-resolution write.
    fn shared_pane_id(state: &AutoCommit, root: &ObjId) -> Option<String> {
        let mut candidates = state
            .get_all(root, "terminal_pane_id")
            .ok()?
            .into_iter()
            .filter_map(|(value, _)| match value {
                Value::Scalar(value) => value.as_str().map(str::to_owned),
                Value::Object(_) => None,
            })
            .filter(|value| uuid::Uuid::parse_str(value).is_ok())
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        candidates.into_iter().next()
    }

    fn terminal_transcript_target(&self) -> String {
        format!("terminal.transcript:{}", self.pane_id)
    }

    fn terminal_link_resource_id(&self, link_id: u16) -> String {
        format!("terminal.link:{}:{link_id}", self.pane_id)
    }

    fn current_visible_text(&self) -> String {
        let start = self.viewport_start_y.min(self.grid.len());
        let end = (start + self.current_visible_rows()).min(self.grid.len());
        self.grid
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(|row| row.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn visible_link_ids(&self) -> Vec<u16> {
        let start = self.viewport_start_y.min(self.grid_attrs.len());
        let end = (start + self.current_visible_rows()).min(self.grid_attrs.len());
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for row in self.grid_attrs.get(start..end).unwrap_or_default() {
            for attr in row {
                if attr.link_id != 0
                    && seen.insert(attr.link_id)
                    && self
                        .links
                        .get(&attr.link_id)
                        .is_some_and(|url| is_public_web_link(url))
                {
                    ids.push(attr.link_id);
                    if ids.len() >= MAX_VISIBLE_RESOURCE_LINKS {
                        return ids;
                    }
                }
            }
        }
        ids
    }

    fn link_resource(&self, link_id: u16, url: &str) -> Resource {
        let mut resource = Resource::new(
            self.terminal_link_resource_id(link_id),
            "terminal.link",
            link_resource_label(url),
        );
        if let Ok(spec) = ActionSpec::operation(
            "open",
            "Open link",
            OperationKind::Effect,
            "terminal.open-link",
            &url,
        ) {
            resource
                .actions
                .push(spec.privileged().retry_policy(RetryPolicy::AtMostOnce));
        }
        resource
    }

    fn transcript_resource(&self) -> Resource {
        let mut transcript = Resource::new(
            self.terminal_transcript_target(),
            "terminal.transcript",
            "Terminal output history",
        );
        for (id, label, available, reason) in [
            (
                "history-top",
                "Show oldest output",
                self.grid.len() > self.current_visible_rows(),
                "No older output is retained",
            ),
            (
                "history-live",
                "Return to live output",
                self.scroll_offset > 0,
                "Already showing live output",
            ),
        ] {
            if let Ok(spec) =
                ActionSpec::operation(id, label, OperationKind::View, "terminal.control", &id)
            {
                transcript.actions.push(if available {
                    spec
                } else {
                    spec.unavailable("history_boundary", reason)
                });
            }
        }
        for (id, label) in [
            ("copy-visible", "Copy visible output"),
            ("copy-selection", "Copy selected output"),
            ("select-all", "Select all output"),
        ] {
            if let Ok(spec) =
                ActionSpec::operation(id, label, OperationKind::Effect, "terminal.control", &id)
            {
                transcript
                    .actions
                    .push(spec.privileged().retry_policy(RetryPolicy::AtMostOnce));
            }
        }
        transcript.children.extend(
            self.visible_link_ids()
                .into_iter()
                .filter_map(|id| self.links.get(&id).map(|url| self.link_resource(id, url))),
        );
        transcript
    }

    fn session_resource(&self) -> Resource {
        self.session_resource_with(None)
    }

    /// Title and pane come from the document once it stores them. Paste and
    /// the context menu, which have no document, keep the widget session.
    fn session_resource_with(&self, doc: Option<&AutoCommit>) -> Resource {
        let (title, pane_id) = self.published_session(doc);
        let disconnected = self.is_disconnected();
        let unavailable_reason =
            "The terminal process has ended or its PTY failed; input cannot be sent.";
        let label = if title == "Terminal" {
            "Terminal session".to_string()
        } else {
            format!("Terminal — {title}")
        };
        let resource_id = if pane_id.is_empty() {
            self.terminal_resource_id()
        } else {
            format!("terminal:{pane_id}")
        };
        let state = crate::resource::SessionState { title, pane_id };
        let mut terminal =
            ui::tilo_resource::publish::<crate::resource::Terminal>(&resource_id, &label, &state);
        if disconnected {
            for action in &mut terminal.actions {
                if matches!(
                    action.id.as_str(),
                    "send" | "run" | "interrupt" | "clear" | "paste" | "paste-primary"
                ) {
                    let current = action.clone();
                    *action = current.unavailable("session_disconnected", unavailable_reason);
                }
            }
        }
        terminal.children.push(self.transcript_resource());
        terminal
    }

    fn published_session(&self, doc: Option<&AutoCommit>) -> (String, String) {
        let widget = (self.title.clone(), self.pane_id.clone());
        let Some(doc) = doc else {
            return widget;
        };
        let recorded = automerge::ReadDoc::get(doc, &automerge::ROOT, "terminal_title")
            .ok()
            .flatten()
            .is_some()
            || automerge::ReadDoc::get(doc, &automerge::ROOT, "terminal_pane_id")
                .ok()
                .flatten()
                .is_some();
        if !recorded {
            return widget;
        }
        match <crate::resource::Terminal as ui::tilo_resource::Resource>::read(doc) {
            Ok(state) => (state.title, state.pane_id),
            Err(_) => widget,
        }
    }

    fn visible_link_accessibility(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        self.visible_link_ids()
            .into_iter()
            .filter_map(|link_id| {
                self.links.get(&link_id).map(|url| {
                    AccessNode::new(Role::Link)
                        .with_name(link_resource_label(url))
                        .with_description("Link in visible terminal output")
                        .with_action_target(self.terminal_link_resource_id(link_id))
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        })
                })
            })
            .collect()
    }

    fn context_menu_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        self.context_menu.is_open().then(|| {
            let action_count = self
                .context_menu
                .items()
                .iter()
                .filter(|item| item.label().is_some())
                .count();
            let mut position = 0usize;
            let children = self
                .context_menu
                .items()
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    let label = item.label()?;
                    let disabled = !item.is_enabled();
                    position = position.saturating_add(1);
                    Some(
                        AccessNode::new(Role::MenuItem)
                            .with_name(label)
                            .with_action_target(format!("terminal.menu:{}:{index}", self.pane_id))
                            .with_position(position, action_count)
                            .with_state(AccessState {
                                focusable: !disabled,
                                disabled,
                                ..Default::default()
                            }),
                    )
                })
                .collect();
            AccessNode::new(Role::Menu)
                .with_hashed_key("terminal-context-menu")
                .with_state(AccessState {
                    expanded: Some(true),
                    modal: true,
                    ..Default::default()
                })
                .with_children(children)
        })
    }

    /// Convert an accessible extended-grapheme offset in the current visible
    /// transcript into a cell coordinate. The terminal grid measures columns,
    /// while a11y measures graphemes, so emoji and combining marks must not be
    /// treated as UTF-8 byte offsets.
    fn visible_grapheme_position(&self, mut offset: usize) -> Option<GridPos> {
        let start = self.viewport_start_y.min(self.grid.len());
        let end = (start + self.current_visible_rows()).min(self.grid.len());
        for row in start..end {
            let text = self.grid.get(row)?.trim_end();
            let graphemes = text.graphemes(true).collect::<Vec<_>>();
            if offset <= graphemes.len() {
                let col = graphemes
                    .iter()
                    .take(offset)
                    .map(|grapheme| grapheme.width())
                    .sum::<usize>()
                    .min(self.width);
                return Some(GridPos { row, col });
            }
            offset = offset.saturating_sub(graphemes.len());
            if row + 1 < end {
                offset = offset.checked_sub(1)?;
            }
        }
        None
    }

    /// Inverse of [`Self::visible_grapheme_position`]. Only visible selection
    /// endpoints are published because the accessibility value is the visible
    /// transcript, not the entire scrollback buffer.
    fn visible_grapheme_offset(&self, position: GridPos) -> Option<usize> {
        let start = self.viewport_start_y.min(self.grid.len());
        let end = (start + self.current_visible_rows()).min(self.grid.len());
        if !(start..end).contains(&position.row) {
            return None;
        }
        let mut offset = 0;
        for row in start..end {
            let text = self.grid.get(row)?.trim_end();
            let graphemes = text.graphemes(true).collect::<Vec<_>>();
            if row == position.row {
                let mut col = 0;
                for (index, grapheme) in graphemes.iter().enumerate() {
                    if position.col <= col {
                        return Some(offset + index);
                    }
                    col += grapheme.width();
                }
                return Some(offset + graphemes.len());
            }
            offset += graphemes.len() + usize::from(row + 1 < end);
        }
        None
    }

    fn visible_selection_offsets(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        Some((
            self.visible_grapheme_offset(selection.anchor)?,
            self.visible_grapheme_offset(selection.cursor)?,
        ))
    }

    /// Translate the terminal's grapheme-space selection back into the UTF-8
    /// offsets carried by [`ui::a11y::AccessNode::with_text_selection`]. The
    /// bridge then exports those boundaries as AccessKit grapheme positions.
    fn visible_selection_bytes(&self, value: &str) -> Option<(usize, usize)> {
        let (anchor, focus) = self.visible_selection_offsets()?;
        let byte_at = |grapheme_offset| {
            value
                .grapheme_indices(true)
                .nth(grapheme_offset)
                .map_or(value.len(), |(byte, _)| byte)
        };
        Some((byte_at(anchor), byte_at(focus)))
    }

    fn set_visible_grapheme_selection(&mut self, anchor: usize, focus: usize) -> bool {
        let Some(anchor) = self.visible_grapheme_position(anchor) else {
            return false;
        };
        let Some(cursor) = self.visible_grapheme_position(focus) else {
            return false;
        };
        self.selection = Some(Selection { anchor, cursor });
        self.focused = true;
        true
    }
    /// Enable/disable "select to copy": when on, finishing a selection also
    /// writes it to the system clipboard (in addition to the always-set
    /// primary selection used by middle-click paste).
    pub fn set_select_to_copy(&mut self, on: bool) {
        self.select_to_copy = on;
    }
    /// Set (or clear) the selection programmatically over buffer-absolute
    /// cells. Host/demo API — e.g. "select all", or a screenshot scenario
    /// that highlights a range. Coordinates are absolute grid rows/columns
    /// (see [`GridPos`]).
    pub fn set_selection_cells(&mut self, anchor: GridPos, cursor: GridPos) {
        self.selection = Some(Selection { anchor, cursor });
    }
    /// Absolute row index (into the full buffer, scrollback included) of the
    /// first grid row whose text contains `needle`. Host/demo helper for
    /// locating content to select without a GPU.
    pub fn find_row(&self, needle: &str) -> Option<usize> {
        self.grid.iter().position(|l| l.contains(needle))
    }
    /// Like [`find_row`](Self::find_row) but returns the *last* matching row —
    /// useful when a shell echoes a command (which contains the text) before
    /// the program prints the real output line (also containing it).
    pub fn find_row_last(&self, needle: &str) -> Option<usize> {
        self.grid.iter().rposition(|l| l.contains(needle))
    }

    /// Open the local find surface. This does not publish the query, the
    /// result positions, or a command to collaborators/agents: scrollback may
    /// contain credentials and the feature is only a viewport aid.
    pub fn open_find(&mut self) {
        self.find_open = true;
        self.find_input.set_focus(true);
        self.refresh_find_matches();
    }

    /// Set the local find text for a host or screenshot. Production typing
    /// routes through the same [`TextInput`] then calls
    /// `refresh_find_matches`.
    pub fn set_find_query(&mut self, query: &str) {
        let bounded: String = query.chars().take(MAX_FIND_QUERY_CHARS).collect();
        self.find_input.set_text(&bounded);
        self.refresh_find_matches();
    }

    fn close_find(&mut self) {
        self.find_open = false;
        self.find_input.set_focus(false);
        self.find_matches.clear();
        self.find_current = 0;
        self.find_query.clear();
        self.find_rect = Rect::ZERO;
        self.find_previous_rect = Rect::ZERO;
        self.find_next_rect = Rect::ZERO;
        self.find_close_rect = Rect::ZERO;
    }

    /// Re-scan the bounded local mirror only when the query changes or fresh
    /// terminal output has reached a sync. Matches are literal and
    /// case-sensitive: terminal identifiers, paths and hashes are commonly
    /// case-significant, so silently folding case would be misleading.
    fn refresh_find_matches(&mut self) {
        let bounded: String = self
            .find_input
            .value()
            .chars()
            .take(MAX_FIND_QUERY_CHARS)
            .collect();
        if bounded != self.find_input.value() {
            self.find_input.set_text(&bounded);
        }
        self.find_query = bounded;
        self.find_matches.clear();
        self.find_current = 0;
        if self.find_query.is_empty() {
            return;
        }
        'rows: for (row, line) in self.grid.iter().enumerate() {
            for (start_byte, _) in line.match_indices(&self.find_query) {
                let start_col = line.get(..start_byte).map_or(0, UnicodeWidthStr::width);
                let end_col = start_col + UnicodeWidthStr::width(self.find_query.as_str());
                self.find_matches.push(FindMatch {
                    row,
                    start_col,
                    end_col,
                });
                if self.find_matches.len() == MAX_FIND_MATCHES {
                    break 'rows;
                }
            }
        }
        self.reveal_current_find_match();
    }

    fn reveal_current_find_match(&mut self) {
        let Some(found) = self.find_matches.get(self.find_current).copied() else {
            return;
        };
        if self.alt_active {
            return;
        }
        let visible = self.current_visible_rows().max(1);
        let max_top = self.grid.len().saturating_sub(visible);
        let top = found.row.saturating_sub(visible / 2).min(max_top);
        self.scroll_offset = max_top.saturating_sub(top);
    }

    fn step_find_match(&mut self, backwards: bool) {
        let len = self.find_matches.len();
        if len == 0 {
            return;
        }
        self.find_current = if backwards {
            self.find_current.checked_sub(1).unwrap_or(len - 1)
        } else {
            (self.find_current + 1) % len
        };
        self.reveal_current_find_match();
    }
    /// Resolve the OSC 8 hyperlink URL for the cell at absolute buffer
    /// `(row, col)`, if any. Host/test accessor: powers click-to-open and
    /// lets a test assert a parsed hyperlink landed on the right cells.
    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<&str> {
        let attrs = self.grid_attrs.get(row)?;
        let link_id = attrs.get(col)?.link_id;
        if link_id == 0 {
            return None;
        }
        self.links.get(&link_id).map(String::as_str)
    }
    /// True while the alternate screen buffer is active. Test/host accessor.
    pub fn is_alt_screen(&self) -> bool {
        self.alt_active
    }
    /// Current terminal mode flags. Test/host accessor.
    pub fn modes(&self) -> TermModes {
        self.modes
    }
    /// Rows currently scrolled back from the live bottom. Test/host accessor.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }
    /// Force the widget into a known *grid* geometry for tests. Sets both the
    /// outer widget rect and the inner grid content rect to `rect` (i.e. the
    /// passed rect IS the cell area, with no chrome inset), so grid-level
    /// hit-testing / selection tests can target known cells directly. The
    /// production `set_geometry` path insets the grid below the chrome; that
    /// inset is exercised separately via `place_at`.
    #[cfg(test)]
    pub fn force_geometry(&mut self, rect: Rect, char_w: f64, char_h: f64) {
        self.rect = rect;
        self.grid_rect = rect;
        self.char_w = char_w;
        self.char_h = char_h;
        self.forced_cell = true;
    }

    /// The inner grid content rect (below the header, inside the padding).
    /// Test accessor for the chrome-inset geometry.
    #[cfg(test)]
    pub fn grid_rect(&self) -> Rect {
        self.grid_rect
    }
    /// The current cell height used for grid layout. Test accessor.
    #[cfg(test)]
    pub fn grid_char_h(&self) -> f64 {
        self.char_h
    }
    /// Bounds of the currently open shared context menu. Test-only so narrow
    /// viewport regression tests can assert that the terminal keeps its
    /// overlay reachable rather than clipping it off-screen.
    #[cfg(test)]
    pub fn context_menu_rect(&self) -> Rect {
        Widget::get_rect(&self.context_menu)
    }
    #[cfg(test)]
    pub fn context_menu_is_open(&self) -> bool {
        self.context_menu.is_open()
    }
    #[cfg(test)]
    pub fn context_menu_find_rect(&self) -> Option<Rect> {
        self.context_menu.item_rect(self.find_menu_index?)
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
        let scroll = if self.alt_active {
            0
        } else {
            self.scroll_offset
        };
        let start = max_top.saturating_sub(scroll);
        self.grid.iter().skip(start).take(vis).cloned().collect()
    }
    /// Find overlay state for geometry and privacy regressions. Kept
    /// test-only: production hosts use the explicit open/set APIs and do not
    /// inspect another person's local query/matches.
    #[cfg(test)]
    pub fn find_test_state(&self) -> (bool, Vec<usize>) {
        (
            self.find_open,
            self.find_matches.iter().map(|found| found.row).collect(),
        )
    }
    /// The overlay and its three controls after the real draw pass.
    #[cfg(test)]
    pub fn find_test_geometry(&self) -> (Rect, Rect, Rect, Rect, Rect) {
        (
            self.find_rect,
            self.find_input.get_rect(),
            self.find_previous_rect,
            self.find_next_rect,
            self.find_close_rect,
        )
    }
    #[cfg(test)]
    pub fn find_test_target(&self, control: &str) -> String {
        format!("terminal.find:{}:{control}", self.pane_id)
    }

    /// Tee subsequent PTY writes so a compositor-path test can see them.
    #[cfg(any(test, debug_assertions))]
    pub fn attach_input_probe(&self) -> std::sync::mpsc::Receiver<Vec<u8>> {
        struct ProbeWriter {
            inner: Box<dyn Write + Send>,
            tx: std::sync::mpsc::Sender<Vec<u8>>,
        }
        impl Write for ProbeWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                let _ = self.tx.send(buf.to_vec());
                self.inner.write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.inner.flush()
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        if let Ok(mut guard) = self.pty_write.lock() {
            let inner = std::mem::replace(&mut *guard, Box::new(std::io::sink()));
            *guard = Box::new(ProbeWriter { inner, tx });
        }
        rx
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
    pub fn test_selected_text(&self) -> Option<String> {
        self.selected_text()
    }
    /// Default-fg color last drawn with (sampled from the theme's terminal
    /// palette). Test-only.
    pub fn last_fg_default(&self) -> Color {
        self.last_fg_default
    }
    /// Default-bg color last drawn with (sampled from the theme's terminal
    /// palette). Test-only.
    pub fn last_bg_default(&self) -> Color {
        self.last_bg_default
    }

    /// The full terminal buffer as text — one grid row per line, including
    /// retained scrollback. Host-facing: lets an embedder read the on-screen
    /// content (search, "copy all", accessibility) without a GPU scene.
    /// Trailing padding spaces are preserved; callers trim as needed.
    pub fn grid_text(&self) -> String {
        self.grid.join("\n")
    }

    /// Current grid dimensions as `(cols, rows)`. Reflects the last
    /// `place_at` / `apply_layout` reflow. Host-facing accessor for sizing.
    pub fn grid_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Shared, content-addressed graphics placements discovered in the output
    /// stream. Visible supported placements are decoded locally and painted on
    /// the cell grid; callers can also inspect the collaborative descriptors.
    pub fn image_placements(&self) -> &[TerminalImagePlacement] {
        &self.image_placements
    }

    /// The store graphics bytes are kept in.
    #[cfg(test)]
    pub(crate) fn content_store(&self) -> &ContentStore {
        &self.content
    }

    /// Number of grid rows currently visible, from the paint geometry.
    fn current_visible_rows(&self) -> usize {
        (self.grid_rect.height() / self.char_h.max(1.0))
            .floor()
            .max(1.0) as usize
    }

    /// Refresh the cached body metrics (padding, scrollbar gutter) from the
    /// theme. Called each `draw`; the cached values then feed `set_geometry`
    /// and pointer hit-testing, which never see a `Theme`. Both derive from the
    /// shared spacing scale so the OS can retune the spacing centrally.
    fn refresh_chrome_metrics(&mut self, theme: &Theme) {
        let sp = &theme.tokens.spacing;
        self.chrome_body_pad = f64::from(sp.s3);
        self.chrome_scrollbar_w = f64::from(sp.s2);
    }

    /// Row pitch of the cell grid from the renderer's measured line box for
    /// one cell — the single number `measure`, `draw`, selection bands, the
    /// block cursor and pointer hit-testing all step rows by. Rounded up so
    /// rows land on whole pixels (20 px at 1× for the 14 px base font), never
    /// below one pixel.
    fn cell_h(measured_line_h: f32) -> f64 {
        f64::from(measured_line_h).ceil().max(1.0)
    }

    /// The inner grid content rect for a given widget rect: the widget rect
    /// inset by a uniform body padding on every side, with a scrollbar gutter
    /// reserved on the right. No header — the compositor owns window
    /// decoration, so the grid starts at the top padding. Uses the cached body
    /// metrics so it is available without a `Theme`. This is the *single*
    /// definition of the grid geometry, shared by `set_geometry` and `draw`.
    fn grid_content_rect(&self, rect: Rect) -> Rect {
        let pad = self.chrome_body_pad;
        let x0 = rect.x0 + pad;
        let y0 = rect.y0 + pad;
        let x1 = (rect.x1 - pad - self.chrome_scrollbar_w).max(x0);
        let y1 = (rect.y1 - pad).max(y0);
        Rect::new(x0, y0, x1, y1)
    }

    /// True when every mirrored grid row is blank — used to gate the
    /// first-run empty-state watermark.
    fn grid_is_blank(&self) -> bool {
        self.grid.iter().all(|l| l.chars().all(char::is_whitespace))
    }

    /// Furthest the viewport can scroll up: the number of rows above the
    /// live viewport.
    fn max_scroll(&self) -> usize {
        self.grid.len().saturating_sub(self.current_visible_rows())
    }

    fn scrollbar_track(&self) -> Rect {
        let x1 = self.rect.x1 - self.chrome_body_pad;
        let width = (self.chrome_scrollbar_w - 2.0).max(3.0);
        Rect::new(x1 - width, self.grid_rect.y0, x1, self.grid_rect.y1)
    }

    fn scrollbar_metrics(&self) -> ScrollMetrics {
        let visible = self.current_visible_rows() as f64;
        let total = self.grid.len() as f64;
        // Scrollbar offsets are measured from the top; terminal history is
        // measured back from the live bottom.
        let from_top = self.max_scroll().saturating_sub(self.scroll_offset) as f64;
        ScrollMetrics::new(total, visible, from_top)
    }

    fn set_scrollbar_offset(&mut self, from_top: f64) {
        let max = self.max_scroll();
        let top = from_top.round().clamp(0.0, max as f64) as usize;
        self.scroll_offset = max.saturating_sub(top);
        self.wheel_remainder_px = 0.0;
    }

    /// Scroll the viewport back (toward older output) by `n` rows.
    pub fn scroll_up(&mut self, n: usize) {
        if self.alt_active {
            return;
        }
        self.scroll_offset = (self.scroll_offset + n).min(self.max_scroll());
    }
    /// Scroll the viewport forward (toward the live bottom) by `n` rows.
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }
    /// Jump to the oldest retained line.
    pub fn scroll_to_top(&mut self) {
        if self.alt_active {
            return;
        }
        self.scroll_offset = self.max_scroll();
    }
    /// Pin the viewport back to the live output.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.wheel_remainder_px = 0.0;
    }

    /// Drain queued PTY bytes and replay them through the vte parser into the
    /// CRDT grid. Shared by both `Widget::update` (for hosts that pump it, e.g.
    /// the compositor's tile host) and `Widget::sync` (for the plain `ui::App`
    /// loop, which never calls `update`). Returns whether any bytes were
    /// processed this call.
    fn pump(&mut self, state: &mut AutoCommit, root: &ObjId) -> bool {
        self.init_crdt(state, root);

        // Drain every queued read and coalesce them into one buffer, then feed
        // the parser once. The reader thread hands over one `Vec` per ~8 KB
        // `read`, and a burst (`cat` of a big file, a full-screen redraw)
        // queues many; parsing them as a single slice constructs one performer,
        // commits the cursor once, and flushes one query-reply batch instead of
        // repeating that per read. The vte parser is stateful across the drain,
        // so an escape sequence split across two reads still parses correctly.
        // Draining first also drops the `output_rx` borrow before `feed` takes
        // `&mut self`.
        let mut batch = Vec::new();
        while batch.len() < MAX_PUMP_BYTES {
            let bytes = match self.deferred_output.get_mut().take() {
                Some(bytes) => bytes,
                None => match self.output_rx.try_recv() {
                    Ok(bytes) => bytes,
                    Err(_) => break,
                },
            };
            let remaining = MAX_PUMP_BYTES.saturating_sub(batch.len());
            // Chunks are reader-owned 8 KiB buffers, so this normally copies
            // whole chunks. If either constant changes, do not drop the tail:
            // parse that single oversize chunk and resume the budget next frame.
            if bytes.len() > remaining && !batch.is_empty() {
                self.deferred_output.set(Some(bytes));
                break;
            }
            batch.extend_from_slice(&bytes);
        }
        let activity = !batch.is_empty();
        if activity {
            self.feed(state, root, &batch);
            // `feed` normally latches this itself, but keep the PTY boundary
            // authoritative: even if CRDT initialisation or a future parser
            // early-return prevents visible cells from being mirrored in this
            // frame, bytes were received from a live session and the loading
            // watermark must never be painted over them.
            self.ever_nonblank = true;
            // Fresh output snaps the viewport back to the live bottom.
            self.scroll_offset = 0;
        }
        activity
    }

    #[cfg(test)]
    pub(crate) fn has_received_pty_output(&self) -> bool {
        self.ever_nonblank
    }

    /// Run `bytes` through the vte parser + performer, mutating the active
    /// CRDT buffer. This is the single production entry point for terminal
    /// input processing — the PTY pump feeds real shell output through it,
    /// and tests / headless screenshot scenarios feed canned VT sequences
    /// through the very same path.
    ///
    /// A no-op if `grid_id`, `alt_grid_id`, `cursor_id` or `links_id` are
    /// still unset after `init_crdt` (called first here) — normally an
    /// internal invariant it always establishes, but if the underlying doc
    /// rejected a `put_object` there is nothing to feed into.
    pub fn feed(&mut self, state: &mut AutoCommit, root: &ObjId, bytes: &[u8]) {
        self.init_crdt(state, root);

        // Any PTY bytes mean the session is live, even when the first batch is
        // entirely cursor movement, colour setup, or a prompt whose visible
        // cells have not reached the render mirror yet. Inferring readiness
        // only from `grid_is_blank()` left the animated first-run watermark
        // painted over a real prompt and kept needless repaint work alive.
        if !bytes.is_empty() {
            self.ever_nonblank = true;
        }

        // Clamp cursors in case of resize.
        self.cursor_x = self.cursor_x.min(self.width.saturating_sub(1));
        self.cursor_y = self.cursor_y.min(self.height.saturating_sub(1));

        let was_alt = self.alt_active;
        let mut title_out: Option<String> = None;
        // Reply bytes for any capability query in this batch (DA1/DA2, DSR,
        // XTVERSION, DECRQM). Written back to the PTY after the parse — the
        // vte performer only reads, so it stages answers here.
        let mut responses: Vec<u8> = Vec::new();
        // `init_crdt` just above always establishes these ids; if it somehow
        // bailed out early (a `put_object` failure on the underlying doc),
        // there is nothing to feed into.
        let (
            Some(main_grid_id),
            Some(alt_grid_id),
            Some(cursor_id),
            Some(links_id),
            Some(images_id),
        ) = (
            self.grid_id.clone(),
            self.alt_grid_id.clone(),
            self.cursor_id.clone(),
            self.links_id.clone(),
            self.images_id.clone(),
        )
        else {
            return;
        };
        let mut performer = TerminalPerformer {
            state,
            main_grid_id,
            alt_grid_id,
            cursor_id,
            links_id,
            images_id,
            content: &self.content,
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
            trimmed_front: &mut self.trimmed_front,
            next_link_id: &mut self.next_link_id,
            current_link_id: self.current_link_id,
            title_out: &mut title_out,
            responses: &mut responses,
            print_buf: None,
        };
        // vte deliberately discards APC payloads, while Kitty graphics uses
        // APC (`ESC _ G … ESC \\`). A tiny streaming pre-parser extracts only
        // that protocol and leaves every ordinary ANSI byte untouched.
        for event in self.graphics_scanner.push(bytes) {
            match event {
                StreamEvent::Bytes(bytes) => self.parser.advance(&mut performer, &bytes),
                StreamEvent::Kitty(payload) => performer.kitty_graphics(&payload),
            }
        }
        // Write the cursor into the CRDT exactly once per feed rather than on
        // every printed cell — the per-cell `put` was pure churn (the cursor
        // is read back only by `sync`/`init_crdt`).
        performer.flush_cursor();
        self.cursor_x = performer.cursor_x;
        self.cursor_y = performer.cursor_y;
        self.current_attr = performer.current_attr;
        self.pending_wrap = performer.pending_wrap;
        self.scroll_top = performer.scroll_top;
        self.scroll_bottom = performer.scroll_bottom;
        self.current_link_id = performer.current_link_id;
        if !self.modes.mouse_report {
            self.mouse_buttons = 0;
        }

        // Answer any capability queries this batch raised. The performer's
        // borrow of `self` has ended, so we can take the PTY writer lock and
        // send the reply back on the shell's stdin — the same wire keystrokes
        // use, which is where the shell expects to read a query response.
        if !responses.is_empty() {
            self.write_input(&responses);
        }

        // Persist an OSC 0/2 title change into the CRDT so a collaborator sees
        // it, and reflect it locally for `app_title`.
        if let Some(t) = title_out
            && t != self.title
        {
            let action = crate::resource::TerminalAction::SetTitle { title: t.clone() };
            self.apply_session_action(state, &action);
            self.title = t;
        }

        // We just mutated the grid: let `sync` take the cheap incremental
        // re-read path (only the live screen + appended tail changed).
        self.pending_local_edit = true;

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

    /// Write raw bytes to the PTY's stdin and report exact transport failure.
    /// Resource actions use this fallible path so the action door cannot turn a
    /// dead PTY into a false `Succeeded` result. Local keyboard/paste callers
    /// use [`Self::write_input`], which records the same terminal state and
    /// exposes it visibly even though winit cannot return an error.
    ///
    /// # Errors
    ///
    /// Returns an error when the PTY has already ended, its writer lock is
    /// poisoned, or either write/flush operation fails. Every such error marks
    /// the session disconnected before returning.
    pub fn try_write_input(&self, bytes: &[u8]) -> Result<(), String> {
        if self.is_disconnected() {
            return Err("terminal session is disconnected; input was not sent".to_string());
        }
        let Ok(mut writer) = self.pty_write.lock() else {
            self.mark_disconnected("Terminal input unavailable — PTY writer stopped");
            return Err("terminal input is unavailable because its PTY writer stopped".to_string());
        };
        let failed = writer.write_all(bytes).is_err() || writer.flush().is_err();
        drop(writer);
        if failed {
            self.mark_disconnected("Terminal input failed — session unavailable");
            return Err("terminal input failed; the session is now disconnected".to_string());
        }
        Ok(())
    }

    /// Best-effort convenience wrapper for keyboard and embedded callers. Any
    /// failure is still made durable through [`Self::is_disconnected`] and the
    /// visible/a11y disconnected state; callers that can report completion use
    /// [`Self::try_write_input`] instead.
    pub fn write_input(&self, bytes: &[u8]) {
        let _ = self.try_write_input(bytes);
    }

    /// Paste `text` into the PTY. When the running app has requested
    /// bracketed paste (`CSI ? 2004 h`) the text is wrapped in the
    /// `ESC[200~`…`ESC[201~` guards so the app can tell a paste from typed
    /// input; otherwise newlines are normalised to carriage returns (what
    /// the Enter key sends). Pasting snaps the viewport to the live bottom.
    pub fn paste(&mut self, text: &str) {
        let _ = self.try_paste(text);
    }

    fn try_paste(&mut self, text: &str) -> Result<(), String> {
        if text.is_empty() {
            return Err("there is no text to paste".into());
        }
        let mut out = Vec::new();
        if self.modes.bracketed_paste {
            out.extend_from_slice(b"\x1b[200~");
            out.extend_from_slice(text.as_bytes());
            out.extend_from_slice(b"\x1b[201~");
        } else {
            let normalised = text.replace("\r\n", "\r").replace('\n', "\r");
            out.extend_from_slice(normalised.as_bytes());
        }
        self.try_write_input(&out)?;
        self.scroll_offset = 0;
        Ok(())
    }

    /// Assign `rect` and, if the cell grid dimensions changed, reflow the
    /// CRDT-backed grid and resize the PTY so the shell sees the new size.
    fn set_geometry(&mut self, rect: Rect) {
        self.rect = rect;
        // The grid lives in the content rect (below the header, inside the
        // padding), so it — not the outer widget rect — drives the cell count
        // and the PTY size.
        self.grid_rect = self.grid_content_rect(rect);
        let grid_rect = self.grid_rect;

        let new_width = (grid_rect.width() / self.char_w.max(1.0)).floor() as usize;
        let new_height = (grid_rect.height() / self.char_h.max(1.0)).floor() as usize;

        // Cap the CRDT-backed grid at a sensible terminal size. The init pass
        // splices `self.height * self.width` characters into a fresh automerge
        // doc; each char is its own column op, and automerge's column-data
        // insert is O(doc_size), so a full-screen embed (186 × 219 ≈ 40 700
        // chars) freezes the process for tens of seconds while the doc
        // compacts. The visible region can be larger than the CRDT grid —
        // overflow rows render empty until the grid is made sparse/lazy.
        let new_width = new_width.min(200);
        let new_height = new_height.min(80);
        if new_width > 0 && new_height > 0 && (new_width != self.width || new_height != self.height)
        {
            let history_offset = self.line_ids.len().saturating_sub(self.height);
            let absolute_y = history_offset + self.cursor_y;

            let new_history_offset = self.line_ids.len().saturating_sub(new_height);
            let new_cursor_y = absolute_y
                .saturating_sub(new_history_offset)
                .min(new_height.saturating_sub(1));

            self.cursor_y = new_cursor_y;
            self.cursor_x = self.cursor_x.min(new_width.saturating_sub(1));

            self.width = new_width;
            self.height = new_height;
            // Reset the scrolling region to the full new screen unless an
            // app narrows it again.
            self.scroll_top = 0;
            self.scroll_bottom = new_height.saturating_sub(1);
            // A poisoned lock means some other thread already panicked while
            // holding it; skip the PTY resize rather than propagate that
            // panic here too — the grid dimensions above are updated either way.
            if let Ok(master) = self.pty_master.lock() {
                let _ = master.resize(PtySize {
                    rows: self.height as u16,
                    cols: self.width as u16,
                    pixel_width: grid_rect.width() as u16,
                    pixel_height: grid_rect.height() as u16,
                });
            }
        }
    }

    pub(crate) fn init_crdt(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Session actions cover the document root and a code-editor terminal
        // panel. Any other embedding still keeps its grid on that object.
        let actions_cover = root == &automerge::ROOT
            || matches!(
                state.get(&automerge::ROOT, "terminal_panel"),
                Ok(Some((_, id))) if &id == root
            );
        if actions_cover {
            self.apply_session_action(state, &crate::resource::TerminalAction::EnsureStructure);
        }
        if self.grid_id.is_none() {
            self.grid_id =
                adopt_or_create(state, root, "terminal_grid", ObjType::List, actions_cover);
            if self.grid_id.is_none() {
                return;
            }
        }
        let Some(grid_id) = self.grid_id.clone() else {
            return;
        };
        Self::ensure_lines(state, &grid_id, false, self.height, &mut self.line_ids);

        if self.alt_grid_id.is_none() {
            self.alt_grid_id = adopt_or_create(
                state,
                root,
                "terminal_grid_alt",
                ObjType::List,
                actions_cover,
            );
            if self.alt_grid_id.is_none() {
                return;
            }
        }
        let Some(alt_grid_id) = self.alt_grid_id.clone() else {
            return;
        };
        Self::ensure_lines(
            state,
            &alt_grid_id,
            true,
            self.height,
            &mut self.alt_line_ids,
        );

        if self.links_id.is_none() {
            self.links_id =
                adopt_or_create(state, root, "terminal_links", ObjType::Map, actions_cover);
            if self.links_id.is_none() {
                return;
            }
        }
        if self.images_id.is_none() {
            self.images_id =
                adopt_or_create(state, root, "terminal_images", ObjType::List, actions_cover);
            if self.images_id.is_none() {
                return;
            }
        }

        // Adopt (or seed) a shared pane id so both peers of a shared terminal
        // scope their collaboration presence under the *same* `PresenceKey`,
        // and a peer's caret/selection is found when rendering. The first peer
        // to touch the doc writes its id; late joiners read it back.
        if let Some(id) = Self::shared_pane_id(state, root) {
            self.pane_id = id;
        } else if actions_cover {
            let action = crate::resource::TerminalAction::ClaimPane {
                pane_id: self.pane_id.clone(),
            };
            self.apply_session_action(state, &action);
            if let Some(id) = Self::shared_pane_id(state, root) {
                self.pane_id = id;
            }
        } else {
            let _ = state.put(root, "terminal_pane_id", self.pane_id.clone());
        }

        if self.cursor_id.is_none() {
            if let Ok(Some((Value::Object(ObjType::Map), id))) = state.get(root, "cursor") {
                self.cursor_id = Some(id.clone());
                if let Ok(Some((Value::Scalar(v), _))) = state.get(&id, "y")
                    && let Some(y) = v.as_i64()
                {
                    let abs_y = y as usize;
                    let history_offset = self.line_ids.len().saturating_sub(self.height);
                    self.cursor_y = abs_y
                        .saturating_sub(history_offset)
                        .min(self.height.saturating_sub(1));
                }
            } else if actions_cover
                && ui::tilo_actions::Action::apply(
                    &crate::resource::TerminalAction::SetCursor { x: 0, y: 0 },
                    state,
                )
                .is_ok()
                && let Ok(Some((Value::Object(ObjType::Map), cursor_id))) =
                    state.get(root, "cursor")
            {
                self.cursor_id = Some(cursor_id);
            } else if !actions_cover
                && let Ok(cursor_id) = state.put_object(root, "cursor", ObjType::Map)
                && state.put(&cursor_id, "x", 0u64).is_ok()
                && state.put(&cursor_id, "y", 0u64).is_ok()
            {
                self.cursor_id = Some(cursor_id);
            }
        }
    }

    /// Ensure `grid_id` holds at least `min_len` blank lines and refresh
    /// `ids` to mirror the list. Shared by the main and alternate buffers.
    /// Lines are created empty; `print` grows them on demand (see below),
    /// so no width is needed here.
    fn ensure_lines(
        state: &mut AutoCommit,
        grid_id: &ObjId,
        alternate: bool,
        min_len: usize,
        ids: &mut Vec<ObjId>,
    ) {
        // Blank lines stay empty. Pre-filling every new line with spaces
        // splices a full row of characters into a fresh text object, and that
        // insert is O(doc) per character — seeding an 80-column screen froze
        // the compositor. Drawing pads visually; print grows a line when the
        // cursor lands past its end.
        let Some(min_len) = u32::try_from(min_len).ok() else {
            return;
        };
        let key = if alternate {
            "terminal_grid_alt"
        } else {
            "terminal_grid"
        };
        if grid_is_document_root(state, key, grid_id) {
            let _ = ui::tilo_actions::Action::apply(
                &crate::resource::TerminalAction::EnsureLines { alternate, min_len },
                state,
            );
        } else {
            let target = min_len as usize;
            let current_len = state.length(grid_id);
            for y in current_len..target {
                let Ok(line_id) = state.insert_object(grid_id, y, ObjType::Map) else {
                    break;
                };
                let _ = state.put_object(&line_id, "text", ObjType::Text);
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

fn grid_is_document_root(state: &AutoCommit, key: &str, grid_id: &ObjId) -> bool {
    let at = |parent: &ObjId| {
        matches!(
            state.get(parent, key),
            Ok(Some((_, id))) if &id == grid_id
        )
    };
    if at(&automerge::ROOT) {
        return true;
    }
    matches!(
        state.get(&automerge::ROOT, "terminal_panel"),
        Ok(Some((Value::Object(ObjType::Map), panel))) if at(&panel)
    )
}

fn adopt_or_create(
    state: &mut AutoCommit,
    root: &ObjId,
    key: &str,
    kind: ObjType,
    at_document_root: bool,
) -> Option<ObjId> {
    if let Ok(Some((Value::Object(found), id))) = state.get(root, key)
        && found == kind
    {
        return Some(id);
    }
    if at_document_root {
        return None;
    }
    state.put_object(root, key, kind).ok()
}

/// Retained scrollback lines for the main buffer. Sized for a daily-driver
/// terminal; the incremental `sync` re-read means a deep history no longer
/// costs anything per frame. Under `cfg(test)` it is small so the trim path
/// is reachable without feeding thousands of lines through the (much slower)
/// unoptimised automerge build.
#[cfg(not(test))]
const MAX_HISTORY: usize = 5000;
#[cfg(test)]
const MAX_HISTORY: usize = 120;
/// The main buffer is allowed to overrun `MAX_HISTORY` by this many lines
/// before a batched front-trim runs, amortising the trim's cost.
#[cfg(not(test))]
const HISTORY_TRIM_SLACK: usize = 256;
#[cfg(test)]
const HISTORY_TRIM_SLACK: usize = 8;

impl TerminalWidget {
    /// The find bar's match status, announced on its previous/next buttons.
    fn find_result_summary(&self) -> String {
        if self.find_query.is_empty() {
            "Type to find in retained local scrollback".to_string()
        } else if self.find_matches.is_empty() {
            "No matches in retained local scrollback".to_string()
        } else if self.find_matches.len() == MAX_FIND_MATCHES {
            format!(
                "Match {} of {} or more",
                self.find_current + 1,
                MAX_FIND_MATCHES
            )
        } else {
            format!(
                "Match {} of {}",
                self.find_current + 1,
                self.find_matches.len()
            )
        }
    }
}

impl TerminalWidget {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        let value: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        match type_id {
            "terminal.send" => {
                if kind != OperationKind::Effect {
                    return Ok(false);
                }
                if value.is_empty() {
                    return Err("text to send cannot be empty".to_string());
                }
                if value.len() > MAX_RESOURCE_SEND_BYTES {
                    return Err(format!(
                        "text to send exceeds the {} KiB reviewed-command limit",
                        MAX_RESOURCE_SEND_BYTES / 1024
                    ));
                }
                if !is_reviewable_terminal_text(&value) {
                    return Err(
                        "text to send may not contain terminal control characters".to_string()
                    );
                }
                self.try_write_input(value.as_bytes())?;
            }
            "terminal.run" => {
                if kind != OperationKind::Effect {
                    return Ok(false);
                }
                if value.trim().is_empty() {
                    return Err("command to run cannot be empty".to_string());
                }
                if value.len() > MAX_RESOURCE_SEND_BYTES {
                    return Err(format!(
                        "command exceeds the {} KiB reviewed-command limit",
                        MAX_RESOURCE_SEND_BYTES / 1024
                    ));
                }
                if !is_reviewable_terminal_text(&value) {
                    return Err(
                        "command must be one line and may not contain terminal control characters"
                            .to_string(),
                    );
                }
                let mut command = value.into_bytes();
                command.push(b'\r');
                self.try_write_input(&command)?;
            }
            "terminal.paste" => {
                if kind != OperationKind::Effect {
                    return Ok(false);
                }
                self.try_paste(&value)?;
            }
            "terminal.control" => match (kind, value.as_str()) {
                (OperationKind::Effect, "interrupt") => self.try_write_input(&[0x03])?,
                (OperationKind::Effect, "clear") => self.try_write_input(&[0x0c])?,
                (OperationKind::Effect, "copy-selection") => {
                    let text = self
                        .selected_text()
                        .filter(|text| !text.is_empty())
                        .ok_or("there is no selected terminal output to copy")?;
                    if !ui::text_selection::try_clipboard_copy(&text) {
                        return Err(
                            "the system clipboard did not accept selected terminal output".into(),
                        );
                    }
                }
                (OperationKind::Effect, "select-all") => self.select_all_output(),
                (OperationKind::Effect, "copy-visible") => {
                    let visible = self.current_visible_text();
                    if visible.trim().is_empty() {
                        return Err("there is no visible terminal output to copy".to_string());
                    }
                    if !ui::text_selection::try_clipboard_copy(&visible) {
                        return Err("the system clipboard did not accept terminal output".into());
                    }
                }
                (OperationKind::View, "history-top") => self.scroll_to_top(),
                (OperationKind::View, "history-live") => self.scroll_to_bottom(),
                _ => return Ok(false),
            },
            "terminal.open-link" => {
                if kind != OperationKind::Effect {
                    return Ok(false);
                }
                if safe_osc_text(value.as_bytes(), MAX_OSC_LINK_BYTES).is_none()
                    || !is_public_web_link(&value)
                {
                    return Err("link target is not a safe public web URL".to_string());
                }
                open_url(&value);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

impl Widget for TerminalWidget {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::presence::capability::PublishesPresence
    );

    fn test_id(&self) -> Option<&str> {
        Some("terminal")
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "terminal.send",
            "terminal.run",
            "terminal.paste",
            "terminal.control",
            "terminal.open-link",
        ]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        // `measure` does not receive a theme; read the cached font size set
        // by the most recent `draw` (which pulls from
        // `theme.tokens.font_size_base`). Before the first draw the cache is
        // 0.0 and we keep the constructor-seeded char_w/char_h so layout still
        // yields a usable grid.
        if self.forced_cell {
            return taffy::Size::ZERO;
        }
        let font_size = if self.theme_font_size > 0.0 {
            text_renderer
                .renderer_input_style(&TextStyle::new(self.theme_font_size))
                .size
        } else if self.font_size > 0.0 {
            self.font_size
        } else {
            ui::tokens::TypeScale::default().base
        };
        let (w, h) =
            text_renderer.measure_styled(" ", font_size, None, TextWeight::Regular, false, true);
        if w > 0.0 && h > 0.0 {
            self.char_w = f64::from(w).max(1.0);
            self.char_h = Self::cell_h(h);
        }
        taffy::Size::ZERO
    }

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        self.reset_stale_local_session(state, root);
        self.find_input.sync(state, root);
        // Drain any bytes the PTY reader thread has queued and replay them
        // through the vte parser *here*, before reading the grid back out.
        // The plain `ui::App` event loop only ever calls `sync` (never
        // `update`), so if the pty were only pumped from `update` the live
        // terminal would never advance. Pumping from `sync` — combined with
        // the standalone waker (or an embedding host's app pump) driving the
        // loop — is what makes a terminal actually show output.
        self.pump(state, root);

        let current_heads = state.get_heads();
        let heads_unchanged = current_heads == self.last_sync_heads;
        if !self.force_resync && self.mirror_valid && heads_unchanged {
            self.pending_local_edit = false;
            return;
        }
        // Choose the re-read strategy. A full re-read is required whenever the
        // mirror can't be trusted line-for-line: the first frame, a main↔alt
        // buffer swap (`force_resync`), or heads that moved without our own
        // `feed` (a remote collaborator could have edited any line). The
        // emulator's *own* writes only ever touch the live screen and the
        // freshly appended tail, so those take the cheap incremental path.
        let full_reread = !self.mirror_valid || self.force_resync || !self.pending_local_edit;
        self.force_resync = false;
        self.pending_local_edit = false;
        self.last_sync_heads = current_heads;

        self.init_crdt(state, root);

        // Mirror whichever buffer is active. The alt buffer has no scrollback,
        // so its list is exactly `height` lines. Cloned rather than borrowed:
        // the mirror refresh below needs `&mut self` while still reading
        // these ids, and a clone of a handful of `ObjId`s is cheap next to a
        // full grid re-read.
        let (grid_id, line_ids) = if self.alt_active {
            (self.alt_grid_id.clone(), self.alt_line_ids.clone())
        } else {
            (self.grid_id.clone(), self.line_ids.clone())
        };
        let Some(grid_id) = grid_id else {
            return;
        };

        let len = state.length(&grid_id);
        self.refresh_grid_mirror(state, &line_ids, len, full_reread);
        self.refresh_title_and_links(state, root);
        self.refresh_image_placements(state);
        self.refresh_cursor_from_crdt(state, line_ids.len());
        if self.find_open {
            self.refresh_find_matches();
        }
    }

    fn update(&mut self, state: &mut AppModel, root: &ModelPath) -> bool {
        let (state, root) = ui::doc::document(state, root);
        self.pump(state, root)
    }

    /// Whether queued output needs another visual frame. A standalone terminal
    /// is woken by its event-loop proxy; an embedded terminal is pumped by its
    /// host independently of painting. Treating an absent standalone proxy as
    /// animation demand made every idle embedded terminal repaint the complete
    /// desktop continuously.
    fn needs_animation_frame(&self) -> bool {
        // A bounded pump deliberately leaves excess output queued. Keep frames
        // flowing until it is drained even when the reader's coalesced wake is
        // already in flight; otherwise a saturated producer could strand the
        // final queue behind `ControlFlow::Wait`.
        let pending = self
            .deferred_output
            .take()
            .or_else(|| self.output_rx.try_recv().ok());
        let queued = pending.is_some();
        self.deferred_output.set(pending);
        queued
    }

    /// The event-loop wake we requested has been delivered. Clear the coalescing
    /// flag so the reader can request the next one; the App's redraw drives
    /// `sync`, which drains the queued PTY bytes. The payload is unused — the
    /// wake itself is the signal.
    fn on_external_event(&mut self, _payload: &str, _state: &mut AppModel, _root: &ModelPath) {
        self.wake_pending.store(false, Ordering::Release);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `apply_layout` reflows the PTY immediately after Taffy calls
        // `measure`. Seed that measurement from the active theme here rather
        // than waiting for `draw`: at a large accessibility text scale, the
        // old default 18px row height produced too many blank PTY rows, and
        // the actual output landed above the first painted viewport.
        self.theme_font_size = theme.tokens.font_size_base;
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see the
        // `TaffyTreeExt` doc comment in `ui::widget::mod`), and
        // `Widget::build_layout` returns a bare `NodeId` across every
        // implementor, so there is nowhere to surface a theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
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

    fn apply_layout(&mut self, taffy: &TaffyTree, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        self.set_geometry(rect);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Terminal painting intentionally keeps grid, selection, cursor, and local find overlays in one ordered compositing pass"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        // Pull every visual parameter from design tokens. Nothing in this
        // method may hard-code a color, font size, or padding — the OS owns
        // the theme and swapping it must reflect in the rendered output.
        let font_size = text_renderer
            .renderer_input_style(&TextStyle::new(theme.tokens.font_size_base))
            .size;
        let palette = &theme.tokens.terminal_palette;
        // Horizontal gutter between the terminal's left edge and the first
        // glyph column. Sourced from the shared spacing scale (`s1` == 4px
        // in the default tokens) so the OS can tune it centrally.
        let padding_x = f64::from(theme.tokens.spacing.s1);
        // Cache the real gutter so pointer hit-testing (which has no `Theme`)
        // maps clicks to the same column the glyphs were painted at.
        self.last_padding_x = padding_x;

        // Cache the theme-derived font size so `measure` (which runs before
        // `draw` each frame and does not receive a theme) can size the grid
        // from the same value.
        self.font_size = font_size;

        // Refresh the body geometry from the theme and re-derive the inner grid
        // rect (inside the padding, minus the scrollbar gutter) — the same
        // `grid_content_rect` `set_geometry` and pointer hit-testing use, so the
        // grid origin is single-sourced. `set_geometry` and hit-testing read the
        // cached metrics — neither sees a `Theme` — mirroring `last_padding_x`.
        self.refresh_chrome_metrics(theme);

        // The terminal body fills the whole widget in the theme's terminal
        // background (a deliberate deep surface, not an accidental void). No
        // header/tab bar — the compositor owns the window-group decoration.
        scene.fill(
            Fill::NonZero,
            transform,
            palette.bg_default,
            None,
            &self.rect,
        );

        // Resolve the terminal font: the theme's named monospace face when it is
        // installed, else the generic monospace family — never the proportional
        // default cosmic-text would silently substitute. Measure that exact
        // family's advance so `char_w` equals the width every glyph is drawn at;
        // if the two disagreed, each colour run would be positioned on a grid
        // finer than its glyphs and the row would smear over itself.
        let mono = self.resolve_mono(theme, text_renderer);
        let (w, h) =
            text_renderer.measure_font(" ", font_size, None, TextWeight::Regular, false, &mono);
        self.char_w = f64::from(w).max(1.0);
        self.char_h = Self::cell_h(h);

        // `measure` happens before we can resolve the theme's actual
        // monospace face. Reconcile the PTY geometry after that exact
        // measurement as well: at 200% text scale a fallback/default advance
        // can be much narrower than the rendered mono advance, otherwise the
        // process sees more columns than fit on screen and its right side is
        // silently clipped. This is a no-op on steady frames.
        self.set_geometry(self.rect);
        let grid_rect = self.grid_rect;

        let visible_rows = (grid_rect.height() / self.char_h).floor() as usize;
        let max_top = self.grid.len().saturating_sub(visible_rows);
        // Alt screen has no scrollback; clamp the offset for the main buffer.
        let scroll = if self.alt_active {
            0
        } else {
            self.scroll_offset.min(max_top)
        };
        self.scroll_offset = scroll;
        let start_y = max_top.saturating_sub(scroll);

        // Cache for `handle_event`: pointer hit-tests translate logical y
        // into viewport-local rows by subtracting `grid_rect.y0` and dividing
        // by `char_h`. The selection-rendering pass below uses the offset
        // directly to find the row in `self.grid`.
        self.viewport_start_y = start_y;

        // Cursor/palette colour caches update every frame regardless of what
        // is painted (empty state, hidden cursor, cursor scrolled off screen).
        let cursor_color = cursor_from_palette(palette);
        self.last_cursor_color = cursor_color;
        self.last_fg_default = palette.fg_default;
        self.last_bg_default = palette.bg_default;

        // Honest first-run empty state: before the session has ever produced
        // output, paint a centred watermark instead of a blank void. Latched
        // via `ever_nonblank` so it never covers a legitimately cleared screen
        // (`clear`, `CSI 2J`) once the shell has printed anything.
        if !self.grid_is_blank() {
            self.ever_nonblank = true;
        }
        if !self.alt_active && !self.ever_nonblank {
            self.draw_empty_state(scene, transform, text_renderer, theme);
            self.draw_disconnected_status(scene, transform, text_renderer, theme);
            // An empty first-run surface is still fully interactive. In
            // particular, right-click's shared Copy/Paste menu must remain
            // visible rather than being hidden by this early return.
            if self.context_menu.is_open() {
                Widget::draw(
                    &mut self.context_menu,
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                );
            }
            self.draw_find_overlay(scene, transform, text_renderer, theme, presence);
            return;
        }

        // Images sit beneath selections and glyphs: text printed over a Kitty
        // placement remains legible, and selecting cells still has the same
        // visible feedback as plain terminal content. Their row anchors are
        // buffer-absolute, so scrollback and history retention move them with
        // the corresponding rows rather than pinning them to the viewport.
        self.draw_terminal_images(scene, transform, theme, grid_rect, start_y, visible_rows);

        // Paint the selection BEFORE the cell text so glyphs read on top of
        // the highlight.
        self.draw_selection_highlight(
            scene,
            transform,
            theme,
            grid_rect,
            padding_x,
            start_y,
            visible_rows,
        );
        self.draw_find_match_highlight(
            scene,
            transform,
            theme,
            grid_rect,
            padding_x,
            start_y,
            visible_rows,
        );

        let cell_style = TextStyle::new(font_size).with_family(mono);
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.draw_cell_rows(&mut ctx, &cell_style, start_y, visible_rows);
        self.draw_block_cursor(&mut ctx, &cell_style, start_y, visible_rows);

        // A themed scrollbar indicator in the reserved right gutter, shown only
        // when the buffer overflows the viewport.
        self.draw_scrollbar(scene, transform, theme, start_y, visible_rows);

        // The right-click Copy/Paste menu draws on top of everything when open.
        if self.context_menu.is_open() {
            Widget::draw(
                &mut self.context_menu,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
        self.draw_disconnected_status(scene, transform, text_renderer, theme);
        self.draw_find_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.init_crdt(ctx.state, ctx.path);

        // The menu is app-owned overlay content, so route every event to the
        // shared primitive while it is open and do not let keys/clicks fall
        // through to the PTY. This gives it Arrow/Enter/Escape keyboard
        // navigation in addition to pointer selection.
        if self.context_menu.is_open() {
            let mut menu_handled = false;
            Widget::handle_event(
                &mut self.context_menu,
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut menu_handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if let Some(index) = self.context_menu.take_selection() {
                self.activate_context_menu_item(index);
            }
            *ctx.handled |= menu_handled;
            return;
        }

        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && self.find_shortcut_matches(ctx.modifiers.state())
            && matches!(logical_key, Key::Character(key) if key.eq_ignore_ascii_case("f"))
        {
            self.open_find();
            *ctx.handled = true;
            return;
        }

        if self.find_open {
            self.handle_find_event(event, ctx);
            return;
        }

        match event {
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_wheel_event(delta, *ctx.modifiers, ctx.handled);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved_event(
                    *position,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.handled,
                );
            }
            WindowEvent::MouseInput {
                state: element_state,
                button,
                ..
            } => {
                self.handle_mouse_button_event(
                    *element_state,
                    *button,
                    *ctx.modifiers,
                    ctx.handled,
                );
            }
            WindowEvent::Focused(focused) if self.modes.focus_report => {
                self.write_input(if *focused { b"\x1b[I" } else { b"\x1b[O" });
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        physical_key,
                        text,
                        state: element_state,
                        ..
                    },
                ..
            } if *element_state == ElementState::Pressed => {
                self.handle_key_press_event(
                    logical_key,
                    *physical_key,
                    text.as_deref(),
                    *ctx.modifiers,
                    ctx.handled,
                );
            }
            // Preedit is owned by the platform IME and must not be sent to a
            // terminal process. Once it commits, however, it is ordinary
            // UTF-8 terminal input just like `KeyEvent::text`.
            WindowEvent::Ime(winit::event::Ime::Commit(text))
                if self.focused && !text.is_empty() =>
            {
                self.write_input(text.as_bytes());
                self.scroll_offset = 0;
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Terminal"
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }

    fn set_platform_conventions(&mut self, conventions: ui::PlatformConventions) {
        self.conventions = conventions;
        self.find_input.set_platform_conventions(conventions);
        self.context_menu.set_platform_conventions(conventions);
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_tab_input(&self) -> bool {
        true
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.focused.then(|| {
            let row = self
                .grid
                .len()
                .saturating_sub(self.height)
                .saturating_add(self.cursor_y)
                .saturating_sub(self.viewport_start_y);
            let x = self.grid_rect.x0 + self.cursor_x as f64 * self.char_w;
            let y = self.grid_rect.y0 + row as f64 * self.char_h;
            Rect::new(x, y, x + self.char_w.max(1.0), y + self.char_h.max(1.0))
        })
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        let value = self.current_visible_text();
        let graphics = self
            .image_placements
            .iter()
            .filter(|placement| placement.alternate_screen == self.alt_active)
            .count();
        let mut description = if graphics == 0 {
            "Terminal output shared with collaborators. Typing is sent to the live process. Control Shift C copies, Control Shift V pastes, and Shift Page Up or Shift Page Down reviews output history".to_string()
        } else {
            format!(
                "Terminal output shared with collaborators. Typing is sent to the live process. \
                 Control Shift C copies, Control Shift V pastes, and Shift Page Up or Shift Page \
                 Down reviews output history. {graphics} graphical placement(s) are visible when \
                 their image blobs are available"
            )
        };
        if self.is_disconnected() {
            description.push_str(". ");
            description.push_str(self.disconnected_message());
            description.push_str("; input actions are unavailable.");
        }
        let selection = self.visible_selection_bytes(&value);
        let mut transcript = AccessNode::new(Role::Terminal)
            .with_name(self.title.clone())
            .with_description(description)
            .with_value(value)
            .with_bounds(self.rect)
            .with_action_target(self.terminal_transcript_target())
            .with_state(AccessState {
                focusable: true,
                focused: self.focused,
                multiline: true,
                read_only: true,
                context_menu: true,
                ..Default::default()
            });
        transcript
            .children
            .extend(self.visible_link_accessibility());
        if self.find_open {
            let prefix = format!("terminal.find:{}", self.pane_id);
            let result = self.find_result_summary();
            transcript.children.push(
                AccessNode::new(Role::Group)
                    .with_name("Find in scrollback")
                    .with_description("Local only. Find text and matches are not shared with collaborators or exposed as terminal resources.")
                    .with_bounds(self.find_rect)
                    .with_children(vec![
                        AccessNode::new(Role::TextInput)
                            .with_name("Find in scrollback")
                            .with_value(self.find_input.value())
                            .with_action_target(format!("{prefix}:query"))
                            .with_bounds(self.find_input.get_rect())
                            .with_state(AccessState::new().focusable(true).focused(self.find_input.is_focused())),
                        AccessNode::new(Role::Button)
                            .with_name("Previous match")
                            .with_value(result.clone())
                            .with_action_target(format!("{prefix}:previous"))
                            .with_bounds(self.find_previous_rect)
                            .with_state(AccessState::new().focusable(true)),
                        AccessNode::new(Role::Button)
                            .with_name("Next match")
                            .with_value(result)
                            .with_action_target(format!("{prefix}:next"))
                            .with_bounds(self.find_next_rect)
                            .with_state(AccessState::new().focusable(true)),
                        AccessNode::new(Role::Button)
                            .with_name("Close find")
                            .with_action_target(format!("{prefix}:close"))
                            .with_bounds(self.find_close_rect)
                            .with_state(AccessState::new().focusable(true)),
                    ]),
            );
        }
        if let Some(menu) = self.context_menu_accessibility() {
            transcript.children.push(menu);
        }
        if self.is_disconnected() {
            transcript.children.push(
                AccessNode::new(Role::StaticText)
                    .with_hashed_key("terminal-lifecycle")
                    .with_name(self.disconnected_message())
                    .with_live(Politeness::Assertive),
            );
        }
        Some(match selection {
            Some((anchor, focus)) => transcript.with_text_selection(anchor, focus),
            None => transcript,
        })
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        match action {
            ui::a11y::Action::ShowContextMenu => {
                self.open_context_menu_at(self.rect.center());
                true
            }
            ui::a11y::Action::Focus | ui::a11y::Action::Activate => {
                self.focused = true;
                true
            }
            ui::a11y::Action::SetSelection { anchor, focus } => {
                self.set_visible_grapheme_selection(*anchor, *focus)
            }
            _ => false,
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        if target == self.terminal_transcript_target() {
            return self.perform_action(action, state, path);
        }
        let find_prefix = format!("terminal.find:{}:", self.pane_id);
        if let Some(control) = target.strip_prefix(&find_prefix) {
            match (control, action) {
                ("query", ui::a11y::Action::Focus | ui::a11y::Action::Activate) => {
                    self.open_find();
                    return true;
                }
                ("query", ui::a11y::Action::SetValue(value)) => {
                    self.open_find();
                    self.set_find_query(value);
                    return true;
                }
                ("previous", ui::a11y::Action::Activate) => {
                    self.step_find_match(true);
                    return true;
                }
                ("next", ui::a11y::Action::Activate) => {
                    self.step_find_match(false);
                    return true;
                }
                ("close", ui::a11y::Action::Activate) => {
                    self.close_find();
                    return true;
                }
                _ => return false,
            }
        }
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let link_prefix = format!("terminal.link:{}:", self.pane_id);
        if let Some(link_id) = target
            .strip_prefix(&link_prefix)
            .and_then(|id| id.parse::<u16>().ok())
        {
            let Some(url) = self.links.get(&link_id) else {
                return false;
            };
            open_url(url);
            return true;
        }
        let menu_prefix = format!("terminal.menu:{}:", self.pane_id);
        let Some(index) = target
            .strip_prefix(&menu_prefix)
            .and_then(|id| id.parse::<usize>().ok())
        else {
            return false;
        };
        self.context_menu.click_item(index);
        if let Some(index) = self.context_menu.take_selection() {
            self.activate_context_menu_item(index);
            true
        } else {
            false
        }
    }

    /// Paint the terminal, then overlay remote collaborators' selections and
    /// carets in their own colours on top (following the `TextInput` model).
    /// Grid content is shared via the CRDT; this adds the per-peer decoration.
    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(key) = ui::presence::capability::presence_key(self) {
            if let Some(list) = presence.selections.get(&key) {
                for (user, range) in list {
                    for rect in self.peer_selection_rects(range.start, range.end) {
                        presence.draw_peer_selection(scene, theme, transform, user, rect);
                    }
                }
            }
            if let Some(list) = presence.carets.get(&key) {
                for (user, caret) in list {
                    if let Some(rect) = self.peer_caret_rect(*caret) {
                        presence.draw_peer_caret(scene, theme, transform, user, rect);
                    }
                }
            }
        }
    }
}

impl ui::presence::capability::PublishesPresence for TerminalWidget {
    /// Scope this terminal's collaboration presence to a shared pane id (see
    /// `terminal_pane_id` in the CRDT) so both peers of a shared session file
    /// their caret/selection under the same key and can find each other's.
    fn presence_key(&self) -> Option<PresenceKey> {
        Some(PresenceKey::App {
            app: PermAppKind::Terminal,
            sub: self.pane_id.clone(),
        })
    }
    /// Broadcast the local view state to peers: the active text selection when
    /// there is one (so peers see what this user is selecting), otherwise the
    /// cursor position. The grid/scrollback itself rides the shared CRDT, so
    /// presence only needs to carry the per-user caret/selection.
    fn presence_payload(&self) -> Option<(PresenceKey, PresencePayload)> {
        let key = self.presence_key()?;
        let payload = if let Some(sel) = self.selection {
            let (a, b) = sel.ordered();
            PresencePayload::Selection {
                start: self.lin(a),
                end: self.lin(b),
            }
        } else {
            let cursor_abs = self.grid.len().saturating_sub(self.height) + self.cursor_y;
            PresencePayload::Caret {
                pos: self.lin(GridPos {
                    row: cursor_abs,
                    col: self.cursor_x,
                }),
            }
        };
        Some((key, payload))
    }
}

impl TerminalWidget {
    pub(crate) fn mark_framework_hosted(&mut self) {
        self.framework_hosted = true;
    }

    #[allow(
        dead_code,
        reason = "called from this crate's tests; dependents compile the library without cfg(test)"
    )]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.framework_hosted
    }

    #[allow(
        dead_code,
        reason = "called from this crate's tests; dependents compile the library without cfg(test)"
    )]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.framework_hosted = true;
    }

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Terminal>,
    ) {
        self.framework_dispatch = Some(dispatcher);
        self.mark_framework_hosted();
    }

    /// Document-level session action: the stored dispatcher when hosted, the
    /// local action host otherwise.
    pub(crate) fn apply_session_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::resource::TerminalAction,
    ) {
        let context =
            ui::tilo_actions::ActionContext::for_action("so.tilo.terminal", "Terminal", action);
        if let Some(dispatcher) = self.framework_dispatch.clone() {
            let _ = dispatcher.apply_now_with(doc, action, context);
        } else {
            self.session_actions.dispatch_with_context(
                doc,
                <crate::resource::Terminal as ui::tilo_resource::Resource>::ACTION_TYPE,
                action,
                context,
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn retains_framework_dispatcher(&self) -> bool {
        self.framework_dispatch.is_some()
    }

    #[cfg(test)]
    pub(crate) fn last_session_action(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.session_actions.last_dispatched()
    }
}

impl ui::resources::PublishesResources for TerminalWidget {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.framework_hosted {
            return vec![self.transcript_resource()];
        }
        vec![self.session_resource_with(Some(doc))]
    }
    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        let resources = (cursor.is_empty() && max_results > 0)
            .then(|| {
                if self.framework_hosted {
                    self.transcript_resource()
                } else {
                    self.session_resource_with(Some(doc))
                }
            })
            .into_iter()
            .collect();
        Some(ui::widget::ActionDirectoryResourcePage {
            resources,
            next_cursor: Vec::new(),
            exhausted: true,
        })
    }
    fn action_directory_revision(&self, _doc: &AutoCommit) -> Option<String> {
        use std::hash::{Hash, Hasher};

        let mut revision = std::collections::hash_map::DefaultHasher::new();
        self.pane_id.hash(&mut revision);
        self.title.hash(&mut revision);
        self.grid.len().hash(&mut revision);
        self.current_visible_rows().hash(&mut revision);
        self.scroll_offset.hash(&mut revision);
        self.is_disconnected().hash(&mut revision);
        for link_id in self.visible_link_ids() {
            link_id.hash(&mut revision);
            self.links.get(&link_id).hash(&mut revision);
        }
        Some(format!("{:016x}", revision.finish()))
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        fn find(resource: &Resource, id: &str) -> Option<Resource> {
            if resource.id.as_str() == id {
                return Some(resource.clone());
            }
            for child in &resource.children {
                if let Some(found) = find(child, id) {
                    return Some(found);
                }
            }
            None
        }
        if self.framework_hosted {
            if id == self.terminal_resource_id() {
                return None;
            }
            let transcript = self.transcript_resource();
            if let Some(found) = find(&transcript, id) {
                return Some(found);
            }
            return self.linked_resource(id);
        }
        let session = self.session_resource_with(Some(doc));
        if id == self.terminal_resource_id() || id == session.id.as_str() {
            return Some(session);
        }
        if let Some(found) = session.children.iter().find_map(|child| find(child, id)) {
            return Some(found);
        }
        self.linked_resource(id)
    }
}

impl TerminalWidget {
    fn linked_resource(&self, id: &str) -> Option<ui::tilo_actions::Resource> {
        let prefix = format!("terminal.link:{}:", self.pane_id);
        let link_id = id.strip_prefix(&prefix)?.parse::<u16>().ok()?;
        self.links
            .get(&link_id)
            .map(|url| self.link_resource(link_id, url))
    }
}

impl ui::widget::controls::ActionControls for TerminalWidget {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.gesture_effects.pop_front().or_else(|| {
            self.menu_effects
                .values_mut()
                .find_map(ui::widget::ActionEmitter::take)
        })
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        for (index, emitter) in &self.menu_effects {
            let id = format!("terminal.menu:{}:{index}", self.pane_id);
            bindings.retain(|binding| binding.control_id != id);
            bindings.push(emitter.metadata(id));
        }
        bindings
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<ui::tilo_actions::ActionAddress>,
    ) {
        self.allowed_menu_actions = Some(allowed.clone());
        let mut availability = Vec::with_capacity(self.menu_effects.len());
        for (index, emitter) in &mut self.menu_effects {
            emitter.refresh_availability(allowed);
            availability.push((*index, emitter.is_available()));
        }
        for (index, available) in availability {
            self.context_menu.set_item_enabled(index, available);
        }
    }
}

impl ui::history_mode::capability::DocumentHistory for TerminalWidget {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl TerminalWidget {
    /// Route the local find overlay before terminal input. Once Find is open,
    /// keystrokes must not leak into the foreground process: the query is a
    /// private viewport aid, not shell input. Enter cycles matches, Shift
    /// Enter reverses, and Escape restores direct terminal typing.
    fn handle_find_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor_pos =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.close_find();
                    *ctx.handled = true;
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    self.step_find_match(ctx.modifiers.state().shift_key());
                    *ctx.handled = true;
                    return;
                }
                _ => {}
            }
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if self.find_previous_rect.contains(self.last_cursor_pos) {
                self.step_find_match(true);
                *ctx.handled = true;
                return;
            }
            if self.find_next_rect.contains(self.last_cursor_pos) {
                self.step_find_match(false);
                *ctx.handled = true;
                return;
            }
            if self.find_close_rect.contains(self.last_cursor_pos) {
                self.close_find();
                *ctx.handled = true;
                return;
            }
        }
        let before = self.find_input.value().to_string();
        let mut input_handled = false;
        Widget::handle_event(
            &mut self.find_input,
            event,
            &mut ui::widget::EventCtx {
                handled: &mut input_handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if self.find_input.value() != before {
            self.refresh_find_matches();
        }
        // A find overlay is a focus boundary. Even navigation keys and clicks
        // outside its panel must not reach a running shell behind it.
        *ctx.handled = true;
    }

    /// Helper for [`Widget::handle_event`]'s `WindowEvent::MouseWheel` arm.
    fn handle_wheel_event(
        &mut self,
        delta: &MouseScrollDelta,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        if !self.rect.contains(self.last_cursor_pos) {
            return;
        }
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => (*y * 3.0).round() as i32,
            MouseScrollDelta::PixelDelta(pos) => {
                self.wheel_remainder_px += pos.y;
                let lines = (self.wheel_remainder_px / self.char_h.max(1.0)).trunc() as i32;
                self.wheel_remainder_px -= f64::from(lines) * self.char_h.max(1.0);
                lines
            }
        };
        if lines == 0 {
            return;
        }
        if self.modes.mouse_report
            && !modifiers.state().shift_key()
            && self.grid_rect.contains(self.last_cursor_pos)
        {
            // xterm encodes wheel up/down as virtual buttons 4/5. One report
            // per platform wheel event avoids turning high-resolution touchpad
            // deltas into an input flood; the application owns acceleration.
            self.report_mouse(if lines > 0 { 64 } else { 65 }, true, modifiers);
            *handled = true;
            return;
        }
        if self.alt_active && !self.modes.mouse_report {
            // In the alternate screen (pager / editor) forward wheel
            // notches as arrow keys so `less`/`man` scroll naturally.
            // Honour DECCKM so apps that requested application cursor
            // keys (`vim`, `less -R`) receive the SS3 form they expect.
            let seq: &[u8] = match (lines > 0, self.modes.app_cursor_keys) {
                (true, false) => b"\x1b[A",
                (false, false) => b"\x1b[B",
                (true, true) => b"\x1bOA",
                (false, true) => b"\x1bOB",
            };
            let reps = lines.unsigned_abs() as usize;
            let mut out = Vec::new();
            for _ in 0..reps {
                out.extend_from_slice(seq);
            }
            self.write_input(&out);
        } else if !self.alt_active {
            if lines > 0 {
                self.scroll_up(lines as usize);
            } else {
                self.scroll_down((-lines) as usize);
            }
        }
        *handled = true;
    }

    /// Helper for [`Widget::handle_event`]'s `WindowEvent::CursorMoved` arm.
    fn handle_cursor_moved_event(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        // Always cache pointer position so the next MouseInput::Pressed
        // event can hit-test without needing the position from
        // winit (Pressed arrives without coordinates).
        let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_cursor_pos = logical;
        if self.scrollbar.is_dragging() {
            let track = self.scrollbar_track();
            let metrics = self.scrollbar_metrics();
            if let Some(offset) = self.scrollbar.drag_to(track, metrics, logical) {
                self.set_scrollbar_offset(offset);
                *handled = true;
            }
            return;
        }
        let track = self.scrollbar_track();
        let metrics = self.scrollbar_metrics();
        self.scrollbar.update_hover(track, metrics, logical);
        if self.modes.mouse_report
            && !modifiers.state().shift_key()
            && (self.modes.mouse_tracking == 1003
                || (self.modes.mouse_tracking == 1002 && self.mouse_buttons != 0))
            && self.grid_rect.contains(logical)
        {
            let button = self.held_mouse_code().unwrap_or(3) + 32;
            self.report_mouse(button, true, modifiers);
            *handled = true;
            return;
        }
        if self.drag_select.is_dragging() && self.selection.is_some() {
            let pos = self.pos_from_logical(logical);
            self.extend_drag_to(pos);
            *handled = true;
        }
    }

    fn handle_mouse_button_event(
        &mut self,
        element_state: ElementState,
        button: MouseButton,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        let Some((mask, code)) = (match button {
            MouseButton::Left => Some((1, 0)),
            MouseButton::Middle => Some((2, 1)),
            MouseButton::Right => Some((4, 2)),
            _ => None,
        }) else {
            return;
        };

        // Shift is the conventional terminal-emulator override: it keeps
        // selection, primary paste, and the context menu reachable while a TUI
        // owns ordinary pointer input.
        let pressed = matches!(element_state, ElementState::Pressed);
        if button == MouseButton::Left && !self.alt_active {
            if pressed && self.scrollbar_visible(self.current_visible_rows()) {
                let track = self.scrollbar_track();
                let metrics = self.scrollbar_metrics();
                match self.scrollbar.press(track, metrics, self.last_cursor_pos) {
                    PressOutcome::Thumb => {
                        *handled = true;
                        return;
                    }
                    PressOutcome::Page(offset) => {
                        self.set_scrollbar_offset(offset);
                        *handled = true;
                        return;
                    }
                    PressOutcome::Miss => {}
                }
            } else if !pressed && self.scrollbar.release() {
                *handled = true;
                return;
            }
        }
        if self.modes.mouse_report
            && !modifiers.state().shift_key()
            && (self.grid_rect.contains(self.last_cursor_pos)
                || (!pressed && self.mouse_buttons & mask != 0))
        {
            self.report_mouse(code, pressed, modifiers);
            if pressed {
                self.mouse_buttons |= mask;
            } else {
                self.mouse_buttons &= !mask;
            }
            *handled = true;
            return;
        }

        match button {
            MouseButton::Left => {
                self.handle_left_click_event(element_state, modifiers, handled);
            }
            MouseButton::Middle if matches!(element_state, ElementState::Pressed) => {
                self.handle_middle_click_event(handled);
            }
            MouseButton::Right if matches!(element_state, ElementState::Pressed) => {
                self.handle_right_click_event(handled);
            }
            _ => {}
        }
    }

    fn held_mouse_code(&self) -> Option<u8> {
        if self.mouse_buttons & 1 != 0 {
            Some(0)
        } else if self.mouse_buttons & 2 != 0 {
            Some(1)
        } else if self.mouse_buttons & 4 != 0 {
            Some(2)
        } else {
            None
        }
    }

    /// Forward one xterm-compatible mouse report. SGR mode is preferred by
    /// modern TUIs and has unbounded coordinates; the legacy X10 form remains
    /// for older applications and is safely capped to its byte range.
    fn report_mouse(&self, mut button: u8, pressed: bool, modifiers: winit::event::Modifiers) {
        let mods = modifiers.state();
        if mods.shift_key() {
            button |= 4;
        }
        if mods.alt_key() {
            button |= 8;
        }
        if mods.control_key() {
            button |= 16;
        }
        let x = ((((self.last_cursor_pos.x - self.grid_rect.x0) / self.char_w.max(1.0))
            .floor()
            .max(0.0) as usize)
            + 1)
        .min(self.width.max(1));
        let y = ((((self.last_cursor_pos.y - self.grid_rect.y0) / self.char_h.max(1.0))
            .floor()
            .max(0.0) as usize)
            + 1)
        .min(self.current_visible_rows());
        if self.modes.mouse_sgr {
            let final_byte = if pressed { 'M' } else { 'm' };
            self.write_input(format!("\x1b[<{button};{x};{y}{final_byte}").as_bytes());
        } else {
            let legacy_button = if pressed { button } else { 3 | (button & 0x1c) };
            let x = x.min(223) as u8 + 32;
            let y = y.min(223) as u8 + 32;
            self.write_input(&[0x1b, b'[', b'M', legacy_button + 32, x, y]);
        }
    }

    /// Helper for [`Widget::handle_event`]'s left-button `WindowEvent::MouseInput` arm.
    fn handle_left_click_event(
        &mut self,
        element_state: ElementState,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        // Application mouse reporting was already handled by
        // `handle_mouse_button_event` above. Reaching this helper therefore
        // means reporting is off or Shift requested the conventional local
        // selection override.
        match element_state {
            ElementState::Pressed if self.grid_rect.contains(self.last_cursor_pos) => {
                let shift = modifiers.state().shift_key();
                let intent = self.drag_select.press(self.last_cursor_pos, shift);
                let pos = self.pos_from_logical(self.last_cursor_pos);
                self.begin_selection(intent.extend, intent.granularity, pos);
                *handled = true;
            }
            ElementState::Released if self.drag_select.release() => {
                self.finish_selection();
                *handled = true;
            }
            _ => {}
        }
    }

    /// Helper for [`Widget::handle_event`]'s middle-button `WindowEvent::MouseInput` arm.
    fn handle_middle_click_event(&mut self, handled: &mut bool) {
        // X11/Wayland primary-selection paste: middle-click inserts the
        // currently-selected text (from any app), matching every
        // terminal. Snaps the viewport back to the live bottom.
        if self.grid_rect.contains(self.last_cursor_pos) {
            if let Some(text) = clipboard_paste_primary() {
                let session = self.session_resource();
                self.queue_gesture_effect(
                    &session,
                    "paste-primary",
                    Self::paste_action_arguments(text),
                );
            }
            *handled = true;
        }
    }

    /// Helper for [`Widget::handle_event`]'s right-button `WindowEvent::MouseInput` arm.
    fn handle_right_click_event(&mut self, handled: &mut bool) {
        // Right-click opens the Copy/Paste context menu (Copy only when
        // there's a selection). The window-group decoration is the OS's;
        // this menu is app content, so it uses the shared toolkit primitive.
        if self.grid_rect.contains(self.last_cursor_pos) {
            self.open_context_menu_at_cursor();
            *handled = true;
        }
    }

    /// Find uses a desktop command chord, leaving terminal Ctrl+F and
    /// Meta-Ctrl+F available to the foreground program on every platform.
    fn find_shortcut_matches(&self, modifiers: winit::keyboard::ModifiersState) -> bool {
        if !self.conventions.command_modifier_down(modifiers) || modifiers.alt_key() {
            return false;
        }
        match self.conventions.platform() {
            ui::Platform::MacOs => !modifiers.control_key() && !modifiers.shift_key(),
            _ => modifiers.shift_key() && !modifiers.super_key(),
        }
    }

    /// Helper for [`Widget::handle_event`]'s `WindowEvent::KeyboardInput` arm.
    fn handle_key_press_event(
        &mut self,
        logical_key: &Key,
        physical_key: PhysicalKey,
        text: Option<&str>,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        // Escape dismisses an open context menu before the app sees it.
        if self.context_menu.is_open()
            && let Key::Named(NamedKey::Escape) = logical_key
        {
            self.context_menu.close();
            *handled = true;
            return;
        }
        let mods = modifiers.state();
        if !mods.control_key()
            && !mods.alt_key()
            && !mods.super_key()
            && (matches!(logical_key, Key::Named(NamedKey::ContextMenu))
                || (mods.shift_key() && matches!(logical_key, Key::Named(NamedKey::F10))))
        {
            self.open_context_menu_at(self.rect.center());
            *handled = true;
            return;
        }
        // Ctrl+Shift+C → clipboard copy; Ctrl+Shift+V → paste.
        // Both must be matched BEFORE the regular Ctrl+C → SIGINT
        // path or the user would lose the ability to interrupt.
        if mods.control_key()
            && mods.shift_key()
            && let Key::Character(c) = logical_key
        {
            if c.as_str().eq_ignore_ascii_case("c") {
                if let Some(text) = self.selected_text()
                    && !text.is_empty()
                {
                    let transcript = self.transcript_resource();
                    self.queue_gesture_effect(
                        &transcript,
                        "copy-selection",
                        serde_json::Map::new(),
                    );
                }
                *handled = true;
                return;
            }
            if c.as_str().eq_ignore_ascii_case("v") {
                if let Some(text) = clipboard_paste() {
                    let session = self.session_resource();
                    self.queue_gesture_effect(
                        &session,
                        "paste",
                        Self::paste_action_arguments(text),
                    );
                }
                *handled = true;
                return;
            }
        }
        // Shift+Insert remains the broadly compatible terminal paste gesture
        // (especially useful over remote-desktop/keymaps where Ctrl+Shift+V is
        // intercepted). It uses the same bracketed-paste-safe path.
        if mods.shift_key() && matches!(logical_key, Key::Named(NamedKey::Insert)) {
            if let Some(text) = clipboard_paste() {
                let session = self.session_resource();
                self.queue_gesture_effect(&session, "paste", Self::paste_action_arguments(text));
            }
            *handled = true;
            return;
        }

        // Conventional terminal scrollback navigation requires Shift. Plain
        // Page Up/Down must reach readline, pagers and other foreground apps;
        // stealing them made the emulator behave unlike established desktop
        // terminals. The alternate screen always receives the keys because a
        // full-screen app owns its own viewport.
        if !self.alt_active
            && mods.shift_key()
            && let Key::Named(named) = logical_key
        {
            let page = self.current_visible_rows().saturating_sub(1).max(1);
            match named {
                NamedKey::PageUp => {
                    self.scroll_up(page);
                    *handled = true;
                    return;
                }
                NamedKey::PageDown => {
                    self.scroll_down(page);
                    *handled = true;
                    return;
                }
                NamedKey::Home if mods.shift_key() => {
                    self.scroll_to_top();
                    *handled = true;
                    return;
                }
                NamedKey::End if mods.shift_key() => {
                    self.scroll_to_bottom();
                    *handled = true;
                    return;
                }
                _ => {}
            }
        }

        self.deliver_mapped_key_bytes(logical_key, physical_key, text, modifiers, handled);
    }

    /// Map a key press to PTY bytes and write them. Physical Enter/Space
    /// fill in when the logical key and `text` are empty (control-socket
    /// and some compositor paths).
    fn deliver_mapped_key_bytes(
        &mut self,
        logical_key: &Key,
        physical_key: PhysicalKey,
        text: Option<&str>,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        let mut bytes = self.map_key(logical_key, modifiers);
        if bytes.is_none() {
            bytes = Self::map_physical_key(physical_key);
        }
        if bytes.is_none()
            && let Some(t) = text
            && !t.chars().any(|c| c as u32 == 0x23CE || c as u32 == 0x21B5)
        {
            bytes = Some(t.as_bytes().to_vec());
        }
        if let Some(b) = bytes {
            let _ = self.try_write_input(&b);
            // Typing snaps the viewport back to the live bottom.
            self.scroll_offset = 0;
            *handled = true;
        }
    }

    /// Re-read `line_ids[start..len]` from `state` into the `grid`/
    /// `grid_attrs` mirror, choosing `start` by whether a full or
    /// incremental re-read is needed. Helper for [`Widget::sync`].
    fn refresh_grid_mirror(
        &mut self,
        state: &AutoCommit,
        line_ids: &[ObjId],
        len: usize,
        full_reread: bool,
    ) {
        // Choose the first line to re-read. A full pass rebuilds the mirror;
        // the incremental pass keeps the mirror aligned by dropping the oldest
        // lines that were trimmed off the CRDT front, then re-reads from the
        // *previous* frame's screen top through the end — that spans every row
        // the emulator could have touched (the old screen scrolled into
        // history plus everything freshly appended).
        let start = if full_reread {
            self.grid.clear();
            self.grid_attrs.clear();
            self.trimmed_front = 0;
            0
        } else {
            let trimmed = self.trimmed_front;
            let drop = trimmed.min(self.grid.len());
            self.grid.drain(0..drop);
            self.grid_attrs.drain(0..drop);
            self.trimmed_front = 0;
            self.synced_len
                .saturating_sub(self.height)
                .saturating_sub(trimmed)
                .min(self.grid.len())
        };
        self.grid.resize(len, String::new());
        self.grid_attrs.resize(len, Vec::new());

        let end = len.min(line_ids.len());
        for (i, line_id) in line_ids.iter().enumerate().take(end).skip(start) {
            if let Ok(Some((Value::Object(ObjType::Text), text_id))) = state.get(line_id, "text")
                && let Ok(text) = state.text(&text_id)
                && let Some(slot) = self.grid.get_mut(i)
            {
                *slot = text;
            }
            if let Ok(Some((Value::Scalar(v), _))) = state.get(line_id, "attrs")
                && let automerge::ScalarValue::Bytes(bytes) = v.as_ref()
                && let Some(slot) = self.grid_attrs.get_mut(i)
            {
                *slot = bytes.chunks_exact(ATTR_BYTES).map(Attr::unpack).collect();
            }
        }
        self.mirror_valid = true;
        self.synced_len = len;
    }

    /// Refresh the window title + hyperlink registry from the CRDT so a
    /// collaborator picking up a remote OSC 0/2/8 sees the same title and
    /// can resolve the same links. The remote document is still treated as
    /// untrusted: revalidate title text and cap the link materialisation rather
    /// than assuming every collaborator used this build. Helper for
    /// [`Widget::sync`].
    fn refresh_title_and_links(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Ok(Some((Value::Scalar(v), _))) = state.get(root, "terminal_title")
            && let Some(t) = v.as_str()
            && let Some(t) = safe_osc_text(t.as_bytes(), MAX_OSC_TITLE_BYTES)
        {
            self.title = t;
        }
        if let Some(links_id) = self.links_id.clone() {
            self.links.clear();
            self.next_link_id = 0;
            for key in state.keys(&links_id).take(MAX_OSC_LINKS) {
                if let Ok(Some((Value::Scalar(v), _))) = state.get(&links_id, &key)
                    && let (Ok(id), Some(url)) = (key.parse::<u16>(), v.as_str())
                    && let Some(url) = safe_osc_text(url.as_bytes(), MAX_OSC_LINK_BYTES)
                {
                    self.next_link_id = self.next_link_id.max(id);
                    self.links.insert(id, url);
                }
            }
        }
    }

    fn refresh_image_placements(&mut self, state: &AutoCommit) {
        let Some(images_id) = &self.images_id else {
            return;
        };
        let scalar_u64 = |id: &ObjId, key: &str| -> Option<u64> {
            let (Value::Scalar(value), _) = state.get(id, key).ok()?? else {
                return None;
            };
            value.as_u64()
        };
        let scalar_string = |id: &ObjId, key: &str| -> Option<String> {
            let (Value::Scalar(value), _) = state.get(id, key).ok()?? else {
                return None;
            };
            value.as_str().map(ToOwned::to_owned)
        };
        let scalar_bool = |id: &ObjId, key: &str| -> Option<bool> {
            let (Value::Scalar(value), _) = state.get(id, key).ok()?? else {
                return None;
            };
            match value.as_ref() {
                automerge::ScalarValue::Boolean(value) => Some(*value),
                _ => None,
            }
        };
        let mut placements = Vec::with_capacity(state.length(images_id));
        for index in 0..state.length(images_id) {
            let Ok(Some((Value::Object(ObjType::Map), id))) = state.get(images_id, index) else {
                continue;
            };
            let (Some(hash), Some(mime)) = (scalar_string(&id, "hash"), scalar_string(&id, "mime"))
            else {
                continue;
            };
            placements.push(TerminalImagePlacement {
                hash,
                mime,
                byte_len: scalar_u64(&id, "byte_len").unwrap_or(0),
                row: scalar_u64(&id, "row")
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(0),
                col: scalar_u64(&id, "col")
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(0),
                columns: scalar_u64(&id, "columns")
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(0),
                rows: scalar_u64(&id, "rows")
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(0),
                pixel_width: scalar_u64(&id, "pixel_width")
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(0),
                pixel_height: scalar_u64(&id, "pixel_height")
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(0),
                protocol_id: scalar_u64(&id, "protocol_id")
                    .and_then(|value| u32::try_from(value).ok()),
                alternate_screen: scalar_bool(&id, "alternate_screen").unwrap_or(false),
            });
        }
        self.image_placements = placements;
    }

    /// Refresh the cursor position from the CRDT, converting the
    /// buffer-absolute row the emulator wrote into a viewport-local row.
    /// `active_len` is the length of the currently active buffer's line
    /// list. Helper for [`Widget::sync`].
    fn refresh_cursor_from_crdt(&mut self, state: &AutoCommit, active_len: usize) {
        if let Some(cursor_id) = self.cursor_id.as_ref() {
            if let Ok(Some((Value::Scalar(v), _))) = state.get(cursor_id, "x")
                && let Some(x) = v.as_i64()
            {
                self.cursor_x = x as usize;
            }
            if let Ok(Some((Value::Scalar(v), _))) = state.get(cursor_id, "y")
                && let Some(y) = v.as_i64()
            {
                let abs_y = y as usize;
                let history_offset = active_len.saturating_sub(self.height);
                self.cursor_y = abs_y
                    .saturating_sub(history_offset)
                    .min(self.height.saturating_sub(1));
            }
        }
    }

    /// Paint the flow-selection highlight, if any, before the cell text so
    /// glyphs read on top of it. Selection rows are buffer-absolute, so a
    /// row only paints when it falls inside the current viewport window
    /// `[start_y, start_y + visible_rows)`, translated to an on-screen row.
    /// Helper for [`Widget::draw`].
    fn draw_selection_highlight(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        grid_rect: Rect,
        padding_x: f64,
        start_y: usize,
        visible_rows: usize,
    ) {
        let Some(sel) = self.selection else { return };
        // A slightly stronger tint than a hover veneer so a live selection
        // reads clearly over black, still translucent so glyphs show.
        let sel_color = theme.accent_primary.with_alpha_factor(0.35);
        for (row, start_col, end_col) in selection_row_spans(&sel, self.width) {
            if row < start_y || row >= start_y + visible_rows {
                continue;
            }
            let vrow = row - start_y;
            let py = (grid_rect.y0 + vrow as f64 * self.char_h).floor();
            let x0 = grid_rect.x0 + padding_x + start_col as f64 * self.char_w;
            let x1 = grid_rect.x0 + padding_x + end_col as f64 * self.char_w;
            let band = Rect::new(x0, py, x1, py + self.char_h);
            scene.fill(Fill::NonZero, transform, sel_color, None, &band);
        }
    }

    /// Paint only the active find match. The index remains local and the
    /// overlay never serializes query/match information into the shared
    /// transcript; this is a visual locator over already-rendered cells.
    fn draw_find_match_highlight(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        grid_rect: Rect,
        padding_x: f64,
        start_y: usize,
        visible_rows: usize,
    ) {
        let Some(found) = self.find_matches.get(self.find_current).copied() else {
            return;
        };
        if found.row < start_y || found.row >= start_y.saturating_add(visible_rows) {
            return;
        }
        let row = found.row - start_y;
        let x0 = grid_rect.x0 + padding_x + found.start_col as f64 * self.char_w;
        let x1 = grid_rect.x0 + padding_x + found.end_col as f64 * self.char_w;
        let y0 = grid_rect.y0 + row as f64 * self.char_h;
        let marker = Rect::new(x0, y0, x1.max(x0 + self.char_w), y0 + self.char_h);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.semantic.warning.with_alpha_factor(0.38),
            None,
            &marker,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            theme.tokens.colors.semantic.warning,
            None,
            &marker,
        );
    }

    /// Shared-text-input find overlay. On narrow or large-text layouts the
    /// field occupies its own row and the three explicit controls stack below
    /// it, avoiding tiny/overlapping targets at 200% text.
    #[allow(
        clippy::too_many_lines,
        reason = "The overlay owns a single responsive geometry and paint path so its focus targets cannot diverge from its visible controls"
    )]
    fn draw_find_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if !self.find_open {
            return;
        }
        let scale = f64::from(theme.text_scale).clamp(1.0, 2.0);
        let compact = self.rect.width() < 620.0 || scale >= 1.5;
        let margin = 8.0 * scale.min(1.25);
        let panel_w = (340.0 * scale)
            .min(self.rect.width() - margin * 2.0)
            .max(0.0);
        let panel_h = if compact {
            104.0 * scale.min(1.35)
        } else {
            42.0 * scale
        };
        let x1 = self.rect.x1 - margin;
        self.find_rect = Rect::new(
            x1 - panel_w,
            self.rect.y0 + margin,
            x1,
            self.rect.y0 + margin + panel_h,
        );
        let panel = RoundedRect::from_rect(self.find_rect, f64::from(theme.tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &panel,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_focus,
            None,
            &panel,
        );

        let gap = 6.0 * scale.min(1.25);
        let button = 28.0 * scale.min(1.25);
        if compact {
            self.find_input.apply_layout_external(Rect::new(
                self.find_rect.x0 + gap,
                self.find_rect.y0 + gap,
                self.find_rect.x1 - gap,
                self.find_rect.y0 + gap + 34.0 * scale.min(1.25),
            ));
            let y = self.find_input.get_rect().y1 + gap;
            self.find_previous_rect = Rect::new(
                self.find_rect.x0 + gap,
                y,
                self.find_rect.x0 + gap + button,
                y + button,
            );
            self.find_next_rect = Rect::new(
                self.find_previous_rect.x1 + gap,
                y,
                self.find_previous_rect.x1 + gap + button,
                y + button,
            );
            self.find_close_rect = Rect::new(
                self.find_rect.x1 - gap - button,
                y,
                self.find_rect.x1 - gap,
                y + button,
            );
        } else {
            self.find_close_rect = Rect::new(
                self.find_rect.x1 - gap - button,
                self.find_rect.y0 + (panel_h - button) / 2.0,
                self.find_rect.x1 - gap,
                self.find_rect.y0 + f64::midpoint(panel_h, button),
            );
            self.find_next_rect = Rect::new(
                self.find_close_rect.x0 - gap - button,
                self.find_close_rect.y0,
                self.find_close_rect.x0 - gap,
                self.find_close_rect.y1,
            );
            self.find_previous_rect = Rect::new(
                self.find_next_rect.x0 - gap - button,
                self.find_close_rect.y0,
                self.find_next_rect.x0 - gap,
                self.find_close_rect.y1,
            );
            self.find_input.apply_layout_external(Rect::new(
                self.find_rect.x0 + gap,
                self.find_rect.y0 + gap,
                self.find_previous_rect.x0 - gap,
                self.find_rect.y1 - gap,
            ));
        }
        self.find_input.draw(scene, transform, tr, theme, presence);
        if self.find_input.value().is_empty() && !self.find_input.is_focused() {
            tr.draw_text(
                scene,
                "Find in scrollback",
                12.0,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        self.find_input.get_rect().x0 + 10.0,
                        self.find_input.get_rect().center().y - 7.0,
                    )),
                None,
            );
        }
        for (rect, icon) in [
            (self.find_previous_rect, Icon::ChevronUp),
            (self.find_next_rect, Icon::ChevronDown),
            (self.find_close_rect, Icon::X),
        ] {
            let active = rect.contains(self.last_cursor_pos);
            let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.sm));
            scene.fill(
                Fill::NonZero,
                transform,
                if active {
                    theme.tokens.colors.overlay_medium
                } else {
                    theme.tokens.colors.overlay_subtle
                },
                None,
                &shape,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_standard,
                None,
                &shape,
            );
            draw_icon(
                scene,
                transform,
                rect.inset(5.0),
                icon,
                theme.tokens.colors.text_primary,
                1.5,
            );
        }
        let count = if self.find_query.is_empty() {
            "Type to find".to_string()
        } else if self.find_matches.is_empty() {
            "No matches".to_string()
        } else if self.find_matches.len() == MAX_FIND_MATCHES {
            format!("{} / {}+", self.find_current + 1, MAX_FIND_MATCHES)
        } else {
            format!("{} / {}", self.find_current + 1, self.find_matches.len())
        };
        let count_x = if compact {
            self.find_next_rect.x1 + gap + 2.0
        } else {
            self.find_input.get_rect().x1 - 58.0 * scale
        };
        let count_y = if compact {
            self.find_next_rect.center().y - 7.0
        } else {
            self.find_input.get_rect().center().y - 7.0
        };
        tr.draw_text(
            scene,
            &count,
            10.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((count_x, count_y)),
            None,
        );
    }

    /// Decode and paint graphics only when their buffer rows are visible.  The
    /// cache holds successful decodes only: a blob which has not arrived at a
    /// collaborator yet is retried on a later frame, rather than being latched
    /// as permanently absent.  Images are clipped to the grid so a malformed
    /// placement cannot paint over terminal chrome or another tile.
    fn draw_terminal_images(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        grid_rect: Rect,
        start_y: usize,
        visible_rows: usize,
    ) {
        const MAX_DECODE_CACHE_ENTRIES: usize = 128;
        let placements: Vec<_> = self
            .image_placements
            .iter()
            .filter(|placement| placement.alternate_screen == self.alt_active)
            .cloned()
            .collect();
        if placements.is_empty() || visible_rows == 0 {
            return;
        }

        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &grid_rect,
        );
        let viewport_end = start_y.saturating_add(visible_rows);
        for placement in placements {
            let rows = usize::try_from(placement.rows.clamp(1, 1024)).unwrap_or(1);
            let columns = usize::try_from(placement.columns.clamp(1, 1024)).unwrap_or(1);
            let placement_end = placement.row.saturating_add(rows);
            let outside_horizontal = placement.col >= self.width;
            let outside_vertical = placement_end <= start_y || placement.row >= viewport_end;
            if outside_horizontal || outside_vertical {
                continue;
            }
            let left = grid_rect.x0 + self.last_padding_x + placement.col as f64 * self.char_w;
            let top = grid_rect.y0 + placement.row.saturating_sub(start_y) as f64 * self.char_h;
            let right = (left + columns as f64 * self.char_w).min(grid_rect.x1);
            let bottom = (top + rows as f64 * self.char_h).min(grid_rect.y1);
            let rect = Rect::new(left, top, right, bottom);
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }

            // The MIME and dimensions form part of this local cache key: raw
            // Kitty payloads have no self-describing header, and a replicated
            // descriptor must not reuse a decoded image under altered metadata.
            let cache_key = format!(
                "{}:{}:{}:{}",
                placement.hash, placement.mime, placement.pixel_width, placement.pixel_height
            );
            if !self.image_cache.contains_key(&cache_key) {
                if self.image_cache.len() >= MAX_DECODE_CACHE_ENTRIES {
                    self.image_cache.clear();
                }
                if let Some(bytes) = self.content.read(&placement.hash)
                    && placement.byte_len == u64::try_from(bytes.len()).unwrap_or(u64::MAX)
                    && let Some(image) = decode_for_rendering(&placement, &bytes)
                {
                    self.image_cache.insert(cache_key.clone(), image);
                }
            }
            if let Some(image) = self.image_cache.get_mut(&cache_key) {
                image.paint_at(scene, transform, rect);
            } else {
                // Honest fallback for an unavailable blob or unsupported /
                // invalid image.  It reserves the promised cells without
                // pretending an image rendered successfully.
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    theme.text_tertiary.with_alpha_factor(0.55),
                    None,
                    &rect,
                );
            }
        }
        scene.pop_layer();
    }

    /// Paint every visible row's cell text as colour runs (consecutive cells
    /// sharing one [`Attr`] drawn in a single [`Self::draw_run`] call), in
    /// `cell_style`'s size and monospace family. Helper for [`Widget::draw`].
    fn draw_cell_rows(
        &self,
        ctx: &mut PaintCtx<'_>,
        cell_style: &TextStyle,
        start_y: usize,
        visible_rows: usize,
    ) {
        let grid_rect = self.grid_rect;
        let padding_x = self.last_padding_x;
        for (y, line_text) in self
            .grid
            .iter()
            .skip(start_y)
            .take(visible_rows)
            .enumerate()
        {
            let py = (grid_rect.y0 + y as f64 * self.char_h).floor();
            let attrs_idx = y + start_y;
            let Some(attrs) = self.grid_attrs.get(attrs_idx) else {
                continue;
            };

            let mut current_run = String::new();
            let mut current_attr: Option<Attr> = None;
            let mut run_start_cell_idx = 0;

            let mut cell_idx = 0;
            for c in line_text.chars() {
                let c_w = c.width().unwrap_or(1);
                if cell_idx >= self.width {
                    break;
                }
                let attr = attrs.get(cell_idx).copied().unwrap_or_default();

                if Some(attr) != current_attr {
                    if !current_run.is_empty()
                        && let Some(prev_attr) = current_attr
                    {
                        self.draw_run(
                            ctx,
                            cell_style,
                            &current_run,
                            prev_attr,
                            Point::new(run_start_cell_idx as f64 * self.char_w + padding_x, py),
                        );
                    }
                    current_run.clear();
                    current_attr = Some(attr);
                    run_start_cell_idx = cell_idx;
                }
                current_run.push(c);
                cell_idx += c_w;
            }
            if !current_run.is_empty()
                && let Some(prev_attr) = current_attr
            {
                self.draw_run(
                    ctx,
                    cell_style,
                    &current_run,
                    prev_attr,
                    Point::new(run_start_cell_idx as f64 * self.char_w + padding_x, py),
                );
            }
        }
    }

    /// Paint the block cursor, pinned to the live output — while the user
    /// has scrolled back into history it hides (matching xterm/kitty), so
    /// it doesn't imply the caret is somewhere on an old line. Helper for
    /// [`Widget::draw`].
    fn draw_block_cursor(
        &self,
        ctx: &mut PaintCtx<'_>,
        cell_style: &TextStyle,
        start_y: usize,
        visible_rows: usize,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let palette = &theme.tokens.terminal_palette;
        let grid_rect = self.grid_rect;
        let padding_x = self.last_padding_x;
        let scroll = self.scroll_offset;
        let cursor_color = self.last_cursor_color;
        // Absolute grid row of the cursor → visible row (may be scrolled off).
        let cursor_abs = self.grid.len().saturating_sub(self.height) + self.cursor_y;
        let scrolled_back = !self.alt_active && scroll > 0;
        if self.modes.cursor_visible
            && !scrolled_back
            && cursor_abs >= start_y
            && cursor_abs < start_y + visible_rows.max(1)
        {
            let cy_local = cursor_abs - start_y;
            let cx = grid_rect.x0 + padding_x + self.cursor_x as f64 * self.char_w;
            let cy = (grid_rect.y0 + cy_local as f64 * self.char_h).floor();
            // A rounded, slightly inset caret reads softer than a hard-edged
            // full-cell block while staying an unmistakable, theme-coloured
            // "inverted cell" cursor.
            let cursor_rect = RoundedRect::from_rect(
                Rect::new(cx, cy + 0.5, cx + self.char_w, cy + self.char_h - 0.5),
                2.0,
            );
            scene.fill(Fill::NonZero, transform, cursor_color, None, &cursor_rect);

            // Re-punch the glyph beneath the block in the background color so
            // the character under the caret stays legible (a solid block that
            // hides its own cell reads as a blank cursor). This is the standard
            // "inverted cell" block cursor.
            if let Some(ch) = self
                .grid
                .get(cursor_abs)
                .and_then(|line| cell_char_at(line, self.cursor_x))
                && !ch.is_whitespace()
            {
                text_renderer.draw_style(
                    scene,
                    &ch.to_string(),
                    palette.bg_default,
                    transform * Affine::translate((cx, cy)),
                    None,
                    cell_style,
                );
            }
        }
    }

    /// Paint one run of cells sharing `attr`, starting `at.x` into the grid
    /// on the row whose top is `at.y`.
    fn draw_run(
        &self,
        ctx: &mut PaintCtx<'_>,
        cell_style: &TextStyle,
        text: &str,
        attr: Attr,
        at: Point,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let palette = &theme.tokens.terminal_palette;
        let (rx, py) = (at.x, at.y);
        let (mut fg, bg_opt) = resolve_pair(attr, palette);
        // SGR 2 (faint): blend the foreground toward the background so it
        // reads as dimmed, matching xterm/kitty. Applied after reverse so a
        // dimmed reverse cell dims its (swapped) text too.
        if attr.dim {
            fg = fg.with_alpha_factor(0.55);
        }
        // OSC 8 hyperlink cells read as links: tint the text toward the
        // palette's link hue (bright blue anchor) unless the program already
        // chose an explicit colour, and always underline so the affordance is
        // obvious. `link_color` comes from the palette so it tracks the theme.
        let is_link = attr.link_id != 0;
        if is_link && attr.fg == CellColor::Default && !attr.reverse {
            fg = palette.ansi[12];
        }
        let tx = self.grid_rect.x0 + rx;

        let mut visual_width = 0.0;
        for c in text.chars() {
            visual_width += c.width().unwrap_or(1) as f64 * self.char_w;
        }

        if let Some(bg_color) = bg_opt {
            let bg_rect = Rect::new(tx, py, tx + visual_width + 0.5, py + self.char_h + 0.5);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                bg_color,
                None,
                &bg_rect,
            );
        }

        let weight = if attr.bold {
            TextWeight::Bold
        } else {
            TextWeight::Regular
        };
        text_renderer.draw_style(
            scene,
            text,
            fg,
            transform * Affine::translate((tx, py)),
            None,
            // The OS's monospace face — a terminal must render fixed-width.
            &cell_style
                .clone()
                .with_weight(weight)
                .with_italic(attr.italic),
        );

        if attr.underline || is_link {
            let uy = py + self.char_h - 1.5;
            let line = Rect::new(tx, uy, tx + visual_width, uy + 1.0);
            scene.fill(vello::peniko::Fill::NonZero, transform, fg, None, &line);
        }
        if attr.strike {
            // A crossed-out line through the vertical centre of the cell.
            let sy = (py + self.char_h * 0.5).floor();
            let line = Rect::new(tx, sy, tx + visual_width, sy + 1.0);
            scene.fill(vello::peniko::Fill::NonZero, transform, fg, None, &line);
        }
    }

    /// Resolve the terminal's monospace [`FontChoice`] from the theme, caching
    /// the result against the theme's mono family name. The theme's named mono
    /// face is used when it is actually installed; otherwise the generic
    /// monospace family, which the platform resolves to a real fixed-width font.
    /// The fallback is the whole point: a `Named` face that is *not* installed
    /// makes cosmic-text silently substitute a proportional default whose space
    /// glyph is far narrower than its letters, so the cell advance
    /// (`measure_font(" ")`) no longer matches the width glyphs are drawn at and
    /// every multi-colour run smears over itself.
    fn resolve_mono(&mut self, theme: &Theme, tr: &TextRenderer) -> FontChoice {
        let fam = theme.tokens.font_mono.family.trim();
        if let Some((cached, choice)) = &self.mono_choice
            && cached == fam
        {
            return choice.clone();
        }
        let choice = if fam.is_empty() || fam.eq_ignore_ascii_case("monospace") {
            FontChoice::Monospace
        } else if tr
            .system_families()
            .iter()
            .any(|f| f.eq_ignore_ascii_case(fam))
        {
            FontChoice::Named(fam.to_string())
        } else {
            FontChoice::Monospace
        };
        self.mono_choice = Some((fam.to_string(), choice.clone()));
        choice
    }

    /// Rows of the buffer that carry anything: the last row with text on it,
    /// or the cursor's row when it sits below that (an empty line the caret
    /// is on is a real position, and has to stay reachable).
    ///
    /// This is deliberately not `grid.len()`. A session allocates a full
    /// screen of rows up front and fills them as output arrives, so the
    /// buffer is `height` rows long from the first frame — before anything
    /// has ever been printed to it.
    fn content_rows(&self) -> usize {
        let written = self
            .grid
            .iter()
            .rposition(|line| !line.trim_end().is_empty())
            .map_or(0, |i| i + 1);
        let cursor_abs = self.grid.len().saturating_sub(self.height) + self.cursor_y;
        written.max(cursor_abs + 1)
    }

    /// Whether the scrollbar represents anything, for a viewport of
    /// `visible_rows` rows. The predicate [`Self::draw_scrollbar`] gates on.
    ///
    /// Two conditions, and the second is the one that is easy to miss: the
    /// buffer has to be longer than the viewport **and** actually have
    /// content past it. A buffer can satisfy the first on its own — a
    /// session allocates a full screen of rows up front, and a window one
    /// row shorter than that screen then makes `grid.len() > visible_rows`
    /// true before anything has ever been printed. A bar drawn on that
    /// advertises travel that does not exist: the thumb moves and the view
    /// goes from blank to blank. A scrollbar is a claim about content, so
    /// it appears only when there is content to reach.
    pub(crate) fn scrollbar_visible(&self, visible_rows: usize) -> bool {
        !self.alt_active
            && visible_rows > 0
            && self.grid.len() > visible_rows
            && self.content_rows() > visible_rows
    }

    /// A themed scrollbar in the reserved right gutter, drawn only when the
    /// buffer overflows the viewport. Brighter while the user is scrolled back
    /// into history, faint when pinned to the live bottom.
    fn draw_scrollbar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        start_y: usize,
        visible_rows: usize,
    ) {
        if !self.scrollbar_visible(visible_rows) {
            return;
        }
        let track = self.scrollbar_track();
        let track_w = track.width();

        // Faint track groove so the bar has a visible extent even when pinned.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.lift(0.2),
            None,
            &RoundedRect::from_rect(track, track_w / 2.0),
        );

        let thumb_alpha = if self.scrollbar.is_active() || self.scroll_offset > 0 {
            0.9
        } else {
            0.4
        };
        let metrics =
            ScrollMetrics::new(self.grid.len() as f64, visible_rows as f64, start_y as f64);
        let Some(thumb_rect) = self.scrollbar.thumb_rect(track, metrics) else {
            return;
        };
        let thumb = RoundedRect::from_rect(thumb_rect, track_w / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.text_dim.with_alpha_factor(thumb_alpha),
            None,
            &thumb,
        );
    }

    /// Honest first-run empty state: a centred, muted watermark shown before
    /// the session has ever produced output — a themed placeholder, not a void.
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let gr = self.grid_rect;
        if gr.width() <= 0.0 || gr.height() <= 0.0 {
            return;
        }
        let cx = gr.x0 + gr.width() / 2.0;
        let cy = gr.y0 + gr.height() / 2.0;

        let icon_side = 44.0_f64.min(gr.width() * 0.4).min(gr.height() * 0.4);
        let icon_rect = Rect::new(
            cx - icon_side / 2.0,
            cy - icon_side - 4.0,
            cx + icon_side / 2.0,
            cy - 4.0,
        );
        draw_icon(
            scene,
            transform,
            icon_rect,
            Icon::ChevronRight,
            theme.text_dim.with_alpha_factor(0.4),
            2.0,
        );

        let label = "Tilo Terminal";
        let size = tr
            .renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.lg))
            .size;
        let (lw, _) = tr.measure(label, size, None);
        tr.draw_text_weighted(
            scene,
            label,
            size,
            theme.text_dim.with_alpha_factor(0.7),
            transform * Affine::translate((cx - f64::from(lw) / 2.0, cy + 4.0)),
            None,
            TextWeight::Medium,
        );
    }

    /// Persistent, token-driven terminal lifecycle state. This deliberately is
    /// not a transient toast: when a shell exits there is no live process to
    /// recover by retrying, so both sighted users and assistive technology need
    /// a stable explanation beside the preserved transcript.
    fn draw_disconnected_status(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.is_disconnected() || self.grid_rect.width() <= 0.0 {
            return;
        }
        let label = self.disconnected_message();
        let size = tr
            .renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
            .size;
        let pad = f64::from(theme.tokens.spacing.s2);
        // Reuse the renderer's shared elision behaviour instead of letting a
        // lifecycle message run under the scrollbar or off a narrow / 200%
        // text-scale surface. The complete state remains in accessibility;
        // this is only the one-line visual badge.
        let label = tr.elide_to_width(
            label,
            size,
            (self.grid_rect.width() - pad * 4.0).max(0.0) as f32,
        );
        let (label_w, label_h) = tr.measure(&label, size, None);
        let height = f64::from(label_h) + pad;
        let width = (f64::from(label_w) + pad * 2.0).min(self.grid_rect.width());
        let rect = Rect::new(
            self.grid_rect.x0 + pad,
            self.grid_rect.y1 - height - pad,
            self.grid_rect.x0 + pad + width,
            self.grid_rect.y1 - pad,
        );
        ui::surface::card(
            scene,
            transform,
            rect,
            f64::from(theme.tokens.spacing.s1),
            theme.tokens.colors.lift(0.3),
        );
        tr.draw_text_weighted(
            scene,
            &label,
            size,
            theme.text_primary,
            transform * Affine::translate((rect.x0 + pad, rect.y0 + pad / 2.0)),
            None,
            TextWeight::Medium,
        );
    }

    /// Convert a logical pointer position into a **buffer-absolute**
    /// `GridPos`. The on-screen row is offset by the current scrollback
    /// position (`viewport_start_y`) so the returned row indexes `self.grid`
    /// directly and a selection anchored to it survives scrolling.
    /// Coordinates outside the cell grid are clamped to the nearest valid
    /// visible cell — matching xterm's behaviour during a drag-select that
    /// leaves the terminal area.
    fn pos_from_logical(&self, p: Point) -> GridPos {
        // Match the padding used in `draw` so a click on column 0 lands
        // on column 0 and not -1 due to the leading gutter.
        let padding_x = self.last_padding_x();
        let local_x = (p.x - self.grid_rect.x0 - padding_x).max(0.0);
        let local_y = (p.y - self.grid_rect.y0).max(0.0);
        let col = (local_x / self.char_w.max(1.0)).floor() as usize;
        let vrow = (local_y / self.char_h.max(1.0)).floor() as usize;
        // `width` is logical grid width; `viewport` height is computed
        // from `grid_rect.height() / char_h` rather than `self.height` because
        // resize-clamp may have a stale `self.height` between frames.
        let visible_rows = (self.grid_rect.height() / self.char_h.max(1.0)).floor() as usize;
        let vrow = vrow.min(visible_rows.saturating_sub(1));
        let abs_row = (self.viewport_start_y + vrow).min(self.grid.len().saturating_sub(1));
        GridPos {
            row: abs_row,
            col: col.min(self.width.saturating_sub(1)),
        }
    }

    /// The buffer-absolute word span containing `pos`, using the toolkit's
    /// shared `word_boundaries` walker over the row's reconstructed text so
    /// terminal word selection matches every other Tilo text surface. Returns
    /// `(start, end)` as `GridPos`es on the same row, `end` exclusive.
    fn word_span_at(&self, pos: GridPos) -> (GridPos, GridPos) {
        let line = self.grid.get(pos.row).map_or("", String::as_str);
        // Column → byte offset (accounting for wide glyphs), find the word in
        // byte space, then map the byte range back to columns.
        let byte = col_to_byte(line, pos.col);
        let (bs, be) = word_boundaries(line, byte);
        let cs = byte_to_col(line, bs);
        let ce = byte_to_col(line, be);
        (
            GridPos {
                row: pos.row,
                col: cs,
            },
            GridPos {
                row: pos.row,
                col: ce.max(cs + 1).min(self.width),
            },
        )
    }

    /// The buffer-absolute line span for `pos.row`: the whole grid row. Each
    /// terminal grid row is one logical line (no embedded `\n`), so this is
    /// column `0` through the grid width. `line_boundaries` is used for parity
    /// with the shared primitive even though a single row has no interior
    /// newline.
    fn line_span_at(&self, pos: GridPos) -> (GridPos, GridPos) {
        let line = self.grid.get(pos.row).map_or("", String::as_str);
        let (_bs, _be) = line_boundaries(line, col_to_byte(line, pos.col));
        (
            GridPos {
                row: pos.row,
                col: 0,
            },
            GridPos {
                row: pos.row,
                col: self.width,
            },
        )
    }

    /// Start (or, with `extend`, grow) a selection at buffer-absolute `pos`
    /// for the given multi-click `granularity`. Records the anchor unit so a
    /// subsequent word-/line-granularity drag keeps that original word/line
    /// inside the selection.
    fn begin_selection(&mut self, extend: bool, granularity: Granularity, pos: GridPos) {
        self.drag_granularity = if extend {
            Granularity::Char
        } else {
            granularity
        };
        if extend {
            // Shift+click: keep the existing anchor (or promote the current
            // cursor), move the caret to the click.
            let anchor = self.selection.map_or(pos, |s| s.anchor);
            self.selection = Some(Selection {
                anchor,
                cursor: pos,
            });
            self.drag_anchor_span = None;
            return;
        }
        match granularity {
            Granularity::Char => {
                self.selection = Some(Selection {
                    anchor: pos,
                    cursor: pos,
                });
                self.drag_anchor_span = None;
            }
            Granularity::Word => {
                let (s, e) = self.word_span_at(pos);
                self.selection = Some(Selection {
                    anchor: s,
                    cursor: e,
                });
                self.drag_anchor_span = Some((s, e));
            }
            Granularity::Line => {
                let (s, e) = self.line_span_at(pos);
                self.selection = Some(Selection {
                    anchor: s,
                    cursor: e,
                });
                self.drag_anchor_span = Some((s, e));
            }
        }
    }

    /// Extend the in-progress drag to buffer-absolute `pos`, honouring the
    /// drag granularity. Word/line drags grow by whole units via the shared
    /// `extend_span_drag` rule so the anchored word/line stays selected.
    fn extend_drag_to(&mut self, pos: GridPos) {
        let new = match self.drag_granularity {
            Granularity::Char => self.selection.map(|mut s| {
                s.cursor = pos;
                s
            }),
            Granularity::Word => self
                .drag_anchor_span
                .map(|(alo, ahi)| {
                    let (ws, we) = self.word_span_at(pos);
                    let (a, c) = extend_span_drag(alo, ahi, pos, ws, we);
                    Selection {
                        anchor: a,
                        cursor: c,
                    }
                })
                .or_else(|| {
                    self.selection.map(|mut s| {
                        s.cursor = pos;
                        s
                    })
                }),
            Granularity::Line => self
                .drag_anchor_span
                .map(|(alo, ahi)| {
                    let (ls, le) = self.line_span_at(pos);
                    let (a, c) = extend_span_drag(alo, ahi, pos, ls, le);
                    Selection {
                        anchor: a,
                        cursor: c,
                    }
                })
                .or_else(|| {
                    self.selection.map(|mut s| {
                        s.cursor = pos;
                        s
                    })
                }),
        };
        if let Some(s) = new {
            self.selection = Some(s);
        }
    }

    /// Finish a selection gesture on mouse-up. A collapsed (no-drag) selection
    /// is dropped — but if it was a plain click on an OSC 8 hyperlink cell,
    /// the link opens first. A non-empty selection is published to the primary
    /// selection (for middle-click paste) and, when "select to copy" is on,
    /// the clipboard too.
    fn finish_selection(&mut self) {
        let collapsed = self.selection.is_none_or(|s| s.is_collapsed());
        if collapsed {
            if self.drag_granularity == Granularity::Char
                && let Some(sel) = self.selection
                && let Some(url) = self.hyperlink_at(sel.anchor.row, sel.anchor.col)
            {
                open_url(url);
            }
            self.selection = None;
        } else if let Some(text) = self.selected_text()
            && !text.is_empty()
            && self.may_publish_selection()
        {
            clipboard_copy_primary(&text);
            if self.select_to_copy {
                clipboard_copy(&text);
            }
        }
        self.drag_anchor_span = None;
    }

    /// Encode a buffer-absolute cell as a single linear index (row-major over
    /// the grid width) for the collaboration presence payload, which carries a
    /// `usize` caret / `start..end` selection rather than a 2-D position.
    fn lin(&self, p: GridPos) -> usize {
        p.row * self.width.max(1) + p.col
    }
    /// Inverse of [`lin`]: decode a linear presence index back to a cell.
    fn unlin(&self, pos: usize) -> GridPos {
        let w = self.width.max(1);
        GridPos {
            row: pos / w,
            col: pos % w,
        }
    }
    /// Number of on-screen rows and the absolute row of the top visible row —
    /// the viewport window used to map buffer-absolute rows to screen rows.
    fn viewport_window(&self) -> (usize, usize) {
        let visible_rows = (self.grid_rect.height() / self.char_h.max(1.0))
            .floor()
            .max(1.0) as usize;
        (self.viewport_start_y, visible_rows)
    }

    /// A remote peer's caret rect (a thin vertical bar at the cell's left
    /// edge), or `None` when the cell is scrolled out of the viewport. Built
    /// from the same geometry the local cursor uses so a peer caret lands on
    /// the right cell.
    fn peer_caret_rect(&self, pos: usize) -> Option<Rect> {
        let cell = self.unlin(pos);
        let (start_y, visible_rows) = self.viewport_window();
        if cell.row < start_y || cell.row >= start_y + visible_rows {
            return None;
        }
        let vrow = cell.row - start_y;
        let x = self.grid_rect.x0 + self.last_padding_x + cell.col as f64 * self.char_w;
        let y = (self.grid_rect.y0 + vrow as f64 * self.char_h).floor();
        Some(Rect::new(x, y, x + 2.0, y + self.char_h))
    }

    /// The per-row highlight rects for a remote peer's selection (linear
    /// `start..end`), clipped to the visible viewport. Mirrors the local
    /// selection painting so a peer's highlight aligns cell-for-cell.
    fn peer_selection_rects(&self, start: usize, end: usize) -> Vec<Rect> {
        let sel = Selection {
            anchor: self.unlin(start),
            cursor: self.unlin(end),
        };
        let (start_y, visible_rows) = self.viewport_window();
        let mut rects = Vec::new();
        for (row, start_col, end_col) in selection_row_spans(&sel, self.width) {
            if row < start_y || row >= start_y + visible_rows {
                continue;
            }
            let vrow = row - start_y;
            let py = (self.grid_rect.y0 + vrow as f64 * self.char_h).floor();
            let x0 = self.grid_rect.x0 + self.last_padding_x + start_col as f64 * self.char_w;
            let x1 = self.grid_rect.x0 + self.last_padding_x + end_col as f64 * self.char_w;
            rects.push(Rect::new(x0, py, x1, py + self.char_h));
        }
        rects
    }

    /// The horizontal glyph gutter the last `draw` actually used. Cached from
    /// the theme each frame so pointer hit-testing (which never receives a
    /// `Theme`) stays pixel-aligned with the painted columns even if the OS
    /// retunes `spacing.s1`.
    fn last_padding_x(&self) -> f64 {
        self.last_padding_x
    }

    /// Build + open the shared terminal context menu at the cached cursor.
    /// Selection and clipboard basics stay first; less common session
    /// affordances remain discoverable without adding permanent app chrome.
    fn open_context_menu_at_cursor(&mut self) {
        let has_selection = self.selected_text().is_some_and(|t| !t.is_empty());
        let mut menu = ContextMenu::new();
        menu.set_platform_conventions(self.conventions);
        self.menu_effects.clear();
        self.menu_paste_index = None;
        let session = self.session_resource();
        let transcript = self.transcript_resource();
        if has_selection {
            self.push_menu_effect(&mut menu, "Copy", &transcript, "copy-selection");
        }
        self.menu_paste_index = Some(menu.items().len());
        self.push_menu_effect(&mut menu, "Paste", &session, "paste");
        menu.push(MenuItem::separator());
        if !self.grid_is_blank() {
            self.push_menu_effect(&mut menu, "Select all output", &transcript, "select-all");
        }
        self.push_menu_effect(&mut menu, "Clear screen", &session, "clear");
        self.find_menu_index = Some(menu.items().len());
        menu.push(MenuItem::action(
            ui::i18n::I18nManager::lookup("terminal-find-scrollback", None),
            || {},
        ));
        // This preference is useful secondary chrome, but never at the cost
        // of an unreachable context menu. A 160px-high embedded/narrow pane
        // can fit the copy/paste/selection/process essentials, not another
        // row plus separator; its keyboard shortcut and desktop-sized menu
        // still expose the toggle.
        if self.rect.height() >= 180.0 {
            menu.push(MenuItem::separator());
            menu.push(MenuItem::action(
                if self.select_to_copy {
                    "Copy on select: On"
                } else {
                    "Copy on select: Off"
                },
                || {},
            ));
        }
        // Keep the shared popup aware of the terminal's real viewport. The
        // popup refreshes its geometry when the current theme applies an
        // accessibility text scale during draw; without this viewport it can
        // grow past a narrow pane's edge after that refresh.
        menu.set_viewport(Some(self.rect));
        let p = self.last_cursor_pos;
        let viewport = if self.rect.width() > 0.0 && self.rect.height() > 0.0 {
            self.rect
        } else {
            Rect::new(p.x, p.y, p.x + 1.0, p.y + 1.0)
        };
        menu.open_anchored(Rect::from_origin_size(p, (0.0, 0.0)), viewport);
        self.context_menu = menu;
    }

    /// Bind effectful rows to their exported contract. Selection only queues
    /// an intent; the host supplies current permissions and confirmation.
    fn push_menu_effect(
        &mut self,
        menu: &mut ContextMenu,
        label: &str,
        resource: &Resource,
        action_id: &str,
    ) {
        let Some(spec) = resource.actions.iter().find(|spec| spec.id == action_id) else {
            tracing::error!(
                action_id,
                "terminal menu action is missing its resource contract"
            );
            return;
        };
        match ui::tilo_actions::ActionBinding::new(resource, spec, |()| serde_json::Map::new()) {
            Ok(binding) => {
                let mut emitter = ui::widget::ActionEmitter::new(binding);
                if let Some(allowed) = &self.allowed_menu_actions {
                    emitter.refresh_availability(allowed);
                }
                let index = menu.items().len();
                let available = emitter.is_available();
                self.menu_effects.insert(index, emitter);
                menu.push(MenuItem::action(label, || {}).with_enabled(available));
            }
            Err(error) => tracing::error!(%error, action_id, "terminal menu binding failed"),
        }
    }

    /// Queue a pointer or keyboard gesture through the exported action
    /// contract. The invocation is still re-authorized by the host when it is
    /// drained, so a role change between input and confirmation cannot race a
    /// PTY or clipboard effect.
    fn queue_gesture_effect(
        &mut self,
        resource: &Resource,
        action_id: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        let Some(spec) = resource.actions.iter().find(|spec| spec.id == action_id) else {
            tracing::error!(
                action_id,
                "terminal gesture action is missing its resource contract"
            );
            return false;
        };
        let Ok(binding) =
            ui::tilo_actions::ActionBinding::new(resource, spec, move |()| arguments.clone())
        else {
            tracing::error!(action_id, "terminal gesture binding failed");
            return false;
        };
        let mut emitter = ui::widget::ActionEmitter::new(binding);
        if let Some(allowed) = &self.allowed_menu_actions {
            emitter.refresh_availability(allowed);
        }
        if !emitter.is_available() || emitter.emit(()).is_err() {
            return false;
        }
        let Some(invocation) = emitter.take() else {
            return false;
        };
        if self.gesture_effects.len() >= MAX_GESTURE_EFFECTS {
            tracing::warn!(
                limit = MAX_GESTURE_EFFECTS,
                "terminal gesture action backlog full; rejecting newest intent"
            );
            return false;
        }
        self.gesture_effects.push_back(invocation);
        true
    }

    fn paste_action_arguments(text: String) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::from_iter([("text".to_string(), serde_json::Value::String(text))])
    }

    /// Open the terminal's shared context menu at a host-owned logical
    /// position. This is useful for embedders that route a context-menu
    /// gesture themselves (touch long-press, keyboard invocation, or a custom
    /// pointer layer) while preserving the terminal's standard actions.
    pub fn open_context_menu_at(&mut self, position: Point) {
        self.last_cursor_pos = position;
        self.open_context_menu_at_cursor();
    }

    fn activate_context_menu_item(&mut self, index: usize) {
        if self.menu_paste_index == Some(index) {
            if let Some(text) = clipboard_paste() {
                let session = self.session_resource();
                self.queue_gesture_effect(&session, "paste", Self::paste_action_arguments(text));
            }
            return;
        }
        if let Some(emitter) = self.menu_effects.get_mut(&index) {
            if let Err(error) = emitter.emit(()) {
                tracing::warn!(%error, "terminal menu action could not be queued");
            }
            return;
        }
        if self.find_menu_index == Some(index) {
            self.open_find();
            return;
        }
        let label = self
            .context_menu
            .items()
            .get(index)
            .and_then(MenuItem::label)
            .map(str::to_owned);
        if let Some("Copy on select: On" | "Copy on select: Off") = label.as_deref() {
            self.select_to_copy = !self.select_to_copy;
        }
    }

    fn may_publish_selection(&self) -> bool {
        self.allowed_menu_actions.as_ref().is_none_or(|allowed| {
            allowed.contains(&ui::tilo_actions::ActionAddress::new(
                self.terminal_transcript_target(),
                "copy-selection",
            ))
        })
    }

    fn select_all_output(&mut self) {
        let Some(last_row) = self.grid.iter().rposition(|row| !row.trim_end().is_empty()) else {
            self.selection = None;
            return;
        };
        let last_col = self
            .grid
            .get(last_row)
            .map_or(0, |row| row.trim_end().width());
        self.selection = Some(Selection {
            anchor: GridPos { row: 0, col: 0 },
            cursor: GridPos {
                row: last_row,
                col: last_col,
            },
        });
        if let Some(text) = self.selected_text()
            && !text.is_empty()
            && self.may_publish_selection()
        {
            clipboard_copy_primary(&text);
        }
    }

    /// Walk the selected cells across grid rows and return the joined
    /// text, with trailing whitespace stripped from each row (standard
    /// terminal-copy semantics). Returns `None` if there is no selection
    /// or the selection covers no rows.
    fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        let spans = selection_row_spans(&sel, self.width);
        if spans.is_empty() {
            return None;
        }

        let mut out = String::new();
        for (i, (row, start_col, end_col)) in spans.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            // Selection rows are already buffer-absolute grid indices.
            let abs_row = *row;
            let line = self.grid.get(abs_row).map_or("", String::as_str);

            // Walk chars and pick out the cells covered by [start_col, end_col).
            let mut cell_idx = 0usize;
            let mut row_text = String::new();
            for c in line.chars() {
                let w = c.width().unwrap_or(1);
                if cell_idx >= *end_col {
                    break;
                }
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

    fn map_physical_key(physical: PhysicalKey) -> Option<Vec<u8>> {
        match physical {
            PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter) => Some(vec![b'\r']),
            PhysicalKey::Code(KeyCode::Space) => Some(vec![b' ']),
            _ => None,
        }
    }

    fn map_key(&self, key: &Key, modifiers: winit::event::Modifiers) -> Option<Vec<u8>> {
        let mods = modifiers.state();
        let shift = mods.shift_key();
        let alt = mods.alt_key();
        let ctrl = mods.control_key();
        let mod_param = Self::key_mod_param(shift, alt, ctrl);

        // GUI-terminal quality-of-life bindings that map to established shell
        // editing bytes rather than inventing a Tilo-only protocol.
        if matches!(key, Key::Named(NamedKey::Backspace)) {
            if ctrl && !alt {
                return Some(vec![0x17]); // ^W: erase previous word
            }
            if alt && !ctrl {
                return Some(vec![0x1b, 0x7f]); // Meta-DEL: backward-kill-word
            }
        }

        match key {
            Key::Named(named) => {
                Self::map_named_key(*named, shift, mod_param, self.modes.app_cursor_keys)
            }
            Key::Character(c) if ctrl => Self::map_ctrl_char(c.as_str(), alt),
            Key::Character(c) if alt && !ctrl && !mods.super_key() => {
                Self::map_alt_char(c.as_str())
            }
            // Hardware Space arrives as `Named(Space)` (often with no `text`).
            // Control-socket `type` sends `Character` glyphs whose `text` may
            // also be empty. Both used to fall through, so App/Frame treated
            // unconsumed Space/Enter as Activate and the PTY never saw them.
            Key::Character(c) if !ctrl && !alt && !mods.super_key() => Some(c.as_bytes().to_vec()),
            _ => None,
        }
    }

    /// xterm modifier parameter: 1 + (shift?1) + (alt?2) + (ctrl?4). Only
    /// meaningful when at least one is held.
    fn key_mod_param(shift: bool, alt: bool, ctrl: bool) -> Option<u8> {
        let mut m = 0u8;
        if shift {
            m |= 1;
        }
        if alt {
            m |= 2;
        }
        if ctrl {
            m |= 4;
        }
        if m == 0 { None } else { Some(m + 1) }
    }

    /// Cursor/edit keys: unmodified use the DECCKM-aware SS3/CSI prefix; a
    /// held modifier forces the CSI `1;<mod><final>` form (xterm behaviour),
    /// which is what shells and editors decode for word-wise motion and
    /// in-app selection.
    fn key_csi_final(final_byte: u8, mod_param: Option<u8>, app_cursor_keys: bool) -> Vec<u8> {
        if let Some(m) = mod_param {
            format!("\x1b[1;{m}{}", final_byte as char).into_bytes()
        } else {
            let prefix: &[u8] = if app_cursor_keys { b"\x1bO" } else { b"\x1b[" };
            let mut v = prefix.to_vec();
            v.push(final_byte);
            v
        }
    }

    /// Keypad-style `CSI <n> ~` keys, with the optional modifier parameter.
    fn key_csi_tilde(n: u8, mod_param: Option<u8>) -> Vec<u8> {
        match mod_param {
            Some(m) => format!("\x1b[{n};{m}~").into_bytes(),
            None => format!("\x1b[{n}~").into_bytes(),
        }
    }

    /// F1..F4 use SS3 finals P/Q/R/S when unmodified, CSI `1;<mod><final>`
    /// when modified.
    fn key_fkey_ss3(final_byte: u8, mod_param: Option<u8>) -> Vec<u8> {
        match mod_param {
            Some(m) => format!("\x1b[1;{m}{}", final_byte as char).into_bytes(),
            None => format!("\x1bO{}", final_byte as char).into_bytes(),
        }
    }

    fn map_named_key(
        named: NamedKey,
        shift: bool,
        mod_param: Option<u8>,
        app_cursor_keys: bool,
    ) -> Option<Vec<u8>> {
        match named {
            NamedKey::Enter => Some(vec![b'\r']),
            NamedKey::Space => Some(vec![b' ']),
            NamedKey::Backspace => Some(vec![127]),
            // Shift+Tab is back-tab (CSI Z) — reverse field / completion.
            NamedKey::Tab if shift => Some(b"\x1b[Z".to_vec()),
            NamedKey::Tab => Some(vec![b'\t']),
            NamedKey::Escape => Some(vec![27]),
            NamedKey::ArrowUp => Some(Self::key_csi_final(b'A', mod_param, app_cursor_keys)),
            NamedKey::ArrowDown => Some(Self::key_csi_final(b'B', mod_param, app_cursor_keys)),
            NamedKey::ArrowRight => Some(Self::key_csi_final(b'C', mod_param, app_cursor_keys)),
            NamedKey::ArrowLeft => Some(Self::key_csi_final(b'D', mod_param, app_cursor_keys)),
            NamedKey::Home => Some(Self::key_csi_final(b'H', mod_param, app_cursor_keys)),
            NamedKey::End => Some(Self::key_csi_final(b'F', mod_param, app_cursor_keys)),
            NamedKey::PageUp => Some(Self::key_csi_tilde(5, mod_param)),
            NamedKey::PageDown => Some(Self::key_csi_tilde(6, mod_param)),
            NamedKey::Insert => Some(Self::key_csi_tilde(2, mod_param)),
            NamedKey::Delete => Some(Self::key_csi_tilde(3, mod_param)),
            NamedKey::F1 => Some(Self::key_fkey_ss3(b'P', mod_param)),
            NamedKey::F2 => Some(Self::key_fkey_ss3(b'Q', mod_param)),
            NamedKey::F3 => Some(Self::key_fkey_ss3(b'R', mod_param)),
            NamedKey::F4 => Some(Self::key_fkey_ss3(b'S', mod_param)),
            NamedKey::F5 => Some(Self::key_csi_tilde(15, mod_param)),
            NamedKey::F6 => Some(Self::key_csi_tilde(17, mod_param)),
            NamedKey::F7 => Some(Self::key_csi_tilde(18, mod_param)),
            NamedKey::F8 => Some(Self::key_csi_tilde(19, mod_param)),
            NamedKey::F9 => Some(Self::key_csi_tilde(20, mod_param)),
            NamedKey::F10 => Some(Self::key_csi_tilde(21, mod_param)),
            NamedKey::F11 => Some(Self::key_csi_tilde(23, mod_param)),
            NamedKey::F12 => Some(Self::key_csi_tilde(24, mod_param)),
            _ => None,
        }
    }

    /// Ctrl+letter → the corresponding C0 control code. When Alt is also
    /// held, prefix ESC (Alt-as-Meta) so e.g. Alt+Ctrl+F works.
    fn map_ctrl_char(s: &str, alt: bool) -> Option<Vec<u8>> {
        let ch = s.chars().next()?;
        let up = ch.to_ascii_uppercase();
        let code = match up {
            'A'..='Z' => (up as u8) - b'A' + 1,
            ' ' | '@' => 0, // Ctrl+Space / Ctrl+@ → NUL
            '[' => 27,
            '\\' => 28,
            ']' => 29,
            '^' => 30,
            '_' => 31,
            _ => return None,
        };
        if alt {
            Some(vec![27, code])
        } else {
            Some(vec![code])
        }
    }

    /// Alt-as-Meta for plain alphanumerics (readline word ops: Alt-f,
    /// Alt-b, Alt-d, numeric arguments). Restricted to ASCII
    /// alphanumeric logical keys so AltGr-composed symbols on layouts
    /// like `de_DE` (AltGr+q=@, AltGr+e=€, AltGr+7={ …) are never poached
    /// — those resolve to non-alphanumeric logical keys and fall
    /// through to their composed `text`.
    fn map_alt_char(s: &str) -> Option<Vec<u8>> {
        if s.chars().count() == 1 && s.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            let mut v = vec![27u8];
            v.extend_from_slice(s.as_bytes());
            Some(v)
        } else {
            None
        }
    }
}

pub struct TerminalPerformer<'a> {
    pub state: &'a mut AutoCommit,
    pub main_grid_id: ObjId,
    pub alt_grid_id: ObjId,
    pub cursor_id: ObjId,
    /// CRDT map (`terminal_links`) storing OSC 8 link id → URL, so hyperlinks
    /// resolve on every peer, not just the host that parsed the sequence.
    pub links_id: ObjId,
    pub images_id: ObjId,
    /// Where Kitty graphics payloads are stored; the CRDT keeps only the hash.
    pub content: &'a ContentStore,
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
    /// Accumulates the count of oldest lines trimmed off the main buffer's
    /// front this feed, so the widget's `sync` can keep its render mirror
    /// aligned without a full re-read.
    pub trimmed_front: &'a mut usize,
    /// Monotonic OSC 8 link-id allocator (shared with the widget so ids stay
    /// unique across feeds). `0` is reserved for "no link".
    pub next_link_id: &'a mut u16,
    /// Link id of the currently-open OSC 8 hyperlink (`0` = none). Every cell
    /// printed while this is non-zero adopts it.
    pub current_link_id: u16,
    /// Out-param for an OSC 0/2 window-title change parsed this feed; the
    /// widget writes it into the CRDT and its own `title` field afterwards.
    pub title_out: &'a mut Option<String>,
    /// Reply bytes accumulated for terminal capability queries (Device
    /// Attributes, Device Status Report, XTVERSION, DECRQM, …). A real
    /// terminal answers these on the same wire the keyboard writes to; the
    /// vte parser only *reads*, so the performer stages the answer here and
    /// [`feed`](TerminalWidget::feed) flushes it through `write_input` after
    /// the parse. This is what stops fish (and any terminfo-probing program)
    /// from blocking for 10s on a Primary Device Attributes query that never
    /// gets answered.
    pub responses: &'a mut Vec<u8>,
    /// Batches consecutive `print`s to the same line so a run of characters is
    /// written to the CRDT once (a diffed text splice + one `attrs` put) rather
    /// than once per cell. Per-cell `splice_text` + full-`attrs`-`put` is an
    /// O(doc) write *per character*, so a screenful of output was O(n²) — the
    /// exact per-keystroke document rewrite HIG §6 forbids, and enough to freeze
    /// a debug build on a bulk paste / `cat`. Any non-`print` action flushes the
    /// buffer first (see `flush_print_buf`)
    /// so it never observes stale grid state.
    pub print_buf: Option<PrintBuf>,
}

/// In-memory accumulator for a run of `print`s landing on one grid line.
/// Mutated per character; written to the CRDT once when the line changes, any
/// other emulator action runs, or the feed ends.
pub struct PrintBuf {
    line_obj: ObjId,
    text: String,
    attrs: Vec<u8>,
    alternate: bool,
    index: u32,
}

fn splice_chars(text: &str, at: usize, delete: usize, insert: &str) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    let at = at.min(chars.len());
    let end = (at + delete).min(chars.len());
    chars.splice(at..end, insert.chars());
    chars.into_iter().collect()
}

fn blank_attr_range(attrs: &mut [u8], from: usize, count: usize) {
    if count == 0 {
        return;
    }
    let empty = Attr::default().pack();
    for index in from..(from + count) {
        let start = index * ATTR_BYTES;
        if let Some(dest) = attrs.get_mut(start..start + ATTR_BYTES) {
            dest.copy_from_slice(&empty);
        }
    }
}

/// Name this emulator reports for itself in the XTVERSION reply
/// (`CSI > 0 q` → `DCS > | tilo-terminal(<version>) ST`).
pub const TERMINAL_NAME: &str = "tilo-terminal";

/// Numeric build version reported in the Secondary Device Attributes (DA2)
/// reply `CSI > 1 ; Pv ; 0 c`. Encoded `major*10000 + minor*100 + patch` so a
/// program keying off it sees the real build as a single integer (xterm's
/// "patch level" convention). Derived from the crate version at compile time.
pub fn terminal_da2_version() -> u32 {
    let major: u32 = env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap_or(0);
    let minor: u32 = env!("CARGO_PKG_VERSION_MINOR").parse().unwrap_or(0);
    let patch: u32 = env!("CARGO_PKG_VERSION_PATCH").parse().unwrap_or(0);
    major * 10000 + minor * 100 + patch
}

impl Perform for TerminalPerformer<'_> {
    fn print(&mut self, c: char) {
        let char_w = c.width().unwrap_or(1);
        if char_w == 0 {
            return;
        }

        if self.pending_wrap {
            self.cursor_x = 0;
            self.newline();
            self.pending_wrap = false;
        }

        let y = self.absolute_y().min(self.active_len().saturating_sub(1));
        let Some(index) = u32::try_from(y).ok() else {
            return;
        };
        let Some(line_obj) = self.active_line_id(y) else {
            return;
        };

        // Point the write buffer at this line, loading its current text + attrs
        // once. Consecutive prints to the same line then mutate memory only; the
        // CRDT is touched a single time when the line changes or an action
        // flushes (below). This is what turns a screenful of output from O(n²)
        // per-cell CRDT writes into O(cells) memory edits + O(lines) commits.
        let need_load = self
            .print_buf
            .as_ref()
            .is_none_or(|b| b.line_obj != line_obj);
        if need_load {
            self.flush_print_buf();
            let Ok(Some((Value::Object(ObjType::Text), text_id))) =
                self.state.get(&line_obj, "text")
            else {
                return;
            };
            let text = self.state.text(&text_id).unwrap_or_default();
            let attrs = match self.state.get(&line_obj, "attrs") {
                Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                    automerge::ScalarValue::Bytes(bytes) => bytes.clone(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            self.print_buf = Some(PrintBuf {
                line_obj,
                text,
                attrs,
                alternate: *self.alt_active,
                index,
            });
        }
        // `need_load` above always leaves `print_buf` populated by this
        // point — unless the CRDT `get` inside it failed, in which case that
        // branch already returned. Nothing left to print into either way.
        let Some(buf) = self.print_buf.as_mut() else {
            return;
        };

        // Text edit, in char space (Automerge `Text` is char-addressed, so the
        // buffer mirrors that exactly). Lines grow on demand: pad with spaces
        // when the cursor sits past the current end (a cursor jump on a fresh
        // line), then overwrite (or append) the cell(s) this glyph occupies.
        let mut chars: Vec<char> = buf.text.chars().collect();
        if self.cursor_x > chars.len() {
            chars.resize(self.cursor_x, ' ');
        }
        let mut repl: Vec<char> = vec![c];
        repl.resize(char_w, ' ');
        if self.cursor_x < chars.len() {
            let end = (self.cursor_x + char_w).min(chars.len());
            chars.splice(self.cursor_x..end, repl);
        } else {
            chars.extend(repl);
        }
        buf.text = chars.into_iter().collect();

        // Attrs mirror the text; the array is grown to fit the cell being
        // written (matching the on-demand text fill).
        let start = self.cursor_x * ATTR_BYTES;
        let end = (self.cursor_x + char_w) * ATTR_BYTES;
        if buf.attrs.len() < end {
            buf.attrs.resize(end, 0);
        }

        // Carry the active OSC 8 hyperlink id onto the cell, independent of SGR
        // so a link and its styling compose.
        let mut cell_attr = self.current_attr;
        cell_attr.link_id = self.current_link_id;
        if let Some(dest) = buf.attrs.get_mut(start..start + ATTR_BYTES) {
            dest.copy_from_slice(&cell_attr.pack());
        }
        if char_w > 1 {
            let mut cont = cell_attr;
            cont.is_wide_cont = true;
            let cont_packed = cont.pack();
            for i in 1..char_w {
                let s = (self.cursor_x + i) * ATTR_BYTES;
                if let Some(dest) = buf.attrs.get_mut(s..s + ATTR_BYTES) {
                    dest.copy_from_slice(&cont_packed);
                }
            }
        }

        self.cursor_x += char_w;
        if self.cursor_x >= self.width {
            self.pending_wrap = true;
            self.cursor_x = self.width - 1;
        }
        self.update_cursor();
    }

    fn execute(&mut self, byte: u8) {
        self.flush_print_buf();
        match byte {
            b'\n' | 0x0B | 0x0C => {
                self.pending_wrap = false;
                self.newline();
            }
            b'\r' => {
                self.pending_wrap = false;
                self.cursor_x = 0;
            }
            8 => {
                self.pending_wrap = false;
                if self.cursor_x > 0 {
                    self.cursor_x -= 1;
                }
            }
            b'\t' => {
                self.pending_wrap = false;
                let next = ((self.cursor_x / 8) + 1) * 8;
                self.cursor_x = next.min(self.width.saturating_sub(1));
            }
            // Everything else, including 7 (BEL), is a no-op — the toolkit
            // has no audible bell yet and no other single byte has meaning.
            _ => {}
        }
        self.update_cursor();
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        self.flush_print_buf();
        match byte {
            b'7' => {
                *self.saved_cursor = Some((self.cursor_x, self.cursor_y, self.current_attr));
            } // DECSC
            b'8' => {
                // DECRC
                if let Some((x, y, a)) = *self.saved_cursor {
                    self.cursor_x = x.min(self.width.saturating_sub(1));
                    self.cursor_y = y.min(self.height.saturating_sub(1));
                    self.current_attr = a;
                }
            }
            b'D' => {
                self.pending_wrap = false;
                self.newline();
            } // IND — index (line feed)
            b'E' => {
                self.pending_wrap = false;
                self.cursor_x = 0;
                self.newline();
            } // NEL
            b'M' => {
                // RI — reverse index
                self.pending_wrap = false;
                if self.cursor_y == self.scroll_top {
                    self.scroll_region_down(1);
                } else if self.cursor_y > 0 {
                    self.cursor_y -= 1;
                }
            }
            b'Z' => self.reply_da1(), // DECID — deprecated primary-DA request
            _ => {}
        }
        self.update_cursor();
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, c: char) {
        self.flush_print_buf();
        self.pending_wrap = false;
        if intermediates.first() == Some(&b'?') {
            self.csi_dispatch_private(params, intermediates, c);
        } else {
            self.csi_dispatch_public(params, intermediates, c);
        }
        self.update_cursor();
    }

    /// OSC — Operating System Command. vte 0.15 pre-splits the payload on `;`
    /// into `params`, so `params[0]` is the command number as ASCII bytes.
    /// Handles window title (0/2), clipboard (52), and hyperlinks (8).
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.flush_print_buf();
        let kind = params.first().copied().unwrap_or(b"");
        let code: u32 = std::str::from_utf8(kind)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(u32::MAX);
        match code {
            // OSC 0 = icon name + window title; OSC 2 = window title. Both set
            // the title for our purposes.
            0 | 2 => {
                if let Some(title) = params.get(1).copied()
                    && let Some(title) = safe_osc_text(title, MAX_OSC_TITLE_BYTES)
                {
                    *self.title_out = Some(title);
                }
            }
            // OSC 52 — manipulate the selection/clipboard. Form:
            // `OSC 52 ; <targets> ; <base64|?> ST`. We honour the *set* path
            // (write decoded text to the clipboard / primary selection); a
            // `?` query would need to write back to the PTY, which the parser
            // can't do, so it's ignored.
            52 => {
                let targets = params.get(1).copied().unwrap_or(b"");
                let data = params.get(2).copied().unwrap_or(b"");
                if data == b"?" || data.is_empty() {
                    return;
                }
                let max_encoded = MAX_OSC_CLIPBOARD_BYTES
                    .saturating_mul(4)
                    .saturating_div(3)
                    .saturating_add(4);
                if data.len() > max_encoded {
                    return;
                }
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data)
                    && bytes.len() <= MAX_OSC_CLIPBOARD_BYTES
                    && let Ok(text) = String::from_utf8(bytes)
                {
                    // `p` selects the primary selection; anything else
                    // (`c`, `s`, default) targets the clipboard.
                    if targets.contains(&b'p') {
                        clipboard_copy_primary(&text);
                    } else {
                        clipboard_copy(&text);
                    }
                }
            }
            // OSC 8 — hyperlink. Form: `OSC 8 ; <params> ; <URI> ST`. An empty
            // URI closes the current link. We register the URI in the CRDT and
            // stamp its id onto subsequent cells via `current_link_id`.
            8 => {
                let uri = params.get(2).copied().unwrap_or(b"");
                if uri.is_empty() {
                    self.current_link_id = 0;
                } else if self.state.length(&self.links_id) < MAX_OSC_LINKS
                    && let Some(url) = safe_osc_text(uri, MAX_OSC_LINK_BYTES)
                    && let Some(id) = self.next_link_id.checked_add(1)
                {
                    *self.next_link_id = id;
                    let action = crate::resource::TerminalAction::RegisterLink { id, url };
                    if ui::tilo_actions::Action::apply(&action, self.state).is_ok() {
                        self.current_link_id = id;
                    }
                }
            }
            _ => {}
        }
    }
}

impl TerminalPerformer<'_> {
    /// Persist a complete direct Kitty transmission as a content-addressed blob
    /// plus a lightweight CRDT placement descriptor. The local renderer
    /// validates and paints this subset, but no protocol success reply is sent:
    /// clients must not infer broad Kitty support from an acknowledgement.
    fn kitty_graphics(&mut self, payload: &[u8]) {
        self.flush_print_buf();
        let Some(image) = parse_kitty(payload) else {
            return;
        };
        if image.mime == "image/png" && tilo_content::sniff(&image.bytes).mime != "image/png" {
            return;
        }
        let hash = match self.content.put(&image.bytes) {
            Ok(hash) => hash,
            Err(error) => {
                tracing::warn!(%error, "terminal: couldn't store graphics payload");
                return;
            }
        };
        let action = crate::resource::TerminalAction::PlaceImage {
            hash,
            mime: image.mime.to_string(),
            byte_len: image.bytes.len() as u64,
            row: self.absolute_y() as u64,
            col: self.cursor_x as u64,
            columns: u64::from(image.columns),
            rows: u64::from(image.rows),
            pixel_width: u64::from(image.pixel_width),
            pixel_height: u64::from(image.pixel_height),
            protocol_id: image.protocol_id.map(u64::from),
            alternate_screen: *self.alt_active,
        };
        let _ = ui::tilo_actions::Action::apply(&action, self.state);
    }

    fn csi_first(p: &vte::Params) -> u16 {
        p.iter()
            .next()
            .and_then(|x| x.first())
            .copied()
            .unwrap_or(0)
    }

    fn csi_first_or1(p: &vte::Params) -> usize {
        let v = Self::csi_first(p);
        if v == 0 { 1 } else { v as usize }
    }

    /// DEC-private CSI sequences (`CSI ? ... <final>`), dispatched separately
    /// from the ANSI/xterm set below since a private and a public sequence
    /// can share the same final byte with unrelated meanings.
    fn csi_dispatch_private(&mut self, params: &vte::Params, intermediates: &[u8], c: char) {
        match c {
            'h' => self.set_dec_modes(params, true),
            'l' => self.set_dec_modes(params, false),
            // DECRQM — request the setting of a DEC-private mode
            // (`CSI ? Pm $ p`). Answered with DECRPM so terminfo probes
            // that ask "is bracketed paste / alt-screen on?" get a reply
            // instead of stalling.
            'p' if intermediates.contains(&b'$') => self.reply_decrqm(Self::csi_first(params)),
            // DECXCPR — DEC-private cursor position report (`CSI ? 6 n`).
            'n' => self.reply_dsr_private(Self::csi_first(params)),
            _ => {}
        }
    }

    /// ANSI/xterm CSI sequences (cursor motion, erase, scroll region,
    /// SGR, device attributes/status).
    fn csi_dispatch_public(&mut self, params: &vte::Params, intermediates: &[u8], c: char) {
        match c {
            'A' => {
                let n = Self::csi_first_or1(params);
                self.cursor_y = self.cursor_y.saturating_sub(n);
            }
            'B' | 'e' => {
                let n = Self::csi_first_or1(params);
                self.cursor_y = (self.cursor_y + n).min(self.height - 1);
            }
            'C' | 'a' => {
                let n = Self::csi_first_or1(params);
                self.cursor_x = (self.cursor_x + n).min(self.width - 1);
            }
            'D' => {
                let n = Self::csi_first_or1(params);
                self.cursor_x = self.cursor_x.saturating_sub(n);
            }
            'G' | '`' => {
                let n = Self::csi_first_or1(params);
                self.cursor_x = (n - 1).min(self.width - 1);
            } // CHA
            'd' => {
                let n = Self::csi_first_or1(params);
                self.cursor_y = (n - 1).min(self.height - 1);
            } // VPA
            'H' | 'f' => {
                let mut it = params.iter();
                let y = it.next().and_then(|p| p.first()).copied().unwrap_or(1) as usize;
                let x = it.next().and_then(|p| p.first()).copied().unwrap_or(1) as usize;
                self.cursor_y = (y.saturating_sub(1)).min(self.height - 1);
                self.cursor_x = (x.saturating_sub(1)).min(self.width - 1);
            }
            'J' => self.erase_display(Self::csi_first(params)),
            'K' => self.erase_line(Self::csi_first(params)),
            'X' => self.erase_chars(Self::csi_first_or1(params)), // ECH
            '@' => self.insert_chars(Self::csi_first_or1(params)), // ICH
            'P' => self.delete_chars(Self::csi_first_or1(params)), // DCH
            'L' => self.insert_lines(Self::csi_first_or1(params)), // IL
            'M' => self.delete_lines(Self::csi_first_or1(params)), // DL
            'S' => self.scroll_region_up(Self::csi_first_or1(params)), // SU
            'T' => self.scroll_region_down(Self::csi_first_or1(params)), // SD
            'r' => {
                // DECSTBM — set scrolling region
                let mut it = params.iter();
                let top = it.next().and_then(|p| p.first()).copied().unwrap_or(1) as usize;
                let bot = it
                    .next()
                    .and_then(|p| p.first())
                    .copied()
                    .unwrap_or(self.height as u16) as usize;
                let top0 = top.saturating_sub(1).min(self.height.saturating_sub(1));
                let bot0 = if bot == 0 {
                    self.height - 1
                } else {
                    (bot - 1).min(self.height - 1)
                };
                if top0 < bot0 {
                    self.scroll_top = top0;
                    self.scroll_bottom = bot0;
                }
                self.cursor_x = 0;
                self.cursor_y = 0;
            }
            's' => {
                *self.saved_cursor = Some((self.cursor_x, self.cursor_y, self.current_attr));
            } // SCP
            'u' => {
                // RCP
                if let Some((x, y, a)) = *self.saved_cursor {
                    self.cursor_x = x.min(self.width - 1);
                    self.cursor_y = y.min(self.height - 1);
                    self.current_attr = a;
                }
            }
            'm' => self.apply_sgr(params),
            // Device Attributes. `CSI c` / `CSI 0 c` is the Primary request
            // (DA1); `CSI > c` / `CSI > 0 c` is the Secondary (DA2). fish
            // blocks on the DA1 reply for up to 10s at startup.
            'c' => match intermediates.first() {
                None => self.reply_da1(),
                Some(&b'>') => self.reply_da2(),
                _ => {}
            },
            // Device Status Report. `CSI 5 n` → terminal OK; `CSI 6 n` →
            // cursor position (CPR). Only the plain (non-`>`) form replies.
            'n' if intermediates.is_empty() => self.reply_dsr(Self::csi_first(params)),
            // XTVERSION — `CSI > 0 q` → the terminal's name + version.
            'q' if intermediates.first() == Some(&b'>') && Self::csi_first(params) == 0 => {
                self.reply_xtversion();
            }
            // DECRQM for ANSI (non-private) modes, `CSI Pm $ p`. We do not
            // track ANSI modes, so answer "mode not recognised" (0) rather
            // than leave the requester waiting.
            'p' if intermediates.contains(&b'$') => {
                let m = Self::csi_first(params);
                self.respond(format!("\x1b[{m};0$y").as_bytes());
            }
            _ => {}
        }
    }

    fn active_grid_id(&self) -> ObjId {
        if *self.alt_active {
            self.alt_grid_id.clone()
        } else {
            self.main_grid_id.clone()
        }
    }
    fn active_len(&self) -> usize {
        if *self.alt_active {
            self.alt_line_ids.len()
        } else {
            self.main_line_ids.len()
        }
    }
    fn active_line_id(&self, idx: usize) -> Option<ObjId> {
        let ids: &Vec<ObjId> = if *self.alt_active {
            self.alt_line_ids
        } else {
            self.main_line_ids
        };
        ids.get(idx).cloned()
    }
    fn active_line_ids_mut(&mut self) -> &mut Vec<ObjId> {
        if *self.alt_active {
            self.alt_line_ids
        } else {
            self.main_line_ids
        }
    }

    /// Insert a fresh blank line object into the active grid at `idx`,
    /// returning its id. Does not touch `line_ids`. Returns `None` if the
    /// underlying doc rejected the insert (nothing sane to fall back to —
    /// callers skip the line rather than fabricate an id).
    fn make_blank_line(&mut self, idx: usize) -> Option<ObjId> {
        let grid_id = self.active_grid_id();
        let text = " ".repeat(self.width);
        let attrs = vec![Attr::default().pack(); self.width].concat();
        let key = if *self.alt_active {
            "terminal_grid_alt"
        } else {
            "terminal_grid"
        };
        if grid_is_document_root(self.state, key, &grid_id) {
            let index = u32::try_from(idx).ok()?;
            ui::tilo_actions::Action::apply(
                &crate::resource::TerminalAction::InsertBlankLine {
                    alternate: *self.alt_active,
                    index,
                    text,
                    attrs,
                },
                self.state,
            )
            .ok()?;
        } else {
            let line_id = self.state.insert_object(&grid_id, idx, ObjType::Map).ok()?;
            let text_id = self
                .state
                .put_object(&line_id, "text", ObjType::Text)
                .ok()?;
            let _ = self.state.splice_text(&text_id, 0, 0, &text);
            let _ = self.state.put(&line_id, "attrs", attrs);
        }
        match self.state.get(&grid_id, idx).ok().flatten() {
            Some((Value::Object(ObjType::Map), id)) => Some(id),
            _ => None,
        }
    }

    fn delete_grid_line(&mut self, index: usize) {
        let grid_id = self.active_grid_id();
        let key = if *self.alt_active {
            "terminal_grid_alt"
        } else {
            "terminal_grid"
        };
        if grid_is_document_root(self.state, key, &grid_id) {
            let Some(index) = u32::try_from(index).ok() else {
                return;
            };
            let _ = ui::tilo_actions::Action::apply(
                &crate::resource::TerminalAction::DeleteGridLine {
                    alternate: *self.alt_active,
                    index,
                },
                self.state,
            );
        } else {
            let _ = self.state.delete(&grid_id, index);
        }
    }

    fn write_grid_line(&mut self, index: usize, text: String, attrs: Vec<u8>) {
        let grid_id = self.active_grid_id();
        let key = if *self.alt_active {
            "terminal_grid_alt"
        } else {
            "terminal_grid"
        };
        if grid_is_document_root(self.state, key, &grid_id) {
            let Some(index) = u32::try_from(index).ok() else {
                return;
            };
            let _ = ui::tilo_actions::Action::apply(
                &crate::resource::TerminalAction::WriteLine {
                    alternate: *self.alt_active,
                    index,
                    text,
                    attrs,
                },
                self.state,
            );
        } else if let Some(line) = self.active_line_id(index) {
            if let Ok(Some((Value::Object(ObjType::Text), text_id))) = self.state.get(&line, "text")
            {
                ui::doc::splice_text_diffed(self.state, &text_id, &text);
            }
            let _ = self.state.put(&line, "attrs", attrs);
        }
    }

    fn line_text_and_attrs(&self, line_obj: &ObjId) -> Option<(String, Vec<u8>)> {
        let Some((Value::Object(ObjType::Text), text_id)) =
            self.state.get(line_obj, "text").ok().flatten()
        else {
            return None;
        };
        let text = self.state.text(&text_id).unwrap_or_default();
        let attrs = match self.state.get(line_obj, "attrs").ok().flatten() {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                automerge::ScalarValue::Bytes(bytes) => bytes.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        Some((text, attrs))
    }

    fn shift_nested_image_placements(&mut self, removed_rows: usize) {
        for index in (0..self.state.length(&self.images_id)).rev() {
            let Ok(Some((Value::Object(ObjType::Map), placement))) =
                self.state.get(&self.images_id, index)
            else {
                continue;
            };
            let alternate = self
                .state
                .get(&placement, "alternate_screen")
                .ok()
                .flatten()
                .and_then(|(value, _)| value.into_scalar().ok())
                .and_then(|value| {
                    matches!(value, automerge::ScalarValue::Boolean(true)).then_some(true)
                })
                .unwrap_or(false);
            if alternate {
                continue;
            }
            let row = self
                .state
                .get(&placement, "row")
                .ok()
                .flatten()
                .and_then(|(value, _)| value.into_scalar().ok())
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(0);
            if row < removed_rows {
                let _ = self.state.delete(&self.images_id, index);
            } else {
                let _ = self
                    .state
                    .put(&placement, "row", (row - removed_rows) as u64);
            }
        }
    }

    fn newline(&mut self) {
        // A line feed leaves the current line; commit any buffered prints first
        // so the appended/scrolled structure is computed over fresh CRDT state
        // (and so a print → auto-wrap → newline doesn't strand the old line).
        self.flush_print_buf();
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
            let idx = self.main_line_ids.len();
            if let Some(line_id) = self.make_blank_line(idx) {
                self.main_line_ids.push(line_id);
            }

            // Bound the retained scrollback. Trimming one line at a time on
            // every append made `Vec::remove(0)` an O(n) shift per output line
            // (O(n²) over a flood); instead let history overrun by a slack
            // margin and drop the overflow in a single `drain`. The trimmed
            // count feeds the widget's incremental `sync` so the render mirror
            // stays aligned.
            if self.main_line_ids.len() > MAX_HISTORY + HISTORY_TRIM_SLACK {
                let overflow = self.main_line_ids.len() - MAX_HISTORY;
                if grid_is_document_root(self.state, "terminal_grid", &self.main_grid_id) {
                    if let Ok(count) = u32::try_from(overflow) {
                        let _ = ui::tilo_actions::Action::apply(
                            &crate::resource::TerminalAction::TrimGridPrefix { count },
                            self.state,
                        );
                        let _ = ui::tilo_actions::Action::apply(
                            &crate::resource::TerminalAction::ShiftImageRows {
                                removed_rows: count,
                            },
                            self.state,
                        );
                    }
                } else {
                    for _ in 0..overflow {
                        let _ = self.state.delete(&self.main_grid_id, 0);
                    }
                    self.shift_nested_image_placements(overflow);
                }
                self.main_line_ids.drain(0..overflow);
                *self.trimmed_front += overflow;
            }
        }
    }

    /// Scroll the current scrolling region up by `n` lines (content moves up;
    /// blank lines appear at the bottom of the region). Honours the DEC
    /// scrolling margins.
    fn scroll_region_up(&mut self, n: usize) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        let top = self.scroll_top.min(len - 1);
        let bot = self.scroll_bottom.min(len - 1);
        if top > bot {
            return;
        }
        let n = n.min(bot - top + 1);
        for _ in 0..n {
            self.delete_grid_line(top);
            self.active_line_ids_mut().remove(top);
            if let Some(line) = self.make_blank_line(bot) {
                self.active_line_ids_mut().insert(bot, line);
            }
        }
    }

    /// Scroll the current scrolling region down by `n` lines (content moves
    /// down; blank lines appear at the top of the region).
    fn scroll_region_down(&mut self, n: usize) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        let top = self.scroll_top.min(len - 1);
        let bot = self.scroll_bottom.min(len - 1);
        if top > bot {
            return;
        }
        let n = n.min(bot - top + 1);
        for _ in 0..n {
            self.delete_grid_line(bot);
            self.active_line_ids_mut().remove(bot);
            if let Some(line) = self.make_blank_line(top) {
                self.active_line_ids_mut().insert(top, line);
            }
        }
    }

    /// IL — insert `n` blank lines at the cursor row, within the region.
    fn insert_lines(&mut self, n: usize) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        let at = self.cursor_y.max(self.scroll_top);
        let bot = self.scroll_bottom.min(len - 1);
        if at > bot {
            return;
        }
        let n = n.min(bot - at + 1);
        for _ in 0..n {
            self.delete_grid_line(bot);
            self.active_line_ids_mut().remove(bot);
            if let Some(line) = self.make_blank_line(at) {
                self.active_line_ids_mut().insert(at, line);
            }
        }
        self.cursor_x = 0;
    }

    /// DL — delete `n` lines at the cursor row, within the region.
    fn delete_lines(&mut self, n: usize) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        let at = self.cursor_y.max(self.scroll_top);
        let bot = self.scroll_bottom.min(len - 1);
        if at > bot {
            return;
        }
        let n = n.min(bot - at + 1);
        for _ in 0..n {
            self.delete_grid_line(at);
            self.active_line_ids_mut().remove(at);
            if let Some(line) = self.make_blank_line(bot) {
                self.active_line_ids_mut().insert(bot, line);
            }
        }
        self.cursor_x = 0;
    }

    fn erase_display(&mut self, mode: u16) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        let screen_top = len.saturating_sub(self.height);
        let cursor_y = self.absolute_y().min(len - 1);
        // The cursor's own row is erased only up to or from the cursor, as
        // `EL` would; blanking it whole wipes a prompt drawn before the cursor.
        let range: Vec<usize> = match mode {
            0 => {
                self.erase_line(0);
                (cursor_y + 1..len).collect()
            }
            1 => {
                self.erase_line(1);
                (screen_top..cursor_y).collect()
            }
            2 | 3 => (screen_top..len).collect(), // whole screen
            _ => return,
        };
        for abs_y in range {
            self.write_grid_line(
                abs_y,
                " ".repeat(self.width),
                vec![Attr::default().pack(); self.width].concat(),
            );
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let y = self.absolute_y().min(self.active_len().saturating_sub(1));
        let Some(line_obj) = self.active_line_id(y) else {
            return;
        };
        let Some((text, mut attrs)) = self.line_text_and_attrs(&line_obj) else {
            return;
        };
        let len = text.chars().count();
        let (from, count): (usize, usize) = match mode {
            0 => (self.cursor_x, len.saturating_sub(self.cursor_x)),
            1 => (0, self.cursor_x.min(len)),
            2 => (0, len),
            _ => return,
        };
        if count == 0 {
            return;
        }
        let text = splice_chars(&text, from, count, &" ".repeat(count));
        blank_attr_range(&mut attrs, from, count);
        self.write_grid_line(y, text, attrs);
    }

    fn erase_chars(&mut self, n: usize) {
        let y = self.absolute_y().min(self.active_len().saturating_sub(1));
        let Some(line_obj) = self.active_line_id(y) else {
            return;
        };
        let Some((text, mut attrs)) = self.line_text_and_attrs(&line_obj) else {
            return;
        };
        let len = text.chars().count();
        let count = n.min(len.saturating_sub(self.cursor_x));
        if count == 0 {
            return;
        }
        let text = splice_chars(&text, self.cursor_x, count, &" ".repeat(count));
        blank_attr_range(&mut attrs, self.cursor_x, count);
        self.write_grid_line(y, text, attrs);
    }

    fn insert_chars(&mut self, n: usize) {
        let y = self.absolute_y().min(self.active_len().saturating_sub(1));
        let Some(line_obj) = self.active_line_id(y) else {
            return;
        };
        let Some((text, attrs)) = self.line_text_and_attrs(&line_obj) else {
            return;
        };
        let n = n.min(self.width.saturating_sub(self.cursor_x));
        if n == 0 {
            return;
        }
        let mut text = splice_chars(&text, self.cursor_x, 0, &" ".repeat(n));
        if text.chars().count() > self.width {
            text = text.chars().take(self.width).collect();
        }
        self.write_grid_line(y, text, attrs);
    }

    fn delete_chars(&mut self, n: usize) {
        let y = self.absolute_y().min(self.active_len().saturating_sub(1));
        let Some(line_obj) = self.active_line_id(y) else {
            return;
        };
        let Some((text, attrs)) = self.line_text_and_attrs(&line_obj) else {
            return;
        };
        let len = text.chars().count();
        let count = n.min(len.saturating_sub(self.cursor_x));
        if count == 0 {
            return;
        }
        let mut text = splice_chars(&text, self.cursor_x, count, "");
        let new_len = text.chars().count();
        if new_len < self.width {
            text.push_str(&" ".repeat(self.width - new_len));
        }
        self.write_grid_line(y, text, attrs);
    }

    /// DEC private set/reset — `CSI ? Ps h` / `CSI ? Ps l`.
    fn set_dec_modes(&mut self, params: &vte::Params, enable: bool) {
        for p in params {
            let Some(&n) = p.first() else { continue };
            match n {
                1 => self.modes.app_cursor_keys = enable,
                25 => self.modes.cursor_visible = enable,
                1000 | 1002 | 1003 => {
                    if enable {
                        self.modes.mouse_report = true;
                        self.modes.mouse_tracking = n;
                    } else if self.modes.mouse_tracking == n {
                        self.modes.mouse_report = false;
                        self.modes.mouse_tracking = 0;
                    }
                }
                1004 => self.modes.focus_report = enable,
                1006 => self.modes.mouse_sgr = enable,
                2004 => self.modes.bracketed_paste = enable,
                1048 => {
                    if enable {
                        *self.saved_cursor =
                            Some((self.cursor_x, self.cursor_y, self.current_attr));
                    } else if let Some((x, y, a)) = *self.saved_cursor {
                        self.cursor_x = x.min(self.width - 1);
                        self.cursor_y = y.min(self.height - 1);
                        self.current_attr = a;
                    }
                }
                47 | 1047 | 1049 => {
                    if enable {
                        self.enter_alt(n == 1049);
                    } else {
                        self.exit_alt(n == 1049);
                    }
                }
                _ => {}
            }
        }
    }

    fn enter_alt(&mut self, save_cursor: bool) {
        if *self.alt_active {
            return;
        }
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
        if !*self.alt_active {
            return;
        }
        *self.alt_active = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.height.saturating_sub(1);
        if restore_cursor && let Some((x, y, a)) = self.saved_cursor.take() {
            self.cursor_x = x.min(self.width.saturating_sub(1));
            self.cursor_y = y.min(self.height.saturating_sub(1));
            self.current_attr = a;
        }
        self.pending_wrap = false;
    }

    /// Blank every line of the alternate buffer and trim it back to exactly
    /// `height` lines (a previous alt session may have left it longer).
    fn clear_alt_buffer(&mut self) {
        let width = self.width;
        let count = self.alt_line_ids.len();
        let text = " ".repeat(width);
        let attrs = vec![Attr::default().pack(); width].concat();
        for index in 0..count {
            self.write_grid_line(index, text.clone(), attrs.clone());
        }
        while self.alt_line_ids.len() > self.height {
            let idx = self.alt_line_ids.len() - 1;
            self.delete_grid_line(idx);
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
        let groups: Vec<Vec<u16>> = params.iter().map(<[u16]>::to_vec).collect();
        if groups.is_empty() {
            self.current_attr = Attr::default();
            return;
        }
        let mut i = 0;
        while let Some(group) = groups.get(i) {
            let code = group.first().copied().unwrap_or(0);
            match code {
                0 => self.current_attr = Attr::default(),
                1 => self.current_attr.bold = true,
                2 => self.current_attr.dim = true,
                3 => self.current_attr.italic = true,
                4 => self.current_attr.underline = true,
                7 => self.current_attr.reverse = true,
                9 => self.current_attr.strike = true,
                // SGR 22 turns off both bold *and* faint (they share the
                // intensity axis in the spec).
                22 => {
                    self.current_attr.bold = false;
                    self.current_attr.dim = false;
                }
                23 => self.current_attr.italic = false,
                24 => self.current_attr.underline = false,
                27 => self.current_attr.reverse = false,
                29 => self.current_attr.strike = false,
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
                        if let Some(col) = group.get(1..).and_then(color_from_subparams) {
                            if is_fg {
                                self.current_attr.fg = col;
                            } else {
                                self.current_attr.bg = col;
                            }
                        }
                    } else {
                        // Semicolon form: pull the following groups.
                        let mode = groups.get(i + 1).and_then(|g| g.first()).copied();
                        match mode {
                            Some(2) => {
                                let r = groups
                                    .get(i + 2)
                                    .and_then(|g| g.first())
                                    .copied()
                                    .unwrap_or(0) as u8;
                                let g = groups
                                    .get(i + 3)
                                    .and_then(|g| g.first())
                                    .copied()
                                    .unwrap_or(0) as u8;
                                let b = groups
                                    .get(i + 4)
                                    .and_then(|g| g.first())
                                    .copied()
                                    .unwrap_or(0) as u8;
                                let col = CellColor::Rgb(r, g, b);
                                if is_fg {
                                    self.current_attr.fg = col;
                                } else {
                                    self.current_attr.bg = col;
                                }
                                i += 4;
                            }
                            Some(5) => {
                                let n = groups
                                    .get(i + 2)
                                    .and_then(|g| g.first())
                                    .copied()
                                    .unwrap_or(0) as u8;
                                let col = CellColor::Indexed(n);
                                if is_fg {
                                    self.current_attr.fg = col;
                                } else {
                                    self.current_attr.bg = col;
                                }
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

    /// Cursor position is tracked in the `cursor_x`/`cursor_y` fields as the
    /// stream is parsed; the CRDT copy is written once per feed by
    /// [`flush_cursor`](Self::flush_cursor) rather than on every cell, so this
    /// is intentionally a no-op kept only so the call sites read naturally.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        clippy::unused_self,
        reason = "deliberate no-op: `&mut self` marks every cursor-moving call site as a mutation for readability/symmetry with flush_cursor, even though the actual CRDT write is batched there"
    )]
    fn update_cursor(&mut self) {}

    /// Commit any buffered run of printed characters to the CRDT: one diffed
    /// text splice (touching only the changed span) + one `attrs` put for the
    /// whole line, in place of the per-character write storm the naive path did.
    /// A no-op when nothing is buffered.
    pub(crate) fn flush_print_buf(&mut self) {
        if let Some(buf) = self.print_buf.take() {
            let grid = if buf.alternate {
                &self.alt_grid_id
            } else {
                &self.main_grid_id
            };
            let key = if buf.alternate {
                "terminal_grid_alt"
            } else {
                "terminal_grid"
            };
            if grid_is_document_root(self.state, key, grid) {
                let _ = ui::tilo_actions::Action::apply(
                    &crate::resource::TerminalAction::WriteLine {
                        alternate: buf.alternate,
                        index: buf.index,
                        text: buf.text,
                        attrs: buf.attrs,
                    },
                    self.state,
                );
            } else if let Ok(Some((Value::Object(ObjType::Text), text_id))) =
                self.state.get(&buf.line_obj, "text")
            {
                ui::doc::splice_text_diffed(self.state, &text_id, &buf.text);
                let _ = self.state.put(&buf.line_obj, "attrs", buf.attrs);
            }
        }
    }

    /// Persist the final cursor position into the CRDT. Called once at the end
    /// of a feed — the cursor is read back only by `sync`/`init_crdt`, so a
    /// single write per batch is enough and avoids per-cell `put` churn.
    pub fn flush_cursor(&mut self) {
        // Commit the last line of a print run that ended the feed (no trailing
        // control byte to flush it).
        self.flush_print_buf();
        let x = self.cursor_x as u64;
        let y = self.absolute_y() as u64;
        if grid_is_document_root(self.state, "terminal_grid", &self.main_grid_id) {
            let _ = ui::tilo_actions::Action::apply(
                &crate::resource::TerminalAction::SetCursor { x, y },
                self.state,
            );
        } else {
            let _ = self.state.put(&self.cursor_id, "x", x);
            let _ = self.state.put(&self.cursor_id, "y", y);
        }
    }

    /// Stage `bytes` to be written back to the PTY once this feed's parse
    /// finishes. The bytes travel the same path a keystroke does
    /// ([`write_input`](TerminalWidget::write_input)), so the shell reads a
    /// query reply exactly as a real terminal's would arrive.
    fn respond(&mut self, bytes: &[u8]) {
        self.responses.extend_from_slice(bytes);
    }

    /// Primary Device Attributes (DA1) reply. We identify as a VT220-class
    /// terminal (`62`) that supports ANSI colour (`22`) — the honest set for an
    /// `xterm-256color` emulator with SGR/truecolor. fish waits on this reply
    /// for up to 10s at startup and disables optional features if it never
    /// arrives.
    fn reply_da1(&mut self) {
        self.respond(b"\x1b[?62;22c");
    }

    /// Secondary Device Attributes (DA2) reply: `CSI > 1 ; Pv ; 0 c`. Terminal
    /// type `1` = VT220, `Pv` = this build's version, trailing `0` = no ROM
    /// cartridge.
    fn reply_da2(&mut self) {
        self.respond(format!("\x1b[>1;{};0c", terminal_da2_version()).as_bytes());
    }

    /// Device Status Report. `Ps == 5` → "terminal OK" (`CSI 0 n`); `Ps == 6`
    /// → Cursor Position Report (`CSI row ; col R`, 1-based, viewport-relative).
    fn reply_dsr(&mut self, ps: u16) {
        match ps {
            5 => self.respond(b"\x1b[0n"),
            6 => {
                let row = self.cursor_y + 1;
                let col = self.cursor_x + 1;
                self.respond(format!("\x1b[{row};{col}R").as_bytes());
            }
            _ => {}
        }
    }

    /// DEC-private DSR. `?6n` (DECXCPR) → `CSI ? row ; col ; 1 R` (the trailing
    /// `1` is the page number).
    fn reply_dsr_private(&mut self, ps: u16) {
        if ps == 6 {
            let row = self.cursor_y + 1;
            let col = self.cursor_x + 1;
            self.respond(format!("\x1b[?{row};{col};1R").as_bytes());
        }
    }

    /// XTVERSION reply — DCS `> | tilo-terminal(<version>) ST`.
    fn reply_xtversion(&mut self) {
        self.respond(
            format!(
                "\x1bP>|{}({})\x1b\\",
                TERMINAL_NAME,
                env!("CARGO_PKG_VERSION")
            )
            .as_bytes(),
        );
    }

    /// DECRQM reply (DECRPM) for a DEC-private mode: `CSI ? Pm ; Pv $ y`, where
    /// `Pv` is `1` (set), `2` (reset) or `0` (mode not recognised). We answer
    /// for the modes this emulator actually tracks so a probe gets the truth.
    fn reply_decrqm(&mut self, mode: u16) {
        let state = match mode {
            1 => Some(self.modes.app_cursor_keys),
            25 => Some(self.modes.cursor_visible),
            1000 | 1002 | 1003 => Some(self.modes.mouse_tracking == mode),
            1004 => Some(self.modes.focus_report),
            1006 => Some(self.modes.mouse_sgr),
            2004 => Some(self.modes.bracketed_paste),
            47 | 1047 | 1049 => Some(*self.alt_active),
            _ => None,
        };
        let pv = match state {
            Some(true) => 1,
            Some(false) => 2,
            None => 0,
        };
        self.respond(format!("\x1b[?{mode};{pv}$y").as_bytes());
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
                let r = sub.get(n - 3).copied().unwrap_or(0) as u8;
                let g = sub.get(n - 2).copied().unwrap_or(0) as u8;
                let b = sub.get(n - 1).copied().unwrap_or(0) as u8;
                Some(CellColor::Rgb(r, g, b))
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

/// Byte offset in `line` where cell column `col` begins, honouring wide
/// (double-width) glyphs. Clamps to the string's byte length for columns
/// past the last glyph (padding region). Inverse of [`byte_to_col`].
pub(crate) fn col_to_byte(line: &str, col: usize) -> usize {
    let mut cells = 0usize;
    for (byte, c) in line.char_indices() {
        if cells >= col {
            return byte;
        }
        cells += c.width().unwrap_or(1).max(1);
    }
    line.len()
}

/// Cell column corresponding to byte offset `byte` in `line`. Inverse of
/// [`col_to_byte`]; a byte at or past the end maps to the total cell width.
pub(crate) fn byte_to_col(line: &str, byte: usize) -> usize {
    let mut cells = 0usize;
    for (b, c) in line.char_indices() {
        if b >= byte {
            return cells;
        }
        cells += c.width().unwrap_or(1).max(1);
    }
    cells
}

/// The character occupying cell column `col` of `line`, honouring wide
/// (double-width) glyphs. Columns that land on a wide glyph's trailing
/// continuation cell report a space. Returns `None` past the line's end.
pub(crate) fn cell_char_at(line: &str, col: usize) -> Option<char> {
    let mut idx = 0usize;
    for c in line.chars() {
        let w = c.width().unwrap_or(1).max(1);
        if idx == col {
            return Some(c);
        }
        if idx + w > col {
            // Inside a wide glyph's continuation cell.
            return Some(' ');
        }
        idx += w;
    }
    None
}

pub(crate) fn map_ansi_256(code: u8, palette: &ui::tokens::TerminalPalette) -> Color {
    if code < 16 {
        palette
            .ansi
            .get(code as usize)
            .copied()
            .unwrap_or(palette.fg_default)
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
pub(crate) fn resolve_color(
    c: CellColor,
    palette: &ui::tokens::TerminalPalette,
    is_fg: bool,
    bold: bool,
) -> Color {
    match c {
        CellColor::Default => {
            if is_fg {
                palette.fg_default
            } else {
                palette.bg_default
            }
        }
        CellColor::Indexed(mut n) => {
            if bold && is_fg && n < 8 {
                n += 8;
            }
            map_ansi_256(n, palette)
        }
        CellColor::Rgb(r, g, b) => Color::from_rgb8(r, g, b),
    }
}

/// Resolve a cell's `(foreground, optional background)` colors, applying
/// reverse video. A `None` background means "leave the default terminal
/// background showing" (no fill).
pub(crate) fn resolve_pair(
    attr: Attr,
    palette: &ui::tokens::TerminalPalette,
) -> (Color, Option<Color>) {
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
    // A near-solid block (90% alpha) reads as a firm caret while the glyph
    // beneath it is re-drawn in the background color for an inverted-cell look.
    // The hue comes from the theme's default foreground so the cursor tracks
    // palette swaps rather than a hard-coded grey.
    palette.fg_default.with_alpha_factor(230.0 / 255.0)
}

/// Resolve the user's native shell for the standalone fallback path.
/// Windows uses `COMSPEC`, falling back to `cmd.exe`.
///
/// Unix resolution order:
/// 1. **`$SHELL`** when set and non-empty — the user's explicit preference
///    (this is how fish, when it is the login shell, gets launched here).
/// 2. **The passwd database** (`getpwuid(getuid()).pw_shell`) — the login
///    shell the system records for this account. A login session that spawned
///    the terminal through a path that does not export `$SHELL` still lands on
///    the right shell instead of a hardcoded `/bin/sh`.
/// 3. **`/bin/sh`** — last resort so a session never fails to start.
pub fn resolve_login_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC")
            .ok()
            .filter(|shell| !shell.trim().is_empty())
            .unwrap_or_else(|| "cmd.exe".into())
    }
    #[cfg(unix)]
    resolve_login_shell_with(std::env::var("SHELL").ok(), login_shell_from_passwd())
}

/// Pure core of [`resolve_login_shell`], with the two environment lookups
/// injected so the precedence is testable without mutating process-global
/// state (which would race parallel tests).
#[cfg(unix)]
pub(crate) fn resolve_login_shell_with(
    shell_env: Option<String>,
    passwd_shell: Option<String>,
) -> String {
    if let Some(s) = shell_env
        && !s.is_empty()
    {
        return s;
    }
    if let Some(s) = passwd_shell
        && !s.is_empty()
    {
        return s;
    }
    "/bin/sh".into()
}

/// The command the best-effort route runs: the user's native shell as chosen
/// by [`resolve_login_shell`], with the process environment inherited.
pub(crate) fn login_shell_command() -> CommandBuilder {
    CommandBuilder::new(resolve_login_shell())
}

/// The login shell recorded for the current real user in the passwd database
/// (`pw_shell`). `None` if the entry is missing or has an empty shell field.
#[cfg(unix)]
pub(crate) fn login_shell_from_passwd() -> Option<String> {
    use std::ffi::CStr;
    // SAFETY: `getpwuid` returns a pointer into a static buffer owned by libc
    // (valid until the next passwd/`getpwuid` call, of which we make none
    // before copying out). We null-check before dereferencing, and copy the
    // `pw_shell` C string into an owned `String` while the pointer is live.
    unsafe {
        let uid = libc::getuid();
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return None;
        }
        let shell = (*pw).pw_shell;
        if shell.is_null() {
            return None;
        }
        Some(CStr::from_ptr(shell).to_string_lossy().into_owned())
    }
}

pub struct TerminalBackend {
    pub writer: Box<dyn Write + Send>,
    pub reader: Box<dyn Read + Send>,
    pub master: Box<dyn MasterPty + Send>,
    pub shell_name: String,
    #[cfg(unix)]
    service_lifetime: Option<std::os::unix::net::UnixStream>,
    #[cfg(windows)]
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
}
impl TerminalBackend {
    /// Open a PTY running a shell.
    ///
    /// **Not `$SHELL` directly.** Inside a Tilo app sandbox there is no `/bin`
    /// at all: `launcher::profile` binds the loader, fonts, `/sys` and the CA
    /// bundle, and deliberately binds no executable directory — so `exec`ing
    /// `$SHELL` fails with `ENOENT`, and would still leave `vim` and `less`
    /// broken (no terminfo) and the prompt printing `I have no name!` (no
    /// `/etc/passwd`) even if it did not.
    ///
    /// What runs instead is an `ush` guest: a root filesystem composed of the
    /// host's, read-only, plus a layer belonging to the user. That gives
    /// `/usr/bin`, terminfo and `/etc/passwd` by construction rather than as
    /// grants. See `crates/runtime/ush`.
    ///
    /// Three routes, and the order is the trust boundary. The route is chosen by
    /// `confinement_route` from a capability probe, not by trying each in turn
    /// and hoping — so a machine that cannot host a guest is never handed one to
    /// fail in, and confinement is never silently downgraded on Tilo.
    ///
    /// 1. **The service socket.** A sandboxed app cannot build a namespace —
    ///    its seccomp filter denies `unshare` and `mount`. So it asks the
    ///    session's supervisor and receives a pty master over `SCM_RIGHTS`.
    ///    This is the production path inside Tilo, and it is **mandatory when
    ///    available**: if the launcher opened the service descriptor, we use it
    ///    and *fail closed* on any handshake error rather than falling through
    ///    to an unconfined shell. That is what makes the Tilo path non-optional —
    ///    nothing an app controls (its own environment) can select best-effort
    ///    while the supervisor is reachable.
    /// 2. **Spawning the runtime.** An unconfined host that can genuinely build
    ///    the namespace a guest needs (unprivileged user namespaces enabled, not
    ///    already behind a seccomp filter) *and* has the runtime installed — a
    ///    development tree, mostly. The terminal starts a guest itself so that
    ///    session is confined the same way Tilo confines it.
    /// 3. **`$SHELL` (best-effort).** No Tilo confinement is available on this
    ///    machine: a plain distribution, or a build of the terminal running
    ///    outside Tilo. Start the user's login shell directly so normal work
    ///    still functions. The child inherits whatever ambient sandbox the
    ///    process already has, so it runs truly unconfined only on a truly
    ///    unconfined host — and this route is unreachable whenever route 1 is.
    ///
    /// The two kernel-feature probes (`seccomp_filtered`,
    /// `unprivileged_userns_enabled`) are the robust discriminator between "I
    /// am a confined Tilo app" and "I am on a normal host": a seccomp filter
    /// cannot be removed once installed, so an attacker inside the sandbox
    /// cannot flip route 2 on to escape, and cannot reach an *escalated*
    /// best-effort shell — route 3's child is still bounded by the launcher's
    /// seccomp + namespace + bind mounts.
    ///
    /// # Errors
    ///
    /// Returns an error if no suitable confinement route is available or the backend fails to initialize.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let program = launch_command_from_env();
        match confinement_route() {
            ConfineRoute::Service => {
                // Mandatory, fail-closed: never fall through to an unconfined
                // shell when the supervisor is present.
                ush_service_backend(&program)
            }
            ConfineRoute::SelfHosted(bin) => {
                let mut cmd = CommandBuilder::new(&bin);
                cmd.arg("run");
                if !program.is_empty() {
                    cmd.arg("-c");
                    cmd.arg(exec_script(&program));
                }
                Self::with_command(cmd)
            }
            ConfineRoute::BestEffort => {
                // A single quiet line, not a wall of errors: this is the normal,
                // expected state off Tilo.
                tracing::info!(
                    "terminal: Tilo app confinement is not available here; \
                     starting your login shell directly"
                );
                if let Some((first, rest)) = program.split_first() {
                    let mut cmd = CommandBuilder::new(first);
                    cmd.args(rest);
                    return Self::with_command(cmd);
                }
                Self::with_command(login_shell_command())
            }
        }
    }

    /// Open a PTY running an arbitrary command. `TERM` is forced to
    /// `xterm-256color` so programs emit the SGR / DEC sequences this emulator
    /// understands, regardless of a launcher that happens to be non-terminal.
    ///
    /// # Errors
    ///
    /// Returns an error if the PTY cannot be created or the command cannot be spawned.
    pub fn with_command(mut cmd: CommandBuilder) -> Result<Self, Box<dyn std::error::Error>> {
        cmd.env("TERM", "xterm-256color");
        let shell_name = cmd
            .get_argv()
            .first()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        #[cfg(unix)]
        drop(child);
        Ok(Self {
            writer,
            reader,
            master: pair.master,
            shell_name,
            #[cfg(unix)]
            service_lifetime: None,
            #[cfg(windows)]
            child,
        })
    }
}

/// The program the desktop asked this terminal to run instead of a login
/// shell — how a `Terminal=true` desktop entry opens. The shell passes it in
/// the launcher's bounded context channel as `{"terminal_command": [argv]}`;
/// anything else there (or nothing) means an ordinary shell.
pub fn launch_command_from_env() -> Vec<String> {
    std::env::var("TILO_TOOL_CONTEXT")
        .ok()
        .map(|context| launch_command_from_context(&context))
        .unwrap_or_default()
}

/// [`launch_command_from_env`] over an explicit context value.
pub fn launch_command_from_context(context: &str) -> Vec<String> {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(context)
    else {
        return Vec::new();
    };
    let Some(serde_json::Value::Array(argv)) = map.get("terminal_command") else {
        return Vec::new();
    };
    let argv: Option<Vec<String>> = argv
        .iter()
        .map(|arg| {
            arg.as_str()
                .filter(|arg| !arg.contains('\0'))
                .map(str::to_string)
        })
        .collect();
    argv.filter(|argv| argv.first().is_some_and(|program| !program.is_empty()))
        .unwrap_or_default()
}

/// One `sh -c` script that runs `argv` exactly, each word single-quoted.
pub fn exec_script(argv: &[String]) -> String {
    let words: Vec<String> = argv
        .iter()
        .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
        .collect();
    format!("exec {}", words.join(" "))
}

/// The environment variable the launcher sets when it has opened the `ush`
/// service socket on this app's behalf, per `[sandbox.services]`.
pub const USH_SERVICE_FD_ENV: &str = "TILO_SVC_USH_FD";

/// Which way the terminal will obtain its shell, decided by capability probe.
///
/// Public + tested: the point of this task is that the decision is *correct*
/// and *robust*, not that a spawn happens to fail gracefully. See
/// [`confinement_route`] and [`TerminalBackend::new`].
#[derive(Debug, PartialEq, Eq)]
pub enum ConfineRoute {
    /// The supervisor service is reachable — the Tilo production path. Mandatory
    /// when chosen: the caller must not fall back to an unconfined shell if the
    /// handshake fails.
    Service,
    /// This unconfined host can build a guest itself and has the runtime at the
    /// given path.
    SelfHosted(std::path::PathBuf),
    /// No Tilo confinement is available; start `$SHELL` directly.
    BestEffort,
}

/// Choose how to obtain the shell from a capability probe, security-first.
///
/// The ordering encodes the invariant "on Tilo, always confine; off Tilo, best
/// effort — and best-effort can never be forced on Tilo":
///
/// 1. If the launcher handed us a live supervisor descriptor, take it. This is
///    the only signal that positively identifies "confined Tilo app with a
///    supervisor", and it wins unconditionally.
/// 2. Otherwise self-host *only* when we could actually succeed: the kernel
///    lets this process build a user namespace (so we are not behind a seccomp
///    filter that would deny `unshare`, and unprivileged userns are enabled)
///    and the runtime binary exists. Gating on the probe — rather than
///    launching `tilo-ush` and letting it fail — is what removes the wall of
///    `bwrap`/mount errors on a machine that could never host a guest.
/// 3. Otherwise there is no confinement to be had here; best-effort.
///
/// The security argument for (1)+(2): a seccomp filter cannot be lifted once
/// installed, so on Tilo an app cannot clear it to unlock route 2, and clearing
/// the service env var only drops it to route 3 — whose shell is still bounded
/// by the launcher's seccomp + namespace + bind mounts. Best-effort therefore
/// never grants more than the ambient sandbox already allows.
pub fn confinement_route() -> ConfineRoute {
    #[cfg(windows)]
    {
        // A claimed hosted grant must fail closed until native service
        // transport exists; never silently replace it with a local shell.
        if std::env::var_os(USH_SERVICE_FD_ENV).is_some() {
            ConfineRoute::Service
        } else {
            ConfineRoute::BestEffort
        }
    }
    #[cfg(unix)]
    decide_route(
        ush_service_fd().is_some(),
        can_build_namespace(),
        ush_binary(),
    )
}

/// Pure core of [`confinement_route`], with the three probed inputs injected so
/// the decision — the actual trust boundary — is unit-testable without touching
/// process-global state (a live service fd, the host's kernel features, an
/// installed runtime). This IS the production decision; `confinement_route`
/// only supplies the probes.
///
/// The invariant under test: `service_available` wins unconditionally, so
/// best-effort is unreachable whenever the supervisor is present — no
/// combination of the other two inputs can override it. And `can_self_host`
/// gates route 2, so a present-but-unusable runtime yields best-effort rather
/// than a doomed spawn.
pub fn decide_route(
    service_available: bool,
    can_self_host: bool,
    runtime: Option<std::path::PathBuf>,
) -> ConfineRoute {
    if service_available {
        return ConfineRoute::Service;
    }
    if can_self_host && let Some(bin) = runtime {
        return ConfineRoute::SelfHosted(bin);
    }
    ConfineRoute::BestEffort
}

/// The supervisor-service descriptor, if the launcher opened one for us and it
/// is still a live fd.
///
/// An env var alone is not enough: the descriptor slot must actually be open,
/// so a stale or spoofed number pointing at nothing does not select the service
/// path (it falls through to the probe below and, off Tilo, to best-effort).
#[cfg(unix)]
pub(crate) fn ush_service_fd() -> Option<i32> {
    let raw: i32 = std::env::var(USH_SERVICE_FD_ENV).ok()?.parse().ok()?;
    if raw < 0 {
        return None;
    }
    // SAFETY: `F_GETFD` only reads the descriptor's flags and never mutates or
    // closes it. `rc == -1` (EBADF) means the slot is empty → no service.
    let rc = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    (rc != -1).then_some(raw)
}

/// Whether this process could build the user + mount namespace a self-hosted
/// guest needs.
///
/// False when a seccomp filter is confining us (it would deny `unshare`), when
/// unprivileged user namespaces are disabled by sysctl, or when `bwrap` — which
/// the runtime shells out to for the actual namespace setup — is not installed.
/// Any of those means launching the runtime here would only produce errors, so
/// we don't.
#[cfg(unix)]
fn can_build_namespace() -> bool {
    !seccomp_filtered() && unprivileged_userns_enabled() && bwrap_present()
}

/// True if a seccomp filter is installed on this process — i.e. the Tilo launcher
/// (or any foreign sandbox) is confining our syscalls.
///
/// Read from `/proc/self/status` (`Seccomp:` ≥ 2 is "filter mode"). A filter
/// cannot be removed once installed and survives `exec`, so an attacker inside
/// a sandbox cannot clear this to make [`can_build_namespace`] return true.
#[cfg(unix)]
fn seccomp_filtered() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|l| l.strip_prefix("Seccomp:").map(|v| v.trim().to_string()))
        })
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|mode| mode >= 2)
}

/// Whether the kernel will let an unprivileged process create a user namespace.
///
/// Reads the two sysctls distributions use to gate this. An absent file means
/// the kernel default (enabled), so this only reports `false` on an explicit
/// disable — never guessing "no" just because a knob is missing.
#[cfg(unix)]
fn unprivileged_userns_enabled() -> bool {
    // Debian/Ubuntu knob: `0` disables unprivileged user namespaces entirely.
    if let Ok(v) = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        && v.trim() == "0"
    {
        return false;
    }
    // Upstream cap on how many user namespaces may exist: `0` means none.
    if let Ok(v) = std::fs::read_to_string("/proc/sys/user/max_user_namespaces")
        && v.trim() == "0"
    {
        return false;
    }
    true
}

/// Whether `bwrap` (Bubblewrap) is on `PATH`. The runtime builds the guest's
/// namespace with it, so its absence means a self-hosted guest cannot start.
#[cfg(unix)]
fn bwrap_present() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("bwrap").exists()))
}

/// Where the runtime binary is when the terminal has to start one itself.
///
/// `$TILO_USH_BIN` first so a development tree can point at `target/debug`,
/// then the installed location, then `PATH`. Absent from all three means this
/// machine has no runtime and the caller falls back to `$SHELL`.
#[cfg(unix)]
pub fn ush_binary() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os("TILO_USH_BIN") {
        let path = std::path::PathBuf::from(explicit);
        if path.exists() {
            return Some(path);
        }
    }
    let installed = std::path::PathBuf::from("/usr/libexec/tilo-ush");
    if installed.exists() {
        return Some(installed);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("tilo-ush"))
        .find(|candidate| candidate.exists())
}

/// Ask the session supervisor for a guest and wrap the pty it hands back.
///
/// The whole exchange is one line of JSON out and one line plus a descriptor
/// back. What crosses the sandbox boundary is a terminal, not a namespace: the
/// guest is built on the supervisor's side, by the supervisor's rules, and this
/// app never learns where its layer is or what it was granted.
#[cfg(unix)]
fn ush_service_backend(program: &[String]) -> Result<TerminalBackend, Box<dyn std::error::Error>> {
    use std::os::fd::{FromRawFd, RawFd};
    use std::os::unix::net::UnixStream;

    let raw: RawFd = std::env::var(USH_SERVICE_FD_ENV)?.parse()?;
    // SAFETY: the launcher passed this descriptor as a connected socket and
    // documents it in `TILO_SVC_<NAME>_FD`. `from_raw_fd` takes ownership, which
    // is correct — nothing else in this process refers to it.
    let stream = unsafe { UnixStream::from_raw_fd(raw) };
    ush_backend_on(&stream, program)
}

#[cfg(windows)]
fn ush_service_backend(_program: &[String]) -> Result<TerminalBackend, Box<dyn std::error::Error>> {
    Err("Tilo hosted terminal service transport is unavailable on Windows".into())
}

#[cfg(unix)]
fn ush_backend_on(
    stream: &std::os::unix::net::UnixStream,
    program: &[String],
) -> Result<TerminalBackend, Box<dyn std::error::Error>> {
    use std::os::fd::AsRawFd;
    // The session runs in the default environment; which target a tab opens is
    // the supervisor's to build, never this app's.
    let mut request = serde_json::json!({
        "op": "open-session",
        "target": tilo_environment::Target::default_environment(),
        "cols": 80,
        "rows": 24,
        "term": "xterm-256color",
    });
    if let (false, Some(fields)) = (program.is_empty(), request.as_object_mut()) {
        fields.insert("command".to_string(), serde_json::json!(program));
    }
    let mut request = serde_json::to_vec(&request)?;
    request.push(b'\n');
    {
        let mut w = stream;
        w.write_all(&request)?;
        w.flush()?;
    }

    let (payload, fd) = runtime_portal::fd_transport::recv_with_owned_fd(stream.as_raw_fd())?;
    let text = String::from_utf8_lossy(&payload);
    let fd = fd.ok_or_else(|| format!("the ush service refused this session: {}", text.trim()))?;

    // SAFETY: `fd` arrived over `SCM_RIGHTS` and this process now owns it. The
    // supervisor sends a pty master and nothing else.
    let master = std::fs::File::from(fd);
    let reader = Box::new(master.try_clone()?) as Box<dyn Read + Send>;
    let writer = Box::new(master.try_clone()?) as Box<dyn Write + Send>;
    Ok(TerminalBackend {
        writer,
        reader,
        master: Box::new(FdMaster { file: master }),
        shell_name: "ush".into(),
        service_lifetime: Some(stream.try_clone()?),
    })
}

/// A [`MasterPty`] over a descriptor someone else opened.
///
/// `portable-pty` can only hand out a master it created itself, and the one
/// that matters here was created by the supervisor and passed over a socket.
/// Resizing has to keep working — the grid reflows on every window change — so
/// this is a real `TIOCSWINSZ`, not a stub.
#[cfg(unix)]
struct FdMaster {
    file: std::fs::File,
}

#[cfg(unix)]
impl MasterPty for FdMaster {
    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        use std::os::fd::AsRawFd;
        // SAFETY: `winsize` is fully written by the ioctl; `self.file` is a
        // live pty master.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(PtySize {
            rows: ws.ws_row,
            cols: ws.ws_col,
            pixel_width: ws.ws_xpixel,
            pixel_height: ws.ws_ypixel,
        })
    }

    fn resize(&self, size: PtySize) -> Result<(), anyhow::Error> {
        use std::os::fd::AsRawFd;
        let ws = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.pixel_width,
            ws_ypixel: size.pixel_height,
        };
        // SAFETY: as above.
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(self.file.try_clone()?))
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(self.file.try_clone()?))
    }

    fn process_group_leader(&self) -> Option<i32> {
        use std::os::fd::AsRawFd;
        let mut pgrp: i32 = -1;
        // SAFETY: `TIOCGPGRP` writes one `pid_t`; the file is a pty master.
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), libc::TIOCGPGRP, &mut pgrp) };
        (rc == 0 && pgrp > 0).then_some(pgrp)
    }

    fn as_raw_fd(&self) -> Option<i32> {
        use std::os::fd::AsRawFd;
        Some(self.file.as_raw_fd())
    }

    fn tty_name(&self) -> Option<std::path::PathBuf> {
        None
    }
}

impl TerminalWidget {
    /// Wire a spawned [`TerminalBackend`] into a widget: share the writer +
    /// master behind mutexes and start a reader thread that funnels PTY output
    /// onto the channel the widget drains in `sync` / `update`. The reader
    /// thread exits when the shell closes the PTY or the widget (hence the
    /// receiver) is dropped.
    fn from_backend(backend: TerminalBackend) -> Self {
        let writer = std::sync::Arc::new(std::sync::Mutex::new(backend.writer));
        let master = std::sync::Arc::new(std::sync::Mutex::new(backend.master));
        let (output_tx, output_rx) = sync_channel(PTY_QUEUE_CHUNKS);

        let mut widget = TerminalWidget::new(writer, master, output_rx, ContentStore::for_app())
            .with_local_session();
        #[cfg(unix)]
        {
            widget.service_lifetime = backend.service_lifetime;
        }
        #[cfg(windows)]
        let widget = {
            let mut widget = widget;
            // ConPTY keeps its output pipe open while the pseudoconsole exists.
            // Observe the child handle too, so exit is visible before closing
            // the terminal window. Keep a killer for normal window teardown.
            widget.child_killer = Some(backend.child.clone_killer());
            let disconnected = widget.session_disconnected.clone();
            let status = widget.session_status_message.clone();
            let waker = widget.waker.clone();
            let pending = widget.wake_pending.clone();
            let mut child = backend.child;
            if std::thread::Builder::new()
                .name("tilo-terminal-child-wait".into())
                .spawn(move || {
                    let message = if child.wait().is_ok() {
                        "Session ended — input unavailable"
                    } else {
                        "Terminal process status failed — input unavailable"
                    };
                    set_terminal_disconnected(&disconnected, &status, message);
                    request_terminal_wake(&waker, &pending);
                })
                .is_err()
            {
                if let Some(killer) = &mut widget.child_killer {
                    let _ = killer.kill();
                }
                widget.mark_disconnected("Terminal unavailable — process monitor could not start");
            }
            widget
        };
        // Share the waker + coalescing flag with the reader thread so fresh PTY
        // output can wake an idle `ui::App` loop (instead of a ~60 Hz poll).
        let waker = widget.waker.clone();
        let wake_pending = widget.wake_pending.clone();
        let session_disconnected = widget.session_disconnected.clone();
        let session_status_message = widget.session_status_message.clone();

        // The reader distinguishes a hosted session's start-up `EIO` from its
        // exit by the lifetime socket the supervisor closes when the guest
        // ends. A local PTY has none.
        #[cfg(unix)]
        let reader_lifetime = widget
            .service_lifetime
            .as_ref()
            .and_then(|socket| socket.try_clone().ok());
        #[cfg(unix)]
        let session_open = move || reader_lifetime.as_ref().is_some_and(lifetime_socket_open);
        #[cfg(not(unix))]
        let session_open = || false;
        let reader = std::sync::Arc::new(std::sync::Mutex::new(backend.reader));
        let reader_result = std::thread::Builder::new()
            .name("tilo-terminal-pty-reader".to_string())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    // A poisoned lock means the reader is unrecoverable (some
                    // other thread panicked while holding it); nothing to do but
                    // stop this reader rather than panic here too.
                    let Ok(mut r) = reader.lock() else {
                        set_terminal_disconnected(
                            &session_disconnected,
                            &session_status_message,
                            "Terminal reader failed — session unavailable",
                        );
                        request_terminal_wake(&waker, &wake_pending);
                        break;
                    };
                    let read = std::io::Read::read(&mut *r, &mut buf);
                    match classify_pty_read(&read, &session_open) {
                        PtyReadDisposition::Retry => {
                            // Avoid a hot loop while the guest is between
                            // opening/closing slave descriptors.
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        PtyReadDisposition::Closed => {
                            set_terminal_disconnected(
                                &session_disconnected,
                                &session_status_message,
                                "Session ended — input unavailable",
                            );
                            request_terminal_wake(&waker, &wake_pending);
                            break;
                        }
                        PtyReadDisposition::Data(n) => {
                            let Some(chunk) = buf.get(..n) else { break };
                            if output_tx.send(chunk.to_vec()).is_err() {
                                break;
                            }
                            // Wake the event loop so `sync` drains the PTY. Coalesced
                            // via `wake_pending`: at most one in-flight wake until the
                            // App delivers it (`on_external_event` clears the flag).
                            request_terminal_wake(&waker, &wake_pending);
                        }
                    }
                }
            });
        if reader_result.is_err() {
            widget.mark_disconnected("Terminal unavailable — PTY reader could not start");
            request_terminal_wake(&widget.waker, &widget.wake_pending);
        }

        widget
    }

    /// Construct a hosted terminal using its required process service.
    ///
    /// # Errors
    ///
    /// Returns an error if PTY creation fails.
    #[cfg(unix)]
    pub fn spawn_hosted(
        services: crate::HostedProcessServices,
    ) -> Result<Self, crate::HostedTerminalError> {
        let stream = services
            .ush
            .ok_or(crate::HostedTerminalError::RequiredServiceMissing("ush"))?;
        let backend = ush_backend_on(&stream, &[])
            .map_err(|error| crate::HostedTerminalError::ServiceFailed(error.to_string()))?;
        Ok(Self::from_backend(backend))
    }

    /// Construct a hosted terminal only when the host provides its service.
    ///
    /// # Errors
    /// Windows currently has no Tilo hosted process-service transport.
    #[cfg(windows)]
    pub fn spawn_hosted(
        _services: crate::HostedProcessServices,
    ) -> Result<Self, crate::HostedTerminalError> {
        Err(crate::HostedTerminalError::RequiredServiceMissing("ush"))
    }

    /// Spawn the standalone user login shell.
    /// # Errors
    /// Returns PTY or process creation errors.
    pub fn spawn_default() -> anyhow::Result<Self> {
        let backend = TerminalBackend::new().map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(Self::from_backend(backend))
    }

    /// Spawn an arbitrary command in a PTY + reader thread and return a fully
    /// wired `TerminalWidget`. This is the embedding entry point for hosts that
    /// want the terminal to run a specific program (a build, a REPL, `ssh …`)
    /// instead of a login shell.
    ///
    /// ```no_run
    /// use terminal::{TerminalWidget, CommandBuilder};
    /// let cmd = CommandBuilder::new("htop");
    /// let _term = TerminalWidget::spawn_command(cmd).unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if PTY creation or the command spawn fails.
    pub fn spawn_command(cmd: CommandBuilder) -> anyhow::Result<Self> {
        let backend =
            TerminalBackend::with_command(cmd).map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(Self::from_backend(backend))
    }

    /// A terminal with a null backend: a sink writer, an empty reader, and a
    /// mock PTY master. No shell is spawned, so nothing arrives on the output
    /// channel — used by headless screenshot scenarios that drive the grid
    /// entirely through `feed`, keeping the frame deterministic.
    ///
    /// # Errors
    ///
    /// This function should not fail under normal circumstances as it uses a null backend.
    pub fn spawn_headless() -> anyhow::Result<Self> {
        struct NullMaster;
        impl MasterPty for NullMaster {
            fn get_size(&self) -> Result<PtySize, anyhow::Error> {
                Ok(PtySize::default())
            }
            fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
                Ok(())
            }
            fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
                Ok(Box::new(std::io::sink()))
            }
            fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
                Ok(Box::new(std::io::empty()))
            }
            #[cfg(unix)]
            fn process_group_leader(&self) -> Option<i32> {
                None
            }
            #[cfg(unix)]
            fn as_raw_fd(&self) -> Option<i32> {
                None
            }
            #[cfg(unix)]
            fn tty_name(&self) -> Option<std::path::PathBuf> {
                None
            }
        }
        let writer: Box<dyn Write + Send> = Box::new(std::io::sink());
        let (_tx, output_rx) = std::sync::mpsc::channel();
        Ok(TerminalWidget::new(
            std::sync::Arc::new(std::sync::Mutex::new(writer)),
            std::sync::Arc::new(std::sync::Mutex::new(Box::new(NullMaster))),
            output_rx,
            ContentStore::memory(),
        ))
    }
}

#[cfg(windows)]
impl Drop for TerminalWidget {
    fn drop(&mut self) {
        if let Some(killer) = &mut self.child_killer {
            let _ = killer.kill();
        }
    }
}
