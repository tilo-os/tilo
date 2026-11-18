//! A page showing one app in a workspace whose host sends a record only once
//! it is opened: what the page opens is never founded before the host answered
//! it holds none of it, and opens with the workspace's content once it comes.

use std::rc::Rc;

use durable_store::{DurableStore as _, MemoryStore, StorageKey, WriteOp};
use ui::application::{AppComponent, ProcessContext, ProcessContextInputs, RestoreInput};
use ui::application_view::SavedInstanceView;
use ui::automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use ui::launch::LaunchError;
use ui::permissions::MemberId;
use ui::resource::{ResourceOwners, ResourceType};
use ui::widget::IconDescriptor;

use super::*;

const PACKAGE: &str = "so.tilo.test";
const WORKSPACE: &str = "workspace-a";
const INSTANCE: u64 = 3;

/// A definition whose factory nothing here runs: what a page opens is decided
/// before its component is built.
fn definition(workspace_resource: bool) -> ApplicationDefinition {
    fn no_component(
        _: ProcessContext,
        _: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, LaunchError> {
        Err(LaunchError::Operation("the factory must not run".into()))
    }
    ApplicationDefinition {
        app_id: PACKAGE.into(),
        title: "Page probe".into(),
        icon: IconDescriptor::Glyph("P"),
        resource_type: ResourceType::new("probe"),
        required: vec![],
        optional: vec![],
        opens: &[],
        workspace_resource,
        member_resource: false,
        opens_before_resource: false,
        create: no_component,
    }
}

/// The person the page opens as: their user root key, which names their own
/// records.
fn author() -> MemberId {
    MemberId::person("ad".repeat(32))
}

/// The record a page naming none opens, as [`opening`] names it.
fn default_resource(definition: &ApplicationDefinition) -> DocumentCheckout {
    DocumentCheckout::new(
        DocumentId::derived("default", &DocumentId::for_tab(INSTANCE)),
        definition.resource_type.clone(),
    )
}

/// A page's store, whose records come from its workspace's host.
fn page(definition: &ApplicationDefinition) -> (Rc<MemoryStore>, ResourceStore) {
    let memory = Rc::new(MemoryStore::queued());
    let mut owners =
        ResourceOwners::with_store(Rc::clone(&memory) as Rc<dyn durable_store::DurableStore>);
    let stores = Rc::new(RefCell::new(Vec::new()));
    let store = store(&mut owners, definition, WORKSPACE, true, &stores).unwrap();
    (memory, store)
}

/// The workspace's copy of `want`, as another device wrote it.
fn written_elsewhere(want: &DocumentCheckout, body: &str) -> Vec<u8> {
    let mut owners = ResourceOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open(PACKAGE, WORKSPACE).unwrap();
    store
        .reserve_resource(want)
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
fn arrives(memory: &MemoryStore, store: &ResourceStore, id: &DocumentId, bytes: Vec<u8>) {
    // An app's own document of the workspace is the workspace's index itself;
    // every other record is a document beneath it.
    let key = if id.is_package_primary() {
        StorageKey::workspace_index(PACKAGE, WORKSPACE)
    } else {
        StorageKey::document(PACKAGE, WORKSPACE, id.storage_key())
    };
    memory.write(&key, WriteOp::Replace(bytes), None).unwrap();
    store.note_kept();
}

/// What the record holds under `body`.
fn body(store: &ResourceStore, id: &DocumentId) -> Option<String> {
    let doc = store.read(id, AutoCommit::clone)?;
    match doc.get(&ROOT, "body").ok().flatten()?.0 {
        ui::automerge::Value::Scalar(scalar) => match scalar.into_owned() {
            ui::automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        ui::automerge::Value::Object(_) => None,
    }
}

tilo_test_env::scenario! {
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test arithmetic: the lint exempts the body of a test function, not the closure a phase is"
    )]
    fn a_page_opens_what_its_workspace_holds_of_a_record;

    phase a_record_the_workspace_still_sends_is_not_founded_as_new;
    // A record the workspace still sends (or still has to say it holds none of)
    // is not a new one, and opens with the workspace's content once it is here.
    {
        let definition = definition(false);
        let (memory, store) = page(&definition);
        let want = default_resource(&definition);

        assert!(matches!(
            opening(&definition, &store, None, false, INSTANCE, &author()).unwrap(),
            Opening::Arriving
        ));
        assert_eq!(
            store.wanted_resources(),
            std::slice::from_ref(&want.id),
            "asked of the workspace"
        );
        assert!(!store.is_ready(&want.id), "nothing was founded");

        let theirs = written_elsewhere(&want, "the workspace's");
        arrives(&memory, &store, &want.id, theirs);
        let Opening::Target(LaunchTarget::Record { checkout, .. }) =
            opening(&definition, &store, None, false, INSTANCE, &author()).unwrap()
        else {
            panic!("the record the workspace sent is what opens");
        };
        assert_eq!(checkout.id, want.id);
        assert_eq!(
            body(&store, &want.id).as_deref(),
            Some("the workspace's"),
            "opened with the workspace's content"
        );
        assert!(store.wanted_resources().is_empty());
    }

    phase a_record_is_new_only_once_the_workspace_answered_it_holds_none_of_it;
    // Only the workspace's answer that it holds none of the record makes it a new
    // one, and the new one is the record the page named.
    {
        let definition = definition(false);
        let (_memory, store) = page(&definition);
        let want = default_resource(&definition);

        assert!(matches!(
            opening(&definition, &store, None, false, INSTANCE, &author()).unwrap(),
            Opening::Arriving
        ));
        store.note_workspace_answered(want.id.clone());
        let Opening::New(id) = opening(&definition, &store, None, false, INSTANCE, &author()).unwrap() else {
            panic!("a record the workspace does not hold is a new one");
        };
        assert_eq!(id, want.id, "the new record is the one the page named");
        assert!(store.wanted_resources().is_empty());
    }

    phase an_apps_own_document_of_the_workspace_waits_for_it_too;
    // An app that keeps one document of the workspace waits for it too: it is the
    // workspace's own that a page naming no record opens, and a page that founded
    // a fresh one before the answer forked it.
    {
        let definition = definition(true);
        let (memory, store) = page(&definition);
        let want = DocumentCheckout::package_primary(definition.resource_type.clone());

        assert!(matches!(
            opening(&definition, &store, None, false, INSTANCE, &author()).unwrap(),
            Opening::Arriving
        ));
        let theirs = written_elsewhere(&want, "the workspace's own");
        arrives(&memory, &store, &want.id, theirs);
        let Opening::Target(LaunchTarget::Record { checkout, .. }) =
            opening(&definition, &store, None, false, INSTANCE, &author()).unwrap()
        else {
            panic!("the workspace's own document is what opens");
        };
        assert_eq!(checkout.id, want.id);

        // A workspace that holds none of it: the app starts it.
        let (_memory, store) = page(&definition);
        assert!(matches!(
            opening(&definition, &store, None, false, INSTANCE, &author()).unwrap(),
            Opening::Arriving
        ));
        store.note_workspace_answered(want.id);
        assert!(matches!(
            opening(&definition, &store, None, false, INSTANCE, &author()).unwrap(),
            Opening::Target(LaunchTarget::PrimaryNew)
        ));
    }

    phase a_store_of_its_own_takes_a_missing_record_as_new;
    // A page whose store keeps its own records creates a missing one, as before:
    // no workspace sends it.
    {
        let mut owners = ResourceOwners::with_store(Rc::new(MemoryStore::queued()));
        let store = owners.open(PACKAGE, WORKSPACE).unwrap();
        let definition = definition(false);
        let want = default_resource(&definition);
        let Opening::New(id) = opening(&definition, &store, None, false, INSTANCE, &author()).unwrap() else {
            panic!("a record of the page's own is new");
        };
        assert_eq!(id, want.id);
        assert!(store.wanted_resources().is_empty());
    }

    phase an_app_each_member_keeps_opens_the_persons_own_record;
    // An app whose record is each member's own (Settings: the person's appearance, accounts and
    // devices) opens the person's own record, never the workspace's: it waits for the workspace
    // to send the one another device of theirs wrote, then starts it as theirs.
    {
        let mut definition = definition(true);
        definition.member_resource = true;
        let own = DocumentId::of_member(author().id(), definition.resource_type.as_str())
            .expect("a root key names an own record");
        let (_memory, store) = page(&definition);
        assert!(matches!(
            opening(&definition, &store, None, false, INSTANCE, &author()).unwrap(),
            Opening::Arriving
        ));
        assert_eq!(
            store.wanted_resources(),
            std::slice::from_ref(&own),
            "the person's own record is asked for, not the workspace's"
        );
        store.note_workspace_answered(own.clone());
        let Opening::New(id) = opening(&definition, &store, None, false, INSTANCE, &author()).unwrap()
        else {
            panic!("the person starts their own record");
        };
        assert_eq!(id, own);

        // Theirs, once a device of theirs wrote it: that is what opens.
        let (memory, store) = page(&definition);
        let checkout = DocumentCheckout::new(own.clone(), definition.resource_type.clone());
        arrives(&memory, &store, &own, written_elsewhere(&checkout, "mine"));
        let Opening::Target(LaunchTarget::Record { checkout, .. }) =
            opening(&definition, &store, None, false, INSTANCE, &author()).unwrap()
        else {
            panic!("the person's own record opens");
        };
        assert_eq!(checkout.id, own);
    }

    phase a_window_on_a_record_the_workspace_will_not_send_says_so;
    // A link to another person's own record: the workspace answers it as theirs, and the
    // window says whose it is instead of showing it arriving for good. A record the workspace
    // holds none of that the window may not start (a link to one deleted since) says it is
    // missing.
    {
        let definition = definition(false);
        let (_memory, store) = page(&definition);
        let theirs = DocumentId::of_member(&"b0".repeat(32), definition.resource_type.as_str()).unwrap();
        let Opening::Target(LaunchTarget::Record { checkout, .. }) = opening(
            &definition,
            &store,
            Some(theirs.as_str()),
            false,
            INSTANCE,
            &author(),
        )
        .unwrap() else {
            panic!("the link names the record");
        };
        assert!(matches!(
            store.try_open_resource(&checkout),
            Err(ResourceOpenError::Arriving)
        ));
        store.note_workspace_answer(theirs, ui::resource::WorkspaceAnswer::NotYours);
        let error = store.try_open_resource(&checkout).unwrap_err();
        assert!(matches!(error, ResourceOpenError::NotYours));
        let Unbuilt::Failed(said) = Unbuilt::of(ui::launch::LaunchError::Record(error)) else {
            panic!("a record that is not theirs is no arrival");
        };
        assert!(said.contains("someone else"), "{said}");
        assert!(matches!(
            Unbuilt::of(ui::launch::LaunchError::Record(ResourceOpenError::MissingResource)),
            Unbuilt::Failed(_)
        ));
    }

    phase a_window_on_a_record_still_on_its_way_waits_for_it;
    // A layout window saved on a record the workspace still sends (one a shared layout names) is
    // loading, not a failure: preparing it says so, asks the workspace for it, and prepares once it
    // is here. The person never reads the store's own words for it.
    {
        let definition = definition(false);
        let (memory, store) = page(&definition);
        let saved = DocumentCheckout::new(
            DocumentId::derived("default", &DocumentId::for_tab(INSTANCE + 1)),
            definition.resource_type.clone(),
        );
        let prepare = |store: &ResourceStore| {
            let views = Rc::new(
                ui::application_view::InstanceViewStore::with_store(
                    Rc::clone(&memory) as Rc<dyn durable_store::DurableStore>,
                    "D",
                    WORKSPACE,
                )
                .unwrap(),
            );
            ProcessContext::prepare(
                &definition,
                ui::application::ProcessContextInputs {
                    records: store.clone(),
                    views,
                    device_id: "D".into(),
                    instance_id: INSTANCE,
                    permissions: ui::permissions::SharedPermissions::new(
                        ui::permissions::PermissionContext::permissive(),
                    ),
                    author: ui::permissions::MemberId::person("ada"),
                    action_context: tilo_actions::ActionContext::default(),
                },
                &LaunchTarget::Record {
                    checkout: saved.clone(),
                    reveal: None,
                },
            )
            .map_err(Unbuilt::of)
        };
        assert!(matches!(prepare(&store), Err(Unbuilt::Arriving)));
        assert_eq!(
            store.wanted_resources(),
            std::slice::from_ref(&saved.id),
            "asked of the workspace"
        );
        arrives(
            &memory,
            &store,
            &saved.id,
            written_elsewhere(&saved, "theirs"),
        );
        assert!(prepare(&store).is_ok(), "it opens once it is here");
    }
}

/// The device the page's windows keep their views for.
const VIEWS_DEVICE: &str = "D";

/// The views the page's windows saved, in the storage its records are in.
fn page_views(memory: &Rc<MemoryStore>) -> Rc<InstanceViewStore> {
    Rc::new(
        InstanceViewStore::with_store(
            Rc::clone(memory) as Rc<dyn durable_store::DurableStore>,
            VIEWS_DEVICE,
            WORKSPACE,
        )
        .unwrap(),
    )
}

/// The view a window of `app` on `record` saved under [`INSTANCE`].
fn view_saved_by(app: &str, record: &DocumentCheckout) -> SavedInstanceView {
    SavedInstanceView {
        version: SavedInstanceView::VERSION,
        app_id: app.into(),
        device_id: VIEWS_DEVICE.into(),
        workspace_id: WORKSPACE.into(),
        instance_id: INSTANCE,
        document_id: record.id.as_str().into(),
        resource_type: record.kind.as_str().into(),
        view: ui::view::View::default(),
        application: None,
        shows: None,
    }
}

/// A definition whose factory says that it ran: a launch that reaches it passed every check a
/// launch makes before it builds anything.
fn reaching_its_factory() -> ApplicationDefinition {
    fn factory_ran(
        _: ProcessContext,
        _: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, LaunchError> {
        Err(LaunchError::Operation("the factory ran".into()))
    }
    ApplicationDefinition {
        create: factory_ran,
        ..definition(false)
    }
}

/// Window [`INSTANCE`] of `definition` launched on `record` as a page launches it, with the saved
/// view the page hands it: what the launch answered.
fn launch_window(
    definition: &ApplicationDefinition,
    memory: &Rc<MemoryStore>,
    store: &ResourceStore,
    record: &DocumentCheckout,
) -> String {
    let target = LaunchTarget::Record {
        checkout: record.clone(),
        reveal: None,
    };
    let views = page_views(memory);
    let restore = saved_view(&views, INSTANCE, definition, &target, None).unwrap();
    let inputs = ProcessContextInputs {
        records: store.clone(),
        views,
        device_id: VIEWS_DEVICE.into(),
        instance_id: INSTANCE,
        permissions: ui::permissions::SharedPermissions::new(
            ui::permissions::PermissionContext::permissive(),
        ),
        author: author(),
        action_context: tilo_actions::ActionContext::default(),
    };
    let context = ProcessContext::prepare(definition, inputs, &target).unwrap();
    match definition.instantiate(context, target, restore) {
        Ok(_) => "built".to_owned(),
        Err(error) => error.to_string(),
    }
}

tilo_test_env::scenario! {
    fn a_window_takes_the_saved_view_under_its_id_only_when_it_is_its_own;

    phase a_closed_windows_leftover_of_another_app_does_not_stop_the_window;
    // A window id comes round again: a desktop mints ids from just above the highest its layout
    // still holds, so after a reload a new window takes the id a closed one had, and every page
    // showing one app alone is the same window. The view the earlier window saved is still under
    // that id. Handed to the new window, the launch refused it ("application binding does not
    // match": a saved view of another device, workspace or record), on every load, so the app
    // never opened in that window again.
    {
        let definition = reaching_its_factory();
        let (memory, store) = page(&definition);
        let want = default_resource(&definition);
        arrives(&memory, &store, &want.id, written_elsewhere(&want, "kept"));
        page_views(&memory).save(&view_saved_by("so.tilo.other", &want)).unwrap();

        let said = launch_window(&definition, &memory, &store, &want);
        assert!(
            said.contains("the factory ran"),
            "the launch stopped before the app was built: {said}"
        );
        // The leftover is not the window's to take, and nothing took it away.
        let target = LaunchTarget::Record { checkout: want, reveal: None };
        assert!(matches!(
            saved_view(&page_views(&memory), INSTANCE, &definition, &target, None),
            Ok(RestoreInput::None)
        ));
        assert_eq!(
            page_views(&memory).load(INSTANCE).unwrap().unwrap().app_id,
            "so.tilo.other"
        );
    }

    phase a_leftover_of_another_record_does_not_stop_the_window;
    // The same app's window on another record had the id: its view is not this record's.
    {
        let definition = reaching_its_factory();
        let (memory, store) = page(&definition);
        let want = default_resource(&definition);
        arrives(&memory, &store, &want.id, written_elsewhere(&want, "kept"));
        let other = DocumentCheckout::new(
            DocumentId::derived("default", &DocumentId::for_tab(INSTANCE + 1)),
            definition.resource_type.clone(),
        );
        page_views(&memory).save(&view_saved_by(PACKAGE, &other)).unwrap();

        let said = launch_window(&definition, &memory, &store, &want);
        assert!(
            said.contains("the factory ran"),
            "the launch stopped before the app was built: {said}"
        );
    }

    phase the_windows_own_saved_view_is_handed_to_it;
    // What the window itself saved, for this app on this record, is what it comes back with.
    {
        let definition = reaching_its_factory();
        let (memory, store) = page(&definition);
        let want = default_resource(&definition);
        arrives(&memory, &store, &want.id, written_elsewhere(&want, "kept"));
        page_views(&memory).save(&view_saved_by(PACKAGE, &want)).unwrap();

        let target = LaunchTarget::Record { checkout: want.clone(), reveal: None };
        assert!(matches!(
            saved_view(&page_views(&memory), INSTANCE, &definition, &target, None),
            Ok(RestoreInput::Local(own)) if own.document_id == want.id.as_str()
        ));
        let said = launch_window(&definition, &memory, &store, &want);
        assert!(
            said.contains("the factory ran"),
            "the launch refused the window's own view: {said}"
        );
    }
}

tilo_test_env::scenario! {
    fn a_second_mount_of_an_app_in_a_page_reads_what_the_first_holds_of_the_workspace;

    phase a_part_of_the_workspaces_chain_opens_in_both_and_a_windows_own_document_in_one;
    // The page's own window of an app and a mount of the same app beside it (a message's view
    // in Chat) each read the page's storage through a store of their own. A part of the chain
    // kept beside the workspace's own document is the workspace's: both hold it. A document of
    // one window keeps its one owner.
    {
        let definition = definition(true);
        let memory = Rc::new(MemoryStore::queued());
        let mount = || {
            let mut owners = ResourceOwners::with_store(
                Rc::clone(&memory) as Rc<dyn durable_store::DurableStore>
            );
            let stores = Rc::new(RefCell::new(Vec::new()));
            store(&mut owners, &definition, WORKSPACE, true, &stores).unwrap()
        };
        let kept = |want: &DocumentCheckout, body: &str| {
            let key = StorageKey::document(PACKAGE, WORKSPACE, want.id.storage_key());
            memory
                .write(&key, WriteOp::Replace(written_elsewhere(want, body)), None)
                .unwrap();
        };
        let chain = DocumentId::derived("messages", &DocumentId::workspace_default());
        let part = DocumentCheckout::new(chain.part(0), ResourceType::new("probe.part"));
        kept(&part, "the first message");
        let own = default_resource(&definition);
        kept(&own, "one window's");

        let first = mount();
        first
            .try_open_resource(&part)
            .expect("the page's own window holds the part");
        first
            .try_open_resource(&own)
            .expect("and its own document");

        let beside = mount();
        assert_eq!(beside.held_parts(&chain), vec![0], "the part is in the page");
        beside
            .try_open_resource(&part)
            .expect("a mount beside reads the part the page's own window holds");
        assert_eq!(
            body(&beside, &part.id).as_deref(),
            Some("the first message")
        );
        assert!(
            beside.try_open_resource(&own).is_err(),
            "a document of one window keeps its one owner"
        );
        // So does a part of a chain of a document that is not the workspace's own.
        let theirs = DocumentCheckout::new(own.id.part(0), ResourceType::new("probe.part"));
        kept(&theirs, "one window's first part");
        first
            .try_open_resource(&theirs)
            .expect("its window holds it");
        assert!(
            beside.try_open_resource(&theirs).is_err(),
            "a part of one window's chain keeps its one owner"
        );
        // And the store of another workspace over the same storage has nothing of this
        // workspace's part: it neither lists nor opens it.
        let elsewhere = {
            let mut owners = ResourceOwners::with_store(
                Rc::clone(&memory) as Rc<dyn durable_store::DurableStore>
            );
            let stores = Rc::new(RefCell::new(Vec::new()));
            store(&mut owners, &definition, "workspace-b", true, &stores).unwrap()
        };
        assert!(
            elsewhere.held_parts(&chain).is_empty(),
            "another workspace's store holds none of it"
        );
        assert!(
            elsewhere.try_open_resource(&part).is_err(),
            "and does not open it"
        );
    }
}
