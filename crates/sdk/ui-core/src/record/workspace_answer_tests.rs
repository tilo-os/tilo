//! A store whose records come from its workspace's hosts never takes a record
//! that is not here for a new one before they answered: a public workspace
//! sends a newcomer only its governance and shared desktop, so every other
//! record is "not here" until it is opened, and a window that founded a fresh
//! one then overwrote or forked the workspace's own.

use super::*;
use durable_store::{MemoryStore, WriteOp};

const PACKAGE: &str = "so.tilo.test";
const WORKSPACE: &str = "workspace-a";

fn put_text(doc: &mut AutoCommit, key: &str, value: &str) {
    let _ = doc.put(&ROOT, key, value);
}

fn text(doc: &AutoCommit, key: &str) -> Option<String> {
    str_at(doc, &ROOT, key)
}

/// The workspace's copy of `want`, as another device wrote it: its saved
/// document.
fn written_elsewhere(want: &RecordCheckout, body: &str) -> Vec<u8> {
    let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open(PACKAGE, WORKSPACE).unwrap();
    store
        .reserve_record(want)
        .unwrap()
        .initialize(|doc| {
            put_text(doc, "body", body);
            Ok(())
        })
        .unwrap();
    store.replication_snapshot(&want.id).unwrap().0
}

/// What the page's replication does with a record the host sent and no
/// window holds open: it keeps it in the page's storage and tells the store.
fn arrives(memory: &MemoryStore, store: &RecordStore, want: &RecordCheckout, bytes: Vec<u8>) {
    let key = durable_store::StorageKey::record(PACKAGE, WORKSPACE, want.id.storage_key());
    memory.write(&key, WriteOp::Replace(bytes), None).unwrap();
    store.note_kept();
}

fn page() -> (Rc<MemoryStore>, RecordStore) {
    let memory = Rc::new(MemoryStore::queued());
    let mut owners =
        RecordOwners::with_store(Rc::clone(&memory) as Rc<dyn durable_store::DurableStore>);
    let store = owners.open(PACKAGE, WORKSPACE).unwrap();
    store.ask_workspace_for_missing();
    (memory, store)
}

/// A record opened before the host answered is on its way, never missing:
/// nothing that opens it (a window's launch, an app's "open or create")
/// starts a fresh one, and it opens with the workspace's content once it is
/// here.
#[test]
fn an_existing_record_opened_before_the_host_answered_is_never_founded_anew() {
    let (memory, store) = page();
    let want = RecordCheckout::new(RecordId::workspace_default(), RecordKind::new("note"));
    let theirs = written_elsewhere(&want, "the workspace's");

    // What a list checks for every record it could name asks for nothing.
    assert!(matches!(
        store.open_if_held(&want),
        Err(RecordOpenError::MissingRecord)
    ));
    assert!(store.wanted_records().is_empty());
    assert!(matches!(
        store.try_open_record(&want),
        Err(RecordOpenError::Arriving)
    ));
    assert_eq!(store.wanted_records(), [want.id.clone()], "asked for");
    // The "open it, or create it where nothing holds it" every stored
    // record goes through waits too.
    assert!(matches!(
        super::person::open_in(&store, &want, &|doc| {
            put_text(doc, "body", "a fresh one");
            Ok(())
        }),
        Err(RecordOpenError::Arriving)
    ));
    assert!(!store.is_ready(&want.id), "nothing was founded");

    arrives(&memory, &store, &want, theirs);
    assert!(store.try_open_record(&want).is_ok());
    assert_eq!(
        store.read(&want.id, |doc| text(doc, "body")).unwrap(),
        Some("the workspace's".to_owned())
    );
    assert!(store.wanted_records().is_empty());
}

/// Only the host's answer that it holds none of a record makes it new; the
/// answer for one record says nothing of another.
#[test]
fn a_record_is_new_only_once_the_host_said_it_holds_none_of_it() {
    let (_memory, store) = page();
    let new = RecordCheckout::new(RecordId::window_default(7), RecordKind::new("note"));
    let other = RecordCheckout::new(RecordId::window_default(8), RecordKind::new("note"));
    assert!(matches!(
        store.try_open_record(&new),
        Err(RecordOpenError::Arriving)
    ));
    assert!(matches!(
        store.try_open_record(&other),
        Err(RecordOpenError::Arriving)
    ));

    store.note_workspace_answered(new.id.clone());
    assert!(matches!(
        store.try_open_record(&new),
        Err(RecordOpenError::MissingRecord)
    ));
    assert!(matches!(
        store.try_open_record(&other),
        Err(RecordOpenError::Arriving)
    ));
    assert_eq!(store.wanted_records(), [other.id.clone()]);
    super::person::open_in(&store, &new, &|doc| {
        put_text(doc, "body", "mine");
        Ok(())
    })
    .unwrap();
    assert_eq!(
        store.read(&new.id, |doc| text(doc, "body")).unwrap(),
        Some("mine".to_owned())
    );
}

/// A store whose records are its own (no workspace sends them) creates a
/// missing record as before.
#[test]
fn a_store_of_its_own_takes_a_missing_record_as_new() {
    let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open(PACKAGE, WORKSPACE).unwrap();
    let want = RecordCheckout::new(RecordId::workspace_default(), RecordKind::new("note"));
    assert!(matches!(
        store.try_open_record(&want),
        Err(RecordOpenError::MissingRecord)
    ));
    assert!(store.wanted_records().is_empty());
}
