//! Two devices of one person converge on the person's own documents directly
//! over iroh, with no host in between and no workspace anywhere, and a device
//! of another identity is refused and holds nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::{addr, device, device_with, localhost_endpoint};

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use durable_store::DurableStore as _;
use ed25519_dalek::SigningKey;
use iroh::Endpoint;
use std::time::Duration;
use tilo_peer_state::{PERSONAL_ALPN, PersonalStore, PersonalSync, RefusalReason};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason};
use ui::permissions::UserId;
use ui::record::RecordKind;
use ui::record::person::PersonRecords;
use web_time::SystemTime;

const APP: &str = "tube";
const RECORD: &str = "prefs";
/// The app whose own record the devices share, and the kind of it.
const NOTES: &str = "so.tilo.notes";
const KIND: &str = "notes.reading-position";

/// A record with `field` set to `value`, as an app's first write of it.
fn record(field: &str, value: &str) -> collab_types::DocumentMutation {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, field, value).unwrap();
    doc.commit_edit(APP, field);
    collab_types::DocumentMutation::Snapshot(doc.save())
}

/// Changes that depend on no other write of the record.
fn change(field: &str, value: &str) -> collab_types::DocumentMutation {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, field, value).unwrap();
    doc.commit_edit(APP, field);
    collab_types::DocumentMutation::Incremental(doc.save())
}

fn field(doc: &[u8], name: &str) -> Option<String> {
    let doc = AutoCommit::load(doc).unwrap();
    let (value, _) = doc.get(ROOT, name).unwrap()?;
    value.into_string().ok()
}

/// Wait until `done` holds of `sync`'s store.
async fn until(sync: &PersonalSync, done: impl Fn(&PersonalStore) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !sync.read(&done) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the stores converge");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_of_one_person_converge_on_their_own_documents() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let root_key = root.verifying_key().to_bytes();
    let (a_key, a_cert) = device(&root, 2);
    let (b_key, b_cert) = device(&root, 3);
    let now = SystemTime::now();

    let a = PersonalStore::open(root_key, a_cert, now).unwrap();
    let b = PersonalStore::open(root_key, b_cert, now).unwrap();
    let a = PersonalSync::new(a, a_key.clone());
    let b = PersonalSync::new(b, b_key.clone());

    // A writes one of the person's records: no workspace exists anywhere.
    a.publish(|store, key, now| store.publish(APP, RECORD, &record("theme", "dark"), key, now))
        .unwrap();
    assert!(b.store().rows().is_empty());

    let a_endpoint = localhost_endpoint(&a_key).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(PERSONAL_ALPN, a.clone())
        .spawn();
    let b_endpoint = localhost_endpoint(&b_key).await;
    let link = tokio::spawn({
        let b = b.clone();
        let b_endpoint = b_endpoint.clone();
        let a_addr = addr(&a_endpoint);
        async move { b.connect(&b_endpoint, a_addr).await }
    });

    // B catches up from A, then reads the record A wrote.
    until(&b, |store| store.holds_document(APP, RECORD)).await;
    let read = b.store().document(APP, RECORD).unwrap();
    assert_eq!(field(&read, "theme").as_deref(), Some("dark"));
    assert_eq!(b.store().rows(), a.store().rows());

    // B writes; A converges to it live.
    b.publish(|store, key, now| store.publish(APP, RECORD, &change("accent", "blue"), key, now))
        .unwrap();
    until(&a, |store| {
        store
            .document(APP, RECORD)
            .is_some_and(|doc| field(&doc, "accent").is_some())
    })
    .await;
    let on_a = a.store().document(APP, RECORD).unwrap();
    assert_eq!(field(&on_a, "accent").as_deref(), Some("blue"));
    assert_eq!(field(&on_a, "theme").as_deref(), Some("dark"));
    assert_eq!(a.store().rows(), b.store().rows());

    // A device of another identity is refused the link and holds nothing.
    let other = SigningKey::from_bytes(&[9; 32]);
    let (c_key, c_cert) = device(&other, 10);
    let c = PersonalSync::new(
        PersonalStore::open(other.verifying_key().to_bytes(), c_cert, now).unwrap(),
        c_key.clone(),
    );
    let c_endpoint = localhost_endpoint(&c_key).await;
    let refused = c.connect(&c_endpoint, addr(&a_endpoint)).await;
    assert!(refused.is_err(), "a device of another identity is refused");
    assert!(c.store().rows().is_empty());
    assert!(a.read(|store| {
        !store
            .devices()
            .iter()
            .any(|cert| cert.device_public_key == c_key.verifying_key().to_bytes())
    }));

    // A store node holds none of the person's documents: it cannot even open
    // a store on their identity.
    let (_, node_cert) = device_with(&root, 4, DeviceCapabilities::host());
    assert!(PersonalStore::open(root_key, node_cert, now).is_err());

    link.abort();
    a_router.shutdown().await.unwrap();
    b_endpoint.close().await;
    c_endpoint.close().await;
}

