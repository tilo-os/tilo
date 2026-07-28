//! Session surfaces — the live area you see after connecting.
//!
//! There are two fundamentally different surfaces:
//!
//! * **Terminal** (SSH): a scrollback grid of styled cells rendered with
//!   the toolkit's monospace text path and ANSI palette. The cell model
//!   lives here ([`Cell`] / [`TermLine`]); the bytes that fill it come from
//!   a real remote PTY, parsed by [`crate::vt`].
//!
//! * **Framebuffer** (VNC): a rectangular pixel surface. The real remote
//!   framebuffer is decoded by [`crate::rfb`] and painted straight into the
//!   scene — no synthetic desktop.
//!
//! The byte transport is real: [`crate::ssh`] speaks SSH over `russh`,
//! [`crate::vnc`] speaks RFB. A live connection is still gated behind
//! [`live_sessions_enabled`] so the *default* build never touches the
//! network and the session surface stays the honest "not connected" card
//! until the user opts in.

/// Whether live network sessions are enabled. Off by default so the app
/// never blocks a render on the network and the session surface is the
/// honest "not connected" card. Set `REMOTE_LIVE_SESSIONS` (to anything) to
/// let Connect open a *real* SSH / VNC session.
pub fn live_sessions_enabled() -> bool {
    std::env::var("REMOTE_LIVE_SESSIONS").is_ok()
}

/// The state of a live transport, shared from the background I/O thread to
/// the UI thread. The UI reads this every frame to choose what to draw:
/// a spinner while connecting, the real surface once up, or the honest
/// error text on failure — never a fabricated session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    /// Handshake / auth in flight.
    Connecting,
    /// Transport up; the surface is live.
    Connected,
    /// The remote closed the session (clean exit).
    Closed,
    /// The connection or auth failed. Carries the real, human-readable
    /// reason (e.g. "Connection refused", "Authentication failed").
    Failed(String),
}

impl SessionStatus {
    pub fn is_live(&self) -> bool {
        matches!(self, SessionStatus::Connecting | SessionStatus::Connected)
    }
}

/// One styled terminal cell. We keep this tiny — a glyph plus a palette
/// index — because a real ANSI grid is thousands of these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub ch: char,
    /// Index into the toolkit's 16-colour ANSI palette, or `None` for the
    /// default foreground.
    pub color: Option<usize>,
    pub bold: bool,
}

impl Cell {
    pub fn blank() -> Self {
        Self { ch: ' ', color: None, bold: false }
    }
}

/// A styled run of cells making up one on-screen row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TermLine {
    pub cells: Vec<Cell>,
}
