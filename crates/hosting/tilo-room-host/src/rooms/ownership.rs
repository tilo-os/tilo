//! Moving a room to another person ([`tilo_peer_state::handoff`]): the
//! room's owner is the person of the host governing it, so a new owner means
//! a new governor.
//!
//! 1. The owner's device offers the room to a member
//!    ([`Rooms::propose_ownership`]); offering again replaces the open offer,
//!    and the owner takes it back with [`Rooms::cancel_ownership_offer`].
//! 2. A device of the recipient accepts that exact offer, naming itself as
//!    the room's next governor and the host it governs from
//!    ([`Rooms::accept_ownership`]). This host commits the room's last
//!    generation under its governance, carrying the handoff, and from then on
//!    changes nothing in the room: it keeps serving what it holds, read-only,
//!    so members that were away catch up with the handoff and move on.
//! 3. The recipient's host takes the room up from its own copy
//!    ([`Rooms::adopt`]): the same room, content and history, governed there.
//!
//! Exactly one host governs the room at any time, whatever crashes: the old
//! host's handoff commit is one write, after which its log says the room is
//! handed on; the new host governs only once its log says the room was
//! handed to it.

use std::path::Path;

use ed25519_dalek::SigningKey;
use tilo_peer_state::StatePeer;
use tilo_peer_state::handoff::{
    self, GovernorHandoff, HandoffAcceptance, HandoffError, MAX_OFFER_NS, OwnershipOffer,
};
use tilo_peer_state::join_ledger::Administered;
use tilo_user_identity::{DeviceCert, RevocationList};

use super::collaboration::request_digest;
use super::{
    CollaborationError, GovernedRoom, RECORD_VERSION, RoomRecord, RoomSummary, Rooms, SystemTime,
    for_members, peer_parts, prepare_governance, publication, select_exact_admission, stewardship,
    system_time, valid_name,
};
use crate::host_authority::HostOperation;

/// Derives an offer's id from the request that made it, so a retried
/// request offers the same thing.
const OFFER_ID_CONTEXT: &str = "tilo room host ownership offer id v1";
/// What a room adopted from a copy without a name of its own is called.
const UNNAMED: &str = "Workspace";

/// Why a step of moving a room to another person was refused.
#[derive(Debug)]
pub enum OwnershipError {
    /// The statement is refused: the asker is not the owner
    /// ([`HandoffError::NotOwner`]), the acceptance is for another offer, the
    /// offer has expired, the room moved already, and the rest.
    Handoff(HandoffError),
    /// The person the room is offered to holds no role in it now.
    RecipientNotMember,
    /// The device asking to take the room is not the one its acceptance
    /// names as the room's next governor.
    NotTheAcceptingDevice,
    /// The copy this host was asked to take up does not hand the room to
    /// this host's device.
    NotHandedHere,
    /// The accepting device holds these channels at an older epoch than
    /// their steward here: it governs them from what it holds, so it takes
    /// what it is owed first and accepts again.
    ChannelsBehind(Vec<collab_types::ChannelId>),
    /// The room is not governed here, the asker is not one of its members,
    /// or the change could not be committed.
    Room(CollaborationError),
}