/// One device's copy of the app's record of `KIND`, opened the way the app
/// opens it: in the person's own scope, under the id every device derives
/// for that kind.
fn app_record(person: &str) -> ui::record::person::StoredRecord {
    let mut owners =
        ui::record::RecordOwners::with_store(std::rc::Rc::new(durable_store::MemoryStore::new()));
    PersonRecords::open(&mut owners, NOTES, UserId(person.to_owned()))
        .unwrap()
        .open_record(RecordKind::new(KIND), &|_| Ok(()))
        .unwrap()
}

fn chapter(record: &ui::record::person::StoredRecord) -> Option<String> {
    record
        .read(|doc| {
            doc.get(ROOT, "chapter")
                .ok()
                .flatten()
                .and_then(|(value, _)| value.into_string().ok())
        })
        .flatten()
}

fn set_chapter(record: &ui::record::person::StoredRecord, value: &str) {
    record
        .edit(true, |doc| {
            doc.put(ROOT, "chapter", value)
                .map_err(|error| error.to_string())
        })
        .unwrap();
}

/// The record an app writes on one of the person's devices is the record the
/// same app reads on another of them, with no workspace anywhere: both
/// devices open one route per app and record in the person's own cluster, and
/// the person's store is what carries the document between them.
#[tokio::test(flavor = "multi_thread")]
async fn an_apps_record_written_on_one_device_is_read_on_another() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let root_key = root.verifying_key().to_bytes();
    let person = hex::encode(root_key);
    let (a_key, a_cert) = device(&root, 2);
    let (b_key, b_cert) = device(&root, 3);
    let now = SystemTime::now();

    let a = PersonalSync::new(
        PersonalStore::open(root_key, a_cert, now).unwrap(),
        a_key.clone(),
    );
    let b = PersonalSync::new(
        PersonalStore::open(root_key, b_cert, now).unwrap(),
        b_key.clone(),
    );

    // Both devices' apps hold the record, on the one route their identity
    // root derives: the store's own key.
    let on_a = app_record(&person);
    let on_b = app_record(&person);
    let route = on_a
        .route()
        .expect("the person's cluster carries the record");
    assert_eq!(route, on_b.route().expect("one route per app and record"));
    assert_eq!(route.cluster_id, collab_types::personal_cluster(&root_key));
    assert_eq!(route.cluster_id.to_string(), a.store().store_key());
    assert_eq!(route.cluster_id.to_string(), b.store().store_key());

    // The person reads on their laptop, and their app's write reaches their
    // own store on the route it opens.
    set_chapter(&on_a, "three");
    let written = on_a.document().expect("the record's document");
    a.publish(|store, key, now| {
        store.publish(
            NOTES,
            route.record.as_deref().unwrap(),
            &collab_types::DocumentMutation::Snapshot(written),
            key,
            now,
        )
    })
    .unwrap();
    assert_eq!(chapter(&on_b), None, "nothing has reached the phone yet");

    // Nothing is joined and no workspace exists: the devices meet over the
    // person's store alone.
    let a_endpoint = localhost_endpoint(&a_key).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(PERSONAL_ALPN, a.clone())
        .spawn();
    let b_endpoint = localhost_endpoint(&b_key).await;
    let link = tokio::spawn({
        let b = b.clone();
        let b_endpoint = b_endpoint.clone();
        let a_addr = addr(&a_endpoint);
        async move { b.connect(&b_endpoint, a_addr).await }
    });
    until(&b, |store| {
        store.holds_document(NOTES, route.record.as_deref().unwrap())
    })
    .await;

    // The phone's app merges what its own store holds, as it merges any
    // device's document of the record, and reads what the laptop wrote.
    let held = b
        .store()
        .document(NOTES, route.record.as_deref().unwrap())
        .unwrap();
    assert!(on_b.merge_peer_document(held).unwrap());
    assert_eq!(chapter(&on_b).as_deref(), Some("three"));

    // The other way round: the phone's edit reaches the laptop's app, and
    // undo on the phone takes back the phone's edit and no one else's.
    set_chapter(&on_b, "four");
    let written = on_b.document().expect("the record's document");
    b.publish(|store, key, now| {
        store.publish(
            NOTES,
            route.record.as_deref().unwrap(),
            &collab_types::DocumentMutation::Snapshot(written),
            key,
            now,
        )
    })
    .unwrap();
    until(&a, |store| {
        store
            .document(NOTES, route.record.as_deref().unwrap())
            .is_some_and(|doc| field(&doc, "chapter").as_deref() == Some("four"))
    })
    .await;
    let held = a
        .store()
        .document(NOTES, route.record.as_deref().unwrap())
        .unwrap();
    assert!(on_a.merge_peer_document(held).unwrap());
    assert_eq!(chapter(&on_a).as_deref(), Some("four"));
    assert!(on_b.undo());
    assert_eq!(chapter(&on_b).as_deref(), Some("three"));

    link.abort();
    a_router.shutdown().await.unwrap();
    b_endpoint.close().await;
}

