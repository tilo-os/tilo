//! Moving and copying resources between workspaces, through the stores a window opens them in.

use crate::document::fields::meta_ro;
use crate::document::retirement::RETIRED;
use crate::resource::transfer::*;
use crate::resource::{WORKSPACE, meta_obj};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ReadDoc, Value};
use durable_store::StorageKey;
use std::rc::Rc;
use tilo_automerge::ApplyIncoming as _;

use crate::resource::{DocumentCheckout, DocumentId, ResourceOpenError, ResourceType, identity};
use crate::resource::{ResourceOwners, ResourceStore, str_at, workspace_scope};
use automerge::ROOT;

const PACKAGE: &str = "so.tilo.transfer-test";
const FROM: &str = "3f2b1c00-0000-4000-8000-00000000000a";
const TO: &str = "3f2b1c00-0000-4000-8000-00000000000b";

fn store(root: &std::path::Path) -> Rc<dyn DurableStore> {
    Rc::new(app_state::FsStore::new(root.to_path_buf()))
}

fn body(doc: &AutoCommit) -> Option<String> {
    match doc.get(ROOT, "body") {
        Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

fn moved() -> Retirement {
    Retirement::Moved(Some(Successor {
        workspace: TO.into(),
        document: "uuid:successor".into(),
        by: "ada".into(),
    }))
}

tilo_test_env::scenario! {
    fn a_retirement_is_one_change_that_names_its_successor;

    phase a_retirement_travels_as_one_change_and_is_read_back_whole;
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "body", "draft").unwrap();
        doc.commit();
        let before = doc.get_heads();
        stamp_retired(&mut doc, &moved()).unwrap();
        assert_eq!(retirement(&doc), Some(moved()));
        assert_eq!(doc.get_changes(&before).len(), 1);
        let mut peer = doc.fork_at(&before).unwrap();
        assert!(!is_retired(&peer));
        peer.merge_incoming(&mut doc).unwrap();
        assert_eq!(
            retirement(&peer),
            Some(moved()),
            "a peer holding it learns it moved"
        );
        stamp_retired(&mut doc, &Retirement::Abandoned).unwrap();
        assert_eq!(
            retirement(&doc),
            Some(moved()),
            "the first retirement stands"
        );
    }

    phase a_retirement_names_its_successor_under_the_stored_key;
    // The successor goes into the stamp under the workspace key and reads back.
    {
        let mut doc = AutoCommit::new();
        stamp_retired(&mut doc, &moved()).unwrap();
        let meta = meta_ro(&doc).expect("a stamped record has meta");
        let Some((Value::Object(ObjType::Map), retired)) = doc.get(&meta, RETIRED).unwrap() else {
            panic!("the retirement is a map");
        };
        assert!(
            doc.get(&retired, "workspace").unwrap().is_some(),
            "stored key"
        );
        assert_eq!(retirement(&doc), Some(moved()));
    }
}

fn seeded(root: &std::path::Path) -> (DocumentCheckout, ResourceStore) {
    let checkout = DocumentCheckout::new(
        DocumentId::derived("default", &DocumentId::for_tab(0)),
        ResourceType::new("transfer.test"),
    );
    let records = ResourceOwners::new(root.to_path_buf())
        .open(PACKAGE, FROM)
        .unwrap();
    records.create_resource(&checkout).unwrap();
    records
        .with(&checkout.id, |doc| {
            doc.put(ROOT, "body", "first draft").unwrap();
        })
        .unwrap();
    records.try_flush().unwrap();
    (checkout, records)
}

