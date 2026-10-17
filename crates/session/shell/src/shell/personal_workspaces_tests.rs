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
    workspace.set_room(RoomId::new());
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
        focus_history: Vec::new(),
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
    deliver_as(PERSON, from, from_device, from_tree, to, to_tree)
}

/// [`deliver`] for the lists of `person`.
fn deliver_as(
    person: &str,
    from: &mut PersonalWorkspaces,
    from_device: &Device,
    from_tree: &WorkspaceTree,
    to: &mut PersonalWorkspaces,
    to_tree: &mut WorkspaceTree,
) -> bool {
    from.publish_local(from_tree, person);
    let update = from.snapshot(&from_device.key, 10).expect("signed list");
    to.publish_local(to_tree, person);
    to.merge(&update, &from_device.cert, 20)
        .expect("same person");
    to.adopt_into(to_tree, &person.to_string())
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
fn an_enrolled_room_waits_for_its_name_before_publishing_to_other_devices() {
    let (laptop_device, board_device) = person_with_two_devices();
    let mut laptop = device_tree(LAPTOP, "Home");
    let room = room_named(&laptop, "Home");
    let mut board = WorkspaceTree::default();
    board.set_salt_override(BOARD);
    // Enrollment adopts this slot before a room snapshot or signed directory
    // supplies its name. Use the same adoption path as join_enrolled_room.
    assert!(board.adopt_personal_room(room, "From another device", &PERSON.to_string(), None));
    assert_eq!(board.room_authority(room), RoomAuthority::Unpinned);
    let mut laptop_list = list_for(&laptop_device);
    let mut board_list = list_for(&board_device);

    for _ in 0..2 {
        assert!(!board_list.publish_local(&board, PERSON));
        assert!(entries_of(&board_list.directory).is_empty());
    }
    // The enrolling device can hear from the new device first. Its real name
    // must survive that signed, authenticated directory exchange.
    deliver(
        &mut board_list,
        &board_device,
        &board,
        &mut laptop_list,
        &mut laptop,
    );
    assert_eq!(room_named(&laptop, "Home"), room);
    assert!(deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board,
    ));
    assert_eq!(room_named(&board, "Home"), room);
    assert_eq!(board.room_authority(room), RoomAuthority::Unpinned);

    // A directory-confirmed room can be renamed on the new device even if
    // its first room snapshot has not arrived yet.
    board.desktop.workspaces[0].name = "Projects".into();
    assert!(deliver(
        &mut board_list,
        &board_device,
        &board,
        &mut laptop_list,
        &mut laptop,
    ));
    assert_eq!(room_named(&laptop, "Projects"), room);
}

