use super::*;
use std::path::Path;

/// A chain's parts and an on-demand record are ordinary record ids, in
/// the wire form the host reads them by.
#[test]
fn a_chain_part_and_an_on_demand_record_are_record_ids() {
    let chat = RecordId::workspace_default();
    let part = chat.part(12);
    assert_eq!(part.as_str(), format!("derived:part.12@{}", chat.as_str()));
    assert_eq!(RecordId::parse(part.as_str()), Some(part.clone()));
    assert_eq!(part.chain_part(), Some((chat.clone(), 12)));
    assert_eq!(chat.part(0).chain_part(), Some((chat.clone(), 0)));
    assert_eq!(chat.chain_part(), None);
    let archive = RecordId::minted().on_demand();
    assert!(archive.as_str().starts_with("derived:on-demand@uuid:"));
    assert_eq!(RecordId::parse(archive.as_str()), Some(archive));
}

/// A child record is an ordinary record id that names its parent and
/// its own id, whatever lists it; an id that cannot name a record is
/// refused.
#[test]
fn a_child_record_names_its_parent_and_its_own_id() {
    let chat = RecordId::workspace_default();
    let channel = chat.child("c_design").unwrap();
    assert_eq!(
        channel.as_str(),
        format!("derived:child.c_design@{}", chat.as_str())
    );
    assert_eq!(RecordId::parse(channel.as_str()), Some(channel.clone()));
    assert_eq!(
        channel.child_of(),
        Some((chat.clone(), "c_design".to_owned()))
    );
    assert_eq!(chat.child_of(), None);
    assert_eq!(chat.part(1).child_of(), None);
    for refused in ["", "has space", "a/b", &"x".repeat(80)] {
        assert_eq!(chat.child(refused), None, "{refused:?}");
    }
    assert_eq!(
        children_resource_id(&chat, "chat.channel"),
        format!("children:chat.channel@{}", chat.as_str())
    );
}

/// Two devices creating the same record make byte for byte the same
/// single genesis change, under the record's own actor at time zero,
/// and converge on one record.
#[test]
fn two_creators_of_one_record_make_one_identical_genesis() {
    let kind = RecordKind::new("test.genesis-note");
    // The app's surface is named in the record's identity.
    crate::app_surface::register_package("so.tilo.test-genesis", b"surface");
    crate::app_surface::register_kind(kind.as_str(), "so.tilo.test-genesis");
    let checkout = RecordCheckout::new(RecordId::for_tab(3), kind);
    let create = |actor: &[u8]| {
        let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(actor));
        genesis(&mut doc, &checkout, "workspace", |doc| {
            doc.put(automerge::ROOT, "title", "Untitled")
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .unwrap();
        // What a store commits after the seed adds nothing to it.
        commit_genesis(&mut doc);
        doc
    };
    let mut one = create(b"device-one");
    let mut two = create(b"device-two");
    let changes = one.get_changes(&[]);
    assert_eq!(changes.len(), 1, "genesis is one change");
    let genesis = &changes[0];
    assert_eq!(
        genesis.actor_id(),
        &genesis_actor_of(&checkout.id, &checkout.kind, "workspace")
    );
    assert_eq!(genesis.timestamp(), 0);
    assert_eq!(
        one.get_changes(&[])
            .iter()
            .map(|change| change.hash())
            .collect::<Vec<_>>(),
        two.get_changes(&[])
            .iter()
            .map(|change| change.hash())
            .collect::<Vec<_>>(),
        "both devices make the identical genesis"
    );
    one.merge(&mut two).unwrap();
    assert_eq!(one.get_changes(&[]).len(), 1, "they converge on one record");
}

/// A part another device started reaches this one while its window
/// already holds the chain: kept in the store, it is a held part, and
/// it opens.
#[test]
fn a_part_stored_after_the_window_opened_is_held_and_opens() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let chain = RecordId::derived("messages", &RecordId::workspace_default());
    let part = RecordCheckout::new(chain.part(0), RecordKind::new("test.chain-part"));
    let key = StorageKey::document(
        "so.tilo.test-chain",
        &workspace,
        chain.part(0).storage_key(),
    );

    let mut starter = RecordOwners::with_store(Rc::new(durable_store::MemoryStore::new()));
    let started = starter.open("so.tilo.test-chain", &workspace).unwrap();
    started
        .reserve_record(&part)
        .unwrap()
        .initialize(|doc| {
            doc.put(automerge::ROOT, "messages", 1)
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let bytes = started.read(&part.id, |doc| doc.clone().save()).unwrap();

    let backend = Rc::new(durable_store::MemoryStore::new());
    let mut owners = RecordOwners::with_store(Rc::clone(&backend) as Rc<dyn DurableStore>);
    let store = owners.open("so.tilo.test-chain", &workspace).unwrap();
    assert!(store.held_parts(&chain).is_empty());
    backend
        .write(&key, durable_store::WriteOp::Replace(bytes), None)
        .unwrap();
    assert_eq!(store.held_parts(&chain), vec![0]);
    store.try_open_record(&part).unwrap();
}

/// A record reads back with the kind it was stamped with — a kind is the name
/// its app writes — and reading it leaves its attributed history alone.
#[test]
fn records_read_back_with_the_kind_they_were_stamped_with() {
    for kind in ["tilo.rich-text", "document"] {
        let stamped = RecordKind::new(kind);
        assert!(stamped.names(kind));
        let id = RecordId::for_tab(7);
        let mut doc = AutoCommit::new();
        init(&mut doc, &id, &stamped, "workspace");
        let meta = meta_obj(&mut doc).unwrap();
        doc.put(&meta, "kind", kind).unwrap();
        doc.commit();
        let heads = doc.get_heads();
        assert_eq!(
            identity(&doc),
            Some(RecordCheckout::new(id.clone(), stamped.clone()))
        );
        assert!(init_scoped(&mut doc, &id, &stamped, "workspace", None));
        doc.commit();
        assert_eq!(doc.get_heads(), heads);
    }
}

/// A record written now names its workspace under the key every record is
/// written with — the name a record written before this build carried it —
/// and reads back with that workspace after a save.
#[test]
fn a_record_written_now_names_its_workspace_under_the_stored_key() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("test.workspace-key"));
    let mut doc = AutoCommit::new();
    assert!(init_scoped(
        &mut doc,
        &checkout.id,
        &checkout.kind,
        &workspace,
        Some(&workspace)
    ));
    let meta = meta_obj(&mut doc).unwrap();
    assert_eq!(
        str_at(&doc, &meta, WORKSPACE).as_deref(),
        Some(workspace.as_str())
    );
    assert!(
        matches!(string_at(&doc, &meta, WORKSPACE_ALIAS), StringAt::Missing),
        "a new record does not also write the key one build named"
    );
    assert_eq!(workspace_scope(&doc).as_deref(), Some(workspace.as_str()));
    let saved = doc.save();
    let reloaded = AutoCommit::load(&saved).unwrap();
    assert_eq!(
        workspace_scope(&reloaded).as_deref(),
        Some(workspace.as_str()),
        "the key is the document's, not the process's"
    );
}

