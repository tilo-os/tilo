//! The browser as a participant device of `channel-crypto` channels.
//!
//! Every document route of a room is an encrypted channel whose MLS group
//! holds the room's participant devices ([`channel_crypto::peer`]). The
//! browser takes part through the host:
//!
//! - it asks the participants for access to each channel it opens, with a
//!   request carrying a fresh key package, signed by its device key (which
//!   stays in page custody), re-asking with backoff until a Welcome arrives
//!   (with a fresh member key once an answer is long overdue);
//! - it applies the commits any participant writes (Welcome with the key
//!   history, then commits), keeps of two commits on one epoch the one with
//!   the lower digest, and hands the session the route's content keys
//!   whenever they change;
//! - it checks every commit against the room's governance as its replica of
//!   the room admits it ([`crate::replica`]): a commit may add only a device
//!   the governance lets read the channel's record, the room's governor or
//!   this device, and must be signed by one of them, the participants a
//!   native device admits ([`tilo_peer_state::PeerGovernance::channel_readers`]).
//!
//! The page writes no commits itself: whether a device may read a record
//! depends on Read grants over that record, which the page does not hold,
//! so it leaves admitting and removing to the native participants, which
//! do. Nothing is ever written in the clear: a route's writes are held until
//! this device holds the route's content keys. The host carries every
//! message as retained, opaque governance and never holds a key. Channel
//! state is exported for the profile's durable store, so a reloaded page
//! keeps its access instead of asking again. It is sealed under a vault key
//! the store keeps apart from it: in the page, wrapped by a non-extractable
//! `WebCrypto` key.

use std::collections::{BTreeSet, HashMap};
use tilo_sort::CompactSort as _;

use channel_crypto::{Access, ChannelContentKeys, ChannelPeer, PeerEvent, VaultKey};
use collab_types::{ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceFrame};

use crate::document::{ContentCipher, DocumentRoute};

/// First and longest wait before a waiting device asks again. An answer
/// takes the request's turn in the host's order and then a participant's
/// commit's: in a public room a crowd is joining, seconds. Every ask is a
/// retained write the host fans out to the whole room, so asking again
/// before an answer could arrive only lengthened that wait.
const REQUEST_FIRST_MS: u64 = 8_000;
const REQUEST_LONGEST_MS: u64 = 120_000;
/// The wait from which a device asks with a fresh member key: the host
/// keeps only a channel's newest commit, so an admission replaced before it
/// reached this page is gone, and the participants see the device admitted
/// with the key it asked with. A fresh key makes them admit it again, a
/// change every member applies: asked sooner, in a crowd whose admissions
/// took longer to arrive, it replaced them faster than they arrived.
const REQUEST_FRESH_MS: u64 = 60_000;
/// Frames kept for a channel no route of this page has opened yet, and for
/// an open one whose signer the room's governance does not admit yet.
const MAX_UNOPENED_FRAMES: usize = 256;
/// Channels whose frames are kept before a route opens them.
const MAX_UNOPENED_CHANNELS: usize = 256;

/// Who may sign and be added to a route's channel, as the room's
/// governance says now; `None` while this page holds no governance of the
/// route's room.
pub(crate) type Vouch<'a> = dyn Fn(&DocumentRoute) -> Option<BTreeSet<Ed25519PublicKey>> + 'a;

/// A route's content keys as the session's cipher.
pub(crate) struct KeysCipher(pub(crate) ChannelContentKeys);

impl ContentCipher for KeysCipher {
    fn holds_epoch(&self, epoch: u64) -> bool {
        self.0.holds_epoch(epoch)
    }

    fn seal(
        &mut self,
        artifact: collab_types::ArtifactId,
        retention: collab_types::ContentRetention,
        cleartext: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        self.0
            .seal_content(artifact, retention, cleartext)
            .map_err(|error| error.to_string())
    }

    fn open(
        &mut self,
        artifact: collab_types::ArtifactId,
        metadata: &collab_types::EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.0
            .open_content(artifact, metadata, payload)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }
}

