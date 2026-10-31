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

/// Whether the person lets what they play here go on playing on their other
/// devices: the `media` switch of their `continuity` settings, under the
/// master switch, each on unless they turned it off. The session's settings
/// watch keeps it current ([`MediaContinuity::read`]).
#[derive(Clone, Debug, Default)]
pub struct MediaContinuity(Arc<AtomicBool>);

impl MediaContinuity {
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
        self.0
            .store(on("enabled") && on("media"), Ordering::Relaxed);
    }

    fn permitted(&self) -> bool {
        self.0.load(Ordering::Relaxed)
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
    media: MediaContinuity,
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
        media: MediaContinuity,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime: Arc::new(Mutex::new(runtime)),
            outgoing,
            user_public_key,
            media,
        })
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
                    tracing::debug!(%error, %peer, "a device sent a snapshot this session does not take");
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
                        tracing::debug!(%error, %peer, "a device sent an action this session does not take");
                    }
                }
            }
            // The other side has none of this device's fabric yet: it gets
            // what this device says of itself, whether or not anything
            // changed since the last announcement.
            PersonalFabricPayload::Request => self.announce(),
            // What the person's devices carry for each other beyond the
            // fabric state this session keeps — file bytes, records, screen
            // observations — belongs to the shell's runtime; this session
            // routes none of it and drops it rather than half-handling it.
            PersonalFabricPayload::NearbyTransfer(_)
            | PersonalFabricPayload::Document { .. }
            | PersonalFabricPayload::RecordContentWant { .. }
            | PersonalFabricPayload::RecordContentPiece { .. }
            | PersonalFabricPayload::RecordContentLacks { .. }
            | PersonalFabricPayload::DisplayConnectors { .. }
            | PersonalFabricPayload::DisplayOutputs { .. } => {}
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
    }

    /// Announce this device's record now, clearing that it has news.
    fn announce(&self) {
        let mut runtime = self.runtime.lock_recover();
        let _ = runtime.changed();
        if let Ok(snapshot) = runtime.own_snapshot() {
            self.publish(None, PersonalFabricPayload::Snapshot(snapshot));
        }
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
        self.media
            .permitted()
            .then_some(())
            .ok_or_else(|| "media continuity is turned off in this device's settings".to_owned())
    }

    fn share_clipboard(&self, entries: Vec<ClipboardEntry>) {
        let taken = self.runtime.lock_recover().share_clipboard(entries);
        if let Err(error) = taken {
            tracing::debug!(%error, "the clipboard is not shared");
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
    #[allow(
        clippy::unused_self,
        reason = "the actions are the session's to route, as the rest of its pass is"
    )]
    pub(crate) fn route_fabric_action(
        &self,
        fabric: &tilo_device_fabric::link::SharedLink,
        envelope: &tilo_device_fabric::PersonalFabricActionEnvelope,
    ) {
        use tilo_device_fabric::PersonalFabricAction;
        if tilo_speaker_service::remote_audio::handle_action(fabric.as_ref(), envelope)
            || tilo_speaker_service::groups::handle_action(fabric.as_ref(), envelope)
        {
            return;
        }
        match &envelope.action {
            PersonalFabricAction::ResumeMedia {
                app_id,
                title,
                locator,
                position_ms,
                resume_playing,
                ..
            } => {
                // What continues on this device follows the same media switch
                // as what leaves it: with media continuity off here, the
                // offer is not made.
                if fabric.require_media().is_ok() {
                    self.offer_media(
                        fabric,
                        envelope,
                        app_id,
                        title,
                        locator,
                        *position_ms,
                        *resume_playing,
                    );
                }
            }
            // The device that offered a session says what became of it, or
            // stops waiting for it: nothing on this session is holding one
            // it offered, so there is nothing to settle here. What the
            // person's devices sent each other before this session ran a
            // fabric — placement, input, accessories, files, notifications,
            // calls, displays, wake, certification — is the shell's
            // device-fabric runtime's to carry, and this session has none of
            // it yet, so it is dropped rather than half-acted on. The speaker
            // services above answer for theirs; these reach here only when
            // their own preconditions refuse, and nothing else on this
            // session acts on them.
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
            | PersonalFabricAction::BluetoothHandoffPrepare { .. }
            | PersonalFabricAction::BluetoothHandoffPrepared { .. }
            | PersonalFabricAction::BluetoothHandoffCommit { .. }
            | PersonalFabricAction::BluetoothHandoffFinished { .. }
            | PersonalFabricAction::BluetoothHandoffAccepted { .. }
            | PersonalFabricAction::BluetoothHandoffPairingOffered { .. }
            | PersonalFabricAction::BluetoothHandoffPairingCommit { .. }
            | PersonalFabricAction::BluetoothHandoffCancel { .. }
            | PersonalFabricAction::NearbyDrop { .. }
            | PersonalFabricAction::NearbyDropResult { .. }
            | PersonalFabricAction::NearbyTransferOffer { .. }
            | PersonalFabricAction::NearbyTransferReady { .. }
            | PersonalFabricAction::NearbyTransferCancel { .. }
            | PersonalFabricAction::NearbyTransferRestart { .. }
            | PersonalFabricAction::NearbyTransferResult { .. }
            | PersonalFabricAction::NotificationProject { .. }
            | PersonalFabricAction::NotificationRetire { .. }
            | PersonalFabricAction::NotificationDismiss { .. }
            | PersonalFabricAction::NotificationInvoke { .. }
            | PersonalFabricAction::RealtimeOffer { .. }
            | PersonalFabricAction::RealtimeAccept { .. }
            | PersonalFabricAction::RealtimeMediaReady { .. }
            | PersonalFabricAction::RealtimeStopSource { .. }
            | PersonalFabricAction::RealtimeCancel { .. }
            | PersonalFabricAction::GamepadForwardRequest { .. }
            | PersonalFabricAction::GamepadForwardGrant { .. }
            | PersonalFabricAction::GamepadForwardEvent { .. }
            | PersonalFabricAction::GamepadForwardStop { .. }
            | PersonalFabricAction::RemoteCaptureSignal { .. }
            | PersonalFabricAction::RemoteCaptureChunk { .. }
            | PersonalFabricAction::DeviceHealthProbe { .. }
            | PersonalFabricAction::DeviceHealthReport { .. }
            | PersonalFabricAction::FabricClockProbe { .. }
            | PersonalFabricAction::FabricClockReport { .. }
            | PersonalFabricAction::Tethering { .. }
            | PersonalFabricAction::WakeDevice { .. }
            | PersonalFabricAction::WakeReport { .. }
            | PersonalFabricAction::WakeProbe { .. }
            | PersonalFabricAction::WakeProbeReply { .. }
            | PersonalFabricAction::WindowViews { .. }
            | PersonalFabricAction::ShowTab { .. }
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

    /// Offer what another device was playing as the now-playing card, so the
    /// person can pick it up here.
    fn offer_media(
        &self,
        fabric: &tilo_device_fabric::link::SharedLink,
        envelope: &tilo_device_fabric::PersonalFabricActionEnvelope,
        app_id: &str,
        title: &str,
        locator: &tilo_device_fabric::MediaSourceLocator,
        position_ms: u64,
        playing: bool,
    ) {
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
        let position_ms = position_ms as f64;
        crate::mount::audio::handoff(
            &self.shared,
            Some(tilo_quick_settings::MediaOffer {
                request_id: hex::encode(envelope.request_id),
                package_id: app_id.to_owned(),
                title: title.to_owned(),
                source_label: label,
                source,
                position_ms,
                playing,
            }),
        );
    }
}