/// A record written while the field was named `workspace` keeps its workspace
/// under that key: it opens with that workspace and its history, and opening
/// it does not rewrite it.
#[test]
fn a_record_written_while_the_field_was_named_workspace_reads_with_its_workspace() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let package = "so.tilo.test-legacy-key";
    let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("test.legacy-note"));
    // The document as the build of the time left it on disk.
    let mut written = AutoCommit::new();
    written.put(ROOT, "title", "Field notes").unwrap();
    assert!(init_scoped(
        &mut written,
        &checkout.id,
        &checkout.kind,
        &workspace,
        None
    ));
    let meta = meta_obj(&mut written).unwrap();
    written.delete(&meta, WORKSPACE).unwrap();
    written.put(&meta, WORKSPACE_ALIAS, &workspace).unwrap();
    written.commit();
    let history = written.get_heads();

    let backend = Rc::new(durable_store::MemoryStore::new());
    let key = StorageKey::document(package, &workspace, checkout.id.storage_key());
    backend
        .write(&key, durable_store::WriteOp::Replace(written.save()), None)
        .unwrap();

    let mut owners = RecordOwners::with_store(Rc::clone(&backend) as Rc<dyn DurableStore>);
    let store = owners.open(package, &workspace).unwrap();
    store
        .try_open_record(&checkout)
        .expect("it opens as before");
    let (title, heads, current, namespaced) = store
        .read(&checkout.id, |doc| {
            let meta = meta_ro(doc).unwrap();
            (
                str_at(doc, &ROOT, "title"),
                doc.clone().get_heads(),
                str_at(doc, &meta, WORKSPACE),
                str_at(doc, &meta, WORKSPACE_ALIAS),
            )
        })
        .unwrap();
    assert_eq!(title.as_deref(), Some("Field notes"));
    assert_eq!(
        namespaced.as_deref(),
        Some(workspace.as_str()),
        "its workspace reads"
    );
    assert_eq!(current, None, "reading it did not move the key");
    assert_eq!(heads, history, "opening it is not a change");

    // It saves, reads and takes edits like any record.
    store
        .with(&checkout.id, |doc| {
            doc.put(ROOT, "title", "Field notes, revised").unwrap();
        })
        .unwrap();
    let edited = store
        .read(&checkout.id, |doc| str_at(doc, &ROOT, "title"))
        .unwrap();
    assert_eq!(edited.as_deref(), Some("Field notes, revised"));
}

/// Two members of a workspace each create the record the workspace derives for
/// them, before either has heard of the other's: the same record, from
/// the same initial content. Their documents merge into one record whose
/// content has one value at every key, not two competing roots.
#[test]
fn members_creating_the_same_record_converge_on_one() {
    let initialize = |doc: &mut AutoCommit| {
        for key in ["pages", "tags"] {
            doc.put_object(ROOT, key, ObjType::Map)
                .map_err(|e| e.to_string())?;
        }
        doc.put(ROOT, "schema_version", 1_u64)
            .map_err(|e| e.to_string())?;
        Ok(())
    };
    let checkout = RecordCheckout::new(
        RecordId::derived("default-notebook", &RecordId::for_tab(7)),
        RecordKind::new("notebook"),
    );
    let temp = tempfile::tempdir().unwrap();
    let created: Vec<Vec<u8>> = ["ada", "bea"]
        .into_iter()
        .map(|member| {
            let mut owners = RecordOwners::new(temp.path().join(member));
            let store = owners.open("so.tilo.test", "workspace-a").unwrap();
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(initialize)
                .unwrap();
            store.read(&checkout.id, |doc| doc.clone().save()).unwrap()
        })
        .collect();
    let mut merged = AutoCommit::load(&created[0]).unwrap();
    let mut other = AutoCommit::load(&created[1]).unwrap();
    tilo_automerge::ApplyIncoming::merge_incoming(&mut merged, &mut other).unwrap();
    for key in ["pages", "tags", "schema_version"] {
        assert_eq!(
            merged.get_all(ROOT, key).unwrap().len(),
            1,
            "{key} has one value once both members' records meet"
        );
    }
    assert_eq!(
        identity(&merged).map(|found| found.id),
        Some(checkout.id.clone()),
        "the record keeps its one identity"
    );
    // Each member still writes as itself after the shared creation.
    let actors: std::collections::BTreeSet<_> = [&created[0], &created[1]]
        .iter()
        .map(|bytes| AutoCommit::load(bytes).unwrap().get_actor().clone())
        .collect();
    assert_eq!(actors.len(), 2, "the members keep their own actors");
}

#[test]
fn record_identities_keys_and_homes_are_stable_and_distinct() {
    {
        let mut doc = AutoCommit::new();
        let blob = BlobRef {
            hash: "ab".repeat(32),
            len: 1234,
            mime: "application/pdf".into(),
            relation: "source".into(),
        };
        assert!(add_blob_reference(&mut doc, &blob));
        assert!(
            !add_blob_reference(&mut doc, &blob),
            "a reference is a fact"
        );
        assert!(!add_blob_reference(
            &mut doc,
            &BlobRef {
                hash: "not-a-hash".into(),
                ..blob.clone()
            }
        ));
        let reloaded = AutoCommit::load(&doc.save()).expect("reload");
        assert_eq!(blob_references(&reloaded), vec![blob]);
    }

    {
        #[cfg(windows)]
        let (root, expected) = ("C:/work", r"file:C:\work\report.odt");
        #[cfg(not(windows))]
        let (root, expected) = ("/work", "file:/work/report.odt");
        let path = Path::new(root).join("report.odt");
        let id = RecordId::for_file(&path);
        assert_eq!(id.as_str(), expected);
        assert_eq!(id.file_path().unwrap(), path);
        assert_eq!(RecordId::parse(expected), Some(id.clone()));
        // Same file, same record, including lexical path aliases.
        assert_eq!(id, RecordId::for_file(Path::new(root).join("./report.odt")));
        assert_eq!(
            id,
            RecordId::for_file(Path::new(root).join("drafts/../report.odt"))
        );
    }

    {
        assert_ne!(RecordId::minted(), RecordId::minted());
    }

    {
        let a = RecordId::for_file("/work/report.odt");
        let b = RecordId::for_file("/work/other.odt");
        assert_eq!(
            a.storage_key(),
            RecordId::for_file("/work/report.odt").storage_key()
        );
        assert_ne!(a.storage_key(), b.storage_key());
        // A traversal in the identity must not survive into the key.
        let key = RecordId::for_file("/work/../../etc/shadow").storage_key();
        assert!(!key.contains('/'), "key {key} must name one directory");
        assert!(!key.contains(".."), "key {key} must not traverse");

        // Paths that agree past the readable part of the key are still
        // two documents — a sanitise-only key would collide them.
        let deep_a =
            RecordId::for_file("/work/clients/northwind/2026/contracts/final/appendix-a.odt");
        let deep_b =
            RecordId::for_file("/work/clients/northwind/2026/contracts/final/appendix-b.odt");
        assert_ne!(
            deep_a.storage_key(),
            deep_b.storage_key(),
            "two long paths sharing a prefix must not share a file"
        );
    }

    {
        let mut doc = AutoCommit::new();
        let id = RecordId::for_file("/work/report.odt");
        let kind = RecordKind::new("document");
        init(&mut doc, &id, &kind, "workspace-1");
        let got = identity(&doc).expect("the document knows which record it is");
        assert_eq!(got.id, id);
        assert_eq!(got.kind, kind);
        assert_eq!(home(&doc).as_deref(), Some("workspace-1"));
    }

    {
        let mut doc = AutoCommit::new();
        let id = RecordId::minted();
        let kind = RecordKind::new("book");
        init(&mut doc, &id, &kind, "workspace-1");
        init(&mut doc, &id, &kind, "workspace-2");
        assert_eq!(
            home(&doc).as_deref(),
            Some("workspace-1"),
            "a record has one home and any number of references"
        );
    }

    {
        let file = RecordId::for_file("/tmp/shared/report.pdf");
        let derived = RecordId::derived("annotations", &file);
        assert!(RecordId::parse(derived.as_str()).is_some(), "{derived}");
    }

    {
        let mut workspace_doc = AutoCommit::new();
        let origin = RecordCheckout::new(RecordId::minted(), RecordKind::new("book"));
        index::reference(&mut workspace_doc, &origin.id, &origin.kind);
        let copy = index::duplicate(&mut workspace_doc, &origin);
        assert_ne!(copy.id, origin.id, "copying mints a new identity");
        assert!(index::holds(&workspace_doc, &copy.id));
        assert!(index::holds(&workspace_doc, &origin.id));
    }
}
use crate::state::State;

