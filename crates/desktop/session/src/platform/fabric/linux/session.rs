//! This session's way onto the person's certified devices: what lasts of each is in the person's
//! own store ([`super::store`]), what each says while it is there arrives on its session with this
//! one ([`super::live`]), and what a service on them can ask of them goes as an action to one.
//! A session that took part as nobody (no certificate of the person's) has none of this and is
//! handed to nothing: there would be no device to name as the author of what it shares.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::followed_resource::SharedFollow;
use iroh::EndpointId;
use settings_store::continuity_privacy::ContinuityCapability;
use tilo_lock::MutexRecover as _;
use tilo_network_actor::PersonalFabricPayload;
use tilo_resource::Invoke;
use tilo_workspace_service::workspaces::person_workspace::devices::Devices;

use super::live::Live;
use super::store;
use super::{
    DeviceAbout, DeviceKind, FabricCapabilities, FabricLink, MediaSourceLocator, PersonDevices,
    PersonalFabricAction, PersonalFabricActionEnvelope, PersonalFabricSendError, SharedLink, clock,
};
use crate::platform::fabric::{DeviceId, nonzero};
/// Where this session's frames leave for the person's other devices.
type Outbox = crate::sync::HostOutbox;

/// What the person lets of this device continue on their other devices: each
/// facet's switch of their `continuity` settings, under the master switch,
/// each on unless they turned it off. The session's settings watch keeps them
/// current ([`Continuity::read`]).
#[derive(Clone, Debug, Default)]
pub struct Continuity {
    media: Arc<AtomicBool>,
    file_transfer: Arc<AtomicBool>,
    notifications: Arc<AtomicBool>,
    display_input: Arc<AtomicBool>,
}

impl Continuity {
    /// Take the switches from a settings document, as Settings writes it.
    pub fn read(&self, document: &serde_json::Map<String, serde_json::Value>) {
        let section = document.get("continuity");
        let on = |name: &str| match section {
            None => settings_store::continuity_privacy::ON_UNLESS_SET,
            Some(serde_json::Value::Object(section)) => section
                .get(name)
                .map_or(settings_store::continuity_privacy::ON_UNLESS_SET, |value| {
                    value.as_bool() == Some(true)
                }),
            Some(_) => false,
        };
        let enabled = on("enabled");
        for (switch, facet) in [
            (&self.media, ContinuityCapability::Media),
            (&self.file_transfer, ContinuityCapability::FileTransfer),
            (&self.notifications, ContinuityCapability::Notifications),
        ] {
            switch.store(enabled && on(facet.key()), Ordering::Relaxed);
        }
        // This device takes the keyboard and pointer of the person's other devices only with
        // Display & input on and its automatic grants left on, as Settings words both.
        self.display_input.store(
            enabled
                && on(ContinuityCapability::DisplayInput.key())
                && on("automatic_display_input"),
            Ordering::Relaxed,
        );
    }

    fn media_permitted(&self) -> bool {
        self.media.load(Ordering::Relaxed)
    }

    /// Whether the person lets notifications continue on their other devices.
    pub(crate) fn notifications_permitted(&self) -> bool {
        self.notifications.load(Ordering::Relaxed)
    }

    /// Whether the person's other devices may drive this one's keyboard and pointer.
    pub(crate) fn display_input_permitted(&self) -> bool {
        self.display_input.load(Ordering::Relaxed)
    }

    /// Whether the person lets files they send move to their other devices.
    pub(crate) fn file_transfer_permitted(&self) -> bool {
        self.file_transfer.load(Ordering::Relaxed)
    }
}

/// What this device says of itself that lasts, as its first session writes it: what the person
/// calls it, and what it does for their other devices. Input, display and speaker groups are not
/// offered here, so they are not claimed.
pub(crate) fn about(label: &str) -> DeviceAbout {
    DeviceAbout {
        label: label.to_owned(),
        kind: DeviceKind::Desktop,
        capabilities: FabricCapabilities {
            // What the person copies here reaches their other devices, and what they play here can
            // go on playing on them.
            clipboard: true,
            audio_output: true,
            video_output: true,
            ..FabricCapabilities::default()
        },
    }
}

/// The session's own link to the person's other devices: what they say live, their store, and the
/// way this device's frames leave.
pub(crate) struct SessionFabric {
    live: Mutex<Live>,
    /// The record of the person's devices in their own workspace, where what lasts of each of
    /// them is kept, as the session follows it, and this device's certificate there. A session
    /// without it keeps nothing of its own and reads only this device.
    personal: Option<(SharedFollow<Devices>, tilo_user_identity::DeviceCert)>,
    /// The person's devices as the record last read, and its reading then: read again only when
    /// it moved.
    kept: Mutex<Option<(u64, PersonDevices)>>,
    label: String,
    outgoing: Outbox,
    /// The person's root key: a frame goes only to the devices that proved a certificate of it.
    user_public_key: [u8; 32],
    continuity: Continuity,
    /// This session's Nearby Send, once the session made it: where the bytes of a transfer one of
    /// the person's devices offers or streams come in.
    /// The endpoints heard on this device's local network, of which the person's devices are
    /// reachable without a server.
    lan: tilo_endpoint_reachability::Nearby,
}

