//! Contacts' exported **actions** and the **resource tree** it publishes to
//! Ctrl+K.
//!
//! Every document-mutating operation the app performs on its shared address
//! book is a [`ContactsAction`]: a small serializable value that
//! deterministically mutates the CRDT (see [`crate::doc`], where each contact
//! lives as a keyed map). Because the book already lives in the shared
//! document, an action replicates and replays for free. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the New button, the
//! edit form's Save, the trash button, the favourite star, the context menu)
//! dispatch and what the Ctrl+K palette dispatches — one source of truth, one
//! replay path.
//!
//! This is the **mutation** side of Contacts. The **navigation** side —
//! finding a contact — stays a [`SearchProvider`](crate::search_provider): a
//! chosen search row selects a record (presentation), it does not mutate the
//! document.
//!
//! The resource tree mirrors the data model: the book owns its contacts, and
//! each contact exports the [`ActionSpec`]s Ctrl+K offers (rename, add-to-group,
//! favourite, delete).
//!
//! Presentation — the current selection, the search text, the scroll position,
//! the delete-confirm dialog, and the edit form's per-field focus/typing until
//! Save — stays out of the document and never becomes an action.
//!
//! ## Determinism contract
//!
//! A new contact's id is minted at *authoring* time (when the palette argument
//! is committed, or when the New button is clicked) and carried in the payload;
//! a favourite toggle carries the resolved target value. [`Action::apply`]
//! never touches the RNG or the wall-clock, and a malformed payload (an
//! unparseable id) is a **no-op** rather than an error, so a bad replay can't
//! wedge a peer.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    OperationKind, Resource, ResourceProvider, RetryPolicy,
};

use crate::doc::ContactsDoc;
use crate::model::{Address, Birthday, Contact, ContactId, Labeled};

/// Registry `type_id` for every contacts action.
pub const CONTACTS_ACTION_TYPE: &str = "contacts.action";
pub const CONTACTS_OPERATION_TYPE: &str = "contacts.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContactsOperation {
    Open { id: String },
    ImportVcard,
    ExportAllVcard,
}

fn contact_operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    operation: ContactsOperation,
) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, CONTACTS_OPERATION_TYPE, &operation)
        .expect("ContactsOperation is always serializable");
    match operation {
        ContactsOperation::Open { .. } => spec.retry_policy(RetryPolicy::Idempotent),
        // These invocations synchronously enqueue a host-portal request and
        // return. The app has no durable job handle or cancellation channel,
        // so advertising a cancellable background lifecycle would be false.
        ContactsOperation::ImportVcard => spec,
        ContactsOperation::ExportAllVcard => spec.destructive(),
    }
}

/// A serializable snapshot of a contact's fields, in exactly the representation
/// the document stores (id and birthday as strings, the repeated rows as the
/// same [`Labeled`]/[`Address`] shapes the doc holds). Carrying the document
/// shape keeps [`Action::apply`] a pure write with no re-derivation, and makes
/// [`Self::from_contact`]/[`Self::to_contact`] lossless round-trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactData {
    pub id: String,
    pub first: String,
    pub last: String,
    pub org: String,
    pub title: String,
    pub website: String,
    /// Canonical birthday string (`Birthday::iso`), or `""` when unset.
    pub birthday: String,
    pub notes: String,
    pub favorite: bool,
    pub groups: Vec<String>,
    pub phones: Vec<Labeled>,
    pub emails: Vec<Labeled>,
    pub addresses: Vec<Address>,
}

impl ContactData {
    pub fn from_contact(c: &Contact) -> Self {
        Self {
            id: c.id.to_string(),
            first: c.first.clone(),
            last: c.last.clone(),
            org: c.org.clone(),
            title: c.title.clone(),
            website: c.website.clone(),
            birthday: c.birthday.map(|b| b.iso()).unwrap_or_default(),
            notes: c.notes.clone(),
            favorite: c.favorite,
            groups: c.groups.clone(),
            phones: c.phones.clone(),
            emails: c.emails.clone(),
            addresses: c.addresses.clone(),
        }
    }

