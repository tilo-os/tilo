//! The encrypted channels of the rooms a node governs.
//!
//! A relay whose certificate lets it own workspaces is the person's node:
//! the device that governs their rooms. Members never write a room's
//! records in the clear; each record's channel is an MLS group, and the node
//! takes part in every one of the rooms it governs, as one participant among
//! the room's member devices ([`channel_crypto::peer`]): it creates a
//! channel's group when the first member asks or its own endpoint writes and
//! no group of it is known, asks to be admitted to one another participant
//! created, admits the room's governed member devices that ask, removes the
//! ones governance removed, and applies and checks the commits any other
//! participant writes. A channel is one record's: a device a Read Deny on
//! that record, its app or a pattern over it withholds ([`Readers`]) is kept
//! out of that channel alone. A channel id is a hash of its record's route,
//! so the node keeps each route a device proved with its request
//! ([`Participant::learn_route`]); a channel whose route it does not know is
//! withheld from every member, as it may be a member's own record, and the
//! node commits nothing to it: it cannot tell who may read it, so it only
//! follows the commits of participants that can. A relay
//! with a plain host certificate governs rooms without taking part in their
//! channels: it only keeps and forwards their sealed frames.
//!
//! Channel state is sealed on disk under a key derived from the node's
//! device key, which already lives beside it.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use channel_crypto::{ChannelPeer, VaultKey};
use collab_types::{
    ChannelId, ChannelRoute, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceFrame,
};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_lock::MutexRecover as _;

use super::{ChannelKeys, DeviceKeys, GovernedRoom, Rooms, RoomsInner, SystemTime};

/// Channels one room takes part in.
const MAX_CHANNELS: usize = 4096;

/// Channels remembered as having a group the node takes no part in.
const MAX_GROUPS_SEEN: usize = 1 << 16;

/// How long the node waits for a Welcome before it asks again, with a fresh
/// member key.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(60);

/// Routes one room keeps: one per channel it may take part in, and as many
/// again for channels asked about that it never opened.
const MAX_ROUTES: usize = 2 * MAX_CHANNELS;

/// The devices governance lets read a room's channels: every reader of the
/// room, less the ones a Read Deny keeps out of a record's channel
/// (`tilo_peer_state::PeerGovernance::withheld_record_readers`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Readers {
    room: BTreeSet<Ed25519PublicKey>,
    /// Each channel whose record the node can name, and who may not read that
    /// record.
    withheld: HashMap<ChannelId, BTreeSet<Ed25519PublicKey>>,
    /// Who is kept out of every channel whose record the node cannot name:
    /// every member device, as such a channel may be a member's own record.
    unrouted: BTreeSet<Ed25519PublicKey>,
}

impl Readers {
    pub(super) fn new(
        room: BTreeSet<Ed25519PublicKey>,
        withheld: HashMap<ChannelId, BTreeSet<Ed25519PublicKey>>,
        unrouted: BTreeSet<Ed25519PublicKey>,
    ) -> Self {
        Self {
            room,
            withheld,
            unrouted,
        }
    }

    /// The devices that may read `channel`; `None` while the node cannot
    /// name its record, and so cannot tell.
    pub(super) fn of(&self, channel: ChannelId) -> Option<BTreeSet<Ed25519PublicKey>> {
        let withheld = self.withheld.get(&channel)?;
        Some(self.room.difference(withheld).copied().collect())
    }

    /// Whether a Read Deny keeps `channel`'s record from any device; a
    /// channel whose record the node cannot name counts as withheld, as it
    /// may be a member's own.
    #[cfg(feature = "web")]
    pub fn withholds(&self, channel: ChannelId) -> bool {
        !self
            .withheld
            .get(&channel)
            .unwrap_or(&self.unrouted)
            .is_empty()
    }

    /// Whether every device these readers let read a channel, `later` lets
    /// read it too: `later` only lets devices in.
    fn within(&self, later: &Self) -> bool {
        // A channel neither names is read by each room less its unrouted.
        let unnamed = |readers: &Self| -> BTreeSet<Ed25519PublicKey> {
            readers
                .room
                .difference(&readers.unrouted)
                .copied()
                .collect()
        };
        unnamed(self).is_subset(&unnamed(later))
            && self
                .withheld
                .keys()
                .chain(later.withheld.keys())
                .all(|channel| {
                    let read =
                        |readers: &Self| readers.of(*channel).unwrap_or_else(|| unnamed(readers));
                    read(self).is_subset(&read(later))
                })
    }
}

/// This node's state for one governed room's channels.
pub(super) struct Participation {
    room: String,
    dir: PathBuf,
    key: SigningKey,
    vault: VaultKey,
    peers: HashMap<ChannelId, ChannelPeer>,
    /// When the node last asked to be admitted to each channel it waits for.
    asked: HashMap<ChannelId, std::time::Instant>,
    /// Channels whose frames passed through while the node took no part in
    /// them: they have a group, which the node joins rather than create one.
    groups_seen: std::collections::HashSet<ChannelId>,
    delivery: u64,
}

