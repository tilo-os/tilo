use super::*;
use crate::state::State;
use std::path::Path;

fn native(root: &Path) -> Rc<dyn DurableStore> {
    Rc::new(app_state::FsStore::new(root))
}

fn put_text(doc: &mut AutoCommit, key: &str, value: &str) {
    doc.put(ROOT, key, value).unwrap();
}

fn text(doc: &AutoCommit, key: &str) -> Option<String> {
    str_at(doc, &ROOT, key)
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("write test file");
}

#[test]
fn file_identity_follows_the_file_not_its_path_or_bytes() {
    // phase: a_file_keeps_its_identity_through_rename_and_move
    {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("report.pdf");
        write(&first, b"%PDF-1.7 report");
        let id = DocumentId::identify_file(&first);
        assert!(
            id.file_document().is_some(),
            "a local file carries a durable id: {id}"
        );
        assert_eq!(DocumentId::parse(id.as_str()), Some(id.clone()));
        assert_eq!(
            DocumentId::identify_file(&first),
            id,
            "reading it again is stable"
        );

        let renamed = dir.path().join("report-final.pdf");
        std::fs::rename(&first, &renamed).unwrap();
        assert_eq!(
            DocumentId::identify_file(&renamed),
            id,
            "rename keeps identity"
        );

        let elsewhere = dir.path().join("archive");
        std::fs::create_dir(&elsewhere).unwrap();
        let moved = elsewhere.join("2026.pdf");
        std::fs::rename(&renamed, &moved).unwrap();
        assert_eq!(DocumentId::identify_file(&moved), id, "move keeps identity");
    }
    // phase: a_copy_or_identical_bytes_is_a_different_file
    {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("a.pdf");
        write(&original, b"%PDF-1.7 same bytes");
        let id = DocumentId::identify_file(&original);

        let copy = dir.path().join("a copy.pdf");
        std::fs::copy(&original, &copy).unwrap();
        let copy_id = DocumentId::identify_file(&copy);
        assert!(copy_id.file_document().is_some());
        assert_ne!(copy_id, id, "a copy starts its own records");

        let twin = dir.path().join("b.pdf");
        write(&twin, b"%PDF-1.7 same bytes");
        let twin_id = DocumentId::identify_file(&twin);
        assert_ne!(twin_id, id, "identical bytes never share an identity");
        assert_ne!(twin_id, copy_id);
    }
    // phase: the_filesystems_record_id_is_used_verbatim
    // The collaborative filesystem serves its id bare; it exists on Linux.
    #[cfg(target_os = "linux")]
    {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shared.pdf");
        write(&file, b"%PDF-1.7 synced");
        let fs_id = uuid::Uuid::new_v4();
        let path = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).unwrap();
        let name = std::ffi::CString::new(collab_types::document::DOCUMENT_ID_XATTR).unwrap();
        // SAFETY: NUL-terminated strings and a 16-byte value.
        let rc = unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr(),
                fs_id.as_bytes().as_ptr().cast(),
                16,
                0,
            )
        };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
        assert_eq!(
            DocumentId::identify_file(&file),
            DocumentId::for_file_document(fs_id)
        );
        assert_eq!(
            DocumentId::identify_file(&file).file_document(),
            Some(fs_id)
        );
    }
    // phase: a_file_that_cannot_carry_an_id_is_identified_by_path
    {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-yet-saved.pdf");
        assert_eq!(
            DocumentId::identify_file(&missing),
            DocumentId::for_file(&missing)
        );
        assert!(!missing.exists(), "identifying never creates the file");
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(
            DocumentId::identify_file(&folder),
            DocumentId::for_file(&folder)
        );
    }
    // phase: file_record_ids_are_canonical_and_nest_in_derived_ids
    {
        let raw = uuid::Uuid::new_v4();
        let id = DocumentId::for_file_document(raw);
        let derived = DocumentId::derived("annotations", &id);
        assert_eq!(DocumentId::parse(derived.as_str()), Some(derived.clone()));
        let upper = format!(
            "file-document:{}",
            raw.hyphenated().to_string().to_uppercase()
        );
        assert!(DocumentId::parse(&upper).is_none(), "one spelling per file");
        assert!(DocumentId::parse(&format!("file-document:{}", uuid::Uuid::nil())).is_none());
        assert!(DocumentId::parse("file-document:not-a-uuid").is_none());
        assert_eq!(id.file_path(), None);
    }
}

