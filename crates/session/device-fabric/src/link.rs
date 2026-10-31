//! What a service on the person's device fabric needs from the transport
//! that carries it: the fabric as last converged, who is online, and a
//! way to send one device an action.

use std::collections::BTreeSet;

use tilo_network_actor::NearbyTransferCheckpoint;
use tilo_network_actor::fabric::DeviceId;

use crate::{
    ClipboardEntry, DeviceFabric, OwnerSession, PersonalFabricAction, RoamingSetting, WakeRoute,
};

/// One of this device's files, staged in its private Nearby spool, on its way
/// to one of the person's other devices: the checkpoint the stream resumes
/// from, and the object to stream.
#[derive(Debug)]
pub struct NearbySpoolOffer {
    /// Where the receiving device wants the stream to start, as it confirmed
    /// it holds the bytes up to there.
    pub checkpoint: NearbyTransferCheckpoint,
    /// The staged spool object the checkpoint names.
    pub file: std::fs::File,
}

/// Why a personal fabric action did not reach the transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonalFabricSendError {
    /// The transport command queue is momentarily full. The action was not
    /// sent, but the same call can succeed once the actor drains the queue.
    Backpressure,
    /// Identity, enrollment, wire validation or a closed transport refused
    /// the action; retrying it unchanged cannot succeed.
    Rejected(String),
}

impl std::fmt::Display for PersonalFabricSendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backpressure => formatter.write_str("personal fabric transport is busy"),
            Self::Rejected(reason) => formatter.write_str(reason),
        }
    }
}

/// The person's device fabric, as a service on it sees it.
pub trait FabricLink {
    /// The fabric as this device last converged it.
    fn device_fabric(&self) -> DeviceFabric;
    /// The devices with a live session right now.
    fn online_device_ids(&self) -> BTreeSet<DeviceId>;
    /// This device's fabric id and label, once it has a certified identity.
    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)>;
    /// Send `action` to `target`, returning its request id.
    ///
    /// # Errors
    ///
    /// Returns why the transport refused the action.
    fn send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], String>;
    /// Send `action` to `target`, telling a full queue apart from a refusal.
    ///
    /// # Errors
    ///
    /// Returns [`PersonalFabricSendError::Backpressure`] while the queue is
    /// full, otherwise why the transport refused the action.
    fn try_send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError>;
    /// Whether the person lets media move between their devices.
    ///
    /// # Errors
    ///
    /// Returns why media continuity is off.
    fn require_media(&self) -> Result<(), String>;
    /// Offer what this device copied to the person's other devices, as this
    /// device's own entry in its next snapshot.
    ///
    /// A link whose transport does not carry the clipboard yet ignores the
    /// offer; what their devices shared still merges in from
    /// [`Self::device_fabric`].
    fn share_clipboard(&self, entries: Vec<ClipboardEntry>) {
        let _ = entries;
    }
    /// Offer the ways this device's interfaces can be woken to the person's
    /// other devices, as this device's own record in its next snapshot.
    ///
    /// A link whose transport does not carry wake routes yet ignores the
    /// offer; what their devices offered still merges in from
    /// [`Self::device_fabric`].
    fn share_wake_routes(&self, routes: Vec<WakeRoute>) {
        let _ = routes;
    }
    /// Offer the person's settings this device holds to their other devices,
    /// as this device's own records in its next snapshot.
    ///
    /// A link whose transport does not carry settings yet ignores the offer;
    /// what their devices set still merges in from [`Self::device_fabric`].
    fn share_settings(&self, settings: Vec<RoamingSetting>) {
        let _ = settings;
    }
    /// Say the person's session on this device to their other devices, as
    /// this device's own record in its next snapshot: whether it is locked,
    /// when they last locked or unlocked it themselves, and when they last
    /// used it. Their devices read it from [`Self::device_fabric`].
    ///
    /// A link whose transport does not carry this yet ignores it.
    fn publish_owner_session(&self, session: OwnerSession) {
        let _ = session;
    }
    /// Stream one of this device's staged spool objects to `target`: the
    /// bytes of an offer already sent to it, from the checkpoint it confirmed.
    ///
    /// A link whose transport does not carry files yet ignores the stream; the
    /// offer itself (an ordinary personal fabric action) still reaches its
    /// device.
    ///
    /// # Errors
    ///
    /// Returns [`PersonalFabricSendError::Backpressure`] while the transport's
    /// command queue is full, otherwise why it refused the stream.
    fn share_nearby_spool(
        &self,
        target: &DeviceId,
        offer: NearbySpoolOffer,
    ) -> Result<(), PersonalFabricSendError> {
        let _ = (target, offer);
        Ok(())
    }
}

/// A link a service keeps beyond one call, for work it finishes later.
pub type SharedLink = std::sync::Arc<dyn FabricLink + Send + Sync>;

impl<T: FabricLink + ?Sized> FabricLink for std::sync::Arc<T> {
    fn device_fabric(&self) -> DeviceFabric {
        (**self).device_fabric()
    }

    fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        (**self).online_device_ids()
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        (**self).local_device_fabric_identity()
    }

    fn send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], String> {
        (**self).send_personal_fabric_action(target, action)
    }

    fn try_send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError> {
        (**self).try_send_personal_fabric_action(target, action)
    }

    fn require_media(&self) -> Result<(), String> {
        (**self).require_media()
    }

    fn share_clipboard(&self, entries: Vec<ClipboardEntry>) {
        (**self).share_clipboard(entries);
    }

    fn share_settings(&self, settings: Vec<RoamingSetting>) {
        (**self).share_settings(settings);
    }

    fn share_wake_routes(&self, routes: Vec<WakeRoute>) {
        (**self).share_wake_routes(routes);
    }

    fn publish_owner_session(&self, session: OwnerSession) {
        (**self).publish_owner_session(session);
    }

    fn share_nearby_spool(
        &self,
        target: &DeviceId,
        offer: NearbySpoolOffer,
    ) -> Result<(), PersonalFabricSendError> {
        (**self).share_nearby_spool(target, offer)
    }
}
