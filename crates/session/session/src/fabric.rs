//! This session's way onto the person's certified devices: the fabric state
//! it holds ([`tilo_device_fabric::runtime::FabricRuntime`]), the commands
//! that carry it to their other devices, and what a service on the fabric
//! can ask of them.
//!
//! A session that took part as nobody — no certificate of the person's —
//! keeps no fabric and is handed to nothing: there would be no device to
//! name as the author of what it copies.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use iroh::EndpointId;
use tilo_device_fabric::link::{FabricLink, PersonalFabricSendError};
use tilo_device_fabric::runtime::FabricRuntime;
use tilo_device_fabric::{
    ClipboardEntry, DeviceFabric, DeviceId, PersonalFabricAction, PersonalFabricActionEnvelope,
};
use tilo_lock::MutexRecover as _;
use tilo_network_actor::PersonalFabricPayload;

use crate::sync::Outgoing;

/// What the person lets of this device continue on their other devices: each
/// facet's switch of their `continuity` settings, under the master switch,
/// each on unless they turned it off. The session's settings watch keeps them
/// current ([`Continuity::read`]).
#[derive(Clone, Debug, Default)]
pub struct Continuity {
    media: Arc<AtomicBool>,
    wifi_settings: Arc<AtomicBool>,
    file_transfer: Arc<AtomicBool>,
    notifications: Arc<AtomicBool>,
}

impl Continuity {
    /// Take the switches from a settings document, as Settings writes it.
    pub(crate) fn read(&self, document: &serde_json::Map<String, serde_json::Value>) {
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
        self.media.store(enabled && on("media"), Ordering::Relaxed);
        self.wifi_settings
            .store(enabled && on("wifi_settings"), Ordering::Relaxed);
        self.file_transfer
            .store(enabled && on("file_transfer"), Ordering::Relaxed);
        self.notifications
            .store(enabled && on("notifications"), Ordering::Relaxed);
    }

    fn media_permitted(&self) -> bool {
        self.media.load(Ordering::Relaxed)
    }

    fn wifi_settings_permitted(&self) -> bool {
        self.wifi_settings.load(Ordering::Relaxed)
    }

    fn notifications_permitted(&self) -> bool {
        self.notifications.load(Ordering::Relaxed)
    }

    /// Whether the person lets files they send move to their other devices.
    pub(crate) fn file_transfer_permitted(&self) -> bool {
        self.file_transfer.load(Ordering::Relaxed)
    }
}

/// The session's own link to the person's other devices: the fabric it
/// converges, and the way its frames leave.
pub(crate) struct SessionFabric {
    runtime: Arc<Mutex<FabricRuntime>>,
    outgoing: tokio::sync::mpsc::UnboundedSender<Outgoing>,
    /// The person's root key: a frame goes only to the devices that proved a
    /// certificate of it.
    user_public_key: [u8; 32],
    /// This device's key: what a credential one of the person's devices
    /// sealed for it opens with.
    device_signing_key: ed25519_dalek::SigningKey,
    continuity: Continuity,
    /// This session's Nearby Send, once the session made it: where the bytes
    /// of a transfer one of the person's devices offers or streams come in.
    nearby: Mutex<Option<Arc<crate::nearby_transfer::NearbyTransfer>>>,
}

impl std::fmt::Debug for SessionFabric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionFabric")
            .field("online", &self.runtime.lock_recover().online_device_ids())
            .finish_non_exhaustive()
    }
}

