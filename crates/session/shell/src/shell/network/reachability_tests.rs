//! A desktop's relays wait until something it has recorded needs it
//! reachable.

use std::time::Duration;

use super::needs_reachability;
use crate::shell::state::{RoomId, WorkspaceTree};

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
    assert_eq!(needs_reachability(&tree, &endpoint(1), false, false), None);
}

#[test]
fn a_room_naming_only_this_device_is_not_shared() {
    let (mut tree, room) = lone_tree();
    tree.cache_admitted_peer_names_for_test(room, &[(endpoint(1), "Me")]);
    assert_eq!(needs_reachability(&tree, &endpoint(1), false, false), None);
}

#[test]
fn a_room_whose_admitted_document_names_another_device_needs_relays() {
    let (mut tree, room) = lone_tree();
    tree.cache_admitted_peer_names_for_test(room, &[(endpoint(1), "Me"), (endpoint(2), "Ada")]);
    assert!(needs_reachability(&tree, &endpoint(1), false, false).is_some());
    tree.cache_admitted_peer_names_for_test(room, &[(endpoint(1), "Me")]);
    assert_eq!(needs_reachability(&tree, &endpoint(1), false, false), None);
}

#[test]
fn an_open_invitation_or_another_device_needs_relays() {
    let (tree, _) = lone_tree();
    assert!(needs_reachability(&tree, &endpoint(1), true, false).is_some());
    assert!(needs_reachability(&tree, &endpoint(1), false, true).is_some());
}
