//! This device's end-to-end encrypted groups: the keys of every record the
//! workspaces it holds keep sealed, the governance that decides who reads each one,
//! and the devices each workspace admits, which its window streams carry between.
//! A sealed record is opened on no one's machine but a member's: the workspace's
//! devices agree, group by group, that exactly the devices its access policy admits
//! take part ([`KeyDirectory`]), and each sealed write travels with the key
//! material of its group. The keys live here, never in the document, which is what
//! lets a person invited today read what the workspace said a year ago while its
//! membership moves on.
//! Nothing here decides who may read what: the workspace's governance does, and
//! this device only follows it.

// The page's session runs no key service of its own: only a desktop's build settles a
// workspace's groups here.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        unused_imports,
        reason = "a page runs its workspaces' groups with its own host"
    )
)]
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_clock::time::UNIX_EPOCH;

use crate::network::{Cmd, RosterDevice, Topic, topic_of};

use collab_types::Ed25519PublicKey;
use neighbour_wire::NeighbourMsg;
#[cfg(target_os = "linux")]
use tilo_broker_core::collab_core::{BrokerChannels, BrokerSealChecks};
use tilo_desktop_model::WorkspaceId as ModelWorkspaceId;
use tilo_identity::policy_change;
use tilo_lock::MutexRecover as _;
use tilo_workspace_keys::{
    GovernancePark, GovernanceTransport, InboundGovernance, KeyDirectory, SharedKeys,
};
#[cfg(target_os = "linux")]
use tilo_workspace_keys::{GovernancePump, KeysConfig, SealCheckRegistry, WorkspaceKeys};

use crate::engine::Engine;
use tilo_peer_state::owner::NativeWorkspaceOwner;

pub mod vault;
pub use vault::MlsVault;

/// The running key service: this device's keys, and the thread that keeps their groups in step
/// with the workspaces' governance. Stops when dropped.
pub struct KeyService {
    /// This device's keys and the groups they are in; `None` while no secret store
    /// keeps its group state.
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(dead_code, reason = "a host with threads runs its own pump")
    )]
    access: Option<SharedKeys>,
    /// The thread that settles the groups, on a host with threads.
    #[cfg(target_os = "linux")]
    _pump: GovernancePump,
}

impl KeyService {
    /// One pass of the group protocol over `transport`: what the thread does on a
    /// host that has one, for a host with a single thread (a page) to drive from its
    /// own loop. Answers how long until the next pass is due.
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(dead_code, reason = "a host with threads runs its own pump")
    )]
    pub fn pump(&self, transport: &dyn GovernanceTransport) -> Option<Duration> {
        self.access
            .as_ref()
            .and_then(|access| access.pump_until_due(transport))
    }
}

#[cfg(target_os = "linux")]
impl KeyService {
    /// Open this device's key service over the workspaces `engine` holds:
    /// its sealed group state under the platform secret store, and its
    /// groups settling through `link`, the workspace transport of this
    /// session's peer network. `settled` runs after every pass that
    /// settles them: keys a record waited for may be here since.
    ///
    /// Without a secret store the service still runs, for the workspaces this
    /// device's own workspace host takes part in: their keys come from that host,
    /// and this device's own sealed state is the only thing it cannot keep.
    ///
    /// # Errors
    /// The thread settling the groups could not start.
    pub fn open(
        engine: &Arc<Engine>,
        link: Arc<KeyLink>,
        broker: Arc<Mutex<BrokerChannels>>,
        mut settled: impl FnMut() + Send + 'static,
    ) -> std::io::Result<Self> {
        let directory = Box::new(OwnerDirectory::member(engine.owner().clone()));
        let checks: Arc<dyn SealCheckRegistry> = Arc::new(BrokerSealChecks(broker));
        let service = match MlsVault::open_for_session() {
            Ok(vault) => WorkspaceKeys::open(
                KeysConfig {
                    state: vault.group_state(),
                    vault: vault.group_state_key(),
                    signer: Arc::clone(&engine.identity().signer),
                    role: tilo_workspace_keys::KeyRole::Committer,
                },
                directory,
                Arc::clone(&checks),
            )
            .inspect_err(|error| {
                tracing::warn!(%error, "group encryption state could not be opened");
            })
            .ok(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "no system secret store answered: only workspaces this device hosts have group keys"
                );
                Some(WorkspaceKeys::without_store(directory, checks))
            }
        };
        let keys = engine.keys();
        let park = link.park();
        let poke = Arc::clone(&park);
        let access = service.map(|service| {
            let access = SharedKeys::with_wake(service, Arc::new(move || poke.poke()));
            keys.set(Some(access.clone()));
            link.answer_with(access.clone());
            access
        });
        let settled_access = access.clone();
        let mut rosters = Rosters::default();
        let engine_settled = Arc::clone(engine);
        let transport = Arc::clone(&link);
        let pump = GovernancePump::spawn(
            "tilo-workspace-keys",
            park,
            access,
            Arc::clone(&link) as Arc<dyn GovernanceTransport + Send + Sync>,
            move || {
                rosters.sweep(&transport, &engine_settled);
                settled();
            },
        )?;
        // Every workspace this device already holds reads its roster: the peer network carries a
        // workspace's window streams between the devices it admits from the moment it runs, not
        // only from the workspace's next change.
        for workspace in engine.workspaces() {
            link.workspace_changed(workspace);
        }
        engine.attach_link(link);
        Ok(Self {
            access: settled_access,
            _pump: pump,
        })
    }
}

