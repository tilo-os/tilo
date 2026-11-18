#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::Arc;

use tilo_network_actor::fabric::FabricStamp;
use tilo_network_actor::fabric::link::FabricLink as _;
use tilo_network_actor::fabric::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerGroup, SpeakerGroupId,
    SpeakerGroupMember, SpeakerRole, SpeakerTrim, StereoSide,
};
use tilo_network_actor::fabric::unlock::{DecisionOutcome, UnlockDecisionEntry};

use super::super::{Continuity, Reach, SessionFabric};
use super::*;
use crate::personal::linked::{hand, store_of};

/// One of the person's devices: its own store, and its session's link onto their other devices.
struct Device {
    id: DeviceId,
    store: PersonalSync,
    link: Arc<SessionFabric>,
}

impl Device {
    fn new(seed: u8, label: &str) -> Self {
        let person = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        let id = DeviceId::new(hex::encode(cert.device_public_key)).expect("a device id");
        let store = store_of(&cert, Arc::new(key.clone()));
        let continuity = Continuity::default();
        // Every switch as the person left it: on.
        continuity.read(&serde_json::Map::new());
        let (reach, _sent) = Reach::listened(cert.user_public_key, key, continuity);
        let link = SessionFabric::new(id.clone(), label.to_owned(), Some(store.clone()), reach);
        Self { id, store, link }
    }
}

/// An answer `decided_by` gave one boot that asked to unlock its disk.
fn answer(
    decided_by: [u8; 32],
    outcome: DecisionOutcome,
    decided_at_ns: u64,
    valid_until_ns: u64,
) -> UnlockDecisionEntry {
    UnlockDecisionEntry {
        boot_cert_digest: [3; 32],
        boot_id: tilo_unlock_protocol::BootId([4; 16]),
        slot: tilo_unlock_protocol::SlotId {
            volume_uuid: [5; 16],
            keyslot: 1,
        },
        outcome,
        decided_by,
        decided_at_ns,
        valid_until_ns,
    }
}

/// The laptop and the desktop of one person, each having handed the other what its store holds.
fn met() -> (Device, Device) {
    let laptop = Device::new(0x21, "Laptop");
    let desktop = Device::new(0x22, "Desktop");
    hand(&laptop.store, &desktop.store);
    hand(&desktop.store, &laptop.store);
    (laptop, desktop)
}

/// What a device is reaches the person's other devices through their store: each lists the other
/// under the name and kind it wrote of itself, from the moment its session started.
#[test]
fn what_a_device_is_reaches_the_persons_other_devices() {
    let (laptop, desktop) = met();

    let there = desktop.link.devices();
    let seen = there.get(&laptop.id).expect("the desktop knows the laptop");
    assert_eq!(seen.about.label, "Laptop");
    assert!(seen.about.capabilities.clipboard);
    assert!(
        !seen.online,
        "nothing said the laptop is on a session with the desktop"
    );
    assert_eq!(
        there
            .get(&desktop.id)
            .map(|device| device.about.label.as_str()),
        Some("Desktop")
    );
}

/// A Wi-Fi passphrase one device learned is sealed to each of the person's devices and kept in
/// their store: the desktop opens what the laptop shared, and no other network's.
#[test]
fn a_shared_wifi_passphrase_opens_on_the_persons_other_device() {
    let (laptop, desktop) = met();

    laptop
        .link
        .share_wifi_password("Home", os_actions::WifiSecurity::Wpa2, "a long secret")
        .expect("the laptop shares the passphrase");
    hand(&laptop.store, &desktop.store);

    assert_eq!(
        desktop
            .link
            .wifi_password("Home", os_actions::WifiSecurity::Wpa2)
            .as_deref(),
        Some("a long secret")
    );
    assert_eq!(
        desktop
            .link
            .wifi_password("Office", os_actions::WifiSecurity::Wpa2),
        None
    );
    assert!(
        desktop
            .store
            .read(|store| store.person_resource(WIFI_KIND))
            .is_some_and(|kept| !String::from_utf8_lossy(&kept).contains("a long secret")),
        "the store keeps the passphrase sealed"
    );
}

