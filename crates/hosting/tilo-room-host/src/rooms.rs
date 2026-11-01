//! Rooms this relay governs.
//!
//! A shared workspace needs one authority that decides who is in it: it
//! issues invitations, admits the device that redeems one, changes roles and
//! removes members, and publishes each decision as signed room governance
//! every member verifies. Here that authority is the relay hosting the room.
//! Its own device is the room's governor: it keeps the room's [`StatePeer`],
//! publishes governance as its own retained changes of the room's workspace,
//! and hands the governed member set to the relay's admission after every
//! decision, so a member it admits can subscribe at once and a member it
//! removes is cut off.
//!
//! Rooms are administered as their own `tilo.workspace` governance actions,
//! and the host's own operations (creating, listing and entering rooms) as
//! `tilo.host` actions (`actions`), or, from a page, over HTTPS: by devices
//! of the relay's own user, and by other members as far as the room's
//! governance lets them (`authority`, one decision for every door).
//! Any device redeems an invitation over the shared workspace-join
//! protocol, and a page over HTTPS, where someone the room lets invite then
//! admits it.

mod actions;
mod admissions;
#[cfg(feature = "agents")]
mod agent_members;
pub mod app_files;
#[cfg(feature = "agents")]
mod applications;
mod authority;
mod collaboration;
mod content;
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures;
pub(crate) use content::lacks_answer;
pub use content::{ContentKeeper, RoomContent};
mod desktop_writes;
mod device_keys;
#[cfg(test)]
mod device_rejoin_tests;
#[cfg(test)]
mod hard_stop_tests;
pub(crate) use desktop_writes::writes_desktop;
mod hosts;
mod layout_scope;
mod listing;
mod pages;
mod peer_parts;
mod private;
#[cfg(test)]
mod private_network_tests;
mod private_store;
pub(crate) use layout_scope::LAYOUT_REFUSAL;
pub use private::PrivateProtocol;
mod publisher;
mod revocations;
#[cfg(any(feature = "agents", feature = "web"))]
pub(crate) mod sealed_records;
#[cfg(all(any(test, feature = "test-support"), feature = "agents"))]
pub use sealed_records::fixtures as sealed_fixtures;
mod participation;
pub use device_keys::{ChannelKeys, DeviceKeys};
mod shares;
#[cfg(feature = "web")]
mod spectator_keys;
#[cfg(test)]
pub(crate) mod tests;
mod unbuildable_windows;
#[cfg(feature = "agents")]
mod views;
mod voice;
mod workspace_layout;
pub(crate) use voice::carried_call_room;
mod wait_list;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use neighbour_wire::Retention;
use serde::{Deserialize, Serialize};
use tilo_peer_state::{GovernedAccess, StatePeer};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::Mutex as AsyncMutex;
use web_time::SystemTime;

pub(crate) use actions::{GovernedPlane, HostRoomActions};
pub(crate) use admissions::DEFAULT_MAX_PRE_ADMISSIONS;
pub use collaboration::{CollaborationError, GovernanceCompletion};
pub use pages::PreAdmitRequest;
pub use wait_list::{Mailbox, Secrets as WaitListSecrets, WaitListed};

/// An invitation someone who may invite asked the room to mail to an
/// address on its wait list ([`Rooms::mail_wait_list`]).
pub struct WaitListMail {
    /// Completed only after the operator durably accepts the mail.
    pub queued: tokio::sync::oneshot::Sender<bool>,
    pub to: Mailbox,
    /// The invitation's web link.
    pub link: String,
    /// A secret that takes the entry off the list, for the mail's removal
    /// link; the entry keeps only its SHA-256.
    pub removal: [u8; 32],
}
// The administration vocabulary (the actions' names and answers), under the
// daemon's historic paths.
pub use tilo_relay_session::rooms::{
    ATTACH_HOST, ActionInvitation, CREATE_ROOM, CancelInvitationRequest, CreatedRoom, DETACH_HOST,
    ENTER_ROOM, EnteredRoom, HOST_ACTIONS_PROVIDER, HOST_ROOM, HOST_ROOMS_RESOURCE, HostedRoom,
    IssueInvitationRequest, IssuedInvitation, LIST_ROOMS, PendingInvitation, RemoveMemberRequest,
    RoomListing, RoomSummary, SetMemberRoleRequest, UNHOST_ROOM, ask_host,
};

use crate::peer::WorkspaceRegistry;
use tilo_lock::RwLockRecover as _;

const RECORD_VERSION: u32 = 2;
/// Governance publications kept after the last full checkpoint before the
/// room's publication is compacted into a fresh one.
const MAX_PUBLICATION_TAIL: usize = 256;
/// How long members' writes the governor took in may wait to be written
/// through to the room file. A hard stop loses at most these; replaying
/// what the relay retains brings them back.
const KEEP_FOLLOWED: Duration = Duration::from_secs(1);
/// Record writes the room authority has yet to look at, per room.
const RECORD_NOTICES: usize = 256;

/// One governance message this room has published, in publication order.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Publication {
    bytes: Vec<u8>,
    checkpoint: bool,
}

/// Everything a governed room keeps across restarts. Written atomically
/// after each decision, before it is published or answered; the large parts
/// of `peer` are files of their own ([`peer_parts`]), written before it.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RoomRecord {
    version: u32,
    room: String,
    workspace_id: u64,
    name: String,
    authority: DeviceCert,
    peer: Vec<peer_parts::PeerPart>,
    generation: u64,
    /// The complete committed publication: the last full checkpoint and every
    /// change after it. A joining member's bootstrap carries all of it.
    outgoing: Vec<Publication>,
    /// How much of `outgoing` the relay has retained, as of the room's last
    /// write. Publishing does not write the room again: after a crash the
    /// room republishes what it published since, which members tolerate.
    published: usize,
    collaboration: collaboration::CollaborationState,
}

/// What a governance change of a room is, as far as starting the room's
/// document over for newcomers goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    /// It only admits people ([`GovernedRoom::commit_admission`]).
    Admission,
    /// Anything else.
    Other,
}

/// The devices governance lets read a room's channels: the room's governed
/// member devices (`access`, as `peer` derived it at `now`), each record's
/// channel less the devices a Read Deny on that record, its app or a pattern
/// over it withholds. The record of a channel is named by a grant, by one
/// of the routes `participant` keeps or by `named`; a channel whose record
/// is none of these may be anyone's own record and is withheld from every
/// member until it is named.
fn readers(
    peer: &StatePeer,
    access: &GovernedAccess,
    participant: Option<&participation::Participant>,
    named: Option<&collab_types::ChannelRoute>,
    now: SystemTime,
) -> Result<participation::Readers, String> {
    let key = |device: [u8; 32]| collab_types::Ed25519PublicKey::from_bytes(device);
    let mut routes = participant
        .map(participation::Participant::routes)
        .unwrap_or_default();
    routes.extend(named.cloned());
    let record_readers = peer.governance().withheld_record_readers(now, &routes)?;
    let withheld = record_readers
        .withheld
        .into_iter()
        .map(|(channel, devices)| (channel, devices.into_iter().map(key).collect()))
        .collect();
    Ok(participation::Readers::new(
        access.member_devices.iter().copied().map(key).collect(),
        withheld,
        record_readers.unrouted.into_iter().map(key).collect(),
    ))
}

/// This host's part in `room`'s channels, under `dir`, when `authority`
/// (this host, a relay or a person's device) takes part in the channels of
/// the rooms it governs
/// ([`tilo_user_identity::DeviceCapabilities::takes_part_in_room_channels`])
/// and runs no channel service beside the host: a host told of its device's
/// channel service ([`DeviceKeys`]) is not that device's peer.
fn participation(
    dir: &Path,
    room: &str,
    authority: &DeviceCert,
    key: &SigningKey,
    device: Option<&DeviceKeys>,
) -> Option<Arc<participation::Participant>> {
    (device.is_none() && authority.capabilities.takes_part_in_room_channels()).then(|| {
        Arc::new(participation::Participant::new(
            participation::Participation::new(room, dir.join(format!("{room}.channels")), key),
        ))
    })
}

/// What taking a member's write into the governor's copy did.
#[derive(Clone, Copy, Debug, Default)]
struct Followed {
    /// The copy changed.
    changed: bool,
    /// A write the copy let go earlier waited for something that has landed
    /// since: replaying what the relay retains brings it in.
    replay_wanted: bool,
}

/// A room this relay governs.
struct GovernedRoom {
    record: RoomRecord,
    peer: StatePeer,
    path: PathBuf,
    /// The files of `record.peer`'s large parts.
    parts: peer_parts::PeerParts,
    /// Its part in the room's channels, when this host is its device's
    /// channel peer ([`participation`]). Locked apart from the room: admitting
    /// a crowd's devices to the channels held the room for an MLS change
    /// and its writes per batch, and every join queued behind them. Taken
    /// after the room by whoever holds both.
    participation: Option<Arc<participation::Participant>>,
    /// The channel keys of the device this host runs on
    /// ([`DeviceKeys`]), asked for the records of the rooms it holds.
    device: Option<DeviceKeys>,
    /// The owner's pre-admissions and notices.
    admissions: admissions::Admissions,
    /// The requests of people who asked at its door without an invitation.
    wait_list: wait_list::WaitList,
    /// Admits the governance each settle retains to the host's order, after
    /// the room's lock is released.
    publisher: Arc<publisher::RoomPublisher>,
    /// Member writes to follow before compacting a sealed record history
    /// again, after a compaction failed: each attempt opens and merges the
    /// whole history.
    #[cfg(any(feature = "agents", feature = "web"))]
    compaction_wait: usize,
    /// The room's shared desktop as members' writes of it are decided,
    /// followed from the first such write ([`desktop_writes`]).
    desktop: Option<tilo_desktop_model::shared_record::guard::DesktopGuard>,
    /// The segments of members' larger desktop writes, until each is whole.
    desktop_segments: collab_types::DocumentReassembler,
    /// Members' writes the governor took in that the room file lacks.
    unkept: bool,
    /// Whether writing them through is due already ([`Rooms::keep_soon`]).
    keeping: bool,
    /// The room's document started over from a base at an admission, and
    /// what the relay retains of it from before is still to be archived,
    /// ahead of the base's publication ([`Rooms::settle`]).
    rebased: bool,
    /// Every change published since the room's document last started over
    /// at an admission was itself an admission ([`Self::commit_admission`]):
    /// a person let in, perhaps named by their own choice, and nothing
    /// changed or taken back. A newcomer may then be given that tail as it
    /// is, since it shows nothing withdrawn before they joined, and the
    /// room need not start over for them. Only kept in memory: a room read
    /// back from its file starts over at its next admission, as before.
    ///
    /// Held as the room document's heads when that was last true, so a
    /// write that reached the document any other way (a member's write the
    /// governor took in) leaves them behind, and the next admission starts
    /// over.
    admissions_since_base: Option<Vec<automerge::ChangeHash>>,
    /// The session each of those hosts last opened here that this host
    /// answered ([`Rooms::answer_host`]), by the host's device.
    answered: HashMap<[u8; 32], hosts::Answer>,
}

