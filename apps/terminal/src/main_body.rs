use ui::ColorCompat;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};
use ui::{
    Theme,
    paint::PaintCtx,
    text_render::{TextRenderer, TextStyle, TextWeight},
    tokens::TextRole,
    widget::{
        Banner, BannerContent, BannerKind, EmptyState, EmptyStateContent, FindOptions, Findable,
        PressOutcome, ScrollMetrics, Scrollbar, Widget, paint_find_match,
    },
};
// Shared text-selection + clipboard primitives from the toolkit — the terminal
// reuses these rather than re-implementing word/line boundary walking, multi-
// click gesture detection, or clipboard access.
use base64::Engine as _;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Mutex};
use taffy::{NodeId, TaffyTree};
use tilo_content::ContentStore;
use ui::collaboration::PresencePayload;
use ui::icons::Icon;
use ui::permissions::{AppKind as PermAppKind, PermissionContext};
use ui::presence::{PresenceCtx, PresenceKey};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::{
    DragSelect, Granularity, clipboard_copy, clipboard_copy_primary, clipboard_paste,
    clipboard_paste_primary, extend_span_drag, line_boundaries, word_boundaries,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use vte::{Parser, Perform};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
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

/// What the session banner says once the shell has exited.
const SESSION_ENDED_MESSAGE: &str =
    "Session ended. The shell has exited, so typing no longer reaches it.";

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
fn set_terminal_disconnected(disconnected: &AtomicBool, message: &Mutex<String>, status: &str) {
    if !disconnected.swap(true, Ordering::AcqRel)
        && let Ok(mut message) = message.lock()
    {
        status.clone_into(&mut message);
    }
}

/// Wake an idle app after either fresh PTY bytes *or a lifecycle transition*.
/// The latter is crucial: EOF has no bytes to queue, but it still changes
/// visible text, accessibility, and action availability. The wake handle
/// coalesces wakes the host has not delivered yet; a host that gave the
/// terminal none pumps it itself (`needs_animation_frame`).
pub(crate) fn request_terminal_wake(waker: &Mutex<ui::WakeHandle>) {
    if let Ok(waker) = waker.lock() {
        waker.wake();
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
    /// widget's link registry (`TerminalWidget::links`, the screen's links)
    /// to resolve the target URL for rendering (underline + link tint) and
    /// click-to-open. Packed alongside the cell so it rides the same
    /// session record and render mirror as every other attr.
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
/// a cell grid its session record shares, and implements the toolkit's `Widget` trait so it
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
    /// Local input/output invalidates deferred speech even at the same caret.
    /// Exhaustion fails closed rather than reusing an epoch.
    input_epoch: std::cell::Cell<u64>,
    native_editor: Option<crate::native_editor::Client>,
    native_capture: Option<crate::native_editor::Capture>,
    native_history: Option<(ui::tilo_actions::InvocationId, ui::text_input::Receipt)>,
    native_ime: Option<ui::text_input::Receipt>,
    native_ime_error: Option<ui::text_input::Failure>,
    native_read_only: bool,
    pty_write: Arc<Mutex<Box<dyn Write + Send>>>,
    pty_master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    service_lifetime: Option<runtime_portal::pending_connection::SessionLifetime>,
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
    session_status_message: Arc<Mutex<String>>,
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
    /// The grid's text style: the theme's `Code` role, cached on every
    /// `build_layout` and `draw`. `measure` runs before `draw` and does not
    /// receive a theme, so it reads this cached style. `None` until the first
    /// layout, when the seeded cell size stands in.
    cell_style: Option<TextStyle>,
    /// The terminal's palette for the active theme: the theme's terminal
    /// palette with the default background pinned to the window ground, so a
    /// default-background cell is the surface the framework already painted.
    palette: ui::tokens::TerminalPalette,

    /// What the terminal shows. A widget running a process keeps the
    /// emulator's screen here; one without (a past version, a collaborator's
    /// view, a screenshot before its output) shows the session record's,
    /// presented to it.
    screen: crate::screen::Screen,
    /// The lines `grid`/`grid_attrs` were last built from, so a rebuild
    /// converts only lines that changed.
    mirrored: Vec<std::rc::Rc<crate::screen::Line>>,
    /// Whether this widget's own emulator draws `screen`: set by the first
    /// output it parses. Until then the screen is the record's.
    emulating: bool,
    /// The screen the session record holds, as this widget last wrote it;
    /// `None` while the record's is unknown (a new process, a refused
    /// write), so the next write replaces it whole.
    synced: Option<crate::screen::Screen>,
    /// Session changes waiting for the next sync, where the record is at
    /// hand: they apply in order, before the output parsed since.
    pending_actions: Vec<crate::resource::TerminalAction>,
    /// Where the session record says this session runs.
    recorded_target: Option<crate::target::SessionTarget>,
    /// Identity of the process attached here, distinct from a replica's record.
    local_session: Option<String>,
    /// The relay session the record says this session re-attaches to.
    recorded_relay_session: Option<String>,

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

    conventions: ui::PlatformConventions,
    /// Local find-in-scrollback state: the matches the find scope that hosts
    /// this terminal asked for ([`Findable`]), painted over their cells. It
    /// is intentionally not mirrored into the CRDT or resource tree: a find
    /// term and its matching output can be sensitive, while the shared
    /// terminal transcript already has its own carefully bounded
    /// collaboration contract.
    find_matches: Vec<FindMatch>,
    find_current: usize,
    /// The context menu's Find entry was chosen: the hosting find scope
    /// opens its bar ([`Self::take_find_request`]).
    find_request: bool,
    /// Bumped whenever a sync brings fresh output, so an open find bar finds
    /// again and its counter follows ([`Findable::content_revision`]).
    content_revision: u64,
    /// Height of the session banner, as laid out by the last `draw`. The
    /// grid starts under it.
    chrome_top_band: f64,
    /// Where the session banner is painted by the last `draw`; empty while the
    /// session is live.
    banner_rect: Rect,
    /// Whether this widget was made without any process behind it.
    headless: bool,

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
    copy_on_select_menu_index: Option<usize>,
    menu_paste_index: Option<usize>,
    /// What each effect row of the open menu asks for.
    menu_effects: HashMap<usize, crate::resource::TerminalEffect>,
    /// Cursor position cached on every `CursorMoved` so the press arm
    /// can hit-test without receiving a fresh position. Logical coords
    /// (post scale-factor divide), matching `text_input`'s convention.
    last_cursor_pos: Point,
    /// Viewport scrollback offset captured on the most recent `draw`.
    /// Stored so `handle_event` can translate logical pixel coordinates
    /// into viewport-local grid rows (the selection model lives in
    /// viewport space). Defaults to 0 until the first frame paints.
    viewport_start_y: usize,

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
    /// theme on every `draw` so `set_geometry` and pointer hit-testing — neither of which receives a
    /// `Theme` — inset the grid by the exact amounts `draw` painted with.
    /// Seeded to the default token values so the first (pre-draw) layout is
    /// already correct.
    chrome_body_pad: f64,
    chrome_scrollbar_w: f64,
    /// Latches true the first time the mirrored grid holds any non-blank
    /// content. Gates the honest empty-state watermark so it only shows before
    /// the session has ever produced output — never over a legitimately
    /// cleared screen (`clear`, `CSI 2J`) once the shell has run.
    ever_nonblank: bool,

    /// Window title set by OSC 0 / OSC 2, recorded in the session so a
    /// collaborator sees the same title; surfaced through `AppWidget::app_title`. Defaults to "Terminal".
    title: String,
    /// Local fast-lookup mirror of the OSC 8 hyperlink registry (id → URL),
    /// rebuilt from the screen's links whenever it changes. Used to render
    /// link tint/underline and to resolve a click into a URL to open.
    links: HashMap<u16, String>,
    /// Monotonic OSC 8 link-id allocator (host side). `0` is reserved for
    /// "no link"; the first hyperlink gets id 1.
    next_link_id: u16,
    /// The link id currently open (between `OSC 8 ; ; URL` and its close);
    /// every printed cell adopts it so a multi-cell link shares one id.
    /// Survives across `feed` calls so a link spanning PTY reads stays whole.
    current_link_id: u16,
    /// Where graphics bytes are stored and read back (verified) by hash.
    content: ContentStore,
    image_placements: Vec<TerminalImagePlacement>,
    /// Decoded images are deliberately local GPU-side state. The session record
    /// holds only immutable content addresses; each peer validates and decodes bytes
    /// on demand when a placement comes into view.
    image_cache: HashMap<String, ui::widget::Image>,
    graphics_scanner: GraphicsScanner,

    /// Stable per-instance id used to scope this terminal's collaboration
    /// presence (`PresenceKey::App { sub }`), so a peer's caret/selection is
    /// attributed to this pane rather than colliding with another terminal.
    pane_id: String,
    /// The session record's dispatcher: the framework's when it hosts this
    /// widget, a parent's when the terminal is nested in its resource. Without
    /// one the terminal is its process's alone, and nothing is recorded.
    dispatch: Option<ui::framework::Dispatcher<crate::resource::Terminal>>,

    /// Wakes the host when the PTY reader has new output, so an idle terminal
    /// waits for its process instead of polling. Captured from the scope the
    /// widget is built in, and replaced by the framework's when it is attached
    /// ([`SurfaceCx::waker`](ui::framework::SurfaceCx::waker)). Shared with the
    /// reader threads, which outlive a replacement.
    waker: Arc<Mutex<ui::WakeHandle>>,
    /// Buttons currently held while an application has mouse tracking active.
    /// Bits 0/1/2 are left/middle/right. This is local input state, never
    /// replicated with the shared terminal screen.
    mouse_buttons: u8,
    focused: bool,
    /// How this tab reaches its sessions, and where the current one runs.
    sessions: crate::sessions::Sessions,
    /// Height of the header that carries the target chip.
    chrome_header_h: f64,
    /// The target chip, as last drawn.
    chip_rect: Rect,
    /// While the open context menu is the target picker: what each row does.
    target_menu_rows: Option<Vec<Option<TargetMenuRow>>>,
    environments_panel: EnvironmentsPanel,
}

impl TerminalWidget {
    pub(crate) fn native_editor_read_only_changed(&mut self, read_only: bool) {
        if self.native_read_only != read_only {
            self.native_read_only = read_only;
            self.invalidate_deferred_input();
            {
                self.native_capture = None;
                if let Some(editor) = &self.native_editor {
                    editor.revoke();
                }
            }
        }
    }

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
            input_epoch: std::cell::Cell::new(0),
            native_editor: None,
            native_capture: None,
            native_history: None,
            native_ime: None,
            native_ime_error: None,
            native_read_only: false,
            pty_write: writer,
            pty_master: master,
            service_lifetime: None,
            output_rx,
            deferred_output: std::cell::Cell::new(None),
            session_disconnected: Arc::new(AtomicBool::new(false)),
            #[cfg(windows)]
            child_killer: None,
            session_status_message: Arc::new(Mutex::new(SESSION_ENDED_MESSAGE.to_owned())),
            parser: Parser::new(),
            grid,
            grid_attrs,
            cursor_x: 0,
            cursor_y: 0,
            width,
            height,
            // Seed values only — `draw` overwrites char_w/char_h from the
            // theme's `Code` role on the first frame, and `measure` uses the
            // cached `cell_style` (unset until the first layout, so it falls
            // back to these dimensions until then).
            char_w: 9.0,
            char_h: 18.0,
            forced_cell: false,
            cell_style: None,
            palette: ui::tokens::TerminalPalette::dark(),
            screen: crate::screen::Screen::default(),
            mirrored: Vec::new(),
            emulating: false,
            synced: None,
            pending_actions: Vec::new(),
            recorded_target: None,
            local_session: None,
            recorded_relay_session: None,
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
            find_matches: Vec::new(),
            find_current: 0,
            find_request: false,
            content_revision: 0,
            chrome_top_band: 0.0,
            banner_rect: Rect::ZERO,
            headless: false,
            last_cursor_color: Color::TRANSPARENT,
            last_fg_default: Color::TRANSPARENT,
            last_bg_default: Color::TRANSPARENT,
            selection: None,
            drag_select: DragSelect::default(),
            drag_granularity: Granularity::Char,
            drag_anchor_span: None,
            select_to_copy: false,
            context_menu: ContextMenu::new(),
            find_menu_index: None,
            copy_on_select_menu_index: None,
            menu_paste_index: None,
            menu_effects: HashMap::new(),
            last_cursor_pos: Point::ZERO,
            viewport_start_y: 0,
            grid_rect: Rect::ZERO,
            // Seeded to the default token metrics (body pad 12, scrollbar
            // gutter 8) so the first layout — which runs before the first
            // `draw` refreshes these from the live theme — already insets the
            // grid correctly.
            chrome_body_pad: 12.0,
            chrome_scrollbar_w: 8.0,
            ever_nonblank: false,
            title: "Terminal".to_string(),
            links: HashMap::new(),
            next_link_id: 0,
            current_link_id: 0,
            image_placements: Vec::new(),
            image_cache: HashMap::new(),
            content,
            graphics_scanner: GraphicsScanner::default(),
            pane_id: uuid::Uuid::new_v4().to_string(),
            dispatch: None,
            waker: Arc::new(Mutex::new(ui::WakeHandle::new())),
            mouse_buttons: 0,
            focused: false,
            sessions: crate::sessions::Sessions::none(),
            chrome_header_h: 32.0,
            chip_rect: Rect::ZERO,
            target_menu_rows: None,
            environments_panel: EnvironmentsPanel::default(),
        }
    }

    /// A new process takes this terminal. Its screen starts blank, and the
    /// session record drops the previous process's screen, title and pane
    /// before this one's output reaches it: a tab is durable, its process is
    /// not, and a shell started after a logout must not inherit the grid of
    /// the one before it.
    fn begin_local_session(&mut self) {
        let session_id = uuid::Uuid::new_v4().to_string();
        self.local_session = Some(session_id.clone());
        self.pane_id = uuid::Uuid::new_v4().to_string();
        self.submit(crate::resource::TerminalAction::BindLocalSession { session_id });
        self.submit(crate::resource::TerminalAction::ClaimPane {
            pane_id: self.pane_id.clone(),
        });
        self.synced = None;
        self.emulating = true;
        self.screen = crate::screen::Screen::default();
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.saved_cursor = None;
        self.ever_nonblank = false;
        self.title = "Terminal".to_string();
        self.ensure_lines();
        self.show_screen();
    }

    /// Wake the host through `waker` from now on: the PTY reader and the
    /// process monitor use it for every later output and lifecycle change.
    pub(crate) fn wake_through(&self, waker: ui::WakeHandle) {
        if let Ok(mut current) = self.waker.lock() {
            *current = waker;
        }
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

    /// Process exit may precede the reader's final bytes. Observe the actual
    /// reader channel closing only after every queued chunk has been pumped.
    #[cfg(test)]
    pub(crate) fn pty_output_drained(&self) -> bool {
        if let Some(bytes) = self.deferred_output.take() {
            self.deferred_output.set(Some(bytes));
            return false;
        }
        match self.output_rx.try_recv() {
            Ok(bytes) => {
                self.deferred_output.set(Some(bytes));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => true,
        }
    }

    fn mark_disconnected(&self, message: &str) {
        set_terminal_disconnected(
            &self.session_disconnected,
            &self.session_status_message,
            message,
        );
    }

    fn disconnected_message(&self) -> String {
        self.session_status_message.lock().map_or_else(
            |_| SESSION_ENDED_MESSAGE.to_owned(),
            |message| message.clone(),
        )
    }

    fn status_banner_message(&self) -> Option<String> {
        if self.is_disconnected() {
            return Some(self.disconnected_message());
        }
        if let Some(failure) = self.native_ime_error {
            use ui::text_input::Failure;
            let key = match failure {
                Failure::Busy => "terminal-editor-busy",
                Failure::Rejected => "terminal-editor-rejected",
                Failure::Unknown | Failure::Retired => "terminal-editor-unconfirmed",
            };
            return Some(crate::i18n::t(key));
        }
        None
    }

    fn poll_native_ime(&mut self) -> bool {
        if let Some(result) = self
            .native_ime
            .as_ref()
            .and_then(ui::text_input::Receipt::poll)
        {
            self.native_ime = None;
            // A second refused commit is a different input; the first ACK
            // must not erase its visible refusal. Fresh admission clears it.
            self.native_ime_error = result.err().or(self.native_ime_error);
            return true;
        }
        false
    }

    fn commit_ordinary_ime(&mut self, text: &str) {
        self.poll_native_ime();
        if self.native_ime.is_some() || self.native_read_only {
            self.native_ime_error = Some(if self.native_read_only {
                ui::text_input::Failure::Rejected
            } else {
                ui::text_input::Failure::Busy
            });
            return;
        }
        if self.native_read_only {
            return;
        }
        if self.ime_requires_deferred_ack() {
            let receipt = self
                .native_editor
                .as_ref()
                .and_then(|editor| {
                    editor
                        .capture()
                        .map(|(capture, _)| editor.commit(capture, text.into()))
                })
                .unwrap_or_else(|| {
                    ui::text_input::Receipt::failed(ui::text_input::Failure::Rejected)
                });
            self.native_ime_error = None;
            self.native_ime = Some(receipt);
            self.poll_native_ime();
            return;
        }
        self.write_input(text.as_bytes());
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

    /// The visible public links, as live rows of the session: each opens
    /// its link.
    pub(crate) fn link_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.visible_link_ids()
            .into_iter()
            .filter_map(|id| self.link_row_for(id))
            .collect()
    }

    /// The link row `id`, visible or not.
    pub(crate) fn link_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        let prefix = format!("terminal.link:{}:", self.pane_id);
        self.link_row_for(id.strip_prefix(&prefix)?.parse::<u16>().ok()?)
    }

    fn link_row_for(&self, link_id: u16) -> Option<ui::framework::LiveRow> {
        let url = self
            .links
            .get(&link_id)
            .filter(|url| is_public_web_link(url))?;
        Some(
            ui::framework::LiveRow::new(
                self.terminal_link_resource_id(link_id),
                "terminal.link",
                link_resource_label(url),
            )
            .effect(&crate::resource::TerminalEffect::OpenLink { url: url.clone() }),
        )
    }

    /// The effects that differ from their declaration now: the process's
    /// input once it ended, and scrolling past either end of the output.
    pub(crate) fn effect_statuses(&self) -> Vec<ui::framework::EffectStatus> {
        use crate::resource::TerminalEffect as E;
        use ui::framework::EffectStatus;
        let mut statuses = Vec::new();
        if self.inspect_editor().get("available") != Some(&serde_json::Value::Bool(true)) {
            statuses.push(EffectStatus::unavailable(
                E::REPLACE_EDITOR_SELECTION,
                "native_editor_unavailable",
                "No public editable field is available in this session",
            ));
        }
        if let Some((code, reason)) = self.input_unavailable() {
            statuses.extend([
                EffectStatus::unavailable(E::SEND, code, reason),
                EffectStatus::unavailable(E::RUN, code, reason),
                EffectStatus::unavailable(E::INTERRUPT, code, reason),
                EffectStatus::unavailable(E::CLEAR, code, reason),
                EffectStatus::unavailable(E::PASTE, code, reason),
                EffectStatus::unavailable(E::PASTE_PRIMARY, code, reason),
            ]);
        }
        if self.grid.len() <= self.current_visible_rows() {
            statuses.push(EffectStatus::unavailable(
                E::HISTORY_TOP,
                "history_boundary",
                "No older output is retained",
            ));
        }
        if self.scroll_offset == 0 {
            statuses.push(EffectStatus::unavailable(
                E::HISTORY_LIVE,
                "history_boundary",
                "Already showing live output",
            ));
        }
        statuses
    }

    fn input_unavailable(&self) -> Option<(&'static str, &'static str)> {
        if self.sessions.is_live()
            && !matches!(self.sessions.state, crate::sessions::OpenState::Open)
        {
            return Some((
                "session_not_open",
                "The terminal process has not opened; input cannot be sent.",
            ));
        }
        self.is_disconnected().then_some((
            "session_disconnected",
            "The terminal process has ended or its PTY failed; input cannot be sent.",
        ))
    }

    pub(crate) fn inspect_session(&self) -> serde_json::Value {
        use crate::sessions::OpenState;
        let (lifecycle, startup_reason) = match &self.sessions.state {
            OpenState::Fixed => ("fixed", None),
            OpenState::Unopened => ("unopened", None),
            OpenState::Opening => ("opening", None),
            OpenState::Open => ("open", None),
            OpenState::AwaitingAllow => ("awaiting_allow", None),
            OpenState::Failed(reason) => ("failed", Some(reason.as_str())),
            OpenState::NoRuntime(reason) => ("no_runtime", Some(reason.as_str())),
        };
        serde_json::json!({
            "lifecycle": lifecycle,
            "target": self.sessions.target,
            "input_available": self.input_unavailable().is_none(),
            "transport_disconnected": self.is_disconnected(),
            "transport_reason": self.is_disconnected().then(|| self.disconnected_message()),
            "startup_reason": startup_reason,
            "session": self.local_session,
            "known_environments": self.sessions.environments.iter().map(|environment| {
                serde_json::json!({ "record": environment.key, "name": environment.name,
                    "tools": environment.tools, "grants": environment.grants })
            }).collect::<Vec<_>>(),
            "host_access_requirement": "requires_person_owner_grant",
        })
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

    fn target_picker_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Button)
                .with_name(self.chip_label()?)
                .with_action_target(format!("terminal.target:{}", self.pane_id))
                .with_bounds(self.chip_rect)
                .with_state(AccessState {
                    focusable: true,
                    expanded: Some(self.target_menu_rows.is_some()),
                    ..Default::default()
                }),
        )
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

    /// Whether the context menu's Find entry was chosen since the last call.
    /// The find scope that hosts this terminal opens its bar; the terminal
    /// itself holds no bar. Nothing here publishes a query, a match or a
    /// command to collaborators or agents: scrollback may contain
    /// credentials, and find is only a viewport aid.
    pub fn take_find_request(&mut self) -> bool {
        std::mem::take(&mut self.find_request)
    }

    /// Match `query` against the retained scrollback. Matches are literal;
    /// the bar's options choose whether case and whole words count.
    fn scan_find_matches(&mut self, query: &str, options: FindOptions) {
        self.find_matches.clear();
        let query: Vec<char> = query.chars().take(MAX_FIND_QUERY_CHARS).collect();
        if query.is_empty() {
            return;
        }
        let same = |a: char, b: char| {
            a == b || (!options.match_case && a.to_lowercase().eq(b.to_lowercase()))
        };
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let query_width: usize = query.iter().map(|c| c.width().unwrap_or(1)).sum();
        'rows: for (row, line) in self.grid.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            // Cell column of each character, so wide glyphs line up.
            let mut columns = Vec::with_capacity(chars.len());
            let mut column = 0usize;
            for c in &chars {
                columns.push(column);
                column += c.width().unwrap_or(1);
            }
            let mut at = 0usize;
            while at + query.len() <= chars.len() {
                let hit = chars
                    .get(at..at + query.len())
                    .is_some_and(|window| window.iter().zip(&query).all(|(a, b)| same(*a, *b)));
                let bounded = !options.whole_word
                    || ((at == 0 || chars.get(at - 1).is_none_or(|c| !word(*c)))
                        && chars.get(at + query.len()).is_none_or(|c| !word(*c)));
                if hit && bounded {
                    let start_col = columns.get(at).copied().unwrap_or(0);
                    self.find_matches.push(FindMatch {
                        row,
                        start_col,
                        end_col: start_col + query_width,
                    });
                    if self.find_matches.len() == MAX_FIND_MATCHES {
                        break 'rows;
                    }
                    at += query.len();
                } else {
                    at += 1;
                }
            }
        }
        self.find_current = self
            .find_current
            .min(self.find_matches.len().saturating_sub(1));
    }

    fn reveal_current_find_match(&mut self) {
        let Some(found) = self.find_matches.get(self.find_current).copied() else {
            return;
        };
        if self.alt_active {
            return;
        }
        let visible = self.current_visible_rows().max(1);
        let max_top = self.view_rows().saturating_sub(visible);
        let top = found.row.saturating_sub(visible / 2).min(max_top);
        self.scroll_offset = max_top.saturating_sub(top);
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
    /// The open menu's row labels (empty for separators). Test accessor.
    #[cfg(test)]
    pub(crate) fn context_menu_labels(&self) -> Vec<String> {
        self.context_menu
            .items()
            .iter()
            .map(|item| item.label().unwrap_or_default().to_owned())
            .collect()
    }

    /// Whether menu row `index` can be chosen. Test accessor.
    #[cfg(test)]
    pub(crate) fn context_menu_item_enabled(&self, index: usize) -> bool {
        self.context_menu
            .items()
            .get(index)
            .is_some_and(MenuItem::is_enabled)
    }

    /// Choose menu row `index`, as a click on it does. Test accessor.
    #[cfg(test)]
    pub(crate) fn activate_menu_row_for_test(&mut self, index: usize) {
        self.activate_context_menu_item(index);
    }

    /// End the process's input, as a failed write does. Test accessor.
    #[cfg(test)]
    pub(crate) fn mark_disconnected_for_test(&self) {
        self.mark_disconnected("Session ended — input unavailable");
    }

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
    /// Replace `grid` from a test. Bypasses the PTY and the emulator so
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
        let max_top = self.view_rows().saturating_sub(vis);
        let scroll = if self.alt_active {
            0
        } else {
            self.scroll_offset
        };
        let start = max_top.saturating_sub(scroll);
        self.grid.iter().skip(start).take(vis).cloned().collect()
    }
    /// The rows of the painted find matches, for regressions.
    #[cfg(test)]
    pub fn find_test_matches(&self) -> Vec<usize> {
        self.find_matches.iter().map(|found| found.row).collect()
    }
    /// The painted session banner after the real draw pass.
    #[cfg(test)]
    pub fn banner_test_rect(&self) -> Rect {
        self.banner_rect
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
        self.chrome_header_h =
            ui::widget::ButtonFace::height(ui::widget::ControlSize::Small, theme)
                + f64::from(sp.s2);
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
    /// decoration, so the grid starts under the find and session bands (when
    /// they are showing) and the top padding. Uses the cached body
    /// metrics so it is available without a `Theme`. This is the *single*
    /// definition of the grid geometry, shared by `set_geometry` and `draw`.
    fn grid_content_rect(&self, rect: Rect) -> Rect {
        let pad = self.chrome_body_pad;
        let x0 = rect.x0 + pad;
        let y0 = rect.y0 + self.chrome_top_band + pad;
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
        self.view_rows().saturating_sub(self.current_visible_rows())
    }

    fn scrollbar_track(&self) -> Rect {
        let x1 = self.rect.x1 - self.chrome_body_pad;
        let width = (self.chrome_scrollbar_w - 2.0).max(3.0);
        Rect::new(x1 - width, self.grid_rect.y0, x1, self.grid_rect.y1)
    }

    fn scrollbar_metrics(&self) -> ScrollMetrics {
        let visible = self.current_visible_rows() as f64;
        let total = self.view_rows() as f64;
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

    /// Drain queued PTY bytes and replay them through the vte parser onto the
    /// screen. Shared by both `Widget::update` (for hosts that pump it, e.g.
    /// the compositor's tile host) and `Widget::sync`. Returns whether any
    /// bytes were processed this call.
    fn pump(&mut self) -> bool {
        let open_activity = self.poll_session_open();
        let native_activity = self.poll_native_ime() || open_activity;
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
            self.feed(&batch);
            // `feed` normally latches this itself, but keep the PTY boundary
            // authoritative: even if a future parser early-return prevents
            // visible cells from being mirrored in this frame, bytes were
            // received from a live session and the loading watermark must
            // never be painted over them.
            self.ever_nonblank = true;
            // Fresh output snaps the viewport back to the live bottom.
            self.scroll_offset = 0;
        }
        activity || native_activity
    }

    /// The screen this terminal shows. Test accessor.
    #[cfg(test)]
    pub(crate) fn screen(&self) -> &crate::screen::Screen {
        &self.screen
    }

    #[cfg(test)]
    pub(crate) fn has_received_pty_output(&self) -> bool {
        self.ever_nonblank
    }

    /// Run `bytes` through the vte parser + performer, mutating the screen.
    /// This is the single production entry point for terminal output
    /// processing — the PTY pump feeds real shell output through it, and
    /// tests and screenshot scenarios feed canned VT sequences through the
    /// very same path. From here on the terminal shows its own emulator's
    /// screen; the session record follows it at the next sync.
    pub fn feed(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            // A native edit's own redisplay must preserve its acknowledged
            // checkpoint. The producer fences actual Readline state changes.
            self.invalidate_deferred_input();
        }
        // Output with no process attached (a scenario, a test) starts the
        // screen's own session, as an attached process does.
        if !self.emulating {
            self.begin_local_session();
        }
        self.ensure_lines();

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

        let mut title_out: Option<String> = None;
        // Reply bytes for any capability query in this batch (DA1/DA2, DSR,
        // XTVERSION, DECRQM). Written back to the PTY after the parse — the
        // vte performer only reads, so it stages answers here.
        let mut responses: Vec<u8> = Vec::new();
        let mut performer = TerminalPerformer {
            screen: &mut self.screen,
            content: &self.content,
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
        // Record the cursor once per feed rather than on every printed cell.
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

        // Record an OSC 0/2 title change so a collaborator sees it, and
        // reflect it locally for `app_title`.
        if let Some(t) = title_out
            && t != self.title
        {
            self.submit(crate::resource::TerminalAction::SetTitle { title: t.clone() });
            self.title = t;
        }
        self.show_screen();
    }

    /// Place the terminal at an explicit `rect` and reflow the grid + PTY to
    /// match. This is the embedding entry point: a host widget (e.g. the code
    /// editor's terminal panel) that owns the terminal outside a Taffy leaf
    /// calls this instead of `apply_layout` to give it a region to render in.
    pub fn place_at(&mut self, rect: Rect) {
        self.set_geometry(rect);
    }

    fn invalidate_deferred_input(&self) {
        self.input_epoch
            .set(self.input_epoch.get().saturating_add(1));
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
        if let Some(("session_not_open", reason)) = self.input_unavailable() {
            return Err(reason.to_owned());
        }
        if self.is_disconnected() {
            return Err("terminal session is disconnected; input was not sent".to_string());
        }
        let Ok(mut writer) = self.pty_write.lock() else {
            self.mark_disconnected(
                "Session ended. The terminal can no longer send input to the shell.",
            );
            return Err("terminal input is unavailable because its PTY writer stopped".to_string());
        };
        if !bytes.is_empty() {
            if let Some(editor) = &self.native_editor {
                editor.revoke();
            }
            self.invalidate_deferred_input();
        }
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
    /// grid and resize the PTY so the shell sees the new size.
    fn set_geometry(&mut self, rect: Rect) {
        self.rect = rect;
        // The grid lives in the content rect (below the header, inside the
        // padding), so it — not the outer widget rect — drives the cell count
        // and the PTY size.
        self.grid_rect = self.grid_content_rect(rect);
        let grid_rect = self.grid_rect;

        let new_width = (grid_rect.width() / self.char_w.max(1.0)).floor() as usize;
        let new_height = (grid_rect.height() / self.char_h.max(1.0)).floor() as usize;

        // Cap the grid at a sensible terminal size: every cell of the screen
        // is recorded in the session's document, one text op per character,
        // and a full-screen embed (186 × 219 ≈ 40 700 chars) made that record
        // freeze the process while the document compacted. The visible region
        // can be larger than the grid — overflow rows render empty.
        let new_width = new_width.min(200);
        let new_height = new_height.min(80);
        if new_width > 0 && new_height > 0 && (new_width != self.width || new_height != self.height)
        {
            let lines = self.screen.main.len();
            let history_offset = lines.saturating_sub(self.height);
            let absolute_y = history_offset + self.cursor_y;

            let new_history_offset = lines.saturating_sub(new_height);
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

    /// Grow both buffers to a screen's height. Blank lines stay empty:
    /// drawing pads them, and `print` grows a line when the cursor lands past
    /// its end.
    fn ensure_lines(&mut self) {
        for alternate in [false, true] {
            let lines = self.screen.lines_mut(alternate);
            while lines.len() < self.height {
                lines.push(std::rc::Rc::default());
            }
        }
    }
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
    /// Do `effect`, invoked by the palette, an agent or a script through the
    /// host, or by the person's own gesture.
    ///
    fn agent_host_input_refused(&self, effect: &crate::resource::TerminalEffect) -> bool {
        use crate::resource::TerminalEffect as E;
        self.sessions
            .target
            .as_ref()
            .is_some_and(crate::target::SessionTarget::is_host)
            && Requester::current() == Requester::Agent
            && matches!(
                effect,
                E::Send { .. }
                    | E::ReplaceEditorSelection { .. }
                    | E::Run { .. }
                    | E::Paste { .. }
                    | E::PastePrimary { .. }
                    | E::Interrupt
                    | E::Clear
            )
    }

    pub(crate) fn inspect_editor(&self) -> serde_json::Value {
        if let Some(snapshot) = self
            .native_editor
            .as_ref()
            .and_then(crate::native_editor::Client::snapshot)
            && self.focused
            && !self.context_menu.is_open()
            && !self.native_read_only
            && !self.alt_active
            && self.input_unavailable().is_none()
        {
            return serde_json::json!({ "available": true, "session": self.local_session, "protocol": runtime_portal::editor::VERSION, "lease": serde_json::to_string(&snapshot.lease).ok(), "can_undo": snapshot.can_undo, "can_redo": snapshot.can_redo });
        }
        serde_json::json!({ "available": false })
    }

    pub(crate) fn replace_editor_selection(
        &mut self,
        session: &str,
        lease: &str,
        text: String,
        reply: ui::framework::EffectReply,
    ) -> Result<(), String> {
        {
            if self.native_read_only
                || (self
                    .sessions
                    .target
                    .as_ref()
                    .is_some_and(crate::target::SessionTarget::is_host)
                    && Requester::current() == Requester::Agent)
                || !self.focused
                || self.context_menu.is_open()
                || self.alt_active
                || self.input_unavailable().is_some()
                || self.local_session.as_deref() != Some(session)
                || lease.len() > 512
                || text.len() > runtime_portal::editor::TEXT_BYTES
            {
                return Err("The original local editor is unavailable".into());
            }
            let lease = serde_json::from_str(lease).map_err(|_| "Invalid original editor lease")?;
            let editor = self
                .native_editor
                .as_ref()
                .ok_or("No qualified native editor is available")?;
            if editor.snapshot().is_none() {
                return Err("No public native editor is available".into());
            }
            self.native_capture = None;
            editor.effect(
                runtime_portal::editor::Request::Replace {
                    lease,
                    transaction: 1,
                    text,
                },
                runtime_portal::editor::Status::Applied,
                reply,
            );
            Ok(())
        }
    }

    /// # Errors
    /// Returns why the effect was refused or failed.
    pub(crate) fn perform(
        &mut self,
        effect: crate::resource::TerminalEffect,
    ) -> Result<(), String> {
        use crate::resource::TerminalEffect as E;
        if self.agent_host_input_refused(&effect) {
            return Err("agents may not send input to the device's own shell".into());
        }
        match effect {
            E::InspectSession => {
                return Err(
                    "Inspect session is answered by the resource's read-only handler".into(),
                );
            }
            E::InspectEditor | E::ReplaceEditorSelection { .. } => {
                return Err(
                    "Native editor operations require their acknowledged resource handler".into(),
                );
            }
            E::Send { text } => {
                if text.is_empty() {
                    return Err("text to send cannot be empty".to_string());
                }
                if text.len() > MAX_RESOURCE_SEND_BYTES {
                    return Err(format!(
                        "text to send exceeds the {} KiB reviewed-command limit",
                        MAX_RESOURCE_SEND_BYTES / 1024
                    ));
                }
                if !is_reviewable_terminal_text(&text) {
                    return Err(
                        "text to send may not contain terminal control characters".to_string()
                    );
                }
                self.try_write_input(text.as_bytes())?;
            }
            E::Run { command } => {
                if command.trim().is_empty() {
                    return Err("command to run cannot be empty".to_string());
                }
                if command.len() > MAX_RESOURCE_SEND_BYTES {
                    return Err(format!(
                        "command exceeds the {} KiB reviewed-command limit",
                        MAX_RESOURCE_SEND_BYTES / 1024
                    ));
                }
                if !is_reviewable_terminal_text(&command) {
                    return Err(
                        "command must be one line and may not contain terminal control characters"
                            .to_string(),
                    );
                }
                let mut command = command.into_bytes();
                command.push(b'\r');
                self.try_write_input(&command)?;
            }
            E::Paste { text } | E::PastePrimary { text } => self.try_paste(&text)?,
            E::Interrupt => self.try_write_input(&[0x03])?,
            E::Clear => self.try_write_input(&[0x0c])?,
            E::CopySelection => {
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
            E::SelectAll => self.select_all_output(),
            E::CopyVisible => {
                let visible = self.current_visible_text();
                if visible.trim().is_empty() {
                    return Err("there is no visible terminal output to copy".to_string());
                }
                if !ui::text_selection::try_clipboard_copy(&visible) {
                    return Err("the system clipboard did not accept terminal output".into());
                }
            }
            E::HistoryTop => self.scroll_to_top(),
            E::HistoryLive => self.scroll_to_bottom(),
            E::OpenLink { url } => {
                if safe_osc_text(url.as_bytes(), MAX_OSC_LINK_BYTES).is_none()
                    || !is_public_web_link(&url)
                {
                    return Err("link target is not a safe public web URL".to_string());
                }
                open_url(&url);
            }
            E::OpenIn { target } => self.open_in(&target)?,
            E::CreateEnvironment { name } => {
                self.sessions.create_environment(&name)?;
            }
            E::GrantFolder { record, path } => self.sessions.grant_folder(&record, &path, true)?,
            E::RenameEnvironment { record, name } => {
                self.sessions.rename_environment(&record, &name)?;
            }
        }
        Ok(())
    }

    /// Execute only an authenticated owner operation on this live device-shell session.
    pub(crate) fn device_shell_operation(
        &mut self,
        operation: &ui::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        use ui::floating_surface::DeviceShellOperation as O;
        if matches!(operation, O::Select) {
            let already_selected = self
                .sessions
                .target
                .as_ref()
                .is_some_and(crate::target::SessionTarget::is_host)
                && matches!(
                    self.sessions.state,
                    crate::sessions::OpenState::Open
                        | crate::sessions::OpenState::Opening
                        | crate::sessions::OpenState::AwaitingAllow
                );
            if !(already_selected || self.request_device_shell()) {
                return Err("The device shell could not open".into());
            }
            return Ok(self.inspect_session());
        }
        if matches!(operation, O::Inspect) {
            return Ok(self.inspect_session());
        }
        let (O::Run { session, .. } | O::Read { session }) = operation else {
            return Err("Invalid device shell operation".into());
        };
        if self.local_session.as_deref() != Some(session.as_str())
            || !self
                .sessions
                .target
                .as_ref()
                .is_some_and(crate::target::SessionTarget::is_host)
        {
            return Err("This device shell session has changed".into());
        }
        if matches!(operation, O::Run { .. }) && self.input_unavailable().is_some() {
            return Err("The device shell is not open".into());
        }
        match operation {
            O::Run { command, .. } => {
                if command.trim().is_empty()
                    || command.len() > MAX_RESOURCE_SEND_BYTES
                    || !is_reviewable_terminal_text(command)
                {
                    return Err(
                        "Provide one bounded command without terminal control characters".into(),
                    );
                }
                let mut bytes = command.as_bytes().to_vec();
                bytes.push(b'\r');
                self.try_write_input(&bytes)?;
                Ok(serde_json::json!({"session":session,"status":"submitted"}))
            }
            O::Read { .. } => {
                let text = self.current_visible_text();
                let mut end = text.len().min(32768);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                let visible = text
                    .get(..end)
                    .ok_or("Terminal output boundary is unavailable")?;
                Ok(
                    serde_json::json!({"session":session,"text":visible,"truncated":end < text.len()}),
                )
            }
            _ => Err("Invalid device shell operation".into()),
        }
    }

    /// Ask for `effect` as the person's own gesture: through the session's
    /// host, which checks their role and asks for confirmation as it would
    /// for any caller, or — for a terminal that has no session record —
    /// directly.
    fn submit_effect(&mut self, effect: crate::resource::TerminalEffect) {
        match &self.dispatch {
            Some(dispatch) => dispatch.effect(&effect),
            None => {
                if let Err(error) = self.perform(effect) {
                    tracing::warn!(%error, "terminal gesture failed");
                }
            }
        }
    }
}

impl TerminalWidget {
    /// Open the session, drain the PTY and mirror the document's screen.
    fn sync_document(&mut self, _state: &AppModel, _root: &ModelPath) {
        if self.sessions.is_live() {
            self.ensure_session();
        }
        self.poll_folder_picker();
        // Drain any bytes the PTY reader thread has queued and replay them
        // through the vte parser *here*. A host that never calls `update`
        // still advances the terminal: pumping from `sync`, driven by the
        // waker (or an embedding host's own pump), is what shows output.
        self.pump();
    }

    /// Bring the session record up to date with this terminal: the session
    /// changes waiting since the last sync, in order, then what the output
    /// parsed since did to the screen, as one action. Nothing is recorded
    /// for a terminal that has no record.
    fn record(&mut self, edits: &mut ui::widget::Edits<'_>) {
        let Some(dispatch) = self.dispatch.clone() else {
            self.pending_actions.clear();
            return;
        };
        for action in std::mem::take(&mut self.pending_actions) {
            if dispatch.apply_now(edits, &action).is_err() {
                self.synced = None;
            }
        }
        if !self.emulating {
            return;
        }
        let screen_edits = crate::screen::edits(self.synced.as_ref(), &self.screen);
        if screen_edits.is_empty() {
            return;
        }
        let output = crate::resource::TerminalAction::Output {
            edits: screen_edits,
        };
        // A refused write leaves the record's screen unknown: the next one
        // replaces it whole.
        self.synced = dispatch
            .apply_now(edits, &output)
            .is_ok()
            .then(|| self.screen.clone());
    }

    /// Queue a session change for the next sync.
    fn submit(&mut self, action: crate::resource::TerminalAction) {
        if self.dispatch.is_some() {
            self.pending_actions.push(action);
        }
    }

    /// Show the session as its record holds it, unless this terminal's own
    /// emulator draws the screen: a past version, a collaborator's view of
    /// a session running elsewhere, a screenshot before its output.
    pub(crate) fn present_session(&mut self, state: &crate::resource::SessionState) {
        self.recorded_target.clone_from(&state.target);
        self.recorded_relay_session.clone_from(&state.relay_session);
        if !state.pane_id.is_empty() {
            self.pane_id.clone_from(&state.pane_id);
        }
        if self.emulating {
            return;
        }
        self.title.clone_from(&state.title);
        self.screen = state.screen.clone();
        self.show_screen();
    }

    /// Rebuild what is drawn from `screen`: the rows of the active buffer,
    /// the cursor, the hyperlinks and the image placements. Only rows whose
    /// line changed are converted again.
    fn show_screen(&mut self) {
        use std::collections::hash_map::Entry;
        let lines = self.screen.active().clone();
        let unchanged = lines.len() == self.mirrored.len()
            && lines
                .iter()
                .zip(&self.mirrored)
                .all(|(line, shown)| std::rc::Rc::ptr_eq(line, shown));
        if !unchanged {
            let mut previous: HashMap<*const crate::screen::Line, (String, Vec<Attr>)> =
                HashMap::with_capacity(self.mirrored.len());
            let rows = std::mem::take(&mut self.grid)
                .into_iter()
                .zip(std::mem::take(&mut self.grid_attrs));
            for (line, row) in self.mirrored.iter().zip(rows) {
                if let Entry::Vacant(slot) = previous.entry(std::rc::Rc::as_ptr(line)) {
                    slot.insert(row);
                }
            }
            for line in &lines {
                let (text, attrs) =
                    previous
                        .remove(&std::rc::Rc::as_ptr(line))
                        .unwrap_or_else(|| {
                            (
                                line.text.clone(),
                                line.attrs
                                    .chunks_exact(ATTR_BYTES)
                                    .map(Attr::unpack)
                                    .collect(),
                            )
                        });
                self.grid.push(text);
                self.grid_attrs.push(attrs);
            }
            self.mirrored = lines;
        }
        self.alt_active = self.screen.alternate;
        if !self.emulating {
            let (x, y) = self.screen.cursor;
            let history_offset = self.grid.len().saturating_sub(self.height);
            self.cursor_x = usize::try_from(x).unwrap_or(0);
            self.cursor_y = usize::try_from(y)
                .unwrap_or(0)
                .saturating_sub(history_offset)
                .min(self.height.saturating_sub(1));
        }
        self.links = self
            .screen
            .links
            .iter()
            .map(|(id, url)| (*id, url.clone()))
            .collect();
        self.next_link_id = self
            .next_link_id
            .max(self.links.keys().copied().max().unwrap_or(0));
        self.image_placements.clone_from(&self.screen.images);
        if self.grid.iter().any(|row| !row.trim().is_empty()) {
            self.ever_nonblank = true;
        }
        self.content_revision = self.content_revision.wrapping_add(1);
    }

    /// Stage screenshot scenario `name`: the output its process printed,
    /// parsed as PTY output, and the view state shown over it.
    pub(crate) fn stage_scenario(&mut self, name: &str) {
        if let Some((target, environments)) = crate::scenarios::target(name) {
            self.show_target(target, environments);
            self.set_relays(crate::scenarios::relays(name));
        }
        self.feed(&crate::scenarios::output(name));
        if name == "ended" {
            self.mark_disconnected("Session ended — input unavailable");
        }
        self.stage_view(name);
    }

    /// The view state scenario `name` shows over its output.
    fn stage_view(&mut self, name: &str) {
        match name {
            "scrollback" => self.scroll_up(20),
            "find" => self.find_request = true,
            "targets" => self.open_target_menu(),
            "environments" => {
                self.open_environments_panel();
                self.set_environment_name_input("Writing");
            }
            "empty-menu" => self.open_context_menu_at(Point::new(24.0, 24.0)),
            // A whole line of the session's output selected, as a
            // triple-click selects it: the printed line, not the command
            // that echoed it.
            "shell-select" => {
                if let Some(row) = self.find_row_last("primes below 20:") {
                    let (cols, _) = self.grid_size();
                    self.set_selection_cells(GridPos { row, col: 0 }, GridPos { row, col: cols });
                }
            }
            _ => {}
        }
    }
}

/// Find in the retained scrollback. The matches live in the widget, which
/// paints them over the matched cells; the bar counts and steps through them.
impl Findable for TerminalWidget {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<ui::widget::FindMatch> {
        self.find_current = 0;
        self.scan_find_matches(query, options);
        // A terminal's content is addressed by row and column; the bar only
        // counts, so each match is reported as the row it is on.
        self.find_matches
            .iter()
            .map(|found| ui::widget::FindMatch::new(found.row, found.row))
            .collect()
    }

    fn reveal(&mut self, index: usize) {
        self.find_current = index;
        self.reveal_current_find_match();
    }

    fn clear_find(&mut self) {
        self.find_matches.clear();
        self.find_current = 0;
    }

    fn content_revision(&self) -> u64 {
        self.content_revision
    }
}

impl Widget for TerminalWidget {
    ui::provides!(
        dyn Findable,
        dyn ui::AppWidget,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::undo::LocalUndo
    );

    fn test_id(&self) -> Option<&str> {
        Some("terminal")
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        // `measure` does not receive a theme; read the style cached by the
        // most recent `build_layout` or `draw`. Before the first layout the
        // cache is empty and the constructor-seeded char_w/char_h keep layout
        // yielding a usable grid.
        if self.forced_cell {
            return taffy::Size::ZERO;
        }
        if let Some(style) = &self.cell_style {
            let (w, h) = text_renderer.measure_style(" ", None, style);
            if w > 0.0 && h > 0.0 {
                self.char_w = f64::from(w).max(1.0);
                self.char_h = Self::cell_h(h);
            }
        }
        taffy::Size::ZERO
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        self.sync_document(state, root);
    }

    fn update(&mut self, _state: &AppModel, _root: &ModelPath) -> bool {
        self.pump()
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, _root: &automerge::ObjId) {
        self.record(edits);
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
        queued || self.folder_picker_pending()
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
        self.cell_style = Some(theme.text(TextRole::Code));
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
        self.set_geometry(rect);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Terminal painting intentionally keeps grid, selection, cursor, and the find and session bands in one ordered compositing pass"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        // Every visual parameter comes from the theme. The grid is document
        // content in the `Code` role; the window ground under it is the
        // framework's, so nothing here fills the widget.
        let cell_style = theme.text(TextRole::Code);
        self.palette = Self::grid_palette(theme);
        // Refresh the body geometry from the theme and re-derive the inner grid
        // rect (inside the padding, minus the scrollbar gutter) — the same
        // `grid_content_rect` `set_geometry` and pointer hit-testing use, so the
        // grid origin is single-sourced. `set_geometry` and hit-testing read the
        // cached metrics — neither sees a `Theme`.
        self.refresh_chrome_metrics(theme);

        // Measure the cell advance of the exact style the glyphs are drawn in
        // so `char_w` equals the width every glyph is drawn at; if the two
        // disagreed, each colour run would be positioned on a grid finer than
        // its glyphs and the row would smear over itself. `measure` reads the
        // cached style for the next layout.
        let (w, h) = text_renderer.measure_style(" ", None, &cell_style);
        self.char_w = f64::from(w).max(1.0);
        self.char_h = Self::cell_h(h);
        self.cell_style = Some(cell_style.clone());

        // The find bar and the session banner stand above the grid; lay them
        // out first so the grid and the PTY size are derived under them.
        self.layout_top_bands(theme, text_renderer);

        // `measure` happens before we can resolve the theme's actual
        // monospace face. Reconcile the PTY geometry after that exact
        // measurement as well: at 200% text scale a fallback/default advance
        // can be much narrower than the rendered mono advance, otherwise the
        // process sees more columns than fit on screen and its right side is
        // silently clipped. This is a no-op on steady frames.
        self.set_geometry(self.rect);
        let grid_rect = self.grid_rect;

        let visible_rows = (grid_rect.height() / self.char_h).floor() as usize;
        let max_top = self.view_rows().saturating_sub(visible_rows);
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
        let cursor_color = cursor_from_palette(&self.palette);
        self.last_cursor_color = cursor_color;
        self.last_fg_default = self.palette.fg_default;
        self.last_bg_default = self.palette.bg_default;

        // Latched via `ever_nonblank` so the empty state never covers a
        // legitimately cleared screen (`clear`, `CSI 2J`) once the shell has
        // printed anything.
        if !self.grid_is_blank() {
            self.ever_nonblank = true;
        }
        self.draw_target_header(scene, transform, text_renderer, theme);
        // With no shell behind the grid (or one that has already ended) and
        // nothing ever printed, say so instead of showing a blank void. A live
        // shell that has not drawn its prompt yet shows its blank grid.
        if !self.alt_active && !self.ever_nonblank && (self.headless || self.is_disconnected()) {
            self.draw_empty_state(scene, transform, text_renderer, theme);
            self.draw_top_bands(scene, transform, text_renderer, theme);
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
        self.draw_selection_highlight(scene, transform, theme, grid_rect, start_y, visible_rows);
        self.draw_find_match_highlight(scene, transform, theme, grid_rect, start_y, visible_rows);

        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.draw_cell_rows(&mut ctx, &cell_style, start_y, visible_rows);
        self.draw_block_cursor(&mut ctx, &cell_style, start_y, visible_rows);

        // A themed scrollbar indicator in the reserved right gutter, shown only
        // when the buffer overflows the viewport.
        self.draw_scrollbar(scene, transform, theme, start_y, visible_rows);
        self.draw_top_bands(scene, transform, text_renderer, theme);
    }

    /// The right-click Copy/Paste menu is a floating Panel: it paints in the
    /// top layer, over the grid and its bands, and on the empty first-run
    /// surface as well.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.context_menu.is_open() {
            Widget::draw_overlay(
                &mut self.context_menu,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
        self.draw_environments_panel(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
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
            if let Some(index) = self.context_menu.take_selection()
                && !self.activate_target_menu_item(index)
            {
                self.activate_context_menu_item(index);
            }
            self.forget_closed_target_menu();
            *ctx.handled |= menu_handled;
            return;
        }

        if self.route_target_chrome(event, ctx) {
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
            // Preedit stays with the platform. A qualified public editor uses
            // the same literal, acknowledged replacement as dictation. Other
            // ordinary IME input retains the terminal's normal PTY semantics.
            WindowEvent::Ime(winit::event::Ime::Commit(text))
                if self.focused && !text.is_empty() =>
            {
                self.commit_ordinary_ime(text);
                self.scroll_offset = 0;
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Terminal"
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
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
        self.context_menu.set_platform_conventions(conventions);
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.invalidate_deferred_input();
            {
                self.native_capture = None;
                if let Some(editor) = &self.native_editor {
                    editor.revoke();
                }
            }
        }
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

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if self.focused
            && !self.context_menu.is_open()
            && !self.native_read_only
            && !self.alt_active
            && self.input_unavailable().is_none()
            && self
                .native_editor
                .as_ref()
                .and_then(crate::native_editor::Client::snapshot)
                .is_some()
        {
            return Some(winit::window::ImePurpose::Normal);
        }
        (self.focused && !self.context_menu.is_open() && self.input_unavailable().is_none())
            .then_some(winit::window::ImePurpose::Terminal)
    }

    fn ime_requires_deferred_ack(&self) -> bool {
        self.ime_purpose() == Some(winit::window::ImePurpose::Normal)
    }

    fn start_deferred_text(&mut self) -> Option<ui::text_input::Receipt> {
        {
            if !self.ime_requires_deferred_ack() {
                return None;
            }
            let (capture, receipt) = self.native_editor.as_ref()?.capture()?;
            self.native_capture = Some(capture);
            Some(receipt)
        }
    }

    fn commit_deferred_text(&mut self, text: &str) -> Option<ui::text_input::Receipt> {
        {
            use ui::text_input::{Failure, Receipt};
            let editor = self.native_editor.as_ref()?;
            let Some(capture) = self.native_capture.take() else {
                return Some(Receipt::failed(Failure::Rejected));
            };
            if self.native_read_only
                || !self.focused
                || self.context_menu.is_open()
                || self.alt_active
                || self.input_unavailable().is_some()
            {
                return Some(Receipt::failed(Failure::Rejected));
            }
            Some(editor.commit(capture, text.into()))
        }
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_purpose()?;
        if let Some(snapshot) = self
            .native_editor
            .as_ref()
            .and_then(crate::native_editor::Client::snapshot)
        {
            let lease = snapshot.lease;
            return Some(ui::ime::edit_fingerprint(
                &format!(
                    "{:?}:{}:{}",
                    lease.incarnation, lease.generation, lease.revision
                ),
                lease.anchor as usize,
                lease.focus as usize,
                key,
            ));
        }
        // The transcript is read-only; the live process input is not. Bind
        // deferred input to its screen/position without copying output or
        // exposing shell contents to the recognizer.
        if self.input_epoch.get() == u64::MAX {
            return None;
        }
        Some(ui::ime::edit_fingerprint(
            &format!("{}:{}", self.alt_active, self.input_epoch.get()),
            self.cursor_x,
            self.cursor_y,
            key,
        ))
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
            description.push_str(&self.disconnected_message());
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
        if let Some(picker) = self.target_picker_accessibility() {
            transcript.children.push(picker);
        }
        transcript
            .children
            .extend(self.visible_link_accessibility());
        if let Some(menu) = self.context_menu_accessibility() {
            transcript.children.push(menu);
        }
        if let Some(message) = self.status_banner_message() {
            transcript.children.push(
                AccessNode::new(Role::StaticText)
                    .with_hashed_key("terminal-lifecycle")
                    .with_name(message)
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
        _key_path: &[u64],
        _state: &mut ui::widget::Edits<'_>,
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
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if target == self.terminal_transcript_target() {
            return self.perform_action(action, &[], state, path);
        }
        if target == format!("terminal.target:{}", self.pane_id)
            && matches!(
                action,
                ui::a11y::Action::Activate | ui::a11y::Action::Expand
            )
        {
            self.open_target_menu();
            return true;
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
            if !self.activate_target_menu_item(index) {
                self.activate_context_menu_item(index);
            }
            self.forget_closed_target_menu();
            true
        } else {
            false
        }
    }

    /// Paint the terminal, then overlay remote collaborators' selections and
    /// carets in their own colours on top (following the `TextInput` model).
    /// Grid content is shared via the session record; this adds the per-peer
    /// decoration.
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

impl ui::undo::LocalUndo for TerminalWidget {
    fn local_undo_is_deferred(&self) -> bool {
        self.native_editor.is_some()
    }
    fn local_undo_status(&self) -> Option<ui::undo::LocalUndoStatus> {
        {
            if !self.focused || self.native_read_only || self.native_editor.is_none() {
                return None;
            }
            Some(
                self.native_editor
                    .as_ref()
                    .and_then(crate::native_editor::Client::snapshot)
                    .map_or(ui::undo::LocalUndoStatus::Pending, |snapshot| {
                        ui::undo::LocalUndoStatus::Ready(ui::undo::UndoStatus {
                            can_undo: snapshot.can_undo,
                            can_redo: snapshot.can_redo,
                            ..Default::default()
                        })
                    }),
            )
        }
    }
    fn handle_local_undo_request(&mut self, request: ui::undo::UndoRequest) -> bool {
        if self.focused
            && !self.native_read_only
            && let Some(editor) = &self.native_editor
        {
            if editor.snapshot().is_none() {
                return false;
            }
            self.native_capture = None;
            let _ = editor.history(request);
            return true;
        }

        false
    }
    fn start_local_undo_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        request: ui::undo::UndoRequest,
    ) -> Result<bool, String> {
        {
            if !self.focused || self.native_read_only {
                return Ok(false);
            }
            let Some(editor) = &self.native_editor else {
                return Ok(false);
            };
            if let Some((original, _)) = &self.native_history {
                return if *original == invocation {
                    Ok(true)
                } else {
                    Err("An original editor history operation is pending".into())
                };
            }
            self.native_capture = None;
            self.native_history = Some((invocation, editor.history(request)));
            Ok(true)
        }
    }
    fn poll_local_undo_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        cancellation_requested: bool,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        {
            let (original, receipt) = self.native_history.as_ref()?;
            if *original != invocation {
                return None;
            }
            if cancellation_requested {
                receipt.cancel();
            }
            let result = receipt.poll()?;
            self.native_history = None;
            Some(match result {
                Ok(None) => ui::tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: ui::tilo_actions::ActionOutput::default(),
                },
                _ => ui::tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: ui::tilo_actions::ActionFailure {
                        code: "native_history_unconfirmed".into(),
                        message: "The original editor did not acknowledge history restoration"
                            .into(),
                        details: None,
                        retryable: false,
                    },
                },
            })
        }
    }
}

impl ui::presence::capability::PublishesPresence for TerminalWidget {
    /// Scope this terminal's collaboration presence to a shared pane id (see
    /// the session record's pane) so both peers of a shared session file
    /// their caret/selection under the same key and can find each other's.
    fn presence_key(&self) -> Option<PresenceKey> {
        Some(PresenceKey::App {
            app: PermAppKind::Terminal,
            sub: self.pane_id.clone(),
        })
    }
    /// Broadcast the local view state to peers: the active text selection when
    /// there is one (so peers see what this user is selecting), otherwise the
    /// cursor position. The grid/scrollback itself rides the session record, so
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
    /// Record this terminal's session through `dispatcher`: the session
    /// record's own when the framework hosts the terminal, or a parent's
    /// child dispatcher when the terminal is nested in another resource (an
    /// editor's terminal panel), whose action carries each
    /// [`TerminalAction`](crate::resource::TerminalAction) and applies it
    /// with [`apply_under`](crate::resource::TerminalAction::apply_under).
    /// The parent then presents the nested session with
    /// [`ResourceSurface::present`](ui::framework::ResourceSurface::present)
    /// as it changes, read with
    /// [`Terminal::read_under`](crate::resource::Terminal::read_under).
    /// Everything the session is already running is recorded from the next
    /// sync on, in full.
    pub fn bind(&mut self, dispatcher: ui::framework::Dispatcher<crate::resource::Terminal>) {
        self.dispatch = Some(dispatcher);
        self.synced = None;
    }
}

impl TerminalWidget {
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
                self.submit_effect(crate::resource::TerminalEffect::PastePrimary { text });
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
        if self.take_no_runtime_offer(logical_key, mods) {
            *handled = true;
            return;
        }
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
                if self.selected_text().is_some_and(|text| !text.is_empty()) {
                    self.submit_effect(crate::resource::TerminalEffect::CopySelection);
                }
                *handled = true;
                return;
            }
            if c.as_str().eq_ignore_ascii_case("v") {
                if let Some(text) = clipboard_paste() {
                    self.submit_effect(crate::resource::TerminalEffect::Paste { text });
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
                self.submit_effect(crate::resource::TerminalEffect::Paste { text });
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
        start_y: usize,
        visible_rows: usize,
    ) {
        let Some(sel) = self.selection else { return };
        // The selection tint is already translucent, so glyphs show through.
        let sel_color = theme.tokens.colors.intent.selection_tint;
        for (row, start_col, end_col) in selection_row_spans(&sel, self.width) {
            if row < start_y || row >= start_y + visible_rows {
                continue;
            }
            let vrow = row - start_y;
            let py = (grid_rect.y0 + vrow as f64 * self.char_h).floor();
            let x0 = grid_rect.x0 + start_col as f64 * self.char_w;
            let x1 = grid_rect.x0 + end_col as f64 * self.char_w;
            let band = snap_to_device_pixels(transform, Rect::new(x0, py, x1, py + self.char_h));
            scene.fill(Fill::NonZero, transform, sel_color, None, &band);
        }
    }

    /// Paint the visible find matches over their glyphs, the current one
    /// emphasised. The matches stay local and are never serialized into the
    /// shared transcript; this is a visual locator over already-rendered
    /// cells.
    fn draw_find_match_highlight(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        grid_rect: Rect,
        start_y: usize,
        visible_rows: usize,
    ) {
        let end_y = start_y.saturating_add(visible_rows);
        for (index, found) in self.find_matches.iter().enumerate() {
            if found.row < start_y || found.row >= end_y {
                continue;
            }
            let row = found.row - start_y;
            let x0 = grid_rect.x0 + found.start_col as f64 * self.char_w;
            let x1 = grid_rect.x0 + found.end_col as f64 * self.char_w;
            let y0 = grid_rect.y0 + row as f64 * self.char_h;
            let marker = snap_to_device_pixels(
                transform,
                Rect::new(x0, y0, x1.max(x0 + self.char_w), y0 + self.char_h),
            );
            paint_find_match(scene, transform, marker, index == self.find_current, theme);
        }
    }

    /// Lay out the bands that stand above the grid: the target header, then
    /// the session banner. Records where they are and how much of the widget
    /// they take, which the grid geometry subtracts.
    fn layout_top_bands(&mut self, theme: &Theme, tr: &mut TextRenderer) {
        let gutter = f64::from(theme.tokens.spacing.s3);
        // The header with the session's target comes first.
        let mut y = self.rect.y0 + self.header_h();
        self.banner_rect = Rect::ZERO;
        if let Some(message) = self.status_banner_message()
            && self.rect.width() > gutter * 2.0
        {
            let width = self.rect.width() - gutter * 2.0;
            let height = Self::banner_height(&message, width, theme, tr);
            self.banner_rect = Rect::new(
                self.rect.x0 + gutter,
                y + gutter,
                self.rect.x1 - gutter,
                y + gutter + height,
            );
            y += gutter + height;
        }
        self.chrome_top_band = y - self.rect.y0;
    }

    /// The height a banner reading `message` needs in `width`.
    fn banner_height(message: &str, width: f64, theme: &Theme, tr: &mut TextRenderer) -> f64 {
        Banner::height(
            &BannerContent::new(BannerKind::Warning, message),
            width,
            tr,
            theme,
        )
    }

    /// Paint the session banner laid out by
    /// [`layout_top_bands`](Self::layout_top_bands).
    fn draw_top_bands(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.banner_rect.width() > 0.0
            && let Some(message) = self.status_banner_message()
        {
            // Transport and unconfirmed edits remain visible beside the
            // transcript and through the same accessible live region.
            let mut ctx = PaintCtx::new(scene, transform, tr, theme);
            Banner::paint(
                &mut ctx,
                self.banner_rect,
                &BannerContent::new(BannerKind::Warning, &message),
                self.last_cursor_pos,
            );
        }
    }

    /// The terminal's palette in `theme`. Its default background is the
    /// window ground, so a default-background cell is the surface the
    /// framework already painted and a reverse-video cell inverts against it.
    pub(crate) fn grid_palette(theme: &Theme) -> ui::tokens::TerminalPalette {
        theme.tokens.terminal_palette.clone()
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
            let left = grid_rect.x0 + placement.col as f64 * self.char_w;
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
                    theme.text_tertiary,
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
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = ctx.reborrow();
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
            // Each row on screen is a damage part: typing repaints its row.
            // Glyphs may overhang their row by up to a row above and below,
            // and the widget's width by up to two cells.
            let band = Rect::new(
                self.rect.x0 - 2.0 * self.char_w,
                py - self.char_h,
                self.rect.x1 + 2.0 * self.char_w,
                py + 2.0 * self.char_h,
            );
            let key = ui::scene_parts::PartKey::new("terminal.row", y);
            let bounds = vec![transform.transform_rect_bbox(band)];
            ui::scene_parts::part(scene, key, bounds, |scene| {
                let mut ctx = PaintCtx::new(scene, transform, text, theme);
                self.draw_cell_row(&mut ctx, cell_style, line_text, attrs, py);
            });
        }
    }

    /// Paint one row of cells, run by run of shared attributes, with its top
    /// at `py`.
    fn draw_cell_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        cell_style: &TextStyle,
        line_text: &str,
        attrs: &[Attr],
        py: f64,
    ) {
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
                        Point::new(run_start_cell_idx as f64 * self.char_w, py),
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
                Point::new(run_start_cell_idx as f64 * self.char_w, py),
            );
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
            ..
        } = ctx.reborrow();
        let palette = &self.palette;
        let grid_rect = self.grid_rect;
        let scroll = self.scroll_offset;
        let cursor_color = self.last_cursor_color;
        // Absolute grid row of the cursor → visible row (may be scrolled off).
        let cursor_abs = self.grid.len().saturating_sub(self.height) + self.cursor_y;
        let scrolled_back = !self.alt_active && scroll > 0;
        let shown = self.modes.cursor_visible
            && !scrolled_back
            && cursor_abs >= start_y
            && cursor_abs < start_y + visible_rows.max(1);
        let cy_local = cursor_abs.saturating_sub(start_y);
        let cx = grid_rect.x0 + self.cursor_x as f64 * self.char_w;
        let cy = (grid_rect.y0 + cy_local as f64 * self.char_h).floor();
        // The cursor is a damage part of its own: moving it repaints the
        // cell it left and the cell it entered. Its glyph may overhang the
        // cell by up to a cell on every side.
        let bounds = if shown {
            vec![transform.transform_rect_bbox(Rect::new(
                cx - self.char_w,
                cy - self.char_h,
                cx + 2.0 * self.char_w,
                cy + 2.0 * self.char_h,
            ))]
        } else {
            Vec::new()
        };
        let key = ui::scene_parts::PartKey::new("terminal.cursor", ());
        ui::scene_parts::part(scene, key, bounds, |scene| {
            if !shown {
                return;
            }
            // An unmistakable, theme-coloured "inverted cell" cursor.
            let cursor_rect = snap_to_device_pixels(
                transform,
                Rect::new(cx, cy, cx + self.char_w, cy + self.char_h),
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
        });
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
            ..
        } = ctx.reborrow();
        let palette = &self.palette;
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
            let bg_rect = snap_to_device_pixels(
                transform,
                Rect::new(tx, py, tx + visual_width, py + self.char_h),
            );
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
            let line =
                snap_to_device_pixels(transform, Rect::new(tx, uy, tx + visual_width, uy + 1.0));
            scene.fill(vello::peniko::Fill::NonZero, transform, fg, None, &line);
        }
        if attr.strike {
            // A crossed-out line through the vertical centre of the cell.
            let sy = (py + self.char_h * 0.5).floor();
            let line =
                snap_to_device_pixels(transform, Rect::new(tx, sy, tx + visual_width, sy + 1.0));
            scene.fill(vello::peniko::Fill::NonZero, transform, fg, None, &line);
        }
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

    /// How many rows the view scrolls over: the whole buffer, minus blank rows
    /// below the last thing on it. A session allocates a full screen of rows
    /// up front, and a band above the grid (the find bar, the session banner)
    /// shows fewer rows than that screen. Anchoring the view to the bottom of
    /// the allocation would then hide the first lines of a short transcript
    /// behind the band, so the view only reaches past what is on screen when
    /// there is something there. The alternate screen is always shown whole.
    fn view_rows(&self) -> usize {
        if self.alt_active {
            return self.grid.len();
        }
        self.content_rows()
            .max(self.current_visible_rows())
            .min(self.grid.len())
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
        !self.alt_active && visible_rows > 0 && self.content_rows() > visible_rows
    }

    /// The shared scrollbar thumb in the reserved right gutter, drawn only
    /// when the buffer overflows the viewport. The toolkit brightens it while
    /// it is hovered or dragged.
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
        let metrics =
            ScrollMetrics::new(self.view_rows() as f64, visible_rows as f64, start_y as f64);
        self.scrollbar.draw(
            scene,
            transform,
            self.scrollbar_track(),
            metrics,
            theme,
            false,
        );
    }

    /// What the grid says while no shell runs behind it: the toolkit's empty
    /// state over the grid area.
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let area = self.grid_rect;
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        // An ended session already says why in the banner above, so the grid
        // only reports that the shell left nothing behind; a widget that never
        // had a process explains where output will appear. A session that is
        // still waiting for the person's allow, or whose target could not be
        // opened, never ran a shell: the banner says why.
        let shell_ran = matches!(
            self.sessions.state,
            crate::sessions::OpenState::Open | crate::sessions::OpenState::Fixed
        );
        let content = if self.is_disconnected() && shell_ran {
            EmptyStateContent {
                icon: Some(Icon::Terminal),
                message: Some("The shell exited before it printed anything."),
                ..EmptyStateContent::new("No output")
            }
        } else {
            EmptyStateContent {
                icon: Some(Icon::Terminal),
                message: Some("Output appears here once a shell session starts."),
                ..EmptyStateContent::new("No shell running")
            }
        };
        EmptyState::paint(&mut ctx, area, &content, self.last_cursor_pos);
    }

    /// Convert a logical pointer position into a **buffer-absolute**
    /// `GridPos`. The on-screen row is offset by the current scrollback
    /// position (`viewport_start_y`) so the returned row indexes `self.grid`
    /// directly and a selection anchored to it survives scrolling.
    /// Coordinates outside the cell grid are clamped to the nearest valid
    /// visible cell — matching xterm's behaviour during a drag-select that
    /// leaves the terminal area.
    fn pos_from_logical(&self, p: Point) -> GridPos {
        let local_x = (p.x - self.grid_rect.x0).max(0.0);
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
        let x = self.grid_rect.x0 + cell.col as f64 * self.char_w;
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
            let x0 = self.grid_rect.x0 + start_col as f64 * self.char_w;
            let x1 = self.grid_rect.x0 + end_col as f64 * self.char_w;
            rects.push(Rect::new(x0, py, x1, py + self.char_h));
        }
        rects
    }

    /// Build + open the shared terminal context menu at the cached cursor.
    /// Selection and clipboard basics stay first; less common session
    /// affordances remain discoverable without adding permanent app chrome.
    fn open_context_menu_at_cursor(&mut self) {
        self.target_menu_rows = None;
        let has_selection = self.selected_text().is_some_and(|t| !t.is_empty());
        let mut menu = ContextMenu::new();
        menu.set_platform_conventions(self.conventions);
        self.menu_effects.clear();
        self.menu_paste_index = None;
        let live = !self.is_disconnected();
        if has_selection {
            self.push_menu_effect(
                &mut menu,
                "Copy",
                crate::resource::TerminalEffect::CopySelection,
                true,
            );
        }
        self.menu_paste_index = Some(menu.items().len());
        menu.push(MenuItem::action("Paste", || {}).with_enabled(live));
        menu.push(MenuItem::separator());
        if !self.grid_is_blank() {
            self.push_menu_effect(
                &mut menu,
                "Select all output",
                crate::resource::TerminalEffect::SelectAll,
                true,
            );
        }
        self.push_menu_effect(
            &mut menu,
            "Clear screen",
            crate::resource::TerminalEffect::Clear,
            live,
        );
        self.find_menu_index = Some(menu.items().len());
        menu.push(MenuItem::action(
            crate::i18n::t("terminal-find-scrollback"),
            || {},
        ));
        // This preference is useful secondary chrome, but never at the cost
        // of an unreachable context menu. A 160px-high embedded/narrow pane
        // can fit the copy/paste/selection/process essentials, not another
        // row plus separator; its keyboard shortcut and desktop-sized menu
        // still expose the toggle.
        self.copy_on_select_menu_index = None;
        if self.rect.height() >= 180.0 {
            menu.push(MenuItem::separator());
            self.copy_on_select_menu_index = Some(menu.items().len());
            menu.push(MenuItem::action("Copy on select", || {}).with_checked(self.select_to_copy));
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

    /// A menu row that asks for `effect` as the person's gesture.
    fn push_menu_effect(
        &mut self,
        menu: &mut ContextMenu,
        label: &str,
        effect: crate::resource::TerminalEffect,
        enabled: bool,
    ) {
        self.menu_effects.insert(menu.items().len(), effect);
        menu.push(MenuItem::action(label, || {}).with_enabled(enabled));
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
                self.submit_effect(crate::resource::TerminalEffect::Paste { text });
            }
            return;
        }
        if let Some(effect) = self.menu_effects.get(&index).cloned() {
            self.submit_effect(effect);
            return;
        }
        if self.find_menu_index == Some(index) {
            self.find_request = true;
            return;
        }
        if self.copy_on_select_menu_index == Some(index) {
            self.select_to_copy = !self.select_to_copy;
        }
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
    /// The screen the output is drawn on.
    pub screen: &'a mut crate::screen::Screen,
    /// Where Kitty graphics payloads are stored; the screen keeps only the hash.
    pub content: &'a ContentStore,
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
    /// Monotonic OSC 8 link-id allocator (shared with the widget so ids stay
    /// unique across feeds). `0` is reserved for "no link".
    pub next_link_id: &'a mut u16,
    /// Link id of the currently-open OSC 8 hyperlink (`0` = none). Every cell
    /// printed while this is non-zero adopts it.
    pub current_link_id: u16,
    /// Out-param for an OSC 0/2 window-title change parsed this feed; the
    /// widget records it in the session and its own `title` field afterwards.
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
    /// Batches consecutive `print`s to the same line so a run of characters
    /// replaces the line once rather than once per cell, which keeps a
    /// screenful of output linear. Any non-`print` action flushes the buffer
    /// first (see `flush_print_buf`) so it never observes stale grid state.
    pub print_buf: Option<PrintBuf>,
}

