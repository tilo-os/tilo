//! This device's end-to-end encrypted channels: the keys of every record the
//! workspaces it holds keep sealed, the governance that decides who reads each one,
//! and the devices each workspace admits, which its window streams carry between.
//! A sealed record is opened on no one's machine but a member's: the workspace's
//! devices agree, group by group, that exactly the devices its access policy admits
//! take part ([`ChannelDirectory`]), and each sealed write travels with the key
//! material of its group. The keys live here, never in the document, which is what
//! lets a person invited today read what the workspace said a year ago while its
//! membership moves on.
//! Nothing here decides who may read what: the workspace's governance does, and
//! this device only follows it.

// The page's session runs no channel service of its own: only a desktop's build settles a
// workspace's groups here.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        unused_imports,
        reason = "a page runs its workspaces' channels with its own host"
    )
)]
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_clock::time::UNIX_EPOCH;

use crate::plane::{Cmd, RosterDevice, Topic, topic_of};

use collab_types::Ed25519PublicKey;
use neighbour_wire::NeighbourMsg;
#[cfg(target_os = "linux")]
use tilo_broker_core::collab_core::{BrokerChannels, BrokerGates};
use tilo_channels::{
    ChannelDirectory, ChannelKeyAccess, GovernancePark, GovernanceTransport,
    InboundChannelGovernance,
};
#[cfg(target_os = "linux")]
use tilo_channels::{ChannelEncryption, ChannelEncryptionConfig, ChannelGates, GovernancePump};
use tilo_desktop_model::WorkspaceId as ModelWorkspaceId;
use tilo_identity_host::policy_change;
use tilo_lock::MutexRecover as _;

use crate::engine::Engine;
use tilo_peer_state::owner::NativeWorkspaceOwner;

mod vault;
pub use vault::MlsVault;

/// The running channel service: this device's keys, and the thread that keeps their groups in step
/// with the workspaces' governance. Stops when dropped.
pub struct Channels {
    /// This device's keys and the groups they are in; `None` while no secret store
    /// keeps its channel state.
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(dead_code, reason = "a host with threads runs its own pump")
    )]
    access: Option<ChannelKeyAccess>,
    /// The thread that settles the groups, on a host with threads.
    #[cfg(target_os = "linux")]
    _pump: GovernancePump,
}

