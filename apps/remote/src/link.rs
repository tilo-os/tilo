//! The app-level connection lifecycle for a remote session.
//!
//! The transport (SSH's `russh` channel, VNC's RFB client, or the qOS relay)
//! reports a coarse [`SessionStatus`](crate::session::SessionStatus): it is
//! connecting, connected, closed, or failed. That is the *transport's* view.
//!
//! The **viewer's** view is richer, and it is what the on-screen status
//! reflects: a link is dialling, it is up, it dropped and is being
//! re-established, the relay is unreachable, or it was cleanly disconnected.
//! [`LinkState`] is that model — a small, pure state machine folded from the
//! transport status plus the app's own reconnect policy, so the whole
//! connecting → connected → reconnecting → disconnected lifecycle is unit
//! testable with no event loop and no network.
//!
//! It also carries the two facts a real remote-desktop client surfaces that a
//! bare "connected" boolean hides: **which path** the session rides (a direct
//! peer hop, or a bounce through a relay) and **how healthy** the link is
//! (round-trip latency → a coarse quality rating). Surfacing these honestly —
//! rather than burying a relayed, high-latency link behind a green dot — is
//! the point.

use crate::session::SessionStatus;

/// How many times a dropped-but-previously-live link re-dials before the
/// viewer gives up and surfaces an honest "unreachable" error. A bounded loop
/// is deliberate: an unbounded one is the silent hang this design refuses.
pub const MAX_RECONNECT_ATTEMPTS: u32 = 5;

/// The transport path a session rides.
///
/// qOS relay sessions ride the compositor's P2P fabric: the peer is dialled
/// directly when the network allows it, and bounced through a relay when a
/// NAT/firewall blocks a direct hop. The viewer surfaces which of the two is
/// in use so a user is never left guessing why a session feels slow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayPath {
    /// Not yet established — still dialling.
    Unknown,
    /// A direct peer-to-peer connection: the lowest-latency path.
    Direct,
    /// Bounced through a named relay because the peer could not be dialled
    /// directly. Carries the relay's friendly name for display.
    Relayed { via: String },
}

impl RelayPath {
    /// Short label for the path chip (e.g. "Direct", "Relayed").
    pub fn label(&self) -> String {
        match self {
            RelayPath::Unknown => "Locating…".to_string(),
            RelayPath::Direct => "Direct P2P".to_string(),
            RelayPath::Relayed { via } => format!("Relayed · {via}"),
        }
    }

    /// Whether this path is bounced through a relay (vs a direct hop).
    pub fn is_relayed(&self) -> bool {
        matches!(self, RelayPath::Relayed { .. })
    }
}

/// A coarse link-health rating, derived from round-trip latency so the viewer
/// can draw a signal indicator without scattering raw millisecond numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkQuality {
    /// Latency unknown (not yet measured / not connected).
    Unknown,
    /// A responsive link.
    Good,
    /// Usable but noticeably laggy.
    Fair,
    /// Struggling — high latency.
    Poor,
}

impl LinkQuality {
    /// Bucket a round-trip latency in milliseconds into a rating.
    pub fn from_latency(ms: u32) -> Self {
        match ms {
            0..=59 => LinkQuality::Good,
            60..=149 => LinkQuality::Fair,
            _ => LinkQuality::Poor,
        }
    }

    /// Human label for the quality chip.
    pub fn label(self) -> &'static str {
        match self {
            LinkQuality::Unknown => "—",
            LinkQuality::Good => "Good",
            LinkQuality::Fair => "Fair",
            LinkQuality::Poor => "Poor",
        }
    }

    /// Number of filled signal bars, `0..=3`, for the indicator glyph.
    pub fn bars(self) -> u8 {
        match self {
            LinkQuality::Unknown => 0,
            LinkQuality::Poor => 1,
            LinkQuality::Fair => 2,
            LinkQuality::Good => 3,
        }
    }
}

/// The colour family a phase / state should read in — mapped to concrete theme
/// tokens by the viewer. Keeping it an enum here (not a `Color`) keeps this
/// module theme-agnostic and unit-testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkTone {
    /// Neutral / idle (disconnected, closed).
    Idle,
    /// In-progress (connecting, reconnecting).
    Pending,
    /// Healthy and live.
    Live,
    /// A real, surfaced error (unreachable).
    Error,
}

