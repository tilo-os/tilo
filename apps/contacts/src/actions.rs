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
//! This is the **mutation** side of Contacts. The **navigation** side is a
//! typed view operation addressed by the resource's search entries; opening a
//! search row is agent-capable without mutating the document.
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
//! never touches the RNG or the wall-clock. Invalid or stale payloads are
//! explicitly refused, so callers can distinguish rejection from success and
//! a bad replay never changes the document.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use ui::tilo_resource::{self, Effect};
use uuid::Uuid;

use tilo_actions::{
    Action, ActionAddress, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ParamType, Resource,
};

use crate::doc::{ContactsDoc, ListRow};
use crate::model::{
    Address, Birthday, Contact, ContactId, GroupEntry, Labeled, group_names_equal, normalize_text,
};

/// Registry `type_id` for every contacts action.
pub const CONTACTS_ACTION_TYPE: &str = "contacts.action";

/// Stable resource id for one contact. Search, the palette, accessibility and
/// remote callers all address this same resource rather than translating a
/// second, private navigation token.
pub fn contact_resource_id(id: ContactId) -> String {
    format!("contacts.contact:{id}")
}

/// The eagerly published command palette page is deliberately small. The
/// address book itself can be arbitrarily large; every omitted contact is
/// still reachable by id through [`resolve_child`] and the complete
/// search index, rather than making each frame allocate every action for the
/// whole book.
pub const MAX_CONTEXT_CONTACT_RESOURCES: usize = 64;

/// Group membership is durable data, but a book can accumulate far more group
/// names than a palette page can responsibly materialise.  Keep the eagerly
/// published page small; [`resolve_child`] and the search provider still
/// resolve every canonical group on demand.
pub const MAX_CONTEXT_GROUP_RESOURCES: usize = 32;

/// Potential duplicate review resources are intentionally capped.  The full
/// detector stays linear in contacts and their fields, but discovery must not
/// turn a large imported book into thousands of palette entries.
pub const MAX_CONTEXT_DUPLICATE_RESOURCES: usize = 32;

/// Canonical view action that reveals one contact in the app.
pub fn open_contact_address(id: ContactId) -> ActionAddress {
    ActionAddress::new(contact_resource_id(id), "open")
}

/// Stable resource id for one logical group. Group names are case- and
/// Unicode-normalised everywhere Contacts compares them, so their resource
/// identity must use that same key rather than whichever spelling arrived
/// first from a collaborator or vCard.
pub fn group_resource_id(name: &str) -> String {
    format!("contacts.group:{}", normalize_text(name.trim()))
}

/// Canonical view action that filters the book to one durable group.
pub fn open_group_address(name: &str) -> ActionAddress {
    ActionAddress::new(group_resource_id(name), "open-group")
}

/// Stable virtual resource for the collaboratively stored favourite flag.
/// The filter itself is presentation state, while the membership it reveals
/// belongs to every contact's replicated document record.
pub const FAVORITES_RESOURCE_ID: &str = "contacts.favorites";

pub fn open_favorites_address() -> ActionAddress {
    ActionAddress::new(FAVORITES_RESOURCE_ID, "open-favorites")
}

/// The one typed external operation for exporting exactly one contact.  Both
/// the resource catalogue and the contextual UI encode this value before it
/// reaches the shared operation handler.
pub fn export_contact_operation(id: ContactId) -> ContactsEffect {
    ContactsEffect::ExportContact { contact: id }
}

/// What Contacts does that is no edit of the address book: showing a
/// contact, a group or the favorites, reviewing a likely duplicate, and
/// moving cards in and out through the system file chooser.
#[derive(Debug, Clone, PartialEq, Eq, Effect)]
#[effect(operation = "contacts.effect")]
pub enum ContactsEffect {
    /// Open contact.
    #[effect(view, row, idempotent)]
    Open {
        #[param("The contact's id")]
        contact: ContactId,
    },
    /// Open group.
    #[effect(view, row, idempotent)]
    OpenGroup {
        #[param("The group's name")]
        name: String,
    },
    /// Open favorites.
    #[effect(view, row, idempotent)]
    OpenFavorites,
    /// Review possible duplicate.
    #[effect(view, row, idempotent)]
    OpenReview {
        #[param("The contact that stays")]
        survivor: ContactId,
        #[param("The contact merged into it")]
        source: ContactId,
    },
    /// Import vCard.
    #[effect(job)]
    ImportVcard,
    /// Export all contacts.
    #[effect(job, sensitive)]
    ExportAllVcard,
    /// Export contact.
    #[effect(row, sensitive)]
    ExportContact {
        #[param("The contact's id")]
        contact: ContactId,
    },
    /// Keep existing contact.
    #[effect(row, idempotent)]
    KeepExisting {
        #[param("The pending import review")]
        token: String,
    },
    /// Import as new contact.
    #[effect(row, destructive)]
    ImportAsNew {
        #[param("The pending import review")]
        token: String,
    },
}

