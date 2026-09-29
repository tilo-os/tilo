//! The browser as a `channel-crypto` member device.
//!
//! Every document route of a room another device governs is an encrypted
//! channel whose steward (that device) keeps the MLS group. The browser
//! takes part exactly as a native member does, through the host:
//!
//! - it learns the steward from the channel's genesis frame, accepted only
//!   from the device the host authenticated as the frame's sender, holding a
//!   valid certificate it announced through the host;
//! - it asks for access with a request carrying a fresh key package, signed
//!   by its device key (which stays in page custody), re-asking with backoff
//!   until the steward's Welcome arrives;
//! - it applies the steward's frames in chain order (Welcome with the key
//!   history, then commits), acknowledges them cumulatively, and hands the
//!   session the route's content keys whenever they change.
//!
//! The host carries every message as retained, opaque governance and never
//! holds a key. Member state lives in memory: a new page session starts
//! waiting and asks again, and the steward re-admits it.

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

/// A route's content keys as the session's cipher.
pub(crate) struct KeysCipher(pub(crate) ChannelContentKeys);

impl ContentCipher for KeysCipher {
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

struct Route {
    route: DocumentRoute,
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

/// Every channel this page takes part in.
pub(crate) struct Channels {
    device: Ed25519PublicKey,
    vault: VaultKey,
    /// Certificates announced through the host, bound to the device the
    /// host authenticated as their sender.
    certs: HashMap<[u8; 32], DeviceCert>,
    routes: HashMap<ChannelId, Route>,
    unopened: HashMap<ChannelId, Vec<([u8; 32], MlsGovernanceFrameV1)>>,
}

impl Channels {
    pub(crate) fn new(device: [u8; 32]) -> Self {
        Self {
            device: Ed25519PublicKey::from_bytes(device),
            // Member state never leaves memory; the key only satisfies the
            // member's persistence seam.
            vault: VaultKey::new(rand::random()),
            certs: HashMap::new(),
            routes: HashMap::new(),
            unopened: HashMap::new(),
        }
    }

    fn channel(route: &DocumentRoute) -> ChannelId {
        collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record)
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
        // name the steward now.
        let channels: Vec<ChannelId> = self
            .routes
            .iter()
            .filter(|(_, route)| route.member.is_none() && !route.early.is_empty())
            .map(|(channel, _)| *channel)
            .collect();
        let mut changes = Changes::default();
        for channel in channels {
            self.settle(channel, now_ms, &mut changes);
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
                route: route.clone(),
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

    /// Apply one governance frame the host delivered from `author`.
    pub(crate) fn receive_frame(
        &mut self,
        author: [u8; 32],
        frame: MlsGovernanceFrameV1,
        now_ms: u64,
    ) -> Changes {
        let mut changes = Changes::default();
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

    /// Whether `route` must not be written in the clear: a steward created
    /// its channel encrypted, so until this page holds the keys its edits
    /// wait.
    pub(crate) fn gated(&self, route: &DocumentRoute) -> bool {
        self.routes
            .get(&Self::channel(route))
            .is_some_and(|route| route.member.is_some())
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

    /// Name the steward from a genesis frame its authenticated, certified
    /// sender issued, then replay what arrived so far.
    fn settle(&mut self, channel: ChannelId, now_ms: u64, changes: &mut Changes) {
        let Some(route) = self.routes.get_mut(&channel) else {
            return;
        };
        if route.member.is_some() {
            return;
        }
        let steward = route.early.iter().find_map(|(author, frame)| {
            let certified = self
                .certs
                .get(author)
                .is_some_and(|cert| !cert.capabilities.can_act_as_host);
            (matches!(frame.action, MlsGovernanceAction::Genesis)
                && frame.previous_frame_digest == [0; 32]
                && frame.signed_by.as_bytes() == author
                && frame.verify_signature().is_ok()
                && certified)
                .then_some(frame.signed_by)
        });
        let Some(steward) = steward else {
            return;
        };
        let Ok(mut member) = ChannelMember::in_memory(channel, self.device, steward) else {
            return;
        };
        let mut events = Vec::new();
        for (_, frame) in std::mem::take(&mut route.early) {
            if frame.signed_by == steward {
                events.extend(member.receive(frame, &self.vault).unwrap_or_default());
            }
        }
        route.member = Some(member);
        route.next_request_ms = now_ms;
        route.request_delay_ms = REQUEST_FIRST_MS;
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
        if events
            .iter()
            .any(|event| !matches!(event, MemberEvent::Duplicate | MemberEvent::Buffered))
            && !member.journal().frames().is_empty()
        {
            changes.outgoing.push(Outgoing::Ack {
                workspace_id: route.workspace_id,
                room: route.route.cluster_id.clone(),
                ack: MlsGovernanceAckV1::unsigned(
                    channel,
                    self.device,
                    member.journal().head_digest(),
                ),
            });
        }
        if events.contains(&MemberEvent::Desynchronised) {
            route.request = None;
            route.next_request_ms = now_ms;
            route.request_delay_ms = REQUEST_FIRST_MS;
        }
        if let Some(keys) = member.content_keys()
            && route.installed_epoch != Some(keys.epoch())
        {
            route.installed_epoch = Some(keys.epoch());
            changes.keys.push((route.route.clone(), keys));
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
            room: route.route.cluster_id.clone(),
            request,
        });
    }
}
