//! This device's end-to-end encrypted channels: the keys of every record the
//! rooms it holds keep sealed, the room's governance that decides who reads
//! each one, and the devices each room admits, which its window streams carry
//! between.
//!
//! A record a room keeps sealed is opened on no one's machine but a member's:
//! the room's devices agree, group by group, that exactly the devices the
//! room's access policy admits take part ([`ChannelDirectory`]), and each
//! sealed write travels with the key material of the group it was written
//! under. The keys live here, never in the document: that is what lets a
//! person invited today read what the room said a year ago, while the room's
//! membership moves on without the history being re-encrypted.
//!
//! This device is one participant of the channels of every room it holds. It
//! asks the room's governor for a channel's keys the first time it reads or
//! writes the channel's record ([`crate::engine::Engine::record`]), takes in
//! the frames the room's transport carries here ([`ChannelLink`]), and settles
//! the groups on their own thread ([`Pump`]) as the room's policy and the
//! people in it change. Nothing here decides who may read what: the room's
//! governance does, and this device only follows it.
//!
//! The same thread tells the collaboration plane which devices each room
//! admits ([`Rosters`]): a room's window streams carry between those devices
//! and no others, and the plane is told again whenever the room's state moves.

mod vault;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use collab_types::{ChannelId, ChannelRoute, Ed25519PublicKey};
use neighbour_wire::NeighbourMsg;
use tilo_action_broker::channel_encryption::{
    ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
    GovernanceTransport, InboundChannelGovernance,
};
use tilo_action_broker::collab_core::BrokerChannels;
use tilo_desktop_model::RoomId;
use tilo_identity_host::policy_change;
use tilo_lock::MutexRecover as _;
use tilo_network_actor::{RosterDevice, WorkspaceId, room_topic_id};

use crate::engine::Engine;
use crate::sync::Outgoing;

pub(crate) use vault::MlsVault;

/// The running channel service: this device's keys, and the thread that keeps
/// their groups in step with the rooms' governance. Stops when dropped.
pub(crate) struct Channels {
    _pump: Pump,
}

impl Channels {
    /// Open this device's channel service over the rooms `engine` holds: its
    /// sealed channel state under the platform secret store, and its groups
    /// settling through `link`, the room transport of this session's
    /// collaboration plane.
    ///
    /// Without a secret store the service still runs, for the rooms this
    /// device's own room host takes part in: their keys come from that host,
    /// and this device's own sealed state is the only thing it cannot keep.
    ///
    /// # Errors
    /// The thread settling the channels could not start.
    pub(crate) fn open(
        engine: &Arc<Engine>,
        link: Arc<ChannelLink>,
        broker: Arc<Mutex<BrokerChannels>>,
    ) -> std::io::Result<Self> {
        let directory = Box::new(Directory {
            engine: Arc::clone(engine),
        });
        let service = match MlsVault::open_for_session() {
            Ok(vault) => ChannelEncryption::open(
                ChannelEncryptionConfig {
                    root: vault.channel_state_root(),
                    vault: vault.channel_state_key(),
                    device_key: engine.identity().device_signing_key.clone(),
                },
                directory,
                Arc::clone(&broker),
            )
            .inspect_err(|error| {
                tracing::warn!(%error, "channel encryption state could not be opened");
            })
            .ok(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "no system secret store answered: only rooms this device hosts have channel keys"
                );
                Some(ChannelEncryption::without_store(directory, broker))
            }
        };
        let keys = engine.keys();
        let access = service.map(|service| {
            let park = Arc::clone(&link.park);
            let access = ChannelKeyAccess::with_wake(service, Arc::new(move || park.poke()));
            keys.set(Some(access.clone()));
            access
        });
        let pump = spawn_pump(access, Arc::clone(engine), Arc::clone(&link), link.park())?;
        // Every room this device already holds reads its roster: the plane
        // carries a room's window streams between the devices it admits from
        // the moment it runs, not only from the room's next change.
        for room in engine.rooms() {
            link.room_changed(room);
        }
        engine.attach_link(link);
        Ok(Self { _pump: pump })
    }
}

