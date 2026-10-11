//! A desktop's relays wait until something needs it reachable: what is
//! already recorded at session start counts, two desktops whose relays wait
//! still find each other, after which both are reachable, an invite names a
//! home relay, and once the last collaborator has left the relays rest again
//! and the desktop sleeps as one that never collaborated.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCapabilities;

use super::idle_quiet_tests::{
    LONE_CPU_BUDGET, LONE_IDLE, LONE_SETTLE, device_reaching, session_threads,
};
use super::{NetCmd, Reach, ShellNetwork, host_identity_for_key, needs_reachability};
use crate::network::actor::gossip_caches_drain_within;
use crate::shell::state::{RoomAuthority, RoomId, WorkspaceTree, WorkspaceTreeHandle};

/// How long a test desktop's relays stay up once nothing needs them: long
/// enough that a pair still converging keeps them, short enough to watch
/// them rest.
pub(super) const REST_AFTER: Duration = Duration::from_secs(2);

/// Most times per second a desktop whose collaborator left may wake once its
/// relays rest and gossip's caches have drained. Measured 0.05/s in five
/// focused runs (96954-96958): the test thread's own `/proc` read, and not
/// one wakeup of the network runtime. A quarter of that is less than one
/// wakeup in [`LONE_IDLE`], so the budget allows one more: 0.10/s.
const RESTED_WAKEUP_BUDGET: f64 = 0.1;

/// Relays nobody listens on, so a woken test desktop's probes fail at once
/// and its peers are reached directly over loopback.
pub(super) fn unused_relays() -> iroh::RelayMap {
    ["http://127.0.0.1:9/", "http://127.0.0.1:10/"]
        .into_iter()
        .map(|url| url.parse::<iroh::RelayUrl>().unwrap())
        .collect()
}

const ME: &str = "this-endpoint";

fn lone_tree() -> (WorkspaceTree, RoomId) {
    let mut tree = WorkspaceTree::default();
    let workspace = tree.append_empty_workspace("Home");
    let room = tree.room_of(workspace).unwrap();
    (tree, room)
}

#[test]
fn a_desktop_with_nobody_to_dial_it_needs_no_relays() {
    let (tree, _) = lone_tree();
    assert_eq!(needs_reachability(&tree, ME, false, false), None);
}

#[test]
fn naming_itself_in_its_own_room_is_not_sharing_it() {
    let (mut tree, room) = lone_tree();
    tree.room_peer_names
        .entry(room)
        .or_default()
        .insert(ME.into(), "Me".into());
    tree.room_authority.insert(room, RoomAuthority::Local);
    assert_eq!(needs_reachability(&tree, ME, false, false), None);
}

#[test]
fn a_room_shared_with_a_peer_needs_relays() {
    let (mut tree, room) = lone_tree();
    tree.room_peer_names
        .entry(room)
        .or_default()
        .insert("another-endpoint".into(), "Ada".into());
    assert!(needs_reachability(&tree, ME, false, false).is_some());
}

#[test]
fn a_peer_no_longer_named_in_the_room_needs_no_relays() {
    let (mut tree, room) = lone_tree();
    tree.room_peer_names
        .entry(room)
        .or_default()
        .insert("another-endpoint".into(), "Ada".into());
    tree.room_peer_names.remove(&room);
    assert_eq!(needs_reachability(&tree, ME, false, false), None);
}

#[test]
fn a_joined_room_needs_relays() {
    let (mut tree, room) = lone_tree();
    tree.room_authority.insert(room, RoomAuthority::Unpinned);
    assert!(needs_reachability(&tree, ME, false, false).is_some());
}

#[test]
fn an_open_invitation_or_another_device_needs_relays() {
    let (tree, _) = lone_tree();
    assert!(needs_reachability(&tree, ME, true, false).is_some());
    assert!(needs_reachability(&tree, ME, false, true).is_some());
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

fn workspace_name(tree: &WorkspaceTreeHandle, room: RoomId) -> Option<String> {
    tree.read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == room)
        .map(|workspace| workspace.name.clone())
}

/// The first device has only a room of its own, so its relays wait; the
/// second dials it, which is a remote party for both. The workspace still
/// arrives, and from then on both devices are reachable.
#[test]
#[serial_test::serial]
fn two_desktops_holding_their_relays_back_still_sync_and_then_wake_them() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[45; 32]);
    let person = match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("a root key names a person: {other:?}"),
    };
    home.env().set("TILO_USER_ID", &person);

    let mut first_tree = WorkspaceTree::default();
    let home_workspace = first_tree.append_empty_workspace("Home");
    first_tree.adopt_ownerless_workspaces(&person);
    let room = first_tree.room_of(home_workspace).unwrap();
    let first = device_reaching(
        first_tree.shared(),
        &dir.path().join("first"),
        &root,
        DeviceCapabilities::primary(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(first.join_default_workspace());

    let mut second_tree = WorkspaceTree::default();
    assert!(second_tree.join_enrolled_room(room));
    let second_tree = second_tree.shared();
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
        (workspace_name(&second_tree, room).as_deref() == Some("Home")).then_some(())
    });
    until("both devices to start their relays", || {
        first.tick();
        second.tick();
        (!first.reachability.is_dormant() && !second.reachability.is_dormant()).then_some(())
    });
    drop((first, second));
}

