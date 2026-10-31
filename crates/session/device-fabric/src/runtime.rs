//! The session's live copy of the person's device fabric.
//!
//! [`DeviceFabric`] is the model: what each of the person's devices last said
//! about itself. This is the state a running session keeps around it — this
//! device's own record, which endpoint each of the person's other devices is
//! reached on, and the actions it was handed — so a service on the fabric
//! ([`crate::link::FabricLink`]) can be answered and the transport can be
//! told what to send next.
//!
//! There is no transport and no async here: the session hands this what
//! arrived, first a device proving itself ([`FabricRuntime::sibling`]) and
//! then whatever that device sent, and asks it for the frames to publish. So
//! the rules below — whose record a snapshot may carry, which actions are
//! addressed here, which were already acted on — are decided once, in one
//! place, whatever carries them.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use iroh::EndpointId;
use tilo_user_identity::{DeviceCert, RevocationEntry};

use crate::{
    ClipboardEntry, DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
    FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricError, FabricStamp, OwnerSession,
    PersonalFabricAction, PersonalFabricActionEnvelope, WakeRoute,
};

/// Request ids remembered so a replayed action is refused.
///
/// An envelope older than [`crate::MAX_FABRIC_ACTION_AGE_MS`] is refused
/// anyway, so remembering the newest actions' ids covers every replay the
/// wire lets through.
const REMEMBERED_ACTIONS: usize = 8192;

/// One of the person's other devices, as this session reaches it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sibling {
    /// Where its frames arrive and its actions leave: the device key that
    /// proved the certificate naming it.
    endpoint: EndpointId,
    /// Whether its session with this device is up.
    present: bool,
}

/// This device's own record in the fabric.
#[derive(Clone, Debug)]
struct Local {
    device: DeviceId,
    label: String,
    snapshot: DeviceFabricSnapshot,
}

/// The person's device fabric as one session keeps it.
#[derive(Clone, Debug)]
pub struct FabricRuntime {
    /// What the person's devices last said of themselves.
    fabric: DeviceFabric,
    /// This device's own record; `None` on a session that is no certified
    /// device of the person's and so takes part in no fabric.
    local: Option<Local>,
    /// The person's other devices heard on a session, by fabric id.
    siblings: BTreeMap<DeviceId, Sibling>,
    /// Request ids of the actions delivered already, oldest first.
    delivered: VecDeque<[u8; 16]>,
    /// The same ids, for the replay check itself.
    delivered_ids: BTreeSet<[u8; 16]>,
    /// Actions addressed to this device, waiting for the session's pass.
    actions: Vec<PersonalFabricActionEnvelope>,
    /// Whether this device's own record moved since it was last announced.
    changed: bool,
}

impl FabricRuntime {
    /// The fabric this device kept, if it kept one, and this device's own
    /// record authored afresh: what a device says of itself holds only while
    /// it can say it, so every other device starts out offline
    /// ([`DeviceFabric::restored`]).
    ///
    /// The new record carries a revision newer than anything in the
    /// checkpoint, so the person's other devices take it over the one they
    /// kept from this device's last run.
    ///
    /// # Errors
    ///
    /// Returns why the checkpoint does not decode as a fabric, or why this
    /// device's own record is not a valid snapshot.
    pub fn restored(
        local: Option<(DeviceId, String)>,
        checkpoint: Option<&[u8]>,
    ) -> Result<Self, FabricError> {
        let kept = match checkpoint {
            Some(bytes) => DeviceFabric::decode_checkpoint(bytes)?,
            None => DeviceFabric::default(),
        };
        let local_device = local.as_ref().map(|(device, _)| device.clone());
        let mut runtime = Self {
            fabric: kept.restored(local_device.as_ref()),
            local: None,
            siblings: BTreeMap::new(),
            delivered: VecDeque::new(),
            delivered_ids: BTreeSet::new(),
            actions: Vec::new(),
            changed: false,
        };
        if let Some((device, label)) = local {
            let counter = runtime.fabric.max_counter().saturating_add(1);
            runtime.local = Some(Local {
                device: device.clone(),
                label: label.clone(),
                snapshot: own_record(&device, &label, counter)?,
            });
            runtime.revise()?;
        }
        Ok(runtime)
    }

