//! A desktop's relays wait until something needs it reachable (TIL-740):
//! what already recorded at session start counts, and two desktops whose
//! relays wait still find each other, after which both are reachable.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCapabilities;

use super::idle_quiet_tests::device_reaching;
use super::{NetCmd, Reach, needs_reachability};
use crate::shell::state::{RoomAuthority, RoomId, WorkspaceTree, WorkspaceTreeHandle};

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