/// The app-level connection lifecycle. A superset of the transport's
/// [`SessionStatus`]: it adds the reconnect loop and an explicit terminal
/// disconnected state, and it is what the visible status reflects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkPhase {
    /// First contact: dialling and authenticating.
    Connecting,
    /// Up and streaming.
    Connected,
    /// The link dropped after being live and is being re-established. Carries
    /// the attempt number (`1..=MAX_RECONNECT_ATTEMPTS`).
    Reconnecting { attempt: u32 },
    /// The relay / peer is unreachable. Carries the real, human-readable
    /// reason — a surfaced error, never a silent hang.
    Unreachable { reason: String },
    /// Cleanly closed: the user disconnected, or the remote ended the session.
    Disconnected,
}

impl LinkPhase {
    /// A live phase keeps the host pumping frames (data may still arrive) and
    /// reads as "not an end state".
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            LinkPhase::Connecting | LinkPhase::Connected | LinkPhase::Reconnecting { .. }
        )
    }

    /// Short status label shown in the pill / bubble.
    pub fn short_label(&self) -> String {
        match self {
            LinkPhase::Connecting => "Connecting…".to_string(),
            LinkPhase::Connected => "Connected".to_string(),
            LinkPhase::Reconnecting { attempt } => {
                format!("Reconnecting… ({attempt}/{MAX_RECONNECT_ATTEMPTS})")
            }
            LinkPhase::Unreachable { .. } => "Unreachable".to_string(),
            LinkPhase::Disconnected => "Disconnected".to_string(),
        }
    }

    /// The colour family this phase reads in.
    pub fn tone(&self) -> LinkTone {
        match self {
            LinkPhase::Connecting | LinkPhase::Reconnecting { .. } => LinkTone::Pending,
            LinkPhase::Connected => LinkTone::Live,
            LinkPhase::Unreachable { .. } => LinkTone::Error,
            LinkPhase::Disconnected => LinkTone::Idle,
        }
    }
}

/// The composite link state the viewer reads every frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkState {
    phase: LinkPhase,
    path: RelayPath,
    /// Last measured round-trip latency in milliseconds, if any.
    latency_ms: Option<u32>,
    /// Whether local keyboard / pointer input is captured and forwarded to the
    /// remote (vs. released back to the local desktop).
    input_captured: bool,
}

impl LinkState {
    /// A fresh link that is dialling over `path`.
    pub fn connecting(path: RelayPath) -> Self {
        Self {
            phase: LinkPhase::Connecting,
            path,
            latency_ms: None,
            input_captured: true,
        }
    }

    /// A link staged directly in `phase` (for previews / tests / demo shots).
    pub fn staged(phase: LinkPhase, path: RelayPath, latency_ms: Option<u32>) -> Self {
        Self { phase, path, latency_ms, input_captured: true }
    }

    pub fn phase(&self) -> &LinkPhase {
        &self.phase
    }

    pub fn path(&self) -> &RelayPath {
        &self.path
    }

    pub fn latency_ms(&self) -> Option<u32> {
        self.latency_ms
    }

    pub fn input_captured(&self) -> bool {
        self.input_captured
    }

    /// Whether the link is in a live (non-terminal) phase.
    pub fn is_live(&self) -> bool {
        self.phase.is_live()
    }

    /// The quality rating derived from the current latency measurement.
    pub fn quality(&self) -> LinkQuality {
        self.latency_ms.map(LinkQuality::from_latency).unwrap_or(LinkQuality::Unknown)
    }

    /// Resolve the transport path once established (direct vs relayed).
    pub fn set_path(&mut self, path: RelayPath) {
        self.path = path;
    }

    /// Record a fresh round-trip latency sample.
    pub fn note_latency(&mut self, ms: u32) {
        self.latency_ms = Some(ms);
    }

    /// Toggle whether local input is captured for the remote. Returns the new
    /// value. Releasing capture hands the keyboard / pointer back to the local
    /// desktop without tearing the session down.
    pub fn toggle_capture(&mut self) -> bool {
        self.input_captured = !self.input_captured;
        self.input_captured
    }

