//! The person's devices read as one list: their screens, their copies and their speakers.

use super::*;
use crate::fabric::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerGroupMember, SpeakerMembership,
    SpeakerRole, SpeakerTrim, StereoSide,
};
use tilo_continuity::clipboard::ClipboardPayload;
use tilo_fabric::{
    DeviceDisplay, DeviceKind, DisplayConnectorPlacement, DisplayOutputObservation, DisplayRect,
    FabricCapabilities, FabricStamp,
};

fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}

fn stamp(device: &str, counter: u64) -> FabricStamp {
    FabricStamp {
        counter,
        device: id(device),
    }
}

/// What the hardware behind connector `DP-1` says, for a screen at `x`.
fn observed(output_id: &str, x: i32, scale_milli: u16) -> DisplayOutputObservation {
    DisplayOutputObservation {
        connector: "DP-1".into(),
        output: settings_store::person_displays::DeviceOutput {
            id: output_id.into(),
            name: "Panel".into(),
            builtin: false,
            x,
            y: 0,
            width: 1000,
            height: 800,
            mode_width: 2000,
            mode_height: 1600,
            refresh_mhz: 59_940,
            scale_milli,
            connected: true,
            enabled: true,
        },
    }
}

fn placed_connector(x: i32, name: &str) -> DisplayConnectorPlacement {
    DisplayConnectorPlacement {
        connector: name.into(),
        label: "Panel".into(),
        rect: DisplayRect {
            x,
            y: 0,
            width: 1000,
            height: 800,
        },
        scale_milli: 1000,
    }
}

/// One panel at `x`, placed and observed alike.
fn display(x: i32) -> DeviceDisplay {
    DeviceDisplay {
        rect: DisplayRect {
            x,
            y: 0,
            width: 1000,
            height: 800,
        },
        scale_milli: 1000,
        connectors: vec![placed_connector(x, "DP-1")],
        outputs: vec![observed("monitor:panel", x, 1000)],
    }
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
    reason = "one fixture keeps each device's screens, copies and speakers in view"
)]
fn the_persons_devices_read_their_screens_copies_and_speakers() {
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
    laptop_device.presence = DevicePresence {
        display: Some(display(0)),
        ..DevicePresence::default()
    };
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
    tablet_device.presence = DevicePresence {
        display: Some(display(1000)),
        ..DevicePresence::default()
    };
    tablet_device.clipboard = vec![clipboard("new", &tablet, 20)];

    let devices = PersonDevices::new(
        vec![laptop_device.clone(), tablet_device.clone()],
        Vec::new(),
    );
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

    let listed = devices.person_devices(&laptop);
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].device, "laptop");
    assert!(listed[0].local);
    assert!(listed[0].online);
    assert_eq!(listed[0].outputs.len(), 1);
    assert_eq!(listed[1].device, "tablet");
    assert!(!listed[1].local);
    assert!(!listed[1].online);
    assert_eq!(listed[1].outputs[0].x, 1000);

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
    let devices = PersonDevices::new(vec![laptop_device.clone(), certified.clone()], Vec::new());
    let group = devices.speaker_group(group_id).unwrap();
    assert_eq!(
        devices.speaker_member_states(group),
        vec![SpeakerMemberStatus::Ready, SpeakerMemberStatus::Offline]
    );
    assert_eq!(devices.admit_speaker_group(group), Ok(()));

    certified.online = true;
    let devices = PersonDevices::new(vec![laptop_device, certified], Vec::new());
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
