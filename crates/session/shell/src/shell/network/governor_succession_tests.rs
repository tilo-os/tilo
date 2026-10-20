//! A device that governs a room is stolen. Its person's root revokes it and
//! names another of their devices to govern in its place, and that device
//! says so in the room: a member refuses the stolen device's governance from
//! then on and takes the successor's.

use ed25519_dalek::SigningKey;
use tilo_desktop_model::PEER_ID_SALT_SHIFT;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, GovernorSuccession, RevocationEntry, RevocationReason,
    sign_device_cert, sign_revocation_entry,
};

use super::{ShellNetwork, room_topic_id, shell_network_for_test};
use crate::network::protocol::NeighbourMsg;
use crate::shell::state::{
    PeerOrigin, RoomAuthority, RoomId, Workspace, WorkspaceId, WorkspaceTree,
};

const ROOM: &str = "6b3f9a10-0000-4000-8000-0000000000a1";

struct Device {
    key: iroh::SecretKey,
    cert: DeviceCert,
}

impl Device {
    fn new(root: &SigningKey, seed: u8) -> Self {
        let key = iroh::SecretKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *key.public().as_bytes(),
            device_label: format!("device {seed}"),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).expect("certified");
        Self { key, cert }
    }

    fn origin(&self) -> PeerOrigin {
        PeerOrigin::peer(&self.key.public())
    }
}

fn person(root: &SigningKey) -> String {
    crate::shell::state::person_id_of(root.verifying_key().to_bytes())
        .expect("a root names a person")
}

fn revoke(root: &SigningKey, device: &Device) -> RevocationEntry {
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: device.cert.device_public_key,
        revoked_at_ns: 2,
        reason: RevocationReason::Stolen,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, root).expect("revoked");
    entry
}

fn hello(
    device: &Device,
    revocations: Vec<RevocationEntry>,
    successions: Vec<GovernorSuccession>,
) -> NeighbourMsg {
    NeighbourMsg::IdentityHello {
        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
        cert: Box::new(device.cert.clone()),
        revocations,
        successions,
    }
}

fn tree_with(salt: u16, room: RoomId) -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    tree.set_salt_override(salt);
    let id = WorkspaceId((u64::from(salt) << PEER_ID_SALT_SHIFT) | 1);
    let mut workspace = Workspace::new(id, "Plans");
    workspace.set_room(room);
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = id;
    tree
}

fn workspace(tree: &mut WorkspaceTree, room: RoomId) -> &mut Workspace {
    tree.desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room == room)
        .expect("in the room")
}

/// `from`'s whole room document, as `device` sends it to `net`.
fn sync(net: &ShellNetwork, from: &mut WorkspaceTree, device: &Device, room: RoomId) {
    let changes = from
        .export_room_changes_since(room, &[])
        .expect("in the room");
    net.dispatch_neighbour_msg(
        room_topic_id(room),
        device.key.public(),
        None,
        false,
        NeighbourMsg::SyncResp {
            changes,
            room: room.to_string(),
        },
    );
}

fn member(net: &ShellNetwork, room: RoomId, who: &str) -> bool {
    let tree = net.replica.lock().expect("replica").tree.clone();
    let tree = tree.read();
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == room)
        .and_then(|workspace| workspace.role_of(who))
        .is_some()
}

fn authority(net: &ShellNetwork, room: RoomId) -> RoomAuthority {
    let tree = net.replica.lock().expect("replica").tree.clone();
    tree.read().room_authority(room)
}

#[test]
fn a_revoked_governor_loses_its_rooms_to_the_device_its_root_names() {
    let _env = tilo_test_env::lock();
    let alice = SigningKey::from_bytes(&[51; 32]);
    let bob = SigningKey::from_bytes(&[54; 32]);
    let laptop = Device::new(&alice, 52);
    let phone = Device::new(&alice, 53);
    let room = RoomId::parse(ROOM).expect("room id");

    // Alice's laptop governs the room; Bob edits in it.
    let mut laptop_tree = tree_with(0xA1, room);
    workspace(&mut laptop_tree, room).set_owner(person(&alice));
    workspace(&mut laptop_tree, room).admit(person(&bob), tilo_action_policy::Role::Editor);

    // Bob accepted the laptop's invitation, which pinned it as governor.
    let mut bob_tree = tree_with(0xB1, room);
    bob_tree.pin_room_authority(room, laptop.origin());
    let (mut net, _commands) = shell_network_for_test(bob_tree.shared(), 7);
    net.require_peer_identity = true;
    let topic = room_topic_id(room);
    net.dispatch_neighbour_msg(
        topic,
        laptop.key.public(),
        None,
        false,
        hello(&laptop, vec![], vec![]),
    );
    sync(&net, &mut laptop_tree, &laptop, room);
    assert!(
        member(&net, room, &person(&bob)),
        "the governor's room arrives"
    );

    // Alice's phone holds the room as another device of its owner.
    let mut phone_tree = tree_with(0xC1, room);
    phone_tree.pin_room_authority(room, laptop.origin());
    let changes = laptop_tree
        .export_room_changes_since(room, &[])
        .expect("in the room");
    assert!(
        phone_tree
            .apply_room_changes(room, &changes, &laptop.origin())
            .expect("admits")
    );

    // The laptop is stolen. Alice's root revokes it and names her phone, which
    // says so in the room.
    let revocation = revoke(&alice, &laptop);
    let succession = GovernorSuccession::sign(
        laptop.cert.clone(),
        revocation.clone(),
        phone.cert.clone(),
        &alice,
    )
    .expect("succession");
    net.dispatch_neighbour_msg(
        topic,
        phone.key.public(),
        None,
        false,
        hello(&phone, vec![revocation], vec![succession.clone()]),
    );
    assert_eq!(
        authority(&net, room),
        RoomAuthority::Peer(phone.origin()),
        "the room follows the laptop's successor"
    );

    // Whoever holds the laptop now cannot let anyone into the room.
    workspace(&mut laptop_tree, room).admit("person-mallory", tilo_action_policy::Role::Editor);
    sync(&net, &mut laptop_tree, &laptop, room);
    assert!(
        !member(&net, room, "person-mallory"),
        "a revoked governor's governance is refused"
    );

    // The phone takes the room as its own, and governs it now.
    phone_tree.follow_governor_successions(std::slice::from_ref(&succession), &phone.origin());
    assert_eq!(phone_tree.room_authority(room), RoomAuthority::Local);
    workspace(&mut phone_tree, room).admit("person-carol", tilo_action_policy::Role::Editor);
    sync(&net, &mut phone_tree, &phone, room);
    assert!(member(&net, room, "person-carol"), "the successor governs");
    assert!(!member(&net, room, "person-mallory"));
}
