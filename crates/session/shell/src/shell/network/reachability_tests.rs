//! A desktop's relays wait until something it has recorded needs it
//! reachable: two desktops whose relays wait still find each other, after
//! which both are reachable, and an invitation names a home relay.

use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCapabilities;

#[cfg(target_os = "linux")]
use super::idle_quiet_tests::in_a_process_of_its_own;
use super::idle_quiet_tests::{
    LONE_CPU_BUDGET, LONE_IDLE, LONE_SETTLE, device_reaching, session_threads,
};
use super::{NetCmd, Reach, ShellNetwork, host_identity_for_key, needs_reachability};
use crate::network::actor::gossip_caches_drain_within;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use crate::shell::state::{GovernanceRequest, RoomId, WorkspaceTree, WorkspaceTreeHandle};

/// How long a test desktop's relays stay up once nothing needs them: long
/// enough that a pair still converging keeps them, short enough to watch
/// them rest.
pub(super) const REST_AFTER: Duration = Duration::from_secs(2);

/// Most times per second a desktop whose collaborator left may wake once its
/// relays rest and gossip's caches have drained. Measured 0.05/s in five
/// focused runs (96954-96958): the test thread's own `/proc` read, which is
/// no longer counted, and not one wakeup of the network runtime. The budget
/// allows two wakeups in [`LONE_IDLE`]: 0.10/s.
const RESTED_WAKEUP_BUDGET: f64 = 0.1;

/// Relays nobody listens on, so a woken test desktop's probes fail at once
/// and its peers are reached directly over loopback.
pub(super) fn unused_relays() -> iroh::RelayMap {
    ["http://127.0.0.1:9/", "http://127.0.0.1:10/"]
        .into_iter()
        .map(|url| url.parse::<iroh::RelayUrl>().unwrap())
        .collect()
}

fn endpoint(seed: u8) -> String {
    iroh::SecretKey::from_bytes(&[seed; 32])
        .public()
        .to_string()
}

fn lone_tree() -> (WorkspaceTree, RoomId) {
    let mut tree = WorkspaceTree::default();
    let workspace = tree.create_hosted_workspace("Home").0;
    let room = tree.room_of(workspace).unwrap();
    (tree, room)
}

#[test]
fn a_desktop_with_nobody_to_dial_it_needs_no_relays() {
    let (tree, _) = lone_tree();
    assert_eq!(
        needs_reachability(&tree, &endpoint(1), &[0; 32], false, false),
        None
    );
}

#[test]
fn a_room_naming_only_this_device_is_not_shared() {
    let (mut tree, room) = lone_tree();
    tree.cache_admitted_peer_names_for_test(room, &[(endpoint(1), "Me")]);
    assert_eq!(
        needs_reachability(&tree, &endpoint(1), &[0; 32], false, false),
        None
    );
}

#[test]
fn a_room_whose_admitted_document_names_another_device_needs_relays() {
    let (mut tree, room) = lone_tree();
    tree.cache_admitted_peer_names_for_test(room, &[(endpoint(1), "Me"), (endpoint(2), "Ada")]);
    assert!(needs_reachability(&tree, &endpoint(1), &[0; 32], false, false).is_some());
    tree.cache_admitted_peer_names_for_test(room, &[(endpoint(1), "Me")]);
    assert_eq!(
        needs_reachability(&tree, &endpoint(1), &[0; 32], false, false),
        None
    );
}

#[test]
fn an_open_invitation_or_another_device_needs_relays() {
    let (tree, _) = lone_tree();
    assert!(needs_reachability(&tree, &endpoint(1), &[0; 32], true, false).is_some());
    assert!(needs_reachability(&tree, &endpoint(1), &[0; 32], false, true).is_some());
}

fn until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The person whose root key is `root`.
fn person_of(root: &SigningKey) -> String {
    match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("a root key names a person: {other:?}"),
    }
}

fn workspace_name(tree: &WorkspaceTreeHandle, room: RoomId) -> Option<String> {
    tree.read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| workspace.name.clone())
}