    /// One of the person's devices proved itself on its session with this
    /// one: from now on its frames are attributed to the device its
    /// certificate names, and actions to it leave the same way.
    ///
    /// The certificates are the ones the transport verified against this
    /// device's roots ([`tilo_network_actor::NetEvent::Sibling`]); this only
    /// reads who they name. A device an entry in `revocations` covers is no
    /// device of the person's any more, so what it said is dropped rather
    /// than kept as theirs.
    pub fn sibling(
        &mut self,
        peer: EndpointId,
        certs: &[DeviceCert],
        revocations: &[RevocationEntry],
    ) {
        let Some(cert) = certs
            .iter()
            .find(|cert| cert.device_public_key == *peer.as_bytes())
        else {
            return;
        };
        let Ok(device) = DeviceId::new(hex::encode(cert.device_public_key)) else {
            return;
        };
        if revocations
            .iter()
            .any(|entry| entry.revoked_device_public_key == cert.device_public_key)
        {
            self.fabric.remove_device(&device);
            self.siblings.remove(&device);
            return;
        }
        self.siblings.insert(
            device,
            Sibling {
                endpoint: peer,
                present: true,
            },
        );
    }

    /// The session with one of the person's devices ended. What it said of
    /// itself holds no longer, so it shows offline — keeping its place, what
    /// it published, and the endpoint an action to it leaves through, until
    /// its next snapshot puts it back.
    pub fn sibling_left(&mut self, peer: EndpointId) {
        let devices: Vec<DeviceId> = self
            .siblings
            .iter()
            .filter(|(_, sibling)| sibling.endpoint == peer)
            .map(|(device, _)| device.clone())
            .collect();
        for device in devices {
            if let Some(sibling) = self.siblings.get_mut(&device) {
                sibling.present = false;
            }
            self.fabric.mark_departed(&device);
        }
    }

    /// Take the fabric one of the person's devices announced.
    ///
    /// A device announces its own record and nothing else: a frame carrying
    /// another device's state would forge it, so it is refused rather than
    /// merged. What a device sends is the encoded aggregate
    /// ([`DeviceFabric::encode_checkpoint`]) carrying its one record, as the
    /// person's other devices publish it. Returns whether the fabric moved.
    ///
    /// # Errors
    ///
    /// Returns why the frame is not one device's own valid record.
    pub fn snapshot(&mut self, peer: EndpointId, bytes: &[u8]) -> Result<bool, FabricError> {
        let incoming = DeviceFabric::decode_checkpoint(bytes)?;
        let mut records = incoming.snapshots().values();
        let Some(snapshot) = records.next().cloned() else {
            return Err(FabricError::ForeignAuthorship);
        };
        if records.next().is_some() {
            return Err(FabricError::ForeignAuthorship);
        }
        let Some(source) = self.device_on(peer) else {
            return Err(FabricError::UnknownDevice);
        };
        if snapshot.owner != source {
            return Err(FabricError::ForeignAuthorship);
        }
        let merged = self.fabric.upsert(snapshot)?;
        if let Some(sibling) = self.siblings.get_mut(&source) {
            sibling.present = true;
        }
        Ok(merged)
    }

    /// Take one action a device addressed to this one.
    ///
    /// The action counts only when the device that sent it over the
    /// transport is the device the envelope names, when it is addressed to
    /// this device, and when it is neither stale nor a replay of one already
    /// delivered: anything else is refused here rather than acted on.
    /// Returns the action once, for the session's next pass to route.
    ///
    /// # Errors
    ///
    /// Returns why the frame is not an action addressed to this device, or
    /// why it was delivered already.
    pub fn action(
        &mut self,
        peer: EndpointId,
        bytes: &[u8],
    ) -> Result<Option<PersonalFabricAction>, FabricError> {
        if bytes.len() > tilo_network_actor::fabric::MAX_FABRIC_ACTION_BYTES {
            return Err(FabricError::ActionTooLarge);
        }
        let (envelope, trailing) = postcard::take_from_bytes::<PersonalFabricActionEnvelope>(bytes)
            .map_err(|_| FabricError::InvalidAction)?;
        if !trailing.is_empty() {
            return Err(FabricError::InvalidAction);
        }
        let Some(source) = self.device_on(peer) else {
            return Err(FabricError::UnknownDevice);
        };
        let Some(local) = self.local.as_ref().map(|local| local.device.clone()) else {
            return Err(FabricError::UnknownDevice);
        };
        envelope.validate_at(&source, &local, tilo_clock::unix_now_ms())?;
        if self.delivered_ids.contains(&envelope.request_id) {
            // Acting on it again would repeat what the person asked for once.
            return Err(FabricError::InvalidAction);
        }
        self.remember(envelope.request_id);
        self.actions.push(envelope.clone());
        Ok(Some(envelope.action))
    }

