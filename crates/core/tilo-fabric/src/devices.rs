//! The person's devices as one of them knows them: what each wrote of itself in the person's store
//! ([`DeviceAbout`]), whether it is here now and on this device's network, and what it last said
//! while it was here ([`DevicePresence`]).

use std::collections::BTreeMap;

use serde::Serialize;

use crate::{DeviceAbout, DeviceId, DevicePresence};

/// One of the person's devices, as this one knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub device: DeviceId,
    /// What it wrote of itself in the person's store.
    pub about: DeviceAbout,
    /// Whether it is reachable from this device now, and said itself. This device always is.
    pub online: bool,
    /// Whether it was heard on this device's local network.
    pub nearby: bool,
    /// When it last said anything live.
    pub last_seen_unix_ms: u64,
    /// What it last said live; empty for a device that said nothing yet.
    pub presence: DevicePresence,
}

impl Device {
    /// `device`, as it says it is, before anything else is known of it.
    #[must_use]
    pub fn new(device: DeviceId, about: DeviceAbout) -> Self {
        Self {
            device,
            about,
            online: false,
            nearby: false,
            last_seen_unix_ms: 0,
            presence: DevicePresence::default(),
        }
    }

    /// Whether a magic packet can wake it through an interface it said it has.
    #[must_use]
    pub fn wakeable(&self) -> bool {
        self.presence
            .wake_routes
            .iter()
            .any(|route| route.magic_packet)
    }
}

/// The person's devices as this one knows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Devices {
    devices: BTreeMap<DeviceId, Device>,
}

impl Devices {
    #[must_use]
    pub fn new(devices: impl IntoIterator<Item = Device>) -> Self {
        Self {
            devices: devices
                .into_iter()
                .map(|device| (device.device.clone(), device))
                .collect(),
        }
    }

    /// Add `device`, or replace what was known of it.
    pub fn insert(&mut self, device: Device) {
        self.devices.insert(device.device.clone(), device);
    }

    #[must_use]
    pub fn devices(&self) -> &BTreeMap<DeviceId, Device> {
        &self.devices
    }

    #[must_use]
    pub fn get(&self, device: &DeviceId) -> Option<&Device> {
        self.devices.get(device)
    }

    pub fn get_mut(&mut self, device: &DeviceId) -> Option<&mut Device> {
        self.devices.get_mut(device)
    }

    #[must_use]
    pub fn contains(&self, device: &DeviceId) -> bool {
        self.devices.contains_key(device)
    }

    pub fn online_devices(&self) -> impl Iterator<Item = &Device> {
        self.devices.values().filter(|device| device.online)
    }

    /// The name the person knows `device` by: what it wrote of itself, else its id.
    #[must_use]
    pub fn label_of(&self, device: &DeviceId) -> String {
        self.devices.get(device).map_or_else(
            || device.as_str().to_owned(),
            |device| device.about.label.clone(),
        )
    }

    /// The devices as a person reads them: this device first, then those here, then by name.
    #[must_use]
    pub fn listed(&self, local: &DeviceId) -> Vec<ListedDevice> {
        let mut listed: Vec<ListedDevice> = self
            .devices
            .values()
            .map(|device| ListedDevice {
                id: device.device.as_str().to_owned(),
                label: device.about.label.clone(),
                kind: device.about.kind,
                local: &device.device == local,
                online: device.online,
                nearby: device.nearby,
                wakeable: device.wakeable(),
                display: device.about.capabilities.display,
                audio_output: device.about.capabilities.audio_output,
                displays: device.presence.display.clone(),
                last_seen_unix_ms: device.last_seen_unix_ms,
            })
            .collect();
        listed.sort_by(|left, right| {
            right
                .local
                .cmp(&left.local)
                .then_with(|| right.online.cmp(&left.online))
                .then_with(|| left.label.cmp(&right.label))
        });
        listed
    }
}

/// One row of the `devices` resource: one of the person's devices as a person reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is a fact about the device a reader shows on its own"
)]
pub struct ListedDevice {
    pub id: String,
    pub label: String,
    pub kind: crate::DeviceKind,
    /// Whether it is the device answering.
    pub local: bool,
    pub online: bool,
    pub nearby: bool,
    /// Whether a magic packet can wake it.
    pub wakeable: bool,
    /// Whether it has a screen.
    pub display: bool,
    /// Whether it plays sound.
    pub audio_output: bool,
    /// Where its screens stand, as it last said.
    pub displays: Option<crate::DeviceDisplay>,
    pub last_seen_unix_ms: u64,
}
