//! What the one walk over the document's nodes costs and answers, and what a
//! change costs instead.
//!
//! `Namespace::rebuild_document_index` reads each node's document once and
//! takes its placement, name, kind and record id from that object: the work
//! opening a namespace pays for the whole workspace, on load only. A rename
//! or move of this replica's own, and a peer's delta through any of the
//! apply entry points, must touch the nodes the change names instead, which
//! the same counter proves. `node_lookup_count` is the unit
//! `crates/core/fs/tests/performance_check.rs` bounds for a listing, so a
//! per-field re-read fails here rather than in a number nobody attributes.

#![allow(clippy::arithmetic_side_effects, reason = "test fixtures")]

use fs_protocol::FileDocumentId;
use iroh_blobs::Hash;

use super::tests::assert_indexes_match_rebuild;
use super::*;

fn h(b: u8) -> Hash {
    Hash::from([b; 32])
}

/// A directory holding `f0`, `f2`, ... beside `f1`, `f3`, ... at the root.
fn saved_tree(files: u64) -> Vec<u8> {
    let mut source = Namespace::new("a").unwrap();
    let dir = source.create_dir(ROOT_INO, "dir", 1).unwrap();
    for index in 0..files {
        let parent = if index.is_multiple_of(2) {
            dir
        } else {
            ROOT_INO
        };
        source
            .create_file(parent, format!("f{index}"), Some(&h(1)), 1, index)
            .unwrap();
    }
    source.save()
}

const WIDE_DIRECTORIES: usize = 10;
const WIDE_FILES: usize = 1_000;

fn wide_tree() -> Namespace {
    let mut ns = Namespace::new("a").unwrap();
    for directory in 0..WIDE_DIRECTORIES {
        let dir = ns
            .create_dir(ROOT_INO, format!("dir-{directory:03}"), 1)
            .unwrap();
        for file in 0..WIDE_FILES {
            ns.create_file(dir, format!("f-{file:04}"), Some(&h(1)), 1, file as u64)
                .unwrap();
        }
    }
    ns
}

#[test]
fn a_change_costs_the_change_not_the_tree() {
    let mut ns = wide_tree();
    let nodes = ns.doc.node_ids().count() as u64;
    assert_eq!(nodes, 10_010, "the fixture is the whole tree");
    assert_eq!(ns.indexed_document_count() as u64, nodes);

    let mut rebuilt = ns.clone();
    let reads_before = rebuilt.node_lookup_count();
    let start = std::time::Instant::now();
    rebuilt.rebuild_document_index();
    let rebuild_ms = start.elapsed().as_millis();
    eprintln!("WORK rebuild-wide-tree: {rebuild_ms} ms");
    assert_eq!(
        rebuilt.node_lookup_count() - reads_before,
        nodes,
        "rebuilding reads each node's document once, and nothing else"
    );

    // A node put under the reserved root id keeps the root record its own.
    let mut reserved = Namespace::from_bytes("a", &ns.save()).unwrap();
    let root_document = reserved.document_id_for(ROOT_INO).unwrap();
    let rows = reserved.indexed_document_count();
    let revision = reserved.indexed_document_revision().to_owned();
    let own_resource = FileDocumentId::new();
    let node = reserved
        .doc
        .put_node(own_resource.chunk(), ROOT_ID)
        .unwrap();
    reserved.doc.put(&node, "parent", ROOT_ID).unwrap();
    reserved.doc.put(&node, "name", b"loop".to_vec()).unwrap();
    reserved.doc.put(&node, "kind", "file").unwrap();
    reserved
        .doc
        .put(&node, "document_id", own_resource.0.to_string())
        .unwrap();
    reserved.rebuild_document_index();
    assert_eq!(
        reserved.indexed_document_count(),
        rows,
        "the reserved id became a row"
    );
    assert_eq!(
        reserved.indexed_document_revision(),
        revision,
        "the reserved id moved the catalogue"
    );
    assert_eq!(reserved.document_id_for(ROOT_INO), Some(root_document));
    assert!(!reserved.document_index_ambiguous.contains(&root_document));
    assert!(reserved.tree.nodes_of(own_resource).any(|id| id == ROOT_ID));
    assert!(
        !reserved
            .tree
            .nodes_of(root_document)
            .any(|id| id == ROOT_ID)
    );
    assert!(reserved.indexed_document(own_resource).is_none());

    local_changes(&mut ns, rebuild_ms);
    // `measure` compares the indexes with a rebuild after each change, the last included.
    peer_changes(&mut ns, rebuild_ms);
}