/// The person `root` names, as `TILO_USER_ID` carries it.
fn person_of(root: &SigningKey) -> String {
    match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("a root key names a person: {other:?}"),
    }
}

/// A desktop alone with its own room, its relays held back, mints an invite
/// at once, as an agent's `invitation.issue` or the palette's `/invite` does:
/// the token names the home relay the invite started, so an invitee behind
/// a NAT reaches the inviter without waiting for address lookup.
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
    let workspace = tree.append_empty_workspace("Home");
    let room = tree.room_of(workspace).unwrap();
    tree.adopt_ownerless_workspaces(&crate::shell::state::local_user_id_for_room(&room));
    tree.desktop.active = workspace;

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
    assert!(
        net.reachability.is_dormant(),
        "a desktop alone with its own room started its relays"
    );

    let token = net.issue_invite(workspace, 3600).unwrap();
    let invite = crate::network::invite::validate_invite_token(&token).unwrap();
    assert_eq!(
        invite.owner_addr.relay_urls().collect::<Vec<_>>(),
        relays.urls::<Vec<_>>().iter().collect::<Vec<_>>(),
        "the invite names no home relay: {:?}",
        invite.owner_addr
    );
    assert!(
        !net.reachability.is_dormant(),
        "an open invitation holds the relays up"
    );
    drop(net);
}

/// Two desktops collaborate, then one leaves: its session ends and the room's
/// owner takes it out of the room. While it is still named there it could
/// come back and dial, so the relays stay up; once its membership lapses,
/// nobody is named and the delay passes, the desktop left alone rests its
/// relays, and from then on it wakes no more than one that never
/// collaborated ([`RESTED_WAKEUP_BUDGET`]). Gossip sweeps the topics the pair linked until their last
/// messages expire ([`gossip_caches_drain_within`], 100 s), long before a
/// real desktop rests its relays five minutes after the last collaborator;
/// this one rests them after [`REST_AFTER`], so it waits that out before it
/// is measured. Before iroh-gossip stopped the sweep on empty caches it ran
/// for the session: 0.60 to 0.65 blocking wakeups a second here against the
/// lone desktop's 0.35 to 0.40 (focused runs 96742-96744).
#[test]
#[serial_test::serial]
fn after_the_last_collaborator_leaves_the_relays_rest_and_the_desktop_sleeps() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[46; 32]);
    let person = person_of(&root);
    home.env().set("TILO_USER_ID", &person);

    let mut first_tree = WorkspaceTree::default();
    let home_workspace = first_tree.append_empty_workspace("Home");
    first_tree.adopt_ownerless_workspaces(&person);
    let room = first_tree.room_of(home_workspace).unwrap();
    let first_tree = first_tree.shared();
    let first = device_reaching(
        first_tree.clone(),
        &dir.path().join("first"),
        &root,
        DeviceCapabilities::primary(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(first.join_default_workspace());
    let mut second_tree = WorkspaceTree::default();
    assert!(second_tree.join_enrolled_room(room));
    let second_tree = second_tree.shared();
    let second = device_reaching(
        second_tree.clone(),
        &dir.path().join("second"),
        &root,
        DeviceCapabilities::member(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(second.join_default_workspace());
    let second_id = second.local_peer_id.to_string();
    let first_addr = until("the first device to listen", || first.dial_addr());
    second
        .cmd_tx
        .try_send(NetCmd::Connect { addr: first_addr })
        .unwrap();
    until("the two desktops to converge with their relays up", || {
        first.tick();
        second.tick();
        (workspace_name(&second_tree, room).as_deref() == Some("Home")
            && !first.reachability.is_dormant()
            && !second.reachability.is_dormant())
        .then_some(())
    });

    // The collaborator leaves: its session ends, and the owner takes it out
    // of the room.
    drop(second);
    assert!(
        first_tree
            .read()
            .room_peer_names
            .get(&room)
            .is_some_and(|names| names.contains_key(&second_id)),
        "the owner named the device it shared the room with"
    );
    assert!(first_tree.write().name_peer(room, &person, &second_id, ""));
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
    let rested = Instant::now();
    std::thread::sleep(LONE_SETTLE.max(gossip_caches_drain_within()));
    eprintln!(
        "a desktop whose collaborator left is measured {} s after it rested its relays",
        rested.elapsed().as_secs()
    );

    // Nothing drives this desktop now, so every thread of the process counts.
    let before = session_threads(None);
    let started = Instant::now();
    std::thread::sleep(LONE_IDLE);
    let measured = started.elapsed();
    let wakeups = tilo_test_env::idle::Wakeups::between(before, session_threads(None), measured);
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