impl Channels {
    /// One pass of the channel protocol over `transport`: what the thread does on a
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
impl Channels {
    /// Open this device's channel service over the workspaces `engine` holds:
    /// its sealed channel state under the platform secret store, and its
    /// groups settling through `link`, the workspace transport of this
    /// session's collaboration plane. `settled` runs after every pass that
    /// settles them: keys a record waited for may be here since.
    ///
    /// Without a secret store the service still runs, for the workspaces this
    /// device's own room host takes part in: their keys come from that host,
    /// and this device's own sealed state is the only thing it cannot keep.
    ///
    /// # Errors
    /// The thread settling the channels could not start.
    pub fn open(
        engine: &Arc<Engine>,
        link: Arc<ChannelLink>,
        broker: Arc<Mutex<BrokerChannels>>,
        mut settled: impl FnMut() + Send + 'static,
    ) -> std::io::Result<Self> {
        let directory = Box::new(OwnerDirectory::participant(engine.owner().clone()));
        let gates: Arc<dyn ChannelGates> = Arc::new(BrokerGates(broker));
        let service = match MlsVault::open_for_session(
            &engine.identity().device_cert.device_public_key,
        ) {
            Ok(vault) => ChannelEncryption::open(
                ChannelEncryptionConfig {
                    state: vault.channel_state(),
                    vault: vault.channel_state_key(),
                    signer: Arc::clone(&engine.identity().signer),
                    role: tilo_channels::Role::Participant,
                },
                directory,
                Arc::clone(&gates),
            )
            .inspect_err(|error| {
                tracing::warn!(%error, "channel encryption state could not be opened");
            })
            .ok(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "no system secret store answered: only workspaces this device hosts have channel keys"
                );
                Some(ChannelEncryption::without_store(directory, gates))
            }
        };
        let keys = engine.keys();
        let park = link.park();
        let poke = Arc::clone(&park);
        let access = service.map(|service| {
            let access = ChannelKeyAccess::with_wake(service, Arc::new(move || poke.poke()));
            keys.set(Some(access.clone()));
            access
        });
        let settled_access = access.clone();
        let mut rosters = Rosters::default();
        let engine_settled = Arc::clone(engine);
        let transport = Arc::clone(&link);
        let pump = GovernancePump::spawn(
            "tilo-channels",
            park,
            access,
            Arc::clone(&link) as Arc<dyn GovernanceTransport + Send + Sync>,
            move || {
                rosters.sweep(&transport, &engine_settled);
                settled();
            },
        )?;
        // Every workspace this device already holds reads its roster: the plane carries a
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
/// retained channel, and what each answered: `None` while the answer is owed, the frames the host
/// sent once it came.
type AskedRecords = std::collections::HashMap<(Topic, [u8; 32]), Option<u32>>;

/// The workspace transport the channels' governance travels on: what this session's collaboration
/// plane hears and says, reached from the thread that settles the groups.
/// It is also how the plane learns that a workspace's state moved ([`Self::workspace_changed`]) and
/// which devices the workspace admits ([`Self::tell_roster`]), so a change the host published is
/// acted on in the pass after it was taken in.
pub struct ChannelLink {
    /// Governance the plane heard, waiting for the next settle.
    inbound: Mutex<Vec<InboundChannelGovernance>>,
    /// The workspaces whose state moved since it was last read: their channels and their rosters
    /// are read again.
    changed: Mutex<BTreeSet<ModelWorkspaceId>>,
    /// The workspaces, by topic, whose host finished replaying what it retains to this device
    /// ([`Self::host_ready`]): every record comes with that history, so a record not here then is
    /// not anywhere.
    replayed: Mutex<BTreeSet<Topic>>,
    /// The workspaces, by topic, whose host sends records only once a device opens them
    /// ([`Self::host_ready`]): what a record not here is decided by, together with the host's answer
    /// about it ([`Self::record_opened`]).
    when_opened: Mutex<BTreeSet<Topic>>,
    /// The records this device asked the workspaces' hosts for, by topic and retained channel, and
    /// what each answered: `None` while the answer is owed, the frames it sent once it came
    /// ([`Self::record_opened`]).
    records: Mutex<AskedRecords>,
    /// Ticks whenever a host finished its replay.
    replays: tokio::sync::watch::Sender<u64>,
    /// Ticks whenever a host answered for a record this device opened.
    answers: tokio::sync::watch::Sender<u64>,
    /// What this session says on the collaboration plane, once it is running.
    outgoing: Mutex<Option<std::sync::Arc<dyn crate::Outbox>>>,
    /// The devices that reach this one as their workspace's host, when it
    /// serves any ([`Self::serve_members`]).
    members: Mutex<Option<Arc<dyn Members>>>,
    park: Arc<GovernancePark>,
}

/// The devices that take part in a workspace through this one, as their
/// workspace's host: a channel's governance reaches them only through this
/// device, so it sends them what it hears and says, and takes what they say
/// to its own channel service and on to the workspace's own host.
pub trait Members: Send + Sync {
    /// Send `message`, a workspace's channel governance, to the members
    /// linked to this device in that workspace.
    fn carry(&self, message: &NeighbourMsg);
}

impl ChannelLink {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inbound: Mutex::new(Vec::new()),
            changed: Mutex::new(BTreeSet::new()),
            replayed: Mutex::new(BTreeSet::new()),
            when_opened: Mutex::new(BTreeSet::new()),
            records: Mutex::new(AskedRecords::new()),
            replays: tokio::sync::watch::Sender::new(0),
            answers: tokio::sync::watch::Sender::new(0),
            outgoing: Mutex::new(None),
            members: Mutex::new(None),
            park: GovernancePark::new(),
        })
    }

    /// Say `message` on the collaboration plane from now on; `None` once the plane is gone.
    pub fn attach(&self, outgoing: std::sync::Arc<dyn crate::Outbox>) {
        *self.outgoing.lock_recover() = Some(outgoing);
        // What was told before the plane ran was told to no one: every workspace this device holds
        // is read and told again.
        self.park.poke();
    }

    /// Wake whatever unsettles the groups: a frame arrived, or a workspace moved.
    pub fn park(&self) -> Arc<GovernancePark> {
        Arc::clone(&self.park)
    }

    /// Whether the plane this device says its workspaces' rosters on is running.
    fn attached(&self) -> bool {
        self.outgoing.lock_recover().is_some()
    }

    /// Send the channel governance this device hears and says to `members`, the devices that take
    /// part in a workspace through this one, and take what they say: [`Self::heard_from_member`].
    pub fn serve_members(&self, members: Arc<dyn Members>) {
        *self.members.lock_recover() = Some(members);
    }

    /// Take in one message the plane heard. Anything but the channels' governance is left to the
    /// plane. The members linked to this device hear it too: the plane is not their path.
    pub fn heard(&self, message: &NeighbourMsg) {
        if !self.take_in(message) {
            return;
        }
        self.carry_to_members(message);
    }

    /// Send `message`, channel governance, to the members linked to this device.
    fn carry_to_members(&self, message: &NeighbourMsg) {
        let members = self.members.lock_recover().clone();
        if let Some(members) = members {
            members.carry(message);
        }
    }

    /// Take in channel governance a member linked to this device said: the channel service settles
    /// its groups with it, and the plane sends it on to the workspace's own host, so the devices
    /// that take part there hear it. The link it came by already sent it to the other members.
    pub fn heard_from_member(&self, message: &NeighbourMsg) {
        if !self.take_in(message) {
            return;
        }
        if let Some(workspace) = governance_workspace(message) {
            self.say(workspace, message.clone());
        }
    }

    /// Queue `message` for the channel service when it is channel governance, and say whether it
    /// was.
    fn take_in(&self, message: &NeighbourMsg) -> bool {
        let Some(governance) = InboundChannelGovernance::from_message(message.clone()) else {
            return false;
        };
        self.inbound.lock_recover().push(governance);
        self.park.poke();
        true
    }

    /// Say `message`, channel governance of `workspace`, on the plane; `false` when the plane is
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

    /// Take what a workspace's governor never admits into its replica: the channels' governance,
    /// which the workspace's devices settle their groups with between each other. Says whether
    /// `message` was one of them, and so never reaches the workspace's state.
    pub fn dispatch(&self, message: &NeighbourMsg) -> bool {
        if message.channel_workspace().is_none() {
            return false;
        }
        self.heard(message);
        true
    }

    /// Workspace `workspace` 's state moved here: the devices it admits and the channels its
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
    /// after the replay's own writes. `records_when_opened`: it sends this device only the
    /// workspace's governance, its shared desktop and the heads of its chained records, and every
    /// other record once it opens it ([`Self::open_record`]).
    pub fn host_ready(&self, ws: Topic, records_when_opened: bool) {
        if records_when_opened {
            self.when_opened.lock_recover().insert(ws);
        }
        self.replayed.lock_recover().insert(ws);
        self.replays
            .send_modify(|count| *count = count.wrapping_add(1));
        // A group that waited on the history is founded now, if it held no genesis.
        self.park.poke();
        // A host session that just came up owes this device the records it asked for before it was
        // there: a record asked for on a session that has since ended reaches here over this one.
        let asked: Vec<[u8; 32]> = self
            .records
            .lock_recover()
            .keys()
            .filter(|(asked_ws, _)| *asked_ws == ws)
            .map(|(_, channel)| *channel)
            .collect();
        self.ask_for(ws, asked);
    }

    /// Whether `workspace`'s host finished replaying what it retains to this device.
    pub fn has_replayed(&self, workspace: ModelWorkspaceId) -> bool {
        self.replayed.lock_recover().contains(&topic_of(workspace))
    }

    /// Whether `workspace`'s host sends a device only the workspace's governance, its shared
    /// desktop and the heads of its chained records, and every other record once the device opens
    /// it ([`Self::open_record`]): a record that is not here is not new until the host answered it
    /// holds none of it ([`Self::record_opened`]).
    pub fn records_when_opened(&self, workspace: ModelWorkspaceId) -> bool {
        self.when_opened
            .lock_recover()
            .contains(&topic_of(workspace))
    }

    /// Ask `workspace`'s hosts for the record on `channel`
    /// (`neighbour_wire::app_document_channel`): what they hold of it, and their changes to it from
    /// then on. Asked once per record and session; a host session that comes up later asks again
    /// ([`Self::host_ready`]). `false` when the plane is not running, so nothing was asked.
    pub fn open_record(&self, workspace: ModelWorkspaceId, channel: [u8; 32]) -> bool {
        let ws = topic_of(workspace);
        {
            let mut records = self.records.lock_recover();
            if records.contains_key(&(ws, channel)) {
                return true;
            }
            records.insert((ws, channel), None);
        }
        self.ask_for(ws, [channel])
    }

    /// Say [`Cmd::OpenRecord`] for each of `channels` on the plane: whether it was taken, or `true`
    /// when there is no plane to ask on.
    fn ask_for(&self, ws: Topic, channels: impl IntoIterator<Item = [u8; 32]>) -> bool {
        let outgoing = self.outgoing.lock_recover().clone();
        channels.into_iter().all(|channel| {
            outgoing
                .as_ref()
                .is_none_or(|outgoing| outgoing.send(Cmd::OpenRecord { ws, channel }).is_ok())
        })
    }

    /// The host of the workspace on topic `ws` sent everything it retains of the record on
    /// `channel`, `frames` of them — none when the workspace has none of it, and then the record is
    /// new to whoever opens it.
    pub fn record_opened(&self, ws: Topic, channel: [u8; 32], frames: u32) {
        self.records
            .lock_recover()
            .insert((ws, channel), Some(frames));
        self.answers
            .send_modify(|count| *count = count.wrapping_add(1));
        self.park.poke();
    }

    /// What the hosts of `workspace` answered about the record on `channel`: `None` while the
    /// answer is owed, the frames they sent once it came.
    pub fn record_answer(&self, workspace: ModelWorkspaceId, channel: &[u8; 32]) -> Option<u32> {
        self.records
            .lock_recover()
            .get(&(topic_of(workspace), *channel))
            .copied()
            .flatten()
    }

    /// Wait for the next record a host answered for, from a value read before the wait.
    #[must_use]
    pub fn answers(&self) -> tokio::sync::watch::Receiver<u64> {
        self.answers.subscribe()
    }

    /// Wait for the next replay a host finished, from a value read before the wait.
    #[must_use]
    pub fn replays(&self) -> tokio::sync::watch::Receiver<u64> {
        self.replays.subscribe()
    }

    /// Ask the host of `workspace` to replay what it retains: a message the workspace held back
    /// waited on something the host sends of its own accord too, and the replay decides it again.
    pub fn ask_replay(&self, workspace: ModelWorkspaceId) {
        let outgoing = self.outgoing.lock_recover().clone();
        if let Some(outgoing) = outgoing {
            let _ = outgoing.send(Cmd::Replay(topic_of(workspace)));
        }
    }

    /// Send this device's owner's outgoing custody of `workspace` to its peers: what the owner
    /// committed leaves this device on the plane's session with the workspace's host, as every
    /// other change of the workspace does.
    pub fn publish_custody(&self, workspace: ModelWorkspaceId) {
        let outgoing = self.outgoing.lock_recover().clone();
        if let Some(outgoing) = outgoing {
            let _ = outgoing.send(Cmd::PublishCustody(topic_of(workspace)));
        }
    }

    /// Tell the plane which devices workspace `ws` admits; `false` when the plane is gone, and the
    /// roster is told again when a later pass finds it changed.
    fn tell_roster(&self, ws: Topic, devices: Roster) -> bool {
        let outgoing = self.outgoing.lock_recover().clone();
        outgoing.is_some_and(|outgoing| outgoing.send(Cmd::Roster { ws, devices }).is_ok())
    }
}

