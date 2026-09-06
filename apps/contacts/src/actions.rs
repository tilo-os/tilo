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
//! never touches the RNG or the wall-clock. Invalid or stale payloads are
//! explicitly refused, so callers can distinguish rejection from success and
//! a bad replay never changes the document.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

use qos_actions::{
    Action, ActionAddress, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry,
    ActionSpec, Doc, EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType,
    Resource, ResourceProvider, ResultSpec, RetryPolicy,
};

use crate::doc::ContactsDoc;
use crate::model::{Address, Birthday, Contact, ContactId, GroupEntry, Labeled};

/// Registry `type_id` for every contacts action.
pub const CONTACTS_ACTION_TYPE: &str = "contacts.action";
pub const CONTACTS_OPERATION_TYPE: &str = "contacts.operation";

/// Stable resource id for one contact. Search, the palette, accessibility and
/// remote callers all address this same resource rather than translating a
/// second, private navigation token.
pub fn contact_resource_id(id: ContactId) -> String {
    format!("contacts.contact:{id}")
}

/// Canonical view action that reveals one contact in the app.
pub fn open_contact_address(id: ContactId) -> ActionAddress {
    ActionAddress::new(contact_resource_id(id), "open")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContactsOperation {
    Open { id: String },
    ImportVcard { path: Option<String> },
    ExportAllVcard { path: Option<String> },
}

#[cfg(test)]
mod file_action_tests {
    use super::*;
    #[test]
    fn path_argument_binds_into_background_import() {
        let spec = file_operation("import-vcard", "Import vCard", false);
        let encoded = spec
            .build_json(
                serde_json::json!({"path":"/tmp/a.vcf"})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        let decoded: ContactsOperation = serde_json::from_slice(&encoded.payload).unwrap();
        assert_eq!(
            decoded,
            ContactsOperation::ImportVcard {
                path: Some("/tmp/a.vcf".into())
            }
        );
        assert!(matches!(
            spec.contract.execution,
            ExecutionMode::Background { .. }
        ));
        assert_eq!(spec.contract.result, ResultSpec::Job);
    }
}

fn contact_operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    operation: &ContactsOperation,
) -> Option<ActionSpec> {
    let spec = ActionSpec::operation(id, label, kind, CONTACTS_OPERATION_TYPE, operation).ok()?;
    match operation {
        ContactsOperation::Open { .. } => Some(spec.retry_policy(RetryPolicy::Idempotent)),
        // These invocations synchronously enqueue a host-portal request and
        // return. The app has no durable job handle or cancellation channel,
        // so advertising a cancellable background lifecycle would be false.
        ContactsOperation::ImportVcard { .. } => Some(spec),
        ContactsOperation::ExportAllVcard { .. } => Some(spec.destructive()),
    }
}

fn file_operation(id: &str, label: &str, export: bool) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "optional absolute path",
        OperationKind::Effect,
        CONTACTS_OPERATION_TYPE,
        vec![ActionParam::optional(
            "path",
            ParamType::Text,
            "vCard path; omit to use the system picker",
        )],
        move |args: &ActionArgs| {
            let path = args
                .text("path")
                .filter(|p| !p.trim().is_empty())
                .map(str::to_string);
            let operation = if export {
                ContactsOperation::ExportAllVcard { path }
            } else {
                ContactsOperation::ImportVcard { path }
            };
            EncodedAction::encode_operation(CONTACTS_OPERATION_TYPE, &operation).unwrap_or_else(
                |error| EncodedAction {
                    type_id: qos_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                    payload: format!("contacts file operation could not be serialized: {error}")
                        .into_bytes(),
                },
            )
        },
    )
    .with_contract(ActionContract {
        risk: if export {
            qos_actions::RiskLevel::Destructive
        } else {
            qos_actions::RiskLevel::Low
        },
        confirmation: if export {
            qos_actions::ConfirmationPolicy::Confirm
        } else {
            qos_actions::ConfirmationPolicy::None
        },
        execution: ExecutionMode::Background { cancellable: true },
        result: ResultSpec::Job,
        ..ActionContract::default()
    })
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
}

