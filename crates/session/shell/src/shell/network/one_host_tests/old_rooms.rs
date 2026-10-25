//! A device whose state file holds a workspace of the shell's former own
//! engine (`tests/fixtures/shell-engine-workspace/state.json`, cut from a
//! real laptop's state file) starts cleanly. Its workspace is the person's
//! own: this device's host places its room as it places a new workspace,
//! under the same room and workspace ids, with its windows and its files as
//! they were.

use std::path::Path;
use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;

use super::{Device, Person, Wakes, scratch, until};
use crate::shell::state::RoomId;

const FIXTURE: &str = "tests/fixtures/shell-engine-workspace/state.json";
/// The workspace the fixture holds, kept by the former engine.
const OLD_ROOM: &str = "5ba25f73-87c5-447f-a5a6-880dc4249a81";

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

    // It boots from that file as a session does: its own rooms are claimed
    // for the person it acts as before the network starts.
    let (mut tree, notice) = crate::shell::persist::load_or_default(&state);
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
        0x0A51,
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
    drop(device);
}
