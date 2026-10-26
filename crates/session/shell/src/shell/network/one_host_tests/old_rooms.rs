//! A workspace of this device's own that no host holds (one from the shell's
//! former own engine, or one put back from the archive) is placed by this
//! device's host as a new workspace is: under the same room and workspace
//! ids, its windows, its apps' data and its files as they were, and its apps
//! open their documents.
//!
//! The former engine's workspace comes from a real laptop's state file
//! (`tests/fixtures/shell-engine-workspace/state.json`), loaded by the device
//! that wrote it: its windows carry that device's salt. A device loading
//! another device's state file is not a case the shell has.

use std::path::Path;
use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;

use super::{Device, Person, Wakes, scratch, until};
use crate::shell::state::RoomId;

const FIXTURE: &str = "tests/fixtures/shell-engine-workspace/state.json";
/// The workspace the fixture holds, kept by the former engine.
const OLD_ROOM: &str = "5ba25f73-87c5-447f-a5a6-880dc4249a81";
/// The salt of the laptop the fixture was cut from: every id it minted
/// carries it.
const LAPTOP_SALT: u16 = 0xF276;

/// The fixture's entry for `room` in a state file's workspace list.
fn workspace_entry(state: &Path, room: &str) -> Option<serde_json::Value> {
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(state).ok()?).ok()?;
    file.get("workspaces")?
        .as_array()?
        .iter()
        .find(|workspace| workspace.get("room").and_then(serde_json::Value::as_str) == Some(room))
        .cloned()
}

#[test]
#[serial_test::serial]
fn a_device_with_a_former_engines_workspace_starts_and_its_host_places_it_as_it_was() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(81);
    let old = RoomId::parse(OLD_ROOM).unwrap();
    let device_dir = dir.path().join("desktop");
    std::fs::create_dir_all(&device_dir).unwrap();
    let state = device_dir.join("state.json");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE), &state).unwrap();
    let before = workspace_entry(&state, OLD_ROOM).expect("the fixture holds the old workspace");

    // A file its apps kept in that workspace's room.
    let data_home = launcher::appdir::host_data_home().expect("a data home");
    let room_files = data_home
        .join(launcher::appdir::ROOMS_DIR)
        .join(launcher::appdir::room_slug(Some(OLD_ROOM)));
    std::fs::create_dir_all(&room_files).unwrap();
    let kept = room_files.join("notes.txt");
    std::fs::write(&kept, b"written before the one host").unwrap();
    // And a record Notes kept there.
    let note = room_files
        .join("so.tilo.notes/.local/share/so.tilo.notes/rooms")
        .join(OLD_ROOM)
        .join("records/derived_default_tab_1/state.automerge");
    std::fs::create_dir_all(note.parent().unwrap()).unwrap();
    std::fs::write(&note, b"a note written before the one host").unwrap();

    // It boots from that file as a session does: its own rooms are claimed
    // for the person it acts as before the network starts.
    let (mut tree, notice) =
        crate::shell::persist::load_device_tree_or_default(&state, LAPTOP_SALT);
    assert!(notice.is_none(), "the state file reads cleanly: {notice:?}");
    tree.claim_own_unhosted_rooms(|_| ada.id());
    assert!(
        tree.pending_placements.contains(&old),
        "the old workspace waits for this device's host"
    );
    let device = Device::start_with(
        &ada,
        "Ada's desktop",
        &device_dir,
        LAPTOP_SALT,
        DeviceCapabilities::primary(),
        &wakes,
        tree,
    );
    until(&[&device], "the device to listen", || {
        device.net().dial_addr()
    });
    // Everything its start set in motion has run: a new workspace it makes
    // now is hosted, so whatever the start does with the old one is done.
    let (_, fresh) = super::hosted_workspace(&device, &[], "Fresh");
    crate::shell::persist::wait_for_saves();

    until(&[&device], "the old workspace to be placed", || {
        device.hosted_rooms().contains(&old).then_some(())
    });
    let mut hosted = device.hosted_rooms();
    hosted.sort_unstable();
    let mut expected = vec![old, fresh];
    expected.sort_unstable();
    assert_eq!(hosted, expected, "the old room is served as the new one is");
    until(&[&device], "the old room to be held", || {
        device.tree.read().holds_room(old).then_some(())
    });
    assert_eq!(
        device.tree.read().documents_unavailable(old),
        None,
        "its apps open their documents"
    );
    crate::shell::persist::wait_for_saves();
    let after = workspace_entry(&state, OLD_ROOM).expect("the old workspace stays");
    for field in ["id", "room", "name", "strips"] {
        assert_eq!(
            after.get(field),
            before.get(field),
            "the old workspace's {field} is as it was"
        );
    }
    assert_eq!(
        std::fs::read(&kept).unwrap(),
        b"written before the one host",
        "the old room's files stay as they were"
    );
    assert_eq!(
        std::fs::read(&note).unwrap(),
        b"a note written before the one host",
        "its Notes records stay as they were"
    );
    drop(device);
}

