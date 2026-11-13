use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Mutex};
use taffy::{NodeId, TaffyTree};
use tilo_content::ContentStore;
use ui::ColorCompat;
use ui::collaboration::PresencePayload;
use ui::icons::Icon;
use ui::permissions::AppKind as PermAppKind;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::widget::TaffyTreeExt;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{AppModel, ModelPath};
use ui::{
    DragSelect, Granularity, clipboard_copy, clipboard_copy_primary, clipboard_paste,
    clipboard_paste_primary, extend_span_drag, line_boundaries, word_boundaries,
};
use ui::{
    Theme,
    paint::PaintCtx,
    text_render::{TextRenderer, TextStyle, TextWeight},
    tokens::TextRole,
    widget::{
        Banner, BannerContent, BannerKind, EmptyState, EmptyStateContent, FindOptions, Findable,
        OverlayScrollbar, PressOutcome, ScrollMetrics, Widget, paint_find_match,
    },
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

use crate::graphics::TerminalImagePlacement;

/// Chunks the PTY reader may queue before it stops reading: output is never dropped, memory is bounded.
const PTY_QUEUE_CHUNKS: usize = 64;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PtyReadDisposition {
    Data(usize),
    Retry,
    Closed,
}

/// A PTY master with no slave open: `EIO` on Linux, end of file on macOS. A hosted session sees it in
/// its hand-off window, so only a closed `session_open` makes it final. `EINTR` is never closure.
pub(crate) fn classify_pty_read(
    result: &std::io::Result<usize>,
    session_open: impl FnOnce() -> bool,
) -> PtyReadDisposition {
    match result {
        Ok(n) if *n > 0 => return PtyReadDisposition::Data(*n),
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            return PtyReadDisposition::Retry;
        }
        _ => {}
    }
    if tilo_process::editor::read_finds_no_slave(result) && session_open() {
        PtyReadDisposition::Retry
    } else {
        PtyReadDisposition::Closed
    }
}

fn session_ended_message() -> String {
    crate::i18n::t("terminal-session-ended")
}

/// Most queued output one frame parses; the rest waits, in order, for the next frame.
const MAX_PUMP_BYTES: usize = 256 * 1024;
pub(crate) const MAX_RESOURCE_SEND_BYTES: usize = 8 * 1024;
const MAX_VISIBLE_RESOURCE_LINKS: usize = 32;
const MAX_LINK_RESOURCE_LABEL_CHARS: usize = 120;
const MAX_FIND_MATCHES: usize = 2_048;
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

/// A cell in the buffer: `row` absolute, scrollback included, `col` a grid column. `Ord` is row then
/// column, the reading order the span walker steps in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GridPos {
    pub row: usize,
    pub col: usize,
}

/// A flow selection: the buffer from `anchor` to `cursor` in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: GridPos,
    pub cursor: GridPos,
}

impl Selection {
    pub fn ordered(&self) -> (GridPos, GridPos) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.cursor
    }
}

/// Open an OSC 8 link through the toolkit's opener. Only plain public `http(s)` targets open: never
/// `file:`, credentials, or a query that may carry a token.
pub(crate) fn open_url(url: &str) {
    if !is_public_web_link(url) {
        return;
    }
    ui::widget::open_external_url(url);
}

/// An OSC 8 target safe to publish as a resource: a small URL subset, because a redaction mistake
/// would put a secret in the payload.
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
    // An authority is required; a path cannot be mistaken for one.
    authority_and_path
        .split_once('/')
        .map_or(!authority_and_path.is_empty(), |(authority, _)| {
            !authority.is_empty()
        })
}

/// Typed resource input carries no control bytes, so `send` appends exactly one Enter.
fn is_reviewable_terminal_text(value: &str) -> bool {
    !value.chars().any(|ch| ch.is_control() && ch != '\t')
}

/// One OSC metadata field as a single-line label: control characters and over-limit values are
/// ignored, never decoded lossily.
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

/// Keep the first transport failure: a dead session cannot revive, and the status stops flapping.
fn set_terminal_disconnected(disconnected: &AtomicBool, message: &Mutex<String>, status: &str) {
    if !disconnected.swap(true, Ordering::AcqRel)
        && let Ok(mut message) = message.lock()
    {
        status.clone_into(&mut message);
    }
}

/// Wake the host after fresh bytes or a lifecycle change: EOF has no bytes but still changes what
/// shows.
pub(crate) fn request_terminal_wake(waker: &Mutex<ui::WakeHandle>) {
    if let Ok(waker) = waker.lock() {
        waker.wake();
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CellColor {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent SGR style bit; grouping them would not change what \
              they mean"
)]
pub struct Attr {
    pub fg: CellColor,
    pub bg: CellColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub dim: bool,
    /// SGR 9 — crossed-out (strikethrough).
    pub strike: bool,
    pub is_wide_cont: bool,
    /// OSC 8 hyperlink id; `0` is no link, other values index the widget's link registry.
    pub link_id: u16,
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

#[derive(Clone, Copy, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent DEC private mode (CSI ? Ps h/l), read separately; \
              grouping them would not change what they mean"
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
    /// The active xterm mouse tracking mode: 0, 1000 (buttons), 1002 (button-motion) or 1003 (all
    /// motion).
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

/// The open context menu's row meanings by index; a row the pane left out has none.
#[derive(Default)]
struct MenuRows {
    paste: Option<usize>,
    find: Option<usize>,
    copy_on_select: Option<usize>,
    bell: Option<usize>,
    osc52: Option<usize>,
    effects: HashMap<usize, crate::resource::TerminalEffect>,
}

/// A terminal emulator as a `ui` widget: a process behind a PTY, its parser, and the cell grid the
/// session record shares. A host gives it a region with [`place_at`](Self::place_at) and pumps it
/// with [`Widget::sync`].
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent piece of terminal/selection state \
              (force_resync, pending_wrap, alt_active, select_to_copy, …) read and \
              set on its own; grouping them would not change what they mean"
)]
pub struct TerminalWidget {
    rect: Rect,
    /// Local input or output invalidates deferred speech even at the same caret; exhaustion fails closed.
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
    /// The one reader chunk that did not fit the last pump; the next frame resumes there.
    deferred_output: std::cell::Cell<Option<Vec<u8>>>,
    /// Set for good when the PTY closes, its reader fails or a write fails: a terminal is not revived.
    session_disconnected: Arc<AtomicBool>,
    #[cfg(windows)]
    child_killer: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    session_status_message: Arc<Mutex<String>>,
    grid: Vec<String>,
    grid_attrs: Vec<Vec<Attr>>,
    cursor_x: usize,
    cursor_y: usize,
    width: usize,
    height: usize,

    char_w: f64,
    char_h: f64,
    forced_cell: bool,
    /// The theme's `Code` style, cached on every layout and draw: `measure` runs first and sees no theme.
    cell_style: Option<TextStyle>,
    /// The theme's terminal palette, its default background set to the window's background.
    palette: ui::tokens::TerminalPalette,

    /// The process's output and the screen it shows: live, never in the session record.
    emulator: crate::emulator::Emulator,
    mirrored: Vec<std::rc::Rc<crate::screen::Line>>,
    emulating: bool,
    pending_actions: Vec<ui::tilo_resource::Invoke<crate::resource::Terminal>>,
    recorded_target: Option<crate::target::SessionTarget>,
    local_session: Option<String>,
    recorded_relay_session: Option<String>,
    /// Who uses this terminal and the device it runs on, when its host says; `None` makes every session
    /// this terminal's own.
    here: Option<Here>,
    runs_elsewhere: bool,

    alt_active: bool,

    modes: TermModes,

    /// Rows scrolled up from the live bottom: 0 is live, and fresh output or a keystroke snaps back.
    scroll_offset: usize,
    wheel_remainder_px: f64,
    view: ScrollbackView,
    scrollbar: OverlayScrollbar,

    conventions: ui::PlatformConventions,
    /// Find matches painted over their cells, never in the record: a find term and its output can be
    /// sensitive.
    find_matches: Vec<FindMatch>,
    find_current: usize,
    find_request: bool,
    content_revision: u64,
    chrome_top_band: f64,
    banner_rect: Rect,
    headless: bool,
    respawn: Option<CommandBuilder>,

    /// The cursor's colour at the last `draw`, for tests.
    last_cursor_color: Color,
    last_fg_default: Color,
    last_bg_default: Color,

    /// The selection, in buffer-absolute rows and grid columns.
    selection: Option<Selection>,
    drag_select: DragSelect,
    drag_granularity: Granularity,
    drag_anchor_span: Option<(GridPos, GridPos)>,
    /// Finishing a selection also copies it to the system clipboard.
    select_to_copy: bool,
    /// Whether a program may write the clipboard through OSC 52. Off by default: anything printing to
    /// the PTY could replace what the person pastes next.
    allow_osc52_clipboard: bool,
    bell: crate::preferences::Bell,
    bell_at: Option<ui::time::Instant>,
    preferences: crate::preferences::Preferences,
    /// The right-click menu, the toolkit's `ContextMenu`; its callbacks cannot reach the PTY, so rows
    /// dispatch here.
    context_menu: ContextMenu,
    menu_rows: MenuRows,
    /// The pointer's position at the last `CursorMoved`: a press arrives without one.
    last_cursor_pos: Point,
    /// The scrollback offset at the last `draw`, for mapping pointer y to viewport rows.
    viewport_start_y: usize,

    /// The grid's rect: the widget rect inset by the body padding, computed once by
    /// [`grid_content_rect`](Self::grid_content_rect) and used by `set_geometry`, `draw` and hit-testing.
    grid_rect: Rect,
    /// The body padding at the last `draw`, for `set_geometry` and hit-testing, which see no theme.
    chrome_body_pad: f64,
    /// Latched once any output arrived: the empty-state watermark shows only before that.
    ever_nonblank: bool,

    /// Window title from OSC 0/2, in the record so collaborators see it.
    title: String,
    links: HashMap<u16, String>,
    content: ContentStore,
    image_placements: Vec<TerminalImagePlacement>,
    /// Images decode locally from content addresses; the record holds no pixels.
    images: crate::image_loads::ImageLoads,

    /// Scopes this pane's presence, so a peer's caret is attributed to this pane.
    pane_id: String,
    /// The record's dispatcher: the framework's, a parent's, or none (then nothing is recorded).
    dispatch: Option<ui::framework::Dispatcher<crate::resource::Terminal>>,

    /// Wakes the host when the reader has output, so an idle terminal does not poll.
    waker: Arc<Mutex<ui::WakeHandle>>,
    /// Buttons held while a program has mouse tracking on: local input state, never in the record.
    mouse_buttons: u8,
    focused: bool,
    sessions: crate::sessions::Sessions,
    life: WindowLife,
    chrome_header_h: f64,
    chip_rect: Rect,
    target_menu_rows: Option<Vec<Option<TargetMenuRow>>>,
    environments_panel: EnvironmentsPanel,
    runs: RunState,
    /// The last copy of terminal output, whose outcome can arrive later.
    copy: ui::CopyWatch,

    dirty: bool,
    run_action: Option<ui::tilo_actions::ActionAddress>,
}

struct Here {
    device: String,
    me: String,
}

struct RunHistory {
    record: ui::framework::PersonRecord<crate::history::CommandHistory>,
    session: String,
    me: String,
}

/// Where this terminal is with its process's command runs (docs/app-model/ush.md, "Command runs").
#[derive(Default)]
struct RunState {
    marks: Option<crate::runs::Marks>,
    current: crate::runs::Runs,
    queue: crate::runs::RunQueue,
    waiting: std::collections::VecDeque<crate::runs::Asked>,
    typed: std::collections::VecDeque<crate::runs::Asked>,
    running: Option<crate::runs::Asked>,
    ended: Vec<(crate::runs::FinishedRun, Option<crate::runs::Asked>)>,
    at_prompt: bool,
    history: Option<RunHistory>,
    session: Option<std::rc::Rc<crate::resource::Terminal>>,
    held: Option<Held>,
}

#[derive(Clone, Debug)]
pub(crate) struct Held {
    pub(crate) refusal: ui::tilo_actions::remote::host_account::Refusal,
    /// An agent asked for the run: the shell does not wait, the agent has
    /// the refusal, and the prompt is the person's to act on or dismiss.
    pub(crate) agent: bool,
}