impl RoomRecord {
    fn governance_messages(&self) -> Vec<Vec<u8>> {
        self.outgoing
            .iter()
            .map(|publication| publication.bytes.clone())
            .collect()
    }
}

/// The people `peer`'s governance admits now.
fn member_roots(peer: &StatePeer, now: SystemTime) -> std::collections::BTreeSet<[u8; 32]> {
    peer.governance()
        .governed_access(now)
        .map(|access| access.member_user_roots)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

impl GovernedRoom {
    /// Whether this room's records are sealed channels its device holds the
    /// keys of: from the moment the device this host runs on takes part in
    /// the room's channels.
    fn seals(&self) -> bool {
        self.participation.is_some() || self.device.is_some()
    }

    fn create(
        dir: &Path,
        name: &str,
        authority: &DeviceCert,
        key: &SigningKey,
        device: Option<&DeviceKeys>,
        now: SystemTime,
    ) -> Result<Self, String> {
        Self::create_in(
            dir,
            &uuid::Uuid::new_v4().to_string(),
            name,
            (authority, key),
            device,
            now,
        )
    }

    /// A new governed room under the identity `room`: a fresh one, or the
    /// room a workspace already is when it moves here from the device that
    /// made it (a workspace is its room, and keeps it when its governance
    /// moves).
    fn create_in(
        dir: &Path,
        room: &str,
        name: &str,
        (authority, key): (&DeviceCert, &SigningKey),
        device: Option<&DeviceKeys>,
        now: SystemTime,
    ) -> Result<Self, String> {
        let name = valid_name(name)?;
        let room = uuid::Uuid::parse_str(room)
            .map_err(|_| "a room is a UUID".to_owned())?
            .hyphenated()
            .to_string();
        if dir.join(format!("{room}.room")).exists() {
            return Err("this host has that room already".into());
        }
        let roster = vec![authority.clone()];
        let mut peer = StatePeer::new(
            &room,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )?;
        select_exact_admission(&mut peer)?;
        let participation = participation(dir, &room, authority, key, device);
        let mut seed = tilo_peer_state::workspace_seed(&room, authority, &roster, name)?;
        if participation.is_some() || device.is_some() {
            // The room's records are sealed channels this node opens on
            // first use: a seed record would go out in the clear before its
            // channel exists, and every member who joins after the channel
            // opened refuses that cleartext. The shared desktop starts with
            // the first layout written to it, sealed, as in a room that never
            // had one (`prepare_workspace_layout`).
            seed.documents.clear();
        }
        peer.install_seed(seed, key, now)?;
        prepare_governance(&mut peer, key, now)?;
        let outgoing = peer
            .publication_mut()
            .durable_bootstrap_messages(key, now, 1)?
            .iter()
            .map(|message| publication(message, true))
            .collect::<Result<Vec<_>, _>>()?;
        let path = dir.join(format!("{room}.room"));
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let record = RoomRecord {
            version: RECORD_VERSION,
            workspace_id: crate::room_workspace_id(&room).map_err(|error| error.to_string())?,
            room: room.clone(),
            name: name.to_owned(),
            authority: authority.clone(),
            peer: parts.stage(&peer)?,
            generation: 1,
            outgoing,
            published: 0,
            collaboration: collaboration::CollaborationState::default(),
        };
        let mut this = Self {
            admissions: admissions::Admissions::open(&path)?,
            wait_list: wait_list::WaitList::open(&path)?,
            parts,
            path,
            participation,
            device: device.cloned(),
            record,
            peer,
            publisher: Arc::default(),
            #[cfg(any(feature = "agents", feature = "web"))]
            compaction_wait: 0,
            desktop: None,
            desktop_segments: collab_types::DocumentReassembler::default(),
            unkept: false,
            keeping: false,
            rebased: false,
            admissions_since_base: None,
            answered: HashMap::new(),
        };
        this.persist()?;
        Ok(this)
    }

    fn open(
        path: PathBuf,
        authority: &DeviceCert,
        key: &SigningKey,
        device: Option<&DeviceKeys>,
    ) -> Result<Self, String> {
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        let (version, _) =
            postcard::take_from_bytes::<u32>(&bytes).map_err(|error| error.to_string())?;
        if version != RECORD_VERSION {
            return Err(format!("room record version {version} is unknown"));
        }
        let record: RoomRecord = postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
        if record.authority != *authority {
            return Err("room is governed by a different device".into());
        }
        if !crate::room_workspace_id_matches(&record.room, record.workspace_id)
            || record.published > record.outgoing.len()
        {
            return Err("room record is inconsistent".into());
        }
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let mut peer = StatePeer::restore(
            &parts.load(&record.peer)?,
            &record.room,
            record.authority.clone(),
            vec![record.authority.clone()],
            RevocationList::new(),
            SystemTime::now(),
        )?;
        select_exact_admission(&mut peer)?;
        if !peer.has_governance() {
            return Err("restored room has no governance".into());
        }
        // Parts an interrupted write left behind.
        parts.collect(&record.peer);
        let participation = path
            .parent()
            .and_then(|dir| participation(dir, &record.room, authority, key, device));
        Ok(Self {
            admissions: admissions::Admissions::open(&path)?,
            wait_list: wait_list::WaitList::open(&path)?,
            record,
            peer,
            parts,
            path,
            participation,
            device: device.cloned(),
            publisher: Arc::default(),
            #[cfg(any(feature = "agents", feature = "web"))]
            compaction_wait: 0,
            desktop: None,
            desktop_segments: collab_types::DocumentReassembler::default(),
            unkept: false,
            keeping: false,
            rebased: false,
            admissions_since_base: None,
            answered: HashMap::new(),
        })
    }

    fn persist(&mut self) -> Result<(), String> {
        // What the publisher could not admit is not published yet, whatever
        // `published` says until the next settle takes the failure back.
        let bytes = match self.publisher.unretained_from() {
            Some(first) if first < self.record.published => {
                let mut record = self.record.clone();
                record.published = first;
                postcard::to_stdvec(&record)
            }
            _ => postcard::to_stdvec(&self.record),
        }
        .map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())?;
        self.parts.collect(&self.record.peer);
        Ok(())
    }

    /// Name the current peer state's parts in the room file, unwritten.
    #[cfg(test)]
    fn store_peer(&mut self) -> Result<(), String> {
        self.record.peer = self.parts.stage(&self.peer)?;
        Ok(())
    }

    fn peer(&self) -> &StatePeer {
        &self.peer
    }

    /// The governance this room published that the relay has not retained,
    /// in order, each with its place reserved in `records`; `published`
    /// moves past them. Stops before a change that does not read, with why.
    fn unpublished(
        &mut self,
        records: &crate::room_records::RoomRecords,
    ) -> (Vec<publisher::Publication>, Result<(), String>) {
        let mut publications = Vec::new();
        while let Some(entry) = self.record.outgoing.get(self.record.published) {
            let channel = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&entry.bytes)
                .map_err(|error| error.to_string())
                .and_then(|message| {
                    neighbour_wire::retention(&message, entry.checkpoint)
                        .map(|(channel, _)| channel)
                        .ok_or_else(|| "room governance message has no retention channel".into())
                });
            let channel = match channel {
                Ok(channel) => channel,
                Err(error) => return (publications, Err(error)),
            };
            publications.push(publisher::Publication::new(
                records,
                self.record.workspace_id,
                Some(self.record.published),
                crate::room_records::HostCommit {
                    body: entry.bytes.clone(),
                    channel,
                    checkpoint: entry.checkpoint,
                },
            ));
            self.record.published = self.record.published.saturating_add(1);
        }
        (publications, Ok(()))
    }

    /// Take a member's write to a record the governor has into its copy,
    /// in memory: [`Self::keep_followed`] writes it through.
    fn follow(&mut self, author: [u8; 32], message: neighbour_wire::NeighbourMsg) -> Followed {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id, record_id, ..
        } = &message
        else {
            return Followed::default();
        };
        if !self.peer.holds_document(app_id, record_id) {
            return Followed::default();
        }
        match self.peer.receive_from(
            tilo_peer_state::TransportOrigin::RetainedRelayDelta(author),
            message,
            SystemTime::now(),
        ) {
            Ok(outcome) => Followed {
                changed: outcome.changed || outcome.metadata_changed,
                replay_wanted: outcome.replay_wanted,
            },
            Err(error) => {
                tracing::debug!(room = %self.record.room, %error, "governor did not take in a record write");
                Followed::default()
            }
        }
    }

    /// Write the governor's copy, as following members' writes changed it,
    /// through to the room file. A start replays what the relay retains,
    /// but that replay alone does not bring a write back: the app surfaces
    /// it is decided against are still on their way then, so it holds the
    /// write and may let it go. A write the governor took in and lost that
    /// way left its record with a gap, once a later write that builds on it
    /// was committed, and the room no longer opened.
    fn keep_followed(&mut self) {
        self.unkept = false;
        let kept = self.parts.stage(&self.peer).and_then(|parts| {
            let prior = std::mem::replace(&mut self.record.peer, parts);
            self.persist().inspect_err(|_| self.record.peer = prior)
        });
        if let Err(error) = kept {
            tracing::warn!(room = %self.record.room, %error, "the governor's copy of members' writes was not kept");
        }
    }

    fn room(&self) -> &str {
        &self.record.room
    }

    const fn workspace_id(&self) -> u64 {
        self.record.workspace_id
    }

    fn collaboration(&self) -> &collaboration::CollaborationState {
        &self.record.collaboration
    }

    /// Take off the wait list everyone who joined with an invitation sent
    /// for their entry. A failed write leaves them for the next time.
    fn forget_joined_wait_list(&mut self) {
        let ledger = &self.record.collaboration;
        match self
            .wait_list
            .joined(|invite| ledger.redeemed(invite), tilo_clock::unix_now_ns())
        {
            Ok(0) => {}
            Ok(count) => tracing::info!(
                room = %self.record.room,
                count,
                "invited addresses left the wait list on joining"
            ),
            Err(error) => tracing::warn!(
                room = %self.record.room,
                %error,
                "the wait list kept invited addresses that joined"
            ),
        }
    }

    /// Record `state` alone: challenges, invitations, receipts. Governance and
    /// its publication are unchanged.
    fn commit_state(&mut self, state: collaboration::CollaborationState) -> Result<(), String> {
        let prior = std::mem::replace(&mut self.record.collaboration, state);
        if let Err(error) = self.persist() {
            self.record.collaboration = prior;
            return Err(error);
        }
        Ok(())
    }

    /// Commit a governance change of `candidate`: sign it as the room's next
    /// generation, append its publication, and persist it with the
    /// collaboration state `state` derives from the committed room, all in
    /// one write before anything is answered.
    fn commit(
        &mut self,
        candidate: StatePeer,
        key: &SigningKey,
        now: SystemTime,
        state: impl FnOnce(&RoomRecord, &StatePeer) -> Result<collaboration::CollaborationState, String>,
    ) -> Result<u64, String> {
        self.commit_as(candidate, (key, now), Change::Other, state)
    }

    /// [`Self::commit`] of a change that only admits people: members added
    /// through their invitations, each perhaps named as they chose, and
    /// nothing else changed. While every change since the room's document
    /// last started over was such an admission, the newcomers' change is
    /// appended to the publication rather than starting the room over: a
    /// crowd arriving together cost the room its whole state, rebuilt,
    /// written and sent to every member, once per arrival.
    pub(crate) fn commit_admission(
        &mut self,
        candidate: StatePeer,
        key: &SigningKey,
        now: SystemTime,
        state: impl FnOnce(&RoomRecord, &StatePeer) -> Result<collaboration::CollaborationState, String>,
    ) -> Result<u64, String> {
        self.commit_as(candidate, (key, now), Change::Admission, state)
    }

    fn commit_as(
        &mut self,
        mut candidate: StatePeer,
        (key, now): (&SigningKey, SystemTime),
        change: Change,
        state: impl FnOnce(&RoomRecord, &StatePeer) -> Result<collaboration::CollaborationState, String>,
    ) -> Result<u64, String> {
        let committing = crate::timing::start();
        if candidate.room() != self.peer.room() || candidate.owner() != self.peer.owner() {
            return Err("a room change cannot replace the room".into());
        }
        // A member admitted from now on is given the room as it stands, not
        // what was withdrawn before they joined.
        let joined = !member_roots(&candidate, now).is_subset(&member_roots(&self.peer, now));
        let preparing = crate::timing::start();
        prepare_governance(&mut candidate, key, now)?;
        crate::timing::window("room.commit.prepare", preparing);
        let generation = self
            .record
            .generation
            .checked_add(1)
            .ok_or("room generation exhausted")?;
        let copying = crate::timing::start();
        let mut record = self.record.clone();
        crate::timing::window("room.commit.record", copying);
        let tail = record
            .outgoing
            .iter()
            .rev()
            .take_while(|publication| !publication.checkpoint)
            .count();
        let mut rebased = false;
        // Newcomers whose arrival only adds to a tail of earlier arrivals
        // see nothing in it that was withdrawn before them: the room's
        // document need not start over for them. Anything else since the
        // last start, or a long tail, starts it over as before.
        let appended_admission = joined
            && change == Change::Admission
            && tail < MAX_PUBLICATION_TAIL
            && self
                .admissions_since_base
                .as_ref()
                .is_some_and(|clean| self.peer.room_heads().as_ref() == Some(clean));
        let started_over = (tail >= MAX_PUBLICATION_TAIL || joined) && !appended_admission;
        if started_over {
            // Keep unchanged record checkpoints on their own channels; replacing
            // those baselines could discard member writes. A record changed by
            // this commit must still publish at the governance rollover.
            let changed_records = candidate
                .clone()
                .publication_mut()
                .durable_mutation_messages(&self.peer, key, now, generation)?
                .into_iter()
                .filter_map(|message| match message {
                    neighbour_wire::NeighbourMsg::AppDocument {
                        app_id, record_id, ..
                    } => Some((app_id, record_id)),
                    _ => None,
                })
                .collect::<std::collections::BTreeSet<_>>();
            rebased = joined && candidate.rebase_room()?;
            record.outgoing = candidate
                .publication_mut()
                .durable_bootstrap_messages(key, now, generation)?
                .iter()
                .filter(|message| match message {
                    neighbour_wire::NeighbourMsg::AppDocument {
                        app_id, record_id, ..
                    } => changed_records.contains(&(app_id.clone(), record_id.clone())),
                    _ => true,
                })
                .filter(|message| for_members(message))
                .map(|message| publication(message, true))
                .collect::<Result<_, _>>()?;
            record.published = 0;
        } else {
            let diffing = crate::timing::start();
            let messages = candidate
                .publication_mut()
                .durable_mutation_messages(&self.peer, key, now, generation)?;
            for message in messages.iter().filter(|message| for_members(message)) {
                record.outgoing.push(publication(message, false)?);
            }
            crate::timing::window("room.commit.messages", diffing);
        }
        let encoding = crate::timing::start();
        record.peer = self.parts.stage(&candidate)?;
        crate::timing::window("room.commit.durable", encoding);
        record.generation = generation;
        let deriving = crate::timing::start();
        record.collaboration = state(&record, &candidate)?;
        crate::timing::window("room.commit.state", deriving);
        let prior = std::mem::replace(&mut self.record, record);
        let persisting = crate::timing::start();
        let persisted = self.persist();
        crate::timing::window("room.commit.persist", persisting);
        if let Err(error) = persisted {
            self.record = prior;
            return Err(error);
        }
        self.peer = candidate;
        self.rebased |= rebased;
        // Clean from a start made at an admission; kept clean only by
        // admissions appended to it; anything else spoils it until the next.
        let clean = if started_over {
            joined
        } else {
            appended_admission
        };
        self.admissions_since_base = clean.then(|| self.peer.room_heads()).flatten();
        crate::timing::event(
            "room.governance_commit",
            committing,
            self.record.outgoing.len() as u64,
        );
        Ok(generation)
    }

    fn summary(&self) -> RoomSummary {
        let now = SystemTime::now();
        RoomSummary {
            room: self.record.room.clone(),
            workspace_id: self.record.workspace_id,
            name: self.record.name.clone(),
            generation: self.record.generation,
            members: self
                .peer
                .governance()
                .governed_members(now)
                .unwrap_or_default(),
        }
    }
}