/// A governed room's channels, and the readers its governance lets in as the
/// room last handed them over.
pub(super) struct Participant {
    /// The newest governance generation handed over and its readers. Apart
    /// from the channels: governance that only lets devices in moves no
    /// channel, so the room hands its readers over without waiting for an
    /// admission in progress.
    readers: std::sync::Mutex<Option<(u64, Readers)>>,
    /// The route of each channel's record a device proved, kept on disk
    /// beside the channels. Apart from the channels too: readers are read
    /// with them before the channels are taken.
    routes: std::sync::Mutex<Routes>,
    /// Shared so a pass can take it to a blocking worker: its MLS work held
    /// a runtime worker for seconds when a crowd asked at once.
    pub(super) channels: Arc<tokio::sync::Mutex<Participation>>,
}

/// The routes a room's node knows, and where it keeps them.
struct Routes {
    room: String,
    dir: PathBuf,
    known: HashMap<ChannelId, ChannelRoute>,
}

impl Participant {
    pub(super) fn new(channels: Participation) -> Self {
        let routes = Routes::load(&channels.room, &channels.dir);
        Self {
            readers: std::sync::Mutex::new(None),
            routes: std::sync::Mutex::new(routes),
            channels: Arc::new(tokio::sync::Mutex::new(channels)),
        }
    }

    fn known_routes(&self) -> std::sync::MutexGuard<'_, Routes> {
        self.routes.lock_recover()
    }

    /// Every route this node knows, for deciding Read on the records
    /// their channels carry.
    pub(super) fn routes(&self) -> Vec<ChannelRoute> {
        self.known_routes().known.values().cloned().collect()
    }

    /// Keep `route` as `channel`'s, durably, when it proves to be the
    /// channel's route in this room. Returns whether it is now known; a
    /// route that does not prove itself, or one past the room's bound, is
    /// not, and the channel stays decided as one whose record is unknown.
    pub(super) fn learn_route(&self, channel: ChannelId, route: &ChannelRoute) -> bool {
        self.known_routes().learn(channel, route)
    }
}

impl Routes {
    fn path(dir: &Path, channel: ChannelId) -> PathBuf {
        dir.join(format!("{}.route", channel.0))
    }

    /// The routes kept under `dir` that still prove themselves in `room`.
    fn load(room: &str, dir: &Path) -> Self {
        let mut known = HashMap::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("route") {
                continue;
            }
            let Some(channel) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                // optional-decode: a `.route` file not named for a channel is not one
                .and_then(|stem| uuid::Uuid::parse_str(stem).ok())
                .map(ChannelId)
            else {
                continue;
            };
            // A route that does not read is reported, then left out: the
            // channel is routed again when its bridge next proves it.
            let route = match std::fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    postcard::from_bytes::<ChannelRoute>(&bytes).map_err(|error| error.to_string())
                }) {
                Ok(route) => Some(route),
                Err(error) => {
                    tracing::warn!(%room, path = %path.display(), %error, "a stored channel route does not read");
                    None
                }
            }
            .filter(|route| route.proves(room, channel));
            if let Some(route) = route
                && known.len() < MAX_ROUTES
            {
                known.insert(channel, route);
            }
        }
        Self {
            room: room.to_owned(),
            dir: dir.to_owned(),
            known,
        }
    }

    fn learn(&mut self, channel: ChannelId, route: &ChannelRoute) -> bool {
        if self.known.contains_key(&channel) {
            return true;
        }
        if !route.proves(&self.room, channel) || self.known.len() >= MAX_ROUTES {
            return false;
        }
        let saved = std::fs::create_dir_all(&self.dir)
            .and_then(|()| postcard::to_stdvec(route).map_err(std::io::Error::other))
            .and_then(|bytes| {
                tilo_files::native::atomic::write_with_mode(
                    &Self::path(&self.dir, channel),
                    &bytes,
                    0o600,
                )
            });
        if let Err(error) = saved {
            // Known for now; after a restart the device proves it again.
            tracing::warn!(room = %self.room, %error, "channel route could not be saved");
        }
        self.known.insert(channel, route.clone());
        true
    }
}