#[test]
fn a_workspace_made_on_one_device_appears_on_the_other_with_its_content() {
    let (laptop_device, board_device) = person_with_two_devices();
    let (person, key) = certified_person(&laptop_device);
    let mut laptop = device_tree(LAPTOP, "Research");
    let mut board = device_tree(BOARD, "Board home");
    for tree in [&mut laptop, &mut board] {
        tree.desktop.workspaces[0].set_owner(person.clone());
        tree.local_person = Some(key);
    }
    board
        .authenticated_persons
        .insert(origin_of(&laptop_device), key);
    let board_home = board.desktop.active;
    let mut laptop_list = list_for(&laptop_device);
    let mut board_list = list_for(&board_device);

    assert!(deliver_as(
        &person,
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
    assert_eq!(joined.owner().map(String::as_str), Some(person.as_str()));
    assert!(joined.strips.is_empty(), "content comes from the room");
    // The person's signed list says the room is theirs, so it is pinned to
    // the person now — not to whichever of their devices answers first.
    assert_eq!(board.room_authority(research), RoomAuthority::Person(key));
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
            .apply_room_changes(research, &changes, &origin_of(&laptop_device))
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
    assert!(deliver_as(
        &person,
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
fn a_person_room_an_older_build_left_unpinned_is_pinned_by_the_list() {
    let (laptop_device, board_device) = person_with_two_devices();
    let (person, key) = certified_person(&laptop_device);
    let mut laptop = device_tree(LAPTOP, "Research");
    laptop.desktop.workspaces[0].set_owner(person.clone());
    let mut board = device_tree(BOARD, "Board home");
    board.desktop.workspaces[0].set_owner(person.clone());
    let mut laptop_list = list_for(&laptop_device);
    let mut board_list = list_for(&board_device);
    assert!(deliver_as(
        &person,
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board
    ));
    let research = room_named(&laptop, "Research");
    assert_eq!(board.room_authority(research), RoomAuthority::Person(key));

    // What an older build persisted: agreed in the list, still waiting for a
    // first answer to pin its governor.
    board
        .room_authority
        .insert(research, RoomAuthority::Unpinned);
    assert!(board_list.adopt_into(&mut board, &person));
    assert_eq!(board.room_authority(research), RoomAuthority::Person(key));
}

#[test]
fn rooms_the_person_is_not_in_are_not_listed() {
    let (laptop_device, _) = person_with_two_devices();
    let mut laptop = device_tree(LAPTOP, "Research");
    let mut colleague = Workspace::new(WorkspaceId(salted(LAPTOP, 2)), "Their project");
    colleague.set_room(RoomId::new());
    colleague.set_owner("person-colleague".to_string());
    laptop.desktop.workspaces.push(colleague);
    let offered = RoomId::new();
    assert!(laptop.adopt_continuation_room(offered, "Board"));

    let listed = member_workspaces(&laptop, PERSON);
    assert_eq!(
        listed
            .values()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Research"]
    );
    assert!(listed.values().all(|entry| entry.governor.is_none()));

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
    assert_eq!(held.owner().map(String::as_str), Some(PERSON));
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
    // Reconciled from the compositor's loop, so it must not wait for the
    // disk: the write goes to the state writer.
    #[cfg(target_os = "linux")]
    assert!(crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(
        || list.publish_local(&laptop, PERSON)
    ));
    #[cfg(not(target_os = "linux"))]
    assert!(list.publish_local(&laptop, PERSON));
    drop(list);
    crate::shell::durable_writer::global().flush();
    assert!(store.exists(), "the list is written");
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

/// The person the test devices are certified for, as owners record them,
/// with the key the network layer authenticates.
fn certified_person(device: &Device) -> (String, [u8; 32]) {
    let key = device.cert.user_public_key;
    (
        crate::shell::state::person_id_of(key).expect("a root names a person"),
        key,
    )
}

fn origin_of(device: &Device) -> PeerOrigin {
    PeerOrigin::peer(
        &iroh::EndpointId::from_bytes(&device.cert.device_public_key).expect("device endpoint"),
    )
}

fn name_of(tree: &WorkspaceTree, room: RoomId) -> String {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == room)
        .map(|workspace| workspace.name.clone())
        .expect("workspace held")
}

fn ship_room(
    from: &mut WorkspaceTree,
    to: &mut WorkspaceTree,
    room: RoomId,
    origin: &PeerOrigin,
) -> bool {
    let changes = from
        .export_room_changes_since(room, &[])
        .expect("in the room");
    to.apply_room_changes(room, &changes, origin)
        .expect("admits")
}

#[test]
fn any_device_of_the_owner_governs_their_room_until_it_is_revoked() {
    let (laptop_device, board_device) = person_with_two_devices();
    let (person, key) = certified_person(&laptop_device);
    let mut laptop = device_tree(LAPTOP, "Research");
    laptop.desktop.workspaces[0].set_owner(person.clone());
    let research = laptop.desktop.workspaces[0].room;
    let mut board = device_tree(BOARD, "Board home");
    board.desktop.workspaces[0].set_owner(person.clone());
    for tree in [&mut laptop, &mut board] {
        tree.local_person = Some(key);
    }
    laptop
        .authenticated_persons
        .insert(origin_of(&board_device), key);
    board
        .authenticated_persons
        .insert(origin_of(&laptop_device), key);

    let mut laptop_list = list_for(&laptop_device);
    let mut board_list = list_for(&board_device);
    laptop_list.publish_local(&laptop, &person);
    let update = laptop_list.snapshot(&laptop_device.key, 10).expect("list");
    board_list
        .merge(&update, &laptop_device.cert, 20)
        .expect("same person");
    assert!(board_list.adopt_into(&mut board, &person));
    // The laptop renames the room before the board first hears from it.
    laptop
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room == research)
        .expect("made here")
        .name = "Research notes".into();
    assert!(ship_room(
        &mut laptop,
        &mut board,
        research,
        &origin_of(&laptop_device)
    ));
    assert_eq!(name_of(&board, research), "Research notes");
    assert_eq!(
        board.room_authority(research),
        RoomAuthority::Person(key),
        "the room is the person's, not the laptop's"
    );
    assert!(board.governs_room(research));

    // The board renames the room; the laptop that made it accepts that from
    // another device of the person who owns it.
    board
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room == research)
        .expect("joined")
        .name = "Thesis".into();
    assert!(ship_room(
        &mut board,
        &mut laptop,
        research,
        &origin_of(&board_device)
    ));
    assert_eq!(name_of(&laptop, research), "Thesis");

    // Membership is governance: the board admits someone and the laptop
    // takes it from another device of the room's owner.
    let board_ws = board
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == research)
        .map(|workspace| workspace.id)
        .expect("joined");
    let grant = |tree: &mut WorkspaceTree, who: &str| {
        tree.admit_in_workspace(board_ws, &person, who, tilo_action_policy::Role::Editor)
            .expect("the person's device administers the person's room")
    };
    grant(&mut board, "person-ada");
    assert!(ship_room(
        &mut board,
        &mut laptop,
        research,
        &origin_of(&board_device)
    ));
    assert!(member_of(&laptop, research, "person-ada"));

    // Once the board is revoked it no longer counts as the person's device.
    laptop.authenticated_persons.clear();
    grant(&mut board, "person-mallory");
    assert!(!ship_room(
        &mut board,
        &mut laptop,
        research,
        &origin_of(&board_device)
    ));
    assert!(!member_of(&laptop, research, "person-mallory"));
}

fn member_of(tree: &WorkspaceTree, room: RoomId, who: &str) -> bool {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == room)
        .and_then(|workspace| workspace.role_of(who))
        .is_some()
}

#[test]
fn a_room_someone_else_invited_the_person_to_follows_them_with_its_governor() {
    let (laptop_device, board_device) = person_with_two_devices();
    let (person, key) = certified_person(&laptop_device);
    let colleague_key = iroh::SecretKey::from_bytes(&[0xC0; 32]);
    let colleague_origin = PeerOrigin::peer(&colleague_key.public());

    // The colleague's room, with this person a member.
    let mut colleague = WorkspaceTree::default();
    colleague.set_salt_override(0xC011);
    let colleague_ws = WorkspaceId(salted(0xC011, 1));
    let mut project = Workspace::new(colleague_ws, "Their project");
    let room = RoomId::new();
    project.set_room(room);
    project.set_owner("person-colleague".to_string());
    colleague.desktop.workspaces.push(project);
    colleague.desktop.active = colleague_ws;
    assert!(
        colleague
            .admit_in_workspace(
                colleague_ws,
                &"person-colleague".to_string(),
                &person.clone(),
                tilo_action_policy::Role::Editor
            )
            .is_some()
    );

    // The laptop accepted the invitation: the colleague is its pinned governor.
    let mut laptop = device_tree(LAPTOP, "Research");
    laptop.desktop.workspaces[0].set_owner(person.clone());
    let mut joined = Workspace::new(WorkspaceId(salted(LAPTOP, 9)), "Their project");
    joined.set_room(room);
    laptop.desktop.workspaces.push(joined);
    laptop.pin_room_authority(room, colleague_origin.clone());
    assert!(ship_room(
        &mut colleague,
        &mut laptop,
        room,
        &colleague_origin
    ));
    let listed = member_workspaces(&laptop, &person);
    assert_eq!(
        listed
            .get(&room)
            .and_then(|entry| entry.governor.as_deref()),
        Some(colleague_origin.as_str())
    );

    let mut board = device_tree(BOARD, "Board home");
    board.desktop.workspaces[0].set_owner(person.clone());
    let board_home = board.desktop.active;
    board.local_person = Some(key);
    board
        .authenticated_persons
        .insert(origin_of(&laptop_device), key);
    let mut laptop_list = list_for(&laptop_device);
    let mut board_list = list_for(&board_device);
    laptop_list.publish_local(&laptop, &person);
    let update = laptop_list.snapshot(&laptop_device.key, 10).expect("list");
    board_list
        .merge(&update, &laptop_device.cert, 20)
        .expect("same person");
    assert!(board_list.adopt_into(&mut board, &person));
    assert_eq!(
        board.room_authority(room),
        RoomAuthority::Peer(colleague_origin.clone()),
        "the board pins the room's real governor"
    );
    assert_eq!(
        board.desktop.active, board_home,
        "joining never moves focus"
    );

    // The governor's document is admitted, and with it this person's
    // membership.
    assert!(ship_room(
        &mut colleague,
        &mut board,
        room,
        &colleague_origin
    ));
    let held = board
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == room)
        .expect("joined");
    assert_eq!(held.owner().map(String::as_str), Some("person-colleague"));
    assert!(held.role_of(&person).is_some());

    // A device of this same person cannot rewrite someone else's room's
    // governance, even if it claims to govern it.
    laptop.room_authority.insert(room, RoomAuthority::Local);
    laptop
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room == room)
        .expect("held")
        .admit("person-mallory", tilo_action_policy::Role::Editor);
    assert!(!ship_room(
        &mut laptop,
        &mut board,
        room,
        &origin_of(&laptop_device)
    ));
    assert!(!member_of(&board, room, "person-mallory"));
}