impl std::fmt::Display for OwnershipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Handoff(error) => write!(f, "{error}"),
            Self::RecipientNotMember => {
                f.write_str("the room is offered only to someone who is in it")
            }
            Self::NotTheAcceptingDevice => {
                f.write_str("the room is taken by the device its acceptance names")
            }
            Self::NotHandedHere => f.write_str("the room was not handed to this device"),
            Self::ChannelsBehind(channels) => write!(
                f,
                "the accepting device has not caught up with {} of the room's channels",
                channels.len()
            ),
            Self::Room(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for OwnershipError {}

impl From<HandoffError> for OwnershipError {
    fn from(error: HandoffError) -> Self {
        Self::Handoff(error)
    }
}

impl From<CollaborationError> for OwnershipError {
    fn from(error: CollaborationError) -> Self {
        Self::Room(error)
    }
}

impl From<String> for OwnershipError {
    fn from(error: String) -> Self {
        Self::Room(CollaborationError::new(error))
    }
}

/// Whether `person` holds a role in `room` now: admitted, not removed and
/// not kept out.
fn active_member(room: &GovernedRoom, person: &[u8; 32], now: SystemTime) -> bool {
    let peer = room.peer();
    let blocked = peer
        .permissions(peer.authority().device_public_key, now)
        .is_ok_and(|member| member.policy.is_blocked(&hex::encode(person)));
    !blocked
        && peer
            .governance()
            .governed_members(now)
            .unwrap_or_default()
            .iter()
            .any(|member| member.user_public_key == *person && !member.removed)
}

impl Rooms {
    /// Offer `room` to the person `recipient`, as the owner's device
    /// `device` asks, in place of any offer open before. A retry of
    /// `request_id` answers with the offer the first attempt recorded.
    ///
    /// # Errors
    ///
    /// The asker is not the room's owner ([`HandoffError::NotOwner`]), the
    /// recipient is the owner or holds no role in the room, the room moved
    /// already, or the offer could not be committed.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed and published"
    )]
    pub async fn propose_ownership(
        &self,
        room: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        recipient: [u8; 32],
    ) -> Result<OwnershipOffer, OwnershipError> {
        let now = tilo_clock::unix_now_ns();
        let governed = self.room(room).await?;
        let mut governed = governed.lock().await;
        self.standing(&governed, device, now)?;
        let host = &self.inner.host.cert;
        if !HostOperation::TransferOwnership.permits(&host.user_public_key, &device.user_public_key)
        {
            return Err(HandoffError::NotOwner.into());
        }
        let digest = request_digest(&("ownership.propose", recipient))?;
        if let Administered::Done(_) = governed.collaboration().administered(request_id, digest)? {
            return governed
                .peer()
                .governance()
                .ownership_offer()
                .ok_or_else(|| HandoffError::WrongOffer.into());
        }
        if governed.moved() {
            return Err(HandoffError::Moved.into());
        }
        if recipient == host.user_public_key {
            return Err(HandoffError::SameOwner.into());
        }
        if !active_member(&governed, &recipient, system_time(now)) {
            return Err(OwnershipError::RecipientNotMember);
        }
        let offer = OwnershipOffer {
            version: handoff::HANDOFF_VERSION,
            room: governed.room().to_owned(),
            offer_id: blake3::derive_key(
                OFFER_ID_CONTEXT,
                &[governed.room().as_bytes(), &request_id].concat(),
            ),
            owner: host.user_public_key,
            recipient,
            proposed_by: device.clone(),
            offered_at_ns: now,
            expires_at_ns: now.saturating_add(MAX_OFFER_NS),
        };
        handoff::check_offer(governed.room(), host, &offer)?;
        let key = self.inner.host.key.clone();
        let recorded = offer.clone();
        self.administer(
            &mut governed,
            request_id,
            digest,
            |peer| {
                peer.governance_mut()
                    .offer_ownership(&recorded, &key, system_time(now))
                    .map_err(|error| error.to_string())
            },
            now,
        )
        .await?;
        Ok(offer)
    }

    /// Take back the open offer `offer_id` of `room`, as the owner's device
    /// `device` asks. Taking back an offer that is no longer open changes
    /// nothing. Answers the room's generation.
    ///
    /// # Errors
    ///
    /// The asker is not the room's owner ([`HandoffError::NotOwner`]), or
    /// the change could not be committed.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed and published"
    )]
    pub async fn cancel_ownership_offer(
        &self,
        room: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        offer_id: [u8; 32],
    ) -> Result<u64, OwnershipError> {
        let now = tilo_clock::unix_now_ns();
        let governed = self.room(room).await?;
        let mut governed = governed.lock().await;
        self.standing(&governed, device, now)?;
        let host = &self.inner.host.cert;
        if !HostOperation::TransferOwnership.permits(&host.user_public_key, &device.user_public_key)
        {
            return Err(HandoffError::NotOwner.into());
        }
        let open = governed.peer().governance().ownership_offer();
        if open.is_none_or(|offer| offer.offer_id != offer_id) {
            return Ok(governed.record.generation);
        }
        let key = self.inner.host.key.clone();
        let done = self
            .administer(
                &mut governed,
                request_id,
                request_digest(&("ownership.cancel", offer_id))?,
                |peer| {
                    peer.governance_mut()
                        .withdraw_ownership_offer(offer_id, &key, system_time(now))
                        .map_err(|error| error.to_string())
                },
                now,
            )
            .await?;
        Ok(done.generation)
    }

    /// Hand `room` to the device that signed `acceptance` of the room's open
    /// offer, as that device, `device`, asks: commit the room's last
    /// generation here, carrying the handoff, and from then on serve the
    /// room read-only. A retry of an acceptance already committed answers
    /// with its handoff.
    ///
    /// # Errors
    ///
    /// `device` is not the acceptance's device or not in the room, the
    /// acceptance does not accept the open offer (or no offer is open), the
    /// offer has expired, its recipient left the room, the room moved
    /// already, or the handoff could not be committed.
    pub async fn accept_ownership(
        &self,
        room: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        acceptance: HandoffAcceptance,
    ) -> Result<GovernorHandoff, OwnershipError> {
        let now = tilo_clock::unix_now_ns();
        if acceptance.successor.device_public_key != device.device_public_key
            || acceptance.successor.user_public_key != device.user_public_key
        {
            return Err(OwnershipError::NotTheAcceptingDevice);
        }
        let governed = self.room(room).await?;
        let mut governed = governed.lock().await;
        if let Some(handoff) = governed
            .peer()
            .governance()
            .governor_handoffs()
            .into_iter()
            .find(|handoff| handoff.acceptance == acceptance)
        {
            return Ok(handoff);
        }
        self.standing(&governed, device, now)?;
        if governed.moved() {
            return Err(HandoffError::Moved.into());
        }
        let offer = governed
            .peer()
            .governance()
            .ownership_offer()
            .ok_or(HandoffError::WrongOffer)?;
        let host = &self.inner.host.cert;
        handoff::check_acceptance(governed.room(), host, &offer, &acceptance, now)?;
        if !active_member(&governed, &offer.recipient, system_time(now)) {
            return Err(OwnershipError::RecipientNotMember);
        }
        if let Some(stewardship) = &governed.stewardship {
            let behind = stewardship
                .channels
                .lock()
                .await
                .behind(acceptance.successor.device_public_key)?;
            if !behind.is_empty() {
                return Err(OwnershipError::ChannelsBehind(behind));
            }
        }
        let digest = request_digest(&("ownership.accept", &acceptance))?;
        let key = self.inner.host.key.clone();
        let generation = governed
            .record
            .generation
            .checked_add(1)
            .ok_or_else(|| "room generation exhausted".to_owned())?;
        let handoff =
            GovernorHandoff::sign(offer, acceptance, host.clone(), generation, now, &key)?;
        let committed = handoff.clone();
        self.administer(
            &mut governed,
            request_id,
            digest,
            |peer| {
                peer.governance_mut()
                    .commit_handoff(&committed, &key, system_time(now))
                    .map_err(|error| error.to_string())
            },
            now,
        )
        .await?;
        // Members still reach the room here until they follow the handoff:
        // it serves them what it holds and takes in nothing more. Its
        // channels are stewarded by the new governor from now on.
        governed.stewardship = None;
        self.bind(&governed).await?;
        tracing::info!(
            room = %governed.room(),
            successor = %hex::encode(handoff.successor().device_public_key),
            "room handed to its new owner's device"
        );
        Ok(handoff)
    }

    /// This host's device's acceptance of `offer`, signed by it: the room
    /// is to be governed here, where members reach this host. The device
    /// sends it to the room's host as `ownership.accept`.
    ///
    /// # Errors
    ///
    /// The offer is not to this host's person, or the acceptance does not
    /// sign.
    pub fn acceptance(&self, offer: &OwnershipOffer) -> Result<HandoffAcceptance, OwnershipError> {
        let host = &self.inner.host;
        if offer.recipient != host.cert.user_public_key {
            return Err(HandoffError::SuccessorNotRecipient.into());
        }
        let workspace_id =
            crate::room_workspace_id(&offer.room).map_err(|error| error.to_string())?;
        let hint = self.hint_for(&offer.room, workspace_id, &host.cert)?;
        Ok(HandoffAcceptance::sign(
            offer,
            host.cert.clone(),
            hint,
            tilo_clock::unix_now_ns(),
            &host.key,
        )?)
    }

    /// Govern `room` from here, taken up from `durable`, this device's own
    /// copy of it ([`StatePeer::durable`]) once that copy followed the
    /// handoff of the room to this host's device. The room keeps its id,
    /// its content and its history; its next generation follows the
    /// handoff's. A room file this host kept of the room before is set
    /// aside, never deleted. Taking up a room this host governs already
    /// answers with it.
    ///
    /// The room's channels go on from `channels`, the groups this device
    /// held as their member: this host stewards each from its own leaf,
    /// under the same channel and epochs, so members keep what they hold
    /// and those who join later read the channels' history.
    ///
    /// # Errors
    ///
    /// The copy does not restore under this host's device, does not hand the
    /// room to it ([`OwnershipError::NotHandedHere`]) or does not make this
    /// host's person its owner ([`HandoffError::NotOwner`]), or the room
    /// could not be written or bound.
    pub async fn adopt(
        &self,
        room: &str,
        durable: &[u8],
        channels: HeldChannels,
    ) -> Result<RoomSummary, OwnershipError> {
        let held = self.inner.rooms.lock().await.get(room).cloned();
        if let Some(held) = held {
            let held = held.lock().await;
            if !held.moved() {
                return Ok(held.summary());
            }
        }
        let adopted = GovernedRoom::adopt(
            &self.inner.dir,
            room,
            durable,
            (&self.inner.host.cert, &self.inner.host.key),
            SystemTime::now(),
        )?;
        take_up_channels(&adopted, channels).await?;
        self.bind(&adopted).await?;
        let summary = adopted.summary();
        let adopted = std::sync::Arc::new(tokio::sync::Mutex::new(adopted));
        self.inner
            .rooms
            .lock()
            .await
            .insert(summary.room.clone(), std::sync::Arc::clone(&adopted));
        self.settle(&mut *adopted.lock().await).await?;
        tracing::info!(room = %summary.room, generation = summary.generation, "room taken up from its handoff");
        Ok(summary)
    }
}

