//! Two devices of one person: what one creates appears on the other, with
//! its content arriving through the workspace's room, while what each device
//! is looking at stays its own.

use tilo_desktop_model::PEER_ID_SALT_SHIFT;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::*;
use crate::shell::state::{
    AppKind, PeerOrigin, RoomAuthority, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId,
};

const PERSON: &str = "person-quinten";
const LAPTOP: u16 = 0x1A97;
const BOARD: u16 = 0xB0A2;

struct Device {
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
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
    (device(0x1A, "Laptop"), device(0xB0, "Orange Pi"))
}

fn list_for(device: &Device) -> PersonalWorkspaces {
    PersonalWorkspaces::new(device.cert.user_public_key, device.cert.device_public_key)
}

/// A device's tree holding one workspace of its own, named `name`.
fn device_tree(salt: u16, name: &str) -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    tree.set_salt_override(salt);
    let id = WorkspaceId(salted(salt, 1));
    let mut workspace = Workspace::new(id, name);
    workspace.room = RoomId::new();
    workspace.set_owner(PERSON.to_string());
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = id;
    tree
}

fn salted(salt: u16, counter: u64) -> u64 {
    (u64::from(salt) << PEER_ID_SALT_SHIFT) | counter
}

/// One strip with one group holding `title`, as `salt`'s device opens it.
fn open_tab(tree: &mut WorkspaceTree, room: RoomId, salt: u16, title: &str) -> u64 {
    let tab = salted(salt, 30);
    let workspace = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room == room)
        .expect("workspace");
    let mut strip = Strip::new(StripId(salted(salt, 10)), "Main");
    strip.window_groups.push(WindowGroup {
        id: salted(salt, 20),
        tabs: vec![Tab {
            id: tab,
            title: title.into(),
            tile: TileContent::AppPlaceholder {
                kind: AppKind::Other("Notes".into()),
                label: title.into(),
            },
        }],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
    });
    workspace.strips.push(strip);
    tree.sync_room_doc(room);
    tab
}

/// Send `from`'s list to `to` as the network does: `to` records its own
/// unpublished changes, merges, and adopts what is new.
fn deliver(
    from: &mut PersonalWorkspaces,
    from_device: &Device,
    from_tree: &WorkspaceTree,
    to: &mut PersonalWorkspaces,
    to_tree: &mut WorkspaceTree,
) -> bool {
    from.publish_local(from_tree, PERSON);
    let update = from.snapshot(&from_device.key, 10).expect("signed list");
    to.publish_local(to_tree, PERSON);
    to.merge(&update, &from_device.cert, 20)
        .expect("same person");
    to.adopt_into(to_tree, &PERSON.to_string())
}

fn room_named(tree: &WorkspaceTree, name: &str) -> RoomId {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.name == name)
        .map_or_else(
            || panic!("no workspace named {name}"),
            |workspace| workspace.room,
        )
}

#[test]
fn a_workspace_made_on_one_device_appears_on_the_other_with_its_content() {
    let (laptop_device, board_device) = person_with_two_devices();
    let mut laptop = device_tree(LAPTOP, "Research");
    let mut board = device_tree(BOARD, "Board home");
    let board_home = board.desktop.active;
    let mut laptop_list = list_for(&laptop_device);
    let mut board_list = list_for(&board_device);

    assert!(deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board
    ));
    let research = room_named(&laptop, "Research");
    let joined = board
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == research)
        .expect("the laptop's workspace is on the board");
    assert_eq!(joined.name, "Research");
    assert_eq!(joined.owner.as_deref(), Some(PERSON));
    assert!(joined.strips.is_empty(), "content comes from the room");
    assert_eq!(board.room_authority(research), RoomAuthority::Unpinned);
    assert_eq!(
        board.desktop.active, board_home,
        "learning of a workspace never moves what the board is looking at"
    );

    // The laptop opens a document app; the board, now in the room, folds it
    // in as the laptop's window.
    let tab = open_tab(&mut laptop, research, LAPTOP, "Draft");
    let changes = laptop
        .export_room_changes_since(research, &[])
        .expect("the laptop is in the room");
    assert!(
        board
            .apply_room_changes(research, &changes, &PeerOrigin::hub(1))
            .expect("same-person room")
    );
    let arrived = board
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == research)
        .and_then(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|candidate| candidate.id == tab)
        })
        .expect("the tab reaches the board");
    assert_eq!(arrived.title, "Draft");
    assert!(matches!(
        &arrived.tile,
        TileContent::AppPlaceholder { kind: AppKind::Other(app), .. } if app == "Notes"
    ));

    // The other direction: the board's own workspace reaches the laptop.
    assert!(deliver(
        &mut board_list,
        &board_device,
        &board,
        &mut laptop_list,
        &mut laptop
    ));
    assert!(
        laptop
            .desktop
            .workspaces
            .iter()
            .any(|workspace| workspace.name == "Board home")
    );
    assert_eq!(laptop.desktop.workspaces.len(), 2);
    assert_eq!(board.desktop.workspaces.len(), 2);
}