/// A serializable snapshot of a contact's fields, in exactly the representation
/// the document stores (id and birthday as strings, the repeated rows as the
/// same [`Labeled`]/[`Address`] shapes the doc holds). Carrying the document
/// shape keeps [`Action::apply`] a pure write with no re-derivation, and makes
/// [`Self::from_contact`]/`Self::to_contact` lossless round-trips.
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
    pub groups: Vec<GroupEntry>,
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

    /// Reconstruct a validated model contact for a first creation.
    fn to_contact(&self) -> Result<Contact, ActionError> {
        validate_contact_data(self)?;
        let id = self
            .id
            .parse::<ContactId>()
            .map_err(|_| ActionError::Refused("contact id is invalid".into()))?;
        Ok(Contact {
            id,
            first: self.first.clone(),
            last: self.last.clone(),
            org: self.org.clone(),
            title: self.title.clone(),
            website: self.website.clone(),
            birthday: if self.birthday.is_empty() {
                None
            } else {
                Some(Birthday::parse(&self.birthday).ok_or_else(|| {
                    ActionError::Refused("birthday must be a valid ISO date".into())
                })?)
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

const MAX_CONTACT_FIELD_BYTES: usize = 32 * 1024;
const MAX_CONTACT_ROWS: usize = 256;

fn validate_text(field: &str, value: &str) -> Result<(), ActionError> {
    if value.len() > MAX_CONTACT_FIELD_BYTES {
        return Err(ActionError::Refused(format!(
            "{field} exceeds the {MAX_CONTACT_FIELD_BYTES}-byte limit"
        )));
    }
    Ok(())
}

fn validate_contact_data(data: &ContactData) -> Result<(), ActionError> {
    for (field, value) in [
        ("first name", &data.first),
        ("last name", &data.last),
        ("organization", &data.org),
        ("title", &data.title),
        ("website", &data.website),
        ("birthday", &data.birthday),
        ("notes", &data.notes),
    ] {
        validate_text(field, value)?;
    }
    if [
        data.groups.len(),
        data.phones.len(),
        data.emails.len(),
        data.addresses.len(),
    ]
    .into_iter()
    .any(|length| length > MAX_CONTACT_ROWS)
    {
        return Err(ActionError::Refused(
            "contact has too many repeated fields".into(),
        ));
    }
    for group in &data.groups {
        validate_non_nil_id(group.id)?;
        validate_text("group", &group.value)?;
    }
    for labeled in data.phones.iter().chain(&data.emails) {
        validate_non_nil_id(labeled.id)?;
        validate_text("field label", &labeled.label)?;
        validate_text("field value", &labeled.value)?;
    }
    for address in &data.addresses {
        validate_non_nil_id(address.id)?;
        for (field, value) in [
            ("address label", &address.label),
            ("address street", &address.street),
            ("address city", &address.city),
            ("address region", &address.region),
            ("address postal code", &address.postal),
            ("address country", &address.country),
        ] {
            validate_text(field, value)?;
        }
    }
    for (name, ids) in [
        (
            "groups",
            data.groups.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        ),
        (
            "phone numbers",
            data.phones.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        ),
        (
            "email addresses",
            data.emails.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        ),
        (
            "postal addresses",
            data.addresses
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
        ),
    ] {
        let distinct = ids.iter().copied().collect::<HashSet<_>>();
        if distinct.len() != ids.len() {
            return Err(ActionError::Refused(format!(
                "contact has duplicate {name} entry ids"
            )));
        }
    }
    if !data.birthday.is_empty() && Birthday::parse(&data.birthday).is_none() {
        return Err(ActionError::Refused(
            "birthday must be a valid ISO date".into(),
        ));
    }
    Ok(())
}

/// The independently mergeable scalar fields a saved form may change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListEdit<T> {
    Add { entry_id: Uuid, value: T },
    Update { entry_id: Uuid, value: T },
    Remove { entry_id: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ContactPatch {
    pub first: Option<String>,
    pub last: Option<String>,
    pub org: Option<String>,
    pub title: Option<String>,
    pub website: Option<String>,
    /// Empty means clear; `None` means unchanged.
    pub birthday: Option<String>,
    pub notes: Option<String>,
    pub group_edits: Vec<ListEdit<GroupEntry>>,
    pub phone_edits: Vec<ListEdit<Labeled>>,
    pub email_edits: Vec<ListEdit<Labeled>>,
    pub address_edits: Vec<ListEdit<Address>>,
}

impl ContactPatch {
    /// Whether the patch carries no durable change. The resource/action door
    /// rejects these before they enter history, while `apply` remains the
    /// authority for untrusted replay payloads.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.first.is_none()
            && self.last.is_none()
            && self.org.is_none()
            && self.title.is_none()
            && self.website.is_none()
            && self.birthday.is_none()
            && self.notes.is_none()
            && self.group_edits.is_empty()
            && self.phone_edits.is_empty()
            && self.email_edits.is_empty()
            && self.address_edits.is_empty()
    }

    #[must_use]
    pub fn between(base: &Contact, edited: &Contact) -> Self {
        let changed = |before: &str, after: &str| (before != after).then(|| after.to_string());
        Self {
            first: changed(&base.first, &edited.first),
            last: changed(&base.last, &edited.last),
            org: changed(&base.org, &edited.org),
            title: changed(&base.title, &edited.title),
            website: changed(&base.website, &edited.website),
            birthday: (base.birthday != edited.birthday).then(|| {
                edited
                    .birthday
                    .map(|birthday| birthday.iso())
                    .unwrap_or_default()
            }),
            notes: changed(&base.notes, &edited.notes),
            group_edits: list_edits(&base.groups, &edited.groups, |entry| entry.id),
            phone_edits: list_edits(&base.phones, &edited.phones, |entry| entry.id),
            email_edits: list_edits(&base.emails, &edited.emails, |entry| entry.id),
            address_edits: list_edits(&base.addresses, &edited.addresses, |entry| entry.id),
        }
    }

    fn validate(&self) -> Result<(), ActionError> {
        for (field, value) in [
            ("first name", self.first.as_deref()),
            ("last name", self.last.as_deref()),
            ("organization", self.org.as_deref()),
            ("title", self.title.as_deref()),
            ("website", self.website.as_deref()),
            ("birthday", self.birthday.as_deref()),
            ("notes", self.notes.as_deref()),
        ] {
            if let Some(value) = value {
                validate_text(field, value)?;
            }
        }
        if self.group_edits.len() > MAX_CONTACT_ROWS * 2
            || self.phone_edits.len() > MAX_CONTACT_ROWS * 2
            || self.email_edits.len() > MAX_CONTACT_ROWS * 2
            || self.address_edits.len() > MAX_CONTACT_ROWS * 2
        {
            return Err(ActionError::Refused(
                "contact patch has too many group edits".into(),
            ));
        }
        validate_unique_edit_ids(&self.group_edits)?;
        validate_unique_edit_ids(&self.phone_edits)?;
        validate_unique_edit_ids(&self.email_edits)?;
        validate_unique_edit_ids(&self.address_edits)?;
        for edit in &self.group_edits {
            match edit {
                ListEdit::Add { entry_id, value } | ListEdit::Update { entry_id, value } => {
                    validate_entry_id(*entry_id, value.id)?;
                    validate_text("group", &value.value)?;
                }
                ListEdit::Remove { entry_id } => validate_non_nil_id(*entry_id)?,
            }
        }
        for edit in self.phone_edits.iter().chain(&self.email_edits) {
            match edit {
                ListEdit::Add { entry_id, value } | ListEdit::Update { entry_id, value } => {
                    validate_entry_id(*entry_id, value.id)?;
                    validate_text("field label", &value.label)?;
                    validate_text("field value", &value.value)?;
                }
                ListEdit::Remove { entry_id } => validate_non_nil_id(*entry_id)?,
            }
        }
        for edit in &self.address_edits {
            match edit {
                ListEdit::Add { entry_id, value } | ListEdit::Update { entry_id, value } => {
                    validate_entry_id(*entry_id, value.id)?;
                    for (field, value) in [
                        ("address label", &value.label),
                        ("address street", &value.street),
                        ("address city", &value.city),
                        ("address region", &value.region),
                        ("address postal code", &value.postal),
                        ("address country", &value.country),
                    ] {
                        validate_text(field, value)?;
                    }
                }
                ListEdit::Remove { entry_id } => validate_non_nil_id(*entry_id)?,
            }
        }
        if let Some(birthday) = &self.birthday
            && !birthday.is_empty()
            && Birthday::parse(birthday).is_none()
        {
            return Err(ActionError::Refused(
                "birthday must be a valid ISO date".into(),
            ));
        }
        Ok(())
    }
}

fn validate_non_nil_id(id: Uuid) -> Result<(), ActionError> {
    if id.is_nil() {
        return Err(ActionError::Refused(
            "repeated-field entry id is invalid".into(),
        ));
    }
    Ok(())
}

fn validate_entry_id(entry_id: Uuid, value_id: Uuid) -> Result<(), ActionError> {
    validate_non_nil_id(entry_id)?;
    if entry_id != value_id {
        return Err(ActionError::Refused(
            "repeated-field payload does not match its entry id".into(),
        ));
    }
    Ok(())
}

fn validate_unique_edit_ids<T>(edits: &[ListEdit<T>]) -> Result<(), ActionError> {
    let mut ids = HashSet::new();
    for edit in edits {
        let entry_id = match edit {
            ListEdit::Add { entry_id, .. }
            | ListEdit::Update { entry_id, .. }
            | ListEdit::Remove { entry_id } => *entry_id,
        };
        if !ids.insert(entry_id) {
            return Err(ActionError::Refused(
                "contact patch repeats a repeated-field entry id".into(),
            ));
        }
    }
    Ok(())
}

fn list_edits<T: Clone + PartialEq>(
    base: &[T],
    edited: &[T],
    id: impl Fn(&T) -> Uuid,
) -> Vec<ListEdit<T>> {
    let mut edits = Vec::new();
    for value in base {
        match edited.iter().find(|candidate| id(candidate) == id(value)) {
            None => edits.push(ListEdit::Remove {
                entry_id: id(value),
            }),
            Some(candidate) if candidate != value => edits.push(ListEdit::Update {
                entry_id: id(value),
                value: candidate.clone(),
            }),
            Some(_) => {}
        }
    }
    for value in edited {
        if !base.iter().any(|candidate| id(candidate) == id(value)) {
            edits.push(ListEdit::Add {
                entry_id: id(value),
                value: value.clone(),
            });
        }
    }
    edits
}

/// Every collaborative operation Contacts exports on its address book.
///
/// Creation is deliberately distinct from later patches. A patch contains only
/// user-edited fields, so a stale form cannot overwrite unrelated peer edits.
/// Group changes are entry operations, never a destructive list replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContactsAction {
    /// Create a new, previously unseen stable contact id.
    CreateContact(Box<ContactData>),
    /// Apply only selected fields to an active contact.
    PatchContact {
        id: String,
        patch: Box<ContactPatch>,
    },
    /// Tombstone a contact.
    DeleteContact { id: String },
    /// Set a contact's favourite flag to a resolved value. The toggling
    /// affordance reads the current state and carries its negation, so replay
    /// is deterministic against the base without a read in `apply`.
    SetFavorite { id: String, favorite: bool },
    /// Move every reviewed field from `source_id` into `survivor_id`, then
    /// tombstone the source in the very same collaborative history entry.
    /// The payload carries the reviewed sparse patch rather than deriving a
    /// policy during replay: peers therefore converge on precisely the
    /// field-level choices the person confirmed.
    MergeContacts {
        survivor_id: String,
        source_id: String,
        patch: Box<ContactPatch>,
    },
}

impl Action for ContactsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            ContactsAction::CreateContact(data) => {
                let contact = data.to_contact()?;
                if !ContactsDoc::create_contact(doc, &contact) {
                    return Err(ActionError::Refused(
                        "contact already exists or was deleted".into(),
                    ));
                }
            }
            ContactsAction::PatchContact { id, patch } => apply_patch(doc, id, patch)?,
            ContactsAction::DeleteContact { id } => {
                let uuid = id
                    .parse::<ContactId>()
                    .map_err(|_| ActionError::Refused("contact id is invalid".into()))?;
                if !ContactsDoc::contact_is_active(doc, uuid) {
                    return Err(ActionError::Refused("contact no longer exists".into()));
                }
                ContactsDoc::delete_contact(doc, uuid);
            }
            ContactsAction::SetFavorite { id, favorite } => {
                let uuid = id
                    .parse::<ContactId>()
                    .map_err(|_| ActionError::Refused("contact id is invalid".into()))?;
                if !ContactsDoc::set_favorite(doc, uuid, *favorite) {
                    return Err(ActionError::Refused("contact no longer exists".into()));
                }
            }
            ContactsAction::MergeContacts {
                survivor_id,
                source_id,
                patch,
            } => apply_merge(doc, survivor_id, source_id, patch)?,
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Apply a reviewed merge without ever partially mutating the survivor.  The
/// complete patch and both active identities are checked before the first
/// durable write.  The host records this as one action, so its standard
/// undo restores both the source and the survivor's reviewed fields.
fn apply_merge(
    doc: &mut Doc,
    survivor_id: &str,
    source_id: &str,
    patch: &ContactPatch,
) -> Result<(), ActionError> {
    patch.validate()?;
    let survivor = survivor_id
        .parse::<ContactId>()
        .map_err(|_| ActionError::Refused("surviving contact id is invalid".into()))?;
    let source = source_id
        .parse::<ContactId>()
        .map_err(|_| ActionError::Refused("source contact id is invalid".into()))?;
    if survivor == source {
        return Err(ActionError::Refused(
            "a contact cannot be merged into itself".into(),
        ));
    }
    preflight_patch(doc, survivor, patch)?;
    if !ContactsDoc::contact_is_active(doc, source) {
        return Err(contact_missing());
    }
    apply_scalar_patch(doc, survivor, patch)?;
    apply_list_edits(doc, survivor, "groups", &patch.group_edits)?;
    apply_list_edits(doc, survivor, "phones", &patch.phone_edits)?;
    apply_list_edits(doc, survivor, "emails", &patch.email_edits)?;
    apply_list_edits(doc, survivor, "addresses", &patch.address_edits)?;
    ContactsDoc::delete_contact(doc, source);
    Ok(())
}

fn apply_patch(doc: &mut Doc, id: &str, patch: &ContactPatch) -> Result<(), ActionError> {
    patch.validate()?;
    let id = id
        .parse::<ContactId>()
        .map_err(|_| ActionError::Refused("contact id is invalid".into()))?;
    preflight_patch(doc, id, patch)?;
    apply_scalar_patch(doc, id, patch)?;
    apply_list_edits(doc, id, "groups", &patch.group_edits)?;
    apply_list_edits(doc, id, "phones", &patch.phone_edits)?;
    apply_list_edits(doc, id, "emails", &patch.email_edits)?;
    apply_list_edits(doc, id, "addresses", &patch.address_edits)
}

/// Reject an invalid/stale row target before an action mutates even one scalar
/// or list entry. `Action::apply` is not an implicit transaction, so this
/// read-only gate is what makes a refused patch observationally atomic.
fn preflight_patch(doc: &Doc, id: ContactId, patch: &ContactPatch) -> Result<(), ActionError> {
    if !ContactsDoc::contact_is_active(doc, id) {
        return Err(contact_missing());
    }
    preflight_list_edits(&patch.group_edits, |entry_id| {
        ContactsDoc::entry_exists(doc, id, "groups", entry_id)
    })?;
    preflight_list_edits(&patch.phone_edits, |entry_id| {
        ContactsDoc::entry_exists(doc, id, "phones", entry_id)
    })?;
    preflight_list_edits(&patch.email_edits, |entry_id| {
        ContactsDoc::entry_exists(doc, id, "emails", entry_id)
    })?;
    preflight_list_edits(&patch.address_edits, |entry_id| {
        ContactsDoc::entry_exists(doc, id, "addresses", entry_id)
    })
}

fn preflight_list_edits<T>(
    edits: &[ListEdit<T>],
    exists: impl Fn(Uuid) -> bool,
) -> Result<(), ActionError> {
    for edit in edits {
        let (entry_id, should_exist) = match edit {
            ListEdit::Add { entry_id, .. } => (*entry_id, false),
            ListEdit::Update { entry_id, .. } | ListEdit::Remove { entry_id } => (*entry_id, true),
        };
        if exists(entry_id) != should_exist {
            return Err(ActionError::Refused(
                "contact row id is stale or collides with an existing row".into(),
            ));
        }
    }
    Ok(())
}

fn contact_missing() -> ActionError {
    ActionError::Refused("contact no longer exists or row id is stale".into())
}

fn apply_scalar_patch(
    doc: &mut Doc,
    id: ContactId,
    patch: &ContactPatch,
) -> Result<(), ActionError> {
    for (field, value) in [
        ("first", patch.first.as_deref()),
        ("last", patch.last.as_deref()),
        ("org", patch.org.as_deref()),
        ("title", patch.title.as_deref()),
        ("website", patch.website.as_deref()),
        ("notes", patch.notes.as_deref()),
    ] {
        if let Some(value) = value
            && !ContactsDoc::set_text_field(doc, id, field, value)
        {
            return Err(contact_missing());
        }
    }
    if let Some(birthday) = &patch.birthday
        && !ContactsDoc::set_birthday(doc, id, birthday)
    {
        return Err(contact_missing());
    }
    Ok(())
}

fn apply_list_edits<T: ListRow>(
    doc: &mut Doc,
    id: ContactId,
    key: &str,
    edits: &[ListEdit<T>],
) -> Result<(), ActionError> {
    for edit in edits {
        let applied = match edit {
            ListEdit::Add { value, .. } => ContactsDoc::add_entry(doc, id, key, value),
            ListEdit::Update { value, .. } => ContactsDoc::update_entry(doc, id, key, value),
            ListEdit::Remove { entry_id } => ContactsDoc::remove_entry(doc, id, key, *entry_id),
        };
        if !applied {
            return Err(contact_missing());
        }
    }
    Ok(())
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

/// Create a new stable contact. Creation refuses an existing or tombstoned id.
pub fn create_contact_action(c: &Contact) -> ContactsAction {
    ContactsAction::CreateContact(Box::new(ContactData::from_contact(c)))
}

/// Patch exactly the differences between a form's opening value and saved
/// draft. This is the canonical form-save builder.
pub fn patch_contact_action(base: &Contact, edited: &Contact) -> ContactsAction {
    ContactsAction::PatchContact {
        id: edited.id.to_string(),
        patch: Box::new(ContactPatch::between(base, edited)),
    }
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

/// One field on which a duplicate review can deliberately keep the other
/// contact's scalar value.  Repeated fields are always unioned by their
/// normalized value, so no unique phone, email, group, or address is silently
/// thrown away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MergeField {
    First,
    Last,
    Organization,
    Title,
    Website,
    Birthday,
    Notes,
}

/// Why two contacts were offered for review.  These are derived locally from
/// already-synced contact data and are not written into the shared document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateReason {
    Email,
    Phone,
    Name,
}

impl DuplicateReason {
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Email => "matching email",
            Self::Phone => "matching phone",
            Self::Name => "matching name",
        }
    }
}

/// A bounded, deterministic potential duplicate.  The lower UUID is always
/// the default survivor, which makes the review URL and resource identity
/// stable across peers; the person may still choose every conflicting scalar
/// from either side before confirming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCandidate {
    pub survivor_id: ContactId,
    pub source_id: ContactId,
    pub reasons: Vec<DuplicateReason>,
}

