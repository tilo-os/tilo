//! What the one walk over the document's nodes costs and answers.
//!
//! `Namespace::read_nodes_once` is the work every namespace open and every
//! received peer delta pay for the whole workspace: `rebuild_record_index`
//! reads each node's document once and takes its placement, name, kind and
//! record id from that object. `node_lookup_count` is the same unit
//! `crates/system/fs/tests/performance_gate.rs` bounds for a listing, so a
//! per-field re-read fails here rather than in a number nobody attributes.
//! A rename or move of this replica's own is not such a walk: it re-paths
//! the moved subtree only, which the same counter proves.

use automerge::ObjType;
use automerge::transaction::Transactable;
use fs_protocol::FileRecordId;
use iroh_blobs::Hash;

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

    // A peer's content edit arrives as a delta, merged and resolved under the
    // same walk: the record-id backfill and the rebuild read each node once,
    // and the cycle check walks its parent (two reads a hop). A rebuild that
    // read a node per field again would add two more per node and fail here.
    let mut peer = Namespace::from_bytes("b", &saved).unwrap();
    let peer_ino = peer.lookup(ROOT_INO, "f3").unwrap().ino.0;
    assert!(peer.set_content(peer_ino, Some(&h(2)), 7, 7));
    let delta = peer.save_incremental();
    let before = reopened.node_lookup_count();
    reopened.apply_remote_and_resolve(&delta).unwrap();
    let reads = reopened.node_lookup_count() - before;
    assert!(
        reads <= nodes.saturating_mul(5).saturating_add(8),
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

/// A rename or move re-paths the moved node and its descendants, not the
/// workspace: on 100,100 nodes it reads a slice of the catalogue, and the
/// rows it leaves behind equal a from-scratch rebuild.
#[test]
fn a_rename_repaths_only_the_moved_subtree() {
    let mut ns = wide_tree();
    let nodes = ns.doc.keys(&ns.nodes).count() as u64;
    assert_eq!(nodes, 100_100, "the fixture is the whole tree");
    assert_eq!(ns.indexed_record_count() as u64, nodes);

    // A file renamed in a 1,000-entry directory.
    let dir = ns.lookup(ROOT_INO, "dir-000").unwrap().ino.0;
    let before = ns.node_lookup_count();
    let start = std::time::Instant::now();
    assert!(ns.rename(dir, "f-0000", dir, "renamed", 2));
    let file_rename_ms = start.elapsed().as_millis();
    let file_rename = ns.node_lookup_count() - before;

    // A 1,000-entry directory moved into another one: its whole subtree.
    let target = ns.lookup(ROOT_INO, "dir-002").unwrap().ino.0;
    let before = ns.node_lookup_count();
    let start = std::time::Instant::now();
    assert!(ns.rename(ROOT_INO, "dir-001", target, "moved", 3));
    let dir_move_ms = start.elapsed().as_millis();
    let dir_move = ns.node_lookup_count() - before;

    eprintln!(
        "WORK rename-file-in-100k-tree: {file_rename} lookups, {file_rename_ms} ms \
         ({} nodes)",
        ns.indexed_record_count()
    );
    eprintln!("WORK rename-1000-entry-dir-in-100k-tree: {dir_move} lookups, {dir_move_ms} ms");

    // The rows are where the moves put them.
    let indexed_path = |ns: &Namespace, ino: u64| {
        ns.indexed_record(ns.record_id_for(ino).unwrap())
            .expect("a live node has a catalogue row")
            .path
    };
    let moved = ns.lookup(dir, "renamed").unwrap().ino.0;
    assert_eq!(indexed_path(&ns, moved), "/dir-000/renamed");
    let moved_dir = ns.lookup(target, "moved").unwrap().ino.0;
    assert_eq!(indexed_path(&ns, moved_dir), "/dir-002/moved");
    let nested = ns.lookup(moved_dir, "f-0001").unwrap().ino.0;
    assert_eq!(indexed_path(&ns, nested), "/dir-002/moved/f-0001");
    assert!(ns.lookup(ROOT_INO, "dir-001").is_none());

    // A rename reads what it touches, never the catalogue: the entries of
    // the directories it joins or leaves (their conflict check) and the moved
    // subtree. The whole-tree walk this replaced read 4-5 documents per node.
    let touched = WIDE_FILES as u64 + 1;
    assert!(
        file_rename <= 4 * touched + 64,
        "a file rename read {file_rename} node documents, more than the {touched} entries \
         of its directory"
    );
    assert!(
        dir_move <= 8 * touched + 64,
        "moving a 1,000-entry directory read {dir_move} node documents, more than its \
         {touched} entries and the directory it joined"
    );

    let mut rebuilt = ns.clone();
    let start = std::time::Instant::now();
    rebuilt.rebuild_record_index();
    eprintln!("WORK rebuild-100k-tree: {} ms", start.elapsed().as_millis());
    assert_eq!(ns.record_index, rebuilt.record_index, "rows");
    assert_eq!(ns.record_index_order, rebuilt.record_index_order, "order");
    assert_eq!(
        ns.record_index_digest, rebuilt.record_index_digest,
        "digest"
    );
    assert_eq!(
        ns.record_index_revision, rebuilt.record_index_revision,
        "revision"
    );
    assert_eq!(
        ns.record_index_ambiguous, rebuilt.record_index_ambiguous,
        "ambiguity"
    );
}
