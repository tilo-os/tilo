//! The record store opening a named record against the real canonical
//! creation path: a native construction holding the record's creation
//! capture (`collaboration::native_initial::named_creation`), not a
//! permissive validator.
use crate::collaboration::native_initial::named_creation::{Fixture, fixture, fixture_graph};
use crate::collaboration::native_initial::{RecordInitialization, take_initialization};
use crate::record::{
    RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordOwners, RecordStore,
    validate_creation_capture,
};
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};

/// Check `original` of `checkout` in `room` against the construction's
/// original capture, as the record store does before initializing it.
fn validate(
    initialization: &RecordInitialization,
    original: &AutoCommit,
    checkout: &RecordCheckout,
    room: &str,
) -> Result<(), RecordOpenError> {
    initialization.validate_original(checkout, |read| {
        validate_creation_capture(original, checkout, room, read)
    })
}

#[tokio::test(flavor = "current_thread")]
async fn named_genesis_install_requires_ack_and_cannot_replace_late_changes() {
    for (accept, late_change) in [(true, false), (false, false), (true, true)] {
        let mut fixture = fixture(true, false).await;
        let directory = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(directory.path().to_owned());
        let store = owners.open("so.tilo.store", &fixture.room).unwrap();
        store.try_open_record(&fixture.checkout).unwrap();
        let initialization = take_initialization("so.tilo.store", &fixture.room, &fixture.checkout)
            .unwrap()
            .unwrap();
        assert_original_scope(&fixture, &store, &initialization);
        assert!(fixture.nothing_queued());
        let ack = fixture.acknowledge_initialization(accept);
        let result = store.initialize_admitted_record(
            &fixture.checkout,
            &initialization,
            |doc| {
                doc.put(ROOT, "initialized", true).unwrap();
                Ok(())
            },
            |_| {
                assert!(
                    store.route(&fixture.checkout).is_err(),
                    "never Ready before ACK"
                );
                if late_change {
                    store
                        .with(&fixture.checkout.id, |doc| {
                            doc.put(ROOT, "late_local_change", true).unwrap();
                            doc.commit();
                        })
                        .unwrap();
                }
                Ok(())
            },
        );
        ack.join().unwrap();
        assert_eq!(result.is_ok(), accept && !late_change);
        assert_eq!(
            store.route(&fixture.checkout).is_ok(),
            accept && !late_change
        );
        store
            .read_checked(&fixture.checkout.id, |doc| {
                assert_eq!(
                    doc.get(ROOT, "initialized").unwrap().is_some(),
                    accept && !late_change
                );
                assert_eq!(
                    doc.get(ROOT, "late_local_change").unwrap().is_some(),
                    late_change
                );
                assert_eq!(
                    doc.get(ROOT, "future_owner_field")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("preserved")
                );
            })
            .unwrap();
        if accept {
            assert!(initialization.workspace().is_none());
            assert!(
                store
                    .initialize_admitted_record(
                        &fixture.checkout,
                        &initialization,
                        |_| Ok(()),
                        |_| Ok(())
                    )
                    .is_err()
            );
        }
        assert!(
            take_initialization("so.tilo.store", &fixture.room, &fixture.checkout)
                .unwrap()
                .is_none()
        );
    }
}