/// A workspace made since rooms have hosts, whose room no host holds now (put
/// back from the archive), is placed with every window it has.
#[test]
#[serial_test::serial]
fn a_workspace_of_this_devices_own_no_host_holds_is_placed_with_every_window() {
    placed_with_every_window(None, 82);
}

/// The same workspace keeps every window when its windows' ids were minted
/// under another salt than the one this device runs under: an id names a
/// window, it does not say whose workspace holds it.
#[test]
#[serial_test::serial]
fn a_workspace_whose_windows_another_salt_minted_is_placed_with_every_window() {
    placed_with_every_window(Some(LAPTOP_SALT), 83);
}

/// Place a workspace of this device's own (running under salt `0x0A51`)
/// holding one window, its ids minted under `minted` (this device's own
/// salt when `None`), and assert it is placed with that window.
fn placed_with_every_window(minted: Option<u16>, person: u8) {
    use crate::shell::state::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};
    const SALT: u16 = 0x0A51;
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(person);
    let device_dir = dir.path().join("desktop");
    std::fs::create_dir_all(&device_dir).unwrap();

    let mut tree = crate::shell::state::WorkspaceTree::default();
    tree.set_peer_salt(SALT);
    let (local, room) = tree.create_hosted_workspace("Kept");
    // Made and placed once, archived and put back: no host holds it now.
    tree.pending_placements.remove(&room);
    let mut other = minted.map(tilo_desktop_model::ids::IdMinter::new);
    let mut mint = |tree: &crate::shell::state::WorkspaceTree| match other.as_mut() {
        Some(other) => other.mint(),
        None => tree.mint_id(),
    };
    let tab = mint(&tree);
    let group = mint(&tree);
    let strip = Strip {
        id: StripId(mint(&tree)),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: group,
            tabs: vec![Tab {
                id: tab,
                title: "Notes".into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
                app_title: None,
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    };
    let workspace = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == local)
        .unwrap();
    workspace.strips.push(strip);
    let before = workspace.strips.clone();
    tree.claim_own_unhosted_rooms(|_| ada.id());
    assert!(
        tree.pending_placements.contains(&room),
        "the workspace waits for this device's host"
    );

    let device = Device::start_with(
        &ada,
        "Ada's desktop",
        &device_dir,
        SALT,
        DeviceCapabilities::primary(),
        &wakes,
        tree,
    );
    until(&[&device], "the workspace to be placed and held", || {
        (device.hosted_rooms().contains(&room) && device.tree.read().holds_room(room)).then_some(())
    });
    let tree = device.tree.read();
    assert_eq!(
        tree.documents_unavailable(room),
        None,
        "its apps open their documents"
    );
    let after = &tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .expect("the workspace stays")
        .strips;
    assert_eq!(
        serde_json::to_value(after).unwrap(),
        serde_json::to_value(&before).unwrap(),
        "every window it had is there"
    );
    drop(tree);
    drop(device);
}