    /// Fold a freshly-observed transport [`SessionStatus`] into the lifecycle.
    /// Returns `true` if the phase changed (so the caller can poke a transient
    /// status bubble and request a redraw).
    ///
    /// The mapping encodes the reconnect policy:
    /// * `Connecting` / `Connected` pass through.
    /// * A `Closed` after being live is a clean `Disconnected`; during dialling
    ///   it means the peer hung up mid-handshake → `Unreachable`.
    /// * A `Failed` while live enters the bounded reconnect loop; a `Failed`
    ///   while dialling (or after retries are exhausted) surfaces as
    ///   `Unreachable` — the honest error, not a hang.
    pub fn observe(&mut self, status: &SessionStatus) -> bool {
        let next = match status {
            SessionStatus::Connecting => {
                // A transport re-entering "connecting" while we are mid-reconnect
                // keeps the reconnect label (with its attempt count) rather than
                // dropping back to a bare "connecting".
                if matches!(self.phase, LinkPhase::Reconnecting { .. }) {
                    self.phase.clone()
                } else {
                    LinkPhase::Connecting
                }
            }
            SessionStatus::Connected => LinkPhase::Connected,
            SessionStatus::Closed => match self.phase {
                LinkPhase::Connected | LinkPhase::Reconnecting { .. } => LinkPhase::Disconnected,
                _ => LinkPhase::Unreachable {
                    reason: "The peer closed the connection before it was established.".to_string(),
                },
            },
            SessionStatus::Failed(reason) => match self.phase {
                LinkPhase::Connected => LinkPhase::Reconnecting { attempt: 1 },
                LinkPhase::Reconnecting { attempt } if attempt < MAX_RECONNECT_ATTEMPTS => {
                    LinkPhase::Reconnecting { attempt: attempt + 1 }
                }
                _ => LinkPhase::Unreachable { reason: reason.clone() },
            },
        };
        let changed = next != self.phase;
        if changed {
            if next == LinkPhase::Connected {
                // A fresh connection clears any stale latency until re-measured.
                self.latency_ms = None;
            }
            self.phase = next;
        }
        changed
    }

    /// Enter (or advance) the reconnect loop explicitly — used when the app
    /// itself decides to re-dial a dropped link. Past the attempt cap this
    /// resolves to [`LinkPhase::Unreachable`]. Returns whether the phase moved.
    pub fn begin_reconnect(&mut self, reason: impl Into<String>) -> bool {
        let next = match self.phase {
            LinkPhase::Reconnecting { attempt } if attempt >= MAX_RECONNECT_ATTEMPTS => {
                LinkPhase::Unreachable { reason: reason.into() }
            }
            LinkPhase::Reconnecting { attempt } => LinkPhase::Reconnecting { attempt: attempt + 1 },
            LinkPhase::Unreachable { .. } | LinkPhase::Disconnected => {
                // A manual retry from a terminal state starts a fresh attempt.
                LinkPhase::Reconnecting { attempt: 1 }
            }
            _ => LinkPhase::Reconnecting { attempt: 1 },
        };
        let changed = next != self.phase;
        self.phase = next;
        changed
    }

