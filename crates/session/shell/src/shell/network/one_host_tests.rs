//! End to end over the one host stack: every device runs the same room host
//! the relay runs, in its own session, and its native owner is a member of
//! every room, wherever the room is hosted. Real Iroh endpoints on loopback,
//! real shells' networks, real hosts and a real relay; nothing is stubbed.
//!
//! Each test drives its devices the way the compositor's loop does: a device
//! runs a turn ([`Device::frame`]) when its network wakes the loop, and
//! otherwise only at the deadlines that loop keeps itself (a room push
//! retry, the layout check after the tree changed, the join card's poll of
//! an enrollment in flight). A wait ([`until`]) blocks on those wakes; it
//! never sleeps on a guess.
//!
//! What the tests assert is what a person sees: the workspace list, a
//! workspace's room, its windows and tabs, an app's document and the room's
//! roster.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use ed25519_dalek::SigningKey;
use tilo_automerge::ApplyIncoming as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::{HostIdentity, NetCmd, Reach, ShellNetwork};
use crate::shell::channel_encryption::{ChannelAccess, ChannelEncryptionConfig, ChannelKeyAccess};
use crate::shell::collab_core::BrokerChannels;
use crate::shell::peer_sync_overlay::{InvitationTerms, PeerSyncResponse};
use crate::shell::state::{
    AppKind, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceId,
    WorkspaceTree, WorkspaceTreeHandle,
};

mod adopted_fields;
mod channels;
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
mod own_devices;
mod sealed_records;

/// The app whose document the tests edit, and its record.
pub(crate) const NOTES: &str = "so.tilo.notes";
const NOTE_RECORD: &str = "plan";
/// The note's text, a field of the record's document.
const NOTE_TEXT: &str = "text";

