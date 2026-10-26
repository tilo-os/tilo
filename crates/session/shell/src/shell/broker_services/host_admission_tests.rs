use super::NativeOwnerCommitService;

/// A room this device's own host serves admits every app opening there,
/// whether the app was launched or restored into a tab: what it needs is
/// for this device's owner to record the authorship, and an app the owner
/// has not let author yet is exactly what `app_opens_by_granting` refuses
/// to skip. A device that treats its own host's room as somebody else's
/// leaves a restored tab without authorship, so it never receives channel
/// keys in a sealed room and never publishes.
///
/// A host is the device this is about: `can_act_as_host` is what a
/// dedicated host's certificate carries, and the room it serves is its
/// own.
#[test]
fn a_room_this_devices_own_host_serves_admits_an_app_opening_there() {
    use tilo_action_broker::services::WorkspaceMemberships;
    use tilo_user_identity::cert::DeviceCapabilities;

    let mut tree = crate::shell::state::WorkspaceTree::default();
    let workspace =
        crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId(1), "host tab");
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    let tree = tree.shared();

    // This device is the room's host, and serves the room itself.
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let state_file = directory.path().join("state.json");
    let room = crate::shell::state::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    {
        let mut tree = tree.write();
        let active = tree.desktop.active;
        assert!(
            tree.stand_in_room(active, room),
            "the active workspace takes the room"
        );
        tree.persist_path = Some(state_file);
    }
    let owner = crate::shell::native_workspace_owner::open(tree.clone()).expect("the owner opens");
    let person = ed25519_dalek::SigningKey::from_bytes(&[62; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
    let mut certificate = tilo_joined_room::testing::certificate(&person, &device, "This host");
    certificate.capabilities = DeviceCapabilities {
        can_act_as_host: true,
        ..DeviceCapabilities::primary()
    };
    tilo_user_identity::cert::sign_device_cert(&mut certificate, &person)
        .expect("the certificate is signed");
    tilo_joined_room::testing::enroll_interactive_governor(&owner, &device, &certificate)
        .expect("this device's host lets its owner in");
    assert!(tree.read().holds_room(room), "the device holds its room");

    let admits = NativeOwnerCommitService::new(tree, None).app_opens_by_granting(
        &room.to_string(),
        "so.tilo.notes",
        &certificate,
    );
    drop(owner);
    drop(directory);
    assert!(
        admits,
        "an app the host's own room does not let author yet opens by granting it"
    );
}
