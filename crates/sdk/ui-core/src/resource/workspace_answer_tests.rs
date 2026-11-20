//! A store whose records come from its workspace's hosts never takes a record
//! that is not here for a new one before they answered: a public workspace
//! sends a newcomer only its governance and shared shell, so every other
//! record is "not here" until it is opened, and a window that founded a fresh
//! one then overwrote or forked the workspace's own.

use super::*;
use std::rc::Rc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT};
use durable_store::{DurableStore as _, MemoryStore, WriteOp};

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
fn written_elsewhere(want: &DocumentCheckout, body: &str) -> Vec<u8> {
    let mut owners = ResourceOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open(PACKAGE, WORKSPACE).unwrap();
    store
        .reserve_resource(want)
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
fn arrives(memory: &MemoryStore, store: &ResourceStore, want: &DocumentCheckout, bytes: Vec<u8>) {
    let key = durable_store::StorageKey::document(PACKAGE, WORKSPACE, want.id.storage_key());
    memory.write(&key, WriteOp::Replace(bytes), None).unwrap();
    store.note_kept();
}

fn page() -> (Rc<MemoryStore>, ResourceStore) {
    let memory = Rc::new(MemoryStore::queued());
    let mut owners =
        ResourceOwners::with_store(Rc::clone(&memory) as Rc<dyn durable_store::DurableStore>);
    let store = owners.open(PACKAGE, WORKSPACE).unwrap();
    store.ask_workspace_for_missing();
    (memory, store)
}

tilo_test_env::scenario! {
    fn a_record_is_new_only_by_the_hosts_answer;

    phase an_existing_record_opened_before_the_host_answered_is_never_founded_anew;
    // A record opened before the host answered is on its way, never missing:
    // nothing that opens it (a window's launch, an app's "open or create")
    // starts a fresh one, and it opens with the workspace's content once it is
    // here.
    {
        let (memory, store) = page();
        let want = DocumentCheckout::new(DocumentId::workspace_default(), ResourceType::new("note"));
        let theirs = written_elsewhere(&want, "the workspace's");

        // What a list checks for every record it could name asks for nothing.
        assert!(matches!(
            store.open_if_held(&want),
            Err(ResourceOpenError::MissingResource)
        ));
        assert!(store.wanted_resources().is_empty());
        assert!(matches!(
            store.try_open_resource(&want),
            Err(ResourceOpenError::Arriving)
        ));
        assert_eq!(
            store.wanted_resources(),
            std::slice::from_ref(&want.id),
            "asked for"
        );
        // The "open it, or create it where nothing holds it" every stored
        // record goes through waits too.
        assert!(matches!(
            super::person::open_in(&store, &want, &|doc| {
                put_text(doc, "body", "a fresh one");
                Ok(())
            }),
            Err(ResourceOpenError::Arriving)
        ));
        assert!(!store.is_ready(&want.id), "nothing was founded");

        arrives(&memory, &store, &want, theirs);
        assert!(store.try_open_resource(&want).is_ok());
        assert_eq!(
            store.read(&want.id, |doc| text(doc, "body")).unwrap(),
            Some("the workspace's".to_owned())
        );
        assert!(store.wanted_resources().is_empty());
    }

    phase a_record_is_new_only_once_the_host_said_it_holds_none_of_it;
    // Only the host's answer that it holds none of a record makes it new; the
    // answer for one record says nothing of another.
    {
        let (_memory, store) = page();
        let new = DocumentCheckout::new(DocumentId::window_default(7), ResourceType::new("note"));
        let other = DocumentCheckout::new(DocumentId::window_default(8), ResourceType::new("note"));
        assert!(matches!(
            store.try_open_resource(&new),
            Err(ResourceOpenError::Arriving)
        ));
        assert!(matches!(
            store.try_open_resource(&other),
            Err(ResourceOpenError::Arriving)
        ));

        store.note_workspace_answered(new.id.clone());
        assert!(matches!(
            store.try_open_resource(&new),
            Err(ResourceOpenError::MissingResource)
        ));
        assert!(matches!(
            store.try_open_resource(&other),
            Err(ResourceOpenError::Arriving)
        ));
        assert_eq!(store.wanted_resources(), std::slice::from_ref(&other.id));
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

    phase another_persons_own_record_is_answered_as_theirs_and_never_founded;
    // The hosts send a person's own record to that person's devices alone, and
    // answer anyone else's ask as not theirs: opening it then says whose it is,
    // never waits, and never starts one in its place.
    {
        let (_memory, store) = page();
        let theirs = DocumentCheckout::new(
            DocumentId::of_member(&"b0".repeat(32), "mail.copies").unwrap(),
            ResourceType::new("mail.copies"),
        );
        assert!(matches!(
            store.try_open_resource(&theirs),
            Err(ResourceOpenError::Arriving)
        ));
        store.note_workspace_answer(theirs.id.clone(), WorkspaceAnswer::NotYours);
        assert_eq!(
            store.workspace_answer(&theirs.id),
            Some(WorkspaceAnswer::NotYours)
        );
        assert!(store.wanted_resources().is_empty(), "not asked for again");
        assert!(matches!(
            store.try_open_resource(&theirs),
            Err(ResourceOpenError::NotYours)
        ));
        assert!(matches!(
            super::person::open_in(&store, &theirs, &|doc| {
                put_text(doc, "body", "a fresh one");
                Ok(())
            }),
            Err(ResourceOpenError::NotYours)
        ));
        assert!(!store.is_ready(&theirs.id), "nothing was founded");
    }

    phase a_store_of_its_own_takes_a_missing_record_as_new;
    // A store whose records are its own (no workspace sends them) creates a
    // missing record as before.
    {
        let mut owners = ResourceOwners::with_store(Rc::new(MemoryStore::queued()));
        let store = owners.open(PACKAGE, WORKSPACE).unwrap();
        let want = DocumentCheckout::new(DocumentId::workspace_default(), ResourceType::new("note"));
        assert!(matches!(
            store.try_open_resource(&want),
            Err(ResourceOpenError::MissingResource)
        ));
        assert!(store.wanted_resources().is_empty());
    }
}
