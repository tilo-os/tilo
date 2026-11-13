#![allow(clippy::unwrap_used, reason = "a failed step is the assertion")]

use std::collections::BTreeSet;

use fs::{Namespace, ROOT_INO};
use iroh_blobs::Hash;

fn hash(byte: u8) -> Hash {
    Hash::from([byte; 32])
}

type StructuralProjection = Vec<(
    fs_protocol::FileRecordId,
    Option<(fs_protocol::FileRecordId, Vec<u8>)>,
)>;

fn structural_projection(
    namespace: &mut Namespace,
    records: &[fs_protocol::FileRecordId],
) -> StructuralProjection {
    let actor = fs_protocol::FileActor {
        person: collab_types::Ed25519PublicKey([0x91; 32]),
        device: collab_types::Ed25519PublicKey([0x92; 32]),
    };
    let trashed = namespace
        .directly_trashed()
        .into_iter()
        .filter_map(|id| namespace.record_id_for_id(&id))
        .collect::<BTreeSet<_>>();
    let mut pending = vec![ROOT_INO];
    let mut visited = BTreeSet::new();
    while let Some(parent) = pending.pop() {
        if !visited.insert(parent) {
            continue;
        }
        if let Some(entries) = namespace.readdir(parent) {
            pending.extend(entries.into_iter().map(|(ino, _, _)| ino));
        }
        assert!(
            visited.len() <= 1_024,
            "bounded structural fixture traversal"
        );
    }
    records
        .iter()
        .copied()
        .map(|record| {
            let live = namespace.ino_for_record_id(record).and_then(|ino| {
                namespace
                    .signed_binding_for(ino, actor, 1)
                    .and_then(|binding| Some((binding.parent?, binding.name.as_bytes().to_vec())))
            });
            assert_ne!(live.is_some(), trashed.contains(&record));
            (record, live)
        })
        .collect()
}

fn converge_structural_partition(
    left: &[u8],
    right: &[u8],
    records: &[fs_protocol::FileRecordId],
) -> (Namespace, Namespace) {
    let mut left_peer = Namespace::from_bytes("structural-left-restart", left).unwrap();
    let mut right_peer = Namespace::from_bytes("structural-right-restart", right).unwrap();
    let mut right_wire = Namespace::from_bytes("right-wire", right).unwrap();
    let mut left_wire = Namespace::from_bytes("left-wire", left).unwrap();
    left_peer.merge_doc(&mut right_wire).unwrap();
    right_peer.merge_doc(&mut left_wire).unwrap();
    left_peer.resolve();
    right_peer.resolve();

    let left_repair = left_peer.save();
    let right_repair = right_peer.save();
    let mut right_repair_wire = Namespace::from_bytes("right-repair-wire", &right_repair).unwrap();
    let mut left_repair_wire = Namespace::from_bytes("left-repair-wire", &left_repair).unwrap();
    left_peer.merge_doc(&mut right_repair_wire).unwrap();
    right_peer.merge_doc(&mut left_repair_wire).unwrap();
    left_peer.resolve();
    right_peer.resolve();
    assert_eq!(
        structural_projection(&mut left_peer, records),
        structural_projection(&mut right_peer, records)
    );

    let expected = structural_projection(&mut left_peer, records);
    let mut left_restarted =
        Namespace::from_bytes("left-final-restart", &left_peer.save()).unwrap();
    let mut right_restarted =
        Namespace::from_bytes("right-final-restart", &right_peer.save()).unwrap();
    let mut replay = Namespace::from_bytes("replay", &right_repair).unwrap();
    left_restarted.merge_doc(&mut replay).unwrap();
    left_restarted.resolve();
    assert_eq!(
        structural_projection(&mut left_restarted, records),
        expected
    );
    assert_eq!(
        structural_projection(&mut right_restarted, records),
        expected
    );
    (left_restarted, right_restarted)
}