/// What a device answered a boot that asked to unlock its disk is in its part of the person's
/// store, so their other devices answer that boot the same way.
#[test]
fn a_devices_unlock_answers_reach_the_persons_other_devices() {
    let (laptop, desktop) = met();
    let now = tilo_clock::unix_now_ns();
    let mut decided_by = [0; 32];
    hex::decode_to_slice(laptop.id.as_str(), &mut decided_by).expect("a device key");
    let answer = answer(decided_by, DecisionOutcome::Deny, now, now + 60_000_000_000);

    laptop.link.share_unlock(UnlockFabricState {
        decisions: vec![answer.clone()],
        ..UnlockFabricState::default()
    });
    hand(&laptop.store, &desktop.store);

    assert_eq!(
        desktop
            .link
            .devices()
            .get(&laptop.id)
            .map(|device| device.unlock.decisions.clone()),
        Some(vec![answer])
    );
}

/// The speaker group a device leads and the outputs it offers are in its part of the person's
/// store, so each of their devices lists the group, also one that comes up after it was made.
#[test]
fn a_speaker_group_reaches_the_persons_other_devices() {
    let (laptop, desktop) = met();
    let output = |name: &str| AudioEndpoint {
        selector: EndpointSelector::mint(&laptop.id, name),
        kind: AudioEndpointKind::Output,
        label: name.to_owned(),
        is_default: false,
    };
    let (left, right) = (output("left"), output("right"));
    let member = |endpoint: &AudioEndpoint, side| SpeakerGroupMember {
        device: laptop.id.clone(),
        output: endpoint.selector,
        role: SpeakerRole::Stereo(side),
        trim: SpeakerTrim::default(),
    };
    let group = SpeakerGroup {
        id: SpeakerGroupId([9; 16]),
        name: "Kitchen".to_owned(),
        leader: laptop.id.clone(),
        members: vec![
            member(&left, StereoSide::Left),
            member(&right, StereoSide::Right),
        ],
        stamp: FabricStamp {
            counter: 1,
            device: laptop.id.clone(),
        },
    };
    let speakers = SpeakerFabricState {
        endpoints: vec![left, right],
        groups: vec![group.clone()],
        ..SpeakerFabricState::default()
    };

    assert!(keep_own(&laptop.store, SPEAKERS, &speakers).expect("the laptop keeps its group"));
    assert!(
        !keep_own(&laptop.store, SPEAKERS, &speakers).expect("kept again"),
        "what did not change is not written again"
    );
    hand(&laptop.store, &desktop.store);
    let later = Device::new(0x23, "Tablet");
    hand(&later.store, &desktop.store);
    hand(&desktop.store, &later.store);

    for device in [&desktop, &later] {
        assert_eq!(
            device.link.devices().speaker_groups(),
            vec![&group],
            "every device of the person lists the group"
        );
    }
}

/// A value under a device's part that does not hold for that device is left out, and the rest of
/// what the device wrote still reads.
#[test]
fn what_does_not_hold_for_its_device_is_left_out() {
    let (laptop, desktop) = met();
    // Answered by a device other than the laptop, whose part it is in.
    let foreign = answer([8; 32], DecisionOutcome::Approve, 1, 2);
    keep_own(
        &laptop.store,
        UNLOCK,
        &UnlockFabricState {
            decisions: vec![foreign],
            ..UnlockFabricState::default()
        },
    )
    .expect("the laptop's own part takes any value");
    hand(&laptop.store, &desktop.store);

    let there = desktop.link.devices();
    let seen = there.get(&laptop.id).expect("the desktop knows the laptop");
    assert!(seen.unlock.decisions.is_empty());
    assert_eq!(seen.about.label, "Laptop");
}
