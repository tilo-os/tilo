//! The channels of the person's own room this page stewards.
//!
//! [`crate::channels`] decides that this page stewards a channel; this
//! keeps the channel's [`ChannelSteward`], the same one a native device
//! keeps, with its records in memory and handed to the page's durable
//! store after every change. It signs with the page's device key, which
//! answers through a promise, so every change is awaited here, in the
//! session, rather than in the synchronous channel state.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use channel_crypto::{ChannelContentKeys, ChannelSteward, MemoryStewardStore, VaultKey};
use collab_types::{
    ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAckV1, MlsGovernanceFrameV1,
};
use tilo_relay_client::{DeviceSigner, SignFuture};

use crate::document::DocumentRoute;

/// The page's device key, shared with the session, as a steward's signer.
struct SharedSigner(Arc<dyn DeviceSigner>);

impl DeviceSigner for SharedSigner {
    fn device_public_key(&self) -> [u8; 32] {
        self.0.device_public_key()
    }

    fn sign(&self, message: Vec<u8>) -> SignFuture<'_> {
        self.0.sign(message)
    }
}

struct Stewarded {
    route: DocumentRoute,
    workspace_id: u64,
    steward: ChannelSteward<MemoryStewardStore>,
    /// The newest request of each device that asked.
    requests: BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>,
}

/// What a steward change leaves for the session to do.
#[derive(Default)]
pub(crate) struct Stewarding {
    /// The channel's keys, for this page's own documents on it.
    pub keys: Option<(DocumentRoute, ChannelContentKeys)>,
    /// The steward's records to keep, when they changed.
    pub saved: Option<(ChannelId, Vec<u8>)>,
    /// Frames to publish to the room, with its workspace and room id.
    pub frames: Vec<(u64, String, MlsGovernanceFrameV1)>,
}

/// Every channel this page stewards.
#[derive(Default)]
pub(crate) struct Stewards {
    channels: HashMap<ChannelId, Stewarded>,
}

impl Stewards {
    /// Steward `route`'s channel from `saved`, or as a new channel whose
    /// genesis goes out to the room.
    pub(crate) async fn take(
        &mut self,
        route: DocumentRoute,
        workspace_id: u64,
        saved: Option<&[u8]>,
        signer: &Arc<dyn DeviceSigner>,
        vault: &VaultKey,
    ) -> Result<Stewarding, String> {
        let channel = crate::channels::Channels::channel(&route);
        let store = match saved {
            Some(bytes) => MemoryStewardStore::import(bytes).map_err(|error| error.to_string())?,
            None => MemoryStewardStore::new(),
        };
        let steward = ChannelSteward::open(
            store,
            channel,
            Box::new(SharedSigner(Arc::clone(signer))),
            vault,
        )
        .await
        .map_err(|error| error.to_string())?;
        let keys = steward.content_keys().map_err(|error| error.to_string())?;
        self.channels.insert(
            channel,
            Stewarded {
                route: route.clone(),
                workspace_id,
                steward,
                requests: BTreeMap::new(),
            },
        );
        let mut done = self.redeliver(channel, true);
        done.keys = Some((route, keys));
        done.saved = self.saved(channel);
        Ok(done)
    }

    /// Let `channel` go: another device of this person stewards it.
    pub(crate) fn release(&mut self, channel: ChannelId) {
        self.channels.remove(&channel);
    }

    /// Admit `request`'s device when it is one of `allowed`, the person's
    /// devices, with every other device that asked since.
    pub(crate) async fn request(
        &mut self,
        channel: ChannelId,
        request: MlsAccessRequestV1,
        allowed: &BTreeSet<Ed25519PublicKey>,
        vault: &VaultKey,
    ) -> Result<Stewarding, String> {
        let Some(entry) = self.channels.get_mut(&channel) else {
            return Ok(Stewarding::default());
        };
        if request.verify().is_err() || !allowed.contains(&request.device) {
            return Ok(Stewarding::default());
        }
        let newer = entry
            .requests
            .get(&request.device)
            .is_none_or(|known| request.issued_at > known.issued_at);
        if !newer {
            return Ok(Stewarding::default());
        }
        entry.requests.insert(request.device, request);
        let mut allowed = allowed.clone();
        allowed.insert(entry.steward.device());
        let outcome = entry
            .steward
            .reconcile_async(&allowed, &entry.requests, vault)
            .await
            .map_err(|error| error.to_string())?;
        if !outcome.changed() {
            return Ok(Stewarding::default());
        }
        let keys = entry
            .steward
            .content_keys()
            .map_err(|error| error.to_string())?;
        let mut done = self.redeliver(channel, false);
        done.keys = self
            .channels
            .get(&channel)
            .map(|entry| (entry.route.clone(), keys));
        done.saved = self.saved(channel);
        Ok(done)
    }

    /// Take a member's acknowledgement: it needs the frames it covers no
    /// more.
    pub(crate) fn acknowledge(
        &mut self,
        channel: ChannelId,
        ack: &MlsGovernanceAckV1,
    ) -> Stewarding {
        let Some(entry) = self.channels.get_mut(&channel) else {
            return Stewarding::default();
        };
        match entry.steward.acknowledge(ack) {
            Ok(completed) if completed > 0 => Stewarding {
                saved: self.saved(channel),
                ..Stewarding::default()
            },
            _ => Stewarding::default(),
        }
    }

    /// The frames some member still lacks, to publish again; with the
    /// genesis too when `announce`, which tells the room who stewards it.
    pub(crate) fn redeliver(&self, channel: ChannelId, announce: bool) -> Stewarding {
        let Some(entry) = self.channels.get(&channel) else {
            return Stewarding::default();
        };
        let room = entry.route.cluster_id.clone();
        let mut frames: Vec<MlsGovernanceFrameV1> = Vec::new();
        if announce && let Some(genesis) = entry.steward.journal().frames().first() {
            frames.push(genesis.clone());
        }
        for frame in entry.steward.outbox_frames() {
            if !frames.contains(&frame) {
                frames.push(frame);
            }
        }
        // An admission goes out whole for the devices it adds and without
        // what they join from for everyone else; the host delivers the
        // whole copy to those devices alone.
        let frames = frames
            .into_iter()
            .flat_map(|frame| frame.into_deliveries().unwrap_or_default())
            .map(|frame| (entry.workspace_id, room.clone(), frame))
            .collect();
        Stewarding {
            frames,
            ..Stewarding::default()
        }
    }

    fn saved(&self, channel: ChannelId) -> Option<(ChannelId, Vec<u8>)> {
        let entry = self.channels.get(&channel)?;
        let bytes = entry.steward.store().export().ok()?;
        Some((channel, bytes))
    }
}
