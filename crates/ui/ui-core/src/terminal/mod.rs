//! The toolkit's half of a terminal: the grid a `ui.Terminal` draws and the session behind it.
//!
//! The engine that runs a shell and keeps its grid is not part of the toolkit (`tilo-terminal`
//! in `crates/engines/`), exactly as a page's engine is not. The window that draws a terminal
//! holds its sessions and hands them here ([`set_window_sessions`]) before it builds the nodes,
//! and every `ui.Terminal` it builds reads the one its `session` names. What travels the other
//! way is bytes: a key, a pasted text or a pointer report goes to the host's session, which is
//! the one place the key table lives (`tilo_terminal::input`), so the toolkit never carries a
//! second encoding of what a shell decodes.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use vello::peniko::Color;

/// The colour a cell's text or background takes, in the engine's own vocabulary: the terminal's
/// palette entry, or a colour the program chose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CellColor {
    /// The palette's default foreground or background.
    #[default]
    Default,
    /// An entry of the theme's ANSI ramp, 0–15 for the standard and bright entries; 16–255 are
    /// the extended ramp entries the theme does not name.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// How one cell is drawn: its colours and its SGR attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: CellColor,
    pub bg: CellColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub dim: bool,
    pub strike: bool,
    /// The cell after a wide glyph, which draws nothing of its own.
    pub wide_cont: bool,
    /// The OSC 8 link the cell belongs to; `0` is none.
    pub link: u16,
}

/// One row of the grid: its text, one character per cell, and each cell's style.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub styles: Vec<Style>,
}

/// The grid a terminal shows: the main buffer with its scrollback, the alternate buffer a
/// full-screen program draws on, where the cursor is, and the links on screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Screen {
    pub lines: Vec<Line>,
    pub alternate: bool,
    /// The cursor's column and its row in `lines`.
    pub cursor: (usize, usize),
    /// The URL each OSC 8 link id on screen names.
    pub links: BTreeMap<u16, String>,
}

/// An image a program placed on the grid (the Kitty protocol), by the hash of its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub hash: String,
    pub mime: String,
    pub row: usize,
    pub column: usize,
    pub columns: u32,
    pub rows: u32,
    /// Whether it was placed on the alternate screen.
    pub alternate: bool,
}

/// What the process asked the terminal to do with keys, paste and the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes {
    pub cursor_visible: bool,
    pub bracketed_paste: bool,
    pub app_cursor_keys: bool,
    pub mouse_report: bool,
    pub mouse_sgr: bool,
    pub focus_report: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            cursor_visible: true,
            bracketed_paste: false,
            app_cursor_keys: false,
            mouse_report: false,
            mouse_sgr: false,
            focus_report: false,
        }
    }
}

/// A grid position: the row in the buffer, scrollback included, and the column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct GridPos {
    pub row: usize,
    pub col: usize,
}

impl GridPos {
    #[must_use]
    pub const fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}

/// A selection from `anchor` to `cursor`, in grid positions; either may come first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GridSelection {
    pub anchor: GridPos,
    pub cursor: GridPos,
}

impl GridSelection {
    /// The two ends with the earlier one first, rows before columns.
    #[must_use]
    pub fn ordered(&self) -> (GridPos, GridPos) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    #[must_use]
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.cursor
    }

    /// The cell span the selection takes on `row` of a grid `columns` wide, `end` excluded.
    #[must_use]
    pub fn span_on(&self, row: usize, columns: usize) -> Option<(usize, usize)> {
        let (start, end) = self.ordered();
        if start == end || row < start.row || row > end.row {
            return None;
        }
        let from = if row == start.row { start.col } else { 0 };
        let to = if row == end.row { end.col } else { columns };
        Some((from, to))
    }
}

/// Something a session reported that the app records or shows, in the order it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// The program set the window's title (OSC 0/2).
    Title(String),
    /// A run the shell marked with OSC 133 ended; the JSON is its `FinishedCommand`.
    Finished(String),
    /// The shell exited, with its status when it had one.
    Exit(i32),
    /// The session ended without exiting: the connection to a remote shell was lost, or the
    /// workspace's host refused it. Carries why.
    Lost(String),
    /// The bell rang.
    Bell,
    /// The program asked for text to be put on the clipboard (OSC 52); the bool says whether it
    /// asked for the primary selection.
    Clipboard(String, bool),
}

/// The session behind a terminal: the grid it shows and the bytes it takes.
///
/// A host implements it over the engine it holds, so the toolkit draws a grid and sends keys
/// without knowing what runs the shell. The handle is shared and may be polled from the frame
/// that draws it.
pub use crate::key_input::{Button, Held, Key, Keys, Named};

