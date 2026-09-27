//! Compositor-free host of the same services used by the Tilo desktop.
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
/// Identity discovery uses the same environment as Tilo; configure it before
/// starting any threads when using an isolated profile.
pub struct Session {
    pub broker: BrokerHandle,
    _identity: identity_broker::IdentityBrokerHandle,
    pub room: String,
    pub actor: String,
    identity_socket: PathBuf,
    blob_socket: PathBuf,
    _media: MediaHost,
    _membership: crate::shell::state::WorkspaceTreeHandle,
    _lock: advisory_lock::FileLock,
}

impl Session {
    /// Start the production identity and collaboration services without a GUI.
    /// # Errors
    /// Rejects a locked profile, invalid persisted state, or failed provisioning.
    pub fn start(root: &Path) -> io::Result<Self> {
        Self::start_reporting(root, &mut |_| {})
    }

    /// [`Session::start`], naming each step to `stage` before taking it, so
    /// whoever waits for the session can tell a slow start from a stuck one.
    /// # Errors
    /// Rejects a locked profile, invalid persisted state, or failed provisioning.
    pub fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        stage("locking the profile");
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
        stage("provisioning the collaboration identity");
        let identity_dir = runtime_identity::identity_dir_from_env()?;
        crate::firstboot::ensure_collaboration_identity(
            &identity_dir,
            &crate::firstboot::collaboration_identity_paths(),
        )?;
        stage("loading the workspace");
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
        let room_id = workspace.room.to_string();
        let profile = crate::identity_registry::selected_identity_profile(&room_id)?
            .ok_or_else(|| io::Error::other("runtime workspace has no selected identity"))?;
        let actor = profile.principal_id();
        if workspace.owner.is_none() {
            workspace.set_owner(actor.clone());
        }
        stage("saving the workspace");
        persist::save_tree_to(&tree, &path)?;
        let tree = tree.shared();
        let identity_socket = root.join("identity.sock");
        let channel_keys = crate::shell::channel_encryption::ChannelKeyProvider::default();
        stage("starting the identity service");
        let identity =
            identity_broker::spawn(identity_socket.clone(), tree.clone(), channel_keys.clone())?;
        stage("starting the collaboration broker");
        let broker = collab_broker::spawn_native(root.join("collab.sock"), &None, tree.clone())?;
        channel_keys.set(broker.channel_encryption());
        let blob_socket = root.join("blobs.sock");
        stage("opening the content store");
        let media = MediaHost::start(&root.join("content"), &blob_socket)?;
        Ok(Self {
            broker,
            _identity: identity,
            room: room_id,
            actor,
            identity_socket,
            blob_socket,
            _media: media,
            _membership: tree,
            _lock: lock,
        })
    }

    #[cfg(test)]
    #[expect(
        clippy::used_underscore_binding,
        reason = "the membership handle is otherwise only held for the session's lifetime"
    )]
    pub(crate) fn workspace_tree(&self) -> crate::shell::state::WorkspaceTreeHandle {
        self._membership.clone()
    }

    /// Read document media an app in this session's room stored, through
    /// the same scoped capability the app holds.
    /// # Errors
    /// Returns connection errors; `Ok(None)` when the content is not readable.
    pub fn read_media(&self, app_id: &str, hash: &str) -> io::Result<Option<Vec<u8>>> {
        use runtime_fs::blobs::{BlobClient, ReadOutcome};
        let session = BlobClient::connect_session(&self.blob_socket).map_err(io::Error::other)?;
        let scoped = session
            .scope(&self.room, app_id)
            .map_err(io::Error::other)?;
        match scoped.read_all(hash).map_err(io::Error::other)? {
            ReadOutcome::Ready { bytes, .. } => Ok(Some(bytes)),
            ReadOutcome::Pending | ReadOutcome::Unavailable => Ok(None),
        }
    }

    /// Launch a trusted native app through the existing SDK descriptor contract.
    /// This is a development host, not a replacement for Tilo package sandboxing.
    /// # Errors
    /// Returns handshake, descriptor or process-spawn errors.
    pub fn launch(&self, app_id: &str, command: Command) -> io::Result<Child> {
        unix::launch(
            command,
            app_id,
            &self.room,
            &unix::ServiceSockets {
                collab: self.broker.capnp_socket(),
                identity: &self.identity_socket,
                blobs: &self.blob_socket,
            },
        )
    }
}

/// Serves the host's apps their document media: the filesystem daemon's
/// content store and capability, on the host's private profile.
struct MediaHost {
    store: std::sync::Arc<tilo_fs::ContentStore>,
    _server: std::thread::JoinHandle<()>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl MediaHost {
    fn start(content: &Path, socket: &Path) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("tilo-media")
            .enable_all()
            .build()?;
        let store = std::sync::Arc::new(
            runtime
                .block_on(tilo_fs::ContentStore::open(content))
                .map_err(io::Error::other)?,
        );
        let media =
            tilo_fs::document_media::DocumentMedia::new(std::sync::Arc::clone(&store), None);
        let server = {
            let _entered = runtime.enter();
            tilo_fs::document_media::serve_socket(socket, media).map_err(io::Error::other)?
        };
        Ok(Self {
            store,
            _server: server,
            runtime: Some(runtime),
        })
    }
}

impl Drop for MediaHost {
    /// Release the store's database so the profile can be served again in
    /// this process; the server thread then only answers with errors.
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        let store = std::sync::Arc::clone(&self.store);
        let _ = std::thread::spawn(move || {
            let _ = runtime.block_on(store.shutdown());
            runtime.shutdown_background();
        })
        .join();
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;
