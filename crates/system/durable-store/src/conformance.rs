//! The behaviour every [`DurableStore`] shares, as a reusable suite. An
//! implementation's tests call [`run`] (and [`reopen`] when its data outlives
//! the store value) so hosts cannot drift apart on ordering, ownership or
//! durability.
#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::missing_panics_doc,
    reason = "a conformance suite reports violations by failing its assertions"
)]

use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Durability, DurableStore, StorageKey, StoreError, Stored, WriteOp};

/// A store under test.
pub struct Subject<'a> {
    pub store: &'a dyn DurableStore,
    /// Drive the store until every accepted write has resolved: nothing for a
    /// store that commits by itself, one host commit loop for a
    /// host-acknowledged store.
    pub settle: &'a dyn Fn(),
}

impl Subject<'_> {
    fn committed(&self, key: &StorageKey) {
        (self.settle)();
        self.store.flush(Some(key)).unwrap();
    }
}

/// Keeps every check's keys distinct when several share one store.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn fresh(tag: &str) -> StorageKey {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    StorageKey::record("so.tilo.conformance", "room", format!("{tag}-{n}"))
}

/// Every check that needs only one live store value.
pub fn run(subject: &Subject<'_>) {
    namespace_is_named(subject);
    missing_keys_read_as_absent(subject);
    writes_apply_in_order(subject);
    writer_identity_survives_writes_without_one(subject);
    accepted_writes_become_durable(subject);
    leases_are_exclusive_until_dropped(subject);
    shared_leases_coexist_and_exclude_an_exclusive_owner(subject);
    one_claimant_wins_custody(subject);
    keys_never_alias(subject);
    views_round_trip(subject);
}

/// Data committed through one store value is visible, unchanged, through a
/// second value opened over the same storage.
pub fn reopen(subject: &Subject<'_>, reopen: &dyn Fn() -> Box<dyn DurableStore>) {
    let key = fresh("reopen");
    {
        let _lease = subject.store.lease(&key).unwrap();
        subject
            .store
            .write(&key, WriteOp::Replace(b"base".to_vec()), Some(b"writer"))
            .unwrap();
        subject
            .store
            .write(&key, WriteOp::Append(b"+tail".to_vec()), None)
            .unwrap();
        subject.committed(&key);
    }
    let reopened = reopen();
    assert_eq!(reopened.namespace(), subject.store.namespace());
    assert_eq!(
        reopened.read(&key).unwrap(),
        Some(Stored {
            bytes: b"base+tail".to_vec(),
            writer: Some(b"writer".to_vec()),
        })
    );
}

fn namespace_is_named(subject: &Subject<'_>) {
    assert!(!subject.store.namespace().is_empty());
}

fn missing_keys_read_as_absent(subject: &Subject<'_>) {
    assert_eq!(subject.store.read(&fresh("missing")).unwrap(), None);
    assert!(subject.store.flush(Some(&fresh("missing"))).is_ok());
}

fn writes_apply_in_order(subject: &Subject<'_>) {
    let key = fresh("order");
    let _lease = subject.store.lease(&key).unwrap();
    let store = subject.store;
    store
        .write(&key, WriteOp::Replace(b"one".to_vec()), None)
        .unwrap();
    store
        .write(&key, WriteOp::Append(b"+two".to_vec()), None)
        .unwrap();
    assert_eq!(store.read(&key).unwrap().unwrap().bytes, b"one+two");
    subject.committed(&key);
    store
        .write(&key, WriteOp::Append(b"+three".to_vec()), None)
        .unwrap();
    store
        .write(&key, WriteOp::Replace(b"fresh".to_vec()), None)
        .unwrap();
    store
        .write(&key, WriteOp::Append(b"+tail".to_vec()), None)
        .unwrap();
    subject.committed(&key);
    assert_eq!(store.read(&key).unwrap().unwrap().bytes, b"fresh+tail");
}

fn writer_identity_survives_writes_without_one(subject: &Subject<'_>) {
    let key = fresh("writer");
    let _lease = subject.store.lease(&key).unwrap();
    let store = subject.store;
    store
        .write(&key, WriteOp::Replace(b"doc".to_vec()), Some(b"first"))
        .unwrap();
    store
        .write(&key, WriteOp::Append(b"+".to_vec()), None)
        .unwrap();
    subject.committed(&key);
    assert_eq!(
        store.read(&key).unwrap().unwrap().writer.as_deref(),
        Some(b"first".as_slice())
    );
    store
        .write(&key, WriteOp::Append(b"+".to_vec()), Some(b"second"))
        .unwrap();
    subject.committed(&key);
    assert_eq!(
        store.read(&key).unwrap(),
        Some(Stored {
            bytes: b"doc++".to_vec(),
            writer: Some(b"second".to_vec()),
        })
    );
}

