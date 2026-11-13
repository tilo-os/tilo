//! Session surfaces — the live area you see after connecting.

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
    NeedsLiveSessionCapability,
    /// The persisted profile is retained for compatibility, but Tilo has no
    BackendUnavailable,
    /// The saved profile does not describe a target/authentication pair this
    InvalidProfile,
}

impl TransportReadiness {
    pub fn can_connect(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// The message naming this state in the meta column of a list row.
    pub fn label_message(self) -> &'static str {
        match self {
            Self::Ready => "remote-readiness-ready",
            Self::NeedsLiveSessionCapability => "remote-readiness-elsewhere",
            Self::BackendUnavailable => "remote-readiness-no-rdp",
            Self::InvalidProfile => "remote-readiness-editing",
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

    /// The catalogue message saying what this state means for the person,
    pub fn detail_message(self, kind: ConnectionKind) -> &'static str {
        match self {
            Self::Ready => "remote-state-ready",
            Self::NeedsLiveSessionCapability => match kind {
                ConnectionKind::Ssh => "remote-state-no-ssh",
                ConnectionKind::Vnc => "remote-state-no-vnc",
                _ => "remote-state-no-session",
            },
            Self::BackendUnavailable => "remote-state-no-rdp",
            Self::InvalidProfile => "remote-state-invalid-profile",
        }
    }
}

/// Whether app-owned SSH/VNC sessions are enabled. Off by default so those
pub fn live_sessions_enabled() -> bool {
    std::env::var("REMOTE_LIVE_SESSIONS").is_ok()
}

/// Resolve a protocol's current, truthful readiness. Keep this at the
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
/// one surface forgot a preflight check. Whether this device last saw the
/// machine offline is not an availability gate: it is stale by design, so a
/// deliberate reconnect is required to discover that a machine recovered.
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
    use ui::tilo_resource::Atom;
    use uuid::Uuid;

    fn connection(kind: ConnectionKind) -> Connection {
        Connection {
            id: Uuid::new_v4(),
            name: "Saved machine".into(),
            host: Atom(iroh_base::SecretKey::generate().public().to_string()),
            port: kind.default_port(),
            user: Atom::default(),
            kind,
            auth: if kind == ConnectionKind::Tilo {
                crate::model::AuthMethod::DeviceIdentity
            } else {
                crate::model::AuthMethod::Password
            },
            last_used_ms: None,
            group: Atom::default(),
            favorite: false,
        }
    }

    #[test]
    fn session_readiness_and_preflight_fail_closed() {
        eprintln!("phase tilo_broker_transport_is_production_on");
        {
            assert!(transport_readiness(ConnectionKind::Tilo).can_connect());
            assert!(!transport_readiness(ConnectionKind::Rdp).can_connect());
            assert_eq!(
                transport_readiness(ConnectionKind::Ssh).can_connect(),
                live_sessions_enabled()
            );
            assert_eq!(
                transport_readiness(ConnectionKind::Vnc).can_connect(),
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
            tilo.host = Atom("friendly-name".into());
            assert_eq!(
                connection_readiness(&tilo),
                TransportReadiness::InvalidProfile
            );
        }
    }
}

/// One styled terminal cell. We keep this tiny — a glyph plus a palette
/// index — because an ANSI grid is thousands of these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub ch: char,
    /// Index into the toolkit's 16-colour ANSI palette, or `None` for the
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TermLine {
    pub cells: Vec<Cell>,
}
