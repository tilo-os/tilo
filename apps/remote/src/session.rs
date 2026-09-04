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
//! [`crate::vnc`] speaks RFB. Those app-owned network transports stay gated
//! behind [`live_sessions_enabled`]; qOS device control uses the production
//! shell broker and is available without the developer opt-in.

use crate::model::ConnectionKind;

/// Whether app-owned SSH/VNC sessions are enabled. Off by default so those
/// transports do not touch the network until a developer opts in. qOS device
/// control does not use this switch.
pub fn live_sessions_enabled() -> bool {
    std::env::var("REMOTE_LIVE_SESSIONS").is_ok()
}

/// Whether this connection kind may start its real transport now. qOS is a
/// production broker capability; SSH/VNC retain the explicit developer gate.
pub fn can_attempt_live(kind: ConnectionKind) -> bool {
    match kind {
        ConnectionKind::Qos => true,
        ConnectionKind::Ssh | ConnectionKind::Vnc => live_sessions_enabled(),
        ConnectionKind::Rdp => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qos_broker_transport_is_production_on() {
        assert!(can_attempt_live(ConnectionKind::Qos));
        assert!(!can_attempt_live(ConnectionKind::Rdp));
        assert_eq!(
            can_attempt_live(ConnectionKind::Ssh),
            live_sessions_enabled()
        );
        assert_eq!(
            can_attempt_live(ConnectionKind::Vnc),
            live_sessions_enabled()
        );
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
        Self {
            ch: ' ',
            color: None,
            bold: false,
        }
    }
}

/// A styled run of cells making up one on-screen row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TermLine {
    pub cells: Vec<Cell>,
}
