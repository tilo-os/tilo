//! A desktop's relays wait until something it has recorded needs it
//! reachable: two desktops whose relays wait still find each other, after
//! which both are reachable, and an invitation names a home relay.

use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCapabilities;

use super::idle_quiet_tests::device_reaching;
use super::{NetCmd, Reach, ShellNetwork, host_identity_for_key, needs_reachability};
use crate::shell::state::{RoomId, WorkspaceTree, WorkspaceTreeHandle};

/// How long a test desktop's relays stay up once nothing needs them: long
/// enough that a pair still converging keeps them, short enough to watch
/// them rest.
pub(super) const REST_AFTER: Duration = Duration::from_secs(2);

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
    let person = match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("a root key names a person: {other:?}"),
    };
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
