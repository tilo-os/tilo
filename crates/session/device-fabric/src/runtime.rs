//! The session's live copy of the person's device fabric.
//! [`DeviceFabric`] is the model: what each of the person's devices last said about
//! itself. This is the state a running session keeps around it — this device's own
//! record, the endpoint each of the person's other devices is reached on, and the
//! actions it was handed. There is no transport and no async here: the session
//! hands this what arrived and asks it for the frames to publish, so the rules —
//! whose record a snapshot may carry, which actions are addressed here, which were
//! already acted on — are decided once, in one place.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use iroh::EndpointId;
use tilo_user_identity::{DeviceCert, RevocationEntry};

use crate::{
    ClipboardEntry, DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
    DisplayPlacement, FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricError, FabricStamp,
    OwnerSession, PersonalFabricAction, PersonalFabricActionEnvelope, WakeRoute,
};

/// Request ids remembered so a replayed action is refused. An envelope older than
/// [`crate::MAX_FABRIC_ACTION_AGE_MS`] is refused anyway, so the newest ids cover
/// every replay the wire lets through.
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
    /// The fabric this device kept, if it kept one, and this device's own record
    /// authored afresh: what a device says of itself holds only while it can say it, so
    /// every other device starts out offline ([`DeviceFabric::restored`]). The new
    /// record's revision is newer than anything in the checkpoint, so the person's
    /// other devices take it over what they kept from this device's last run.
    /// # Errors
    /// Why the checkpoint does not decode as a fabric, or the record is not a valid
    /// snapshot.
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

    /// One of the person's devices proved itself on its session with this one: from now
    /// on its frames are attributed to the device its certificate names, and actions to
    /// it leave the same way. The certificates are the ones the transport verified
    /// ([`tilo_network_actor::NetEvent::Sibling`]). A device an entry in `revocations`
    /// covers is no device of the person's any more, so what it said is dropped.
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

    /// Take the fabric one of the person's devices announced. A device announces its own
    /// record and nothing else: a frame carrying another device's state would forge it,
    /// so it is refused rather than merged. Returns whether the fabric moved.
    /// # Errors
    /// Why the frame is not one device's own valid record.
    pub fn snapshot(&mut self, peer: EndpointId, bytes: &[u8]) -> Result<bool, FabricError> {
        let incoming = DeviceFabric::decode_checkpoint(bytes)?;
        let mut records = incoming.snapshots().values();
        let Some(snapshot) = records.next().cloned() else {
            return Err(FabricError::ForeignAuthorship);
        };
        if records.next().is_some() {
            return Err(FabricError::ForeignAuthorship);
        }
        let Some(source) = self.device_behind(peer) else {
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

    /// Take one action a device addressed to this one. It counts only when the device
    /// that sent it is the device the envelope names, when it is addressed here, and when
    /// it is neither stale nor a replay: anything else is refused rather than acted on.
    /// Returns the action once, for the session's next pass to route.
    /// # Errors
    /// Why the frame is not an action addressed to this device, or why it was delivered
    /// already.
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
        let Some(source) = self.device_behind(peer) else {
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

    /// This device's own record as the frame that announces it: only what this device
    /// authors.
    /// # Errors
    /// Why this device's record cannot be encoded.
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

    /// Take what this device's clipboard now offers the person's other devices,
    /// replacing what it offered before: a copy removed from the history stops being
    /// shared. Returns whether this device's record changed.
    /// # Errors
    /// Why the offered copies do not make a valid record.
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

    /// Seal the Wi-Fi passphrase this device learned for `ssid` to itself and to every
    /// device of the person's in this fabric, as this device's own record in its next
    /// snapshot: the passphrase itself never leaves the sealed envelopes. A network this
    /// device already shared is replaced. Returns whether the record changed.
    /// # Errors
    /// Why the passphrase is not one to carry between the person's devices, or why the
    /// record does not make a valid snapshot.
    pub fn share_wifi_password(
        &mut self,
        ssid: &str,
        security: os_actions::WifiSecurity,
        password: &str,
    ) -> Result<bool, FabricError> {
        let Some((device, counter)) = self.local.as_ref().map(|local| {
            (
                local.device.clone(),
                local.snapshot.presence.stamp.counter.saturating_add(1),
            )
        }) else {
            return Ok(false);
        };
        let recipients = self
            .fabric
            .snapshots()
            .keys()
            .cloned()
            .chain([device.clone()])
            .collect::<BTreeSet<_>>();
        let credential = crate::secrets::seal_wifi_credential(
            ssid,
            security,
            password,
            &device,
            recipients,
            FabricStamp {
                counter,
                device: device.clone(),
            },
        )
        .map_err(|_| FabricError::InvalidWifiCredential)?;
        if let Some(local) = &mut self.local {
            local
                .snapshot
                .wifi
                .retain(|saved| saved.network_id != credential.network_id);
            local.snapshot.wifi.push(credential);
        }
        self.revise()
    }

    /// Take the wake routes this device's interfaces now offer, replacing what it offered
    /// before. Returns whether this device's record changed.
    /// # Errors
    /// Why the routes do not make a valid record.
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

    /// Say the person's session on this device to their other devices: whether it is
    /// locked, when they last locked or unlocked it themselves, and when they last used
    /// it. Returns whether this device's record changed.
    /// # Errors
    /// Why the record does not make a valid snapshot.
    pub fn set_owner_session(&mut self, session: OwnerSession) -> Result<bool, FabricError> {
        let Some(local) = &mut self.local else {
            return Ok(false);
        };
        if local.snapshot.presence.session == session {
            return Ok(false);
        }
        local.snapshot.presence.session = session;
        self.revise()
    }

    /// Take what this device publishes about disk unlock: its enrolments, one per slot,
    /// and the auto-approvals the person made here.
    /// # Errors
    /// Why the state does not make a valid record.
    pub fn set_unlock(
        &mut self,
        unlock: crate::unlock::UnlockFabricState,
    ) -> Result<bool, FabricError> {
        let Some(local) = &mut self.local else {
            return Ok(false);
        };
        if local.snapshot.unlock == unlock {
            return Ok(false);
        }
        local.snapshot.unlock = unlock;
        self.revise()
    }

    /// Take the displays this device shows now, replacing what it showed before: where
    /// each connector's output stands, how big the plane they make is, and what the
    /// hardware behind each one says. Returns whether this device's record changed.
    /// # Errors
    /// Why the placement and the facts do not make a valid record, or do not agree with
    /// each other.
    pub fn set_display(
        &mut self,
        rect: crate::DisplayRect,
        scale_milli: u16,
        connectors: Vec<crate::DisplayConnectorPlacement>,
        facts: Vec<crate::DisplayOutputObservation>,
    ) -> Result<bool, FabricError> {
        let Some(local) = &mut self.local else {
            return Ok(false);
        };
        let unchanged = local.snapshot.display.as_ref().is_some_and(|shown| {
            shown.rect == rect && shown.scale_milli == scale_milli && shown.outputs == connectors
        }) && self.fabric.display_outputs(&local.device) == Some(facts.as_slice());
        if unchanged {
            return Ok(false);
        }
        let device = local.device.clone();
        let counter = local.snapshot.presence.stamp.counter.saturating_add(1);
        let placement = DisplayPlacement {
            device: device.clone(),
            outputs: connectors,
            rect,
            scale_milli,
            stamp: FabricStamp {
                counter,
                device: device.clone(),
            },
        };
        // The record and what the hardware says are admitted together: a
        // record whose outputs the facts do not describe is never what this
        // device shows.
        let mut candidate = self.fabric.clone();
        let mut record = local.snapshot.clone();
        record.presence.stamp = placement.stamp.clone();
        record.presence.online = true;
        record.presence.last_seen_unix_ms = tilo_clock::unix_now_ms();
        record.display = Some(placement.clone());
        candidate.upsert(record.clone())?;
        candidate.observe_display_outputs(&device, &placement.stamp, facts)?;
        self.fabric = candidate;
        if let Some(local) = &mut self.local {
            local.snapshot = record;
        }
        self.changed = true;
        Ok(true)
    }

    /// What this device shows now, as its next announcement carries it: the
    /// revision of its own display, the outputs' places, and what their
    /// hardware says.
    pub fn local_display(
        &self,
    ) -> Option<(
        FabricStamp,
        Vec<crate::DisplayConnectorPlacement>,
        Vec<crate::DisplayOutputObservation>,
    )> {
        let local = self.local.as_ref()?;
        let display = local.snapshot.display.as_ref()?;
        Some((
            display.stamp.clone(),
            display.outputs.clone(),
            self.fabric.display_outputs(&local.device)?.to_vec(),
        ))
    }

    /// Take the outputs one of the person's devices says it shows, in the revision of its
    /// record it says it for. The device is the one behind `peer`: another device's
    /// state cannot be put under its name.
    /// # Errors
    /// Why `peer` names no device of the person's, or why the observation is not for its
    /// current revision.
    pub fn observe_display_connectors(
        &mut self,
        peer: EndpointId,
        stamp: &FabricStamp,
        outputs: Vec<crate::DisplayConnectorPlacement>,
    ) -> Result<bool, FabricError> {
        let Some(owner) = self.device_behind(peer) else {
            return Err(FabricError::UnknownDevice);
        };
        self.fabric
            .observe_display_connectors(&owner, stamp, outputs)
    }

    /// Take what the hardware behind one of the person's devices' outputs says, in the
    /// revision of its record it says it for.
    /// # Errors
    /// Why `peer` names no device of the person's, or why the facts are not for its
    /// current revision.
    pub fn observe_display_outputs(
        &mut self,
        peer: EndpointId,
        stamp: &FabricStamp,
        outputs: Vec<crate::DisplayOutputObservation>,
    ) -> Result<bool, FabricError> {
        let Some(owner) = self.device_behind(peer) else {
            return Err(FabricError::UnknownDevice);
        };
        self.fabric.observe_display_outputs(&owner, stamp, outputs)
    }

    /// The person's devices and the outputs each says it shows, as the
    /// shared arrangement lays them out.
    pub fn person_devices(&self) -> Vec<settings_store::person_displays::PersonDevice> {
        let Some(local) = self.local.as_ref() else {
            return Vec::new();
        };
        self.fabric.person_devices(&local.device)
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
    #[must_use]
    pub fn device_behind(&self, peer: EndpointId) -> Option<DeviceId> {
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
                // What this session does with the fabric: what the person copies here reaches
                // their other devices, and what they play here can go on playing on them. Input,
                // display and speaker groups are not offered, so they are not claimed.
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
    use crate::{DisplayConnectorPlacement, DisplayOutputObservation, DisplayRect};

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

    /// The frame a device that shows one screen announces itself with.
    fn announcement_with_screen(device: &DeviceId, label: &str, counter: u64) -> Vec<u8> {
        let mut record = own_record(device, label, counter).expect("a valid record");
        record.display = Some(DisplayPlacement {
            device: device.clone(),
            outputs: Vec::new(),
            rect: DisplayRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            scale_milli: 1000,
            stamp: FabricStamp {
                counter,
                device: device.clone(),
            },
        });
        let mut own = DeviceFabric::default();
        own.upsert(record).expect("one device's record");
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

    /// One output of this device at `x`, with the facts that describe it.
    fn output(connector: &str, x: i32) -> (DisplayConnectorPlacement, DisplayOutputObservation) {
        let output = settings_store::person_displays::DeviceOutput {
            id: format!("edid:{connector}"),
            name: format!("{connector} monitor"),
            builtin: false,
            x,
            y: 0,
            width: 1920,
            height: 1080,
            mode_width: 1920,
            mode_height: 1080,
            refresh_mhz: 60_000,
            scale_milli: 1000,
            connected: true,
            enabled: true,
        };
        (
            DisplayConnectorPlacement {
                connector: connector.to_owned(),
                label: "A display".to_owned(),
                rect: DisplayRect {
                    x,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                scale_milli: 1000,
            },
            DisplayOutputObservation {
                connector: connector.to_owned(),
                output,
            },
        )
    }

    #[test]
    fn what_this_device_shows_is_its_record_and_lists_its_outputs() {
        let mut runtime = runtime();
        let (left, left_facts) = output("eDP-1", 0);
        let (right, right_facts) = output("DP-2", 1920);
        assert!(
            runtime
                .set_display(
                    DisplayRect {
                        x: 0,
                        y: 0,
                        width: 3840,
                        height: 1080,
                    },
                    1000,
                    vec![left.clone(), right.clone()],
                    vec![left_facts.clone(), right_facts.clone()],
                )
                .expect("a valid display"),
            "two outputs are news for the person's other devices"
        );
        assert!(
            runtime.changed(),
            "its devices hear it on the next announcement"
        );
        let devices = runtime.person_devices();
        assert_eq!(devices.len(), 1, "this device only: {devices:?}");
        assert!(devices[0].local);
        assert_eq!(
            devices[0]
                .outputs
                .iter()
                .map(|output| output.name.as_str())
                .collect::<Vec<_>>(),
            ["eDP-1 monitor", "DP-2 monitor"],
            "each output says what hardware is behind it"
        );
        assert!(
            !runtime
                .set_display(
                    DisplayRect {
                        x: 0,
                        y: 0,
                        width: 3840,
                        height: 1080,
                    },
                    1000,
                    vec![left, right],
                    vec![left_facts, right_facts],
                )
                .expect("a valid display"),
            "what it showed before is not said again"
        );
    }

    #[test]
    fn another_device_s_screens_are_taken_only_as_it_says_them() {
        let mut runtime = runtime();
        let peer = endpoint(3);
        let other = device_at(peer);
        runtime.sibling(peer, &[cert(peer, "Studio")], &[]);
        runtime
            .snapshot(peer, &announcement_with_screen(&other, "Studio", 5))
            .expect("its own record");
        let stamp = FabricStamp {
            counter: 5,
            device: other.clone(),
        };
        let (screen, facts) = output("DP-1", 0);
        assert!(
            runtime
                .observe_display_connectors(peer, &stamp, vec![screen.clone()])
                .expect("its screen"),
            "where that device's screen stands is new"
        );
        assert!(
            runtime
                .observe_display_outputs(peer, &stamp, vec![facts.clone()])
                .expect("its screen's facts"),
            "what its hardware says is new"
        );
        let devices = runtime.person_devices();
        let studio = devices
            .iter()
            .find(|device| device.device == other.as_str())
            .expect("the other device is listed");
        assert!(!studio.local);
        assert_eq!(studio.outputs.len(), 1);
        assert_eq!(studio.outputs[0].name, "DP-1 monitor");
        assert!(
            runtime
                .observe_display_outputs(
                    peer,
                    &FabricStamp {
                        counter: 4,
                        device: other.clone(),
                    },
                    vec![facts],
                )
                .is_err(),
            "facts for a revision the device no longer says are refused"
        );
        assert!(
            !runtime
                .observe_display_connectors(peer, &stamp, vec![screen])
                .expect("its screen, as it stands"),
            "the place it already gave is not news"
        );
    }

    #[test]
    fn the_person_s_session_is_this_device_s_record_and_moves_only_when_it_changes() {
        let mut runtime = runtime();
        assert!(
            !runtime
                .set_owner_session(OwnerSession::default())
                .expect("a valid record"),
            "what it said before is not said again"
        );
        let session = OwnerSession {
            locked: true,
            changed_unix_ms: 1_000,
            active_unix_ms: 2_000,
        };
        assert!(
            runtime.set_owner_session(session).expect("a valid record"),
            "a lock of the person's is news for their other devices"
        );
        assert!(
            runtime.changed(),
            "their devices hear it on the next announcement"
        );
        assert_eq!(
            runtime.device_fabric().snapshots()[&local().0]
                .presence
                .session,
            session,
            "the record this device shares says it"
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

    /// This laptop's key, and its fabric id as the device that key names.
    fn keyed_local() -> (ed25519_dalek::SigningKey, (DeviceId, String)) {
        let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device =
            DeviceId::new(hex::encode(key.verifying_key().to_bytes())).expect("a device id");
        let local = (device, "This laptop".to_owned());
        (key, local)
    }

    #[test]
    fn a_shared_passphrase_is_sealed_in_the_fabric_and_opens_only_here() {
        let (key, local) = keyed_local();
        let mut runtime = FabricRuntime::restored(Some(local.clone()), None).expect("a runtime");
        assert!(
            runtime
                .share_wifi_password("Home", os_actions::WifiSecurity::Wpa2, "the-passphrase")
                .expect("a shareable passphrase"),
            "the device's record changed"
        );
        let fabric = runtime.device_fabric();
        let credential = fabric
            .merged_wifi()
            .into_values()
            .next()
            .expect("one credential");
        assert_eq!(credential.ssid, "Home");
        assert_eq!(credential.security, os_actions::WifiSecurity::Wpa2);
        assert_eq!(credential.stamp.device, local.0, "authored by this device");
        assert!(
            !credential
                .sealed_secret
                .windows("the-passphrase".len())
                .any(|window| window == b"the-passphrase"),
            "the passphrase itself never reaches the fabric"
        );
        assert_eq!(
            crate::secrets::open_wifi_credential(&credential, &key),
            Ok("the-passphrase".to_owned()),
            "this device reads what it shared"
        );
        let another = ed25519_dalek::SigningKey::from_bytes(&[10; 32]);
        assert!(
            crate::secrets::open_wifi_credential(&credential, &another).is_err(),
            "a key the passphrase was not sealed to cannot read it"
        );

        // Sharing the same network again replaces what this device said
        // before rather than keeping two passphrases for one network.
        runtime
            .share_wifi_password("Home", os_actions::WifiSecurity::Wpa2, "changed")
            .expect("a shareable passphrase");
        let fabric = runtime.device_fabric();
        let credential = fabric
            .merged_wifi()
            .into_values()
            .next()
            .expect("one credential");
        assert_eq!(
            crate::secrets::open_wifi_credential(&credential, &key),
            Ok("changed".to_owned())
        );
    }

    #[test]
    fn a_passphrase_no_device_can_hold_is_refused() {
        let (_, local) = keyed_local();
        let mut runtime = FabricRuntime::restored(Some(local), None).expect("a runtime");
        assert_eq!(
            runtime.share_wifi_password("Open", os_actions::WifiSecurity::Open, "no-passphrase"),
            Err(FabricError::InvalidWifiCredential),
            "an open network has no passphrase to share"
        );
        assert!(
            runtime.device_fabric().merged_wifi().is_empty(),
            "nothing was said"
        );
    }
}
