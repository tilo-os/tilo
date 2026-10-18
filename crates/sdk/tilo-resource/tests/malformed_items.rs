//! One malformed item never makes its record unreadable, the same item id
//! made on two replicas is one complete item, and a compared write never
//! stores half an item.
#![allow(clippy::unwrap_used, reason = "test assertions")]

use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ReadDoc, Value};
use tilo_automerge::ApplyIncoming;
use tilo_resource::{Collection, Delta, Id, Model, Slot, read_key, write_key};

#[derive(Model, Debug, Clone, PartialEq)]
pub struct Group {
    pub owner: Id,
    pub name: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub groups: Collection<Group>,
}

/// A board record whose collection holds `ok` whole and `broken` without
/// its required `owner`: what a half-written item looks like on disk.
fn board_with_a_broken_item() -> automerge::AutoCommit {
    let mut doc = automerge::AutoCommit::new();
    let mut board = Board::default();
    board.groups.push(
        Id::from_raw("ok"),
        Group {
            owner: Id::from_raw("home"),
            name: "Fine".into(),
        },
    );
    board.groups.push(
        Id::from_raw("broken"),
        Group {
            owner: Id::from_raw("home"),
            name: "Channels".into(),
        },
    );
    write_key(&mut doc, &ROOT, "board", &board).unwrap();
    let value = value_of(&doc, "broken");
    doc.delete(&value, "owner").unwrap();
    doc.commit();
    doc
}

fn groups_map(doc: &automerge::AutoCommit) -> automerge::ObjId {
    let Some((_, board)) = doc.get(ROOT, "board").unwrap() else {
        panic!("the board is stored");
    };
    let Some((_, groups)) = doc.get(&board, "groups").unwrap() else {
        panic!("the groups are stored");
    };
    groups
}

fn value_of(doc: &automerge::AutoCommit, id: &str) -> automerge::ObjId {
    let groups = groups_map(doc);
    let Some((_, entry)) = doc.get(&groups, id).unwrap() else {
        panic!("{id} is stored");
    };
    let Some((_, value)) = doc.get(&entry, "v").unwrap() else {
        panic!("{id} has a value");
    };
    value
}

#[test]
fn a_malformed_item_is_quarantined_and_the_rest_reads() {
    let doc = board_with_a_broken_item();
    let board: Board = read_key(&doc, &ROOT, "board").expect("the record still reads");
    assert_eq!(
        board.groups.ids().map(Id::as_str).collect::<Vec<_>>(),
        ["ok"],
        "the rest of the collection is shown"
    );
    let quarantined = board.groups.quarantined();
    assert_eq!(quarantined.len(), 1);
    let held = quarantined.first().unwrap();
    assert_eq!(held.id.as_str(), "broken");
    assert!(
        held.reason.contains("owner") && held.reason.contains("missing"),
        "the diagnostic names what is missing: {}",
        held.reason
    );
}

#[test]
fn a_quarantined_item_survives_writes_and_is_repaired_by_a_whole_item() {
    let mut doc = board_with_a_broken_item();
    let mut board: Board = read_key(&doc, &ROOT, "board").unwrap();
    // A whole write of the rest keeps the broken item for its repair.
    board.groups.get_mut(&Id::from_raw("ok")).unwrap().name = "Renamed".into();
    write_key(&mut doc, &ROOT, "board", &board).unwrap();
    assert!(doc.get(&groups_map(&doc), "broken").unwrap().is_some());

    board.groups.push(
        Id::from_raw("broken"),
        Group {
            owner: Id::from_raw("home"),
            name: "Channels".into(),
        },
    );
    assert!(board.groups.quarantined().is_empty());
    write_key(&mut doc, &ROOT, "board", &board).unwrap();
    let repaired: Board = read_key(&doc, &ROOT, "board").unwrap();
    assert!(repaired.groups.quarantined().is_empty());
    assert_eq!(
        repaired.groups.get(&Id::from_raw("broken")).unwrap().owner,
        Id::from_raw("home")
    );
    assert_eq!(
        repaired.groups.get(&Id::from_raw("ok")).unwrap().name,
        "Renamed"
    );
}

#[test]
fn a_discarded_quarantined_item_is_deleted() {
    let mut doc = board_with_a_broken_item();
    let mut board: Board = read_key(&doc, &ROOT, "board").unwrap();
    let before = board.clone();
    assert!(board.groups.discard(&Id::from_raw("broken")));
    board
        .store_changed(
            &before,
            &mut doc,
            Slot::Key(&ROOT, "board"),
            &Delta::nothing(),
        )
        .unwrap();
    assert!(doc.get(&groups_map(&doc), "broken").unwrap().is_none());
    let read: Board = read_key(&doc, &ROOT, "board").unwrap();
    assert!(read.groups.quarantined().is_empty());
    assert_eq!(read.groups.len(), 1);
}