impl GovernanceTransport for ChannelLink {
    fn publish(&self, message: NeighbourMsg) -> bool {
        let Some(workspace) = governance_workspace(&message) else {
            return false;
        };
        // The members linked to this device hear it from this device alone.
        self.carry_to_members(&message);
        self.say(workspace, message)
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        std::mem::take(&mut *self.inbound.lock_recover())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Members for tilo_peer_state::direct::Owning {
    fn carry(&self, message: &NeighbourMsg) {
        self.carry_governance(message);
    }
}

/// The workspace `message`, a channel's governance, is of; `None` for any other message.
fn governance_workspace(message: &NeighbourMsg) -> Option<ModelWorkspaceId> {
    message
        .channel_workspace()
        .and_then(ModelWorkspaceId::parse)
}

/// The workspaces whose rosters this session has told the plane, by workspace topic: a roster is
/// told when the workspace it is of changed, and only then.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct Rosters {
    told: HashMap<Topic, Roster>,
}

#[cfg(target_os = "linux")]
impl Rosters {
    /// Whether the plane is not holding `roster` as workspace `ws` 's yet.
    fn moved(&self, ws: Topic, roster: &Roster) -> bool {
        self.told.get(&ws) != Some(roster)
    }

    /// Tell the plane the roster of each workspace that moved: the devices its governance admits
    /// now, which its window streams carry between. A device the workspace removed, or whose
    /// certificate ran out, is not on it, and what it sends or would be sent is refused from then
    /// on.
    fn sweep(&mut self, link: &ChannelLink, engine: &Engine) {
        // A session with no plane says its rosters to no one: the workspaces stay marked, and the
        // pass after the plane starts tells them all ([`ChannelLink::attach`]).
        if !link.attached() {
            return;
        }
        for workspace in link.take_changed() {
            if !engine.holds(workspace) {
                // The workspace is gone here, and what the plane held of it went with the leave.
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

    /// Tell the plane the workspace's `roster` , and remember it as told only when the plane took
    /// it: one it did not take is read again on a later pass.
    fn tell(&mut self, link: &ChannelLink, workspace: ModelWorkspaceId, roster: Roster) {
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
/// workspace lets read each channel.
pub struct OwnerDirectory {
    owner: NativeWorkspaceOwner,
    /// A reader also follows what the host it joined a workspace through and it itself sign: the
    /// host vouched for the workspace it was let into.
    reader: bool,
}

impl OwnerDirectory {
    /// The directory of a device that settles its groups.
    #[must_use]
    pub const fn participant(owner: NativeWorkspaceOwner) -> Self {
        Self {
            owner,
            reader: false,
        }
    }

    /// The directory of a [`tilo_channels::Role::Reader`] device.
    #[must_use]
    pub const fn reader(owner: NativeWorkspaceOwner) -> Self {
        Self {
            owner,
            reader: true,
        }
    }
}

impl ChannelDirectory for OwnerDirectory {
    /// The devices that hold the workspace's keys at `now` (Unix seconds): every device of its
    /// members.
    fn allowed_devices(
        &self,
        workspace: ModelWorkspaceId,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let at = UNIX_EPOCH.checked_add(tilo_channels::policy_instant(now))?;
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

    /// A workspace entered while hosted elsewhere is its host's: the host answers whether a channel
    /// has a group.
    fn hosts(&self, workspace: ModelWorkspaceId) -> bool {
        !self
            .owner
            .with_current(workspace, |view| Ok(view.entered() && !view.hosted_here()))
            .unwrap_or(false)
    }

    /// Announced whenever a workspace's governance moves, so a group re-checks on the change itself
    /// rather than on a clock; without the announcements the service keeps its one-second re-check.
    fn policy_epoch(&self) -> Option<u64> {
        (policy_change::timers_reliable() && policy_change::identity_files_watched())
            .then(policy_change::epoch)
    }

    /// The first second after `now` at which the workspace's channels can answer differently with
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
    let second = tilo_channels::schedule::second_at_or_after;
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

    /// What the plane heard is the channels' governance and nothing else.
    #[test]
    fn only_governance_reaches_the_channel_service() {
        let link = ChannelLink::new();
        let hello = NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(device_cert()),
            revocations: Vec::new(),
        };
        link.heard(&hello);
        assert!(
            link.take_inbound().is_empty(),
            "an identity hello is the plane's own, not the channels'"
        );
        let workspace = ModelWorkspaceId::new();
        link.heard(&NeighbourMsg::MlsGovernance {
            workspace: workspace.to_string(),
            frame: Box::new(collab_types::MlsGovernanceFrame {
                version: collab_types::MLS_GOVERNANCE_VERSION,
                channel: collab_types::ChannelId::new(),
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

    /// A roster is told when its workspace moved, and one that moved without its devices changing
    /// is told nothing.
    #[test]
    fn a_roster_moves_only_when_its_devices_do() {
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
            "the roster the plane holds is not told again"
        );
        assert!(
            rosters.moved(workspace, &one),
            "a device the workspace removed leaves, and the plane is told"
        );
    }

    /// A roster the plane did not take is not remembered as told: the workspace is read again and
    /// the roster told when the plane takes it.
    #[test]
    fn a_roster_the_plane_did_not_take_is_told_again() {
        let link = ChannelLink::new();
        let workspace = ModelWorkspaceId::new();
        let device = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let mut rosters = Rosters::default();
        rosters.tell(&link, workspace, vec![(device, None)]);
        assert_eq!(
            link.take_changed(),
            vec![workspace],
            "the plane took nothing, so the workspace is read again"
        );

        let (outgoing, mut sent) = tokio::sync::mpsc::unbounded_channel();
        link.attach(std::sync::Arc::new(outgoing));
        rosters.tell(&link, workspace, vec![(device, None)]);
        assert!(
            link.take_changed().is_empty(),
            "once the plane takes the roster, the workspace is not read again"
        );
        assert!(
            matches!(sent.try_recv(), Ok(Cmd::Roster { .. })),
            "and the plane was told it"
        );
    }

    /// The members linked to a device that hosts their workspace hear a channel's governance
    /// through that device alone: what it says and what the plane brings it reach them, and what
    /// they say reaches its channel service and, on the plane, the workspace's own host.
    #[test]
    fn governance_passes_between_the_plane_and_the_members_linked_here() {
        #[derive(Default)]
        struct Linked(Mutex<Vec<NeighbourMsg>>);
        impl Members for Linked {
            fn carry(&self, message: &NeighbourMsg) {
                self.0.lock_recover().push(message.clone());
            }
        }
        let link = ChannelLink::new();
        let (outgoing, mut said) = tokio::sync::mpsc::unbounded_channel();
        link.attach(std::sync::Arc::new(outgoing));
        let members = Arc::new(Linked::default());
        link.serve_members(Arc::clone(&members) as Arc<dyn Members>);
        let workspace = ModelWorkspaceId::new();
        let frame = |epoch| NeighbourMsg::MlsGovernance {
            workspace: workspace.to_string(),
            frame: Box::new(collab_types::MlsGovernanceFrame {
                version: collab_types::MLS_GOVERNANCE_VERSION,
                channel: collab_types::ChannelId::new(),
                previous_frame_digest: [0; 32],
                epoch,
                action: collab_types::MlsGovernanceAction::Genesis,
                signed_by: Ed25519PublicKey::from_bytes([2; 32]),
                signature: collab_types::Signature([0; 64]),
            }),
            delivery: epoch,
        };
        let published = |said: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>| {
            let mut published = Vec::new();
            while let Ok(command) = said.try_recv() {
                if let Cmd::Publish { msg, .. } = command {
                    published.push(*msg);
                }
            }
            published
        };

        // What this device's channel service says goes out on the plane and to the members.
        let own = frame(1);
        assert!(link.publish(own.clone()));
        assert_eq!(published(&mut said), vec![own.clone()]);
        assert_eq!(*members.0.lock_recover(), vec![own]);

        // What the plane brings reaches the channel service and the members, and is not said
        // again on the plane.
        let from_the_host = frame(2);
        link.heard(&from_the_host);
        assert_eq!(link.take_inbound().len(), 1);
        assert!(published(&mut said).is_empty());
        assert_eq!(members.0.lock_recover().last(), Some(&from_the_host));

        // What a member says reaches the channel service and goes on to the workspace's host; its
        // link already sent it to the other members.
        let from_a_member = frame(3);
        link.heard_from_member(&from_a_member);
        assert_eq!(link.take_inbound().len(), 1);
        assert_eq!(published(&mut said), vec![from_a_member]);
        assert_eq!(members.0.lock_recover().len(), 2);
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