/// A governance message this device has to sign and send: the device key
/// is in page custody, so signing happens in the session.
pub(crate) enum Outgoing {
    /// Ask the participants for access, naming the record the channel
    /// carries so they can decide Read on it.
    Request {
        workspace_id: u64,
        room: String,
        request: MlsAccessRequestV1,
        route: collab_types::ChannelRoute,
    },
}

/// What changed for the session after channel state moved.
#[derive(Default)]
pub(crate) struct Changes {
    /// Routes whose content keys changed, with the new keys.
    pub keys: Vec<(DocumentRoute, ChannelContentKeys)>,
    /// Messages to sign and send.
    pub outgoing: Vec<Outgoing>,
}

impl Changes {
    /// Whether nothing changed.
    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.outgoing.is_empty()
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.keys.extend(other.keys);
        self.outgoing.extend(other.outgoing);
    }
}

/// The verdict on an op for `route` from the gate of the page's state for
/// its channel, through the one gate a broker checks too
/// ([`channel_crypto::admission`]). A page that holds the channel's content
/// keys (`sealed`) knows the channel requires them, as a broker that
/// registered its gate does. `governance_known`: the host answered `Ready`
/// for the route's room, after the governance it holds, so a channel the
/// page has no state of requires nothing; before that, cleartext waits.
pub fn document_admission(
    gate: Option<channel_crypto::ChannelGate>,
    governance_known: bool,
    sealed: bool,
    route: &DocumentRoute,
    encryption: Option<&collab_types::EncryptionMetadata>,
) -> collab_types::inbound::OpAdmission {
    let keys = sealed.then_some(channel_crypto::ChannelGate {
        encryption_required: true,
        head_epoch: None,
    });
    channel_crypto::admission(
        gate.into_iter().chain(keys),
        governance_known,
        Channels::channel(route),
        encryption,
    )
}

/// The page's signer as its channel state sees it: the device key stays in
/// page custody and signs in the session, and the page writes no commits.
struct PageCustody([u8; 32]);

impl channel_crypto::DeviceSigner for PageCustody {
    fn device_public_key(&self) -> [u8; 32] {
        self.0
    }

    fn sign(&self, _: Vec<u8>) -> channel_crypto::SignFuture<'_> {
        Box::pin(async {
            Err(collab_types::signer::SignError(
                "this page writes no channel commits".into(),
            ))
        })
    }
}

struct Route {
    document: DocumentRoute,
    workspace_id: u64,
    peer: ChannelPeer,
    /// Frames refused for a signer the room's governance does not admit
    /// yet, applied again when it changes.
    early: Vec<MlsGovernanceFrame>,
    /// The pending request, re-sent until a participant answers it.
    request: Option<MlsAccessRequestV1>,
    next_request_ms: u64,
    request_delay_ms: u64,
    installed_epoch: Option<u64>,
}

/// A channel's state as [`Channels::export_if_changed`] hands it to the
/// durable store.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedMember {
    pub channel: ChannelId,
    pub sealed: Vec<u8>,
}

/// This page's channel state for the durable store, sealed under the
/// store's vault ([`crate::PeerStore::channel_vault`]), which is kept apart
/// from it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedChannels {
    pub members: Vec<SavedMember>,
}

/// Every channel this page takes part in.
pub(crate) struct Channels {
    device: Ed25519PublicKey,
    vault: VaultKey,
    routes: HashMap<ChannelId, Route>,
    unopened: HashMap<ChannelId, Vec<MlsGovernanceFrame>>,
    /// Restored channel state, waiting for its route to open.
    saved: HashMap<ChannelId, SavedMember>,
    saved_requests: HashMap<ChannelId, MlsAccessRequestV1>,
    dirty: bool,
}

