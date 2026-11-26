//! The catalogue a change keeps is the one a rebuild gives, through the changes a person and
//! a peer make.

use fs_protocol::FileDocumentId;
use iroh_blobs::Hash;

use super::tests::assert_indexes_match_rebuild;
use super::*;

fn h(b: u8) -> Hash {
    Hash::from([b; 32])
}

fn tree() -> Namespace {
    let mut ns = Namespace::new("a").unwrap();
    for directory in 0..8 {
        let dir = ns
            .create_dir(ROOT_INO, format!("dir-{directory:03}"), 1)
            .unwrap();
        for file in 0..6 {
            ns.create_file(dir, format!("f-{file:04}"), Some(&h(1)), 1, file)
                .unwrap();
        }
    }
    ns
}

fn indexed_path(ns: &Namespace, ino: u64) -> String {
    let record = ns
        .indexed_document(ns.document_id_for(ino).unwrap())
        .expect("a live node has a catalogue row");
    ns.path_for_document(&record)
        .expect("a live node sits at a path")
}

#[test]
fn a_node_under_the_reserved_root_id_leaves_the_root_record_its_own() {
    let mut ns = tree();
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
    assert_eq!(reserved.indexed_document_count(), rows);
    assert_eq!(reserved.indexed_document_revision(), revision);
    assert_eq!(reserved.document_id_for(ROOT_INO), Some(root_document));
    assert!(!reserved.document_index_ambiguous.contains(&root_document));
    assert!(reserved.indexed_document(own_resource).is_none());
}

#[test]
fn the_catalogue_a_change_kept_is_the_one_a_rebuild_gives() {
    let mut ns = tree();
    let dir = ns.lookup(ROOT_INO, "dir-000").unwrap().ino.0;
    assert!(ns.rename(dir, "f-0000", dir, "renamed", 2));
    assert_indexes_match_rebuild(&ns);
    let renamed = ns.lookup(dir, "renamed").unwrap().ino.0;
    assert_eq!(indexed_path(&ns, renamed), "/dir-000/renamed");

    let target = ns.lookup(ROOT_INO, "dir-002").unwrap().ino.0;
    assert!(ns.rename(ROOT_INO, "dir-001", target, "moved", 3));
    assert_indexes_match_rebuild(&ns);
    let moved_dir = ns.lookup(target, "moved").unwrap().ino.0;
    let nested = ns.lookup(moved_dir, "f-0001").unwrap().ino.0;
    assert_eq!(indexed_path(&ns, nested), "/dir-002/moved/f-0001");
    assert!(ns.lookup(ROOT_INO, "dir-001").is_none());

    let mut peer = Namespace::from_bytes("b", &ns.save()).unwrap();
    peer.prime_baseline();
    let peer_dir = peer.lookup(ROOT_INO, "dir-003").unwrap().ino.0;
    let here = ns.lookup(ROOT_INO, "dir-003").unwrap().ino.0;
    peer.create_file(peer_dir, "created", Some(&h(2)), 2, 4)
        .unwrap();
    ns.apply_incremental(&peer.save_incremental()).unwrap();
    assert_indexes_match_rebuild(&ns);
    let created = ns.lookup(here, "created").unwrap().ino.0;
    assert_eq!(indexed_path(&ns, created), "/dir-003/created");

    let doomed = ns.lookup(here, "f-0002").unwrap().ino.0;
    let doomed_document = ns.document_id_for(doomed).unwrap();
    let theirs = peer.lookup(peer_dir, "f-0002").unwrap().ino.0;
    assert!(peer.remove(theirs));
    ns.merge_doc(&mut peer).unwrap();
    assert_indexes_match_rebuild(&ns);
    assert!(ns.indexed_document(doomed_document).is_none());

    let harvest = peer.lookup(ROOT_INO, "dir-006").unwrap().ino.0;
    assert!(peer.rename(ROOT_INO, "dir-005", harvest, "harvest", 5));
    let harvest_here = ns.lookup(ROOT_INO, "dir-006").unwrap().ino.0;
    ns.merge_remote_and_resolve(&peer.save()).unwrap();
    assert_indexes_match_rebuild(&ns);
    let harvest_dir = ns.lookup(harvest_here, "harvest").unwrap().ino.0;
    let harvest_file = ns.lookup(harvest_dir, "f-0005").unwrap().ino.0;
    assert_eq!(indexed_path(&ns, harvest_file), "/dir-006/harvest/f-0005");

    let contested_dir = peer.lookup(ROOT_INO, "dir-007").unwrap().ino.0;
    assert!(peer.rename(contested_dir, "f-0003", contested_dir, "peer-name", 6));
    let ours = ns.lookup(ROOT_INO, "dir-007").unwrap().ino.0;
    let contested = ns.lookup(ours, "f-0003").unwrap().ino.0;
    assert!(ns.rename(ours, "f-0003", ours, "local-name", 7));
    ns.apply_remote_and_resolve(&peer.save_incremental())
        .unwrap();
    assert_indexes_match_rebuild(&ns);
    let winner = ns
        .name_of(ns.id_for_ino(contested).unwrap())
        .unwrap_or_default();
    assert_eq!(
        indexed_path(&ns, contested),
        format!("/dir-007/{}", String::from_utf8_lossy(&winner))
    );
}

#[test]
fn a_folders_subfolders_are_the_directories_its_listing_shows() {
    let mut ns = tree();
    let dir = ns.lookup(ROOT_INO, "dir-000").unwrap().ino.0;
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
    assert_eq!(
        ns.subfolder_names(ROOT_INO).unwrap(),
        listed(&mut ns, ROOT_INO)
    );
    assert_eq!(ns.subfolder_names(dir).unwrap(), listed(&mut ns, dir));
    assert_eq!(ns.subfolder_names(dir).unwrap().len(), 2);
    let file = ns.lookup(dir, "f-0001").unwrap().ino.0;
    assert_eq!(ns.subfolder_names(file), None);
}
