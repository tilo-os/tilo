//! The server's persisted counter snapshot.
//!
//! The daemon periodically writes its counters to
//! `<state_root>/metrics.json`; offline status commands and Settings read the
//! file back to tell whether the server is alive and what it has carried.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::WorkspaceId;
use thiserror::Error;

/// Last daemon counter snapshot, consumed by offline admin/status commands.
pub const METRICS_FILE: &str = "metrics.json";

/// The server's own state root below the daemon's state root (`state_root` in
/// its server configuration): it keeps [`METRICS_FILE`] there. A workspace host
/// reads the snapshot from here.
#[must_use]
pub fn workspace_server_root(workspace_state: &Path) -> PathBuf {
    workspace_state.join("server")
}

/// A folder of the name the server's own folder had in earlier builds
/// ([`crate::earlier::SERVER_ROOT`]), found while this build's folder is not
/// there yet: under `workspace_state`, or beside it when `workspace_state` is
/// itself a folder of the present name, as the image's start script passes it.
///
/// Nothing of it is used. Whoever starts a server there says so, so that a
/// server which comes up empty beside what an earlier build kept names where
/// that lies.
#[must_use]
pub fn earlier_server_root_beside(workspace_state: &Path) -> Option<PathBuf> {
    if workspace_server_root(workspace_state).exists() {
        return None;
    }
    let under = workspace_state.join(crate::earlier::SERVER_ROOT);
    let named_as_now = workspace_state.file_name() == Some(std::ffi::OsStr::new("server"));
    let beside = named_as_now.then(|| workspace_state.with_file_name(crate::earlier::SERVER_ROOT));
    [Some(under), beside]
        .into_iter()
        .flatten()
        .find(|earlier| earlier.is_dir())
}

/// Where a connector worker kept its counters as Prometheus text, one
/// `<app>.prom` each, below the state root of the server that ran it. The
/// daemon runs no connector worker, so nothing writes here now.
#[must_use]
pub fn workspace_textfile_dir(workspace_state: &Path) -> PathBuf {
    workspace_state.join("metrics.d")
}

/// Immutable copy of every server counter for status reports.
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
    /// from servers that predate the counter.
    #[serde(default)]
    pub bytes_sent: u64,
    /// The part of [`Self::bytes_sent`] that live fan-out wrote: deltas and
    /// presence forwarded to subscribers as they arrived, not replay.
    #[serde(default)]
    pub bytes_sent_live: u64,
    /// Documents read out of the workspaces this device or server keeps: one per
    /// capture of a record's document, whether it answered the whole record, the
    /// part of it past the revision the reader already has, or that nothing
    /// changed. A refresh that reads only what changed shows here as the
    /// documents it read, not as every record in its workspace. Zero in
    /// snapshots from servers that predate the counter.
    #[serde(default)]
    pub documents_read: u64,
    /// Peer sessions connected when the snapshot was taken (a gauge, unlike
    /// the counters above).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sessions_open: u64,
    /// Egress per workspace, for the frames that belong to one. Lists only
    /// workspaces that were sent something since the daemon started. It keeps
    /// the key `workspaces`, which status readers and dashboards read.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        rename = "workspaces"
    )]
    pub workspaces: BTreeMap<WorkspaceId, WorkspaceEgress>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if passes a reference"
)]
fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// Bytes one server workspace's frames put on the wire since the daemon started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceEgress {
    /// Every frame of the workspace written to a peer session.
    pub bytes_sent: u64,
    /// The workspace's live fan-out frames (deltas and presence), not replay.
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
            workspaces: BTreeMap::from([(
                WorkspaceId::from_u128(7),
                WorkspaceEgress {
                    bytes_sent: 1_000,
                    bytes_sent_live: 900,
                },
            )]),
            ..MetricsSnapshot::default()
        };
        save_metrics(tmp.path(), &snapshot).unwrap();
        assert_eq!(load_metrics(tmp.path()).unwrap(), Some(snapshot));
    }

    /// A server that would start empty beside what an earlier build kept is
    /// told where that lies; one with a folder of its own is told nothing.
    #[test]
    fn a_folder_of_the_earlier_name_is_named_while_this_builds_is_not_there() {
        let state = tempfile::tempdir().unwrap();
        assert_eq!(earlier_server_root_beside(state.path()), None);
        let under = state.path().join(crate::earlier::SERVER_ROOT);
        std::fs::create_dir(&under).unwrap();
        assert_eq!(earlier_server_root_beside(state.path()), Some(under));

        // As the image's start script passes it: `--state` is itself a folder
        // of the present name, and the earlier one lies beside it.
        let above = tempfile::tempdir().unwrap();
        let given = above.path().join("server");
        let beside = above.path().join(crate::earlier::SERVER_ROOT);
        std::fs::create_dir(&given).unwrap();
        std::fs::create_dir(&beside).unwrap();
        assert_eq!(earlier_server_root_beside(&given), Some(beside));

        // Once this build's own folder is there, nothing is said.
        std::fs::create_dir(given.join("server")).unwrap();
        assert_eq!(earlier_server_root_beside(&given), None);
        std::fs::create_dir(state.path().join("server")).unwrap();
        assert_eq!(earlier_server_root_beside(state.path()), None);
    }
}