impl SessionFabric {
    pub(crate) fn new(
        runtime: FabricRuntime,
        outgoing: tokio::sync::mpsc::UnboundedSender<Outgoing>,
        user_public_key: [u8; 32],
        device_signing_key: ed25519_dalek::SigningKey,
        continuity: Continuity,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime: Arc::new(Mutex::new(runtime)),
            outgoing,
            user_public_key,
            device_signing_key,
            continuity,
            nearby: Mutex::new(None),
        })
    }

    /// This session's Nearby Send takes what the person's devices offer and
    /// stream here.
    pub(crate) fn attach_nearby(&self, nearby: Arc<crate::nearby_transfer::NearbyTransfer>) {
        *self.nearby.lock_recover() = Some(nearby);
    }

    /// One of the person's devices proved itself on its session with this
    /// one. What it proves is the grant to tell it this device's fabric, so
    /// it is told at once: both sides announce on the session's start and
    /// neither has to ask first.
    pub(crate) fn sibling(
        &self,
        peer: EndpointId,
        certs: &[tilo_user_identity::DeviceCert],
        revocations: &[tilo_user_identity::RevocationEntry],
    ) {
        self.runtime
            .lock_recover()
            .sibling(peer, certs, revocations);
        self.announce();
    }

    /// The session with one of the person's devices ended: it is no longer
    /// here, and what it said stays as what it last said.
    pub(crate) fn sibling_left(&self, peer: EndpointId) {
        self.runtime.lock_recover().sibling_left(peer);
    }

    /// Take one frame of the fabric a device sent on its session: what it
    /// says of itself, an action for this device, or its request for this
    /// device's fabric.
    pub(crate) fn arrived(&self, peer: EndpointId, payload: &PersonalFabricPayload) {
        match payload {
            PersonalFabricPayload::Snapshot(bytes) => {
                let taken = self.runtime.lock_recover().snapshot(peer, bytes);
                if let Err(error) = taken {
                    tracing::warn!(%error, %peer, "a device sent a snapshot this session does not take");
                }
            }
            PersonalFabricPayload::Action(bytes) => {
                let taken = self.runtime.lock_recover().action(peer, bytes);
                match taken {
                    Ok(Some(action)) => {
                        tracing::debug!(%peer, action = ?action, "an action from one of the person's devices");
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%error, %peer, "a device sent an action this session does not take");
                    }
                }
            }
            // The other side has none of this device's fabric yet: it gets
            // what this device says of itself, whether or not anything
            // changed since the last announcement.
            PersonalFabricPayload::Request => self.announce(),
            // Where one of the person's devices says its outputs stand, and
            // what their hardware says: the fabric admits both for the exact
            // revision the device's own record carries, and the shared
            // arrangement reads them from there.
            PersonalFabricPayload::DisplayConnectors { stamp, outputs } => {
                let taken = self.runtime.lock_recover().observe_display_connectors(
                    peer,
                    stamp,
                    outputs.clone(),
                );
                if let Err(error) = taken {
                    tracing::warn!(%error, %peer, "a device's outputs are not taken as they stand");
                }
            }
            PersonalFabricPayload::DisplayOutputs { stamp, outputs } => {
                let taken = self.runtime.lock_recover().observe_display_outputs(
                    peer,
                    stamp,
                    outputs.clone(),
                );
                if let Err(error) = taken {
                    tracing::warn!(%error, %peer, "a device's output facts are not taken");
                }
            }
            // The bytes of a Nearby Send this device took: the transfer
            // engine holds them for the device that proved itself here.
            PersonalFabricPayload::NearbyTransfer(frame) => {
                let nearby = self.nearby.lock_recover().clone();
                let device = self.runtime.lock_recover().device_behind(peer);
                if let (Some(nearby), Some(device)) = (nearby, device) {
                    nearby.frame(&device, frame);
                }
            }
            // What the person's devices carry for each other beyond the
            // fabric state this session keeps — records — belongs to their
            // own planes; this session routes none of it and drops it rather
            // than half-handling it.
            PersonalFabricPayload::Document { .. }
            | PersonalFabricPayload::RecordContentWant { .. }
            | PersonalFabricPayload::RecordContentPiece { .. }
            | PersonalFabricPayload::RecordContentLacks { .. } => {}
        }
    }

    /// Tell the person's other devices what this device says of itself, when
    /// its record moved since the last announcement: a copy it made that its
    /// clipboard now shares, or a restarted session. Nothing is announced
    /// while nothing moved.
    pub(crate) fn announce_if_changed(&self) {
        let mut runtime = self.runtime.lock_recover();
        if !runtime.changed() {
            return;
        }
        if let Ok(snapshot) = runtime.own_snapshot() {
            self.publish(None, PersonalFabricPayload::Snapshot(snapshot));
        }
        drop(runtime);
        self.announce_display();
    }

    /// Tell the person's other devices where this device's displays stand
    /// and what their hardware says, on the revision its record announced.
    fn announce_display(&self) {
        let display = self.runtime.lock_recover().local_display();
        let Some((stamp, connectors, outputs)) = display else {
            return;
        };
        self.publish(
            None,
            PersonalFabricPayload::DisplayConnectors {
                stamp: stamp.clone(),
                outputs: connectors,
            },
        );
        self.publish(
            None,
            PersonalFabricPayload::DisplayOutputs { stamp, outputs },
        );
    }

    /// Announce this device's record now, clearing that it has news.
    fn announce(&self) {
        let mut runtime = self.runtime.lock_recover();
        let _ = runtime.changed();
        if let Ok(snapshot) = runtime.own_snapshot() {
            self.publish(None, PersonalFabricPayload::Snapshot(snapshot));
        }
        drop(runtime);
        self.announce_display();
    }

    /// The actions addressed to this device since the last call, for the
    /// session's pass to route.
    pub(crate) fn take_actions(&self) -> Vec<PersonalFabricActionEnvelope> {
        self.runtime.lock_recover().take_actions()
    }

    fn publish(&self, target: Option<EndpointId>, payload: PersonalFabricPayload) {
        let _ = self.outgoing.send(Outgoing::Fabric {
            user_public_key: self.user_public_key,
            target,
            payload,
        });
    }
}

