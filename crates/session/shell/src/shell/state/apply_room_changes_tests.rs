//! The shared room admission (`tilo_peer_state::room::admit_room_changes`) on
//! the shell's production entry point, [`WorkspaceTree::apply_room_changes`]:
//! nobody governs a room by answering first, a viewer cannot write, durable
//! receipts never rewind, and a persisted `Unpinned` room migrates to
//! read-only governance.
use super::*;
use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
use std::collections::BTreeMap;

const OWNER: &str = "alice-administers-the-room";

/// A host in a room it minted and owns, its room document projected.
fn hosted_room() -> (WorkspaceTree, WorkspaceId, RoomId) {
    let mut host = WorkspaceTree::default();
    host.set_salt_override(0x1111);
    let ws = host.append_empty_workspace("Design review");
    let room = host.room_of(ws).expect("a fresh workspace mints a room");
    host.adopt_ownerless_workspaces(&OWNER.to_string());
    host.sync_room_doc(room);
    (host, ws, room)
}

/// A machine that joined `room`; no invite has named its governor yet.
fn joiner(room: RoomId) -> (WorkspaceTree, WorkspaceId) {
    let mut guest = WorkspaceTree::default();
    guest.set_salt_override(0x2222);
    let ws = guest.join_room("Mine", room).expect("joins the room");
    (guest, ws)
}