impl Channels {
    /// Channel state for `device`, sealed at rest under `vault`.
    pub(crate) fn new(device: [u8; 32], vault: [u8; 32]) -> Self {
        Self {
            device: Ed25519PublicKey::from_bytes(device),
            vault: VaultKey::new(vault),
            routes: HashMap::new(),
            unopened: HashMap::new(),
            saved: HashMap::new(),
            saved_requests: HashMap::new(),
            dirty: false,
        }
    }

    /// Pick up where a previous page left off.
    pub(crate) fn restore(&mut self, saved: SavedChannels) {
        self.saved = saved
            .members
            .into_iter()
            .map(|member| (member.channel, member))
            .collect();
    }

    /// Restore requests with their sealed state, so a reload cannot race an
    /// earlier request with an unnecessary replacement key package.
    pub(crate) fn restore_requests(&mut self, requests: Vec<MlsAccessRequestV1>) {
        self.saved_requests = requests
            .into_iter()
            .filter(|request| {
                request.device == self.device && self.saved.contains_key(&request.channel)
            })
            .map(|request| (request.channel, request))
            .collect();
    }

    pub(crate) fn pending_requests(&self) -> Vec<MlsAccessRequestV1> {
        let mut requests = self.saved_requests.clone();
        for (channel, route) in &self.routes {
            if let Some(request) = &route.request
                && matches!(route.peer.access(), Access::Waiting)
            {
                requests.insert(*channel, request.clone());
            } else {
                requests.remove(channel);
            }
        }
        let mut requests: Vec<_> = requests.into_values().collect();
        requests.compact_sort_by_key(|request| request.channel.0);
        requests
    }

    /// Have the next [`Self::export_if_changed`] export.
    pub(crate) fn mark_changed(&mut self) {
        self.dirty = true;
    }