pub trait Session: Keys {
    /// The grid as it stands now, as the host last converted it: a handle, so a frame that did
    /// not change costs a clone of the pointer and never the rows.
    fn screen(&self) -> Arc<Screen>;
    /// What the process asked of keys, paste and the mouse.
    fn modes(&self) -> Modes;
    /// A counter that rises whenever the grid or the modes changed.
    fn revision(&self) -> u64;
    /// The grid is `columns` by `rows` cells.
    fn resize(&self, columns: u16, rows: u16);
    /// The words the selection covers, each row's trailing blanks cut; `None` when it covers
    /// nothing.
    fn selection_text(&self, anchor: GridPos, cursor: GridPos) -> Option<String>;
    /// The images a program placed on the grid.
    fn placements(&self) -> Vec<Placement>;
    /// The bytes of an image a placement names, by content hash; `None` when this device does
    /// not hold them.
    fn image_bytes(&self, hash: &str) -> Option<Vec<u8>>;
    

    /// Everything the session reported after `since`, and the counter to ask from next.
    fn events(&self, since: u64) -> (Vec<SessionEvent>, u64);
    /// Whether `url` is a plain public web address, which a terminal may offer: the one rule
    /// lives with the engine, so a view never opens what an app would not publish.
    fn public_link(&self, url: &str) -> bool;
    /// The session's shell ended (or never started): nothing it is sent arrives anywhere.
    fn ended(&self) -> bool;
}

/// A handle on one session.
pub type SessionHandle = Arc<dyn Session>;

/// A window's sessions by the id the app names them by.
pub type Sessions = HashMap<String, SessionHandle>;

thread_local! {
    /// The sessions the window drawing now hands to the `ui.Terminal` nodes it builds.
    static WINDOW_SESSIONS: RefCell<Option<Arc<Sessions>>> = const { RefCell::new(None) };
}

/// Hand `sessions` to the `ui.Terminal` nodes the window builds until the next call; `None`
/// when the window draws no session.
pub fn set_window_sessions(sessions: Option<Arc<Sessions>>) {
    WINDOW_SESSIONS.with(|held| *held.borrow_mut() = sessions);
}

/// The sessions the window drawing now hands its `ui.Terminal` nodes.
#[must_use]
pub fn window_sessions() -> Option<Arc<Sessions>> {
    WINDOW_SESSIONS.with(|held| held.borrow().clone())
}

/// The session `id` names, as the window drawing now holds it.
#[must_use]
pub fn window_session(id: &str) -> Option<SessionHandle> {
    WINDOW_SESSIONS.with(|held| {
        held.borrow()
            .as_ref()
            .and_then(|sessions| sessions.get(id).cloned())
    })
}

/// Take the session `id` names out of what the window drew with: a view that has no session of
/// its own still names the one the window holds.
#[must_use]
pub fn take_window_session(id: &str) -> Option<SessionHandle> {
    WINDOW_SESSIONS.with(|held| {
        let mut held = held.borrow_mut();
        let sessions = Arc::make_mut(held.as_mut()?);
        sessions.remove(id)
    })
}

/// The colour a cell draws its text in, from the theme's terminal palette; a bold cell brightens
/// the low eight slots, as xterm does.
#[must_use]
pub fn text_color(color: CellColor, bold: bool, palette: &Palette) -> Color {
    match color {
        CellColor::Default => palette.foreground,
        CellColor::Indexed(index) => palette.ansi_256(if bold && index < 8 {
            index + 8
        } else {
            index
        }),
        CellColor::Rgb(r, g, b) => Color::from_rgba8(r, g, b, 255),
    }
}

/// The colour a cell draws its background in.
#[must_use]
pub fn background_color(color: CellColor, palette: &Palette) -> Color {
    match color {
        CellColor::Default => palette.background,
        CellColor::Indexed(index) => palette.ansi_256(index),
        CellColor::Rgb(r, g, b) => Color::from_rgba8(r, g, b, 255),
    }
}

/// The terminal colours the theme gives a grid.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub foreground: Color,
    pub background: Color,
    /// ANSI 0..=15, the ramp the theme names.
    pub ansi: [Color; 16],
}

impl Palette {
    /// A palette from the theme's own terminal colours.
    #[must_use]
    pub fn from_theme(theme: &crate::theme::Theme) -> Self {
        let named = &theme.tokens.terminal_palette;
        Self {
            foreground: named.fg_default,
            background: named.bg_default,
            ansi: named.ansi,
        }
    }

    /// An entry of the 256-colour ramp: the theme names the first 16, and the cube and its greys
    /// are the standard's own arithmetic, not a themable surface.
    #[must_use]
    pub fn ansi_256(&self, code: u8) -> Color {
        if let Some(color) = self.ansi.get(usize::from(code)) {
            return *color;
        }
        if code < 232 {
            let n = code - 16;
            let r = (n / 36) * 51;
            let g = ((n % 36) / 6) * 51;
            let b = (n % 6) * 51;
            Color::from_rgba8(r, g, b, 255)
        } else {
            let gray = (code - 232) * 10 + 8;
            Color::from_rgba8(gray, gray, gray, 255)
        }
    }
}
