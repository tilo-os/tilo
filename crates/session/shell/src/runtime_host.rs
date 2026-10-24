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
    sync::Arc,
    time::Duration,
};
use tilo_runtime::RuntimeServices;
use tilo_runtime::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, unix};

#[cfg(all(test, target_os = "linux"))]
#[path = "runtime_host/admission_tests.rs"]
mod admission_tests;

/// A single-user service session. Drop applications before dropping the session.
/// Identity discovery uses the same environment as Tilo; configure it before
/// starting any threads when using an isolated profile.
pub struct Session {
    pub broker: BrokerHandle,
    _peer_sync: PeerSync,
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
        Self::start_with_identity(root, stage, None)
    }

    fn start_with_identity(
        root: &Path,
        stage: &mut dyn FnMut(&str),
        admission_owner: Option<u32>,
    ) -> io::Result<Self> {
        if let Some(owner_uid) = admission_owner
            && (owner_uid == 0 || rustix::process::geteuid().as_raw() != owner_uid)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native admission must run as the existing non-root device owner",
            ));
        }
        stage("locking the profile");
        let lock = tilo_runtime::profile::lock(root)?;
        stage("provisioning the collaboration identity");
        let identity_dir = runtime_identity::identity_dir_from_env()?;
        let identity_paths = crate::firstboot::collaboration_identity_paths();
        if admission_owner.is_some() {
            stage("verifying the existing collaboration identity");
            crate::firstboot::verify_collaboration_identity(
                &identity_dir,
                &identity_paths,
                tilo_clock::unix_now_ns(),
            )?;
            if !tilo_identity_host::provisioning::active_identity_owns_device(
                &identity_dir,
                &identity_paths,
            )? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "native admission identity does not own this device",
                ));
            }
        } else {
            crate::firstboot::ensure_collaboration_identity(&identity_dir, &identity_paths)?;
        }
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
        let workspace_room = workspace.room();
        let room_id = workspace_room.to_string();
        let profile = crate::identity_registry::selected_identity_profile(&room_id)?
            .ok_or_else(|| io::Error::other("runtime workspace has no selected identity"))?;
        let actor = profile.principal_id();
        if workspace.owner().is_none() {
            workspace.set_owner(actor.clone());
        }
        stage("saving the workspace");
        persist::save_tree_to(&tree, &path)?;
        // The room policy the native seat and every launched app are admitted
        // against is the accepted one just saved; publish it as a save does,
        // or nothing is admitted until the first later write.
        crate::shell::action_authority::Export::of(&tree).publish();
        let tree = tree.shared();
        let identity_socket = root.join(IDENTITY_SOCKET);
        let channel_keys = crate::shell::channel_encryption::ChannelKeyProvider::default();
        stage("starting the identity service");
        let identity =
            identity_broker::spawn(identity_socket.clone(), tree.clone(), channel_keys.clone())?;
        let blob_socket = root.join(BLOB_SOCKET);
        stage("opening the content store");
        let media = tilo_runtime::profile::MediaHost::start(&root.join("content"), &blob_socket)?;
        stage("starting this device's network and room host");
        let mut peer_sync = PeerSync::start(&tree, root)?;
        // No frame loop waits here: open the owner before the broker, whose
        // services otherwise answer that it is still opening until it is.
        stage("opening the workspace owner");
        if let Err(error) = peer_sync.network.native_owner() {
            tracing::warn!(%error, "the runtime's workspace owner did not open");
        }
        stage("hosting the workspace on this device");
        peer_sync.host(workspace_room)?;
        stage("starting the collaboration broker");
        let broker = collab_broker::spawn_native_offloading(
            root.join(COLLAB_SOCKET),
            &Some(Arc::clone(&peer_sync.network)),
            tree.clone(),
            // Offloadable actions of the room may run on a better-placed
            // device of this person, over the room's media in this store.
            tilo_runtime::offload::action_offload(&[room_id.as_str()], media.store()),
        )?;
        channel_keys.set(broker.channel_encryption());
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

    #[cfg(test)]
    #[expect(
        clippy::used_underscore_binding,
        reason = "the network is otherwise only held for the session's lifetime"
    )]
    pub(crate) fn network(&self) -> &Arc<ShellNetwork> {
        &self._peer_sync.network
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

    fn start_admission_reporting(
        root: &Path,
        owner_uid: u32,
        stage: &mut dyn FnMut(&str),
    ) -> io::Result<Self> {
        Self::start_with_identity(root, stage, Some(owner_uid))
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

/// This device's network, with the room host the desktop runs: the session's
/// workspace is placed on and served by this device's own host, as the
/// desktop's are. When `TILO_COLLAB_HOST(S)` names a workspace host, the
/// network also joins it, so apps of this session share their rooms with
/// the host's other devices (other shells, browsers, agents).
///
/// Nothing here runs on a timer: the pump turns the network only when it
/// wakes the session (an event from the actor, a socket, one of its own
/// timers), the way the desktop's loop does. An idle session with no peers
/// sleeps.
struct PeerSync {
    network: Arc<ShellNetwork>,
    wake: Arc<Wake>,
    pump: Option<std::thread::JoinHandle<()>>,
}

/// The session's wake: set by the network from any thread, taken by the one
/// turning it.
#[derive(Default)]
struct Wake {
    state: std::sync::Mutex<WakeState>,
    ready: std::sync::Condvar,
}

#[derive(Default)]
struct WakeState {
    woken: bool,
    stopping: bool,
}

impl Wake {
    fn wake(&self) {
        self.lock().woken = true;
        self.ready.notify_all();
    }

    fn stop(&self) {
        self.lock().stopping = true;
        self.ready.notify_all();
    }

    /// Block until woken or `deadline` passes: whether it was woken.
    fn wait_until(&self, deadline: std::time::Instant) -> bool {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let (mut state, timeout) = self
            .ready
            .wait_timeout_while(self.lock(), left, |state| !state.woken && !state.stopping)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.woken = false;
        !timeout.timed_out()
    }

    /// Block until woken; `false` once the session stops.
    fn wait(&self) -> bool {
        let mut state = self
            .ready
            .wait_while(self.lock(), |state| !state.woken && !state.stopping)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.woken = false;
        !state.stopping
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WakeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl PeerSync {
    fn start(tree: &crate::shell::state::WorkspaceTreeHandle, root: &Path) -> io::Result<Self> {
        // The session's own peer state, beside its workspace: a desktop on
        // the same device keeps its own.
        let network = Arc::new(ShellNetwork::spawn(
            tree.clone(),
            &crate::shell::network::peer_key_path(),
            &root.join("peer-sync"),
        )?);
        let wake = Arc::new(Wake::default());
        network.set_wake({
            let wake = Arc::clone(&wake);
            Arc::new(move || wake.wake())
        });
        if crate::shell::network::collab_host_configured() {
            let _ = network.join_default_workspace();
            tracing::info!("peer-sync: attached to the configured workspace host");
        }
        Ok(Self {
            network,
            wake,
            pump: None,
        })
    }

    /// Place `room`, the session's workspace, on this device's host and wait,
    /// turning the network as it wakes, until the native owner holds it; then
    /// leave the network to the pump. A session whose room is not served
    /// within [`hosting_bound`] fails to start, naming the step it stopped at,
    /// rather than serving nothing.
    fn host(&mut self, room: crate::shell::state::RoomId) -> io::Result<()> {
        use crate::shell::network::Placement;
        let deadline = std::time::Instant::now()
            .checked_add(hosting_bound())
            .ok_or_else(|| io::Error::other("the hosting bound is past what this clock holds"))?;
        // The rooms this device hosted before are served again first: a
        // session reopening its workspace finds it held already.
        self.network.open_hosted_rooms();
        loop {
            // A turn places what waits to be placed and installs what
            // enrolled, as the desktop's frame does.
            self.network.tick();
            let placement = self.network.placement(room);
            match &placement {
                Placement::Held => break,
                Placement::Placing | Placement::Enrolling => {}
                Placement::NotPlaced(error) | Placement::NotInstalled(error) => {
                    return Err(io::Error::other(format!(
                        "the session's workspace is not hosted: stopped at {}: {error}",
                        placement.step()
                    )));
                }
            }
            if !self.wake.wait_until(deadline) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "the session's workspace is not hosted: still waiting on {}",
                        placement.step()
                    ),
                ));
            }
        }
        let network = Arc::clone(&self.network);
        let wake = Arc::clone(&self.wake);
        self.pump = Some(
            std::thread::Builder::new()
                .name("tilo-peer-sync".into())
                .spawn(move || {
                    while wake.wait() {
                        network.tick();
                    }
                })?,
        );
        Ok(())
    }
}

/// How long a starting session waits for its workspace to be served by this
/// device's host before it reports where it stopped.
fn hosting_bound() -> Duration {
    #[cfg(test)]
    {
        tilo_test_env::hang_bound()
    }
    #[cfg(not(test))]
    {
        Duration::from_secs(60)
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
        self.wake.stop();
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
