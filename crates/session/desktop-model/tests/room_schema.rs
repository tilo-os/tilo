//! Room documents written by one build must read the same in another, and
//! two peers whose documents started empty must both survive a merge.
//!
//! `fixtures/room_contents.automerge` pins the room document schema: it is a
//! saved document, so a change to the keys or value encodings the projection
//! writes makes it read differently and fails these tests. When a schema
//! change is intended, rewrite it from the current writer with
//!
//! ```text
//! cargo test -p tilo-desktop-model --test room_schema rewrite_room_fixture -- --ignored
//! ```
//!
//! and commit the rewritten fixture alongside the change.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, ROOT, ReadDoc};
use tilo_automerge::ApplyIncoming;
use tilo_desktop_model::projection::{
    EmptyStrips, ROOM_CONTENTS_KEY, ROOM_ID_KEY, ROOM_NAME_KEY, Reader, apply_admitted_room,
    contents_of, declared_room, project_owned_contents,
};
use tilo_desktop_model::{
    AppKind, DesktopTree, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    compose_id,
};

const ALICE: u16 = 0x0a;
const BOB: u16 = 0x0b;

/// Written by [`fixture_document`]: two peers whose room documents started
/// empty each minted `contents`; Bob also created an explicitly kept, empty
/// strip. Merged, so `contents` holds two candidates.
const ROOM_FIXTURE: &[u8] = include_bytes!("fixtures/room_contents.automerge");
const ROOM_FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/room_contents.automerge"
);
const ROOM_ID: &str = "6f1f2d3c-1111-4222-8333-944445555666";

fn strip(salt: u16, counter: u64, label: &str) -> Strip {
    let id = compose_id(salt, counter);
    Strip {
        id: StripId(id),
        name: label.into(),
        window_groups: vec![WindowGroup {
            id: id + 1,
            active_tab: id + 2,
            width: WindowWidth::Normal,
            maximized: false,
            tabs: vec![Tab {
                id: id + 2,
                title: label.into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other("so.tilo.notes".into()),
                    label: label.into(),
                },
            }],
        }],
    }
}

/// A fixed commit time (and a fixed actor per peer, below), so rewriting the
/// fixture under an unchanged schema reproduces the same document.
fn commit(doc: &mut AutoCommit) {
    doc.commit_with(CommitOptions::default().with_time(0));
}

/// The document [`ROOM_FIXTURE`] holds, built by the current writer.
fn fixture_document() -> AutoCommit {
    let mut base = AutoCommit::new().with_actor(ActorId::from(vec![0x01]));
    base.put(ROOT, ROOM_ID_KEY, ROOM_ID).unwrap();
    base.put(ROOT, ROOM_NAME_KEY, "Fixture").unwrap();
    commit(&mut base);

    let mut alice = base.fork().with_actor(ActorId::from(vec![0x0a]));
    project_owned_contents(
        &mut alice,
        &[strip(ALICE, 10, "Alice")],
        ALICE,
        None,
        EmptyStrips::Skip,
    )
    .unwrap();
    commit(&mut alice);

    let mut bob = base.fork().with_actor(ActorId::from(vec![0x0b]));
    let mut kept = strip(BOB, 10, "Kept");
    kept.window_groups.clear();
    project_owned_contents(
        &mut bob,
        std::slice::from_ref(&kept),
        BOB,
        None,
        EmptyStrips::Retain,
    )
    .unwrap();
    commit(&mut bob);
    project_owned_contents(
        &mut bob,
        &[kept, strip(BOB, 20, "Bob")],
        BOB,
        None,
        EmptyStrips::Skip,
    )
    .unwrap();
    commit(&mut bob);

    alice.merge_incoming(&mut bob).unwrap();
    alice
}

/// Rewrites the fixture from the current writer; see the module docs.
#[test]
#[ignore = "rewrites tests/fixtures/room_contents.automerge"]
fn rewrite_room_fixture() {
    std::fs::write(ROOM_FIXTURE_PATH, fixture_document().save()).unwrap();
}

/// The stored fixture reads exactly like the document the current writer
/// produces.
#[test]
fn the_fixture_matches_the_current_writer() {
    let fresh = fixture_document();
    let stored = AutoCommit::load(ROOM_FIXTURE).unwrap();
    assert_eq!(
        format!("{:?}", contents_of(&stored)),
        format!("{:?}", contents_of(&fresh)),
        "the room document schema changed; rewrite the fixture (see the module docs)"
    );
    assert_eq!(declared_room(&stored), declared_room(&fresh));
}

fn names(doc: &AutoCommit) -> Vec<String> {
    let mut names: Vec<String> = contents_of(doc)
        .into_iter()
        .map(|strip| strip.name.unwrap_or_default())
        .collect();
    names.sort();
    names
}

