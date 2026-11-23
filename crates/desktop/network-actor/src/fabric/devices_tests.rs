//! What a device says of itself while it is there, and the person's devices read as one list:
//! their screens, their copies and their speakers.

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

#[test]
fn a_display_keeps_its_outputs_and_placements_describing_each_other() {
    assert_eq!(display(0).validate(), Ok(()));

    let mut resized = display(0);
    resized.outputs[0].output.width = 1001;
    assert_eq!(
        resized.validate(),
        Err(FabricError::InvalidDisplayPlacement),
        "the output is not where the connector places it"
    );

    let mut rescaled = display(0);
    rescaled.outputs[0].output.scale_milli = 1500;
    assert_eq!(
        rescaled.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );

    let mut unobserved = display(0);
    unobserved.outputs.clear();
    assert_eq!(
        unobserved.validate(),
        Err(FabricError::InvalidDisplayPlacement),
        "a placed connector with no active output"
    );

    let mut inactive = display(0);
    inactive.outputs[0].output.connected = false;
    assert_eq!(
        inactive.validate(),
        Err(FabricError::InvalidDisplayPlacement),
        "a placed connector whose output is not active"
    );

    let mut stray = display(0);
    stray.outputs.push(DisplayOutputObservation {
        connector: "HDMI-1".into(),
        ..observed("monitor:other", 0, 1000)
    });
    assert_eq!(
        stray.validate(),
        Err(FabricError::InvalidDisplayPlacement),
        "an active output no connector places"
    );
}

#[test]
fn a_display_refuses_bounds_it_cannot_place_and_repeats() {
    let mut flat = display(0);
    flat.rect.height = 0;
    assert_eq!(flat.validate(), Err(FabricError::InvalidDisplayPlacement));

    let mut tiny = display(0);
    tiny.scale_milli = 499;
    assert_eq!(tiny.validate(), Err(FabricError::InvalidDisplayPlacement));

    let mut huge = display(0);
    huge.connectors[0].scale_milli = 4001;
    assert_eq!(huge.validate(), Err(FabricError::InvalidDisplayPlacement));

    let mut unreadable = display(0);
    unreadable.connectors[0].label = "line\nbreak".into();
    assert_eq!(
        unreadable.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );

    let mut nameless = display(0);
    nameless.connectors[0].connector = String::new();
    assert_eq!(
        nameless.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );

    let mut crowded = display(0);
    crowded.connectors = (0..33)
        .map(|n| placed_connector(0, &format!("DP-{n}")))
        .collect();
    assert_eq!(
        crowded.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );

    let mut overfull = display(0);
    overfull.outputs = (0..=settings_store::person_displays::MAX_DEVICE_OUTPUTS)
        .map(|n| DisplayOutputObservation {
            connector: format!("DP-{n}"),
            ..observed(&format!("monitor:{n}"), 0, 1000)
        })
        .collect();
    assert_eq!(
        overfull.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );

    let mut twice_placed = display(0);
    twice_placed.connectors.push(placed_connector(0, "DP-1"));
    assert_eq!(
        twice_placed.validate(),
        Err(FabricError::DuplicateItem),
        "one connector is placed once"
    );

    let mut twice_observed = display(0);
    twice_observed
        .outputs
        .push(observed("monitor:panel", 0, 1000));
    assert_eq!(
        twice_observed.validate(),
        Err(FabricError::DuplicateItem),
        "one connector is observed once"
    );

    let mut same_panel = display(0);
    same_panel.connectors.push(placed_connector(1000, "DP-2"));
    same_panel.outputs.push(DisplayOutputObservation {
        connector: "DP-2".into(),
        ..observed("monitor:panel", 1000, 1000)
    });
    assert_eq!(
        same_panel.validate(),
        Err(FabricError::DuplicateItem),
        "one output id names one screen"
    );
}

#[test]
fn a_device_says_only_wake_routes_and_screens_it_could_have() {
    let route = |mac: [u8; 6]| WakeRoute {
        mac,
        kind: WakeInterfaceKind::Ethernet,
        magic_packet: true,
        subnet: None,
    };
    let presence = DevicePresence {
        wake_routes: vec![route([0x02; 6])],
        session: OwnerSession::default(),
        display: Some(display(0)),
    };
    assert_eq!(presence.validate(), Ok(()));

    let mut crowded = presence.clone();
    crowded.wake_routes = vec![route([0x02; 6]); MAX_WAKE_ROUTES + 1];
    assert_eq!(crowded.validate(), Err(FabricError::TooLarge));

    let mut multicast = presence.clone();
    multicast.wake_routes = vec![route([0x01; 6])];
    assert_eq!(multicast.validate(), Err(FabricError::InvalidWakeRoute));

    let mut repeated = presence.clone();
    repeated.wake_routes = vec![route([0x02; 6]), route([0x02; 6])];
    assert_eq!(repeated.validate(), Err(FabricError::DuplicateItem));

    let mut unplaceable = presence;
    unplaceable.display.as_mut().unwrap().rect.width = 0;
    assert_eq!(
        unplaceable.validate(),
        Err(FabricError::InvalidDisplayPlacement)
    );
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
