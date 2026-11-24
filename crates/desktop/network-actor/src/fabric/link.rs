//! What a service on the person's devices needs from the session that reaches
//! them: the devices as this one knows them, who is online, and a way to send
//! one device an action.

use std::collections::BTreeSet;

use tilo_workspace_service::workspaces::person_workspace::devices::Devices;

use crate::fabric::DeviceId;

use crate::fabric::{ClipboardEntry, PersonDevices, PersonalFabricAction};

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
    /// The person's devices as this one knows them: what each keeps in their store and what
    /// each said live.
    fn devices(&self) -> PersonDevices;
    /// The record of the person's devices in their own workspace, as this device's daemon last
    /// read it: what every device of theirs keeps, and reads, of their speaker groups.
    fn person_devices(&self) -> Option<Devices> {
        None
    }
    /// Apply `action`, a typed action on the person's devices record, through this device's
    /// daemon, as this device.
    ///
    /// # Errors
    /// This session holds no copy of the person's workspace, or the daemon refused the action.
    fn apply_person_devices(&self, action: tilo_resource::Invoke<Devices>) -> Result<(), String> {
        let _ = action;
        Err("this device holds no record of the person's devices".to_owned())
    }
    /// The devices with a live session right now.
    fn online_device_ids(&self) -> BTreeSet<DeviceId>;
    /// The person's other devices heard on this device's local network now,
    /// so reachable without a server.
    fn nearby_device_ids(&self) -> BTreeSet<DeviceId> {
        BTreeSet::new()
    }
    /// This device's fabric id and label, once it has a certified identity.
    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)>;
    /// The device this session's fabric knows to speak from `endpoint`, when
    /// it proved itself there: what something heard at a bare endpoint is
    /// named by.
    fn device_behind(&self, endpoint: iroh::EndpointId) -> Option<DeviceId> {
        let _ = endpoint;
        None
    }
    /// Send `action` to `target`, telling a full queue apart from a refusal.
    /// # Errors
    /// [`PersonalFabricSendError::Backpressure`] while the queue is full, otherwise why
    /// the transport refused the action.
    fn try_send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError>;
    /// Whether the person lets media move between their devices.
    /// # Errors
    /// Why media continuity is off.
    fn require_media(&self) -> Result<(), String>;
    /// Offer what this device copies to the person's other devices that are there now. A
    /// link whose transport does not carry the clipboard ignores the offer; what their
    /// devices offer is in [`Self::devices`].
    fn share_clipboard(&self, entries: Vec<ClipboardEntry>) {
        let _ = entries;
    }
}

/// A link a service keeps beyond one call, for work it finishes later.
pub type SharedLink = std::sync::Arc<dyn FabricLink + Send + Sync>;

impl<T: FabricLink + ?Sized> FabricLink for std::sync::Arc<T> {
    fn devices(&self) -> PersonDevices {
        (**self).devices()
    }

    fn person_devices(&self) -> Option<Devices> {
        (**self).person_devices()
    }

    fn apply_person_devices(&self, action: tilo_resource::Invoke<Devices>) -> Result<(), String> {
        (**self).apply_person_devices(action)
    }

    fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        (**self).online_device_ids()
    }

    fn nearby_device_ids(&self) -> BTreeSet<DeviceId> {
        (**self).nearby_device_ids()
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        (**self).local_device_fabric_identity()
    }

    fn device_behind(&self, endpoint: iroh::EndpointId) -> Option<DeviceId> {
        (**self).device_behind(endpoint)
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
}
