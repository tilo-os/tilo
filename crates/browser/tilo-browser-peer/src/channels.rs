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
//! - it checks every commit against the room as the host vouches for it: a
//!   commit may add only a device whose certificate the host relayed (the
//!   room's admitted member devices), a member of the group or the room's
//!   governor, and must be signed by one of them.
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
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};

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
/// an open one whose signer's certificate has not arrived yet.
const MAX_UNOPENED_FRAMES: usize = 256;
/// Channels whose frames are kept before a route opens them.
const MAX_UNOPENED_CHANNELS: usize = 256;

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
    /// Frames refused for a signer whose certificate has not arrived yet,
    /// applied again when one does.
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
    /// The identity that certified this device.
    user: [u8; 32],
    vault: VaultKey,
    /// Certificates announced through the host, bound to the device the
    /// host authenticated as their sender: the room's admitted devices.
    certs: HashMap<[u8; 32], DeviceCert>,
    routes: HashMap<ChannelId, Route>,
    unopened: HashMap<ChannelId, Vec<MlsGovernanceFrame>>,
    /// Governors the page learned from the room itself.
    pinned: HashMap<String, Ed25519PublicKey>,
    /// The host, when its root certified it to own workspaces: the node
    /// that governs the rooms it hosts.
    host: Option<Ed25519PublicKey>,
    /// Devices this page's own root revoked: none of them takes part in a
    /// channel here.
    revocations: RevocationList,
    /// Restored channel state, waiting for its route to open.
    saved: HashMap<ChannelId, SavedMember>,
    saved_requests: HashMap<ChannelId, MlsAccessRequestV1>,
    dirty: bool,
}

impl Channels {
    /// Channel state for `device` of `user`, sealed at rest under `vault`.
    pub(crate) fn new(device: [u8; 32], user: [u8; 32], vault: [u8; 32]) -> Self {
        Self {
            device: Ed25519PublicKey::from_bytes(device),
            user,
            vault: VaultKey::new(vault),
            certs: HashMap::new(),
            routes: HashMap::new(),
            unopened: HashMap::new(),
            pinned: HashMap::new(),
            host: None,
            revocations: RevocationList::new(),
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

    /// Take the host as the governor of the rooms it hosts, when `host`
    /// (its verified certificate) owns workspaces: the node a person's root
    /// certified to host and govern their rooms. A plain host never governs.
    pub(crate) fn govern_by_host(&mut self, host: &DeviceCert) {
        self.host = (host.capabilities.can_own_workspaces
            && !host.capabilities.foreign
            && tilo_user_identity::verify_device_cert(host).is_ok())
        .then(|| Ed25519PublicKey::from_bytes(host.device_public_key));
    }

    /// `room`'s governor as this page knows it: a participant of every
    /// channel of the room.
    fn governor(&self, room: &str) -> Option<Ed25519PublicKey> {
        self.pinned
            .get(room)
            .copied()
            .or(self.host)
            .filter(|governor| !self.revocations.is_revoked(governor.as_bytes()))
    }

    /// Learn revocations this page's root signed: a revoked device takes
    /// part in no channel from then on. Entries of another root, or that do
    /// not verify, are ignored.
    pub(crate) fn note_revocations(
        &mut self,
        entries: impl IntoIterator<Item = RevocationEntry>,
        _now_ms: u64,
    ) -> Changes {
        for entry in entries {
            if entry.user_public_key == self.user {
                let _ = self.revocations.insert(entry);
            }
        }
        Changes::default()
    }

    /// Name `room`'s governor as the page learned it from the room itself.
    pub(crate) fn pin_governor(&mut self, room: &str, governor: [u8; 32], now_ms: u64) -> Changes {
        self.pinned
            .insert(room.to_owned(), Ed25519PublicKey::from_bytes(governor));
        self.retry_early(now_ms)
    }

    /// Remember a certificate a device sent through the host, and apply
    /// what waited for it.
    pub(crate) fn note_cert(&mut self, author: [u8; 32], cert: DeviceCert, now_ms: u64) -> Changes {
        if cert.device_public_key != author
            || tilo_user_identity::verify_device_cert(&cert).is_err()
        {
            return Changes::default();
        }
        self.certs.insert(author, cert);
        self.retry_early(now_ms)
    }

    /// Apply again the frames held for a signer this page could not vouch
    /// for yet.
    fn retry_early(&mut self, now_ms: u64) -> Changes {
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
                self.apply(channel, frame, now_ms, &mut changes);
            }
        }
        changes
    }

