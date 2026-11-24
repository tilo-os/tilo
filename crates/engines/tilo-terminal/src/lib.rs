//! A terminal session's engine: the VT emulator, the grid and its scrollback, the pty a
//! session's shell runs on, and the parsers that read what a command printed.
//!
//! Nothing here draws. A view feeds what a [`Session`] prints into an [`Emulator`] and paints its
//! [`Screen`]; the session owns the process and answers the calls made on it ([`Session::write`],
//! [`Session::run`], [`Session::interrupt`], a resize, a close).
//!
//! A session is opened by the workspace's host where it runs: `ush` in an environment, the
//! device's own login shell for `host`, a relay's session when one was carried. What the
//! process printed never reaches a record; the runs its shell marked with OSC 133 are what a
//! record keeps.

// The parsers step through byte and cell indices, where a wrap costs one session's grid rather
// than the desktop. The lints whose fault is a crash stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "index and dimension arithmetic over a bounded grid and stream"
)]

use serde::{Deserialize, Serialize};

pub mod commands;
// The grid is a device's: a page has no shell of its own to draw.
#[cfg(not(target_arch = "wasm32"))]
mod emulator;
#[cfg(not(target_arch = "wasm32"))]
mod graphics;
pub mod model;
pub mod parse;
#[cfg(not(target_arch = "wasm32"))]
pub mod input;
#[cfg(not(target_arch = "wasm32"))]
pub mod provider;
#[cfg(not(target_arch = "wasm32"))]
pub mod selection;
#[cfg(not(target_arch = "wasm32"))]
mod screen;
#[cfg(not(target_arch = "wasm32"))]
mod session;
pub mod target;
pub mod undoability;

pub use commands::{Commands, FinishedCommand, HistoryAsk, Mark, Marks, Piece};
#[cfg(not(target_arch = "wasm32"))]
pub use emulator::{DEFAULT_HISTORY, Emulator, MAX_OSC_LINK_BYTES, Output, TEST_HISTORY};
#[cfg(not(target_arch = "wasm32"))]
pub use graphics::{
    KittyImage, StreamEvent, TerminalImagePlacement, parse_kitty, placement_is_sound,
};
pub use model::{Item, ItemKind};
#[cfg(not(target_arch = "wasm32"))]
pub use provider::{
    ConfineRoute, NullMaster, OpenError, OpenOptions, Requester, decide_route, environments_available,
    exec_script, launch_command_from_context, launch_command_from_env, open_host, open_on_socket,
    open_self_hosted, open_with, refusal, route, ush_binary, ush_request,
};
#[cfg(not(target_arch = "wasm32"))]
pub use screen::{Line, Screen};
#[cfg(not(target_arch = "wasm32"))]
pub use session::{PtyReadDisposition, ReaderLease, Session, SessionEnd, classify_pty_read};
pub use target::{EnvironmentSummary, SessionTarget, default_target, picker_targets};

/// A grid position: the row in the whole scrollback (history included), and the column.
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
pub struct Selection {
    pub anchor: GridPos,
    pub cursor: GridPos,
}

impl Selection {
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
}

/// The colour a cell's text or background takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CellColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// How one cell is drawn: its colours and its SGR attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent SGR style bit; grouping them would not change what they mean"
)]
pub struct Attr {
    pub fg: CellColor,
    pub bg: CellColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub dim: bool,
    /// SGR 9 — crossed out.
    pub strike: bool,
    /// The cell after a wide glyph, which draws nothing of its own.
    pub is_wide_cont: bool,
    /// The OSC 8 link a cell belongs to; `0` is none.
    pub link_id: u16,
}

/// What the process asked the terminal to do with keys, paste and the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent DEC private mode (CSI ? Ps h/l), read separately; grouping \
              them would not change what they mean"
)]
pub struct TermModes {
    /// `?25` — the cursor shows (DECTCEM).
    pub cursor_visible: bool,
    /// `?2004` — pasted text is wrapped in `ESC[200~`…`ESC[201~`.
    pub bracketed_paste: bool,
    /// `?1` — arrow keys send `ESC O A` instead of `ESC [ A` (DECCKM).
    pub app_cursor_keys: bool,
    /// `?1000`/`?1002`/`?1003` — some form of mouse reporting requested.
    pub mouse_report: bool,
    /// The active xterm mouse tracking mode: 0, 1000, 1002 or 1003.
    pub mouse_tracking: u16,
    /// `?1006` — SGR-encoded mouse coordinates requested.
    pub mouse_sgr: bool,
    /// `?1004` — report window focus in and out to the foreground program.
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

/// Text a process named (a title or a link), refused when it holds a control character or is
/// longer than the protocol allows.
#[must_use]
pub fn safe_osc_text(bytes: &[u8], max_bytes: usize) -> Option<String> {
    if bytes.is_empty() || bytes.len() > max_bytes {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    Some(text.to_string())
}
