use super::*;

#[test]
fn portable_owner_preserves_reservation_identity_custody_and_durability() {
    let cache = crate::portable_storage::PortableStorage::new("profile", "device").unwrap();
    let mut owners = RecordOwners::portable(cache.clone());
    let store = owners.open("so.tilo.test", "workspace").unwrap();
    let again = owners.open("so.tilo.test", "workspace").unwrap();
    assert!(store.same_owner(&again));
    let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
    let mut reservation = store.reserve_record(&checkout).unwrap();
    assert!(again.reserve_record(&checkout).is_err());
    assert!(!store.is_ready(&checkout.id));
    reservation
        .initialize(|doc| {
            doc.put(automerge::ROOT, "title", "Hello").unwrap();
            Ok(())
        })
        .unwrap();
    let actor = store
        .read(&checkout.id, |doc| doc.get_actor().clone())
        .unwrap();
    assert!(store.try_flush().is_err());
    for write in cache.pending_writes() {
        cache.acknowledge(&write, Ok(())).unwrap();
    }
    assert!(store.try_flush().is_ok());
    assert_eq!(
        store.save_status(&checkout.id).unwrap(),
        RecordSaveStatus::Saved
    );
    drop(reservation);
    drop(again);
    drop(store);
    let reopened = owners.open("so.tilo.test", "workspace").unwrap();
    reopened.try_open_record(&checkout).unwrap();
    assert_ne!(
        reopened
            .read(&checkout.id, |doc| doc.get_actor().clone())
            .unwrap(),
        actor,
        "a page writes the record under a writer it mints, never the one a saved row carries"
    );
    let title = reopened
        .read(&checkout.id, |doc| {
            doc.get(automerge::ROOT, "title")
                .unwrap()
                .map(|(value, _)| value.to_string())
        })
        .unwrap();
    assert_eq!(
        title.as_deref(),
        Some("\"Hello\""),
        "what the first page wrote"
    );
    let wrong_kind = RecordCheckout::new(checkout.id.clone(), RecordKind::new("other"));
    assert!(matches!(
        reopened.try_open_record(&wrong_kind),
        Err(RecordOpenError::IdentityMismatch)
    ));
}
#[test]
fn portable_failed_initializer_keeps_reservation_until_drop() {
    let cache = crate::portable_storage::PortableStorage::new("profile", "device").unwrap();
    let store = RecordOwners::portable(cache.clone())
        .open("so.tilo.test", "workspace")
        .unwrap();
    let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
    let mut reservation = store.reserve_record(&checkout).unwrap();
    assert!(reservation.initialize(|_| Err("failed".into())).is_err());
    assert!(store.reserve_record(&checkout).is_err());
    assert!(cache.pending_writes().is_empty());
    drop(reservation);
    assert!(store.reserve_record(&checkout).is_ok());
}

/// What a window says it did to a record nobody else decides is said
/// at once; in a record a workspace decides, once the workspace answered every
/// commit that carried it, and never when the workspace refused the change.
#[test]
fn a_notice_waits_for_the_workspace_that_decides_the_record() {
    use collab_types::CommitNamed as _;
    let store = RecordStore::in_memory();
    let checkout = RecordCheckout::new(RecordId::for_tab(78), RecordKind::new("tab"));
    store.create_record(&checkout).unwrap();
    let commit = |field: &str, value: &str| {
        store
            .with(&checkout.id, |doc| {
                doc.put(automerge::ROOT, field, value).unwrap();
                doc.commit_edit("test.note", field).unwrap()
            })
            .unwrap()
    };
    let hold = |text: &str| store.hold_notice(&checkout.id, text.into()).unwrap();
    commit("title", "alone");
    hold("Saved alone");
    assert_eq!(
        store.take_said(&checkout.id),
        ["Saved alone"],
        "nobody else decides"
    );

    let (_, heads) = store.replication_snapshot(&checkout.id).unwrap();
    let mut frontier = crate::state::DocumentFrontier::default();
    frontier.advance(heads);
    store.workspace_settled(&checkout.id);
    commit("title", "shared");
    hold("Saved shared");
    assert!(
        store.take_said(&checkout.id).is_empty(),
        "not published yet"
    );
    let (delta, heads) = store
        .replication_delta(&checkout.id, &mut frontier)
        .unwrap();
    assert!(!delta.is_empty());
    frontier.advance(heads);
    assert!(
        store.take_said(&checkout.id).is_empty(),
        "the workspace has not answered"
    );
    store.workspace_settled(&checkout.id);
    assert_eq!(store.take_said(&checkout.id), ["Saved shared"]);
    assert!(store.take_said(&checkout.id).is_empty(), "said once");

    let refused = commit("body", "refused");
    hold("Saved the body");
    let (_, heads) = store
        .replication_delta(&checkout.id, &mut frontier)
        .unwrap();
    frontier.advance(heads);
    store
        .withdraw_refused(
            &checkout.id,
            &[refused],
            "This workspace doesn't allow that.",
        )
        .unwrap()
        .expect("withdrawn");
    store.workspace_settled(&checkout.id);
    assert!(
        store.take_said(&checkout.id).is_empty(),
        "a refused change is never said done"
    );
}

