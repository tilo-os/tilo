//! A room hosted on a person's device is sealed, as a room a relay hosts
//! is: an app's records are channels each member device opens itself with
//! the keys the room's steward gives it. Any member device makes a record,
//! signing as itself; the host admits it by membership and policy, and a
//! device that is no longer a member holds no key to what is written after.

use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;

use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::channel_encryption::ChannelAccess;
use crate::shell::state::RoomId;

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
#[allow(
    clippy::too_many_lines,
    reason = "one journey: three devices, a removal, and what each opens either side of it"
)]
fn a_removed_member_opens_what_was_written_before_its_removal_and_nothing_after() {
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
    let second = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A42,
        DeviceCapabilities::member(),
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
    let all = [&first, &second, &guest];
    let (local, room) = hosted_workspace(&first, &[&second, &guest], "Projects");
    second.reach(&first);
    until(&all, "the laptop to hold the workspace", || {
        second.holds(room).then_some(())
    });
    guest
        .join(&first.invite(local, "Grace", Some(grace.user())))
        .unwrap();
    until(&all, "Grace to hold the workspace", || {
        guest.holds(room).then_some(())
    });

    // While Grace is a member her device holds the note's channel and reads
    // what the host writes there.
    first.open_notes_first(room);
    until(&all, "the note's channel on every device", || {
        all.iter()
            .all(|device| device.holds_note(room))
            .then_some(())
    });
    first.write_note(room, "Before Grace left");
    until(&all, "the note before the removal everywhere", || {
        all.iter()
            .all(|device| device.note(room).as_deref() == Some("Before Grace left"))
            .then_some(())
    });
    let before = guest.note_writes(room);
    let kept = current_keys(&guest, room).expect("Grace holds the note's channel");

    // Ada removes her. The host and Ada's laptop each write afterwards, and
    // each reads the other's write.
    first.remove_member(room, &grace);
    until(&all, "the note's channel to move on without Grace", || {
        let host = current_epoch(&first, room)?;
        let laptop = current_epoch(&second, room)?;
        (host > kept.epoch() && laptop == host).then_some(())
    });
    assert_eq!(first.role_of(room, &grace), None);
    let (on_host, on_laptop) = (
        first.note_writes(room).len(),
        second.note_writes(room).len(),
    );
    first.write_note(room, "Written on the desktop after Grace left");
    until(&all, "the desktop's write on the laptop", || {
        (second.note(room).as_deref() == Some("Written on the desktop after Grace left"))
            .then_some(())
    });
    second.write_note(room, "Written on the laptop after Grace left");
    until(&all, "the laptop's write on the desktop", || {
        (first.note(room).as_deref() == Some("Written on the laptop after Grace left"))
            .then_some(())
    });

    // What was written after: the remaining member opens both writes, the
    // removed device's keys open neither.
    let after: Vec<_> = first.note_writes(room)[on_host..]
        .iter()
        .chain(&second.note_writes(room)[on_laptop..])
        .cloned()
        .collect();
    let authors: std::collections::BTreeSet<_> = after
        .iter()
        .map(|write| write.author.as_bytes().to_owned())
        .collect();
    assert_eq!(
        authors,
        [
            first.cert().device_public_key,
            second.cert().device_public_key
        ]
        .into(),
        "the host and the remaining member each wrote after the removal"
    );
    let laptop = current_keys(&second, room).expect("Ada's laptop holds the note's channel");
    for write in &after {
        assert!(
            opens(&laptop, write),
            "the remaining member opens every write after the removal"
        );
        assert!(
            !opens(&kept, write),
            "the removed device opens no write after its removal"
        );
    }
    // What was written before: the removed device still opens it.
    assert!(!before.is_empty());
    for write in &before {
        assert!(
            opens(&kept, write),
            "the removed device still opens what it read before"
        );
    }
    drop((first, second, guest));
}

/// Whether `keys` open the sealed note write `write`.
fn opens(keys: &channel_crypto::ChannelContentKeys, write: &collab_types::SignedOp) -> bool {
    write.encryption.as_ref().is_some_and(|encryption| {
        keys.open_content(write.artifact, encryption, &write.payload)
            .is_ok()
    })
}

/// The note's channel keys `device` holds in `room`, if any.
fn current_keys(device: &Device, room: RoomId) -> Option<channel_crypto::ChannelContentKeys> {
    match device.note_access(room) {
        ChannelAccess::Ready(keys) => Some(keys),
        _ => None,
    }
}

/// The note channel's current epoch as `device` holds it in `room`.
fn current_epoch(device: &Device, room: RoomId) -> Option<u64> {
    current_keys(device, room).map(|keys| keys.epoch())
}