#[test]
fn concurrent_structural_conflicts_preserve_sources_and_converge_after_restart() {
    let mut seed = Namespace::new("structural-seed-move").unwrap();
    let left_dir = seed.create_dir(ROOT_INO, "left", 1).unwrap();
    let _right_dir = seed.create_dir(ROOT_INO, "right", 1).unwrap();
    let source = seed
        .create_file(left_dir, "source.txt", Some(&hash(1)), 1, 1)
        .unwrap();
    let source_record = seed.record_id_for(source).unwrap();
    let baseline = seed.save();
    let mut left = Namespace::from_bytes("move-left", &baseline).unwrap();
    let mut right = Namespace::from_bytes("move-right", &baseline).unwrap();
    let left_parent = left.lookup(ROOT_INO, "left").unwrap().ino.0;
    let left_destination = left.lookup(ROOT_INO, "right").unwrap().ino.0;
    let right_parent = right.lookup(ROOT_INO, "left").unwrap().ino.0;
    assert!(left.rename(left_parent, "source.txt", left_destination, "moved.txt", 2));
    assert!(right.rename(right_parent, "source.txt", right_parent, "renamed.txt", 2));
    let (mut moved_left, mut moved_right) =
        converge_structural_partition(&left.save(), &right.save(), &[source_record]);
    assert!(moved_left.ino_for_record_id(source_record).is_some());
    assert_eq!(
        structural_projection(&mut moved_left, &[source_record]),
        structural_projection(&mut moved_right, &[source_record])
    );

    let mut seed = Namespace::new("structural-seed-delete").unwrap();
    let source = seed
        .create_file(ROOT_INO, "delete-me.txt", Some(&hash(2)), 1, 1)
        .unwrap();
    let source_record = seed.record_id_for(source).unwrap();
    let baseline = seed.save();
    let mut left = Namespace::from_bytes("delete-left", &baseline).unwrap();
    let mut right = Namespace::from_bytes("delete-right", &baseline).unwrap();
    let left_source = left.lookup(ROOT_INO, "delete-me.txt").unwrap().ino.0;
    assert!(left.remove(left_source));
    assert!(right.rename(ROOT_INO, "delete-me.txt", ROOT_INO, "renamed.txt", 2));
    let (mut deleted_left, mut deleted_right) =
        converge_structural_partition(&left.save(), &right.save(), &[source_record]);
    assert!(deleted_left.ino_for_record_id(source_record).is_none());
    let deleted_id = deleted_left
        .directly_trashed()
        .into_iter()
        .find(|id| deleted_left.record_id_for_id(id) == Some(source_record))
        .unwrap();
    assert_eq!(deleted_left.deleted_hash(&deleted_id), Some(hash(2)));
    assert_eq!(
        structural_projection(&mut deleted_left, &[source_record]),
        structural_projection(&mut deleted_right, &[source_record])
    );

    let mut seed = Namespace::new("structural-seed-replace").unwrap();
    let _archive = seed.create_dir(ROOT_INO, "archive", 1).unwrap();
    let source = seed
        .create_file(ROOT_INO, "source.txt", Some(&hash(3)), 1, 1)
        .unwrap();
    let victim = seed
        .create_file(ROOT_INO, "target.txt", Some(&hash(4)), 1, 1)
        .unwrap();
    let source_record = seed.record_id_for(source).unwrap();
    let victim_record = seed.record_id_for(victim).unwrap();
    let baseline = seed.save();
    let mut left = Namespace::from_bytes("replace-left", &baseline).unwrap();
    let mut right = Namespace::from_bytes("replace-right", &baseline).unwrap();
    let right_archive = right.lookup(ROOT_INO, "archive").unwrap().ino.0;
    assert!(left.rename(ROOT_INO, "source.txt", ROOT_INO, "target.txt", 2));
    assert!(right.rename(ROOT_INO, "target.txt", right_archive, "saved.txt", 2));
    let records = [source_record, victim_record];
    let (mut replaced_left, mut replaced_right) =
        converge_structural_partition(&left.save(), &right.save(), &records);
    let projection = structural_projection(&mut replaced_left, &records);
    assert_eq!(
        projection,
        structural_projection(&mut replaced_right, &records)
    );
    assert!(projection.iter().all(|(_, state)| {
        state.is_some()
            || replaced_left.directly_trashed().iter().any(|id| {
                replaced_left.record_id_for_id(id) == Some(source_record)
                    || replaced_left.record_id_for_id(id) == Some(victim_record)
            })
    }));
}

#[test]
fn forced_partition_offline_edits_restart_and_converge_as_a_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let mut seed = Namespace::new("seed").unwrap();
    seed.create_file(ROOT_INO, "offline.txt", Some(&hash(1)), 1, 1)
        .unwrap();
    let baseline = seed.save();
    let mut left = Namespace::from_bytes("left", &baseline).unwrap();
    let mut right = Namespace::from_bytes("right", &baseline).unwrap();
    let left_ino = left.lookup(ROOT_INO, "offline.txt").unwrap().ino.0;
    let right_ino = right.lookup(ROOT_INO, "offline.txt").unwrap().ino.0;
    assert!(left.set_content(left_ino, Some(&hash(10)), 10, 10));
    assert!(right.set_content(right_ino, Some(&hash(20)), 20, 20));

    let left_path = temp.path().join("left.automerge");
    let right_path = temp.path().join("right.automerge");
    tilo_files::native::atomic::write(&left_path, &left.save()).unwrap();
    tilo_files::native::atomic::write(&right_path, &right.save()).unwrap();
    drop(left);
    drop(right);

    let mut left =
        Namespace::from_bytes("left-restart", &std::fs::read(left_path).unwrap()).unwrap();
    let mut right =
        Namespace::from_bytes("right-restart", &std::fs::read(right_path).unwrap()).unwrap();
    assert_eq!(left.entry_at("offline.txt").unwrap().hash, Some(hash(10)));
    assert_eq!(right.entry_at("offline.txt").unwrap().hash, Some(hash(20)));

    let mut right_for_left = right.clone();
    let mut left_for_right = left.clone();
    left.merge_doc(&mut right_for_left).unwrap();
    right.merge_doc(&mut left_for_right).unwrap();
    for namespace in [&mut left, &mut right] {
        let conflicts = fs::conflict::detect(namespace);
        assert_eq!(conflicts.len(), 1);
        assert!(fs::conflict::materialize(namespace, &conflicts[0], 20, 21));
        assert!(fs::conflict::detect(namespace).is_empty());
    }

    let names = |namespace: &mut Namespace| {
        namespace
            .readdir(ROOT_INO)
            .unwrap()
            .into_iter()
            .map(|(_, _, name)| name.display_lossy())
            .collect::<BTreeSet<_>>()
    };
    let left_names = names(&mut left);
    let right_names = names(&mut right);
    assert_eq!(left_names, right_names);
    assert!(left_names.contains("offline.txt"));
    assert!(
        left_names
            .iter()
            .any(|name| name.contains(".sync-conflict-"))
    );
}
