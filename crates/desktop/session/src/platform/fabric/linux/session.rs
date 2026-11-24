//! This session's way onto the person's certified devices: what lasts of each is in the person's
//! own store ([`super::store`]), what each says while it is there arrives on its session with this
//! one ([`super::live`]), and what a service on them can ask of them goes as an action to one.
//! A session that took part as nobody (no certificate of the person's) has none of this and is
//! handed to nothing: there would be no device to name as the author of what it shares.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::PersonResources;
use iroh::EndpointId;
use settings_store::continuity_privacy::ContinuityCapability;
use tilo_lock::MutexRecover as _;
use tilo_network_actor::PersonalFabricPayload;

use super::live::Live;
use super::store;
use super::{
    ClipboardEntry, ContinuityNotificationProjection, DeviceAbout, DeviceKind, FabricCapabilities,
    FabricLink, MediaSourceLocator, NearbySpoolOffer, PersonDevices, PersonalFabricAction,
    PersonalFabricActionEnvelope, PersonalFabricSendError, SharedLink, clock,
};
use crate::platform::fabric::{DeviceId, FabricStamp, nonzero};
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

    fn notifications_permitted(&self) -> bool {
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
    /// The person's own workspace, where what lasts of each of their devices is kept, and this
    /// device's certificate there. A session without it keeps nothing of its own and reads only
    /// this device.
    personal: Option<(PersonResources, tilo_user_identity::DeviceCert)>,
    /// The person's devices as the workspace last read, and its change count then: read again
    /// only when it moved.
    kept: Mutex<Option<(u64, PersonDevices)>>,
    /// Whether the copies this device offers moved since the person's other devices were last
    /// told.
    offered_moved: AtomicBool,
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
    /// `personal`. What this device is reaches the record through its daemon
    /// ([`crate::owner_session::OwnerDevices`]).
    pub(crate) fn new(
        device: DeviceId,
        label: String,
        personal: Option<(PersonResources, tilo_user_identity::DeviceCert)>,
        reach: Reach,
    ) -> Arc<Self> {
        Arc::new(Self {
            live: Mutex::new(Live::new(device)),
            personal,
            kept: Mutex::new(None),
            offered_moved: AtomicBool::new(false),
            label,
            outgoing: reach.outgoing,
            user_public_key: reach.user_public_key,
            continuity: reach.continuity,
            lan: reach.lan,
        })
    }

    /// The person's own workspace and this device's certificate there: where what lasts of this
    /// device is kept.
    pub(crate) fn personal(&self) -> Option<(PersonResources, tilo_user_identity::DeviceCert)> {
        self.personal.clone()
    }

    /// The person's own workspace's resources this fabric keeps what lasts of this device in.
    pub(crate) fn person_resources(&self) -> Option<PersonResources> {
        self.personal
            .as_ref()
            .map(|(resources, _)| resources.clone())
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
        let clipboard = {
            let mut live = self.live.lock_recover();
            if live.sibling(peer, certs, revocations).is_none() {
                return;
            }
            live.own_clipboard().to_vec()
        };
        self.publish(Some(peer), PersonalFabricPayload::Here);
        if !clipboard.is_empty() {
            self.publish(Some(peer), PersonalFabricPayload::Clipboard(clipboard));
        }
    }

    /// The session with one of the person's devices ended: it is no longer here.
    pub(crate) fn sibling_left(&self, peer: EndpointId) {
        self.live.lock_recover().sibling_left(peer);
    }

    /// Take one frame a device sent on its session, at the moment the transport took it: what it
    /// says of itself, the copies it offers, an action for this device, or the bytes of a transfer.
    pub(crate) fn arrived(
        &self,
        peer: EndpointId,
        payload: &PersonalFabricPayload,
        received: Instant,
    ) {
        match payload {
            PersonalFabricPayload::Here => {
                if let Err(error) = self.live.lock_recover().here(peer) {
                    tracing::warn!(%error, %peer, "an endpoint said it is there that proved no device of the person's");
                }
            }
            PersonalFabricPayload::Clipboard(entries) => {
                let taken = self.live.lock_recover().clipboard(peer, entries.clone());
                if let Err(error) = taken {
                    tracing::warn!(%error, %peer, "a device offered copies this session does not take");
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
            // A Nearby Send's bytes are the device daemon's, carried over its action service:
            // none arrive on the session's fabric.
            PersonalFabricPayload::NearbyTransfer(_frame) => {
                tracing::debug!(%peer, "a nearby spool frame is dropped");
            }
            // What the person's devices carry for each other of their records belongs to their
            // own store's links; this session routes none of it and drops it rather than
            // half-handling it.
            PersonalFabricPayload::Document { .. }
            | PersonalFabricPayload::DocumentContentWant { .. }
            | PersonalFabricPayload::DocumentContentPiece { .. }
            | PersonalFabricPayload::DocumentContentLacks { .. } => {}
        }
    }

    /// Tell the person's other devices the copies this device offers, when they moved since they
    /// were last told, and keep the speaker groups this device leads
    /// and joined in its part of the person's store when they moved: the session's pass calls this
    /// once it offered the clipboard, so a copy leaves on the pass that made it.
    pub(crate) fn announce(&self) {
        if self.offered_moved.swap(false, Ordering::Relaxed) {
            let clipboard = self.live.lock_recover().own_clipboard().to_vec();
            self.publish(None, PersonalFabricPayload::Clipboard(clipboard));
        }
        self.keep_speakers();
    }

    /// Keep the speaker groups this device leads and joined, the memberships it gave and the
    /// outputs it offers in its part of the person's store, when they moved. Nothing is offered
    /// while Media continuity is off.
    fn keep_speakers(&self) {
        let Some((resources, cert)) = &self.personal else {
            return;
        };
        let local = self.live.lock_recover().local().clone();
        let kept = self
            .devices()
            .get(&local)
            .map(|device| device.speakers.clone());
        let speakers = tilo_speaker_service::groups::authored_state(
            &local,
            kept.as_ref(),
            &FabricStamp {
                counter: 1,
                device: local.clone(),
            },
            self.continuity.media_permitted(),
        );
        if kept.as_ref() == Some(&speakers) {
            return;
        }
        if let Err(error) = store::keep_own(resources, cert, store::SPEAKERS, &speakers) {
            tracing::warn!(%error, "this device's speaker groups were not kept");
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
    fn devices(&self) -> PersonDevices {
        let mut kept = match &self.personal {
            Some((resources, _)) => {
                let seen = *resources.changes().borrow();
                let mut cache = self.kept.lock_recover();
                match cache.as_ref() {
                    Some((at, devices)) if *at == seen => devices.clone(),
                    _ => {
                        let devices =
                            PersonDevices::new(store::kept(resources, &self.user_public_key));
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

    fn share_nearby_spool(
        &self,
        target: &DeviceId,
        offer: NearbySpoolOffer,
    ) -> Result<(), PersonalFabricSendError> {
        let endpoint = self
            .live
            .lock_recover()
            .endpoint_of(target)
            .ok_or_else(|| {
                PersonalFabricSendError::Rejected("that device has no session with this one".into())
            })?;
        if !self
            .outgoing
            .nearby_spool(self.user_public_key, endpoint, offer.checkpoint, offer.file)
        {
            return Err(PersonalFabricSendError::Rejected(
                "this session's transport stopped".into(),
            ));
        }
        Ok(())
    }

    fn notifications_permitted(&self) -> bool {
        self.continuity.notifications_permitted()
    }

    fn share_clipboard(&self, entries: Vec<ClipboardEntry>) {
        let offered = self.live.lock_recover().offer(entries);
        match offered {
            Ok(true) => self.offered_moved.store(true, Ordering::Relaxed),
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, "the clipboard is not shared"),
        }
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
        if self.service_took(fabric, envelope) {
            return;
        }
        match &envelope.action {
            PersonalFabricAction::ResumeMedia { .. } => self.offer_media(fabric, envelope),
            // A notification an app sent on another of the person's devices is shown here named
            // after that device; what the person does with one shown here goes back to where it
            // came from.
            PersonalFabricAction::NotificationProject { notification } => {
                self.show_remote_notification(fabric, envelope, notification);
            }
            PersonalFabricAction::NotificationRetire { id } => {
                self.shared
                    .notifications
                    .borrow()
                    .remote_retire(&envelope.source, *id);
            }
            // A choice on one this device projected: the app's notification closes here, and every
            // copy goes with it.
            PersonalFabricAction::NotificationDismiss { .. }
            | PersonalFabricAction::NotificationInvoke { .. } => {
                self.shared.notifications.borrow_mut().answered(&envelope.action);
            }
            // Nothing on this session holds a media offer, and what the person's devices send each
            // other that no service here carries — accessories, files, calls,
            // displays, wake — is dropped rather than half-acted on. The speaker services and the
            // certification above answer for theirs; the rest reach here only when their own
            // preconditions refuse.
            PersonalFabricAction::MediaAccepted { .. }
            | PersonalFabricAction::MediaRejected { .. }
            | PersonalFabricAction::MediaCancelled { .. }
            // The person's keyboard and pointer between their devices, while this session takes
            // part in no fabric that could carry them.
            | PersonalFabricAction::InputLeaseAsk
            | PersonalFabricAction::InputLeaseGranted { .. }
            | PersonalFabricAction::InputLeaseRevoked { .. }
            | PersonalFabricAction::Input { .. }
            | PersonalFabricAction::DragCarried { .. }
            | PersonalFabricAction::NearbyDrop { .. }
            | PersonalFabricAction::NearbyDropResult { .. }
            // A Nearby Send reaches no session here: the client that carries one hands its offer,
            // checkpoint, withdrawal and ending to its own engine, so what is left is a session
            // running none.
            | PersonalFabricAction::NearbyTransferOffer { .. }
            | PersonalFabricAction::NearbyTransferReady { .. }
            | PersonalFabricAction::NearbyTransferCancel { .. }
            | PersonalFabricAction::NearbyTransferRestart { .. }
            | PersonalFabricAction::NearbyTransferResult { .. }
            // What validation refuses never reaches here.
            | PersonalFabricAction::RetiredBluetoothPrepare
            | PersonalFabricAction::RetiredBluetoothPrepared
            | PersonalFabricAction::RetiredBluetoothCommit
            | PersonalFabricAction::RetiredBluetoothFinished
            | PersonalFabricAction::RetiredBluetoothAccepted
            | PersonalFabricAction::RetiredBluetoothBondOffered
            | PersonalFabricAction::RetiredBluetoothBondCommit
            | PersonalFabricAction::RetiredBluetoothCancel
            | PersonalFabricAction::RetiredWakeDevice
            | PersonalFabricAction::RetiredWakeReport
            | PersonalFabricAction::RetiredWakeProbe
            | PersonalFabricAction::RetiredWakeProbeReply
            | PersonalFabricAction::GamepadForwardRequest { .. }
            | PersonalFabricAction::GamepadForwardGrant { .. }
            | PersonalFabricAction::GamepadForwardEvent { .. }
            | PersonalFabricAction::GamepadForwardStop { .. }
            // A clock probe is answered where its frame arrived and a report measured from there,
            // so nothing on this side routes one.
            | PersonalFabricAction::FabricClockProbe { .. }
            | PersonalFabricAction::FabricClockReport { .. }
            | PersonalFabricAction::WindowViews { .. }
            // No device of the person certifies another over the fabric any more: an ask or an
            // answer from an older device is dropped.
            | PersonalFabricAction::CertifyAsk { .. }
            | PersonalFabricAction::CertifyAnswer { .. }
            | PersonalFabricAction::SpeakerGroupInvite { .. }
            | PersonalFabricAction::SpeakerGroupDecision { .. }
            | PersonalFabricAction::SpeakerGroupLeave { .. }
            | PersonalFabricAction::SpeakerGroupSessionOffer { .. }
            | PersonalFabricAction::SpeakerGroupSessionDecision { .. }
            | PersonalFabricAction::SpeakerGroupSessionStop { .. }
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

    /// Whether one of this session's services took `envelope` 's action: a speaker service's, the
    /// person's keyboard and pointer. What they did not take is the session's own.
    fn service_took(&self, fabric: &SharedLink, envelope: &PersonalFabricActionEnvelope) -> bool {
        tilo_speaker_service::remote_audio::handle_action(fabric.as_ref(), envelope)
            || tilo_speaker_service::groups::handle_action(fabric.as_ref(), envelope)
            || self
                .shared
                .keyboard_and_pointer
                .as_ref()
                .is_some_and(|shared_input| shared_input.route(envelope))
    }

    /// Show the notification another of the person's devices projected,
    /// named after that device.
    fn show_remote_notification(
        &self,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
        notification: &ContinuityNotificationProjection,
    ) {
        let label = fabric.devices().get(&envelope.source).map_or_else(
            || envelope.source.as_str().to_owned(),
            |device| device.about.label.clone(),
        );
        self.shared
            .notifications
            .borrow()
            .remote_project(&envelope.source, &label, notification);
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
            MediaSourceLocator::PersonalTransfer { .. } => return,
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