/// A workspace named `name` added to `tree` as `salt`'s device creates it.
fn add_workspace(tree: &mut WorkspaceTree, salt: u16, counter: u64, name: &str) -> RoomId {
    let mut workspace = Workspace::new(WorkspaceId(salted(salt, counter)), name);
    let room = RoomId::new();
    workspace.set_room(room);
    workspace.set_owner(PERSON.to_string());
    tree.desktop.workspaces.push(workspace);
    room
}

fn id_named(tree: &WorkspaceTree, name: &str) -> WorkspaceId {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.name == name)
        .map_or_else(
            || panic!("no workspace named {name}"),
            |workspace| workspace.id,
        )
}

fn names(tree: &WorkspaceTree) -> Vec<String> {
    tree.desktop
        .workspaces
        .iter()
        .map(|workspace| workspace.name.clone())
        .collect()
}

/// Two devices that have exchanged lists once: the laptop with "Research"
/// and "Thesis", the board with "Board home".
fn two_devices_in_step() -> (
    (Device, WorkspaceTree, PersonalWorkspaces),
    (Device, WorkspaceTree, PersonalWorkspaces),
) {
    let (laptop_device, board_device) = person_with_two_devices();
    let mut laptop = device_tree(LAPTOP, "Research");
    add_workspace(&mut laptop, LAPTOP, 2, "Thesis");
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
    deliver(
        &mut board_list,
        &board_device,
        &board,
        &mut laptop_list,
        &mut laptop,
    );
    (
        (laptop_device, laptop, laptop_list),
        (board_device, board, board_list),
    )
}