impl std::fmt::Debug for SessionFabric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionFabric")
            .field("online", &self.live.lock_recover().online_device_ids())
            .finish_non_exhaustive()
    }
}

/// How this device reaches the person's other devices, as the session hands it over.
pub(crate) struct Reach {
    pub(crate) outgoing: Outbox,
    pub(crate) user_public_key: [u8; 32],
    pub(crate) continuity: Continuity,
    pub(crate) lan: tilo_endpoint_reachability::Nearby,
}

#[cfg(test)]
impl Reach {
    /// A reach for a test, whose frames go nowhere but the list it returns with: each call reads
    /// what the link sent since the last.
    pub(crate) fn listened(
        user_public_key: [u8; 32],
        continuity: Continuity,
    ) -> (Self, impl FnMut() -> Vec<PersonalFabricPayload>) {
        let (outgoing, mut frames) = tokio::sync::mpsc::unbounded_channel();
        let reach = Self {
            outgoing: Outbox::of(outgoing),
            user_public_key,
            continuity,
            lan: tilo_endpoint_reachability::Nearby::new(Arc::new(|| {})),
        };
        let sent = move || {
            let mut sent = Vec::new();
            while let Ok(command) = frames.try_recv() {
                if let crate::sync::HostCmd::Fabric { payload, .. } = command {
                    sent.push(payload);
                }
            }
            sent
        };
        (reach, sent)
    }
}

impl SessionFabric {
    /// The link of `device`, called `label`, onto the person's devices, with what lasts of them in
    /// the record `personal` follows. What this device is reaches the record through its daemon
    /// ([`crate::owner_session::OwnerDevices`]).
    pub(crate) fn new(
        device: DeviceId,
        label: String,
        personal: Option<(SharedFollow<Devices>, tilo_user_identity::DeviceCert)>,
        reach: Reach,
    ) -> Arc<Self> {
        Arc::new(Self {
            live: Mutex::new(Live::new(device)),
            personal,
            kept: Mutex::new(None),
            label,
            outgoing: reach.outgoing,
            user_public_key: reach.user_public_key,
            continuity: reach.continuity,
            lan: reach.lan,
        })
    }

    /// This session's Nearby Send takes what the person's devices offer and stream here.
    /// One of the person's devices proved itself on its session with this one. What it proves is
    /// the grant to tell it what this device says of itself, so it is told at once: both sides say
    /// themselves on the session's start and neither has to ask. Two devices can bring up a
    /// connection each and keep one of them, so what either said on the other is lost: each says
    /// itself again on every one.
    pub(crate) fn sibling(
        &self,
        peer: EndpointId,
        certs: &[tilo_user_identity::DeviceCert],
        revocations: &[tilo_user_identity::RevocationEntry],
    ) {
        if self
            .live
            .lock_recover()
            .sibling(peer, certs, revocations)
            .is_none()
        {
            return;
        }
        self.publish(Some(peer), PersonalFabricPayload::Here);
    }

    /// The session with one of the person's devices ended: it is no longer here.
    pub(crate) fn sibling_left(&self, peer: EndpointId) {
        self.live.lock_recover().sibling_left(peer);
    }

    /// Take one frame a device sent on its session, at the moment the transport took it: what it
    /// says of itself, the copies it offers, or an action for this device.
    pub(crate) fn arrived(
        &self,
        peer: EndpointId,
        payload: &PersonalFabricPayload,
        received: Instant,
    ) {
        match payload {
            PersonalFabricPayload::Here => {
                let proved = self.live.lock_recover().here(peer);
                if let Err(error) = proved {
                    tracing::warn!(%error, %peer, "an endpoint said it is there that proved no device of the person's");
                }
            }
            PersonalFabricPayload::Action(bytes) => {
                let taken = self.live.lock_recover().action(peer, bytes);
                match taken {
                    Ok(action) => {
                        // A clock probe is answered where its frame arrived,
                        // and a report is measured from there: the exchange is
                        // worth only the timestamps it carries.
                        let device = self.live.lock_recover().device_behind(peer);
                        if let Some(device) = device
                            && clock::arrived(self, &device, &action, received)
                        {
                            return;
                        }
                        tracing::debug!(%peer, action = ?action, "an action from one of the person's devices");
                    }
                    Err(error) => {
                        tracing::warn!(%error, %peer, "a device sent an action this session does not take");
                    }
                }
            }
            // What the person's devices carry for each other of their records belongs to their
            // own store's links; this session routes none of it and drops it rather than
            // half-handling it.
            PersonalFabricPayload::Document { .. }
            | PersonalFabricPayload::DocumentContentWants { .. }
            | PersonalFabricPayload::DocumentContentPieces { .. }
            | PersonalFabricPayload::DocumentContentLacks { .. }
            // A copy's frame from an earlier build: clipboard continuity is the daemon's now,
            // and this session routes none of it.
            | PersonalFabricPayload::RetiredClipboard => {}
        }
    }

