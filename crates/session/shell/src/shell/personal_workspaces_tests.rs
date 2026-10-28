//! Two devices of one person: a room one holds is listed with the device
//! hosting it, and the other enters it at that host as the list brings it;
//! a workspace let go on one device is released on the other.

use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::*;
use crate::shell::state::{Workspace, WorkspaceId};

struct Device {
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
}

impl Device {
    fn key(&self) -> [u8; 32] {
        self.cert.device_public_key
    }
}

fn person_with_two_devices() -> (Device, Device) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
    let device = |seed: u8, label: &str| {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).expect("certified device");
        Device { key, cert }
    };
    (device(0x1A, "Laptop"), device(0xB0, "Board"))
}

fn list_for(device: &Device) -> PersonalWorkspaces {
    PersonalWorkspaces::new(device.cert.user_public_key, device.cert.device_public_key)
}

/// A tree holding a workspace for each of `rooms`, in order.
fn tree_of(rooms: &[(&str, RoomId)]) -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.clear();
    for (index, (name, room)) in (1u64..).zip(rooms) {
        let id = WorkspaceId(index);
        tree.desktop
            .workspaces
            .push(Workspace::in_room(id, *room, *name));
        tree.desktop.active = id;
    }
    tree
}

/// Send `from`'s whole list, signed by `device`, into `to`.
fn send(from: &mut PersonalWorkspaces, device: &Device, to: &mut PersonalWorkspaces) {
    let update = from.snapshot(&device.key, 10).expect("signed list");
    to.merge(&update, &device.cert, 20).expect("same person");
}

#[test]
fn a_room_held_here_is_listed_with_the_device_hosting_it_and_entered_there_by_the_other() {
    let (laptop, board) = person_with_two_devices();
    let plans = RoomId::new();
    let laptop_tree = tree_of(&[("Plans", plans)]);
    let mut laptop_list = list_for(&laptop);
    // The laptop's own host serves the room.
    let held = HeldRooms::from([(plans, laptop.key())]);
    assert!(laptop_list.publish_local(&laptop_tree, &held));
    let listed = entries_of(&laptop_list.directory);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].1.host_devices, vec![laptop.key()]);

    let mut board_list = list_for(&board);
    let mut board_tree = tree_of(&[("Board's own", RoomId::new())]);
    send(&mut laptop_list, &laptop, &mut board_list);
    let adoption = board_list.adopt_into(&mut board_tree, &board.key());
    assert_eq!(
        adoption.enter,
        vec![ToEnter {
            room: plans,
            name: "Plans".into(),
            host: laptop.key(),
        }],
        "the board enters the laptop's room at the laptop's host"
    );
    assert!(adoption.release.is_empty());

    // Brought again before it is held, it is asked for again (entering is
    // idempotent), its workspace here or not: a join left from the session
    // before goes on by being entered.
    let again = board_list.adopt_into(&mut board_tree, &board.key());
    assert_eq!(again.enter.len(), 1);
    board_tree = tree_of(&[("Board's own", RoomId::new()), ("Plans", plans)]);
    board_tree.joining_rooms.insert(plans);
    let with_workspace = board_list.adopt_into(&mut board_tree, &board.key());
    assert_eq!(
        with_workspace.enter.len(),
        1,
        "a workspace of the room that is not its replica: the room is entered"
    );
}

#[test]
fn a_room_not_yet_held_is_not_listed() {
    let (laptop, _) = person_with_two_devices();
    let placing = RoomId::new();
    let tree = tree_of(&[("Being placed", placing)]);
    let mut list = list_for(&laptop);
    // The native owner holds nothing yet: the workspace is still joining.
    list.publish_local(&tree, &HeldRooms::new());
    assert!(entries_of(&list.directory).is_empty());
}

#[test]
fn an_entry_naming_no_host_or_this_device_is_never_entered() {
    let (laptop, board) = person_with_two_devices();
    let unhosted = RoomId::new();
    let own = RoomId::new();
    let mut board_list = list_for(&board);
    board_list
        .directory
        .add_room_workspace(entry_id(unhosted), &unhosted.to_string(), "Nowhere")
        .expect("listed");
    board_list
        .directory
        .add_room_workspace(entry_id(own), &own.to_string(), "Mine")
        .and_then(|()| {
            board_list
                .directory
                .set_host_devices(entry_id(own), &[board.key()])
        })
        .expect("listed");
    let mut tree = tree_of(&[("Other", RoomId::new())]);
    let adoption = board_list.adopt_into(&mut tree, &board.key());
    assert!(
        adoption.enter.is_empty(),
        "a device never guesses where a room lives, nor enters at itself"
    );
    let _ = laptop;
}

