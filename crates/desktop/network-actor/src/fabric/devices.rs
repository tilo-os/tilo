//! The person's devices as one of them knows them.
//!
//! What lasts is in the person's own store, each device's in its own part, which that device alone
//! writes: what it is ([`DeviceAbout`]), its speaker groups
//! ([`tilo_workspace_service::workspaces::person_workspace::speaker`]), and what its daemon keeps
//! of its disk unlock.
//! What holds only while a device is there is said on its session with each of the others and kept
//! nowhere: that it is there.
//!
//! [`PersonDevices`] is the two read together: each device the person's store lists, with what it
//! wrote there and what it last said live.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::fabric::{DeviceId, DeviceKind, FabricCapabilities, FabricError};

/// What a device says it is, in its own part of the person's store: the name the person knows it
/// by, what kind of device it is, and what it does for their other devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceAbout {
    pub label: String,
    pub kind: DeviceKind,
    pub capabilities: FabricCapabilities,
}

impl DeviceAbout {
    /// A device of the person's that wrote nothing of itself yet: named as its certificate names
    /// it, and offering nothing.
    #[must_use]
    pub fn named(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            kind: DeviceKind::Desktop,
            capabilities: FabricCapabilities::default(),
        }
    }

    /// # Errors
    /// [`FabricError::InvalidLabel`] for a name no one could read.
    pub fn validate(&self) -> Result<(), FabricError> {
        if self.label.len() > 128 || self.label.chars().any(char::is_control) {
            return Err(FabricError::InvalidLabel);
        }
        Ok(())
    }
}

/// One of the person's devices, as this one knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonDevice {
    pub device: DeviceId,
    /// What it wrote of itself in the person's store.
    pub about: DeviceAbout,
    /// Whether it is on a session with this device now. This device is always online.
    pub online: bool,
    /// When it last said anything live.
    pub last_seen_unix_ms: u64,
}

impl PersonDevice {
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

/// The person's devices as this one knows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonDevices {
    devices: BTreeMap<DeviceId, PersonDevice>,
}

impl PersonDevices {
    #[must_use]
    pub fn new(devices: impl IntoIterator<Item = PersonDevice>) -> Self {
        Self {
            devices: devices
                .into_iter()
                .map(|device| (device.device.clone(), device))
                .collect(),
        }
    }

    /// Add `device`, or replace what was known of it.
    pub fn insert(&mut self, device: PersonDevice) {
        self.devices.insert(device.device.clone(), device);
    }

    #[must_use]
    pub fn devices(&self) -> &BTreeMap<DeviceId, PersonDevice> {
        &self.devices
    }

    #[must_use]
    pub fn get(&self, device: &DeviceId) -> Option<&PersonDevice> {
        self.devices.get(device)
    }

    #[must_use]
    pub fn contains(&self, device: &DeviceId) -> bool {
        self.devices.contains_key(device)
    }

    pub fn online_devices(&self) -> impl Iterator<Item = &PersonDevice> {
        self.devices.values().filter(|device| device.online)
    }
}
