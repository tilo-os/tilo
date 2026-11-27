//! The person's devices as the services that reach them read them: the record of them in the
//! person's own workspace, which the device's daemon keeps, read with who is online now.
//!
//! There is no second copy. What lasts is the record ([`Devices`]): each device's own part of it,
//! which that device alone writes, holding what it is ([`DeviceAbout`]) and its speaker groups
//! ([`tilo_workspace_service::workspaces::person_workspace::speaker`]). What holds only while a
//! device is there is live, from the transport: that it is online.
//!
//! [`Listing`] is the two read together, derived on each read: each device the record lists, with
//! what it wrote there and whether it is online now.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tilo_network_actor::fabric::{ABOUT_FACT, DeviceAbout, DeviceId};
use tilo_network_actor::fabric::{PersonalFabricAction, PersonalFabricActionEnvelope};
use tilo_resource::Invoke;
use tilo_workspace_service::workspaces::person_workspace::devices::{Devices, KeyKind};

/// The person's record of their devices, as the device's daemon has it: what this device reads
/// and what its changes go through.
pub trait Record {
    /// The record as the daemon last read it into this device: `None` until it has been read.
    fn record(&self) -> Option<Devices>;
    /// Apply `action`, a typed action on the record, through this device's daemon.
    /// # Errors
    /// The daemon refused the action, or this session holds no copy of the record.
    fn apply(&self, action: Invoke<Devices>) -> Result<(), String>;
    /// The person whose devices these are: the root their certificates chain to.
    fn person(&self) -> [u8; 32];
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

/// The person's devices as the speaker service reaches them: the record of them and the transport
/// between them, which the device's daemon carries.
pub trait Speakers: Record + Send + Sync {
    /// The person's devices as this link knows them now: the record read with who is online.
    fn known(&self) -> Listing;

    /// The envelopes the person's other devices sent this one since the last call, each with the
    /// device that sent it: what the clocks and the routes read.
    fn take_heard(&self) -> Vec<(DeviceId, PersonalFabricActionEnvelope, u64)> {
        Vec::new()
    }

    /// The devices with a live session right now.
    fn online_device_ids(&self) -> BTreeSet<DeviceId>;

    /// The person's other devices heard on this device's local network now, so reachable without
    /// a server. Empty where the transport does not say.
    fn nearby_device_ids(&self) -> BTreeSet<DeviceId> {
        BTreeSet::new()
    }

    /// This device's fabric id and label, once it has a certified identity.
    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)>;

    /// Send `action` to `target`, telling a full queue apart from a refusal.
    /// # Errors
    /// [`PersonalFabricSendError::Backpressure`] while the queue is full, otherwise why the
    /// transport refused the action.
    fn try_send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError>;

    /// Whether the person lets media move between their devices.
    /// # Errors
    /// Why media continuity is off.
    fn require_media(&self) -> Result<(), String>;
}

/// A link a service keeps beyond one call, for work it finishes later.
pub type SharedLink = Arc<dyn Speakers>;

impl<T: Record + ?Sized> Record for Arc<T> {
    fn record(&self) -> Option<Devices> {
        (**self).record()
    }

    fn apply(&self, action: Invoke<Devices>) -> Result<(), String> {
        (**self).apply(action)
    }

    fn person(&self) -> [u8; 32] {
        (**self).person()
    }
}

impl<T: Speakers + ?Sized> Speakers for Arc<T> {
    fn known(&self) -> Listing {
        (**self).known()
    }

    fn take_heard(&self) -> Vec<(DeviceId, PersonalFabricActionEnvelope, u64)> {
        (**self).take_heard()
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
}

/// One of the person's devices, as this one knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedDevice {
    pub device: DeviceId,
    /// What it wrote of itself in the person's record.
    pub about: DeviceAbout,
    /// Whether it is on a session with this device now. This device is always online.
    pub online: bool,
    /// When it last said anything of itself in the record.
    pub last_seen_unix_ms: u64,
}

/// The person's devices as this one knows them, derived from the record and who is online now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    devices: BTreeMap<DeviceId, ListedDevice>,
}

impl Listing {
    #[must_use]
    pub fn new(devices: impl IntoIterator<Item = ListedDevice>) -> Self {
        Self {
            devices: devices
                .into_iter()
                .map(|device| (device.device.clone(), device))
                .collect(),
        }
    }

    /// Add `device`, or replace what was known of it.
    pub fn insert(&mut self, device: ListedDevice) {
        self.devices.insert(device.device.clone(), device);
    }

    #[must_use]
    pub fn devices(&self) -> &BTreeMap<DeviceId, ListedDevice> {
        &self.devices
    }

    #[must_use]
    pub fn get(&self, device: &DeviceId) -> Option<&ListedDevice> {
        self.devices.get(device)
    }

    #[must_use]
    pub fn contains(&self, device: &DeviceId) -> bool {
        self.devices.contains_key(device)
    }

}

/// The devices `record` lists for `person`, `local` this one and `online` those on a session with
/// it now.
#[must_use]
pub fn known_from(
    record: &Devices,
    person: &[u8; 32],
    local: Option<&DeviceId>,
    online: &BTreeSet<DeviceId>,
) -> Listing {
    Listing::new(
        record
            .listed(person)
            .into_iter()
            .filter(|listed| {
                listed.device.kind == KeyKind::Device && !listed.device.revoked(&listed.cert)
            })
            .filter_map(|listed| {
                let device = DeviceId::new(listed.key).ok()?;
                let about = listed
                    .device
                    .facts
                    .get(ABOUT_FACT)
                    .and_then(|value| {
                        postcard::from_bytes::<DeviceAbout>(&hex::decode(value).ok()?).ok()
                    })
                    .unwrap_or_else(|| DeviceAbout::named(listed.device.called(&listed.cert)));
                Some(ListedDevice {
                    online: local == Some(&device) || online.contains(&device),
                    device,
                    about,
                    last_seen_unix_ms: listed
                        .device
                        .seen_ms()
                        .and_then(|ms| u64::try_from(ms).ok())
                        .unwrap_or(0),
                })
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The record lists what each device wrote of itself, with this one online and another not.
    #[test]
    fn a_listing_is_the_record_with_who_is_online_now() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let person = root.verifying_key().to_bytes();
        let local = DeviceId::new(hex::encode([5; 32])).expect("a device id");
        let other = DeviceId::new(hex::encode([6; 32])).expect("a device id");
        let mut record = tilo_resource::Replica::<Devices>::new().in_app("so.tilo.model", "Model");
        for (key, label) in [([5; 32], "Laptop"), ([6; 32], "Tablet")] {
            let mut cert = tilo_user_identity::DeviceCert {
                user_public_key: person,
                device_public_key: key,
                device_label: label.into(),
                capabilities: tilo_user_identity::DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut cert, &root)
                .expect("the device certificate signs");
            record
                .apply(Devices::keeping(&cert).expect("the device certificate encodes"))
                .expect("the device is kept");
        }
        let listed = known_from(
            &record.state().expect("the record reads"),
            &person,
            Some(&local),
            &BTreeSet::new(),
        );
        assert_eq!(listed.get(&local).map(|device| device.online), Some(true));
        assert_eq!(listed.get(&other).map(|device| device.online), Some(false));
        assert_eq!(
            listed.get(&other).map(|device| device.about.label.as_str()),
            Some("Tablet"),
            "what the device wrote of itself names it"
        );
    }
}