use tilo_peer_state::workspace_membership::valid_room_name as valid_name;

/// Keep exact admission evidence in the governor's copy of the room. A
/// member of a room whose channels are sealed writes its records sealed,
/// and the governor follows those writes into its copy: with exact evidence
/// it keeps each one as the record's ciphertext history and acknowledges
/// it in its next governance change. Without it a sealed write only marks
/// the record encrypted, and every later change of the room (letting the
/// next person in among them) is refused as incomplete encryption state.
/// A room committed before this selection takes it on its next commit.
///
/// The copy also writes under the host's own actor
/// ([`tilo_peer_state::room_host_actor`]): a device hosting its own rooms
/// never writes one room from two documents under one actor.
fn select_exact_admission(peer: &mut StatePeer) -> Result<(), String> {
    peer.write_as_room_host();
    peer.select_admission_v5()
        .map_err(|error| format!("room exceeds exact admission bounds: {error}"))
}

fn prepare_governance(
    peer: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
) -> Result<(), String> {
    peer.governance_mut()
        .reconcile_administrator_grants(key, now)?;
    peer.media_mut().reconcile_media_governance(key, now)?;
    peer.media().validate_media_governance_for_publication()
}

/// Whether the room publishes `message` of a commit to its members. The
/// governor's compaction of a record's sealed history bounds its own copy:
/// every member already holds each write it covers, and the relay retains
/// those writes for whoever joins later, so sending it on would push the
/// whole record to every member each time the history fills, for nothing.
/// It stays in the governor's durable copy alone.
fn for_members(message: &neighbour_wire::NeighbourMsg) -> bool {
    !matches!(
        message,
        neighbour_wire::NeighbourMsg::AppDocument { envelope, .. }
            if envelope.kind == collab_types::OpKind::Compaction
    )
}

fn publication(
    message: &neighbour_wire::NeighbourMsg,
    checkpoint: bool,
) -> Result<Publication, String> {
    let (_, kind) = neighbour_wire::retention(message, checkpoint)
        .ok_or("room governance message has no retention channel")?;
    Ok(Publication {
        bytes: postcard::to_stdvec(message).map_err(|error| error.to_string())?,
        checkpoint: kind == Retention::Checkpoint,
    })
}

/// The extension of the marker beside a room file this host released.
const RELEASED: &str = "released";

/// What the room authority needs from the relay hosting its rooms.
#[derive(Clone)]
pub(crate) struct RoomServices {
    pub(crate) workspaces: WorkspaceRegistry,
    pub(crate) access: crate::host::GovernedAccessWriter,
    pub(crate) records: crate::room_records::RoomRecords,
    pub(crate) endpoint: iroh::Endpoint,
    pub(crate) cert: DeviceCert,
    pub(crate) key: SigningKey,
    pub(crate) revocations: Arc<std::sync::RwLock<RevocationList>>,
    /// The host's metrics, which the sessions it dials count in too.
    pub(crate) metrics: Arc<crate::metrics::Metrics>,
    /// Pre-admissions each room keeps, spent ones included.
    pub(crate) max_pre_admissions: usize,
    /// The live handshake trust policy, for [`crate::room_access::decide`],
    /// which a host with app surfaces or bots of its own decides with.
    #[cfg_attr(
        not(any(feature = "agents", feature = "web")),
        expect(dead_code, reason = "only such a host asks for its own keys")
    )]
    pub(crate) trust_policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    /// Agent delegations, for [`crate::room_access::decide`] and for
    /// refusing an agent the administration only a person holds.
    pub(crate) agents: crate::peer::AgentRegistry,
    /// What the join answers say the host is: a certified host, or the
    /// governing device serving its own rooms.
    pub(crate) mode: tilo_delivery_protocol::workspace_join::WorkspaceHostMode,
    /// Each device whose session to this host authenticated: a host it
    /// dials and could not reach is dialled again then.
    pub(crate) arrivals: crate::attach::Arrivals,
    /// The channel keys of the device this host runs on, when the embedder
    /// runs that device's channel service beside it: the host seals and
    /// opens the records of the rooms it governs with them and admits
    /// nobody. `None` makes the host its device's own channel peer.
    pub(crate) keys: Option<DeviceKeys>,
}

/// The rooms this relay governs.
#[derive(Clone)]
pub struct Rooms {
    inner: Arc<RoomsInner>,
}

/// [`Rooms`] for whoever must not keep them alive: the browser plane, which
/// hands sessions the calls of the rooms this relay carries.
#[cfg(feature = "web")]
#[derive(Clone)]
pub struct WeakRooms(std::sync::Weak<RoomsInner>);

#[cfg(feature = "web")]
impl WeakRooms {
    /// The rooms, while the relay still has them.
    #[must_use]
    pub fn upgrade(&self) -> Option<Rooms> {
        self.0.upgrade().map(|inner| Rooms { inner })
    }
}

