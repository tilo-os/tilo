//! This device's own host: where it keeps its rooms, when it opens, which
//! workspaces a placement pass takes, and asking another of the person's
//! devices to enter a room its host holds.
//!
//! A host serving rooms over real endpoints (placement, the native owner's
//! local session, invitations from a device-hosted room) is exercised end
//! to end in `one_host_tests`.

use super::*;
use crate::shell::network::shell_network_for_test;
use crate::shell::network::tests::personal_workspaces_network_for_test;
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
    assert!(network.tree.read().pending_placements.contains(&plans));
    assert!(
        network
            .embedded_host
            .placing
            .lock()
            .expect("placing")
            .is_empty(),
        "the next pass takes it again"
    );
    assert!(
        !network.placements_pending(),
        "a network with no host to place on asks for no pass on any frame"
    );
}

#[test]
fn a_failed_placement_waits_for_something_it_depends_on_to_change() {
    let plans = RoomId::new();
    let mut tree = tree_with(&[("Plans", plans)]);
    tree.pending_placements.insert(plans);
    let (mut network, _commands) = personal_workspaces_network_for_test(
        &ed25519_dalek::SigningKey::from_bytes(&[0x61; 32]),
        &iroh::SecretKey::from_bytes(&[0x62; 32]),
        tree,
    );
    // A device that places its workspaces, whose host cannot open: it
    // keeps no workspaces on disk to host them beside.
    network.embedded_host = State::new(crate::network::local_host::LocalHost::deferred().0);
    let passes = || network.embedded_host.passes.load(Ordering::Acquire);
    network.place_pending_workspaces();
    assert_eq!(passes(), 1);
    for _ in 0..10 {
        network.place_pending_workspaces();
        network.place_pending_workspaces_off_loop();
    }
    assert_eq!(passes(), 1, "nothing changed, so no pass ran again");
    assert!(
        !network.placements_pending(),
        "the frame loop queues no pass"
    );

    // A new workspace waits too: the pass runs again, once.
    let notes = network.tree.write().create_hosted_workspace("Notes").1;
    network.place_pending_workspaces();
    network.place_pending_workspaces();
    assert_eq!(passes(), 2);
    let pending = network.tree.read().pending_placements.clone();
    assert_eq!(pending, BTreeSet::from([plans, notes]));

    // The host opening is another such change.
    network
        .embedded_host
        .host_generation
        .fetch_add(1, Ordering::AcqRel);
    assert!(network.placements_pending());
    network.place_pending_workspaces();
    assert_eq!(passes(), 3);
}

