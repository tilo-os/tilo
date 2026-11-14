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

tilo_test_env::scenario! {
    fn the_record_index_is_what_a_rebuild_of_the_document_gives;

    phase rebuilding_the_index_reads_each_node_document_once;
    {
        let saved = saved_tree(64);
        let mut reopened = Namespace::from_bytes("a", &saved).unwrap();
        let nodes = reopened.doc.keys(&reopened.nodes).count() as u64;
        assert_eq!(nodes, 65, "the fixture holds the directory and its files");
        assert_eq!(reopened.indexed_record_count() as u64, nodes);

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

    phase a_rebuild_reads_and_indexes_exactly_the_nodes_of_the_smallest_trees;
    // The walk at the sizes where a cost that is not per node would show: no node, one node, and a
    // directory with its first file.
    {
        let lone_file = {
            let mut source = Namespace::new("a").unwrap();
            source
                .create_file(ROOT_INO, "f", Some(&h(1)), 1, 1)
                .unwrap();
            source.save()
        };
        let rows = [
            ("no node", Namespace::new("a").unwrap().save(), vec![]),
            ("one file", lone_file, vec!["/f"]),
            (
                "a directory and its first file",
                saved_tree(1),
                vec!["/dir", "/dir/f0"],
            ),
        ];
        for (row, saved, paths) in rows {
            let mut reopened = Namespace::from_bytes("a", &saved).unwrap();
            let nodes = reopened.doc.keys(&reopened.nodes).count();
            assert_eq!(nodes, paths.len(), "{row}: nodes in the document");
            let before = reopened.node_lookup_count();
            reopened.rebuild_record_index();
            let reads = reopened.node_lookup_count() - before;
            assert_eq!(reads, nodes as u64, "{row}: node documents read");
            let indexed: std::collections::BTreeSet<&str> = reopened
                .record_index
                .values()
                .map(|record| record.path.as_str())
                .collect();
            assert_eq!(
                indexed.into_iter().collect::<Vec<_>>(),
                paths,
                "{row}: catalogue paths"
            );
            assert_eq!(
                reopened.indexed_record_count(),
                nodes,
                "{row}: catalogue rows"
            );
        }
    }

    phase the_catalogue_follows_each_small_change_as_a_rebuild_would;
    // The changes the 100,100-node test does not make, on a tree small enough to read. After each
    // the indexes are what a rebuild of the document gives and the catalogue holds exactly the
    // row's paths.
    {
        let rows: [SmallChange; 6] = [
            (
                "a directory removed here",
                a_directory_removed_here,
                &["/f1", "/f3"],
            ),
            (
                "moves that are refused",
                moves_that_are_refused,
                &["/dir", "/dir/f0", "/dir/f2", "/dir/inner", "/f1", "/f3"],
            ),
            (
                "a file that comes and goes",
                a_file_that_comes_and_goes,
                &["/dir", "/dir/f0", "/dir/f2", "/f1", "/f3"],
            ),
            (
                "a directory removed by the peer",
                a_directory_removed_by_the_peer,
                &["/f1", "/f3"],
            ),
            (
                "a peer's file in a directory removed here",
                a_peer_file_in_a_directory_removed_here,
                &["/f1", "/f3"],
            ),
            (
                "a peer's move delivered twice",
                a_peer_move_delivered_twice,
                &["/dir", "/dir/f0", "/dir/f2", "/dir/moved", "/f3"],
            ),
        ];
        for (row, change, paths) in rows {
            // The whole-index comparison names the field that differs; this names the row.
            eprintln!("row: {row}");
            let mut replicas = small_replicas();
            assert_eq!(
                catalogue_paths(&replicas.here),
                ["/dir", "/dir/f0", "/dir/f2", "/f1", "/f3"],
                "{row}: the tree before the change"
            );
            change(&mut replicas);
            assert_indexes_match_rebuild(&replicas.here);
            assert_eq!(
                catalogue_paths(&replicas.here),
                paths,
                "{row}: catalogue paths"
            );
            assert_eq!(
                replicas.here.indexed_record_count(),
                paths.len(),
                "{row}: catalogue rows"
            );
        }
    }

    phase a_broken_move_cycle_repaths_its_subtree_in_both_catalogues;
    // Two directories, each moved into the other on another replica. The repair puts the one with
    // the lower node id back at the root; the catalogue re-paths everything below it, and both
    // replicas end with the same rows.
    {
        let Replicas { mut here, mut peer } = small_replicas();
        let other = here.create_dir(ROOT_INO, "other", 2).unwrap();
        let made = here.save_incremental();
        peer.apply_remote_and_resolve(&made).unwrap();

        let dir = dir_of(&mut here);
        assert!(here.rename(ROOT_INO, "dir", other, "dir", 3));
        let their_dir = dir_of(&mut peer);
        assert!(peer.rename(ROOT_INO, "other", their_dir, "other", 3));
        let from_here = here.save_incremental();
        let from_peer = peer.save_incremental();
        here.apply_remote_and_resolve(&from_peer).unwrap();
        peer.apply_remote_and_resolve(&from_here).unwrap();

        let dir_stays = here.id_for_ino(dir).unwrap() < here.id_for_ino(other).unwrap();
        let paths: [&str; 6] = if dir_stays {
            ["/dir", "/dir/f0", "/dir/f2", "/dir/other", "/f1", "/f3"]
        } else {
            [
                "/f1",
                "/f3",
                "/other",
                "/other/dir",
                "/other/dir/f0",
                "/other/dir/f2",
            ]
        };
        for (replica, ns) in [("here", &here), ("the peer", &peer)] {
            eprintln!("replica: {replica}");
            assert_indexes_match_rebuild(ns);
            assert_eq!(catalogue_paths(ns), paths, "{replica}: catalogue paths");
            assert_eq!(
                ns.indexed_record_count(),
                paths.len(),
                "{replica}: catalogue rows"
            );
        }
        assert_eq!(
            here.indexed_record_revision(),
            peer.indexed_record_revision(),
            "the replicas hold the same rows under different revisions"
        );
    }

    phase a_node_under_the_reserved_root_id_keeps_the_root_record_its_own;
    {
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
        assert!(ns.tree.nodes_of(own_record).any(|id| id == ROOT_ID));
        assert!(!ns.tree.nodes_of(root_record).any(|id| id == ROOT_ID));
        assert!(ns.indexed_record(own_record).is_none());
    }
}

/// One replica and a peer of it, each opened from the same saved tree.
struct Replicas {
    here: Namespace,
    peer: Namespace,
}

/// `saved_tree(4)` twice: a directory holding `f0` and `f2`, beside `f1` and `f3`.
fn small_replicas() -> Replicas {
    let saved = saved_tree(4);
    let mut peer = Namespace::from_bytes("b", &saved).unwrap();
    peer.prime_baseline();
    Replicas {
        here: Namespace::from_bytes("a", &saved).unwrap(),
        peer,
    }
}

fn catalogue_paths(ns: &Namespace) -> Vec<String> {
    let mut paths: Vec<String> = ns
        .record_index
        .values()
        .map(|record| record.path.clone())
        .collect();
    paths.sort();
    paths
}

fn dir_of(ns: &mut Namespace) -> u64 {
    ns.lookup(ROOT_INO, "dir").unwrap().ino.0
}

fn a_directory_removed_here(replicas: &mut Replicas) {
    let dir = dir_of(&mut replicas.here);
    assert!(replicas.here.remove(dir));
}

fn moves_that_are_refused(replicas: &mut Replicas) {
    let here = &mut replicas.here;
    let dir = dir_of(here);
    let inner = here.create_dir(dir, "inner", 2).unwrap();
    let revision = here.indexed_record_revision().to_owned();
    assert!(
        !here.rename(ROOT_INO, "dir", inner, "dir", 3),
        "a directory moved under its own child"
    );
    assert!(
        !here.rename(ROOT_INO, "dir", dir, "dir", 3),
        "a directory moved into itself"
    );
    assert!(
        !here.rename(ROOT_INO, "absent", dir, "found", 3),
        "a name nothing has was moved"
    );
    assert_eq!(
        here.indexed_record_revision(),
        revision,
        "a refused move moved the catalogue"
    );
}

fn a_file_that_comes_and_goes(replicas: &mut Replicas) {
    let here = &mut replicas.here;
    let revision = here.indexed_record_revision().to_owned();
    let dir = dir_of(here);
    let brief = here.create_file(dir, "brief", Some(&h(2)), 1, 2).unwrap();
    assert_ne!(
        here.indexed_record_revision(),
        revision,
        "a new row left the revision where it was"
    );
    assert!(here.remove(brief));
    assert_eq!(
        here.indexed_record_revision(),
        revision,
        "the same rows have another revision"
    );
}

fn a_directory_removed_by_the_peer(replicas: &mut Replicas) {
    let dir = dir_of(&mut replicas.peer);
    assert!(replicas.peer.remove(dir));
    let delta = replicas.peer.save_incremental();
    replicas.here.apply_incremental(&delta).unwrap();
}

fn a_peer_file_in_a_directory_removed_here(replicas: &mut Replicas) {
    let dir = dir_of(&mut replicas.here);
    assert!(replicas.here.remove(dir));
    let theirs = dir_of(&mut replicas.peer);
    replicas
        .peer
        .create_file(theirs, "late", Some(&h(2)), 1, 9)
        .unwrap();
    let delta = replicas.peer.save_incremental();
    replicas.here.apply_remote_and_resolve(&delta).unwrap();
}

fn a_peer_move_delivered_twice(replicas: &mut Replicas) {
    let dir = dir_of(&mut replicas.peer);
    assert!(replicas.peer.rename(ROOT_INO, "f1", dir, "moved", 9));
    let delta = replicas.peer.save_incremental();
    replicas.here.apply_remote_and_resolve(&delta).unwrap();
    let revision = replicas.here.indexed_record_revision().to_owned();
    replicas.here.apply_remote_and_resolve(&delta).unwrap();
    assert_eq!(
        replicas.here.indexed_record_revision(),
        revision,
        "a delta delivered again moved the catalogue"
    );
}

/// A row: its name, the change, and the catalogue's paths after it.
type SmallChange = (&'static str, fn(&mut Replicas), &'static [&'static str]);

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

#[test]
fn a_change_costs_the_change_not_the_tree() {
    let mut ns = wide_tree();
    let nodes = ns.doc.keys(&ns.nodes).count() as u64;
    assert_eq!(nodes, 100_100, "the fixture is the whole tree");
    assert_eq!(ns.indexed_record_count() as u64, nodes);

    let mut rebuilt = ns.clone();
    let start = std::time::Instant::now();
    rebuilt.rebuild_record_index();
    let rebuild_ms = start.elapsed().as_millis();
    eprintln!("WORK rebuild-100k-tree: {rebuild_ms} ms");

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
    let doomed_record = ns.record_id_for(doomed);
    let theirs = peer.lookup(peer_dir, "f-0002").unwrap().ino.0;
    assert!(peer.remove(theirs));
    measure("delete", ns, 64, quarter, rebuild_ms, |ns| {
        ns.merge_doc(&mut peer).unwrap();
    });
    assert!(ns.indexed_record(doomed_record.unwrap()).is_none());

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
