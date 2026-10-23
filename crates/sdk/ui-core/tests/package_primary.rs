use automerge::{AutoCommit, ROOT, transaction::Transactable};
use tilo_ui::portable_storage::{PortableStorage, StorageKey};
use tilo_ui::record::{RecordCheckout, RecordId, RecordKind, RecordOwners};

#[test]
fn primary_owner_is_shared_and_keeps_recordless_wire_identity() {
    let cache = PortableStorage::new("profile", "device").unwrap();
    let checkout = RecordCheckout::package_primary(RecordKind::new("chat.workspace"));
    let a = RecordOwners::portable(cache.clone())
        .open("so.tilo.chat", "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
        .unwrap();
    let b = RecordOwners::portable(cache.clone())
        .open("so.tilo.chat", "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
        .unwrap();
    a.reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "message", "one").unwrap();
            Ok(())
        })
        .unwrap();
    b.try_open_record(&checkout).unwrap();
    assert_eq!(a.route(&checkout).unwrap().record, None);
    b.with(&checkout.id, |doc| {
        doc.put(ROOT, "message", "two").unwrap();
    })
    .unwrap();
    let heads = a
        .read(
            &checkout.id,
            automerge::transaction::Transactable::base_heads,
        )
        .unwrap();
    assert_eq!(
        b.read(
            &checkout.id,
            automerge::transaction::Transactable::base_heads
        )
        .unwrap(),
        heads
    );
    assert!(
        a.read(&checkout.id, tilo_ui::record::identity)
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .pending_writes()
            .iter()
            .any(|w| matches!(w.key, StorageKey::Primary { .. }))
    );
    assert_ne!(checkout.id, RecordId::for_tab(1));
}

#[test]
fn native_primary_directory_matches_existing_room_document() {
    let root = tempfile::tempdir().unwrap();
    let package = "so.tilo.chat";
    let room_id = "room-a";
    let path = app_state::DocStore::room_primary_dir_at(root.path(), package, room_id).unwrap();
    assert_eq!(path, root.path().join(package).join("rooms").join(room_id));
    let mut owner = app_state::DocStore::open_canonical_at(package, &path).unwrap();
    let mut doc = owner.new_document();
    doc.put(ROOT, "message", "native").unwrap();
    doc.commit();
    owner.save(&mut doc).unwrap();
    let actor = doc.get_actor().clone();
    let bytes = doc.save();
    drop(owner);
    let store = RecordOwners::new(root.path().into())
        .open(package, room_id)
        .unwrap();
    let checkout = RecordCheckout::package_primary(RecordKind::new("chat.workspace"));
    store.try_open_record(&checkout).unwrap();
    let original = AutoCommit::load(&bytes).unwrap();
    assert_eq!(
        store.read(&checkout.id, |d| d.get_actor().clone()).unwrap(),
        actor
    );
    assert_eq!(
        store
            .read(
                &checkout.id,
                automerge::transaction::Transactable::base_heads
            )
            .unwrap(),
        original.base_heads()
    );
}

#[test]
fn primary_admission_rejects_stamped_records_and_wire_parse_rejects_internal_key() {
    let cache = PortableStorage::new("profile", "device").unwrap();
    let checkout = RecordCheckout::package_primary(RecordKind::new("chat.workspace"));
    assert!(RecordId::parse(checkout.id.as_str()).is_none());
    let mut doc = AutoCommit::new();
    doc.put(
        ROOT,
        app_state::SCHEMA_VERSION_KEY,
        u64::from(app_state::SCHEMA_VERSION),
    )
    .unwrap();
    tilo_ui::record::init_scoped(
        &mut doc,
        &RecordId::for_tab(7),
        &checkout.kind,
        "room",
        Some("room"),
    );
    doc.commit();
    cache
        .hydrate(
            StorageKey::Primary {
                package: "so.tilo.chat".into(),
                workspace: "room".into(),
            },
            doc.save(),
            Some(doc.get_actor().to_bytes().to_vec()),
        )
        .unwrap();
    let store = RecordOwners::portable(cache)
        .open("so.tilo.chat", "room")
        .unwrap();
    assert!(matches!(
        store.try_open_record(&checkout),
        Err(tilo_ui::record::RecordOpenError::IdentityMismatch)
    ));
}

#[test]
fn primary_custody_is_partitioned_by_package_room_and_kind() {
    let cache = PortableStorage::new("profile", "device").unwrap();
    let checkout = RecordCheckout::package_primary(RecordKind::new("chat.workspace"));
    let mut owners = RecordOwners::portable(cache);
    let a = owners.open("so.tilo.chat", "room-a").unwrap();
    a.reserve_record(&checkout)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    assert!(
        owners
            .open("so.tilo.chat", "room-b")
            .unwrap()
            .try_open_record(&checkout)
            .is_err()
    );
    assert!(
        owners
            .open("so.tilo.other", "room-a")
            .unwrap()
            .try_open_record(&checkout)
            .is_err()
    );
    assert!(
        a.try_open_record(&RecordCheckout::package_primary(RecordKind::new(
            "wrong.kind"
        )))
        .is_err()
    );
}