fn changes_since(doc: &mut AutoCommit, heads: &[ChangeHash]) -> Vec<Vec<u8>> {
    doc.get_changes(heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

fn owner_of(tree: &WorkspaceTree, ws: WorkspaceId) -> Option<String> {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.id == ws)
        .and_then(|workspace| workspace.owner().cloned())
}

fn holds_changes(tree: &mut WorkspaceTree, room: RoomId, heads: &[ChangeHash]) -> bool {
    tree.room_docs.get_mut(&room).is_some_and(|doc| {
        heads
            .iter()
            .all(|head| doc.get_change_by_hash(head).is_some())
    })
}

#[test]
fn unpinned_room_refuses_first_comer_governance() {
    let (mut host, _, room) = hosted_room();
    let snapshot = host
        .export_room_changes_since(room, &[])
        .expect("the host is in its room");
    let (mut guest, guest_ws) = joiner(room);
    assert_eq!(guest.room_authority(room), RoomAuthority::Unpinned);

    // Whoever answers first, before any invite named a governor.
    let first = PeerOrigin::hub(1);
    assert!(
        !guest
            .apply_room_changes(room, &snapshot, &first)
            .expect("the snapshot parses"),
        "an unpinned room must not take governance from whoever answers first"
    );
    assert_eq!(
        guest.room_authority(room),
        RoomAuthority::Unpinned,
        "a refused answer pins nobody"
    );
    assert!(
        !guest.room_docs.contains_key(&room),
        "nothing of the refused batch is stored"
    );
    assert_ne!(owner_of(&guest, guest_ws).as_deref(), Some(OWNER));

    // The invite naming the host is what admits the very same snapshot.
    assert!(guest.pin_room_authority(room, first.clone()));
    assert!(
        guest
            .apply_room_changes(room, &snapshot, &first)
            .expect("the snapshot parses")
    );
    assert_eq!(owner_of(&guest, guest_ws).as_deref(), Some(OWNER));
}

#[test]
fn viewer_cannot_rewrite_room_contents() {
    let (mut host, ws, room) = hosted_room();
    let viewer = [0x33; 32];
    let editor = [0x44; 32];
    for (root, role) in [(viewer, CapRole::Viewer), (editor, CapRole::Editor)] {
        let person = person_id_of(root).expect("a root names a person");
        host.admit_in_workspace(ws, &OWNER.to_string(), &person, role)
            .expect("the owner admits a member");
    }
    host.sync_room_doc(room);

    // A strip under the member's own id salt: room content, not governance.
    let mut doc = host
        .room_docs
        .get(&room)
        .expect("the host's room document")
        .clone();
    let heads = doc.get_heads();
    let contents = match doc
        .get(ROOT, projection::ROOM_CONTENTS_KEY)
        .expect("readable")
    {
        Some((_, contents)) => contents,
        None => doc
            .put_object(ROOT, projection::ROOM_CONTENTS_KEY, ObjType::Map)
            .expect("contents"),
    };
    let strip = doc
        .put_object(
            &contents,
            projection::contents_key(compose_id(0x3333, 1)).as_str(),
            ObjType::Map,
        )
        .expect("strip");
    doc.put(&strip, "name", "Member's strip").expect("name");
    doc.commit();
    let edit = changes_since(&mut doc, &heads);
    let edit_heads = doc.get_heads();
    let held = host.room_heads(room);

    let from_viewer = PeerOrigin::hub(2);
    host.authenticated_persons
        .insert(from_viewer.clone(), viewer);
    assert!(
        !host
            .apply_room_changes(room, &edit, &from_viewer)
            .expect("the batch parses"),
        "a viewer's SyncResp is refused, as peer-state refuses it: room write forbidden"
    );
    assert_eq!(host.room_heads(room), held, "and nothing of it is stored");

    let from_editor = PeerOrigin::hub(3);
    host.authenticated_persons
        .insert(from_editor.clone(), editor);
    host.apply_room_changes(room, &edit, &from_editor)
        .expect("the batch parses");
    assert!(
        holds_changes(&mut host, room, &edit_heads),
        "the same content from an editor is admitted"
    );
}

#[test]
fn durability_rewind_refused() {
    use tilo_peer_state::{coverage, durability};
    let (host, _, room) = hosted_room();
    let governor = PeerOrigin::hub(1);
    let (mut guest, _) = joiner(room);
    assert!(guest.pin_room_authority(room, governor.clone()));
    let route = coverage::route_key("so.tilo.notes", "record");
    let receipt = |generation| {
        durability::DurableReceipt::new(
            coverage::Coverage {
                room: room.to_string(),
                app: "so.tilo.notes".into(),
                record: "record".into(),
                payload_hash: [3; 32],
                heads: vec!["head".into()],
                author: [1; 32],
                sequence: 2,
                watermarks: BTreeMap::new(),
            },
            generation,
        )
        .expect("a durable receipt")
    };

    let mut doc = host
        .room_docs
        .get(&room)
        .expect("the host's room document")
        .clone();
    durability::put(&mut doc, BTreeMap::from([(route.clone(), receipt(7))]))
        .expect("the governor publishes a receipt");
    doc.commit();
    let published = changes_since(&mut doc, &[]);
    guest
        .apply_room_changes(room, &published, &governor)
        .expect("the batch parses");
    let durable = doc.get_heads();
    assert!(
        holds_changes(&mut guest, room, &durable),
        "the governor's durable receipt is admitted"
    );

    let held = guest.room_heads(room);
    durability::put(&mut doc, BTreeMap::from([(route, receipt(6))]))
        .expect("an older generation can be written");
    doc.commit();
    let rewind = changes_since(&mut doc, &durable);
    assert!(
        !guest
            .apply_room_changes(room, &rewind, &governor)
            .expect("the batch parses"),
        "a durable receipt never rewinds, even from the governor"
    );
    assert_eq!(guest.room_heads(room), held, "and nothing of it is stored");
}

#[test]
fn a_persisted_unpinned_room_stays_read_only_until_an_authority_is_pinned() {
    let (mut host, _, room) = hosted_room();
    let snapshot = host
        .export_room_changes_since(room, &[])
        .expect("the host is in its room");
    let (guest, guest_ws) = joiner(room);

    // A state file written while the room waited for a first answer to pin
    // its governor loads as the same `Unpinned`, which now pins nobody.
    let persisted = serde_json::to_vec(&guest).expect("the tree serialises");
    let mut restored: WorkspaceTree = serde_json::from_slice(&persisted).expect("and loads");
    assert_eq!(restored.room_authority(room), RoomAuthority::Unpinned);
    let first = PeerOrigin::hub(1);
    assert!(
        !restored
            .apply_room_changes(room, &snapshot, &first)
            .expect("the snapshot parses")
    );
    assert_eq!(restored.room_authority(room), RoomAuthority::Unpinned);

    // An invite pins the authority; then governance arrives.
    assert!(restored.pin_room_authority(room, first.clone()));
    assert!(
        restored
            .apply_room_changes(room, &snapshot, &first)
            .expect("the snapshot parses")
    );
    assert_eq!(owner_of(&restored, guest_ws).as_deref(), Some(OWNER));
}

#[test]
fn a_continuation_room_is_pinned_to_the_person_and_governed_only_if_theirs() {
    let person = [0x51; 32];
    let device = PeerOrigin::hub(7);
    // Someone else's room the person joined on another of their devices.
    let (mut host, _, room) = hosted_room();
    let snapshot = host
        .export_room_changes_since(room, &[])
        .expect("the host is in its room");
    let mut tree = WorkspaceTree {
        local_person: Some(person),
        ..WorkspaceTree::default()
    };
    tree.authenticated_persons.insert(device.clone(), person);
    assert!(tree.adopt_continuation_room(room, "Laptop"));
    assert_eq!(tree.room_authority(room), RoomAuthority::Person(person));

    assert!(
        !tree
            .apply_room_changes(room, &snapshot, &PeerOrigin::hub(9))
            .expect("the snapshot parses"),
        "a stranger answering first does not supply the room's governance"
    );
    assert!(
        tree.apply_room_changes(room, &snapshot, &device)
            .expect("the snapshot parses"),
        "the person's own device relays it"
    );
    assert!(
        !tree.governs_room(room),
        "relaying someone else's room does not make the person its governor"
    );
    // The person's signed directory names the room's real governor, which
    // replaces the provisional person pin.
    let governor = PeerOrigin::hub(1);
    let person_id = person_id_of(person).expect("a root names a person");
    assert!(tree.adopt_personal_room(room, "Design review", &person_id, Some(&governor)));
    assert_eq!(tree.room_authority(room), RoomAuthority::Peer(governor));

    // A continuation room recorded `Unpinned` (no certified person yet) is
    // pinned by the next offer once the person is known.
    let mut earlier = WorkspaceTree::default();
    assert!(earlier.adopt_continuation_room(room, "Laptop"));
    assert_eq!(earlier.room_authority(room), RoomAuthority::Unpinned);
    earlier.local_person = Some(person);
    assert!(earlier.adopt_continuation_room(room, "Laptop"));
    assert_eq!(earlier.room_authority(room), RoomAuthority::Person(person));
}

/// A batch admitted without the tree held ([`WorkspaceTree::merge_room_changes`])
/// installs on top of what the room's document gained meanwhile, and a
/// batch whose sender lost the right to write meanwhile is refused.
#[test]
fn a_batch_admitted_off_the_tree_installs_on_what_the_room_gained_meanwhile() {
    let (mut host, ws, room) = hosted_room();
    let editor = [0x44; 32];
    let person = person_id_of(editor).expect("a root names a person");
    host.admit_in_workspace(ws, &OWNER.to_string(), &person, CapRole::Editor)
        .expect("the owner admits a member");
    host.sync_room_doc(room);
    let from_editor = PeerOrigin::hub(3);
    host.authenticated_persons
        .insert(from_editor.clone(), editor);

    let mut doc = host
        .room_docs
        .get(&room)
        .expect("the host's room document")
        .clone()
        .with_actor(automerge::ActorId::random());
    let heads = doc.get_heads();
    let contents = match doc
        .get(ROOT, projection::ROOM_CONTENTS_KEY)
        .expect("readable")
    {
        Some((_, contents)) => contents,
        None => doc
            .put_object(ROOT, projection::ROOM_CONTENTS_KEY, ObjType::Map)
            .expect("contents"),
    };
    let strip = doc
        .put_object(
            &contents,
            projection::contents_key(compose_id(0x4444, 1)).as_str(),
            ObjType::Map,
        )
        .expect("strip");
    doc.put(&strip, "name", "Member's strip").expect("name");
    doc.commit();
    let edit = changes_since(&mut doc, &heads);
    let edit_heads = doc.get_heads();

    let basis = host
        .room_batch_basis(room, &from_editor)
        .expect("the host takes batches for its room");
    let admitted = basis.admit(&edit, &from_editor);
    // Meanwhile the host renames its room.
    host.desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == ws)
        .expect("the room's workspace")
        .name = "Renamed meanwhile".into();
    host.sync_room_doc(room);
    let renamed = host.room_heads(room);
    assert!(
        host.install_room_batch(&basis, admitted, &edit, &from_editor)
            .expect("the batch parses")
    );
    assert!(
        holds_changes(&mut host, room, &edit_heads),
        "the batch landed"
    );
    assert!(
        holds_changes(&mut host, room, &renamed),
        "so did the rename"
    );
    assert_eq!(
        host.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == ws)
            .map(|workspace| workspace.name.as_str()),
        Some("Renamed meanwhile")
    );

    // A sender demoted while its batch was admitted is admitted again, and
    // refused.
    let held = host.room_heads(room);
    let basis = host
        .room_batch_basis(room, &from_editor)
        .expect("the host takes batches for its room");
    let mut doc = host
        .room_docs
        .get(&room)
        .expect("room")
        .clone()
        .with_actor(automerge::ActorId::random());
    let heads = doc.get_heads();
    doc.put(&strip, "name", "Edited again").expect("name");
    doc.commit();
    let again = changes_since(&mut doc, &heads);
    let admitted = basis.admit(&again, &from_editor);
    assert!(admitted.is_ok(), "an editor may write");
    host.authenticated_persons
        .insert(from_editor.clone(), [0x55; 32]);
    assert!(
        !host
            .install_room_batch(&basis, admitted, &again, &from_editor)
            .expect("the batch parses")
    );
    assert_eq!(host.room_heads(room), held, "nothing of it is stored");
}