/// The native store over one test's own directory: no process
/// environment, so these tests run in parallel.
fn native(root: &Path) -> Rc<dyn DurableStore> {
    Rc::new(app_state::FsStore::new(root))
}

#[test]
fn canonical_records_persist_through_the_host_store_and_report_save_status() {
    {
        let temp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(temp.path().join("state"));
        let store = owners.open("so.tilo.test", "status-workspace").unwrap();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let mut reservation = store.reserve_record(&checkout).unwrap();
        reservation
            .initialize(|doc| {
                doc.put(ROOT, "value", 0_u64)
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .unwrap();
        store.try_flush().unwrap();
        store
            .with(&checkout.id, |doc| doc.put(ROOT, "value", 1_u64).unwrap())
            .unwrap();
        store.tick().unwrap();
        let older = store.read(&checkout.id, AutoCommit::base_heads).unwrap();
        store
            .with(&checkout.id, |doc| doc.put(ROOT, "value", 2_u64).unwrap())
            .unwrap();
        let pending = store
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        assert!(pending.1 > 0);
        store.await_submitted(&checkout.id).unwrap();
        assert_eq!(
            store.0.borrow().open[&checkout.id]
                .autosave
                .as_ref()
                .unwrap()
                .durability_snapshot()
                .durable_heads,
            Some(older.clone()),
            "older writer acknowledgement missing"
        );
        assert_eq!(
            store.save_status(&checkout.id).unwrap(),
            RecordSaveStatus::Saving
        );
        assert_eq!(
            store
                .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap(),
            pending
        );
        store.try_flush().unwrap();
        assert_eq!(
            store.save_status(&checkout.id).unwrap(),
            RecordSaveStatus::Saved
        );
        drop(reservation);
        drop(store);
        drop(owners);
    }

    {
        use durable_store::{DurableStore, MemoryStore, StorageKey};
        let backend = Rc::new(MemoryStore::queued());
        let mut owners = RecordOwners::with_store(backend.clone());
        let store = owners.open("so.tilo.test", "workspace-a").unwrap();
        let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        store
            .reserve_record(&want)
            .unwrap()
            .initialize(|doc| {
                put_text(doc, "body", "hosted");
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.save_status(&want.id).unwrap(),
            RecordSaveStatus::Saving
        );
        assert!(
            store.try_flush().is_err(),
            "nothing is saved before the host commits"
        );
        for write in backend.drain() {
            backend.acknowledge(write.revision, Ok(())).unwrap();
        }
        store.try_flush().unwrap();
        assert_eq!(
            store.save_status(&want.id).unwrap(),
            RecordSaveStatus::Saved
        );
        assert_eq!(store.canonical_namespace().unwrap().0, backend.namespace());
        drop((store, owners));

        let key = StorageKey::document("so.tilo.test", "workspace-a", want.id.storage_key());
        let restarted = Rc::new(MemoryStore::queued().named(backend.namespace()));
        restarted
            .hydrate(key.clone(), backend.read(&key).unwrap().unwrap())
            .unwrap();
        let mut owners = RecordOwners::with_store(restarted);
        let store = owners.open("so.tilo.test", "workspace-a").unwrap();
        assert_eq!(
            store.try_open_record(&want).unwrap(),
            RecordOpenOutcome::Loaded
        );
        assert_eq!(
            store
                .read(&want.id, |doc| text(doc, "body"))
                .unwrap()
                .as_deref(),
            Some("hosted")
        );
        assert_eq!(
            store.save_status(&want.id).unwrap(),
            RecordSaveStatus::Saved
        );
    }

    {
        let store = RecordStore::in_memory();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        assert!(store.open_record(&checkout));
        store
            .with_action_state(&checkout.id, |_, _, _| {
                assert!(matches!(
                    store.save_status(&checkout.id),
                    Err(RecordOpenError::Busy)
                ));
            })
            .unwrap();
        assert_eq!(
            store.save_status(&checkout.id).unwrap(),
            RecordSaveStatus::MemoryOnly
        );
    }

    {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("state");
        let mut owners = RecordOwners::new(root);
        let store = owners.open("so.tilo.test", "workspace-a").unwrap();
        let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let mut token = store.reserve_record(&want).unwrap();
        let generation = token.generation;
        let owner = store.0.borrow().pending[&want.id].store.clone().unwrap();
        let failure = token.initialize_with(
            |doc| {
                put_text(doc, "discard", "private");
                Ok(())
            },
            |_| Ok(()),
            |_, _| {
                Err(app_state::StoreError::Io {
                    context: "injected initial write refusal".into(),
                    source: std::io::Error::from(std::io::ErrorKind::WouldBlock),
                })
            },
        );
        assert!(matches!(failure, Err(RecordOpenError::Store(_))));
        assert!(store.open_records().is_empty());
        let directory = app_state::DocStore::workspace_document_dir_at(
            &tmp.path().join("state"),
            "so.tilo.test",
            "workspace-a",
            &want.id.storage_key(),
        )
        .unwrap();
        assert!(!directory.join(app_state::DOCUMENT_FILE).exists());
        {
            let set = store.0.borrow();
            let reserved = &set.pending[&want.id];
            assert_eq!(reserved.generation, generation);
            assert!(Rc::ptr_eq(&owner, reserved.store.as_ref().unwrap()));
        }
        drop(owner);
        assert!(app_state::DocStore::open_canonical_at("so.tilo.test", &directory).is_err());
        token
            .initialize(|doc| {
                assert!(doc.get(ROOT, "discard").unwrap().is_none());
                Ok(())
            })
            .unwrap();
        assert!(
            token
                .initialize(|_| panic!("second initializer must not run"))
                .is_err()
        );
        drop(token);
        assert!(store.is_ready(&want.id));
        // Construct a stale-generation witness privately: no public handle can do this.
        let other = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let token = store.reserve_record(&other).unwrap();
        let stale_generation = token.generation;
        drop(token);
        let newer = store.reserve_record(&other).unwrap();
        let stale = RecordReservation {
            store: store.clone(),
            checkout: other.clone(),
            generation: stale_generation,
        };
        drop(stale);
        assert!(matches!(
            store.try_open_record(&other),
            Err(RecordOpenError::Busy)
        ));
        drop(newer);
        assert!(matches!(
            store.try_open_record(&other),
            Err(RecordOpenError::MissingRecord)
        ));
    }

    {
        let tmp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(tmp.path().join("state"));
        let store = owners.open("so.tilo.test", "workspace-a").unwrap();
        let second = owners.open("so.tilo.test", "workspace-a").unwrap();
        let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        store
            .reserve_record(&want)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        store.try_flush().unwrap();
        let before = {
            let set = store.0.borrow();
            let p = &set.open[&want.id];
            let saver = p.autosave.as_ref().unwrap();
            let (full_writes, appends) = (saver.full_writes(), saver.appends());
            (full_writes, appends, p.history.revision())
        };
        store.with(&want.id, |doc| put_text(doc, "body", "one edit"));
        store.tick().unwrap();
        second.tick().unwrap();
        assert!(matches!(
            second.try_open_record(&want),
            Ok(RecordOpenOutcome::AlreadyOpen)
        ));
        second.try_flush().unwrap();
        let after = {
            let set = store.0.borrow();
            let p = &set.open[&want.id];
            let saver = p.autosave.as_ref().unwrap();
            let (full_writes, appends) = (saver.full_writes(), saver.appends());
            (full_writes, appends, p.history.revision())
        };
        assert_eq!(after, (before.0, before.1 + 1, before.2));
    }
}

fn put_text(doc: &mut AutoCommit, key: &str, value: &str) {
    let _ = doc.put(&ROOT, key, value);
}

fn text(doc: &AutoCommit, key: &str) -> Option<String> {
    str_at(doc, &ROOT, key)
}

#[test]
fn workspaces_hold_references_and_discover_metadata_only() {
    {
        let mut markup = AutoCommit::new();
        let book = RecordId::for_file("/work/book.pdf");
        init(
            &mut markup,
            &RecordId::minted(),
            &RecordKind::new("annotation-set"),
            "workspace-1",
        );
        assert!(add_reference(
            &mut markup,
            &RecordRef::new(book.clone(), "marks-up")
        ));
        // Asserting the same reference twice is not two references.
        assert!(!add_reference(
            &mut markup,
            &RecordRef::new(book.clone(), "marks-up")
        ));
        let refs = references(&markup);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, book);
        assert_eq!(refs[0].relation, "marks-up");
    }

    {
        let mut workspace_doc = AutoCommit::new();
        let a = RecordId::for_file("/work/a.odt");
        let b = RecordId::for_file("/work/b.odt");
        let kind = RecordKind::new("document");
        assert!(index::reference(&mut workspace_doc, &a, &kind));
        assert!(
            !index::reference(&mut workspace_doc, &a, &kind),
            "referencing twice adds nothing"
        );
        assert!(index::reference(&mut workspace_doc, &b, &kind));
        assert!(index::holds(&workspace_doc, &a) && index::holds(&workspace_doc, &b));
        assert_eq!(index::references(&workspace_doc).len(), 2);

        assert!(index::unreference(&mut workspace_doc, &a));
        assert!(
            !index::holds(&workspace_doc, &a),
            "dropping the reference removes the access"
        );
        assert!(index::holds(&workspace_doc, &b), "and touches nothing else");
    }

    {
        let mut remote_workspace = AutoCommit::new();
        let remote = RecordCheckout::new(RecordId::minted(), RecordKind::new("annotation-set"));
        assert!(index::reference(
            &mut remote_workspace,
            &remote.id,
            &remote.kind
        ));

        // Simulate a collaborator's workspace-index snapshot arriving before this
        // process starts. Learning it must not manufacture a record document.
        let mut local_workspace = AutoCommit::new();
        local_workspace
            .merge(&mut remote_workspace)
            .expect("workspace indexes merge like other collaboration documents");
        let mut set = RecordSet::in_memory().with_home("workspace-a");
        assert_eq!(
            set.discover_workspace(&local_workspace),
            vec![remote.clone()]
        );
        assert_eq!(set.known_records(), vec![remote.clone()]);
        assert!(
            set.open_records().is_empty(),
            "discovery does not open replicas"
        );

        assert!(index::unreference(&mut local_workspace, &remote.id));
        assert!(set.discover_workspace(&local_workspace).is_empty());
        assert!(
            set.known_records().is_empty(),
            "revocation removes future discovery"
        );
        assert!(set.open_records().is_empty());
    }

    {
        let mut workspace_doc = AutoCommit::new();
        let idx = workspace_doc
            .put_object(&ROOT, INDEX, ObjType::Map)
            .expect("workspace index");
        // Not an ID shape the host understands, and not a typed map.
        let _ = workspace_doc.put(&idx, "../../escape", "annotation-set");
        let _ = workspace_doc.put_object(&idx, "uuid:not-a-uuid", ObjType::Map);
        for _ in 0..(MAX_DISCOVERED_RECORDS + 8) {
            let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
            assert!(index::reference(
                &mut workspace_doc,
                &checkout.id,
                &checkout.kind
            ));
        }

        let mut set = RecordSet::in_memory().with_home("workspace-a");
        let found = set.discover_workspace(&workspace_doc);
        assert_eq!(found.len(), MAX_DISCOVERED_RECORDS);
        assert!(found.iter().all(RecordCheckout::is_valid));
        assert!(set.open_records().is_empty());
    }

    {
        let dir = tempfile::tempdir().expect("temporary state directory");
        let documents = native(dir.path());
        let package = "so.tilo.workspace-discovery";
        let a = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let b = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let entry = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));

        let open_workspace = |workspace: &str| {
            ::collaboration::record::save::open_stored(
                ::collaboration::record::save::workspace_document(&documents, package, workspace)
                    .expect("workspace index"),
            )
        };
        let (mut workspace_a, mut saver_a) = open_workspace(a);
        assert!(index::reference(&mut workspace_a, &entry.id, &entry.kind));
        workspace_a.commit();
        saver_a.request(&workspace_a);
        saver_a.flush();

        let (workspace_b, _) = open_workspace(b);
        let mut set_b = RecordSet::open_in(Rc::clone(&documents), package, b);
        assert!(set_b.discover_workspace(&workspace_b).is_empty());

        let (workspace_a_restarted, _) = open_workspace(a);
        let mut set_a = RecordSet::open_in(Rc::clone(&documents), package, a);
        assert_eq!(
            set_a.discover_workspace(&workspace_a_restarted),
            vec![entry]
        );
        assert!(set_a.open_records().is_empty());
    }

    {
        let mut workspace_doc = AutoCommit::new();
        let mut set = RecordSet::in_memory();
        let mut state = State::new();
        let file = RecordId::for_file("/work/book.pdf");
        index::reference(&mut workspace_doc, &file, &RecordKind::new("file"));

        let markup = RecordCheckout::new(RecordId::minted(), RecordKind::new("annotation-set"));
        set.checkout(&mut state, Some(&mut workspace_doc), &markup);
        add_reference(&mut state.doc, &RecordRef::new(file.clone(), "marks-up"));
        // Park it so `peek` can see it — the live document is not parked.
        let other = RecordCheckout::new(RecordId::minted(), RecordKind::new("annotation-set"));
        set.checkout(&mut state, Some(&mut workspace_doc), &other);

        let found = index::referencing(&workspace_doc, &set, &file, "marks-up");
        assert_eq!(found.len(), 1, "exactly the markup for this file");
        assert_eq!(found[0].id, markup.id);
    }
}

