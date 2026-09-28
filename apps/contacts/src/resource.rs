//! Contacts as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path for the address book.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{CONTACTS_ACTION_TYPE, ContactsAction, register};
use crate::app::ContactsApp;
use crate::doc::ContactsDoc;
use crate::model::Contact;

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
        // New-contact / import prompts stay on the published tree.
        Vec::new()
    }

    fn search(state: &ContactsState) -> Vec<SearchEntry> {
        state
            .contacts
            .iter()
            .map(|contact| SearchEntry {
                anchor: Some(crate::actions::contact_resource_id(contact.id)),
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
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Contacts>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Contacts>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Contacts>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
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

    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}