impl FabricLink for SessionFabric {
    fn device_fabric(&self) -> DeviceFabric {
        self.runtime.lock_recover().device_fabric().clone()
    }

    fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        self.runtime.lock_recover().online_device_ids()
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        self.runtime.lock_recover().local_identity()
    }

    fn device_behind(&self, endpoint: iroh::EndpointId) -> Option<DeviceId> {
        self.runtime.lock_recover().device_behind(endpoint)
    }

    fn send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], String> {
        self.try_send_personal_fabric_action(target, action)
            .map_err(|error| error.to_string())
    }

    fn try_send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError> {
        let (request_id, endpoint, bytes) = {
            let runtime = self.runtime.lock_recover();
            let Some((local, _)) = runtime.local_identity() else {
                return Err(PersonalFabricSendError::Rejected(
                    "this session is no certified device of the person's".into(),
                ));
            };
            if &local == target {
                return Err(PersonalFabricSendError::Rejected(
                    "an action needs one of the person's other devices".into(),
                ));
            }
            let Some(endpoint) = runtime.endpoint_of(target) else {
                return Err(PersonalFabricSendError::Rejected(
                    "that device has no session with this one".into(),
                ));
            };
            let request_id = tilo_device_fabric::random::nonzero::<16>();
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
            // The fabric is this device's own state, not the transport's:
            // the frame leaves with the lock released.
            let built = (request_id, endpoint, bytes);
            drop(runtime);
            built
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
        offer: tilo_device_fabric::link::NearbySpoolOffer,
    ) -> Result<(), PersonalFabricSendError> {
        let endpoint = self
            .runtime
            .lock_recover()
            .endpoint_of(target)
            .ok_or_else(|| {
                PersonalFabricSendError::Rejected("that device has no session with this one".into())
            })?;
        self.outgoing
            .send(Outgoing::NearbySpool {
                user_public_key: self.user_public_key,
                target: endpoint,
                checkpoint: offer.checkpoint,
                file: offer.file,
            })
            .map_err(|_| {
                PersonalFabricSendError::Rejected("this session's transport stopped".into())
            })
    }

    fn require_wifi_settings(&self) -> Result<(), String> {
        self.continuity
            .wifi_settings_permitted()
            .then_some(())
            .ok_or_else(|| "Wi-Fi continuity is turned off in this device's settings".to_owned())
    }

    fn notifications_permitted(&self) -> bool {
        self.continuity.notifications_permitted()
    }

    fn wifi_password(&self, ssid: &str, security: os_actions::WifiSecurity) -> Option<String> {
        if self.require_wifi_settings().is_err() || !security.supports_portable_passphrase() {
            return None;
        }
        let fabric = self.runtime.lock_recover().device_fabric().clone();
        fabric
            .merged_wifi()
            .into_values()
            .filter(|credential| credential.ssid == ssid && credential.security == security)
            .find_map(|credential| {
                tilo_device_fabric::secrets::open_wifi_credential(
                    &credential,
                    &self.device_signing_key,
                )
                .ok()
            })
    }

    fn share_wifi_password(
        &self,
        ssid: &str,
        security: os_actions::WifiSecurity,
        password: &str,
    ) -> Result<(), String> {
        self.require_wifi_settings()?;
        let shared = self
            .runtime
            .lock_recover()
            .share_wifi_password(ssid, security, password)
            .map_err(|error| error.to_string())?;
        if !shared {
            return Err("this session is no certified device of the person's".to_owned());
        }
        Ok(())
    }

    fn share_wake_routes(&self, routes: Vec<tilo_device_fabric::WakeRoute>) {
        let taken = self.runtime.lock_recover().set_wake_routes(routes);
        if let Err(error) = taken {
            tracing::warn!(%error, "this device's wake routes are not shared");
        }
    }

    fn share_clipboard(&self, entries: Vec<ClipboardEntry>) {
        let taken = self.runtime.lock_recover().share_clipboard(entries);
        if let Err(error) = taken {
            tracing::warn!(%error, "the clipboard is not shared");
        }
    }

    fn publish_owner_session(&self, session: tilo_device_fabric::OwnerSession) {
        let taken = self.runtime.lock_recover().set_owner_session(session);
        if let Err(error) = taken {
            tracing::warn!(%error, "this device's session is not said to the others");
        }
    }

    fn share_display(
        &self,
        rect: tilo_network_actor::fabric::DisplayRect,
        scale_milli: u16,
        connectors: Vec<tilo_network_actor::fabric::DisplayConnectorPlacement>,
        outputs: Vec<tilo_network_actor::fabric::DisplayOutputObservation>,
    ) -> bool {
        let taken = self
            .runtime
            .lock_recover()
            .set_display(rect, scale_milli, connectors, outputs);
        match taken {
            Ok(changed) => changed,
            Err(error) => {
                tracing::warn!(%error, "this device's displays are not shared");
                false
            }
        }
    }

    fn person_displays(&self) -> Vec<settings_store::person_displays::PersonDevice> {
        self.runtime.lock_recover().person_devices()
    }

    fn share_unlock(&self, unlock: tilo_device_fabric::unlock::UnlockFabricState) {
        let taken = self.runtime.lock_recover().set_unlock(unlock);
        if let Err(error) = taken {
            tracing::warn!(%error, "this device's disk unlock is not shared");
        }
    }
}

