//! End to end over the one host stack: every device runs the same room host
//! the relay runs, in its own session, and its native owner is a member of
//! every room, wherever the room is hosted. Real Iroh endpoints on loopback,
//! real shells' networks, real hosts and a real relay; nothing is stubbed.
//!
//! Each test drives its devices the way the compositor's loop does: a device
//! runs a turn ([`Device::frame`]) when its network wakes the loop, and
//! otherwise only at the deadlines that loop keeps itself (a room push
//! retry, the join card's poll of
//! an enrollment in flight). A wait ([`until`]) blocks on those wakes; it
//! never sleeps on a guess.
//!
//! What the tests assert is what a person sees: the workspace list, a
//! workspace's room, its windows and tabs, an app's document and the room's
//! roster.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use ed25519_dalek::SigningKey;
use tilo_automerge::ApplyIncoming as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::{HostIdentity, NetCmd, Reach, ShellNetwork};
use crate::shell::channel_encryption::{ChannelAccess, ChannelEncryptionConfig, ChannelKeyAccess};
use crate::shell::peer_sync_overlay::{InvitationTerms, PeerSyncResponse};
use crate::shell::state::{
    AppKind, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceId,
    WorkspaceTree, WorkspaceTreeHandle,
};
use tilo_action_broker::collab_core::BrokerChannels;

mod adopted_fields;
mod agent_invite;
mod channels;
mod chat_seed;
pub(crate) mod cold_reads;
mod content_index;
mod edit_latency;
mod failover;
mod history_across_admission;
mod home_relay;
#[cfg(target_os = "linux")]
mod idle_host;
mod invite_join;
mod live_frames;
mod member_controls;
mod no_secret_store;
mod old_rooms;
mod on_demand_records;
mod own_devices;
mod pointers;
mod sealed_records;
mod sealed_writes;
mod workspace_emoji;

/// The app whose document the tests edit, and its record.
pub(crate) const NOTES: &str = "so.tilo.notes";
const NOTE_RECORD: &str = "plan";
/// The note's text, a field of the record's document.
const NOTE_TEXT: &str = "text";

/// How often the join card asks after an enrollment in flight
/// (`PeerSyncOverlay::pending_enrollment_request`).
const ENROLLMENT_POLL: Duration = Duration::from_millis(200);

/// The wakes of every device's loop in one test: set by any of them,
/// consumed by the one waiting.
#[derive(Default)]
pub(crate) struct Wakes {
    woken: Mutex<bool>,
    ready: Condvar,
}

impl Wakes {
    fn wake(&self) {
        *self.woken.lock().unwrap() = true;
        self.ready.notify_all();
    }

    /// Forget wakes already handled: the turn about to run sees their cause.
    fn consume(&self) {
        *self.woken.lock().unwrap() = false;
    }

    /// Block until a wake arrives or `deadline` passes.
    fn wait_until(&self, deadline: Instant) {
        let left = deadline.saturating_duration_since(Instant::now());
        let (mut woken, _) = self
            .ready
            .wait_timeout_while(self.woken.lock().unwrap(), left, |woken| !*woken)
            .unwrap();
        *woken = false;
        drop(woken);
    }
}

/// A person: their root key, which certifies each of their devices.
pub(crate) struct Person {
    root: SigningKey,
}

impl Person {
    pub(crate) fn new(seed: u8) -> Self {
        Self {
            root: SigningKey::from_bytes(&[seed; 32]),
        }
    }

    pub(crate) fn user(&self) -> [u8; 32] {
        self.root.verifying_key().to_bytes()
    }

    /// The person's id as their own devices name them.
    pub(crate) fn id(&self) -> String {
        match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(self.user()),
        ) {
            collab_types::caps::Principal::Person(id) => id,
            other => panic!("a root key names a person: {other:?}"),
        }
    }

    /// A certificate from this person's root for `key`.
    pub(crate) fn certify(
        &self,
        key: &SigningKey,
        label: &str,
        capabilities: DeviceCapabilities,
    ) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: self.user(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &self.root).unwrap();
        cert
    }
}

/// When the compositor's loop of one device next has to run though nothing
/// woke it, and what it last did at those deadlines.
#[derive(Default)]
struct Loop {
    /// When the enrollments in flight were last asked after.
    polled: Option<Instant>,
}

/// Where a device's key and certificate come from.
enum Identity {
    /// The person's root certifies the device's key, with `capabilities`,
    /// each time it starts.
    Certified {
        root: Box<SigningKey>,
        capabilities: DeviceCapabilities,
    },
    /// The identity this host was set up with, under `root`: its device key
    /// and the certificate its person's root gave it, read as a session's
    /// network reads them.
    ThisHost { root: PathBuf },
}

/// One of a person's devices: its session's network over its tree, its
/// state on disk, and its device key and certificate (made at its first
/// start and kept across restarts, as a device's are).
pub(crate) struct Device {
    pub(crate) label: String,
    dir: PathBuf,
    identity: Identity,
    /// The id namespace this device mints its windows in.
    salt: u16,
    pub(crate) tree: WorkspaceTreeHandle,
    net: Option<Arc<ShellNetwork>>,
    custody: Arc<Mutex<Option<(SigningKey, DeviceCert)>>>,
    wakes: Arc<Wakes>,
    state: Mutex<Loop>,
    /// Joins in flight, by enrollment.
    enrollments: Mutex<Vec<u64>>,
    /// How each finished join ended.
    pub(crate) joins: Mutex<Vec<Result<RoomId, String>>>,
    /// This device's channel encryption service, and the thread that pumps
    /// its governance, as the session broker's drain does; while online.
    channels: Option<(ChannelKeyAccess, Drain)>,
    /// Whether a secret store keeps this device's own channel state, as on
    /// a machine whose session found one.
    secret_store: bool,
    /// The note's document in each room as the notes app here holds it:
    /// what it wrote and every sealed write it opened.
    notes: Mutex<HashMap<RoomId, automerge::AutoCommit>>,
    /// Every sealed write to the note in each room this device made or
    /// opened, in order, as it travelled.
    note_writes: Mutex<HashMap<RoomId, Vec<collab_types::SignedOp>>>,
    /// The channel keys this device's session broker hands its apps, once
    /// it serves through one: what an app connected to it encrypts under.
    pub(crate) endpoint_keys: Mutex<Option<ui::collaboration::EndpointChannelKeys>>,
    /// Whether this device's session keeps its workspaces' layouts through
    /// its broker, as a desktop session does ([`Self::carry_layouts`]).
    carries_layouts: bool,
    /// Which of this thread's [`SESSION_LAYOUTS`] are this device's: its
    /// session broker and the layout records it keeps through it, while
    /// online.
    layouts: u64,
}

thread_local! {
    /// Each device's session layouts, by [`Device::layouts`], on the test's
    /// own thread: the records are kept on the thread that pumps them, as a
    /// session's loop keeps them, so a device stays shareable across the
    /// threads a test runs it from.
    static SESSION_LAYOUTS: std::cell::RefCell<std::collections::HashMap<u64, SessionLayouts>> =
        std::cell::RefCell::default();
}