    /// This device's own record as the frame that announces it: only what
    /// this device authors, never another one's state.
    ///
    /// # Errors
    ///
    /// Returns why this device's record cannot be encoded.
    pub fn own_snapshot(&self) -> Result<Vec<u8>, FabricError> {
        let Some(local) = &self.local else {
            return Err(FabricError::UnknownDevice);
        };
        let mut own = DeviceFabric::default();
        own.upsert(local.snapshot.clone())?;
        own.encode_checkpoint()
    }

    /// Whether this device's own record moved since it was last announced,
    /// clearing that: the session announces a change once, and nothing while
    /// nothing moved.
    pub fn changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// The actions addressed to this device that arrived since the last call.
    pub fn take_actions(&mut self) -> Vec<PersonalFabricActionEnvelope> {
        std::mem::take(&mut self.actions)
    }

    /// Take what this device's clipboard now offers the person's other
    /// devices, replacing what it offered before: a copy removed from the
    /// history stops being shared. Returns whether this device's record
    /// changed.
    ///
    /// # Errors
    ///
    /// Returns why the offered copies do not make a valid record.
    pub fn share_clipboard(&mut self, entries: Vec<ClipboardEntry>) -> Result<bool, FabricError> {
        let Some(local) = &mut self.local else {
            return Ok(false);
        };
        if local.snapshot.clipboard == entries {
            return Ok(false);
        }
        local.snapshot.clipboard = entries;
        self.revise()
    }

    /// Take the wake routes this device's interfaces now offer, replacing
    /// what it offered before: an interface that went away stops being a way
    /// to wake this machine. Returns whether this device's record changed.
    ///
    /// # Errors
    ///
    /// Returns why the routes do not make a valid record.
    pub fn set_wake_routes(&mut self, routes: Vec<WakeRoute>) -> Result<bool, FabricError> {
        let Some(local) = &mut self.local else {
            return Ok(false);
        };
        if local.snapshot.presence.wake_routes == routes {
            return Ok(false);
        }
        local.snapshot.presence.wake_routes = routes;
        self.revise()
    }

    /// The fabric as this device last converged it.
    pub fn device_fabric(&self) -> &DeviceFabric {
        &self.fabric
    }

    /// The person's devices this session has a live session with, and whose
    /// own record says they are here.
    pub fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        self.siblings
            .iter()
            .filter(|(_, sibling)| sibling.present)
            .filter(|(device, _)| {
                self.fabric
                    .snapshots()
                    .get(*device)
                    .is_some_and(|snapshot| snapshot.presence.online)
            })
            .map(|(device, _)| device.clone())
            .collect()
    }

    /// This device's fabric id and label, once it is a certified device of
    /// the person's.
    pub fn local_identity(&self) -> Option<(DeviceId, String)> {
        self.local
            .as_ref()
            .map(|local| (local.device.clone(), local.label.clone()))
    }

    /// Where an action to `device` leaves: the endpoint it proved to this
    /// session, once the two had one.
    pub fn endpoint_of(&self, device: &DeviceId) -> Option<EndpointId> {
        self.siblings.get(device).map(|sibling| sibling.endpoint)
    }

    /// The device behind `peer`, from the certificate it proved.
    fn device_on(&self, peer: EndpointId) -> Option<DeviceId> {
        self.siblings
            .iter()
            .find(|(_, sibling)| sibling.endpoint == peer)
            .map(|(device, _)| device.clone())
    }

    /// Remember that `request_id` was delivered, forgetting the oldest once
    /// the window is full.
    fn remember(&mut self, request_id: [u8; 16]) {
        while self.delivered.len() >= REMEMBERED_ACTIONS {
            if let Some(oldest) = self.delivered.pop_front() {
                self.delivered_ids.remove(&oldest);
            }
        }
        self.delivered.push_back(request_id);
        self.delivered_ids.insert(request_id);
    }

    /// Move this device's own record to its next revision and merge it into
    /// the fabric, which is where a service reads it and what the next
    /// announcement carries.
    fn revise(&mut self) -> Result<bool, FabricError> {
        let Some(local) = &mut self.local else {
            return Ok(false);
        };
        let counter = local.snapshot.presence.stamp.counter.saturating_add(1);
        local.snapshot.presence.stamp = FabricStamp {
            counter,
            device: local.device.clone(),
        };
        local.snapshot.presence.online = true;
        local.snapshot.presence.last_seen_unix_ms = tilo_clock::unix_now_ms();
        let snapshot = local.snapshot.clone();
        self.fabric.upsert(snapshot)?;
        self.changed = true;
        Ok(true)
    }
}

