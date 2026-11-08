//! What the one walk over the document's nodes costs and answers, and what a
//! change costs instead.
//!
//! `Namespace::rebuild_record_index` reads each node's document once and
//! takes its placement, name, kind and record id from that object: the work
//! opening a namespace pays for the whole workspace, on load only. A rename
//! or move of this replica's own, and a peer's delta through any of the
//! apply entry points, must touch the nodes the change names instead, which
//! the same counter proves. `node_lookup_count` is the unit
//! `crates/system/fs/tests/performance_gate.rs` bounds for a listing, so a
//! per-field re-read fails here rather than in a number nobody attributes.

#![allow(clippy::arithmetic_side_effects, reason = "test fixtures")]

use automerge::ObjType;
use automerge::transaction::Transactable;
use fs_protocol::FileRecordId;
use iroh_blobs::Hash;

use super::tests::assert_indexes_match_rebuild;
use super::*;

fn h(b: u8) -> Hash {
    Hash::from([b; 32])
}

/// A saved namespace of `files` files in one directory under the root.
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

#[test]
fn rebuilding_the_index_reads_each_node_document_once() {
    let saved = saved_tree(64);
    let mut reopened = Namespace::from_bytes("a", &saved).unwrap();
    let nodes = reopened.doc.keys(&reopened.nodes).count() as u64;
    assert_eq!(nodes, 65, "the fixture holds the directory and its files");
    assert_eq!(reopened.indexed_record_count() as u64, nodes);

    // The walk binds every node to its stable record, so a record still
    // resolves to its live inode after the merge rebuilt the index.
    let ino = reopened.lookup(ROOT_INO, "f1").unwrap().ino.0;
    let record_id = reopened.record_id_for(ino).unwrap();
    assert_eq!(reopened.ino_for_record_id(record_id), Some(ino));

    let before = reopened.node_lookup_count();
    reopened.rebuild_record_index();
    let reads = reopened.node_lookup_count() - before;
    assert_eq!(
        reads, nodes,
        "rebuilding read {reads} node documents for {nodes} nodes"
    );

    // A peer's content edit arrives as a delta, merged and resolved: it reads
    // the one node the change names, not the 65 here. A rebuild that read a
    // node per field again would read every node and fail here.
    let mut peer = Namespace::from_bytes("b", &saved).unwrap();
    let peer_ino = peer.lookup(ROOT_INO, "f3").unwrap().ino.0;
    assert!(peer.set_content(peer_ino, Some(&h(2)), 7, 7));
    let delta = peer.save_incremental();
    let before = reopened.node_lookup_count();
    reopened.apply_remote_and_resolve(&delta).unwrap();
    let reads = reopened.node_lookup_count() - before;
    assert!(
        reads <= 32,
        "receiving a peer delta read {reads} node documents for {nodes} nodes"
    );
    let applied = reopened.lookup(ROOT_INO, "f3").unwrap().ino.0;
    assert_eq!(reopened.hash_for(applied), Some(h(2)));
}

/// The reserved root id is not a node: a document that nevertheless carries
/// one under the nodes map must not gain a row, lose the root record, or
/// change which record the structural index binds for that key.
#[test]
fn a_node_under_the_reserved_root_id_keeps_the_root_record_its_own() {
    let saved = saved_tree(8);
    let mut ns = Namespace::from_bytes("a", &saved).unwrap();
    let root_record = ns.record_id_for(ROOT_INO).unwrap();
    let rows = ns.indexed_record_count();
    let revision = ns.indexed_record_revision().to_owned();

    let own_record = FileRecordId::new();
    let node = ns.doc.put_object(&ns.nodes, ROOT_ID, ObjType::Map).unwrap();
    ns.doc.put(&node, "parent", ROOT_ID).unwrap();
    ns.doc.put(&node, "name", b"loop".to_vec()).unwrap();
    ns.doc.put(&node, "kind", "file").unwrap();
    ns.doc
        .put(&node, "record_id", own_record.0.to_string())
        .unwrap();

    ns.rebuild_record_index();

    assert_eq!(
        ns.indexed_record_count(),
        rows,
        "the reserved id became an index row"
    );
    assert_eq!(
        ns.indexed_record_revision(),
        revision,
        "the reserved id moved the record catalogue"
    );
    assert_eq!(ns.record_id_for(ROOT_INO), Some(root_record));
    assert!(!ns.record_index_ambiguous.contains(&root_record));
    // The structural index binds the node's own key, as the scan it replaced
    // did, so the root record stays the document root's.
    assert!(ns.tree.nodes_of(own_record).any(|id| id == ROOT_ID));
    assert!(!ns.tree.nodes_of(root_record).any(|id| id == ROOT_ID));
    assert!(ns.indexed_record(own_record).is_none());
}

/// A workspace the size a real Files tree reaches.
const WIDE_DIRECTORIES: usize = 100;
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