/// The next [`Device::layouts`] key.
static NEXT_LAYOUTS: AtomicU64 = AtomicU64::new(1);

impl Drop for Device {
    fn drop(&mut self) {
        // Its session's layouts end with it, as its session would.
        let key = self.layouts;
        let _ = SESSION_LAYOUTS.try_with(|held| held.borrow_mut().remove(&key));
    }
}

/// A session's layout records kept in step with its rooms through its own
/// broker (`shell::workspace_layouts`). The records go before the broker.
struct SessionLayouts {
    records: crate::shell::workspace_layouts::WorkspaceLayouts,
    /// Wakes the test's loop on the broker's channel changes; ends first.
    _channel_wake: Option<ChannelWake>,
    broker: crate::shell::collab_broker::BrokerHandle,
}

/// Wakes the test's loop whenever a channel's keys or access change in a
/// session broker's channel service, as the device's own drain does when no
/// broker runs ([`Drain`]): what the loop's probes read (a note's keys)
/// changes then, and nothing else may wake it.
struct ChannelWake {
    quit: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ChannelWake {
    fn start(mut changes: tokio::sync::watch::Receiver<u64>, wakes: &Arc<Wakes>) -> Self {
        let (quit, mut quitting) = tokio::sync::oneshot::channel();
        let wakes = Arc::clone(wakes);
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("the channel wake's runtime");
            runtime.block_on(async move {
                loop {
                    tokio::select! {
                        seen = changes.changed() => {
                            if seen.is_err() {
                                return;
                            }
                            wakes.wake();
                        }
                        _ = &mut quitting => return,
                    }
                }
            });
        });
        Self {
            quit: Some(quit),
            thread: Some(thread),
        }
    }
}