/// What the person's app wrote while their records were still filed under a
/// workspace of their own: the record of `kind` under its own id, with the
/// history it had then. Answers the key it is kept under there.
fn kept_before_the_store(
    store: &durable_store::MemoryStore,
    retired: &str,
    id: &ui::record::RecordId,
    kind: &RecordKind,
) -> durable_store::StorageKey {
    let key = durable_store::StorageKey::document(NOTES, retired, id.storage_key());
    let mut written = AutoCommit::new();
    written.put(ROOT, "chapter", "three").unwrap();
    written.commit_edit(NOTES, "chapter");
    assert!(
        ui::record::init_scoped(&mut written, id, kind, retired, Some(retired)),
        "the record was stamped for that workspace then"
    );
    store
        .write(&key, durable_store::WriteOp::Replace(written.save()), None)
        .unwrap();
    key
}

/// The person's store carries the record `id` names to their other device,
/// which reads it as the record of theirs it is: filed there before their
/// store existed too, and taken into their own scope with the same identity,
/// history and attribution.
async fn carries_to_the_other_device(
    person: &str,
    retired: &str,
    old: &[u8],
    document: Vec<u8>,
    id: &ui::record::RecordId,
    taken: &str,
    a: (&SigningKey, &DeviceCert),
    b: (&SigningKey, &DeviceCert),
) {
    let now = SystemTime::now();
    let root_key = a.1.user_public_key;
    let a_sync = PersonalSync::new(
        PersonalStore::open(root_key, a.1.clone(), now).unwrap(),
        a.0.clone(),
    );
    let b_sync = PersonalSync::new(
        PersonalStore::open(root_key, b.1.clone(), now).unwrap(),
        b.0.clone(),
    );
    a_sync
        .publish(|store, key, now| {
            store.publish(
                NOTES,
                taken,
                &collab_types::DocumentMutation::Snapshot(document),
                key,
                now,
            )
        })
        .unwrap();
    let a_endpoint = localhost_endpoint(a.0).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(PERSONAL_ALPN, a_sync.clone())
        .spawn();
    let b_endpoint = localhost_endpoint(b.0).await;
    let link = tokio::spawn({
        let b_sync = b_sync.clone();
        let b_endpoint = b_endpoint.clone();
        let a_addr = addr(&a_endpoint);
        async move { b_sync.connect(&b_endpoint, a_addr).await }
    });
    until(&b_sync, |store| store.holds_document(NOTES, taken)).await;
    let held = b_sync.store().document(NOTES, taken).unwrap();
    assert_eq!(
        field(&held, "chapter").as_deref(),
        Some("three"),
        "the phone's store holds what the laptop kept before the store existed"
    );

    {
        // The phone kept the same record before their store existed: its own copy
        // of the same document, filed under the same workspace then. Opening it
        // takes it into their own scope here too, as the record it is.
        let phone = std::rc::Rc::new(durable_store::MemoryStore::new());
        let filed_there = durable_store::StorageKey::document(NOTES, retired, id.storage_key());
        phone
            .write(
                &filed_there,
                durable_store::WriteOp::Replace(old.to_vec()),
                None,
            )
            .unwrap();
        let mut owners = ui::record::RecordOwners::with_store(phone.clone());
        let on_b = PersonRecords::open(&mut owners, NOTES, UserId(person.to_owned()))
            .unwrap()
            .open_record(RecordKind::new(KIND), &|_| Ok(()))
            .unwrap();
        assert_eq!(&on_b.checkout().id, id);
        assert_eq!(
            chapter(&on_b).as_deref(),
            Some("three"),
            "the phone reads what it kept, under the same record id"
        );

        assert!(
            !phone.claim(&filed_there, "another-record").unwrap(),
            "the phone took its copy into their own scope too"
        );

        // Both devices' copies are one record: what the laptop's app did since
        // reaches the phone's, which is where their own history still is.
        assert!(on_b.merge_peer_document(held.clone()).unwrap());
        assert_eq!(chapter(&on_b).as_deref(), Some("three"));
        set_chapter(&on_b, "five");
        assert!(on_b.undo(), "their own edit is theirs to take back");
        assert_eq!(chapter(&on_b).as_deref(), Some("three"));

        // A device that never had the record at all starts without it, and reads
        // it out of their store as the same record.
        let fresh = std::rc::Rc::new(durable_store::MemoryStore::new());
        fresh
            .write(
                &durable_store::StorageKey::document(
                    NOTES,
                    ui::record::person::PERSONAL_SCOPE,
                    id.storage_key(),
                ),
                durable_store::WriteOp::Replace(held),
                None,
            )
            .unwrap();
        let mut fresh_owners = ui::record::RecordOwners::with_store(fresh);
        let on_c = PersonRecords::open(&mut fresh_owners, NOTES, UserId(person.to_owned()))
            .unwrap()
            .open_record(RecordKind::new(KIND), &|_| Ok(()))
            .unwrap();
        assert_eq!(&on_c.checkout().id, id);
        assert_eq!(chapter(&on_c).as_deref(), Some("three"));
    }

    link.abort();
    a_router.shutdown().await.unwrap();
    b_endpoint.close().await;
}