/// The records this device asked a workspace's hosts for, by the workspace's topic and the record's
/// retained channel.
type AskedRecords = std::collections::HashMap<(Topic, [u8; 32]), AskedRecord>;

/// A record this device asked a workspace's hosts for.
#[derive(Default)]
struct AskedRecord {
    /// What the hosts answered: `None` while the answer is owed, the frames they sent once it came.
    answer: Option<tilo_server_protocol::DocumentAnswer>,
}

/// The workspace transport the channels' governance travels on: what this session's collaboration
/// peer network hears and says, reached from the thread that settles the groups.
/// It is also how the peer network learns that a workspace's state moved ([`Self::workspace_changed`]) and
/// which devices the workspace admits ([`Self::tell_roster`]), so a change the host published is
/// acted on in the pass after it was taken in.
pub struct KeyLink {
    /// Governance the peer network heard, waiting for the next settle.
    inbound: Mutex<Vec<InboundGovernance>>,
    /// The workspaces whose state moved since it was last read: their channels and their rosters
    /// are read again.
    changed: Mutex<BTreeSet<ModelWorkspaceId>>,
    /// The records this device asked the workspaces' hosts for, by topic and retained channel, and
    /// what each answered: `None` while the answer is owed, the frames it sent once it came
    /// ([`Self::resource_opened`]).
    records: Mutex<AskedRecords>,
    /// What this session says on the peer network, once it is running.
    outgoing: Mutex<Option<std::sync::Arc<dyn crate::Outbox>>>,
    /// The workspaces whose group's frames this device asked the person's other devices for, each
    /// with the epoch it held then ([`Self::want_frames`]).
    wanted: Mutex<BTreeMap<ModelWorkspaceId, u64>>,
    /// This device's keys, which answer what the person's other devices ask of its groups.
    keys: Mutex<Option<SharedKeys>>,
    park: Arc<GovernancePark>,
}