    /// The actions addressed to this device since the last call, for the session's pass to route.
    pub(crate) fn take_actions(&self) -> Vec<PersonalFabricActionEnvelope> {
        self.live.lock_recover().take_actions()
    }

    /// Probe the clock of every peer something here holds in sync, and forget every estimate while
    /// Media continuity is off: the session's loop calls this every pass.
    pub(crate) fn pump_clock(&self) {
        clock::pump(self, self.continuity.media_permitted());
    }

    fn publish(&self, target: Option<EndpointId>, payload: PersonalFabricPayload) {
        self.outgoing.fabric(self.user_public_key, target, payload);
    }
}

impl FabricLink for SessionFabric {
    fn person_devices(&self) -> Option<Devices> {
        let (record, _) = self.personal.as_ref()?;
        Some(Devices::clone(&record.get()))
    }

    fn apply_person_devices(&self, action: Invoke<Devices>) -> Result<(), String> {
        let (record, _) = self
            .personal
            .as_ref()
            .ok_or_else(|| "this device holds no record of the person's devices".to_owned())?;
        let kind = action.id;
        record.invoke_unwaited(&action, move |made| {
            if let Err(error) = made {
                tracing::warn!(%error, action = kind, "the record of the person's devices was not changed");
            }
        });
        Ok(())
    }

    fn devices(&self) -> PersonDevices {
        let mut kept = match &self.personal {
            Some((record, _)) => {
                let seen = record.revision();
                let mut cache = self.kept.lock_recover();
                match cache.as_ref() {
                    Some((at, devices)) if *at == seen => devices.clone(),
                    _ => {
                        let devices =
                            PersonDevices::new(store::kept(&record.get(), &self.user_public_key));
                        *cache = Some((seen, devices.clone()));
                        devices
                    }
                }
            }
            None => PersonDevices::default(),
        };
        let live = self.live.lock_recover();
        // This device is the person's whether or not their own workspace reached it yet.
        if !kept.contains(live.local()) {
            kept.insert(super::PersonDevice::new(
                live.local().clone(),
                about(&self.label),
            ));
        }
        live.overlay(&mut kept);
        kept
    }

    fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        self.live.lock_recover().online_device_ids()
    }

    fn nearby_device_ids(&self) -> BTreeSet<DeviceId> {
        let heard = self.lan.heard();
        let known = self.devices();
        self.live.lock_recover().nearby_device_ids(&heard, &known)
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        Some((self.live.lock_recover().local().clone(), self.label.clone()))
    }

    fn device_behind(&self, endpoint: iroh::EndpointId) -> Option<DeviceId> {
        self.live.lock_recover().device_behind(endpoint)
    }

    fn try_send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError> {
        let (request_id, endpoint, bytes) = {
            let live = self.live.lock_recover();
            let local = live.local().clone();
            if &local == target {
                return Err(PersonalFabricSendError::Rejected(
                    "an action needs one of the person's other devices".into(),
                ));
            }
            let Some(endpoint) = live.endpoint_of(target) else {
                return Err(PersonalFabricSendError::Rejected(
                    "that device has no session with this one".into(),
                ));
            };
            drop(live);
            let request_id = nonzero::<16>();
            let envelope = PersonalFabricActionEnvelope {
                request_id,
                source: local.clone(),
                target: target.clone(),
                issued_unix_ms: tilo_clock::unix_now_ms(),
                action,
            };
            envelope
                .validate_at(&local, target, envelope.issued_unix_ms)
                .map_err(|error| PersonalFabricSendError::Rejected(error.to_string()))?;
            let bytes = postcard::to_allocvec(&envelope)
                .map_err(|error| PersonalFabricSendError::Rejected(error.to_string()))?;
            (request_id, endpoint, bytes)
        };
        self.publish(Some(endpoint), PersonalFabricPayload::Action(bytes));
        Ok(request_id)
    }

    fn require_media(&self) -> Result<(), String> {
        self.continuity
            .media_permitted()
            .then_some(())
            .ok_or_else(|| "media continuity is turned off in this device's settings".to_owned())
    }
}