struct RoomsInner {
    host: RoomServices,
    dir: PathBuf,
    rooms: AsyncMutex<BTreeMap<String, Arc<AsyncMutex<GovernedRoom>>>>,
    /// Held while a room is created: two devices placing the same room at
    /// once create it once.
    creating: AsyncMutex<()>,
    calls: Arc<voice::Calls>,
    page_joins: std::sync::Mutex<pages::PageJoins>,
    /// The room UUID after each settled governance decision.
    changes: tokio::sync::broadcast::Sender<String>,
    /// Each record write of a member the governor's copy took in.
    record_writes: tokio::sync::broadcast::Sender<RecordWrite>,
    /// What members asking for content are answered from.
    content: content::ContentSlot,
    /// Channel access requests and frames waiting for this node's part in
    /// their room's channels, by room.
    channel_notices: std::sync::Mutex<HashMap<String, Vec<ChannelNotice>>>,
    /// Pre-admitted redemptions waiting for their room's next admission
    /// pass, by room.
    queued_admissions: std::sync::Mutex<HashMap<String, Vec<pages::QueuedAdmission>>>,
    /// Those waiting for their room's next write of the changes it staged,
    /// by room.
    queued_writes: std::sync::Mutex<HashMap<String, Vec<collaboration::Written>>>,
    /// How many members' windows each room could not take in because they
    /// build on history it no longer has
    /// ([`unbuildable_windows::missing_history`]), by room.
    unbuildable_windows: std::sync::Mutex<HashMap<String, u64>>,
    /// The bounds of the governor's private door, over every carrier.
    private: private::PrivateBounds,
    /// The rooms listed as public workspaces, and how lively each is.
    listings: listing::Listings,
    /// Each room's layout scope, as its governance says
    /// ([`layout_scope`]).
    layouts: layout_scope::Layouts,
    /// Whether the records its rooms hold in the clear are sealed: only once
    /// the host's operator decided it (the relay's `reseal`).
    #[cfg(feature = "agents")]
    resealing: std::sync::atomic::AtomicBool,
    /// The rooms this host serves for the device of its person that governs
    /// them ([`crate::hosted`]).
    hosting: crate::hosted::Hosting,
    /// Where invitations to mail to wait-list addresses go, once whoever
    /// runs the relay sends mail ([`Rooms::mail_wait_list`]).
    wait_list_mail: std::sync::OnceLock<(String, tokio::sync::mpsc::UnboundedSender<WaitListMail>)>,
    /// The channel keys of the device this host runs on: handed over by an
    /// embedder that runs the device's channel service beside the host, or
    /// the host's own participation when it is its device's one peer.
    device: std::sync::OnceLock<DeviceKeys>,
    /// Whether this host is that peer ([`Rooms::holds_channels`]).
    holds_channels: std::sync::atomic::AtomicBool,
}

/// What a member sent a room's channels.
enum ChannelNoticeKind {
    /// An access request, with the route of the channel's record when the
    /// device sent it.
    Request(
        Box<collab_types::MlsAccessRequestV1>,
        Option<Box<collab_types::ChannelRoute>>,
    ),
    /// A commit the member wrote.
    Frame(Box<collab_types::MlsGovernanceFrame>),
}

/// A channel notice and the authenticated device that sent it.
type ChannelNotice = ([u8; 32], ChannelNoticeKind);

/// Notices one room holds for this node; a device asks again when its
/// request is dropped.
const MAX_QUEUED_CHANNEL_NOTICES: usize = 16_384;

/// One person's place in a governed room, as installed bots compare it
/// between passes to see who joined, left, changed role or was banned.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemberMark {
    /// `viewer`, `commenter`, `editor`, `moderator`, or `owner` for an
    /// administrator.
    pub role: String,
    /// They were removed (or left).
    pub removed: bool,
    /// They are kept out until unbanned.
    pub banned: bool,
}

/// A member's write to a record of a governed room, after the governor's
/// copy took it in: what wakes the room's installed bots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordWrite {
    /// The room's UUID.
    pub room: String,
    /// The record's app.
    pub app: String,
    /// The record's id on the wire (`""` for the app's primary).
    pub record: String,
}

/// The authority governing one room, as the room's workspace refers to it.
#[derive(Clone)]
pub struct RoomAuthority {
    rooms: std::sync::Weak<RoomsInner>,
    room: String,
    /// The device governing the room: this relay's own, which also takes
    /// part in the room's channels.
    governor: [u8; 32],
}

impl RoomAuthority {
    /// The rooms, while the relay still governs them.
    pub fn rooms(&self) -> Option<Rooms> {
        self.rooms.upgrade().map(|inner| Rooms { inner })
    }

    /// The governed room.
    pub fn room(&self) -> &str {
        &self.room
    }

    /// The device governing the room, which takes part in its channels.
    pub(crate) const fn governor(&self) -> &[u8; 32] {
        &self.governor
    }

    /// An authority governed by `governor`, for a test of the decision.
    #[cfg(test)]
    pub(crate) fn governed_by(room: &str, governor: [u8; 32]) -> Self {
        Self {
            rooms: std::sync::Weak::new(),
            room: room.to_owned(),
            governor,
        }
    }
}

impl std::fmt::Debug for Rooms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rooms")
            .field("dir", &self.inner.dir)
            .finish_non_exhaustive()
    }
}

impl Rooms {
    /// The channel keys of the device this host runs on, when it takes part
    /// in the rooms' channels: the handle its embedder handed it at open, or
    /// its own participation when the host is that device's one peer.
    #[must_use]
    pub fn device_keys(&self) -> Option<DeviceKeys> {
        self.inner.device.get().cloned()
    }

    /// Take the device's channel keys: `RoomServices::keys` when the
    /// embedder runs the device's channel service beside this host, else
    /// this host's participation when its certificate takes part in the
    /// rooms' channels and its device runs no other service.
    fn install_device_keys(&self) {
        if let Some(keys) = self.inner.host.keys.clone() {
            let _ = self.inner.device.set(keys);
        } else if self
            .inner
            .host
            .cert
            .capabilities
            .takes_part_in_room_channels()
        {
            self.inner
                .holds_channels
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = self
                .inner
                .device
                .set(participation::Custody::of(&self.inner));
        }
    }

    /// The channel keys an embedder handed over because it runs the device's
    /// channel service beside this host: then the rooms keep no
    /// participation, asking the device for keys instead.
    fn injected_keys(&self) -> Option<DeviceKeys> {
        self.inner.host.keys.clone()
    }

    /// Whether this host is its device's own channel peer, holding the
    /// leaf in its participation: then it admits the members who ask and
    /// commits the channels itself. A host told of its device's channel
    /// service keeps no channels of its own.
    fn holds_channels(&self) -> bool {
        self.inner
            .holds_channels
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// These rooms, held without keeping them alive.
    #[cfg(feature = "web")]
    #[must_use]
    pub fn downgrade(&self) -> WeakRooms {
        WeakRooms(Arc::downgrade(&self.inner))
    }

    /// Open every room recorded under `dir` and bind each to the relay:
    /// its workspace pinned and admitting only governed members, its
    /// unpublished governance retained, its members' new records created.
    pub(crate) async fn open(host: RoomServices, dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let listings = listing::Listings::open(&dir);
        let this = Self {
            inner: Arc::new(RoomsInner {
                dir: dir.clone(),
                rooms: AsyncMutex::new(BTreeMap::new()),
                calls: Arc::default(),
                creating: AsyncMutex::new(()),
                page_joins: std::sync::Mutex::default(),
                changes: tokio::sync::broadcast::channel(64).0,
                record_writes: tokio::sync::broadcast::channel(256).0,
                content: content::ContentSlot::default(),
                channel_notices: std::sync::Mutex::default(),
                queued_admissions: std::sync::Mutex::default(),
                queued_writes: std::sync::Mutex::default(),
                unbuildable_windows: std::sync::Mutex::default(),
                private: private::PrivateBounds::default(),
                listings,
                layouts: layout_scope::Layouts::default(),
                #[cfg(feature = "agents")]
                resealing: std::sync::atomic::AtomicBool::new(false),
                hosting: crate::hosted::Hosting::new(host.clone()),
                device: std::sync::OnceLock::new(),
                holds_channels: std::sync::atomic::AtomicBool::new(false),
                host,
                wait_list_mail: std::sync::OnceLock::new(),
            }),
        };
        this.install_device_keys();
        let device = this.injected_keys();
        let mut rooms = BTreeMap::new();
        let entries = std::fs::read_dir(&dir).map_err(|error| error.to_string())?;
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.extension().is_none_or(|extension| extension != "room") {
                continue;
            }
            // A room this host released stays on disk, unserved.
            if path.with_extension(RELEASED).exists() {
                continue;
            }
            let room = GovernedRoom::open(
                path.clone(),
                &this.inner.host.cert,
                &this.inner.host.key,
                device.as_ref(),
            )
            .map_err(|error| format!("{}: {error}", path.display()))?;
            rooms.insert(room.room().to_owned(), Arc::new(AsyncMutex::new(room)));
        }
        *this.inner.rooms.lock().await = rooms;
        this.learn_layouts().await;
        let rooms = this
            .inner
            .rooms
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for room in rooms {
            let mut room = room.lock().await;
            this.bind(&room).await?;
            this.settle(&mut room).await?;
            this.recall(&mut room).await?;
            this.prepare_workspace_layout(&mut room).await?;
            // What the relay's log revoked while the room was closed.
            this.carry_revocations(&mut room, &[]).await?;
        }
        this.settle_layouts().await;
        this.follow_activity();
        this.inner.hosting.resume().await;
        Ok(this)
    }

    /// Create a new governed workspace named `name`.
    ///
    /// # Errors
    ///
    /// Returns why the room could not be created, persisted or bound.
    pub async fn create(&self, name: &str) -> Result<RoomSummary, String> {
        self.create_room(name, None).await
    }

    /// Create a governed room named `name`, under the identity `room` when
    /// given: a workspace moving here keeps the room it is. Refused when
    /// this host holds that room already.
    ///
    /// # Errors
    ///
    /// The name is not 1–80 characters, `room` is no UUID or held here
    /// already, or the room could not be written.
    pub async fn create_room(&self, name: &str, room: Option<&str>) -> Result<RoomSummary, String> {
        let _creating = self.inner.creating.lock().await;
        if let Some(room) = room
            && self.inner.rooms.lock().await.contains_key(room)
        {
            return Err("this host has that room already".into());
        }
        if let Some(room) = room
            && self.inner.dir.join(room).with_extension(RELEASED).exists()
        {
            return Err("this host released that room; its files stay here".into());
        }
        // A room this host serves for its governor elsewhere is that
        // governor's: governing it here as well would fork it.
        if let Some(workspace_id) =
            room.and_then(|room| tilo_relay_protocol::room_workspace_id(room).ok())
        {
            let bound = self
                .inner
                .host
                .workspaces
                .lock()
                .await
                .get(&workspace_id)
                .cloned();
            if let Some(bound) = bound
                && bound.lock().await.admission == crate::room_access::AdmissionMode::Governed
            {
                return Err("this host has that room already".into());
            }
        }
        let device = self.injected_keys();
        let room = match room {
            Some(room) => GovernedRoom::create_in(
                &self.inner.dir,
                room,
                name,
                (&self.inner.host.cert, &self.inner.host.key),
                device.as_ref(),
                SystemTime::now(),
            )?,
            None => GovernedRoom::create(
                &self.inner.dir,
                name,
                &self.inner.host.cert,
                &self.inner.host.key,
                device.as_ref(),
                SystemTime::now(),
            )?,
        };
        self.bind(&room).await?;
        let summary = room.summary();
        let room = Arc::new(AsyncMutex::new(room));
        self.inner
            .rooms
            .lock()
            .await
            .insert(summary.room.clone(), Arc::clone(&room));
        self.settle(&mut *room.lock().await).await?;
        tracing::info!(room = %summary.room, workspace_id = summary.workspace_id, "governed room created");
        Ok(summary)
    }

