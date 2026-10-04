//! The encrypted channels of the rooms a node governs.
//!
//! A relay whose certificate lets it own workspaces is the person's node:
//! the device that governs their rooms. Members never write a room's
//! records in the clear; each record's channel is an MLS group, and the
//! room's governor stewards it. So a node also stewards the channels of the
//! rooms it governs: it admits exactly the room's governed member devices
//! that ask for a channel, removes the ones governance removed, and sends
//! each of them the channel's governance frames. A relay with a plain host
//! certificate governs rooms without stewarding them; members accept a
//! steward only from a device certified to own workspaces.
//!
//! Channel state is sealed on disk under a key derived from the node's
//! device key, which already lives beside it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use channel_crypto::{ChannelSteward, VaultKey};
use collab_types::{ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAckV1};
use ed25519_dalek::SigningKey;
use tilo_peer_state::NeighbourMsg;

/// Channels one room keeps a steward for.
const MAX_CHANNELS: usize = 4096;

/// The stewards of one governed room's channels.
pub(super) struct Stewardship {
    room: String,
    dir: PathBuf,
    key: SigningKey,
    vault: VaultKey,
    stewards: HashMap<ChannelId, ChannelSteward>,
    /// Each device's newest request per channel, kept until it is a member.
    requests: HashMap<ChannelId, BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>>,
    delivery: u64,
}

impl Stewardship {
    /// The stewardship of `room`, kept under `dir`, when `key`'s certificate
    /// lets it own workspaces; channels already stewarded there are opened
    /// as members ask for them.
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
            stewards: HashMap::new(),
            requests: HashMap::new(),
            delivery: 0,
        }
    }

    fn device(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.key.verifying_key().to_bytes())
    }

    fn steward<'a>(
        stewards: &'a mut HashMap<ChannelId, ChannelSteward>,
        dir: &std::path::Path,
        key: &SigningKey,
        vault: &VaultKey,
        channel: ChannelId,
    ) -> Result<&'a mut ChannelSteward, String> {
        if !stewards.contains_key(&channel) {
            if stewards.len() >= MAX_CHANNELS {
                return Err("the room stewards too many channels".into());
            }
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
            let steward = ChannelSteward::open_or_create(dir, channel, key.clone(), vault)
                .map_err(|error| error.to_string())?;
            stewards.insert(channel, steward);
        }
        stewards
            .get_mut(&channel)
            .ok_or_else(|| "steward missing".into())
    }

    /// Take in `request`, which the authenticated device `author` sent:
    /// admit it when governance lets it read the room. Returns the
    /// governance messages to publish.
    pub(super) fn request(
        &mut self,
        author: [u8; 32],
        request: MlsAccessRequestV1,
        allowed: &BTreeSet<Ed25519PublicKey>,
    ) -> Result<Vec<NeighbourMsg>, String> {
        if request.device.as_bytes() != &author || request.verify().is_err() {
            return Err("access request is not the sender's own".into());
        }
        if !allowed.contains(&request.device) {
            return Ok(Vec::new());
        }
        let channel = request.channel;
        let pending = self.requests.entry(channel).or_default();
        if pending
            .get(&request.device)
            .is_some_and(|known| known.issued_at > request.issued_at)
        {
            return Ok(Vec::new());
        }
        pending.insert(request.device, request);
        self.reconcile(channel, allowed)
    }

    /// Record a member's acknowledgement of the frames it applied.
    pub(super) fn acknowledge(&mut self, author: [u8; 32], ack: &MlsGovernanceAckV1) {
        if ack.device.as_bytes() != &author {
            return;
        }
        if let Some(steward) = self.stewards.get_mut(&ack.channel)
            && let Err(error) = steward.acknowledge(ack)
        {
            tracing::debug!(room = %self.room, %error, "channel acknowledgement refused");
        }
    }

    /// Move every open channel to `allowed`, after governance changed.
    pub(super) fn reconcile_all(
        &mut self,
        allowed: &BTreeSet<Ed25519PublicKey>,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let channels: Vec<ChannelId> = self.stewards.keys().copied().collect();
        let mut messages = Vec::new();
        for channel in channels {
            messages.extend(self.reconcile(channel, allowed)?);
        }
        Ok(messages)
    }

    fn reconcile(
        &mut self,
        channel: ChannelId,
        allowed: &BTreeSet<Ed25519PublicKey>,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let own = self.device();
        let mut allowed = allowed.clone();
        allowed.insert(own);
        let pending = self.requests.remove(&channel).unwrap_or_default();
        let steward = Self::steward(
            &mut self.stewards,
            &self.dir,
            &self.key,
            &self.vault,
            channel,
        )?;
        steward
            .reconcile(&allowed, &pending, &self.vault)
            .map_err(|error| error.to_string())?;
        let members = steward.members();
        let frames: Vec<_> = steward
            .outbox()
            .into_iter()
            .flat_map(|outgoing| outgoing.frames)
            .collect();
        // A request stays until its device is a member: one that could not
        // be admitted yet is retried on the next change.
        let waiting: BTreeMap<_, _> = pending
            .into_iter()
            .filter(|(device, _)| !members.contains(device))
            .collect();
        if !waiting.is_empty() {
            self.requests.insert(channel, waiting);
        }
        let mut seen = BTreeSet::new();
        let mut messages = Vec::new();
        for frame in frames {
            let Ok(digest) = frame.digest() else { continue };
            if !seen.insert(digest) {
                continue;
            }
            self.delivery = self.delivery.saturating_add(1);
            messages.push(NeighbourMsg::MlsGovernance {
                room: self.room.clone(),
                frame: Box::new(frame),
                delivery: self.delivery,
            });
        }
        Ok(messages)
    }
}