    /// Mark the link cleanly disconnected (user pressed Disconnect, or the
    /// remote ended it). Always lands in the terminal [`LinkPhase::Disconnected`].
    pub fn disconnect(&mut self) {
        self.phase = LinkPhase::Disconnected;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_buckets_by_latency() {
        assert_eq!(LinkQuality::from_latency(20), LinkQuality::Good);
        assert_eq!(LinkQuality::from_latency(90), LinkQuality::Fair);
        assert_eq!(LinkQuality::from_latency(400), LinkQuality::Poor);
        assert_eq!(LinkQuality::Good.bars(), 3);
        assert_eq!(LinkQuality::Unknown.bars(), 0);
    }

    #[test]
    fn happy_path_connecting_then_connected() {
        let mut link = LinkState::connecting(RelayPath::Unknown);
        assert_eq!(*link.phase(), LinkPhase::Connecting);
        assert!(link.is_live());
        assert!(link.observe(&SessionStatus::Connected), "phase advanced to connected");
        assert_eq!(*link.phase(), LinkPhase::Connected);
        assert_eq!(link.phase().tone(), LinkTone::Live);
        // Idempotent: observing the same status again does not report a change.
        assert!(!link.observe(&SessionStatus::Connected));
    }

    #[test]
    fn live_drop_enters_bounded_reconnect_then_unreachable() {
        let mut link = LinkState::connecting(RelayPath::Direct);
        link.observe(&SessionStatus::Connected);
        // First failure after being live → reconnecting (attempt 1).
        assert!(link.observe(&SessionStatus::Failed("reset".into())));
        assert_eq!(*link.phase(), LinkPhase::Reconnecting { attempt: 1 });
        assert_eq!(link.phase().tone(), LinkTone::Pending);
        // Repeated failures climb the attempt counter, capped, then surface an
        // honest unreachable error — never an unbounded silent retry.
        for _ in 0..MAX_RECONNECT_ATTEMPTS + 2 {
            link.observe(&SessionStatus::Failed("reset".into()));
        }
        assert!(matches!(link.phase(), LinkPhase::Unreachable { .. }));
        assert_eq!(link.phase().tone(), LinkTone::Error);
    }

    #[test]
    fn reconnect_recovers_to_connected() {
        let mut link = LinkState::connecting(RelayPath::Direct);
        link.observe(&SessionStatus::Connected);
        link.observe(&SessionStatus::Failed("blip".into()));
        assert!(matches!(link.phase(), LinkPhase::Reconnecting { .. }));
        // A transport re-entering "connecting" keeps the reconnect label…
        assert!(!link.observe(&SessionStatus::Connecting));
        assert!(matches!(link.phase(), LinkPhase::Reconnecting { .. }));
        // …and a recovered transport lands back on connected.
        assert!(link.observe(&SessionStatus::Connected));
        assert_eq!(*link.phase(), LinkPhase::Connected);
    }

    #[test]
    fn failure_while_dialling_is_unreachable_not_reconnect() {
        // A relay that is down from the start must not spin in reconnect — it
        // surfaces immediately as unreachable.
        let mut link = LinkState::connecting(RelayPath::Relayed { via: "eu-relay".into() });
        assert!(link.observe(&SessionStatus::Failed("relay unreachable".into())));
        match link.phase() {
            LinkPhase::Unreachable { reason } => assert!(reason.contains("relay")),
            other => panic!("expected unreachable, got {other:?}"),
        }
    }

    #[test]
    fn disconnect_is_terminal() {
        let mut link = LinkState::connecting(RelayPath::Direct);
        link.observe(&SessionStatus::Connected);
        link.disconnect();
        assert_eq!(*link.phase(), LinkPhase::Disconnected);
        assert!(!link.is_live());
        assert_eq!(link.phase().tone(), LinkTone::Idle);
    }

    #[test]
    fn latency_drives_quality_and_path_is_surfaced() {
        let mut link = LinkState::connecting(RelayPath::Unknown);
        assert_eq!(link.quality(), LinkQuality::Unknown);
        link.set_path(RelayPath::Relayed { via: "eu-1".into() });
        assert!(link.path().is_relayed());
        assert_eq!(link.path().label(), "Relayed · eu-1");
        link.note_latency(42);
        assert_eq!(link.quality(), LinkQuality::Good);
    }

    #[test]
    fn capture_toggles_without_tearing_down() {
        let mut link = LinkState::connecting(RelayPath::Direct);
        link.observe(&SessionStatus::Connected);
        assert!(link.input_captured(), "capture on by default while connected");
        assert!(!link.toggle_capture(), "released");
        assert!(link.is_live(), "releasing capture does not end the session");
        assert!(link.toggle_capture(), "re-captured");
    }

    #[test]
    fn manual_retry_from_unreachable_restarts_attempts() {
        let mut link = LinkState::staged(
            LinkPhase::Unreachable { reason: "down".into() },
            RelayPath::Relayed { via: "eu-1".into() },
            None,
        );
        assert!(link.begin_reconnect("retry"));
        assert_eq!(*link.phase(), LinkPhase::Reconnecting { attempt: 1 });
    }
}