/// In-memory accumulator for a run of `print`s landing on one grid line.
/// Mutated per character; written to the line once when the line changes,
/// any other emulator action runs, or the feed ends.
pub struct PrintBuf {
    text: String,
    attrs: Vec<u8>,
    alternate: bool,
    index: usize,
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
        let alternate = self.screen.alternate;
        // Point the write buffer at this line, loading its current text + attrs
        // once. Consecutive prints to the same line then mutate memory only;
        // the line is replaced a single time when the print run ends.
        let need_load = self
            .print_buf
            .as_ref()
            .is_none_or(|b| b.index != y || b.alternate != alternate);
        if need_load {
            self.flush_print_buf();
            let Some(line) = self.screen.active().get(y) else {
                return;
            };
            self.print_buf = Some(PrintBuf {
                text: line.text.clone(),
                attrs: line.attrs.clone(),
                alternate,
                index: y,
            });
        }
        // `need_load` above always leaves `print_buf` populated by this
        // point — unless the line is missing, in which case that branch
        // already returned.
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
            // URI closes the current link. We register the URI on the screen and
            // stamp its id onto subsequent cells via `current_link_id`.
            8 => {
                let uri = params.get(2).copied().unwrap_or(b"");
                if uri.is_empty() {
                    self.current_link_id = 0;
                } else if self.screen.links.len() < MAX_OSC_LINKS
                    && let Some(url) = safe_osc_text(uri, MAX_OSC_LINK_BYTES)
                    && let Some(id) = self.next_link_id.checked_add(1)
                {
                    *self.next_link_id = id;
                    self.screen.links.insert(id, url);
                    self.current_link_id = id;
                }
            }
            _ => {}
        }
    }
}