/// What this session does with the actions the person's other devices sent
/// it: the services that take their own, and the offer a device's session
/// becomes here.
impl crate::Session {
    /// Carry out one action one of the person's devices addressed to this
    /// one. The speaker services take their own; a session another device
    /// offers becomes the now-playing card's offer, which this device
    /// continues from.
    pub(crate) fn route_fabric_action(
        &self,
        fabric: &tilo_device_fabric::link::SharedLink,
        envelope: &tilo_device_fabric::PersonalFabricActionEnvelope,
    ) {
        use tilo_device_fabric::PersonalFabricAction;
        if self.service_took(fabric, envelope) {
            return;
        }
        match &envelope.action {
            PersonalFabricAction::ResumeMedia { .. } => self.offer_media(fabric, envelope),
            // A notification an app sent on another of the person's devices
            // is shown here named after that device; what the person does
            // with one shown here goes back to where it came from.
            PersonalFabricAction::NotificationProject { notification } => {
                self.show_remote_notification(fabric, envelope, notification);
            }
            PersonalFabricAction::NotificationRetire { id } => {
                self.shared
                    .notifications
                    .borrow()
                    .remote_retire(&envelope.source, *id);
            }
            // A choice on one this device projected: the app's notification
            // closes here, and every copy goes with it.
            PersonalFabricAction::NotificationDismiss { .. }
            | PersonalFabricAction::NotificationInvoke { .. } => {
                self.shared.notifications.borrow_mut().answered(&envelope.action);
            }
            // Nothing on this session holds a media offer, and what the
            // person's devices send each other that no service here carries —
            // placement, input, accessories, files, calls, displays, wake —
            // is dropped rather than half-acted on. The speaker services and
            // the certification above answer for theirs; the rest reach here
            // only when their own preconditions refuse.
            PersonalFabricAction::MediaAccepted { .. }
            | PersonalFabricAction::MediaRejected { .. }
            | PersonalFabricAction::MediaCancelled { .. }
            | PersonalFabricAction::SetDisplayPlacement { .. }
            | PersonalFabricAction::SetDisplayPlacementChecked { .. }
            | PersonalFabricAction::InputLeaseGranted { .. }
            | PersonalFabricAction::InputLeaseRevoked { .. }
            | PersonalFabricAction::Input { .. }
            | PersonalFabricAction::SpatialInputProtocol { .. }
            | PersonalFabricAction::SpatialInputSequenced { .. }
            | PersonalFabricAction::NearbyDrop { .. }
            | PersonalFabricAction::NearbyDropResult { .. }
            // A Nearby Send reaches no session here: the client that carries
            // one hands its offer, checkpoint, withdrawal and ending to its
            // own engine, so what is left is a session running none.
            | PersonalFabricAction::NearbyTransferOffer { .. }
            | PersonalFabricAction::NearbyTransferReady { .. }
            | PersonalFabricAction::NearbyTransferCancel { .. }
            | PersonalFabricAction::NearbyTransferRestart { .. }
            | PersonalFabricAction::NearbyTransferResult { .. }
            // A move of a Bluetooth accessory between devices that was not
            // addressed to this session's person ([`Shared::bluetooth`]
            // answers for those) is somebody else's to carry.
            | PersonalFabricAction::BluetoothHandoffPrepare { .. }
            | PersonalFabricAction::BluetoothHandoffPrepared { .. }
            | PersonalFabricAction::BluetoothHandoffCommit { .. }
            | PersonalFabricAction::BluetoothHandoffFinished { .. }
            | PersonalFabricAction::BluetoothHandoffAccepted { .. }
            | PersonalFabricAction::BluetoothHandoffPairingOffered { .. }
            | PersonalFabricAction::BluetoothHandoffPairingCommit { .. }
            | PersonalFabricAction::BluetoothHandoffCancel { .. }
            // What no build sends and the fabric refuses before it reaches
            // here; the variants stay for the wire's numbering alone.
            | PersonalFabricAction::RetiredRealtimeOffer
            | PersonalFabricAction::RetiredRealtimeAccept
            | PersonalFabricAction::RetiredRealtimeMediaReady
            | PersonalFabricAction::RetiredRealtimeStopSource
            | PersonalFabricAction::RetiredRealtimeCancel
            | PersonalFabricAction::RetiredRemoteCaptureSignal
            | PersonalFabricAction::RetiredRemoteCaptureChunk
            | PersonalFabricAction::GamepadForwardRequest { .. }
            | PersonalFabricAction::GamepadForwardGrant { .. }
            | PersonalFabricAction::GamepadForwardEvent { .. }
            | PersonalFabricAction::GamepadForwardStop { .. }
            | PersonalFabricAction::RetiredDeviceHealthProbe
            | PersonalFabricAction::RetiredDeviceHealthReport
            | PersonalFabricAction::RetiredTethering
            | PersonalFabricAction::FabricClockProbe { .. }
            | PersonalFabricAction::FabricClockReport { .. }
            | PersonalFabricAction::WakeDevice { .. }
            | PersonalFabricAction::WakeReport { .. }
            | PersonalFabricAction::WakeProbe { .. }
            | PersonalFabricAction::WakeProbeReply { .. }
            | PersonalFabricAction::WindowViews { .. }
            | PersonalFabricAction::ShowTab { .. }
            // The device-certification exchange answered above, for the other
            // door's preconditions.
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

    /// Whether one of this session's services took `envelope`'s action:
    /// a Bluetooth accessory's move, a device to wake, a certification, a
    /// speaker service's. What they did not take is the session's own.
    fn service_took(
        &self,
        fabric: &tilo_device_fabric::link::SharedLink,
        envelope: &tilo_device_fabric::PersonalFabricActionEnvelope,
    ) -> bool {
        self.shared
            .bluetooth
            .arrived(&self.shared, fabric, envelope)
            || self
                .shared
                .waking
                .handle_action(&self.shared, fabric, envelope)
            || crate::certification::handle_action(fabric, envelope)
            || tilo_speaker_service::remote_audio::handle_action(fabric.as_ref(), envelope)
            || tilo_speaker_service::groups::handle_action(fabric.as_ref(), envelope)
    }

    /// Show the notification another of the person's devices projected,
    /// named after that device.
    fn show_remote_notification(
        &self,
        fabric: &tilo_device_fabric::link::SharedLink,
        envelope: &tilo_device_fabric::PersonalFabricActionEnvelope,
        notification: &tilo_device_fabric::notification::ContinuityNotificationProjection,
    ) {
        let label = fabric
            .device_fabric()
            .snapshots()
            .get(&envelope.source)
            .map_or_else(
                || envelope.source.as_str().to_owned(),
                |snapshot| snapshot.presence.label.clone(),
            );
        self.shared
            .notifications
            .borrow()
            .remote_project(&envelope.source, &label, notification);
    }

    /// Offer what another device was playing as the now-playing card, so the
    /// person can pick it up here.
    fn offer_media(
        &self,
        fabric: &tilo_device_fabric::link::SharedLink,
        envelope: &tilo_device_fabric::PersonalFabricActionEnvelope,
    ) {
        // What continues on this device follows the same media switch as what
        // leaves it: with media continuity off here, the offer is not made.
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
            tilo_device_fabric::MediaSourceLocator::Workspace { path } => {
                media::handoff::HandoffSource::Workspace {
                    path: std::path::PathBuf::from(path),
                }
            }
            tilo_device_fabric::MediaSourceLocator::RemoteStream { url } => {
                media::handoff::HandoffSource::RemoteStream { url: url.clone() }
            }
            tilo_device_fabric::MediaSourceLocator::AppResource { uri } => {
                media::handoff::HandoffSource::AppResource { uri: uri.clone() }
            }
            // What arrives that way is held in the receiving shell's verified
            // transfer store, which this session has none of, so there is
            // nothing here the card could resume.
            tilo_device_fabric::MediaSourceLocator::PersonalTransfer { .. } => return,
        };
        let label = fabric
            .device_fabric()
            .snapshots()
            .get(&envelope.source)
            .map_or_else(
                || envelope.source.as_str().to_owned(),
                |snapshot| snapshot.presence.label.clone(),
            );
        #[allow(
            clippy::cast_precision_loss,
            reason = "a playhead in a recording is far inside f64's exact integers"
        )]
        let position_ms = *position_ms as f64;
        crate::mount::audio::handoff(
            &self.shared,
            Some(tilo_quick_settings::MediaOffer {
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
