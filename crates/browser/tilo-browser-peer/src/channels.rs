//! The browser as a `channel-crypto` member device.
//!
//! Every document route of a room another device governs is an encrypted
//! channel whose steward is the room's governor, which keeps the MLS group.
//! The browser takes part exactly as a native member does, through the host:
//!
//! - it learns the room's governor once per room: from a pin the page holds,
//!   or from the first genesis frame of any of the room's channels whose
//!   sender the host authenticated and whose announced, valid certificate
//!   names a device of this browser's own identity that may govern
//!   workspaces. Every channel of the room is stewarded by that device;
//!   another device's genesis in the room is refused;
//! - it asks the governor for access to each channel it opens, with a
//!   request carrying a fresh key package, signed by its device key (which
//!   stays in page custody), re-asking with backoff until the Welcome
//!   arrives;
//! - it applies the steward's frames in chain order (Welcome with the key
//!   history, then commits), acknowledges them cumulatively, and hands the
//!   session the route's content keys whenever they change.
//!
//! Nothing is ever written in the clear: a route's writes are held until this
//! device holds the route's content keys. A replay that names no governor
//! does not mean the room has none, since the governor's genesis may still
//! be on its way to the host. The host carries every message as retained,
//! opaque governance and never holds a key. Member state and the room governors
//! are exported for the profile's durable store, so a reloaded page keeps
//! its access instead of asking again. Member state is sealed under a vault
//! key the store keeps apart from it: in the page, wrapped by a
//! non-extractable `WebCrypto` key.

use std::collections::HashMap;

use channel_crypto::{Access, ChannelContentKeys, ChannelMember, MemberEvent, VaultKey};
use collab_types::{
    ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAckV1, MlsGovernanceAction,
    MlsGovernanceFrameV1,
};
use tilo_user_identity::DeviceCert;

use crate::document::{ContentCipher, DocumentRoute};

/// First and longest wait before a waiting member asks again.
const REQUEST_FIRST_MS: u64 = 2_000;
const REQUEST_LONGEST_MS: u64 = 120_000;
/// Frames kept for a channel no route of this page has opened yet.
const MAX_UNOPENED_FRAMES: usize = 256;
/// Channels whose frames are kept before a route opens them.
const MAX_UNOPENED_CHANNELS: usize = 256;
/// Genesis frames kept while their sender's certificate is on its way.
const MAX_CANDIDATES: usize = 256;

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
    /// Ask the steward for access.
    Request {
        workspace_id: u64,
        room: String,
        request: MlsAccessRequestV1,
    },
    /// Acknowledge every frame applied so far.
    Ack {
        workspace_id: u64,
        room: String,
        ack: MlsGovernanceAckV1,
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

/// What this page knows of who governs a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Governance {
    /// No governor has shown up yet.
    Unknown,
    /// Every channel of the room is stewarded by this device.
    Governed(Ed25519PublicKey),
}

struct Route {
    document: DocumentRoute,
    workspace_id: u64,
    member: Option<ChannelMember>,
    /// Frames seen before the steward was known, with their senders.
    early: Vec<([u8; 32], MlsGovernanceFrameV1)>,
    /// The pending request, re-sent until the steward answers it.
    request: Option<MlsAccessRequestV1>,
    next_request_ms: u64,
    request_delay_ms: u64,
    installed_epoch: Option<u64>,
}

/// A member's state as [`Channels::export`] hands it to the durable store.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedMember {
    pub channel: ChannelId,
    pub steward: [u8; 32],
    pub sealed: Vec<u8>,
}

/// This page's channel state for the durable store. Member state in it is
/// sealed under the store's vault ([`crate::PeerStore::channel_vault`]),
/// which is kept apart from it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedChannels {
    pub governors: Vec<(String, [u8; 32])>,
    pub members: Vec<SavedMember>,
}

/// Channel state as pages stored it while the vault was kept inside it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LegacySavedChannels {
    pub vault: [u8; 32],
    pub governors: Vec<(String, [u8; 32])>,
    pub members: Vec<SavedMember>,
}

