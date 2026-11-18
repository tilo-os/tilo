//! What the person's devices say live, as one session keeps it: which of their devices is on a
//! session with this one and on which endpoint, what each last said of itself there
//! ([`DevicePresence`]) and the copies each offers, what this device says of itself, and the
//! actions addressed to it. Nothing here is kept beyond the session: a device that went away is
//! gone until it says itself again, and what lasts of the person's devices is in their store
//! ([`super::store`]).
//!
//! There is no transport and no async here: the session hands this what arrived and asks it what to
//! say, so the rules (whose word a frame carries, which actions are addressed here, which were
//! acted on already) are decided once, in one place.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use iroh::EndpointId;
use tilo_user_identity::{DeviceCert, RevocationEntry};

use super::{
    ClipboardEntry, DeviceAbout, DevicePresence, FabricError, PersonDevice, PersonDevices,
    PersonalFabricAction, PersonalFabricActionEnvelope,
};
use crate::platform::fabric::DeviceId;

/// Request ids remembered so a replayed action is refused. An envelope older than
/// [`super::MAX_FABRIC_ACTION_AGE_MS`] is refused anyway, so the newest ids cover every replay the wire
/// lets through.
const REMEMBERED_ACTIONS: usize = 8192;

/// One of the person's other devices, as this session reaches it.
#[derive(Clone, Debug, Default)]
struct Sibling {
    /// Where its frames arrive and its actions leave: the device key that proved the certificate
    /// naming it.
    endpoint: Option<EndpointId>,
    /// Whether its session with this device is up.
    present: bool,
    /// What it last said of itself, once it said anything.
    presence: Option<DevicePresence>,
    /// The copies it offers.
    clipboard: Vec<ClipboardEntry>,
    /// When it last said anything.
    last_seen_unix_ms: u64,
    /// The name its certificate gives it, for while the person's store has not listed it yet.
    label: String,
}

/// The person's devices as this session hears them live.
#[derive(Debug)]
pub(crate) struct Live {
    /// This device.
    local: DeviceId,
    /// What this device says of itself.
    presence: DevicePresence,
    /// The copies this device offers.
    clipboard: Vec<ClipboardEntry>,
    /// The person's other devices heard on a session, by device id.
    siblings: BTreeMap<DeviceId, Sibling>,
    /// Request ids of the actions delivered already, oldest first.
    delivered: VecDeque<[u8; 16]>,
    /// The same ids, for the replay check itself.
    delivered_ids: BTreeSet<[u8; 16]>,
    /// Actions addressed to this device, waiting for the session's pass.
    actions: Vec<PersonalFabricActionEnvelope>,
}

impl Live {
    pub(crate) fn new(local: DeviceId) -> Self {
        Self {
            local,
            presence: DevicePresence::default(),
            clipboard: Vec::new(),
            siblings: BTreeMap::new(),
            delivered: VecDeque::new(),
            delivered_ids: BTreeSet::new(),
            actions: Vec::new(),
        }
    }

    pub(crate) fn local(&self) -> &DeviceId {
        &self.local
    }

    /// One of the person's devices proved itself on its session with this one: from now on its
    /// frames are attributed to the device its certificate names, and actions to it leave the same
    /// way. A device an entry in `revocations` covers is no device of the person's any more, and
    /// what it said is dropped. Only the person's own root speaks for their devices: another
    /// root's entry naming this one refuses nothing of it. Returns the device, when it is one.
    pub(crate) fn sibling(
        &mut self,
        peer: EndpointId,
        certs: &[DeviceCert],
        revocations: &[RevocationEntry],
    ) -> Option<DeviceId> {
        let cert = certs
            .iter()
            .find(|cert| cert.device_public_key == *peer.as_bytes())?;
        let device = DeviceId::new(hex::encode(cert.device_public_key)).ok()?;
        if revocations.iter().any(|entry| {
            (entry.user_public_key, entry.revoked_device_public_key)
                == (cert.user_public_key, cert.device_public_key)
        }) {
            self.siblings.remove(&device);
            return None;
        }
        let sibling = self.siblings.entry(device.clone()).or_default();
        tilo_user_identity::device_name(&cert.device_label)
            .unwrap_or_default()
            .clone_into(&mut sibling.label);
        sibling.endpoint = Some(peer);
        sibling.present = true;
        Some(device)
    }