impl KeyLink {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inbound: Mutex::new(Vec::new()),
            changed: Mutex::new(BTreeSet::new()),
            records: Mutex::new(AskedRecords::new()),
            outgoing: Mutex::new(None),
            wanted: Mutex::new(BTreeMap::new()),
            keys: Mutex::new(None),
            park: GovernancePark::new(),
        })
    }

    /// Say `message` on the peer network from now on; `None` once the peer network is gone.
    pub fn attach(&self, outgoing: std::sync::Arc<dyn crate::Outbox>) {
        *self.outgoing.lock_recover() = Some(outgoing);
        // What was told before the peer network ran was told to no one: every workspace this device holds
        // is read and told again.
        self.park.poke();
    }

    /// Wake whatever unsettles the groups: a frame arrived, or a workspace moved.
    pub fn park(&self) -> Arc<GovernancePark> {
        Arc::clone(&self.park)
    }

    /// Whether the peer network this device says its workspaces' rosters on is running.
    fn attached(&self) -> bool {
        self.outgoing.lock_recover().is_some()
    }

    /// Take in one message the peer network heard. Anything but the groups' governance is left to the
    /// peer network.
    pub fn heard(&self, message: &NeighbourMsg) {
        let Some(governance) = InboundGovernance::from_message(message.clone()) else {
            return;
        };
        self.inbound.lock_recover().push(governance);
        self.park.poke();
    }

    /// Answer what the person's other devices ask of this device's groups with `keys`.
    pub fn answer_with(&self, keys: SharedKeys) {
        *self.keys.lock_recover() = Some(keys);
    }

    /// This device holds a write of `workspace` sealed under a key its group at `epoch` lacks: ask
    /// the person's other devices for the group's frames from there on, as the workspace's host
    /// would replay them, and for the frames of each of the workspace's reader groups this
    /// device is in from the epoch it holds of that one. Asked once per epoch held, and again
    /// whenever one of those devices comes up ([`Self::sibling_arrived`]).
    pub fn want_frames(&self, workspace: ModelWorkspaceId, epoch: u64) {
        let keys = self.keys.lock_recover().clone();
        let groups = keys
            .map(|keys| keys.reader_groups_of(workspace))
            .unwrap_or_default();
        for (group, epoch) in std::iter::once((workspace, epoch)).chain(groups) {
            self.want_group(group, epoch);
        }
    }

    /// This device holds `write` of `workspace` and could not open it: ask for the frames of
    /// the group it is sealed under, which for a document sealed under its own group (the
    /// workspace's vault) is no group of the workspace's, and for the workspace's own
    /// ([`Self::want_frames`]).
    pub fn want_write_frames(&self, workspace: ModelWorkspaceId, write: &collab_types::SignedOp) {
        let group = write
            .encryption
            .map_or(workspace, |metadata| metadata.key_group(workspace));
        if group != workspace {
            self.want_group(group, 0);
        }
        self.want_frames(workspace, 0);
    }

    /// Ask the person's other devices for `group`'s frames from `epoch` on, once per epoch.
    fn want_group(&self, group: ModelWorkspaceId, epoch: u64) {
        if self.wanted.lock_recover().insert(group, epoch) == Some(epoch) {
            return;
        }
        self.tell_siblings(NeighbourMsg::MlsGovernanceWanted {
            workspace: group.to_string(),
            from_epoch: epoch,
        });
    }

    /// One of the person's devices came up: it hears what this device still waits for.
    pub fn sibling_arrived(&self) {
        // What a group moved past since it was asked for is not asked again.
        let keys = self.keys.lock_recover().clone();
        let wanted = {
            let mut wanted = self.wanted.lock_recover();
            wanted.retain(|workspace, epoch| {
                keys.as_ref()
                    .and_then(|keys| keys.record_keys(*workspace))
                    .and_then(Result::ok)
                    .is_none_or(|held| held.epoch() <= *epoch)
            });
            wanted.clone()
        };
        for (workspace, epoch) in wanted {
            self.tell_siblings(NeighbourMsg::MlsGovernanceWanted {
                workspace: workspace.to_string(),
                from_epoch: epoch,
            });
        }
    }

    /// Take in what `peer`, one of the person's devices, said on the session between the two: a
    /// group's frames and access requests go to the key service as the host's do, which judges
    /// them alike; an ask for frames is answered with this device's own, when the group admits
    /// `peer`, each in the workspace that carries the group.
    pub fn heard_from_sibling(&self, peer: [u8; 32], message: &NeighbourMsg) {
        match message {
            NeighbourMsg::MlsGovernance { .. } | NeighbourMsg::MlsAccessRequest { .. } => {
                self.heard(message);
            }
            NeighbourMsg::MlsGovernanceWanted {
                workspace,
                from_epoch,
            } => {
                let Some(workspace) = ModelWorkspaceId::parse(workspace) else {
                    return;
                };
                let keys = self.keys.lock_recover().clone();
                let Some(keys) = keys else {
                    return;
                };
                let frames =
                    keys.frames_for(workspace, &Ed25519PublicKey::from_bytes(peer), *from_epoch);
                // The frames go under the scope their group's governance travels in: a document's
                // frames name its route, and are read as that group's, not as the workspace's.
                let scope = keys
                    .scope_of(workspace)
                    .unwrap_or_else(|| keys.carrier(workspace).to_string());
                for frame in frames {
                    self.tell_siblings(NeighbourMsg::MlsGovernance {
                        workspace: scope.clone(),
                        frame: Box::new(frame),
                        delivery: 0,
                    });
                }
            }
            _ => {}
        }
    }

    /// Say `message` to the person's other devices whose sessions are up.
    fn tell_siblings(&self, message: NeighbourMsg) {
        let outgoing = self.outgoing.lock_recover().clone();
        if let Some(outgoing) = outgoing {
            let _ = outgoing.send(Cmd::TellSiblings(Box::new(message)));
        }
    }

    /// Say `message`, group governance of `workspace`, on the peer network; `false` when the peer network is
    /// not running.
    fn say(&self, workspace: ModelWorkspaceId, message: NeighbourMsg) -> bool {
        let outgoing = self.outgoing.lock_recover().clone();
        outgoing.is_some_and(|outgoing| {
            outgoing
                .send(Cmd::Publish {
                    ws: topic_of(workspace),
                    msg: Box::new(message),
                })
                .is_ok()
        })
    }

    /// Take what a workspace's governor never admits into its replica: the groups' governance,
    /// which the workspace's devices settle their groups with between each other. Says whether
    /// `message` was one of them, and so never reaches the workspace's state.
    pub fn dispatch(&self, message: &NeighbourMsg) -> bool {
        if message.governance_workspace().is_none() {
            return false;
        }
        self.heard(message);
        true
    }

    /// Workspace `workspace` 's state moved here: the devices it admits and the groups its
    /// records are may differ from what was last read.
    pub fn workspace_changed(&self, workspace: ModelWorkspaceId) {
        self.changed.lock_recover().insert(workspace);
        self.park.poke();
    }

    /// Read `workspace` again on a later pass, without waking the pump to it: a roster that was not
    /// told stays untold until it is.
    fn changed_again(&self, workspace: ModelWorkspaceId) {
        self.changed.lock_recover().insert(workspace);
    }

    /// The workspaces that moved since the last call, in a stable order.
    pub fn take_changed(&self) -> Vec<ModelWorkspaceId> {
        let mut changed = self.changed.lock_recover();
        let workspaces = changed.iter().copied().collect();
        changed.clear();
        workspaces
    }

    /// The host of the workspace on topic `ws` finished replaying what it retains to this device,
    /// after the replay's own writes.
    pub fn host_ready(&self, ws: Topic) {
        // A group that waited on the history is founded now, if it held no genesis.
        self.park.poke();
        // A host session that just came up owes this device the records it asked for before it was
        // there: a record asked for on a session that has since ended reaches here over this one.
        let asked: Vec<_> = self
            .records
            .lock_recover()
            .iter()
            .filter(|((asked_ws, _), _)| *asked_ws == ws)
            .map(|((_, channel), _)| *channel)
            .collect();
        self.ask_for(ws, asked);
    }

    /// Ask `workspace`'s hosts for the record on `channel`
    /// (`neighbour_wire::app_document_group`): what they hold of it, and their changes to it from
    /// then on. Asked once per record and session; a host session that comes up later asks again
    /// ([`Self::host_ready`]). `false` when the peer network is not running, so nothing was asked.
    pub fn open_resource(&self, workspace: ModelWorkspaceId, channel: [u8; 32]) -> bool {
        let ws = topic_of(workspace);
        {
            let mut records = self.records.lock_recover();
            if records.contains_key(&(ws, channel)) {
                return true;
            }
            records.insert((ws, channel), AskedRecord { answer: None });
        }
        self.ask_for(ws, [channel])
    }

    /// Say [`Cmd::OpenDocument`] for each of `channels` on the peer network: whether it was taken, or `true`
    /// when there is no peer network to ask on.
    fn ask_for(&self, ws: Topic, channels: impl IntoIterator<Item = [u8; 32]>) -> bool {
        let outgoing = self.outgoing.lock_recover().clone();
        channels.into_iter().all(|channel| {
            outgoing
                .as_ref()
                .is_none_or(|outgoing| outgoing.send(Cmd::OpenDocument { ws, channel }).is_ok())
        })
    }

    /// The host of the workspace on topic `ws` answered the record on `channel`: it sent
    /// everything it retains of it — none when the workspace has none of it, and then the record is
    /// new to whoever opens it — or it is another person's own, which this device never opens.
    pub fn resource_opened(
        &self,
        ws: Topic,
        channel: [u8; 32],
        answer: tilo_server_protocol::DocumentAnswer,
    ) {
        self.records
            .lock_recover()
            .entry((ws, channel))
            .or_default()
            .answer = Some(answer);
        self.park.poke();
    }

    /// What the hosts of `workspace` answered about the record on `channel`: `None` while the
    /// answer is owed.
    pub fn resource_answer(
        &self,
        workspace: ModelWorkspaceId,
        channel: &[u8; 32],
    ) -> Option<tilo_server_protocol::DocumentAnswer> {
        self.records
            .lock_recover()
            .get(&(topic_of(workspace), *channel))
            .and_then(|asked| asked.answer)
    }

    /// Ask the host of `workspace` to replay what it retains: a message the workspace held back
    /// waited on something the host sends of its own accord too, and the replay decides it again.
    pub fn ask_replay(&self, workspace: ModelWorkspaceId) {
        let outgoing = self.outgoing.lock_recover().clone();
        if let Some(outgoing) = outgoing {
            let _ = outgoing.send(Cmd::Replay(topic_of(workspace)));
        }
    }

    /// Send this device's owner's outgoing ownership of `workspace` to its peers: what the owner
    /// committed leaves this device on the peer network's session with the workspace's host, as every
    /// other change of the workspace does.
    pub fn publish_ownership(&self, workspace: ModelWorkspaceId) {
        let outgoing = self.outgoing.lock_recover().clone();
        if let Some(outgoing) = outgoing {
            let _ = outgoing.send(Cmd::PublishOwnership(topic_of(workspace)));
        }
    }

    /// Tell the peer network which devices workspace `ws` admits; `false` when the peer network is gone, and the
    /// roster is told again when a later pass finds it changed.
    fn tell_roster(&self, ws: Topic, devices: Roster) -> bool {
        let outgoing = self.outgoing.lock_recover().clone();
        outgoing.is_some_and(|outgoing| outgoing.send(Cmd::Roster { ws, devices }).is_ok())
    }
}