#[test]
fn a_room_another_device_hosts_is_entered_at_that_devices_host() {
    let person_root = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
    let other = ed25519_dalek::SigningKey::from_bytes(&[0xB0; 32])
        .verifying_key()
        .to_bytes();
    let dir = tempfile::tempdir().expect("temp");
    let peers = dir.path().join("home-peers.json");
    std::fs::write(&peers, b"[]").expect("registry");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_HOME_PEERS_PATH", &peers);
    // A device keeping its workspaces on disk, as every session does: its
    // native owner opens beside them.
    let tree = WorkspaceTree {
        persist_path: Some(dir.path().join("state.json")),
        ..WorkspaceTree::default()
    };
    let (network, mut commands) = personal_workspaces_network_for_test(
        &person_root,
        &iroh::SecretKey::from_bytes(&[0x52; 32]),
        tree,
    );
    let plans = RoomId::new();
    // The other device's host, answering what reaches it. The list is
    // followed on this thread, which holds the environment it reads.
    let host = std::thread::spawn(move || {
        while let Some(command) = commands.blocking_recv() {
            if let NetCmd::OwnDeviceHost(asked) = command {
                let request = &asked.governance;
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
    let held = network.tree.read().holds_room(plans);
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

/// A new workspace is this person's own room becoming hosted, not a join:
/// while its room is created on this device's host and its native owner
/// enrolls, it never says Joining…, and the window opened in it stays.
#[test]
#[serial_test::serial]
fn a_new_workspace_keeps_its_windows_and_never_joins_while_it_is_placed() {
    use crate::shell::state::compose_id;
    use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};

    let mut home = tilo_test_env::TestHome::new().expect("home");
    let dir = tempfile::tempdir().expect("temp");
    let root = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
    home.env()
        .set("TILO_USER_ID", hex::encode(root.verifying_key().to_bytes()));
    let mut tree = WorkspaceTree::default();
    tree.set_salt_override(7);
    let (workspace, room) = tree.create_hosted_workspace("Plans");
    tree.persist_path = Some(dir.path().join("workspaces.json"));
    let tab = compose_id(7, 1);
    // Its strip is this device's own, as every id it mints here is.
    let strip = &mut tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|candidate| candidate.id == workspace)
        .expect("the workspace")
        .strips[0];
    strip.id = crate::shell::state::StripId::from(compose_id(7, 3));
    strip.window_groups.push(WindowGroup {
        id: compose_id(7, 2),
        tabs: vec![Tab {
            id: tab,
            title: "Terminal".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.terminal".into(),
                label: "Terminal".into(),
                note: String::new(),
            },
            app_title: None,
        }],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    let tree = tree.shared();
    let tabs = |tree: &crate::shell::state::WorkspaceTreeHandle| {
        tree.read()
            .desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .map(|candidate| {
                candidate
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>()
            })
    };
    let network = crate::shell::network::idle_quiet_tests::device_reaching(
        tree.clone(),
        dir.path(),
        &root,
        tilo_user_identity::DeviceCapabilities::primary(),
        crate::shell::network::Reach::Loopback,
    );
    let deadline = tilo_test_env::hang_deadline();
    while !tree.read().native_authority.holds(room) {
        assert!(
            !tree.read().is_joining(room),
            "a workspace being placed says Joining…"
        );
        assert_eq!(tabs(&tree), Some(vec![tab]), "its window went away");
        assert!(
            std::time::Instant::now() < deadline,
            "the workspace was never placed"
        );
        network.tick();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    network.tick();
    assert!(!tree.read().is_joining(room));
    assert_eq!(tabs(&tree), Some(vec![tab]), "placing it took its window");
    assert!(network.administered_room(room).is_some());
    drop(network);
}

/// Placing a workspace on this device's own host waits on nothing but this
/// device: the pass that places it returns with its room held, with no
/// enrollment left running and nothing left to install.
#[test]
#[serial_test::serial]
fn a_placed_workspace_is_held_when_its_placement_pass_returns() {
    let mut home = tilo_test_env::TestHome::new().expect("home");
    let dir = tempfile::tempdir().expect("temp");
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x53; 32]);
    home.env().set(
        "TILO_USER_ID",
        hex::encode(person.verifying_key().to_bytes()),
    );
    let mut tree = WorkspaceTree::default();
    let (_, room) = tree.create_hosted_workspace("Plans");
    tree.persist_path = Some(dir.path().join("workspaces.json"));
    let tree = tree.shared();
    let network = crate::shell::network::idle_quiet_tests::device_reaching(
        tree.clone(),
        dir.path(),
        &person,
        tilo_user_identity::DeviceCapabilities::primary(),
        crate::shell::network::Reach::Loopback,
    );
    network.place_pending_workspaces();
    assert!(tree.read().native_authority.holds(room), "it is not held");
    assert!(!tree.read().pending_placements.contains(&room));
    assert!(network.enrolling_into(room).is_none());
    assert!(network.placement_failure(room).is_none());
    network.tick();
    assert_eq!(
        network.shared_room_passes(),
        0,
        "something was left to install"
    );
    drop(network);
}

/// A first boot with no network: no relay answers and the endpoint is bound
/// to this machine only. Home, waiting for this device's host, is placed
/// before apps start, without waking the relays, so an app restored into it
/// opens in it at once.
#[test]
#[serial_test::serial]
fn on_a_first_boot_with_no_network_home_is_placed_before_apps_start() {
    let mut home = tilo_test_env::TestHome::new().expect("home");
    let dir = tempfile::tempdir().expect("temp");
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x54; 32]);
    home.env().set(
        "TILO_USER_ID",
        hex::encode(person.verifying_key().to_bytes()),
    );
    let mut tree = WorkspaceTree::default();
    let (_, room) = tree.create_hosted_workspace("Home");
    tree.persist_path = Some(dir.path().join("workspaces.json"));
    let tree = tree.shared();
    let network = crate::shell::network::idle_quiet_tests::device_reaching(
        tree.clone(),
        dir.path(),
        &person,
        tilo_user_identity::DeviceCapabilities::primary(),
        crate::shell::network::Reach::LoopbackHoldingRelays,
    );
    network.place_before_launches();
    assert!(tree.read().native_authority.holds(room), "Home is not held");
    assert_eq!(
        tree.read().documents_unavailable(room),
        None,
        "an app opening in Home would wait"
    );
    assert!(
        network.reachability.is_dormant(),
        "placing Home started the relays"
    );
    drop(network);
}

#[test]
fn a_room_waiting_with_no_workspace_is_placed_no_more_and_asks_for_no_pass() {
    let plans = RoomId::new();
    let gone = RoomId::new();
    let mut tree = tree_with(&[("Plans", plans)]);
    // Its workspace is gone; the room still waits to be placed.
    tree.pending_placements.insert(gone);
    let (mut network, _commands) = personal_workspaces_network_for_test(
        &ed25519_dalek::SigningKey::from_bytes(&[0x63; 32]),
        &iroh::SecretKey::from_bytes(&[0x64; 32]),
        tree,
    );
    network.embedded_host = State::new(crate::network::local_host::LocalHost::deferred().0);
    let passes = || network.embedded_host.passes.load(Ordering::Acquire);
    assert!(network.placements_pending());
    network.place_pending_workspaces();
    assert!(
        network.tree.read().pending_placements.is_empty(),
        "the entry is dropped"
    );
    for _ in 0..10 {
        assert!(
            !network.placements_pending(),
            "the frame loop queues no pass"
        );
        network.place_pending_workspaces_off_loop();
    }
    assert_eq!(passes(), 1, "no pass ran after the one that dropped it");
}

#[test]
fn a_room_waiting_with_no_workspace_is_placed_no_more_after_a_restart() {
    let dir = tempfile::tempdir().expect("temp");
    let path = dir.path().join("state.json");
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.persist_path = Some(path.clone());
    let gone = RoomId::new();
    tree.pending_placements.insert(gone);
    let (_, notes) = tree.create_hosted_workspace("Notes");
    let (old, archived) = tree.create_hosted_workspace("Old");
    assert!(tree.archive_workspace(old));
    crate::shell::persist::save_tree_to(&tree, &path).expect("save");
    let loaded = crate::shell::persist::load_tree_from(&path).expect("load");
    assert!(
        loaded
            .pending_placements
            .is_superset(&BTreeSet::from([notes, archived])),
        "a room whose workspace is shown or archived still waits"
    );
    assert!(
        !loaded.pending_placements.contains(&gone),
        "the one with no workspace does not"
    );
}

#[test]
fn a_room_waiting_with_its_workspace_still_waits_after_a_pass() {
    let plans = RoomId::new();
    let archived = RoomId::new();
    let gone = RoomId::new();
    let mut tree = tree_with(&[("Plans", plans)]);
    tree.archived_workspaces
        .push(Workspace::in_room(WorkspaceId(200), archived, "Old"));
    tree.pending_placements.extend([plans, archived, gone]);
    let (mut network, _commands) = personal_workspaces_network_for_test(
        &ed25519_dalek::SigningKey::from_bytes(&[0x65; 32]),
        &iroh::SecretKey::from_bytes(&[0x66; 32]),
        tree,
    );
    network.embedded_host = State::new(crate::network::local_host::LocalHost::deferred().0);
    network.place_pending_workspaces();
    assert_eq!(
        network.tree.read().pending_placements,
        BTreeSet::from([plans, archived]),
        "only the room with no workspace stops waiting"
    );
}

#[test]
fn a_workspace_let_go_before_it_was_placed_is_placed_no_more() {
    let mut tree = WorkspaceTree::bootstrap_default();
    let (_, notes) = tree.create_hosted_workspace("Notes");
    assert!(tree.pending_placements.contains(&notes));
    tree.forget_personal_room(notes);
    assert!(!tree.pending_placements.contains(&notes));
}

#[test]
fn a_request_that_changed_nothing_does_not_wake_the_frame_loop() {
    use std::sync::atomic::AtomicUsize;
    let (network, _commands) = shell_network_for_test(WorkspaceTree::default().shared(), 1);
    let network = Arc::new(network);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&wakes);
    network.set_wake(Arc::new(move || {
        counted.fetch_add(1, Ordering::AcqRel);
    }));
    network.start_peer_sync_worker();
    // A placement pass with nothing to place, on a network that places none.
    assert!(network.queue_placements());
    // Requests run in order: this one runs after that pass and its wake.
    let (seen, wakes_seen) = std::sync::mpsc::channel();
    let at_job = Arc::clone(&wakes);
    network
        .run_off_loop(Box::new(move || {
            let _ = seen.send(at_job.load(Ordering::Acquire));
        }))
        .unwrap_or_else(|_| panic!("the worker runs"));
    assert_eq!(
        wakes_seen.recv().expect("the request ran"),
        0,
        "the pass that changed nothing woke the loop"
    );
    assert!(!network.placements_pending(), "nor asks for another pass");
    drop(network);
}