#[test]
fn the_fixture_reads_every_peers_windows_and_kept_strips() {
    let doc = AutoCommit::load(ROOM_FIXTURE).unwrap();
    assert_eq!(
        doc.get_all(ROOT, ROOM_CONTENTS_KEY).unwrap().len(),
        2,
        "the fixture holds two concurrently minted containers"
    );
    assert_eq!(names(&doc), ["Alice", "Bob", "Kept"]);
    let strips = contents_of(&doc);
    let bob = strips.iter().find(|s| s.id == compose_id(BOB, 20)).unwrap();
    assert_eq!(bob.groups[0].tabs[0].title, "Bob");
    assert!(matches!(
        bob.groups[0].tabs[0].tile,
        Some(TileContent::AppPlaceholder { .. })
    ));

    let room = RoomId::parse(ROOM_ID).unwrap();
    let mut tree = DesktopTree::default();
    assert!(apply_admitted_room(&mut tree, room, &doc).unwrap());
    assert_eq!(tree.workspaces[0].name, "Fixture");
    assert_eq!(tree.workspaces[0].strips.len(), 3);
    assert!(
        apply_admitted_room(&mut tree, RoomId::new(), &doc).is_err(),
        "a document is applied only to the room it declares"
    );
}

#[test]
fn a_kept_strip_stays_through_an_ordinary_projection_and_sweeps_across_candidates() {
    let mut doc = AutoCommit::load(ROOM_FIXTURE).unwrap();
    let mut kept = strip(BOB, 10, "Kept");
    kept.window_groups.clear();
    project_owned_contents(
        &mut doc,
        &[kept, strip(BOB, 20, "Bob")],
        BOB,
        None,
        EmptyStrips::Skip,
    )
    .unwrap();
    assert_eq!(names(&doc), ["Alice", "Bob", "Kept"]);

    // Alice closes her strip; it sat in whichever container lost, and must
    // not come back from there.
    project_owned_contents(&mut doc, &[], ALICE, None, EmptyStrips::Skip).unwrap();
    assert_eq!(names(&doc), ["Bob", "Kept"]);
}

#[test]
fn a_single_writers_document_is_complete_along_the_contents_path() {
    // `contents/<strip>/groups/<group>/tabs/<tab>`, taking the winning map at
    // each level, must reach every field a single writer projected.
    let mut doc = AutoCommit::new();
    project_owned_contents(
        &mut doc,
        &[strip(ALICE, 10, "Alice")],
        ALICE,
        None,
        EmptyStrips::Skip,
    )
    .unwrap();
    let key = |id: u64| format!("{id:016x}");
    let id = compose_id(ALICE, 10);
    let mut object = ROOT;
    for step in [
        ROOM_CONTENTS_KEY.to_owned(),
        key(id),
        "groups".into(),
        key(id + 1),
        "tabs".into(),
        key(id + 2),
    ] {
        object = doc.get(&object, step).unwrap().unwrap().1;
    }
    let (title, _) = doc.get(&object, "title").unwrap().unwrap();
    assert_eq!(title.as_str(), Some("Alice"));
    assert!(doc.get(&object, "tile_json").unwrap().is_some());
}

#[test]
fn two_peers_projecting_into_empty_documents_both_survive_the_merge() {
    for _ in 0..8 {
        let mut base = AutoCommit::new();
        base.put(ROOT, "room", ROOM_ID).unwrap();
        base.commit();
        let mut a = base.fork();
        let mut b = base.fork();
        project_owned_contents(
            &mut a,
            &[strip(ALICE, 10, "Alice")],
            ALICE,
            None,
            EmptyStrips::Skip,
        )
        .unwrap();
        project_owned_contents(
            &mut b,
            &[strip(BOB, 10, "Bob")],
            BOB,
            None,
            EmptyStrips::Skip,
        )
        .unwrap();
        a.merge_incoming(&mut b).unwrap();
        assert_eq!(names(&a), ["Alice", "Bob"]);

        // Each peer's later writes land in the winning container and its
        // sweep reaches the losing one.
        project_owned_contents(
            &mut a,
            &[strip(ALICE, 10, "Alice"), strip(ALICE, 30, "Again")],
            ALICE,
            None,
            EmptyStrips::Skip,
        )
        .unwrap();
        project_owned_contents(&mut a, &[], BOB, None, EmptyStrips::Skip).unwrap();
        assert_eq!(names(&a), ["Again", "Alice"]);

        let room = RoomId::parse(ROOM_ID).unwrap();
        let mut viewer = DesktopTree::default();
        apply_admitted_room(&mut viewer, room, &a).unwrap();
        let mut owner = viewer.clone();
        assert!(!tilo_desktop_model::projection::merge_room_contents(
            &mut owner,
            room,
            contents_of(&a),
            Reader::Owner(ALICE),
        ));
    }
}