impl GovernanceTransport for KeyLink {
    fn publish(&self, message: NeighbourMsg) -> bool {
        let Some(workspace) = governance_workspace(&message) else {
            return false;
        };
        // The person's other devices hear it whether the workspace's host is there or not: a device
        // of theirs that fell behind asks them to be admitted again, and is admitted by them.
        self.tell_siblings(message.clone());
        self.say(workspace, message)
    }

    fn take_inbound(&self) -> Vec<InboundGovernance> {
        std::mem::take(&mut *self.inbound.lock_recover())
    }
}

/// The workspace `message`, a group's governance, is of; `None` for any other message.
fn governance_workspace(message: &NeighbourMsg) -> Option<ModelWorkspaceId> {
    message
        .governance_workspace()
        .and_then(ModelWorkspaceId::parse)
}

/// The workspaces whose rosters this session has told the peer network, by workspace topic: a roster is
/// told when the workspace it is of changed, and only then.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct Rosters {
    told: HashMap<Topic, Roster>,
}

#[cfg(target_os = "linux")]
impl Rosters {
    /// Whether the peer network is not holding `roster` as workspace `ws` 's yet.
    fn moved(&self, ws: Topic, roster: &Roster) -> bool {
        self.told.get(&ws) != Some(roster)
    }