/// Durable, browseable history metadata for an address-book mutation.
pub fn history_context(action: &ContactsAction) -> qos_actions::ActionContext {
    let mut context =
        qos_actions::ActionContext::for_action("org.qos.Contacts", "Contacts", action);
    let (label, id, name) = match action {
        ContactsAction::CreateContact(contact) => {
            let name = format!("{} {}", contact.first, contact.last)
                .trim()
                .to_string();
            ("Create contact", contact.id.as_str(), name)
        }
        ContactsAction::PatchContact { id, .. } => {
            ("Update contact", id.as_str(), "Contact".into())
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
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

fn apply_patch(doc: &mut Doc, id: &str, patch: &ContactPatch) -> Result<(), ActionError> {
    patch.validate()?;
    let id = id
        .parse::<ContactId>()
        .map_err(|_| ActionError::Refused("contact id is invalid".into()))?;
    preflight_patch(doc, id, patch)?;
    apply_scalar_patch(doc, id, patch)?;
    apply_group_edits(doc, id, &patch.group_edits)?;
    apply_labeled_edits(doc, id, "phones", &patch.phone_edits)?;
    apply_labeled_edits(doc, id, "emails", &patch.email_edits)?;
    apply_address_edits(doc, id, &patch.address_edits)
}

/// Reject an invalid/stale row target before an action mutates even one scalar
/// or list entry. `Action::apply` is not an implicit transaction, so this
/// read-only gate is what makes a refused patch observationally atomic.
fn preflight_patch(doc: &Doc, id: ContactId, patch: &ContactPatch) -> Result<(), ActionError> {
    if !ContactsDoc::contact_is_active(doc, id) {
        return Err(contact_missing());
    }
    preflight_list_edits(&patch.group_edits, |entry_id| {
        ContactsDoc::group_row_exists(doc, id, entry_id)
    })?;
    preflight_list_edits(&patch.phone_edits, |entry_id| {
        ContactsDoc::labeled_row_exists(doc, id, "phones", entry_id)
    })?;
    preflight_list_edits(&patch.email_edits, |entry_id| {
        ContactsDoc::labeled_row_exists(doc, id, "emails", entry_id)
    })?;
    preflight_list_edits(&patch.address_edits, |entry_id| {
        ContactsDoc::address_row_exists(doc, id, entry_id)
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

fn apply_group_edits(
    doc: &mut Doc,
    id: ContactId,
    edits: &[ListEdit<GroupEntry>],
) -> Result<(), ActionError> {
    for edit in edits {
        let applied = match edit {
            ListEdit::Add { value, .. } => ContactsDoc::add_group(doc, id, value),
            ListEdit::Update { value, .. } => ContactsDoc::update_group(doc, id, value),
            ListEdit::Remove { entry_id } => ContactsDoc::remove_group(doc, id, *entry_id),
        };
        if !applied {
            return Err(contact_missing());
        }
    }
    Ok(())
}

fn apply_labeled_edits(
    doc: &mut Doc,
    id: ContactId,
    key: &str,
    edits: &[ListEdit<Labeled>],
) -> Result<(), ActionError> {
    for edit in edits {
        let applied = match edit {
            ListEdit::Add { value, .. } => ContactsDoc::add_labeled(doc, id, key, value),
            ListEdit::Update { value, .. } => ContactsDoc::update_labeled(doc, id, key, value),
            ListEdit::Remove { entry_id } => ContactsDoc::remove_labeled(doc, id, key, *entry_id),
        };
        if !applied {
            return Err(contact_missing());
        }
    }
    Ok(())
}

fn apply_address_edits(
    doc: &mut Doc,
    id: ContactId,
    edits: &[ListEdit<Address>],
) -> Result<(), ActionError> {
    for edit in edits {
        let applied = match edit {
            ListEdit::Add { value, .. } => ContactsDoc::add_address(doc, id, value),
            ListEdit::Update { value, .. } => ContactsDoc::update_address(doc, id, value),
            ListEdit::Remove { entry_id } => ContactsDoc::remove_address(doc, id, *entry_id),
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
    let mut import = file_operation("import-vcard", "Import vCard", false);
    if ctx.import_busy {
        import = import.unavailable("already_running", "A contact import is already in progress");
    }
    root = root.with_action(import);
    let mut export = file_operation("export-all-vcard", "Export all contacts", true);
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
            encode(&create_contact_action(&c))
        },
    ));

    for contact in &ctx.contacts {
        let id = contact.id.to_string();
        let mut child = Resource::new(
            contact_resource_id(contact.id),
            "contacts.contact",
            contact.display_name(),
        );
        if let Some(open) = contact_operation(
            "open",
            "Open contact",
            OperationKind::View,
            &ContactsOperation::Open { id: id.clone() },
        ) {
            child = child.with_action(open);
        }

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
                if !g.is_empty()
                    && !edited
                        .groups
                        .iter()
                        .any(|x| x.value.eq_ignore_ascii_case(g))
                {
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

    /// Replaying a granular field patch also converges
    /// byte-identically — the create/edit path, not just the favourite toggle.
    #[test]
    fn patch_contact_replays_byte_identical() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let mut c = first_contact(&a);
        c.title = "Renamed via action".into();
        let base = first_contact(&b);
        let action = patch_contact_action(&base, &c);
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

    #[test]
    fn concurrent_independent_field_and_group_patches_converge() {
        let (_app, mut a) = seeded();
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

        a.merge(&mut b).unwrap();
        let merged = ContactsDoc::list_contacts(&a)
            .into_iter()
            .find(|contact| contact.id == base.id)
            .unwrap();
        assert_eq!(merged.title, "Compiler pioneer");
        assert!(merged.groups.iter().any(|group| group.value == "Research"));
    }

    #[test]
    fn duplicate_rows_keep_separate_ids_across_edit_remove_and_reordered_replay() {
        let (_app, mut seed) = seeded();
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
            qos_actions::apply_local(&mut edit_peer, &edit_action, encode(&edit_action)).unwrap();
        let remove_envelope =
            qos_actions::apply_local(&mut remove_peer, &remove_action, encode(&remove_action))
                .unwrap();
        // Each envelope was authored from the shared base. Replay into a peer
        // that has already advanced locally; the action layer must replay at
        // the pinned base and CRDT-merge, rather than validate against the
        // receiver's current state.
        qos_actions::replay(&mut edit_peer, &remove_envelope, &registry()).unwrap();
        qos_actions::replay(&mut remove_peer, &edit_envelope, &registry()).unwrap();

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

    #[test]
    fn repeated_field_actions_refuse_nil_mismatched_and_duplicate_entry_ids() {
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

    #[test]
    fn late_stale_row_refusal_is_observationally_atomic() {
        let (_app, mut doc) = seeded();
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

    #[test]
    fn delete_tombstone_wins_over_a_cross_envelope_stale_patch() {
        let (_app, mut a) = seeded();
        a.commit();
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let base = first_contact(&a);
        let mut stale = base.clone();
        stale.notes = "late note".into();
        let patch = patch_contact_action(&base, &stale);

        let delete = delete_contact_action(base.id.to_string());
        let delete_envelope = qos_actions::apply_local(&mut a, &delete, encode(&delete)).unwrap();
        let patch_envelope = qos_actions::apply_local(&mut b, &patch, encode(&patch)).unwrap();
        qos_actions::replay(&mut a, &patch_envelope, &registry()).unwrap();
        qos_actions::replay(&mut b, &delete_envelope, &registry()).unwrap();
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

    #[test]
    fn independent_patches_are_value_convergent_when_replayed_in_either_order() {
        let (_app, mut seed) = seeded();
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

    #[test]
    fn malformed_and_oversized_mutations_are_refused() {
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

    /// Single source of truth: the palette's "Delete contact" / "Favorite" specs
    /// and the builders the trash button / star call encode byte-identically,
    /// and the prompted "Rename" spec matches the patch builder for the same
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

        // Rename: prompted spec == patch builder for the renamed record.
        let edit_spec = contact.actions.iter().find(|a| a.id == "edit").unwrap();
        let from_spec = prompted_encode(edit_spec, "Grace Hopper");
        let base = stored.clone();
        let mut renamed = stored;
        let (first, last) = split_name("Grace Hopper");
        renamed.first = first;
        renamed.last = last;
        let from_builder = encode(&patch_contact_action(&base, &renamed));
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

    /// Search addresses the exact view action in the live resource tree; it
    /// does not mint a parallel navigation namespace.
    #[test]
    fn search_provider_addresses_an_exported_contact_action() {
        let (app, doc) = seeded();

        // The mutation side is live.
        assert!(
            !Widget::resources(&app, &doc).is_empty(),
            "resources are exported"
        );

        // The search side finds the contact and names its exported action.
        let providers = Widget::search_providers(&app);
        assert_eq!(providers.len(), 1, "one contacts search provider");
        let hits = providers[0].search("hopper");
        let hit = hits
            .iter()
            .find(|r| r.title == "Grace Hopper")
            .expect("the provider surfaces the real matching contact");
        let id = ContactsDoc::list_contacts(&doc)
            .into_iter()
            .find(|contact| contact.display_name() == hit.title)
            .expect("the result names a stored contact")
            .id;
        let address = hit.action_address().expect("search hit is canonical");
        assert_eq!(address, open_contact_address(id));
        assert!(
            qos_actions::door::resolve(&Widget::resources(&app, &doc), &address).is_some(),
            "the address is exported to the palette, accessibility and agents"
        );
    }
}