fn assert_original_scope(
    fixture: &Fixture,
    store: &RecordStore,
    initialization: &RecordInitialization,
) {
    let original = store
        .read_checked(&fixture.checkout.id, Clone::clone)
        .unwrap();
    let wrong_kind =
        RecordCheckout::new(fixture.checkout.id.clone(), RecordKind::new("wrong.kind"));
    let wrong_record = RecordCheckout::new(RecordId::minted(), fixture.checkout.kind.clone());
    for checkout in [&wrong_kind, &wrong_record] {
        assert!(validate(initialization, &original, checkout, &fixture.room).is_err());
    }
    assert!(validate(initialization, &original, &fixture.checkout, "wrong-room").is_err());
    let mut changed = original;
    changed.put(ROOT, "changed_heads", true).unwrap();
    changed.commit();
    assert!(validate(initialization, &changed, &fixture.checkout, &fixture.room).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn named_original_empty_creation_admits_before_initialization() {
    let fixture = fixture_graph(true, false, false).await;
    let directory = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(directory.path().to_owned());
    let store = owners.open("so.tilo.store", &fixture.room).unwrap();
    store.try_open_record(&fixture.checkout).unwrap();
    assert!(store.admission_route(&fixture.checkout).is_ok());
    assert!(store.route(&fixture.checkout).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn named_original_creation_admits_exact_graph_but_not_ready_identity() {
    let fixture = fixture(true, false).await;
    let directory = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(directory.path().to_owned());
    let store = owners.open("so.tilo.store", &fixture.room).unwrap();
    store.try_open_record(&fixture.checkout).unwrap();
    store.try_open_record(&fixture.checkout).unwrap();
    assert!(
        store.route(&fixture.checkout).is_err(),
        "not Ready before Initialize"
    );
    let admitted = store.admission_route(&fixture.checkout).unwrap();
    assert_eq!(admitted.app_id, "so.tilo.store");
    assert_eq!(
        admitted.record.as_deref(),
        Some(fixture.checkout.id.as_str())
    );
    let wrong_kind =
        RecordCheckout::new(fixture.checkout.id.clone(), RecordKind::new("wrong.kind"));
    assert!(store.admission_route(&wrong_kind).is_err());
    store
        .read(&fixture.checkout.id, |doc| {
            assert_eq!(
                doc.get(ROOT, "future_owner_field")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("preserved")
            );
        })
        .unwrap();
    assert!(
        take_initialization("so.tilo.store", &fixture.room, &fixture.checkout)
            .unwrap()
            .is_some()
    );
    fixture.initialized();
    assert!(store.admission_route(&fixture.checkout).is_err());
    assert!(
        store.try_open_record(&fixture.checkout).is_err(),
        "consumed receipt cannot reopen an unfinished graph"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn named_identityless_restore_and_wrong_room_refuse_without_cache_install() {
    for (receipt, wrong_room) in [(false, false), (true, true)] {
        let fixture = fixture(receipt, wrong_room).await;
        let directory = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(directory.path().to_owned());
        let store = owners.open("so.tilo.store", &fixture.room).unwrap();
        assert!(store.try_open_record(&fixture.checkout).is_err());
        assert!(store.open_records().is_empty());
        assert!(store.admission_route(&fixture.checkout).is_err());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn named_creation_does_not_adopt_modified_or_foreign_local_cache() {
    let fixture = fixture(true, false).await;
    let directory = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(directory.path().to_owned());
    let store = owners.open("so.tilo.store", &fixture.room).unwrap();
    store.try_open_record(&fixture.checkout).unwrap();
    store
        .with(&fixture.checkout.id, |doc| {
            doc.put(ROOT, "unadmitted_edit", true).unwrap();
            doc.commit();
        })
        .unwrap();
    assert!(store.try_open_record(&fixture.checkout).is_err());
    assert!(store.admission_route(&fixture.checkout).is_err());
    let other_directory = tempfile::tempdir().unwrap();
    let mut other_owners = RecordOwners::new(other_directory.path().to_owned());
    let foreign = other_owners
        .open_person("so.tilo.store", &fixture.room)
        .unwrap();
    let wrong_kind =
        RecordCheckout::new(fixture.checkout.id.clone(), RecordKind::new("other.kind"));
    foreign.create_record(&wrong_kind).unwrap();
    foreign.flush().unwrap();
    drop(foreign);
    drop(other_owners);
    let mut restored = RecordOwners::new(other_directory.path().to_owned());
    let restored = restored.open("so.tilo.store", &fixture.room).unwrap();
    assert!(restored.try_open_record(&fixture.checkout).is_err());
    assert!(restored.open_records().is_empty());
    assert!(restored.admission_route(&fixture.checkout).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn named_original_receipt_survives_same_graph_refresh_but_never_masks_new_history() {
    let fixture = fixture(true, false).await;
    let directory = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(directory.path().to_owned());
    let store = owners.open("so.tilo.store", &fixture.room).unwrap();
    store.try_open_record(&fixture.checkout).unwrap();
    let initialization = take_initialization("so.tilo.store", &fixture.room, &fixture.checkout)
        .unwrap()
        .unwrap();
    assert!(
        take_initialization("so.tilo.store", &fixture.room, &fixture.checkout)
            .unwrap()
            .is_none(),
        "construction custody remains one-shot"
    );
    let same = fixture.refresh(false).await;
    assert!(same.creation_workspace().is_none());
    assert!(store.admission_route(&fixture.checkout).is_ok());
    assert!(
        store.route(&fixture.checkout).is_err(),
        "not Ready without Initialize ACK"
    );
    assert!(initialization.workspace().is_some());
    fixture.refresh(true).await;
    assert!(store.admission_route(&fixture.checkout).is_err());
    assert!(store.route(&fixture.checkout).is_err());
    fixture.initialized();
    assert!(initialization.workspace().is_none());
    assert!(store.admission_route(&fixture.checkout).is_err());
}
