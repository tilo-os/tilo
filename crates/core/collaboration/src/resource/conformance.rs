//! What every [`DocumentPort`] does alike: the in-process [`MemoryWorkspace`] tests stand on, and
//! the workspace service an app reaches over its connection ([`ConnectionDocuments`]). Both run these
//! scenarios, so the port the tests use cannot drift from the one apps use.
//!
//! Each scenario starts the processes of an app it needs: `start` hands a new process's port onto
//! the workspace it names, and the process ends when the stores holding that port are dropped.
//!
//! [`MemoryWorkspace`]: super::MemoryWorkspace
//! [`ConnectionDocuments`]: super::ConnectionDocuments
#![allow(
    clippy::expect_used,
    reason = "a scenario's step failing is its assertion failing"
)]

use std::rc::Rc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT, ReadDoc as _};

use super::{
    DocumentAsk, DocumentCheckout, DocumentId, DocumentPort, DocumentRead, ResourceOpenOutcome,
    ResourceOwners, ResourceSaveStatus, ResourceType,
};

/// A new process of the app, its port onto the workspace named.
pub type Start<'a> = &'a mut dyn FnMut(&str) -> Rc<dyn DocumentPort>;

/// Run every scenario for `app`, in two workspaces the port reaches.
///
/// # Panics
/// A scenario's assertion fails: the port does not do what the workspace's one store does.
pub fn run(start: Start<'_>, app: &str, workspaces: [&str; 2]) {
    a_record_one_process_wrote_opens_in_the_next(start, app, workspaces[0]);
    two_processes_of_one_app_both_keep_their_changes(start, app, workspaces[0]);
    a_listing_names_what_the_workspace_holds(start, app, workspaces[0]);
    records_of_two_workspaces_never_alias(start, app, workspaces);
}

fn new_note() -> DocumentCheckout {
    DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"))
}

/// `key` set to `value` as the app `app`'s edit made outside any action is kept: as the app's own
/// change, named so every reader takes it.
fn put_text(doc: &mut AutoCommit, app: &str, key: &str, value: &str) {
    doc.put(ROOT, key, value).expect("a text puts");
    doc.commit_with(
        automerge::transaction::CommitOptions::default().with_message(
            tilo_actions::ActionContext::whole_edit_by(&tilo_actions::Acting::by_app(app), &*doc),
        ),
    );
}

fn text(doc: &AutoCommit, key: &str) -> Option<String> {
    doc.get(ROOT, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|scalar| scalar.into_string().ok())
}

/// What a new process of `app` reads of `key` in the record `note` of `workspace`.
fn read_back(
    start: Start<'_>,
    app: &str,
    workspace: &str,
    note: &DocumentCheckout,
    key: &str,
) -> Option<String> {
    let mut owners = ResourceOwners::writing(start(workspace));
    let store = owners.open(app, workspace).expect("the store opens");
    assert_eq!(
        store.try_open_resource(note).expect("the record opens"),
        ResourceOpenOutcome::Loaded,
        "{} is read from the workspace, not made anew",
        note.id
    );
    store.read(&note.id, |doc| text(doc, key)).flatten()
}

/// An app process writes what it typed to its workspace; once it is gone, the next process of the
/// app opens the record as it was left.
fn a_record_one_process_wrote_opens_in_the_next(start: Start<'_>, app: &str, workspace: &str) {
    let note = new_note();
    {
        let mut owners = ResourceOwners::writing(start(workspace));
        let store = owners.open(app, workspace).expect("the store opens");
        store.create_resource(&note).expect("the record is made");
        store.with(&note.id, |doc| {
            put_text(doc, app, "body", "typed before closing")
        });
        store.flush().expect("the workspace took what was typed");
        assert_eq!(
            store.save_status(&note.id).expect("the record is open"),
            ResourceSaveStatus::Saved
        );
    }
    assert_eq!(
        read_back(start, app, workspace, &note, "body").as_deref(),
        Some("typed before closing")
    );
}

/// Two processes of an app write one record at once, each after what the workspace held when it
/// opened it: both changes are the record's.
fn two_processes_of_one_app_both_keep_their_changes(start: Start<'_>, app: &str, workspace: &str) {
    let note = new_note();
    let mut first_owners = ResourceOwners::writing(start(workspace));
    let first = first_owners.open(app, workspace).expect("the store opens");
    first.create_resource(&note).expect("the record is made");
    first.flush().expect("the workspace took the record");
    let mut second_owners = ResourceOwners::writing(start(workspace));
    let second = second_owners.open(app, workspace).expect("the store opens");
    assert_eq!(
        second.try_open_resource(&note).expect("the record opens"),
        ResourceOpenOutcome::Loaded
    );
    first.with(&note.id, |doc| put_text(doc, app, "first", "one"));
    second.with(&note.id, |doc| put_text(doc, app, "second", "two"));
    first.flush().expect("the workspace took the first change");
    second
        .flush()
        .expect("the workspace took the second change");
    drop((first, first_owners, second, second_owners));
    assert_eq!(
        read_back(start, app, workspace, &note, "first").as_deref(),
        Some("one")
    );
    assert_eq!(
        read_back(start, app, workspace, &note, "second").as_deref(),
        Some("two")
    );
}

/// A listing of the app's records in a workspace names one it wrote, held there, and a record it
/// never wrote reads as absent.
fn a_listing_names_what_the_workspace_holds(start: Start<'_>, app: &str, workspace: &str) {
    let note = new_note();
    let port = start(workspace);
    {
        let mut owners = ResourceOwners::writing(port.clone());
        let store = owners.open(app, workspace).expect("the store opens");
        store.create_resource(&note).expect("the record is made");
        store.flush().expect("the workspace took the record");
    }
    let listing = port
        .read(app, workspace, None)
        .expect("the workspace lists");
    assert!(
        listing
            .iter()
            .any(|(id, read)| id == note.id.as_str() && *read == DocumentRead::Held),
        "the listing names {} as held: {listing:?}",
        note.id
    );
    let never = DocumentId::minted();
    let read = port
        .read(
            app,
            workspace,
            Some(vec![DocumentAsk {
                id: never.as_str().to_owned(),
                since: None,
            }]),
        )
        .expect("the workspace answers");
    assert_eq!(
        read,
        vec![(never.as_str().to_owned(), DocumentRead::Absent)],
        "a record nobody wrote is absent"
    );
}

/// One record id in two workspaces is two records: each workspace keeps what was written in it,
/// and lists only its own.
fn records_of_two_workspaces_never_alias(start: Start<'_>, app: &str, [one, two]: [&str; 2]) {
    let note = new_note();
    let only_in_one = new_note();
    for (workspace, body) in [(one, "kept in one"), (two, "kept in two")] {
        let mut owners = ResourceOwners::writing(start(workspace));
        let store = owners.open(app, workspace).expect("the store opens");
        store.create_resource(&note).expect("the record is made");
        store.with(&note.id, |doc| put_text(doc, app, "body", body));
        if workspace == one {
            store
                .create_resource(&only_in_one)
                .expect("the record is made");
        }
        store.flush().expect("the workspace took the record");
    }
    assert_eq!(
        read_back(start, app, one, &note, "body").as_deref(),
        Some("kept in one")
    );
    assert_eq!(
        read_back(start, app, two, &note, "body").as_deref(),
        Some("kept in two")
    );
    let listed = start(two)
        .read(app, two, None)
        .expect("the workspace lists");
    assert!(
        listed.iter().all(|(id, _)| id != only_in_one.id.as_str()),
        "a record of one workspace is not listed in the other: {listed:?}"
    );
}