/// The room transport the channels' governance travels on: what this
/// session's collaboration plane hears and says, reached from the thread that
/// settles the groups.
///
/// It is also how the plane learns that a room's state moved
/// ([`Self::room_changed`]) and which devices the room admits
/// ([`Self::tell_roster`]), so a change the host published is acted on in the
/// pass after it was taken in.
pub struct ChannelLink {
    /// Governance the plane heard, waiting for the next settle.
    inbound: Mutex<Vec<InboundChannelGovernance>>,
    /// The rooms whose state moved since it was last read: their channels and
    /// their rosters are read again.
    changed: Mutex<BTreeSet<RoomId>>,
    /// The records this device asked a room's hosts for, by room topic and
    /// channel: a read asks once, and the plane asks a host session that
    /// starts later itself.
    asked: Mutex<BTreeSet<(WorkspaceId, [u8; 32])>>,
    /// What this session says on the collaboration plane, once it is running.
    outgoing: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Outgoing>>>,
    park: Arc<Park>,
}

impl ChannelLink {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inbound: Mutex::new(Vec::new()),
            changed: Mutex::new(BTreeSet::new()),
            asked: Mutex::new(BTreeSet::new()),
            outgoing: Mutex::new(None),
            park: Arc::new(Park::default()),
        })
    }

    /// Say `message` on the collaboration plane from now on; `None` once the
    /// plane is gone.
    pub(crate) fn attach(&self, outgoing: tokio::sync::mpsc::UnboundedSender<Outgoing>) {
        *self.outgoing.lock_recover() = Some(outgoing);
        // What was told before the plane ran was told to no one: every room
        // this device holds is read and told again.
        self.park.poke();
    }

    /// Wake whatever unsettles the groups: a frame arrived, or a room moved.
    pub(crate) fn park(&self) -> Arc<Park> {
        Arc::clone(&self.park)
    }

    /// Take in one message the plane heard. Anything but the channels'
    /// governance is left to the plane.
    pub(crate) fn heard(&self, message: &NeighbourMsg) {
        let Some(governance) = InboundChannelGovernance::from_message(message.clone()) else {
            return;
        };
        self.inbound.lock_recover().push(governance);
        self.park.poke();
    }

    /// Take what a room's governor never admits into its replica: the
    /// channels' governance, which the room's devices settle their groups
    /// with between each other. Says whether `message` was one of them, and
    /// so never reaches the room's state.
    pub(crate) fn dispatch(&self, message: &NeighbourMsg) -> bool {
        if !matches!(
            message,
            NeighbourMsg::MlsGovernance { .. }
                | NeighbourMsg::MlsAccessRequest { .. }
                | NeighbourMsg::MlsRoutedAccessRequest { .. }
        ) {
            return false;
        }
        self.heard(message);
        true
    }

    /// Room `room`'s state moved here: the devices it admits and the channels
    /// its records are may differ from what was last read.
    pub(crate) fn room_changed(&self, room: RoomId) {
        self.changed.lock_recover().insert(room);
        self.park.poke();
    }

    /// Read `room` again on a later pass, without waking the pump to it: a
    /// roster that was not told stays untold until it is.
    fn changed_again(&self, room: RoomId) {
        self.changed.lock_recover().insert(room);
    }

    /// The rooms that moved since the last call, in a stable order.
    pub(crate) fn take_changed(&self) -> Vec<RoomId> {
        let mut changed = self.changed.lock_recover();
        let rooms = changed.iter().copied().collect();
        changed.clear();
        rooms
    }

    /// Ask `room`'s hosts for the record on `channel` the first time a local
    /// reader reads it (an on-demand record, or a part of a chain behind its
    /// head): its history then arrives as the room's other documents do, and
    /// later reads ask nobody.
    pub(crate) fn ask_record(&self, room: RoomId, channel: [u8; 32]) {
        let ws = room_topic_id(room);
        let outgoing = self.outgoing.lock_recover().clone();
        let Some(outgoing) = outgoing else {
            return;
        };
        let mut asked = self.asked.lock_recover();
        if asked.contains(&(ws, channel)) {
            return;
        }
        if outgoing.send(Outgoing::OpenRecord { ws, channel }).is_err() {
            // The plane is gone: the next read asks again.
            return;
        }
        asked.insert((ws, channel));
    }

    /// Ask the host of `room` to replay what it retains: a message the room
    /// held back waited on something the host sends of its own accord too, and
    /// the replay decides it again.
    pub(crate) fn ask_replay(&self, room: RoomId) {
        let outgoing = self.outgoing.lock_recover().clone();
        if let Some(outgoing) = outgoing {
            let _ = outgoing.send(Outgoing::Replay(room_topic_id(room)));
        }
    }

    /// Tell the plane which devices room `ws` admits; `false` when the plane
    /// is gone, and the roster is told again when a later pass finds it
    /// changed.
    fn tell_roster(&self, ws: WorkspaceId, devices: Roster) -> bool {
        let outgoing = self.outgoing.lock_recover().clone();
        outgoing.is_some_and(|outgoing| outgoing.send(Outgoing::Roster { ws, devices }).is_ok())
    }
}

