//! The person's devices as one of them knows them.
//!
//! What lasts is in the person's own store, each device's in its own part, which that device alone
//! writes: what it is ([`DeviceAbout`]), the speaker groups it leads and joined
//! ([`SpeakerFabricState`]), and its disk-unlock enrolments ([`UnlockFabricState`]).
//! What holds only while a device is there is said on its session with each of the others and kept
//! nowhere: whether the person's session on it is locked, how it is woken and where its displays
//! stand ([`DevicePresence`]), and what was copied on it.
//!
//! [`PersonDevices`] is the two read together: each device the person's store lists, with what it
//! wrote there and what it last said live.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::fabric::model::unique;
use crate::fabric::speaker_group::{
    SpeakerFabricState, SpeakerGroup, SpeakerGroupId, SpeakerMemberStatus,
};
use crate::fabric::unlock::UnlockFabricState;
use crate::fabric::{
    ClipboardEntry, ClipboardHistory, DeviceId, DeviceKind, DisplayConnectorPlacement,
    DisplayOutputObservation, DisplayRect, FabricCapabilities, FabricError, MAX_WAKE_ROUTES,
    OwnerSession, WakeRoute,
};

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

/// Where a device's displays stand and what the hardware behind each says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDisplay {
    /// The area its outputs make together.
    pub rect: DisplayRect,
    pub scale_milli: u16,
    /// Where each connector's output stands.
    pub connectors: Vec<DisplayConnectorPlacement>,
    /// What the hardware behind each output says: every placed connector has its active output
    /// here, at the same place and scale.
    pub outputs: Vec<DisplayOutputObservation>,
}

impl DeviceDisplay {
    /// # Errors
    /// [`FabricError::InvalidDisplayPlacement`] when the area, a connector or an output is out of
    /// bounds, or the outputs do not describe the connectors.
    pub fn validate(&self) -> Result<(), FabricError> {
        let invalid = Err(FabricError::InvalidDisplayPlacement);
        if self.rect.width == 0
            || self.rect.height == 0
            || !(500..=4000).contains(&self.scale_milli)
            || self.connectors.len() > 32
            || self.outputs.len() > settings_store::person_displays::MAX_DEVICE_OUTPUTS
        {
            return invalid;
        }
        unique(
            self.connectors
                .iter()
                .map(|output| output.connector.as_str()),
        )?;
        unique(self.outputs.iter().map(|item| item.connector.as_str()))?;
        unique(self.outputs.iter().map(|item| item.output.id.as_str()))?;
        for connector in &self.connectors {
            if !valid_name(&connector.connector)
                || connector.label.len() > 256
                || connector.label.chars().any(char::is_control)
                || connector.rect.logical_placement().is_none()
                || !(500..=4000).contains(&connector.scale_milli)
            {
                return invalid;
            }
        }
        for item in &self.outputs {
            if !valid_name(&item.connector) || !item.output.valid() {
                return invalid;
            }
            let placed = self
                .connectors
                .iter()
                .find(|connector| connector.connector == item.connector);
            let output = &item.output;
            match placed {
                Some(connector) if output.active() => {
                    let rect = DisplayRect {
                        x: output.x,
                        y: output.y,
                        width: output.width,
                        height: output.height,
                    };
                    if connector.rect != rect || connector.scale_milli != output.scale_milli {
                        return invalid;
                    }
                }
                Some(_) => return invalid,
                None if output.active() => return invalid,
                None => {}
            }
        }
        if self.connectors.iter().any(|connector| {
            !self
                .outputs
                .iter()
                .any(|item| item.connector == connector.connector && item.output.active())
        }) {
            return invalid;
        }
        Ok(())
    }
}

/// What a device says of itself while it is there, on its session with each of the person's other
/// devices. Nothing keeps it: once the session ends, the device is gone until it says it again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePresence {
    /// How siblings on the same LAN can wake it.
    pub wake_routes: Vec<WakeRoute>,
    /// The person's session on it, which their other devices' locks follow.
    pub session: OwnerSession,
    /// Its displays, when it offers them.
    pub display: Option<DeviceDisplay>,
}

