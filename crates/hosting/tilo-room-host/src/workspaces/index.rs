//! Where each governed workspace is held, by its id.
//!
//! A policy lookup answers for one workspace and is paid once for each
//! workspace a request lists, so it must not compare the id against every
//! workspace the host governs. This index holds where each is held: filled
//! when the host opens its workspaces, and kept as each is made, released or
//! deleted. A workspace the index does not know costs one walk, which notes it
//! ([`super::views`]).

use std::collections::HashMap;
use std::sync::Mutex;

use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;

use super::device_plane::WorkspacesMap;

/// The cluster id each governed workspace is held under, by its id.
#[derive(Default)]
pub(super) struct IdIndex {
    ids: Mutex<HashMap<WorkspaceId, String>>,
}

impl IdIndex {
    /// Where `workspace_id` is held, when it was noted.
    pub(super) fn get(&self, workspace_id: WorkspaceId) -> Option<String> {
        self.ids.lock_recover().get(&workspace_id).cloned()
    }

    /// Note where `workspace_id` is held.
    pub(super) fn note(&self, workspace_id: WorkspaceId, cluster: &str) {
        self.ids
            .lock_recover()
            .insert(workspace_id, cluster.to_owned());
    }

    /// A workspace this host serves no more.
    pub(super) fn forget(&self, workspace_id: WorkspaceId) {
        self.ids.lock_recover().remove(&workspace_id);
    }

    /// Note every workspace the host opened.
    pub(super) async fn reset(&self, workspaces: &WorkspacesMap) {
        let mut entries = HashMap::new();
        let held = workspaces.lock().await;
        for workspace in held.values() {
            let workspace = workspace.lock().await;
            entries.insert(workspace.workspace_id(), workspace.workspace().to_owned());
        }
        drop(held);
        *self.ids.lock_recover() = entries;
    }
}
