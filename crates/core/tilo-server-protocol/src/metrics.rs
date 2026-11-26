//! The server's counter snapshot: what it has carried, in total and per workspace.

use std::collections::BTreeMap;

use crate::WorkspaceId;

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
