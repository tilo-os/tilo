//! Where a caller's workspace and this device's writes stand on the session that serves it,
//! typed: what a program waits for instead of asking again on a timer.

use serde::{Deserialize, Serialize};

/// Where a joined workspace stands on this device's session with its host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceState {
    /// The session holds no connection to the workspace's host right now.
    Connecting,
    /// Connected and subscribed: the host has not said the workspace is ready yet.
    Admitting,
    /// The host said it is ready: what it replayed is in this device's replica, and what this
    /// device writes goes through.
    Open,
    /// Open, and the workspace's governance admits this device: its records read as a
    /// member's and its apps answer this device's actions.
    Serving,
}

/// Whether what this device wrote is with its host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SyncState {
    /// Every write this device made is durable on its host.
    Done,
    /// `pending` writes wait for their host's answer.
    Behind { pending: usize },
}

/// Where the caller's workspace and this device's writes stand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub workspace: WorkspaceState,
    pub sync: SyncState,
}