impl Drop for ChannelWake {
    fn drop(&mut self) {
        if let Some(quit) = self.quit.take() {
            let _ = quit.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The session broker's drain, as far as channels go: parked until the
/// network or the channel service pokes it, it pumps channel governance
/// and wakes the loop, which reads what arrived.
struct Drain {
    quit: Arc<AtomicBool>,
    park: Arc<crate::idle_wake::DrainPark>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drain {
    fn start(net: &Arc<ShellNetwork>, channels: ChannelKeyAccess, wakes: &Arc<Wakes>) -> Self {
        let quit = Arc::new(AtomicBool::new(false));
        let park = Arc::clone(&net.drain_wake);
        let net = Arc::downgrade(net);
        let thread = std::thread::spawn({
            let quit = Arc::clone(&quit);
            let park = Arc::clone(&park);
            let wakes = Arc::clone(wakes);
            move || {
                let mut due = Some(Duration::ZERO);
                loop {
                    park.wait(due);
                    if quit.load(Ordering::Acquire) {
                        return;
                    }
                    let Some(net) = net.upgrade() else {
                        return;
                    };
                    due = channels.pump_until_due(&*net);
                    drop(net);
                    wakes.wake();
                }
            }
        });
        Self {
            quit,
            park,
            thread: Some(thread),
        }
    }
}

impl Drop for Drain {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Release);
        self.park.poke();
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

impl Device {
    /// A device of `person` with an empty tree, its state under `dir`,
    /// started.
    pub(crate) fn start(
        person: &Person,
        label: &str,
        dir: &Path,
        salt: u16,
        capabilities: DeviceCapabilities,
        wakes: &Arc<Wakes>,
    ) -> Self {
        Self::start_with(
            person,
            label,
            dir,
            salt,
            capabilities,
            wakes,
            WorkspaceTree::default(),
        )
    }

    /// [`Self::start`] over `tree`, which persists to the device's state
    /// file and mints under `salt` from now on, as a session sets its
    /// device's salt on the tree it loads.
    pub(crate) fn start_with(
        person: &Person,
        label: &str,
        dir: &Path,
        salt: u16,
        capabilities: DeviceCapabilities,
        wakes: &Arc<Wakes>,
        tree: WorkspaceTree,
    ) -> Self {
        let identity = Identity::Certified {
            root: Box::new(person.root.clone()),
            capabilities,
        };
        Self::start_from(identity, label, dir, salt, wakes, tree)
    }

    /// A device acting as the identity this host was set up with under
    /// `identity_root`, as a desktop session does, with an empty tree and
    /// its state under `dir`, started.
    pub(crate) fn start_as_this_host(
        identity_root: &Path,
        label: &str,
        dir: &Path,
        salt: u16,
        wakes: &Arc<Wakes>,
    ) -> Self {
        let tree = WorkspaceTree::default();
        let identity = Identity::ThisHost {
            root: identity_root.to_path_buf(),
        };
        Self::start_from(identity, label, dir, salt, wakes, tree)
    }

    fn start_from(
        identity: Identity,
        label: &str,
        dir: &Path,
        salt: u16,
        wakes: &Arc<Wakes>,
        mut tree: WorkspaceTree,
    ) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        tree.set_peer_salt(salt);
        tree.persist_path = Some(dir.join("state.json"));
        let mut device = Self {
            label: label.into(),
            dir: dir.to_path_buf(),
            identity,
            salt,
            tree: tree.shared(),
            net: None,
            custody: Arc::default(),
            wakes: Arc::clone(wakes),
            state: Mutex::default(),
            enrollments: Mutex::default(),
            joins: Mutex::default(),
            channels: None,
            secret_store: true,
            notes: Mutex::default(),
            note_writes: Mutex::default(),
            endpoint_keys: Mutex::default(),
            carries_layouts: false,
            layouts: NEXT_LAYOUTS.fetch_add(1, Ordering::Relaxed),
        };
        device.go_online();
        device
    }

    /// Start this device's session over its current tree, as a session
    /// starts: the network, its wake into the loop, and its rooms.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the network it starts is this device's for as long as it is online"
    )]
    fn go_online(&mut self) {
        assert!(self.net.is_none(), "{} is already online", self.label);
        let label = self.label.clone();
        let custody = Arc::clone(&self.custody);
        let peer_sync = self.dir.join("peer-sync");
        let net = match &self.identity {
            Identity::Certified { root, capabilities } => {
                let root = SigningKey::clone(root);
                let capabilities = capabilities.clone();
                ShellNetwork::spawn_with(
                    self.tree.clone(),
                    &self.dir.join("device-private/keypair.bin"),
                    &peer_sync,
                    Reach::Loopback,
                    move |secret, _| {
                        let key = SigningKey::from_bytes(&secret.to_bytes());
                        let mut cert = DeviceCert {
                            user_public_key: root.verifying_key().to_bytes(),
                            device_public_key: key.verifying_key().to_bytes(),
                            device_label: label,
                            capabilities,
                            issued_at_ns: 1,
                            expires_at_ns: None,
                            signature: [0; 64],
                        };
                        sign_device_cert(&mut cert, &root).unwrap();
                        *custody.lock().unwrap() = Some((key.clone(), cert.clone()));
                        Some(HostIdentity {
                            device_cert: cert,
                            device_signing_key: key,
                            workspace_device_certs: std::collections::HashMap::new(),
                            workspace_revocations: std::collections::HashMap::new(),
                            identity_paths: None,
                        })
                    },
                )
            }
            Identity::ThisHost { root } => ShellNetwork::spawn_with(
                self.tree.clone(),
                &tilo_user_identity::IdentityPaths::under(root).device_privkey,
                &peer_sync,
                Reach::Loopback,
                move |secret, keypair| {
                    let identity = super::host_identity_for_key(secret, keypair)?;
                    *custody.lock().unwrap() = Some((
                        identity.device_signing_key.clone(),
                        identity.device_cert.clone(),
                    ));
                    Some(identity)
                },
            ),
        }
        .unwrap();
        let wakes = Arc::clone(&self.wakes);
        net.set_wake(Arc::new(move || wakes.wake()));
        assert!(net.join_default_workspace());
        // As a session starts: the rooms this device hosted before are
        // served again.
        net.open_local_host();
        let net = Arc::new(net);
        let channels = self.open_channels(&net);
        let drain = Drain::start(&net, channels.clone(), &self.wakes);
        self.channels = Some((channels, drain));
        self.net = Some(net);
    }

    /// Take this device off the network, as shutting it down does: its
    /// session ends, its state stays on disk.
    pub(crate) fn go_offline(&mut self) {
        // The session's broker holds its network: it ends with the session,
        // and so do the channel keys it handed its apps, which reach the
        // network through its channel service.
        SESSION_LAYOUTS.with(|held| held.borrow_mut().remove(&self.layouts));
        *self.endpoint_keys.lock().unwrap() = None;
        let net = self.net.take().expect("the device is online");
        self.channels = None;
        self.notes.lock().unwrap().clear();
        crate::shell::persist::wait_for_saves();
        drop(
            Arc::try_unwrap(net)
                .unwrap_or_else(|_| panic!("{}'s network is not shared", self.label)),
        );
        // A shut down device writes nothing more. In this process the
        // network's blocking work (an admission it was making) outlives it,
        // and would publish into the state file after the device started
        // again from it: its writer is closed here, after whatever it was
        // writing, as the end of the process would stop it.
        // The tree is let go first: a publication takes the writer, then
        // the tree.
        let writer = self.tree.read().native_runtime.clone();
        writer.close_for_shutdown();
    }

    /// Start this device again from what it kept on disk, as booting it
    /// does.
    pub(crate) fn come_back(&mut self) {
        let path = self.dir.join("state.json");
        let (mut tree, notice) =
            crate::shell::persist::load_device_tree_or_default(&path, self.salt);
        assert!(
            notice.is_none(),
            "{} reads its own state file: {notice:?}",
            self.label
        );
        tree.persist_path = Some(path);
        self.tree = tree.shared();
        *self.state.lock().unwrap() = Loop::default();
        self.go_online();
        if self.carries_layouts {
            self.serve_layouts();
        }
    }

    /// Keep this device's workspaces' layouts in step with their rooms from
    /// now on, as a desktop session does: through a collaboration broker its
    /// session serves through, which holds each room's shared desktop record
    /// on the room's record route, its loop writing the device's layout
    /// into it and folding in the others' (`shell::workspace_layouts`). The
    /// room's host decides every write of it, as for any record.
    pub(crate) fn carry_layouts(&mut self) {
        self.carries_layouts = true;
        self.serve_layouts();
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the broker starts before the layout records, whose identity carries the keys it hands its apps, and is kept with them"
    )]
    fn serve_layouts(&mut self) {
        use std::os::fd::IntoRawFd as _;
        use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_KIND, DESKTOP_RECORD};
        // The broker binds only under a directory its owner alone may enter.
        let parent = self.dir.join("session-collab");
        std::fs::create_dir_all(&parent).expect("the session's broker directory");
        std::fs::set_permissions(&parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .expect("the broker directory is the device's alone");
        let socket = parent.join("collab.sock");
        let _ = std::fs::remove_file(&socket);
        // Started first: the layout records' identity carries the channel
        // keys this broker hands its apps.
        let broker = self.start_broker(&socket);
        let (key, cert) = (self.key(), self.cert());
        let keys = self.endpoint_keys.lock().unwrap().clone();
        let wakes = Arc::clone(&self.wakes);
        let records =
            crate::shell::workspace_layouts::WorkspaceLayouts::with_connect(move |room, woken| {
                let room_text = room.to_string();
                crate::shell::launcher::register_in_process_launch(DESKTOP_APP, &room_text);
                let identity = cold_reads::certified_identity(&key, &cert, room, keys.clone());
                let device = hex::encode(cert.device_public_key);
                let route = ui::collaboration::RouteSpec::record(
                    DESKTOP_APP,
                    room.as_uuid(),
                    DESKTOP_RECORD,
                );
                let socket = socket.clone();
                let wakes = Arc::clone(&wakes);
                ui::app::record_routes::RouteDocument::connect_with(
                    {
                        let route = route.clone();
                        move || {
                            let mut stream = std::os::unix::net::UnixStream::connect(&socket)
                                .expect("connect to the session's broker");
                            ::launcher::services::write_collab_handoff(
                                &mut stream,
                                DESKTOP_APP,
                                &room_text,
                            )
                            .expect("hand the connection over as the session does");
                            ui::collaboration::CollaborationManager::for_record_fd_with_identity(
                                stream.into_raw_fd(),
                                route,
                                Some(DESKTOP_KIND.to_owned()),
                                Some(device),
                                identity,
                            )
                        }
                    },
                    route,
                    move || {
                        woken.store(true, Ordering::Release);
                        wakes.wake();
                    },
                )
            });
        let channel_wake = broker
            .channel_encryption()
            .and_then(|channels| channels.changes())
            .map(|changes| ChannelWake::start(changes, &self.wakes));
        let held = SessionLayouts {
            records,
            _channel_wake: channel_wake,
            broker,
        };
        SESSION_LAYOUTS.with(|layouts| layouts.borrow_mut().insert(self.layouts, held));
    }