    /// Stop governing and serving `room`: its workspace admits nobody from
    /// now on, which ends its sessions here, and the host leaves it closed
    /// when it opens again. Its files stay on disk. A device releases a
    /// room after moving it to another host, whose hints its members follow.
    ///
    /// # Errors
    ///
    /// The room is not governed here, or the release could not be recorded
    /// or its access narrowed.
    pub async fn release(&self, room: &str) -> Result<(), String> {
        let governed = self
            .inner
            .rooms
            .lock()
            .await
            .get(room)
            .cloned()
            .ok_or("this host does not govern that room")?;
        let governed = governed.lock().await;
        // Recorded first: a host stopped halfway keeps it released.
        let marker = governed.path.with_extension(RELEASED);
        let file = std::fs::File::create(&marker).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        if let Some(dir) = marker.parent() {
            std::fs::File::open(dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|error| error.to_string())?;
        }
        self.inner.rooms.lock().await.remove(room);
        self.inner
            .host
            .access
            .replace(
                governed.workspace_id(),
                true,
                GovernedAccess {
                    member_devices: Vec::new(),
                    member_user_roots: Vec::new(),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        tracing::info!(%room, workspace_id = governed.workspace_id(), "governed room released");
        drop(governed);
        let _ = self.inner.changes.send(room.to_owned());
        Ok(())
    }

    /// Name `room` `name`, in its governance (what members show) and in
    /// this relay's record of it. Naming it what it is called changes
    /// nothing.
    ///
    /// # Errors
    ///
    /// The room is not governed here, the name is not 1–80 characters, or
    /// the change could not be committed.
    pub async fn rename(&self, room: &str, name: &str) -> Result<RoomSummary, String> {
        let name = valid_name(name)?;
        let state = self.room(room).await.map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        if governed.record.name == name
            && governed.peer.governance().room_name().as_deref() == Some(name)
        {
            return Ok(governed.summary());
        }
        let now = SystemTime::now();
        let key = &self.inner.host.key;
        let mut candidate = governed.peer.clone();
        candidate
            .governance_mut()
            .author_room_name(name, key, now)?;
        let prior = std::mem::replace(&mut governed.record.name, name.to_owned());
        let collaboration = governed.collaboration().clone();
        if let Err(error) = governed.commit(candidate, key, now, |_, _| Ok(collaboration)) {
            governed.record.name = prior;
            return Err(error);
        }
        self.settle(&mut governed).await?;
        Ok(governed.summary())
    }

    /// Every room this relay governs.
    pub async fn list(&self) -> Vec<RoomSummary> {
        let rooms = self
            .inner
            .rooms
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut summaries = Vec::with_capacity(rooms.len());
        for room in rooms {
            summaries.push(room.lock().await.summary());
        }
        summaries
    }

    /// The delegated agents on `room`'s roster, acting or not now.
    ///
    /// # Errors
    ///
    /// Returns why the room's roster could not be read.
    pub async fn agents(
        &self,
        room: &str,
    ) -> Result<Vec<tilo_peer_state::GovernedAgent>, CollaborationError> {
        let room = self.room(room).await?;
        let agents = room
            .lock()
            .await
            .peer()
            .governance()
            .governed_agents(SystemTime::now());
        agents.map_err(CollaborationError::new)
    }

    /// The device that governs every room here: this relay's own.
    #[must_use]
    pub fn governor(&self) -> &DeviceCert {
        &self.inner.host.cert
    }

    /// The governor answering the workspace-join exchange for these rooms.
    #[must_use]
    pub fn join_service(&self) -> tilo_peer_state::join_answer::JoinAnswer<Self> {
        tilo_peer_state::join_answer::JoinAnswer(self.clone())
    }

    /// The current role of `device` only when room Write permission is allowed,
    /// including live capability restrictions; `None` otherwise.
    pub async fn legacy_writer_role(
        &self,
        room: &str,
        device: [u8; 32],
    ) -> Option<collab_types::role::Role> {
        let governed = self.room(room).await.ok()?;
        let governed = governed.lock().await;
        let now = SystemTime::now();
        let member = governed.peer().permissions(device, now).ok()?;
        drop(governed);
        member
            .allows(
                &member.policy.node(),
                tilo_action_policy::ActionKind::Undoable,
                now,
            )
            .then_some(member.role)
    }

    /// Room UUIDs, each sent after a governance decision on that room was
    /// committed and published. A slow receiver may lag; it then re-reads.
    #[must_use]
    pub fn governance_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.changes.subscribe()
    }

    /// The invitations members ask to mail to addresses on a wait list,
    /// for whoever runs the relay to send from the web entry `door`: until
    /// this is called, the rooms send no mail and refuse to, and they mail
    /// only invitations on that door
    /// ([`tilo_relay_protocol::rooms::invitation_on_door`]). `None` once
    /// called before.
    #[must_use]
    pub fn mail_wait_list(
        &self,
        door: &str,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<WaitListMail>> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        self.inner
            .wait_list_mail
            .set((door.to_owned(), sender))
            .ok()?;
        Some(receiver)
    }

    /// Each member write to a record the governor's copy took in. A slow
    /// receiver may lag; it then re-reads the records it follows.
    #[must_use]
    pub fn record_writes(&self) -> tokio::sync::broadcast::Receiver<RecordWrite> {
        self.inner.record_writes.subscribe()
    }

    /// The governor's copy of record `record` (`""` for the primary) of
    /// `app` in `room`, saved: a record members write sealed is read with
    /// the keys this relay holds as a member of its channel.
    ///
    /// # Errors
    ///
    /// The room is not governed here, or a sealed record cannot be opened.
    pub async fn record_document(
        &self,
        room: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, CollaborationError> {
        let state = self.room(room).await?;
        let governed = state.lock().await;
        #[cfg(any(feature = "agents", feature = "web"))]
        let document = governed
            .readable_document(app, record)
            .await
            .map_err(CollaborationError::new)?;
        #[cfg(not(any(feature = "agents", feature = "web")))]
        let document = governed.peer().document(app, record);
        drop(governed);
        Ok(document)
    }

    /// The `(app, record)` of every record of `room` the governor's copy
    /// holds readable: in the clear, or sealed under channels this relay
    /// takes part in ([`Self::record_document`] opens those).
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn record_routes(
        &self,
        room: &str,
    ) -> Result<Vec<(String, String)>, CollaborationError> {
        let state = self.room(room).await?;
        let governed = state.lock().await;
        let mut routes = governed.peer().plaintext_routes();
        if governed.seals() {
            routes.extend(governed.peer().sealed_routes());
        }
        drop(governed);
        Ok(routes)
    }

    /// `room` as governance records it now.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn summary(&self, room: &str) -> Result<RoomSummary, CollaborationError> {
        Ok(self.room(room).await?.lock().await.summary())
    }

    /// The spectators watching `room` now: those holding a seat, not every
    /// page that asked for one.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn seated_spectators(&self, room: &str) -> Result<usize, CollaborationError> {
        let workspace_id = self.room(room).await?.lock().await.workspace_id();
        let workspace = self
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned();
        Ok(match workspace {
            Some(workspace) => crate::room_access::seated_spectators(&*workspace.lock().await),
            None => 0,
        })
    }

