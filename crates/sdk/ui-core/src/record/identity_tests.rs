use super::*;
use crate::state::State;

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
        let id = RecordId::identify_file(&first);
        assert!(
            id.file_record().is_some(),
            "a local file carries a durable id: {id}"
        );
        assert_eq!(RecordId::parse(id.as_str()), Some(id.clone()));
        assert_eq!(
            RecordId::identify_file(&first),
            id,
            "reading it again is stable"
        );

        let renamed = dir.path().join("report-final.pdf");
        std::fs::rename(&first, &renamed).unwrap();
        assert_eq!(
            RecordId::identify_file(&renamed),
            id,
            "rename keeps identity"
        );

        let elsewhere = dir.path().join("archive");
        std::fs::create_dir(&elsewhere).unwrap();
        let moved = elsewhere.join("2026.pdf");
        std::fs::rename(&renamed, &moved).unwrap();
        assert_eq!(RecordId::identify_file(&moved), id, "move keeps identity");
    }
    // phase: a_copy_or_identical_bytes_is_a_different_file
    {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("a.pdf");
        write(&original, b"%PDF-1.7 same bytes");
        let id = RecordId::identify_file(&original);

        let copy = dir.path().join("a copy.pdf");
        std::fs::copy(&original, &copy).unwrap();
        let copy_id = RecordId::identify_file(&copy);
        assert!(copy_id.file_record().is_some());
        assert_ne!(copy_id, id, "a copy starts its own records");

        let twin = dir.path().join("b.pdf");
        write(&twin, b"%PDF-1.7 same bytes");
        let twin_id = RecordId::identify_file(&twin);
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
        let name = std::ffi::CString::new(file_identity::ATTRIBUTE).unwrap();
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
            RecordId::identify_file(&file),
            RecordId::for_file_record(fs_id)
        );
        assert_eq!(RecordId::identify_file(&file).file_record(), Some(fs_id));
    }
    // phase: a_file_that_cannot_carry_an_id_is_identified_by_path
    {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-yet-saved.pdf");
        assert_eq!(
            RecordId::identify_file(&missing),
            RecordId::for_file(&missing)
        );
        assert!(!missing.exists(), "identifying never creates the file");
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(
            RecordId::identify_file(&folder),
            RecordId::for_file(&folder)
        );
    }
    // phase: file_record_ids_are_canonical_and_nest_in_derived_ids
    {
        let raw = uuid::Uuid::new_v4();
        let id = RecordId::for_file_record(raw);
        let derived = RecordId::derived("annotations", &id);
        assert_eq!(RecordId::parse(derived.as_str()), Some(derived.clone()));
        let upper = format!(
            "file-record:{}",
            raw.hyphenated().to_string().to_uppercase()
        );
        assert!(RecordId::parse(&upper).is_none(), "one spelling per file");
        assert!(RecordId::parse(&format!("file-record:{}", uuid::Uuid::nil())).is_none());
        assert!(RecordId::parse("file-record:not-a-uuid").is_none());
        assert_eq!(id.file_path(), None);
    }
}

fn markup_checkouts(path: &str, file: &RecordId) -> (RecordCheckout, RecordCheckout) {
    let by_path = RecordId::for_file(path);
    let old = RecordCheckout::new(
        RecordId::derived("annotations", &by_path),
        RecordKind::new("annotation-set"),
    )
    .referencing(
        RecordCheckout::new(by_path.clone(), RecordKind::new("file")),
        "marks-up",
    );
    let new = RecordCheckout::new(
        RecordId::derived("annotations", file),
        RecordKind::new("annotation-set"),
    )
    .formerly(old.id.clone())
    .referencing(
        RecordCheckout::new(file.clone(), RecordKind::new("file")).formerly(by_path),
        "marks-up",
    );
    (old, new)
}