/// A record the person kept before their own store existed — filed under the
/// workspace their records were keyed by then — is still theirs: the app
/// opens the same record id with what was in it, on the person's own
/// cluster, and their store carries it to their other device.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_kept_before_the_store_reads_on_the_persons_other_device() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let root_key = root.verifying_key().to_bytes();
    let person = hex::encode(root_key);
    let (a_key, a_cert) = device(&root, 2);
    let (b_key, b_cert) = device(&root, 3);
    let store = std::rc::Rc::new(durable_store::MemoryStore::new());
    let kind = RecordKind::new(KIND);
    let id = ui::record::person::record_id(&kind);
    let retired = collab_types::retired_person_workspace(&root_key).to_string();
    let retired_key = kept_before_the_store(&store, &retired, &id, &kind);
    let old = store
        .read(&retired_key)
        .unwrap()
        .expect("the record was kept there")
        .bytes;

    // The app opens its record as it does on any device: the same id, now in
    // the person's own scope, with what the person had in it.
    let mut owners = ui::record::RecordOwners::with_store(store.clone());
    let records = PersonRecords::open(&mut owners, NOTES, UserId(person.clone())).unwrap();
    let on_a = records
        .open_record(RecordKind::new(KIND), &|_| Ok(()))
        .unwrap();
    assert_eq!(on_a.checkout().id, id, "the record keeps its id");
    assert_eq!(
        chapter(&on_a).as_deref(),
        Some("three"),
        "what the person had is still there"
    );
    let route = on_a.route().expect("the person's cluster carries it now");
    assert_eq!(route.cluster_id, collab_types::personal_cluster(&root_key));

    // It is the person's own record now: they change it and take the change
    // back, and what they had before the change is under it.
    set_chapter(&on_a, "four");
    assert_eq!(chapter(&on_a).as_deref(), Some("four"));
    assert!(on_a.undo(), "their own edit is theirs to take back");
    assert_eq!(chapter(&on_a).as_deref(), Some("three"));
    // The device's own store records the handover: no other record can take
    // the same one afterwards.
    assert!(
        !store.claim(&retired_key, "another-record").unwrap(),
        "the record was taken into the person's own scope"
    );

    // It is the person's own record from here on: another start reads it
    // with nothing left in the workspace it used to be filed under.
    records.flush().unwrap();
    drop(on_a);
    drop(records);
    drop(owners);
    let kept = store
        .read(&durable_store::StorageKey::document(
            NOTES,
            ui::record::person::PERSONAL_SCOPE,
            id.storage_key(),
        ))
        .unwrap();
    assert!(
        kept.is_some_and(|stored| !stored.bytes.is_empty()),
        "the record is written in the person's own scope"
    );
    store
        .write(
            &retired_key,
            durable_store::WriteOp::Replace(Vec::new()),
            None,
        )
        .unwrap();
    let mut reopened = ui::record::RecordOwners::with_store(store.clone());
    let again = PersonRecords::open(&mut reopened, NOTES, UserId(person.clone()))
        .unwrap()
        .open_record(RecordKind::new(KIND), &|_| Ok(()))
        .unwrap();
    assert_eq!(chapter(&again).as_deref(), Some("three"));

    let taken = route.record.clone().expect("the record has a name");
    carries_to_the_other_device(
        &person,
        &retired,
        &old,
        again.document().expect("the record's document"),
        &again.checkout().id,
        &taken,
        (&a_key, &a_cert),
        (&b_key, &b_cert),
    )
    .await;
}