    /// The session with one of the person's devices ended: what it said of itself holds no
    /// longer, so it is offline, keeping the endpoint an action to it leaves through until it is
    /// back.
    pub(crate) fn sibling_left(&mut self, peer: EndpointId) {
        for sibling in self.siblings.values_mut() {
            if sibling.endpoint == Some(peer) {
                sibling.present = false;
                sibling.presence = None;
                sibling.clipboard.clear();
            }
        }
    }

    /// Take what the device behind `peer` says of itself. Returns whether it changed.
    /// # Errors
    /// Why it is not what one of the person's devices could say.
    pub(crate) fn presence(
        &mut self,
        peer: EndpointId,
        presence: DevicePresence,
    ) -> Result<bool, FabricError> {
        presence.validate()?;
        let sibling = self.heard(peer)?;
        let changed = sibling.presence.as_ref() != Some(&presence);
        sibling.presence = Some(presence);
        Ok(changed)
    }

    /// Take the copies the device behind `peer` offers now, replacing what it offered before. A
    /// copy is that device's own: one it says another device made is refused.
    /// # Errors
    /// Why the copies are not that device's own valid ones.
    pub(crate) fn clipboard(
        &mut self,
        peer: EndpointId,
        entries: Vec<ClipboardEntry>,
    ) -> Result<bool, FabricError> {
        let Some(source) = self.device_behind(peer) else {
            return Err(FabricError::UnknownDevice);
        };
        if entries.len() > super::MAX_CLIPBOARD_ITEMS {
            return Err(FabricError::TooLarge);
        }
        for entry in &entries {
            entry.validate(&source)?;
        }
        let sibling = self.heard(peer)?;
        let changed = sibling.clipboard != entries;
        sibling.clipboard = entries;
        Ok(changed)
    }

    /// Take one action a device addressed to this one. It counts only when the device that sent it
    /// is the device the envelope names, when it is addressed here, and when it is neither stale
    /// nor a replay: anything else is refused rather than acted on. Returns the action once, for
    /// the session's next pass to route.
    /// # Errors
    /// Why the frame is not an action addressed to this device, or why it was delivered already.
    pub(crate) fn action(
        &mut self,
        peer: EndpointId,
        bytes: &[u8],
    ) -> Result<PersonalFabricAction, FabricError> {
        if bytes.len() > tilo_network_actor::fabric::MAX_FABRIC_ACTION_BYTES {
            return Err(FabricError::ActionTooLarge);
        }
        let (envelope, trailing) = postcard::take_from_bytes::<PersonalFabricActionEnvelope>(bytes)
            .map_err(|_| FabricError::InvalidAction)?;
        if !trailing.is_empty() {
            return Err(FabricError::InvalidAction);
        }
        let Some(source) = self.device_behind(peer) else {
            return Err(FabricError::UnknownDevice);
        };
        envelope.validate_at(&source, &self.local, tilo_clock::unix_now_ms())?;
        if self.delivered_ids.contains(&envelope.request_id) {
            // Acting on it again would repeat what the person asked for once.
            return Err(FabricError::InvalidAction);
        }
        self.remember(envelope.request_id);
        self.actions.push(envelope.clone());
        Ok(envelope.action)
    }

    /// The actions addressed to this device that arrived since the last call.
    pub(crate) fn take_actions(&mut self) -> Vec<PersonalFabricActionEnvelope> {
        std::mem::take(&mut self.actions)
    }

    /// What this device says of itself.
    pub(crate) fn own_presence(&self) -> &DevicePresence {
        &self.presence
    }

    /// Change what this device says of itself with `change`. Returns whether it changed.
    /// # Errors
    /// Why the result is not what a device could say of itself; nothing changes then.
    pub(crate) fn say(
        &mut self,
        change: impl FnOnce(&mut DevicePresence),
    ) -> Result<bool, FabricError> {
        let mut presence = self.presence.clone();
        change(&mut presence);
        presence.validate()?;
        if presence == self.presence {
            return Ok(false);
        }
        self.presence = presence;
        Ok(true)
    }

    /// The copies this device offers.
    pub(crate) fn own_clipboard(&self) -> &[ClipboardEntry] {
        &self.clipboard
    }