impl Participant {
    fn handed_over(&self) -> std::sync::MutexGuard<'_, Option<(u64, Readers)>> {
        self.readers.lock_recover()
    }

    /// Whether governance whose readers are `readers` must move the
    /// channels: it removed a reader handed over before, from the room or
    /// from one record's channel, or none were yet (the channels' published
    /// state is then unknown). A device let in is admitted to a channel when
    /// it asks.
    pub(super) fn moves_channels(&self, readers: &Readers) -> bool {
        self.handed_over()
            .as_ref()
            .is_none_or(|(_, before)| !before.within(readers))
    }

    pub(super) fn hand_over(&self, generation: u64, readers: &Readers) {
        let mut handed = self.handed_over();
        if handed
            .as_ref()
            .is_none_or(|(known, _)| *known <= generation)
        {
            *handed = Some((generation, readers.clone()));
        }
    }

    pub(super) fn readers(&self, generation: u64, taken: Readers) -> Readers {
        match &*self.handed_over() {
            Some((handed, readers)) if *handed > generation => readers.clone(),
            _ => taken,
        }
    }
}

impl Participation {
    /// The channels of `room`, kept under `dir`; channels already kept there
    /// are opened as members ask for them.
    pub(super) fn new(room: &str, dir: PathBuf, key: &SigningKey) -> Self {
        let vault = VaultKey::new(blake3::derive_key(
            "tilo-relay governed room channel vault v1",
            &key.to_bytes(),
        ));
        Self {
            room: room.to_owned(),
            dir,
            key: key.clone(),
            vault,
            peers: HashMap::new(),
            asked: HashMap::new(),
            groups_seen: std::collections::HashSet::new(),
            delivery: 0,
        }
    }