tilo_test_env::scenario! {
    fn a_moved_record_keeps_its_history_and_names_its_new_workspace;

    phase a_moved_record_keeps_its_history_and_later_edits_and_leaves_its_workspace;
    {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let (checkout, old_writer) = seeded(temp.path());
        let origin = Origin::Record(checkout.clone());

        // The old app still holds the resource: reading it needs no writer.
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        let known = snapshot.clone().get_heads();
        let target = DocumentId::minted();
        let mut arrived = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        let (installed, _) = install(&store, PACKAGE, TO, &mut arrived).unwrap();
        assert_eq!(installed.id, target);
        assert_eq!(provenance(&arrived), std::slice::from_ref(&checkout.id));
        assert!(
            arrived.has_change(&known[0]),
            "the source history travels"
        );

        // It edits once more after the read, then exits.
        old_writer
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "first draft, last words").unwrap();
            })
            .unwrap();
        old_writer.try_flush().unwrap();
        assert!(matches!(
            retire(&store, PACKAGE, FROM, &origin, &moved()),
            Err(TransferError::Busy)
        ));
        drop(old_writer);

        let last = read(&store, PACKAGE, FROM, &origin).unwrap();
        assert_eq!(changes_since(&last, &known), 1);
        assert_eq!(
            catch_up(&store, PACKAGE, TO, &installed, &last).unwrap().0,
            1
        );
        assert_eq!(
            catch_up(&store, PACKAGE, TO, &installed, &last).unwrap(),
            (0, None),
            "nothing more to write"
        );
        let _ = retire(&store, PACKAGE, FROM, &origin, &moved()).unwrap();

        let destination = read(&store, PACKAGE, TO, &Origin::Record(installed)).unwrap();
        assert_eq!(
            body(&destination).as_deref(),
            Some("first draft, last words")
        );
        assert_eq!(workspace_scope(&destination).as_deref(), Some(TO));
        assert!(matches!(
            read(&store, PACKAGE, FROM, &origin),
            Err(TransferError::Open(ResourceOpenError::Retired))
        ));
        let source_workspace = ResourceOwners::new(temp.path().to_path_buf())
            .open(PACKAGE, FROM)
            .unwrap();
        assert!(matches!(
            source_workspace.try_open_resource(&checkout),
            Err(ResourceOpenError::Retired)
        ));
        let retired = decode(&store, &key(PACKAGE, FROM, &checkout.id).unwrap()).unwrap();
        assert_eq!(retirement(&retired), Some(moved()));
        assert_eq!(
            body(&retired).as_deref(),
            Some("first draft, last words"),
            "the retirement is one more change, not an erasure"
        );
    }
}