/// How often the compositor looks at the windows it keeps in its rooms
/// after its tree changed (`compositor/native.rs`).
const LAYOUT_CHECK: Duration = Duration::from_millis(500);
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
    /// The tree's write generation the windows were last looked at in, and
    /// when.
    layout: Option<(u64, Instant)>,
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
        }
        .unwrap();
        let wakes = Arc::clone(&self.wakes);
        net.set_wake(Arc::new(move || wakes.wake()));
        assert!(net.join_default_workspace());
        // As a session starts: the rooms this device hosted before are
        // served again.
        net.open_hosted_rooms();
        let net = Arc::new(net);
        let channels = self.open_channels(&net);
        let drain = Drain::start(&net, channels.clone(), &self.wakes);
        self.channels = Some((channels, drain));
        self.net = Some(net);
    }

    /// Take this device off the network, as shutting it down does: its
    /// session ends, its state stays on disk.
    pub(crate) fn go_offline(&mut self) {
        let net = self.net.take().expect("the device is online");
        self.channels = None;
        self.notes.lock().unwrap().clear();
        crate::shell::persist::wait_for_saves();
        drop(
            Arc::try_unwrap(net)
                .unwrap_or_else(|_| panic!("{}'s network is not shared", self.label)),
        );
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
        let mut vault = [0x5A; 32];
        vault[..2].copy_from_slice(&self.salt.to_le_bytes());
        crate::shell::collab_broker::channel_key_access(
            self.secret_store.then(|| ChannelEncryptionConfig {
                root: self.dir.join("channels"),
                vault: channel_crypto::VaultKey::new(vault),
                device_key: self.key(),
            }),
            self.tree.clone(),
            Some(Arc::clone(net)),
            Arc::new(Mutex::new(BrokerChannels::default())),
        )
        .expect("the device's channel state opens")
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

    /// One turn of this device's compositor loop: the network's tick, the
    /// windows it keeps in its rooms when its tree changed, and the join
    /// card's poll of an enrollment in flight. Nothing while it is offline.
    pub(crate) fn frame(&self) {
        let Some(net) = self.net.as_ref() else {
            return;
        };
        net.tick();
        let now = Instant::now();
        let generation = self.tree.generation();
        let look = {
            let state = self.state.lock().unwrap();
            state.layout.is_none_or(|(seen, at)| {
                seen != generation && now.saturating_duration_since(at) >= LAYOUT_CHECK
            })
        };
        if look {
            let rooms: Vec<RoomId> = self
                .tree
                .read()
                .desktop
                .workspaces
                .iter()
                .map(crate::shell::state::Workspace::room)
                .collect();
            for room in rooms {
                net.publish_hosted_legacy_windows(room, &self.tree);
            }
            self.state.lock().unwrap().layout = Some((self.tree.generation(), now));
        }
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
        let (layout, polled) = {
            let state = self.state.lock().unwrap();
            (state.layout, state.polled)
        };
        let layout = layout.and_then(|(seen, at)| {
            (seen != self.tree.generation()).then(|| at.checked_add(LAYOUT_CHECK).unwrap_or(at))
        });
        let poll = (!self.enrollments.lock().unwrap().is_empty()).then(|| {
            polled.map_or_else(Instant::now, |at| {
                at.checked_add(ENROLLMENT_POLL).unwrap_or(at)
            })
        });
        [layout, poll].into_iter().flatten().min()
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
    /// its sealed channel, which this device asks the room's steward for.
    /// Every hosted room is sealed, wherever it is hosted. Whoever opens
    /// the app later finds the record.
    pub(crate) fn open_notes_first(&self, room: RoomId) {
        self.let_notes_author(room)
            .unwrap_or_else(|error| panic!("the host lets the notes app author: {error}"));
        self.assert_sealed(room);
        let _ = self.note_keys(room);
    }

    /// Ask `room`'s host to let the notes app author the room's records, as
    /// this device: the host's answer.
    pub(crate) fn let_notes_author(&self, room: RoomId) -> Result<(), String> {
        let net = Arc::clone(self.net());
        let cert = self.cert();
        let runtime = net.runtime_handle().unwrap();
        runtime
            .block_on(net.grant_app_authorship_at_host(room, &cert, NOTES))
            .unwrap_or_else(|| Err(format!("{} reaches no host governing the room", self.label)))
            .map(|_| ())
    }

    /// Ask `room`'s host to let `app` author the room's records, as this
    /// device, as opening the app there first does: the host's answer.
    pub(crate) fn let_app_author(&self, room: RoomId, app: &str) -> Result<(), String> {
        let net = Arc::clone(self.net());
        let cert = self.cert();
        let runtime = net.runtime_handle().unwrap();
        runtime
            .block_on(net.grant_app_authorship_at_host(room, &cert, app))
            .unwrap_or_else(|| Err(format!("{} reaches no host governing the room", self.label)))
            .map(|_| ())
    }

    /// `room`'s records are sealed channels each app opens itself, never
    /// documents this device's owner commits.
    fn assert_sealed(&self, room: RoomId) {
        let (sealed, committed) = {
            let tree = self.tree.read();
            (
                tree.seals_records(room),
                tree.committed_record_rooms.contains(&room),
            )
        };
        let status = self.net().native_owner_status();
        let standing = status.describe();
        let failure = self.net().open_failure().unwrap_or_default();
        let governor = status
            .open()
            .and_then(|owner| {
                owner
                    .with_current(room, |view| {
                        Ok(format!(
                            "{:?} {:?}",
                            view.governor().device_label,
                            view.governor().capabilities
                        ))
                    })
                    .ok()
            })
            .unwrap_or_default();
        assert!(
            sealed,
            "{} holds {room} as a sealed room (its records committed instead: {committed}; \
             its enrollment names a governor that stewards them: {}, the governor {governor}; \
             its native owner is {standing} {failure})",
            self.label,
            self.net().native_room_seals_records(room)
        );
    }

    /// The note's channel keys in `room`, once the room's steward admitted
    /// this device: what the identity service answers the notes app.
    fn note_keys(&self, room: RoomId) -> Option<channel_crypto::ChannelContentKeys> {
        let (channels, _) = self.channels.as_ref()?;
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
        let Some((channels, _)) = self.channels.as_ref() else {
            return ChannelAccess::Unavailable;
        };
        let route = collab_types::ChannelRoute::new(NOTES, NOTE_RECORD);
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
        self.assert_sealed(room);
        self.note_keys(room).is_some()
    }

    /// Write `text` into the note in `room`, as the notes app does: a
    /// change to its document sealed under the record's channel keys,
    /// signed by this device and published to the room's host.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the note's document is edited and its change taken in one hold"
    )]
    pub(crate) fn write_note(&self, room: RoomId, text: &str) {
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
            .publish_document(NOTES.into(), room_id, NOTE_RECORD, op);
    }

    /// Open every sealed write to the note in `room` that reached this
    /// device, into the note's document here.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "every arrived write opens into the note's document in one hold"
    )]
    fn read_notes(&self, room: RoomId, keys: &channel_crypto::ChannelContentKeys) {
        let room_id = room.to_string();
        let arrived: Vec<_> = {
            let inbound = self.net().inbound_documents_handle();
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
        let mut notes = self.notes.lock().unwrap();
        let document = notes.entry(room).or_default();
        for entry in arrived {
            let envelope = entry.envelope;
            self.note_writes
                .lock()
                .unwrap()
                .entry(room)
                .or_default()
                .push((*envelope).clone());
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

    /// The note's text in `room` as the notes app here shows it.
    pub(crate) fn note(&self, room: RoomId) -> Option<String> {
        self.net.as_ref()?;
        self.assert_sealed(room);
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
                    .permissions(view.device().device_public_key, SystemTime::now())?
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
    /// settings: the room's `set-role` governance action, asked of the
    /// room's host.
    pub(crate) fn set_role(&self, room: RoomId, person: &Person, role: &str) {
        let net = self.net();
        let target = net
            .administered_room(room)
            .unwrap_or_else(|| panic!("{} administers the room it owns", self.label));
        let mut arguments = serde_json::Map::new();
        arguments.insert("person".into(), hex::encode(person.user()).into());
        arguments.insert("role".into(), role.into());
        net.govern(
            &target,
            tilo_action_policy::roles::governance::SET_ROLE,
            arguments,
        )
        .unwrap_or_else(|error| panic!("the room's host changes the role: {error}"));
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
pub(crate) fn until<T>(devices: &[&Device], what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
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
        assert!(Instant::now() < bound, "timed out waiting for {what}");
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