    /// Tell the peer network the roster of each workspace that moved: the devices its governance admits
    /// now, which its window streams carry between. A device the workspace removed, or whose
    /// certificate ran out, is not on it, and what it sends or would be sent is refused from then
    /// on.
    fn sweep(&mut self, link: &KeyLink, engine: &Engine) {
        // A session with no peer network says its rosters to no one: the workspaces stay marked, and the
        // pass after the peer network starts tells them all ([`KeyLink::attach`]).
        if !link.attached() {
            return;
        }
        for workspace in link.take_changed() {
            if !engine.holds(workspace) {
                // The workspace is gone here, and what the peer network held of it went with the leave.
                self.told.remove(&topic_of(workspace));
                continue;
            }
            if let Ok(roster) = roster_of(engine, workspace) {
                self.tell(link, workspace, roster);
            } else {
                // A workspace whose governance cannot be read yet tells no roster: the one it had
                // stays until a pass reads it.
                tracing::debug!(%workspace, "no roster to tell");
                link.changed_again(workspace);
            }
        }
    }

    /// Tell the peer network the workspace's `roster` , and remember it as told only when the peer network took
    /// it: one it did not take is read again on a later pass.
    fn tell(&mut self, link: &KeyLink, workspace: ModelWorkspaceId, roster: Roster) {
        let ws = topic_of(workspace);
        if !self.moved(ws, &roster) {
            return;
        }
        if link.tell_roster(ws, roster.clone()) {
            self.told.insert(ws, roster);
        } else {
            link.changed_again(workspace);
        }
    }
}