/// Every channel this page takes part in.
pub(crate) struct Channels {
    device: Ed25519PublicKey,
    /// The identity that certified this device; only its devices govern
    /// the rooms this page takes part in.
    user: [u8; 32],
    vault: VaultKey,
    /// Certificates announced through the host, bound to the device the
    /// host authenticated as their sender.
    certs: HashMap<[u8; 32], DeviceCert>,
    routes: HashMap<ChannelId, Route>,
    unopened: HashMap<ChannelId, Vec<([u8; 32], MlsGovernanceFrameV1)>>,
    rooms: HashMap<String, Governance>,
    /// Governors the page learned from the room itself, which no genesis
    /// frame can override.
    pinned: HashMap<String, Ed25519PublicKey>,
    /// The host, when its root certified it to own workspaces: a node
    /// governs the rooms it hosts unless a genesis frame names another of
    /// this person's devices.
    host: Option<Ed25519PublicKey>,
    /// Genesis frames whose sender's certificate has not arrived yet.
    candidates: Vec<(String, [u8; 32], MlsGovernanceFrameV1)>,
    /// Restored member state, waiting for its route to open.
    saved: HashMap<ChannelId, SavedMember>,
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
            rooms: HashMap::new(),
            pinned: HashMap::new(),
            host: None,
            candidates: Vec::new(),
            saved: HashMap::new(),
            dirty: false,
        }
    }

    /// Pick up where a previous page left off: the governors it learned and
    /// its members' state.
    pub(crate) fn restore(&mut self, saved: SavedChannels) {
        for (room, governor) in saved.governors {
            self.rooms.insert(
                room,
                Governance::Governed(Ed25519PublicKey::from_bytes(governor)),
            );
        }
        self.saved = saved
            .members
            .into_iter()
            .map(|member| (member.channel, member))
            .collect();
    }

    /// Pick up state stored with its vault beside it: its members are
    /// sealed again under this page's vault, and the next export stores
    /// them without the old one.
    pub(crate) fn restore_legacy(&mut self, legacy: LegacySavedChannels) {
        let old = VaultKey::new(legacy.vault);
        let members = legacy
            .members
            .into_iter()
            .filter_map(|saved| {
                let steward = Ed25519PublicKey::from_bytes(saved.steward);
                let member = ChannelMember::restore_in_memory(
                    saved.channel,
                    self.device,
                    steward,
                    &saved.sealed,
                    &old,
                )
                .ok()?;
                Some(SavedMember {
                    sealed: member.export_sealed(&self.vault).ok()?,
                    ..saved
                })
            })
            .collect();
        self.restore(SavedChannels {
            governors: legacy.governors,
            members,
        });
        self.dirty = true;
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
            if let Some(member) = &route.member
                && let Ok(sealed) = member.export_sealed(&self.vault)
            {
                members.insert(
                    *channel,
                    SavedMember {
                        channel: *channel,
                        steward: *member.steward().as_bytes(),
                        sealed,
                    },
                );
            }
        }
        let mut governors: Vec<(String, [u8; 32])> = self
            .rooms
            .iter()
            .filter_map(|(room, governance)| match governance {
                Governance::Governed(device) => Some((room.clone(), *device.as_bytes())),
                Governance::Unknown => None,
            })
            .collect();
        governors.sort();
        let mut members: Vec<SavedMember> = members.into_values().collect();
        members.sort_by_key(|member| member.channel.0);
        Some(SavedChannels { governors, members })
    }

    pub(crate) fn channel(route: &DocumentRoute) -> ChannelId {
        collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record)
    }

    /// Let the host govern the rooms no device of this person was seen
    /// governing, when `host` (its verified certificate) owns workspaces:
    /// the node a person's root certified to host and govern their rooms.
    /// A plain host never governs.
    pub(crate) fn govern_by_host(&mut self, host: &DeviceCert) {
        self.host = (host.capabilities.can_own_workspaces
            && !host.capabilities.foreign
            && tilo_user_identity::verify_device_cert(host).is_ok())
        .then(|| Ed25519PublicKey::from_bytes(host.device_public_key));
    }

    fn governance(&self, room: &str) -> Governance {
        if let Some(pinned) = self.pinned.get(room) {
            return Governance::Governed(*pinned);
        }
        match (self.rooms.get(room), self.host) {
            (Some(learned), _) => *learned,
            (None, Some(host)) => Governance::Governed(host),
            (None, None) => Governance::Unknown,
        }
    }

    /// Name `room`'s governor as the page learned it from the room itself.
    /// Channels already stewarded by another device start over under it.
    pub(crate) fn pin_governor(&mut self, room: &str, governor: [u8; 32], now_ms: u64) -> Changes {
        let governor = Ed25519PublicKey::from_bytes(governor);
        self.pinned.insert(room.to_owned(), governor);
        self.set_governor(room, governor, now_ms)
    }

    fn set_governor(&mut self, room: &str, governor: Ed25519PublicKey, now_ms: u64) -> Changes {
        self.rooms
            .insert(room.to_owned(), Governance::Governed(governor));
        self.dirty = true;
        let channels: Vec<ChannelId> = self
            .routes
            .iter()
            .filter(|(_, route)| route.document.cluster_id == room)
            .map(|(channel, _)| *channel)
            .collect();
        let mut changes = Changes::default();
        for channel in channels {
            if let Some(route) = self.routes.get_mut(&channel)
                && route
                    .member
                    .as_ref()
                    .is_some_and(|member| member.steward() != governor)
            {
                route.member = None;
                route.request = None;
                route.installed_epoch = None;
            }
            self.settle(channel, now_ms, &mut changes);
        }
        changes
    }

    /// Whether `author`'s verified certificate lets it govern a room this
    /// page takes part in: a device of this identity that owns workspaces.
    /// A host governs only when its root also granted it that (a node that
    /// hosts and governs a person's workspaces); a plain host never does.
    fn may_govern(&self, author: &[u8; 32]) -> bool {
        self.certs.get(author).is_some_and(|cert| {
            cert.user_public_key == self.user
                && cert.capabilities.can_own_workspaces
                && !cert.capabilities.foreign
        })
    }

    /// Learn `room`'s governor from a genesis frame `author` sent, when the
    /// room has none yet and the frame is the author's own, valid genesis.
    fn consider_genesis(
        &mut self,
        room: &str,
        author: [u8; 32],
        frame: &MlsGovernanceFrameV1,
        now_ms: u64,
    ) -> Changes {
        let genesis = matches!(frame.action, MlsGovernanceAction::Genesis)
            && frame.previous_frame_digest == [0; 32]
            && frame.signed_by.as_bytes() == &author
            && frame.verify_signature().is_ok();
        let learned = self.pinned.contains_key(room)
            || matches!(self.rooms.get(room), Some(Governance::Governed(_)));
        if !genesis || learned {
            return Changes::default();
        }
        if !self.certs.contains_key(&author) {
            if self.candidates.len() < MAX_CANDIDATES {
                self.candidates
                    .push((room.to_owned(), author, frame.clone()));
            }
            return Changes::default();
        }
        if !self.may_govern(&author) {
            return Changes::default();
        }
        self.set_governor(room, frame.signed_by, now_ms)
    }

    /// Remember a certificate a device sent through the host.
    pub(crate) fn note_cert(&mut self, author: [u8; 32], cert: DeviceCert, now_ms: u64) -> Changes {
        if cert.device_public_key != author
            || tilo_user_identity::verify_device_cert(&cert).is_err()
        {
            return Changes::default();
        }
        self.certs.insert(author, cert);
        // A genesis frame that arrived before its sender's certificate can
        // name its room's governor now.
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.candidates)
            .into_iter()
            .partition(|(_, sender, _)| *sender == author);
        self.candidates = waiting;
        let mut changes = Changes::default();
        for (room, sender, frame) in ready {
            changes.extend(self.consider_genesis(&room, sender, &frame, now_ms));
        }
        changes
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
        let early = self.unopened.remove(&channel).unwrap_or_default();
        self.routes.insert(
            channel,
            Route {
                document: route.clone(),
                workspace_id,
                member: None,
                early,
                request: None,
                next_request_ms: now_ms,
                request_delay_ms: REQUEST_FIRST_MS,
                installed_epoch: None,
            },
        );
        self.settle(channel, now_ms, &mut changes);
        changes
    }

    /// Apply one governance frame of `room` the host delivered from
    /// `author`.
    pub(crate) fn receive_frame(
        &mut self,
        author: [u8; 32],
        room: &str,
        frame: MlsGovernanceFrameV1,
        now_ms: u64,
    ) -> Changes {
        let mut changes = self.consider_genesis(room, author, &frame, now_ms);
        let channel = frame.channel;
        let Some(route) = self.routes.get_mut(&channel) else {
            if self.unopened.len() < MAX_UNOPENED_CHANNELS || self.unopened.contains_key(&channel) {
                let frames = self.unopened.entry(channel).or_default();
                if frames.len() < MAX_UNOPENED_FRAMES {
                    frames.push((author, frame));
                }
            }
            return changes;
        };
        match route.member.as_mut() {
            None => {
                if route.early.len() < MAX_UNOPENED_FRAMES {
                    route.early.push((author, frame));
                }
                self.settle(channel, now_ms, &mut changes);
            }
            Some(member) => {
                let events = member.receive(frame, &self.vault).unwrap_or_default();
                self.after_events(channel, &events, now_ms, &mut changes);
            }
        }
        changes
    }

    /// One line per room, certificate and route for diagnostics: whom a
    /// room follows, which senders may govern, and where each route's
    /// membership stands.
    pub(crate) fn describe(&self) -> String {
        let short = |key: &[u8]| hex::encode(key.get(..4).unwrap_or_default());
        let mut lines = Vec::new();
        for room in self.rooms.keys() {
            let governance = match self.governance(room) {
                Governance::Governed(device) => format!("governed by {}", short(device.as_bytes())),
                Governance::Unknown => "unknown".to_owned(),
            };
            lines.push(format!("room {room}: {governance}"));
        }
        for (device, cert) in &self.certs {
            lines.push(format!(
                "cert {}: may govern {}",
                short(device),
                self.may_govern(&cert.device_public_key)
            ));
        }
        for (room, sender, _) in &self.candidates {
            lines.push(format!("candidate genesis {room} from {}", short(sender)));
        }
        for (channel, route) in &self.routes {
            let access = route.member.as_ref().map_or_else(
                || "no member".to_owned(),
                |member| match member.access() {
                    Access::Ready { .. } => "ready".to_owned(),
                    other => format!("{other:?}"),
                },
            );
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

    /// Re-ask stewards whose answer is overdue.
    pub(crate) fn tick(&mut self, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        let channels: Vec<ChannelId> = self.routes.keys().copied().collect();
        for channel in channels {
            self.request_if_due(channel, now_ms, &mut changes);
        }
        changes
    }

    /// Take part in `channel` under its room's governor, once known, then
    /// replay what the governor sent so far.
    fn settle(&mut self, channel: ChannelId, now_ms: u64, changes: &mut Changes) {
        let Some(route) = self.routes.get(&channel) else {
            return;
        };
        if route.member.is_some() {
            return;
        }
        let Governance::Governed(steward) = self.governance(&route.document.cluster_id) else {
            return;
        };
        let restored = self
            .saved
            .remove(&channel)
            .filter(|saved| saved.steward == *steward.as_bytes())
            .and_then(|saved| {
                ChannelMember::restore_in_memory(
                    channel,
                    self.device,
                    steward,
                    &saved.sealed,
                    &self.vault,
                )
                .ok()
            });
        let member = match restored {
            Some(member) => member,
            None => match ChannelMember::in_memory(channel, self.device, steward) {
                Ok(member) => member,
                Err(_) => return,
            },
        };
        let Some(route) = self.routes.get_mut(&channel) else {
            return;
        };
        let mut member = member;
        let mut events = Vec::new();
        for (_, frame) in std::mem::take(&mut route.early) {
            if frame.signed_by == steward {
                events.extend(member.receive(frame, &self.vault).unwrap_or_default());
            }
        }
        route.member = Some(member);
        route.next_request_ms = now_ms;
        route.request_delay_ms = REQUEST_FIRST_MS;
        self.dirty = true;
        self.after_events(channel, &events, now_ms, changes);
    }

    fn after_events(
        &mut self,
        channel: ChannelId,
        events: &[MemberEvent],
        now_ms: u64,
        changes: &mut Changes,
    ) {
        let Some(route) = self.routes.get_mut(&channel) else {
            return;
        };
        let Some(member) = route.member.as_ref() else {
            return;
        };
        let moved = events
            .iter()
            .any(|event| !matches!(event, MemberEvent::Duplicate | MemberEvent::Buffered));
        if moved {
            self.dirty = true;
        }
        if moved && !member.journal().frames().is_empty() {
            changes.outgoing.push(Outgoing::Ack {
                workspace_id: route.workspace_id,
                room: route.document.cluster_id.clone(),
                ack: MlsGovernanceAckV1::unsigned(
                    channel,
                    self.device,
                    member.journal().head_digest(),
                ),
            });
        }
        // Ask again with a fresh key package: the request sent before was
        // answered with a Welcome this device cannot open, or by an
        // admission the steward has since replaced.
        if events.contains(&MemberEvent::Desynchronised) || events.contains(&MemberEvent::Removed) {
            route.request = None;
            route.next_request_ms = now_ms;
            route.request_delay_ms = REQUEST_FIRST_MS;
        }
        if let Some(keys) = member.content_keys()
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
        let Some(member) = route.member.as_mut() else {
            return;
        };
        if matches!(member.access(), Access::Ready { .. }) || now_ms < route.next_request_ms {
            return;
        }
        if route.request.is_none() {
            route.request = member
                .unsigned_access_request(now_ms / 1000, &self.vault)
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
        });
    }
}

