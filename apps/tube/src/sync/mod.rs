//! Subscription and watch-state sync with Grayjay over its own "Sync" protocol: the Noise Protocol
//! Framework on TCP (port 12315, QR or manual pairing, relay fallback).

pub mod protocol;

#[cfg(feature = "grayjay-sync")]
mod lifecycle;

#[cfg(feature = "grayjay-sync")]
pub mod transport;

#[cfg(feature = "grayjay-sync")]
pub use lifecycle::state_from_model as lifecycle_state;
#[cfg(feature = "grayjay-sync")]
pub use lifecycle::{DeviceSync, PairedDevices, PeerRecord, SyncStatus, SyncView};

#[cfg(not(feature = "grayjay-sync"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyncStatus {
    #[default]
    Unavailable,
}

#[cfg(not(feature = "grayjay-sync"))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncView {
    pub status: SyncStatus,
    pub summary: String,
    pub device_public_key: Option<String>,
    pub peers: Vec<PeerRecord>,
}

#[cfg(not(feature = "grayjay-sync"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRecord {
    pub id: String,
    pub name: String,
    pub pending_approval: bool,
    pub revoked_at: Option<i64>,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
}

#[cfg(not(feature = "grayjay-sync"))]
impl PeerRecord {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
    pub fn short_id(&self) -> String {
        self.id.chars().take(10).collect()
    }
}

use crate::model::{Settings, VideoId};

pub fn normalized_subscription_url(value: &str) -> Option<String> {
    const PREFIXES: [&str; 3] = [
        "https://youtube.com/",
        "https://www.youtube.com/",
        "https://m.youtube.com/",
    ];
    let value = value.trim();
    let path = PREFIXES
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix))?;
    (!path.is_empty() && path.len() <= 232 && !path.chars().any(char::is_control))
        .then(|| format!("https://youtube.com/{path}"))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncState {
    pub subscriptions: Vec<String>,
    pub watch_history: Vec<VideoId>,
    pub settings: Option<Settings>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PeerId(pub String);

#[derive(Clone, Debug, PartialEq)]
pub enum SyncError {
    Unavailable(String),
    Pairing(String),
    Transport(String),
    KeyUnavailable(String),
    Persistence(String),
    Revoked,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Unavailable(message) => write!(f, "Grayjay sync unavailable: {message}"),
            SyncError::Pairing(m) => write!(f, "pairing failed: {m}"),
            SyncError::Transport(m) => write!(f, "transport error: {m}"),
            SyncError::KeyUnavailable(m) => write!(f, "device key unavailable: {m}"),
            SyncError::Persistence(m) => write!(f, "peer store error: {m}"),
            SyncError::Revoked => write!(f, "peer was revoked on this device"),
        }
    }
}

impl std::error::Error for SyncError {}

pub type SyncResult<T> = Result<T, SyncError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncCapability {
    pub available: bool,
    /// The catalogue message saying whether sync is available and what it needs.
    pub summary_key: &'static str,
}

/// Product-facing status for device sync. Availability still depends on the host secret broker at
/// runtime; [`DeviceSync::production`] fails closed when that broker cannot provide app-scoped key
/// material.
pub const fn capability() -> SyncCapability {
    #[cfg(feature = "grayjay-sync")]
    return SyncCapability {
        available: true,
        summary_key: "tube-sync-available-note",
    };
    #[cfg(not(feature = "grayjay-sync"))]
    SyncCapability {
        available: false,
        summary_key: "tube-sync-not-built",
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerDescriptor {
    pub public_key: String,
    pub addresses: Vec<String>,
    pub port: i32,
}

pub trait SyncTransport: Send {
    fn pair(&mut self, pairing_blob: &str) -> SyncResult<PeerId>;

    fn push(&mut self, peer: &PeerId, state: &SyncState) -> SyncResult<()>;

    fn pull(&mut self, peer: &PeerId) -> SyncResult<SyncState>;

    fn restore_peer(&mut self, _peer: &PeerDescriptor) -> SyncResult<()> {
        Err(SyncError::Unavailable(
            "this sync transport cannot restore peer routes".into(),
        ))
    }

    fn forget_peer(&mut self, _peer: &PeerId) {}

    fn peer_descriptor(&self, _peer: &PeerId) -> Option<PeerDescriptor> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_urls_converge_without_accepting_other_sites() {
        assert_eq!(
            normalized_subscription_url("https://www.youtube.com/@tilo"),
            Some("https://youtube.com/@tilo".into())
        );
        assert_eq!(
            normalized_subscription_url("https://m.youtube.com/channel/UC123"),
            Some("https://youtube.com/channel/UC123".into())
        );
        assert_eq!(
            normalized_subscription_url("https://example.com/@tilo"),
            None
        );
        assert_eq!(
            normalized_subscription_url("https://youtube.com/\nunsafe"),
            None
        );
    }
}