    /// Every person governance records in `room` other than its owner, by
    /// person (hex root), with the room's governance generation.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn membership(
        &self,
        room: &str,
    ) -> Result<(u64, std::collections::BTreeMap<String, MemberMark>), CollaborationError> {
        let now = SystemTime::now();
        let state = self.room(room).await?;
        let governed = state.lock().await;
        let policy = governed
            .peer()
            .permissions(self.inner.host.cert.device_public_key, now)?
            .policy;
        let owner = self.inner.host.cert.user_public_key;
        let marks = governed
            .peer()
            .governance()
            .governed_members(now)
            .unwrap_or_default()
            .into_iter()
            .filter(|member| member.user_public_key != owner)
            .map(|member| {
                let person = hex::encode(member.user_public_key);
                let role = if member.administrator {
                    "owner"
                } else {
                    match member.role {
                        tilo_peer_state::join::MemberRole::Viewer => "viewer",
                        tilo_peer_state::join::MemberRole::Commenter => "commenter",
                        tilo_peer_state::join::MemberRole::Editor => "editor",
                        tilo_peer_state::join::MemberRole::Moderator => "moderator",
                    }
                };
                let mark = MemberMark {
                    role: role.to_owned(),
                    removed: member.removed,
                    banned: policy.is_blocked(&person),
                };
                (person, mark)
            })
            .collect();
        Ok((governed.summary().generation, marks))
    }

    /// The apps `room` lets author its content
    /// ([`tilo_action_policy::roles::app_authorship`]), as its governance
    /// holds them now.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn app_authors(
        &self,
        room: &str,
    ) -> Result<std::collections::BTreeSet<String>, CollaborationError> {
        let now = SystemTime::now();
        let state = self.room(room).await?;
        let policy = state
            .lock()
            .await
            .peer()
            .permissions(self.inner.host.cert.device_public_key, now)?
            .policy;
        let cx = tilo_action_policy::Context::at_time(now);
        Ok(policy
            .grants
            .iter()
            .filter_map(|grant| match &grant.subject {
                tilo_action_policy::Subject::App(app) => Some(app.clone()),
                _ => None,
            })
            .filter(|app| policy.app_authors(app, &cx))
            .collect())
    }

    /// The workspace-join contract's membership answer for `device`, a
    /// device the room just admitted to subscribe ([`member_bootstrap`]).
    pub(crate) async fn subscriber_bootstrap(
        &self,
        room: &str,
        device: &DeviceCert,
    ) -> Option<Vec<u8>> {
        let room = self.room(room).await.ok()?;
        let room = room.lock().await;
        member_bootstrap(
            room.peer(),
            &room.record.authority,
            room.workspace_id(),
            room.record.generation,
            device,
        )
    }

    async fn room(&self, room: &str) -> Result<Arc<AsyncMutex<GovernedRoom>>, CollaborationError> {
        self.inner
            .rooms
            .lock()
            .await
            .get(room)
            .cloned()
            .ok_or_else(|| CollaborationError::new("this relay does not govern that room"))
    }

    /// Pin the room's workspace, admitting only its governed members, and
    /// follow its members' writes: create the records they start and keep the
    /// governor's copy of the others current, as calls bind to chat records.
    ///
    /// Members' changes stay retained as signed tails: members write directly,
    /// not through the governor, so the owner-baseline selection that retires
    /// a native owner's covered tails does not apply.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace is configured under one lock so no subscriber sees it half bound"
    )]
    async fn bind(&self, room: &GovernedRoom) -> Result<(), String> {
        let host = &self.inner.host;
        let workspace_id = room.workspace_id();
        let open = host.workspaces.lock().await.contains_key(&workspace_id);
        if !open {
            let store = crate::store::WorkspaceStore::open_with_payloads(
                host.access.state_root(),
                workspace_id,
                true,
                host.access.shell_payloads().clone(),
            )
            .map_err(|error| error.to_string())?;
            if store
                .load_snapshot()
                .map_err(|error| error.to_string())?
                .is_none()
            {
                let baseline = room
                    .peer()
                    .admitted_room()
                    .ok_or("room governance missing")?
                    .clone()
                    .save();
                store
                    .save_snapshot(&baseline)
                    .map_err(|error| error.to_string())?;
            }
        }
        let workspace = crate::workspace_io::get_or_initialize(
            &host.workspaces,
            host.access.state_root().to_path_buf(),
            host.access.shell_payloads(),
            workspace_id,
            true,
            crate::room_access::AdmissionMode::Governed,
            crate::peer::MAX_AUTOBOUND_WORKSPACES,
        )
        .await
        .map_err(|error| error.to_string())?;
        let mut runtime = workspace.lock().await;
        // This relay's room authority governs the room: never auto-enrol,
        // even if the room was autobound before the authority took it.
        if std::mem::replace(
            &mut runtime.admission,
            crate::room_access::AdmissionMode::Governed,
        ) != crate::room_access::AdmissionMode::Governed
        {
            crate::authority_epoch::bump();
        }
        runtime.room_authority = Some(RoomAuthority {
            rooms: Arc::downgrade(&self.inner),
            room: room.room().to_owned(),
            governor: host.cert.device_public_key,
        });
        // A room whose layout is personal shares no member's layout: what
        // members wrote of it before is replayed to nobody.
        runtime.store.replay_member_layout(
            self.is_public(room.room())
                .then_some(host.cert.device_public_key),
        );
        if runtime.governor_tap.is_none() {
            let (tap, notices) = tokio::sync::mpsc::channel(RECORD_NOTICES);
            runtime.governor_tap = Some(tap);
            self.follow(room.room().to_owned(), notices);
        }
        Ok(())
    }

    /// Follow each record write a member makes, see [`Self::absorb`], and
    /// replay what the relay retains each time an app surface arrives: the
    /// writes held for it, those let go past the held queue's bound
    /// included, are taken in then rather than when the next write lands.
    fn follow(&self, room: String, mut notices: tokio::sync::mpsc::Receiver<([u8; 32], Vec<u8>)>) {
        let rooms = Arc::downgrade(&self.inner);
        let mut arrivals = crate::surfaces::arrivals();
        arrivals.mark_unchanged();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    notice = notices.recv() => {
                        let Some((author, body)) = notice else { break };
                        let Some(inner) = rooms.upgrade() else { break };
                        if let Err(error) = (Self { inner }).absorb(&room, author, &body).await {
                            tracing::warn!(%room, %error, "governed room did not follow a record write");
                        }
                    }
                    Ok(()) = arrivals.changed() => {
                        let Some(inner) = rooms.upgrade() else { break };
                        (Self { inner }).replay_soon(room.clone());
                    }
                }
            }
        });
    }

    /// Replay what the relay retains for `room` into the governor's copy
    /// ([`Self::recall_retained`]), apart from whatever asked for it. The
    /// relay's copy is read once every write the governor was handed is
    /// durable in it, and before the room is locked: their delivery waits
    /// for the governor to follow them.
    fn replay_soon(&self, room: String) {
        let rooms = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let Some(inner) = rooms.upgrade() else {
                return;
            };
            let rooms = Self { inner };
            let Ok(state) = rooms.room(&room).await else {
                return;
            };
            let workspace_id = state.lock().await.workspace_id();
            let retained = match rooms
                .inner
                .host
                .records
                .retained_settled(workspace_id)
                .await
            {
                Ok(retained) => retained,
                Err(error) => {
                    tracing::warn!(%room, %error, "governed room did not replay what its members wrote");
                    return;
                }
            };
            rooms.recall_retained(&mut *state.lock().await, retained);
        });
    }

    /// Take in `message` if it is a notice rather than a record write: an
    /// access request, a channel commit, the revocations an identity hello
    /// carries, or the room's topology. Hands a record write back.
    async fn absorb_notice(
        &self,
        room: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<Option<neighbour_wire::NeighbourMsg>, String> {
        if matches!(
            message,
            neighbour_wire::NeighbourMsg::MlsAccessRequest { .. }
                | neighbour_wire::NeighbourMsg::MlsRoutedAccessRequest { .. }
                | neighbour_wire::NeighbourMsg::MlsGovernance { .. }
        ) && !self.holds_channels()
        {
            if self.device_keys().is_some() {
                // The device's own channel service takes these in from its
                // own sessions; this host keeps no channels of its own.
                return Ok(None);
            }
            // A host that is no device's peer keeps what the room's channels
            // are made of like any other governance, for whoever takes part:
            // what a governed member device sent, and only that.
            let state = self.room(room).await.map_err(|error| error.to_string())?;
            let (workspace_id, member) = {
                let governed = state.lock().await;
                (
                    governed.workspace_id(),
                    governed
                        .peer()
                        .governance()
                        .governed_access(SystemTime::now())
                        .is_ok_and(|access| access.member_devices.contains(&author)),
                )
            };
            if !member {
                return Ok(None);
            }
            self.publish_channel_governance(workspace_id, vec![message])
                .await?;
            return Ok(None);
        }
        match message {
            neighbour_wire::NeighbourMsg::MlsAccessRequest {
                room: named,
                request,
                ..
            } if named == room => {
                self.channel_notice(room, (author, ChannelNoticeKind::Request(request, None)))
            }
            neighbour_wire::NeighbourMsg::MlsRoutedAccessRequest {
                room: named,
                request,
                route,
                ..
            } if named == room => self.channel_notice(
                room,
                (author, ChannelNoticeKind::Request(request, Some(route))),
            ),
            neighbour_wire::NeighbourMsg::MlsGovernance {
                room: named, frame, ..
            } if named == room => {
                self.channel_notice(room, (author, ChannelNoticeKind::Frame(frame)))
            }
            // The transport authenticated the author, but the entries are
            // the roots' own signatures: who carries them does not matter.
            neighbour_wire::NeighbourMsg::IdentityHello { revocations, .. } => {
                self.take_revocations(revocations).await
            }
            neighbour_wire::NeighbourMsg::SyncResp {
                room: ref named, ..
            } if named == room => self.absorb_topology(room, author, message).await,
            _ => return Ok(Some(message)),
        }
        .map(|()| None)
    }

    /// Take in the record write `body` of `author`. A record the room lacks
    /// is created from it, if `author` is a device of an editor: records are
    /// born only from a governor checkpoint. A write to a record the room has
    /// updates the governor's copy, written through to the room file soon
    /// after ([`Self::keep_soon`]).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed, published and answered"
    )]
    async fn absorb(&self, room: &str, author: [u8; 32], body: &[u8]) -> Result<(), String> {
        let host = &self.inner.host;
        // Every member's record write reaches the governor here, so this is
        // where one that does not decode is reported (the layout and content
        // classifiers that read the same body before it leave it to this).
        let message = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body)
            .map_err(|error| format!("a member's write does not decode: {error}"))?;
        // The relay's own device writes records through the room too: the
        // actions it runs for members (a visitor linking their Discord
        // account) are committed as that device, and the governor's copy,
        // which the room's bots read, must take them in like any member's.
        // Its notices it made itself, so it has them already. A device
        // hosting its own rooms is also their member under the same key, and
        // that member's windows (its topology) are its own to send: those
        // are taken in like any member's.
        // A host told of its device's channel service retains its own
        // device's channel governance like any member's, for the other
        // participants: the host keeps no channels to have it already.
        if author == host.cert.device_public_key
            && !matches!(
                message,
                neighbour_wire::NeighbourMsg::AppDocument { .. }
                    | neighbour_wire::NeighbourMsg::SyncResp { .. }
                    | neighbour_wire::NeighbourMsg::MlsAccessRequest { .. }
                    | neighbour_wire::NeighbourMsg::MlsRoutedAccessRequest { .. }
                    | neighbour_wire::NeighbourMsg::MlsGovernance { .. }
            )
        {
            return Ok(());
        }
        let Some(message) = self.absorb_notice(room, author, message).await? else {
            return Ok(());
        };
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        } = &message
        else {
            return Ok(());
        };
        let (app_id, record_id) = (app_id.clone(), record_id.clone());
        let room = self.room(room).await.map_err(|error| error.to_string())?;
        let waiting = crate::timing::start();
        let mut room = room.lock().await;
        crate::timing::window("room.follow_wait", waiting);
        if room.peer().holds_document(&app_id, &record_id) {
            self.follow_write(&mut room, author, message, (app_id, record_id))
                .await;
            return Ok(());
        }
        let sealed = envelope.encryption.is_some();
        // Only the record's channel members read a sealed write: unless this
        // relay takes part in that channel, it retains the write and the governor
        // keeps no copy. A record that starts sealed, as each new part of a
        // chained record does, is followed like any other where it does.
        #[cfg(any(feature = "agents", feature = "web"))]
        let readable = !sealed || room.takes_part_in_channel(&app_id, &record_id).await;
        #[cfg(not(any(feature = "agents", feature = "web")))]
        let readable = !sealed;
        if !readable {
            return Ok(());
        }
        let now = SystemTime::now();
        let member = room
            .peer()
            .permissions(author, now)
            .map_err(|error| error.to_string())?;
        let node = collab_types::validator::document_node(&member.policy.room, &app_id, &record_id);
        if !member.allows(&node, tilo_action_policy::ActionKind::Undoable, now) {
            return Err(format!(
                "{app_id} record written by a device that may not edit"
            ));
        }
        // The checkpoint includes the write that created the record: the
        // relay retains a member's changes only as a tail of a checkpoint
        // that resolves their dependencies.
        let mut candidate = room.peer().clone();
        candidate.publication_mut().publish_document(
            &app_id,
            &record_id,
            automerge::AutoCommit::new().save(),
            &host.key,
            now,
        )?;
        candidate.receive_from(
            tilo_peer_state::TransportOrigin::RetainedRelayDelta(author),
            message,
            now,
        )?;
        // A sealed write stays in the record's sealed history; a readable
        // one is folded into the governor's checkpoint.
        if !sealed {
            let written = candidate
                .document(&app_id, &record_id)
                .ok_or("created record missing")?;
            candidate
                .publication_mut()
                .publish_document(&app_id, &record_id, written, &host.key, now)?;
        }
        room.commit(candidate, &host.key, now, |record, _| {
            Ok(record.collaboration.clone())
        })?;
        self.settle(&mut room).await
    }

    /// Take a member's own windows into the room's governance: the changes
    /// a member publishes to the room's layout. The room's admission decides
    /// them exactly as every member's does (a change to what someone else
    /// made, without the room's leave, is refused); the governor commits what
    /// it admitted as the room's next generation, which every member then
    /// takes in. Until this, a window someone opened after joining reached
    /// nobody: members take governance only from the governor, and the
    /// governor never took these in, so the member published them again
    /// every few seconds for nothing.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed, published and answered"
    )]
    async fn absorb_topology(
        &self,
        room: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<(), String> {
        let public = self.is_public(room);
        let host = &self.inner.host;
        let room = self.room(room).await.map_err(|error| error.to_string())?;
        let mut room = room.lock().await;
        if let neighbour_wire::NeighbourMsg::SyncResp { changes, .. } = &message {
            // A room whose layout is personal takes in no member's windows;
            // its doors refuse them before they get here (`layout_scope`).
            // Its membership, which members change themselves, it takes in.
            if public
                && !room
                    .peer()
                    .admitted_room()
                    .is_some_and(|before| layout_scope::leaves_layout(before, changes))
            {
                return Ok(());
            }
            let missing =
                unbuildable_windows::missing_history(room.peer().admitted_room(), changes)?;
            if missing > 0 {
                return Err(self.unbuildable_windows(room.room(), missing));
            }
        }
        let now = SystemTime::now();
        let mut candidate = room.peer().clone();
        let outcome = candidate.receive_from(
            tilo_peer_state::TransportOrigin::RetainedRelayDelta(author),
            message,
            now,
        )?;
        if !outcome.changed {
            return Ok(());
        }
        // A member removed takes every way in the room handed them with
        // them: an invitation issued to them before, still unredeemed,
        // would otherwise admit them again at once.
        let removed: std::collections::BTreeSet<_> = member_roots(room.peer(), now)
            .difference(&member_roots(&candidate, now))
            .copied()
            .collect();
        room.commit(candidate, &host.key, now, |record, _| {
            let mut state = record.collaboration.clone();
            state.withdraw_invitations(&removed);
            Ok(state)
        })?;
        self.settle(&mut room).await
    }

    /// Queue `notice` for `room`'s channels. True when it is the first since
    /// they were last taken, so its sender must hand them over.
    fn queue_channel_notice(&self, room: &str, notice: ChannelNotice) -> Result<bool, String> {
        let mut queued = self
            .inner
            .channel_notices
            .lock()
            .map_err(|_| "channel notices poisoned")?;
        let waiting = queued.entry(room.to_owned()).or_default();
        let first = waiting.len() < MAX_QUEUED_CHANNEL_NOTICES && {
            waiting.push(notice);
            waiting.len() == 1
        };
        drop(queued);
        Ok(first)
    }

    /// Take in `author`'s access request or commit for a channel of `room`.
    ///
    /// A room follows its members' writes one at a time, and a crowd joining
    /// sends a request each. So notices only queue here; the first since
    /// they were last taken starts a task that takes the room once for all
    /// of them.
    fn channel_notice(&self, room: &str, notice: ChannelNotice) -> Result<(), String> {
        if self.queue_channel_notice(room, notice)? {
            let rooms = self.clone();
            let room = room.to_owned();
            tokio::spawn(async move {
                if let Err(error) = rooms.take_channel_notices(&room).await {
                    tracing::warn!(%room, %error, "the room's channels did not take their notices");
                }
            });
        }
        Ok(())
    }

    /// Hand `room`'s channels every queued notice: apply the commits, then
    /// admit every device asking in one change per channel, and hold the
    /// channels until that change is durable.
    ///
    /// Each change is an epoch every member applies before it reads what
    /// was written after it, and a member that joined from an admission
    /// the host replaced before it arrived asks again only after seconds.
    /// While one change is written, the requests that arrive wait for the
    /// next one, which admits all of them: a crowd joins in a few changes,
    /// not one per pass.
    async fn take_channel_notices(&self, room: &str) -> Result<(), String> {
        let governed = self.room(room).await.map_err(|error| error.to_string())?;
        let (workspace_id, generation, readers, participant, publisher) = {
            let governed = governed.lock().await;
            let Some(participant) = governed.participation.clone() else {
                self.inner
                    .channel_notices
                    .lock()
                    .map_err(|_| "channel notices poisoned")?
                    .remove(room);
                return Ok(());
            };
            let now = SystemTime::now();
            let access = governed.peer().governance().governed_access(now)?;
            self.learn_queued_routes(room, &participant, &access)?;
            let room_readers = readers(governed.peer(), &access, Some(&*participant), None, now)?;
            let taken = (
                governed.workspace_id(),
                governed.record.generation,
                room_readers,
                participant,
                Arc::clone(&governed.publisher),
            );
            drop(governed);
            taken
        };
        {
            let participation = Arc::clone(&participant.channels).lock_owned().await;
            // Taken under the channels: notices that queue while they work
            // start the next pass, which takes all of them at once.
            let waiting = self
                .inner
                .channel_notices
                .lock()
                .map_err(|_| "channel notices poisoned")?
                .remove(room)
                .unwrap_or_default();
            let readers = participant.readers(generation, readers);
            let passing = crate::timing::start();
            let named = room.to_owned();
            // Verifying requests and the MLS changes admitting them run on a
            // blocking worker: a crowd's took seconds, and held a runtime
            // worker (and every task queued on it) the whole time.
            let (participation, admitted) = tokio::task::spawn_blocking(move || {
                let mut participation = participation;
                let admitted = take_notices(&mut participation, &named, waiting, &readers, passing);
                (participation, admitted)
            })
            .await
            .map_err(|error| error.to_string())?;
            let Some((messages, requests)) = admitted? else {
                return Ok(());
            };
            // After the channel changes a settle handed over.
            publisher.flushed().await;
            let admissions = self
                .enqueue_channel_governance(workspace_id, messages)
                .await?;
            for admission in admissions {
                admission.durable().await?;
            }
            crate::timing::event("channels.pass", passing, requests);
            drop(participation);
        }
        Ok(())
    }

    /// Keep the route of every routed access request queued for `room`'s
    /// `participant` whose author is a governed member device (`access`) and
    /// whose request is its own: the readers the queued requests are decided
    /// with then decide each named record itself. A route that does not
    /// prove its channel is not kept ([`participation::Participant::learn_route`]),
    /// and the request carrying it is refused when it is taken.
    fn learn_queued_routes(
        &self,
        room: &str,
        participant: &participation::Participant,
        access: &GovernedAccess,
    ) -> Result<(), String> {
        let queued = self
            .inner
            .channel_notices
            .lock()
            .map_err(|_| "channel notices poisoned")?;
        for (author, notice) in queued.get(room).into_iter().flatten() {
            if let ChannelNoticeKind::Request(request, Some(route)) = notice
                && request.device.as_bytes() == author
                && access.member_devices.contains(author)
            {
                participant.learn_route(request.channel, route);
            }
        }
        drop(queued);
        Ok(())
    }

    /// Retain channel governance messages as this relay's own.
    async fn publish_channel_governance(
        &self,
        workspace_id: u64,
        messages: Vec<neighbour_wire::NeighbourMsg>,
    ) -> Result<(), String> {
        for admission in self
            .enqueue_channel_governance(workspace_id, messages)
            .await?
        {
            admission.durable().await?;
        }
        Ok(())
    }

    /// Take `message`, `author`'s write to the record `(app, record)` the
    /// governor holds, into its copy, compact the room's sealed histories
    /// before one is full (past its bound the copy would stop following
    /// the record), and tell whoever follows the room's records.
    #[cfg_attr(
        not(any(feature = "agents", feature = "web")),
        expect(
            clippy::unused_async,
            reason = "only a relay that takes part in channels compacts what it follows"
        )
    )]
    async fn follow_write(
        &self,
        room: &mut GovernedRoom,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
        (app, record): (String, String),
    ) {
        let following = crate::timing::start();
        let followed = room.follow(author, message);
        if followed.changed {
            self.keep_soon(room);
        }
        if followed.replay_wanted {
            self.replay_soon(room.room().to_owned());
        }
        crate::timing::window("room.follow", following);
        #[cfg(any(feature = "agents", feature = "web"))]
        {
            let compacting = crate::timing::start();
            if let Err(error) = self.compact_sealed_histories(room).await {
                tracing::warn!(room = %room.room(), %error, "governed room did not compact a sealed record");
            }
            crate::timing::window("room.compact", compacting);
        }
        let _ = self.inner.record_writes.send(RecordWrite {
            room: room.room().to_owned(),
            app,
            record,
        });
    }

    /// Write the members' writes `room` took in through to its room file
    /// within [`KEEP_FOLLOWED`], once for all of them: a write each made a
    /// busy chat queue behind its own room file.
    fn keep_soon(&self, room: &mut GovernedRoom) {
        room.unkept = true;
        if std::mem::replace(&mut room.keeping, true) {
            return;
        }
        let rooms = Arc::downgrade(&self.inner);
        let id = room.room().to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(KEEP_FOLLOWED).await;
            let Some(inner) = rooms.upgrade() else {
                return;
            };
            let Ok(room) = (Self { inner }).room(&id).await else {
                return;
            };
            let mut room = room.lock().await;
            room.keeping = false;
            if room.unkept {
                room.keep_followed();
            }
        });
    }

    /// Queue channel governance messages in the host's order as this
    /// relay's own, without waiting for them to be durable.
    async fn enqueue_channel_governance(
        &self,
        workspace_id: u64,
        messages: Vec<neighbour_wire::NeighbourMsg>,
    ) -> Result<Vec<crate::room_records::RoomAdmission>, String> {
        let mut admissions = Vec::with_capacity(messages.len());
        for message in messages {
            let commit = channel_commit(&message)?;
            admissions.push(
                self.inner
                    .host
                    .records
                    .enqueue(workspace_id, commit)
                    .await?,
            );
        }
        Ok(admissions)
    }

    /// Replay every record write the relay retains for `room` into the
    /// governor's copy, and keep what it took in.
    async fn recall(&self, room: &mut GovernedRoom) -> Result<(), String> {
        let retained = self
            .inner
            .host
            .records
            .retained(room.workspace_id())
            .await?;
        self.recall_retained(room, retained);
        Ok(())
    }

    /// Take `retained`, the record writes the relay retains for `room`, into
    /// the governor's copy, and keep what it took in. A write held for an
    /// app surface not here yet may be let go past the held queue's bound,
    /// and so is every later write of its author to its record, which
    /// would otherwise overtake it for good. The relay still retains them
    /// all: they are replayed again, in the order their authors wrote them,
    /// once the surface arrives ([`Self::follow`]) or something they waited
    /// for lands ([`Followed::replay_wanted`]).
    fn recall_retained(
        &self,
        room: &mut GovernedRoom,
        retained: Vec<crate::room_records::RetainedBody>,
    ) {
        let (mut changed, mut again) = (false, false);
        for retained in retained {
            // What the relay's device wrote is retained under its key, the
            // governor's own commits and its sessions' writes alike: the
            // first are the governor's copy already, the second it took in
            // as they came (`absorb`). Neither is replayed into it.
            if retained.author == self.inner.host.cert.device_public_key {
                continue;
            }
            let message = match postcard::from_bytes(&retained.body) {
                Ok(message) => message,
                Err(error) => {
                    tracing::warn!(
                        author = %hex::encode(retained.author),
                        %error,
                        "a retained write does not decode; it is not replayed"
                    );
                    continue;
                }
            };
            if self.recalls(room, &message) {
                let followed = room.follow(retained.author, message);
                changed |= followed.changed;
                again |= followed.replay_wanted;
            }
        }
        if changed {
            room.keep_followed();
        }
        // What a write let go waited for landed in this replay: the relay's
        // copy is read again, with what reached it since this one was read.
        // Each further replay needs something new to land, so it ends.
        if again {
            self.replay_soon(room.room().to_owned());
        }
    }

    /// Bring the relay in line with the room's committed state: admit exactly
    /// its governed members, then retain every governance change not yet
    /// published, then re-derive its calls' participants. Runs after each
    /// decision, before the decision is answered.
    ///
    /// The changes are handed to the room's [`publisher::RoomPublisher`]
    /// here, under the room's lock, in publication order, each with its
    /// place reserved so that whoever reads the room's retention waits for
    /// it. The publisher admits them to the host's order and waits for them
    /// to be durable after the lock is released: the next decision waits
    /// neither for room in the workspace owner's queue, which a crowd fills,
    /// nor behind this one's disk writes (and every member write the host
    /// queued before them). A change the publisher could not admit is
    /// published again by the next settle.
    async fn settle(&self, room: &mut GovernedRoom) -> Result<(), String> {
        let host = &self.inner.host;
        let now = SystemTime::now();
        let access: GovernedAccess = room.peer().governance().governed_access(now)?;
        let readers = readers(
            room.peer(),
            &access,
            room.participation.as_deref(),
            None,
            now,
        )?;
        host.access
            .replace(room.workspace_id(), true, access)
            .await
            .map_err(|error| error.to_string())?;
        host.access
            .replace_standings(
                room.workspace_id(),
                true,
                room.peer()
                    .governance()
                    .governed_standings(SystemTime::now())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        let publisher = Arc::clone(&room.publisher);
        if room.rebased {
            publisher.flushed().await;
            self.archive_room_history(room).await?;
            room.rebased = false;
        }
        let epoch = publisher.reconcile(&mut room.record.published);
        let workspace_id = room.workspace_id();
        let (mut publications, unreadable) = room.unpublished(&host.records);
        if let Err(error) = unreadable {
            // What precedes it is still published, in order.
            publisher.push(
                &host.records,
                room.room(),
                workspace_id,
                epoch,
                publications,
            );
            return Err(error);
        }
        match room.participation.clone() {
            // A join only lets a device in, which moves no channel: the
            // room does not wait for an admission in progress.
            Some(participant) if participant.moves_channels(&readers) => {
                let mut channels = participant.channels.lock().await;
                let commits = channels.reconcile_all(&readers).and_then(|messages| {
                    participant.hand_over(room.record.generation, &readers);
                    messages
                        .iter()
                        .map(channel_commit)
                        .collect::<Result<Vec<_>, _>>()
                });
                // Handed over under the channels' lock, which orders their
                // changes: whoever admits one next waits for these first.
                // The room's own changes go first either way.
                for commit in commits.iter().flatten().cloned() {
                    publications.push(publisher::Publication::new(
                        &host.records,
                        workspace_id,
                        None,
                        commit,
                    ));
                }
                publisher.push(
                    &host.records,
                    room.room(),
                    workspace_id,
                    epoch,
                    publications,
                );
                drop(channels);
                commits?;
            }
            participant => {
                if let Some(participant) = participant {
                    participant.hand_over(room.record.generation, &readers);
                }
                publisher.push(
                    &host.records,
                    room.room(),
                    workspace_id,
                    epoch,
                    publications,
                );
            }
        }
        self.observe_calls(room);
        // Watchers (a persistent room's operator) learn that this room's
        // governance moved; nobody listening is fine.
        let _ = self.inner.changes.send(room.room().to_owned());
        Ok(())
    }

    /// Archive what the relay retains of `room`'s document, once every
    /// governance change published before is retained: the room starts over
    /// from the base its next publication carries.
    async fn archive_room_history(&self, room: &GovernedRoom) -> Result<(), String> {
        let host = &self.inner.host;
        let workspace_id = room.workspace_id();
        host.records.settled(workspace_id).await;
        let Some((channel, _)) = neighbour_wire::retention(&room.peer().room_message(), true)
        else {
            return Err("the room's document has no retention channel".into());
        };
        let hosted = host.workspaces.lock().await.get(&workspace_id).cloned();
        let workspace = hosted.ok_or("the room's workspace is not hosted here")?;
        let archived =
            crate::compaction::owner_run(&workspace, move |store| store.archive_channel(&channel))
                .await?;
        drop(workspace);
        tracing::info!(room = %room.room(), archived, "the room's history before an admission is archived");
        Ok(())
    }

    /// The signed Iroh address a joining device dials to redeem an
    /// invitation of `room`.
    fn transport_hint(
        &self,
        room: &GovernedRoom,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        let host = &self.inner.host;
        let endpoint_addr = host.endpoint.addr();
        let packet_relay_url = endpoint_addr
            .relay_urls()
            .next()
            .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string);
        tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1 {
            version: 1,
            mode: host.mode,
            room: room.room().to_owned(),
            workspace_id: room.workspace_id(),
            endpoint_addr,
            host_user_public_key: host.cert.user_public_key,
            host_device_certificate: host.cert.clone(),
            packet_relay_url,
        }
        .transport_hint(&host.key)
        .map_err(|error| CollaborationError::new(error.to_string()))
    }

    fn revocations(&self) -> RevocationList {
        self.inner.host.revocations.read_recover().clone()
    }
}