impl TerminalPerformer<'_> {
    /// Persist a complete direct Kitty transmission as a content-addressed blob
    /// plus a lightweight placement on the screen. The local renderer
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
        if image.columns == 0 || image.rows == 0 || image.columns > 1024 || image.rows > 1024 {
            return;
        }
        self.screen.images.push(TerminalImagePlacement {
            hash,
            mime: image.mime.to_string(),
            byte_len: image.bytes.len() as u64,
            row: self.absolute_y(),
            col: self.cursor_x,
            columns: image.columns,
            rows: image.rows,
            pixel_width: image.pixel_width,
            pixel_height: image.pixel_height,
            protocol_id: image.protocol_id,
            alternate_screen: self.screen.alternate,
        });
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

    fn active_len(&self) -> usize {
        self.screen.active().len()
    }

    fn active_lines_mut(&mut self) -> &mut Vec<std::rc::Rc<crate::screen::Line>> {
        let alternate = self.screen.alternate;
        self.screen.lines_mut(alternate)
    }

    fn blank_line(&self) -> std::rc::Rc<crate::screen::Line> {
        std::rc::Rc::new(crate::screen::Line {
            text: " ".repeat(self.width),
            attrs: vec![Attr::default().pack(); self.width].concat(),
        })
    }

    /// Insert a blank line into the active buffer at `index`.
    fn insert_blank_line(&mut self, index: usize) {
        let line = self.blank_line();
        let lines = self.active_lines_mut();
        let index = index.min(lines.len());
        lines.insert(index, line);
    }

    fn delete_grid_line(&mut self, index: usize) {
        let lines = self.active_lines_mut();
        if index < lines.len() {
            lines.remove(index);
        }
    }

    /// Replace one line of the active buffer. An unchanged line keeps its
    /// allocation, which is how the session record learns it did not change.
    fn write_grid_line(&mut self, index: usize, text: String, attrs: Vec<u8>) {
        if let Some(line) = self.active_lines_mut().get_mut(index)
            && (line.text != text || line.attrs != attrs)
        {
            *line = std::rc::Rc::new(crate::screen::Line { text, attrs });
        }
    }

    fn line_text_and_attrs(&self, index: usize) -> Option<(String, Vec<u8>)> {
        self.screen
            .active()
            .get(index)
            .map(|line| (line.text.clone(), line.attrs.clone()))
    }

    fn newline(&mut self) {
        // A line feed leaves the current line; commit any buffered prints first
        // so the appended/scrolled structure is computed over the fresh line
        // (and so a print → auto-wrap → newline doesn't strand the old line).
        self.flush_print_buf();
        self.cursor_y += 1;
        if self.cursor_y < self.height {
            return;
        }
        self.cursor_y = self.height - 1;

        if self.screen.alternate {
            // Fixed-height alternate buffer: scroll the region up in place.
            self.scroll_region_up(1);
        } else {
            // Main buffer accumulates scrollback.
            let line = self.blank_line();
            self.screen.main.push(line);

            // Bound the retained scrollback. Trimming one line at a time on
            // every append made `Vec::remove(0)` an O(n) shift per output line
            // (O(n²) over a flood); instead let history overrun by a slack
            // margin and drop the overflow in a single `drain`. Images on the
            // dropped rows go with them; the others move up.
            if self.screen.main.len() > MAX_HISTORY + HISTORY_TRIM_SLACK {
                let overflow = self.screen.main.len() - MAX_HISTORY;
                self.screen.main.drain(0..overflow);
                self.screen.images.retain_mut(|image| {
                    if image.alternate_screen {
                        return true;
                    }
                    let kept = image.row >= overflow;
                    image.row = image.row.saturating_sub(overflow);
                    kept
                });
            }
        }
    }

    /// Scroll the current scrolling region up by `n` lines (content moves up;
    /// blank lines appear at the bottom of the region). Honours the DEC
    /// scrolling margins.
    fn scroll_region_up(&mut self, n: usize) {
        let Some(last) = self.active_len().checked_sub(1) else {
            return;
        };
        self.shift_rows(
            self.scroll_top.min(last),
            self.scroll_bottom.min(last),
            n,
            false,
        );
    }

    /// Scroll the current scrolling region down by `n` lines (content moves
    /// down; blank lines appear at the top of the region).
    fn scroll_region_down(&mut self, n: usize) {
        let Some(last) = self.active_len().checked_sub(1) else {
            return;
        };
        self.shift_rows(
            self.scroll_top.min(last),
            self.scroll_bottom.min(last),
            n,
            true,
        );
    }

    /// IL — insert `n` blank lines at the cursor row, within the region.
    fn insert_lines(&mut self, n: usize) {
        let Some(last) = self.active_len().checked_sub(1) else {
            return;
        };
        let at = self.cursor_y.max(self.scroll_top);
        if self.shift_rows(at, self.scroll_bottom.min(last), n, true) {
            self.cursor_x = 0;
        }
    }

    /// DL — delete `n` lines at the cursor row, within the region.
    fn delete_lines(&mut self, n: usize) {
        let Some(last) = self.active_len().checked_sub(1) else {
            return;
        };
        let at = self.cursor_y.max(self.scroll_top);
        if self.shift_rows(at, self.scroll_bottom.min(last), n, false) {
            self.cursor_x = 0;
        }
    }

    /// Move rows `top..=bot` by `n` (at most the whole span): `down` pushes
    /// content towards `bot` and opens blank lines at `top`, otherwise the
    /// reverse. Whether the span held any rows; nothing moves when it is
    /// empty.
    fn shift_rows(&mut self, top: usize, bot: usize, n: usize, down: bool) -> bool {
        if top > bot {
            return false;
        }
        let (removed, opened) = if down { (bot, top) } else { (top, bot) };
        for _ in 0..n.min(bot - top + 1) {
            self.delete_grid_line(removed);
            self.insert_blank_line(opened);
        }
        true
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
        let Some((text, mut attrs)) = self.line_text_and_attrs(y) else {
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
        let Some((text, mut attrs)) = self.line_text_and_attrs(y) else {
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
        let Some((text, attrs)) = self.line_text_and_attrs(y) else {
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
        let Some((text, attrs)) = self.line_text_and_attrs(y) else {
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
        if self.screen.alternate {
            return;
        }
        if save_cursor {
            *self.saved_cursor = Some((self.cursor_x, self.cursor_y, self.current_attr));
        }
        self.screen.alternate = true;
        // Present a cleared alternate screen and home the cursor.
        self.clear_alt_buffer();
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.pending_wrap = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.height.saturating_sub(1);
    }

    fn exit_alt(&mut self, restore_cursor: bool) {
        if !self.screen.alternate {
            return;
        }
        self.screen.alternate = false;
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
        let count = self.screen.alt.len();
        let text = " ".repeat(width);
        let attrs = vec![Attr::default().pack(); width].concat();
        for index in 0..count {
            self.write_grid_line(index, text.clone(), attrs.clone());
        }
        while self.screen.alt.len() > self.height {
            let idx = self.screen.alt.len() - 1;
            self.delete_grid_line(idx);
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
    /// stream is parsed; the screen's copy is written once per feed by
    /// [`flush_cursor`](Self::flush_cursor) rather than on every cell, so this
    /// is intentionally a no-op kept only so the call sites read naturally.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        clippy::unused_self,
        reason = "deliberate no-op: `&mut self` marks every cursor-moving call site as a mutation for readability/symmetry with flush_cursor, even though the actual screen write is batched there"
    )]
    fn update_cursor(&mut self) {}

    /// Commit any buffered run of printed characters to its line, once for
    /// the whole run. A no-op when nothing is buffered.
    pub(crate) fn flush_print_buf(&mut self) {
        if let Some(buf) = self.print_buf.take()
            && let Some(line) = self.screen.lines_mut(buf.alternate).get_mut(buf.index)
            && (line.text != buf.text || line.attrs != buf.attrs)
        {
            *line = std::rc::Rc::new(crate::screen::Line {
                text: buf.text,
                attrs: buf.attrs,
            });
        }
    }

    /// Record the final cursor position. Called once at the end of a feed
    /// rather than on every printed cell.
    pub fn flush_cursor(&mut self) {
        // Commit the last line of a print run that ended the feed (no trailing
        // control byte to flush it).
        self.flush_print_buf();
        self.screen.cursor = (self.cursor_x as u64, self.absolute_y() as u64);
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
            47 | 1047 | 1049 => Some(self.screen.alternate),
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
    // An opaque inverted cell: the block is the theme's default foreground and
    // the glyph beneath it is re-drawn in the background colour. Taking the hue
    // from the palette keeps the cursor tracking theme swaps.
    palette.fg_default.with_alpha_factor(1.0)
}

/// Snap `rect` to whole device pixels under `transform`, so a cell fill has
/// crisp edges instead of an anti-aliased fringe. Edges are rounded as device
/// coordinates, so two rects that meet at one coordinate still meet after
/// snapping: adjacent runs and rows tile without a seam or an overlap.
/// `transform` is the paint transform of an axis-aligned scale and offset.
pub(crate) fn snap_to_device_pixels(transform: Affine, rect: Rect) -> Rect {
    let [sx, _, _, sy, tx, ty] = transform.as_coeffs();
    if sx == 0.0 || sy == 0.0 {
        return rect;
    }
    let snap = |v: f64, scale: f64, offset: f64| ((v * scale + offset).round() - offset) / scale;
    Rect::new(
        snap(rect.x0, sx, tx),
        snap(rect.y0, sy, ty),
        snap(rect.x1, sx, tx),
        snap(rect.y1, sy, ty),
    )
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

/// The command the Host target runs off Tilo: the user's native shell as chosen
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

/// A PTY master with no process behind it.
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

pub struct TerminalBackend {
    pub writer: Box<dyn Write + Send>,
    pub reader: Box<dyn Read + Send>,
    pub master: Box<dyn MasterPty + Send>,
    pub shell_name: String,
    /// For an environment on a relay: the session's id there, which
    /// re-attaches to it.
    pub relay_session: Option<String>,
    native_editor: Option<crate::native_editor::Socket>,
    service_lifetime: Option<runtime_portal::pending_connection::SessionLifetime>,
    #[cfg(windows)]
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
}
impl TerminalBackend {
    /// A backend with no process behind it — for tests of what opens where.
    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn for_test() -> Self {
        Self {
            writer: Box::new(std::io::sink()),
            reader: Box::new(std::io::empty()),
            master: Box::new(NullMaster),
            shell_name: "test".into(),
            relay_session: None,
            native_editor: None,
            service_lifetime: None,
        }
    }

    /// Open a PTY on `target`.
    ///
    /// **Never `$SHELL` by default.** Inside a Tilo app sandbox there is no
    /// `/bin` at all: `launcher::profile` binds the loader, fonts, `/sys` and
    /// the CA bundle, and deliberately binds no executable directory. A session
    /// runs where its target says (`docs/app-model/environments.md`), and the
    /// route to it is chosen by [`decide_route`] from a capability probe, not
    /// by trying each in turn — so confinement is never silently downgraded.
    ///
    /// 1. **The service socket.** A sandboxed app cannot build a namespace, so
    ///    it asks the session's supervisor and receives a pty master over
    ///    `SCM_RIGHTS`. This is the production path inside Tilo and it is
    ///    **mandatory when available**, for every target: the device's own
    ///    shell too goes through the supervisor, which alone decides whether
    ///    this person may have it.
    /// 2. **Spawning the runtime.** An environment on an unconfined host that
    ///    can build the namespace a guest needs and has the runtime installed —
    ///    a development tree, mostly.
    /// 3. **The device's own shell, chosen.** Off Tilo, with no supervisor,
    ///    the Host target is the user's login shell, started directly. It is
    ///    only ever reached because the session's target names it, never as a
    ///    fallback for an environment that cannot run.
    ///
    /// # Errors
    ///
    /// Returns why the session could not open: the target needs the person's
    /// allow first, the supervisor refused it, or nothing here can run it.
    pub fn open(
        target: &crate::target::SessionTarget,
        requested_by: Requester,
        relay_session: Option<&str>,
    ) -> Result<Self, OpenError> {
        let program = launch_command_from_env();
        match confinement_route(target) {
            ConfineRoute::Service => {
                ush_service_backend(&program, target, requested_by, relay_session)
            }
            ConfineRoute::SelfHosted(bin) => {
                let mut cmd = CommandBuilder::new(&bin);
                cmd.arg("run");
                if !program.is_empty() {
                    cmd.arg("-c");
                    cmd.arg(exec_script(&program));
                }
                Self::with_command(cmd).map_err(|error| OpenError::Failed(error.to_string()))
            }
            ConfineRoute::Host => {
                if requested_by == Requester::Agent {
                    return Err(OpenError::Refused(
                        "agents may not open the device's own shell".into(),
                    ));
                }
                let cmd = if let Some((first, rest)) = program.split_first() {
                    let mut cmd = CommandBuilder::new(first);
                    cmd.args(rest);
                    cmd
                } else {
                    login_shell_command()
                };
                Self::with_command(cmd).map_err(|error| OpenError::Failed(error.to_string()))
            }
            ConfineRoute::Unavailable(reason) => Err(OpenError::Unavailable(reason)),
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
            relay_session: None,
            native_editor: None,
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

/// Ownership and refusal retries live with the runtime's service transport.
static USH_SERVICE: runtime_portal::service_socket::InheritedServiceSocket =
    runtime_portal::service_socket::InheritedServiceSocket::new("ush");

/// Who caused a session to open. The device's own shell is never opened for
/// an agent, whatever else allows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Requester {
    /// The person, through the terminal's own controls or by reopening a tab.
    #[default]
    Person,
    /// An agent, through the terminal's published actions.
    Agent,
}

impl Requester {
    /// Who is behind the current invocation. Only the person at this screen
    /// acts without a delivered invocation; anyone reaching the terminal
    /// through the action broker — an agent, another app, or a collaborator
    /// in a shared room — is not that person, and is treated as an agent.
    pub fn current() -> Self {
        if ui::invocation_scope::current_invoker().is_some() {
            Self::Agent
        } else {
            Self::Person
        }
    }

    fn wire(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Agent => "agent",
        }
    }
}

/// Why a session did not open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The device's own shell needs the person's allow first.
    NeedsAllow(String),
    /// The supervisor (or this terminal) refused the request.
    Refused(String),
    /// Nothing here can run this target.
    Unavailable(String),
    /// No environment can be built on this device: no `env-runtime` image is
    /// installed. The device's own shell may still be opened instead.
    NoRuntime(String),
    /// Opening failed.
    Failed(String),
}

impl OpenError {
    /// Whether the supervisor answered and refused, leaving the connection
    /// usable for another request.
    pub fn is_refusal(&self) -> bool {
        matches!(
            self,
            Self::NeedsAllow(_) | Self::Refused(_) | Self::NoRuntime(_)
        )
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NeedsAllow(reason)
            | Self::Refused(reason)
            | Self::Unavailable(reason)
            | Self::NoRuntime(reason)
            | Self::Failed(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for OpenError {}

/// Which way the terminal will reach a target, decided by capability probe.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfineRoute {
    /// The supervisor service is reachable — the Tilo production path, for
    /// every target. Mandatory when chosen: a refusal is final.
    Service,
    /// This unconfined host can build a guest itself and has the runtime at
    /// the given path.
    SelfHosted(std::path::PathBuf),
    /// No supervisor, and the target is the device's own shell: start the
    /// login shell directly.
    Host,
    /// Nothing here can run the target.
    Unavailable(String),
}

/// Choose how to reach `target` from a capability probe, security-first.
pub fn confinement_route(target: &crate::target::SessionTarget) -> ConfineRoute {
    #[cfg(windows)]
    {
        // A claimed hosted grant must fail closed until native service
        // transport exists; never silently replace it with a local shell.
        decide_route(
            target,
            std::env::var_os(USH_SERVICE_FD_ENV).is_some(),
            false,
            None,
        )
    }
    #[cfg(unix)]
    decide_route(
        target,
        USH_SERVICE.was_taken() || ush_service_fd().is_some(),
        can_build_namespace(),
        ush_binary(),
    )
}

/// Whether an environment can run here at all — what decides that a new tab
/// on a machine without Tilo confinement opens the device's own shell.
pub fn environments_available() -> bool {
    !matches!(
        confinement_route(&crate::target::SessionTarget::default_environment()),
        ConfineRoute::Unavailable(_)
    )
}

/// Pure core of [`confinement_route`], with the probed inputs injected so the
/// decision — the actual trust boundary — is unit-testable without touching
/// process-global state.
///
/// The invariants under test: `service_available` wins unconditionally, so no
/// target reaches a local process while the supervisor is present; the
/// device's own shell is reached only by naming it; and an environment that
/// cannot run is unavailable, never quietly the host shell instead. Relay and
/// relay targets are carried there by this device's supervisor; legacy VMs
/// are not served by it.
pub fn decide_route(
    target: &crate::target::SessionTarget,
    service_available: bool,
    can_self_host: bool,
    runtime: Option<std::path::PathBuf>,
) -> ConfineRoute {
    use crate::target::{Location, SessionTarget};
    match target {
        SessionTarget::Environment {
            location: Location::Relay { .. },
            ..
        } if !service_available => {
            return ConfineRoute::Unavailable(
                "Environments on a relay are reached through Tilo's shell runtime, which is \
                 not available here"
                    .into(),
            );
        }
        SessionTarget::Legacy { .. } => {
            return ConfineRoute::Unavailable("The legacy VM service is not running".into());
        }
        _ => {}
    }
    if service_available {
        return ConfineRoute::Service;
    }
    if target.is_host() {
        return ConfineRoute::Host;
    }
    if can_self_host && let Some(bin) = runtime {
        return ConfineRoute::SelfHosted(bin);
    }
    ConfineRoute::Unavailable(
        "Environments need Tilo's shell runtime, which is not available here".into(),
    )
}

/// The supervisor-service descriptor, if the launcher opened one for us and it
/// is still a live fd.
///
/// An env var alone is not enough: the descriptor slot must actually be open,
/// so a stale or spoofed number pointing at nothing does not select the service
/// path (off Tilo, only a session whose target is the device's own shell runs locally).
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

/// Ask the session supervisor for a session on `target` and wrap the pty it
/// hands back.
///
/// The whole exchange is one line of JSON out and one line plus a descriptor
/// back. What crosses the sandbox boundary is a terminal, not a namespace: the
/// session is built on the supervisor's side, by the supervisor's rules.
#[cfg(unix)]
fn ush_service_backend(
    program: &[String],
    target: &crate::target::SessionTarget,
    requested_by: Requester,
    relay_session: Option<&str>,
) -> Result<TerminalBackend, OpenError> {
    use runtime_portal::service_socket::ServiceSocketError;

    // SAFETY: this process-wide instance alone owns the launcher's ush fd.
    let stream = unsafe { USH_SERVICE.take() }.map_err(|error| match error {
        ServiceSocketError::InUse => OpenError::Unavailable(
            "This window's shell service is already in use; open a new tab".into(),
        ),
        other => OpenError::Failed(other.to_string()),
    })?;
    let opened = ush_backend_on(&stream, program, target, requested_by, relay_session);
    if opened.as_ref().is_err_and(OpenError::is_refusal) {
        let _ = USH_SERVICE.retry_after_refusal(stream);
    }
    opened
}

#[cfg(windows)]
fn ush_service_backend(
    _program: &[String],
    _target: &crate::target::SessionTarget,
    _requested_by: Requester,
    _relay_session: Option<&str>,
) -> Result<TerminalBackend, OpenError> {
    Err(OpenError::Unavailable(
        "Tilo hosted terminal service transport is unavailable on Windows".into(),
    ))
}

pub(crate) fn ush_request(
    program: &[String],
    target: &crate::target::SessionTarget,
    requested_by: Requester,
    relay_session: Option<&str>,
    (cols, rows): (u16, u16),
) -> Result<Vec<u8>, OpenError> {
    let wire_target = target.to_supervisor().ok_or_else(|| {
        OpenError::Unavailable("This device's shell service does not serve that target".into())
    })?;
    let mut request = serde_json::json!({
        "op": "open-session",
        "cols": cols,
        "rows": rows,
        "term": "xterm-256color",
        "target": wire_target,
        "requested_by": requested_by.wire(),
    });
    if let Some(fields) = request.as_object_mut() {
        if tilo_process::editor::supported() && program.is_empty() && !target.is_host() {
            fields.insert(
                "editor_protocol".into(),
                serde_json::json!(runtime_portal::editor::VERSION),
            );
        }
        if !program.is_empty() {
            fields.insert("command".to_string(), serde_json::json!(program));
        }
        if let Some(session) = relay_session {
            fields.insert("relay_session".to_string(), serde_json::json!(session));
        }
    }
    let mut request =
        serde_json::to_vec(&request).map_err(|error| OpenError::Failed(error.to_string()))?;
    request.push(b'\n');
    Ok(request)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UshSessionReply {
    result: String,
    session: String,
    environment: String,
    #[serde(default)]
    relay_session: Option<String>,
    #[serde(default)]
    editor_protocol: Option<u8>,
}

#[cfg(unix)]
pub(crate) fn ush_backend_on(
    stream: &std::os::unix::net::UnixStream,
    program: &[String],
    target: &crate::target::SessionTarget,
    requested_by: Requester,
    relay_session: Option<&str>,
) -> Result<TerminalBackend, OpenError> {
    use std::os::fd::AsRawFd;
    // How long the answer may take. Before it answers, the supervisor checks
    // that the environment can be built, which on a device's first open reads
    // the system store's images: ~400 ms on the CI VM, past the 250 ms a
    // hosted connection is given to connect (TIL-708). Never shorten a longer
    // bound, nor add one where the connection has none.
    const USH_OPEN_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
    let failed = |error: &dyn std::fmt::Display| OpenError::Failed(error.to_string());
    let request = ush_request(program, target, requested_by, relay_session, (80, 24))?;
    {
        let mut w = stream;
        w.write_all(&request).map_err(|e| failed(&e))?;
        w.flush().map_err(|e| failed(&e))?;
    }
    if stream
        .read_timeout()
        .map_err(|e| failed(&e))?
        .is_some_and(|bound| bound < USH_OPEN_REPLY_TIMEOUT)
    {
        stream
            .set_read_timeout(Some(USH_OPEN_REPLY_TIMEOUT))
            .map_err(|e| failed(&e))?;
    }

    let (payload, mut descriptors) =
        runtime_portal::fd_transport::recv_with_owned_fds(stream.as_raw_fd())
            .map_err(|e| failed(&e))?;
    if descriptors.is_empty() {
        return Err(refusal(&payload));
    }
    let response: UshSessionReply = serde_json::from_slice(&payload)
        .map_err(|_| OpenError::Failed("Malformed ush session descriptor declaration".into()))?;
    if response.result != "session"
        || response.session.len() != 32
        || !response.session.bytes().all(|b| b.is_ascii_hexdigit())
        || response.environment.is_empty()
        || response.environment.len() > 1024
        || response.relay_session.as_ref().is_some_and(|session| {
            session.len() != 32 || !session.bytes().all(|b| b.is_ascii_hexdigit())
        })
        || response
            .editor_protocol
            .is_some_and(|version| version != runtime_portal::editor::VERSION)
        || (response.editor_protocol.is_some() && (!program.is_empty() || target.is_host()))
        || descriptors.len()
            != if response.editor_protocol.is_some() {
                2
            } else {
                1
            }
    {
        return Err(OpenError::Failed(
            "Invalid ush session descriptor roles".into(),
        ));
    }
    let native_editor = if response.editor_protocol.is_some() {
        let fd = descriptors
            .pop()
            .ok_or_else(|| OpenError::Failed("Missing editor descriptor".into()))?;
        Some(crate::native_editor::Socket::adopt(fd).map_err(|e| failed(&e))?)
    } else {
        None
    };
    let fd = descriptors
        .pop()
        .ok_or_else(|| OpenError::Failed("Missing PTY descriptor".into()))?;

    // `fd` arrived over `SCM_RIGHTS` and this process now owns it. The
    // supervisor declares PTY first and, only after negotiation, editor second.
    let master = std::fs::File::from(fd);
    tilo_process::editor::validate_master(&master).map_err(|e| failed(&e))?;
    let reader = Box::new(master.try_clone().map_err(|e| failed(&e))?) as Box<dyn Read + Send>;
    let writer = Box::new(master.try_clone().map_err(|e| failed(&e))?) as Box<dyn Write + Send>;
    Ok(TerminalBackend {
        writer,
        reader,
        master: Box::new(FdMaster { file: master }),
        shell_name: if target.is_host() { "host" } else { "ush" }.into(),
        relay_session: response.relay_session,
        native_editor,
        service_lifetime: Some(runtime_portal::pending_connection::SessionLifetime::new(
            stream.try_clone().map_err(|e| failed(&e))?,
        )),
    })
}

/// What a supervisor reply without a descriptor means.
pub(crate) fn refusal(payload: &[u8]) -> OpenError {
    #[derive(serde::Deserialize)]
    struct Reply {
        result: String,
        #[serde(default)]
        reason: String,
    }
    match serde_json::from_slice::<Reply>(payload) {
        Ok(reply) if reply.result == "host-not-allowed" => OpenError::NeedsAllow(reply.reason),
        Ok(reply) if reply.result == "no-runtime" => OpenError::NoRuntime(reply.reason),
        Ok(reply) if !reply.reason.is_empty() => OpenError::Refused(reply.reason),
        _ => OpenError::Refused(format!(
            "the ush service refused this session: {}",
            String::from_utf8_lossy(payload).trim()
        )),
    }
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
        let mut widget = Self::without_process(ContentStore::memory());
        widget.attach_backend(backend);
        widget
    }

    /// A widget with no process behind it yet: a sink writer, an empty
    /// reader, and a mock PTY master. A session is attached later with
    /// [`Self::attach_backend`].
    fn without_process(content: ContentStore) -> Self {
        let writer: Box<dyn Write + Send> = Box::new(std::io::sink());
        let (_tx, output_rx) = sync_channel(1);
        let widget = TerminalWidget::new(
            std::sync::Arc::new(std::sync::Mutex::new(writer)),
            std::sync::Arc::new(std::sync::Mutex::new(Box::new(NullMaster))),
            output_rx,
            content,
        );
        widget.mark_disconnected("Terminal process has not opened — input unavailable");
        widget
    }

    /// Put a freshly opened process behind this widget: its writer, master
    /// and lifetime replace the previous ones, a reader thread starts feeding
    /// the output channel, and the tab's screen is reset for the new process
    /// at the next sync.
    fn attach_native_editor(&mut self, socket: Option<crate::native_editor::Socket>) {
        self.native_capture = None;
        self.native_history = None;
        self.native_ime = None;
        self.native_ime_error = None;
        self.native_editor = socket.and_then(|socket| {
            crate::native_editor::Client::new(socket, Arc::clone(&self.waker)).ok()
        });
    }

    fn attach_backend(&mut self, backend: TerminalBackend) {
        let (output_tx, output_rx) = sync_channel(PTY_QUEUE_CHUNKS);
        self.pty_write = std::sync::Arc::new(std::sync::Mutex::new(backend.writer));
        self.pty_master = std::sync::Arc::new(std::sync::Mutex::new(backend.master));
        self.output_rx = output_rx;
        self.deferred_output.set(None);
        self.session_disconnected = Arc::new(AtomicBool::new(false));
        self.session_status_message =
            Arc::new(Mutex::new("Session ended — input unavailable".into()));
        self.begin_local_session();
        let widget = self;
        widget.attach_native_editor(backend.native_editor);
        widget.service_lifetime = backend.service_lifetime;
        #[cfg(windows)]
        {
            // ConPTY keeps its output pipe open while the pseudoconsole exists.
            // Observe the child handle too, so exit is visible before closing
            // the terminal window. Keep a killer for normal window teardown.
            widget.child_killer = Some(backend.child.clone_killer());
            let disconnected = widget.session_disconnected.clone();
            let status = widget.session_status_message.clone();
            let waker = widget.waker.clone();
            let mut child = backend.child;
            if std::thread::Builder::new()
                .name("tilo-terminal-child-wait".into())
                .spawn(move || {
                    let message = if child.wait().is_ok() {
                        SESSION_ENDED_MESSAGE
                    } else {
                        "Session ended. The shell's exit status could not be read."
                    };
                    set_terminal_disconnected(&disconnected, &status, message);
                    request_terminal_wake(&waker);
                })
                .is_err()
            {
                if let Some(killer) = &mut widget.child_killer {
                    let _ = killer.kill();
                }
                widget.mark_disconnected(
                    "Session unavailable. The terminal could not watch the shell for exit.",
                );
            }
        }
        // Share the waker with the reader thread so fresh PTY output wakes an
        // idle host instead of waiting for a poll.
        let waker = widget.waker.clone();
        let session_disconnected = widget.session_disconnected.clone();
        let session_status_message = widget.session_status_message.clone();

        // The reader distinguishes a hosted session's start-up `EIO` from its
        // exit by the lifetime socket the supervisor closes when the guest
        // ends. A local PTY has none.
        #[cfg(unix)]
        let reader_lifetime = widget
            .service_lifetime
            .as_ref()
            .and_then(|socket| socket.clone_for_reader().ok());
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
                            "Session ended. The terminal can no longer read from the shell.",
                        );
                        request_terminal_wake(&waker);
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
                                SESSION_ENDED_MESSAGE,
                            );
                            request_terminal_wake(&waker);
                            break;
                        }
                        PtyReadDisposition::Data(n) => {
                            let Some(chunk) = buf.get(..n) else { break };
                            if output_tx.send(chunk.to_vec()).is_err() {
                                break;
                            }
                            // Wake the host so `sync` drains the PTY.
                            request_terminal_wake(&waker);
                        }
                    }
                }
            });
        if reader_result.is_err() {
            widget.mark_disconnected("Terminal unavailable — PTY reader could not start");
            request_terminal_wake(&widget.waker);
        }
    }

    /// Construct a hosted terminal using its required process service. The
    /// session opens at the first sync, on the target its record names (or
    /// the default for a new tab).
    ///
    /// # Errors
    ///
    /// Returns an error if the host has no `ush` service to give it (always,
    /// where no hosted process-service transport exists).
    pub fn spawn_hosted(
        services: crate::HostedProcessServices,
    ) -> Result<Self, crate::HostedTerminalError> {
        if !services.has_ush() {
            return Err(crate::HostedTerminalError::RequiredServiceMissing("ush"));
        }
        let mut widget = Self::without_process(ContentStore::memory());
        widget.sessions = crate::sessions::Sessions::hosted(services);
        Ok(widget)
    }

    /// Construct a live hosted tab without doing socket IO on the host.
    pub(crate) fn spawn_hosted_deferred() -> Self {
        let mut widget = Self::without_process(ContentStore::memory());
        widget.sessions = crate::sessions::Sessions::hosted_deferred();
        widget
    }

    /// A live hosted tab whose required supervisor is unavailable. Keep the
    /// startup cause visible and input refused, rather than presenting it as
    /// a capture-only terminal that was never meant to open a process.
    pub(crate) fn without_hosted_runtime(reason: String) -> Self {
        let mut widget = Self::without_process(ContentStore::memory());
        widget.sessions =
            crate::sessions::Sessions::hosted(crate::HostedProcessServices::unavailable());
        widget.show_status(&reason);
        widget.sessions.state = crate::sessions::OpenState::NoRuntime(reason);
        widget
    }

    /// The standalone terminal. Its session opens at the first sync, on the
    /// target its record names (or the default for a new tab).
    ///
    /// # Errors
    /// Infallible today; kept fallible for hosts that construct it eagerly.
    pub fn spawn_default() -> anyhow::Result<Self> {
        let mut widget = Self::without_process(ContentStore::memory());
        widget.sessions = crate::sessions::Sessions::standalone();
        Ok(widget)
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
        let mut widget = Self::without_process(ContentStore::memory());
        widget.headless = true;
        Ok(widget)
    }

    /// Keep the pictures programs show in `content`: the store the host
    /// lends the window, which carries them to the terminal record's other
    /// readers. A terminal made without it keeps them in memory.
    #[must_use]
    pub fn with_content(mut self, content: ContentStore) -> Self {
        self.content = content;
        self
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

include!("native_editing_tests.rs");