/// The workspace's access policy as this device's own workspaces hold it: which devices the
/// workspace lets read each group.
pub struct OwnerDirectory {
    owner: NativeWorkspaceOwner,
    /// A reader also follows what the host it joined a workspace through and it itself sign: the
    /// host vouched for the workspace it was let into.
    reader: bool,
}

impl OwnerDirectory {
    /// The directory of a device that settles its groups.
    #[must_use]
    pub const fn member(owner: NativeWorkspaceOwner) -> Self {
        Self {
            owner,
            reader: false,
        }
    }

    /// The directory of a [`tilo_workspace_keys::KeyRole::Follower`] device.
    #[must_use]
    pub const fn reader(owner: NativeWorkspaceOwner) -> Self {
        Self {
            owner,
            reader: true,
        }
    }
}

impl KeyDirectory for OwnerDirectory {
    fn requires_document_keys(
        &self,
        workspace: ModelWorkspaceId,
        app: &str,
        document: &str,
    ) -> Option<bool> {
        self.owner
            .with_current(workspace, |view| {
                view.peer()
                    .requires_document_keys(app, document, tilo_clock::wall())
            })
            .ok()
    }

    fn document_devices(
        &self,
        workspace: ModelWorkspaceId,
        app: &str,
        document: &str,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let at = UNIX_EPOCH.checked_add(tilo_workspace_keys::policy_instant(now))?;
        self.owner
            .with_current(workspace, |view| {
                view.peer().document_key_holders(app, document, at)
            })
            .ok()
            .map(|readers| {
                readers
                    .into_iter()
                    .map(Ed25519PublicKey::from_bytes)
                    .collect()
            })
    }

    /// The devices that hold the workspace's keys at `now` (Unix seconds): every device of its
    /// members.
    fn allowed_devices(
        &self,
        workspace: ModelWorkspaceId,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let at = UNIX_EPOCH.checked_add(tilo_workspace_keys::policy_instant(now))?;
        let reader = self.reader;
        let readers = self
            .owner
            .with_current(workspace, |view| {
                let mut readers = view.key_holders(at)?;
                if reader {
                    readers.insert(view.workspace_host().device_public_key);
                    readers.insert(view.device().device_public_key);
                }
                Ok(readers)
            })
            // A workspace whose governance cannot be read says nothing of who reads it.
            .ok()?;
        Some(
            readers
                .into_iter()
                .map(Ed25519PublicKey::from_bytes)
                .collect(),
        )
    }

    /// The devices of `people` among those that hold the workspace's keys at `now`, and this
    /// device when it follows its groups as one of theirs.
    fn reader_devices(
        &self,
        workspace: ModelWorkspaceId,
        people: &BTreeSet<[u8; 32]>,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let at = UNIX_EPOCH.checked_add(tilo_workspace_keys::policy_instant(now))?;
        let reader = self.reader;
        let readers = self
            .owner
            .with_current(workspace, |view| {
                let mut readers = view.reader_key_holders(people, at)?;
                let device = view.device();
                if reader && people.contains(&device.user_public_key) {
                    readers.insert(device.device_public_key);
                }
                Ok(readers)
            })
            .ok()?;
        Some(
            readers
                .into_iter()
                .map(Ed25519PublicKey::from_bytes)
                .collect(),
        )
    }

    fn founds(&self, workspace: ModelWorkspaceId) -> bool {
        self.owner
            .with_current(workspace, |view| Ok(view.founded_here()))
            .unwrap_or(false)
    }

    /// Announced whenever a workspace's governance moves, so a group re-checks on the change itself
    /// rather than on a clock; without the announcements the service keeps its one-second re-check.
    fn policy_epoch(&self) -> Option<u64> {
        (policy_change::timers_reliable() && policy_change::identity_files_watched())
            .then(policy_change::epoch)
    }

    /// The first second after `now` at which the workspace's groups can answer differently with
    /// nothing written ([`next_policy_change`]).
    fn next_policy_change(&self, workspace: ModelWorkspaceId, now: u64) -> Option<u64> {
        next_policy_change(&self.owner, workspace, now)
    }
}

