//! Settings opened beside another window on a page in a workspace whose host
//! holds Settings' record empty: an earlier window of it founded the record and
//! Settings keeps nothing there. The empty landing is the host's answer, so the
//! window builds Settings and shows the pane it was opened on instead of
//! waiting on its arrival view for a record that never comes.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::RefCell;
use std::rc::Rc;

use durable_store::{DurableStore, MemoryStore, StorageKey};
use ui::application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput};
use ui::automerge::{AutoCommit, transaction::Transactable as _};
use ui::headless::{HeadlessApp, fixture_runner_inputs};
use ui::record::{RecordCheckout, RecordId, RecordOpenError, RecordOwners};

use super::{Landed, answered, keep};

const WORKSPACE: &str = "workspace-a";

/// The page's storage key of `app`'s record `id` in the workspace, as its
/// replication keeps a landed record.
fn key(app: &str, id: &RecordId) -> StorageKey {
    StorageKey::document(app, WORKSPACE, id.storage_key())
}

#[test]
fn settings_opened_on_a_record_the_host_holds_empty_shows_its_pane() {
    ui::i18n::I18nManager::init("en-US");
    let definition = settings::definition();
    let app = definition.app_id();
    let memory = Rc::new(MemoryStore::queued());
    let mut owners = RecordOwners::with_store(Rc::clone(&memory) as Rc<dyn DurableStore>);
    // The page's store of Settings' records, which its workspace's host sends
    // only once a window asks for one.
    let stores = Rc::new(RefCell::new(Vec::new()));
    let store = crate::app_page::store(&mut owners, definition, WORKSPACE, true, &stores)
        .expect("the page opens Settings' records");

    // The window opens the workspace's record of Settings, which is not here:
    // the host is asked for it and the window waits.
    let record = RecordCheckout::new(
        RecordId::workspace_default(),
        definition.record_kind.clone(),
    );
    assert!(matches!(
        store.try_open_record(&record),
        Err(RecordOpenError::Arriving)
    ));
    let wire = tilo_desktop_surface::host::records::wire_record(&record.id).to_owned();

    // The host sends what it holds of it: a document with no change in it.
    let empty = AutoCommit::new().save();
    assert_eq!(
        keep(&*memory, &key(app, &record.id), empty, &[]).expect("the page reads it"),
        Landed::Empty,
        "an empty document is no record to keep"
    );
    assert!(
        answered(
            &stores.borrow(),
            app,
            &wire,
            ui::record::WorkspaceAnswer::HoldsNone
        ),
        "the empty landing answers the store that waited for it"
    );
    assert!(
        matches!(
            store.try_open_record(&record),
            Err(RecordOpenError::MissingRecord)
        ),
        "the window waits no longer: the record is new"
    );

    // The window builds Settings on it, as a new record, and opens the pane
    // the search result or the link named.
    let context = HostContext::prepare_new_record(
        definition,
        HostContextInputs {
            records: store,
            views: Rc::new(
                ui::application_view::InstanceViewStore::with_store(
                    Rc::clone(&memory) as Rc<dyn DurableStore>,
                    "page",
                    WORKSPACE,
                )
                .expect("the page keeps its windows' views"),
            ),
            device_id: "page".into(),
            instance_id: 7,
            permissions: ui::permissions::SharedPermissions::new(
                ui::permissions::PermissionContext::permissive(),
            ),
            author: ui::permissions::MemberId::person("person"),
            action_context: tilo_actions::ActionContext::default(),
        },
        record.id,
    )
    .expect("Settings prepares on the new record");
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .expect("Settings builds");
    let mut window = HeadlessApp::mount_sized(component, (1200, 800), fixture_runner_inputs())
        .expect("Settings mounts");
    window.settle();
    // The app's own `open`, at the anchor of the pane a page's link names
    // ([`crate::settings_link`]), as the page shows a search hit or a link.
    let own = window
        .resources()
        .into_iter()
        .map(|resource| resource.id.0)
        .find(|id| !id.starts_with("settings.section:") && window.publishes(id, "open"))
        .expect("Settings publishes its open");
    let pane = crate::settings_link::resource("settings://appearance").unwrap();
    window
        .call_on(&own, "open", serde_json::json!({ "anchor": pane }))
        .expect("Settings opens Appearance");
    for _ in 0..10 {
        window.settle();
        let _ = window.frame_mut().poll_frame();
    }
    let labels: Vec<String> = window
        .frame()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    assert!(
        labels.iter().any(|label| label == "Your colour"),
        "the Appearance pane is painted: {labels:?}"
    );
}

/// A landing that holds changes is kept, and one that merges nothing new into
/// what the page holds stands where it was.
#[test]
fn a_record_with_changes_is_kept_for_whoever_opens_it() {
    let memory = MemoryStore::queued();
    let id = RecordId::workspace_default();
    let key = key("so.tilo.notes", &id);
    let mut doc = AutoCommit::new();
    doc.put(ui::automerge::ROOT, "body", "hello").unwrap();
    let heads: Vec<[u8; 32]> = doc.get_heads().iter().map(|head| head.0).collect();
    let bytes = doc.save();
    let Landed::Kept(kept) = keep(&memory, &key, bytes.clone(), &heads).unwrap() else {
        panic!("a record with a change is kept");
    };
    assert_eq!(kept, doc.get_heads());
    assert_eq!(
        keep(&memory, &key, bytes, &heads).unwrap(),
        Landed::Kept(doc.get_heads())
    );
    assert_eq!(
        keep(&memory, &key, AutoCommit::new().save(), &[]).unwrap(),
        Landed::Kept(doc.get_heads()),
        "an empty landing leaves what the page holds"
    );
}
