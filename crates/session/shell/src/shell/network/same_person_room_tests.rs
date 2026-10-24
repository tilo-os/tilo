//! Two devices of one person, the second enrolled by the first. The new
//! device adopts the first device's workspace room from the enrollment's
//! room list and receives that workspace over the direct connection: its
//! name and the apps opened in it, and later edits.

use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::{HostIdentity, NetCmd, Reach, ShellNetwork};
use crate::shell::state::{RoomId, WorkspaceTree, WorkspaceTreeHandle};

fn device(
    tree: WorkspaceTreeHandle,
    dir: &std::path::Path,
    root: &SigningKey,
    capabilities: DeviceCapabilities,
) -> ShellNetwork {
    let root = root.clone();
    ShellNetwork::spawn_with(
        tree,
        &dir.join("device-private/keypair.bin"),
        &dir.join("peer-sync"),
        Reach::Loopback,
        move |secret, _| {
            let key = SigningKey::from_bytes(&secret.to_bytes());
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: key.verifying_key().to_bytes(),
                device_label: "Device".into(),
                capabilities,
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &root).unwrap();
            Some(HostIdentity {
                device_cert: cert,
                device_signing_key: key,
                workspace_device_certs: std::collections::HashMap::new(),
                workspace_revocations: std::collections::HashMap::new(),
                identity_paths: None,
            })
        },
    )
    .unwrap()
}

fn workspace_name(tree: &WorkspaceTreeHandle, room: RoomId) -> Option<String> {
    tree.read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| workspace.name.clone())
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

#[test]
#[serial_test::serial]
fn an_enrolled_device_receives_and_follows_the_first_devices_workspace() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[41; 32]);
    let person = match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("a root key names a person: {other:?}"),
    };
    home.env().set("TILO_USER_ID", &person);

    let mut first_tree = WorkspaceTree::default();
    let home_workspace = first_tree.create_hosted_workspace("Home").0;
    first_tree.own_workspaces_for_test(|_| person.clone());
    let room = first_tree.room_of(home_workspace).unwrap();
    let first_tree = first_tree.shared();
    let first = device(
        first_tree.clone(),
        &dir.path().join("first"),
        &root,
        DeviceCapabilities::primary(),
    );
    assert!(first.join_default_workspace());

    let mut second_tree = WorkspaceTree::default();
    assert!(second_tree.join_room("Home", room).is_some());
    let second_tree = second_tree.shared();
    let second = device(
        second_tree.clone(),
        &dir.path().join("second"),
        &root,
        DeviceCapabilities::member(),
    );
    assert!(second.join_default_workspace());

    let first_addr = until("the first device to listen", || first.dial_addr());
    second
        .cmd_tx
        .try_send(NetCmd::Connect { addr: first_addr })
        .unwrap();

    until(
        "the adopted room to show the first device's workspace",
        || {
            first.tick();
            second.tick();
            (workspace_name(&second_tree, room).as_deref() == Some("Home")).then_some(())
        },
    );

    {
        let mut tree = first_tree.write();
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room() == room)
            .unwrap()
            .name = "Projects".into();
        tree.seed_doc_from_cache();
    }
    until("a later edit to reach the enrolled device", || {
        first.tick();
        second.tick();
        (workspace_name(&second_tree, room).as_deref() == Some("Projects")).then_some(())
    });
    drop((first, second));
}
