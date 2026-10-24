//! Two devices of one person. The first hosts its workspace's room; the
//! second finds it in the person's workspace directory, enters it at the
//! first device's host and holds it as a member: its name, and later a
//! rename made where the room is hosted.

use std::sync::Arc;

use tilo_desktop_model::commands::DesktopCommand;
use tilo_user_identity::DeviceCapabilities;

use super::one_host_tests::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::state::{GovernanceRequest, RoomId};

fn workspace_name(device: &Device, room: RoomId) -> Option<String> {
    device
        .tree
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| workspace.name.clone())
}

#[test]
#[serial_test::serial]
fn an_enrolled_device_receives_and_follows_the_first_devices_workspace() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let person = Person::new(41);
    home.env().set("TILO_USER_ID", person.id());
    let first = Device::start(
        &person,
        "First device",
        &dir.path().join("first"),
        0x0C11,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let second = Device::start(
        &person,
        "Second device",
        &dir.path().join("second"),
        0x0C12,
        DeviceCapabilities::member(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&first, &[&second], "Home");

    second.reach(&first);
    until(
        &[&first, &second],
        "the adopted room to show the first device's workspace",
        || {
            (second.holds(room) && workspace_name(&second, room).as_deref() == Some("Home"))
                .then_some(())
        },
    );

    // The first device, where the room is hosted, renames it.
    first.tree.write().request_governance(
        room,
        GovernanceRequest::Command(DesktopCommand::RenameWorkspace {
            name: "Projects".into(),
        }),
    );
    until(
        &[&first, &second],
        "a later edit to reach the enrolled device",
        || (workspace_name(&second, room).as_deref() == Some("Projects")).then_some(()),
    );
    assert_eq!(workspace_name(&first, room).as_deref(), Some("Projects"));
    drop((first, second));
}