impl TerminalWidget {
    fn draw_bell_flash(&mut self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let flash = self.bell_flash(theme.motion());
        if flash <= 0.0 {
            self.bell_at = None;
            return;
        }
        let wash = theme
            .tokens
            .colors
            .semantic
            .warning
            // ratchet-allow(literal_alpha): how strongly the grid's content is washed at the bell's peak
            .with_alpha_factor(flash * 0.22);
        // ratchet-allow(raw_paint): the bell washes the grid's content, which the terminal paints
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            wash,
            None,
            &self.rect,
        );
    }

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

    pub fn new(
        writer: Arc<Mutex<Box<dyn Write + Send>>>,
        master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
        output_rx: Receiver<Vec<u8>>,
        content: ContentStore,
    ) -> Self {
        let (width, height) = (80, 24);

        Self {
            dirty: true,
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
            session_status_message: Arc::new(Mutex::new(session_ended_message())),
            grid: vec![" ".repeat(width); height],
            grid_attrs: vec![vec![Attr::default(); width]; height],
            cursor_x: 0,
            cursor_y: 0,
            width,
            height,
            char_w: 9.0,
            char_h: 18.0,
            forced_cell: false,
            cell_style: None,
            palette: ui::tokens::TerminalPalette::dark(),
            emulator: crate::emulator::Emulator::new(width, height),
            mirrored: Vec::new(),
            emulating: false,
            pending_actions: Vec::new(),
            recorded_target: None,
            local_session: None,
            recorded_relay_session: None,
            here: None,
            runs_elsewhere: false,
            alt_active: false,
            modes: TermModes::default(),
            scroll_offset: 0,
            wheel_remainder_px: 0.0,
            view: ScrollbackView::default(),
            scrollbar: OverlayScrollbar::vertical(),
            conventions: ui::PlatformConventions::default(),
            find_matches: Vec::new(),
            find_current: 0,
            find_request: false,
            content_revision: 0,
            chrome_top_band: 0.0,
            banner_rect: Rect::ZERO,
            headless: false,
            respawn: None,
            last_cursor_color: Color::TRANSPARENT,
            last_fg_default: Color::TRANSPARENT,
            last_bg_default: Color::TRANSPARENT,
            selection: None,
            drag_select: DragSelect::default(),
            drag_granularity: Granularity::Char,
            drag_anchor_span: None,
            select_to_copy: false,
            allow_osc52_clipboard: false,
            bell: crate::preferences::Bell::default(),
            bell_at: None,
            preferences: crate::preferences::Preferences::none(),
            context_menu: ContextMenu::new(),
            menu_rows: MenuRows::default(),
            last_cursor_pos: Point::ZERO,
            viewport_start_y: 0,
            grid_rect: Rect::ZERO,
            chrome_body_pad: 12.0,
            ever_nonblank: false,
            title: "Terminal".to_string(),
            links: HashMap::new(),
            image_placements: Vec::new(),
            images: crate::image_loads::ImageLoads::new(content.clone()),
            content,
            pane_id: uuid::Uuid::new_v4().to_string(),
            dispatch: None,
            waker: Arc::new(Mutex::new(ui::WakeHandle::new())),
            mouse_buttons: 0,
            focused: false,
            sessions: crate::sessions::Sessions::none(),
            life: WindowLife::default(),
            chrome_header_h: 32.0,
            chip_rect: Rect::ZERO,
            target_menu_rows: None,
            environments_panel: EnvironmentsPanel::default(),
            runs: RunState::default(),
            copy: ui::CopyWatch::default(),
            run_action: None,
        }
    }

    /// A new process takes the session: its screen starts blank and it claims the pane; a tab outlives
    /// its process.
    fn begin_local_session(&mut self) {
        self.local_session = Some(uuid::Uuid::new_v4().to_string());
        self.pane_id = uuid::Uuid::new_v4().to_string();
        let device = self.here_device();
        self.submit(crate::resource::Terminal::CLAIM_PANE.with((self.pane_id.clone(), device)));
        self.begin_runs();
        self.emulating = true;
        self.emulator = crate::emulator::Emulator::new(self.width, self.height);
        self.ever_nonblank = false;
        self.title = "Terminal".to_string();
        self.show_screen();
    }

    pub(crate) fn wake_through(&self, waker: ui::WakeHandle) {
        if let Ok(mut current) = self.waker.lock() {
            *current = waker;
        }
    }

    pub(crate) fn runs_on(&mut self, device: &str, me: &str) {
        self.here = Some(Here {
            device: device.to_owned(),
            me: me.to_owned(),
        });
    }

    pub(crate) fn here_device(&self) -> Option<String> {
        self.here.as_ref().map(|here| here.device.clone())
    }

    pub fn last_cursor_color(&self) -> Color {
        self.last_cursor_color
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }
    pub fn is_dragging(&self) -> bool {
        self.drag_select.is_dragging()
    }

    /// The process or its transport ended, as this peer sees it: local, not a shared claim.
    pub fn is_disconnected(&self) -> bool {
        self.session_disconnected.load(Ordering::Acquire)
    }

    // The reader channel closing says the process ended: wait for every queued chunk first.
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
        self.session_status_message
            .lock()
            .map_or_else(|_| session_ended_message(), |message| message.clone())
    }

    fn status_banner_message(&self) -> Option<String> {
        if self.closing_for_exit()
            || self.start_failure().is_some()
            || self.life.reconnect.is_some()
        {
            return None;
        }
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
            // A second refusal must not erase the first's visible refusal.
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

    #[cfg(test)]
    pub(crate) const fn disconnected_badge_label() -> &'static str {
        "Session ended"
    }
    pub fn title(&self) -> &str {
        &self.title
    }

    fn terminal_transcript_target(&self) -> String {
        format!("terminal.transcript:{}", self.pane_id)
    }

    fn terminal_prompt_target(&self) -> String {
        format!("terminal.prompt:{}", self.pane_id)
    }

    #[must_use]
    pub fn prompt_rect(&self) -> Rect {
        let row = self
            .grid
            .len()
            .saturating_sub(self.height)
            .saturating_add(self.cursor_y)
            .saturating_sub(self.viewport_start_y);
        let x = self.grid_rect.x0 + self.cursor_x as f64 * self.char_w;
        let y = self.grid_rect.y0 + row as f64 * self.char_h - self.view.sub;
        Rect::new(x, y, x + self.char_w.max(1.0), y + self.char_h.max(1.0))
    }

    pub(crate) fn set_run_action(&mut self, run: ui::tilo_actions::ActionAddress) {
        self.run_action = Some(run);
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

    pub(crate) fn link_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.visible_link_ids()
            .into_iter()
            .filter_map(|id| self.link_row_for(id))
            .collect()
    }

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
                EffectStatus::unavailable(E::INTERRUPT, code, reason),
                EffectStatus::unavailable(E::CLEAR, code, reason),
                EffectStatus::unavailable(E::PASTE, code, reason),
                EffectStatus::unavailable(E::PASTE_PRIMARY, code, reason),
            ]);
        }
        if !self.can_restart() {
            statuses.push(if self.is_disconnected() {
                EffectStatus::unavailable(
                    E::RESTART,
                    "restart_unavailable",
                    "This terminal cannot start its shell again",
                )
            } else {
                EffectStatus::unavailable(
                    E::RESTART,
                    "session_running",
                    "The shell is still running",
                )
            });
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

    pub(crate) fn read_session(&self) -> serde_json::Value {
        let (output, truncated) = crate::resource::readable_output(self.emulator.screen());
        serde_json::json!({
            "kind": <crate::resource::Terminal as ui::tilo_resource::Resource>::KIND,
            "title": self.title,
            "target": self.sessions.target.as_ref().or(self.recorded_target.as_ref()),
            "session": self.local_session,
            "relay_session": self.recorded_relay_session,
            "pane": self.pane_id,
            "alternate": self.emulator.screen().alternate,
            "cursor": { "column": self.emulator.screen().cursor.0, "row": self.emulator.screen().cursor.1 },
            "output": output,
            "output_truncated": truncated,
            "retained_lines": self.emulator.screen().active().len(),
        })
    }

    pub(crate) fn inspect_session(&self) -> serde_json::Value {
        use crate::sessions::OpenState;
        let (lifecycle, startup_reason) = match &self.sessions.state {
            OpenState::Fixed => ("fixed", None),
            OpenState::Unopened => ("unopened", None),
            OpenState::Opening => ("opening", None),
            OpenState::Open => ("open", None),
            OpenState::Joined => ("joined", None),
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
                        .with_description(crate::i18n::t("terminal-link-in-output"))
                        .with_semantic_key(self.terminal_link_resource_id(link_id))
                        .with_action_target(self.terminal_link_resource_id(link_id))
                        .with_state(AccessState::new().focusable(true))
                })
            })
            .collect()
    }

    fn target_picker_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if self.bar_shows_target() {
            return None;
        }
        Some(
            AccessNode::new(Role::Button)
                .with_name(self.chip_label()?)
                .with_semantic_key(format!("terminal.target:{}", self.pane_id))
                .with_action_target(format!("terminal.target:{}", self.pane_id))
                .with_bounds(self.chip_rect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .expanded(self.target_menu_rows.is_some()),
                ),
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
                            .with_state(AccessState::new().focusable(!disabled).disabled(disabled)),
                    )
                })
                .collect();
            AccessNode::new(Role::Menu)
                .with_hashed_key("terminal-context-menu")
                .with_state(AccessState::new().expanded(true).modal(true))
                .with_children(children)
        })
    }

    /// A visible grapheme offset as a cell: the grid counts columns, accessibility graphemes.
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

    /// The inverse: only visible endpoints are published, since the value is the visible transcript.
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

    /// A grapheme-space selection as the UTF-8 offsets `with_text_selection` carries.
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
    pub fn set_select_to_copy(&mut self, on: bool) {
        if self.select_to_copy == on {
            return;
        }
        self.select_to_copy = on;
        self.preferences.set_copy_on_select(on);
    }
    pub fn set_allow_osc52_clipboard(&mut self, on: bool) {
        if self.allow_osc52_clipboard == on {
            return;
        }
        self.allow_osc52_clipboard = on;
        self.preferences.set_allow_osc52_clipboard(on);
    }

    pub fn set_bell(&mut self, bell: crate::preferences::Bell) {
        self.bell = bell;
        self.preferences.set_bell(bell);
    }

    pub fn use_preferences(&mut self, preferences: crate::preferences::Preferences) {
        let stored = preferences.load();
        self.select_to_copy = stored.copy_on_select;
        self.allow_osc52_clipboard = stored.allow_osc52_clipboard;
        self.bell = stored.bell;
        self.preferences = preferences;
    }

    fn ring_bell(&mut self) {
        if self.bell == crate::preferences::Bell::Off {
            return;
        }
        self.bell_at = Some(ui::time::now());
    }

    #[cfg(test)]
    pub(crate) fn bell_flash_for_test(&self) -> f32 {
        self.bell_flash(ui::theme::Theme::default().motion())
    }

    #[cfg(test)]
    pub(crate) fn selects_to_copy_for_test(&self) -> bool {
        self.select_to_copy
    }

    /// How much of the bell's flash is left, `0.0..=1.0`.
    fn bell_flash(&self, motion: ui::tokens::EffectiveMotion) -> f32 {
        // A quarter second: long enough to catch the eye from another window,
        // short enough not to sit on the text.
        const FLASH: std::time::Duration = std::time::Duration::from_millis(260);
        let Some(at) = self.bell_at else {
            return 0.0;
        };
        ui::motion::Transient::opacity(
            ui::motion::Transient::CONFIRMATION,
            (at, at + FLASH),
            ui::time::now(),
            motion,
        )
    }
    pub fn set_selection_cells(&mut self, anchor: GridPos, cursor: GridPos) {
        self.selection = Some(Selection { anchor, cursor });
    }
    pub fn find_row(&self, needle: &str) -> Option<usize> {
        self.grid.iter().position(|l| l.contains(needle))
    }
    pub fn find_row_last(&self, needle: &str) -> Option<usize> {
        self.grid.iter().rposition(|l| l.contains(needle))
    }

    pub fn take_find_request(&mut self) -> bool {
        std::mem::take(&mut self.find_request)
    }

    pub(crate) fn conventions(&self) -> ui::PlatformConventions {
        self.conventions
    }

    fn scan_find_matches(&mut self, query: &str, options: FindOptions) {
        self.find_matches.clear();
        let query: Vec<char> = query.chars().take(MAX_FIND_QUERY_CHARS).collect();
        if query.is_empty() {
            return;
        }
        if options.mode == ui::widget::FindMode::Regex {
            self.scan_find_pattern(&query.iter().collect::<String>(), options);
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

    fn scan_find_pattern(&mut self, pattern: &str, options: FindOptions) {
        let Ok(pattern) = regex::RegexBuilder::new(pattern)
            .case_insensitive(!options.match_case)
            .build()
        else {
            return;
        };
        let word = |c: char| c.is_alphanumeric() || c == '_';
        'rows: for (row, line) in self.grid.iter().enumerate() {
            // Cell column at each byte where a character starts, so wide
            // glyphs line up.
            let mut column = 0usize;
            let mut columns = std::collections::BTreeMap::new();
            for (at, c) in line.char_indices() {
                columns.insert(at, column);
                column += c.width().unwrap_or(1);
            }
            columns.insert(line.len(), column);
            for hit in pattern.find_iter(line) {
                if hit.is_empty() {
                    continue;
                }
                let bounded = !options.whole_word
                    || (line
                        .get(..hit.start())
                        .and_then(|head| head.chars().next_back())
                        .is_none_or(|c| !word(c))
                        && line
                            .get(hit.end()..)
                            .and_then(|tail| tail.chars().next())
                            .is_none_or(|c| !word(c)));
                if !bounded {
                    continue;
                }
                let start_col = columns.get(&hit.start()).copied().unwrap_or(0);
                let end_col = columns.get(&hit.end()).copied().unwrap_or(start_col);
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
        self.set_scroll_offset(max_top.saturating_sub(top));
    }

    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<&str> {
        let attrs = self.grid_attrs.get(row)?;
        let link_id = attrs.get(col)?.link_id;
        if link_id == 0 {
            return None;
        }
        self.links.get(&link_id).map(String::as_str)
    }
    pub fn is_alt_screen(&self) -> bool {
        self.alt_active
    }
    pub fn modes(&self) -> TermModes {
        self.modes
    }
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }
    #[cfg(test)]
    pub fn force_geometry(&mut self, rect: Rect, char_w: f64, char_h: f64) {
        self.rect = rect;
        self.grid_rect = rect;
        self.char_w = char_w;
        self.char_h = char_h;
        self.forced_cell = true;
    }

    #[cfg(test)]
    pub(crate) fn scrollbar_opacity(&self) -> f32 {
        self.scrollbar.opacity()
    }

    #[cfg(test)]
    pub fn grid_rect(&self) -> Rect {
        self.grid_rect
    }
    #[cfg(test)]
    pub fn grid_char_h(&self) -> f64 {
        self.char_h
    }
    #[cfg(test)]
    pub fn context_menu_rect(&self) -> Rect {
        Widget::get_rect(&self.context_menu)
    }
    #[cfg(test)]
    #[cfg(test)]
    pub(crate) fn context_menu_labels(&self) -> Vec<String> {
        self.context_menu
            .items()
            .iter()
            .map(|item| item.label().unwrap_or_default().to_owned())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn context_menu_item_enabled(&self, index: usize) -> bool {
        self.context_menu
            .items()
            .get(index)
            .is_some_and(MenuItem::is_enabled)
    }

    #[cfg(test)]
    pub(crate) fn activate_menu_row_for_test(&mut self, index: usize) {
        self.activate_context_menu_item(index);
    }

    #[cfg(test)]
    pub(crate) fn mark_disconnected_for_test(&self) {
        self.mark_disconnected(&crate::i18n::t("terminal-session-input-unavailable"));
    }

    pub fn context_menu_is_open(&self) -> bool {
        self.context_menu.is_open()
    }
    #[cfg(test)]
    pub fn context_menu_find_rect(&self) -> Option<Rect> {
        self.context_menu.item_rect(self.menu_rows.find?)
    }
    #[cfg(test)]
    pub fn force_viewport_start(&mut self, start: usize) {
        self.viewport_start_y = start;
    }
    #[cfg(test)]
    pub fn force_selection(&mut self, sel: Option<Selection>) {
        self.selection = sel;
    }
    #[cfg(test)]
    pub fn force_grid(&mut self, grid: Vec<String>) {
        self.grid = grid;
    }
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
    #[cfg(test)]
    pub fn find_test_matches(&self) -> Vec<usize> {
        self.find_matches.iter().map(|found| found.row).collect()
    }
    #[cfg(test)]
    pub fn banner_test_rect(&self) -> Rect {
        self.banner_rect
    }

    #[cfg(test)]
    pub(crate) fn accept_input_for_test(&self) {
        self.session_disconnected.store(false, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn own_gesture_verdict(
        &self,
        effect: &crate::resource::TerminalEffect,
    ) -> Option<bool> {
        self.dispatch.as_ref().map(|d| d.is_own_gesture(effect))
    }

    #[cfg(any(test, debug_assertions, feature = "test-support"))]
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
    #[cfg(test)]
    pub fn cell_attr(&self, row: usize, col: usize) -> Attr {
        self.grid_attrs
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or_default()
    }
    #[cfg(test)]
    pub fn grid_row(&self, row: usize) -> String {
        self.grid.get(row).cloned().unwrap_or_default()
    }
    #[cfg(test)]
    pub fn current_row_spans(&self) -> Vec<(usize, usize, usize)> {
        self.selection
            .as_ref()
            .map(|sel| selection_row_spans(sel, self.width))
            .unwrap_or_default()
    }
    #[cfg(test)]
    pub fn test_selected_text(&self) -> Option<String> {
        self.selected_text()
    }
    pub fn last_fg_default(&self) -> Color {
        self.last_fg_default
    }
    pub fn last_bg_default(&self) -> Color {
        self.last_bg_default
    }

    pub fn grid_text(&self) -> String {
        self.grid.join("\n")
    }

    pub fn grid_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn image_placements(&self) -> &[TerminalImagePlacement] {
        &self.image_placements
    }

    #[cfg(test)]
    pub(crate) fn content_store(&self) -> &ContentStore {
        &self.content
    }

    fn current_visible_rows(&self) -> usize {
        (self.grid_rect.height() / self.char_h.max(1.0))
            .floor()
            .max(1.0) as usize
    }

    /// Refresh the body padding and the history bar's sizes from `theme`, for the paths that see no
    /// theme.
    fn refresh_chrome_metrics(&mut self, theme: &Theme) {
        let sp = &theme.tokens.spacing;
        self.chrome_body_pad = f64::from(sp.s3);
        self.scrollbar.set_theme(theme);
        self.chrome_header_h =
            ui::widget::ButtonFace::height(ui::widget::ControlSize::Small, theme)
                + f64::from(sp.s2);
    }

    /// Row height of the grid, rounded up to whole pixels and never below one.
    fn cell_h(measured_line_h: f32) -> f64 {
        f64::from(measured_line_h).ceil().max(1.0)
    }

    /// The grid's rect: the widget rect inset by the body padding, with the history bar over the right
    /// padding. The one definition of the grid geometry.
    fn grid_content_rect(&self, rect: Rect) -> Rect {
        let pad = self.chrome_body_pad;
        let x0 = rect.x0 + pad;
        let y0 = rect.y0 + self.chrome_top_band + pad;
        let x1 = (rect.x1 - pad).max(x0);
        let y1 = (rect.y1 - pad).max(y0);
        Rect::new(x0, y0, x1, y1)
    }

    fn grid_is_blank(&self) -> bool {
        self.grid.iter().all(|l| l.chars().all(char::is_whitespace))
    }

    fn max_scroll(&self) -> usize {
        self.view_rows().saturating_sub(self.current_visible_rows())
    }

    fn scrollbar_area(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.grid_rect.y0,
            self.rect.x1,
            self.grid_rect.y1,
        )
    }

    fn scrollbar_track(&self) -> Rect {
        self.scrollbar.track(self.scrollbar_area())
    }

    fn scrollback_depth(&self) -> f64 {
        self.scroll_offset as f64 * self.char_h.max(1.0) - self.view.sub
    }

    fn scrollbar_metrics(&self) -> ScrollMetrics {
        let visible = self.current_visible_rows() as f64;
        let total = self.view_rows() as f64;
        // Scrollbar offsets are measured from the top; terminal history is
        // measured back from the live bottom.
        let from_top = self.scroll_from_top() / self.char_h.max(1.0);
        ScrollMetrics::new(total, visible, from_top)
    }

    fn set_scrollbar_offset(&mut self, from_top: f64) {
        let to = from_top.clamp(0.0, self.max_scroll() as f64) * self.char_h.max(1.0);
        self.scroll_by_px(to - self.scroll_from_top());
    }

    /// Parse queued PTY bytes onto the screen; says whether anything arrived.
    fn pump(&mut self) -> bool {
        let open_activity = self.poll_session_open();
        let native_activity = self.poll_native_ime() || open_activity;
        // Drain every queued read into one buffer so a burst parses once: one performer, one cursor commit,
        // one query reply.
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
            // Output makes the session live even when the batch only moves the cursor, so the watermark never
            // covers a real prompt.
            self.ever_nonblank = true;
            self.set_scroll_offset(0);
        }
        let images = self.images.take_finished();
        activity || native_activity || images
    }

    #[cfg(test)]
    pub(crate) fn has_received_pty_output(&self) -> bool {
        self.ever_nonblank
    }

    /// The one path terminal output takes; tests and scenarios feed the same parser.
    pub fn feed(&mut self, bytes: &[u8]) {
        let Some(marks) = self.runs.marks.as_mut() else {
            self.feed_output(bytes);
            return;
        };
        for piece in marks.push(bytes) {
            if let crate::runs::Piece::Output(output) = &piece {
                self.feed_output(output);
            }
            self.follow_run(&piece);
        }
    }

    fn feed_output(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            // A redisplay must keep the acknowledged checkpoint; only real Readline state changes fence.
            self.invalidate_deferred_input();
        }
        if !self.emulating {
            self.begin_local_session();
        }

        // Any PTY bytes make the session live, even a batch that only moves the cursor.
        if !bytes.is_empty() {
            self.ever_nonblank = true;
        }

        let output = self
            .emulator
            .feed(bytes, &self.content, self.allow_osc52_clipboard);
        self.modes = self.emulator.modes();
        if !self.modes.mouse_report {
            self.mouse_buttons = 0;
        }
        for (text, primary) in &output.clipboard {
            if *primary {
                clipboard_copy_primary(text);
            } else {
                clipboard_copy(text);
            }
        }

        // Answer capability queries on the shell's stdin, the wire its keystrokes use.
        if !output.responses.is_empty() {
            self.write_input(&output.responses);
        }

        // An OSC 0/2 title change shows as the window's title.
        if let Some(t) = output.title {
            self.title = t;
        }
        // A bell rings once per feed, however many times it was rung.
        if output.bells > 0 {
            self.ring_bell();
        }
        self.show_screen();
    }

    /// Place the terminal at `rect` and reflow the grid and the PTY to match, for a host that owns it
    /// outside a layout.
    pub fn place_at(&mut self, rect: Rect) {
        self.set_geometry(rect);
    }

    fn invalidate_deferred_input(&self) {
        self.input_epoch
            .set(self.input_epoch.get().saturating_add(1));
    }

    /// Write to the PTY and report transport failure, so an action cannot report success on a dead PTY.
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
            self.mark_disconnected("Terminal input failed. Session unavailable.");
            return Err("terminal input failed; the session is now disconnected".to_string());
        }
        Ok(())
    }

    /// The same, for callers that cannot see an error; a failure still shows as disconnected.
    pub fn write_input(&self, bytes: &[u8]) {
        let _ = self.try_write_input(bytes);
    }

    /// Paste `text`, wrapped in the bracketed-paste guards when the program asked for them.
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
        self.set_scroll_offset(0);
        Ok(())
    }

    fn set_geometry(&mut self, rect: Rect) {
        self.rect = rect;
        // The grid lives in the content rect, so that rect, not the widget's, drives the cell count and
        // the PTY size.
        self.grid_rect = self.grid_content_rect(rect);
        let grid_rect = self.grid_rect;

        let new_width = (grid_rect.width() / self.char_w.max(1.0)).floor() as usize;
        let new_height = (grid_rect.height() / self.char_h.max(1.0)).floor() as usize;

        // Cap the grid's size, which bounds what a feed converts for drawing.
        let new_width = new_width.min(200);
        let new_height = new_height.min(80);
        if new_width > 0 && new_height > 0 && (new_width != self.width || new_height != self.height)
        {
            self.width = new_width;
            self.height = new_height;
            self.emulator.resize(new_width, new_height);
            self.show_screen();
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
}

impl TerminalWidget {
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
                    | E::Paste { .. }
                    | E::PastePrimary { .. }
                    | E::Interrupt
                    | E::Clear
                    | E::Restart
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

    pub(crate) fn perform(
        &mut self,
        effect: crate::resource::TerminalEffect,
    ) -> Result<(), String> {
        use crate::resource::TerminalEffect as E;
        if self.agent_host_input_refused(&effect) {
            return Err("agents may not send input to the device's own shell".into());
        }
        match effect {
            E::InspectSession | E::ReadScreen => {
                return Err(
                    "inspecting and reading the session are answered by the resource's read-only handler".into(),
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
                        "text to send exceeds the {} reviewed-command limit",
                        ui::format::memory(MAX_RESOURCE_SEND_BYTES as u64)
                    ));
                }
                if !is_reviewable_terminal_text(&text) {
                    return Err(
                        "text to send may not contain terminal control characters".to_string()
                    );
                }
                self.try_write_input(text.as_bytes())?;
            }
            E::Paste { text } | E::PastePrimary { text } => self.try_paste(&text)?,
            E::Interrupt => {
                if self.headless {
                    // A terminal with no process of its own has no shell to
                    // interrupt: the workspace's host stops the run it has
                    // there, as Ctrl+C in that session does.
                    self.submit(crate::resource::Terminal::STOP.with(()));
                } else {
                    self.try_write_input(&[0x03])?;
                }
            }
            E::Clear => self.try_write_input(&[0x0c])?,
            E::Restart => self.restart_session()?,
            E::CopySelection => {
                let text = self
                    .selected_text()
                    .filter(|text| !text.is_empty())
                    .ok_or("there is no selected terminal output to copy")?;
                let copy = ui::text_selection::try_clipboard_copy(&text);
                if !self.copy.watch(copy) {
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
                let copy = ui::text_selection::try_clipboard_copy(&visible);
                if !self.copy.watch(copy) {
                    return Err("the system clipboard did not accept terminal output".into());
                }
            }
            E::HistoryTop => self.scroll_to_top(),
            E::HistoryLive => self.scroll_to_bottom(),
            E::OpenLink { url } => {
                if safe_osc_text(url.as_bytes(), crate::emulator::MAX_OSC_LINK_BYTES).is_none()
                    || !is_public_web_link(&url)
                {
                    return Err("link target is not a safe public web URL".to_string());
                }
                open_url(&url);
            }
            E::OpenItem { run, position } => self.show_item(&run, position)?,
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

    /// Do `effect` as the person's own gesture: it reaches the process at once, or goes through the
    /// session's host when the role does not allow it.
    fn submit_effect(&mut self, effect: crate::resource::TerminalEffect) {
        if let Some(dispatch) = &self.dispatch
            && !dispatch.is_own_gesture(&effect)
        {
            dispatch.effect(&effect);
            return;
        }
        if let Err(error) = self.perform(effect) {
            tracing::warn!(%error, "terminal gesture failed");
        }
    }
}

impl TerminalWidget {
    pub fn run_through(&mut self, queue: crate::runs::RunQueue) {
        queue.set_pane(if self.emulating { &self.pane_id } else { "" });
        self.runs.queue = queue;
    }

    pub(crate) fn recent_in(&mut self, recent: crate::target::RecentTarget) {
        self.sessions.recent_in(recent);
    }

    pub(crate) fn remember_in(
        &mut self,
        record: ui::framework::PersonRecord<crate::history::CommandHistory>,
        session: String,
        me: String,
    ) {
        self.runs.history = Some(RunHistory {
            record,
            session,
            me,
        });
    }

    /// A new process: what the last one had not run settles as failed.
    fn begin_runs(&mut self) {
        let runs = &mut self.runs;
        runs.current = crate::runs::Runs::default();
        runs.at_prompt = false;
        let unanswered: Vec<_> = runs
            .waiting
            .drain(..)
            .chain(runs.typed.drain(..))
            .chain(runs.running.take())
            .collect();
        for asked in unanswered {
            fail_run(asked, "the session's process ended before the command ran");
        }
        runs.marks = None;
        runs.queue.set_pane(&self.pane_id);
    }

    fn follow_run(&mut self, piece: &crate::runs::Piece) {
        use crate::runs::{Mark, Piece};
        let runs = &mut self.runs;
        match piece {
            Piece::Mark(Mark::Prompt) => {
                runs.at_prompt = true;
                if let Some(asked) = runs.typed.pop_front() {
                    fail_run(asked, "the shell did not run the command");
                }
            }
            Piece::Mark(Mark::Started { command, .. }) => {
                runs.at_prompt = false;
                // The typed line matches the oldest waiting request when the shell stops showing its prompt;
                // anything else the shell runs is nobody's request.
                let typed = if runs
                    .typed
                    .front()
                    .is_some_and(|asked| asked.command.trim() == command.trim())
                {
                    runs.typed.pop_front()
                } else {
                    None
                };
                runs.running = typed;
            }
            Piece::Mark(Mark::History(ask)) => {
                self.answer_history(ask);
                return;
            }
            Piece::Mark(Mark::Held { agent, refusal }) => {
                runs.held = Some(Held {
                    refusal: (**refusal).clone(),
                    agent: *agent,
                });
                self.dirty = true;
            }
            Piece::Mark(Mark::Released) => {
                // An agent's prompt stays for the person to act on.
                if runs.held.as_ref().is_some_and(|held| !held.agent) {
                    runs.held = None;
                    self.dirty = true;
                }
            }
            Piece::Mark(Mark::Ended { .. }) => {
                if runs.held.as_ref().is_some_and(|held| !held.agent) {
                    runs.held = None;
                    self.dirty = true;
                }
            }
            Piece::Mark(
                Mark::JobStarted { .. } | Mark::JobOutput { .. } | Mark::JobEnded { .. },
            )
            | Piece::Output(_) => {}
        }
        if let Some(run) = runs
            .current
            .follow(piece, ui::tilo_resource::origin_now_ms())
        {
            // A background job's run is nobody's request: the line that
            // started it was.
            let asked = if run.job.is_some() {
                None
            } else {
                runs.running.take()
            };
            runs.ended.push((run, asked));
        }
    }

    /// Answer the shell's history request with the newest lines past `skip`
    /// (docs/app-model/ush.md, "The session's shell").
    fn answer_history(&self, ask: &crate::runs::HistoryAsk) {
        use base64::Engine as _;
        const ANSWER_BYTES: usize = 3072;
        let Some(key) = self.runs.marks.as_ref().map(|m| m.key().to_owned()) else {
            return;
        };
        let b64 = base64::engine::general_purpose::STANDARD;
        let limit = ask.limit.clamp(1, 256);
        let (entries, end) = self.runs.history.as_ref().map_or_else(
            || (Vec::new(), true),
            |history| {
                history.record.with(|history| {
                    let mut seen = std::collections::HashSet::new();
                    let mut lines = history
                        .matching(&ask.matching)
                        .map(|run| run.command.0.as_str())
                        .filter(|line| seen.insert(*line))
                        .skip(ask.skip);
                    let mut entries = Vec::new();
                    let mut bytes = 0;
                    let mut end = true;
                    for line in lines.by_ref() {
                        let encoded = b64.encode(line);
                        if !entries.is_empty()
                            && (entries.len() >= limit || bytes + encoded.len() + 1 > ANSWER_BYTES)
                        {
                            end = false;
                            break;
                        }
                        bytes += encoded.len() + 1;
                        entries.push(encoded);
                    }
                    (entries, end)
                })
            },
        );
        let answer = format!(
            "\x1b]133;H;k={key};s={};q={};e={}{}\x07",
            ask.skip,
            b64.encode(&ask.matching),
            entries.join(","),
            if end { ";end=1" } else { "" }
        );
        if let Err(error) = self.try_write_input(answer.as_bytes()) {
            tracing::warn!(%error, "terminal could not answer the shell's history");
        }
    }

    fn type_asked_run(&mut self) {
        for asked in self.runs.queue.take() {
            // The same request handed over twice runs once and answers the newest asker.
            let runs = &mut self.runs;
            match runs
                .waiting
                .iter_mut()
                .chain(runs.typed.iter_mut())
                .chain(runs.running.as_mut())
                .find(|known| known.request == asked.request)
            {
                Some(known) => known.reply = asked.reply,
                None => runs.waiting.push_back(asked),
            }
        }
        if self.runs.waiting.is_empty() {
            return;
        }
        if self.runs.marks.is_none() {
            for asked in self.runs.waiting.drain(..) {
                fail_run(asked, "this session's shell does not mark its runs");
            }
            return;
        }
        if self
            .sessions
            .target
            .as_ref()
            .is_some_and(crate::target::SessionTarget::is_host)
        {
            for asked in self.runs.waiting.drain(..) {
                fail_run(
                    asked,
                    "commands are not run in the device's own shell for anyone",
                );
            }
            return;
        }
        if !self.runs.at_prompt || !self.runs.typed.is_empty() {
            return;
        }
        let Some(asked) = self.runs.waiting.pop_front() else {
            return;
        };
        // A shell that reads Terminal's marks takes the line whole, in place
        // of whatever was typed, and knows who asked.
        let line = if let Some(marks) = self.runs.marks.as_ref().filter(|m| m.reads_input()) {
            use base64::Engine as _;
            format!(
                "\x1b]133;R;k={};cmd={}{}\x07",
                marks.key(),
                base64::engine::general_purpose::STANDARD.encode(&asked.command),
                if asked.agent { ";agent=1" } else { "" }
            )
            .into_bytes()
        } else {
            // Ctrl-U first: what was half typed at the prompt does not
            // become part of the command.
            let mut line = vec![0x15];
            line.extend_from_slice(asked.command.as_bytes());
            line.push(b'\r');
            line
        };
        match self.try_write_input(&line) {
            Ok(()) => {
                self.runs.at_prompt = false;
                self.runs.typed.push_back(asked);
            }
            Err(error) => fail_run(asked, &error),
        }
    }

    fn record_runs(
        &mut self,
        dispatch: &ui::framework::Dispatcher<crate::resource::Terminal>,
        edits: &mut ui::widget::Edits<'_>,
    ) {
        for (run, asked) in std::mem::take(&mut self.runs.ended) {
            // Runs name their output by the content store's reference: a record's content travels sealed.
            let output = if run.output.is_empty() {
                None
            } else {
                match self.content.put(&run.output) {
                    Ok(reference) => {
                        Some(ui::tilo_resource::Bytes::kept_as(&run.output, &reference))
                    }
                    Err(error) => {
                        tracing::warn!(%error, "terminal could not keep a run's output");
                        None
                    }
                }
            };
            let request = asked.as_ref().map(|asked| asked.request.clone());
            // A request's run takes the request's id, so the answer names it.
            let taken = request.as_ref().is_some_and(|request| {
                self.runs
                    .session
                    .as_ref()
                    .is_some_and(|session| session.runs.contains(request))
            });
            let id = match request.clone() {
                Some(request) if !taken => request,
                _ => ui::tilo_resource::Id::from_raw(uuid::Uuid::new_v4().simple().to_string()),
            };
            let finished = crate::resource::Finished {
                tool: crate::parse::tool_of(&run.command).to_owned(),
                undoable: crate::undoability::of(&run.command) == Some(true),
                items: crate::parse::items(&run.command, &run.output),
                output,
                command: run.command,
                cwd: run.cwd,
                started_ms: run.started_ms,
                ended_ms: run.ended_ms,
                exit: run.exit,
                truncated: run.truncated,
                request: request.clone(),
                run: id.clone(),
            };
            let ran = crate::history::Ran {
                command: finished.command.clone(),
                cwd: finished.cwd.clone(),
                environment: self
                    .sessions
                    .target
                    .as_ref()
                    .map(crate::target::SessionTarget::encode),
                exit: finished.exit,
                ended_ms: finished.ended_ms,
                actor: asked
                    .as_ref()
                    .map(|asked| asked.requested_by.clone())
                    .unwrap_or_default(),
                session: String::new(),
                run: id.clone(),
            };
            let job = run.job.is_some();
            let recorded =
                dispatch.submit_now(edits, crate::resource::Terminal::RECORD_RUN.with(finished));
            match &recorded {
                Ok(()) if job => {}
                Ok(()) => self.remember(ran),
                Err(error) => tracing::warn!(%error, "terminal session record refused a run"),
            }
            if let Some(asked) = asked {
                let answer = crate::resource::RunAnswer {
                    run: recorded.is_ok().then_some(asked.request.clone()),
                    exit: Some(run.exit),
                    refusal: run.refusal.as_deref().cloned(),
                };
                asked.reply.finish(Ok(ui::tilo_actions::ActionOutput {
                    value: serde_json::to_value(answer).ok(),
                    resources: Vec::new(),
                }));
            }
        }
    }

    fn show_item(&self, run: &str, position: u32) -> Result<(), String> {
        let session = self
            .runs
            .session
            .as_ref()
            .ok_or("the session's runs are not here")?;
        let run = session
            .runs
            .get(&ui::tilo_resource::Id::from_raw(run))
            .ok_or("no such run")?;
        let position = usize::try_from(position).unwrap_or(usize::MAX);
        match crate::parse::shown(&run.cwd.0, &run.items, position)? {
            crate::parse::Shown::Page(url) => {
                if !is_public_web_link(&url) {
                    return Err("the link is not a public web page".into());
                }
                open_url(&url);
            }
            crate::parse::Shown::File { path, .. } => {
                if !path.is_file() {
                    return Err(format!("{} is not a file on this device", path.display()));
                }
                ui::widget::open_external_url(&format!("file://{}", path.display()));
            }
        }
        Ok(())
    }

    fn remember(&self, mut ran: crate::history::Ran) {
        let Some(history) = &self.runs.history else {
            return;
        };
        if ran.actor.is_empty() {
            ran.actor.clone_from(&history.me);
        }
        ran.session.clone_from(&history.session);
        let remember = crate::history::CommandHistory::REMEMBER.with(ran);
        if let Err(error) = history.record.invoke(&remember) {
            tracing::warn!(%error, "the command history refused a run");
        }
    }

    fn sync_document(&mut self, _state: &AppModel, _root: &ModelPath) {
        self.dirty = true;
        if self.sessions.is_live() {
            self.ensure_session();
        }
        self.poll_folder_picker();
        // Pump here too: a host that never calls `update` still advances the terminal.
        self.pump();
        self.type_asked_run();
        self.show_setup_failure();
        self.begin_reconnect_if_lost();
        self.reconnect_when_due();
        self.sync_window_bar();
        self.close_if_exited();
    }

    fn record(&mut self, edits: &mut ui::widget::Edits<'_>) {
        let Some(dispatch) = self.dispatch.clone() else {
            self.pending_actions.clear();
            self.runs.ended.clear();
            return;
        };
        for action in std::mem::take(&mut self.pending_actions) {
            if let Err(error) = dispatch.submit_now(edits, action) {
                tracing::warn!(%error, "terminal session record refused a change");
            }
        }
        self.record_runs(&dispatch, edits);
    }

    fn submit(&mut self, action: ui::tilo_resource::Invoke<crate::resource::Terminal>) {
        if self.dispatch.is_some() {
            self.pending_actions.push(action);
        }
    }

    pub(crate) fn present_session(&mut self, state: &std::rc::Rc<crate::resource::Terminal>) {
        self.runs.session = Some(std::rc::Rc::clone(state));
        self.recorded_target = state.session_target();
        self.recorded_relay_session = state.relay().map(str::to_owned);
        self.runs_elsewhere = self
            .here
            .as_ref()
            .is_some_and(|here| state.runs_elsewhere(&here.device, &here.me));
        if !state.pane_id().is_empty() {
            state.pane_id().clone_into(&mut self.pane_id);
        }
    }

    /// Rebuild what is drawn from `screen`; only rows whose line changed are converted again.
    fn show_screen(&mut self) {
        use std::collections::hash_map::Entry;
        let screen = self.emulator.screen();
        let lines = screen.active().clone();
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
                let (text, attrs) = previous
                    .remove(&std::rc::Rc::as_ptr(line))
                    .unwrap_or_else(|| (line.text.clone(), line.attrs.clone()));
                self.grid.push(text);
                self.grid_attrs.push(attrs);
            }
            self.mirrored = lines;
        }
        self.alt_active = screen.alternate;
        (self.cursor_x, self.cursor_y) = self.emulator.cursor();
        self.links = screen
            .links
            .iter()
            .map(|(id, url)| (*id, url.clone()))
            .collect();
        self.image_placements.clone_from(&screen.images);
        if self.grid.iter().any(|row| !row.trim().is_empty()) {
            self.ever_nonblank = true;
        }
        self.content_revision = self.content_revision.wrapping_add(1);
    }

    #[cfg(feature = "samples")]
    pub(crate) fn stage_scenario(&mut self, name: &str) {
        self.session_disconnected.store(false, Ordering::Release);
        if let Some((target, environments)) = crate::scenarios::target(name) {
            self.show_target(target, environments);
        }
        if name == "held" {
            let key = crate::runs::new_key();
            self.runs.marks = Some(crate::runs::Marks::new(key.clone()));
            self.feed(&crate::scenarios::held(&key));
        } else {
            self.feed(&crate::scenarios::output(name));
        }
        if name == "ended" {
            self.mark_disconnected(&crate::i18n::t("terminal-session-input-unavailable"));
        }
        self.stage_view(name);
    }

    #[cfg(feature = "samples")]
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

/// Find in the retained scrollback; the matches live here and the bar counts them.
impl Findable for TerminalWidget {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<ui::widget::FindMatch> {
        self.find_current = 0;
        self.scan_find_matches(query, options);
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

    fn supports_regex(&self) -> bool {
        true
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
        dyn ui::undo::LocalUndo,
        dyn ui::widget::controls::ActionControls
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
        // `measure` sees no theme: read the style cached by the last layout or draw.
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
        self.dirty = true;
        if self.copy.failed() {
            tracing::warn!("the system clipboard did not accept terminal output");
        }
        self.sync_document(state, root);
    }

    fn update(&mut self, _state: &AppModel, _root: &ModelPath) -> bool {
        self.advance_glide();
        self.pump()
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, _root: &automerge::ObjId) {
        self.record(edits);
    }

    /// Whether queued output needs another frame; an embedded terminal is pumped by its host.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        match (
            self.reconnect_deadline(),
            self.scrollbar.next_wake_deadline(),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn needs_animation_frame(&self) -> bool {
        // A bounded pump leaves output queued: keep frames coming until it drains.
        let pending = self
            .deferred_output
            .take()
            .or_else(|| self.output_rx.try_recv().ok());
        let queued = pending.is_some();
        self.deferred_output.set(pending);
        // A glide, or the give past an end springing back.
        let gliding = self.view.glide.is_active()
            || self.view.glide.give_needs_frame(ui::motion::frame_now());
        queued
            || gliding
            || self.scrollbar.needs_animation_frame()
            || self.folder_picker_pending()
            || self.bell_flash(ui::theme::Theme::default().motion()) > 0.0
    }

    /// The rows that changed, came into view or went, and a moved scroll as a copy of its rows.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.view.overlay_drawn || self.view.overlay_presented {
            regions.push(self.rect);
            return;
        }
        self.view.shown.damage_within(self, self.rect, regions);
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.view.shown.present();
        self.view.overlay_presented = self.view.overlay_drawn;
    }

    /// Keep the terminal's scene: the next frame's scroll does not know what is on screen.
    fn release_scene_cache(&mut self) {
        self.view.shown.forget();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Seed the measurement from the theme: at a large text scale an 18px row height overflowed the PTY.
        self.cell_style = Some(theme.text(TextRole::Code));
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
        self.set_geometry(rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        // Every visual parameter comes from the theme. The grid is document
        // content in the `Code` role; the window ground under it is the
        // framework's, so nothing here fills the widget.
        let cell_style = theme.text(TextRole::Code);
        self.palette = Self::grid_palette(theme);
        self.refresh_chrome_metrics(theme);

        // `char_w` must be the advance of the drawn style, or colour runs smear over each other.
        let (w, h) = text_renderer.measure_style(" ", None, &cell_style);
        self.char_w = f64::from(w).max(1.0);
        self.char_h = Self::cell_h(h);
        self.cell_style = Some(cell_style.clone());

        // The find bar and the session banner stand above the grid; lay them
        // out first so the grid and the PTY size are derived under them.
        self.layout_top_bands(theme, text_renderer);

        // Reconcile the PTY geometry once the real face is known: at 200% a narrower fallback advance would
        // clip the process's right side.
        self.set_geometry(self.rect);
        let grid_rect = self.grid_rect;

        self.view.motion = theme.motion();
        let visible_rows = (grid_rect.height() / self.char_h).floor() as usize;
        let max_top = self.view_rows().saturating_sub(visible_rows);
        // Alt screen has no scrollback; clamp the offset for the main buffer.
        let scroll = if self.alt_active {
            0
        } else {
            self.scroll_offset.min(max_top)
        };
        if scroll != self.scroll_offset || scroll == 0 {
            self.view.sub = 0.0;
        }
        self.scroll_offset = scroll;
        let start_y = max_top.saturating_sub(scroll);

        // Cached for pointer hit-tests, which map logical y to a viewport row.
        self.viewport_start_y = start_y;

        // The colour caches update every frame, whatever is painted.
        let cursor_color = cursor_from_palette(&self.palette);
        self.last_cursor_color = cursor_color;
        self.last_fg_default = self.palette.fg_default;
        self.last_bg_default = self.palette.bg_default;

        if !self.grid_is_blank() {
            self.ever_nonblank = true;
        }
        // What does not scroll: the header and bands above the grid, or what replaces it.
        self.life.restart_action.set(Rect::ZERO);
        let mut outside = Scene::new();
        self.draw_target_header(&mut outside, transform, text_renderer, theme);
        // A shell that could not start says so, with a retry, in place of
        // the grid.
        if let Some(reason) = self.start_failure().map(str::to_owned) {
            self.draw_start_failure(&mut outside, transform, text_renderer, theme, &reason);
            self.draw_top_bands(&mut outside, transform, text_renderer, theme);
            scene.append(&outside, None);
            self.view.shown.forget();
            return;
        }
        self.life.retry_rect = Rect::ZERO;
        if self.shows_empty_state() {
            self.draw_empty_state(&mut outside, transform, text_renderer, theme);
            self.draw_top_bands(&mut outside, transform, text_renderer, theme);
            scene.append(&outside, None);
            self.view.shown.forget();
            return;
        }
        self.draw_top_bands(&mut outside, transform, text_renderer, theme);
        self.draw_grid(scene, transform, text_renderer, theme, presence, outside);
        self.draw_bell_flash(scene, transform, theme);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let before = scene.encoding().draw_tags.len();
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
        self.view.overlay_drawn = scene.encoding().draw_tags.len() != before;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.dirty = true;
        // Every event goes to the menu while it is open; keys do not fall through to the PTY.
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
            // Ctrl+scroll zooms the window, and with it the grid's font.
            WindowEvent::MouseWheel { delta, phase, .. }
                if !ui::content_zoom::is_zoom_scroll(event, ctx.modifiers) =>
            {
                self.handle_wheel_event(
                    delta,
                    *phase,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.handled,
                );
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved_event(
                    *position,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.handled,
                );
            }
            WindowEvent::CursorLeft { .. } => {
                self.scrollbar.pointer_left();
            }
            WindowEvent::MouseInput {
                state: element_state,
                button,
                ..
            } => {
                // A press on the grid gives it the keyboard: typing already reached the session, but Tab did not.
                if *element_state == ElementState::Pressed
                    && self.grid_rect.contains(self.last_cursor_pos)
                {
                    *ctx.focus_target = Some(ctx.node_id);
                }
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
                self.set_scroll_offset(0);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
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
        self.focused.then(|| self.prompt_rect())
    }

    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        (self.focused && !self.context_menu.is_open() && self.input_unavailable().is_none())
            .then_some(ui::ime::InputContent::TERMINAL)
    }

    /// A native editor prompt offers acknowledged edits, which lets dictation write to a terminal; a bare
    /// PTY offers none.
    fn ime_requires_deferred_ack(&self) -> bool {
        self.ime_content().is_some()
            && !self.native_read_only
            && !self.alt_active
            && self
                .native_editor
                .as_ref()
                .and_then(crate::native_editor::Client::snapshot)
                .is_some()
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
        self.ime_content()?;
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
        let mut description = crate::i18n::t("terminal-a11y-description");
        if graphics > 0 {
            description.push(' ');
            description.push_str(&crate::i18n::t_args(
                "terminal-a11y-graphics",
                &[("count", graphics.to_string().into())],
            ));
        }
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
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.focused)
                    .multiline(true)
                    .read_only(true)
                    .context_menu(true),
            );
        if let Some(picker) = self.target_picker_accessibility() {
            transcript.children.push(picker);
        }
        // The prompt: the line input goes in, where a record's run is shown landing. It carries no verb:
        // typing reaches the process as keys.
        if self.run_action.is_some() {
            transcript.children.push(
                AccessNode::new(Role::TextInput)
                    .with_name(t("terminal-prompt"))
                    .with_semantic_key(self.terminal_prompt_target())
                    .with_bounds(self.prompt_rect()),
            );
        }
        if let Some(failure) = self.start_failure_accessibility() {
            transcript.children.push(failure);
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
        if self.can_restart() {
            transcript.children.push(
                AccessNode::new(Role::Button)
                    .with_hashed_key("terminal-restart")
                    .with_name(t("terminal-restart"))
                    .with_action_target(self.restart_target())
                    .with_bounds(self.life.restart_action.get())
                    .with_state(AccessState::new().focusable(true)),
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
        if target == format!("terminal.retry:{}", self.pane_id) {
            return self.retry_session();
        }
        if target == self.restart_target() {
            if !self.can_restart() {
                return false;
            }
            self.submit_effect(crate::resource::TerminalEffect::Restart);
            return true;
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
}

impl ui::widget::controls::ActionControls for TerminalWidget {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let Some(run) = &self.run_action else {
            return Vec::new();
        };
        vec![ui::widget::ControlBindingMetadata {
            control_id: self.terminal_prompt_target(),
            kind: ui::widget::ControlBindingKind::Action(run.clone()),
        }]
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
    /// Scope this terminal's presence to a shared pane id, so both peers file their caret under the
    /// same key.
    fn presence_key(&self) -> Option<PresenceKey> {
        Some(PresenceKey::App {
            app: PermAppKind::Terminal,
            sub: self.pane_id.clone(),
        })
    }
    /// Broadcast the view state to peers: the selection when there is one, else the cursor.
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
    fn draw_peer_marks(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(key) = ui::presence::capability::presence_key(self) else {
            return;
        };
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

    /// Record the session through `dispatcher`: the framework's, or a parent's when this terminal is
    /// nested in another resource.
    pub fn bind(&mut self, dispatcher: ui::framework::Dispatcher<crate::resource::Terminal>) {
        self.dispatch = Some(dispatcher);
    }
}

impl TerminalWidget {
    fn handle_cursor_moved_event(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_cursor_pos = logical;
        let area = self.scrollbar_area();
        let metrics = self.scrollbar_metrics();
        if self.scrollbar.is_dragging() {
            if let Some(offset) = self.scrollbar.drag_to(area, metrics, logical) {
                self.set_scrollbar_offset(offset);
                *handled = true;
            }
            return;
        }
        if self.scrollbar_visible(self.current_visible_rows()) {
            self.scrollbar.update_pointer(area, metrics, logical);
        } else {
            self.scrollbar.pointer_left();
        }
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
                let area = self.scrollbar_area();
                let metrics = self.scrollbar_metrics();
                match self.scrollbar.press(area, metrics, self.last_cursor_pos) {
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

    fn handle_left_click_event(
        &mut self,
        element_state: ElementState,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        // Application mouse reporting was handled above, so reaching here means reporting is off or Shift
        // asked for a local selection.
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

    fn handle_middle_click_event(&mut self, handled: &mut bool) {
        // X11/Wayland primary-selection paste, then snap to the live bottom.
        if self.grid_rect.contains(self.last_cursor_pos) {
            if let Some(text) = clipboard_paste_primary() {
                self.submit_effect(crate::resource::TerminalEffect::PastePrimary { text });
            }
            *handled = true;
        }
    }

    fn handle_right_click_event(&mut self, handled: &mut bool) {
        // Right-click opens the Copy/Paste menu; Copy only when something is selected.
        if self.grid_rect.contains(self.last_cursor_pos) {
            self.open_context_menu_at_cursor();
            *handled = true;
        }
    }

    fn handle_key_press_event(
        &mut self,
        logical_key: &Key,
        physical_key: PhysicalKey,
        text: Option<&str>,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        // Escape dismisses an open context menu before the app sees it.
        if self.context_menu.is_open() && matches!(logical_key, Key::Named(NamedKey::Escape)) {
            self.context_menu.close();
            *handled = true;
            return;
        }
        let mods = modifiers.state();
        // Ctrl+Plus/Minus/0 zoom the window, never bytes for the shell.
        if ui::content_zoom::ZoomStep::from_key(
            ui::platform::PlatformConventions::default(),
            mods,
            logical_key,
        )
        .is_some()
        {
            return;
        }
        // The desktop's tab chords, left unhandled: Ctrl+Tab and Ctrl+Shift+T have no bytes of their own,
        // and Ctrl+T/Ctrl+W stay the shell's.
        if matches!(
            ui::shell_tabs::chord_request(
                logical_key,
                ui::platform::PlatformConventions::default().command_modifier_down(mods),
                mods.shift_key(),
                mods.alt_key() || (mods.control_key() && mods.super_key()),
            ),
            Some(
                ui::shell_tabs::TabRequest::CycleTab { .. }
                    | ui::shell_tabs::TabRequest::ReopenClosed
            )
        ) {
            return;
        }
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
        // Shift+Insert pastes too, through the same bracketed-paste path.
        if mods.shift_key() && matches!(logical_key, Key::Named(NamedKey::Insert)) {
            if let Some(text) = clipboard_paste() {
                self.submit_effect(crate::resource::TerminalEffect::Paste { text });
            }
            *handled = true;
            return;
        }

        // Scrollback navigation needs Shift: plain Page Up/Down goes to readline and pagers.
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
            bytes = Self::map_physical_key(physical_key, modifiers);
        }
        if bytes.is_none()
            && let Some(t) = text
            && !t.chars().any(|c| c as u32 == 0x23CE || c as u32 == 0x21B5)
        {
            bytes = Some(t.as_bytes().to_vec());
        }
        if let Some(b) = bytes {
            let _ = self.try_write_input(&b);
            if b == [0x03] && self.headless {
                self.submit(crate::resource::Terminal::STOP.with(()));
            }
            self.set_scroll_offset(0);
            *handled = true;
        }
    }

    fn layout_top_bands(&mut self, theme: &Theme, tr: &mut TextRenderer) {
        let gutter = f64::from(theme.tokens.spacing.s3);
        let mut y = self.rect.y0 + self.header_h();
        self.banner_rect = Rect::ZERO;
        if let Some(banner) = self.top_banner()
            && self.rect.width() > gutter * 2.0
        {
            let width = self.rect.width() - gutter * 2.0;
            let height = Banner::height(&banner.content(), width, tr, theme);
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

    /// The bar over the grid: the reconnecting one while the tab reattaches, else the session's lifecycle
    /// line.
    fn top_banner(&self) -> Option<TopBanner> {
        if let Some(message) = self.reconnect_banner() {
            return Some(TopBanner::new(
                BannerKind::Neutral,
                message,
                Some(t("terminal-start-retry")),
            ));
        }
        let restart =
            (self.can_restart() && !self.shows_empty_state()).then(|| t("terminal-restart"));
        if let Some(message) = self.status_banner_message() {
            return Some(TopBanner::new(BannerKind::Warning, message, restart));
        }
        self.runs.held.as_ref().map(TopBanner::held)
    }

    #[cfg(test)]
    pub(crate) fn held(&self) -> Option<&Held> {
        self.runs.held.as_ref()
    }

    /// A press on the held run's prompt; says whether the event was taken.
    fn route_held(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let Some(held) = self.runs.held.clone() else {
            return false;
        };
        let at = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.last_cursor_pos,
            WindowEvent::Touch(touch) if touch.phase == winit::event::TouchPhase::Started => {
                ctx.logical(touch.location)
            }
            _ => return false,
        };
        if self.life.held_action.get().contains(at) {
            if let Some(url) = held.add_card_url() {
                ui::widget::open_external_url(url);
            }
        } else if self.life.held_secondary.get().contains(at) {
            self.dismiss_held(&held);
        } else {
            return false;
        }
        *ctx.handled = true;
        self.dirty = true;
        true
    }

    pub(crate) fn dismiss_held(&mut self, held: &Held) {
        if held.agent {
            self.runs.held = None;
        } else if let Err(error) = self.try_write_input(&[0x03]) {
            tracing::warn!(%error, "terminal could not cancel the held command");
        }
    }

    fn draw_top_bands(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.life.banner_action.set(Rect::ZERO);
        self.life.held_action.set(Rect::ZERO);
        self.life.held_secondary.set(Rect::ZERO);
        if self.banner_rect.width() > 0.0
            && let Some(banner) = self.top_banner()
        {
            let mut ctx = PaintCtx::new(scene, transform, tr, theme);
            let hit = Banner::paint(
                &mut ctx,
                self.banner_rect,
                &banner.content(),
                self.last_cursor_pos,
            );
            let action = hit.action.unwrap_or(Rect::ZERO);
            if self.life.reconnect.is_some() {
                self.life.banner_action.set(action);
            } else if banner.held {
                self.life.held_action.set(action);
                self.life
                    .held_secondary
                    .set(hit.secondary_action.unwrap_or(Rect::ZERO));
            } else if self.can_restart() {
                self.life.restart_action.set(action);
            }
        }
    }

    pub(crate) fn grid_palette(theme: &Theme) -> ui::tokens::TerminalPalette {
        theme.tokens.terminal_palette.clone()
    }

    // Decode graphics only while their rows are visible; a failed decode is retried, not latched. Images
    // are clipped to the grid.
    fn draw_terminal_images(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        grid_rect: Rect,
        start_y: usize,
        visible_rows: usize,
    ) {
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

            if let Some(image) = self.images.image(&placement, &self.waker) {
                image.paint_at(scene, transform, rect);
            } else {
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
        // Faint blends the foreground toward the background, after reverse so a dimmed reverse cell dims its
        // text too.
        if attr.dim {
            fg = fg.with_alpha_factor(0.55);
        }
        // Link cells are tinted to the palette's link hue unless the program chose a colour, and always
        // underlined.
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

    /// Rows of the buffer that carry anything: the last row with text, or the cursor's row when it sits
    /// below that.
    fn content_rows(&self) -> usize {
        let written = self
            .grid
            .iter()
            .rposition(|line| !line.trim_end().is_empty())
            .map_or(0, |i| i + 1);
        let cursor_abs = self.grid.len().saturating_sub(self.height) + self.cursor_y;
        written.max(cursor_abs + 1)
    }

    /// How many rows the view scrolls over: the buffer minus the blank rows below the last text, so a
    /// band above the grid does not hide the first lines.
    fn view_rows(&self) -> usize {
        if self.alt_active {
            return self.grid.len();
        }
        self.content_rows()
            .max(self.current_visible_rows())
            .min(self.grid.len())
    }

    /// Whether the scrollbar represents anything: the buffer must be longer than the viewport and have
    /// content past it, since a session allocates a full screen of rows up front.
    pub(crate) fn scrollbar_visible(&self, visible_rows: usize) -> bool {
        !self.alt_active && visible_rows > 0 && self.content_rows() > visible_rows
    }

    fn draw_scrollbar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        visible_rows: usize,
    ) {
        self.scrollbar.note_offset(self.scrollback_depth());
        if !self.scrollbar_visible(visible_rows) {
            return;
        }
        let from_top = self.viewport_start_y as f64 + self.view.sub / self.char_h.max(1.0);
        let metrics = ScrollMetrics::new(self.view_rows() as f64, visible_rows as f64, from_top);
        self.scrollbar
            .paint(scene, transform, self.scrollbar_area(), metrics, theme);
    }

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
        let shell_ran = matches!(
            self.sessions.state,
            crate::sessions::OpenState::Open | crate::sessions::OpenState::Fixed
        );
        let restart = t("terminal-restart");
        let content = if self.is_disconnected() && shell_ran {
            EmptyStateContent {
                icon: Some(Icon::Terminal),
                message: Some("The shell exited before it printed anything."),
                action_label: self.can_restart().then_some(restart.as_str()),
                ..EmptyStateContent::new("No output")
            }
        } else {
            EmptyStateContent {
                icon: Some(Icon::Terminal),
                ..EmptyStateContent::new("No shell running")
            }
        };
        if let Some(action) = EmptyState::paint(&mut ctx, area, &content, self.last_cursor_pos)
            && content.action_label.is_some()
        {
            self.life.restart_action.set(action);
        }
    }

    /// A pointer position as a buffer-absolute `GridPos`, clamped to the visible grid.
    fn pos_from_logical(&self, p: Point) -> GridPos {
        let local_x = (p.x - self.grid_rect.x0).max(0.0);
        let local_y = (p.y - self.grid_rect.y0).max(0.0) + self.view.sub;
        let col = (local_x / self.char_w.max(1.0)).floor() as usize;
        let vrow = (local_y / self.char_h.max(1.0)).floor() as usize;
        // `width` comes from the grid rect, not `self.height`, which resize clamping can leave stale between
        // frames.
        let (_, visible_rows) = self.viewport_window();
        let vrow = vrow.min(visible_rows.saturating_sub(1));
        let abs_row = (self.viewport_start_y + vrow).min(self.grid.len().saturating_sub(1));
        GridPos {
            row: abs_row,
            col: col.min(self.width.saturating_sub(1)),
        }
    }

    /// The word span containing `pos`, from the toolkit's shared walker; `end` exclusive.
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

    /// The line span for `pos.row`: the whole grid row, column 0 through the width.
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

    /// Start (or, with `extend`, grow) a selection at `pos` for the multi-click `granularity`.
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

    /// Extend the drag to `pos`, growing word and line drags by whole units.
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

    /// Finish a selection on mouse-up: a collapsed one opens an OSC 8 link it landed on and is dropped;
    /// a non-empty one goes to the primary selection and, with select-to-copy, the clipboard.
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

    /// A cell as one linear index for the presence payload, which carries a `usize`.
    fn lin(&self, p: GridPos) -> usize {
        p.row * self.width.max(1) + p.col
    }
    fn unlin(&self, pos: usize) -> GridPos {
        let w = self.width.max(1);
        GridPos {
            row: pos / w,
            col: pos % w,
        }
    }
    fn viewport_window(&self) -> (usize, usize) {
        let visible_rows = (self.grid_rect.height() / self.char_h.max(1.0))
            .floor()
            .max(1.0) as usize;
        (
            self.viewport_start_y,
            visible_rows + usize::from(self.view.sub > 0.0),
        )
    }

    /// A peer's caret as a thin bar at the cell's left edge, or `None` when it is scrolled out of view.
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

    /// A peer's selection as per-row rects, clipped to the viewport.
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

    fn open_context_menu_at_cursor(&mut self) {
        self.target_menu_rows = None;
        let has_selection = self.selected_text().is_some_and(|t| !t.is_empty());
        let mut menu = ContextMenu::new();
        menu.set_platform_conventions(self.conventions);
        self.menu_rows = MenuRows::default();
        let live = !self.is_disconnected();
        if has_selection {
            self.push_menu_effect(
                &mut menu,
                "Copy",
                crate::resource::TerminalEffect::CopySelection,
                true,
            );
        }
        self.menu_rows.paste = Some(menu.items().len());
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
        if self.can_restart() {
            self.push_menu_effect(
                &mut menu,
                &t("terminal-restart"),
                crate::resource::TerminalEffect::Restart,
                true,
            );
        }
        self.menu_rows.find = Some(menu.items().len());
        menu.push(MenuItem::action(
            crate::i18n::t("terminal-find-scrollback"),
            || {},
        ));
        // The bell toggle is secondary chrome, but never at the cost of an unreachable menu: a 160px pane
        // fits the essentials, and its shortcut exposes the toggle.
        if self.rect.height() >= 180.0 {
            menu.push(MenuItem::separator());
            self.menu_rows.copy_on_select = Some(menu.items().len());
            menu.push(MenuItem::action("Copy on select", || {}).with_checked(self.select_to_copy));
            self.menu_rows.bell = Some(menu.items().len());
            menu.push(
                MenuItem::action(self.bell.label(), || {})
                    .with_checked(self.bell == crate::preferences::Bell::Visual),
            );
            self.menu_rows.osc52 = Some(menu.items().len());
            menu.push(
                MenuItem::action("Let programs set the clipboard", || {})
                    .with_checked(self.allow_osc52_clipboard),
            );
        }
        // The shared popup refreshes its geometry on a text-scale change, so give it the terminal's viewport
        // or it grows past a narrow pane.
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

    fn push_menu_effect(
        &mut self,
        menu: &mut ContextMenu,
        label: &str,
        effect: crate::resource::TerminalEffect,
        enabled: bool,
    ) {
        self.menu_rows.effects.insert(menu.items().len(), effect);
        menu.push(MenuItem::action(label, || {}).with_enabled(enabled));
    }

    pub fn open_context_menu_at(&mut self, position: Point) {
        self.last_cursor_pos = position;
        self.open_context_menu_at_cursor();
    }

    fn activate_context_menu_item(&mut self, index: usize) {
        if self.menu_rows.bell == Some(index) {
            self.set_bell(self.bell.toggled());
            return;
        }
        if self.menu_rows.paste == Some(index) {
            if let Some(text) = clipboard_paste() {
                self.submit_effect(crate::resource::TerminalEffect::Paste { text });
            }
            return;
        }
        if let Some(effect) = self.menu_rows.effects.get(&index).cloned() {
            self.submit_effect(effect);
            return;
        }
        if self.menu_rows.find == Some(index) {
            self.find_request = true;
            return;
        }
        if self.menu_rows.copy_on_select == Some(index) {
            self.set_select_to_copy(!self.select_to_copy);
        }
        if self.menu_rows.osc52 == Some(index) {
            self.set_allow_osc52_clipboard(!self.allow_osc52_clipboard);
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

    /// The selected text, trailing whitespace stripped from each row; `None` without a selection.
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
            let abs_row = *row;
            let line = self.grid.get(abs_row).map_or("", String::as_str);

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

    fn map_physical_key(
        physical: PhysicalKey,
        modifiers: winit::event::Modifiers,
    ) -> Option<Vec<u8>> {
        match physical {
            PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter) => Some(vec![b'\r']),
            PhysicalKey::Code(KeyCode::Space) if modifiers.state().control_key() => {
                Self::map_ctrl_char(" ", modifiers.state().alt_key())
            }
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
            Key::Named(NamedKey::Space) if ctrl => Self::map_ctrl_char(" ", alt),
            Key::Named(named) => {
                Self::map_named_key(*named, shift, mod_param, self.modes.app_cursor_keys)
            }
            Key::Character(c) if ctrl => Self::map_ctrl_char(c.as_str(), alt),
            Key::Character(c) if alt && !ctrl && !mods.super_key() => {
                Self::map_alt_char(c.as_str())
            }
            // Hardware Space arrives as `Named(Space)` and a control-socket `type` may carry empty `text`; both
            // must still reach the PTY, not become Activate.
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

    /// Alt as Meta for plain ASCII alphanumerics only: AltGr-composed symbols on layouts like `de_DE`
    /// keep their `text`.
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

/// A flow selection as per-row pixel spans `(viewport_row, start_col, end_col)`, `end_col` excluded.
pub(crate) fn selection_row_spans(sel: &Selection, cols: usize) -> Vec<(usize, usize, usize)> {
    let (start, end) = sel.ordered();
    if start == end {
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

pub(crate) fn selection_span_on(
    sel: &Selection,
    row: usize,
    cols: usize,
) -> Option<(usize, usize)> {
    let (start, end) = sel.ordered();
    if start == end || row < start.row || row > end.row {
        return None;
    }
    let from = if row == start.row { start.col } else { 0 };
    let to = if row == end.row { end.col } else { cols };
    Some((from, to))
}

/// Byte offset in `line` where cell column `col` begins, honouring wide glyphs; clamps past the last
/// glyph.
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

/// The character in cell column `col`, honouring wide glyphs: a trailing continuation cell reports a
/// space.
pub(crate) fn cell_char_at(line: &str, col: usize) -> Option<char> {
    let mut idx = 0usize;
    for c in line.chars() {
        let w = c.width().unwrap_or(1).max(1);
        if idx == col {
            return Some(c);
        }
        if idx + w > col {
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

/// A `CellColor` as a concrete `Color`; bold brightens the low 8 ANSI slots (xterm behaviour).
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

/// A cell's colours with reverse video applied. A `None` background leaves the default showing.
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
        let text = bg.unwrap_or(palette.bg_default);
        (text, Some(fg))
    } else {
        (fg, bg)
    }
}

pub(crate) fn cursor_from_palette(palette: &ui::tokens::TerminalPalette) -> Color {
    // An opaque inverted cell: the block is the theme's default foreground and
    // the glyph beneath it is re-drawn in the background colour. Taking the hue
    // from the palette keeps the cursor tracking theme swaps.
    palette.fg_default.with_alpha_factor(1.0)
}

/// Snap `rect` to whole device pixels so a cell fill has crisp edges and adjacent runs tile without
/// a seam.
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
    pub relay_session: Option<String>,
    pub supervisor_session: Option<String>,
    pub runs: Option<String>,
    native_editor: Option<crate::native_editor::Socket>,
    service_lifetime: Option<runtime_portal::pending_connection::SessionLifetime>,
    #[cfg(windows)]
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
}
impl TerminalBackend {
    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn for_test() -> Self {
        Self {
            writer: Box::new(std::io::sink()),
            reader: Box::new(std::io::empty()),
            master: Box::new(NullMaster),
            shell_name: "test".into(),
            relay_session: None,
            supervisor_session: None,
            runs: None,
            native_editor: None,
            service_lifetime: None,
        }
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn for_test_remote(
        output: std::os::unix::net::UnixStream,
        lease: std::os::unix::net::UnixStream,
        relay_session: &str,
    ) -> Self {
        Self {
            relay_session: Some(relay_session.to_owned()),
            ..Self::for_test_leased(output, lease)
        }
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn for_test_leased(
        output: std::os::unix::net::UnixStream,
        lease: std::os::unix::net::UnixStream,
    ) -> Self {
        Self {
            writer: Box::new(std::io::sink()),
            reader: Box::new(output),
            master: Box::new(NullMaster),
            shell_name: "ush".into(),
            relay_session: None,
            supervisor_session: None,
            runs: None,
            native_editor: None,
            service_lifetime: Some(runtime_portal::pending_connection::SessionLifetime::new(
                lease,
            )),
        }
    }

    /// Open a PTY on `target`.
    ///
    /// Never `$SHELL` by default: an app sandbox binds no executable directory, so a session runs where
    /// its target says (docs/app-model/environments.md), and the route comes from a capability probe,
    /// never from trying each in turn.
    ///
    /// # Errors
    ///
    /// Why the session could not open: the person's allow is missing, the supervisor refused it, or
    /// nothing here can run it.
    pub fn open(
        target: &crate::target::SessionTarget,
        requested_by: Requester,
        relay_session: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<Self, OpenError> {
        let program = launch_command_from_env();
        match confinement_route(target) {
            ConfineRoute::Service => {
                ush_service_backend(&program, target, requested_by, relay_session, workspace)
            }
            ConfineRoute::SelfHosted(bin) => {
                let mut cmd = CommandBuilder::new(&bin);
                cmd.arg("run");
                // The runtime's interactive bash marks its runs with this
                // session's key, as it does for the supervisor's sessions.
                let key = program.is_empty().then(crate::runs::new_key);
                if let Some(key) = &key {
                    cmd.env("TILO_USH_RUNS", key);
                } else {
                    cmd.arg("-c");
                    cmd.arg(exec_script(&program));
                }
                let mut backend = Self::with_command(cmd)
                    .map_err(|error| OpenError::Failed(error.to_string()))?;
                backend.runs = key;
                Ok(backend)
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
                    // The login shell this account starts, with the process
                    // environment inherited.
                    CommandBuilder::new(tilo_process::login_shell::login_shell())
                };
                Self::with_command(cmd).map_err(|error| OpenError::Failed(error.to_string()))
            }
            ConfineRoute::Unavailable(reason) => Err(OpenError::Unavailable(reason)),
        }
    }

    /// Open a PTY running `command`, with `TERM` forced to `xterm-256color` so programs emit the SGR and
    /// DEC sequences the emulator reads.
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
            supervisor_session: None,
            runs: None,
            native_editor: None,
            service_lifetime: None,
            #[cfg(windows)]
            child,
        })
    }
}

/// The program the desktop asked this terminal to run instead of a login shell, as
/// `{"terminal_command": [argv]}` in the launcher's context channel.
pub fn launch_command_from_env() -> Vec<String> {
    std::env::var("TILO_TOOL_CONTEXT")
        .ok()
        .map(|context| launch_command_from_context(&context))
        .unwrap_or_default()
}

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

pub fn exec_script(argv: &[String]) -> String {
    let words: Vec<String> = argv
        .iter()
        .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
        .collect();
    format!("exec {}", words.join(" "))
}

/// The environment variable the launcher sets when it opened the `ush` service socket for this app.
pub const USH_SERVICE_FD_ENV: &str = "TILO_SVC_USH_FD";

static USH_SERVICE: runtime_portal::service_socket::InheritedControlSocket =
    runtime_portal::service_socket::InheritedControlSocket::new("ush");

/// Who caused a session to open. The device's own shell is never opened for an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Requester {
    #[default]
    Person,
    Agent,
}

impl Requester {
    /// Who is behind the current invocation: only the person at this screen acts without one.
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
    NeedsAllow(String),
    Refused(String),
    Unavailable(String),
    /// No environment can be built on this device: no `env-runtime` image is
    /// installed. The device's own shell may still be opened instead.
    NoRuntime(String),
    Failed(String),
}

impl OpenError {
    /// Whether the supervisor answered and refused, leaving the connection usable.
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

/// Which way the terminal reaches a target, decided by a capability probe.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfineRoute {
    /// The supervisor is reachable: the production path for every target, and a refusal is final.
    Service,
    /// This unconfined host can build a guest itself and has the runtime at the given path.
    SelfHosted(std::path::PathBuf),
    Host,
    Unavailable(String),
}

/// Choose how to reach `target` from a capability probe, security first.
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

pub fn environments_available() -> bool {
    !matches!(
        confinement_route(&crate::target::SessionTarget::default_environment()),
        ConfineRoute::Unavailable(_)
    )
}

/// [`confinement_route`] with its probes injected, so the trust boundary is testable: the supervisor
/// wins unconditionally, the device's own shell is reached only by naming it, and an environment that
/// cannot run is unavailable rather than the host shell.
pub fn decide_route(
    target: &crate::target::SessionTarget,
    service_available: bool,
    can_self_host: bool,
    runtime: Option<std::path::PathBuf>,
) -> ConfineRoute {
    if let SessionTarget::Legacy { .. } = target {
        return ConfineRoute::Unavailable("The legacy VM service is not running".into());
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

/// The supervisor's descriptor, if the launcher opened one and it is still live: a spoofed number
/// pointing at nothing does not select the service path.
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

/// Whether this process could build the user and mount namespace a self-hosted guest needs: false
/// under a seccomp filter, without unprivileged user namespaces, or without `bwrap`.
#[cfg(unix)]
fn can_build_namespace() -> bool {
    !seccomp_filtered() && unprivileged_userns_enabled() && bwrap_present()
}

/// Whether a seccomp filter confines this process.
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

/// Whether the kernel allows unprivileged user namespaces. An absent sysctl means the kernel default,
/// so only an explicit disable reports false.
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

#[cfg(unix)]
fn bwrap_present() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("bwrap").exists()))
}

/// Where the runtime binary is: `$TILO_USH_BIN`, then the installed location, then `PATH`.
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

/// Ask the supervisor for a session on `target` and wrap the pty it hands back: a terminal crosses
/// the sandbox boundary, not a namespace.
#[cfg(unix)]
fn ush_service_backend(
    program: &[String],
    target: &crate::target::SessionTarget,
    requested_by: Requester,
    relay_session: Option<&str>,
    workspace: Option<&str>,
) -> Result<TerminalBackend, OpenError> {
    use runtime_portal::service_socket::ServiceSocketError;

    // SAFETY: this instance alone owns the inherited fd. ush_backend_on
    // negotiates protocol two and returns only its independent session lease.
    unsafe {
        USH_SERVICE.with_control(|stream| {
            ush_backend_on(
                stream,
                program,
                target,
                requested_by,
                relay_session,
                workspace,
            )
        })
    }
    .map_err(|error| match error {
        ServiceSocketError::InUse => {
            OpenError::Unavailable(t("terminal-service-control-unavailable"))
        }
        other => OpenError::Failed(other.to_string()),
    })?
}

/// One request about the environments themselves to the supervisor, over the launcher's control
/// connection; its answer is one line.
///
/// # Errors
///
/// No launcher connection, or the supervisor did not answer.
pub(crate) fn ush_service_ask(
    request: &serde_json::Value,
    workspace: Option<&str>,
) -> Result<String, String> {
    use runtime_portal::service_socket::{ControlError, ServiceSocketError};

    let mut request = request.clone();
    crate::sessions::with_workspace(&mut request, workspace);
    let mut line = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    line.push(b'\n');
    // SAFETY: this instance alone owns the inherited fd; the exchange keeps
    // only the control connection and hands out no lease.
    let exchanged = unsafe {
        USH_SERVICE.exchange(|link| -> Result<String, String> {
            link.send(b"{\"op\":\"hello\",\"protocol\":2}\n")
                .map_err(|e| e.to_string())?;
            let negotiation = link.answer().map_err(|e| e.to_string())?;
            let ready = serde_json::from_slice::<serde_json::Value>(&negotiation).ok();
            if ready
                .as_ref()
                .is_none_or(|value| value["result"] != "ready" || value["protocol"] != 2)
            {
                return Err(t("terminal-service-protocol-unavailable"));
            }
            link.send(&line).map_err(|e| e.to_string())?;
            let answer = link.answer().map_err(|e| e.to_string())?;
            String::from_utf8(answer).map_err(|e| e.to_string())
        })
    };
    exchanged.map_err(|error| match error {
        ControlError::Socket(
            ServiceSocketError::Missing | ServiceSocketError::InUse | ServiceSocketError::Poisoned,
        ) => "environments are managed through Tilo's shell runtime".to_owned(),
        other => other.to_string(),
    })?
}

#[cfg(windows)]
fn ush_service_backend(
    _program: &[String],
    _target: &crate::target::SessionTarget,
    _requested_by: Requester,
    _relay_session: Option<&str>,
    _workspace: Option<&str>,
) -> Result<TerminalBackend, OpenError> {
    Err(OpenError::Unavailable(
        "Tilo hosted terminal service transport is unavailable on Windows".into(),
    ))
}

/// The `open-session` line for the supervisor; `workspace` is left out when the terminal does not
/// know it.
pub(crate) fn ush_request(
    program: &[String],
    target: &crate::target::SessionTarget,
    requested_by: Requester,
    relay_session: Option<&str>,
    workspace: Option<&str>,
    runs: Option<&str>,
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
        // The lease says how the session ended: only a shell that exited
        // closes the window; a lost connection to a remote shell reconnects,
        // and an environment that could not be set up says so on the tab.
        fields.insert("end_report".to_string(), serde_json::json!(true));
        if let Some(key) = runs {
            fields.insert("runs".to_string(), serde_json::json!(key));
        }
    }
    crate::sessions::with_workspace(&mut request, workspace);
    let mut request =
        serde_json::to_vec(&request).map_err(|error| OpenError::Failed(error.to_string()))?;
    request.push(b'\n');
    Ok(request)
}

/// The supervisor's answer to `open-session`, field for field (`ServeResponse::Session` in
/// `crates/runtime/ush/src/supervisor.rs`). Unknown fields are refused, so every field the supervisor
/// sends is named here; `a_session_reply_keeps_its_wire_form` pins the form.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UshSessionReply {
    result: String,
    session: String,
    environment: String,
    #[serde(default)]
    relay_session: Option<String>,
    /// The session's shell marks its runs with the key the request named.
    #[serde(default)]
    runs: bool,
    #[serde(default)]
    editor_protocol: Option<u8>,
    /// The control protocol that granted this session's private lease.
    #[serde(default)]
    protocol: u16,
}

impl UshSessionReply {
    /// Whether the session declaration is well formed and matches what was asked.
    fn is_valid(
        &self,
        descriptor_count: usize,
        program: &[String],
        target: &crate::target::SessionTarget,
    ) -> bool {
        let hex_key = |key: &str| key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit());
        let descriptors = if self.editor_protocol.is_some() { 3 } else { 2 };
        self.result == "session"
            && hex_key(&self.session)
            && !self.environment.is_empty()
            && self.environment.len() <= 1024
            && self.relay_session.as_deref().is_none_or(hex_key)
            && self
                .editor_protocol
                .is_none_or(|version| version == runtime_portal::editor::VERSION)
            && (self.editor_protocol.is_none() || (program.is_empty() && !target.is_host()))
            && self.protocol == 2
            && descriptor_count == descriptors
    }
}

#[cfg(unix)]
pub(crate) fn ush_backend_on(
    stream: &std::os::unix::net::UnixStream,
    program: &[String],
    target: &crate::target::SessionTarget,
    requested_by: Requester,
    relay_session: Option<&str>,
    workspace: Option<&str>,
) -> Result<TerminalBackend, OpenError> {
    // How long the answer may take. Before it answers, the supervisor checks
    // that the environment can be built, which on a device's first open reads
    // the system store's images: ~400 ms on the CI VM, past the 250 ms a
    // hosted connection is given to connect. Never shorten a longer
    // bound, nor add one where the connection has none.
    const USH_OPEN_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
    let failed = |error: &dyn std::fmt::Display| OpenError::Failed(error.to_string());
    {
        let mut writer = stream;
        writer
            .write_all(b"{\"op\":\"hello\",\"protocol\":2}\n")
            .map_err(|e| failed(&e))?;
        writer.flush().map_err(|e| failed(&e))?;
    }
    let (hello, rights) =
        runtime_portal::fd_transport::recv_with_fds(stream).map_err(|e| failed(&e))?;
    let ready = serde_json::from_slice::<serde_json::Value>(&hello).ok();
    if !rights.is_empty()
        || ready
            .as_ref()
            .is_none_or(|value| value["result"] != "ready" || value["protocol"] != 2)
    {
        return Err(OpenError::Unavailable(t(
            "terminal-service-protocol-unavailable",
        )));
    }
    let key = crate::runs::new_key();
    let request = ush_request(
        program,
        target,
        requested_by,
        relay_session,
        workspace,
        Some(&key),
        (80, 24),
    )?;
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
        runtime_portal::fd_transport::recv_with_fds(stream).map_err(|e| failed(&e))?;
    if descriptors.is_empty() {
        return Err(refusal(&payload));
    }
    let response: UshSessionReply = serde_json::from_slice(&payload)
        .map_err(|_| OpenError::Failed("Malformed ush session descriptor declaration".into()))?;
    if !response.is_valid(descriptors.len(), program, target) {
        return Err(OpenError::Failed(t("terminal-service-invalid-session")));
    }
    let native_editor = if response.editor_protocol.is_some() {
        let fd = descriptors
            .pop()
            .ok_or_else(|| OpenError::Failed("Missing editor descriptor".into()))?;
        Some(crate::native_editor::Socket::adopt(fd).map_err(|e| failed(&e))?)
    } else {
        None
    };
    let lifetime = descriptors
        .pop()
        .ok_or_else(|| OpenError::Failed(t("terminal-service-missing-lease")))?;
    let fd = descriptors
        .pop()
        .ok_or_else(|| OpenError::Failed(t("terminal-service-missing-terminal")))?;

    // Every descriptor arrived over `SCM_RIGHTS` and this process now owns it.
    // The supervisor declares PTY first, then the private session lease, and
    // only after negotiation the editor capability last.
    let master = std::fs::File::from(fd);
    let lifetime = runtime_portal::pending_connection::SessionLifetime::adopt(lifetime)
        .map_err(|_| OpenError::Failed(t("terminal-service-invalid-lease")))?;
    tilo_process::editor::validate_master(&master).map_err(|e| failed(&e))?;
    let reader = Box::new(master.try_clone().map_err(|e| failed(&e))?) as Box<dyn Read + Send>;
    let writer = Box::new(master.try_clone().map_err(|e| failed(&e))?) as Box<dyn Write + Send>;
    Ok(TerminalBackend {
        writer,
        reader,
        master: Box::new(FdMaster { file: master }),
        shell_name: if target.is_host() { "host" } else { "ush" }.into(),
        relay_session: response.relay_session,
        supervisor_session: Some(response.session),
        runs: response.runs.then_some(key),
        native_editor,
        service_lifetime: Some(lifetime),
    })
}

struct TopBanner {
    kind: BannerKind,
    title: String,
    message: Option<String>,
    action: Option<String>,
    secondary: Option<String>,
    held: bool,
}

impl TopBanner {
    fn new(kind: BannerKind, title: String, action: Option<String>) -> Self {
        Self {
            kind,
            title,
            message: None,
            action,
            secondary: None,
            held: false,
        }
    }

    /// The prompt of a run its host holds.
    fn held(held: &Held) -> Self {
        use ui::tilo_actions::remote::host_account::Reason;
        let (title, message) = match held.refusal.reason {
            Reason::NoPaymentMethod => (
                t("terminal-held-title"),
                t("terminal-held-add-card-message"),
            ),
            _ => (t("terminal-held-title"), held.refusal.message.clone()),
        };
        Self {
            kind: BannerKind::Info,
            title,
            message: Some(message),
            action: held.add_card_url().map(|_| t("terminal-held-add-card")),
            secondary: Some(t("terminal-held-cancel")),
            held: true,
        }
    }

    fn content(&self) -> BannerContent<'_> {
        let mut content =
            BannerContent::new(self.kind, &self.title).with_message(self.message.as_deref());
        content.action = self.action.as_deref();
        content.secondary_action = self.secondary.as_deref();
        content
    }
}

impl Held {
    /// The provider's page to add a card on: only a secure web page.
    pub(crate) fn add_card_url(&self) -> Option<&str> {
        use ui::tilo_actions::remote::host_account::NextStepKind;
        self.refusal
            .step(NextStepKind::AddPaymentMethod)
            .and_then(|step| step.url.as_deref())
            .filter(|url| url.starts_with("https://") && !url.chars().any(char::is_whitespace))
    }
}

fn fail_run(asked: crate::runs::Asked, reason: &str) {
    let settled = ui::tilo_resource::Settled::Failed {
        reason: reason.to_owned(),
        partial: crate::resource::RunAnswer::default(),
    };
    asked.reply.finish(Ok(ui::tilo_actions::ActionOutput {
        value: serde_json::to_value(settled).ok(),
        resources: Vec::new(),
    }));
}

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

/// A [`MasterPty`] over a descriptor someone else opened. Resizing is a real `TIOCSWINSZ`: the grid
/// reflows on every window change.
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
    /// Wire a spawned backend into the widget and start the reader thread that feeds `sync` and
    /// `update`.
    fn from_backend(backend: TerminalBackend) -> Self {
        let mut widget = Self::without_process(ContentStore::memory());
        widget.use_preferences(crate::preferences::Preferences::discover());
        widget.attach_backend(backend);
        widget
    }

    fn without_process(content: ContentStore) -> Self {
        let writer: Box<dyn Write + Send> = Box::new(std::io::sink());
        let (_tx, output_rx) = sync_channel(1);
        let widget = TerminalWidget::new(
            std::sync::Arc::new(std::sync::Mutex::new(writer)),
            std::sync::Arc::new(std::sync::Mutex::new(Box::new(NullMaster))),
            output_rx,
            content,
        );
        widget.mark_disconnected("Terminal process has not opened. Input unavailable.");
        widget
    }

    fn attach_native_editor(&mut self, socket: Option<crate::native_editor::Socket>) {
        self.native_capture = None;
        self.native_history = None;
        self.native_ime = None;
        self.native_ime_error = None;
        self.native_editor = socket.and_then(|socket| {
            crate::native_editor::Client::new(socket, Arc::clone(&self.waker)).ok()
        });
    }

    /// ConPTY keeps its output pipe open while the pseudoconsole exists, so
    /// the child handle is watched too: its exit is the session's.
    #[cfg(windows)]
    fn watch_child_exit(
        &mut self,
        mut child: Box<dyn portable_pty::Child + Send + Sync>,
        child_exit: SessionExit,
    ) {
        // Keep a killer for normal window teardown.
        self.child_killer = Some(child.clone_killer());
        let disconnected = self.session_disconnected.clone();
        let status = self.session_status_message.clone();
        let waker = self.waker.clone();
        if std::thread::Builder::new()
            .name("tilo-terminal-child-wait".into())
            .spawn(move || {
                let message = if child.wait().is_ok() {
                    session_ended_message()
                } else {
                    crate::i18n::t("terminal-session-exit-unknown")
                };
                set_terminal_disconnected(&disconnected, &status, &message);
                child_exit.ended(SessionEnd::Exited);
                request_terminal_wake(&waker);
            })
            .is_err()
        {
            if let Some(killer) = &mut self.child_killer {
                let _ = killer.kill();
            }
            self.mark_disconnected(
                "Session unavailable. The terminal could not watch the shell for exit.",
            );
        }
    }

    /// What the reader asks whether the session is still open: the lifetime socket the supervisor closes
    /// when the guest ends, which tells a hosted start-up `EIO` from an exit.
    fn reader_lease(&self) -> ReaderLease {
        ReaderLease::new(
            self.service_lifetime
                .as_ref()
                .and_then(|socket| socket.clone_for_reader().ok()),
        )
    }

    fn attach_backend(&mut self, backend: TerminalBackend) {
        // What this session's threads say when its shell exits.
        let exit = self.next_session_exit();
        self.life.close_asked = false;
        let (output_tx, output_rx) = sync_channel(PTY_QUEUE_CHUNKS);
        self.pty_write = std::sync::Arc::new(std::sync::Mutex::new(backend.writer));
        self.pty_master = std::sync::Arc::new(std::sync::Mutex::new(backend.master));
        self.output_rx = output_rx;
        self.deferred_output.set(None);
        self.session_disconnected = Arc::new(AtomicBool::new(false));
        self.session_status_message = Arc::new(Mutex::new(crate::i18n::t(
            "terminal-session-input-unavailable",
        )));
        self.begin_local_session();
        self.runs.marks = backend.runs.map(crate::runs::Marks::new);
        let widget = self;
        widget.attach_native_editor(backend.native_editor);
        widget.service_lifetime = backend.service_lifetime;
        #[cfg(windows)]
        widget.watch_child_exit(backend.child, exit.clone());
        let waker = widget.waker.clone();
        let session_disconnected = widget.session_disconnected.clone();
        let session_status_message = widget.session_status_message.clone();

        let lease = widget.reader_lease();
        let reader = std::sync::Arc::new(std::sync::Mutex::new(backend.reader));
        let reader_result = std::thread::Builder::new()
            .name("tilo-terminal-pty-reader".to_string())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    let Ok(mut r) = reader.lock() else {
                        set_terminal_disconnected(
                            &session_disconnected,
                            &session_status_message,
                            &crate::i18n::t("terminal-session-read-failed"),
                        );
                        request_terminal_wake(&waker);
                        break;
                    };
                    let read = std::io::Read::read(&mut *r, &mut buf);
                    match classify_pty_read(&read, || lease.open()) {
                        PtyReadDisposition::Retry => {
                            // Avoid a hot loop while the guest is between
                            // opening/closing slave descriptors.
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        PtyReadDisposition::Closed => {
                            set_terminal_disconnected(
                                &session_disconnected,
                                &session_status_message,
                                &session_ended_message(),
                            );
                            // The shell exited (its window closes), or the
                            // connection to a remote one was lost (the tab
                            // reconnects): the supervisor's report says which.
                            exit.ended(lease.end());
                            request_terminal_wake(&waker);
                            break;
                        }
                        PtyReadDisposition::Data(n) => {
                            let Some(chunk) = buf.get(..n) else { break };
                            if output_tx.send(chunk.to_vec()).is_err() {
                                break;
                            }
                            request_terminal_wake(&waker);
                        }
                    }
                }
            });
        if reader_result.is_err() {
            widget.mark_disconnected("Terminal unavailable. Its input could not be read.");
            request_terminal_wake(&widget.waker);
        }
    }

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

    pub(crate) fn spawn_hosted_deferred() -> Self {
        let mut widget = Self::without_process(ContentStore::memory());
        widget.sessions = crate::sessions::Sessions::hosted_deferred();
        widget
    }

    /// A live hosted tab whose supervisor is unavailable: the cause stays visible and input refused,
    /// rather than a capture-only terminal.
    pub(crate) fn without_hosted_runtime(reason: String) -> Self {
        let mut widget = Self::without_process(ContentStore::memory());
        widget.sessions =
            crate::sessions::Sessions::hosted(crate::HostedProcessServices::unavailable());
        widget.show_status(&reason);
        widget.sessions.state = crate::sessions::OpenState::NoRuntime(reason);
        widget
    }

    pub fn spawn_default() -> anyhow::Result<Self> {
        let mut widget = Self::without_process(ContentStore::memory());
        widget.sessions = crate::sessions::Sessions::standalone();
        Ok(widget)
    }

    /// Spawn `command` in a PTY and return a wired widget, for hosts that want a specific program instead
    /// of a login shell.
    ///
    /// ```no_run
    /// use terminal::{TerminalWidget, CommandBuilder};
    /// let cmd = CommandBuilder::new("htop");
    /// let _term = TerminalWidget::spawn_command(cmd).unwrap();
    /// ```
    pub fn spawn_command(cmd: CommandBuilder) -> anyhow::Result<Self> {
        let backend = TerminalBackend::with_command(cmd.clone())
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let mut widget = Self::from_backend(backend);
        widget.respawn = Some(cmd);
        Ok(widget)
    }

    pub fn spawn_headless() -> anyhow::Result<Self> {
        let mut widget = Self::without_process(ContentStore::memory());
        widget.headless = true;
        Ok(widget)
    }

    /// Keep program pictures in `content`, the window's store, which carries them to the record's other
    /// readers; without it they stay in memory.
    #[must_use]
    pub fn with_content(mut self, content: ContentStore) -> Self {
        self.images.set_content(content.clone());
        self.content = content;
        self
    }
}

impl Drop for TerminalWidget {
    fn drop(&mut self) {
        self.end_session_exit();
        #[cfg(windows)]
        if let Some(killer) = &mut self.child_killer {
            let _ = killer.kill();
        }
    }
}

include!("native_editing_tests.rs");