fn stored_bytes(root: &Path, package: &str, room: &str, id: &RecordId) -> Vec<u8> {
    app_state::FsStore::new(root)
        .read(&StorageKey::record(package, room, id.storage_key()))
        .unwrap()
        .unwrap()
        .bytes
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn former_records_are_validated_and_adopted_exactly_once() {
    // phase: formerly_is_validated
    {
        let id = RecordId::minted();
        let checkout = RecordCheckout::new(id.clone(), RecordKind::new("note")).formerly(id);
        assert!(
            checkout.formerly.is_empty(),
            "a record is not its own predecessor"
        );
        let mut many = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        for _ in 0..=MAX_FORMER_IDS {
            many = many.formerly(RecordId::minted());
        }
        assert!(!many.is_valid());
    }
    // phase: a_path_record_is_adopted_once_by_its_file_record
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let package = "so.tilo.migration";
        let room = "migration-room";
        let path = "/home/ada/papers/draft.pdf";
        let file = RecordId::for_file_record(uuid::Uuid::new_v4());
        let (old, new) = markup_checkouts(path, &file);

        {
            let mut owners = RecordOwners::new(root.clone());
            let store = owners.open(package, room).unwrap();
            store.create_record(&old).unwrap();
            store
                .with(&old.id, |doc| {
                    put_text(doc, "note", "check the proof");
                    add_reference(doc, &RecordRef::new(RecordId::for_file(path), "marks-up"));
                })
                .unwrap();
            store.try_flush().unwrap();
        }
        let old_bytes = stored_bytes(&root, package, room, &old.id);

        {
            let mut owners = RecordOwners::new(root.clone());
            let store = owners.open(package, room).unwrap();
            assert!(matches!(
                store.try_open_record(&new),
                Ok(RecordOpenOutcome::Loaded)
            ));
            store
                .read(&new.id, |doc| {
                    assert_eq!(text(doc, "note").as_deref(), Some("check the proof"));
                    assert_eq!(identity(doc).unwrap().id, new.id, "identity is restamped");
                    assert_eq!(former_identities(doc), vec![old.id.clone()]);
                    let refs = references(doc);
                    assert!(
                        refs.contains(&RecordRef::new(file.clone(), "marks-up")),
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
            stored_bytes(&root, package, room, &old.id),
            old_bytes,
            "the former record is only read"
        );

        // Reopening finds it under the new id; a different file later found at
        // the same path names the same former record, and the first successor
        // keeps custody of it.
        let mut owners = RecordOwners::new(root);
        let store = owners.open(package, room).unwrap();
        assert!(matches!(
            store.try_open_record(&new),
            Ok(RecordOpenOutcome::Loaded)
        ));
        store
            .read(&new.id, |doc| {
                assert_eq!(text(doc, "note").as_deref(), Some("check the proof"));
            })
            .unwrap();
        let other = RecordId::for_file_record(uuid::Uuid::new_v4());
        let (_, stranger) = markup_checkouts(path, &other);
        assert!(matches!(
            store.try_open_record(&stranger),
            Err(RecordOpenError::MissingRecord)
        ));
    }
    // phase: checkout_adopts_a_former_record_once
    {
        let temp = tempfile::tempdir().unwrap();
        let documents = native(temp.path());
        let package = "so.tilo.migration-launch";
        let room = "22222222-2222-4222-8222-222222222222";
        let path = "/home/ada/papers/draft.pdf";
        let file = RecordId::for_file_record(uuid::Uuid::new_v4());
        let (old, new) = markup_checkouts(path, &file);
        {
            let mut set = RecordSet::open_in(Rc::clone(&documents), package, room);
            let mut state = State::new();
            assert!(set.checkout(&mut state, None, &old));
            put_text(&mut state.doc, "note", "made before files had ids");
            set.flush(&mut state.doc);
        }
        {
            let mut set = RecordSet::open_in(Rc::clone(&documents), package, room);
            let mut state = State::new();
            let mut room_doc = AutoCommit::new();
            assert!(set.checkout(&mut state, Some(&mut room_doc), &new));
            assert_eq!(
                text(&state.doc, "note").as_deref(),
                Some("made before files had ids")
            );
            assert_eq!(identity(&state.doc).unwrap().id, new.id);
            assert!(room::holds(&room_doc, &file));
            assert!(!room::holds(&room_doc, &RecordId::for_file(path)));
            set.flush(&mut state.doc);
        }
        let store = RecordStore::new(RecordSet::open_in(Rc::clone(&documents), package, room));
        store.open_existing_record_checked(&new).unwrap();
        store
            .read(&new.id, |doc| {
                assert_eq!(
                    text(doc, "note").as_deref(),
                    Some("made before files had ids")
                );
            })
            .unwrap();
        let other = RecordId::for_file_record(uuid::Uuid::new_v4());
        let (_, stranger) = markup_checkouts(path, &other);
        assert!(matches!(
            store.open_existing_record_checked(&stranger),
            Err(RecordOpenError::MissingRecord)
        ));
    }
    // phase: existing_only_admission_adopts_a_former_record
    {
        let temp = tempfile::tempdir().unwrap();
        let documents = native(temp.path());
        let package = "so.tilo.migration-existing";
        let room = "33333333-3333-4333-8333-333333333333";
        let file = RecordId::for_file_record(uuid::Uuid::new_v4());
        let (old, new) = markup_checkouts("/srv/notes/plan.pdf", &file);
        {
            let store = RecordStore::new(RecordSet::open_in(Rc::clone(&documents), package, room));
            store.open_record(&old);
            store
                .with(&old.id, |doc| put_text(doc, "note", "adopt me"))
                .unwrap();
            store.flush();
        }
        let store = RecordStore::new(RecordSet::open_in(Rc::clone(&documents), package, room));
        store.open_existing_record_checked(&new).unwrap();
        store
            .read(&new.id, |doc| {
                assert_eq!(text(doc, "note").as_deref(), Some("adopt me"));
                assert_eq!(identity(doc).unwrap().id, new.id);
            })
            .unwrap();
    }
}

#[test]
fn a_record_has_one_route_and_the_custody_key_is_none() {
    let primary = RecordId::package_primary();
    assert_eq!(primary.route(), "");
    assert_eq!(RecordId::from_route(""), Some(primary.clone()));
    assert_eq!(RecordId::from_route(primary.as_str()), None);
    let window = RecordId::room_default();
    assert_eq!(window.route(), window.as_str());
    assert_eq!(RecordId::from_route(window.route()), Some(window));
}