#[test]
fn records_persist_per_file_and_never_alias_across_workspaces() {
    {
        // An id is an app-level name, not a capability.  Two workspace-scoped
        // sets must not accidentally turn a matching id into shared state.
        let mut workspace_a = AutoCommit::new();
        let mut workspace_b = AutoCommit::new();
        let id = RecordId::minted();
        let kind = RecordKind::new("book");
        index::reference(&mut workspace_a, &id, &kind);
        index::reference(&mut workspace_b, &id, &kind);

        let mut set_a = RecordSet::in_memory().with_home("workspace-a");
        let mut state_a = State::new();
        let want = RecordCheckout::new(id.clone(), kind.clone());
        assert!(set_a.checkout(&mut state_a, Some(&mut workspace_a), &want));
        put_text(&mut state_a.doc, "title", "A only");

        let mut set_b = RecordSet::in_memory().with_home("workspace-b");
        let mut state_b = State::new();
        assert!(set_b.checkout(&mut state_b, Some(&mut workspace_b), &want));
        assert_eq!(text(&state_b.doc, "title"), None);
        put_text(&mut state_b.doc, "title", "B only");

        assert_eq!(
            text(&state_a.doc, "title").as_deref(),
            Some("A only"),
            "workspace B must not alias workspace A's record by id"
        );
    }

    {
        let tmp = tempfile::tempdir().expect("temp dir");
        let documents = native(tmp.path());
        let a = "22222222-2222-4222-8222-222222222222";
        let b = "33333333-3333-4333-8333-333333333333";
        let record = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));

        for (workspace, body) in [(a, "private to a"), (b, "private to b")] {
            let mut set =
                RecordSet::open_in(Rc::clone(&documents), "so.tilo.scoped-records", workspace);
            let mut state = State::new();
            assert!(set.checkout(&mut state, None, &record));
            put_text(&mut state.doc, "body", body);
            set.flush(&mut state.doc).unwrap();
        }

        for (workspace, expected) in [(a, "private to a"), (b, "private to b")] {
            let mut set =
                RecordSet::open_in(Rc::clone(&documents), "so.tilo.scoped-records", workspace);
            let mut state = State::new();
            assert!(set.checkout(&mut state, None, &record));
            assert_eq!(text(&state.doc, "body").as_deref(), Some(expected));
            assert_eq!(workspace_scope(&state.doc).as_deref(), Some(workspace));
        }
    }

    {
        let id = RecordId::minted();
        let kind = RecordKind::new("note");
        let mut remote = AutoCommit::new();
        assert!(init_scoped(
            &mut remote,
            &id,
            &kind,
            "workspace-a",
            Some("workspace-a")
        ));
        put_text(&mut remote, "body", "remote merge");

        assert!(
            !init_scoped(&mut remote, &id, &kind, "workspace-b", Some("workspace-b")),
            "a received document's recorded workspace is authority, not a hint"
        );
        assert_eq!(workspace_scope(&remote).as_deref(), Some("workspace-a"));
    }

    {
        let tmp = tempfile::tempdir().expect("temp dir");
        let documents = native(tmp.path());
        let workspace_id = "11111111-1111-4111-8111-111111111111";

        let a = RecordCheckout::new(
            RecordId::for_file("/work/a.odt"),
            RecordKind::new("document"),
        );
        let b = RecordCheckout::new(
            RecordId::for_file("/work/b.odt"),
            RecordKind::new("document"),
        );

        {
            let mut set =
                RecordSet::open_in(Rc::clone(&documents), "so.tilo.test-records", workspace_id);
            let mut state = State::new();
            let mut workspace = AutoCommit::new();
            // `a` is adopted: content exists before the record is named,
            // as when an app loads a file first.
            put_text(&mut state.doc, "body", "a's body");
            set.checkout(&mut state, Some(&mut workspace), &a);
            set.checkout(&mut state, Some(&mut workspace), &b);
            put_text(&mut state.doc, "body", "b's body");
            set.flush(&mut state.doc).unwrap();
        }

        // A fresh process: same package, same record ids, nothing else shared.
        let mut set =
            RecordSet::open_in(Rc::clone(&documents), "so.tilo.test-records", workspace_id);
        let mut state = State::new();
        let mut workspace = AutoCommit::new();
        set.checkout(&mut state, Some(&mut workspace), &a);
        assert_eq!(
            text(&state.doc, "body").as_deref(),
            Some("a's body"),
            "the adopted record was written to its own file and came back"
        );
        set.checkout(&mut state, Some(&mut workspace), &b);
        assert_eq!(
            text(&state.doc, "body").as_deref(),
            Some("b's body"),
            "and the second record is its own file, not the first's"
        );

        let dir = tmp
            .path()
            .join("so.tilo.test-records")
            .join(app_state::WORKSPACES_DIR)
            .join(workspace_id)
            .join(app_state::DOCUMENTS_DIR);
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("records directory exists")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "one directory per record under {}, got {entries:?}",
            dir.display()
        );
    }
}

