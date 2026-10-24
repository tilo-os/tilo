//! A device whose state file holds a workspace of the shell's former own
//! engine (`tests/fixtures/shell-engine-workspace/state.json`, cut from a
//! real laptop's state file) starts cleanly. Nothing of that workspace is
//! deleted or rewritten, on disk or in the state file, and its room is not
//! served: no host serves it and no transport carries it.

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
fn a_device_with_a_former_engines_workspace_starts_and_leaves_it_untouched_and_unserved() {
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

    // It boots from that file as a session does.
    let (tree, notice) = crate::shell::persist::load_or_default(&state);
    assert!(notice.is_none(), "the state file reads cleanly: {notice:?}");
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

    assert!(
        !device.hosted_rooms().contains(&old),
        "the old room is not served"
    );
    assert_eq!(device.hosted_rooms(), vec![fresh]);
    assert!(
        !device
            .net()
            .joined_topics
            .lock()
            .unwrap()
            .contains(&super::super::room_topic_id(old)),
        "no transport carries the old room"
    );
    assert_eq!(
        workspace_entry(&state, OLD_ROOM).as_ref(),
        Some(&before),
        "the old workspace stays in the state file as it was"
    );
    assert_eq!(
        std::fs::read(&kept).unwrap(),
        b"written before the one host",
        "the old room's files stay as they were"
    );
    drop(device);
}
