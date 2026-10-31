//! A device whose session has no secret store holds no channel keys: the
//! room host it runs keeps none of its own to lend it, so its apps seal
//! nothing in any sealed room, the ones it hosts included, and it says so
//! instead of being asked about forever.

use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;

use super::home_relay::holds;
use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::channel_encryption::ChannelAccess;

#[test]
#[serial_test::serial]
fn a_device_with_no_secret_store_seals_nothing_in_the_room_it_hosts() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(121);
    let mut p = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("ada"),
        0x0A91,
        DeviceCapabilities::primary(),
        &wakes,
    );
    p.lose_secret_store();
    let (_, room) = hosted_workspace(&p, &[], "Garden");
    assert_eq!(p.hosted_rooms(), vec![room]);

    // The notes app gets no keys: this device's channel service has no
    // secret store, and the host it writes to runs no channel service of
    // its own to seal in its stead.
    p.open_notes_first(room);
    let access = p.note_access(room);
    assert!(
        matches!(access, ChannelAccess::NoSecretStore),
        "a device with no secret store holds no keys of its own: {access:?}"
    );
    assert!(
        !p.holds_note(room),
        "the room's host lends this device no keys"
    );
    assert!(
        p.note_writes(room).is_empty(),
        "nothing was sealed in the room"
    );
    let host = p.host_state_root();
    assert!(
        !holds(&host, b"Plant the tulips"),
        "no write reached the host at all"
    );
    drop(p);
}

#[test]
#[serial_test::serial]
fn a_room_hosted_elsewhere_says_this_device_has_no_secret_store_and_nothing_asks_again() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(122), Person::new(123));
    let p = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("ada"),
        0x0A92,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let mut q = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B92,
        DeviceCapabilities::primary(),
        &wakes,
    );
    q.lose_secret_store();
    let (local, room) = hosted_workspace(&p, &[&q], "Finance");
    q.join(&p.invite(local, "Grace", Some(grace.user())))
        .unwrap();
    until(&[&p, &q], "Grace to hold the workspace", || {
        q.holds(room).then_some(())
    });
    assert!(
        q.hosted_rooms().is_empty(),
        "the room is hosted on Ada's laptop"
    );

    // Grace's notes app learns why it holds no keys, and asking again
    // changes nothing: her channel service has nothing to do.
    for _ in 0..3 {
        let access = q.note_access(room);
        assert!(
            matches!(access, ChannelAccess::NoSecretStore),
            "a room hosted elsewhere needs a secret store here: {access:?}"
        );
        assert!(
            !q.channels_live(),
            "nothing is retried while the session has no secret store"
        );
    }
    drop((p, q));
}