#[test]
fn a_room_being_entered_is_not_taken_for_one_deleted_here() {
    let (laptop, board) = person_with_two_devices();
    let plans = RoomId::new();
    let mut laptop_list = list_for(&laptop);
    laptop_list.publish_local(
        &tree_of(&[("Plans", plans)]),
        &HeldRooms::from([(plans, laptop.key())]),
    );
    let mut board_list = list_for(&board);
    let mut board_tree = tree_of(&[("Other", RoomId::new())]);
    send(&mut laptop_list, &laptop, &mut board_list);
    assert_eq!(
        board_list
            .adopt_into(&mut board_tree, &board.key())
            .enter
            .len(),
        1
    );
    // The board's next reconciliation runs before the room's workspace is
    // made: the room stays listed.
    board_list.publish_local(&board_tree, &HeldRooms::new());
    assert!(
        entries_of(&board_list.directory)
            .iter()
            .any(|(room, _)| *room == plans)
    );
}

#[test]
fn a_workspace_deleted_on_one_device_is_released_on_the_other() {
    let (laptop, board) = person_with_two_devices();
    let plans = RoomId::new();
    let notes = RoomId::new();
    let held = HeldRooms::from([(plans, laptop.key()), (notes, laptop.key())]);
    let mut laptop_list = list_for(&laptop);
    laptop_list.publish_local(&tree_of(&[("Plans", plans), ("Notes", notes)]), &held);
    let mut board_list = list_for(&board);
    let mut board_tree = tree_of(&[("Plans", plans), ("Notes", notes)]);
    send(&mut laptop_list, &laptop, &mut board_list);
    let adoption = board_list.adopt_into(&mut board_tree, &board.key());
    assert!(adoption.release.is_empty());

    // Deleted on the laptop.
    assert!(laptop_list.publish_local(&tree_of(&[("Notes", notes)]), &held));
    send(&mut laptop_list, &laptop, &mut board_list);
    let adoption = board_list.adopt_into(&mut board_tree, &board.key());
    assert_eq!(adoption.release, vec![plans]);
}

#[test]
fn a_workspace_kept_off_a_device_is_released_there() {
    let (laptop, board) = person_with_two_devices();
    let plans = RoomId::new();
    let notes = RoomId::new();
    let held = HeldRooms::from([(plans, laptop.key()), (notes, laptop.key())]);
    let laptop_tree = tree_of(&[("Plans", plans), ("Notes", notes)]);
    let mut laptop_list = list_for(&laptop);
    laptop_list.publish_local(&laptop_tree, &held);
    let mut board_list = list_for(&board);
    let mut board_tree = tree_of(&[("Plans", plans), ("Notes", notes)]);
    send(&mut laptop_list, &laptop, &mut board_list);
    board_list.adopt_into(&mut board_tree, &board.key());

    assert!(laptop_list.set_shown(plans, board.key(), false));
    send(&mut laptop_list, &laptop, &mut board_list);
    let adoption = board_list.adopt_into(&mut board_tree, &board.key());
    assert_eq!(adoption.release, vec![plans]);
    assert!(
        adoption.enter.iter().all(|entry| entry.room != plans),
        "nor is it entered"
    );
}

#[test]
fn the_agreed_list_survives_a_restart() {
    let (laptop, _) = person_with_two_devices();
    let dir = tempfile::tempdir().expect("temp");
    let store = dir.path().join("personal-workspaces.postcard");
    let plans = RoomId::new();
    let tree = tree_of(&[("Plans", plans)]);
    let held = HeldRooms::from([(plans, laptop.key())]);
    let mut list =
        PersonalWorkspaces::open(laptop.cert.user_public_key, laptop.key(), store.clone());
    assert!(list.publish_local(&tree, &held));
    crate::shell::durable_writer::global().flush();
    let mut reopened = PersonalWorkspaces::open(laptop.cert.user_public_key, laptop.key(), store);
    assert!(
        !reopened.publish_local(&tree, &held),
        "nothing changed since the list was agreed"
    );
}

#[test]
fn an_unchanged_tree_is_not_compared_against_the_directory_again() {
    let (laptop, _) = person_with_two_devices();
    let plans = RoomId::new();
    let mut tree = tree_of(&[("Plans", plans)]);
    let held = HeldRooms::from([(plans, laptop.key())]);
    let mut list = list_for(&laptop);
    list.publish_local(&tree, &held);
    let passes = list.publish_passes;
    for _ in 0..3 {
        assert!(!list.publish_local(&tree, &held));
    }
    assert_eq!(list.publish_passes, passes, "nothing changed");

    // A rename here is written to the directory.
    tree.desktop.workspaces[0].name = "Reading".into();
    assert!(list.publish_local(&tree, &held));
    assert!(!list.publish_local(&tree, &held));
    assert_eq!(list.publish_passes, passes + 1);

    // So is the room's host moving.
    let moved = HeldRooms::from([(plans, [0x77; 32])]);
    assert!(list.publish_local(&tree, &moved));
    assert_eq!(
        entries_of(&list.directory)[0].1.host_devices,
        vec![[0x77; 32]]
    );
}