pub fn duplicate_resource_id(survivor_id: ContactId, source_id: ContactId) -> String {
    format!("contacts.duplicate:{survivor_id}:{source_id}")
}

pub fn open_duplicate_review_address(
    survivor_id: ContactId,
    source_id: ContactId,
) -> ActionAddress {
    ActionAddress::new(duplicate_resource_id(survivor_id, source_id), "open-review")
}

/// The single canonical view operation for a locally-derived duplicate
/// candidate.  The candidate remains a review suggestion, never a mutation:
/// the destructive merge still requires the field-by-field confirmation in
/// the review surface.
#[must_use]
pub fn open_duplicate_review_operation(candidate: &DuplicateCandidate) -> ContactsEffect {
    ContactsEffect::OpenReview {
        survivor: candidate.survivor_id,
        source: candidate.source_id,
    }
}

/// Unicode/case-insensitive email key.  The mail local part can technically
/// be case-sensitive; Contacts follows the ordinary address-book expectation
/// here only to *suggest* review — it never merges without a confirmation.
fn normalized_email(value: &str) -> String {
    normalize_text(value.trim())
}

/// Formatting-insensitive phone key.  Keeping only decimal digits lets
/// `+1 (555) 0100` and `15550100` meet, while an empty/non-numeric value never
/// creates a spurious candidate.
fn normalized_phone(value: &str) -> String {
    value.chars().filter(char::is_ascii_digit).collect()
}