fn markup_checkouts(path: &str, file: &DocumentId) -> (DocumentCheckout, DocumentCheckout) {
    let by_path = DocumentId::for_file(path);
    let old = DocumentCheckout::new(
        DocumentId::derived("annotations", &by_path),
        ResourceType::new("annotation-set"),
    )
    .referencing(
        DocumentCheckout::new(by_path.clone(), ResourceType::new("file")),
        "marks-up",
    );
    let new = DocumentCheckout::new(
        DocumentId::derived("annotations", file),
        ResourceType::new("annotation-set"),
    )
    .formerly(old.id.clone())
    .referencing(
        DocumentCheckout::new(file.clone(), ResourceType::new("file")).formerly(by_path),
        "marks-up",
    );
    (old, new)
}

fn stored_bytes(root: &Path, package: &str, workspace: &str, id: &DocumentId) -> Vec<u8> {
    app_state::FsStore::new(root)
        .read(&StorageKey::document(package, workspace, id.storage_key()))
        .unwrap()
        .unwrap()
        .bytes
}

tilo_test_env::scenario! {
    fn former_records_are_validated_and_adopted_exactly_once;

    phase formerly_is_validated;
    {
        let id = DocumentId::minted();
        let checkout = DocumentCheckout::new(id.clone(), ResourceType::new("note")).formerly(id);
        assert!(
            checkout.formerly.is_empty(),
            "a record is not its own predecessor"
        );
        let mut many = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        for _ in 0..=MAX_FORMER_IDS {
            many = many.formerly(DocumentId::minted());
        }
        assert!(!many.is_valid());
    }

    phase a_path_record_is_adopted_once_by_its_file_record;
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let package = "so.tilo.migration";
        let workspace = "migration-workspace";
        let path = "/home/ada/papers/draft.pdf";
        let file = DocumentId::for_file_document(uuid::Uuid::new_v4());
        let (old, new) = markup_checkouts(path, &file);

        {
            let mut owners = ResourceOwners::new(root.clone());
            let store = owners.open(package, workspace).unwrap();
            store.create_resource(&old).unwrap();
            store
                .with(&old.id, |doc| {
                    put_text(doc, "note", "check the proof");
                    add_reference(doc, &DocumentRef::new(DocumentId::for_file(path), "marks-up"));
                })
                .unwrap();
            store.try_flush().unwrap();
        }
        let old_bytes = stored_bytes(&root, package, workspace, &old.id);

        {
            let mut owners = ResourceOwners::new(root.clone());
            let store = owners.open(package, workspace).unwrap();
            assert!(matches!(
                store.try_open_resource(&new),
                Ok(ResourceOpenOutcome::Loaded)
            ));
            store
                .read(&new.id, |doc| {
                    assert_eq!(text(doc, "note").as_deref(), Some("check the proof"));
                    assert_eq!(identity(doc).unwrap().id, new.id, "identity is restamped");
                    assert_eq!(former_identities(doc), vec![old.id.clone()]);
                    let refs = references(doc);
                    assert!(
                        refs.contains(&DocumentRef::new(file.clone(), "marks-up")),
                        "{refs:?}"
                    );
                    assert!(
                        !refs.iter().any(|r| r.target.file_path().is_some()),
                        "the path file reference is replaced: {refs:?}"
                    );
                })
                .unwrap();
            store.try_flush().unwrap();
        }
        assert_eq!(
            stored_bytes(&root, package, workspace, &old.id),
            old_bytes,
            "the former record is only read"
        );

        // Reopening finds it under the new id; a different file later found at
        // the same path names the same former record, and the first successor
        // keeps owning it.
        let mut owners = ResourceOwners::new(root);
        let store = owners.open(package, workspace).unwrap();
        assert!(matches!(
            store.try_open_resource(&new),
            Ok(ResourceOpenOutcome::Loaded)
        ));
        store
            .read(&new.id, |doc| {
                assert_eq!(text(doc, "note").as_deref(), Some("check the proof"));
            })
            .unwrap();
        let other = DocumentId::for_file_document(uuid::Uuid::new_v4());
        let (_, stranger) = markup_checkouts(path, &other);
        assert!(matches!(
            store.try_open_resource(&stranger),
            Err(ResourceOpenError::MissingResource)
        ));
    }

    phase checkout_adopts_a_former_record_once;
    {
        let temp = tempfile::tempdir().unwrap();
        let documents = native(temp.path());
        let package = "so.tilo.migration-launch";
        let workspace = "22222222-2222-4222-8222-222222222222";
        let path = "/home/ada/papers/draft.pdf";
        let file = DocumentId::for_file_document(uuid::Uuid::new_v4());
        let (old, new) = markup_checkouts(path, &file);
        {
            let mut set = ResourceSet::open_in(Rc::clone(&documents), package, workspace);
            let mut state = State::new();
            assert!(set.checkout(&mut state, None, &old));
            put_text(&mut state.doc, "note", "made before files had ids");
            set.flush(&mut state.doc).unwrap();
        }
        {
            let mut set = ResourceSet::open_in(Rc::clone(&documents), package, workspace);
            let mut state = State::new();
            let mut workspace_doc = AutoCommit::new();
            assert!(set.checkout(&mut state, Some(&mut workspace_doc), &new));
            assert_eq!(
                text(&state.doc, "note").as_deref(),
                Some("made before files had ids")
            );
            assert_eq!(identity(&state.doc).unwrap().id, new.id);
            assert!(index::holds(&workspace_doc, &file));
            assert!(!index::holds(&workspace_doc, &DocumentId::for_file(path)));
            set.flush(&mut state.doc).unwrap();
        }
        let store = ResourceStore::new(ResourceSet::open_in(
            Rc::clone(&documents),
            package,
            workspace,
        ));
        store.open_existing_resource_checked(&new).unwrap();
        store
            .read(&new.id, |doc| {
                assert_eq!(
                    text(doc, "note").as_deref(),
                    Some("made before files had ids")
                );
            })
            .unwrap();
        let other = DocumentId::for_file_document(uuid::Uuid::new_v4());
        let (_, stranger) = markup_checkouts(path, &other);
        assert!(matches!(
            store.open_existing_resource_checked(&stranger),
            Err(ResourceOpenError::MissingResource)
        ));
    }

    phase existing_only_admission_adopts_a_former_record;
    {
        let temp = tempfile::tempdir().unwrap();
        let documents = native(temp.path());
        let package = "so.tilo.migration-existing";
        let workspace = "33333333-3333-4333-8333-333333333333";
        let file = DocumentId::for_file_document(uuid::Uuid::new_v4());
        let (old, new) = markup_checkouts("/srv/notes/plan.pdf", &file);
        {
            let store = ResourceStore::new(ResourceSet::open_in(
                Rc::clone(&documents),
                package,
                workspace,
            ));
            store.open_resource(&old);
            store
                .with(&old.id, |doc| put_text(doc, "note", "adopt me"))
                .unwrap();
            store.flush().unwrap();
        }
        let store = ResourceStore::new(ResourceSet::open_in(
            Rc::clone(&documents),
            package,
            workspace,
        ));
        store.open_existing_resource_checked(&new).unwrap();
        store
            .read(&new.id, |doc| {
                assert_eq!(text(doc, "note").as_deref(), Some("adopt me"));
                assert_eq!(identity(doc).unwrap().id, new.id);
            })
            .unwrap();
    }
}

#[test]
fn a_record_has_one_route_and_the_ownership_key_is_none() {
    let primary = DocumentId::package_primary();
    assert_eq!(primary.route(), "");
    assert_eq!(DocumentId::from_route(""), Some(primary.clone()));
    assert_eq!(DocumentId::from_route(primary.as_str()), None);
    let window = DocumentId::workspace_default();
    assert_eq!(window.route(), window.as_str());
    assert_eq!(DocumentId::from_route(window.route()), Some(window));
}
