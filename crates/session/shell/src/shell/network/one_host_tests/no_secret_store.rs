//! A device whose session has no secret store: the rooms it hosts itself
//! are sealed all the same, and their keys come from its own room host, so
//! its apps write there. A room hosted elsewhere needs this device's own
//! channel state, and says so once instead of being asked about forever.

use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;

use super::home_relay::holds;
use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::channel_encryption::ChannelAccess;

#[test]
#[serial_test::serial]
fn a_device_with_no_secret_store_writes_in_the_sealed_room_it_hosts() {
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

    // The notes app gets the note's keys from Ada's own host and writes.
    p.open_notes_first(room);
    until(&[&p], "the note's keys from Ada's own host", || {
        p.holds_note(room).then_some(())
    });
    p.write_note(room, "Plant the tulips");
    let written = p.note_writes(room);
    let sealed = &written.last().expect("the note was written").payload;
    let host = p.host_state_root();
    until(&[&p], "Ada's host to keep the sealed write", || {
        holds(&host, sealed).then_some(())
    });
    assert!(
        !holds(&host, b"Plant the tulips"),
        "the host keeps only ciphertext"
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
