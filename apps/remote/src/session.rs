//! Session surfaces — the live area you see after connecting.
//!
//! There are two fundamentally different surfaces:
//!
//! * **Terminal** (SSH): a scrollback grid of styled cells rendered with
//!   the toolkit's monospace text path and ANSI palette. The cell model
//!   lives here ([`Cell`] / [`TermLine`]); the bytes that fill it come from
//!   a remote PTY, parsed by [`crate::vt`].
//!
//! * **Framebuffer** (VNC): a rectangular pixel surface. The remote
//!   framebuffer is decoded by [`crate::rfb`] and painted straight into the
//!   scene — no synthetic desktop.
//!
//! The byte transport is real: [`crate::ssh`] speaks SSH over `russh`,
//! [`crate::vnc`] speaks RFB. Those app-owned transports deliberately remain
//! behind an explicit development capability: the packaged Remote sandbox has
//! no arbitrary egress and SSH host-key trust still needs a per-profile policy.
//! Tilo device control uses the production shell broker and is available without
//! that opt-in.

use crate::model::{Connection, ConnectionKind};

/// What Remote can do with a saved connection *in this process*.
///
/// This is intentionally more expressive than a boolean. It is used by the
/// on-screen list, session recovery card, Ctrl+K resources, and agents, so an
/// unavailable action always explains whether the missing piece is a local
/// capability or an absent protocol backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportReadiness {
    /// A production or explicitly opted-in transport can be opened now.
    Ready,
    /// The protocol implementation exists, but this process has not been
    /// granted the deliberately explicit live-session capability.
    NeedsLiveSessionCapability,
    /// The persisted profile is retained for compatibility, but Tilo has no
    /// implementation for this protocol.
    BackendUnavailable,
    /// The saved profile does not describe a target/authentication pair this
    /// app can safely hand to a transport.
    InvalidProfile,
}

impl TransportReadiness {
    pub fn can_connect(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Short state label that fits alongside a protocol badge in a list row.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::NeedsLiveSessionCapability => "Needs live access",
            Self::BackendUnavailable => "Not supported",
            Self::InvalidProfile => "Fix profile",
        }
    }

    /// Stable machine-readable availability code for resources and agents.
    pub fn code(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NeedsLiveSessionCapability => "live_session_capability_required",
            Self::BackendUnavailable => "transport_backend_unavailable",
            Self::InvalidProfile => "invalid_connection_profile",
        }
    }

    /// Concise explanation shown without implying that a connection was tried.
    pub fn detail(self, kind: ConnectionKind) -> &'static str {
        match self {
            Self::Ready => "This transport is ready to connect.",
            Self::NeedsLiveSessionCapability => match kind {
                ConnectionKind::Ssh => {
                    "SSH is installed, but this build has not been granted live network access."
                }
                ConnectionKind::Vnc => {
                    "VNC is installed, but this build has not been granted live network access."
                }
                _ => "This transport needs a capability before it can connect.",
            },
            Self::BackendUnavailable => {
                "RDP profiles are preserved, but this Tilo build has no RDP client backend."
            }
            Self::InvalidProfile => {
                "This saved connection has incompatible target or authentication settings."
            }
        }
    }
}

/// Whether app-owned SSH/VNC sessions are enabled. Off by default so those
/// transports do not touch the network until a developer opts in. Tilo device
/// control does not use this switch.
pub fn live_sessions_enabled() -> bool {
    std::env::var("REMOTE_LIVE_SESSIONS").is_ok()
}

/// Resolve a protocol's current, truthful readiness. Keep this at the
/// transport boundary rather than duplicating environment checks in widgets,
/// resources, and keyboard handlers.
pub fn transport_readiness(kind: ConnectionKind) -> TransportReadiness {
    match kind {
        ConnectionKind::Tilo => TransportReadiness::Ready,
        ConnectionKind::Ssh | ConnectionKind::Vnc if live_sessions_enabled() => {
            TransportReadiness::Ready
        }
        ConnectionKind::Ssh | ConnectionKind::Vnc => TransportReadiness::NeedsLiveSessionCapability,
        ConnectionKind::Rdp => TransportReadiness::BackendUnavailable,
    }
}

/// Resolve whether one saved profile may begin a connection attempt now.
///
/// Every invocation path uses this one decision: the visible connection row,
/// the accessibility tree, Ctrl+K/agent resources, and the effect handler. A
/// malformed profile therefore never reaches an I/O constructor merely because
/// one surface forgot a preflight check. Cached `Presence::Offline` is not an
/// availability gate: it is stale by design, so a deliberate reconnect is
/// required to discover that a machine recovered.
pub fn connection_readiness(connection: &Connection) -> TransportReadiness {
    let transport = transport_readiness(connection.kind);
    if !transport.can_connect() {
        return transport;
    }
    if connection.profile_issue().is_some() {
        return TransportReadiness::InvalidProfile;
    }
    TransportReadiness::Ready
}

/// Whether this connection kind may start its transport now. Tilo is a
/// production broker capability; SSH/VNC retain the explicit developer gate.
pub fn can_attempt_live(kind: ConnectionKind) -> bool {
    transport_readiness(kind).can_connect()
}

/// The state of a live transport, shared from the background I/O thread to
/// the UI thread. The UI reads this every frame to choose what to draw:
/// a spinner while connecting, the session surface once up, or the error text
/// on failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    /// Handshake / auth in flight.
    Connecting,
    /// Transport up; the surface is live.
    Connected,
    /// The remote closed the session (clean exit).
    Closed,
    /// No connection was attempted because a local prerequisite is absent.
    /// This must never be presented as a network or authentication failure.
    Unavailable(String),
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
    use crate::model::Presence;
    use uuid::Uuid;

    fn connection(kind: ConnectionKind) -> Connection {
        Connection {
            id: Uuid::new_v4(),
            name: "Saved machine".into(),
            host: iroh_base::SecretKey::generate().public().to_string(),
            port: kind.default_port(),
            user: String::new(),
            kind,
            auth: if kind == ConnectionKind::Tilo {
                crate::model::AuthMethod::DeviceIdentity
            } else {
                crate::model::AuthMethod::Password
            },
            last_used: "Never".into(),
            presence: Presence::Unknown,
            group: String::new(),
            favorite: false,
        }
    }

    #[test]
    fn session_readiness_and_preflight_fail_closed() {
        eprintln!("phase tilo_broker_transport_is_production_on");
        {
            assert!(can_attempt_live(ConnectionKind::Tilo));
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

        eprintln!("phase readiness_distinguishes_a_missing_capability_from_a_missing_backend");
        {
            assert_eq!(
                transport_readiness(ConnectionKind::Rdp),
                TransportReadiness::BackendUnavailable
            );
            assert_eq!(
                transport_readiness(ConnectionKind::Tilo),
                TransportReadiness::Ready
            );
            assert_eq!(
                transport_readiness(ConnectionKind::Ssh).can_connect(),
                live_sessions_enabled()
            );
        }

        eprintln!(
            "phase connection_preflight_fails_closed_for_bad_profiles_but_not_cached_presence"
        );
        {
            let mut tilo = connection(ConnectionKind::Tilo);
            tilo.host = "friendly-name".into();
            assert_eq!(
                connection_readiness(&tilo),
                TransportReadiness::InvalidProfile
            );

            let mut offline = connection(ConnectionKind::Tilo);
            offline.presence = crate::model::Presence::Offline;
            assert_eq!(connection_readiness(&offline), TransportReadiness::Ready);
        }
    }
}

/// One styled terminal cell. We keep this tiny — a glyph plus a palette
/// index — because an ANSI grid is thousands of these.
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