tilo_test_env::scenario! {
    fn an_arriving_document_is_kept_only_as_the_record_it_claims;

    phase a_document_joins_another_workspace_only_as_exactly_the_minted_record_it_claims;
    {
        let dir = tempfile::tempdir().unwrap();
        let (checkout, writer) = seeded(dir.path());
        drop(writer);
        let source = read(
            &store(dir.path()),
            PACKAGE,
            FROM,
            &Origin::Record(checkout.clone()),
        )
        .unwrap();
        let target = DocumentId::minted();
        let arriving = |doc: &AutoCommit| doc.clone().save();
        let mut rehomed = rehome(
            &source,
            &Origin::Record(checkout.clone()),
            FROM,
            TO,
            &target,
        )
        .unwrap();
        let bytes = arriving(&rehomed);
        let (kept, identity) = validate_import(&bytes, TO, &target, bytes.len()).unwrap();
        assert_eq!(identity.id, target);
        assert_eq!(body(&kept).as_deref(), Some("first draft"));
        assert_eq!(provenance(&kept), std::slice::from_ref(&checkout.id));

        assert!(
            validate_import(&bytes, TO, &target, bytes.len() - 1).is_err(),
            "size is bounded"
        );
        assert!(
            validate_import(&bytes, FROM, &target, bytes.len()).is_err(),
            "scoped to the workspace it joins"
        );
        assert!(
            validate_import(&bytes, TO, &DocumentId::minted(), bytes.len()).is_err(),
            "it is only the resource it claims"
        );
        let derived = DocumentId::derived("notes", &target);
        let under_derived = rehome(
            &source,
            &Origin::Record(checkout.clone()),
            FROM,
            TO,
            &derived,
        )
        .unwrap();
        let derived_bytes = arriving(&under_derived);
        assert!(
            validate_import(&derived_bytes, TO, &derived, derived_bytes.len()).is_err(),
            "only a freshly minted id joins a workspace"
        );

        let mut referring = rehomed.clone();
        assert!(crate::resource::add_reference(
            &mut referring,
            &crate::resource::DocumentRef {
                target: DocumentId::minted(),
                relation: "cites".into(),
            }
        ));
        let referring = arriving(&referring);
        assert!(
            validate_import(&referring, TO, &target, referring.len()).is_err(),
            "no identities of other resources ride along"
        );
        let mut indexing = rehomed.clone();
        indexing
            .put_object(ROOT, crate::resource::INDEX, ObjType::Map)
            .unwrap();
        let indexing = arriving(&indexing);
        assert!(validate_import(&indexing, TO, &target, indexing.len()).is_err());
        let meta = meta_obj(&mut rehomed).unwrap();
        rehomed.put(&meta, "alias", "uuid:other").unwrap();
        let aliased = arriving(&rehomed);
        assert!(validate_import(&aliased, TO, &target, aliased.len()).is_err());
        let mut retired = read(
            &store(dir.path()),
            PACKAGE,
            FROM,
            &Origin::Record(checkout.clone()),
        )
        .unwrap();
        let mut retired = rehome(
            &retired.fork(),
            &Origin::Record(checkout),
            FROM,
            TO,
            &target,
        )
        .unwrap();
        stamp_retired(&mut retired, &moved()).unwrap();
        let retired = arriving(&retired);
        assert!(validate_import(&retired, TO, &target, retired.len()).is_err());
    }

    phase a_document_imports_with_its_workspace;
    // A resource's workspace is imported as the document names it. A key the import does not know
    // is refused.
    {
        let kind = ResourceType::new("transfer.test");
        {
            let key = WORKSPACE;
            let target = DocumentId::minted();
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "body", "a draft").unwrap();
            assert!(crate::resource::init_scoped(
                &mut doc, &target, &kind, FROM, None
            ));
            let meta = meta_obj(&mut doc).unwrap();
            doc.put(&meta, key, FROM).unwrap();
            doc.commit();
            let bytes = doc.save();
            let (mut kept, identity) = validate_import(&bytes, FROM, &target, bytes.len()).unwrap();
            assert_eq!(identity.id, target, "{key}");
            assert_eq!(workspace_scope(&kept).as_deref(), Some(FROM), "{key}");
            let kept_meta = meta_obj(&mut kept).unwrap();
            assert_eq!(
                str_at(&kept, &kept_meta, key).as_deref(),
                Some(FROM),
                "the document keeps the key it named its workspace under"
            );
            kept.put(&kept_meta, "alias", "uuid:other").unwrap();
            let aliased = kept.save();
            assert!(
                validate_import(&aliased, FROM, &target, aliased.len()).is_err(),
                "a meta key it does not know is refused"
            );
        }
    }

    phase a_record_from_another_device_is_kept_only_as_the_live_record_it_claims_to_be;
    {
        let holder_dir = tempfile::tempdir().unwrap();
        let holder = store(holder_dir.path());
        let (checkout, writer) = seeded(holder_dir.path());
        drop(writer);
        let bytes = read_by_id(&holder, PACKAGE, FROM, &checkout.id)
            .unwrap()
            .save();

        let here_dir = tempfile::tempdir().unwrap();
        let here = store(here_dir.path());
        assert!(
            adopt_fetched(&here, PACKAGE, FROM, &DocumentId::minted(), &bytes).is_err(),
            "bytes offered as another resource are refused"
        );
        assert!(
            adopt_fetched(&here, PACKAGE, TO, &checkout.id, &bytes).is_err(),
            "a resource of one workspace is never kept as another workspace's"
        );
        assert!(adopt_fetched(&here, PACKAGE, FROM, &checkout.id, b"not a document").is_err());
        let (kept, _) = adopt_fetched(&here, PACKAGE, FROM, &checkout.id, &bytes).unwrap();
        assert_eq!(kept, checkout);
        assert_eq!(
            adopt_fetched(&here, PACKAGE, FROM, &checkout.id, &bytes)
                .unwrap()
                .0,
            checkout,
            "holding it already merges rather than refuses"
        );
        let doc = read_by_id(&here, PACKAGE, FROM, &checkout.id).unwrap();
        assert_eq!(body(&doc).as_deref(), Some("first draft"));

        let mut retired = doc.clone();
        stamp_retired(&mut retired, &moved()).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(
            adopt_fetched(
                &store(elsewhere.path()),
                PACKAGE,
                FROM,
                &checkout.id,
                &retired.save()
            )
            .is_err(),
            "a resource that left its workspace is never brought back to life"
        );
    }

    phase a_transfer_on_the_frame_loop_starts_its_write_and_reports_when_it_lands;
    // On the frame loop a transfer neither reads the disk nor waits for a
    // commit: it is told to come back while the store's own thread opens
    // the document, and then only starts its write. Until the write lands
    // the document stays the store's, so no other process takes it or
    // reads it half-written, and the landing says when the next one may.
    {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let (checkout, writer) = seeded(temp.path());
        drop(writer);
        let origin = Origin::Record(checkout.clone());
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        let mut arrived = rehome(&snapshot, &origin, FROM, TO, &DocumentId::minted()).unwrap();
        let (installed, landing) = {
            let _loop = durable_store::waiting::forbid_strictly_on_this_thread();
            tilo_test_env::wait_for("the store's thread to open the record", || {
                match install(&store, PACKAGE, TO, &mut arrived) {
                    Ok(installed) => Some(installed),
                    Err(error) if error.is_opening() => None,
                    Err(error) => panic!("installing on the frame loop: {error}"),
                }
            })
        };
        let key = key(PACKAGE, TO, &installed.id).unwrap();
        let other_process = app_state::FsStore::new(temp.path());
        if other_process.lease(&key).is_ok() {
            assert_eq!(
                landing.durability(store.as_ref()),
                Durability::Durable,
                "the document is free only once the write is on the disk"
            );
        }
        store.flush(Some(&key)).unwrap();
        assert_eq!(landing.durability(store.as_ref()), Durability::Durable);
        assert_eq!(
            Landing::all(store.as_ref(), std::slice::from_ref(&landing)),
            Durability::Durable
        );
        let other_process: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(temp.path()));
        let there = read(&other_process, PACKAGE, TO, &Origin::Record(installed)).unwrap();
        assert_eq!(body(&there).as_deref(), Some("first draft"));
    }

    phase a_transfer_never_overwrites_or_crosses_into_its_own_workspace;
    {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let (checkout, writer) = seeded(temp.path());
        drop(writer);
        let origin = Origin::Record(checkout.clone());
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        assert!(matches!(
            rehome(&snapshot, &origin, FROM, FROM, &DocumentId::minted()),
            Err(TransferError::NotTransferable(_))
        ));
        assert!(
            rehome(&snapshot, &origin, TO, FROM, &DocumentId::minted()).is_err(),
            "a resource is only read from the workspace it is scoped to"
        );
        let target = DocumentId::minted();
        let mut first = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        let _ = install(&store, PACKAGE, TO, &mut first).unwrap();
        let mut again = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        assert!(matches!(
            install(&store, PACKAGE, TO, &mut again),
            Err(TransferError::Open(ResourceOpenError::AlreadyExists))
        ));
        let foreign = Origin::Record(DocumentCheckout::new(target, checkout.kind.clone()));
        assert!(
            read(&store, PACKAGE, FROM, &foreign).is_err(),
            "a destination id means nothing in the source workspace"
        );
    }

    phase an_app_document_arrives_as_a_record_and_its_workspace_keeps_a_retired_one;
    {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let mut app =
            app_state::DocStore::shared(Rc::clone(&store), StorageKey::app(PACKAGE)).unwrap();
        let mut doc = app.load_or_new();
        doc.put(ROOT, "body", "kept in the app document").unwrap();
        app.save(&mut doc).unwrap();

        let origin = Origin::AppDocument(ResourceType::new("tilo.app-document"));
        let snapshot = read(&store, PACKAGE, FROM, &origin).unwrap();
        let target = DocumentId::minted();
        let mut arrived = rehome(&snapshot, &origin, FROM, TO, &target).unwrap();
        let (installed, _) = install(&store, PACKAGE, TO, &mut arrived).unwrap();
        assert_eq!(installed.kind.as_str(), "tilo.app-document");
        let destination = read(&store, PACKAGE, TO, &Origin::Record(installed)).unwrap();
        assert_eq!(
            body(&destination).as_deref(),
            Some("kept in the app document")
        );
        assert_eq!(crate::resource::home(&destination).as_deref(), Some(FROM));

        assert!(
            matches!(
                retire(&store, PACKAGE, FROM, &origin, &moved()),
                Err(TransferError::Busy)
            ),
            "not while the app still writes it"
        );
        drop(app);
        let _ = retire(&store, PACKAGE, FROM, &origin, &moved()).unwrap();
        assert!(matches!(
            read(&store, PACKAGE, FROM, &origin),
            Err(TransferError::Open(ResourceOpenError::Retired))
        ));
    }
}
tilo_test_env::scenario! {
    fn a_branch_keeps_the_record_under_the_branchs_name;

    phase a_branch_keeps_the_record_and_its_history_and_names_the_branch;
    // A branch's copy keeps the resource's id, where it was minted and its whole history, names the
    // branch as its workspace, and writes as a writer of its own: the source and the copy go on
    // apart.
    {
        let id = DocumentId::minted();
        let mut source = AutoCommit::new();
        assert!(crate::resource::init_scoped(
            &mut source,
            &id,
            &ResourceType::new("transfer.test"),
            FROM,
            Some(FROM),
        ));
        source.put(ROOT, "body", "plan").unwrap();
        source.commit();
        let before = source.get_heads();
        let mut copy = branch(&source, TO).unwrap();
        assert_eq!(workspace_scope(&copy).as_deref(), Some(TO));
        assert_eq!(
            workspace_scope(&source).as_deref(),
            Some(FROM),
            "the source stays"
        );
        assert_eq!(identity(&copy).map(|checkout| checkout.id), Some(id));
        assert_eq!(crate::resource::home(&copy).as_deref(), Some(FROM));
        assert_eq!(body(&copy).as_deref(), Some("plan"));
        assert!(
            copy.get_changes(&[]).len() > source.get_changes(&[]).len(),
            "the copy holds the source's history and the change that names the branch"
        );
        assert!(copy.has_change(&before[0]));
        assert_ne!(copy.get_actor(), source.get_actor(), "a writer of its own");
        copy.put(ROOT, "body", "branch").unwrap();
        copy.commit();
        source.put(ROOT, "body", "source").unwrap();
        source.commit();
        assert_eq!(body(&copy).as_deref(), Some("branch"));
        assert_eq!(body(&source).as_deref(), Some("source"));
        // A document no workspace scopes is forked as it is.
        let mut plain = AutoCommit::new();
        plain.put(ROOT, "body", "loose").unwrap();
        plain.commit();
        let mut forked = branch(&plain, TO).unwrap();
        assert_eq!(forked.get_heads(), plain.get_heads());
        assert!(branch(&plain, "").is_err());
    }
}