    /// Hold `keys` for every channel of this device's rooms, as its channel
    /// membership would give them, so it seals and opens live frames.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the drain stops before the network changes and starts again after"
    )]
    pub(crate) fn hold_live_keys(
        &mut self,
        keys: fn(collab_types::ChannelId) -> channel_crypto::ChannelContentKeys,
    ) {
        drop(self.channels.take().expect("the device is online"));
        let net = self.net.as_mut().expect("the device is online");
        Arc::get_mut(net)
            .expect("nothing else holds the device's network")
            .live_keys = Box::new(move |_, channel| Some(keys(channel)));
        let net = Arc::clone(net);
        let channels = self.open_channels(&net);
        let drain = Drain::start(&net, channels.clone(), &self.wakes);
        self.channels = Some((channels, drain));
    }

    /// Run this device's session with no secret store, as on a machine
    /// that has none: its channel service keeps no channel state of its own.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the drain stops before the network changes and starts again after"
    )]
    pub(crate) fn lose_secret_store(&mut self) {
        drop(self.channels.take().expect("the device is online"));
        self.secret_store = false;
        let net = Arc::clone(self.net());
        let channels = self.open_channels(&net);
        let drain = Drain::start(&net, channels.clone(), &self.wakes);
        self.channels = Some((channels, drain));
    }

    /// Serve this device's session through a collaboration broker on its
    /// network from now on, as a desktop session does: the broker opens the
    /// session's channel service and runs the network's one drain, so this
    /// device's own stand-in for both stops first. Two drains parked on the
    /// network's one wake would each take pokes meant for the other.
    pub(crate) fn serve_through_broker(&mut self) {
        drop(self.channels.take().expect("the device is online"));
    }

    /// Whether this device's channel service has anything for its pump to
    /// do, now or later.
    pub(crate) fn channels_live(&self) -> bool {
        self.channels
            .as_ref()
            .is_some_and(|(channels, _)| channels.has_live_channels())
    }

    /// This device's channel state, opened from its directory, as its
    /// session opens it: under its secret store's key while it has one.
    fn open_channels(&self, net: &Arc<ShellNetwork>) -> ChannelKeyAccess {
        crate::shell::collab_broker::channel_key_access(
            self.channel_store(),
            self.tree.clone(),
            Some(Arc::clone(net)),
            Arc::new(Mutex::new(BrokerChannels::default())),
        )
        .expect("the device's channel state opens")
    }

    /// Where this device keeps its channel state, under its secret
    /// store's key, while it has one.
    pub(crate) fn channel_store(&self) -> Option<ChannelEncryptionConfig> {
        let mut vault = [0x5A; 32];
        vault[..2].copy_from_slice(&self.salt.to_le_bytes());
        self.secret_store.then(|| ChannelEncryptionConfig {
            root: self.dir.join("channels"),
            vault: channel_crypto::VaultKey::new(vault),
            device_key: self.key(),
        })
    }

    /// Keep this device's identity on this machine as a device its person
    /// set up from another of theirs keeps it, with its directory as the
    /// system's identity tree (`TILO_IDENTITY_ROOT`): its device key (the
    /// one its network already uses) and the certificate its person's root
    /// gave it, and under `identity_dir` (the account's) the person's public
    /// record as the account's identity and a fleet that knows the device.
    /// The root never comes here. The device's broker then signs what it
    /// admits as this device.
    pub(crate) fn keep_identity_on_disk(&self, identity_dir: &Path) {
        let paths = tilo_user_identity::IdentityPaths::under(&self.dir);
        assert_eq!(
            paths.device_privkey,
            self.dir.join("device-private/keypair.bin"),
            "the identity tree keeps the key the device's network uses"
        );
        let now = tilo_clock::unix_now_ns();
        tilo_user_identity::persist::install_device_cert(&paths, &self.cert(), now)
            .expect("the device keeps the certificate its person's root gave it");
        assert!(
            tilo_identity_host::provisioning::complete_enrolled_device(identity_dir, &paths, now)
                .expect("the account takes the device's person"),
            "the device is set up for its person"
        );
    }

    pub(crate) fn tree(&self) -> &WorkspaceTreeHandle {
        &self.tree
    }

    pub(crate) fn net(&self) -> &Arc<ShellNetwork> {
        self.net
            .as_ref()
            .unwrap_or_else(|| panic!("{} is offline", self.label))
    }

    pub(crate) fn cert(&self) -> DeviceCert {
        self.custody
            .lock()
            .unwrap()
            .as_ref()
            .expect("the device has its certificate once it started")
            .1
            .clone()
    }

    fn key(&self) -> SigningKey {
        self.custody
            .lock()
            .unwrap()
            .as_ref()
            .expect("the device has its key once it started")
            .0
            .clone()
    }

    /// One turn of this device's compositor loop: the network's tick and the
    /// join card's poll of an enrollment in flight. Nothing while it is
    /// offline. A device that carries its layouts ([`Self::carry_layouts`])
    /// also pumps them, as the session's loop does.
    pub(crate) fn frame(&self) {
        let Some(net) = self.net.as_ref() else {
            return;
        };
        net.tick();
        SESSION_LAYOUTS.with(|held| {
            if let Some(layouts) = held.borrow_mut().get_mut(&self.layouts) {
                layouts.records.pump(&self.tree);
            }
        });
        let now = Instant::now();
        let pending = self.enrollments.lock().unwrap().clone();
        if !pending.is_empty() {
            self.state.lock().unwrap().polled = Some(now);
        }
        for id in pending {
            let ended = match net.poll_enrollment(id) {
                Ok(PeerSyncResponse::EnrollmentPending { .. }) => continue,
                Ok(PeerSyncResponse::EnrollmentJoined { room, .. }) => {
                    Ok(RoomId::parse(&room).expect("a joined room is a room"))
                }
                Ok(other) => Err(format!("the join answered {other:?}")),
                Err(error) => Err(error),
            };
            self.enrollments
                .lock()
                .unwrap()
                .retain(|other| *other != id);
            self.joins.lock().unwrap().push(ended);
        }
    }

    /// When this device's loop next has to run though nothing wakes it.
    fn next_deadline(&self) -> Option<Instant> {
        self.net.as_ref()?;
        let polled = self.state.lock().unwrap().polled;
        (!self.enrollments.lock().unwrap().is_empty()).then(|| {
            polled.map_or_else(Instant::now, |at| {
                at.checked_add(ENROLLMENT_POLL).unwrap_or(at)
            })
        })
    }

    // What the person does.

    /// Make a new workspace called `name`, as the create-workspace action
    /// does: placed on this device's own host.
    pub(crate) fn create_workspace(&self, name: &str) -> (WorkspaceId, RoomId) {
        self.tree.write().create_hosted_workspace(name)
    }

    /// Paste an invitation's text into the join field.
    pub(crate) fn join(&self, invitation: &str) -> Result<(), String> {
        match self.net().accept_peer_invitation(invitation)? {
            PeerSyncResponse::EnrollmentPending { id } => {
                self.enrollments.lock().unwrap().push(id);
                Ok(())
            }
            PeerSyncResponse::Message(room) | PeerSyncResponse::EnrollmentJoined { room, .. } => {
                self.joins
                    .lock()
                    .unwrap()
                    .push(Ok(RoomId::parse(&room).expect("a joined room is a room")));
                Ok(())
            }
            other => Err(format!("the join field answered {other:?}")),
        }
    }

    /// Invite someone to `local` from the workspace's sharing card.
    pub(crate) fn invite(
        &self,
        local: WorkspaceId,
        invitee: &str,
        recipient: Option<[u8; 32]>,
    ) -> String {
        self.net()
            .issue_invitation(
                local,
                &InvitationTerms {
                    role: crate::network::invite::InviteRole::Editor,
                    expires_in_seconds: 3600,
                    invitee_name: Some(invitee.into()),
                    recipient_user_public_key: recipient,
                },
            )
            .unwrap_or_else(|error| panic!("{} could not invite {invitee}: {error}", self.label))
    }

    /// Cancel the pending invitation to `room` for `invitee`.
    pub(crate) fn cancel_invitation(&self, room: RoomId, invitee: &str) {
        let pending = self
            .net()
            .pending_invitations(room)
            .into_iter()
            .find(|invitation| invitation.name == invitee)
            .unwrap_or_else(|| panic!("an invitation for {invitee} is pending"));
        assert!(
            self.net().cancel_invitation(&pending.id),
            "the invitation for {invitee} is cancelled"
        );
    }

    /// Open a window of the notes app in `room`'s workspace, in a group of
    /// its own on the workspace's first strip. Answers its tab.
    pub(crate) fn open_window(&self, room: RoomId, title: &str) -> u64 {
        let (group, tab) = (self.mint(), self.mint());
        let strip_id = self.mint();
        let mut tree = self.tree.write();
        let workspace = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room() == room)
            .unwrap_or_else(|| panic!("{} shows the workspace", self.label));
        if workspace.strips.is_empty() {
            workspace.strips.push(Strip {
                id: StripId(strip_id),
                name: "Main".into(),
                window_groups: Vec::new(),
            });
        }
        workspace.strips[0].window_groups.push(WindowGroup {
            id: group,
            tabs: vec![Tab {
                id: tab,
                title: title.into(),
                app_title: None,
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other(NOTES.into()),
                    label: title.into(),
                },
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        drop(tree);
        tab
    }

    /// Close the window showing `tab` in `room`'s workspace.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the window closes in one write of the tree"
    )]
    pub(crate) fn close_window(&self, room: RoomId, tab: u64) {
        let mut tree = self.tree.write();
        let workspace = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room() == room)
            .unwrap_or_else(|| panic!("{} shows the workspace", self.label));
        for strip in &mut workspace.strips {
            strip
                .window_groups
                .retain(|group| !group.tabs.iter().any(|shown| shown.id == tab));
        }
    }

    /// A fresh id in this device's namespace, as its tree mints.
    fn mint(&self) -> u64 {
        self.tree.read().mint_id()
    }

    /// Let the notes app keep records in `room`, as opening the app there
    /// first does: the room's host lets it author, and the note's record is
    /// its sealed channel, which this device asks the room's participants for.
    /// Every hosted room is sealed, wherever it is hosted. Whoever opens
    /// the app later finds the record.
    pub(crate) fn open_notes_first(&self, room: RoomId) {
        self.let_notes_author(room)
            .unwrap_or_else(|error| panic!("the host lets the notes app author: {error}"));
        self.assert_sealed(room);
        let _ = self.note_keys(room);
    }

    /// Let the notes app author `room`'s records, written by this device.
    pub(crate) fn let_notes_author(&self, room: RoomId) -> Result<(), String> {
        self.let_app_author(room, NOTES)
    }

    /// Let `app` keep records in `room`, as opening the app there first
    /// does: this device lets it author. Every hosted room is sealed: the
    /// app opens its record's channel itself, so nothing is made here.
    pub(crate) fn open_app_first(&self, room: RoomId, app: &str) {
        self.let_app_author(room, app)
            .unwrap_or_else(|error| panic!("{} lets {app} author: {error}", self.label));
        self.assert_sealed(room);
    }

    /// What `asked`, a request this device's session makes, answers, while
    /// the device's loop keeps turning as the compositor's does meanwhile.
    pub(crate) fn answer<T: Send + 'static>(
        &self,
        what: &str,
        asked: impl std::future::Future<Output = T> + Send + 'static,
    ) -> T {
        let runtime = self.net().runtime_handle().unwrap();
        let wakes = Arc::clone(&self.wakes);
        let task = runtime.spawn(async move {
            let answer = asked.await;
            wakes.wake();
            answer
        });
        until(&[self], what, || task.is_finished().then_some(()));
        runtime
            .block_on(task)
            .expect("the request runs to its answer")
    }

    /// Let `app` author `room`'s records, as opening the app there first
    /// does: this device writes the grant itself, while its loop keeps
    /// turning.
    pub(crate) fn let_app_author(&self, room: RoomId, app: &str) -> Result<(), String> {
        let net = Arc::clone(self.net());
        let command =
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
                app: app.to_owned(),
            };
        let written = async move {
            tokio::task::spawn_blocking(move || {
                let owner = net.native_owner()?;
                let caller = owner.with_current(room, |view| Ok(view.device().clone()))?;
                net.commit_membership_here(&owner, room, caller, command)
            })
            .await
            .map_err(|error| error.to_string())?
        };
        self.answer("the app's authorship written", written)
    }

    /// `room`'s records are sealed channels each app opens itself, never
    /// documents this device's owner commits.
    fn assert_sealed(&self, room: RoomId) {
        let status = self.net().native_owner_status();
        self.assert_sealed_under(room, &status);
    }

    /// Whether `room`'s records can be read here yet: not while this
    /// device's native owner opens, which on a busy disk takes seconds after
    /// a session starts and holds the room's records neither way until it
    /// has. Once it has opened, or failed to, they must be sealed. The
    /// owner's standing is read once, so the answer and the assertion are
    /// about the same moment.
    fn records_sealed(&self, room: RoomId) -> bool {
        let status = self.net().native_owner_status();
        if matches!(status, super::native_enrollment::NativeOwnerStatus::Opening) {
            return false;
        }
        self.assert_sealed_under(room, &status);
        true
    }

    /// [`Self::assert_sealed`] with the native owner standing as `status`.
    fn assert_sealed_under(
        &self,
        room: RoomId,
        status: &super::native_enrollment::NativeOwnerStatus,
    ) {
        let (sealed, committed) = {
            let tree = self.tree.read();
            (
                tree.seals_records(room),
                tree.committed_record_rooms.contains(&room),
            )
        };
        let standing = status.describe();
        let failure = self.net().open_failure().unwrap_or_default();
        let governor = match status {
            super::native_enrollment::NativeOwnerStatus::Open(owner) => Some(owner),
            _ => None,
        }
        .and_then(|owner| {
            owner
                .with_current(room, |view| {
                    Ok(format!(
                        "{:?} {:?}",
                        view.room_host().device_label,
                        view.room_host().capabilities
                    ))
                })
                .ok()
        })
        .unwrap_or_default();
        assert!(
            sealed,
            "{} holds {room} as a sealed room (its records committed instead: {committed}; \
             its enrollment names a governor that takes part in them: {}, the governor {governor}; \
             its native owner is {standing} {failure})",
            self.label,
            self.net().native_room_seals_records(room)
        );
    }

    /// The note's channel keys in `room`, once a participant admitted
    /// this device: what the identity service answers the notes app.
    fn note_keys(&self, room: RoomId) -> Option<channel_crypto::ChannelContentKeys> {
        let Some((channels, _)) = self.channels.as_ref() else {
            // A device serving through its session's broker
            // ([`Self::carry_layouts`]) holds its channels there: asked as
            // the notes app asks it.
            let keys = self.endpoint_keys.lock().unwrap().clone()?;
            let reply = keys(&room.to_string(), NOTES, NOTE_RECORD, 0).ok()?;
            return match reply.access {
                runtime_identity::wire::ChannelAccess::Ready => {
                    channel_crypto::ChannelContentKeys::from_bytes(&reply.keys?).ok()
                }
                runtime_identity::wire::ChannelAccess::Waiting
                | runtime_identity::wire::ChannelAccess::Unavailable => None,
                other => panic!("{} cannot open the note's channel: {other:?}", self.label),
            };
        };
        let route = collab_types::ChannelRoute::new(NOTES, NOTE_RECORD);
        match channels.access_record(room, &route).0 {
            ChannelAccess::Ready(keys) => Some(keys),
            ChannelAccess::Waiting => None,
            other => panic!("{} cannot open the note's channel: {other:?}", self.label),
        }
    }

    /// Where this device stands on the note's channel in `room`, as the
    /// identity service answers the notes app, refusals included.
    pub(crate) fn note_access(&self, room: RoomId) -> ChannelAccess {
        self.record_access(room, NOTE_RECORD)
    }

    /// Where this device stands on the channel of the notes app's `record`
    /// in `room`, as the identity service answers the app, refusals
    /// included.
    pub(crate) fn record_access(&self, room: RoomId, record: &str) -> ChannelAccess {
        let Some((channels, _)) = self.channels.as_ref() else {
            return ChannelAccess::Unavailable;
        };
        let route = collab_types::ChannelRoute::new(NOTES, record);
        channels.access_record(room, &route).0
    }

    /// Every sealed write to the note in `room` this device made or opened
    /// so far, in order.
    pub(crate) fn note_writes(&self, room: RoomId) -> Vec<collab_types::SignedOp> {
        self.note_writes
            .lock()
            .unwrap()
            .get(&room)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether this device holds the note's channel in `room`, so the notes
    /// app here reads and writes it.
    pub(crate) fn holds_note(&self, room: RoomId) -> bool {
        if !self.records_sealed(room) {
            return false;
        }
        self.note_keys(room).is_some()
    }

    /// Write `text` into the note in `room`, as the notes app does: a
    /// change to its document sealed under the record's channel keys,
    /// signed by this device and published to the room's host. Answers the
    /// signed write.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the note's document is edited and its change taken in one hold"
    )]
    pub(crate) fn write_note(&self, room: RoomId, text: &str) -> collab_types::SignedOp {
        self.assert_sealed(room);
        let keys = self
            .note_keys(room)
            .unwrap_or_else(|| panic!("{} holds the note's channel", self.label));
        self.read_notes(room, &keys);
        let key = self.key();
        let changes = {
            let mut notes = self.notes.lock().unwrap();
            let document = notes.entry(room).or_default();
            document.set_actor(automerge::ActorId::from(
                key.verifying_key().to_bytes().to_vec(),
            ));
            let heads = document.get_heads();
            document.put(automerge::ROOT, NOTE_TEXT, text).unwrap();
            document.commit_with(typed());
            document.save_after(&heads)
        };
        let room_id = room.to_string();
        let artifact = collab_types::document_artifact_id(NOTES, &room_id, NOTE_RECORD);
        let mut frames = collab_types::DocumentMutation::Incremental(changes)
            .encode_frames()
            .unwrap();
        assert_eq!(frames.len(), 1, "a note's edit is one frame");
        let frame = frames.remove(0);
        // Sealed under the storage class its frame is, as every app's
        // collaboration seals it: an edit is a delta, never a checkpoint.
        let (metadata, sealed) = keys
            .seal_content(
                artifact,
                collab_types::ContentRetention::of_frame(&frame),
                &frame,
            )
            .unwrap();
        let now = SystemTime::now();
        let mut op = collab_types::SignedOp::new_unsigned_with_encryption(
            sealed,
            artifact,
            collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            collab_types::document_attestation_id(NOTES, &room_id, NOTE_RECORD),
            collab_types::OpKind::Change,
            now,
            collab_types::next_unordered_document_sequence(now),
            metadata,
        );
        collab_types::sign_op(&mut op, &key).unwrap();
        self.note_writes
            .lock()
            .unwrap()
            .entry(room)
            .or_default()
            .push(op.clone());
        self.net()
            .publish_document(NOTES.into(), room_id, NOTE_RECORD, op.clone());
        op
    }

    /// Open every sealed write to the note in `room` that reached this
    /// device, into the note's document here.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "every arrived write opens into the note's document in one hold"
    )]
    fn read_notes(&self, room: RoomId, keys: &channel_crypto::ChannelContentKeys) {
        let arrived = self.arrived_note_writes(room);
        let mut notes = self.notes.lock().unwrap();
        let document = notes.entry(room).or_default();
        for envelope in arrived {
            {
                let mut opened = self.note_writes.lock().unwrap();
                let opened = opened.entry(room).or_default();
                if !opened.contains(&envelope) {
                    opened.push(envelope.clone());
                }
            }
            let opened = keys
                .open_content(
                    envelope.artifact,
                    envelope.encryption.as_ref().expect("the note is sealed"),
                    &envelope.payload,
                )
                .unwrap_or_else(|error| panic!("{} opens the note: {error}", self.label));
            match collab_types::DocumentFrame::decode(&opened).unwrap() {
                collab_types::DocumentFrame::Whole(collab_types::DocumentMutation::Snapshot(
                    bytes,
                )) => {
                    document
                        .merge_incoming(&mut automerge::AutoCommit::load(&bytes).unwrap())
                        .unwrap();
                }
                collab_types::DocumentFrame::Whole(
                    collab_types::DocumentMutation::Incremental(bytes),
                ) => {
                    document.load_incoming(&bytes).unwrap();
                }
                other => panic!("a note's write is one whole frame: {other:?}"),
            }
        }
    }

    /// The sealed writes to the note in `room` that reached this device. A
    /// device serving through its session's broker ([`Self::carry_layouts`])
    /// has the broker drain what the network received, and finds them where
    /// the notes app's read does: every write the broker admitted, which
    /// the note's document takes again as the changes it holds. Otherwise
    /// those that arrived since it last looked wait in the network's
    /// inbound documents.
    fn arrived_note_writes(&self, room: RoomId) -> Vec<collab_types::SignedOp> {
        let room_id = room.to_string();
        let artifact = collab_types::document_artifact_id(NOTES, &room_id, NOTE_RECORD);
        let admitted = SESSION_LAYOUTS.with(|held| {
            held.borrow().get(&self.layouts).map(|layouts| {
                layouts
                    .broker
                    .channels()
                    .lock()
                    .unwrap()
                    .sealed_writes(artifact)
            })
        });
        if let Some(admitted) = admitted {
            return admitted;
        }
        let inbound = self.net().inbound_documents_handle();
        let notes: Vec<_> = {
            let mut inbound = inbound.lock().unwrap();
            let (notes, rest) = std::mem::take(&mut *inbound)
                .into_iter()
                .partition(|document| {
                    document.app_id == NOTES
                        && document.cluster_id == room_id
                        && document.record_id == NOTE_RECORD
                });
            *inbound = rest;
            notes
        };
        notes.into_iter().map(|entry| *entry.envelope).collect()
    }

    /// The note's text in `room` as the notes app here shows it.
    pub(crate) fn note(&self, room: RoomId) -> Option<String> {
        self.net.as_ref()?;
        if !self.records_sealed(room) {
            return None;
        }
        let keys = self.note_keys(room)?;
        self.read_notes(room, &keys);
        let notes = self.notes.lock().unwrap();
        let text = notes
            .get(&room)?
            .get(automerge::ROOT, NOTE_TEXT)
            .ok()??
            .0
            .as_str()
            .map(str::to_owned);
        drop(notes);
        text
    }

    // What the person sees.

    /// The rooms of the workspaces this device lists, in order.
    pub(crate) fn workspace_rooms(&self) -> Vec<RoomId> {
        self.tree
            .read()
            .desktop
            .workspaces
            .iter()
            .map(crate::shell::state::Workspace::room)
            .collect()
    }

    /// How many workspaces this device lists for `room`.
    pub(crate) fn workspaces_of(&self, room: RoomId) -> usize {
        self.workspace_rooms()
            .into_iter()
            .filter(|listed| *listed == room)
            .count()
    }

    /// Whether this device holds `room`: its workspace is listed and no
    /// longer says Joining….
    pub(crate) fn holds(&self, room: RoomId) -> bool {
        let tree = self.tree.read();
        tree.holds_room(room)
            && !tree.is_joining(room)
            && tree
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room() == room)
    }

    /// The tabs of `room`'s workspace as this device shows them.
    pub(crate) fn tabs(&self, room: RoomId) -> Vec<(u64, String)> {
        self.tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .map(|workspace| {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .map(|tab| (tab.id, tab.title.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn shows_tab(&self, room: RoomId, tab: u64) -> bool {
        self.tabs(room).iter().any(|(shown, _)| *shown == tab)
    }

    /// The people in `room`'s roster as this device holds it.
    pub(crate) fn roster(&self, room: RoomId) -> BTreeSet<[u8; 32]> {
        self.net
            .as_ref()
            .and_then(|net| net.native_owner().ok())
            .and_then(|owner| {
                owner
                    .with_current(room, |view| {
                        Ok(view
                            .peer()
                            .certificates()
                            .iter()
                            .map(|cert| cert.user_public_key)
                            .collect())
                    })
                    .ok()
            })
            .unwrap_or_default()
    }

    /// `room`'s governance as this device holds it: its owner, its members'
    /// roles and its grants.
    pub(crate) fn policy(&self, room: RoomId) -> Option<tilo_action_policy::RoomPolicy> {
        let owner = self.net.as_ref()?.native_owner().ok()?;
        owner
            .with_current(room, |view| {
                Ok(view
                    .peer()
                    .permissions(view.device().device_public_key, SystemTime::now())
                    .map_err(|error| error.to_string())?
                    .policy)
            })
            .ok()
    }

    /// The role `person` holds in `room`, as this device sees it.
    pub(crate) fn role_of(
        &self,
        room: RoomId,
        person: &Person,
    ) -> Option<tilo_action_policy::Role> {
        self.policy(room)?.role_of(&person.id())
    }

    /// Whether `room`'s governance lets `app` keep records there, as this
    /// device sees it.
    pub(crate) fn app_may_author(&self, room: RoomId, app: &str) -> bool {
        self.policy(room).is_some_and(|policy| {
            policy
                .grants
                .iter()
                .any(|grant| grant.subject == tilo_action_policy::Subject::App(app.into()))
        })
    }

    /// Ask the host of `room`, which this device administers, for its
    /// governance action `action` with `arguments`, as the room's own
    /// settings do: what the host answered.
    pub(crate) fn govern(
        &self,
        room: RoomId,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let net = self.net();
        let target = net
            .administered_room(room)
            .ok_or_else(|| format!("{} administers no session with the room's host", self.label))?;
        net.govern(&target, action, arguments)
    }

    /// Change `person`'s role in `room` from the workspace's people
    /// settings: written by this device into its replica of the room.
    pub(crate) fn set_role(&self, room: RoomId, person: &Person, role: &str) {
        let role = match role {
            "viewer" => tilo_action_policy::Role::Viewer,
            "commenter" => tilo_action_policy::Role::Commenter,
            "editor" => tilo_action_policy::Role::Editor,
            "moderator" => tilo_action_policy::Role::Moderator,
            other => panic!("no role {other}"),
        };
        self.net()
            .write_governance_change(
                room,
                crate::shell::state::GovernanceRequest::Policy(
                    tilo_action_policy::Change::SetRole {
                        person: hex::encode(person.user()),
                        role,
                    },
                ),
            )
            .unwrap_or_else(|error| {
                panic!("this device changes the role: {}", String::from(error))
            });
    }

    /// Remove `person` from `room`, as its owner on this device.
    pub(crate) fn remove_member(&self, room: RoomId, person: &Person) {
        let net = self.net();
        let target = net
            .administered_room(room)
            .unwrap_or_else(|| panic!("{} administers the room it owns", self.label));
        let mut arguments = serde_json::Map::new();
        arguments.insert("person".into(), hex::encode(person.user()).into());
        net.govern(
            &target,
            tilo_action_policy::roles::governance::REMOVE_MEMBER,
            arguments,
        )
        .unwrap_or_else(|error| panic!("the room's host removes the member: {error}"));
    }

    /// The rooms this device's own host serves; empty while it runs none.
    pub(crate) fn hosted_rooms(&self) -> Vec<RoomId> {
        let Some(host) = self.net.as_ref().and_then(|net| net.room_host()) else {
            return Vec::new();
        };
        let runtime = self.net().runtime_handle().unwrap();
        runtime
            .block_on(host.rooms().list())
            .into_iter()
            .filter_map(|summary| RoomId::parse(&summary.room))
            .collect()
    }

    /// Where this device's own host keeps what it retains of its rooms.
    pub(crate) fn host_state_root(&self) -> PathBuf {
        assert!(self.net().room_host().is_some(), "the device runs its host");
        super::embedded_host::host_state_root(&self.dir.join("state.json"))
    }

    /// Where this device stands on `room`'s note channel, for
    /// [`Self::catch_up`].
    fn channel_standing(&self, room: RoomId) -> String {
        // A device serving through its session's broker holds its channels
        // there: what the broker hands the notes app, and the shared desktop
        // record, whose writes carry the room's windows.
        if self.carries_layouts {
            let note = self.note_keys(room).map_or("not handed", |_| "ready");
            let desktop = self.endpoint_keys.lock().unwrap().clone().map_or_else(
                || "no broker".to_owned(),
                |keys| match keys(
                    &room.to_string(),
                    tilo_desktop_model::ownership::DESKTOP_APP,
                    tilo_desktop_model::ownership::DESKTOP_RECORD,
                    0,
                ) {
                    Ok(reply) => format!("{:?}", reply.access),
                    Err(error) => error.to_string(),
                },
            );
            format!("{note} by the session's broker, the desktop record's {desktop}")
        } else {
            match self.note_access(room) {
                ChannelAccess::Ready(_) => "ready".to_owned(),
                other => format!("{other:?}"),
            }
        }
    }

    /// Where this device stands on `room`, for a wait that timed out: its
    /// native owner, whether it holds the room, the note as the notes app
    /// here has it (and the changes it still lacks), the note's writes
    /// waiting in its inbound documents, what its own host retains of the
    /// room, its tabs, and its sessions with the room's hosts.
    pub(crate) fn catch_up(&self, room: RoomId) -> String {
        let Some(net) = self.net.as_ref() else {
            return format!("{}: offline", self.label);
        };
        let status = net.native_owner_status();
        let owner = format!(
            "{}{}",
            status.describe(),
            net.open_failure()
                .map(|failure| format!(" ({failure})"))
                .unwrap_or_default()
        );
        let (holds, sealed) = {
            let tree = self.tree.read();
            (
                tree.holds_room(room) && !tree.is_joining(room),
                tree.seals_records(room),
            )
        };
        let access = self.channel_standing(room);
        let (text, heads, missing) = {
            let mut notes = self.notes.lock().unwrap();
            notes.get_mut(&room).map_or((None, 0, 0), |document| {
                let text = document
                    .get(automerge::ROOT, NOTE_TEXT)
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| value.into_string().ok());
                (
                    text,
                    document.get_heads().len(),
                    document.get_missing_deps(&[]).len(),
                )
            })
        };
        let writes = self.note_writes(room).len();
        let room_id = room.to_string();
        let waiting = net
            .inbound_documents_handle()
            .lock()
            .unwrap()
            .iter()
            .filter(|document| document.cluster_id == room_id && document.record_id == NOTE_RECORD)
            .count();
        let retained = net.room_host().map_or_else(
            || "no host".to_owned(),
            |host| {
                let workspace = tilo_relay::room_workspace_id(&room_id).unwrap();
                let records = host.room_records();
                let retained = net
                    .wait_on(async move { records.retained(workspace).await })
                    .and_then(std::convert::identity);
                match retained {
                    Ok(bodies) => {
                        let authors: Vec<String> = bodies
                            .iter()
                            .filter(|body| {
                                matches!(
                                    postcard::from_bytes::<crate::network::protocol::NeighbourMsg>(&body.body),
                                    Ok(crate::network::protocol::NeighbourMsg::AppDocument { app_id, .. })
                                        if app_id == NOTES
                                )
                            })
                            .map(|body| hex::encode(&body.author[..2]))
                            .collect();
                        format!("{} bodies, note writes by {authors:?}", bodies.len())
                    }
                    Err(error) => error,
                }
            },
        );
        let tabs: Vec<u64> = self.tabs(room).into_iter().map(|(tab, _)| tab).collect();
        let dials = net.wire_tally().map(|tally| {
            let counts = tally.snapshot();
            let count = |kind| counts.get(kind).copied().unwrap_or(0);
            format!(
                "governor {}, other host {}; host inputs {}, sealed writes replayed {}, \
                 live {}; ready {}, closed {}",
                count("NativeGovernorDial"),
                count("NativeHostDial"),
                count("HostNativeInput"),
                count("HostSealedReplay"),
                count("HostSealedLive"),
                count("HostReady"),
                count("HostClosed"),
            )
        });
        format!(
            "{} ({}): owner {owner}; holds the room {holds}, sealed {sealed}; note channel {access}; \
             note {text:?} ({heads} heads, {missing} changes missing, {writes} writes opened, \
             {waiting} waiting); own host retains {retained}; tabs {tabs:?}; \
             sessions {}",
            self.label,
            hex::encode(&self.key().verifying_key().to_bytes()[..2]),
            dials.unwrap_or_default()
        )
    }

    /// Open a connection to `other`, as two of a person's devices that find
    /// each other on the network do.
    pub(crate) fn reach(&self, other: &Self) {
        let addr = until(&[self, other], "the other device to listen", || {
            other.net().dial_addr()
        });
        self.net()
            .cmd_tx
            .try_send(NetCmd::Connect { addr })
            .unwrap();
    }
}

/// How the notes app commits what a person typed: a direct edit of the
/// record that names no finer action.
fn typed() -> automerge::transaction::CommitOptions {
    automerge::transaction::CommitOptions::default()
        .with_message(tilo_actions::FrameworkChange::Native.change_message())
}

/// Run `devices`' loops until `probe` answers, waking only when one of
/// them is woken or reaches a deadline of its own loop; fail at the shared
/// hang bound naming `what`.
pub(crate) fn until<T>(devices: &[&Device], what: &str, probe: impl FnMut() -> Option<T>) -> T {
    until_or_say(devices, what, probe, String::new)
}

/// [`until`] for a device catching up on `room`: at the hang bound it
/// says where each of `devices` stands on the room, so a timeout names
/// what is missing.
pub(crate) fn until_caught_up<T>(
    devices: &[&Device],
    what: &str,
    room: RoomId,
    probe: impl FnMut() -> Option<T>,
) -> T {
    until_or_say(devices, what, probe, || {
        devices.iter().fold(String::new(), |mut said, device| {
            said.push_str("\n  ");
            said.push_str(&device.catch_up(room));
            said
        })
    })
}

/// [`until`], saying `standing` when it times out.
fn until_or_say<T>(
    devices: &[&Device],
    what: &str,
    mut probe: impl FnMut() -> Option<T>,
    standing: impl Fn() -> String,
) -> T {
    let bound = tilo_test_env::hang_deadline();
    let wakes = &devices
        .first()
        .expect("a wait is for at least one device")
        .wakes;
    loop {
        wakes.consume();
        for device in devices {
            device.frame();
        }
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            Instant::now() < bound,
            "timed out waiting for {what}{}\n  what became of requests to rooms' hosts:{}",
            standing(),
            tilo_network_actor::room_governance::trace::said()
        );
        let next = devices
            .iter()
            .filter_map(|device| device.next_deadline())
            .min()
            .map_or(bound, |next| next.min(bound));
        wakes.wait_until(next);
    }
}

/// A fresh directory for one test's devices.
pub(crate) fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

/// Make `name` on `device` and wait until its host serves it and its owner
/// holds it as a member.
pub(crate) fn hosted_workspace(
    device: &Device,
    others: &[&Device],
    name: &str,
) -> (WorkspaceId, RoomId) {
    let (local, room) = device.create_workspace(name);
    let mut all = vec![device];
    all.extend_from_slice(others);
    until(&all, "the new workspace to be hosted on its device", || {
        (device.holds(room) && device.hosted_rooms().contains(&room)).then_some(())
    });
    drop(all);
    (local, room)
}