fn normalized_name(contact: &Contact) -> String {
    normalize_text(&format!("{} {}", contact.first.trim(), contact.last.trim()))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Find likely duplicate pairs without allocating a full quadratic result for
/// a huge imported book.  The visible resource page caps its children, while
/// this pure helper returns at most `limit` deterministic pairs for callers
/// that have already chosen a bounded presentation budget.
pub fn duplicate_candidates(contacts: &[Contact], limit: usize) -> Vec<DuplicateCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let mut ordered: Vec<&Contact> = contacts.iter().collect();
    ordered.sort_by_key(|contact| contact.id);
    let mut email_owner = HashMap::<String, ContactId>::new();
    let mut phone_owner = HashMap::<String, ContactId>::new();
    let mut name_owner = HashMap::<String, ContactId>::new();
    let mut pairs = BTreeMap::<(ContactId, ContactId), Vec<DuplicateReason>>::new();
    let mut add = |left: ContactId, right: ContactId, reason: DuplicateReason| {
        let pair = left.min(right);
        let other = left.max(right);
        let reasons = pairs.entry((pair, other)).or_default();
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    };
    for contact in ordered {
        for key in contact
            .emails
            .iter()
            .map(|entry| normalized_email(&entry.value))
            .filter(|key| !key.is_empty())
        {
            if let Some(existing) = email_owner.insert(key, contact.id) {
                add(existing, contact.id, DuplicateReason::Email);
            }
        }
        for key in contact
            .phones
            .iter()
            .map(|entry| normalized_phone(&entry.value))
            .filter(|key| !key.is_empty())
        {
            if let Some(existing) = phone_owner.insert(key, contact.id) {
                add(existing, contact.id, DuplicateReason::Phone);
            }
        }
        let name = normalized_name(contact);
        if !name.is_empty()
            && let Some(existing) = name_owner.insert(name, contact.id)
        {
            add(existing, contact.id, DuplicateReason::Name);
        }
    }
    pairs
        .into_iter()
        .take(limit)
        .map(|((survivor_id, source_id), reasons)| DuplicateCandidate {
            survivor_id,
            source_id,
            reasons,
        })
        .collect()
}

/// Resolve one exact candidate without building a catalogue page.  Resource
/// lookup uses this path so a stale/deep link costs only two contacts' fields,
/// not an unbounded scan result allocation.
pub fn duplicate_candidate(survivor: &Contact, source: &Contact) -> Option<DuplicateCandidate> {
    if survivor.id >= source.id {
        return None;
    }
    let mut reasons = Vec::new();
    let source_emails: HashSet<_> = source
        .emails
        .iter()
        .map(|entry| normalized_email(&entry.value))
        .filter(|key| !key.is_empty())
        .collect();
    if survivor
        .emails
        .iter()
        .map(|entry| normalized_email(&entry.value))
        .any(|key| source_emails.contains(&key))
    {
        reasons.push(DuplicateReason::Email);
    }
    let source_phones: HashSet<_> = source
        .phones
        .iter()
        .map(|entry| normalized_phone(&entry.value))
        .filter(|key| !key.is_empty())
        .collect();
    if survivor
        .phones
        .iter()
        .map(|entry| normalized_phone(&entry.value))
        .any(|key| source_phones.contains(&key))
    {
        reasons.push(DuplicateReason::Phone);
    }
    let left_name = normalized_name(survivor);
    if !left_name.is_empty() && left_name == normalized_name(source) {
        reasons.push(DuplicateReason::Name);
    }
    (!reasons.is_empty()).then_some(DuplicateCandidate {
        survivor_id: survivor.id,
        source_id: source.id,
        reasons,
    })
}

fn same_labeled(left: &Labeled, right: &Labeled, phone: bool) -> bool {
    normalize_text(left.label.trim()) == normalize_text(right.label.trim())
        && if phone {
            normalized_phone(&left.value) == normalized_phone(&right.value)
        } else {
            normalized_email(&left.value) == normalized_email(&right.value)
        }
}

fn same_address(left: &Address, right: &Address) -> bool {
    [
        &left.label,
        &left.street,
        &left.city,
        &left.region,
        &left.postal,
        &left.country,
    ]
    .into_iter()
    .zip([
        &right.label,
        &right.street,
        &right.city,
        &right.region,
        &right.postal,
        &right.country,
    ])
    .all(|(left, right)| normalize_text(left.trim()) == normalize_text(right.trim()))
}

fn merged_scalar<S: std::hash::BuildHasher>(
    survivor: &str,
    source: &str,
    field: MergeField,
    choices: &HashSet<MergeField, S>,
) -> String {
    if survivor.trim().is_empty() || choices.contains(&field) {
        source.to_string()
    } else {
        survivor.to_string()
    }
}

/// Materialise the exact reviewed survivor contact.  Empty scalar values fill
/// from the other record; a non-empty conflict stays with the survivor until
/// the review selects `MergeField`.  Repeated rows are a lossless union —
/// the source rows retain their stable UUIDs, so the generated sparse patch
/// uses normal add operations instead of replacing an entire list.
pub fn reviewed_merge_contact<S: std::hash::BuildHasher>(
    survivor: &Contact,
    source: &Contact,
    choices: &HashSet<MergeField, S>,
) -> Contact {
    let mut merged = survivor.clone();
    merged.first = merged_scalar(&survivor.first, &source.first, MergeField::First, choices);
    merged.last = merged_scalar(&survivor.last, &source.last, MergeField::Last, choices);
    merged.org = merged_scalar(
        &survivor.org,
        &source.org,
        MergeField::Organization,
        choices,
    );
    merged.title = merged_scalar(&survivor.title, &source.title, MergeField::Title, choices);
    merged.website = merged_scalar(
        &survivor.website,
        &source.website,
        MergeField::Website,
        choices,
    );
    merged.notes = merged_scalar(&survivor.notes, &source.notes, MergeField::Notes, choices);
    merged.birthday = if survivor.birthday.is_none() || choices.contains(&MergeField::Birthday) {
        source.birthday
    } else {
        survivor.birthday
    };
    for group in &source.groups {
        if !merged
            .groups
            .iter()
            .any(|existing| group_names_equal(&existing.value, &group.value))
        {
            merged.groups.push(group.clone());
        }
    }
    for phone in &source.phones {
        if !merged
            .phones
            .iter()
            .any(|existing| same_labeled(existing, phone, true))
        {
            merged.phones.push(phone.clone());
        }
    }
    for email in &source.emails {
        if !merged
            .emails
            .iter()
            .any(|existing| same_labeled(existing, email, false))
        {
            merged.emails.push(email.clone());
        }
    }
    for address in &source.addresses {
        if !merged
            .addresses
            .iter()
            .any(|existing| same_address(existing, address))
        {
            merged.addresses.push(address.clone());
        }
    }
    merged.favorite |= source.favorite;
    merged
}

/// Canonical destructive action produced after the review screen's explicit
/// field choices.  It carries only a sparse survivor patch, so unrelated peer
/// edits remain convergent and normal action history provides recovery.
pub fn merge_contacts_action<S: std::hash::BuildHasher>(
    survivor: &Contact,
    source: &Contact,
    choices: &HashSet<MergeField, S>,
) -> ContactsAction {
    ContactsAction::MergeContacts {
        survivor_id: survivor.id.to_string(),
        source_id: source.id.to_string(),
        patch: Box::new(ContactPatch::between(
            survivor,
            &reviewed_merge_contact(survivor, source, choices),
        )),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// One deterministic display spelling for every logical group in the shared
/// book.  The key is the same NFC/case-folded identity used by filtering and
/// resource addressing; choosing the lexical spelling makes separate peers
/// publish the same resource label even when Automerge projects rows in a
/// different order.
pub fn ordered_groups(contacts: &[Contact]) -> Vec<String> {
    let mut groups = std::collections::BTreeMap::<String, String>::new();
    for contact in contacts {
        for entry in &contact.groups {
            let name = entry.value.trim();
            if name.is_empty() {
                continue;
            }
            let key = normalize_text(name);
            groups
                .entry(key)
                .and_modify(|existing| {
                    if name < existing.as_str() {
                        *existing = name.to_string();
                    }
                })
                .or_insert_with(|| name.to_string());
        }
    }
    groups.into_values().collect()
}

/// Split a typed full name into first / rest-as-last, so a palette "New
/// contact" / "Rename" that collects a single name string lands the same way
/// the form's two fields would.
pub(crate) fn split_name(name: &str) -> (String, String) {
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

fn invalid_arguments(message: impl Into<String>) -> EncodedAction {
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
        payload: message.into().into_bytes(),
    }
}

/// The complete agent-facing edit surface. Unlike the convenient Rename and
/// Add-to-group prompts, this accepts the typed sparse [`ContactPatch`] JSON
/// directly, covering every scalar and UUID-addressed repeated field without
/// asking a caller to reproduce a private form interaction.
fn patch_spec(contact: &Contact) -> ActionSpec {
    let id = contact.id.to_string();
    ActionSpec::typed(
        "update-fields",
        "Update contact fields",
        "Sparse ContactPatch object",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "patch",
            ParamType::Object,
            "Sparse ContactPatch with scalar fields and UUID-addressed list edits",
        )],
        move |args| {
            let Some(value) = args.to_json().remove("patch") else {
                return invalid_arguments("patch is required");
            };
            let patch = match serde_json::from_value::<ContactPatch>(value) {
                Ok(patch) if !patch.is_empty() => patch,
                Ok(_) => return invalid_arguments("patch must change at least one contact field"),
                Err(error) => return invalid_arguments(format!("invalid ContactPatch: {error}")),
            };
            if let Err(error) = patch.validate() {
                return invalid_arguments(error.to_string());
            }
            encode(&ContactsAction::PatchContact {
                id: id.clone(),
                patch: Box::new(patch),
            })
        },
    )
}