fn measure<T>(
    label: &str,
    ns: &mut Namespace,
    reads: u64,
    ms_bound: u128,
    rebuild_ms: u128,
    change: impl FnOnce(&mut Namespace) -> T,
) -> T {
    let before = ns.node_lookup_count();
    let start = std::time::Instant::now();
    let outcome = change(ns);
    let ms = start.elapsed().as_millis();
    let lookups = ns.node_lookup_count() - before;
    eprintln!(
        "WORK {label}-in-wide-tree: {lookups} lookups, {ms} ms (one whole rebuild: {rebuild_ms} ms)"
    );
    assert!(
        lookups <= reads,
        "{label} read {lookups} node documents, more than the {reads} its change touches"
    );
    assert!(
        ms <= ms_bound,
        "{label} took {ms} ms, more than its {ms_bound} ms bound (one whole rebuild: {rebuild_ms} ms)"
    );
    assert_indexes_match_rebuild(ns);
    outcome
}

fn indexed_path(ns: &Namespace, ino: u64) -> String {
    let record = ns
        .indexed_document(ns.document_id_for(ino).unwrap())
        .expect("a live node has a catalogue row");
    ns.path_for_document(&record)
        .expect("a live node sits at a path")
}

fn local_changes(ns: &mut Namespace, rebuild_ms: u128) {
    let one_directory = WIDE_FILES as u64 + 1;
    let quarter = rebuild_ms / 4;
    let dir = ns.lookup(ROOT_INO, "dir-000").unwrap().ino.0;
    measure(
        "local-rename",
        ns,
        4 * one_directory + 64,
        quarter,
        rebuild_ms,
        |ns| assert!(ns.rename(dir, "f-0000", dir, "renamed", 2)),
    );
    let renamed = ns.lookup(dir, "renamed").unwrap().ino.0;
    assert_eq!(indexed_path(ns, renamed), "/dir-000/renamed");

    let target = ns.lookup(ROOT_INO, "dir-002").unwrap().ino.0;
    measure(
        "local-move-1000-entry-dir",
        ns,
        8 * one_directory + 64,
        quarter,
        rebuild_ms,
        |ns| assert!(ns.rename(ROOT_INO, "dir-001", target, "moved", 3)),
    );
    let moved_dir = ns.lookup(target, "moved").unwrap().ino.0;
    assert_eq!(indexed_path(ns, moved_dir), "/dir-002/moved");
    let nested = ns.lookup(moved_dir, "f-0001").unwrap().ino.0;
    assert_eq!(indexed_path(ns, nested), "/dir-002/moved/f-0001");
    assert!(ns.lookup(ROOT_INO, "dir-001").is_none());
}

