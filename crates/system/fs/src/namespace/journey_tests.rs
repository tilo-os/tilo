//! A workspace's tree through the interface two devices and the control actions use: documents
//! keep their identity, changes travel in chunks, a device that founded its tree before reaching
//! the workspace merges, and what a person or agent did is named in the record's history.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard, and a journey is its steps"
)]

use std::collections::BTreeMap;
use std::time::{Duration, UNIX_EPOCH};

use iroh_blobs::Hash;

use super::*;

fn h(byte: u8) -> Hash {
    Hash::from([byte; 32])
}

/// Every write the namespace has for `into` from what `from` wrote, as records.
fn take(into: &mut Namespace, from: &mut Namespace) {
    let writes = from.document_writes(&BTreeMap::new());
    into.take_documents(
        writes
            .into_iter()
            .map(|write| (write.chunk, write.changes))
            .collect(),
    )
    .unwrap();
}

/// The entries of a record's changes, oldest first: the file action verbs and who they name.
fn entries_of(document: &[u8]) -> Vec<(Vec<String>, Option<String>)> {
    let mut doc = automerge::AutoCommit::load(document).unwrap();
    doc.get_changes(&[])
        .into_iter()
        .filter_map(|change| tilo_actions::ActionContext::from_change_message(change.message()?))
        .filter(|context| {
            context
                .resource
                .as_ref()
                .map(|resource| resource.kind.as_str())
                == Some(fs_protocol::tree::TREE_KIND)
        })
        .map(|context| {
            let verbs = match &context.what {
                Some(tilo_actions::Did::Verb(verb)) => vec![verb.action.action.clone()],
                Some(tilo_actions::Did::Group(group)) => group
                    .distinct
                    .iter()
                    .filter_map(|did| match did {
                        tilo_actions::Did::Verb(verb) => Some(verb.action.action.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let who = context
                .invoker
                .as_ref()
                .and_then(|invoker| invoker.name.clone());
            (verbs, who)
        })
        .collect()
}

#[test]
fn a_documents_identity_is_one_through_rename_persist_and_conflict_copies() {
    let mut original = Namespace::new("a").unwrap();
    let file = original
        .create_file(ROOT_INO, "stable.txt", Some(&h(1)), 1, 1)
        .unwrap();
    let root_document = original.document_id_for(ROOT_INO).unwrap();
    let file_document = original.document_id_for(file).unwrap();
    assert_ne!(
        root_document, file_document,
        "the root is not a file's record"
    );

    let mut reloaded = Namespace::from_bytes("a", &original.save()).unwrap();
    let reloaded_file = reloaded.lookup(ROOT_INO, "stable.txt").unwrap().ino.0;
    assert_eq!(reloaded.document_id_for(ROOT_INO), Some(root_document));
    assert_eq!(reloaded.document_id_for(reloaded_file), Some(file_document));
    assert_eq!(
        reloaded.entry_at("stable.txt").unwrap().document_id,
        file_document,
        "the entry names the same record the inode does"
    );
    assert!(reloaded.rename(ROOT_INO, "stable.txt", ROOT_INO, "renamed.txt", 2));
    let renamed_ino = reloaded.lookup(ROOT_INO, "renamed.txt").unwrap().ino.0;
    assert_eq!(
        reloaded.document_id_for(renamed_ino),
        Some(file_document),
        "a rename keeps the record"
    );

    let mut kept = Namespace::new("kept").unwrap();
    let kept_file = kept
        .create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1)
        .unwrap();
    let kept_document = kept.document_id_for(kept_file).unwrap();
    assert!(kept.create_conflict_copy(ROOT_ID, "doc.sync-conflict", &h(2), 1, 2));
    let copy = kept.resolve_path_ino("doc.sync-conflict").unwrap();
    assert_ne!(kept.document_id_for(copy), Some(kept_document));
    assert!(
        kept.document_id_for(copy).is_some(),
        "a copy is a record of its own"
    );

    // A durable record identity written on two nodes is ambiguous: the tree withholds both
    // rather than routing reads to an arbitrary node.
    let mut ambiguous = Namespace::new("owner").unwrap();
    let first = ambiguous.create_file(ROOT_INO, "a", None, 0, 1).unwrap();
    let duplicate = ambiguous.create_file(ROOT_INO, "b", None, 0, 1).unwrap();
    let document_id = ambiguous.document_id_for(first).unwrap();
    let duplicate_id = ambiguous.id_for_ino(duplicate).unwrap().to_string();
    let duplicate_obj = ambiguous.node(&duplicate_id).unwrap();
    ambiguous
        .doc
        .put(&duplicate_obj, "document_id", document_id.0.to_string())
        .unwrap();
    ambiguous.rebuild_document_index();
    assert!(
        ambiguous.indexed_document(document_id).is_none(),
        "an ambiguous durable record identity must be withheld"
    );
    assert_eq!(ambiguous.hash_for(first), None);
    assert_eq!(ambiguous.hash_for(duplicate), None);

    // Nodes placed under each other in a cycle are unreachable and are withheld too.
    let mut cyclic = Namespace::new("owner").unwrap();
    let a = cyclic.create_dir(ROOT_INO, "a", 1).unwrap();
    let b = cyclic.create_dir(ROOT_INO, "b", 1).unwrap();
    let a_id = cyclic.id_for_ino(a).unwrap().to_string();
    let b_id = cyclic.id_for_ino(b).unwrap().to_string();
    cyclic.set_field(&a_id, "parent", b_id.clone());
    cyclic.set_field(&b_id, "parent", a_id);
    cyclic.rebuild_document_index();
    assert_eq!(
        cyclic.indexed_document_count(),
        0,
        "unreachable cyclic nodes must be withheld rather than making a rebuild non-terminating"
    );
}

#[test]
fn a_projected_file_is_one_record_one_name_and_stays_in_the_workspace() {
    let mut ns = Namespace::new("recipient").unwrap();
    let record = FileDocumentId::new();
    let ino = ns
        .create_projected_file(ROOT_INO, "shared.png", record, Some(&h(8)), 42, 9)
        .unwrap();
    assert_eq!(ns.document_id_for(ino), Some(record));
    assert_eq!(ns.hash_for(ino), Some(h(8)));
    assert_eq!(ns.getattr(ino).unwrap().size, 42);
    assert!(
        ns.create_projected_file(ROOT_INO, "alias.png", record, None, 0, 0)
            .is_none(),
        "one record is one name"
    );
    assert!(
        ns.create_projected_file(ROOT_INO, "shared.png", FileDocumentId::new(), None, 0, 0)
            .is_none(),
        "one name is one record"
    );
    assert!(
        ns.create_projected_file(ROOT_INO, "../escape", FileDocumentId::new(), None, 0, 0)
            .is_none(),
        "a name never leaves the workspace"
    );
}

#[test]
fn signed_bindings_apply_once_and_never_rename_an_unsigned_node() {
    let actor = FileActor {
        person: collab_types::Ed25519PublicKey([3; 32]),
        device: collab_types::Ed25519PublicKey([4; 32]),
    };
    let mut ns = Namespace::new("a").unwrap();
    let left = ns.create_dir(ROOT_INO, "left", 1).unwrap();
    let right = ns.create_dir(ROOT_INO, "right", 1).unwrap();
    let file = ns
        .create_file(left, "before.txt", Some(&h(1)), 1, 1)
        .unwrap();
    let occupied = ns
        .create_file(right, "occupied.txt", Some(&h(2)), 1, 1)
        .unwrap();
    let binding = FileBindingV1 {
        schema_version: fs_protocol::FILE_BINDING_VERSION,
        document_id: ns.document_id_for(file).unwrap(),
        parent: Some(ns.document_id_for(right).unwrap()),
        name: "after.txt".into(),
        kind: FileNodeKind::File,
        tombstoned: false,
        actor,
        observed_at_ms: 2,
    };
    assert_eq!(ns.apply_signed_binding(&binding), Ok(true));
    assert_eq!(ns.lookup(right, "after.txt").unwrap().ino.0, file);
    assert_eq!(ns.apply_signed_binding(&binding), Ok(false), "applied once");

    // A name already taken keeps both: the loser gets a conflict sibling beside the winner.
    let occupied_binding = FileBindingV1 {
        name: "occupied.txt".into(),
        ..binding.clone()
    };
    let file_document = ns.document_id_for(file).unwrap();
    let occupied_document = ns.document_id_for(occupied).unwrap();
    assert_eq!(ns.apply_signed_binding(&occupied_binding), Ok(true));
    let (winner, loser_document) = if file_document < occupied_document {
        (file, occupied_document)
    } else {
        (occupied, file_document)
    };
    assert_eq!(ns.lookup(right, "occupied.txt").unwrap().ino.0, winner);
    let conflict = ns
        .readdir(right)
        .unwrap()
        .into_iter()
        .map(|(ino, _, name)| (ino, name))
        .find(|(_, name)| name.starts_with(b"occupied") && name.as_bytes() != b"occupied.txt")
        .expect("the loser keeps a conflict sibling");
    assert_eq!(
        ns.document_id_for(conflict.0).map(|record| record.0),
        Some(loser_document.0),
        "the conflict sibling is the loser's record"
    );
    assert_eq!(ns.apply_signed_binding(&occupied_binding), Ok(false));

    let deleted = FileBindingV1 {
        tombstoned: true,
        ..binding
    };
    assert_eq!(ns.apply_signed_binding(&deleted), Ok(true));
    assert_eq!(ns.apply_signed_binding(&deleted), Ok(false));
    assert!(ns.lookup(right, "after.txt").is_none());

    // A device that rebuilt its tree takes the root's binding and holds the identity the
    // originator gave it; a signed create keeps that identity.
    let origin = Namespace::new("origin").unwrap();
    let root_document = origin.document_id_for(ROOT_INO).unwrap();
    let root_binding = origin.signed_binding_for(ROOT_INO, actor, 1).unwrap();
    let mut rebuilt = Namespace::new("rebuilt").unwrap();
    assert_ne!(rebuilt.document_id_for(ROOT_INO), Some(root_document));
    assert_eq!(rebuilt.apply_signed_binding(&root_binding), Ok(true));
    assert_eq!(rebuilt.document_id_for(ROOT_INO), Some(root_document));
    let record = FileDocumentId::new();
    let created = FileBindingV1 {
        schema_version: fs_protocol::FILE_BINDING_VERSION,
        document_id: record,
        parent: Some(root_document),
        name: b"created.txt".to_vec().into(),
        kind: FileNodeKind::File,
        tombstoned: false,
        actor,
        observed_at_ms: 2,
    };
    assert_eq!(rebuilt.apply_signed_binding(&created), Ok(true));
    assert_eq!(
        rebuilt
            .ensure_ino_for_document_id(record)
            .and_then(|ino| rebuilt.id_for_ino(ino).map(ToString::to_string)),
        Some(format!("record:{}", record.0))
    );

    // A node this device made without a signature keeps its name; the signed record with the
    // same name lands beside it as a conflict, not by renaming the unsigned node.
    let unsigned = rebuilt
        .create_file(ROOT_INO, "contested.txt", Some(&h(7)), 1, 3)
        .unwrap();
    let unsigned_id = rebuilt.id_for_ino(unsigned).unwrap().to_string();
    let unsigned_obj = rebuilt.node(&unsigned_id).unwrap();
    rebuilt.doc.delete(&unsigned_obj, "document_id").unwrap();
    rebuilt.rebuild_document_index();
    assert!(rebuilt.document_id_for(unsigned).is_none());
    let signed_document = FileDocumentId::new();
    let contested = FileBindingV1 {
        schema_version: fs_protocol::FILE_BINDING_VERSION,
        document_id: signed_document,
        parent: Some(root_document),
        name: b"contested.txt".to_vec().into(),
        kind: FileNodeKind::File,
        tombstoned: false,
        actor,
        observed_at_ms: 4,
    };
    assert_eq!(rebuilt.apply_signed_binding(&contested), Ok(true));
    let entries = rebuilt
        .readdir(ROOT_INO)
        .unwrap()
        .into_iter()
        .filter(|(_, _, name)| name != "." && name != "..")
        .collect::<Vec<_>>();
    assert_eq!(
        entries
            .iter()
            .find(|(_, _, name)| name.as_bytes() == b"contested.txt")
            .map(|(ino, _, _)| *ino),
        Some(unsigned),
        "the unsigned node keeps its name"
    );
    let signed_ino = rebuilt.ensure_ino_for_document_id(signed_document).unwrap();
    assert!(
        entries.iter().any(|(ino, _, name)| *ino == signed_ino
            && name.starts_with(b"contested")
            && name.as_bytes() != b"contested.txt"),
        "the signed record keeps a conflict sibling"
    );
}

#[test]
fn trees_founded_before_reaching_the_workspace_merge_with_one_identity() {
    use collab_types::document::DocumentId;

    let names = |ns: &mut Namespace| -> Vec<String> {
        let mut names: Vec<String> = ns
            .live_node_ids()
            .iter()
            .filter_map(|id| ns.entry_for_node(id))
            .map(|entry| String::from_utf8_lossy(entry.name.as_bytes()).into_owned())
            .collect();
        names.sort();
        names
    };
    let workspace = "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11";
    let mount = |device: &str| MountAuthor::device(device).in_workspace(workspace);

    // Two devices wrote into the workspace's folder before either reached it: each founded the
    // tree's documents, with the one identity every founder derives.
    let mut laptop = Namespace::new(mount("laptop")).unwrap();
    laptop
        .create_file(ROOT_INO, "contract.txt", Some(&h(1)), 6, 1)
        .unwrap();
    let mut desktop = Namespace::new(mount("desktop")).unwrap();
    desktop
        .create_file(ROOT_INO, "handbook.md", Some(&h(2)), 6, 1)
        .unwrap();
    let from_laptop = laptop.document_writes(&BTreeMap::new());
    let from_desktop = desktop.document_writes(&BTreeMap::new());
    for (into, from) in [(&mut laptop, from_desktop), (&mut desktop, from_laptop)] {
        into.take_documents(from.into_iter().map(|w| (w.chunk, w.changes)).collect())
            .unwrap();
    }
    for ns in [&mut laptop, &mut desktop] {
        let index = ns.save_document(0).unwrap();
        let doc = automerge::AutoCommit::load(&index).unwrap();
        assert_eq!(
            collab_types::document::identity::identity(&doc).map(|found| found.id),
            Some(DocumentId::parse(&fs_protocol::tree::document_id()).unwrap()),
            "the tree is one document with one identity, not two founders' metadata"
        );
        assert_eq!(names(ns), ["contract.txt", "handbook.md"]);
    }
    let held = laptop.save_document(1).expect("the laptop holds its chunk");
    let doc = automerge::AutoCommit::load(&held).unwrap();
    assert_eq!(
        collab_types::document::identity::identity(&doc).map(|found| found.id),
        Some(DocumentId::parse(&fs_protocol::tree::chunk_document_id(1)).unwrap()),
    );

    // A page that put the first file into the workspace's empty record before any computer
    // reached it: the page's placing makes the map.
    let mut empty = Namespace::new(mount("laptop")).unwrap();
    let mut record = automerge::AutoCommit::new();
    let mut tree = fs_protocol::tree::Tree::of_document(&record.save()).unwrap();
    fs_protocol::tree::place(
        (&mut record, 1),
        &mut tree,
        fs_protocol::tree::ROOT_ID,
        "notice.pdf",
        fs_protocol::tree::Placing::File {
            hash: &h(3).to_hex(),
            size: 3,
        },
        (1, "page"),
    )
    .unwrap();
    let _ = record.commit();
    empty.take_documents(vec![(1, record.save())]).unwrap();
    assert_eq!(names(&mut empty), ["notice.pdf"]);

    // A tree an earlier build wrote named nothing; two computers take it in and each names it
    // as it next writes, before either hears of the other's name.
    let mut earlier = Namespace::new(MountAuthor::device("laptop")).unwrap();
    earlier
        .create_file(ROOT_INO, "contract.txt", Some(&h(1)), 6, 1)
        .unwrap();
    let written: Vec<(fs_protocol::TreeChunk, Vec<u8>)> = earlier
        .document_writes(&BTreeMap::new())
        .into_iter()
        .map(|w| (w.chunk, w.changes))
        .collect();
    assert_eq!(written.len(), 2, "the tree's own document and its chunk");
    let mut laptop = Namespace::from_records(mount("laptop"), written.clone()).unwrap();
    let mut desktop = Namespace::from_records(mount("desktop"), written).unwrap();
    let from_laptop = laptop.document_writes(&BTreeMap::new());
    let from_desktop = desktop.document_writes(&BTreeMap::new());
    for (into, from) in [(&mut laptop, from_desktop), (&mut desktop, from_laptop)] {
        into.take_documents(from.into_iter().map(|w| (w.chunk, w.changes)).collect())
            .unwrap();
    }
    for ns in [&mut laptop, &mut desktop] {
        for chunk in [0, 1] {
            let doc = automerge::AutoCommit::load(&ns.save_document(chunk).unwrap()).unwrap();
            assert_eq!(
                collab_types::document::identity::identity(&doc).map(|found| found.id),
                Some(DocumentId::parse(&fs_protocol::tree::chunk_document_id(chunk)).unwrap()),
                "document {chunk} names itself once"
            );
            assert_eq!(
                collab_types::document::identity::workspace_scope(&doc).as_deref(),
                Some(workspace)
            );
        }
    }
}

#[test]
fn changes_travel_as_chunks_and_only_what_a_holder_lacks() {
    let files = fs_protocol::tree::CHUNK_NODES * 2 + 10;
    let mut laptop = Namespace::new("laptop").unwrap();
    let mut first = None;
    for n in 0..files {
        let ino = laptop
            .create_file(ROOT_INO, format!("f{n}.txt"), Some(&h(1)), 1, 1)
            .unwrap();
        first.get_or_insert(ino);
    }
    assert_eq!(
        laptop.records(),
        [0, 1, 2, 3],
        "the files filled three chunks"
    );

    let writes = laptop.document_writes(&BTreeMap::new());
    assert_eq!(writes.len(), 4, "the tree's own record and each chunk");
    let mut desktop = Namespace::new("desktop").unwrap();
    desktop
        .take_documents(writes.into_iter().map(|w| (w.chunk, w.changes)).collect())
        .unwrap();
    assert!(
        desktop
            .lookup(ROOT_INO, format!("f{}.txt", files - 1))
            .is_some()
    );
    assert_eq!(
        desktop.indexed_document_count(),
        laptop.indexed_document_count()
    );
    assert_eq!(
        desktop.document_id_for(ROOT_INO),
        laptop.document_id_for(ROOT_INO)
    );

    // Renaming the first file writes its chunk alone, and carries the rename, not the chunk.
    let known = laptop.document_heads();
    let first = first.unwrap();
    let chunk = laptop.document_id_for(first).unwrap().chunk();
    assert!(laptop.rename(ROOT_INO, "f0.txt", ROOT_INO, "renamed.txt", 2));
    let writes = laptop.document_writes(&known);
    assert_eq!(
        writes.iter().map(|w| w.chunk).collect::<Vec<_>>(),
        [chunk],
        "a rename is a change of the file's chunk"
    );
    assert!(
        writes[0].changes.len() < 1024,
        "and carries the rename, not the chunk: {} bytes",
        writes[0].changes.len()
    );
    desktop
        .take_documents(vec![(chunk, writes[0].changes.clone())])
        .unwrap();
    assert!(desktop.lookup(ROOT_INO, "renamed.txt").is_some());
    assert!(desktop.lookup(ROOT_INO, "f0.txt").is_none());
    assert_eq!(desktop.document_heads(), laptop.document_heads());
    let kept = Namespace::from_bytes("laptop", &laptop.save()).unwrap();
    assert_eq!(
        kept.indexed_document_count(),
        laptop.indexed_document_count()
    );

    // A record that changed more than one write carries goes out over several writes, each
    // within the budget and building on the last, until the holder has every change.
    let mut laptop = Namespace::new("laptop").unwrap();
    for n in 0..200_u32 {
        laptop
            .create_file(ROOT_INO, format!("file-{n}.rs"), Some(&h(1)), 4, 1)
            .unwrap();
        laptop.document_heads();
    }
    let whole = laptop.document_write(1, &[], usize::MAX).unwrap();
    let budget = whole.changes.len() / 5;
    let mut record = Vec::new();
    let mut sent = Vec::new();
    let mut writes = 0;
    while let Some(write) = laptop.document_write(1, &sent, budget) {
        assert!(
            !write.changes.is_empty(),
            "a write carries something until the heads are reached"
        );
        assert!(
            write.changes.len() <= budget,
            "{} bytes over a budget of {budget}",
            write.changes.len()
        );
        record.extend_from_slice(&write.changes);
        sent = write.heads;
        writes += 1;
        assert!(writes < 50, "the writes reach the record's heads");
    }
    assert!(writes > 1, "the record went out in one write");
    let mut record = automerge::AutoCommit::load(&record).unwrap();
    assert_eq!(
        record.get_heads(),
        whole.heads,
        "the writes carry every change"
    );

    // A change this device wrote and has not published is not counted as one the holders have:
    // it goes out with the next publication after another device's change arrives.
    let mut laptop = Namespace::new("laptop").unwrap();
    laptop
        .create_file(ROOT_INO, "report.txt", Some(&h(1)), 1, 1)
        .unwrap();
    laptop
        .create_file(ROOT_INO, "build.bin", Some(&h(2)), 1, 1)
        .unwrap();
    let mut desktop = Namespace::new("desktop").unwrap();
    take(&mut desktop, &mut laptop);
    let (laptop_published, desktop_published) = (laptop.document_heads(), desktop.document_heads());
    assert!(desktop.rename(ROOT_INO, "report.txt", ROOT_INO, "notes.txt", 2));
    assert!(laptop.rename(ROOT_INO, "build.bin", ROOT_INO, "out.bin", 2));
    let from_laptop = laptop.document_writes(&laptop_published);
    let holders = desktop
        .take_published_documents(
            from_laptop
                .into_iter()
                .map(|w| (w.chunk, w.changes))
                .collect(),
            &desktop_published,
        )
        .unwrap();
    assert!(desktop.lookup(ROOT_INO, "out.bin").is_some());
    let from_desktop = desktop.document_writes(&holders);
    assert!(!from_desktop.is_empty(), "the desktop's rename goes out");
    laptop
        .take_documents(
            from_desktop
                .into_iter()
                .map(|w| (w.chunk, w.changes))
                .collect(),
        )
        .unwrap();
    assert!(laptop.lookup(ROOT_INO, "notes.txt").is_some());
    assert_eq!(laptop.document_heads(), desktop.document_heads());
}

#[test]
fn a_delta_applies_and_a_concurrent_write_settles_on_one_writers_content() {
    let mut source = Namespace::new("source").unwrap();
    source.create_file(ROOT_INO, "before", None, 0, 1).unwrap();
    let mut peer = Namespace::from_bytes("peer", &source.save()).unwrap();
    let heads = source.heads();
    source.create_file(ROOT_INO, "one", None, 0, 2).unwrap();
    let _ = source.save();
    source.create_file(ROOT_INO, "two", None, 0, 3).unwrap();
    let delta = source.changes_since(&heads);
    peer.apply_remote_and_resolve(&delta).unwrap();
    for name in ["one", "two"] {
        assert!(peer.lookup(ROOT_INO, name).is_some(), "{name} arrived");
    }
    let after = source.heads();
    assert!(source.changes_since(&after).is_empty());

    // Two writers of one file settle on one writer's (hash, size) pair.
    let mut a = Namespace::new("a").unwrap();
    let ino = a
        .create_file(ROOT_INO, "atomic.bin", Some(&h(1)), 1, 1)
        .unwrap();
    let mut b = Namespace::from_bytes("b", &a.save()).unwrap();
    let bino = b.lookup(ROOT_INO, "atomic.bin").unwrap().ino.0;
    assert!(a.set_content(ino, Some(&h(10)), 10, 2));
    assert!(b.set_content(bino, Some(&h(20)), 20, 2));
    a.merge_doc(&mut b).unwrap();
    let entry = a.entry_at("atomic.bin").unwrap();
    assert!(
        (entry.hash, entry.size) == (Some(h(10)), 10)
            || (entry.hash, entry.size) == (Some(h(20)), 20),
        "the hash and the size come from the same write: {entry:?}"
    );
    assert!(a.set_content(ino, None, 0, 3));
    assert_eq!(a.hash_for(ino), None, "an empty file names no content");
    assert_eq!(a.getattr(ino).unwrap().size, 0);
}

#[test]
fn a_folder_reads_as_changed_with_its_newest_entry() {
    let mut ns = Namespace::new("laptop").unwrap();
    let opened = ns.getattr(ROOT_INO).unwrap().mtime;
    assert!(
        opened > UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        "an empty folder reads as changed when it opened, not in 1970: {opened:?}"
    );
    let newest = 1_790_000_000_000;
    ns.create_file(ROOT_INO, "terms.txt", Some(&h(1)), 6, newest)
        .unwrap();
    ns.create_file(ROOT_INO, "older.txt", Some(&h(2)), 6, newest - 60_000)
        .unwrap();
    assert_eq!(
        ns.getattr(ROOT_INO).unwrap().mtime,
        UNIX_EPOCH + Duration::from_millis(newest)
    );
}

#[test]
fn a_write_names_its_file_actions_and_the_invoker_that_made_it() {
    let agent = tilo_actions::ActionInvoker::admitted(
        &tilo_actions::Acting::agent("claude").in_session("s1"),
    )
    .named(Some("Tidy the notes".into()));
    let mut ns = Namespace::new("ada").unwrap();
    let ino = {
        let _acting = acting_as(Some(
            tilo_actions::ActionContext::default().with_invoker(agent),
        ));
        let ino = ns
            .create_file(ROOT_INO, "plan.md", Some(&h(1)), 4, 1)
            .unwrap();
        assert!(ns.set_content(ino, Some(&h(2)), 5, 2));
        ino
    };
    let chunk = ns.document_id_for(ino).unwrap().chunk();
    assert!(ns.rename(ROOT_INO, "plan.md", ROOT_INO, "final.md", 3));
    let document = ns.doc.doc_mut(chunk).unwrap().save();
    let entries = entries_of(&document);
    assert_eq!(
        entries,
        vec![
            (
                vec!["new-file".to_owned(), "write".to_owned()],
                Some("Tidy the notes".to_owned())
            ),
            (vec!["move".to_owned()], None),
        ],
        "one change by the agent, one by the person"
    );
    assert_eq!(ns.hash_for(ino), Some(h(2)));

    // The tree's own document changes only as part of making a folder or a file.
    let agent = tilo_actions::ActionInvoker::admitted(
        &tilo_actions::Acting::agent("claude").in_session("s1"),
    )
    .named(Some("Tidy the notes".into()));
    let mut ns = Namespace::new("ada").unwrap();
    {
        let _acting = acting_as(Some(
            tilo_actions::ActionContext::default().with_invoker(agent),
        ));
        ns.create_file(ROOT_INO, "plan.md", None, 0, 1).unwrap();
    }
    let index = ns.doc.doc_mut(0).unwrap().save();
    let mut doc = automerge::AutoCommit::load(&index).unwrap();
    let named = doc
        .get_changes(&[])
        .iter()
        .filter(|change| !change.is_empty())
        .all(|change| {
            change.message().is_some_and(|message| {
                tilo_actions::ActionContext::from_change_message(message).is_some()
                    || tilo_actions::FrameworkChange::from_change_message(message)
                        == Some(tilo_actions::FrameworkChange::Structure)
            })
        });
    assert!(
        named,
        "every change of the tree's own document names what it was part of"
    );
    assert_eq!(
        entries_of(&index),
        vec![
            (vec!["new-folder".to_owned()], None),
            (
                vec!["new-file".to_owned()],
                Some("Tidy the notes".to_owned())
            ),
        ]
    );
}

#[test]
fn a_lease_has_one_device_whose_fence_and_copies_agree() {
    const ME: [u8; 32] = [1; 32];
    const OTHER: [u8; 32] = [2; 32];
    const TTL: u64 = 60_000;

    let first = WriteLease::take(None, ME, 1_000, TTL).unwrap();
    assert_eq!(
        (first.holder, first.epoch, first.expires_ms),
        (ME, 1, 61_000)
    );
    let renewed = WriteLease::take(Some(first), ME, 30_000, TTL).unwrap();
    assert_eq!(
        (renewed.epoch, renewed.expires_ms),
        (1, 90_000),
        "renewal keeps the epoch"
    );
    let held = WriteLease::take(Some(renewed), OTHER, 40_000, TTL).unwrap_err();
    assert_eq!(held.holder, ME);
    let lapsed = WriteLease::take(Some(renewed), OTHER, 90_000, TTL).unwrap();
    assert_eq!((lapsed.holder, lapsed.epoch), (OTHER, 2));
    let released = WriteLease {
        expires_ms: 45_000,
        ..renewed
    };
    assert!(!released.granted, "a device's own take is optimistic");
    let handed_over = WriteLease::take(Some(released), OTHER, 45_000, TTL).unwrap();
    assert_eq!(handed_over.epoch, 2, "a released lease is a new tenancy");

    // Replicas converge on one holder, and a granted lease beats an optimistic one.
    let mut a = Namespace::new("a").unwrap();
    let record = FileDocumentId::new();
    let mut b = Namespace::from_bytes("b", &a.save()).unwrap();
    let optimistic = WriteLease::take(None, [9; 32], 0, TTL).unwrap();
    let granted = WriteLease {
        granted: true,
        ..WriteLease::take(None, [1; 32], 0, TTL).unwrap()
    };
    a.put_write_lease(record, &granted);
    b.put_write_lease(record, &optimistic);
    let from_a = a.save_incremental();
    let from_b = b.save_incremental();
    a.apply_incremental(&from_b).unwrap();
    b.apply_incremental(&from_a).unwrap();
    for ns in [&a, &b] {
        assert_eq!(ns.write_lease(record), Some(granted));
        assert_eq!(ns.lease_holder(record, 1), Some([1; 32]));
        assert_eq!(ns.write_leases().len(), 1, "one record, one lease");
    }

    // A fence binds a version to one holder's epoch, and a different version or holder gets none.
    let fence = VersionFence {
        holder: [3; 32],
        epoch: 4,
    };
    assert!(a.put_version_fence(record, Some([7; 32]), 10, fence));
    assert_eq!(a.version_fence(record, Some([7; 32]), 10), Some(fence));
    assert_eq!(a.version_fence(record, Some([8; 32]), 10), None);

    // An authority and a request replicate; a request is removed once answered.
    let mut a = Namespace::new("a").unwrap();
    let record = FileDocumentId::new();
    assert!(a.lease_authority().is_none());
    assert!(a.put_lease_authority([4; 32]));
    assert!(a.put_lease_request(record, [2; 32], LeaseRequest { nonce: 7 }));
    let b = Namespace::from_bytes("b", &a.save()).unwrap();
    assert_eq!(b.lease_authority(), Some([4; 32]));
    assert_eq!(
        b.lease_requests(),
        vec![(record, [2; 32], LeaseRequest { nonce: 7 })]
    );
    a.remove_lease_request(record, [2; 32]);
    assert!(a.lease_requests().is_empty());
}

#[test]
fn byte_names_survive_load_save_rename_and_replication_exactly() {
    use automerge::ScalarValue;

    {
        let mut legacy = Namespace::new("legacy-name").unwrap();
        let ino = legacy
            .create_file(ROOT_INO, "placeholder", None, 0, 1)
            .unwrap();
        let id = legacy.id_for_ino(ino).unwrap().to_string();
        let node = legacy.node(&id).unwrap();
        legacy
            .doc
            .put(&node, "name", ScalarValue::Str("café.txt".into()))
            .unwrap();
        let mut migrated = Namespace::from_bytes("migrated", &legacy.save()).unwrap();
        assert!(migrated.lookup(ROOT_INO, "café.txt").is_some());
        assert!(migrated.rename(ROOT_INO, "café.txt", ROOT_INO, b"caf\xc3\xa9-final.txt", 2));
        let node = migrated.node(&id).unwrap();
        assert!(matches!(
            migrated.doc.get(&node, "name").unwrap(),
            Some((automerge::Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Bytes(bytes) if bytes.as_slice() == b"caf\xc3\xa9-final.txt")
        ));
        let mut reopened = Namespace::from_bytes("reopened", &migrated.save()).unwrap();
        assert!(
            reopened
                .lookup(ROOT_INO, b"caf\xc3\xa9-final.txt")
                .is_some(),
            "legacy UTF-8 identity must survive byte-scalar migration and restart"
        );
    }
    {
        let mut ns = Namespace::new("bytes").unwrap();
        let original = b"photo-\xFF.png";
        let renamed = b"photo-\xFE.png";
        let ino = ns.create_file(ROOT_INO, original, None, 0, 1).unwrap();
        assert_eq!(ns.lookup(ROOT_INO, original).unwrap().ino.0, ino);
        let snapshot = ns.save();
        let mut reopened = Namespace::from_bytes("reopened", &snapshot).unwrap();
        let reopened_ino = reopened.lookup(ROOT_INO, original).unwrap().ino.0;
        let document_id = reopened.document_id_for(reopened_ino).unwrap();
        assert!(reopened.rename(ROOT_INO, original, ROOT_INO, renamed, 2));
        assert!(reopened.lookup(ROOT_INO, original).is_none());
        assert_eq!(
            reopened.lookup(ROOT_INO, renamed).unwrap().ino.0,
            reopened_ino
        );
        let mut final_open = Namespace::from_bytes("final", &reopened.save()).unwrap();
        assert!(final_open.lookup(ROOT_INO, original).is_none());
        let final_ino = final_open.lookup(ROOT_INO, renamed).unwrap().ino.0;
        assert_eq!(final_open.document_id_for(final_ino), Some(document_id));
    }
    {
        let name = b"replicated-\xFF.bin";
        let mut source = Namespace::new("source").unwrap();
        source.create_file(ROOT_INO, name, None, 0, 1).unwrap();
        let mut peer = Namespace::from_bytes("peer", &source.save()).unwrap();
        assert!(peer.lookup(ROOT_INO, name).is_some());
        let renamed = b"replicated-\xFE.bin";
        assert!(source.rename(ROOT_INO, name, ROOT_INO, renamed, 2));
        let delta = source.save_incremental();
        peer.apply_remote_and_resolve(&delta).unwrap();
        assert!(peer.lookup(ROOT_INO, name).is_none());
        assert!(peer.lookup(ROOT_INO, renamed).is_some());
    }
}