/// Resource and action catalogue for one stable contact identity, the same
/// whether it is on the published page or resolved by id.
pub fn contact_resource(contact: &Contact) -> Resource {
    let id = contact.id.to_string();
    let mut child = Resource::new(
        contact_resource_id(contact.id),
        "contacts.contact",
        contact.display_name(),
    );
    child = child.with_action(
        ContactsEffect::Open {
            contact: contact.id,
        }
        .bound(),
    );

    child = child.with_action(patch_spec(contact));
    child = child.with_action(
        crate::account_editor::AccountEffect::OpenAccounts {
            person: contact.id.to_string(),
        }
        .bound(),
    );

    // Rename — a prompted patch that never carries unrelated fields from
    // the resource snapshot, so peer edits remain intact.
    let base = contact.clone();
    child = child.with_action(ActionSpec::prompted(
        "edit",
        "Rename contact",
        "Name",
        "full name…",
        ExecutionLocus::ClientLocal,
        move |name: &str| {
            let (first, last) = split_name(name);
            let mut edited = base.clone();
            edited.first = first;
            edited.last = last;
            encode(&patch_contact_action(&base, &edited))
        },
    ));

    // Add to a group — a prompted entry-level list patch (deduped,
    // case-insensitive) rather than a full record replacement.
    let base = contact.clone();
    child = child.with_action(ActionSpec::prompted(
        "add-to-group",
        "Add to group",
        "Group",
        "group name…",
        ExecutionLocus::ClientLocal,
        move |group: &str| {
            let mut edited = base.clone();
            let g = group.trim();
            if !g.is_empty() && !edited.groups.iter().any(|x| group_names_equal(&x.value, g)) {
                edited.groups.push(GroupEntry::new(g));
            }
            encode(&patch_contact_action(&base, &edited))
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

    // Export is a privacy-sensitive effect, but it still belongs to this
    // contact resource rather than only to the book-level export-all action.
    // The app opens the same version/portal workflow as the row context menu;
    // no unmediated filesystem path is ever accepted.
    child = child.with_action(export_contact_operation(contact.id).bound());

    // Delete.
    child = child.with_action(
        immediate_spec("delete", "Delete contact", &delete_contact_action(id)).destructive(),
    );

    child
}

/// The minimal exact resource a retained search provider needs to invoke the
/// canonical view action. Rich edit actions are published from the
/// resource's state, not copied into the search index.
pub fn contact_open_resource(id: ContactId, label: &str) -> Resource {
    Resource::new(contact_resource_id(id), "contacts.contact", label)
        .with_action(ContactsEffect::Open { contact: id }.bound())
}

/// Resource and canonical view action for a durable membership group. Groups
/// are derived from UUID-addressed contact rows rather than stored as a second
/// mutable index, so opening one is safely replayable without manufacturing
/// new collaborative state.
pub fn group_resource(name: &str) -> Resource {
    let name = name.trim();
    Resource::new(
        group_resource_id(name),
        "contacts.group",
        format!("Group: {name}"),
    )
    .with_action(
        ContactsEffect::OpenGroup {
            name: name.to_string(),
        }
        .bound(),
    )
}

pub fn favorites_resource() -> Resource {
    Resource::new(FAVORITES_RESOURCE_ID, "contacts.favorites", "Favorites")
        .with_action(ContactsEffect::OpenFavorites.bound())
}

/// The reviewed merge of `source` into `survivor`, offered on their
/// duplicate resource: the caller names the scalar fields to take from the
/// second contact; repeated fields are always unioned.
fn merge_declaration(
    survivor: &Contact,
    source: &Contact,
) -> ui::tilo_resource::ActionDecl<crate::resource::Contacts> {
    let survivor = survivor.clone();
    let source = source.clone();
    ui::tilo_resource::ActionDecl::new(
        "merge",
        "Merge contacts",
        vec![ActionParam::required(
            "take_from_source",
            ParamType::Array,
            "Array of MergeField names: First, Last, Organization, Title, Website, Birthday, Notes",
        )],
        move |args, _| {
            let value = args
                .to_json()
                .remove("take_from_source")
                .ok_or_else(|| "take_from_source is required".to_string())?;
            let fields = serde_json::from_value::<Vec<MergeField>>(value)
                .map_err(|error| format!("invalid merge choices: {error}"))?;
            let choices = fields.into_iter().collect::<HashSet<_>>();
            Ok(merge_contacts_action(&survivor, &source, &choices))
        },
    )
    .describe("Merge the second contact into the first, taking the named fields from the second")
    .destructive()
}

/// A derived, agent-addressable duplicate-review resource.  It offers review
/// first and a confirmed merge second: opening it cannot change shared data,
/// while the merge records every selected scalar field and is subject to the
/// destructive-action confirmation contract.
pub fn duplicate_resource(
    candidate: &DuplicateCandidate,
    survivor: &Contact,
    source: &Contact,
) -> Resource {
    let mut resource = Resource::new(
        duplicate_resource_id(candidate.survivor_id, candidate.source_id),
        "contacts.duplicate",
        format!(
            "Possible duplicate: {} and {}",
            survivor.display_name(),
            source.display_name()
        ),
    );
    resource = resource.with_action(open_duplicate_review_operation(candidate).bound());
    resource.with_action(ui::tilo_resource::declaration_spec(&merge_declaration(
        survivor, source,
    )))
}

/// The duplicate resource of the contacts `survivor_id` and `source_id`, while
/// they are still a likely duplicate.
fn duplicate_of(
    contacts: &[Contact],
    survivor_id: ContactId,
    source_id: ContactId,
) -> Option<Resource> {
    let survivor = contacts.iter().find(|contact| contact.id == survivor_id)?;
    let source = contacts.iter().find(|contact| contact.id == source_id)?;
    let candidate = duplicate_candidate(survivor, source)?;
    (candidate.survivor_id == survivor_id && candidate.source_id == source_id)
        .then(|| duplicate_resource(&candidate, survivor, source))
}

/// The published page of the book's children: favorites, and a bounded
/// number of groups, contacts and duplicate reviews. Everything beyond the
/// page resolves by id through [`resolve_child`].
pub(crate) fn publish_children(contacts: &[Contact]) -> Vec<Resource> {
    let mut children = vec![favorites_resource()];
    for group in ordered_groups(contacts)
        .into_iter()
        .take(MAX_CONTEXT_GROUP_RESOURCES)
    {
        children.push(group_resource(&group));
    }
    for contact in contacts.iter().take(MAX_CONTEXT_CONTACT_RESOURCES) {
        children.push(contact_resource(contact));
    }
    for candidate in duplicate_candidates(contacts, MAX_CONTEXT_DUPLICATE_RESOURCES) {
        if let Some(duplicate) = duplicate_of(contacts, candidate.survivor_id, candidate.source_id)
        {
            children.push(duplicate);
        }
    }
    children
}

/// Resolve one child of the book from the complete current state, without
/// materialising the published page. A missing id stays missing: the broker
/// then refuses a stale search result before it can reach a handler.
pub(crate) fn resolve_child(contacts: &[Contact], id: &str) -> Option<Resource> {
    if id == FAVORITES_RESOURCE_ID {
        return Some(favorites_resource());
    }
    if let Some(rest) = id.strip_prefix("contacts.duplicate:") {
        let (survivor, source) = rest.split_once(':')?;
        return duplicate_of(contacts, survivor.parse().ok()?, source.parse().ok()?);
    }
    if let Some(key) = id.strip_prefix("contacts.group:") {
        return ordered_groups(contacts)
            .into_iter()
            .find(|name| normalize_text(name) == key)
            .map(|name| group_resource(&name));
    }
    let contact_id = id
        .strip_prefix("contacts.contact:")?
        .parse::<ContactId>()
        .ok()?;
    contacts
        .iter()
        .find(|contact| contact.id == contact_id)
        .map(contact_resource)
}

/// The id of the pending review of one imported card whose UID collides with
/// an existing contact.
pub fn import_conflict_resource_id(token: &str) -> String {
    format!("contacts.import-conflict:{token}")
}

/// A pending import review, published while it waits for a decision. It is
/// not document state: an untrusted card never enters the shared book before
/// the person chooses to keep the existing contact or import it as new.
pub(crate) fn import_conflict_row(token: &str, incoming_name: &str) -> ui::framework::LiveRow {
    ui::framework::LiveRow::new(
        import_conflict_resource_id(token),
        "contacts.import-conflict",
        format!("Import review: {incoming_name}"),
    )
    .effect(&ContactsEffect::KeepExisting {
        token: token.to_owned(),
    })
    .effect(&ContactsEffect::ImportAsNew {
        token: token.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{Contacts, sample_book};
    use automerge::AutoCommit;
    use tilo_actions::{ActionInput, LocalTransport};
    use tilo_actions::{ExecutionMode, ResultSpec};
    use tilo_automerge::ApplyIncoming;
    use ui::tilo_resource::Resource as _;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
    }

    /// The sample address book created in a fresh document through its
    /// actions.
    fn seeded_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        for contact in sample_book() {
            create_contact_action(&contact).apply(&mut doc).unwrap();
        }
        doc.commit();
        doc
    }

    fn first_contact(doc: &AutoCommit) -> Contact {
        ContactsDoc::list_contacts(doc)
            .into_iter()
            .next()
            .expect("the sample book is non-empty")
    }

    /// The book `doc` holds as the framework publishes it.
    fn resources_for(doc: &AutoCommit) -> Vec<Resource> {
        vec![ui::tilo_resource::publish::<Contacts>(
            Contacts::KIND,
            Contacts::LABEL,
            &Contacts::read(doc).unwrap(),
        )]
    }

    /// The stored contact a `contacts.contact` resource addresses.
    fn contact_of(doc: &AutoCommit, res: &Resource) -> Contact {
        let id = res.id.as_str().trim_start_matches("contacts.contact:");
        ContactsDoc::list_contacts(doc)
            .into_iter()
            .find(|c| c.id.to_string() == id)
            .expect("the resource addresses a stored contact")
    }

    fn retitled(doc: &AutoCommit) -> EncodedAction {
        let base = first_contact(doc);
        let mut edited = base.clone();
        edited.title = "Renamed via action".into();
        encode(&patch_contact_action(&base, &edited))
    }

    ui::action_contract_tests! {
        seed: seeded_doc(),
        registry: registry(),
        resources: |doc| resources_for(doc),
        palette: ["New contact", "Delete contact", "Export contact", "Rename contact"],
        replay: [
            favorite => |doc| {
                let target = first_contact(doc);
                encode(&set_favorite_action(target.id.to_string(), !target.favorite))
            },
            patch_contact => |doc| retitled(doc),
        ],
        undo: [
            delete_contact => |doc| encode(&delete_contact_action(first_contact(doc).id.to_string())),
            patch_contact => |doc| retitled(doc),
        ],
        encodes: [
            delete => ("contacts.contact", "delete", |doc, res| {
                encode(&delete_contact_action(contact_of(doc, res).id.to_string()))
            }),
            favorite => ("contacts.contact", "favorite", |doc, res| {
                let stored = contact_of(doc, res);
                encode(&set_favorite_action(stored.id.to_string(), !stored.favorite))
            }),
            rename => ("contacts.contact", "edit", arg: "Grace Hopper", |doc, res| {
                let base = contact_of(doc, res);
                let mut renamed = base.clone();
                (renamed.first, renamed.last) = split_name("Grace Hopper");
                encode(&patch_contact_action(&base, &renamed))
            }),
        ],
    }

    /// The contacts tree is bounded yet every contact and group resolves directly, with typed export and search-addressed actions.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn contacts_resource_tree_publishes_bounded_typed_operations() {
        // --- phase: ctrl_k_publishes_the_contacts_tree ---
        // Ctrl+K on Contacts yields the tree: the book with a "New contact" action
        // and a `contacts.contact` child per record exporting rename / add-to-group
        // / favourite / export / delete. Drives the real resource hook.
        {
            let doc = seeded_doc();
            let resources = resources_for(&doc);
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
            assert!(actions.contains(&"export-contact"), "{actions:?}");
            assert!(actions.contains(&"delete"), "{actions:?}");

            let group = book
                .children
                .iter()
                .find(|resource| resource.kind == "contacts.group")
                .expect("a durable group resource");
            assert!(
                group.actions.iter().any(|action| action.id == "open-group"),
                "groups export their canonical open action"
            );

            // The full flattened tree exports every action.
            let flat: Vec<&str> = tilo_actions::flatten(&resources)
                .iter()
                .map(|f| f.action.label.as_str())
                .collect();
            assert!(flat.contains(&"New contact"), "{flat:?}");
            assert!(flat.contains(&"Delete contact"), "{flat:?}");
            assert!(flat.contains(&"Export contact"), "{flat:?}");
            assert!(flat.contains(&"Rename contact"), "{flat:?}");
        }

        // --- phase: contextual_catalogue_is_bounded_but_every_contact_resolves_directly ---
        {
            let contacts = (0..(MAX_CONTEXT_CONTACT_RESOURCES + 2))
                .map(|index| Contact {
                    id: Uuid::new_v4(),
                    first: format!("Person {index:03}"),
                    ..Contact::default()
                })
                .collect::<Vec<_>>();
            let page = publish_children(&contacts);
            assert_eq!(
                page.iter()
                    .filter(|resource| resource.kind == "contacts.contact")
                    .count(),
                MAX_CONTEXT_CONTACT_RESOURCES,
                "the permanent Favorites collection does not consume the bounded contact page"
            );

            let target = contacts.last().expect("fixture is populated");
            let address = open_contact_address(target.id);
            let direct = resolve_child(&contacts, &address.resource)
                .expect("contacts outside the contextual page resolve lazily");
            assert!(
                tilo_actions::door::resolve(&[direct], &address).is_some(),
                "the lazy resource exposes its canonical open action"
            );
            assert!(
                resolve_child(&contacts, &contact_resource_id(Uuid::new_v4())).is_none(),
                "a stale contact id is refused rather than resurrected"
            );
        }

        // --- phase: group_resources_are_canonical_bounded_and_resolve_outside_the_page ---
        {
            let contacts = (0..(MAX_CONTEXT_GROUP_RESOURCES + 2))
                .map(|index| Contact {
                    id: Uuid::new_v4(),
                    first: format!("Person {index:03}"),
                    groups: vec![GroupEntry::new(format!("Group {index:03}"))],
                    ..Contact::default()
                })
                .collect::<Vec<_>>();
            let page = publish_children(&contacts);
            let group_count = page
                .iter()
                .filter(|resource| resource.kind == "contacts.group")
                .count();
            assert_eq!(group_count, MAX_CONTEXT_GROUP_RESOURCES);

            let name = format!("Group {:03}", MAX_CONTEXT_GROUP_RESOURCES + 1);
            let address = open_group_address(&name);
            let resource = resolve_child(&contacts, &address.resource)
                .expect("a group outside the palette page resolves lazily");
            assert!(
                tilo_actions::door::resolve(&[resource], &address).is_some(),
                "the lazy resource exposes the exact group view operation"
            );
        }

        // --- phase: search_provider_addresses_an_exported_contact_action ---
        // Search addresses the exact view action in the live resource tree; it
        // does not mint a parallel navigation namespace.
        {
            let doc = seeded_doc();
            let entries = Contacts::search(&Contacts::read(&doc).unwrap());
            let hit = entries
                .iter()
                .find(|entry| entry.title == "Grace Hopper")
                .expect("search surfaces the real contact");
            assert!(hit.text.contains("US Navy"), "{:?}", hit.text);
            let id = ContactsDoc::list_contacts(&doc)
                .into_iter()
                .find(|contact| contact.display_name() == hit.title)
                .expect("the result names a stored contact")
                .id;
            let address = hit.action.clone().expect("search hit is canonical");
            assert_eq!(address, open_contact_address(id));
            assert!(
                tilo_actions::door::resolve(&resources_for(&doc), &address).is_some(),
                "the address is exported to the palette, accessibility and agents"
            );
            let group = entries
                .iter()
                .find(|entry| entry.title == "Group: Work")
                .expect("groups are found too");
            assert_eq!(group.action, Some(open_group_address("Work")));
        }

        // --- phase: export_contact_is_a_typed_sensitive_operation ---
        // Per-contact export is a typed resource operation, not an unmodelled
        // context-menu side effect. The portal path stays absent from the
        // payload and the action is confirmation-labelled because it can
        // disclose contact data outside the address book.
        {
            let resources = resources_for(&seeded_doc());
            let (contact, export_spec) = ui::testing::action_contract::find_spec(
                &resources,
                "contacts.contact",
                "export-contact",
            );
            let id = contact.id.as_str().trim_start_matches("contacts.contact:");
            assert!(matches!(
                export_spec.contract.risk,
                tilo_actions::RiskLevel::Sensitive
            ));
            assert_eq!(export_spec.contract.execution, ExecutionMode::Immediate);
            assert_eq!(export_spec.contract.result, ResultSpec::None);
            let export = ContactsEffect::from_payload(
                &ui::testing::action_contract::spec_encoding(export_spec, None).payload,
            )
            .expect("export payload is typed");
            assert_eq!(
                export,
                ContactsEffect::ExportContact {
                    contact: id.parse().unwrap(),
                }
            );
        }
    }

    /// Duplicate detection is normalised and bounded; the reviewed merge is destructive, undoable and replays on a peer.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn duplicate_merge_is_reviewed_reversible_and_replayable() {
        // --- phase: reviewed_merge_is_destructive_reversible_and_undo_restores_both_contacts ---
        {
            let mut state = ui::State::new();
            state.doc = seeded_doc();
            let mut contacts = ContactsDoc::list_contacts(&state.doc);
            contacts.sort_by_key(|contact| contact.id);
            let survivor = contacts.remove(0);
            let mut source = contacts.remove(0);
            source.first = survivor.first.clone();
            source.last = survivor.last.clone();
            source
                .emails
                .push(Labeled::new("work", "unique@example.test"));
            ContactsDoc::upsert_contact(&mut state.doc, &source);
            state.doc.commit();
            let survivor = ContactsDoc::list_contacts(&state.doc)
                .into_iter()
                .find(|contact| contact.id == survivor.id)
                .unwrap();
            let source = ContactsDoc::list_contacts(&state.doc)
                .into_iter()
                .find(|contact| contact.id == source.id)
                .unwrap();
            let candidate =
                duplicate_candidate(&survivor, &source).expect("matching names are reviewable");
            let resource = duplicate_resource(&candidate, &survivor, &source);
            let spec = resource
                .actions
                .iter()
                .find(|action| action.id == "merge")
                .unwrap();
            assert_eq!(spec.contract.risk, tilo_actions::RiskLevel::Destructive);
            assert_eq!(
                spec.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
            assert!(spec.contract.reversible, "merge truthfully advertises undo");

            let action = merge_contacts_action(&survivor, &source, &HashSet::new());
            let encoded = encode(&action);
            let decoded = registry().decode(&encoded).unwrap();
            let before_heads = state.doc.get_heads();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
            state.record_committed_action(before_heads, None, None);
            let after = ContactsDoc::list_contacts(&state.doc);
            assert!(!after.iter().any(|contact| contact.id == source.id));
            assert!(
                after
                    .iter()
                    .find(|contact| contact.id == survivor.id)
                    .unwrap()
                    .emails
                    .iter()
                    .any(|email| email.value == "unique@example.test")
            );

            state
                .undo(None, None)
                .expect("merge has one recoverable undo boundary");
            let restored = ContactsDoc::list_contacts(&state.doc);
            assert!(restored.iter().any(|contact| contact.id == source.id));
            assert_eq!(
                restored
                    .iter()
                    .find(|contact| contact.id == survivor.id)
                    .unwrap()
                    .emails,
                survivor.emails,
                "undo restores the survivor rather than only reviving the source"
            );
        }

        // --- phase: duplicate_detector_is_normalized_bounded_and_merge_replays_on_a_peer ---
        {
            let mut seed = seeded_doc();
            let mut contacts = ContactsDoc::list_contacts(&seed);
            contacts.sort_by_key(|contact| contact.id);
            let survivor = contacts.remove(0);
            let mut source = contacts.remove(0);
            source.first = survivor.first.clone();
            source.last = survivor.last.clone();
            source.emails = vec![Labeled::new("home", "ADA@EXAMPLE.TEST")];
            let mut survivor = survivor;
            survivor.emails = vec![Labeled::new("home", "ada@example.test")];
            ContactsDoc::upsert_contact(&mut seed, &survivor);
            ContactsDoc::upsert_contact(&mut seed, &source);
            seed.commit();
            let bytes = seed.save();
            let mut peer = AutoCommit::load(&bytes).unwrap();
            let candidates = duplicate_candidates(&ContactsDoc::list_contacts(&seed), 1);
            assert_eq!(
                candidates.len(),
                1,
                "bounded discovery returns one stable pair"
            );
            assert!(candidates[0].reasons.contains(&DuplicateReason::Email));
            assert!(candidates[0].reasons.contains(&DuplicateReason::Name));
            let action = merge_contacts_action(&survivor, &source, &HashSet::new());
            let encoded = encode(&action);
            let envelope = tilo_actions::apply_local(&mut seed, &action, encoded).unwrap();
            tilo_actions::replay(&mut peer, &envelope, &registry()).unwrap();
            assert_eq!(
                seed.get_heads(),
                peer.get_heads(),
                "reviewed merge converges exactly"
            );
        }
    }

    /// Independent patches converge in any order, row ids survive replay, stale rows are refused atomically and delete tombstones win.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn patches_converge_across_replicas_and_refuse_stale_rows() {
        // --- phase: concurrent_independent_field_and_group_patches_converge ---
        {
            let mut a = seeded_doc();
            a.commit();
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let base = first_contact(&a);

            let mut title_edit = base.clone();
            title_edit.title = "Compiler pioneer".into();
            patch_contact_action(&base, &title_edit)
                .apply(&mut a)
                .unwrap();
            a.commit();

            let mut group_edit = base.clone();
            group_edit.groups.push(GroupEntry::new("Research"));
            patch_contact_action(&base, &group_edit)
                .apply(&mut b)
                .unwrap();
            b.commit();

            a.merge_incoming(&mut b).unwrap();
            let merged = ContactsDoc::list_contacts(&a)
                .into_iter()
                .find(|contact| contact.id == base.id)
                .unwrap();
            assert_eq!(merged.title, "Compiler pioneer");
            assert!(merged.groups.iter().any(|group| group.value == "Research"));
        }

        // --- phase: duplicate_rows_keep_separate_ids_across_edit_remove_and_reordered_replay ---
        {
            let mut seed = seeded_doc();
            let mut contact = first_contact(&seed);
            let first = Labeled::new("mobile", "+1 555 0100");
            let second = Labeled::new("work", "+1 555 0100");
            contact.phones = vec![first.clone(), second];
            ContactsDoc::upsert_contact(&mut seed, &contact);
            seed.commit();
            let bytes = seed.save();

            let mut edit_peer = AutoCommit::load(&bytes).unwrap();
            let mut remove_peer = AutoCommit::load(&bytes).unwrap();
            let base = ContactsDoc::list_contacts(&seed)
                .into_iter()
                .find(|candidate| candidate.id == contact.id)
                .unwrap();
            let mut edited = base.clone();
            edited.phones[0].value = "+1 555 0199".into();
            let edit_action = patch_contact_action(&base, &edited);
            let mut removed = base.clone();
            removed.phones.remove(1);
            let remove_action = patch_contact_action(&base, &removed);

            let edit_envelope =
                tilo_actions::apply_local(&mut edit_peer, &edit_action, encode(&edit_action))
                    .unwrap();
            let remove_envelope =
                tilo_actions::apply_local(&mut remove_peer, &remove_action, encode(&remove_action))
                    .unwrap();
            // Each envelope was authored from the shared base. Replay into a peer
            // that has already advanced locally; the action layer must replay at
            // the pinned base and CRDT-merge, rather than validate against the
            // receiver's current state.
            tilo_actions::replay(&mut edit_peer, &remove_envelope, &registry()).unwrap();
            tilo_actions::replay(&mut remove_peer, &edit_envelope, &registry()).unwrap();

            let merged = ContactsDoc::list_contacts(&edit_peer)
                .into_iter()
                .find(|candidate| candidate.id == contact.id)
                .unwrap();
            assert_eq!(merged.phones.len(), 1);
            assert_eq!(merged.phones[0].id, first.id);
            assert_eq!(merged.phones[0].value, "+1 555 0199");

            assert_eq!(
                ContactsDoc::list_contacts(&edit_peer),
                ContactsDoc::list_contacts(&remove_peer),
                "cross-envelope replay converges UUID-addressed duplicate rows"
            );
        }

        // --- phase: late_stale_row_refusal_is_observationally_atomic ---
        {
            let mut doc = seeded_doc();
            let base = first_contact(&doc);
            let added_phone = Labeled::new("mobile", "+1 555 0188");
            let action = ContactsAction::PatchContact {
                id: base.id.to_string(),
                patch: Box::new(ContactPatch {
                    first: Some("Would partially write".into()),
                    phone_edits: vec![ListEdit::Add {
                        entry_id: added_phone.id,
                        value: added_phone,
                    }],
                    // This comes last in normal application order. Before the
                    // preflight it left the first name and phone mutation behind.
                    address_edits: vec![ListEdit::Remove {
                        entry_id: Uuid::new_v4(),
                    }],
                    ..ContactPatch::default()
                }),
            };
            let heads_before = doc.get_heads();
            let bytes_before = doc.save();
            let content_before = ContactsDoc::list_contacts(&doc);
            assert!(matches!(
                action.apply(&mut doc),
                Err(ActionError::Refused(_))
            ));
            assert_eq!(doc.get_heads(), heads_before, "refusal creates no change");
            assert_eq!(doc.save(), bytes_before, "refusal leaves bytes untouched");
            assert_eq!(ContactsDoc::list_contacts(&doc), content_before);
        }

        // --- phase: delete_tombstone_wins_over_a_cross_envelope_stale_patch ---
        {
            let mut a = seeded_doc();
            a.commit();
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let base = first_contact(&a);
            let mut stale = base.clone();
            stale.notes = "late note".into();
            let patch = patch_contact_action(&base, &stale);

            let delete = delete_contact_action(base.id.to_string());
            let delete_envelope =
                tilo_actions::apply_local(&mut a, &delete, encode(&delete)).unwrap();
            let patch_envelope = tilo_actions::apply_local(&mut b, &patch, encode(&patch)).unwrap();
            tilo_actions::replay(&mut a, &patch_envelope, &registry()).unwrap();
            tilo_actions::replay(&mut b, &delete_envelope, &registry()).unwrap();
            assert!(
                !ContactsDoc::list_contacts(&a)
                    .iter()
                    .any(|contact| contact.id == base.id),
                "a concurrent or reordered stale patch cannot resurrect a tombstoned contact"
            );
            assert!(
                !ContactsDoc::list_contacts(&b)
                    .iter()
                    .any(|contact| contact.id == base.id),
                "the other replay order preserves the tombstone too"
            );
        }

        // --- phase: independent_patches_are_value_convergent_when_replayed_in_either_order ---
        {
            let mut seed = seeded_doc();
            let bytes = seed.save();
            let mut first_then_second = AutoCommit::load(&bytes).unwrap();
            let mut second_then_first = AutoCommit::load(&bytes).unwrap();
            let base = first_contact(&first_then_second);
            let mut org_edit = base.clone();
            org_edit.org = "Navy".into();
            let mut title_edit = base.clone();
            title_edit.title = "Rear admiral".into();
            let org = patch_contact_action(&base, &org_edit);
            let title = patch_contact_action(&base, &title_edit);

            org.apply(&mut first_then_second).unwrap();
            title.apply(&mut first_then_second).unwrap();
            title.apply(&mut second_then_first).unwrap();
            org.apply(&mut second_then_first).unwrap();

            let one = ContactsDoc::list_contacts(&first_then_second);
            let two = ContactsDoc::list_contacts(&second_then_first);
            assert_eq!(one, two, "reordering independent patch envelopes converges");
        }
    }

    /// Create, typed patch and import-review actions apply to the book and refuse malformed, oversized or mismatched input.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn contact_mutation_actions_validate_and_apply() {
        // --- phase: new_contact_action_creates_a_contact ---
        // Dispatching the "New contact" prompted action creates a contact in the
        // document with the committed name split into first/last.
        {
            let mut doc = seeded_doc();
            let resources = resources_for(&doc);
            let spec = resources[0]
                .actions
                .iter()
                .find(|a| a.id == "new-contact")
                .unwrap();
            let before = ContactsDoc::list_contacts(&doc).len();

            let encoded = prompted_encode(spec, "Dana Scott");
            let decoded = registry().decode(&encoded).unwrap();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

            let book = ContactsDoc::list_contacts(&doc);
            assert_eq!(book.len(), before + 1, "the action added a contact");
            let created = book.iter().find(|c| c.first == "Dana" && c.last == "Scott");
            assert!(
                created.is_some(),
                "the created contact carries the committed name"
            );
        }

        // --- phase: typed_field_patch_action_covers_the_complete_contact_form ---
        {
            let mut doc = seeded_doc();
            let contact = first_contact(&doc);
            let resource = contact_resource(&contact);
            let spec = resource
                .actions
                .iter()
                .find(|action| action.id == "update-fields")
                .expect("every contact exports its typed field patch action");

            let encoded = spec
                .build_json(
                    serde_json::json!({"patch": {"title": "Admiral"}})
                        .as_object()
                        .expect("the fixture is an argument object"),
                )
                .expect("the typed patch is accepted at the action door");
            let decoded = registry()
                .decode(&encoded)
                .expect("the resource action is a registered ContactsAction");
            decoded
                .apply(&mut doc)
                .expect("the canonical patch action applies");
            assert_eq!(
                ContactsDoc::list_contacts(&doc)
                    .into_iter()
                    .find(|candidate| candidate.id == contact.id)
                    .expect("target contact remains active")
                    .title,
                "Admiral"
            );

            let empty = spec
                .build_json(
                    serde_json::json!({"patch": {}})
                        .as_object()
                        .expect("the fixture is an argument object"),
                )
                .expect("the invalid payload is still an encoded rejection");
            assert_eq!(
                empty.type_id,
                tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
                "empty patches cannot create meaningless history entries"
            );
        }

        // --- phase: imported_uid_review_actions_are_addressable_and_guard_new_contact_creation ---
        {
            let token = Uuid::new_v4().to_string();
            let review = import_conflict_row(&token, "Incoming Ada").into_resource();
            assert_eq!(review.id.as_str(), import_conflict_resource_id(&token));
            assert!(
                review
                    .actions
                    .iter()
                    .any(|action| action.id == "keep-existing")
            );
            let import_as_new = review
                .actions
                .iter()
                .find(|action| action.id == "import-as-new")
                .expect("review exposes import-as-new");
            assert_eq!(
                import_as_new.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm,
                "adding an untrusted collision as a new contact needs confirmation"
            );
            assert_eq!(
                import_as_new.contract.risk,
                tilo_actions::RiskLevel::Destructive
            );
        }

        // --- phase: repeated_field_actions_refuse_nil_mismatched_and_duplicate_entry_ids ---
        {
            let base = Contact::new();
            let row = Labeled::new("home", "+1 555 0110");
            let mismatched = ContactsAction::PatchContact {
                id: base.id.to_string(),
                patch: Box::new(ContactPatch {
                    phone_edits: vec![ListEdit::Add {
                        entry_id: Uuid::new_v4(),
                        value: row,
                    }],
                    ..ContactPatch::default()
                }),
            };
            let nil = ContactsAction::PatchContact {
                id: base.id.to_string(),
                patch: Box::new(ContactPatch {
                    phone_edits: vec![ListEdit::Remove {
                        entry_id: Uuid::nil(),
                    }],
                    ..ContactPatch::default()
                }),
            };
            let entry_id = Uuid::new_v4();
            let duplicate = ContactsAction::PatchContact {
                id: base.id.to_string(),
                patch: Box::new(ContactPatch {
                    phone_edits: vec![ListEdit::Remove { entry_id }, ListEdit::Remove { entry_id }],
                    ..ContactPatch::default()
                }),
            };
            for action in [mismatched, nil, duplicate] {
                assert!(matches!(
                    action.apply(&mut AutoCommit::new()),
                    Err(ActionError::Refused(_))
                ));
            }
        }

        // --- phase: malformed_and_oversized_mutations_are_refused ---
        {
            let mut doc = AutoCommit::new();
            let malformed = ContactsAction::PatchContact {
                id: "not-a-uuid".into(),
                patch: Box::default(),
            };
            assert!(matches!(
                malformed.apply(&mut doc),
                Err(ActionError::Refused(_))
            ));

            let oversized = ContactsAction::CreateContact(Box::new(ContactData {
                id: uuid::Uuid::new_v4().to_string(),
                notes: "x".repeat(MAX_CONTACT_FIELD_BYTES + 1),
                ..ContactData::from_contact(&Contact::new())
            }));
            assert!(matches!(
                oversized.apply(&mut doc),
                Err(ActionError::Refused(_))
            ));

            let invalid_birthday = ContactsAction::CreateContact(Box::new(ContactData {
                id: uuid::Uuid::new_v4().to_string(),
                birthday: "not-a-date".into(),
                ..ContactData::from_contact(&Contact::new())
            }));
            assert!(matches!(
                invalid_birthday.apply(&mut doc),
                Err(ActionError::Refused(_))
            ));
        }
    }
}