#[test]
fn checkout_parks_the_previous_record_or_keeps_the_live_one() {
    {
        // A second record must not clear the first — it's a different
        // document.
        let mut workspace_doc = AutoCommit::new();
        let mut set = RecordSet::in_memory();
        let mut state = State::new();
        let kind = RecordKind::new("annotation-set");
        let a = RecordCheckout::new(RecordId::for_file("/work/a.pdf"), kind.clone());
        let b = RecordCheckout::new(RecordId::for_file("/work/b.pdf"), kind);

        set.checkout(&mut state, Some(&mut workspace_doc), &a);
        put_text(&mut state.doc, "note", "a's note");

        set.checkout(&mut state, Some(&mut workspace_doc), &b);
        assert_eq!(text(&state.doc, "note"), None, "b starts empty");
        put_text(&mut state.doc, "note", "b's note");

        assert_eq!(
            set.peek(&a.id).and_then(|d| text(d, "note")).as_deref(),
            Some("a's note"),
            "a's document is intact while b is live"
        );

        set.checkout(&mut state, Some(&mut workspace_doc), &a);
        assert_eq!(text(&state.doc, "note").as_deref(), Some("a's note"));
    }

    {
        let mut set = RecordSet::in_memory();
        let mut state = State::new();
        let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("document"));
        assert!(set.checkout(&mut state, None, &want));
        put_text(&mut state.doc, "body", "typed");
        assert!(!set.checkout(&mut state, None, &want), "already live");
        assert_eq!(text(&state.doc, "body").as_deref(), Some("typed"));
    }
}