/// The first device hosts a room only it holds, so its relays wait; the
/// second dials it, which is a remote party for both. The person's list
/// names the first device as the room's host, the second enters the room
/// there by its device key, and from then on both devices are reachable.
#[test]
#[serial_test::serial]
fn two_desktops_holding_their_relays_back_still_sync_and_then_wake_them() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[45; 32]);
    let person = person_of(&root);
    home.env().set("TILO_USER_ID", &person);

    let mut first_tree = WorkspaceTree::default();
    // A new workspace waits to be placed on this device's host.
    let (_, room) = first_tree.create_hosted_workspace("Home");
    first_tree.persist_path = Some(dir.path().join("first/workspaces.json"));
    let first = device_reaching(
        first_tree.shared(),
        &dir.path().join("first"),
        &root,
        DeviceCapabilities::primary(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(first.join_default_workspace());
    until("the first device to host its workspace", || {
        first.tick();
        first.administered_room(room)
    });

    let second_tree = WorkspaceTree {
        persist_path: Some(dir.path().join("second/workspaces.json")),
        ..WorkspaceTree::default()
    }
    .shared();
    let second = device_reaching(
        second_tree.clone(),
        &dir.path().join("second"),
        &root,
        DeviceCapabilities::member(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(second.join_default_workspace());

    let first_addr = until("the first device to listen", || first.dial_addr());
    assert!(
        first.reachability.is_dormant(),
        "a desktop alone with its own room started its relays"
    );
    second
        .cmd_tx
        .try_send(NetCmd::Connect { addr: first_addr })
        .unwrap();

    until("the first device's workspace to reach the second", || {
        first.tick();
        second.tick();
        (workspace_name(&second_tree, room).as_deref() == Some("Home")
            && second_tree.read().native_authority.holds(room))
        .then_some(())
    });
    until("both devices to start their relays", || {
        first.tick();
        second.tick();
        (!first.reachability.is_dormant() && !second.reachability.is_dormant()).then_some(())
    });
    drop((first, second));
}

/// Two desktops collaborate, then one leaves: its session ends and the room's
/// owner takes its person out of the room at the room's host. While they are
/// still a member they could come back and dial, so the relays stay up; once
/// their membership lapses, nobody else is a member and the delay passes, the
/// desktop left alone rests its relays, and from then on it wakes no more
/// than one that never collaborated ([`RESTED_WAKEUP_BUDGET`]). Gossip sweeps
/// the topics the pair linked until their last messages expire
/// ([`gossip_caches_drain_within`], 100 s), long before a real desktop rests
/// its relays five minutes after the last collaborator; this one rests them
/// after [`REST_AFTER`], so it waits that out before it is measured.
#[test]
#[serial_test::serial]
fn after_the_last_collaborator_leaves_the_relays_rest_and_the_desktop_sleeps() {
    // Every thread of the process counts below, so it measures alone.
    #[cfg(target_os = "linux")]
    if in_a_process_of_its_own(concat!(
        module_path!(),
        "::after_the_last_collaborator_leaves_the_relays_rest_and_the_desktop_sleeps"
    )) {
        return;
    }
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[46; 32]);
    let person = person_of(&root);
    home.env().set("TILO_USER_ID", &person);

    let mut first_tree = WorkspaceTree::default();
    // A new workspace waits to be placed on this device's host.
    let (home_workspace, room) = first_tree.create_hosted_workspace("Home");
    first_tree.persist_path = Some(dir.path().join("first/workspaces.json"));
    let first_tree = first_tree.shared();
    let first = device_reaching(
        first_tree.clone(),
        &dir.path().join("first"),
        &root,
        DeviceCapabilities::primary(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(first.join_default_workspace());
    until("the first device to host its workspace", || {
        first.tick();
        first.administered_room(room)
    });
    let second_tree = WorkspaceTree {
        persist_path: Some(dir.path().join("second/workspaces.json")),
        ..WorkspaceTree::default()
    }
    .shared();
    // A collaborator: another person, invited into the room by its host.
    let collaborator_root = SigningKey::from_bytes(&[47; 32]);
    let collaborator = person_of(&collaborator_root);
    let second = device_reaching(
        second_tree.clone(),
        &dir.path().join("second"),
        &collaborator_root,
        DeviceCapabilities::member(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(second.join_default_workspace());
    let first_addr = until("the first device to listen", || first.dial_addr());
    second
        .cmd_tx
        .try_send(NetCmd::Connect { addr: first_addr })
        .unwrap();
    let invitation = first.issue_invite(home_workspace, 3600).unwrap();
    assert_eq!(
        first.pending_invitations(room).len(),
        1,
        "the invitation waits to be redeemed"
    );
    // The collaborator's enrollment finishes when it is asked about, as the
    // join sheet asks.
    let PeerSyncResponse::EnrollmentPending { id } =
        second.accept_peer_invitation(&invitation).unwrap()
    else {
        panic!("redeeming the invitation enrolls with the room's host");
    };
    until("the collaborator to enroll at the room's host", || {
        first.tick();
        second.tick();
        match second.poll_enrollment(id).unwrap() {
            PeerSyncResponse::EnrollmentPending { .. } => None,
            joined => Some(joined),
        }
    });
    until("the collaborator to hold the room", || {
        first.tick();
        second.tick();
        (workspace_name(&second_tree, room).as_deref() == Some("Home")
            && second_tree.read().native_authority.holds(room))
        .then_some(())
    });
    // The room's state can reach the collaborator before its host sees them
    // as a member. Keep the collaborator until it does.
    until("the host to count the collaborator as a member", || {
        first.tick();
        second.tick();
        first.hosts_other_members().then_some(())
    });
    // Redeemed, as the room's document records, the invitation is no longer
    // pending: nobody is still to come with it.
    until("the redeemed invitation to leave the pending list", || {
        first.tick();
        first.pending_invitations(room).is_empty().then_some(())
    });
    until("the two desktops to converge with their relays up", || {
        first.tick();
        second.tick();
        (!first.reachability.is_dormant() && !second.reachability.is_dormant()).then_some(())
    });

    // The collaborator leaves: its session ends, and the owner takes them
    // out of the room at its host.
    drop(second);
    first_tree.write().request_governance(
        room,
        GovernanceRequest::Policy(tilo_action_policy::Change::RemoveMember {
            person: collaborator,
        }),
    );
    let deadline = tilo_test_env::hang_deadline();
    while !first.reachability.is_dormant() {
        assert!(
            Instant::now() < deadline,
            "the desktop left alone never rested its relays; held up by (transport, session) {:?}",
            first.reachability.holders()
        );
        first.tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !first.hosts_other_members(),
        "the removed collaborator is still a member of the room hosted here"
    );
    let rested = Instant::now();
    std::thread::sleep(LONE_SETTLE.max(gossip_caches_drain_within()));
    eprintln!(
        "a desktop whose collaborator left is measured {} s after it rested its relays",
        rested.elapsed().as_secs()
    );

    // Nothing drives this desktop now, so every thread of the process counts
    // but this one, which only sleeps and reads `/proc`: a loaded runner
    // blocks it on page faults (6 times in 20 s, run r3d54512c6c), which is
    // the runner and not the desktop.
    let measurer = Some(tilo_test_env::idle::current_thread_id().expect("this thread's id"));
    let before = session_threads(measurer);
    let started = Instant::now();
    std::thread::sleep(LONE_IDLE);
    let measured = started.elapsed();
    let wakeups =
        tilo_test_env::idle::Wakeups::between(before, session_threads(measurer), measured);
    eprintln!(
        "a desktop whose collaborator left, its threads {wakeups}  blocked {:.2} times a second",
        wakeups.blocking_per_second()
    );
    assert!(
        wakeups.blocking_per_second() <= RESTED_WAKEUP_BUDGET,
        "a desktop whose collaborator left woke {:.2} times a second, over \
         {RESTED_WAKEUP_BUDGET}/s:\n{wakeups}",
        wakeups.blocking_per_second(),
    );
    assert!(
        wakeups.total_cpu_percent() <= LONE_CPU_BUDGET,
        "a desktop whose collaborator left used {:.2} % of a core, over {LONE_CPU_BUDGET} %:\n{wakeups}",
        wakeups.total_cpu_percent(),
    );
    assert!(
        first.reachability.is_dormant(),
        "the relays started again with nobody to reach this desktop"
    );
    drop(first);
}

/// A desktop hosting its own room, its relays held back, has its host issue
/// an invitation at once, as an agent's `invitation.issue` or the palette's
/// `/invite` does: hosting a room only this device holds keeps the relays
/// down, and the invitation names the home relay the invite started, so an
/// invitee behind a NAT reaches the host without waiting for address lookup.
#[test]
#[serial_test::serial]
fn an_invite_minted_on_an_idle_desktop_names_its_home_relay() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    // The person and this device, as first login leaves them on disk: the
    // inviter signs with the identity selected for the room.
    let system = home.root().join("system");
    home.env().set("TILO_IDENTITY_ROOT", &system);
    let paths = crate::firstboot::collaboration_identity_paths();
    crate::firstboot::ensure_collaboration_identity(&home.config().join("tilo/identity"), &paths)
        .unwrap();
    let mut tree = WorkspaceTree::default();
    // A new workspace waits to be placed on this device's host.
    let (workspace, room) = tree.create_hosted_workspace("Home");
    tree.desktop.active = workspace;
    tree.persist_path = Some(home.state().join("workspaces.json"));

    let relay_runtime = tokio::runtime::Runtime::new().unwrap();
    let (_relay, relays) =
        relay_runtime.block_on(crate::network::reachability::test_support::local_relay());
    let net = ShellNetwork::spawn_with(
        tree.shared(),
        &paths.device_privkey,
        &home.state().join("peer-sync"),
        // The network outlives no test; its relay map lives as long.
        Reach::LoopbackHolding(Box::leak(Box::new(relays.clone()))),
        host_identity_for_key,
    )
    .unwrap();
    until("the desktop to listen", || net.dial_addr());
    until("the workspace to be hosted here", || {
        net.tick();
        net.administered_room(room)
    });
    assert!(
        net.reachability.is_dormant(),
        "a desktop hosting a room only it holds started its relays; held up by (transport, session) {:?}",
        net.reachability.holders()
    );

    let token = net.issue_invite(workspace, 3600).unwrap();
    let invitation = tilo_peer_state::join::decode_and_verify_invitation_text(
        &token,
        None,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let named: Vec<_> = invitation
        .grant()
        .transport
        .iter()
        .map(|hint| {
            let (host, _) =
                tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1::authenticate_transport_hint(hint)
                    .unwrap();
            host.endpoint_addr.relay_urls().cloned().collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        named,
        vec![relays.urls::<Vec<_>>()],
        "the invitation names no home relay"
    );
    assert!(
        !net.reachability.is_dormant(),
        "an open invitation holds the relays up"
    );
    drop(net);
}

/// A desktop governs the room it hosts through its own host's in-process
/// caller, as an endpoint cannot dial itself: changing a collaborator's
/// role and then removing them are the host's room governance actions, and
/// both reach the room's admitted document here.
#[test]
#[serial_test::serial]
fn a_room_hosted_here_is_governed_through_the_hosts_in_process_caller() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[48; 32]);
    home.env().set("TILO_USER_ID", person_of(&root));

    let mut first_tree = WorkspaceTree::default();
    // A new workspace waits to be placed on this device's host.
    let (workspace, room) = first_tree.create_hosted_workspace("Home");
    first_tree.persist_path = Some(dir.path().join("first/workspaces.json"));
    let first_tree = first_tree.shared();
    let first = device_reaching(
        first_tree.clone(),
        &dir.path().join("first"),
        &root,
        DeviceCapabilities::primary(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(first.join_default_workspace());
    until("the workspace to be hosted here", || {
        first.tick();
        first.administered_room(room)
    });
    let second_tree = WorkspaceTree {
        persist_path: Some(dir.path().join("second/workspaces.json")),
        ..WorkspaceTree::default()
    }
    .shared();
    let collaborator_root = SigningKey::from_bytes(&[49; 32]);
    let collaborator = person_of(&collaborator_root);
    let second = device_reaching(
        second_tree,
        &dir.path().join("second"),
        &collaborator_root,
        DeviceCapabilities::member(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(second.join_default_workspace());
    let first_addr = until("the first device to listen", || first.dial_addr());
    second
        .cmd_tx
        .try_send(NetCmd::Connect { addr: first_addr })
        .unwrap();
    let invitation = first.issue_invite(workspace, 3600).unwrap();
    let PeerSyncResponse::EnrollmentPending { id } =
        second.accept_peer_invitation(&invitation).unwrap()
    else {
        panic!("redeeming the invitation enrolls with the room's host");
    };
    until("the collaborator to enroll at the room's host", || {
        first.tick();
        second.tick();
        match second.poll_enrollment(id).unwrap() {
            PeerSyncResponse::EnrollmentPending { .. } => None,
            joined => Some(joined),
        }
    });
    let role_of = |tree: &WorkspaceTreeHandle| {
        tree.read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .and_then(|workspace| workspace.policy.members.get(&collaborator).copied())
    };
    until("the host to count the collaborator as a member", || {
        first.tick();
        second.tick();
        (first.hosts_other_members() && role_of(&first_tree).is_some()).then_some(())
    });

    first
        .ask_governor(
            room,
            GovernanceRequest::Policy(tilo_action_policy::Change::SetRole {
                person: collaborator.clone(),
                role: tilo_action_policy::Role::Commenter,
            }),
        )
        .expect("this device's host changes the collaborator's role");
    until("the role change to reach the room's document here", || {
        first.tick();
        (role_of(&first_tree) == Some(tilo_action_policy::Role::Commenter)).then_some(())
    });
    drop(second);
    first
        .ask_governor(
            room,
            GovernanceRequest::Policy(tilo_action_policy::Change::RemoveMember {
                person: collaborator.clone(),
            }),
        )
        .expect("this device's host removes the collaborator");
    until("the removal to reach the room's document here", || {
        first.tick();
        role_of(&first_tree).is_none().then_some(())
    });
    assert!(
        !first.hosts_other_members(),
        "the removed collaborator is still a member of the room hosted here"
    );
    drop(first);
}

/// A rename made on a desktop of a room its own host holds, with the relays
/// down and nobody else to reach, is the host's room governance action,
/// asked through its in-process caller: the room's admitted document here
/// carries the new name, and nothing waits.
#[test]
#[serial_test::serial]
fn a_rename_of_a_room_hosted_here_applies_with_the_relays_down() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[50; 32]);
    home.env().set("TILO_USER_ID", person_of(&root));

    let mut tree = WorkspaceTree::default();
    let (_, room) = tree.create_hosted_workspace("Home");
    tree.persist_path = Some(dir.path().join("workspaces.json"));
    let tree = tree.shared();
    let device = device_reaching(
        tree.clone(),
        dir.path(),
        &root,
        DeviceCapabilities::primary(),
        Reach::LoopbackHoldingRelays,
    );
    let name_of = |tree: &WorkspaceTreeHandle| {
        tree.read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .map(|workspace| workspace.name.clone())
    };
    until("the workspace to be hosted here", || {
        device.tick();
        tree.read().native_authority.holds(room).then_some(())
    });

    tree.write().request_governance(
        room,
        GovernanceRequest::Command(
            tilo_desktop_model::commands::DesktopCommand::RenameWorkspace {
                name: "Design".into(),
            },
        ),
    );
    until("the rename to reach the room's document here", || {
        device.tick();
        (name_of(&tree).as_deref() == Some("Design")).then_some(())
    });
    assert!(
        tree.read().governance_requests.is_empty(),
        "the rename still waits for a governor"
    );
    drop(device);
}
