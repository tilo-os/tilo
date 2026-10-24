//! Whose pointer a frame carries is the device the transport authenticated,
//! never who the frame says is sitting at it. A `PresenceMoved` names a
//! `user` of its sender's choosing; a member who wrote another member's
//! identity there would otherwise wear that member's name on the cursor and
//! take their place on the room's roster.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use ui::presence::{PresenceCtx, PresenceKey};

use super::{NeighbourMsg, ShellNetwork, room_topic_id, shell_network_for_test};
use crate::network::actor::MembershipEpoch;
use crate::shell::peer_presence::PeerPresence;
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

const TAB: u64 = 0x5eed;

fn handle_of(device: &iroh::SecretKey) -> u64 {
    crate::network::behaviour::peer_liveness_handle(&device.public())
}

/// `device` proves it belongs to `root`, as its identity hello does on
/// every link it opens.
fn says_hello(
    net: &ShellNetwork,
    root: &SigningKey,
    device: &iroh::SecretKey,
    epoch: MembershipEpoch,
    topic: u64,
) {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: *device.public().as_bytes(),
        device_label: "Laptop".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).expect("a valid device certificate");
    net.dispatch_neighbour_msg(
        topic,
        device.public(),
        Some(epoch),
        false,
        NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations: Vec::new(),
            successions: Vec::new(),
        },
    );
}

/// A pointer frame in `room`, over tab [`TAB`], as a session publishes it.
fn pointer(peer: u64, user: &str, room: &str) -> NeighbourMsg {
    NeighbourMsg::PresenceMoved {
        peer,
        user: user.into(),
        key: serde_json::to_string(&PresenceKey::tile_pointer(room, TAB))
            .expect("a tile pointer key serialises"),
        point: (40.0, 30.0),
        ts: 1,
        colour: None,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario keeps the room, both members and every refusal visible"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the network and its presence relay stand for one session, alive to the last assertion"
)]
fn a_member_who_claims_anothers_identity_is_still_only_themselves() {
    let alice_root = SigningKey::from_bytes(&[0xA1; 32]);
    let alice_device = iroh::SecretKey::from_bytes(&[0xA2; 32]);
    let mallory_root = SigningKey::from_bytes(&[0x3A; 32]);
    let mallory_device = iroh::SecretKey::from_bytes(&[0x3B; 32]);
    let alice = crate::shell::collab_broker_capnp::principal_id_for_root(
        alice_root.verifying_key().to_bytes(),
    );
    let mallory = crate::shell::collab_broker_capnp::principal_id_for_root(
        mallory_root.verifying_key().to_bytes(),
    );

    // Alice's room, governed here; Mallory is an editor in it. The room
    // calls each member's device by that member's name.
    let mut workspace = Workspace::new(WorkspaceId(51), "Shared room");
    workspace.set_owner(alice.clone());
    let room = workspace.room();
    workspace.admit(mallory, tilo_action_policy::Role::Editor);
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = WorkspaceId(51);
    for (device, called) in [(&alice_device, "Alice"), (&mallory_device, "Mallory")] {
        assert!(
            tree.name_peer(room, &alice, &device.public().to_string(), called),
            "the room's owner names the devices in it",
        );
    }
    let tree = tree.shared();
    let (mut net, _commands) = shell_network_for_test(tree.clone(), 0xC);
    net.require_peer_identity = true;
    let net = Arc::new(net);
    let topic = room_topic_id(room);
    let epoch = MembershipEpoch::for_test(1);
    says_hello(&net, &mallory_root, &mallory_device, epoch, topic);

    let alice_handle = handle_of(&alice_device);
    let mallory_handle = handle_of(&mallory_device);
    let room_id = room.to_string();

    // Phase `copying_anothers_session_handle_is_dropped`. The frame's
    // session handle is bound to the transport that carried it.
    net.dispatch_neighbour_msg(
        topic,
        mallory_device.public(),
        Some(epoch),
        false,
        pointer(alice_handle, &alice, &room_id),
    );
    assert!(
        net.inbound_presence_handle()
            .lock()
            .expect("the presence queue")
            .is_empty(),
        "a frame naming Alice's session but carried by Mallory's device is refused",
    );

    // Phase `claiming_anothers_identity_names_nobody`. Mallory's own
    // session, saying Alice is sitting at it.
    net.dispatch_neighbour_msg(
        topic,
        mallory_device.public(),
        Some(epoch),
        false,
        pointer(mallory_handle, &alice, &room_id),
    );
    let queued: Vec<u64> = net
        .inbound_presence_handle()
        .lock()
        .expect("the presence queue")
        .iter()
        .map(|entry| entry.peer)
        .collect();
    assert_eq!(
        queued,
        vec![mallory_handle],
        "the frame is Mallory's, from the device that carried it",
    );

    let relay = PeerPresence::for_network(&net, vec![room_id.clone()]);
    let mut presence = PresenceCtx::default();
    let tiles = [(TAB, vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0))];
    assert!(relay.pump(&mut presence, &tiles), "the frame is folded in");
    relay.refresh_roster(&presence);

    let seen = presence
        .pointers
        .get(&PresenceKey::Workspace { id: room_id })
        .expect("Mallory's pointer is placed over the tab");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.0, format!("peer@{mallory_handle}"));
    assert_eq!(
        presence.display_name(&seen[0].0),
        "Mallory",
        "the cursor reads what the room calls its sender, not who the frame claimed",
    );

    let rows = tree
        .read()
        .desktop
        .active_workspace()
        .expect("the room is on screen")
        .collaborators
        .clone();
    assert!(
        rows.iter().all(|row| row.id != alice),
        "a claimed identity never becomes a roster row: {rows:?}",
    );
    let mallory_row = rows
        .iter()
        .find(|row| row.session == Some(mallory_handle))
        .expect("Mallory is on the roster");
    assert_eq!(mallory_row.id, format!("peer@{mallory_handle}"));
    assert_eq!(mallory_row.name, "Mallory");
    assert!(!mallory_row.invited, "and she is here");
    let alice_row = rows
        .iter()
        .find(|row| row.name == "Alice")
        .expect("Alice is listed as the room names her");
    assert_eq!(
        alice_row.session,
        Some(alice_handle),
        "Alice's row keeps Alice's session",
    );
    assert!(
        alice_row.invited,
        "and Mallory's pointer does not make Alice present",
    );
}
