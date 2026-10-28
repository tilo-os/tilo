//! A window a member shares with a room whose layout is personal, from the
//! room's record of the share to the tab every other member shows.

use crate::shell::state::{Strip, Tab, TileContent, WindowGroup, WindowWidth};

mod personal_app_shares;
mod personal_window_shares;

fn tab(id: u64, tile: TileContent) -> Tab {
    Tab {
        id,
        title: format!("tab {id:x}"),
        tile,
        app_title: None,
    }
}

/// A room `host` governs whose owner admitted `person` as an editor, and
/// whose governance holds the device `device` of theirs: what tells which
/// device a window minted under its salt runs on.
fn governed_room(
    id: &str,
    host: [u8; 32],
    person: [u8; 32],
    device: [u8; 32],
) -> automerge::AutoCommit {
    use automerge::transaction::Transactable as _;
    let mut room = automerge::AutoCommit::new().with_actor(automerge::ActorId::from(host.to_vec()));
    room.put(automerge::ROOT, "room", id).expect("a room");
    // The room's authority as its owner governs it: every role's grants,
    // and the person admitted as an editor.
    let owner = hex::encode([0x11; 32]);
    let mut policy = tilo_action_policy::RoomPolicy::new(id, owner.clone());
    let governing = policy.clone();
    policy
        .apply(
            &owner,
            &governing.person_on_device(&owner, false, &[]),
            tilo_action_policy::Change::AddMember {
                person: hex::encode(person),
                role: tilo_action_policy::Role::Editor,
            },
            &tilo_action_policy::Context::at_secs(1_000),
            false,
        )
        .expect("the owner admits the person");
    tilo_desktop_model::projection::project_room_policy(&mut room, &policy)
        .expect("the room's authority");
    let identities = room
        .put_object(
            automerge::ROOT,
            "peer_identities_v1",
            automerge::ObjType::Map,
        )
        .expect("a map");
    room.put(&identities, hex::encode(device), "{}")
        .expect("the device");
    room.commit();
    room
}