    fn device(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.key.verifying_key().to_bytes())
    }

    /// The devices whose membership of `channel` governance backs: its
    /// readers and this node. `None` while the node cannot name the channel's
    /// record.
    fn allowed(&self, readers: &Readers, channel: ChannelId) -> Option<BTreeSet<Ed25519PublicKey>> {
        let mut allowed = readers.of(channel)?;
        allowed.insert(self.device());
        Some(allowed)
    }

    /// Whether `channel`'s peer is open, opening it from disk when cold, or
    /// creating it with its group when `create` and no group of it is known
    /// here.
    fn ensure(&mut self, channel: ChannelId, create: bool) -> Result<bool, String> {
        let wanted = create;
        let create = create && !self.groups_seen.contains(&channel);
        if create && let Some(peer) = self.peers.get_mut(&channel) {
            peer.found_now(&self.vault)
                .map_err(|error| error.to_string())?;
        }
        if !self.peers.contains_key(&channel) {
            if self.peers.len() >= MAX_CHANNELS {
                return Err("the room takes part in too many channels".into());
            }
            let signer = Box::new(self.key.clone());
            let opened = ChannelPeer::open(&self.dir, channel, signer, &self.vault)
                .map_err(|error| error.to_string())?;
            let peer = match opened {
                Some(mut peer) => {
                    if create {
                        peer.found_now(&self.vault)
                            .map_err(|error| error.to_string())?;
                    }
                    peer
                }
                None if create => {
                    std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
                    ChannelPeer::create_now(
                        Some(&self.dir),
                        channel,
                        Box::new(self.key.clone()),
                        &self.vault,
                    )
                    .map_err(|error| error.to_string())?
                }
                // A group the node saw and is not in: it asks to join.
                None if wanted && self.groups_seen.contains(&channel) => {
                    std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
                    ChannelPeer::waiting(
                        Some(&self.dir),
                        channel,
                        Box::new(self.key.clone()),
                        &self.vault,
                    )
                    .map_err(|error| error.to_string())?
                }
                None => return Ok(false),
            };
            self.peers.insert(channel, peer);
        }
        Ok(true)
    }

    /// Read keys for an existing channel, recovering its sealed state if cold.
    /// Missing channels are refused; this path never initializes a group.
    pub(super) fn existing_keys(
        &mut self,
        channel: ChannelId,
    ) -> Result<channel_crypto::ChannelContentKeys, String> {
        if !self.ensure(channel, false)? {
            return Err("the source channel is not open on this host".into());
        }
        self.peers
            .get(&channel)
            .and_then(ChannelPeer::content_keys)
            .ok_or_else(|| "this host holds no key of the channel".into())
    }

    /// Open `channel` for this device's own endpoint, which is about to
    /// write its first record there: create its group when it has none yet,
    /// with `allowed` admitted as they ask, exactly as the first member's
    /// request would. Returns the governance messages to publish.
    pub(super) fn open_channel(
        &mut self,
        channel: ChannelId,
        allowed: &Readers,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.ensure(channel, true)?;
        self.reconcile(channel, allowed)
    }

    /// The members of `channel`'s group, when this room takes part in it.
    #[cfg(test)]
    pub(super) fn members(&self, channel: ChannelId) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.peers.get(&channel).map(ChannelPeer::members)
    }

    /// Take in `request`, which the authenticated device `author` sent, to
    /// be admitted by the next [`Self::admit_pending`] when governance lets
    /// it read the channel's record. The first request governance allows
    /// creates the channel's group.
    pub(super) fn request(
        &mut self,
        author: [u8; 32],
        request: MlsAccessRequestV1,
        route: Option<&ChannelRoute>,
        allowed: &Readers,
    ) -> Result<(), String> {
        if request.device.as_bytes() != &author || request.verify().is_err() {
            return Err("access request is not the sender's own".into());
        }
        if route.is_some_and(|route| !route.proves(&self.room, request.channel)) {
            return Err("access request names a route that is not its channel's".into());
        }
        if !allowed
            .of(request.channel)
            .is_some_and(|readers| readers.contains(&request.device))
        {
            return Ok(());
        }
        let channel = request.channel;
        self.ensure(channel, true)?;
        let Some(peer) = self.peers.get_mut(&channel) else {
            return Err("the channel could not be opened".into());
        };
        peer.learn_request(request, &self.vault)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Apply `frame`, a frame another participant wrote (the frame is
    /// signed, so whoever relayed it does not matter), to the channel when
    /// this room takes part in it. A frame of a channel the node holds
    /// nothing of tells it the channel has a group: when a member asks for
    /// it or its own endpoint opens it, the node asks to be admitted rather
    /// than create a second one.
    pub(super) fn frame(
        &mut self,
        frame: MlsGovernanceFrame,
        allowed: &Readers,
    ) -> Result<(), String> {
        let channel = frame.channel;
        let members = self.allowed(allowed, channel);
        if !self.ensure(channel, false)? {
            if self.groups_seen.len() < MAX_GROUPS_SEEN {
                self.groups_seen.insert(channel);
            }
            return Ok(());
        }
        let Some(peer) = self.peers.get_mut(&channel) else {
            return Ok(());
        };
        match &members {
            Some(members) => peer.receive(frame, members, &self.vault),
            None => peer.follow(frame, &self.vault),
        }
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    /// Bring every channel that has a device asking in line with `allowed`,
    /// one commit per channel for all of them. Returns the governance
    /// messages to publish.
    pub(super) fn admit_pending(&mut self, allowed: &Readers) -> Result<Vec<NeighbourMsg>, String> {
        let channels: Vec<ChannelId> = self
            .peers
            .iter()
            .filter(|(channel, peer)| {
                peer.holds_unshared_keys()
                    || self
                        .allowed(allowed, **channel)
                        .is_some_and(|members| !peer.asking(&members).is_empty())
            })
            .map(|(channel, _)| *channel)
            .collect();
        let mut messages = self.ask()?;
        for channel in channels {
            messages.extend(self.reconcile(channel, allowed)?);
        }
        Ok(messages)
    }

    /// Ask to be admitted to every channel the node waits for: once, and
    /// again with a fresh member key when no Welcome came in [`ASK_AGAIN`].
    fn ask(&mut self) -> Result<Vec<NeighbourMsg>, String> {
        let now = std::time::Instant::now();
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let mut messages = Vec::new();
        for (channel, peer) in &mut self.peers {
            if peer.access() != channel_crypto::Access::Waiting {
                self.asked.remove(channel);
                continue;
            }
            let last = self.asked.get(channel).copied();
            if last.is_some_and(|last| now.duration_since(last) < ASK_AGAIN) {
                continue;
            }
            let request = peer
                .access_request_now(wall, last.is_some(), &self.vault)
                .map_err(|error| error.to_string())?;
            self.asked.insert(*channel, now);
            if let Some(request) = request {
                self.delivery = self.delivery.saturating_add(1);
                messages.push(NeighbourMsg::MlsAccessRequest {
                    room: self.room.clone(),
                    request: Box::new(request),
                    delivery: self.delivery,
                });
            }
        }
        Ok(messages)
    }

    /// Move every open channel to the devices `allowed` lets read it, after
    /// governance removed a reader. A channel whose readers did not change
    /// makes no commit, so a Read Deny on one record starts a new epoch in
    /// that record's channel alone.
    pub(super) fn reconcile_all(&mut self, allowed: &Readers) -> Result<Vec<NeighbourMsg>, String> {
        let channels: Vec<ChannelId> = self.peers.keys().copied().collect();
        let mut messages = Vec::new();
        for channel in channels {
            messages.extend(self.reconcile(channel, allowed)?);
        }
        Ok(messages)
    }

    fn reconcile(
        &mut self,
        channel: ChannelId,
        readers: &Readers,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let room = self.room.clone();
        if !self.ensure(channel, false)? {
            return Ok(Vec::new());
        }
        let Some(peer) = self.peers.get_mut(&channel) else {
            return Ok(Vec::new());
        };
        // A node that cannot name the channel's record cannot tell who may
        // read it: it commits nothing, and removes no one.
        let allowed = readers.of(channel).map(|mut allowed| {
            allowed.insert(Ed25519PublicKey::from_bytes(
                self.key.verifying_key().to_bytes(),
            ));
            allowed
        });
        if let Some(allowed) = &allowed {
            peer.reconcile_now(allowed, &self.vault)
                .map_err(|error| error.to_string())?;
        }
        let frames = peer.outbox().to_vec();
        // The host retains them for whoever subscribes later and delivers
        // them to whoever is live as soon as they are handed over.
        peer.published(&self.vault)
            .map_err(|error| error.to_string())?;
        let mut messages = Vec::with_capacity(frames.len());
        for frame in frames {
            self.delivery = self.delivery.saturating_add(1);
            messages.push(NeighbourMsg::MlsGovernance {
                room: room.clone(),
                frame: Box::new(frame),
                delivery: self.delivery,
            });
        }
        Ok(messages)
    }
}

#[cfg(any(feature = "agents", feature = "web"))]
impl Rooms {
    /// The content keys of `route`'s channel, for this host's own endpoints
    /// (the room actions it runs for its members' pages). `create` opens the
    /// channel when the room actions are about to write a record that has
    /// none yet; otherwise only an existing channel is read and none is ever
    /// created. `None` when its device takes no part in the room's channels:
    /// a participant admits the room actions over the channel like any
    /// member.
    ///
    /// The keys come from the device through [`Rooms::device_keys`], after
    /// the one admission decision that hands them out
    /// ([`RoomOperation::OwnChannelKeys`]), taken under the governed room's
    /// lock. Who may make the room actions act at all is decided there: only
    /// the relay owner's person, through each record's own door.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the decision is taken under the room's lock, the keys read after it"
    )]
    pub async fn own_route_keys(
        &self,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        create: bool,
    ) -> Option<Result<channel_crypto::ChannelContentKeys, String>> {
        let device = self.device_keys()?;
        let state = self.room(&route.cluster_id).await.ok()?;
        {
            let room = state.lock().await;
            if route.workspace_id().ok() != Some(room.workspace_id()) {
                return Some(Err("the record is not in this room".into()));
            }
            let workspace = self
                .inner
                .host
                .workspaces
                .lock()
                .await
                .get(&room.workspace_id())
                .cloned();
            drop(room);
            let Some(workspace) = workspace else {
                return Some(Err("this relay does not hold the room".into()));
            };
            let own = crate::auth::AuthenticatedPeer {
                cert: self.inner.host.cert.clone(),
                user_public_key: self.inner.host.cert.user_public_key,
                device_public_key: self.inner.host.cert.device_public_key,
            };
            let trust = crate::room_access::RelayTrust {
                revocations: &self.inner.host.revocations,
                policy: &self.inner.host.trust_policy,
                own_user: &self.inner.host.cert.user_public_key,
                agents: &self.inner.host.agents,
            };
            let decision = crate::room_access::decide(
                &*workspace.lock().await,
                &trust,
                &own,
                crate::room_access::RoomOperation::OwnChannelKeys,
            );
            if let crate::room_access::Decision::Deny(reason) = decision {
                tracing::warn!(room = %route.cluster_id, reason = reason.reason(),
                    "the host's own channel keys refused");
                return Some(Err(reason.reason().into()));
            }
        }
        // This host's own endpoint names the record it writes: the channel
        // is decided by it from its first admission.
        let named = collab_types::ChannelRoute::new(route.app_id.as_str(), route.record.as_str());
        device.channel_keys(&route.cluster_id, &named, create).await
    }
}

