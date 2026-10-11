//! The relay's persisted counter snapshot.
//!
//! The daemon periodically writes its counters to
//! `<state_root>/metrics.json`; offline status commands and Settings read the
//! file back to tell whether the relay is alive and what it has carried.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use thiserror::Error;

/// Last daemon counter snapshot, consumed by offline admin/status commands.
pub const METRICS_FILE: &str = "metrics.json";

/// Immutable copy of every relay counter for status reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MetricsSnapshot {
    /// Connections accepted since start (pre-auth).
    pub conns_accepted: u64,
    /// Connections that completed the auth handshake successfully.
    pub conns_authed: u64,
    /// Connections rejected by auth (bad cert, wrong user, revoked).
    pub auth_rejected: u64,
    /// Total deltas seen (across all workspaces).
    pub deltas_received: u64,
    /// Total delta bytes queued to disk for offline peers.
    pub bytes_queued: u64,
    /// Total deltas fanned out to live peers.
    pub deltas_fanned_out: u64,
    /// Workspace subscriptions accepted.
    pub subscribes: u64,
    /// Retained envelopes replayed to subscribers.
    pub replayed_retained: u64,
    /// Pending (offline-queued) envelopes replayed to reconnecting peers.
    pub replayed_pending: u64,
    /// Live subscribers dropped for any reason.
    pub subscriber_dropped: u64,
    /// Live subscribers dropped because they could not keep up.
    pub subscriber_dropped_backpressure: u64,
    /// Egress: bytes written to authenticated peer sessions, framed, before
    /// QUIC. Counts everything a session is sent: live fan-out, replay,
    /// replies, positions and private-action deliveries. Zero in snapshots
    /// from relays that predate the counter.
    #[serde(default)]
    pub bytes_sent: u64,
    /// The part of [`Self::bytes_sent`] that live fan-out wrote: deltas and
    /// presence forwarded to subscribers as they arrived, not replay.
    #[serde(default)]
    pub bytes_sent_live: u64,
    /// Egress per workspace (relay room), for the frames that belong to one.
    /// Lists only rooms that were sent something since the daemon started.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rooms: BTreeMap<u64, RoomEgress>,
}

/// Bytes one relay room's frames put on the wire since the daemon started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RoomEgress {
    /// Every frame of the room written to a peer session.
    pub bytes_sent: u64,
    /// The room's live fan-out frames (deltas and presence), not replay.
    pub bytes_sent_live: u64,
}

/// Failure reading or writing [`METRICS_FILE`].
#[derive(Debug, Error)]
pub enum MetricsFileError {
    /// I/O failure.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// The snapshot could not be JSON-encoded or decoded.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Atomically persist a daemon metrics snapshot.
///
/// # Errors
///
/// Returns an error if the state root can't be created, `snapshot`
/// can't be JSON-encoded, or the atomic write fails.
pub fn save_metrics(state_root: &Path, snapshot: &MetricsSnapshot) -> Result<(), MetricsFileError> {
    std::fs::create_dir_all(state_root)?;
    let bytes = serde_json::to_vec_pretty(snapshot)?;
    tilo_files::native::atomic::write(&state_root.join(METRICS_FILE), &bytes)?;
    Ok(())
}

/// Read the last daemon metrics snapshot, if the daemon has written one.
///
/// # Errors
///
/// Returns an error if the file exists but can't be read, or fails to
/// decode as JSON.
pub fn load_metrics(state_root: &Path) -> Result<Option<MetricsSnapshot>, MetricsFileError> {
    let path = state_root.join(METRICS_FILE);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&std::fs::read(path)?)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_through_the_state_root() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load_metrics(tmp.path()).unwrap(), None);
        let snapshot = MetricsSnapshot {
            conns_accepted: 3,
            deltas_fanned_out: 9,
            bytes_sent: 1_200,
            bytes_sent_live: 900,
            rooms: BTreeMap::from([(
                7,
                RoomEgress {
                    bytes_sent: 1_000,
                    bytes_sent_live: 900,
                },
            )]),
            ..MetricsSnapshot::default()
        };
        save_metrics(tmp.path(), &snapshot).unwrap();
        assert_eq!(load_metrics(tmp.path()).unwrap(), Some(snapshot));
    }
}
