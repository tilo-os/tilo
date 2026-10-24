//! A device's own rooms: claimed for the person it acts as, on this device
//! only until a host holds them.
use super::*;

/// A desktop saved before rooms had hosts: its rooms are no host's and
/// none waits for one.
fn saved_before_hosts(path: &std::path::Path) -> Vec<u8> {
    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.pending_placements.clear();
    tree.joining_rooms.clear();
    for workspace in &mut tree.desktop.workspaces {
        workspace.policy.owner = None;
    }
    let bytes = crate::shell::persist::encode_tree(&tree)
        .expect("encoded")
        .bytes;
    std::fs::write(path, &bytes).expect("written");
    bytes
}

#[test]
fn a_room_from_before_hosts_loads_as_it_was_and_is_on_this_device_only() {
    let dir = tempfile::tempdir().expect("temp");
    let path = dir.path().join("state.json");
    let saved = saved_before_hosts(&path);

    let (mut tree, notice) = crate::shell::persist::load_or_default(&path);
    assert!(notice.is_none(), "the old file is read: {notice:?}");
    tree.persist_path = Some(path.clone());
    let shape = |tree: &WorkspaceTree| {
        tree.desktop
            .workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.id,
                    workspace.room(),
                    workspace.name.clone(),
                    workspace
                        .strips
                        .iter()
                        .map(|strip| strip.window_groups.len())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let loaded = shape(&tree);
    assert!(!loaded.is_empty());

    // Starting up claims them for the person this device acts as.
    tree.claim_own_unhosted_rooms(|_| "me".to_owned());
    assert_eq!(shape(&tree), loaded, "ids, names and layout are as saved");
    for (_, room, _, _) in &loaded {
        assert!(
            tree.on_this_device_only(*room),
            "a room no host holds is on this device only"
        );
        assert!(!tree.is_joining(*room), "it is never Joining…");
    }
    assert!(
        tree.pending_placements.is_empty(),
        "nothing waits to be placed, so nothing is tried again"
    );
    assert!(
        tree.claim_own_unhosted_rooms(|_| "me".to_owned())
            .is_empty()
    );
    crate::shell::persist::wait_for_saves();
    assert_eq!(
        std::fs::read(&path).expect("read"),
        saved,
        "nothing is written to the saved desktop"
    );
}

#[test]
fn a_new_desktops_first_workspace_waits_for_its_host() {
    let tree = WorkspaceTree::bootstrap_default();
    let home = tree.desktop.workspaces[0].room();
    assert!(
        tree.pending_placements.contains(&home),
        "first boot's Home is a room this device's host creates"
    );
    assert!(!tree.on_this_device_only(home));
}

#[test]
fn a_plain_desktops_own_rooms_are_its_persons_until_a_host_holds_them() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let own = tree.desktop.workspaces[0].room();
    tree.desktop.workspaces[0].policy.owner = None;
    tree.pending_placements.clear();
    tree.joining_rooms.clear();
    let joined = RoomId::new();
    tree.append_workspace_for_room("Someone else's", joined);
    tree.joining_rooms.insert(joined);

    let ids = |tree: &WorkspaceTree| {
        tree.desktop
            .workspaces
            .iter()
            .map(|workspace| (workspace.id, workspace.room()))
            .collect::<Vec<_>>()
    };
    let before = ids(&tree);

    let claimed = tree.claim_own_unhosted_rooms(|_| "me".to_owned());
    assert_eq!(
        ids(&tree),
        before,
        "claiming changes no workspace or room id"
    );

    let owner_of = |room: RoomId| {
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .and_then(|workspace| workspace.owner().cloned())
    };
    let own_workspaces = before.len() - 1;
    assert_eq!(
        claimed.len(),
        own_workspaces,
        "every workspace of this device's own is claimed, the joined one not"
    );
    assert_eq!(
        owner_of(own),
        Some("me".to_owned()),
        "with no host and no network, the person works in their own room as its owner"
    );
    assert!(
        tree.pending_placements.is_empty(),
        "claiming places nothing: a room waits for a host only when made to"
    );
    assert_eq!(
        owner_of(joined),
        None,
        "a room joined from elsewhere is its host's to say"
    );

    assert!(
        tree.claim_own_unhosted_rooms(|_| "me".to_owned())
            .is_empty(),
        "asked again with nothing changed, it changes nothing"
    );
    // First-login setup: the account becomes a key identity.
    let renamed = tree.claim_own_unhosted_rooms(|_| "ab".repeat(32));
    assert_eq!(renamed.len(), own_workspaces);
    assert_eq!(ids(&tree), before);
    let owner_now = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == own)
        .and_then(|workspace| workspace.owner().cloned());
    assert_eq!(
        owner_now,
        Some("ab".repeat(32)),
        "the room is the new identity's"
    );
}