/// The person's root revokes `device`: the fact one of their devices signs
/// and their others hand on.
fn revocation(root: &SigningKey, device: &SigningKey) -> RevocationEntry {
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: device.verifying_key().to_bytes(),
        revoked_at_ns: 1,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, root).unwrap();
    entry
}

/// The changes `change` writes, as the ids a refusal names and a history row
/// carries.
fn written_hashes(change: &collab_types::DocumentMutation) -> Vec<[u8; 32]> {
    let collab_types::DocumentMutation::Incremental(bytes) = change else {
        panic!("a change of the record");
    };
    collab_types::parse_changes(bytes)
        .unwrap()
        .iter()
        .map(|change| change.hash().0)
        .collect()
}

/// Two stores meeting over a link: `a` serves its records, `b` dials, and
/// both are up until the meeting parts.
struct Meeting {
    a_endpoint: Endpoint,
    b_endpoint: Endpoint,
    router: iroh::protocol::Router,
    link: tokio::task::JoinHandle<Result<(), String>>,
}

impl Meeting {
    /// Serve `a`'s store and dial it as `b`, each device at its own key.
    async fn of(
        a: &PersonalSync,
        a_key: &SigningKey,
        b: &PersonalSync,
        b_key: &SigningKey,
    ) -> Self {
        let a_endpoint = localhost_endpoint(a_key).await;
        let router = iroh::protocol::Router::builder(a_endpoint.clone())
            .accept(PERSONAL_ALPN, a.clone())
            .spawn();
        let b_endpoint = localhost_endpoint(b_key).await;
        let link = tokio::spawn({
            let b = b.clone();
            let b_endpoint = b_endpoint.clone();
            let a_addr = addr(&a_endpoint);
            async move { b.connect(&b_endpoint, a_addr).await }
        });
        Self {
            a_endpoint,
            b_endpoint,
            router,
            link,
        }
    }

    /// Part the two: the link ends, the server stops serving, and the dialling
    /// device's endpoint closes.
    async fn part(self) {
        self.link.abort();
        let _ = self.link.await;
        self.router.shutdown().await.unwrap();
        self.b_endpoint.close().await;
        self.a_endpoint.close().await;
    }
}

/// The device `cert` certifies, opening the store its identity keeps at
/// `root_key` as it stands now.
fn device_store(
    root_key: [u8; 32],
    cert: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
) -> PersonalSync {
    PersonalSync::new(
        PersonalStore::open(root_key, cert.clone(), now).unwrap(),
        key.clone(),
    )
}