    /// Reconstruct the model contact, or `None` when the id is unparseable, so
    /// `apply` can no-op instead of fabricating a record. A malformed birthday
    /// degrades to "no birthday" (the string is already validated on the way
    /// in, so this only bites a hand-crafted payload).
    fn to_contact(&self) -> Option<Contact> {
        let id = self.id.parse::<ContactId>().ok()?;
        Some(Contact {
            id,
            first: self.first.clone(),
            last: self.last.clone(),
            org: self.org.clone(),
            title: self.title.clone(),
            website: self.website.clone(),
            birthday: if self.birthday.is_empty() {
                None
            } else {
                Birthday::parse(&self.birthday)
            },
            notes: self.notes.clone(),
            favorite: self.favorite,
            groups: self.groups.clone(),
            phones: self.phones.clone(),
            emails: self.emails.clone(),
            addresses: self.addresses.clone(),
        })
    }
}

/// Every collaborative operation Contacts exports on its address book.
///
/// One whole-contact write covers create, every field edit, and every
/// add/remove of a phone / email / address / group row — each of those is just
/// a new field set on the same record. A favourite toggle and a delete are
/// their own small variants because they have direct affordances (the star, the
/// trash button) that don't open the edit form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContactsAction {
    /// Create or replace a contact's fields. The single variant behind the New
    /// button, every field edit committed by Save, and each row add/remove.
    /// Boxed: `ContactData` carries the whole record (repeated rows
    /// included) and dwarfs the other variants.
    UpsertContact(Box<ContactData>),
    /// Remove a contact from the book.
    DeleteContact { id: String },
    /// Set a contact's favourite flag to a resolved value. The toggling
    /// affordance reads the current state and carries its negation, so replay
    /// is deterministic against the base without a read in `apply`.
    SetFavorite { id: String, favorite: bool },
}

/// Durable, browseable history metadata for an address-book mutation.
pub fn history_context(action: &ContactsAction) -> qos_actions::ActionContext {
    let mut context =
        qos_actions::ActionContext::for_action("org.qos.Contacts", "Contacts", action);
    let (label, id, name) = match action {
        ContactsAction::UpsertContact(contact) => {
            let name = format!("{} {}", contact.first, contact.last)
                .trim()
                .to_string();
            ("Update contact", contact.id.as_str(), name)
        }
        ContactsAction::DeleteContact { id } => ("Delete contact", id.as_str(), "Contact".into()),
        ContactsAction::SetFavorite { id, favorite } => (
            if *favorite {
                "Favorite contact"
            } else {
                "Unfavorite contact"
            },
            id.as_str(),
            "Contact".into(),
        ),
    };
    context.action_label = label.to_string();
    context.resource = Some(qos_actions::ActionResource {
        id: format!("contacts.contact:{id}"),
        kind: "contacts.contact".to_string(),
        label: if name.is_empty() {
            "Contact".to_string()
        } else {
            name
        },
    });
    context
}