#[test]
fn a_workspace_deleted_on_one_device_goes_from_the_other_and_an_undo_brings_it_back() {
    let ((laptop_device, mut laptop, mut laptop_list), (_, mut board, mut board_list)) =
        two_devices_in_step();
    let research = room_named(&laptop, "Research");
    let research_id = id_named(&laptop, "Research");
    // The board is looking at the workspace when it goes.
    let on_board = board
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == research)
        .map(|workspace| workspace.id)
        .expect("joined");
    board.desktop.active = on_board;

    let (index, removed) = laptop
        .delete_workspace(research_id)
        .expect("one of several workspaces");
    assert!(deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board
    ));
    assert!(
        !board.desktop.workspaces.iter().any(|w| w.room == research),
        "the board drops the deleted workspace"
    );
    assert!(
        board
            .desktop
            .workspaces
            .iter()
            .any(|w| w.id == board.desktop.active),
        "the board shows a workspace that still exists"
    );

    assert!(laptop.restore_workspace(index, removed));
    assert!(deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board
    ));
    assert!(
        board.desktop.workspaces.iter().any(|w| w.room == research),
        "undoing the deletion brings it back to the board"
    );
}

#[test]
fn the_order_of_the_list_follows_the_person_and_concurrent_moves_converge() {
    let ((laptop_device, mut laptop, mut laptop_list), (board_device, mut board, mut board_list)) =
        two_devices_in_step();
    // Once both have each other's list, both hold the person's one order.
    deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board,
    );
    assert_eq!(names(&laptop), names(&board));

    // Put Thesis right before Research on the laptop.
    while names(&laptop).iter().position(|n| n == "Thesis")
        > names(&laptop).iter().position(|n| n == "Research")
    {
        assert!(laptop.move_workspace(id_named(&laptop, "Thesis"), -1));
    }
    deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board,
    );
    let shared_order = |tree: &WorkspaceTree| -> Vec<String> {
        names(tree)
            .into_iter()
            .filter(|name| name == "Research" || name == "Thesis")
            .collect()
    };
    assert_eq!(shared_order(&board), ["Thesis", "Research"]);

    // Both move a different workspace while apart; after exchanging lists
    // both hold one order.
    let board_home = id_named(&board, "Board home");
    let laptop_research = id_named(&laptop, "Research");
    let _ = laptop.move_workspace(laptop_research, -1);
    let _ = laptop.move_workspace(laptop_research, 1);
    let _ = board.move_workspace(board_home, 1);
    let _ = board.move_workspace(board_home, -1);
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
    deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board,
    );
    assert_eq!(names(&laptop), names(&board));
}

