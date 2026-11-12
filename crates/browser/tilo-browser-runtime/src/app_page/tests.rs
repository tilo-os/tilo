//! A page showing one app in a workspace whose host sends a record only once
//! it is opened: what the page opens is never founded before the host answered
//! it holds none of it, and opens with the workspace's content once it comes.

use std::rc::Rc;

use durable_store::{DurableStore as _, MemoryStore, StorageKey, WriteOp};
use ui::application::{AppComponent, HostContext, RestoreInput};
use ui::automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use ui::launch::LaunchError;
use ui::record::{RecordKind, RecordOwners};
use ui::widget::IconDescriptor;

use super::*;

const PACKAGE: &str = "so.tilo.test";
const WORKSPACE: &str = "workspace-a";
const INSTANCE: u64 = 3;

/// A definition whose factory nothing here runs: what a page opens is decided
/// before its component is built.
fn definition(workspace_record: bool) -> ApplicationDefinition {
    fn no_component(
        _: HostContext,
        _: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, LaunchError> {
        Err(LaunchError::Operation("the factory must not run".into()))
    }
    ApplicationDefinition {
        app_id: PACKAGE.into(),
        title: "Page probe".into(),
        icon: IconDescriptor::Glyph("P"),
        record_kind: RecordKind::new("probe"),
        required: vec![],
        optional: vec![],
        opens: &[],
        workspace_record,
        member_record: false,
        opens_before_record: false,
        create: no_component,
    }
}

/// The record a page naming none opens, as [`opening`] names it.
fn default_record(definition: &ApplicationDefinition) -> RecordCheckout {
    RecordCheckout::new(
        RecordId::derived("default", &RecordId::for_tab(INSTANCE)),
        definition.record_kind.clone(),
    )
}

/// A page's store, whose records come from its workspace's host.
fn page(definition: &ApplicationDefinition) -> (Rc<MemoryStore>, RecordStore) {
    let memory = Rc::new(MemoryStore::queued());
    let mut owners =
        RecordOwners::with_store(Rc::clone(&memory) as Rc<dyn durable_store::DurableStore>);
    let stores = Rc::new(RefCell::new(Vec::new()));
    let store = store(&mut owners, definition, WORKSPACE, true, &stores).unwrap();
    (memory, store)
}

/// The workspace's copy of `want`, as another device wrote it.
fn written_elsewhere(want: &RecordCheckout, body: &str) -> Vec<u8> {
    let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open(PACKAGE, WORKSPACE).unwrap();
    store
        .reserve_record(want)
        .unwrap()
        .initialize(|doc| {
            let _ = doc.put(&ROOT, "body", body);
            Ok(())
        })
        .unwrap();
    store.replication_snapshot(&want.id).unwrap().0
}

/// What the page's replication does with a record the host sent and no window
/// holds open: it keeps it in the page's storage and tells the store.
fn arrives(memory: &MemoryStore, store: &RecordStore, id: &RecordId, bytes: Vec<u8>) {
    // An app's own document of the workspace is the workspace's index itself;
    // every other record is a document beneath it.
    let key = if id.is_package_primary() {
        StorageKey::workspace_index(PACKAGE, WORKSPACE)
    } else {
        StorageKey::record(PACKAGE, WORKSPACE, id.storage_key())
    };
    memory.write(&key, WriteOp::Replace(bytes), None).unwrap();
    store.note_kept();
}

/// What the record holds under `body`.
fn body(store: &RecordStore, id: &RecordId) -> Option<String> {
    let doc = store.read(id, AutoCommit::clone)?;
    match doc.get(&ROOT, "body").ok().flatten()?.0 {
        ui::automerge::Value::Scalar(scalar) => match scalar.into_owned() {
            ui::automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        ui::automerge::Value::Object(_) => None,
    }
}

/// A record the workspace still sends (or still has to say it holds none of)
/// is not a new one, and opens with the workspace's content once it is here.
#[test]
fn a_record_the_workspace_still_sends_is_not_founded_as_new() {
    let definition = definition(false);
    let (memory, store) = page(&definition);
    let want = default_record(&definition);

    assert!(matches!(
        opening(&definition, &store, None, false, INSTANCE).unwrap(),
        Opening::Arriving
    ));
    assert_eq!(
        store.wanted_records(),
        std::slice::from_ref(&want.id),
        "asked of the workspace"
    );
    assert!(!store.is_ready(&want.id), "nothing was founded");

    let theirs = written_elsewhere(&want, "the workspace's");
    arrives(&memory, &store, &want.id, theirs);
    let Opening::Target(LaunchTarget::Record { checkout, .. }) =
        opening(&definition, &store, None, false, INSTANCE).unwrap()
    else {
        panic!("the record the workspace sent is what opens");
    };
    assert_eq!(checkout.id, want.id);
    assert_eq!(
        body(&store, &want.id).as_deref(),
        Some("the workspace's"),
        "opened with the workspace's content"
    );
    assert!(store.wanted_records().is_empty());
}

/// Only the workspace's answer that it holds none of the record makes it a new
/// one, and the new one is the record the page named.
#[test]
fn a_record_is_new_only_once_the_workspace_answered_it_holds_none_of_it() {
    let definition = definition(false);
    let (_memory, store) = page(&definition);
    let want = default_record(&definition);

    assert!(matches!(
        opening(&definition, &store, None, false, INSTANCE).unwrap(),
        Opening::Arriving
    ));
    store.note_workspace_answered(want.id.clone());
    let Opening::New(id) = opening(&definition, &store, None, false, INSTANCE).unwrap() else {
        panic!("a record the workspace does not hold is a new one");
    };
    assert_eq!(id, want.id, "the new record is the one the page named");
    assert!(store.wanted_records().is_empty());
}

/// An app that keeps one document of the workspace waits for it too: it is the
/// workspace's own that a page naming no record opens, and a page that founded
/// a fresh one before the answer forked it.
#[test]
fn an_apps_own_document_of_the_workspace_waits_for_it_too() {
    let definition = definition(true);
    let (memory, store) = page(&definition);
    let want = RecordCheckout::package_primary(definition.record_kind.clone());

    assert!(matches!(
        opening(&definition, &store, None, false, INSTANCE).unwrap(),
        Opening::Arriving
    ));
    let theirs = written_elsewhere(&want, "the workspace's own");
    arrives(&memory, &store, &want.id, theirs);
    let Opening::Target(LaunchTarget::Record { checkout, .. }) =
        opening(&definition, &store, None, false, INSTANCE).unwrap()
    else {
        panic!("the workspace's own document is what opens");
    };
    assert_eq!(checkout.id, want.id);

    // A workspace that holds none of it: the app starts it.
    let (_memory, store) = page(&definition);
    assert!(matches!(
        opening(&definition, &store, None, false, INSTANCE).unwrap(),
        Opening::Arriving
    ));
    store.note_workspace_answered(want.id);
    assert!(matches!(
        opening(&definition, &store, None, false, INSTANCE).unwrap(),
        Opening::Target(LaunchTarget::PrimaryNew)
    ));
}

/// A page whose store keeps its own records creates a missing one, as before:
/// no workspace sends it.
#[test]
fn a_store_of_its_own_takes_a_missing_record_as_new() {
    let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open(PACKAGE, WORKSPACE).unwrap();
    let definition = definition(false);
    let want = default_record(&definition);
    let Opening::New(id) = opening(&definition, &store, None, false, INSTANCE).unwrap() else {
        panic!("a record of the page's own is new");
    };
    assert_eq!(id, want.id);
    assert!(store.wanted_records().is_empty());
}
