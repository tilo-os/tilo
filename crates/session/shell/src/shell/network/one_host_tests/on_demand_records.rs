//! An earlier part of a chained record reaches a device that comes to the
//! room later only once it asks: the room's host replays it the chain's
//! head, and a local app reading back (`read_on_demand`, which sends
//! `OpenRecord`) has the host send the earlier part, as the room's other
//! documents arrive. A part the host never had is answered as absent.

use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;

use super::cold_reads::CHAT;
use super::{Device, Person, RoomId, Wakes, hosted_workspace, scratch, until};
use crate::shell::network::OnDemand;

/// Part `index` of the chat's chained messages record.
fn part(index: u64) -> String {
    neighbour_wire::record_part("derived:messages@room", index)
}

/// Whether the network of `device` took in a write of the chat's `record`
/// in `room` from the room's host.
fn arrived(device: &Device, room: RoomId, record: &str) -> bool {
    let room = room.to_string();
    device
        .net()
        .inbound_documents_handle()
        .lock()
        .unwrap()
        .iter()
        .any(|document| {
            document.app_id == CHAT && document.cluster_id == room && document.record_id == record
        })
}

/// Ask for the chat's `record` in `room` on `device`, as an app reading it
/// back does, until the room's host answered that it holds none of it:
/// first seen on its way (a host of the room is connected), then absent.
fn until_absent(devices: &[&Device], device: &Device, room: RoomId, record: &str) {
    let net = Arc::clone(device.net());
    until(devices, "a host of the room to be asked", || {
        (net.read_on_demand(room, CHAT, record) == OnDemand::Arriving).then_some(())
    });
    until(devices, "the host to say it has none of the part", || {
        (net.read_on_demand(room, CHAT, record) == OnDemand::Absent).then_some(())
    });
}

#[test]
#[serial_test::serial]
fn a_later_device_opens_an_earlier_part_of_a_chain_at_the_host() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(83);
    home.env().set("TILO_USER_ID", ada.id());
    let desktop = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A61,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&desktop, &[], "Projects");

    // The desktop's chat writes two parts of its messages; the room's host,
    // on the desktop, keeps both.
    desktop.open_app_first(room, CHAT);
    for (index, text) in [
        (0, "the first messages"),
        (1, "the messages after the first part"),
    ] {
        let mut document = automerge::AutoCommit::new();
        automerge::transaction::Transactable::put(&mut document, automerge::ROOT, "text", text)
            .unwrap();
        document.commit();
        desktop.write_sealed(room, CHAT, &part(index), document.save());
    }
    until(&[&desktop], "the host to retain both parts", || {
        (desktop.retained_writes(room, CHAT) >= 2).then_some(())
    });

    // Only now does the laptop come to the room.
    let laptop = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A62,
        DeviceCapabilities::member(),
        &wakes,
    );
    laptop.reach(&desktop);
    let both = [&desktop, &laptop];
    until(&both, "the laptop to hold the workspace", || {
        laptop.holds(room).then_some(())
    });
    until(
        &both,
        "the host to replay the laptop the chain's head",
        || arrived(&laptop, room, &part(1)).then_some(()),
    );
    // A part the host never had, asked for after the replay: once its
    // answer is here, the replay is done.
    until_absent(&both, &laptop, room, &part(8));
    assert!(
        !arrived(&laptop, room, &part(0)),
        "an earlier part waits until it is asked for"
    );

    // An app on the laptop reads the earlier part back.
    assert_eq!(
        laptop.net().read_on_demand(room, CHAT, &part(0)),
        OnDemand::Arriving,
        "a host of the room is connected to send it"
    );
    until(
        &both,
        "the laptop to receive the earlier part it asked for",
        || arrived(&laptop, room, &part(0)).then_some(()),
    );

    // A part the host never had: it says so, with nothing sent. Its answer
    // comes after the earlier part's, which named what it sent: that part
    // is not absent.
    until_absent(&both, &laptop, room, &part(7));
    assert!(!arrived(&laptop, room, &part(7)), "nothing of it is sent");
    assert_eq!(
        laptop.net().read_on_demand(room, CHAT, &part(0)),
        OnDemand::Arriving,
        "the host said it sent what it has of the earlier part"
    );
    drop((desktop, laptop));
}