/// The channel keys of a host that is its own device's peer: the leaf its
/// participation holds is the device's one leaf, so its own asks are answered
/// from there.
pub(super) struct Custody(std::sync::Weak<RoomsInner>);

impl Custody {
    /// The handle of `inner`, these rooms' host.
    pub(super) fn of(inner: &Arc<RoomsInner>) -> DeviceKeys {
        Arc::new(Self(Arc::downgrade(inner)))
    }
}

impl ChannelKeys for Custody {
    fn channel_keys<'a>(
        &'a self,
        room: &'a str,
        route: &'a ChannelRoute,
        create: bool,
    ) -> Pin<
        Box<
            dyn Future<Output = Option<Result<channel_crypto::ChannelContentKeys, String>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let rooms = Rooms {
                inner: self.0.upgrade()?,
            };
            rooms.custody_keys(room, route, create).await
        })
    }
}

impl Rooms {
    /// The keys of `route`'s channel in `room` as the participation this
    /// host takes part with holds them
    /// ([`ChannelKeys`] of a host that is its own device's peer): `create`
    /// opens the channel with the devices that may read its record when it
    /// has none yet.
    async fn custody_keys(
        &self,
        room: &str,
        route: &ChannelRoute,
        create: bool,
    ) -> Option<Result<channel_crypto::ChannelContentKeys, String>> {
        let state = self.room(room).await.ok()?;
        let governed = state.lock().await;
        let participant = governed.participation.clone()?;
        let channel = route.channel(room);
        if !route.proves(room, channel) {
            return Some(Err("the record is not this channel's".into()));
        }
        participant.learn_route(channel, route);
        let mut participation = participant.channels.lock().await;
        let keys = match participation.existing_keys(channel) {
            Ok(keys) => keys,
            Err(error) => {
                if !create {
                    return Some(Err(error));
                }
                match self
                    .open_record_channel(&governed, &participant, &mut participation, channel)
                    .await
                {
                    Ok(()) => match participation.existing_keys(channel) {
                        Ok(keys) => keys,
                        Err(error) => return Some(Err(error)),
                    },
                    Err(error) => return Some(Err(error)),
                }
            }
        };
        // Handed over once published: the channel's new epoch reaches the
        // other participants before a write sealed under it does.
        drop(participation);
        tracing::debug!(room = %governed.room(), epoch = keys.epoch(),
            "handed the host's own channel keys");
        Some(Ok(keys))
    }