#[test]
fn history_existing_records_reopen_and_refuse_corruption() {
    // Case `history_existing_record_reopens_without_initializing_missing_content`:
    // A record adopted from a live document (the "save as" case) is
    // written out rather than mistaken for something already on disk.
    {
        let tmp = tempfile::tempdir().unwrap();
        let documents = native(tmp.path());
        let checkout = RecordCheckout::new(RecordId::for_tab(401), RecordKind::new("tab"));
        let package = "dev.tilo.history-existing-legacy";
        let workspace = "11111111-1111-4111-8111-111111111111";
        let original_heads = {
            let store = RecordStore::new(RecordSet::open_in(
                Rc::clone(&documents),
                package,
                workspace,
            ));
            store.open_record(&checkout);
            let heads = store
                .with(&checkout.id, |doc| {
                    put_text(doc, "body", "preserved record");
                    doc.get_heads()
                })
                .unwrap();
            store.flush().unwrap();
            heads
        };
        let store = RecordStore::new(RecordSet::open_in(
            Rc::clone(&documents),
            package,
            workspace,
        ));
        assert!(
            store.open_existing_record_checked(&checkout).is_ok(),
            "existing-only presentation must load a saved legacy record"
        );
        store
            .with(&checkout.id, |doc| {
                assert_eq!(doc.get_heads(), original_heads);
                assert_eq!(text(doc, "body").as_deref(), Some("preserved record"));
            })
            .unwrap();
        let missing = RecordCheckout::new(RecordId::for_tab(402), RecordKind::new("tab"));
        assert!(matches!(
            store.open_existing_record_checked(&missing),
            Err(RecordOpenError::MissingRecord)
        ));
        assert!(store.read(&missing.id, |_| ()).is_none());
    }

    {
        let tmp = tempfile::tempdir().unwrap();
        let documents = native(tmp.path());
        let package = "dev.tilo.history-record-refusals";
        let workspace = "11111111-1111-4111-8111-111111111111";
        for (index, payload) in [
            b"not automerge".to_vec(),
            Vec::new(),
            {
                let mut doc = AutoCommit::new();
                init_scoped(
                    &mut doc,
                    &RecordId::for_tab(999),
                    &RecordKind::new("tab"),
                    workspace,
                    Some(workspace),
                );
                doc.save()
            },
            {
                let mut doc = AutoCommit::new();
                init_scoped(
                    &mut doc,
                    &RecordId::for_tab(413),
                    &RecordKind::new("tab"),
                    workspace,
                    Some(workspace),
                );
                let mut bytes = doc.save();
                put_text(&mut doc, "body", "truncated later content");
                let increment = doc.save_incremental();
                assert!(increment.len() > 8);
                bytes.extend_from_slice(&increment[..increment.len() - 3]);
                bytes
            },
        ]
        .into_iter()
        .enumerate()
        {
            let checkout = RecordCheckout::new(
                RecordId::for_tab(410 + index as u64),
                RecordKind::new("tab"),
            );
            let key = checkout.id.storage_key();
            let path = {
                let owner = app_state::DocStore::shared(
                    Rc::clone(&documents),
                    StorageKey::document(package, workspace, &key),
                )
                .unwrap();
                owner.document_path().to_path_buf()
            };
            std::fs::write(&path, &payload).unwrap();
            let store = RecordStore::new(RecordSet::open_in(
                Rc::clone(&documents),
                package,
                workspace,
            ));
            assert!(store.open_existing_record_checked(&checkout).is_err());
            assert!(store.create_record(&checkout).is_err());
            assert!(store.read(&checkout.id, |_| ()).is_none());
            store.tick().unwrap();
            store.flush().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), payload);
        }
    }
}

#[test]
fn absent_private_checkpoint_store_is_empty_but_never_accepts_evidence() {
    let store = RecordStore::in_memory();
    let checkout = RecordCheckout::new(RecordId::for_tab(403), RecordKind::new("tab"));
    store.create_record(&checkout).unwrap();
    assert!(store.private_actions(&checkout.id).unwrap().is_empty());
    let evidence = app_state::PrivateAction {
        id: [1; 16],
        hashes: Vec::new(),
        intent: b"{}".to_vec(),
        receipt: None,
    };
    assert_eq!(
        store.record_private_action(&checkout.id, evidence),
        Err("record has no private durable writer".into())
    );
    let _loan = store.0.borrow_mut();
    assert_eq!(
        store.private_actions(&checkout.id).unwrap_err(),
        "record store is busy"
    );
}

#[test]
fn history_new_record_creation_refuses_reuse_and_reports_busy_loans() {
    {
        let store = RecordStore::in_memory();
        let checkout = RecordCheckout::new(RecordId::for_tab(404), RecordKind::new("tab"));
        store.create_record(&checkout).unwrap();
        let before = store
            .with(&checkout.id, |doc| {
                put_text(doc, "body", "already authored");
                doc.get_heads()
            })
            .unwrap();
        assert!(matches!(
            store.create_record(&checkout),
            Err(RecordOpenError::AlreadyExists)
        ));
        assert_eq!(
            store.with(&checkout.id, AutoCommit::get_heads).unwrap(),
            before
        );
    }

    {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("state");
        let checkout = RecordCheckout::new(RecordId::for_tab(420), RecordKind::new("tab"));
        let heads = {
            let mut owners = RecordOwners::new(root.clone());
            let store = owners.open("dev.tilo.test", "history-workspace").unwrap();
            store.create_record(&checkout).unwrap();
            let heads = store
                .with(&checkout.id, |doc| {
                    put_text(doc, "body", "new canonical content");
                    doc.get_heads()
                })
                .unwrap();
            store.try_flush().unwrap();
            heads
        };
        let mut owners = RecordOwners::new(root);
        let store = owners.open("dev.tilo.test", "history-workspace").unwrap();
        assert!(store.create_record(&checkout).is_err());
        store.open_existing_record_checked(&checkout).unwrap();
        assert_eq!(
            store.with(&checkout.id, AutoCommit::get_heads).unwrap(),
            heads
        );
        assert_eq!(
            store
                .read(&checkout.id, |doc| text(doc, "body"))
                .flatten()
                .as_deref(),
            Some("new canonical content")
        );
    }

    {
        let store = RecordStore::in_memory();
        let existing = RecordCheckout::new(RecordId::for_tab(421), RecordKind::new("tab"));
        let new = RecordCheckout::new(RecordId::for_tab(422), RecordKind::new("tab"));
        store.create_record(&existing).unwrap();
        let result = store
            .with(&existing.id, |_| store.create_record(&new))
            .unwrap();
        assert!(matches!(result, Err(RecordOpenError::Busy)));
        assert!(store.read(&new.id, |_| ()).is_none());
    }

    {
        let tmp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(tmp.path().join("state"));
        let store = owners.open("dev.tilo.test", "read-loan").unwrap();
        let existing = RecordCheckout::new(RecordId::for_tab(423), RecordKind::new("tab"));
        let new = RecordCheckout::new(RecordId::for_tab(424), RecordKind::new("tab"));
        store.create_record(&existing).unwrap();
        let result = store
            .read(&existing.id, |_| store.create_record(&new))
            .unwrap();
        assert!(matches!(result, Err(RecordOpenError::Busy)));
        assert!(store.read(&new.id, |_| ()).is_none());
    }
}

