//! What a device says of itself while it is there, and the person's devices read as one list:
//! their copies and their speakers.

use super::*;
use crate::fabric::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerGroupMember, SpeakerMembership,
    SpeakerRole, SpeakerTrim, StereoSide,
};
use crate::fabric::{ClipboardPayload, FabricStamp, WakeInterfaceKind};

fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}

fn stamp(device: &str, counter: u64) -> FabricStamp {
    FabricStamp {
        counter,
        device: id(device),
    }
}

#[test]
fn a_device_says_only_wake_routes_it_could_have() {
    let route = |mac: [u8; 6]| WakeRoute {
        mac,
        kind: WakeInterfaceKind::Ethernet,
        magic_packet: true,
        subnet: None,
    };
    let presence = DevicePresence {
        wake_routes: vec![route([0x02; 6])],
    };
    assert_eq!(presence.validate(), Ok(()));

    let mut crowded = presence.clone();
    crowded.wake_routes = vec![route([0x02; 6]); MAX_WAKE_ROUTES + 1];
    assert_eq!(crowded.validate(), Err(FabricError::TooLarge));

    let mut multicast = presence.clone();
    multicast.wake_routes = vec![route([0x01; 6])];
    assert_eq!(multicast.validate(), Err(FabricError::InvalidWakeRoute));

    let mut repeated = presence;
    repeated.wake_routes = vec![route([0x02; 6]), route([0x02; 6])];
    assert_eq!(repeated.validate(), Err(FabricError::DuplicateItem));
}

fn clipboard(text: &str, origin: &DeviceId, created_unix_ms: u64) -> ClipboardEntry {
    ClipboardEntry::shareable(
        ClipboardPayload::Text(text.into()),
        origin.clone(),
        created_unix_ms,
        stamp(origin.as_str(), 1),
    )
    .unwrap()
}

fn speaker(selector: [u8; 16]) -> AudioEndpoint {
    AudioEndpoint {
        selector: EndpointSelector(selector),
        kind: AudioEndpointKind::Output,
        label: "Speakers".into(),
        is_default: true,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture keeps each device's copies and speakers in view"
)]
fn the_persons_devices_read_their_copies_and_speakers() {
    let laptop = id("laptop");
    let tablet = id("tablet");
    let group_id = SpeakerGroupId([5; 16]);
    let output = EndpointSelector([9; 16]);
    let member = |device: &DeviceId, side| SpeakerGroupMember {
        device: device.clone(),
        output,
        role: SpeakerRole::Stereo(side),
        trim: SpeakerTrim::default(),
    };

    let mut laptop_device = PersonDevice::new(
        laptop.clone(),
        DeviceAbout {
            label: "Laptop".into(),
            kind: DeviceKind::Laptop,
            capabilities: FabricCapabilities {
                audio_group: true,
                ..FabricCapabilities::default()
            },
        },
    );
    laptop_device.online = true;
    laptop_device.clipboard = vec![clipboard("old", &laptop, 10)];
    laptop_device.speakers = SpeakerFabricState {
        endpoints: vec![speaker([9; 16])],
        groups: vec![SpeakerGroup {
            id: group_id,
            name: "Studio pair".into(),
            leader: laptop.clone(),
            members: vec![
                member(&laptop, StereoSide::Left),
                member(&tablet, StereoSide::Right),
            ],
            stamp: stamp("laptop", 1),
        }],
        ..SpeakerFabricState::default()
    };

    let mut tablet_device = PersonDevice::new(tablet.clone(), DeviceAbout::named("Tablet"));
    tablet_device.clipboard = vec![clipboard("new", &tablet, 20)];

    let devices = PersonDevices::new(vec![laptop_device.clone(), tablet_device.clone()]);
    let group = devices.speaker_group(group_id).unwrap();
    assert_eq!(
        devices.speaker_member_states(group),
        vec![SpeakerMemberStatus::Ready, SpeakerMemberStatus::Unavailable,],
        "the tablet cannot play in a group"
    );
    assert_eq!(
        devices.admit_speaker_group(group),
        Err(FabricError::IneligibleSpeakerMember)
    );

    let merged: Vec<_> = devices
        .merged_clipboard()
        .entries()
        .map(|entry| entry.origin.as_str().to_owned())
        .collect();
    assert_eq!(merged, vec!["tablet", "laptop"], "newest copy first");

    let mut certified = tablet_device;
    certified.about.capabilities.audio_group = true;
    certified.speakers = SpeakerFabricState {
        endpoints: vec![speaker([9; 16])],
        memberships: vec![SpeakerMembership {
            group: group_id,
            leader: laptop.clone(),
            output,
            role: SpeakerRole::Stereo(StereoSide::Right),
            stamp: stamp("tablet", 1),
        }],
        ..SpeakerFabricState::default()
    };
    let devices = PersonDevices::new(vec![laptop_device.clone(), certified.clone()]);
    let group = devices.speaker_group(group_id).unwrap();
    assert_eq!(
        devices.speaker_member_states(group),
        vec![SpeakerMemberStatus::Ready, SpeakerMemberStatus::Offline]
    );
    assert_eq!(devices.admit_speaker_group(group), Ok(()));

    certified.online = true;
    let devices = PersonDevices::new(vec![laptop_device, certified]);
    let group = devices.speaker_group(group_id).unwrap();
    assert_eq!(
        devices.speaker_member_states(group),
        vec![SpeakerMemberStatus::Ready, SpeakerMemberStatus::Ready]
    );

    let stranger = SpeakerGroup {
        id: SpeakerGroupId([6; 16]),
        name: "Studio pair".into(),
        leader: laptop.clone(),
        members: vec![
            member(&laptop, StereoSide::Left),
            member(&id("stranger"), StereoSide::Right),
        ],
        stamp: stamp("laptop", 1),
    };
    assert_eq!(
        devices.admit_speaker_group(&stranger),
        Err(FabricError::UnknownDevice)
    );
}