impl GovernanceTransport for ChannelLink {
    fn publish(&self, message: NeighbourMsg) -> bool {
        let room = match &message {
            NeighbourMsg::MlsGovernance { room, .. }
            | NeighbourMsg::MlsAccessRequest { room, .. }
            | NeighbourMsg::MlsRoutedAccessRequest { room, .. } => RoomId::parse(room),
            _ => None,
        };
        let Some(room) = room else {
            return false;
        };
        let outgoing = self.outgoing.lock_recover().clone();
        outgoing.is_some_and(|outgoing| {
            outgoing
                .send(Outgoing::Publish {
                    ws: room_topic_id(room),
                    msg: Box::new(message),
                })
                .is_ok()
        })
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        std::mem::take(&mut *self.inbound.lock_recover())
    }
}

/// The rooms whose rosters this session has told the plane, by workspace
/// topic: a roster is told when the room it is of changed, and only then.
#[derive(Default)]
struct Rosters {
    told: HashMap<WorkspaceId, Roster>,
}

impl Rosters {
    /// Whether the plane is not holding `roster` as room `ws`'s yet.
    fn moved(&self, ws: WorkspaceId, roster: &Roster) -> bool {
        self.told.get(&ws) != Some(roster)
    }

    /// Tell the plane the roster of each room that moved: the devices its
    /// governance admits now, which its window streams carry between. A
    /// device the room removed, or whose certificate ran out, is not on it,
    /// and what it sends or would be sent is refused from then on.
    fn sweep(&mut self, link: &ChannelLink, engine: &Engine) {
        for room in link.take_changed() {
            if !engine.holds(room) {
                // The room is gone here, and what the plane held of it went
                // with the leave.
                self.told.remove(&room_topic_id(room));
                continue;
            }
            if let Ok(roster) = roster_of(engine, room) {
                self.tell(link, room, roster);
            } else {
                // A room whose governance cannot be read yet tells no
                // roster: the one it had stays until a pass reads it.
                tracing::debug!(%room, "no roster to tell");
                link.changed_again(room);
            }
        }
    }

    /// Tell the plane `room`'s `roster`, and remember it as told only when
    /// the plane took it: one it did not take is read again on a later pass.
    fn tell(&mut self, link: &ChannelLink, room: RoomId, roster: Roster) {
        let ws = room_topic_id(room);
        if !self.moved(ws, &roster) {
            return;
        }
        if link.tell_roster(ws, roster.clone()) {
            self.told.insert(ws, roster);
        } else {
            link.changed_again(room);
        }
    }
}

/// The room's access policy as this device's own rooms hold it: who governs
/// each room's channels, and which devices the room lets read one.
struct Directory {
    engine: Arc<Engine>,
}

impl ChannelDirectory for Directory {
    fn governor_of(&self, room: RoomId) -> Option<Ed25519PublicKey> {
        self.engine
            .owner()
            .with_current(room, |view| Ok(view.room_host().device_public_key))
            .ok()
            .map(Ed25519PublicKey::from_bytes)
    }

