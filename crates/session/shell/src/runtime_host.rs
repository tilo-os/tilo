//! Compositor-free host of the same services used by the Tilo desktop.
//!
//! This module contains lifecycle wiring only. Policy and RPC remain in the
//! existing brokers; the profile, native endpoint, app attachment and MCP
//! projection are `tilo_runtime`'s, which reaches these services through
//! [`RuntimeServices`].

use crate::{
    identity_broker,
    shell::{
        collab_broker::{self, BrokerHandle},
        network::ShellNetwork,
        persist,
        state::WorkspaceTree,
    },
};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tilo_runtime::RuntimeServices;
use tilo_runtime::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, unix};

/// A single-user service session. Drop applications before dropping the session.
/// Identity discovery uses the same environment as Tilo; configure it before
/// starting any threads when using an isolated profile.
pub struct Session {
    pub broker: BrokerHandle,
    _peer_sync: Option<PeerSync>,
    _identity: identity_broker::IdentityBrokerHandle,
    pub room: String,
    pub actor: String,
    identity_socket: PathBuf,
    blob_socket: PathBuf,
    _media: tilo_runtime::profile::MediaHost,
    _membership: crate::shell::state::WorkspaceTreeHandle,
    // Fields retire in declaration order: stop all save producers and release
    // membership before flushing, while the profile lock still excludes restart.
    _final_saves: FinalSessionSaves,
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
        let lock = tilo_runtime::profile::lock(root)?;
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
        if workspace.owner().is_none() {
            workspace.set_owner(actor.clone());
        }
        stage("saving the workspace");
        persist::save_tree_to(&tree, &path)?;
        let tree = tree.shared();
        let identity_socket = root.join(IDENTITY_SOCKET);
        let channel_keys = crate::shell::channel_encryption::ChannelKeyProvider::default();
        stage("starting the identity service");
        let identity =
            identity_broker::spawn(identity_socket.clone(), tree.clone(), channel_keys.clone())?;
        stage("starting the collaboration broker");
        let peer_sync = PeerSync::start(&tree);
        let broker = collab_broker::spawn_native(
            root.join(COLLAB_SOCKET),
            &peer_sync.as_ref().map(|sync| Arc::clone(&sync.network)),
            tree.clone(),
        )?;
        channel_keys.set(broker.channel_encryption());
        let blob_socket = root.join(BLOB_SOCKET);
        stage("opening the content store");
        let media = tilo_runtime::profile::MediaHost::start(&root.join("content"), &blob_socket)?;
        Ok(Self {
            broker,
            _peer_sync: peer_sync,
            _identity: identity,
            room: room_id,
            actor,
            identity_socket,
            blob_socket,
            _media: media,
            _membership: tree,
            _final_saves: FinalSessionSaves,
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

    /// The identity service an app this session connects joins with.
    pub fn identity_socket(&self) -> &Path {
        &self.identity_socket
    }

    /// Read document media an app in this session's room stored, through
    /// the same scoped capability the app holds.
    /// # Errors
    /// Returns connection errors; `Ok(None)` when the content is not readable.
    pub fn read_media(&self, app_id: &str, hash: &str) -> io::Result<Option<Vec<u8>>> {
        tilo_runtime::profile::read_media(&self.blob_socket, &self.room, app_id, hash)
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

impl RuntimeServices for Session {
    type ActionHost = crate::shell::remote_actions::BrokerActionHost;

    fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start_reporting(root, stage)
    }

    fn room(&self) -> &str {
        &self.room
    }

    fn actor(&self) -> &str {
        &self.actor
    }

    fn action_host(&self) -> io::Result<Self::ActionHost> {
        self.broker
            .app_action_broker()
            .map(crate::shell::remote_actions::BrokerActionHost::new)
            .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))
    }

    fn launch(&self, app_id: &str, command: Command) -> io::Result<Child> {
        Self::launch(self, app_id, command)
    }

    fn media(&self) -> Option<std::sync::Arc<dyn tilo_runtime::MediaReader>> {
        Some(std::sync::Arc::new(tilo_runtime::profile::BlobMedia::new(
            &self.blob_socket,
            &self.room,
        )))
    }
}

/// The desktop's peer network, attached when `TILO_COLLAB_HOST(S)` names a
/// workspace host, so apps of this session share their rooms with the
/// host's other devices (other shells, browsers, agents). Without a host the
/// session stays local, as before.
struct PeerSync {
    network: Arc<ShellNetwork>,
    stop: Arc<AtomicBool>,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl PeerSync {
    /// How often the actor's events are drained into the replica and the
    /// broker's inbound queues; the compositor does this every frame.
    const TICK: Duration = Duration::from_millis(16);

    fn start(tree: &crate::shell::state::WorkspaceTreeHandle) -> Option<Self> {
        if !crate::shell::network::collab_host_configured() {
            return None;
        }
        let network = match ShellNetwork::spawn_for_session(tree.clone()) {
            Ok(network) => Arc::new(network),
            Err(error) => {
                tracing::warn!(%error, "peer-sync: spawn failed; the session stays local");
                return None;
            }
        };
        let _ = network.join_default_workspace();
        let stop = Arc::new(AtomicBool::new(false));
        let pump = {
            let network = Arc::clone(&network);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("tilo-peer-sync".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        network.tick();
                        std::thread::sleep(Self::TICK);
                    }
                })
                .ok()
        };
        tracing::info!("peer-sync: attached to the configured workspace host");
        Some(Self {
            network,
            stop,
            pump,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Keep brokers owned until retirement and close admission before their
        // fields retire. BrokerHandle's close is idempotent; the final-save
        // guard drains any shutdown work after those producers have stopped.
        self.broker.local_session_lifecycle().close();
    }
}

struct FinalSessionSaves;

impl Drop for FinalSessionSaves {
    /// Broker teardown can enqueue snapshots. Drain them after the brokers
    /// retire, before the next session can acquire this profile lock.
    fn drop(&mut self) {
        persist::wait_for_saves_within(Duration::from_secs(10));
    }
}

impl Drop for PeerSync {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;

#[cfg(test)]
#[path = "runtime_host/mcp_process_tests.rs"]
mod mcp_process_tests;
#[cfg(test)]
#[path = "runtime_host/mcp_tests.rs"]
mod mcp_tests;
