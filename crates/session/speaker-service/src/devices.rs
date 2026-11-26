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

use tilo_network_actor::fabric::link::FabricLink;
use tilo_network_actor::fabric::{ABOUT_FACT, DeviceAbout, DeviceId};
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

/// The person's devices as the speaker service reaches them: the transport between them and the
/// record of them.
pub trait Speakers: FabricLink + Record + Send + Sync {
    /// The person's devices as this link knows them now: the record read with who is online.
    fn known(&self) -> Listing;
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

impl ListedDevice {
    /// `device`, as it says it is, before anything else is known of it.
    #[must_use]
    pub fn new(device: DeviceId, about: DeviceAbout) -> Self {
        Self {
            device,
            about,
            online: false,
            last_seen_unix_ms: 0,
        }
    }
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

    pub fn online_devices(&self) -> impl Iterator<Item = &ListedDevice> {
        self.devices.values().filter(|device| device.online)
    }
}

/// The devices `network` knows now: the record their daemon keeps, and the devices online on its
/// transport.
#[must_use]
pub fn known_from_network(network: &dyn Speakers) -> Listing {
    let Some(record) = network.record() else {
        return Listing::default();
    };
    let local = network
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let online = network.online_device_ids();
    known_from(&record, &network.person(), local.as_ref(), &online)
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
