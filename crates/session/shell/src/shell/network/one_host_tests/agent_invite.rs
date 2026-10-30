//! An agent invited into a workspace its device hosts leaves the workspace's
//! apps opening: the invitation's grants and an opening app's authorship
//! are both the room's governance, written by the one host that governs it,
//! and both reach the device's own replica, which a launch waits on.

use std::sync::Arc;

use tilo_action_policy::roles::AgentPreset;
use tilo_user_identity::DeviceCapabilities;

use super::{Device, NOTES, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::state::RoomId;

/// The agents `device`'s replica of `room` admits.
fn agents(device: &Device, room: RoomId) -> Vec<String> {
    device
        .policy(room)
        .map(|policy| {
            let mut agents: Vec<String> = policy
                .grants
                .iter()
                .filter_map(|grant| match &grant.subject {
                    tilo_action_policy::Subject::Agent(agent) => Some(agent.clone()),
                    _ => None,
                })
                .collect();
            agents.sort();
            agents.dedup();
            agents
        })
        .unwrap_or_default()
}

#[test]
#[serial_test::serial]
fn an_app_opens_in_a_hosted_workspace_after_an_agent_is_invited() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(91);
    home.env().set("TILO_USER_ID", ada.id());
    let device = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A91,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (local, room) = hosted_workspace(&device, &[], "Projects");

    // An app opened before the invitation, as a session's first launch.
    device.open_app_first(room, NOTES);
    until(&[&device], "the first app authoring the room here", || {
        device.app_may_author(room, NOTES).then_some(())
    });

    // The invitation, as `agent.invite` makes it: the preset's grants,
    // asked of the room's governor.
    {
        let mut tree = device.tree.write();
        let held = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == local)
            .map(|workspace| workspace.policy.grants.clone())
            .expect("the workspace");
        let changes = AgentPreset::Contributor.admission(
            &room.to_string(),
            "claude",
            &tilo_action_policy::Subject::person(ada.id()),
            &held,
        );
        tree.govern_in_workspace(local, &ada.id(), changes)
            .expect("the owner invites the agent");
    }
    until(&[&device], "the agent a member of the room here", || {
        (agents(&device, room) == ["claude"]).then_some(())
    });

    // Opening another app afterwards: the host lets it author the room,
    // and this device's replica, which the launch waits on, carries it.
    let app = "so.tilo.tasks";
    device.open_app_first(room, app);
    until(
        &[&device],
        "the app opened after the invitation authoring the room here",
        || device.app_may_author(room, app).then_some(()),
    );
    assert_eq!(
        agents(&device, room),
        ["claude"],
        "the agent stays a member"
    );
    assert!(
        device.tree.read().governance_refusals.is_empty(),
        "nothing was refused"
    );
    drop(device);
}
