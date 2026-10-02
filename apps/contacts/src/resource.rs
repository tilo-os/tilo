//! Contacts as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path for the address book.
//!
//! Document mutations (new contact, per-contact patch / favourite / delete)
//! live on this resource via [`Resource::declarations`] / [`Resource::children`].
//! Import and export are Effects with no document delta
//! ([`Resource::local_operations`]). In-flight import conflicts and pending
//! duplicate merges stay hosted live extras — never a second hand-built
//! `contacts.book` root beside [`tilo_resource::publish`].

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    ActionParam, ActionSpec, EncodedAction, OperationKind, PROMPTED_PARAM, ParamType,
    Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    CONTACTS_ACTION_TYPE, ContactsAction, book_effect_specs, contact_resource_id,
    create_contact_action, live_presentation_extras, publish_children, register, split_name,
};
use crate::app::ContactsApp;
use crate::doc::ContactsDoc;
use crate::model::Contact;

/// Stable id for the published address-book root.
pub const BOOK_RESOURCE_ID: &str = "contacts.book";

/// Address-book projection for search and the published tree.
#[derive(Clone, Debug, Default)]
pub struct ContactsState {
    pub contacts: Vec<Contact>,
}

/// The contacts book resource.
pub struct Contacts;

impl Resource for Contacts {
    const KIND: &'static str = "contacts.book";
    const LABEL: &'static str = "Contacts";
    const ACTION_TYPE: &'static str = CONTACTS_ACTION_TYPE;

    type State = ContactsState;
    type Action = ContactsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<ContactsState, ModelError> {
        Ok(ContactsState {
            contacts: ContactsDoc::list_contacts(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "new-contact",
                "New contact",
                vec![ActionParam::required(
                    PROMPTED_PARAM,
                    ParamType::Text,
                    "Name",
                )],
                |args, _| {
                    let name = args.text(PROMPTED_PARAM).unwrap_or_default();
                    let (first, last) = split_name(name);
                    let mut contact = Contact::new();
                    contact.first = first;
                    contact.last = last;
                    Ok(create_contact_action(&contact))
                },
            )
            .describe("Create a new contact from a typed full name."),
        ]
    }

    fn children(state: &ContactsState) -> Vec<PublishedResource> {
        publish_children(&state.contacts)
    }

    fn local_operations() -> Vec<ActionSpec> {
        book_effect_specs()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &ContactsState) -> Vec<SearchEntry> {
        state
            .contacts
            .iter()
            .map(|contact| SearchEntry {
                anchor: Some(contact_resource_id(contact.id)),
                title: contact.display_name(),
                text: contact
                    .emails
                    .first()
                    .map(|email| email.value.clone())
                    .unwrap_or_default(),
            })
            .collect()
    }
}

/// Hosted-only extras that are not document state: in-flight import conflicts
/// and pending duplicate-merge reviews. The book door stays on
/// [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &ContactsApp) -> Vec<PublishedResource> {
    live_presentation_extras(&app.contacts_resource_ctx())
}

/// Framework app: the contacts widget is the surface over [`Contacts`].
pub struct ContactsShell;

impl ResourceApp for ContactsShell {
    type Resource = Contacts;
    const ID: &'static str = "so.tilo.contacts";
    const TITLE: &'static str = "Contacts";
    const ICON: Option<&'static str> = Some("contacts");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Contacts>) -> Element {
        cx.surface(ContactsApp::new())
    }
}

impl ResourceSurface<Contacts> for ContactsApp {
    fn present(&mut self, state: &std::rc::Rc<ContactsState>) {
        // Adopt the shared book projection only — never wipe a local address
        // book that has not been published yet.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Contacts>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Contacts>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Contacts>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &ContactsAction,
        context: ui::tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                dispatcher.apply_now_with(doc, action, context)
            }
            None => self
                .local
                .try_dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
pub fn provide_resources(state: &ContactsState) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Contacts>(
        BOOK_RESOURCE_ID,
        "Contacts",
        state,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::Action;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_book_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        let mut contact = Contact::new();
        contact.first = "Ada".into();
        contact.last = "Lovelace".into();
        contact.groups = vec![crate::model::GroupEntry::new("Scientists")];
        create_contact_action(&contact).apply(&mut doc).unwrap();
        doc.commit();

        let state = Contacts::read(&doc).expect("seeded contacts state");
        let published =
            ui::tilo_resource::publish::<Contacts>(BOOK_RESOURCE_ID, "Contacts", &state);
        assert_eq!(published.kind, "contacts.book");
        for id in ["new-contact", "import-vcard", "export-all-vcard"] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework resource"
            );
        }
        assert!(
            published
                .children
                .iter()
                .any(|child| child.id.as_str() == actions::FAVORITES_RESOURCE_ID),
            "favorites child stays on the published door"
        );
        assert!(
            published.children.iter().any(|child| {
                child.kind == "contacts.group"
                    && child.actions.iter().any(|action| action.id == "open")
            }),
            "group children keep their open View"
        );
        let contact_child = published
            .children
            .iter()
            .find(|child| child.kind == "contacts.contact")
            .expect("contact child on the published door");
        for id in [
            "open",
            "edit",
            "add-to-group",
            "favorite",
            "export-contact",
            "delete",
            "update-fields",
        ] {
            assert!(
                contact_child.actions.iter().any(|action| action.id == id),
                "{id} must be on the published contact child"
            );
        }

        let mut app = ContactsApp::sample();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "contacts.book");
        assert!(
            standalone[0]
                .actions
                .iter()
                .any(|action| action.id == "new-contact"),
            "standalone book keeps New contact"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &seeded, "contacts.book").is_none());
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "contacts.book"),
            "hosted must not republish contacts.book beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "contacts.book");
        assert!(
            door[0]
                .actions
                .iter()
                .any(|action| action.id == "new-contact")
        );

        // Empty present must not wipe a local address book that has not been published.
        let mut local = ContactsApp::sample();
        let mut local_doc = AutoCommit::new();
        local.sync(&mut local_doc, &ui::automerge::ROOT);
        let before = local.contacts_resource_ctx().contact_count;
        assert!(before > 0, "sample book starts with contacts after sync");
        local.adopt_resource_state(&ContactsState::default());
        assert_eq!(
            local.contacts_resource_ctx().contact_count,
            before,
            "adopting an empty projection must not clear unpublished contacts"
        );
    }
}