    /// Open `channel` of `room` as this host's device, creating its group
    /// when it has none, with the devices that may read its record admitted,
    /// and publish that change before anything is written there.
    pub(super) async fn open_record_channel(
        &self,
        room: &GovernedRoom,
        participant: &Participant,
        participation: &mut Participation,
        channel: ChannelId,
    ) -> Result<(), String> {
        let now = SystemTime::now();
        let allowed = super::readers(
            room.peer(),
            &room.peer().governance().governed_access(now)?,
            Some(participant),
            None,
            now,
        )?;
        let messages = participation.open_channel(channel, &allowed)?;
        // After the channel changes a settle handed over.
        room.publisher.flushed().await;
        self.publish_channel_governance(room.workspace_id(), messages)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices(devices: &[u8]) -> BTreeSet<Ed25519PublicKey> {
        devices
            .iter()
            .map(|device| Ed25519PublicKey::from_bytes([*device; 32]))
            .collect()
    }

    fn readers(devices: &[u8]) -> Readers {
        Readers::new(self::devices(devices), HashMap::new(), BTreeSet::new())
    }

    #[test]
    fn only_a_removed_reader_moves_the_channels() {
        let tmp = tempfile::tempdir().unwrap();
        let participant = Participant::new(Participation::new(
            "room",
            tmp.path().join("room.channels"),
            &SigningKey::from_bytes(&[9; 32]),
        ));
        // Nothing handed over yet: the channels' published state is unknown.
        assert!(participant.moves_channels(&readers(&[1])));
        participant.hand_over(1, &readers(&[1]));
        // A join only lets a device in.
        assert!(!participant.moves_channels(&readers(&[1, 2])));
        participant.hand_over(2, &readers(&[1, 2]));
        assert!(participant.moves_channels(&readers(&[2])));
        // An admission that read the room at generation 1 takes the readers
        // handed over since; one that read the newest keeps its own.
        assert_eq!(participant.readers(1, readers(&[1])), readers(&[1, 2]));
        assert_eq!(participant.readers(2, readers(&[1])), readers(&[1]));
        participant.hand_over(1, &readers(&[1]));
        assert_eq!(participant.readers(0, Readers::default()), readers(&[1, 2]));
    }

    /// A Read Deny on one record withholds its channel from the denied
    /// device and moves the channels; the room's other channels keep every
    /// reader, and lifting the Deny only lets the device in again.
    #[test]
    fn a_device_withheld_from_one_record_moves_the_channels() {
        let tmp = tempfile::tempdir().unwrap();
        let participant = Participant::new(Participation::new(
            "room",
            tmp.path().join("room.channels"),
            &SigningKey::from_bytes(&[9; 32]),
        ));
        let (payroll, notes) = (ChannelId::new(), ChannelId::new());
        let room = readers(&[1, 2]);
        participant.hand_over(1, &room);
        let denied = Readers::new(
            devices(&[1, 2]),
            HashMap::from([(payroll, devices(&[2]))]),
            BTreeSet::new(),
        );
        assert_eq!(denied.of(payroll), Some(devices(&[1])));
        assert_eq!(
            denied.of(notes),
            None,
            "a record not named is no one's to read"
        );
        assert!(participant.moves_channels(&denied));
        participant.hand_over(2, &denied);
        assert!(!participant.moves_channels(&denied));
        assert!(!participant.moves_channels(&room));
        assert!(!participant.moves_channels(&Readers::new(
            devices(&[1, 2, 3]),
            HashMap::from([(payroll, devices(&[2]))]),
            BTreeSet::new(),
        )));
        assert!(participant.moves_channels(&Readers::new(
            devices(&[1, 2]),
            HashMap::from([(payroll, devices(&[2])), (notes, devices(&[2]))]),
            BTreeSet::new(),
        )));
        // So does a Deny over an app: every channel whose record is
        // unknown may be one of it.
        assert!(participant.moves_channels(&Readers::new(
            devices(&[1, 2]),
            HashMap::from([(payroll, devices(&[2]))]),
            devices(&[2]),
        )));
    }

    /// A withheld device's request for the denied record's channel is
    /// never admitted, and no group is created for it; its request for
    /// another record's channel is.
    #[test]
    fn a_withheld_device_is_not_admitted_to_the_denied_records_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let mut participation = Participation::new(
            "room",
            tmp.path().join("room.channels"),
            &SigningKey::from_bytes(&[9; 32]),
        );
        let device = SigningKey::from_bytes(&[2; 32]);
        let device_key = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
        let (payroll, notes) = (ChannelId::new(), ChannelId::new());
        let allowed = Readers::new(
            BTreeSet::from([device_key]),
            HashMap::from([
                (payroll, BTreeSet::from([device_key])),
                (notes, BTreeSet::new()),
            ]),
            BTreeSet::new(),
        );
        std::fs::create_dir_all(tmp.path().join("member")).unwrap();
        for channel in [payroll, notes] {
            let request = access_request(&tmp.path().join("member"), &device, channel, 1);
            participation
                .request(device.verifying_key().to_bytes(), request, None, &allowed)
                .unwrap();
        }
        participation.admit_pending(&allowed).unwrap();
        assert!(participation.members(payroll).is_none());
        assert!(participation.members(notes).unwrap().contains(&device_key));
    }

