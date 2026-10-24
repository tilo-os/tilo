//! This device's own host: where it keeps its rooms, when it opens, which
//! workspaces a placement pass takes, and asking another of the person's
//! devices to enter a room its host holds.
//!
//! A host serving rooms over real endpoints (placement, the native owner's
//! local session, invitations from a device-hosted room) is exercised end
//! to end in `one_host_tests`.

use super::*;
use crate::shell::network::shell_network_for_test;
use crate::shell::network::tests::personal_continuity_network_for_test;
use crate::shell::state::{Workspace, WorkspaceTree};

fn tree_with(rooms: &[(&str, RoomId)]) -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    for (index, (name, room)) in (100u64..).zip(rooms) {
        tree.desktop
            .workspaces
            .push(Workspace::in_room(WorkspaceId(index), *room, *name));
    }
    tree
}

#[test]
fn the_host_keeps_its_rooms_beside_the_native_owners_state() {
    let state = Path::new("/data/tilo/native-owner.postcard");
    assert_eq!(
        host_state_root(state),
        Path::new("/data/tilo").join(HOST_STATE_DIR)
    );
}

#[test]
fn a_device_hosted_before_only_once_its_host_wrote_something() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path().join(HOST_STATE_DIR);
    assert!(!hosted_before(&root), "no host state at all");
    std::fs::create_dir_all(&root).expect("dir");
    assert!(!hosted_before(&root), "an empty host state hosts nothing");
    std::fs::write(root.join("rooms"), b"x").expect("write");
    assert!(hosted_before(&root));
}

#[test]
fn a_placement_pass_takes_each_waiting_workspace_once() {
    let plans = RoomId::new();
    let notes = RoomId::new();
    let elsewhere = RoomId::new();
    let mut tree = tree_with(&[("Plans", plans), ("Notes", notes)]);
    tree.pending_placements.insert(plans);
    tree.pending_placements.insert(notes);
    // A room waiting with no workspace for it is not placed.
    tree.pending_placements.insert(elsewhere);

    let mut placing = BTreeSet::new();
    let first = waiting(&tree, &placing);
    let rooms: BTreeSet<RoomId> = first.iter().map(|(room, _, _)| *room).collect();
    assert_eq!(rooms, BTreeSet::from([plans, notes]));
    let plans_entry = first
        .iter()
        .find(|(room, _, _)| *room == plans)
        .expect("plans waits");
    assert_eq!(plans_entry.1, WorkspaceId(100));
    assert_eq!(plans_entry.2, "Plans");

    // While the first pass places them, a second takes neither.
    placing.extend(rooms);
    assert!(waiting(&tree, &placing).is_empty());
}

#[test]
fn a_device_that_hosts_nothing_opens_no_host() {
    let tree = WorkspaceTree::default().shared();
    let (network, _commands) = shell_network_for_test(tree, 1);
    network.open_hosted_rooms();
    assert!(network.embedded_host.host.is_none());
    assert!(network.local_host().is_err());
}

#[test]
fn a_workspace_not_placed_stays_waiting_for_the_next_pass() {
    let plans = RoomId::new();
    let mut tree = tree_with(&[("Plans", plans)]);
    tree.pending_placements.insert(plans);
    let (network, _commands) = shell_network_for_test(tree.shared(), 1);
    // This network has no host to place it on.
    network.place_pending_workspaces();
    let replica = network.replica.lock().expect("replica");
    assert!(replica.tree.read().pending_placements.contains(&plans));
    drop(replica);
    assert!(
        network
            .embedded_host
            .placing
            .lock()
            .expect("placing")
            .is_empty(),
        "the next pass takes it again"
    );
}

#[test]
fn a_room_another_device_hosts_is_entered_at_that_devices_host() {
    let root = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
    let other = ed25519_dalek::SigningKey::from_bytes(&[0xB0; 32])
        .verifying_key()
        .to_bytes();
    let user = root.verifying_key().to_bytes();
    let dir = tempfile::tempdir().expect("temp");
    let peers = dir.path().join("home-peers.json");
    std::fs::write(&peers, b"[]").expect("registry");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_HOME_PEERS_PATH", &peers);
    let (network, mut commands) = personal_continuity_network_for_test(user);
    let plans = RoomId::new();
    // The other device's host, answering what reaches it. The list is
    // followed on this thread, which holds the environment it reads.
    let host = std::thread::spawn(move || {
        while let Some(command) = commands.blocking_recv() {
            if let NetCmd::OwnDeviceHost(asked) = command {
                let request = &asked.request;
                let answer = (
                    asked.device,
                    *asked.addr.id.as_bytes(),
                    request.invoke.target.address.action.clone(),
                    match &request.invoke.argument {
                        tilo_actions::remote::InvokeArgument::Json(arguments) => {
                            arguments.get("room").cloned()
                        }
                        _ => None,
                    },
                );
                let _ = request.reply.send(Err("not answered here".into()));
                return Some(answer);
            }
        }
        None
    });
    network.follow_list(crate::shell::personal_workspaces::Adoption {
        changed: true,
        enter: vec![crate::shell::personal_workspaces::ToEnter {
            room: plans,
            name: "Plans".into(),
            host: other,
        }],
        release: Vec::new(),
    });
    let held = network
        .replica
        .lock()
        .expect("replica")
        .tree
        .read()
        .holds_room(plans);
    // The network's queue closes with it, which ends the host's wait.
    drop(network);
    let (device, endpoint, action, room) = host
        .join()
        .expect("the host answers")
        .expect("the device the list names is asked");
    assert_eq!(device, other, "the device the list names is asked");
    assert_eq!(endpoint, other);
    assert_eq!(action, ENTER_ROOM);
    assert_eq!(room, Some(serde_json::Value::from(plans.to_string())));
    assert!(!held, "a room its host did not let in is not held");
}
