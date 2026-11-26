//! A record reserved before it exists: one creator initializes it, a failed initializer leaves
//! nothing reserved, and canceling a reservation gives the record its absence back.

use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
use tilo_ui::resource::{
    DocumentCheckout, DocumentId, ResourceOpenError, ResourceOwners, ResourceSaveStatus,
    ResourceStore, ResourceType,
};

fn target() -> DocumentCheckout {
    DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"))
}

fn body(store: &ResourceStore, want: &DocumentCheckout) -> String {
    store
        .read(&want.id, |doc| {
            doc.get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        })
        .unwrap()
}

#[test]
fn a_reservation_initializes_once_and_a_failed_initializer_leaves_nothing() {
    let mut owners = ResourceOwners::in_memory();
    let store = owners.open("so.tilo.test", "workspace-a").unwrap();
    let want = target();
    assert!(matches!(
        store.try_open_resource(&want),
        Err(ResourceOpenError::MissingResource)
    ));

    let mut reservation = store.reserve_resource(&want).unwrap();
    assert_eq!(reservation.checkout().id, want.id);
    assert!(!store.is_ready(&want.id));
    assert!(store.open_resources().is_empty());
    assert!(matches!(
        store.save_status(&want.id),
        Err(ResourceOpenError::Busy)
    ));
    // A second creator may not reserve or open the record under reservation.
    assert!(matches!(
        store.reserve_resource(&want),
        Err(ResourceOpenError::Busy)
    ));
    assert!(matches!(
        store.try_open_resource(&want),
        Err(ResourceOpenError::Busy)
    ));

    // A failed initializer leaves the record as it was: reserved, with nothing of its half.
    assert!(matches!(
        reservation.initialize(|doc| {
            doc.put(ROOT, "discard", "partial").unwrap();
            Err("retry me".into())
        }),
        Err(ResourceOpenError::Initialization(_))
    ));
    assert!(!store.is_ready(&want.id));
    reservation
        .initialize(|doc| {
            assert!(doc.get(ROOT, "discard").unwrap().is_none());
            doc.put(ROOT, "body", "mine").unwrap();
            Ok(())
        })
        .unwrap();
    assert!(
        reservation
            .initialize(|_| panic!("must not initialize twice"))
            .is_err(),
        "a reservation initializes exactly once"
    );
    drop(reservation);
    assert!(store.is_ready(&want.id));
    assert_eq!(body(&store, &want), "mine");
}

#[test]
fn canceling_a_reservation_gives_the_record_its_absence_back() {
    let mut owners = ResourceOwners::in_memory();
    let store = owners.open("so.tilo.test", "workspace-a").unwrap();
    let want = target();
    let canceled = store.reserve_resource(&want).unwrap();
    drop(canceled);
    assert!(matches!(
        store.try_open_resource(&want),
        Err(ResourceOpenError::MissingResource)
    ));
    assert!(matches!(
        store.save_status(&want.id),
        Err(ResourceOpenError::MissingResource)
    ));
    assert!(
        store.reserve_resource(&want).is_ok(),
        "it may be taken again"
    );
}

#[test]
fn a_store_in_memory_alone_says_its_records_are_kept_nowhere() {
    let store = ResourceStore::in_memory();
    let want = target();
    store.open_resource(&want);
    assert_eq!(
        store.save_status(&want.id).unwrap(),
        ResourceSaveStatus::MemoryOnly
    );
}