    /// The member devices the room's governance lets read `channel` at `now`
    /// (Unix seconds): the same projection the room's doors apply to the
    /// record, so a person's device is in a channel exactly when it may read
    /// the record that channel carries.
    fn allowed_devices(
        &self,
        room: RoomId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let at = UNIX_EPOCH.checked_add(Duration::from_secs(now))?;
        let readers = self
            .engine
            .owner()
            .with_current(room, |view| {
                view.peer().governance().channel_readers(at, channel, route)
            })
            // A room whose governance cannot be read says nothing of who
            // reads it.
            .ok()
            .flatten()?;
        Some(
            readers
                .into_iter()
                .map(Ed25519PublicKey::from_bytes)
                .collect(),
        )
    }

    /// Announced whenever a room's governance moves, so a group re-checks on
    /// the change itself rather than on a clock; without the announcements
    /// the service keeps its one-second re-check.
    fn policy_epoch(&self) -> Option<u64> {
        (policy_change::timers_reliable() && policy_change::identity_files_watched())
            .then(policy_change::epoch)
    }

    /// The first second after `now` at which the room's channels can answer
    /// differently with nothing written: a capability or a device
    /// certificate expiring.
    fn next_policy_change(&self, room: RoomId, now: u64) -> Option<u64> {
        let second = tilo_action_broker::policy_schedule::second_at_or_after;
        let (certificates, capabilities) = self
            .engine
            .owner()
            .with_current(room, |view| {
                let document = view.peer().admitted_room().ok_or("no admitted room")?;
                let governance = tilo_peer_state::room::governance_of(Some(document));
                let certificates = governance
                    .identities
                    .values()
                    .filter_map(|bytes| tilo_peer_state::room::identity_row(bytes).ok())
                    .filter_map(|row| row.device_cert.expires_at_ns)
                    .map(second);
                let capabilities = governance
                    .grants
                    .values()
                    .filter_map(|bytes| {
                        serde_json::from_slice::<tilo_action_policy::Grant>(bytes).ok()
                    })
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
}

/// Sleeps the governance pump until it is poked or its elapsed time is up.
///
/// A poke that arrives before the pump waits is kept, so a frame queued just
/// before it parks is not left behind.
#[derive(Default)]
pub(crate) struct Park {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl Park {
    pub(crate) fn poke(&self) {
        let mut pending = self.pending.lock_recover();
        *pending = true;
        drop(pending);
        self.ready.notify_one();
    }

    /// Sleep until poked, for at most `timeout` when there is one, else until
    /// poked.
    fn wait(&self, timeout: Option<Duration>) {
        let pending = self.pending.lock_recover();
        let pending = match timeout {
            Some(timeout) => {
                self.ready
                    .wait_timeout_while(pending, timeout, |pending| !*pending)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => self
                .ready
                .wait_while(pending, |pending| !*pending)
                .unwrap_or_else(PoisonError::into_inner),
        };
        drop(pending);
    }
}

/// The thread settling the channels' groups; stops when dropped.
struct Pump {
    park: Arc<Park>,
    stop: Arc<AtomicBool>,
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.park.poke();
    }
}

/// Run `access`'s governance over `transport` on a thread of its own, and tell
/// the plane the roster of every room that moved: governance runs at once
/// whenever it is poked, then when the protocol next has work that no frame
/// brings, and parks in between. A group re-checks its channels' access when
/// a policy change is announced or an expiry it registered comes due, not on
/// a clock: the watch is armed only while groups wait for that, and a device
/// taking part in no channel sleeps through every announcement.
fn spawn_pump(
    access: Option<ChannelKeyAccess>,
    engine: Arc<Engine>,
    transport: Arc<ChannelLink>,
    park: Arc<Park>,
) -> std::io::Result<Pump> {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let woken = Arc::clone(&park);
    let wake = Arc::clone(&park);
    std::thread::Builder::new()
        .name("tilo-channels".into())
        .spawn(move || {
            let policy_watch = policy_change::Watch::new(move || wake.poke());
            let mut rosters = Rosters::default();
            while !stopped.load(Ordering::Relaxed) {
                let mut due = access
                    .as_ref()
                    .and_then(|access| access.pump_until_due(&*transport));
                rosters.sweep(&transport, &engine);
                if let Some(access) = &access
                    && !access.arm_policy_watch(&policy_watch)
                {
                    // A policy change arrived that the arming will not wake
                    // for: the groups are checked again now.
                    due = Some(Duration::ZERO);
                }
                woken.wait(due);
            }
        })?;
    Ok(Pump { park, stop })
}

/// The devices a room admits, by the publisher each of their sessions is
/// known by, and when the certificate the room holds of them expires (Unix
/// nanoseconds).
pub(crate) type Roster = Vec<RosterDevice>;

/// The roster of `room`: the devices its governance admits now, which are the
/// ones its window streams carry between. A device the room removed, or whose
/// certificate ran out, is not on it, and what it sent or would receive is
/// refused from then on.
pub(crate) fn roster_of(engine: &Engine, room: RoomId) -> Result<Roster, String> {
    let now = SystemTime::now();
    engine.owner().with_current(room, |view| {
        let admitted: BTreeSet<[u8; 32]> = view
            .peer()
            .governance()
            .governed_access(now)?
            .member_devices
            .into_iter()
            .collect();
        let document = view.peer().admitted_room().ok_or("no admitted room")?;
        let mut roster = BTreeMap::new();
        for bytes in tilo_peer_state::room::governance_of(Some(document))
            .identities
            .values()
        {
            let Ok(row) = tilo_peer_state::room::identity_row(bytes) else {
                continue;
            };
            let device = row.device_cert.device_public_key;
            if admitted.contains(&device) {
                roster.insert(device, row.device_cert.expires_at_ns);
            }
        }
        roster
            .into_iter()
            .map(|(device, expires_at_ns)| {
                iroh::EndpointId::from_bytes(&device)
                    .map(|publisher| (publisher, expires_at_ns))
                    .map_err(|error| error.to_string())
            })
            .collect()
    })
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
        let room = RoomId::new();
        link.heard(&NeighbourMsg::MlsGovernance {
            room: room.to_string(),
            frame: Box::new(collab_types::MlsGovernanceFrame {
                version: collab_types::MLS_GOVERNANCE_VERSION,
                channel: ChannelId::new(),
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

    /// A roster is told when its room moved, and a room that moved without
    /// its devices changing is told nothing.
    #[test]
    fn a_roster_moves_only_when_its_devices_do() {
        let room = room_topic_id(RoomId::new());
        let device = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[9; 32]).public();
        let mut rosters = Rosters::default();
        let one: Roster = vec![(device, None)];
        let both: Roster = vec![(device, None), (other, None)];
        assert!(rosters.moved(room, &one), "a room never told is told");
        rosters.told.insert(room, both.clone());
        assert!(
            !rosters.moved(room, &both),
            "the roster the plane holds is not told again"
        );
        assert!(
            rosters.moved(room, &one),
            "a device the room removed leaves, and the plane is told"
        );
    }

    /// A roster the plane did not take is not remembered as told: the room is
    /// read again and the roster told when the plane takes it.
    #[test]
    fn a_roster_the_plane_did_not_take_is_told_again() {
        let link = ChannelLink::new();
        let room = RoomId::new();
        let device = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let mut rosters = Rosters::default();
        rosters.tell(&link, room, vec![(device, None)]);
        assert_eq!(
            link.take_changed(),
            vec![room],
            "the plane took nothing, so the room is read again"
        );

        let (outgoing, mut sent) = tokio::sync::mpsc::unbounded_channel();
        link.attach(outgoing);
        rosters.tell(&link, room, vec![(device, None)]);
        assert!(
            link.take_changed().is_empty(),
            "once the plane takes the roster, the room is not read again"
        );
        assert!(
            matches!(sent.try_recv(), Ok(Outgoing::Roster { .. })),
            "and the plane was told it"
        );
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
        }
    }
}
