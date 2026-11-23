//! The person's devices as one of them knows them.
//!
//! What lasts is in the person's own store, each device's in its own part, which that device alone
//! writes: what it is ([`DeviceAbout`]), the speaker groups it leads and joined
//! ([`SpeakerFabricState`]), and its disk-unlock enrolments ([`UnlockFabricState`]). The Wi-Fi
//! passphrases they share are the person's, sealed to each of their devices ([`WifiCredential`]).
//! What holds only while a device is there is said on its session with each of the others and kept
//! nowhere: whether the person's session on it is locked, how it is woken and where its displays
//! stand ([`DevicePresence`]), and what was copied on it.
//!
//! [`PersonDevices`] is the two read together: each device the person's store lists, with what it
//! wrote there and what it last said live.

use std::collections::BTreeMap;

use tilo_continuity::clipboard::{ClipboardEntry, ClipboardHistory};
use tilo_continuity::wifi::WifiCredential;
use tilo_fabric::unlock::UnlockFabricState;
use tilo_fabric::{DeviceAbout, DeviceId, DevicePresence, FabricError};

use crate::fabric::speaker_group::{
    SpeakerFabricState, SpeakerGroup, SpeakerGroupId, SpeakerMemberStatus,
};

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

/// The person's devices as this one knows them, and the Wi-Fi passphrases they share.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonDevices {
    devices: BTreeMap<DeviceId, PersonDevice>,
    wifi: Vec<WifiCredential>,
}

impl PersonDevices {
    #[must_use]
    pub fn new(devices: impl IntoIterator<Item = PersonDevice>, wifi: Vec<WifiCredential>) -> Self {
        Self {
            devices: devices
                .into_iter()
                .map(|device| (device.device.clone(), device))
                .collect(),
            wifi,
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

    /// The Wi-Fi passphrases the person's devices shared, each sealed to the devices that may
    /// open it.
    #[must_use]
    pub fn wifi(&self) -> &[WifiCredential] {
        &self.wifi
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

    /// The devices as the fabric reads them: what each is and last said, and whether it is here.
    #[must_use]
    pub fn listed(&self) -> tilo_fabric::Devices {
        tilo_fabric::Devices::new(self.devices.values().map(|device| tilo_fabric::Device {
            device: device.device.clone(),
            about: device.about.clone(),
            online: device.online,
            nearby: false,
            last_seen_unix_ms: device.last_seen_unix_ms,
            presence: device.presence.clone(),
        }))
    }

    /// Every copy the person's devices offer, newest first, as one bounded history.
    #[must_use]
    pub fn merged_clipboard(&self) -> ClipboardHistory {
        tilo_continuity::clipboard::merged(
            self.devices
                .values()
                .flat_map(|device| device.clipboard.iter().cloned()),
        )
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

#[cfg(test)]
#[path = "devices_tests.rs"]
mod tests;
