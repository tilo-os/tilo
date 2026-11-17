//! A page showing one app in a workspace whose host sends a record only once
//! it is opened: what the page opens is never founded before the host answered
//! it holds none of it, and opens with the workspace's content once it comes.

use std::rc::Rc;

use durable_store::{DurableStore as _, MemoryStore, StorageKey, WriteOp};
use ui::application::{AppComponent, HostContext, RestoreInput};
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
            HostContext::prepare(
                &definition,
                ui::application::HostContextInputs {
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
