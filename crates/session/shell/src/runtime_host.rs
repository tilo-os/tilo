//! Compositor-free host of the same services used by the qOS desktop.
//!
//! This module contains lifecycle wiring only. Policy and RPC remain in the
//! existing brokers. Keeping it in this library initially avoids copying the
//! workspace model while its dependencies are separated from shell chrome.

use crate::{
    identity_broker,
    shell::{
        collab_broker::{self, BrokerHandle},
        persist,
        state::WorkspaceTree,
    },
};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command},
};

pub mod daemon;
pub mod mcp;
pub mod mcp_process;
mod unix;

/// A single-user service session. Drop applications before dropping the session.
/// Identity discovery uses the same environment as qOS; configure it before
/// starting any threads when using an isolated profile.
pub struct Session {
    pub broker: BrokerHandle,
    _identity: identity_broker::IdentityBrokerHandle,
    pub room: String,
    pub actor: String,
    identity_socket: PathBuf,
    content_root: PathBuf,
    _membership: crate::shell::state::WorkspaceTreeHandle,
    _lock: advisory_lock::FileLock,
}

impl Session {
    /// Start the production identity and collaboration services without a GUI.
    /// # Errors
    /// Rejects a locked profile, invalid persisted state, or failed provisioning.
    pub fn start(root: &Path) -> io::Result<Self> {
        unix::private_directory(root)?;
        let lock_path = root.join("runtime.lock");
        let lock = advisory_lock::FileLock::try_exclusive(unix::private_file(&lock_path)?)
            .map_err(|error| match error {
                advisory_lock::TryLockError::WouldBlock => {
                    let owner = advisory_lock::owner_pid(&lock_path)
                        .map(|pid| format!(" by process {pid}"))
                        .unwrap_or_default();
                    io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!("runtime profile {} is in use{owner}", root.display()),
                    )
                }
                advisory_lock::TryLockError::Error(error) => error,
            })?;
        lock.record_owner()?;
        let identity_dir = runtime_identity::identity_dir_from_env()?;
        crate::firstboot::ensure_collaboration_identity(
            &identity_dir,
            &crate::firstboot::collaboration_identity_paths(),
        )?;
        let path = root.join("workspace.json");
        let mut tree = match persist::load_tree_from(&path) {
            Ok(tree) => tree,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                WorkspaceTree::bootstrap_default()
            }
            Err(error) => return Err(error),
        };
        // `persist_path` is deliberately skipped by serde. Restore the live
        // destination before sharing the tree with brokers so their eager
        // capability, task and effect-receipt flushes survive this session.
        tree.persist_path = Some(path.clone());
        let workspace = tree
            .desktop
            .workspaces
            .first_mut()
            .ok_or_else(|| io::Error::other("runtime profile has no workspace"))?;
        let room = workspace.room.to_string();
        let profile = crate::identity_registry::selected_identity_profile(&room)?
            .ok_or_else(|| io::Error::other("runtime workspace has no selected identity"))?;
        let actor = profile.principal_id();
        if workspace.owner.is_none() {
            workspace.set_owner(actor.clone());
        }
        persist::save_tree_to(&tree, &path)?;
        let tree = tree.shared();
        let identity_socket = root.join("identity.sock");
        let identity = identity_broker::spawn(identity_socket.clone(), tree.clone())?;
        let broker = collab_broker::spawn_native(root.join("collab.sock"), &None, tree.clone())?;
        Ok(Self {
            broker,
            _identity: identity,
            room,
            actor,
            identity_socket,
            content_root: root.join("blobs"),
            _membership: tree,
            _lock: lock,
        })
    }

    #[cfg(test)]
    pub(crate) fn workspace_tree(&self) -> crate::shell::state::WorkspaceTreeHandle {
        self._membership.clone()
    }

    /// Launch a trusted native app through the existing SDK descriptor contract.
    /// This is a development host, not a replacement for qOS package sandboxing.
    /// # Errors
    /// Returns handshake, descriptor or process-spawn errors.
    pub fn launch(&self, app_id: &str, mut command: Command) -> io::Result<Child> {
        command.env("TILO_BLOB_STORE", &self.content_root);
        unix::launch(
            command,
            app_id,
            &self.room,
            self.broker.capnp_socket(),
            &self.identity_socket,
        )
    }
}

#[cfg(test)]
#[path = "runtime_host/daemon_process_tests.rs"]
mod daemon_process_tests;