/// Two replicas each make the item `shared` from nothing; each made its
/// own entry map, and the document keeps both under one key. Here one copy
/// lacks `owner`, as a copy half-made by a compared write did.
#[test]
fn the_same_id_made_on_two_replicas_reads_as_one_complete_item() {
    let mut base = automerge::AutoCommit::new();
    write_key(&mut base, &ROOT, "board", &Board::default()).unwrap();
    base.commit();
    let mut left = base.fork();
    let mut right = base.fork();
    let make = |doc: &mut automerge::AutoCommit, owner: Option<&str>, name: &str| {
        let groups = groups_map(doc);
        let entry = doc.put_object(&groups, "shared", ObjType::Map).unwrap();
        doc.put(&entry, "k", "a0").unwrap();
        let value = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        if let Some(owner) = owner {
            doc.put(&value, "owner", owner).unwrap();
        }
        doc.put(&value, "name", name).unwrap();
        doc.commit();
    };
    make(&mut left, Some("home"), "Channels");
    make(&mut right, None, "Channels");
    left.merge_incoming(&mut right).unwrap();
    right.merge_incoming(&mut left).unwrap();

    for doc in [&mut left, &mut right] {
        let groups = groups_map(doc);
        assert_eq!(
            doc.get_all(&groups, "shared").unwrap().len(),
            2,
            "both copies are kept"
        );
        let board: Board = read_key(doc, &ROOT, "board").expect("the record reads");
        assert!(board.groups.quarantined().is_empty());
        let shared = board.groups.get(&Id::from_raw("shared")).expect("one item");
        assert_eq!(shared.owner, Id::from_raw("home"));
        assert_eq!(shared.name, "Channels");
        assert_eq!(board.groups.len(), 1);

        // The next write stores it whole over the winning copy, which then
        // reads complete on its own.
        let before = board.clone();
        board
            .store_changed(&before, doc, Slot::Key(&ROOT, "board"), &Delta::nothing())
            .unwrap();
        let Some((Value::Object(ObjType::Map), winner)) = doc.get(&groups, "shared").unwrap()
        else {
            panic!("the entry is a map");
        };
        let Some((_, value)) = doc.get(&winner, "v").unwrap() else {
            panic!("the winner holds a value");
        };
        assert!(doc.get(&value, "owner").unwrap().is_some());
    }
}

/// An item whose entry a concurrent change deleted, changed by a compared
/// write: stored whole, never as the changed field alone in a new map.
#[test]
fn a_compared_write_never_stores_half_an_item() {
    let mut doc = automerge::AutoCommit::new();
    let mut board = Board::default();
    board.groups.push(
        Id::from_raw("g"),
        Group {
            owner: Id::from_raw("home"),
            name: "Channels".into(),
        },
    );
    write_key(&mut doc, &ROOT, "board", &board).unwrap();
    doc.commit();
    let before: Board = read_key(&doc, &ROOT, "board").unwrap();
    // The entry's value is gone under the record read before.
    let groups = groups_map(&doc);
    let Some((_, entry)) = doc.get(&groups, "g").unwrap() else {
        panic!("stored");
    };
    doc.delete(&entry, "v").unwrap();
    doc.commit();
    let mut now = before.clone();
    now.groups.get_mut(&Id::from_raw("g")).unwrap().name = "Text".into();
    now.store_changed(
        &before,
        &mut doc,
        Slot::Key(&ROOT, "board"),
        &Delta::nothing(),
    )
    .unwrap();
    let read: Board = read_key(&doc, &ROOT, "board").unwrap();
    assert!(read.groups.quarantined().is_empty());
    let group = read.groups.get(&Id::from_raw("g")).unwrap();
    assert_eq!(group.owner, Id::from_raw("home"));
    assert_eq!(group.name, "Text");
}

#[derive(Model, Debug, Clone, PartialEq)]
pub struct Defaulted {
    #[model(default = Id::from_raw("home"))]
    pub owner: Id,
    pub name: String,
}

#[test]
fn a_field_with_a_declared_default_repairs_an_item_missing_it() {
    let mut doc = automerge::AutoCommit::new();
    let item = doc.put_object(ROOT, "item", ObjType::Map).unwrap();
    doc.put(&item, "name", "Channels").unwrap();
    let read: Defaulted = read_key(&doc, &ROOT, "item").unwrap();
    assert_eq!(read.owner, Id::from_raw("home"));
    assert_eq!(read.name, "Channels");
}