#[test]
fn package_primary_admission_uses_the_primary_identity_rule() {
    let temp = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(temp.path().join("state"));
    let store = owners
        .open("so.tilo.primary-test", "primary-workspace")
        .unwrap();
    let primary = RecordCheckout::package_primary(RecordKind::new("primary"));
    store
        .reserve_record(&primary)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    store.try_open_record(&primary).unwrap();
    // A primary document has no minted id/kind, so `identity` is None;
    // admission must still accept the actual opened primary record.
    let admission = store.admission(&primary).unwrap();
    assert_eq!(
        store.with(&primary.id, |doc| admission.matches(doc, &primary)),
        Some(true)
    );
    // A document claiming a minted identity is not the package primary.
    let minted = RecordCheckout::new(RecordId::minted(), RecordKind::new("primary"));
    let mut foreign = AutoCommit::new();
    assert!(init_scoped(
        &mut foreign,
        &minted.id,
        &minted.kind,
        "primary-workspace",
        Some("primary-workspace")
    ));
    assert!(!admission.matches(&foreign, &primary));
}

/// Every window of a workspace-record app is its own process on the workspace's
/// one notebook, and a relaunch overlaps the instance still exiting: the
/// workspace's own document opens for all of them at once, and again after
/// the first closes, instead of failing on the writer's gate.
#[test]
fn the_workspace_primary_opens_in_a_second_window_and_across_a_quick_relaunch() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let primary = RecordCheckout::package_primary(RecordKind::new("primary"));

    let mut first_owners = RecordOwners::new(root.clone());
    let first = first_owners
        .open("so.tilo.shared-test", "shared-workspace")
        .unwrap();
    first
        .reserve_record(&primary)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    first.try_open_record(&primary).unwrap();

    // A second window: its own process, so its own owners and store.
    let mut second_owners = RecordOwners::new(root.clone());
    let second = second_owners
        .open("so.tilo.shared-test", "shared-workspace")
        .unwrap();
    // The first window's creation is on disk after its debounce window;
    // until then there is nothing to open, which is not a lock failure.
    tilo_test_env::wait_until(
        "the second window opens the shared notebook",
        || match second.try_open_record(&primary) {
            Ok(_) => true,
            Err(RecordOpenError::MissingRecord) => false,
            Err(other) => panic!("a second window must share the notebook: {other}"),
        },
    );

    // Close both and reopen at once, with the previous holders still
    // being torn down.
    let mut relaunch_owners = RecordOwners::new(root.clone());
    let relaunched = relaunch_owners
        .open("so.tilo.shared-test", "shared-workspace")
        .unwrap();
    drop(first);
    relaunched
        .try_open_record(&primary)
        .expect("a relaunch overlapping the exiting window opens");
    drop(second);
    drop(relaunched);
    let mut again = RecordOwners::new(root);
    again
        .open("so.tilo.shared-test", "shared-workspace")
        .unwrap()
        .try_open_record(&primary)
        .expect("a reopen after everything closed opens");
}

/// A record the workspace's people share — a person's own state of an app,
/// which the app's window writes and the session's own parts do too — is held
/// by every writer of the workspace at once, as the workspace's document is.
/// A record of one window's own keeps its one owner.
#[test]
fn a_shared_workspace_record_opens_beside_another_writer_and_an_ordinary_one_does_not() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let shared = RecordCheckout::new(
        RecordId::parse(&collab_types::workspace_record_id("tilo.workspace.emoji")).unwrap(),
        RecordKind::new("tilo.workspace.emoji"),
    );
    let ordinary = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));

    let mut first_owners = RecordOwners::new(root.clone());
    let first = first_owners
        .open("so.tilo.chat", "shared-workspace")
        .unwrap();
    for want in [&shared, &ordinary] {
        first
            .reserve_record(want)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        first.try_open_record(want).unwrap();
    }

    // A second writer of the workspace: its own process, so its own owners
    // and store. It joins the shared record and is refused the ordinary one.
    let mut second_owners = RecordOwners::new(root.clone());
    let second = second_owners
        .open("so.tilo.chat", "shared-workspace")
        .unwrap();
    tilo_test_env::wait_until("the second writer opens the shared record", || match second
        .try_open_record(&shared)
    {
        Ok(_) => true,
        Err(RecordOpenError::MissingRecord) => false,
        Err(other) => panic!("a second writer must share the record: {other}"),
    });
    assert!(
        matches!(
            second.try_open_record(&ordinary),
            Err(RecordOpenError::Store(
                app_state::StoreError::WriterBusy { .. }
            ))
        ),
        "a record of one window's own keeps its one owner"
    );
}

#[test]
fn strict_generic_shape_guard_preserves_valid_package_primary() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let mut owners = RecordOwners::new(state.clone());
    let store = owners
        .open("dev.tilo.primary-test", "primary-workspace")
        .unwrap();
    let primary = RecordCheckout::package_primary(RecordKind::new("primary"));
    store
        .reserve_record(&primary)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    store.try_flush().unwrap();
    assert!(store.try_open_record(&primary).is_ok());
    let memory = RecordStore::in_memory();
    memory.create_record(&primary).unwrap();

    let malformed = RecordCheckout::package_primary(RecordKind::new("1invalid"));
    assert!(matches!(
        store.reserve_record(&malformed),
        Err(RecordOpenError::IdentityMismatch)
    ));
    let malformed = RecordCheckout::package_primary(RecordKind::new("primary")).referencing(
        RecordCheckout::new(RecordId::for_tab(1), RecordKind::new("target")),
        "bad relation",
    );
    assert!(matches!(
        store.reserve_record(&malformed),
        Err(RecordOpenError::IdentityMismatch)
    ));
}

