//! A room hosted on a person's device is sealed, as a room a relay hosts
//! is: an app's records are channels each member device opens itself with
//! the keys the room's steward gives it. Any member device makes a record,
//! signing as itself; the host admits it by membership and policy, and a
//! device that is no member gets no keys to write one.

use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;

use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::channel_encryption::ChannelAccess;

#[test]
#[serial_test::serial]
fn a_persons_second_device_makes_a_note_in_a_room_the_first_device_hosts_and_both_read_it() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(61);
    home.env().set("TILO_USER_ID", ada.id());
    let first = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A31,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let second = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A32,
        DeviceCapabilities::member(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&first, &[&second], "Projects");
    second.reach(&first);
    until(
        &[&first, &second],
        "the laptop to hold the workspace",
        || second.holds(room).then_some(()),
    );
    assert_eq!(first.hosted_rooms(), vec![room]);
    assert!(second.hosted_rooms().is_empty());

    // The laptop opens the notes app there first: the note's record is its
    // sealed channel, made by the laptop, which is no governor.
    second.open_notes_first(room);
    until(
        &[&first, &second],
        "the note's channel on both devices",
        || (first.holds_note(room) && second.holds_note(room)).then_some(()),
    );
    second.write_note(room, "Written on the laptop");
    until(
        &[&first, &second],
        "the laptop's note on the desktop",
        || (first.note(room).as_deref() == Some("Written on the laptop")).then_some(()),
    );
    assert_eq!(second.note(room).as_deref(), Some("Written on the laptop"));

    // The hosting device writes into the same record the same way.
    first.write_note(room, "Answered on the desktop");
    until(
        &[&first, &second],
        "the desktop's note on the laptop",
        || (second.note(room).as_deref() == Some("Answered on the desktop")).then_some(()),
    );
    drop((first, second));
}

#[test]
#[serial_test::serial]
fn a_device_that_is_no_longer_a_member_gets_no_keys_to_write_a_record() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(71), Person::new(72));
    home.env().set("TILO_USER_ID", ada.id());
    let first = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A41,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let guest = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B41,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (local, room) = hosted_workspace(&first, &[&guest], "Projects");
    guest
        .join(&first.invite(local, "Grace", Some(grace.user())))
        .unwrap();
    until(&[&first, &guest], "Grace to hold the workspace", || {
        guest.holds(room).then_some(())
    });

    // While Grace is a member her device holds the note's channel and
    // writes into it.
    first.open_notes_first(room);
    until(
        &[&first, &guest],
        "the note's channel on both devices",
        || (first.holds_note(room) && guest.holds_note(room)).then_some(()),
    );
    guest.write_note(room, "Grace's draft");
    until(&[&first, &guest], "Grace's note on the desktop", || {
        (first.note(room).as_deref() == Some("Grace's draft")).then_some(())
    });

    // Ada removes her: the room's steward takes her device out of the
    // record's channel, so it holds no keys to seal a write any member
    // opens, and nobody lets an app author records for her there.
    first.remove_member(room, &grace);
    until(&[&first, &guest], "Grace out of the note's channel", || {
        matches!(
            guest.note_access(room),
            ChannelAccess::Removed | ChannelAccess::Unavailable
        )
        .then_some(())
    });
    assert!(
        guest.let_notes_author(room).is_err(),
        "a device outside the room cannot have an app author its records"
    );
    assert_eq!(first.role_of(room, &grace), None);
    assert_eq!(first.note(room).as_deref(), Some("Grace's draft"));
    drop((first, guest));
}
