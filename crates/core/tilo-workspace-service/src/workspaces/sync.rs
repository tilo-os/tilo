//! Where each workspace this device is a member of stands with the device serving it, as a
//! device's daemon publishes it for its person's shell: what an app shows as it opens ("Connecting
//! to Lounge…"), and when the workspace's first sync has completed.
//!
//! A member workspace's link to its host is the daemon's, so only the daemon knows this: it follows
//! the link of each member workspace it keeps and publishes the states here as one reading. A
//! workspace this device hosts has no entry: its apps' empty states are their own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The resource under which a device's daemon publishes where its member workspaces stand.
pub const RESOURCE: &str = "so.tilo.device/workspace-sync";

/// Where one of this device's member workspaces stands with its host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Its first sync with its host has not completed yet.
    #[default]
    Connecting,
    /// Its host is reached and this device has what the host served.
    Synced,
    /// It synced before, and its host is not reached now.
    Unreachable,
}

/// One member workspace of this device's, as the daemon publishes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub status: Status,
    /// This device's writes to it that are not with its host yet.
    #[serde(default)]
    pub unsynced_writes: u32,
}

/// Every member workspace of this device's, by its id, as the daemon publishes them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspaces {
    #[serde(default)]
    pub workspaces: BTreeMap<String, Workspace>,
}

impl Workspaces {
    /// Where `workspace` (its id) stands, when this device is a member of it.
    #[must_use]
    pub fn get(&self, workspace: &str) -> Option<&Workspace> {
        self.workspaces.get(workspace)
    }
}