    /// Offer `entries`, replacing what this device offered before. Returns whether they changed.
    /// # Errors
    /// Why they are not this device's own valid copies; nothing changes then.
    pub(crate) fn offer(&mut self, entries: Vec<ClipboardEntry>) -> Result<bool, FabricError> {
        if entries.len() > super::MAX_CLIPBOARD_ITEMS {
            return Err(FabricError::TooLarge);
        }
        for entry in &entries {
            entry.validate(&self.local)?;
        }
        if entries == self.clipboard {
            return Ok(false);
        }
        self.clipboard = entries;
        Ok(true)
    }

    /// `kept`, the person's devices as their store holds them, with what each said live here put
    /// in: this device as it says itself, the others as they last said themselves on a session
    /// with this one. A device that was on a session here and that the store does not list yet is
    /// the person's all the same: it proved a certificate of theirs, under the name that names it.
    pub(crate) fn overlay(&self, kept: &mut PersonDevices) {
        let now = tilo_clock::unix_now_ms();
        for (device, sibling) in &self.siblings {
            if !kept.contains(device) {
                kept.insert(PersonDevice::new(
                    device.clone(),
                    DeviceAbout::named(sibling.label.clone()),
                ));
            }
        }
        let devices: Vec<_> = kept.devices().values().cloned().collect();
        for mut device in devices {
            if device.device == self.local {
                device.online = true;
                device.last_seen_unix_ms = now;
                device.presence = self.presence.clone();
                device.clipboard.clone_from(&self.clipboard);
            } else if let Some(sibling) = self.siblings.get(&device.device) {
                device.online = sibling.present && sibling.presence.is_some();
                device.last_seen_unix_ms = sibling.last_seen_unix_ms;
                device.presence = sibling.presence.clone().unwrap_or_default();
                device.clipboard.clone_from(&sibling.clipboard);
            }
            kept.insert(device);
        }
    }

    /// The person's devices this session has a live session with, and which said themselves on it.
    pub(crate) fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        self.siblings
            .iter()
            .filter(|(_, sibling)| sibling.present && sibling.presence.is_some())
            .map(|(device, _)| device.clone())
            .collect()
    }

    /// The person's other devices among `heard`, the endpoints heard on this device's local
    /// network: each that proved itself to this session, or that `known` (their store) holds.
    /// Anyone else heard there is no device of the person's, and is named nowhere.
    pub(crate) fn nearby_device_ids(
        &self,
        heard: &BTreeSet<EndpointId>,
        known: &PersonDevices,
    ) -> BTreeSet<DeviceId> {
        heard
            .iter()
            .filter_map(|peer| {
                self.device_behind(*peer).or_else(|| {
                    DeviceId::new(hex::encode(peer.as_bytes()))
                        .ok()
                        .filter(|device| known.contains(device))
                })
            })
            .filter(|device| device != &self.local)
            .collect()
    }

    /// Where an action to `device` leaves: the endpoint it proved to this session, once the two had
    /// one.
    pub(crate) fn endpoint_of(&self, device: &DeviceId) -> Option<EndpointId> {
        self.siblings
            .get(device)
            .and_then(|sibling| sibling.endpoint)
    }

    /// The device behind `peer`, from the certificate it proved.
    pub(crate) fn device_behind(&self, peer: EndpointId) -> Option<DeviceId> {
        self.siblings
            .iter()
            .find(|(_, sibling)| sibling.endpoint == Some(peer))
            .map(|(device, _)| device.clone())
    }

    /// The sibling behind `peer`, which just said something.
    fn heard(&mut self, peer: EndpointId) -> Result<&mut Sibling, FabricError> {
        let device = self.device_behind(peer).ok_or(FabricError::UnknownDevice)?;
        let sibling = self
            .siblings
            .get_mut(&device)
            .ok_or(FabricError::UnknownDevice)?;
        sibling.present = true;
        sibling.last_seen_unix_ms = tilo_clock::unix_now_ms();
        Ok(sibling)
    }

    /// Remember that `request_id` was delivered, forgetting the oldest once the window is full.
    fn remember(&mut self, request_id: [u8; 16]) {
        while self.delivered.len() >= REMEMBERED_ACTIONS {
            if let Some(oldest) = self.delivered.pop_front() {
                self.delivered_ids.remove(&oldest);
            }
        }
        self.delivered.push_back(request_id);
        self.delivered_ids.insert(request_id);
    }
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
