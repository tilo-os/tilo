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