/// Hand `room`'s `participation` the notices `waiting` for it: apply each
/// commit and take each request, then admit every device asking. Synchronous
/// MLS work, for a blocking worker. The governance messages to publish and
/// how many requests they answer; `None` when nobody asked.
fn take_notices(
    participation: &mut participation::Participation,
    room: &str,
    waiting: Vec<ChannelNotice>,
    readers: &participation::Readers,
    passing: Option<std::time::Instant>,
) -> Result<Option<(Vec<neighbour_wire::NeighbourMsg>, u64)>, String> {
    #[cfg(test)]
    tests::channels_pass_hook(room);
    let mut asked = false;
    let mut requests = 0u64;
    for (author, notice) in waiting {
        match notice {
            ChannelNoticeKind::Frame(frame) => {
                // A commit may leave the group short of the readers: the
                // admission below then catches up.
                asked = true;
                if let Err(error) = participation.frame(*frame, readers) {
                    tracing::debug!(room, %error, "channel commit refused");
                }
            }
            ChannelNoticeKind::Request(request, route) => {
                asked = true;
                requests = requests.saturating_add(1);
                if let Err(error) =
                    participation.request(author, *request, route.as_deref(), readers)
                {
                    tracing::debug!(room, %error, "channel access request refused");
                }
            }
        }
    }
    if !asked {
        return Ok(None);
    }
    let messages = participation.admit_pending(readers)?;
    crate::timing::event("channels.admit", passing, requests);
    Ok(Some((messages, requests)))
}

