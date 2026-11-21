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
use ui::application::{LaunchTarget, ProcessContext, ProcessContextInputs, RestoreInput};
use ui::automerge::{AutoCommit, ROOT, transaction::Transactable as _};
use ui::headless::{HeadlessApp, fixture_runner_inputs};
use ui::resource::{DocumentCheckout, DocumentId, ResourceOpenError, ResourceOwners};

use super::{Landed, answered, keep, sends_whole};

const WORKSPACE: &str = "workspace-a";

/// The page's storage key of `app`'s record `id` in the workspace, as its
/// replication keeps a landed record.
fn key(app: &str, id: &DocumentId) -> StorageKey {
    StorageKey::document(app, WORKSPACE, id.storage_key())
}

#[test]
fn settings_opened_on_a_record_the_host_holds_empty_shows_its_pane() {
    ui::i18n::I18nManager::init("en-US");
    let definition = settings::definition();
    let app = definition.app_id();
    let memory = Rc::new(MemoryStore::queued());
    let mut owners = ResourceOwners::with_store(Rc::clone(&memory) as Rc<dyn DurableStore>);
    // The page's store of Settings' records, which its workspace's host sends
    // only once a window asks for one.
    let stores = Rc::new(RefCell::new(Vec::new()));
    let store = crate::app_page::store(&mut owners, definition, WORKSPACE, true, &stores)
        .expect("the page opens Settings' records");

    // The window opens the workspace's record of Settings, which is not here:
    // the host is asked for it and the window waits.
    let record = DocumentCheckout::new(
        DocumentId::workspace_default(),
        definition.resource_type.clone(),
    );
    assert!(matches!(
        store.try_open_resource(&record),
        Err(ResourceOpenError::Arriving)
    ));
    let wire = tilo_desktop_surface::host::records::wire_document(&record.id).to_owned();

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
            ui::resource::WorkspaceAnswer::HoldsNone
        ),
        "the empty landing answers the store that waited for it"
    );
    assert!(
        matches!(
            store.try_open_resource(&record),
            Err(ResourceOpenError::MissingResource)
        ),
        "the window waits no longer: the record is new"
    );

    // The window builds Settings on it, as a new record, and opens the pane
    // the search result or the link named.
    let context = ProcessContext::prepare_new_resource(
        definition,
        ProcessContextInputs {
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
    // ([`ui::shell::settings_anchor`]), as the page shows a search hit or a link.
    let own = window
        .resources()
        .into_iter()
        .map(|resource| resource.id.0)
        .find(|id| !id.starts_with("settings.section:") && window.publishes(id, "open"))
        .expect("Settings publishes its open");
    let pane = ui::shell::settings_anchor("settings://appearance").unwrap();
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
    let id = DocumentId::workspace_default();
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

/// A document the page keeps names the heads its bytes stand at, so a reader on this thread that
/// already holds it (the replica that captured it) is found by them: a window opening the document
/// takes its copy instead of loading it again.
#[test]
fn a_kept_document_names_the_heads_a_reader_holds_it_at() {
    let memory = Rc::new(MemoryStore::queued());
    let id = DocumentId::parse("tab:00000000000000a1").expect("a document id");
    let key = key("so.tilo.notes", &id);
    let mut doc = AutoCommit::new();
    for page in 0..100 {
        doc.put(ROOT, format!("page-{page}"), text(page)).unwrap();
        doc.commit();
    }
    let heads = doc.committed_heads();
    let raw: Vec<[u8; 32]> = heads.iter().map(|head| head.0).collect();
    let bytes = doc.save();
    tilo_automerge::shared::clear();
    // The workspace's replica captured these bytes: a reader on this thread holds the document.
    tilo_automerge::shared::hold(&bytes, &doc);
    assert_eq!(
        keep(&*memory, &key, bytes, &raw).expect("the page reads it"),
        Landed::Kept(heads.clone())
    );
    let stored = memory.read(&key).unwrap().unwrap().bytes;
    let hint = memory
        .read_hint(&key)
        .expect("a kept document names its heads");
    assert_eq!(
        tilo_automerge::load_hint::heads_for(&hint, &stored),
        Some(heads),
        "the reader that holds it is found by the heads the document stands at"
    );

    // A newer landing names the heads it merged to, not the ones before it: the document a window
    // opens is the one the store now holds.
    let mut newer = doc.fork();
    newer
        .put(ROOT, "page-extra", "a page written elsewhere")
        .unwrap();
    newer.commit();
    let newer_heads = newer.committed_heads();
    let raw_newer: Vec<[u8; 32]> = newer_heads.iter().map(|head| head.0).collect();
    assert_eq!(
        keep(&*memory, &key, newer.save(), &raw_newer).expect("the page reads it"),
        Landed::Kept(newer_heads.clone())
    );
    let stored = memory.read(&key).unwrap().unwrap().bytes;
    let hint = memory
        .read_hint(&key)
        .expect("the newer landing names the heads it stands at");
    assert_eq!(
        tilo_automerge::load_hint::heads_for(&hint, &stored),
        Some(newer_heads)
    );
}

/// Text that does not compress much, as a page of a notebook imported from another editor's
/// export does not: `len` hex characters from a counter's own values.
fn text(seed: u64) -> String {
    use std::fmt::Write as _;

    let mut value = seed;
    let mut text = String::with_capacity(2_600);
    while text.len() < 2_560 {
        value = value
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let _ = write!(text, "{value:016x}");
    }
    text
}

#[test]
fn a_window_sends_its_copy_whole_only_when_the_workspace_holds_none_of_it() {
    // The workspace answered that it holds none: the window's copy is all there is.
    assert!(sends_whole(true, Some(false)));
    // It sent the document: the window's copy goes as what it changed past that.
    assert!(!sends_whole(true, Some(true)));
    // Asked and not yet answered: nothing goes until the answer says what it holds.
    assert!(!sends_whole(true, None));
    // Never asked: the window founded it, so the first write is the document whole.
    assert!(sends_whole(false, None));
}
