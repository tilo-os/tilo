//! The home relay switches of Devices & Sync, against a stand-in host.

use std::sync::Arc;

use super::tests::PairingHost;
use super::{DeviceMeshEffect, DeviceMeshModel};

pub(super) const PERSONAL_ROOM: &str = "4d6f1a2c-0000-4000-8000-000000000001";
pub(super) const STUDIO_ROOM: &str = "4d6f1a2c-0000-4000-8000-000000000002";

/// Whether the home relay hosts a workspace is the host's answer, from
/// the room's hosts: the switch shows it, and flipping one asks the host
/// for exactly that room, then reads what the room's hosts are now.
#[test]
fn a_workspace_is_put_on_the_home_relay_through_the_host() {
    use ui::tilo_actions::remote::{InvokeArgument, home_relay_hosting};
    let dir = tempfile::tempdir().unwrap();
    let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
    let host = Arc::new(PairingHost::default());
    pane.remote = ui::RemoteActions::with_host(
        host.clone(),
        ui::tilo_actions::remote::Caller::local_person("owner"),
    );
    assert!(
        pane.home_relay_section().is_none(),
        "nothing to show before the host answers"
    );

    pane.refresh_home_relay();
    pane.poll_background_tasks();
    let on_relay = |pane: &DeviceMeshModel| {
        pane.home_relay
            .hosting
            .as_ref()
            .unwrap()
            .workspaces
            .iter()
            .map(|workspace| (workspace.name.clone(), workspace.on_relay))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        on_relay(&pane),
        [("Personal".to_owned(), true), ("Studio".to_owned(), false)]
    );
    assert!(pane.home_relay_section().is_some());

    pane.apply(&DeviceMeshEffect::SetHomeRelayHosting {
        room: STUDIO_ROOM.into(),
        on: true,
    })
    .unwrap();
    assert_eq!(
        on_relay(&pane)[1],
        ("Studio".to_owned(), true),
        "the switch shows the choice at once"
    );
    pane.poll_background_tasks();
    pane.poll_background_tasks();
    let asked = host.asked();
    let set = asked
        .iter()
        .find(|request| request.target.address.action == home_relay_hosting::SET)
        .expect("the host is asked");
    let InvokeArgument::Json(arguments) = &set.argument else {
        panic!("the choice names its room");
    };
    assert_eq!(arguments["room"], STUDIO_ROOM);
    assert_eq!(arguments["on"], true);
    assert_eq!(
        on_relay(&pane),
        [("Personal".to_owned(), true), ("Studio".to_owned(), true)],
        "and the room's hosts, read again, say so"
    );
    assert!(
        pane.apply(&DeviceMeshEffect::SetHomeRelayHosting {
            room: "4d6f1a2c-0000-4000-8000-0000000000ff".into(),
            on: true,
        })
        .is_err(),
        "a workspace this device does not host is no choice here"
    );
}
