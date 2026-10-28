//! What the one walk over the document's nodes costs and answers.
//!
//! `Namespace::read_nodes_once` is the work every namespace open and every
//! received peer delta pay for the whole workspace: `rebuild_record_index`
//! reads each node's document once and takes its placement, name, kind and
//! record id from that object. `node_lookup_count` is the same unit
//! `crates/system/fs/tests/performance_gate.rs` bounds for a listing, so a
//! per-field re-read fails here rather than in a number nobody attributes.

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