impl Changes {
    pub(crate) fn extend(&mut self, other: Self) {
        self.keys.extend(other.keys);
        self.outgoing.extend(other.outgoing);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;
    use channel_crypto::ChannelSteward;
    use ed25519_dalek::{Signer as _, SigningKey};
    use std::collections::{BTreeMap, BTreeSet};
    use tilo_user_identity::DeviceCapabilities;

    const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    fn cert(root: &SigningKey, key: &SigningKey, capabilities: DeviceCapabilities) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "device".into(),
            capabilities,
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

    fn genesis(key: &SigningKey, route: &DocumentRoute) -> MlsGovernanceFrameV1 {
        MlsGovernanceFrameV1::signed(
            Channels::channel(route),
            [1; 32],
            1,
            [0; 32],
            0,
            MlsGovernanceAction::Genesis,
            key,
        )
        .unwrap()
    }

    fn device(key: &SigningKey) -> [u8; 32] {
        key.verifying_key().to_bytes()
    }

    fn steward_of(channels: &Channels, route: &DocumentRoute) -> Option<[u8; 32]> {
        channels.routes[&Channels::channel(route)]
            .member
            .as_ref()
            .map(|member| *member.steward().as_bytes())
    }

    fn requests(changes: &Changes) -> usize {
        changes
            .outgoing
            .iter()
            .filter(|outgoing| matches!(outgoing, Outgoing::Request { .. }))
            .count()
    }

    /// Only a device of this browser's identity that may govern names the
    /// room's governor, from a genesis on any of the room's channels; it
    /// then stewards every channel of the room, and another device's
    /// genesis does not take a channel over. A pin from the room wins.
    #[test]
    fn a_room_is_governed_by_one_governing_device_of_this_identity() {
        let user = SigningKey::from_bytes(&[1; 32]);
        let stranger_root = SigningKey::from_bytes(&[2; 32]);
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop = SigningKey::from_bytes(&[4; 32]);
        let desktop = SigningKey::from_bytes(&[5; 32]);
        let stranger = SigningKey::from_bytes(&[6; 32]);
        let server = SigningKey::from_bytes(&[7; 32]);
        let mut channels = Channels::new(device(&browser), device(&user), [4; 32]);
        let note = route("note-1");
        let other = route("note-2");
        assert_eq!(requests(&channels.open(&note, 0)), 0);
        for (key, root, capabilities) in [
            (&stranger, &stranger_root, DeviceCapabilities::primary()),
            (&server, &user, DeviceCapabilities::host()),
        ] {
            channels.note_cert(device(key), cert(root, key, capabilities), 0);
            let changes = channels.receive_frame(device(key), ROOM, genesis(key, &note), 0);
            assert_eq!(requests(&changes), 0);
            assert_eq!(steward_of(&channels, &note), None);
        }
        // A genesis whose sender's certificate arrives later counts then.
        let changes = channels.receive_frame(device(&laptop), ROOM, genesis(&laptop, &other), 0);
        assert_eq!(requests(&changes), 0);
        let changes = channels.note_cert(
            device(&laptop),
            cert(&user, &laptop, DeviceCapabilities::primary()),
            0,
        );
        assert_eq!(requests(&changes), 1, "asks the governor for the open note");
        assert_eq!(steward_of(&channels, &note), Some(device(&laptop)));

        channels.note_cert(
            device(&desktop),
            cert(&user, &desktop, DeviceCapabilities::primary()),
            0,
        );
        channels.receive_frame(device(&desktop), ROOM, genesis(&desktop, &note), 0);
        assert_eq!(steward_of(&channels, &note), Some(device(&laptop)));

        let changes = channels.pin_governor(ROOM, device(&desktop), 0);
        assert_eq!(requests(&changes), 1);
        assert_eq!(steward_of(&channels, &note), Some(device(&desktop)));
    }

    /// A node the person's root certified to host and govern their
    /// workspaces governs; a foreign device that owns workspaces does not.
    #[test]
    fn a_node_that_hosts_and_governs_names_the_governor() {
        let user = SigningKey::from_bytes(&[1; 32]);
        let browser = SigningKey::from_bytes(&[3; 32]);
        let foreign = SigningKey::from_bytes(&[4; 32]);
        let server = SigningKey::from_bytes(&[5; 32]);
        let mut channels = Channels::new(device(&browser), device(&user), [4; 32]);
        let note = route("note-1");
        channels.open(&note, 0);
        let mut foreign_capabilities = DeviceCapabilities::primary();
        foreign_capabilities.foreign = true;
        channels.note_cert(
            device(&foreign),
            cert(&user, &foreign, foreign_capabilities),
            0,
        );
        channels.receive_frame(device(&foreign), ROOM, genesis(&foreign, &note), 0);
        assert_eq!(steward_of(&channels, &note), None);

        let capabilities = tilo_user_identity::cloud_enrollment::host_capabilities();
        channels.note_cert(device(&server), cert(&user, &server, capabilities), 0);
        let changes = channels.receive_frame(device(&server), ROOM, genesis(&server, &note), 0);
        assert_eq!(requests(&changes), 1);
        assert_eq!(steward_of(&channels, &note), Some(device(&server)));
    }

    /// A page on a node asks the node for a room's channels without waiting
    /// for a genesis, also in another person's room; a plain host governs
    /// nothing, and a device of this person that governs the room still
    /// takes its channels over.
    #[test]
    fn a_node_host_governs_the_rooms_it_hosts() {
        let owner = SigningKey::from_bytes(&[1; 32]);
        let guest = SigningKey::from_bytes(&[2; 32]);
        let browser = SigningKey::from_bytes(&[3; 32]);
        let server = SigningKey::from_bytes(&[5; 32]);
        let laptop = SigningKey::from_bytes(&[6; 32]);
        let note = route("note-1");
        let node_cert = cert(
            &owner,
            &server,
            tilo_user_identity::cloud_enrollment::host_capabilities(),
        );

        let mut plain = Channels::new(device(&browser), device(&guest), [4; 32]);
        plain.govern_by_host(&cert(&owner, &server, DeviceCapabilities::host()));
        plain.open(&note, 0);
        assert_eq!(steward_of(&plain, &note), None);

        let mut invited = Channels::new(device(&browser), device(&guest), [4; 32]);
        invited.govern_by_host(&node_cert);
        assert_eq!(requests(&invited.open(&note, 0)), 1);
        assert_eq!(steward_of(&invited, &note), Some(device(&server)));

        let mut own = Channels::new(device(&browser), device(&owner), [4; 32]);
        own.govern_by_host(&node_cert);
        own.open(&note, 0);
        own.note_cert(
            device(&laptop),
            cert(&owner, &laptop, DeviceCapabilities::primary()),
            0,
        );
        let changes = own.receive_frame(device(&laptop), ROOM, genesis(&laptop, &note), 0);
        assert_eq!(requests(&changes), 1);
        assert_eq!(steward_of(&own, &note), Some(device(&laptop)));
    }

    /// A room whose governor has not shown up is not ungoverned: a genesis
    /// that arrives later still names it, and the route asks for access.
    #[test]
    fn a_late_genesis_still_names_the_governor_of_an_open_route() {
        let user = SigningKey::from_bytes(&[1; 32]);
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop = SigningKey::from_bytes(&[4; 32]);
        let mut channels = Channels::new(device(&browser), device(&user), [4; 32]);
        let note = route("note-1");
        let changes = channels.open(&note, 0);
        assert!(changes.keys.is_empty() && changes.outgoing.is_empty());
        assert_eq!(steward_of(&channels, &note), None);

        channels.note_cert(
            device(&laptop),
            cert(&user, &laptop, DeviceCapabilities::primary()),
            0,
        );
        let changes = channels.receive_frame(device(&laptop), ROOM, genesis(&laptop, &note), 0);
        assert_eq!(requests(&changes), 1);
        assert!(changes.keys.is_empty());
        assert_eq!(steward_of(&channels, &note), Some(device(&laptop)));
    }

    /// A reloaded page restores its membership: the keys it was admitted
    /// with are back without a new request.
    #[test]
    fn membership_survives_a_reload_without_asking_again() {
        let user = SigningKey::from_bytes(&[1; 32]);
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-1");
        let channel = Channels::channel(&note);
        let vault = VaultKey::new([9; 32]);
        let dir = tempfile::tempdir().unwrap();
        let mut steward =
            ChannelSteward::open_or_create(dir.path(), channel, laptop.clone(), &vault).unwrap();
        let mut channels = Channels::new(device(&browser), device(&user), [4; 32]);
        channels.open(&note, 0);
        channels.note_cert(
            device(&laptop),
            cert(&user, &laptop, DeviceCapabilities::primary()),
            0,
        );
        let changes = channels.receive_frame(
            device(&laptop),
            ROOM,
            steward.journal().frames()[0].clone(),
            0,
        );
        let Some(Outgoing::Request { mut request, .. }) = changes
            .outgoing
            .into_iter()
            .find(|outgoing| matches!(outgoing, Outgoing::Request { .. }))
        else {
            panic!("an access request");
        };
        request.signature = collab_types::Signature::from_bytes(
            browser.sign(&request.signing_bytes().unwrap()).to_bytes(),
        );
        let allowed = BTreeSet::from([
            Ed25519PublicKey::from_bytes(device(&laptop)),
            Ed25519PublicKey::from_bytes(device(&browser)),
        ]);
        let pending = BTreeMap::from([(request.device, request)]);
        steward.reconcile(&allowed, &pending, &vault).unwrap();
        let mut keys = None;
        for outgoing in steward.outbox() {
            for frame in outgoing.frames {
                let changes = channels.receive_frame(device(&laptop), ROOM, frame, 0);
                keys = keys.or_else(|| changes.keys.into_iter().next());
            }
        }
        let (_, keys) = keys.expect("admitted");
        let saved = channels.export_if_changed().expect("state to save");
        assert!(
            channels.export_if_changed().is_none(),
            "nothing new to save"
        );

        let mut reloaded = Channels::new(device(&browser), device(&user), [4; 32]);
        reloaded.restore(saved.clone());
        let changes = reloaded.open(&note, 0);
        assert_eq!(requests(&changes), 0, "a reload does not ask again");
        let (_, restored) = changes.keys.into_iter().next().expect("keys are back");
        assert_eq!(restored.epoch(), keys.epoch());

        // Under another vault the stored state does not open, so the page
        // asks again.
        let mut elsewhere = Channels::new(device(&browser), device(&user), [5; 32]);
        elsewhere.restore(saved.clone());
        let changes = elsewhere.open(&note, 0);
        assert!(changes.keys.is_empty());
        assert_eq!(requests(&changes), 1);

        // State stored with its vault beside it moves under the page's vault
        // and is stored without the old one.
        let mut migrated = Channels::new(device(&browser), device(&user), [5; 32]);
        migrated.restore_legacy(LegacySavedChannels {
            vault: [4; 32],
            governors: saved.governors,
            members: saved.members,
        });
        let resaved = migrated
            .export_if_changed()
            .expect("migrated state to save");
        let mut after = Channels::new(device(&browser), device(&user), [5; 32]);
        after.restore(resaved);
        let changes = after.open(&note, 0);
        assert_eq!(requests(&changes), 0, "a migrated page does not ask again");
        let (_, restored) = changes.keys.into_iter().next().expect("keys are back");
        assert_eq!(restored.epoch(), keys.epoch());
    }
}
