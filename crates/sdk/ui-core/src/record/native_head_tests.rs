//! A native app's store reads its records through its broker: a record it holds
//! nothing of is asked for, and the store never founds one until the workspace
//! answered it holds none. A chain's head is the one such record that is never a
//! document at all — it names whichever of the chain's parts is the newest — so
//! opening it asks the workspace for that part and opens nothing here, however
//! the broker answers.

use std::rc::Rc;

use durable_store::MemoryStore;

use crate::collaboration::CollaborationManager;
use crate::collaboration::native_initial::NativeConstruction;
use crate::record::{
    RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordOwners, RecordStore,
};

const PACKAGE: &str = "so.tilo.test";

/// A native app's store of `workspace`'s records, reading them through `broker`.
fn native_store(
    workspace: uuid::Uuid,
    broker: CollaborationManager,
) -> (NativeConstruction, RecordStore) {
    let construction = NativeConstruction::enter(broker).expect("the app's construction");
    let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners
        .open(PACKAGE, &workspace.to_string())
        .expect("the workspace's records");
    (construction, store)
}

/// The name of the newest part of a chat's chain of messages.
fn head() -> RecordCheckout {
    RecordCheckout::new(
        RecordId::derived("messages", &RecordId::workspace_default()).chain_head(),
        RecordKind::new("chat.part"),
    )
}

/// Opening a chain's head asks the workspace for whichever part is its newest and
/// opens nothing here: a document founded under the head's name would be a record
/// of the app's own beside the chain's parts, published to everyone.
#[test]
fn a_native_apps_store_asks_for_a_chains_head_and_never_founds_one() {
    let workspace = uuid::Uuid::new_v4();
    let (broker, _joins) = CollaborationManager::native_initial_fixture_for_test(
        PACKAGE,
        workspace,
        automerge::AutoCommit::new().save(),
    );
    let (construction, store) = native_store(workspace, broker);
    let head = head();
    assert!(
        matches!(store.try_open_record(&head), Err(RecordOpenError::Arriving)),
        "a chain's head is on its way, never a record made here"
    );
    assert!(!store.is_ready(&head.id), "no document lives under a head");
    assert!(
        matches!(store.try_open_record(&head), Err(RecordOpenError::Arriving)),
        "and still on its way while the workspace has not answered"
    );
    drop(construction);
}

/// The answer that a chain has no part at all is the workspace's word, not this
/// device's guess: until it comes, the head is on its way, and then it is missing —
/// which is what starts the chain's first part.
#[test]
fn a_chains_head_is_missing_only_once_the_workspace_answered_it_has_no_part() {
    let workspace = uuid::Uuid::new_v4();
    let (broker, _joins) = CollaborationManager::native_initial_fixture_for_test(
        PACKAGE,
        workspace,
        automerge::AutoCommit::new().save(),
    );
    let (construction, store) = native_store(workspace, broker);
    let head = head();
    assert!(matches!(
        store.try_open_record(&head),
        Err(RecordOpenError::Arriving)
    ));
    assert!(!store.workspace_answered(&head.id));
    store.note_workspace_answered(head.id.clone());
    assert!(store.workspace_answered(&head.id));
    assert!(
        !store.is_ready(&head.id),
        "the workspace has no part at all"
    );
    drop(construction);
}