/// The channels of a room handed to this device, as the device held them
/// when it took the room up ([`Rooms::adopt`]).
#[derive(Debug, Default)]
pub struct HeldChannels {
    /// Each channel the device was admitted to, its group taken over from
    /// its member state ([`channel_crypto::ChannelMember::into_succession`]).
    pub held: Vec<channel_crypto::Succession>,
    /// Channels the device knows the room has and holds no member state
    /// of: each starts over with a fresh group when first asked for, and
    /// its earlier history reaches nobody who joins it later.
    pub unheld: Vec<collab_types::ChannelId>,
}

/// Steward `adopted`'s channels from `channels`.
async fn take_up_channels(adopted: &GovernedRoom, channels: HeldChannels) -> Result<(), String> {
    let HeldChannels { held, unheld } = channels;
    let Some(steward) = adopted.stewardship.clone() else {
        if held.is_empty() {
            return Ok(());
        }
        return Err("this host stewards no channels, so it cannot take up the room's".into());
    };
    let mut stewardship = steward.channels.lock().await;
    for succession in held {
        stewardship.succeed(succession)?;
    }
    drop(stewardship);
    for channel in unheld {
        tracing::warn!(
            event = "channel_started_over",
            room = %adopted.room(),
            channel = %channel.0,
            "a channel of a room taken up here was not held by this device: it starts over, \
             and its earlier history reaches nobody who joins it later"
        );
    }
    Ok(())
}