    /// This page's channel state for the durable store, when it changed
    /// since the last call.
    pub(crate) fn export_if_changed(&mut self) -> Option<SavedChannels> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        let mut members: HashMap<ChannelId, SavedMember> = self.saved.clone();
        for (channel, route) in &self.routes {
            if let Ok(sealed) = route.peer.export_sealed(&self.vault) {
                members.insert(
                    *channel,
                    SavedMember {
                        channel: *channel,
                        sealed,
                    },
                );
            }
        }
        let mut members: Vec<SavedMember> = members.into_values().collect();
        members.compact_sort_by_key(|member| member.channel.0);
        Some(SavedChannels { members })
    }

    pub(crate) fn channel(route: &DocumentRoute) -> ChannelId {
        collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record)
    }

    /// The verdict on an op for `route` from this page's state for its
    /// channel ([`document_admission`]).
    pub(crate) fn admission(
        &self,
        route: &DocumentRoute,
        (governance_known, sealed): (bool, bool),
        encryption: Option<&collab_types::EncryptionMetadata>,
    ) -> collab_types::inbound::OpAdmission {
        let gate = self
            .routes
            .get(&Self::channel(route))
            .map(|route| route.peer.gate());
        document_admission(gate, governance_known, sealed, route, encryption)
    }

    /// Apply again the frames held for a signer the room's governance did
    /// not admit yet, now that it changed.
    pub(crate) fn retry_early(&mut self, vouch: &Vouch<'_>, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        let channels: Vec<ChannelId> = self
            .routes
            .iter()
            .filter(|(_, route)| !route.early.is_empty())
            .map(|(channel, _)| *channel)
            .collect();
        for channel in channels {
            let early = self
                .routes
                .get_mut(&channel)
                .map(|route| std::mem::take(&mut route.early))
                .unwrap_or_default();
            for frame in early {
                self.apply(channel, frame, vouch, now_ms, &mut changes);
            }
        }
        changes
    }

    /// Take part in `route`'s channel from now on.
    pub(crate) fn open(
        &mut self,
        route: &DocumentRoute,
        vouch: &Vouch<'_>,
        now_ms: u64,
    ) -> Changes {
        let mut changes = Changes::default();
        let channel = Self::channel(route);
        if self.routes.contains_key(&channel) {
            return changes;
        }
        let Ok(workspace_id) = route.workspace_id() else {
            return changes;
        };
        let signer = || Box::new(PageCustody(*self.device.as_bytes()));
        let restored = self.saved.remove(&channel).and_then(|saved| {
            ChannelPeer::restore(channel, signer(), &saved.sealed, &self.vault).ok()
        });
        let request = restored
            .as_ref()
            .filter(|peer| matches!(peer.access(), Access::Waiting))
            .and_then(|_| self.saved_requests.remove(&channel));
        let peer = match restored {
            Some(peer) => peer,
            None => match ChannelPeer::waiting(None, channel, signer(), &self.vault) {
                Ok(peer) => peer,
                Err(_) => return changes,
            },
        };
        self.routes.insert(
            channel,
            Route {
                document: route.clone(),
                workspace_id,
                peer,
                early: Vec::new(),
                request,
                next_request_ms: now_ms,
                request_delay_ms: REQUEST_FIRST_MS,
                installed_epoch: None,
            },
        );
        self.dirty = true;
        for frame in self.unopened.remove(&channel).unwrap_or_default() {
            self.apply(channel, frame, vouch, now_ms, &mut changes);
        }
        self.after(channel, None, now_ms, &mut changes);
        changes
    }

    /// Apply one governance frame of `room` the host delivered.
    pub(crate) fn receive_frame(
        &mut self,
        room: &str,
        frame: MlsGovernanceFrame,
        vouch: &Vouch<'_>,
        now_ms: u64,
    ) -> Changes {
        let mut changes = Changes::default();
        let channel = frame.channel;
        if !self.routes.contains_key(&channel) {
            if self.unopened.len() < MAX_UNOPENED_CHANNELS || self.unopened.contains_key(&channel) {
                let frames = self.unopened.entry(channel).or_default();
                if frames.len() < MAX_UNOPENED_FRAMES {
                    frames.push(frame);
                }
            }
            return changes;
        }
        if self
            .routes
            .get(&channel)
            .is_some_and(|route| route.document.cluster_id == room)
        {
            self.apply(channel, frame, vouch, now_ms, &mut changes);
        }
        changes
    }

    fn apply(
        &mut self,
        channel: ChannelId,
        frame: MlsGovernanceFrame,
        vouch: &Vouch<'_>,
        now_ms: u64,
        changes: &mut Changes,
    ) {
        let Some(route) = self.routes.get_mut(&channel) else {
            return;
        };
        // A commit of a room whose governance this page does not hold yet,
        // or naming a device that governance does not admit yet, waits for
        // the governance to change.
        let Some(allowed) = vouch(&route.document) else {
            if route.early.len() < MAX_UNOPENED_FRAMES {
                route.early.push(frame);
            }
            return;
        };
        let unknown = std::iter::once(&frame.signed_by)
            .chain(frame.added())
            .any(|device| !allowed.contains(device));
        match route.peer.receive(frame.clone(), &allowed, &self.vault) {
            Ok(event) => self.after(channel, Some(event), now_ms, changes),
            Err(_) if unknown => {
                if route.early.len() < MAX_UNOPENED_FRAMES {
                    route.early.push(frame);
                }
            }
            Err(_) => {}
        }
    }

    /// One line per route for diagnostics: where each route's membership
    /// stands.
    pub(crate) fn describe(&self) -> String {
        let mut lines = Vec::new();
        for (channel, route) in &self.routes {
            let access = match route.peer.access() {
                Access::Ready { .. } => "ready".to_owned(),
                other => format!("{other:?}"),
            };
            lines.push(format!(
                "route {}/{} ({channel:?}): {access}; request {}; early {}",
                route.document.app_id,
                route.document.record,
                route.request.is_some(),
                route.early.len()
            ));
        }
        for (channel, frames) in &self.unopened {
            lines.push(format!("unopened {channel:?}: {} frames", frames.len()));
        }
        lines.join("\n")
    }

    /// Ask again where an answer is overdue.
    pub(crate) fn tick(&mut self, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        let channels: Vec<ChannelId> = self.routes.keys().copied().collect();
        for channel in channels {
            self.request_if_due(channel, now_ms, &mut changes);
        }
        changes
    }

    /// Ask at once wherever a channel of `workspace_id` awaits its keys: the
    /// host admitted this session to the room just now, and a request is
    /// sent only to a room that admitted the session, so what was asked
    /// meanwhile went nowhere and is owed now, not after its backoff.
    pub(crate) fn ask_now(&mut self, workspace_id: u64, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        let channels: Vec<ChannelId> = self
            .routes
            .iter()
            .filter(|(_, route)| route.workspace_id == workspace_id)
            .map(|(channel, _)| *channel)
            .collect();
        for channel in channels {
            if let Some(route) = self.routes.get_mut(&channel) {
                route.next_request_ms = now_ms;
            }
            self.request_if_due(channel, now_ms, &mut changes);
        }
        changes
    }

    fn after(
        &mut self,
        channel: ChannelId,
        event: Option<PeerEvent>,
        now_ms: u64,
        changes: &mut Changes,
    ) {
        let Some(route) = self.routes.get_mut(&channel) else {
            return;
        };
        if event.is_some_and(|event| !matches!(event, PeerEvent::Duplicate | PeerEvent::Ignored)) {
            self.dirty = true;
        }
        // Ask again with a fresh member key: the request sent before was
        // answered with a Welcome this device cannot open, by an admission
        // since replaced, or this device was removed.
        if matches!(event, Some(PeerEvent::Desynchronised | PeerEvent::Removed)) {
            route.request = None;
            route.next_request_ms = now_ms;
            route.request_delay_ms = REQUEST_FIRST_MS;
        }
        if let Some(keys) = route.peer.content_keys()
            && route.installed_epoch != Some(keys.epoch())
        {
            route.installed_epoch = Some(keys.epoch());
            changes.keys.push((route.document.clone(), keys));
        }
        self.request_if_due(channel, now_ms, changes);
    }

    fn request_if_due(&mut self, channel: ChannelId, now_ms: u64, changes: &mut Changes) {
        let Some(route) = self.routes.get_mut(&channel) else {
            return;
        };
        if matches!(route.peer.access(), Access::Ready { .. }) || now_ms < route.next_request_ms {
            return;
        }
        let fresh = route.request_delay_ms >= REQUEST_FRESH_MS;
        if fresh {
            route.request = None;
        }
        if route.request.is_none() {
            route.request = route
                .peer
                .unsigned_access_request(now_ms / 1000, fresh, &self.vault)
                .ok()
                .flatten();
            self.dirty = true;
        }
        let Some(request) = route.request.clone() else {
            return;
        };
        route.next_request_ms = now_ms.saturating_add(route.request_delay_ms);
        route.request_delay_ms = route
            .request_delay_ms
            .saturating_mul(2)
            .min(REQUEST_LONGEST_MS);
        changes.outgoing.push(Outgoing::Request {
            workspace_id: route.workspace_id,
            room: route.document.cluster_id.clone(),
            request,
            route: collab_types::ChannelRoute::new(
                route.document.app_id.as_str(),
                route.document.record.as_str(),
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    /// The room's governance admitting exactly `devices`.
    fn admitting(
        devices: &[&SigningKey],
    ) -> impl Fn(&DocumentRoute) -> Option<BTreeSet<Ed25519PublicKey>> + use<> {
        let devices: BTreeSet<_> = devices.iter().map(|key| public(key)).collect();
        move |_| Some(devices.clone())
    }

    fn route(record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: "so.tilo.notes".into(),
            cluster_id: ROOM.into(),
            record: record.into(),
        }
    }

    fn device(key: &SigningKey) -> [u8; 32] {
        key.verifying_key().to_bytes()
    }

    fn public(key: &SigningKey) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(device(key))
    }

    fn requests(changes: &Changes) -> Vec<MlsAccessRequestV1> {
        changes
            .outgoing
            .iter()
            .map(|Outgoing::Request { request, .. }| request.clone())
            .collect()
    }

    fn signed(browser: &SigningKey, mut request: MlsAccessRequestV1) -> MlsAccessRequestV1 {
        request.signature = collab_types::Signature::from_bytes(
            browser.sign(&request.signing_bytes().unwrap()).to_bytes(),
        );
        request
    }

    /// A native participant that created `route`'s channel.
    fn laptop(key: &SigningKey, route: &DocumentRoute) -> ChannelPeer {
        ChannelPeer::create_now(
            None,
            Channels::channel(route),
            Box::new(key.clone()),
            &VaultKey::new([9; 32]),
        )
        .unwrap()
    }

    /// Hand the page what `committer` queued, as the host would, with the
    /// room's governance admitting `vouch`.
    fn deliver(channels: &mut Channels, committer: &mut ChannelPeer, vouch: &Vouch<'_>) -> Changes {
        let mut changes = Changes::default();
        for frame in committer.outbox().to_vec() {
            changes.extend(channels.receive_frame(ROOM, frame, vouch, 0));
        }
        committer.published(&VaultKey::new([9; 32])).unwrap();
        changes
    }

    /// A request asked before the host admitted the session to its room
    /// went nowhere: the room's admission asks again at once, for that
    /// room's channels only, not after the request's backoff.
    #[test]
    fn a_rooms_admission_asks_again_at_once_for_its_channels() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-before-admission");
        let vouch = admitting(&[&laptop_key, &browser]);
        let mut channels = Channels::new(device(&browser), [4; 32]);
        let asked = requests(&channels.open(&note, &vouch, 0));
        assert_eq!(asked.len(), 1, "opening asks");
        assert!(
            requests(&channels.tick(1)).is_empty(),
            "the backoff holds the next ask back"
        );
        let workspace_id = note.workspace_id().unwrap();
        assert!(
            requests(&channels.ask_now(workspace_id.wrapping_add(1), 1)).is_empty(),
            "another room's admission asks nothing of this one"
        );
        let again = requests(&channels.ask_now(workspace_id, 1));
        assert_eq!(again, asked, "the room's admission asks the same again");
    }

    #[test]
    fn membership_survives_a_reload_without_asking_again() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-1");
        let mut laptop = laptop(&laptop_key, &note);
        let vouch = admitting(&[&laptop_key, &browser]);
        let mut channels = Channels::new(device(&browser), [4; 32]);
        let changes = channels.open(&note, &vouch, 0);
        let request = requests(&changes).pop().expect("an access request");
        deliver(&mut channels, &mut laptop, &vouch);
        laptop
            .learn_request(signed(&browser, request), &VaultKey::new([9; 32]))
            .unwrap();
        let allowed = BTreeSet::from([public(&laptop_key), public(&browser)]);
        laptop
            .reconcile_now(&allowed, &VaultKey::new([9; 32]))
            .unwrap();
        let (_, keys) = deliver(&mut channels, &mut laptop, &vouch)
            .keys
            .into_iter()
            .next()
            .expect("admitted");
        let saved = channels.export_if_changed().expect("state to save");
        assert!(
            channels.export_if_changed().is_none(),
            "nothing new to save"
        );

        let mut reloaded = Channels::new(device(&browser), [4; 32]);
        reloaded.restore(saved.clone());
        let changes = reloaded.open(&note, &vouch, 0);
        assert!(requests(&changes).is_empty(), "a reload does not ask again");
        let (_, restored) = changes.keys.into_iter().next().expect("keys are back");
        assert_eq!(restored.epoch(), keys.epoch());

        // Under another vault the stored state does not open, so the page
        // asks again.
        let mut elsewhere = Channels::new(device(&browser), [5; 32]);
        elsewhere.restore(saved);
        let changes = elsewhere.open(&note, &vouch, 0);
        assert!(changes.keys.is_empty());
        assert_eq!(requests(&changes).len(), 1);
    }

    /// A participant may take the request the old page queued before the
    /// reloaded page's. The reload joins from whichever admission names the
    /// key it holds.
    #[test]
    fn a_pending_reload_survives_the_old_request_being_admitted_first() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-pending-reload");
        let mut laptop = laptop(&laptop_key, &note);
        let vouch = admitting(&[&laptop_key, &browser]);
        let mut first = Channels::new(device(&browser), [4; 32]);
        let first_changes = first.open(&note, &vouch, 0);
        let saved = first.export_if_changed().expect("waiting state is stored");
        let pending = first.pending_requests();
        let mut reload = Channels::new(device(&browser), [4; 32]);
        reload.restore(saved);
        reload.restore_requests(pending);
        let reload_changes = reload.open(&note, &vouch, 1);
        let allowed = BTreeSet::from([public(&laptop_key), public(&browser)]);
        deliver(&mut reload, &mut laptop, &vouch);
        for changes in [first_changes, reload_changes] {
            for request in requests(&changes) {
                laptop
                    .learn_request(signed(&browser, request), &VaultKey::new([9; 32]))
                    .unwrap();
                laptop
                    .reconcile_now(&allowed, &VaultKey::new([9; 32]))
                    .unwrap();
                deliver(&mut reload, &mut laptop, &vouch);
            }
        }
        let keys = reload.routes[&Channels::channel(&note)]
            .peer
            .content_keys()
            .expect("the reload keeps its admitted channel keys");
        let artifact =
            collab_types::document_artifact_id(&note.app_id, &note.cluster_id, &note.record);
        let (metadata, sealed) = keys
            .seal_content(
                artifact,
                collab_types::ContentRetention::Checkpoint,
                b"before the reload",
            )
            .unwrap();
        assert_eq!(
            laptop
                .content_keys()
                .unwrap()
                .open_content(artifact, &metadata, &sealed)
                .unwrap()
                .to_vec(),
            b"before the reload",
        );
    }

    /// The page follows a commit whoever of the room's devices wrote it, and
    /// holds one adding a device the room's governance does not admit until
    /// it does.
    #[test]
    fn commits_of_any_participant_are_followed_and_checked() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let phone_key = SigningKey::from_bytes(&[5; 32]);
        let note = route("note-any-participant");
        let mut laptop = laptop(&laptop_key, &note);
        let vault = VaultKey::new([9; 32]);
        let before = admitting(&[&laptop_key, &browser]);
        let mut channels = Channels::new(device(&browser), [4; 32]);
        let request = requests(&channels.open(&note, &before, 0)).pop().unwrap();
        deliver(&mut channels, &mut laptop, &before);
        laptop
            .learn_request(signed(&browser, request), &vault)
            .unwrap();
        let everyone = BTreeSet::from([public(&laptop_key), public(&browser), public(&phone_key)]);
        laptop.reconcile_now(&everyone, &vault).unwrap();
        deliver(&mut channels, &mut laptop, &before);
        let joined = channels.routes[&Channels::channel(&note)].peer.access();
        assert_eq!(joined, Access::Ready { epoch: 1 });

        // The laptop admits the phone, which the room's governance does not
        // admit as the page holds it: the commit waits, and applies once it
        // does.
        let mut phone = ChannelPeer::waiting(
            None,
            Channels::channel(&note),
            Box::new(phone_key.clone()),
            &vault,
        )
        .unwrap();
        let ask = phone.access_request_now(1, false, &vault).unwrap().unwrap();
        laptop.learn_request(ask, &vault).unwrap();
        laptop.reconcile_now(&everyone, &vault).unwrap();
        deliver(&mut channels, &mut laptop, &before);
        let state = |channels: &Channels| channels.routes[&Channels::channel(&note)].peer.access();
        assert_eq!(state(&channels), Access::Ready { epoch: 1 });
        let after = admitting(&[&laptop_key, &browser, &phone_key]);
        let changes = channels.retry_early(&after, 0);
        assert_eq!(state(&channels), Access::Ready { epoch: 2 });
        assert_eq!(changes.keys.len(), 1);
    }
}