/// One change costs the change, not the 100,100-node tree: renames and moves
/// of this replica's own, and a peer's create, delete, move and concurrent
/// rename through every entry point that takes a delta, each read the nodes
/// they name, and each leave behind the index a from-scratch rebuild makes.
#[test]
fn a_change_costs_the_change_not_the_tree() {
    let mut ns = wide_tree();
    let nodes = ns.doc.keys(&ns.nodes).count() as u64;
    assert_eq!(nodes, 100_100, "the fixture is the whole tree");
    assert_eq!(ns.indexed_record_count() as u64, nodes);

    // What one whole walk of this tree costs, so each change is bounded
    // against real work on this machine rather than a wall-clock number.
    let mut rebuilt = ns.clone();
    let start = std::time::Instant::now();
    rebuilt.rebuild_record_index();
    let rebuild_ms = start.elapsed().as_millis();
    eprintln!("WORK rebuild-100k-tree: {rebuild_ms} ms");

    local_changes(&mut ns, rebuild_ms);
    peer_changes(&mut ns, rebuild_ms);
    assert_indexes_match_rebuild(&ns);
}

/// Make one change, bounded by the node documents it may read and the time;
/// the index it leaves must equal a rebuild's.
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
        "WORK {label}-in-100k-tree: {lookups} lookups, {ms} ms (one whole rebuild: {rebuild_ms} ms)"
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
    ns.indexed_record(ns.record_id_for(ino).unwrap())
        .expect("a live node has a catalogue row")
        .path
}

/// The renames and moves this replica makes itself. A rename reads the
/// entries of the directories it joins or leaves (for the name conflict
/// check) and the moved subtree; the walk it replaced read 4-5 documents per
/// node.
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

    // A 1,000-entry directory moved into another: its whole subtree is
    // re-pathed and nothing else in the catalogue moves.
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

/// What a peer does to the tree. Inodes are this replica's own, so the peer's
/// work is found here by path.
fn peer_changes(ns: &mut Namespace, rebuild_ms: u128) {
    let one_directory = WIDE_FILES as u64 + 1;
    let quarter = rebuild_ms / 4;
    let mut peer = Namespace::from_bytes("b", &ns.save()).unwrap();
    peer.prime_baseline();
    let peer_dir = peer.lookup(ROOT_INO, "dir-003").unwrap().ino.0;

    // A peer creates a file: the row arrives with it.
    let here = ns.lookup(ROOT_INO, "dir-003").unwrap().ino.0;
    peer.create_file(peer_dir, "created", Some(&h(2)), 2, 4)
        .unwrap();
    let delta = peer.save_incremental();
    measure("create", ns, 64, quarter, rebuild_ms, |ns| {
        ns.apply_incremental(&delta).unwrap();
    });
    let created = ns.lookup(here, "created").unwrap().ino.0;
    assert_eq!(indexed_path(ns, created), "/dir-003/created");

    // A peer deletes (= trashes) a file: its row goes.
    let doomed = ns.lookup(here, "f-0002").unwrap().ino.0;
    let doomed_record = ns.record_id_for(doomed);
    let theirs = peer.lookup(peer_dir, "f-0002").unwrap().ino.0;
    assert!(peer.remove(theirs));
    measure("delete", ns, 64, quarter, rebuild_ms, |ns| {
        ns.merge_doc(&mut peer).unwrap();
    });
    assert!(ns.indexed_record(doomed_record.unwrap()).is_none());

    // A peer moves a 1,000-entry directory, arriving as its whole document:
    // reading that is the merge's own load, the catalogue work is the move.
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

    // Both sides rename one file at once: the delta arrives by sync, and the
    // row names the winner.
    let contested_dir = peer.lookup(ROOT_INO, "dir-007").unwrap().ino.0;
    assert!(peer.rename(contested_dir, "f-0003", contested_dir, "peer-name", 6));
    let ours = ns.lookup(ROOT_INO, "dir-007").unwrap().ino.0;
    let contested = ns.lookup(ours, "f-0003").unwrap().ino.0;
    assert!(ns.rename(ours, "f-0003", ours, "local-name", 7));
    let mut our_state = ns.begin_sync_with(&PEER);
    let mut their_state = peer.begin_sync_with(&DEVICE);
    measure(
        "sync-conflicting-rename",
        ns,
        64,
        quarter,
        rebuild_ms,
        |ns| {
            exchange(ns, &mut our_state, &mut peer, &mut their_state);
        },
    );
    measure(
        "post-sync-resolution",
        ns,
        64,
        quarter,
        rebuild_ms,
        Namespace::post_sync_resolve,
    );
    let winner = ns
        .name_of(ns.id_for_ino(contested).unwrap())
        .unwrap_or_default();
    assert_eq!(
        indexed_path(ns, contested),
        format!("/dir-007/{}", String::from_utf8_lossy(&winner))
    );
}

const PEER: [u8; 32] = [7; 32];
const DEVICE: [u8; 32] = [9; 32];

/// One sync exchange between `ns` and `peer` until both have nothing left to
/// send.
fn exchange(
    ns: &mut Namespace,
    our_state: &mut automerge::sync::State,
    peer: &mut Namespace,
    their_state: &mut automerge::sync::State,
) {
    for _ in 0..16 {
        let mut quiet = true;
        if let Some(message) = peer.sync_generate(their_state) {
            ns.sync_receive(our_state, &message).unwrap();
            quiet = false;
        }
        if let Some(message) = ns.sync_generate(our_state) {
            peer.sync_receive(their_state, &message).unwrap();
            quiet = false;
        }
        if quiet {
            break;
        }
    }
}