#[test]
fn renames_follow_the_list_and_concurrent_ones_converge() {
    let (laptop_device, board_device) = person_with_two_devices();
    let mut laptop = device_tree(LAPTOP, "Research");
    let mut board = device_tree(BOARD, "Board home");
    let mut laptop_list = list_for(&laptop_device);
    let mut board_list = list_for(&board_device);
    deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board,
    );
    let research = room_named(&laptop, "Research");

    laptop.desktop.workspaces[0].name = "Thesis".into();
    assert!(deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board
    ));
    assert_eq!(room_named(&board, "Thesis"), research);

    // Both rename while apart; after exchanging lists both hold one name.
    laptop.desktop.workspaces[0].name = "Chapter one".into();
    let board_copy = board
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room == research)
        .expect("joined");
    board_copy.name = "Chapter 1".into();
    deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board,
    );
    deliver(
        &mut board_list,
        &board_device,
        &board,
        &mut laptop_list,
        &mut laptop,
    );
    let name_on = |tree: &WorkspaceTree| {
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room == research)
            .map(|workspace| workspace.name.clone())
    };
    assert_eq!(name_on(&laptop), name_on(&board));
}

#[test]
fn only_the_persons_own_workspaces_are_listed() {
    let (laptop_device, _) = person_with_two_devices();
    let mut laptop = device_tree(LAPTOP, "Research");
    let mut colleague = Workspace::new(WorkspaceId(salted(LAPTOP, 2)), "Their project");
    colleague.room = RoomId::new();
    colleague.set_owner("person-colleague".to_string());
    laptop.desktop.workspaces.push(colleague);
    let offered = RoomId::new();
    assert!(laptop.adopt_continuation_room(offered, "Board"));

    let listed = owned_workspaces(&laptop, PERSON);
    assert_eq!(
        listed.values().map(String::as_str).collect::<Vec<_>>(),
        vec!["Research"]
    );

    let mut list = list_for(&laptop_device);
    assert!(list.publish_local(&laptop, PERSON));
    assert!(
        !list.publish_local(&laptop, PERSON),
        "an unchanged list is quiet"
    );
}

#[test]
fn a_room_only_offered_by_continuation_becomes_a_workspace_when_listed() {
    let (laptop_device, board_device) = person_with_two_devices();
    let laptop = device_tree(LAPTOP, "Research");
    let research = room_named(&laptop, "Research");
    let mut board = device_tree(BOARD, "Board home");
    assert!(board.adopt_continuation_room(research, "Laptop"));
    let mut laptop_list = list_for(&laptop_device);
    let mut board_list = list_for(&board_device);
    let mut laptop = laptop;

    assert!(deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board
    ));
    assert!(!board.continuation_rooms.contains(&research));
    let held = board
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == research)
        .expect("one entry for the room");
    assert_eq!(held.name, "Research");
    assert_eq!(held.owner.as_deref(), Some(PERSON));
    assert_eq!(
        board
            .desktop
            .workspaces
            .iter()
            .filter(|workspace| workspace.room == research)
            .count(),
        1
    );
    // The board's own workspace still reaches the laptop.
    assert!(deliver(
        &mut board_list,
        &board_device,
        &board,
        &mut laptop_list,
        &mut laptop
    ));
}

#[test]
fn another_persons_list_is_refused() {
    let (laptop_device, _) = person_with_two_devices();
    let stranger_root = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
    let stranger_key = ed25519_dalek::SigningKey::from_bytes(&[0x78; 32]);
    let mut stranger_cert = DeviceCert {
        user_public_key: stranger_root.verifying_key().to_bytes(),
        device_public_key: stranger_key.verifying_key().to_bytes(),
        device_label: "Stranger".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut stranger_cert, &stranger_root).expect("certified");
    let mut stranger_list = PersonalWorkspaces::new(
        stranger_cert.user_public_key,
        stranger_cert.device_public_key,
    );
    let stranger_tree = device_tree(0x5757, "Theirs");
    stranger_list.publish_local(&stranger_tree, PERSON);
    let update = stranger_list
        .snapshot(&stranger_key, 10)
        .expect("signed list");

    let mut laptop_list = list_for(&laptop_device);
    assert!(laptop_list.merge(&update, &stranger_cert, 20).is_err());
}

#[test]
fn the_agreed_list_survives_a_restart() {
    let (laptop_device, _) = person_with_two_devices();
    let laptop = device_tree(LAPTOP, "Research");
    let dir = tempfile::tempdir().expect("state dir");
    let store = dir.path().join("personal-workspaces.postcard");
    let open = || {
        PersonalWorkspaces::open(
            laptop_device.cert.user_public_key,
            laptop_device.cert.device_public_key,
            store.clone(),
        )
    };
    let mut list = open();
    assert!(list.publish_local(&laptop, PERSON));
    drop(list);
    let mut reopened = open();
    assert!(
        !reopened.publish_local(&laptop, PERSON),
        "a restart restates nothing the other devices already have"
    );
    let update = reopened
        .snapshot(&laptop_device.key, 10)
        .expect("signed list");
    assert!(reopened.covered_by(&update));
}