/// The first second after `now` at which who may read `workspace`'s records can change with
/// nothing written: a capability or a device certificate expiring.
pub fn next_policy_change(
    owner: &NativeWorkspaceOwner,
    workspace: ModelWorkspaceId,
    now: u64,
) -> Option<u64> {
    let second = tilo_workspace_keys::schedule::second_at_or_after;
    let (certificates, capabilities) = owner
        .with_current(workspace, |view| {
            let document = view
                .peer()
                .admitted_workspace()
                .ok_or("no admitted workspace")?;
            let governance = tilo_peer_state::workspace::governance_of(Some(document));
            let certificates = governance
                .identities
                .values()
                .filter_map(|bytes| tilo_peer_state::workspace::identity_row(bytes).ok())
                .filter_map(|row| row.device_cert.expires_at_ns)
                .map(second);
            let capabilities = tilo_peer_state::members::replay_declared(document)
                .grants
                .into_iter()
                .filter_map(|grant| match grant.lifetime {
                    tilo_action_policy::Lifetime::Until(ms) => Some(ms.div_ceil(1_000)),
                    _ => None,
                });
            Ok((
                certificates.collect::<Vec<_>>(),
                capabilities.collect::<Vec<_>>(),
            ))
        })
        .ok()?;
    certificates
        .into_iter()
        .chain(capabilities)
        .filter(|at| *at > now)
        .min()
}

/// The devices a workspace admits, by the publisher each of their sessions is known by, and when
/// the certificate the workspace holds of them expires (Unix nanoseconds).
pub type Roster = Vec<RosterDevice>;

/// One device `workspace` admits, with the certificate the workspace certified it under: what its
/// role in the workspace is read from.
#[derive(Clone, Debug)]
pub struct Device {
    pub device: [u8; 32],
    pub cert: tilo_user_identity::DeviceCert,
}

/// The devices `workspace` admits now, with the certificate it holds of each. A device the
/// workspace removed, or whose certificate ran out, is not one, and what it sent or would receive
/// is refused from then on.
pub fn devices_of(engine: &Engine, workspace: ModelWorkspaceId) -> Result<Vec<Device>, String> {
    let now = tilo_clock::wall();
    engine.owner().with_current(workspace, |view| {
        let admitted: BTreeSet<[u8; 32]> = view
            .peer()
            .governance()
            .governed_access(now)?
            .member_devices
            .into_iter()
            .collect();
        let document = view
            .peer()
            .admitted_workspace()
            .ok_or("no admitted workspace")?;
        let mut devices = BTreeMap::new();
        for bytes in tilo_peer_state::workspace::governance_of(Some(document))
            .identities
            .values()
        {
            let Ok(row) = tilo_peer_state::workspace::identity_row(bytes) else {
                continue;
            };
            let device = row.device_cert.device_public_key;
            if admitted.contains(&device) {
                devices.insert(
                    device,
                    Device {
                        device,
                        cert: row.device_cert,
                    },
                );
            }
        }
        Ok(devices.into_values().collect())
    })
}