impl DevicePresence {
    /// # Errors
    /// Why this is not what a device could say of itself.
    pub fn validate(&self) -> Result<(), FabricError> {
        if self.wake_routes.len() > MAX_WAKE_ROUTES {
            return Err(FabricError::TooLarge);
        }
        if !self.wake_routes.iter().all(WakeRoute::valid) {
            return Err(FabricError::InvalidWakeRoute);
        }
        unique(self.wake_routes.iter().map(|route| route.mac))?;
        self.display
            .as_ref()
            .map_or(Ok(()), DeviceDisplay::validate)
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
    /// What it last said live; empty for a device that said nothing yet.
    pub presence: DevicePresence,
    /// The copies it offers, as it last offered them.
    pub clipboard: Vec<ClipboardEntry>,
    /// Its speaker groups, memberships, outputs and microphones.
    pub speakers: SpeakerFabricState,
    /// Its disk-unlock enrolments, the auto-approvals made on it and its answers to boots.
    pub unlock: UnlockFabricState,
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
            presence: DevicePresence::default(),
            clipboard: Vec::new(),
            speakers: SpeakerFabricState::default(),
            unlock: UnlockFabricState::default(),
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

    /// The person's devices and the outputs each says it shows, as the shared placement lays
    /// them out.
    #[must_use]
    pub fn person_devices(
        &self,
        local: &DeviceId,
    ) -> Vec<settings_store::person_displays::PersonDevice> {
        self.devices
            .values()
            .map(|device| settings_store::person_displays::PersonDevice {
                device: device.device.as_str().to_owned(),
                label: device.about.label.clone(),
                local: &device.device == local,
                online: device.online,
                outputs: device
                    .presence
                    .display
                    .iter()
                    .flat_map(|display| &display.outputs)
                    .map(|item| item.output.clone())
                    .collect(),
            })
            .collect()
    }

    /// Every copy the person's devices offer, newest first, as one bounded history.
    #[must_use]
    pub fn merged_clipboard(&self) -> ClipboardHistory {
        let mut items: Vec<_> = self
            .devices
            .values()
            .flat_map(|device| device.clipboard.iter().cloned())
            .collect();
        // Oldest first: each copy goes in front of the ones before it, and a full history lets
        // the oldest go.
        items.sort_by(|a, b| (a.created_unix_ms, &a.stamp).cmp(&(b.created_unix_ms, &b.stamp)));
        let mut history = ClipboardHistory::default();
        for item in items {
            if history.push(item).is_err() {
                break;
            }
        }
        history
    }

    /// Every group any of the person's devices leads, the newest of each.
    #[must_use]
    pub fn speaker_groups(&self) -> Vec<&SpeakerGroup> {
        let mut winners = BTreeMap::<SpeakerGroupId, &SpeakerGroup>::new();
        for group in self
            .devices
            .values()
            .flat_map(|device| &device.speakers.groups)
        {
            if winners
                .get(&group.id)
                .is_none_or(|current| group.stamp.is_newer_than(&current.stamp))
            {
                winners.insert(group.id, group);
            }
        }
        let mut groups = winners.into_values().collect::<Vec<_>>();
        groups.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        groups
    }

    #[must_use]
    pub fn speaker_group(&self, id: SpeakerGroupId) -> Option<&SpeakerGroup> {
        self.speaker_groups()
            .into_iter()
            .find(|group| group.id == id)
    }

    /// Where each member of `group` stands, in member order.
    #[must_use]
    pub fn speaker_member_states(&self, group: &SpeakerGroup) -> Vec<SpeakerMemberStatus> {
        group
            .members
            .iter()
            .map(|member| {
                let Some(device) = self.devices.get(&member.device) else {
                    return SpeakerMemberStatus::NotCertified;
                };
                if member.device != group.leader && !device.about.capabilities.audio_group {
                    return SpeakerMemberStatus::Unavailable;
                }
                if device.speakers.output(member.output).is_none() {
                    return SpeakerMemberStatus::OutputMissing;
                }
                let consented = member.device == group.leader
                    || device
                        .speakers
                        .memberships
                        .iter()
                        .any(|membership| membership.covers(group, member));
                if !consented {
                    SpeakerMemberStatus::AwaitingConsent
                } else if device.online {
                    SpeakerMemberStatus::Ready
                } else {
                    SpeakerMemberStatus::Offline
                }
            })
            .collect()
    }

    /// Admission rules for a group its leader is about to keep: every member is one of the
    /// person's devices, can join speaker groups (the leader's own outputs always can), and offers
    /// the named output.
    /// # Errors
    /// [`FabricError::InvalidSpeakerGroup`] for a structural problem,
    /// [`FabricError::UnknownDevice`] for a device that is not the person's, and
    /// [`FabricError::IneligibleSpeakerMember`] for a device that cannot join or does not offer
    /// that output.
    pub fn admit_speaker_group(&self, group: &SpeakerGroup) -> Result<(), FabricError> {
        group.validate(&group.leader)?;
        for member in &group.members {
            let device = self
                .devices
                .get(&member.device)
                .ok_or(FabricError::UnknownDevice)?;
            if (member.device != group.leader && !device.about.capabilities.audio_group)
                || device.speakers.output(member.output).is_none()
            {
                return Err(FabricError::IneligibleSpeakerMember);
            }
        }
        Ok(())
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
}

#[cfg(test)]
#[path = "devices_tests.rs"]
mod tests;