/// What this session does with the actions the person's other devices sent it: the services that
/// take their own, and the offer a device's session becomes here.
impl crate::Session {
    /// Carry out one action one of the person's devices addressed to this one. The speaker services
    /// take their own; a session another device offers becomes the now-playing card's offer, which
    /// this device continues from.
    pub(crate) fn route_fabric_action(
        &self,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
    ) {
        if Self::service_took(fabric, envelope) {
            return;
        }
        match &envelope.action {
            PersonalFabricAction::ResumeMedia { .. } => self.offer_media(fabric, envelope),
            // Nothing on this session holds a media offer, and what the person's devices send each
            // other that no service here carries — accessories, calls, wake — is dropped rather
            // than half-acted on. The speaker services answer for theirs; the rest reach here only
            // when their own preconditions refuse.
            PersonalFabricAction::MediaAccepted { .. }
            | PersonalFabricAction::MediaRejected { .. }
            | PersonalFabricAction::MediaCancelled { .. }
            // A clock probe is answered where its frame arrived and a report measured from there,
            // so nothing on this side routes one.
            | PersonalFabricAction::FabricClockProbe { .. }
            | PersonalFabricAction::FabricClockReport { .. }
            | PersonalFabricAction::RetiredSpeakerGroupInvite
            | PersonalFabricAction::RetiredSpeakerGroupDecision
            | PersonalFabricAction::RetiredSpeakerGroupLeave
            | PersonalFabricAction::RetiredSpeakerGroupSessionOffer
            | PersonalFabricAction::RetiredSpeakerGroupSessionDecision
            | PersonalFabricAction::RetiredSpeakerGroupSessionStop
            | PersonalFabricAction::SpeakerIdentify { .. }
            | PersonalFabricAction::SpeakerSweep { .. }
            | PersonalFabricAction::SpeakerListenInvite { .. }
            | PersonalFabricAction::SpeakerListenDecision { .. }
            | PersonalFabricAction::SpeakerListen { .. }
            | PersonalFabricAction::SpeakerHeard { .. }
            | PersonalFabricAction::RemoteAudioOffer { .. }
            | PersonalFabricAction::RemoteAudioDecision { .. }
            | PersonalFabricAction::RemoteAudioFrame { .. }
            | PersonalFabricAction::RemoteAudioStop { .. }
            | PersonalFabricAction::RemoteAudioCancel { .. }
            | PersonalFabricAction::RemoteAudioKeepalive { .. } => {}
        }
    }

    /// Whether one of this session's services took `envelope` 's action: a speaker service's.
    /// What they did not take is the session's own.
    fn service_took(fabric: &SharedLink, envelope: &PersonalFabricActionEnvelope) -> bool {
        tilo_speaker_service::remote_audio::handle_action(fabric.as_ref(), envelope)
            || tilo_speaker_service::groups::handle_action(fabric.as_ref(), envelope)
    }

    /// Offer what another device was playing as the now-playing card, so the person can pick it up
    /// here.
    fn offer_media(&self, fabric: &SharedLink, envelope: &PersonalFabricActionEnvelope) {
        // What continues on this device follows the same media switch as what leaves it: with media
        // continuity off here, the offer is not made.
        if fabric.require_media().is_err() {
            return;
        }
        let PersonalFabricAction::ResumeMedia {
            app_id,
            title,
            locator,
            position_ms,
            resume_playing,
            ..
        } = &envelope.action
        else {
            return;
        };
        let source = match locator {
            MediaSourceLocator::Workspace { path } => media::handoff::HandoffSource::Workspace {
                path: std::path::PathBuf::from(path),
            },
            MediaSourceLocator::RemoteStream { url } => {
                media::handoff::HandoffSource::RemoteStream { url: url.clone() }
            }
            MediaSourceLocator::AppResource { uri } => {
                media::handoff::HandoffSource::AppResource { uri: uri.clone() }
            }
            // What arrives that way is held in the receiving shell's verified transfer store, which
            // this session has none of, so there is nothing here the card could resume.
            MediaSourceLocator::RetiredPersonalTransfer { .. } => return,
        };
        let label = fabric.devices().get(&envelope.source).map_or_else(
            || envelope.source.as_str().to_owned(),
            |device| device.about.label.clone(),
        );
        #[allow(
            clippy::cast_precision_loss,
            reason = "a playhead in a recording is far inside f64's exact integers"
        )]
        let position_ms = *position_ms as f64;
        crate::devices::handoff(
            &self.shared,
            Some(crate::hosted::MediaOffer {
                request_id: hex::encode(envelope.request_id),
                package_id: app_id.to_owned(),
                title: title.to_owned(),
                source_label: label,
                source,
                position_ms,
                playing: *resume_playing,
            }),
        );
    }
}