/// A channel governance message, retained as this relay's own.
fn channel_commit(
    message: &neighbour_wire::NeighbourMsg,
) -> Result<crate::room_records::HostCommit, String> {
    let (channel, retention) = neighbour_wire::retention(message, false)
        .ok_or("channel governance has no retention channel")?;
    let body = postcard::to_stdvec(message).map_err(|error| error.to_string())?;
    Ok(crate::room_records::HostCommit {
        body,
        channel,
        checkpoint: retention == neighbour_wire::Retention::Checkpoint,
    })
}

fn system_time(now_ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(now_ns))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The workspace-join contract's membership answer for `device`, a device
/// the room `peer` replicates just admitted to subscribe, postcard encoded:
/// what it builds its replica of the room from before the room's replay
/// ([`tilo_relay_protocol::Envelope::RoomBootstrap`]). A device of the
/// room's owner gets the owner's answer, a member's device its role's, and
/// a device that only watches a reader's. The governance messages and
/// records stay out: the replay that follows carries them.
pub(crate) fn member_bootstrap(
    peer: &StatePeer,
    host: &DeviceCert,
    workspace_id: u64,
    generation: u64,
    device: &DeviceCert,
) -> Option<Vec<u8>> {
    use tilo_peer_state::join::{
        JoinBootstrap, JoinTransportResponse, MemberRole, OwnerBootstrapV1,
    };
    let role = peer
        .permissions(device.device_public_key, SystemTime::now())
        .map_or(collab_types::Role::Viewer, |authority| authority.role);
    let answer = match role {
        collab_types::Role::Owner => {
            JoinTransportResponse::OwnerMembershipComplete(OwnerBootstrapV1 {
                version: 1,
                device_cert: device.clone(),
                room: peer.room().to_owned(),
                workspace_id,
                host: host.clone(),
                certified_roster: peer.certificates(),
                revocations: peer.revocations(),
                records: Vec::new(),
                governance_messages: Vec::new(),
                generation,
            })
        }
        role => JoinTransportResponse::MembershipComplete(JoinBootstrap {
            version: 1,
            invite_id: [0; 32],
            device_cert: device.clone(),
            role: match role {
                collab_types::Role::Commenter => MemberRole::Commenter,
                collab_types::Role::Editor => MemberRole::Editor,
                collab_types::Role::Moderator => MemberRole::Moderator,
                _ => MemberRole::Viewer,
            },
            room: peer.room().to_owned(),
            workspace_id,
            host: host.clone(),
            certified_roster: peer.certificates(),
            revocations: peer.revocations(),
            records: Vec::new(),
            governance_messages: Vec::new(),
            generation,
        }),
    };
    postcard::to_stdvec(&answer).ok()
}