fn peer_changes(ns: &mut Namespace, rebuild_ms: u128) {
    let one_directory = WIDE_FILES as u64 + 1;
    let quarter = rebuild_ms / 4;
    let mut peer = Namespace::from_bytes("b", &ns.save()).unwrap();
    peer.prime_baseline();
    let peer_dir = peer.lookup(ROOT_INO, "dir-003").unwrap().ino.0;

    let here = ns.lookup(ROOT_INO, "dir-003").unwrap().ino.0;
    peer.create_file(peer_dir, "created", Some(&h(2)), 2, 4)
        .unwrap();
    let delta = peer.save_incremental();
    measure("create", ns, 64, quarter, rebuild_ms, |ns| {
        ns.apply_incremental(&delta).unwrap();
    });
    let created = ns.lookup(here, "created").unwrap().ino.0;
    assert_eq!(indexed_path(ns, created), "/dir-003/created");

    let doomed = ns.lookup(here, "f-0002").unwrap().ino.0;
    let doomed_document = ns.document_id_for(doomed);
    let theirs = peer.lookup(peer_dir, "f-0002").unwrap().ino.0;
    assert!(peer.remove(theirs));
    measure("delete", ns, 64, quarter, rebuild_ms, |ns| {
        ns.merge_doc(&mut peer).unwrap();
    });
    assert!(ns.indexed_document(doomed_document.unwrap()).is_none());

    let harvest = peer.lookup(ROOT_INO, "dir-006").unwrap().ino.0;
    assert!(peer.rename(ROOT_INO, "dir-005", harvest, "harvest", 5));
    let harvest_here = ns.lookup(ROOT_INO, "dir-006").unwrap().ino.0;
    let document = peer.save();
    measure(
        "remote-move-1000-entry-dir",
        ns,
        8 * one_directory + 64,
        3 * rebuild_ms,
        rebuild_ms,
        |ns| ns.merge_remote_and_resolve(&document).unwrap(),
    );
    let harvest_dir = ns.lookup(harvest_here, "harvest").unwrap().ino.0;
    assert_eq!(indexed_path(ns, harvest_dir), "/dir-006/harvest");
    let harvest_file = ns.lookup(harvest_dir, "f-0005").unwrap().ino.0;
    assert_eq!(indexed_path(ns, harvest_file), "/dir-006/harvest/f-0005");

    let contested_dir = peer.lookup(ROOT_INO, "dir-007").unwrap().ino.0;
    assert!(peer.rename(contested_dir, "f-0003", contested_dir, "peer-name", 6));
    let ours = ns.lookup(ROOT_INO, "dir-007").unwrap().ino.0;
    let contested = ns.lookup(ours, "f-0003").unwrap().ino.0;
    assert!(ns.rename(ours, "f-0003", ours, "local-name", 7));
    let delta = peer.save_incremental();
    measure("conflicting-rename", ns, 64, quarter, rebuild_ms, |ns| {
        ns.apply_remote_and_resolve(&delta).unwrap()
    });
    let winner = ns
        .name_of(ns.id_for_ino(contested).unwrap())
        .unwrap_or_default();
    assert_eq!(
        indexed_path(ns, contested),
        format!("/dir-007/{}", String::from_utf8_lossy(&winner))
    );
}

#[test]
fn a_folder_counts_its_subfolders_without_reading_its_files() {
    let saved = saved_tree(64);
    let mut ns = Namespace::from_bytes("a", &saved).unwrap();
    let dir = ns.lookup(ROOT_INO, "dir").unwrap().ino.0;
    ns.create_dir(dir, "inner", 1).unwrap();
    ns.create_dir(dir, "other", 1).unwrap();
    let listed = |ns: &mut Namespace, ino| -> Vec<FileNameV1> {
        ns.readdir(ino)
            .unwrap()
            .into_iter()
            .filter(|(_, kind, name)| *kind == FileType::Directory && name != "." && name != "..")
            .map(|(_, _, name)| name)
            .collect()
    };

    let before = ns.node_lookup_count();
    let at_root = ns.subfolder_names(ROOT_INO).unwrap();
    let in_dir = ns.subfolder_names(dir).unwrap();
    let reads = ns.node_lookup_count() - before;
    assert!(
        reads <= 2,
        "counting the subfolders of two folders of 32 files each read {reads} node documents"
    );
    assert_eq!(at_root, listed(&mut ns, ROOT_INO));
    assert_eq!(in_dir, listed(&mut ns, dir));
    assert_eq!(in_dir.len(), 2);
    let file = ns.lookup(ROOT_INO, "f1").unwrap().ino.0;
    assert_eq!(ns.subfolder_names(file), None);
}