    /// The devices a commit to `route`'s channel may be signed by and add,
    /// as the page knows the room: every device whose certificate the host
    /// relayed and this page's root did not revoke, the group's members, the
    /// room's governor and this device. A device `frame` removes (and does
    /// not add again) counts as not backed: whether a Read grant still lets
    /// it read the record is the native participants' call.
    fn allowed(&self, route: &Route, frame: &MlsGovernanceFrame) -> BTreeSet<Ed25519PublicKey> {
        let mut allowed: BTreeSet<Ed25519PublicKey> = self
            .certs
            .keys()
            .filter(|device| !self.revocations.is_revoked(device))
            .map(|device| Ed25519PublicKey::from_bytes(*device))
            .collect();
        allowed.extend(route.peer.members());
        allowed.extend(self.governor(&route.document.cluster_id));
        allowed.insert(self.device);
        for removed in frame.removed() {
            if !frame.added().contains(removed) {
                allowed.remove(removed);
            }
        }
        allowed
    }

    /// Take part in `route`'s channel from now on.
    pub(crate) fn open(&mut self, route: &DocumentRoute, now_ms: u64) -> Changes {
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
            self.apply(channel, frame, now_ms, &mut changes);
        }
        self.after(channel, None, now_ms, &mut changes);
        changes
    }

    /// Apply one governance frame of `room` the host delivered.
    pub(crate) fn receive_frame(
        &mut self,
        _author: [u8; 32],
        room: &str,
        frame: MlsGovernanceFrame,
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
            self.apply(channel, frame, now_ms, &mut changes);
        }
        changes
    }

    fn apply(
        &mut self,
        channel: ChannelId,
        frame: MlsGovernanceFrame,
        now_ms: u64,
        changes: &mut Changes,
    ) {
        let Some(route) = self.routes.get(&channel) else {
            return;
        };
        let allowed = self.allowed(route, &frame);
        // A commit naming a device the page cannot vouch for yet waits for
        // that device's certificate.
        let unknown = std::iter::once(&frame.signed_by)
            .chain(frame.added())
            .any(|device| !allowed.contains(device));
        let Some(route) = self.routes.get_mut(&channel) else {
            return;
        };
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

    /// One line per certificate and route for diagnostics: which senders
    /// this page vouches for, and where each route's membership stands.
    pub(crate) fn describe(&self) -> String {
        let short = |key: &[u8]| hex::encode(key.get(..4).unwrap_or_default());
        let mut lines = Vec::new();
        for (room, governor) in &self.pinned {
            lines.push(format!(
                "room {room}: governed by {}",
                short(governor.as_bytes())
            ));
        }
        for device in self.certs.keys() {
            lines.push(format!(
                "cert {}: revoked {}",
                short(device),
                self.revocations.is_revoked(device)
            ));
        }
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
    use tilo_user_identity::DeviceCapabilities;

    const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    fn cert(root: &SigningKey, key: &SigningKey) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "device".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
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

    /// Hand the page what `committer` queued, as the host would.
    fn deliver(channels: &mut Channels, committer: &mut ChannelPeer) -> Changes {
        let mut changes = Changes::default();
        for frame in committer.outbox().to_vec() {
            changes.extend(channels.receive_frame([0; 32], ROOM, frame, 0));
        }
        committer.published(&VaultKey::new([9; 32])).unwrap();
        changes
    }

    #[test]
    fn membership_survives_a_reload_without_asking_again() {
        let user = SigningKey::from_bytes(&[1; 32]);
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-1");
        let mut laptop = laptop(&laptop_key, &note);
        let mut channels = Channels::new(device(&browser), device(&user), [4; 32]);
        channels.note_cert(device(&laptop_key), cert(&user, &laptop_key), 0);
        let changes = channels.open(&note, 0);
        let request = requests(&changes).pop().expect("an access request");
        deliver(&mut channels, &mut laptop);
        laptop
            .learn_request(signed(&browser, request), &VaultKey::new([9; 32]))
            .unwrap();
        let allowed = BTreeSet::from([public(&laptop_key), public(&browser)]);
        laptop
            .reconcile_now(&allowed, &VaultKey::new([9; 32]))
            .unwrap();
        let (_, keys) = deliver(&mut channels, &mut laptop)
            .keys
            .into_iter()
            .next()
            .expect("admitted");
        let saved = channels.export_if_changed().expect("state to save");
        assert!(
            channels.export_if_changed().is_none(),
            "nothing new to save"
        );

        let mut reloaded = Channels::new(device(&browser), device(&user), [4; 32]);
        reloaded.restore(saved.clone());
        let changes = reloaded.open(&note, 0);
        assert!(requests(&changes).is_empty(), "a reload does not ask again");
        let (_, restored) = changes.keys.into_iter().next().expect("keys are back");
        assert_eq!(restored.epoch(), keys.epoch());

        // Under another vault the stored state does not open, so the page
        // asks again.
        let mut elsewhere = Channels::new(device(&browser), device(&user), [5; 32]);
        elsewhere.restore(saved);
        let changes = elsewhere.open(&note, 0);
        assert!(changes.keys.is_empty());
        assert_eq!(requests(&changes).len(), 1);
    }

    /// A participant may take the request the old page queued before the
    /// reloaded page's. The reload joins from whichever admission names the
    /// key it holds.
    #[test]
    fn a_pending_reload_survives_the_old_request_being_admitted_first() {
        let user = SigningKey::from_bytes(&[1; 32]);
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-pending-reload");
        let mut laptop = laptop(&laptop_key, &note);
        let mut first = Channels::new(device(&browser), device(&user), [4; 32]);
        let first_changes = first.open(&note, 0);
        let saved = first.export_if_changed().expect("waiting state is stored");
        let pending = first.pending_requests();
        let mut reload = Channels::new(device(&browser), device(&user), [4; 32]);
        reload.note_cert(device(&laptop_key), cert(&user, &laptop_key), 0);
        reload.restore(saved);
        reload.restore_requests(pending);
        let reload_changes = reload.open(&note, 1);
        let allowed = BTreeSet::from([public(&laptop_key), public(&browser)]);
        deliver(&mut reload, &mut laptop);
        for changes in [first_changes, reload_changes] {
            for request in requests(&changes) {
                laptop
                    .learn_request(signed(&browser, request), &VaultKey::new([9; 32]))
                    .unwrap();
                laptop
                    .reconcile_now(&allowed, &VaultKey::new([9; 32]))
                    .unwrap();
                deliver(&mut reload, &mut laptop);
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
    /// holds one adding a device the host never vouched for until its
    /// certificate arrives.
    #[test]
    fn commits_of_any_participant_are_followed_and_checked() {
        let user = SigningKey::from_bytes(&[1; 32]);
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let phone_key = SigningKey::from_bytes(&[5; 32]);
        let note = route("note-any-participant");
        let mut laptop = laptop(&laptop_key, &note);
        let vault = VaultKey::new([9; 32]);
        let mut channels = Channels::new(device(&browser), device(&user), [4; 32]);
        channels.note_cert(device(&laptop_key), cert(&user, &laptop_key), 0);
        let request = requests(&channels.open(&note, 0)).pop().unwrap();
        deliver(&mut channels, &mut laptop);
        laptop
            .learn_request(signed(&browser, request), &vault)
            .unwrap();
        let everyone = BTreeSet::from([public(&laptop_key), public(&browser), public(&phone_key)]);
        laptop.reconcile_now(&everyone, &vault).unwrap();
        deliver(&mut channels, &mut laptop);
        let joined = channels.routes[&Channels::channel(&note)].peer.access();
        assert_eq!(joined, Access::Ready { epoch: 1 });

        // The laptop admits the phone, whose certificate the page has not
        // seen: the commit waits, and applies once it arrives.
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
        deliver(&mut channels, &mut laptop);
        let state = |channels: &Channels| channels.routes[&Channels::channel(&note)].peer.access();
        assert_eq!(state(&channels), Access::Ready { epoch: 1 });
        let changes = channels.note_cert(device(&phone_key), cert(&user, &phone_key), 0);
        assert_eq!(state(&channels), Access::Ready { epoch: 2 });
        assert_eq!(changes.keys.len(), 1);
    }
}