#[test]
fn a_workspace_kept_off_a_device_leaves_it_and_returns_when_shown_again() {
    let ((laptop_device, mut laptop, mut laptop_list), (board_device, mut board, mut board_list)) =
        two_devices_in_step();
    let research = room_named(&laptop, "Research");
    let board_key = board_device.cert.device_public_key;
    let choices = laptop_list.device_choices(research);
    assert!(
        choices.iter().all(|choice| choice.shown),
        "every device shows a workspace by default"
    );

    // The board wrote itself into the list when it listed its own
    // workspace? It did not register a label; the laptop knows it only
    // once it does.
    board_list = board_list.with_label("Orange Pi");
    deliver(
        &mut board_list,
        &board_device,
        &board,
        &mut laptop_list,
        &mut laptop,
    );
    let choices = laptop_list.device_choices(research);
    assert!(
        choices
            .iter()
            .any(|choice| choice.device == board_key && !choice.here)
    );

    assert!(laptop_list.set_shown(research, board_key, false));
    deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board,
    );
    assert!(
        !board.desktop.workspaces.iter().any(|w| w.room == research),
        "kept off the board"
    );
    assert!(
        laptop.desktop.workspaces.iter().any(|w| w.room == research),
        "still on the laptop"
    );
    // The board still carries the list: it does not delete what it hides.
    deliver(
        &mut board_list,
        &board_device,
        &board,
        &mut laptop_list,
        &mut laptop,
    );
    assert!(laptop.desktop.workspaces.iter().any(|w| w.room == research));

    assert!(laptop_list.set_shown(research, board_key, true));
    deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut board_list,
        &mut board,
    );
    assert!(
        board.desktop.workspaces.iter().any(|w| w.room == research),
        "shown on the board again"
    );
}

