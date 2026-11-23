//! The files of a workspace whose work this device takes, held as a replica of the workspace's
//! own Files record ([`replica`]) and shown at a folder where its shells run.

mod replica;

pub(crate) use replica::{ReplicaHost, Replicas};

/// The workspaces' files, once they are held: a member taking a workspace's work is set up
/// before the broker their records travel on runs. Held weakly: the shells that ask for a
/// workspace's folder keep no replica alive past the daemon that holds them.
pub(crate) type ReplicaSlot = Arc<std::sync::OnceLock<std::sync::Weak<Replicas>>>;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// The profile directory holding the workspace files' state.
const FILES: &str = "files";
const NAMESPACE: &str = "namespace";
/// Workspace changes reach the folder, and are persisted, within this.
const INTERVAL: Duration = Duration::from_secs(2);

/// Who the workspace attributes this host's versions to, and the device
/// they come from.
#[derive(Clone, Copy)]
pub(crate) struct Author {
    pub(crate) person: [u8; 32],
    pub(crate) device: [u8; 32],
}

/// The folder of the workspace named `workspace`, below `home`. A name
/// that is not a plain folder name is replaced, never interpreted.
pub(crate) fn folder_in(home: &Path, workspace: &str) -> PathBuf {
    let plain =
        !workspace.is_empty() && !workspace.starts_with('.') && !workspace.contains(['/', '\0']);
    home.join("Tilo")
        .join(if plain { workspace } else { "Workspace" })
}