    /// The access request `device` makes for `channel` at second `at`, as a
    /// member would.
    fn access_request(
        dir: &Path,
        device: &SigningKey,
        channel: ChannelId,
        at: u64,
    ) -> MlsAccessRequestV1 {
        std::fs::create_dir_all(dir).unwrap();
        ChannelPeer::waiting(
            Some(dir),
            channel,
            Box::new(device.clone()),
            &VaultKey::new([2; 32]),
        )
        .unwrap()
        .access_request_now(at, false, &VaultKey::new([2; 32]))
        .unwrap()
        .unwrap()
    }

    /// A device that started over with its clock behind the requests its
    /// earlier instance made is not kept waiting behind them: its latest
    /// request is the one admitted.
    #[test]
    fn a_devices_latest_request_wins_whatever_its_clock() {
        let tmp = tempfile::tempdir().unwrap();
        let mut participation = Participation::new(
            "room",
            tmp.path().join("room.channels"),
            &SigningKey::from_bytes(&[9; 32]),
        );
        let device = SigningKey::from_bytes(&[2; 32]);
        let device_key = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
        let channel = ChannelId::new();
        let allowed = Readers::new(
            BTreeSet::from([device_key]),
            HashMap::from([(channel, BTreeSet::new())]),
            BTreeSet::new(),
        );
        let ahead = access_request(&tmp.path().join("earlier"), &device, channel, 100);
        let fresh = access_request(&tmp.path().join("fresh"), &device, channel, 1);
        assert!(fresh.issued_at < ahead.issued_at);
        for request in [ahead, fresh] {
            participation
                .request(device.verifying_key().to_bytes(), request, None, &allowed)
                .unwrap();
        }
        participation.admit_pending(&allowed).unwrap();
        assert!(
            participation
                .members(channel)
                .unwrap()
                .contains(&device_key)
        );
    }

    /// "Finance: Deny Read" for one device, as `readers` hands it over: every
    /// finance channel whose route the node knows is withheld from it, and a
    /// channel whose record it does not know from everyone (no group is
    /// made for it); another app's routed channel is not. A route that is
    /// not its channel's refuses the request.
    #[test]
    fn a_read_deny_over_an_app_withholds_its_routed_and_its_unknown_channels() {
        let tmp = tempfile::tempdir().unwrap();
        let mut participation = Participation::new(
            "room",
            tmp.path().join("room.channels"),
            &SigningKey::from_bytes(&[9; 32]),
        );
        let (denied, other) = (
            SigningKey::from_bytes(&[2; 32]),
            SigningKey::from_bytes(&[3; 32]),
        );
        let key =
            |device: &SigningKey| Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
        let payroll = ChannelRoute::new("finance", "payroll");
        let draft = ChannelRoute::new("notes", "draft");
        let unknown = ChannelRoute::new("notes", "plan").channel("room");
        let (payroll_channel, draft_channel) = (payroll.channel("room"), draft.channel("room"));
        let allowed = Readers::new(
            BTreeSet::from([key(&denied), key(&other)]),
            HashMap::from([
                (payroll_channel, BTreeSet::from([key(&denied)])),
                (draft_channel, BTreeSet::new()),
            ]),
            BTreeSet::from([key(&denied)]),
        );
        assert_eq!(
            allowed.of(payroll_channel),
            Some(BTreeSet::from([key(&other)]))
        );
        assert_eq!(allowed.of(unknown), None);
        assert_eq!(
            allowed.of(draft_channel),
            Some(BTreeSet::from([key(&denied), key(&other)]))
        );

        let member = tmp.path().join("member");
        for device in [&denied, &other] {
            for (channel, route) in [
                (payroll_channel, Some(&payroll)),
                (draft_channel, Some(&draft)),
                (unknown, None),
            ] {
                let request = access_request(
                    &member.join(hex::encode(device.to_bytes())),
                    device,
                    channel,
                    1,
                );
                participation
                    .request(device.verifying_key().to_bytes(), request, route, &allowed)
                    .unwrap();
            }
        }
        let forged = access_request(&member.join("forged"), &other, unknown, 1);
        assert!(
            participation
                .request(
                    other.verifying_key().to_bytes(),
                    forged,
                    Some(&draft),
                    &allowed
                )
                .is_err()
        );
        participation.admit_pending(&allowed).unwrap();
        let members = |channel: ChannelId| participation.members(channel).unwrap();
        assert!(!members(payroll_channel).contains(&key(&denied)));
        assert!(members(payroll_channel).contains(&key(&other)));
        assert!(participation.members(unknown).is_none());
        assert!(members(draft_channel).contains(&key(&denied)));
        assert!(members(draft_channel).contains(&key(&other)));
    }