/// A device's record as it first publishes itself: what it is called, what it
/// does with the fabric, and the revision it says it at.
fn own_record(
    device: &DeviceId,
    label: &str,
    counter: u64,
) -> Result<DeviceFabricSnapshot, FabricError> {
    let snapshot = DeviceFabricSnapshot {
        schema_version: FABRIC_SCHEMA_VERSION,
        owner: device.clone(),
        presence: DevicePresence {
            device: device.clone(),
            label: label.to_owned(),
            kind: DeviceKind::Desktop,
            capabilities: FabricCapabilities {
                // What this session does with the fabric: what the person
                // copies here reaches their other devices, and what they
                // play here can go on playing on them. Input, display,
                // speaker groups and the rest are not offered, so they are
                // not claimed.
                clipboard: true,
                audio_output: true,
                video_output: true,
                ..FabricCapabilities::default()
            },
            online: true,
            last_seen_unix_ms: tilo_clock::unix_now_ms(),
            wake_routes: Vec::new(),
            session: OwnerSession::default(),
            stamp: FabricStamp {
                counter,
                device: device.clone(),
            },
        },
        media: Vec::new(),
        clipboard: Vec::new(),
        settings: Vec::new(),
        wifi: Vec::new(),
        display: None,
        speakers: crate::speaker_group::SpeakerFabricState::default(),
        unlock: crate::unlock::UnlockFabricState::default(),
    };
    snapshot.validate()?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    /// A certificate naming the device at `peer`. The transport verified it
    /// before this runtime saw it, so what is read here is only whose key it
    /// names.
    fn cert(peer: EndpointId, label: &str) -> DeviceCert {
        DeviceCert {
            user_public_key: [7; 32],
            device_public_key: *peer.as_bytes(),
            device_label: label.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 0,
            expires_at_ns: None,
            signature: [0; 64],
        }
    }

    /// The fabric id the device at `peer` publishes under.
    fn device_at(peer: EndpointId) -> DeviceId {
        DeviceId::new(hex::encode(peer.as_bytes())).expect("a device id")
    }

    fn local() -> (DeviceId, String) {
        (
            DeviceId::new("this-laptop").expect("a device id"),
            "This laptop".to_owned(),
        )
    }

    fn runtime() -> FabricRuntime {
        FabricRuntime::restored(Some(local()), None).expect("a runtime")
    }

    /// The frame `device` announces itself with, carrying only its record.
    fn announcement(device: &DeviceId, label: &str, counter: u64) -> Vec<u8> {
        let mut own = DeviceFabric::default();
        own.upsert(own_record(device, label, counter).expect("a valid record"))
            .expect("one device's record");
        own.encode_checkpoint().expect("an encoded checkpoint")
    }

    fn envelope(
        source: &DeviceId,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> PersonalFabricActionEnvelope {
        PersonalFabricActionEnvelope {
            request_id: crate::random::nonzero::<16>(),
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: tilo_clock::unix_now_ms(),
            action,
        }
    }

    fn bytes(envelope: &PersonalFabricActionEnvelope) -> Vec<u8> {
        postcard::to_allocvec(envelope).expect("an encoded envelope")
    }

    #[test]
    fn a_snapshot_from_another_device_merges_and_marks_it_online() {
        let mut runtime = runtime();
        let peer = endpoint(2);
        let other = device_at(peer);
        runtime.sibling(peer, &[cert(peer, "Studio")], &[]);
        assert!(
            runtime.online_device_ids().is_empty(),
            "a device whose record has not arrived yet is not one to send work to"
        );

        let merged = runtime
            .snapshot(peer, &announcement(&other, "Studio", 5))
            .expect("its own record");
        assert!(merged, "the first record of that device is new");
        assert!(
            runtime.online_device_ids().contains(&other),
            "it says it is here, and its session is up"
        );
        assert_eq!(
            runtime.device_fabric().snapshots()[&other].presence.label,
            "Studio"
        );
        assert!(
            runtime.device_fabric().snapshots().contains_key(&local().0),
            "this device's own record is in the fabric it shares"
        );
    }

    #[test]
    fn sibling_left_marks_it_departed() {
        let mut runtime = runtime();
        let peer = endpoint(2);
        let other = device_at(peer);
        runtime.sibling(peer, &[cert(peer, "Studio")], &[]);
        runtime
            .snapshot(peer, &announcement(&other, "Studio", 5))
            .expect("its own record");
        assert!(runtime.online_device_ids().contains(&other));

        runtime.sibling_left(peer);
        assert!(
            !runtime.device_fabric().snapshots()[&other].presence.online,
            "a device whose session ended is no longer here"
        );
        assert!(runtime.online_device_ids().is_empty());
        assert!(
            runtime.endpoint_of(&other).is_some(),
            "where an action to it leaves is kept until its session is back"
        );
    }

    #[test]
    fn an_action_addressed_to_another_device_is_refused() {
        let mut runtime = runtime();
        let peer = endpoint(2);
        let other = device_at(peer);
        let third = DeviceId::new("their-phone").expect("a device id");
        runtime.sibling(peer, &[cert(peer, "Studio")], &[]);
        let action = envelope(
            &other,
            &third,
            PersonalFabricAction::WakeDevice {
                target: third.clone(),
            },
        );

        assert!(
            runtime.action(peer, &bytes(&action)).is_err(),
            "an action for another device is not this device's to act on"
        );
        assert!(runtime.take_actions().is_empty());
    }

    #[test]
    fn a_valid_action_is_delivered_once_and_its_replay_refused() {
        let mut runtime = runtime();
        let peer = endpoint(2);
        let other = device_at(peer);
        let third = DeviceId::new("their-phone").expect("a device id");
        runtime.sibling(peer, &[cert(peer, "Studio")], &[]);
        let action = envelope(
            &other,
            &local().0,
            PersonalFabricAction::WakeDevice {
                target: third.clone(),
            },
        );

        assert_eq!(
            runtime
                .action(peer, &bytes(&action))
                .expect("an action addressed here"),
            Some(PersonalFabricAction::WakeDevice {
                target: third.clone()
            })
        );
        let delivered = runtime.take_actions();
        assert_eq!(delivered.len(), 1, "it is handed on exactly once");
        assert_eq!(delivered[0].source, other);

        assert!(
            runtime.action(peer, &bytes(&action)).is_err(),
            "the same request is not acted on twice"
        );
        assert!(runtime.take_actions().is_empty());

        let stale = PersonalFabricActionEnvelope {
            issued_unix_ms: tilo_clock::unix_now_ms()
                .saturating_sub(crate::MAX_FABRIC_ACTION_AGE_MS.saturating_add(1)),
            ..envelope(
                &other,
                &local().0,
                PersonalFabricAction::WakeDevice { target: third },
            )
        };
        assert!(
            runtime.action(peer, &bytes(&stale)).is_err(),
            "an envelope that waited too long is not acted on"
        );
    }

    #[test]
    fn the_checkpoint_round_trips_a_merged_fabric() {
        let mut runtime = runtime();
        let peer = endpoint(2);
        let other = device_at(peer);
        runtime.sibling(peer, &[cert(peer, "Studio")], &[]);
        runtime
            .snapshot(peer, &announcement(&other, "Studio", 5))
            .expect("its own record");
        let checkpoint = runtime
            .device_fabric()
            .encode_checkpoint()
            .expect("an encoded checkpoint");

        let mut restarted = FabricRuntime::restored(Some(local()), Some(&checkpoint))
            .expect("the checkpoint this device wrote");
        let fabric = restarted.device_fabric();
        assert!(
            fabric.snapshots().contains_key(&other),
            "it kept the other device"
        );
        assert!(
            fabric.snapshots().contains_key(&local().0),
            "and this device"
        );
        assert!(
            !fabric.snapshots()[&other].presence.online,
            "a device that has not said anything since is not assumed here"
        );
        assert!(
            fabric.snapshots()[&local().0].presence.online,
            "this device is here"
        );
        assert!(
            fabric.max_counter() > 5,
            "its own record is newer than anything it kept"
        );
        assert!(restarted.changed(), "a restarted device announces itself");
        assert!(
            restarted.online_device_ids().is_empty(),
            "no session is up yet, so there is no device to send work to"
        );
    }
}