#[test]
fn a_device_linked_after_an_opt_out_shows_every_workspace() {
    let ((laptop_device, laptop, mut laptop_list), (board_device, _, _)) = two_devices_in_step();
    let research = room_named(&laptop, "Research");
    assert!(laptop_list.set_shown(research, board_device.cert.device_public_key, false));

    let root = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
    let phone_key = ed25519_dalek::SigningKey::from_bytes(&[0x9E; 32]);
    let mut phone_cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: phone_key.verifying_key().to_bytes(),
        device_label: "Phone".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut phone_cert, &root).expect("certified device");
    let phone_device = Device {
        key: phone_key,
        cert: phone_cert,
    };
    let mut phone = device_tree(0x9E0E, "Phone home");
    let mut phone_list = list_for(&phone_device);
    deliver(
        &mut laptop_list,
        &laptop_device,
        &laptop,
        &mut phone_list,
        &mut phone,
    );
    assert!(
        phone.desktop.workspaces.iter().any(|w| w.room == research),
        "only the board opted out; a device linked later shows it"
    );
}

#[test]
fn deleting_from_the_menu_offers_an_undo() {
    let mut tree = device_tree(LAPTOP, "Research");
    add_workspace(&mut tree, LAPTOP, 2, "Thesis");
    let chrome = crate::shell::chrome_state::ChromeState::new(tree);
    let mut state = chrome.borrow_mut();
    assert!(state.delete_active_workspace());
    assert_eq!(names(&state.tree()), ["Thesis"]);
    assert!(!state.delete_active_workspace(), "the last workspace stays");
    let toast = state
        .notifications()
        .last()
        .map(|toast| toast.id)
        .expect("an undo toast");
    assert!(state.activate_notification(toast));
    assert_eq!(names(&state.tree()), ["Research", "Thesis"]);
}

#[test]
fn an_unchanged_tree_is_not_compared_against_the_directory_again() {
    let ((_, mut laptop, mut laptop_list), (board_device, _, mut board_list)) =
        two_devices_in_step();
    laptop_list.publish_local(&laptop, PERSON);
    let passes = laptop_list.publish_passes;
    for _ in 0..3 {
        assert!(!laptop_list.publish_local(&laptop, PERSON));
    }
    assert_eq!(laptop_list.publish_passes, passes, "nothing changed");

    // A rename here is written to the directory.
    let research = id_named(&laptop, "Research");
    laptop
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == research)
        .expect("workspace")
        .name = "Reading".into();
    assert!(laptop_list.publish_local(&laptop, PERSON));
    assert!(!laptop_list.publish_local(&laptop, PERSON));
    assert_eq!(laptop_list.publish_passes, passes + 1);

    // So is a new order.
    laptop.desktop.workspaces.reverse();
    assert!(laptop_list.publish_local(&laptop, PERSON));
    assert_eq!(laptop_list.publish_passes, passes + 2);

    // Another device's list moves the directory under an unchanged tree,
    // so the next call compares again.
    let update = board_list
        .snapshot(&board_device.key, 30)
        .expect("signed list");
    laptop_list
        .merge(&update, &board_device.cert, 40)
        .expect("same person");
    laptop_list.publish_local(&laptop, PERSON);
    assert_eq!(laptop_list.publish_passes, passes + 3);
    assert!(!laptop_list.publish_local(&laptop, PERSON));
    assert_eq!(laptop_list.publish_passes, passes + 3);

    // As does bringing the directory into the tree.
    laptop_list.adopt_into(&mut laptop, &PERSON.to_string());
    laptop_list.publish_local(&laptop, PERSON);
    assert_eq!(laptop_list.publish_passes, passes + 4);
}
