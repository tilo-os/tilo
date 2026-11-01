//! A room's native route through the network actor: custody it may not
//! fall back from, and a room hosted here that is dialled nowhere.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_network_actor::native_ingress::Connect;
use tilo_network_actor::{Actor, HostIdentity, NetCmd, room_sync, room_topic_id};

/// A room placed on this device's own host.
const HOSTED_HERE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

#[test]
fn native_custody_missing_disk_never_uses_retained_certificate() {
    let (directory, owner, room, _, key) =
        crate::shell::native_workspace_owner::test_enrolled_owner();
    let certificate = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let mut identity = HostIdentity {
        device_cert: certificate,
        device_signing_key: key,
        workspace_device_certs: HashMap::default(),
        workspace_revocations: HashMap::default(),
        identity_paths: None,
    };
    let topic = room_topic_id(room);
    assert!(identity.current_native_material(topic).is_ok());
    identity.identity_paths = Some(tilo_user_identity::IdentityPaths::under(
        directory.path().join("missing-custody"),
    ));
    assert!(identity.current_native_material(topic).is_err());
    assert!(
        Connect::new(owner, room)
            .unwrap()
            .material(&identity)
            .is_err()
    );
}

/// A room this device hosts itself is dialled nowhere and its apps'
/// empty states are their own; a room whose host cannot be reached says
/// so once the try failed, rather than connecting forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_room_hosted_here_dials_nothing_and_an_unreachable_host_is_said() {
    use ui::room_sync::RoomSyncState;
    let (_directory, owner, room, _, key) =
        crate::shell::native_workspace_owner::test_enrolled_owner();
    let certificate = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let identity = HostIdentity {
        device_cert: certificate,
        device_signing_key: key.clone(),
        workspace_device_certs: HashMap::default(),
        workspace_revocations: HashMap::default(),
        identity_paths: None,
    };
    let elsewhere = Connect::new(owner, room).unwrap();
    assert!(
        !elsewhere.hosted_here(&identity),
        "the enrollment pins another device as the room's host"
    );

    // A room placed on this device's own host: the host its enrollment
    // pins is this very device.
    let governed_directory = tempfile::tempdir().unwrap();
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
    tree.persist_path = Some(governed_directory.path().join("state.json"));
    let workspace = tree.desktop.active;
    let governed = crate::shell::state::RoomId::parse(HOSTED_HERE).expect("room");
    assert!(tree.stand_in_room(workspace, governed));
    let tree = tree.shared();
    let governed_owner = crate::shell::native_workspace_owner::open(tree).unwrap();
    let governor = tilo_peer_state::owner::testing::certificate(
        &ed25519_dalek::SigningKey::from_bytes(&[72; 32]),
        &key,
        "Desktop",
    );
    tilo_peer_state::owner::testing::enroll_interactive_governor_in(
        &governed_owner,
        &key,
        &governor,
        HOSTED_HERE,
    )
    .unwrap();
    let here = Connect::new(governed_owner, governed).unwrap();
    assert!(here.hosted_here(&identity), "this device hosts it");

    let (endpoint, lookup) =
        tilo_network_actor::loopback_endpoint(Some(iroh::SecretKey::from_bytes(&key.to_bytes())))
            .await;
    let (commands, actor) = Actor::spawn_reporting_addrs(
        endpoint,
        lookup,
        Some(identity),
        Arc::new(std::sync::Mutex::new(Vec::new())),
    );
    // In this order: once the second room has an answer, the actor has
    // handled the first.
    for connect in [here, elsewhere] {
        commands
            .send(NetCmd::NativeConnect(connect, None))
            .await
            .unwrap();
    }
    let topic = room_topic_id(room);
    tilo_test_env::within("the room to say its host cannot be reached", async {
        while room_sync::state(topic) != Some(RoomSyncState::Unreachable) {
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        room_sync::state(room_topic_id(governed)),
        None,
        "a room hosted here is not waiting for anyone"
    );
    actor.abort();
}
