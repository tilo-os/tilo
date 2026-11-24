//! The record of a person's devices, kept in their own workspace on two of their devices: what
//! one device says there reaches the other as any change of the workspace does.
#![allow(
    clippy::large_futures,
    clippy::significant_drop_tightening,
    reason = "a fixture failing is its test failing, and each test keeps both devices to the end"
)]

use std::time::Duration;

use tilo_resource::{Id, Resource as _};
use tilo_server_protocol::servers::person_workspace_id;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::super::Workspaces;
use super::super::fixtures::{TestHost, device_cert, governor, test_host_holding};
use super::super::person_workspace::{
    self, apply_on,
    devices::{DEVICES, Devices, OwnerSession, UnlockAnswer},
    read_on,
};
use super::tests::{catching_up, group_keys, invitation, member_service};

/// The person's desktop, which founded their own workspace and keeps itself in the record of
/// their devices, its certificate, their laptop, which took the workspace in, and the workspace.
async fn desk_and_laptop(
    one: &std::path::Path,
    two: &std::path::Path,
) -> (TestHost, DeviceCert, TestHost, String) {
    let (desk_key, desk_cert) = governor();
    let desk = test_host_holding(one, &desk_key, &desk_cert, group_keys()).await;
    let own = person_workspace::found(desk.workspaces())
        .await
        .expect("the desktop founds the person's own workspace");
    assert_eq!(own, person_workspace_id(&desk_cert.user_public_key));
    // The record is there before the laptop takes the workspace in, as it is once the desktop
    // kept itself: the laptop's copy follows it from then on.
    apply_on(
        desk.workspaces(),
        &own,
        DEVICES,
        Devices::keeping(&desk_cert).unwrap(),
    )
    .await
    .expect("the desktop is among the person's devices");
    let laptop = device_cert(
        &ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let invited = invitation(&desk, &desk_cert, &own, 51, &laptop.1).await;
    let laptop = member_service(two, laptop).await;
    let joined = laptop
        .workspaces()
        .join_member(&invited)
        .await
        .expect("the laptop takes the person's workspace in");
    assert_eq!(
        joined.to_string(),
        own,
        "the laptop holds the same workspace"
    );
    catching_up(laptop.workspaces(), joined).await;
    (desk, desk_cert, laptop, own)
}

/// Wait until `reader`'s record of the person's devices in `workspace` reads as `done` says.
async fn reads(reader: &Workspaces, workspace: &str, what: &str, done: fn(&Devices) -> bool) {
    let (reader, workspace) = (reader.clone(), workspace.to_owned());
    tilo_test_env::within(what, async move {
        loop {
            if read_on::<Devices>(&reader, &workspace, DEVICES)
                .await
                .is_ok_and(|devices| done(&devices))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A lock the person says on their desktop shows on their laptop: the desktop writes it into the
/// record of their devices in their own workspace, and the laptop, holding that workspace too,
/// reads it there with no word between the two sessions.
#[tokio::test]
async fn the_person_s_lock_follows_their_other_devices() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (desk, desk_cert, laptop, own) = desk_and_laptop(one.path(), two.path()).await;
    let locked = OwnerSession {
        locked: true,
        changed_ms: 1_700_000_000_000,
        active_ms: 1_699_999_940_000,
        using: false,
    };
    apply_on(
        desk.workspaces(),
        &own,
        DEVICES,
        Devices::saying_session(&desk_cert.device_public_key, locked),
    )
    .await
    .expect("the desktop says it locked");

    let desktop = Id::from_raw(hex::encode(desk_cert.device_public_key));
    let said = read_on::<Devices>(desk.workspaces(), &own, DEVICES)
        .await
        .unwrap();
    assert_eq!(said.devices.get(&desktop).unwrap().session, locked);
    reads(
        laptop.workspaces(),
        &own,
        "the desktop's lock reaches the laptop",
        |devices| devices.devices.values().any(|device| device.session.locked),
    )
    .await;
    let read = read_on::<Devices>(laptop.workspaces(), &own, DEVICES)
        .await
        .unwrap();
    assert_eq!(
        read.devices.get(&desktop).map(|device| device.session),
        Some(locked),
        "the laptop reads the desktop's lock as the desktop said it"
    );
    drop(desk);
}

/// What the person answered a boot on their desktop is theirs on their laptop too: the answer is in
/// the laptop's record, so a boot that asks it next hears the same answer, and the record's
/// history there lists the answer as the desktop's step.
#[tokio::test]
async fn an_answer_on_one_device_reads_in_the_others_record_and_history() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (desk, desk_cert, laptop, own) = desk_and_laptop(one.path(), two.path()).await;
    let answer = UnlockAnswer {
        device: hex::encode(desk_cert.device_public_key),
        enrolment: hex::encode([0x21; 32]),
        boot: hex::encode([0x33; 16]),
        slot: format!("{}:1", hex::encode([0x11; 16])),
        allowed: true,
        at_ns: 1_700_000_000_000_000_000,
        until_ns: 1_700_000_300_000_000_000,
    };
    apply_on(
        desk.workspaces(),
        &own,
        DEVICES,
        Devices::answering(&answer),
    )
    .await
    .expect("the person lets the boot unlock, on the desktop");

    reads(
        laptop.workspaces(),
        &own,
        "the desktop's answer reaches the laptop",
        |devices| !devices.unlocks.is_empty(),
    )
    .await;
    let read = read_on::<Devices>(laptop.workspaces(), &own, DEVICES)
        .await
        .unwrap();
    assert_eq!(
        read.unlocks.values().cloned().collect::<Vec<_>>(),
        [answer],
        "the laptop holds the answer as it was made"
    );

    let bytes = laptop
        .workspaces()
        .resource_document(&own, DEVICES.0, DEVICES.1)
        .await
        .unwrap()
        .expect("the laptop holds the record");
    let mut doc = automerge::AutoCommit::load(&bytes).unwrap();
    let allow = Devices::declarations()
        .into_iter()
        .map(|declared| declared.id)
        .find(|id| id.starts_with("allow"))
        .expect("the record declares letting a boot unlock");
    let desktop = automerge::ActorId::from(desk_cert.device_public_key.to_vec());
    assert!(
        doc.get_changes(&[]).iter().any(|change| {
            change.actor_id() == &desktop
                && change
                    .message()
                    .is_some_and(|message| message.contains(&*allow))
        }),
        "the record's history lists the answer as the desktop's step"
    );
    drop(desk);
}

/// The person wakes their sleeping laptop from their desktop: the desktop asks it in the record of
/// their devices, the desktop's daemon finds itself on the laptop's LAN and sends the packets, and
/// the laptop, once back, says so in the record, where the desktop reads that it woke.
#[tokio::test]
async fn a_device_woken_from_another_says_it_is_back_in_the_record() {
    use std::collections::BTreeSet;

    use super::super::person_workspace::wake::{
        self, WAKE_FACT, WakeInterfaceKind, WakeRoute, WakeState,
    };

    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (desk, desk_cert, laptop, own) = desk_and_laptop(one.path(), two.path()).await;
    let laptop_cert = laptop.workspaces().device_cert().clone();
    let lan = "192.168.1.0/24".parse().unwrap();
    let on_lan = |mac: u8, magic_packet: bool| WakeRoute {
        mac: [0x02, 0, 0, 0, 0, mac],
        kind: WakeInterfaceKind::Ethernet,
        magic_packet,
        subnet: Some(lan),
    };
    let keeps = |cert: &DeviceCert, routes: &[WakeRoute]| {
        Devices::KEEP_FACT.with((
            hex::encode(cert.device_public_key),
            hex::encode(postcard::to_allocvec(cert).unwrap()),
            WAKE_FACT.to_owned(),
            wake::encoded(routes),
        ))
    };
    apply_on(
        laptop.workspaces(),
        &own,
        DEVICES,
        keeps(&laptop_cert, &[on_lan(9, true)]),
    )
    .await
    .expect("the laptop keeps the interface it wakes through");
    apply_on(
        desk.workspaces(),
        &own,
        DEVICES,
        keeps(&desk_cert, &[on_lan(1, false)]),
    )
    .await
    .expect("the desktop keeps its own");
    reads(
        desk.workspaces(),
        &own,
        "the laptop's interface reaches the desktop",
        |devices| devices.devices.values().any(wake::wakeable),
    )
    .await;

    let (desk_key, laptop_key) = (
        hex::encode(desk_cert.device_public_key),
        hex::encode(laptop_cert.device_public_key),
    );
    let asked_ns = 1_700_000_000_000_000_000;
    let record = read_on::<Devices>(desk.workspaces(), &own, DEVICES)
        .await
        .unwrap();
    let ask = wake::asking(
        &record,
        &desk_cert.user_public_key,
        &desk_key,
        "laptop",
        asked_ns,
    )
    .expect("the laptop can be woken from the desktop's LAN");
    apply_on(desk.workspaces(), &own, DEVICES, ask)
        .await
        .expect("the desktop asks the wake");

    // The desktop's daemon reads the ask and sends the packets from here.
    let record = read_on::<Devices>(desk.workspaces(), &own, DEVICES)
        .await
        .unwrap();
    let mut sent = Vec::new();
    let mut done = BTreeSet::new();
    let writes = wake::answering(
        &record,
        &desk_key,
        &[on_lan(1, false)],
        asked_ns + 1,
        &mut done,
        |mac, subnet| {
            sent.push((mac, subnet));
            true
        },
    );
    assert_eq!(
        sent,
        [([0x02, 0, 0, 0, 0, 9], lan)],
        "one packet, for the laptop"
    );
    for write in writes {
        apply_on(desk.workspaces(), &own, DEVICES, write)
            .await
            .expect("the desktop says it sent them");
    }
    assert!(
        wake::answering(
            &record,
            &desk_key,
            &[on_lan(1, false)],
            asked_ns + 2,
            &mut done,
            |_, _| { panic!("a wake taken up is not sent again") }
        )
        .is_empty()
    );

    // The laptop is back: its daemon reads the ask of itself and says so.
    reads(
        laptop.workspaces(),
        &own,
        "the wake reaches the laptop",
        |devices| devices.wakes.values().any(|ask| ask.sent_ns > 0),
    )
    .await;
    let record = read_on::<Devices>(laptop.workspaces(), &own, DEVICES)
        .await
        .unwrap();
    let writes = wake::answering(
        &record,
        &laptop_key,
        &[on_lan(9, true)],
        asked_ns + 3,
        &mut BTreeSet::new(),
        |_, _| panic!("the laptop does not wake itself"),
    );
    for write in writes {
        apply_on(laptop.workspaces(), &own, DEVICES, write)
            .await
            .expect("the laptop says it is back");
    }
    reads(
        desk.workspaces(),
        &own,
        "the laptop's word reaches the desktop",
        |devices| devices.wakes.values().any(|ask| ask.awake_ns > 0),
    )
    .await;
    let record = read_on::<Devices>(desk.workspaces(), &own, DEVICES)
        .await
        .unwrap();
    let ask = record.wakes.get(&Id::from_raw(laptop_key)).unwrap();
    assert_eq!(ask.asked_by, desk_key);
    assert_eq!(ask.sent_by, desk_key, "the desktop sent the packets");
    assert_eq!(ask.state(i64::MAX), WakeState::Awake);
    drop(desk);
}

/// A speaker moves from the person's desktop to their laptop through the record of their devices:
/// the desktop asks, the laptop says it holds the speaker's bond, the desktop says it let go, and
/// the laptop says it connected it, each reading the other's step in its own copy.
#[tokio::test]
async fn an_accessory_moves_to_another_device_through_the_record() {
    use super::super::person_workspace::accessory::{self, Part, Radio};
    use super::super::person_workspace::devices::MoveStage;

    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (desk, desk_cert, laptop, own) = desk_and_laptop(one.path(), two.path()).await;
    let laptop_cert = laptop.workspaces().device_cert().clone();
    let (desk_key, laptop_key) = (
        hex::encode(desk_cert.device_public_key),
        hex::encode(laptop_cert.device_public_key),
    );
    apply_on(
        laptop.workspaces(),
        &own,
        DEVICES,
        Devices::keeping(&laptop_cert).unwrap(),
    )
    .await
    .expect("the laptop is among the person's devices");
    reads(
        desk.workspaces(),
        &own,
        "the laptop reaches the desktop's record",
        |devices| devices.devices.iter().count() == 2,
    )
    .await;

    let asked_ns = 1_700_000_000_000_000_000;
    let speaker = "AA:BB:CC:DD:EE:01";
    let here = |_: &str| true;
    let (mut on_desk, mut on_laptop) = (Part::default(), Part::default());
    let record = read_on::<Devices>(desk.workspaces(), &own, DEVICES)
        .await
        .unwrap();
    let ask = accessory::asking(
        &record,
        &desk_key,
        speaker,
        "Speaker",
        &laptop_key,
        asked_ns,
    )
    .expect("the speaker can move to the laptop");
    apply_on(desk.workspaces(), &own, DEVICES, ask)
        .await
        .expect("the desktop asks the move");

    // Each device takes its part as its copy of the record reaches the step before it.
    let steps = [
        (true, MoveStage::Asked, Radio::Inspect),
        (false, MoveStage::Ready, Radio::Release),
        (true, MoveStage::Released, Radio::Take),
    ];
    for (offset, (on_the_laptop, stage, radio)) in (1..).zip(steps) {
        let (host, key, part) = if on_the_laptop {
            (&laptop, laptop_key.as_str(), &mut on_laptop)
        } else {
            (&desk, desk_key.as_str(), &mut on_desk)
        };
        let (workspaces, workspace) = (host.workspaces().clone(), own.clone());
        let record = tilo_test_env::within("the step before reaches the device", async move {
            loop {
                if let Ok(record) = read_on::<Devices>(&workspaces, &workspace, DEVICES).await
                    && record.moves.values().any(|kept| kept.stage == stage)
                {
                    return record;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let (writes, work) = accessory::turns(&record, key, here, asked_ns + offset, part);
        assert!(writes.is_empty(), "nothing is given up");
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].radio, radio);
        let write = accessory::finished(&work[0], true, part, asked_ns + offset).unwrap();
        apply_on(host.workspaces(), &own, DEVICES, write)
            .await
            .expect("the device says how its step went");
    }

    reads(
        desk.workspaces(),
        &own,
        "the laptop's word that it connected the speaker reaches the desktop",
        |devices| {
            devices
                .moves
                .values()
                .any(|kept| kept.stage == MoveStage::Moved)
        },
    )
    .await;
    let record = read_on::<Devices>(desk.workspaces(), &own, DEVICES)
        .await
        .unwrap();
    let kept = record.moves.get(&Id::from_raw(speaker.to_owned())).unwrap();
    assert_eq!(
        (kept.from.as_str(), kept.to.as_str()),
        (desk_key.as_str(), laptop_key.as_str())
    );
    assert_eq!(kept.name, "Speaker");
    assert_eq!(kept.released_ns, asked_ns + 2);
    // A move that ended asks nothing more of either device.
    assert!(
        accessory::turns(&record, &desk_key, here, asked_ns + 4, &mut on_desk)
            .1
            .is_empty()
    );
    drop(desk);
}

/// A computer seals the records of the person's own workspace with its own key service from the
/// moment its service is built, and again once it starts over: nobody hands the service to the
/// keys, so what the service writes while it starts finds who reads the workspace.
#[cfg(feature = "device-keys")]
#[tokio::test(flavor = "multi_thread")]
async fn a_computer_seals_its_person_s_records_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let own = {
        let (computer, _keys) =
            super::super::fixtures::test_host_keyed(dir.path(), &key, &cert).await;
        let own = person_workspace::found(computer.workspaces())
            .await
            .expect("the computer founds the person's own workspace");
        apply_on(
            computer.workspaces(),
            &own,
            DEVICES,
            Devices::keeping(&cert).unwrap(),
        )
        .await
        .expect("the computer keeps itself among the person's devices");
        own
    };
    let (computer, _keys) = super::super::fixtures::test_host_keyed(dir.path(), &key, &cert).await;
    let sealed = computer
        .workspaces()
        .read_now(&own, |state| {
            state.publication().is_sealed_document(DEVICES.0, DEVICES.1)
        })
        .map(|(_, sealed)| sealed);
    assert_eq!(sealed, Some(true), "the record is sealed");
    let (_, laptop) = device_cert(
        &ed25519_dalek::SigningKey::from_bytes(&[2; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    apply_on(
        computer.workspaces(),
        &own,
        DEVICES,
        Devices::keeping(&laptop).unwrap(),
    )
    .await
    .expect("the computer writes the sealed record again once it started over");
    let devices = read_on::<Devices>(computer.workspaces(), &own, DEVICES)
        .await
        .expect("the computer opens its own record");
    for kept in [&cert, &laptop] {
        let id = Id::from_raw(hex::encode(kept.device_public_key));
        assert!(devices.devices.get(&id).is_some(), "both writes read back");
    }
}