#[test]
fn committed_initial_checkout_preserves_original_history_before_local_initialization() {
    let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("document"));
    let mut actual = AutoCommit::new();
    assert!(init_scoped(
        &mut actual,
        &want.id,
        &want.kind,
        "native-workspace",
        Some("native-workspace")
    ));
    put_text(&mut actual, "body", "actual committed content");
    actual.commit();
    let heads = actual.get_heads();
    let bytes = actual.save();
    let mut set = RecordSet::in_memory().with_home("native-workspace");
    let mut state = State::new();
    assert!(set.checkout_with_initial(
        &mut state,
        None,
        &want,
        &collab_types::DocumentMutation::Snapshot(bytes)
    ));
    assert_eq!(
        state.doc.get_heads(),
        heads,
        "no empty/default graph before actual history"
    );
    assert_eq!(
        text(&state.doc, "body").as_deref(),
        Some("actual committed content")
    );
}

#[test]
fn committed_initial_checkout_does_not_finish_or_discard_local_work() {
    let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("document"));
    let mut actual = AutoCommit::new();
    assert!(init_scoped(
        &mut actual,
        &want.id,
        &want.kind,
        "native-workspace",
        Some("native-workspace")
    ));
    actual.commit();
    let mut set = RecordSet::in_memory().with_home("native-workspace");
    let mut state = State::new();
    state.doc.put(ROOT, "unfinished", "keep").unwrap();
    let pending = state.doc.pending_ops();
    assert!(!set.checkout_with_initial(
        &mut state,
        None,
        &want,
        &collab_types::DocumentMutation::Snapshot(actual.save())
    ));
    assert_eq!(state.doc.pending_ops(), pending);
    assert!(set.active().is_none());
}

/// A store that takes reads and refuses writes once told to, so a flush
/// has a failure of the store's own to report.
struct RefusingStore {
    inner: durable_store::MemoryStore,
    refuse: std::cell::Cell<bool>,
}

impl RefusingStore {
    fn new() -> Self {
        Self {
            inner: durable_store::MemoryStore::new(),
            refuse: std::cell::Cell::new(false),
        }
    }
}

impl durable_store::DurableStore for RefusingStore {
    fn namespace(&self) -> &str {
        self.inner.namespace()
    }
    fn read(
        &self,
        key: &StorageKey,
    ) -> Result<Option<durable_store::Stored>, durable_store::StoreError> {
        self.inner.read(key)
    }
    fn write(
        &self,
        key: &StorageKey,
        op: durable_store::WriteOp,
        writer: Option<&[u8]>,
    ) -> Result<durable_store::Revision, durable_store::StoreError> {
        if self.refuse.get() {
            return Err(durable_store::StoreError::Failed {
                context: "refusing store".into(),
                message: "the disk is full".into(),
            });
        }
        self.inner.write(key, op, writer)
    }
    fn durability(
        &self,
        key: &StorageKey,
        revision: durable_store::Revision,
    ) -> durable_store::Durability {
        self.inner.durability(key, revision)
    }
    fn flush(&self, key: Option<&StorageKey>) -> Result<(), durable_store::StoreError> {
        self.inner.flush(key)
    }
    fn wake_when_settled(
        &self,
        key: &StorageKey,
        wake: durable_store::ReadyWake,
    ) -> durable_store::SettleWatch {
        self.inner.wake_when_settled(key, wake)
    }
    fn lease(&self, key: &StorageKey) -> Result<durable_store::Lease, durable_store::StoreError> {
        self.inner.lease(key)
    }
    fn lease_shared(
        &self,
        key: &StorageKey,
    ) -> Result<durable_store::SharedLease, durable_store::StoreError> {
        self.inner.lease_shared(key)
    }
    fn claim(&self, key: &StorageKey, claimant: &str) -> Result<bool, durable_store::StoreError> {
        self.inner.claim(key, claimant)
    }
}

/// A save the store refused reaches whoever flushes the record, naming
/// it, rather than a log line the caller never sees.
#[test]
fn a_failed_record_save_reaches_the_caller() {
    let documents = Rc::new(RefusingStore::new());
    let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("document"));
    let store = RecordStore::new(RecordSet::open_in(
        Rc::clone(&documents) as Rc<dyn DurableStore>,
        "dev.tilo.refusing-store",
        "11111111-1111-4111-8111-111111111111",
    ));
    assert!(store.open_record(&checkout));
    store
        .with(&checkout.id, |doc| put_text(doc, "body", "typed"))
        .unwrap();
    assert_eq!(store.flush(), Ok(()), "the store took the first save");

    documents.refuse.set(true);
    store
        .with(&checkout.id, |doc| put_text(doc, "body", "typed again"))
        .unwrap();
    let error = store.flush().expect_err("the store refused the save");
    assert!(
        error.contains(&checkout.id.to_string()),
        "the failure names the record: {error}"
    );
    assert!(error.contains("the disk is full"), "{error}");
}

/// A record the workspace's index names is one the store lists with the name
/// the index gives it, though no window of this device started it: what a
/// window on another device, or an agent in a hosted workspace, published
/// here is reached again.
#[test]
fn a_record_the_index_names_is_listed_with_its_name_though_no_window_started_it() {
    let tmp = tempfile::tempdir().expect("a store");
    let documents = native(tmp.path());
    let (package, workspace) = ("so.tilo.pdf-viewer", "22222222-2222-4222-8222-222222222222");
    let kind = RecordKind::new("markup");
    let started = RecordCheckout::new(RecordId::window_default(7), kind.clone());
    let published = RecordCheckout::new(
        RecordId::derived("annotations", &RecordId::minted()),
        kind.clone(),
    );
    // A window of this device started one of them…
    {
        let store = RecordStore::new(RecordSet::open_in(
            Rc::clone(&documents),
            package,
            workspace,
        ));
        store.open_record(&started);
        store.flush().expect("saved");
    }
    // …and the workspace holds another a window elsewhere made, which the
    // workspace's own document names as the reader calls it.
    let mut published_doc = AutoCommit::new();
    assert!(init_scoped(
        &mut published_doc,
        &published.id,
        &kind,
        workspace,
        Some(workspace)
    ));
    let _landing =
        transfer::install(&documents, package, workspace, &mut published_doc).expect("kept");
    let mut index_doc = AutoCommit::new();
    assert!(index::reference(&mut index_doc, &published.id, &kind));
    assert!(index::set_name(
        &mut index_doc,
        &published.id,
        Some("application.pdf")
    ));
    let _landing =
        transfer::install_workspace_document(&documents, package, workspace, &mut index_doc)
            .expect("the workspace's index is kept");

    let store = RecordStore::new(RecordSet::open_in(
        Rc::clone(&documents),
        package,
        workspace,
    ));
    let listed = store.workspace_records(&kind);
    assert_eq!(
        listed
            .iter()
            .map(|(id, _, name)| (id.clone(), name.clone()))
            .collect::<Vec<_>>(),
        [
            (published.id.clone(), Some("application.pdf".to_owned())),
            (started.id.clone(), None),
        ],
        "both are listed, and the index's name comes with the one it names"
    );
    assert!(
        store
            .window_records(&kind)
            .iter()
            .all(|(id, _)| id == &started.id),
        "a name in the index does not make a record one a window started"
    );
}