impl Action for ContactsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            ContactsAction::UpsertContact(data) => {
                if let Some(c) = data.to_contact() {
                    ContactsDoc::upsert_contact(doc, &c);
                }
            }
            ContactsAction::DeleteContact { id } => {
                if let Ok(uuid) = id.parse::<ContactId>() {
                    ContactsDoc::delete_contact(doc, uuid);
                }
            }
            ContactsAction::SetFavorite { id, favorite } => {
                if let Ok(uuid) = id.parse::<ContactId>() {
                    ContactsDoc::set_favorite(doc, uuid, *favorite);
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register the contacts action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<ContactsAction>(CONTACTS_ACTION_TYPE);
}

/// Encode any [`ContactsAction`] into the wire form the palette and the
/// on-screen affordances both dispatch. The single point that turns a contacts
/// action into a replayable envelope.
///
/// # Panics
///
/// Never panics: falls back to an empty-payload envelope rather than
/// crashing the caller if `action` somehow fails to serialize.
pub fn encode(action: &ContactsAction) -> EncodedAction {
    EncodedAction::encode(CONTACTS_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: CONTACTS_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Create or replace a contact (New, any field edit, or a row add/remove).
pub fn upsert_contact_action(c: &Contact) -> ContactsAction {
    ContactsAction::UpsertContact(Box::new(ContactData::from_contact(c)))
}

/// Delete a contact by id.
pub fn delete_contact_action(id: impl Into<String>) -> ContactsAction {
    ContactsAction::DeleteContact { id: id.into() }
}

/// Set a contact's favourite flag to `favorite`.
pub fn set_favorite_action(id: impl Into<String>, favorite: bool) -> ContactsAction {
    ContactsAction::SetFavorite {
        id: id.into(),
        favorite,
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context the app hands [`provide_resources`]: a snapshot of
/// the contacts currently in the book. Everything here is derived from state
/// the document already holds; nothing is fabricated.
#[derive(Debug, Clone)]
pub struct ContactsResourceCtx {
    pub contacts: Vec<Contact>,
    pub import_busy: bool,
    pub export_busy: bool,
}

/// Split a typed full name into first / rest-as-last, so a palette "New
/// contact" / "Rename" that collects a single name string lands the same way
/// the form's two fields would.
fn split_name(name: &str) -> (String, String) {
    let name = name.trim();
    match name.split_once(char::is_whitespace) {
        Some((first, rest)) => (first.trim().to_string(), rest.trim().to_string()),
        None => (name.to_string(), String::new()),
    }
}

fn immediate_spec(id: &str, label: &str, action: &ContactsAction) -> ActionSpec {
    ActionSpec::immediate(id, label, CONTACTS_ACTION_TYPE, action)
        // `ActionSpec`'s fields are private now that it carries typed
        // parameters; build the fallback through the constructor that
        // takes an already-encoded action.
        .unwrap_or_else(|_| {
            ActionSpec::immediate_encoded(id, label.to_string(), action.locus(), encode(action))
        })
}

/// Build the resource tree Contacts currently has open: the book (with a "New
/// contact" action) and each contact (rename, add-to-group, favourite, delete).
/// Every document-mutating operation is exported as the same action the
/// on-screen affordance dispatches.
pub fn provide_resources(ctx: &ContactsResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("contacts.book", "contacts.book", "Contacts");
    let mut import = contact_operation(
        "import-vcard",
        "Import vCard",
        OperationKind::Effect,
        ContactsOperation::ImportVcard,
    );
    if ctx.import_busy {
        import = import.unavailable("already_running", "A contact import is already in progress");
    }
    root = root.with_action(import);
    let mut export = contact_operation(
        "export-all-vcard",
        "Export all contacts",
        OperationKind::Effect,
        ContactsOperation::ExportAllVcard,
    );
    if ctx.contacts.is_empty() {
        export = export.unavailable("book_empty", "There are no contacts to export");
    } else if ctx.export_busy {
        export = export.unavailable("already_running", "A contact export is already in progress");
    }
    root = root.with_action(export);

    // Book-level: create a contact from a typed name. The uuid is minted once
    // when the argument is committed, then baked into the envelope, so replay
    // stays deterministic.
    root = root.with_action(ActionSpec::prompted(
        "new-contact",
        "New contact",
        "Name",
        "full name…",
        ExecutionLocus::ClientLocal,
        move |name: &str| {
            let (first, last) = split_name(name);
            let mut c = Contact::new(); // id minted HERE, once, at commit time
            c.first = first;
            c.last = last;
            encode(&upsert_contact_action(&c))
        },
    ));

    for contact in &ctx.contacts {
        let id = contact.id.to_string();
        let mut child = Resource::new(
            format!("contacts.contact:{id}"),
            "contacts.contact",
            contact.display_name(),
        );
        child = child.with_action(contact_operation(
            "open",
            "Open contact",
            OperationKind::View,
            ContactsOperation::Open { id: id.clone() },
        ));

        // Rename — a prompted action carrying the rest of the record unchanged.
        let base = contact.clone();
        child = child.with_action(ActionSpec::prompted(
            "edit",
            "Rename contact",
            "Name",
            "full name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let (first, last) = split_name(name);
                let mut d = base.clone();
                d.first = first;
                d.last = last;
                encode(&upsert_contact_action(&d))
            },
        ));

        // Add to a group — a prompted action appending a group (deduped,
        // case-insensitive) to the same record.
        let base = contact.clone();
        child = child.with_action(ActionSpec::prompted(
            "add-to-group",
            "Add to group",
            "Group",
            "group name…",
            ExecutionLocus::ClientLocal,
            move |group: &str| {
                let mut d = base.clone();
                let g = group.trim();
                if !g.is_empty() && !d.groups.iter().any(|x| x.eq_ignore_ascii_case(g)) {
                    d.groups.push(g.to_string());
                }
                encode(&upsert_contact_action(&d))
            },
        ));

        // Favourite — one immediate action toggles to the resolved value.
        let fav_label = if contact.favorite {
            "Remove from favorites"
        } else {
            "Add to favorites"
        };
        child = child.with_action(immediate_spec(
            "favorite",
            fav_label,
            &set_favorite_action(id.clone(), !contact.favorite),
        ));

        // Delete.
        child = child.with_action(
            immediate_spec(
                "delete",
                "Delete contact",
                &delete_contact_action(id.clone()),
            )
            .destructive(),
        );

        root = root.with_child(child);
    }

    vec![root]
}

impl ResourceProvider for crate::app::ContactsApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.contacts_resource_ctx(), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ContactsApp;
    use crate::search_provider::ContactSearchProvider;
    use automerge::ROOT;
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    /// The sample address book synced into a fresh document, plus the app that
    /// owns it — the real widget, driven through its real `sync`.
    fn seeded() -> (ContactsApp, AutoCommit) {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    fn first_contact(doc: &AutoCommit) -> Contact {
        ContactsDoc::list_contacts(doc)
            .into_iter()
            .next()
            .expect("the sample book is non-empty")
    }

    /// Ctrl+K on Contacts yields the tree: the book with a "New contact" action
    /// and a `contacts.contact` child per record exporting rename / add-to-group
    /// / favourite / delete. Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_contacts_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one book root");
        let book = &resources[0];
        assert_eq!(book.kind, "contacts.book");
        let book_actions: Vec<&str> = book.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(book_actions.contains(&"new-contact"), "{book_actions:?}");

        let contact = book
            .children
            .iter()
            .find(|r| r.kind == "contacts.contact")
            .expect("a contact resource");
        let actions: Vec<&str> = contact.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(actions.contains(&"edit"), "{actions:?}");
        assert!(actions.contains(&"add-to-group"), "{actions:?}");
        assert!(actions.contains(&"favorite"), "{actions:?}");
        assert!(actions.contains(&"delete"), "{actions:?}");

        // The full flattened tree exports every action, and the one palette
        // surfaces them as rows (the Actions group is display-capped, so
        // reachability is asserted on the flatten and the leading rows on the
        // palette itself).
        let flat: Vec<&str> = qos_actions::flatten(&resources)
            .iter()
            .map(|f| f.action.label.as_str())
            .collect();
        assert!(flat.contains(&"New contact"), "{flat:?}");
        assert!(flat.contains(&"Delete contact"), "{flat:?}");
        assert!(flat.contains(&"Rename contact"), "{flat:?}");

        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("New contact")),
            "{labels:?}"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut app = ContactsApp::sample();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);

        let victim = first_contact(&state.doc).id;
        let before_heads = state.doc.get_heads();
        let before = ContactsDoc::list_contacts(&state.doc).len();

        let action = delete_contact_action(victim.to_string());
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            ContactsDoc::list_contacts(&state.doc).len(),
            before - 1,
            "the action deleted a contact"
        );
        assert!(
            !ContactsDoc::list_contacts(&state.doc)
                .iter()
                .any(|c| c.id == victim),
            "the right contact is gone"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            ContactsDoc::list_contacts(&state.doc).len(),
            before,
            "undo reverted the deletion and nothing else"
        );
        assert!(
            ContactsDoc::list_contacts(&state.doc)
                .iter()
                .any(|c| c.id == victim),
            "the contact is back after undo"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a
    /// second independent replica (from the same base state) lands on the
    /// byte-identical document — same head hashes, not merely the same values.
    #[test]
    fn favorite_action_replays_byte_identical_on_a_peer() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let target = first_contact(&a);
        let action = set_favorite_action(target.id.to_string(), !target.favorite);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        let fav_a = ContactsDoc::list_contacts(&a)
            .into_iter()
            .find(|c| c.id == target.id)
            .unwrap()
            .favorite;
        let fav_b = ContactsDoc::list_contacts(&b)
            .into_iter()
            .find(|c| c.id == target.id)
            .unwrap()
            .favorite;
        assert_eq!(fav_a, !target.favorite, "the toggle landed on replica A");
        assert_eq!(fav_b, !target.favorite, "and byte-identically on replica B");
    }

    /// Replaying an `UpsertContact` (a field edit) also converges
    /// byte-identically — the create/edit path, not just the favourite toggle.
    #[test]
    fn upsert_contact_replays_byte_identical() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let mut c = first_contact(&a);
        c.title = "Renamed via action".into();
        let action = upsert_contact_action(&c);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "edit replayed identically");
        let title_b = ContactsDoc::list_contacts(&b)
            .into_iter()
            .find(|x| x.id == c.id)
            .unwrap()
            .title;
        assert_eq!(title_b, "Renamed via action");
    }

    /// Single source of truth: the palette's "Delete contact" / "Favorite" specs
    /// and the builders the trash button / star call encode byte-identically,
    /// and the prompted "Rename" spec matches the upsert builder for the same
    /// name.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let book = &resources[0];
        let contact = book
            .children
            .iter()
            .find(|r| r.kind == "contacts.contact")
            .unwrap();
        let id = contact.id.as_str().trim_start_matches("contacts.contact:");
        let stored = ContactsDoc::list_contacts(&doc)
            .into_iter()
            .find(|c| c.id.to_string() == id)
            .unwrap();

        // Delete: spec == builder.
        let delete_spec = contact.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        let from_builder = encode(&delete_contact_action(id.to_string()));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for delete"
        );

        // Favourite: spec toggles to the negation of the current state, exactly
        // as the star affordance builds it.
        let fav_spec = contact.actions.iter().find(|a| a.id == "favorite").unwrap();
        let from_spec = immediate_encode(fav_spec);
        let from_builder = encode(&set_favorite_action(id.to_string(), !stored.favorite));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for favourite"
        );

        // Rename: prompted spec == upsert builder for the renamed record.
        let edit_spec = contact.actions.iter().find(|a| a.id == "edit").unwrap();
        let from_spec = prompted_encode(edit_spec, "Grace Hopper");
        let mut renamed = stored;
        let (first, last) = split_name("Grace Hopper");
        renamed.first = first;
        renamed.last = last;
        let from_builder = encode(&upsert_contact_action(&renamed));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );
    }

    /// Dispatching the "New contact" prompted action creates a contact in the
    /// document with the committed name split into first/last.
    #[test]
    fn new_contact_action_creates_a_contact() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "new-contact")
            .unwrap();
        let before = ContactsDoc::list_contacts(&doc).len();

        let encoded = prompted_encode(spec, "Dana Scott");
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let book = ContactsDoc::list_contacts(&doc);
        assert_eq!(book.len(), before + 1, "the action added a contact");
        let created = book.iter().find(|c| c.first == "Dana" && c.last == "Scott");
        assert!(
            created.is_some(),
            "the created contact carries the committed name"
        );
    }

    /// The action side and the search side coexist: the same live app exports a
    /// mutating resource tree AND a working `SearchProvider` that resolves a real
    /// contact. Adding actions did not break navigation.
    #[test]
    fn search_provider_still_resolves_a_contact() {
        let (app, doc) = seeded();

        // The mutation side is live.
        assert!(
            !Widget::resources(&app, &doc).is_empty(),
            "resources are exported"
        );

        // The navigation side still finds and resolves a contact.
        let providers = Widget::search_providers(&app);
        assert_eq!(providers.len(), 1, "one contacts search provider");
        let hits = providers[0].search("hopper");
        let hit = hits
            .iter()
            .find(|r| r.title == "Grace Hopper")
            .expect("the provider surfaces the real matching contact");
        let id = ContactSearchProvider::parse_action_id(&hit.action_id)
            .expect("action_id parses back to a ContactId");
        assert!(
            ContactsDoc::list_contacts(&doc).iter().any(|c| c.id == id),
            "the chosen row resolves to a real stored contact"
        );
    }
}