/// A change the workspace refused leaves the record with every change built
/// on it, each named for the app as the action it was; what came before
/// stays.
#[test]
fn a_refused_change_is_withdrawn_with_what_was_built_on_it() {
    use collab_types::CommitNamed as _;
    let store = RecordStore::in_memory();
    let checkout = RecordCheckout::new(RecordId::for_tab(77), RecordKind::new("tab"));
    store.create_record(&checkout).unwrap();
    let commit = |field: &str, value: &str| {
        store
            .with(&checkout.id, |doc| {
                doc.put(automerge::ROOT, field, value).unwrap();
                doc.commit_edit("test.note", field).unwrap()
            })
            .unwrap()
    };
    commit("title", "kept");
    let refused = commit("body", "refused");
    commit("footer", "built on it");
    let heads = store
        .withdraw_refused(
            &checkout.id,
            &[refused],
            "This workspace doesn't allow that.",
        )
        .unwrap()
        .expect("something was withdrawn");
    assert_eq!(
        store.with(&checkout.id, AutoCommit::get_heads).unwrap(),
        heads
    );
    assert_eq!(
        store
            .read(&checkout.id, |doc| str_at(doc, &ROOT, "title"))
            .flatten()
            .as_deref(),
        Some("kept")
    );
    assert_eq!(
        store
            .read(&checkout.id, |doc| str_at(doc, &ROOT, "body"))
            .flatten(),
        None
    );
    assert_eq!(
        store
            .read(&checkout.id, |doc| str_at(doc, &ROOT, "footer"))
            .flatten(),
        None
    );
    let refusals = store.take_refusals(&checkout.id);
    assert_eq!(
        refusals
            .iter()
            .map(|refusal| refusal.action.as_str())
            .collect::<Vec<_>>(),
        ["body", "footer"]
    );
    assert!(
        refusals
            .iter()
            .all(|refusal| refusal.reason == "This workspace doesn't allow that.")
    );
    assert!(store.take_refusals(&checkout.id).is_empty(), "told once");
    assert_eq!(
        store
            .withdraw_refused(&checkout.id, &[refused], "again")
            .unwrap(),
        None,
        "a change the record no longer holds withdraws nothing"
    );
}

/// A record the person kept in a page before their own store existed — filed
/// under the workspace their records were keyed by then — opens as theirs:
/// the same record id, in their own scope, with what was in it.
#[test]
fn a_record_a_page_kept_before_the_store_reads_in_the_persons_own_scope() {
    let cache = crate::portable_storage::PortableStorage::new("profile", "device").unwrap();
    let root = [7u8; 32];
    let person = hex::encode(root);
    let kind = RecordKind::new("so.tilo.test.personal");
    let id = RecordId::derived(kind.as_str(), &RecordId::for_tab(0));
    let retired = collab_types::retired_person_workspace(&root).to_string();
    let mut written = AutoCommit::new();
    written.put(automerge::ROOT, "chapter", "three").unwrap();
    assert!(crate::record::init_scoped(
        &mut written,
        &id,
        &kind,
        &retired,
        Some(&retired)
    ));
    let former = crate::portable_storage::StorageKey::Record {
        package: "so.tilo.test".into(),
        workspace: retired,
        record: id.as_str().into(),
    };
    cache
        .put(
            former,
            written.save(),
            Some(written.get_actor().to_bytes().to_vec()),
        )
        .unwrap();

    let mut owners = RecordOwners::portable(cache.clone());
    let store = owners.open_personal("so.tilo.test", &person).unwrap();
    let checkout = RecordCheckout::new(id.clone(), kind.clone());
    store.try_open_record(&checkout).unwrap();
    assert_eq!(
        store
            .read(&checkout.id, |doc| str_at(doc, &ROOT, "chapter"))
            .flatten()
            .as_deref(),
        Some("three"),
        "what the person had in it is still there"
    );
    assert!(
        cache
            .get(&crate::portable_storage::StorageKey::Record {
                package: "so.tilo.test".into(),
                workspace: crate::record::person::PERSONAL_SCOPE.into(),
                record: id.as_str().into(),
            })
            .is_some(),
        "the record is kept in the person's own scope now"
    );
    assert_eq!(
        store
            .route(&checkout)
            .expect("the person's own cluster carries it")
            .cluster_id,
        collab_types::personal_cluster(&root)
    );
}