/// The roster of `workspace` , as [`devices_of`] reads it: the devices whose window streams carry
/// between.
pub fn roster_of(engine: &Engine, workspace: ModelWorkspaceId) -> Result<Roster, String> {
    devices_of(engine, workspace)?
        .into_iter()
        .map(|device| {
            iroh_base::EndpointId::from_bytes(&device.device)
                .map(|publisher| (publisher, device.cert.expires_at_ns))
                .map_err(|error| error.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn governance_reaches_the_groups_through_the_key_link;

        phase only_governance_reaches_the_group_service;
        // What the peer network heard is the groups' governance and nothing else.
        {
            let link = KeyLink::new();
            let hello = NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(device_cert()),
                revocations: Vec::new(),
            };
            link.heard(&hello);
            assert!(
                link.take_inbound().is_empty(),
                "an identity hello is the peer network's own, not the groups'"
            );
            let workspace = ModelWorkspaceId::new();
            link.heard(&NeighbourMsg::MlsGovernance {
                workspace: workspace.to_string(),
                frame: Box::new(collab_types::MlsGovernanceFrame {
                    version: collab_types::MLS_GOVERNANCE_VERSION,
                    workspace: collab_types::WorkspaceId::new(),
                    previous_frame_digest: [0; 32],
                    epoch: 1,
                    action: collab_types::MlsGovernanceAction::Genesis,
                    signed_by: Ed25519PublicKey::from_bytes([2; 32]),
                    signature: collab_types::Signature([0; 64]),
                }),
                delivery: 1,
            });
            assert_eq!(
                link.take_inbound().len(),
                1,
                "a governance frame waits for the next settle"
            );
            assert!(link.take_inbound().is_empty(), "and is taken once");
        }
    }

    tilo_test_env::scenario! {
        fn a_write_this_device_cannot_open_asks_for_the_group_it_is_sealed_under;

        phase a_document_sealed_under_its_own_group_asks_for_that_group;
        // A document sealed under a group of its own (the workspace's vault) is opened with that
        // group's keys, not the workspace's: a device that lacks them asks the person's other
        // devices for that group's frames, beside the workspace's own, and once per epoch.
        {
            let link = KeyLink::new();
            let (outgoing, mut said) = tokio::sync::mpsc::unbounded_channel();
            link.attach(Arc::new(outgoing));
            let workspace = ModelWorkspaceId::new();
            let vault = ModelWorkspaceId::new();
            let mut write = collab_types::SignedOp::new_unsigned(
                b"sealed".to_vec(),
                collab_types::ArtifactId::new(),
                Ed25519PublicKey::from_bytes([1; 32]),
                collab_types::AttestationId::new(),
                collab_types::OpKind::Change,
                std::time::SystemTime::UNIX_EPOCH,
                1,
            );
            write.encryption = Some(collab_types::EncryptionMetadata::content_group(
                workspace,
                vault,
                3,
                collab_types::ContentRetention::Delta,
            ));
            let wanted = |said: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>| {
                let mut groups = Vec::new();
                while let Ok(command) = said.try_recv() {
                    if let Cmd::TellSiblings(message) = command
                        && let NeighbourMsg::MlsGovernanceWanted { workspace, .. } = *message
                    {
                        groups.push(workspace);
                    }
                }
                groups.sort();
                groups
            };
            link.want_write_frames(workspace, &write);
            let mut both = vec![vault.to_string(), workspace.to_string()];
            both.sort();
            assert_eq!(wanted(&mut said), both);
            link.want_write_frames(workspace, &write);
            assert!(
                wanted(&mut said).is_empty(),
                "what was asked for at an epoch is not asked again"
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_roster_is_told_to_the_network_when_it_moved;

        phase a_roster_moves_only_when_its_devices_do;
        // A roster is told when its workspace moved, and one that moved without its devices changing
        // is told nothing.
        {
            let workspace = topic_of(ModelWorkspaceId::new());
            let device = iroh::SecretKey::from_bytes(&[7; 32]).public();
            let other = iroh::SecretKey::from_bytes(&[9; 32]).public();
            let mut rosters = Rosters::default();
            let one: Roster = vec![(device, None)];
            let both: Roster = vec![(device, None), (other, None)];
            assert!(
                rosters.moved(workspace, &one),
                "a workspace never told is told"
            );
            rosters.told.insert(workspace, both.clone());
            assert!(
                !rosters.moved(workspace, &both),
                "the roster the peer network holds is not told again"
            );
            assert!(
                rosters.moved(workspace, &one),
                "a device the workspace removed leaves, and the peer network is told"
            );
        }

        phase a_roster_the_network_did_not_take_is_told_again;
        // A roster the peer network did not take is not remembered as told: the workspace is read again and
        // the roster told when the peer network takes it.
        {
            let link = KeyLink::new();
            let workspace = ModelWorkspaceId::new();
            let device = iroh::SecretKey::from_bytes(&[7; 32]).public();
            let mut rosters = Rosters::default();
            rosters.tell(&link, workspace, vec![(device, None)]);
            assert_eq!(
                link.take_changed(),
                vec![workspace],
                "the peer network took nothing, so the workspace is read again"
            );

            let (outgoing, mut sent) = tokio::sync::mpsc::unbounded_channel();
            link.attach(std::sync::Arc::new(outgoing));
            rosters.tell(&link, workspace, vec![(device, None)]);
            assert!(
                link.take_changed().is_empty(),
                "once the peer network takes the roster, the workspace is not read again"
            );
            assert!(
                matches!(sent.try_recv(), Ok(Cmd::Roster { .. })),
                "and the peer network was told it"
            );
        }
    }

    fn device_cert() -> tilo_user_identity::DeviceCert {
        tilo_user_identity::DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [2; 32],
            device_label: "This laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 0,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        }
    }
}