fn accepted_writes_become_durable(subject: &Subject<'_>) {
    let key = fresh("durable");
    let _lease = subject.store.lease(&key).unwrap();
    let store = subject.store;
    let first = store
        .write(&key, WriteOp::Replace(b"a".to_vec()), None)
        .unwrap();
    let second = store
        .write(&key, WriteOp::Append(b"b".to_vec()), None)
        .unwrap();
    assert!(second > first, "revisions must increase");
    for revision in [first, second] {
        assert_ne!(
            std::mem::discriminant(&store.durability(&key, revision)),
            std::mem::discriminant(&Durability::Failed(String::new())),
        );
    }
    subject.committed(&key);
    assert_eq!(store.durability(&key, first), Durability::Durable);
    assert_eq!(store.durability(&key, second), Durability::Durable);
    assert!(store.flush(None).is_ok());
}

fn leases_are_exclusive_until_dropped(subject: &Subject<'_>) {
    let key = fresh("lease");
    let lease = subject.store.lease(&key).unwrap();
    assert!(matches!(
        subject.store.lease(&key),
        Err(StoreError::Busy(_))
    ));
    let other = subject.store.lease(&fresh("lease")).unwrap();
    drop(lease);
    drop(subject.store.lease(&key).unwrap());
    drop(other);
}

fn shared_leases_coexist_and_exclude_an_exclusive_owner(subject: &Subject<'_>) {
    let store = subject.store;
    let package = format!("so.tilo.shared-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    for key in [fresh("shared"), StorageKey::package(package)] {
        let first = store.lease_shared(&key).unwrap();
        let second = store.lease_shared(&key).unwrap();
        assert!(
            first.primary,
            "the first shared writer continues the identity"
        );
        assert!(!second.primary, "only one shared writer is primary");
        assert!(matches!(store.lease(&key), Err(StoreError::Busy(_))));
        store
            .write(&key, WriteOp::Replace(b"a".to_vec()), Some(b"primary"))
            .unwrap();
        store
            .write(&key, WriteOp::Append(b"b".to_vec()), None)
            .unwrap();
        subject.committed(&key);
        assert_eq!(store.read(&key).unwrap().unwrap().bytes, b"ab");
        drop(first);
        let third = store.lease_shared(&key).unwrap();
        assert!(third.primary, "primacy passes on once released");
        drop((second, third));
        let exclusive = store.lease(&key).unwrap();
        assert!(matches!(store.lease_shared(&key), Err(StoreError::Busy(_))));
        drop(exclusive);
    }
}

fn one_claimant_wins_custody(subject: &Subject<'_>) {
    let store = subject.store;
    let package = format!("so.tilo.claim-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let key = StorageKey::record(package.clone(), "room", "former");
    // A former record exists before any successor claims it.
    let lease = store.lease_shared(&key).unwrap();
    store
        .write(&key, WriteOp::Replace(b"source".to_vec()), None)
        .unwrap();
    subject.committed(&key);
    drop(lease);
    assert!(store.claim(&key, "successor-a").unwrap());
    assert!(!store.claim(&key, "successor-b").unwrap());
    assert!(
        store.claim(&key, "successor-a").unwrap(),
        "the claimant may resume"
    );
    let other = StorageKey::record(package, "room", "other");
    assert!(
        store.claim(&other, "successor-b").unwrap(),
        "claims are per key"
    );
}

fn keys_never_alias(subject: &Subject<'_>) {
    let store = subject.store;
    let record = format!("alias-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let keys = [
        StorageKey::record("so.tilo.conformance", "room", record.clone()),
        StorageKey::record("so.tilo.conformance", "other-room", record.clone()),
        StorageKey::record("so.tilo.other", "room", record.clone()),
        StorageKey::room_index("so.tilo.conformance", record.clone()),
        StorageKey::package(format!("so.tilo.alias-{record}")),
    ];
    let leases: Vec<_> = keys.iter().map(|key| store.lease(key).unwrap()).collect();
    for (n, key) in keys.iter().enumerate() {
        store
            .write(key, WriteOp::Replace(vec![u8::try_from(n).unwrap()]), None)
            .unwrap();
    }
    (subject.settle)();
    for (n, key) in keys.iter().enumerate() {
        store.flush(Some(key)).unwrap();
        assert_eq!(
            store.read(key).unwrap().unwrap().bytes,
            vec![u8::try_from(n).unwrap()]
        );
    }
    drop(leases);
}

fn views_round_trip(subject: &Subject<'_>) {
    let store = subject.store;
    let instance = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let key = StorageKey::view("device-a", "room", instance);
    let neighbour = StorageKey::view("device-b", "room", instance);
    assert_eq!(store.read(&key).unwrap(), None);
    store
        .write(&key, WriteOp::Replace(b"view".to_vec()), None)
        .unwrap();
    store
        .write(&key, WriteOp::Replace(b"newer view".to_vec()), None)
        .unwrap();
    subject.committed(&key);
    assert_eq!(store.read(&key).unwrap().unwrap().bytes, b"newer view");
    assert_eq!(store.read(&neighbour).unwrap(), None);
}