/// A write a device refuses reaches its author, whose own history then says
/// where the write is not: on the link that carried the write, and, after
/// both stores restart from what they kept, on the next link. A device the
/// person took away writes nothing that lands.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_write_reaches_its_author_over_the_link_and_the_next_one() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let root_key = root.verifying_key().to_bytes();
    let (a_key, a_cert) = device(&root, 2);
    let (b_key, b_cert) = device(&root, 3);
    let now = SystemTime::now();
    let a = device_store(root_key, &a_cert, &a_key, now);
    let b = device_store(root_key, &b_cert, &b_key, now);
    a.publish(|store, key, now| store.publish(APP, RECORD, &record("theme", "dark"), key, now))
        .unwrap();

    // B catches up on the record, and A holds B's certificate.
    let meeting = Meeting::of(&a, &a_key, &b, &b_key).await;
    until(&b, |store| store.holds_document(APP, RECORD)).await;
    until(&a, |store| {
        store
            .devices()
            .iter()
            .any(|cert| cert.device_public_key == b_key.verifying_key().to_bytes())
    })
    .await;
    meeting.part().await;

    // The person's identity takes B's device away, and A hears it from a
    // sibling's hello while B is away: A says nothing of it to B yet.
    assert!(
        a.store()
            .write(|store| store.revoke(revocation(&root, &b_key)))
            .unwrap()
    );

    // B writes on with no link, and knows nothing yet.
    let font = change("font", "serif");
    let refused = written_hashes(&font);
    b.publish(move |store, key, now| store.publish(APP, RECORD, &font, key, now))
        .unwrap();
    assert!(
        b.store()
            .document(APP, RECORD)
            .is_some_and(|doc| field(&doc, "font").is_some())
    );
    // The store as B keeps it before any link: what a restart of B opens.
    let before_reply = b.store().durable().unwrap();

    // B links again: A's record takes none of the write, A files its verdict,
    // and B hears it on the stream that carried the write.
    let meeting = Meeting::of(&a, &a_key, &b, &b_key).await;
    until(&b, |store| !store.write_refusals().is_empty()).await;
    let heard = b.store().write_refusals();
    assert_eq!(heard.len(), 1, "one verdict, one author: {heard:?}");
    assert_eq!(heard[0].by, a_key.verifying_key().to_bytes());
    assert_eq!(heard[0].author, b_key.verifying_key().to_bytes());
    assert_eq!(heard[0].reason, RefusalReason::Revoked);
    assert_eq!(
        heard[0].changes, refused,
        "the history row the write left is the one the refusal names"
    );
    assert!(
        a.store()
            .document(APP, RECORD)
            .is_some_and(|doc| field(&doc, "font").is_none())
    );

    // The verdict is kept, not only said: both stores restart from the bytes
    // they kept, B's from before it heard anything.
    let a_kept = a.store().durable().unwrap();
    meeting.part().await;
    let a = PersonalSync::new(
        PersonalStore::restore(&a_kept, &a_cert, now).unwrap(),
        a_key.clone(),
    );
    let b = PersonalSync::new(
        PersonalStore::restore(&before_reply, &b_cert, now).unwrap(),
        b_key.clone(),
    );
    assert!(
        b.store().write_refusals().is_empty(),
        "this copy of B never heard A's verdict"
    );

    // The next link teaches B from what A kept: no live receipt was needed.
    let meeting = Meeting::of(&a, &a_key, &b, &b_key).await;
    until(&b, |store| {
        store
            .write_refusals()
            .iter()
            .any(|refusal| refusal.changes == refused)
    })
    .await;
    let heard = b.store().write_refusals();
    assert_eq!(heard.len(), 1, "a revoked device is told once: {heard:?}");
    assert_eq!(heard[0].reason, RefusalReason::Revoked);
    assert!(
        a.store()
            .document(APP, RECORD)
            .is_some_and(|doc| field(&doc, "font").is_none())
    );
    // What the person took away stays out of B's hands too.
    assert!(!b.store().holds_document(APP, RECORD));
    meeting.part().await;
}