/// Move every file of `room` under `dir` (its room file, parts, admissions,
/// channels and release marker) into a directory of their own beside them,
/// named for when: kept, out of the way of the room this host takes up.
fn set_aside(dir: &Path, room: &str, now_ns: u64) -> Result<(), String> {
    let prefix = format!("{room}.");
    let held: Vec<_> = std::fs::read_dir(dir)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.strip_prefix(&prefix)
                    .is_some_and(|rest| !rest.starts_with("set-aside-"))
            })
        })
        .map(|entry| entry.path())
        .collect();
    if held.is_empty() {
        return Ok(());
    }
    let aside = dir.join(format!("{room}.set-aside-{now_ns}"));
    std::fs::create_dir(&aside).map_err(|error| error.to_string())?;
    for path in held {
        let name = path.file_name().ok_or("a room file has no name")?;
        std::fs::rename(&path, aside.join(name)).map_err(|error| error.to_string())?;
    }
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| error.to_string())?;
    tracing::info!(%room, aside = %aside.display(), "the room's earlier files were set aside");
    Ok(())
}

impl GovernedRoom {
    /// The room `room`, governed by `authority` from its copy `durable`,
    /// which hands it to `authority` ([`Rooms::adopt`]).
    fn adopt(
        dir: &Path,
        room: &str,
        durable: &[u8],
        (authority, key): (&DeviceCert, &SigningKey),
        now: SystemTime,
    ) -> Result<Self, OwnershipError> {
        let room = uuid::Uuid::parse_str(room)
            .map_err(|_| "a room is a UUID".to_owned())?
            .hyphenated()
            .to_string();
        let mut peer = StatePeer::restore(
            durable,
            &room,
            authority.clone(),
            vec![authority.clone()],
            RevocationList::new(),
            now,
        )
        .map_err(|_| OwnershipError::NotHandedHere)?;
        if !peer.has_governance() {
            return Err("the room's copy has no governance".to_owned().into());
        }
        let handoffs = peer.governance().governor_handoffs();
        let handoff = handoffs
            .last()
            .filter(|handoff| handoff.successor() == authority)
            .ok_or(OwnershipError::NotHandedHere)?;
        // Signed by the governor it names, with no clock: the copy took it
        // in under that governor's governance.
        handoff.verify(&room, &handoff.previous, 0)?;
        if peer.governance().handed_on().is_some() {
            return Err(OwnershipError::NotHandedHere);
        }
        let doc = peer.admitted_room().ok_or(OwnershipError::NotHandedHere)?;
        if tilo_desktop_model::projection::room_policy_of(doc, &room).owner
            != Some(hex::encode(authority.user_public_key))
        {
            return Err(HandoffError::NotOwner.into());
        }
        let generation = handoff
            .generation
            .checked_add(1)
            .ok_or_else(|| "room generation exhausted".to_owned())?;
        let name = peer
            .governance()
            .room_name()
            .and_then(|name| valid_name(&name).ok().map(str::to_owned))
            .unwrap_or_else(|| UNNAMED.to_owned());
        select_exact_admission(&mut peer)?;
        prepare_governance(&mut peer, key, now)?;
        let outgoing = peer
            .durable_bootstrap_messages(key, now, generation)?
            .iter()
            .filter(|message| for_members(message))
            .map(|message| publication(message, true))
            .collect::<Result<Vec<_>, _>>()?;
        set_aside(dir, &room, tilo_clock::unix_now_ns())?;
        let path = dir.join(format!("{room}.room"));
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let record = RoomRecord {
            version: RECORD_VERSION,
            workspace_id: crate::room_workspace_id(&room).map_err(|error| error.to_string())?,
            room: room.clone(),
            name,
            authority: authority.clone(),
            peer: parts.stage(&peer)?,
            generation,
            outgoing,
            published: 0,
            collaboration: super::collaboration::CollaborationState::default(),
        };
        let mut this = Self {
            admissions: super::admissions::Admissions::open(&path)?,
            parts,
            path,
            stewardship: stewardship(dir, &room, authority, key),
            record,
            peer,
            publisher: std::sync::Arc::default(),
            #[cfg(any(feature = "agents", feature = "web"))]
            compaction_wait: 0,
            desktop: None,
            desktop_segments: collab_types::DocumentReassembler::default(),
            unkept: false,
            keeping: false,
            rebased: false,
        };
        this.persist()?;
        Ok(this)
    }
}

#[cfg(test)]
mod channel_tests;
#[cfg(test)]
mod network_tests;
#[cfg(test)]
pub(super) mod tests;