    /// A route a device proved is kept with the room's channels: a node
    /// opened again knows it from the start, a route that does not prove its
    /// channel is never kept, and a kept file that no longer proves its
    /// channel names nothing.
    #[test]
    fn learned_routes_survive_a_restart_and_only_true_ones_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("room.channels");
        let open = || {
            Participant::new(Participation::new(
                "room",
                dir.clone(),
                &SigningKey::from_bytes(&[9; 32]),
            ))
        };
        let payroll = ChannelRoute::new("finance", "payroll");
        let channel = payroll.channel("room");
        let participant = open();
        assert!(participant.routes().is_empty());
        assert!(!participant.learn_route(channel, &ChannelRoute::new("finance", "budget")));
        assert!(!participant.learn_route(
            ChannelRoute::new("notes", "x").channel("other"),
            &ChannelRoute::new("notes", "x")
        ));
        assert!(participant.learn_route(channel, &payroll));
        assert_eq!(participant.routes(), vec![payroll.clone()]);
        drop(participant);

        assert_eq!(open().routes(), vec![payroll]);
        let budget = ChannelRoute::new("finance", "budget");
        std::fs::write(
            Routes::path(&dir, channel),
            postcard::to_stdvec(&budget).unwrap(),
        )
        .unwrap();
        assert!(open().routes().is_empty());
    }

    /// What a member seals in a channel opens with the content keys the node
    /// reads for a spectator, also from a node that only finds the channel
    /// on disk; a channel never created stays missing.
    #[cfg(any(feature = "agents", feature = "web"))]
    #[test]
    fn a_members_content_opens_with_the_nodes_existing_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("room.channels");
        let node_key = SigningKey::from_bytes(&[9; 32]);
        let mut participation = Participation::new("room", dir.clone(), &node_key);
        let device = SigningKey::from_bytes(&[2; 32]);
        let device_key = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
        let channel = ChannelId::new();
        let allowed = Readers::new(
            BTreeSet::from([device_key]),
            HashMap::from([(channel, BTreeSet::new())]),
            BTreeSet::new(),
        );
        let mut member = ChannelPeer::waiting(
            None,
            channel,
            Box::new(device.clone()),
            &VaultKey::new([2; 32]),
        )
        .unwrap();
        let request = member
            .access_request_now(1, false, &VaultKey::new([2; 32]))
            .unwrap()
            .unwrap();
        let members = BTreeSet::from([
            device_key,
            Ed25519PublicKey::from_bytes(node_key.verifying_key().to_bytes()),
        ]);
        participation
            .request(device.verifying_key().to_bytes(), request, None, &allowed)
            .unwrap();
        for message in participation.admit_pending(&allowed).unwrap() {
            if let NeighbourMsg::MlsGovernance { frame, .. } = message {
                let adds_me = frame.added().contains(&device_key);
                if frame.join_material().is_some() == adds_me {
                    member
                        .receive(*frame, &members, &VaultKey::new([2; 32]))
                        .unwrap();
                }
            }
        }
        let artifact = collab_types::ArtifactId::new();
        let (metadata, sealed) = member
            .content_keys()
            .expect("the admitted member holds the channel's keys")
            .seal_content(artifact, collab_types::ContentRetention::Delta, b"hello")
            .unwrap();
        let opened = |participation: &mut Participation| {
            participation
                .existing_keys(channel)
                .unwrap()
                .open_content(artifact, &metadata, &sealed)
                .unwrap()
                .to_vec()
        };
        assert_eq!(opened(&mut participation), b"hello");
        drop(participation);
        let mut cold = Participation::new("room", dir, &node_key);
        assert_eq!(opened(&mut cold), b"hello");
        assert!(cold.existing_keys(ChannelId::new()).is_err());
    }
}
