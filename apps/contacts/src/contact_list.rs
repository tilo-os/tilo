//! The address book's contacts, kept in numbered parts beside the book.
//!
//! One resource cannot hold every contact a workspace ever took at a cost
//! that stays flat: every holder of a resource loads, verifies, diffs and
//! saves all of it, so a change to one contact of thousands would cost
//! thousands. The contacts live in a chain of [`ContactPart`] resources
//! instead (`tilo_app`'s `Chained`), each holding up to [`PART_CONTACTS`]
//! contacts or [`PART_TEXT_BYTES`] of their text and pictures; a change
//! touches the one part that holds its contact. The book's own document
//! ([`crate::resource::Contacts`]) holds nothing of theirs.
//!
//! - **Where a new contact goes.** To the newest part, unless it is full
//!   ([`new_part`]): then to the next. Two writers starting the same part
//!   start the same resource, so their contacts meet there.
//! - **Where a change goes.** To the part holding the contact it names.
//! - **Deleting.** A deleted contact's id is kept in the part that held it
//!   ([`ContactPart::removed`]), and every read hides a contact whose id is
//!   a tombstone in any part; its id never names a contact again, so a late
//!   copy of its card cannot bring it back. A merge leaves the merged
//!   contact where it is and tombstones it: the part holding the survivor
//!   is the one change.
//! - **What reads across parts.** The list, the groups, the favorites and
//!   the likely duplicates read every part together ([`Book`]); nothing is
//!   rewritten in another part.

use std::collections::HashSet;
use std::rc::Rc;

use ui::tilo_resource::{
    self, ActionCx, Derived, Id, Invoke, Keyed, Model, NonBlank, Refusal, SearchEntry, Set,
    resource,
};

use crate::actions::{
    ContactsEffect, MAX_CONTEXT_DUPLICATE_RESOURCES, MergeField, duplicate_candidate,
    duplicate_candidates, reviewed_merge_contact, split_name,
};
use crate::model::{Contact, ContactId, normalize_text, ordered_groups};

/// Contacts a part holds before the next one starts.
pub const PART_CONTACTS: usize = 500;

/// Text, in bytes, a part's contacts hold (names, notes, rows and pictures)
/// before the next one starts: what keeps one part, and so one change, cheap
/// whatever its contacts say.
pub const PART_TEXT_BYTES: usize = 512 * 1024;

/// One part of the book's contacts: every contact made while it was the
/// newest, and everything done to them since.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct ContactPart {
    /// Contacts
    pub contacts: Keyed<Contact>,
    /// The contacts taken out of the book, by id. A deleted contact's id
    /// never names a contact again, so a late copy of its card cannot bring
    /// it back.
    #[model(readonly)]
    pub removed: Set<ContactId>,
}

impl ContactPart {
    /// The text its contacts hold, in bytes: names, organization, title,
    /// website, notes, their rows and their pictures.
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        self.contacts
            .iter()
            .map(|contact| {
                let rows = contact
                    .phones
                    .iter()
                    .chain(&contact.emails)
                    .map(|row| row.label.len().saturating_add(row.value.len()))
                    .sum::<usize>()
                    .saturating_add(
                        contact
                            .addresses
                            .iter()
                            .map(|address| {
                                address
                                    .label
                                    .len()
                                    .saturating_add(address.street.len())
                                    .saturating_add(address.city.len())
                                    .saturating_add(address.region.len())
                                    .saturating_add(address.postal.len())
                                    .saturating_add(address.country.len())
                            })
                            .sum::<usize>(),
                    )
                    .saturating_add(
                        contact
                            .groups
                            .iter()
                            .map(|group| group.value.len())
                            .sum::<usize>(),
                    );
                contact
                    .first
                    .len()
                    .saturating_add(contact.last.len())
                    .saturating_add(contact.org.len())
                    .saturating_add(contact.title.len())
                    .saturating_add(contact.website.len())
                    .saturating_add(contact.notes.len())
                    .saturating_add(contact.photo.len())
                    .saturating_add(rows)
            })
            .sum()
    }

    /// The contact `id`, when this part keeps it.
    #[must_use]
    pub fn contact(&self, id: ContactId) -> Option<&Contact> {
        self.contacts.iter().find(|contact| contact.id == id)
    }

    /// Whether `id` is a contact taken out of the book.
    #[must_use]
    pub fn is_removed(&self, id: &ContactId) -> bool {
        self.removed.contains(id)
    }

    /// What system search finds of the part: each contact, by its name and
    /// everything it holds.
    #[must_use]
    pub fn search(&self) -> Vec<SearchEntry> {
        self.contacts
            .iter()
            .map(|contact| {
                SearchEntry::new(contact.display_name(), searchable_text(contact))
                    .anchored(contact_resource_id(contact.id))
            })
            .collect()
    }

    /// What a part's contacts hold, as [`crate::resource::Contacts::read_document`]
    /// answers.
    ///
    /// # Errors
    /// Refuses an item the part does not hold.
    pub fn read_document(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

/// The part a new contact goes to, given the newest part held (its number
/// and state): that part, or the next once it is full.
#[must_use]
pub fn new_part(head: Option<(u64, &ContactPart)>) -> u64 {
    match head {
        None => 0,
        Some((index, part))
            if part.contacts.len() >= PART_CONTACTS || part.text_bytes() >= PART_TEXT_BYTES =>
        {
            index.saturating_add(1)
        }
        Some((index, _)) => index,
    }
}

/// The book as the window reads it: every contact the parts keep, and the
/// ones taken out of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Book {
    /// The contacts, the ones every part keeps but a tombstone hides left
    /// out.
    pub contacts: Keyed<Contact>,
    /// The contacts taken out of the book.
    pub removed: Set<ContactId>,
}

impl Book {
    /// The book `parts` hold, oldest part first. A contact two parts both
    /// keep (never written so, but read safely) is the newer part's.
    #[must_use]
    pub fn of(parts: &[(u64, Rc<ContactPart>)]) -> Self {
        let mut removed = Set::new();
        for (_, part) in parts {
            for id in part.removed.iter() {
                removed.insert(*id);
            }
        }
        let mut found: HashSet<ContactId> = HashSet::new();
        let mut contacts = Keyed::new();
        for (_, part) in parts {
            for contact in &part.contacts {
                if removed.contains(&contact.id) || !found.insert(contact.id) {
                    continue;
                }
                contacts.push(contact.clone());
            }
        }
        contacts.settle();
        Self { contacts, removed }
    }

    /// The contact `id`, when the book keeps it.
    #[must_use]
    pub fn contact(&self, id: ContactId) -> Option<&Contact> {
        self.contacts.iter().find(|contact| contact.id == id)
    }

    /// Every distinct group across all contacts, sorted alphabetically.
    #[must_use]
    pub fn all_groups(&self) -> Vec<String> {
        ordered_groups(&self.contacts)
    }

    /// Ids of the contacts that pass `query` and the optional `group` filter,
    /// ordered by `sort`: what the list pane renders.
    #[must_use]
    pub fn filtered_sorted(
        &self,
        query: &str,
        group: Option<&str>,
        sort: crate::model::SortKey,
    ) -> Vec<ContactId> {
        let q = normalize_text(query.trim());
        let mut out: Vec<&Contact> = self
            .contacts
            .iter()
            .filter(|candidate| candidate.matches(&q))
            .filter(|candidate| match group {
                None => true,
                Some(group) => candidate
                    .groups
                    .iter()
                    .any(|entry| crate::model::group_names_equal(&entry.value, group)),
            })
            .collect();
        out.sort_by(|a, b| crate::model::sort_cmp(a, b, sort));
        out.into_iter().map(|contact| contact.id).collect()
    }

    /// How many contacts there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.contacts.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.contacts.is_empty()
    }
}

/// The id the contact `id` is stored under.
#[must_use]
pub fn key(id: ContactId) -> Id {
    Id::from_raw(id.to_string())
}

impl ContactPart {
    /// The contact `id` is stored under.
    #[must_use]
    pub fn key(id: ContactId) -> Id {
        key(id)
    }

    /// Adding `contact` to the book.
    #[must_use]
    pub fn add(contact: &Contact) -> Invoke<Self> {
        Self::add_all(vec![contact.clone()])
    }

    /// Adding `contacts` to the book.
    #[must_use]
    pub fn add_all(contacts: Vec<Contact>) -> Invoke<Self> {
        Self::ADD_CONTACTS.with(contacts)
    }

    /// Deleting the contact `id`.
    #[must_use]
    pub fn delete(id: ContactId) -> Invoke<Self> {
        Self::DELETE_CONTACT.with(key(id))
    }

    /// Marking the contact `id` a favorite or not.
    pub fn favorite(id: ContactId, favorite: bool) -> ui::tilo_resource::Edit<Self> {
        Self::paths().contacts.item(&key(id)).favorite.set(favorite)
    }

    /// The edits that change `base` into `edited`, field by field and row by
    /// row: what a form that opened on `base` saves.
    #[must_use]
    pub fn edits(base: &Contact, edited: &Contact) -> Vec<ui::tilo_resource::Edit<Self>> {
        Self::paths()
            .contacts
            .item_path(&key(base.id))
            .edits(base, edited)
    }

    /// Saving a form that opened on `base` as `edited`: its [edits](Self::edits)
    /// as one change, or nothing when it changed nothing.
    #[must_use]
    pub fn save(base: &Contact, edited: &Contact) -> Option<Invoke<Self>> {
        let edits: Vec<Invoke<Self>> = Self::edits(base, edited)
            .iter()
            .filter_map(|edit| Invoke::edit(edit).ok())
            .collect();
        (!edits.is_empty()).then(|| Invoke::group(edits))
    }

    /// Merging `source` into `survivor`, taking the fields `take` from
    /// `source`.
    #[must_use]
    pub fn merge(
        survivor: ContactId,
        source: ContactId,
        take: impl IntoIterator<Item = MergeField>,
    ) -> Invoke<Self> {
        Self::MERGE_CONTACTS.with(contact_part::MergeContacts {
            contact: key(survivor),
            source,
            take_from_source: take.into_iter().collect(),
            merged: None,
        })
    }
}

/// Where the book's groups, favorites and likely duplicates are published:
/// the chain's own children.
#[must_use]
pub fn places(parts: &[(u64, Rc<ContactPart>)]) -> Vec<Derived<ContactPart>> {
    let book = Book::of(parts);
    let favorites = Derived::new("Favorites")
        .of_kind("favorites")
        .identified(crate::resource::FAVORITES_PLACE)
        .with_effect(&ContactsEffect::OpenFavorites);
    let groups = ordered_groups(&book.contacts).into_iter().map(|name| {
        Derived::new(format!("Group: {name}"))
            .of_kind("group")
            .identified(crate::resource::group_place(&name))
            .with_effect(&ContactsEffect::OpenGroup { name })
    });
    let duplicates = duplicate_candidates(&book.contacts, MAX_CONTEXT_DUPLICATE_RESOURCES)
        .into_iter()
        .filter_map(|candidate| {
            let survivor = book.contact(candidate.survivor_id)?;
            let source = book.contact(candidate.source_id)?;
            Some(
                Derived::new(format!(
                    "Possible duplicate: {} and {}",
                    survivor.display_name(),
                    source.display_name()
                ))
                .of_kind("duplicate")
                .identified(crate::resource::duplicate_place(survivor.id, source.id))
                .with_effect(&ContactsEffect::ReadDuplicate {
                    survivor: survivor.id,
                    source: source.id,
                })
                .with_effect(&ContactsEffect::OpenReview {
                    survivor: survivor.id,
                    source: source.id,
                })
                .with(
                    ContactPart::MERGE_CONTACTS
                        .bind(
                            contact_part::MergeContacts::paths().contact,
                            key(survivor.id),
                        )
                        .and(contact_part::MergeContacts::paths().source, source.id),
                ),
            )
        });
    std::iter::once(favorites)
        .chain(groups)
        .chain(duplicates)
        .collect()
}

/// What search finds beyond the contacts themselves: every group reveals
/// its members.
#[must_use]
pub fn search(parts: &[(u64, Rc<ContactPart>)]) -> Vec<SearchEntry> {
    let book = Book::of(parts);
    ordered_groups(&book.contacts)
        .into_iter()
        .map(|group| {
            let place = crate::resource::place_resource_id(&crate::resource::group_place(&group));
            SearchEntry::new(format!("Group: {group}"), "Filter contacts in this group")
                .opening(place, &ContactsEffect::OpenGroup { name: group })
        })
        .collect()
}

/// Whether `survivor` and `source` are a likely duplicate of the book.
#[must_use]
pub fn duplicate(parts: &[(u64, Rc<ContactPart>)], survivor: ContactId, source: ContactId) -> bool {
    let book = Book::of(parts);
    book.contact(survivor)
        .zip(book.contact(source))
        .and_then(|(survivor, source)| duplicate_candidate(survivor, source))
        .is_some()
}

/// What a person may look a contact up by, one field per line so a query
/// never matches across two of them.
#[must_use]
pub fn searchable_text(contact: &Contact) -> String {
    let mut fields = vec![
        contact.first.clone(),
        contact.last.clone(),
        contact.org.clone(),
        contact.title.clone(),
        contact.website.clone(),
        contact.notes.clone(),
    ];
    if let Some(birthday) = contact.birthday {
        fields.extend([birthday.iso(), birthday.pretty()]);
    }
    for row in contact.phones.iter().chain(&contact.emails) {
        fields.extend([row.label.clone(), row.value.clone()]);
    }
    fields.extend(
        contact
            .addresses
            .iter()
            .map(crate::model::Address::one_line),
    );
    fields.extend(contact.groups.iter().map(|group| group.value.clone()));
    fields.retain(|field| !field.trim().is_empty());
    fields.join("\n")
}

/// The resource address of the contact `id`, the one its reference chip, its
/// search hit and its `open` name, whichever part keeps it.
#[must_use]
pub fn contact_resource_id(id: ContactId) -> String {
    tilo_resource::items::item_resource_id_in(
        <ContactPart as tilo_resource::Resource>::KIND,
        "contacts",
        &key(id),
    )
}

/// The part `action` changes, given the parts held, oldest first: the one
/// holding the contact it names, or for a new contact the newest part (or
/// the next, once that is full).
///
/// # Errors
/// Refuses an action naming a contact no part held here keeps.
pub fn part_for(
    action: &<ContactPart as tilo_resource::Resource>::Action,
    parts: &[(u64, Rc<ContactPart>)],
) -> Result<u64, String> {
    let holding = |contact: &Id| {
        parts
            .iter()
            .rev()
            .find(|(_, part)| part.contacts.find(contact).is_some())
            .map(|(index, _)| *index)
            .ok_or_else(no_contact)
    };
    match &action.operation {
        ContactPartAction::NewContact(_) | ContactPartAction::AddContacts(_) => Ok(new_part(
            parts.last().map(|(index, part)| (*index, &**part)),
        )),
        other => match item_of(other) {
            Some(contact) => holding(contact),
            None => Ok(parts.last().map_or(0, |(index, _)| *index)),
        },
    }
}

/// The contact a verb on one contact names.
#[must_use]
pub fn item_of(action: &ContactPartAction) -> Option<&Id> {
    use contact_part as verb;
    match action {
        ContactPartAction::DeleteContact(verb::DeleteContact { contact })
        | ContactPartAction::MergeContacts(verb::MergeContacts { contact, .. }) => Some(contact),
        // A field edit of one contact names it in its path.
        ContactPartAction::FieldEdit(edit) => match edit.path() {
            [
                tilo_resource::Segment::Field(field),
                tilo_resource::Segment::Item(contact),
                ..,
            ] if field == "contacts" => Some(contact),
            _ => None,
        },
        _ => None,
    }
}

/// Fill in, where `action` is invoked, what it reads of the whole book, and
/// refuse what it does not allow: a contact id already in the book or taken
/// out of it, a merge of a contact that is not one.
///
/// # Errors
/// Refuses as the action's verb would, had it every part.
pub fn prepare(
    action: &mut <ContactPart as tilo_resource::Resource>::Action,
    parts: &[(u64, Rc<ContactPart>)],
) -> Result<(), String> {
    use contact_part::{AddContacts, MergeContacts};
    let book = Book::of(parts);
    match &mut action.operation {
        ContactPartAction::AddContacts(AddContacts { contacts }) => {
            for contact in contacts.iter() {
                if book.removed.contains(&contact.id) || book.contact(contact.id).is_some() {
                    return Err("a contact with this id is in the book or was deleted".to_owned());
                }
            }
        }
        ContactPartAction::MergeContacts(MergeContacts {
            contact,
            source,
            take_from_source,
            merged,
        }) => {
            let survivor = book.contacts.find(contact).ok_or_else(no_contact)?;
            if survivor.id == *source {
                return Err("a contact is not merged into itself".to_owned());
            }
            let other = book
                .contact(*source)
                .ok_or_else(|| "no contact to merge".to_owned())?;
            let choices: HashSet<MergeField> = take_from_source.iter().copied().collect();
            *merged = Some(reviewed_merge_contact(survivor, other, &choices));
        }
        _ => {}
    }
    Ok(())
}

fn no_contact() -> String {
    Refusal::new("no such contact").to_string()
}

#[resource(
    kind = "contacts.contacts",
    params = contact_part,
    search = ContactPart::search,
    read_content = ContactPart::read_document
)]
impl ContactPart {
    /// New contact.
    ///
    /// A contact with a typed full name: its first word is the first name,
    /// the rest the last name.
    #[action(step = "create")]
    pub fn new_contact(&mut self, cx: &mut ActionCx, #[param("Full name")] name: NonBlank) {
        let (first, last) = split_name(&name);
        self.contacts.push(Contact {
            id: cx.uuid(),
            first,
            last,
            ..Contact::default()
        });
        self.contacts.settle();
    }

    /// Add contacts.
    ///
    /// Whole cards, written in the form or imported, added as one change.
    /// Each id is new to the book: neither a contact's nor a deleted
    /// contact's.
    #[action(step = "create")]
    pub fn add_contacts(
        &mut self,
        #[param("The contacts")] contacts: Vec<Contact>,
    ) -> Result<(), Refusal> {
        if contacts.is_empty() {
            return Err(Refusal::new("no contacts to add"));
        }
        for contact in contacts {
            self.contacts.push(contact);
        }
        self.contacts.settle();
        Ok(())
    }

    /// Delete contact.
    #[action(destructive, item = contacts, step = "delete")]
    pub fn delete_contact(&mut self, contact: &Id) -> Result<(), Refusal> {
        let removed = self.contacts.take(contact).ok_or_else(no_contact)?;
        self.removed.insert(removed.id);
        Ok(())
    }

    /// Merge contacts.
    ///
    /// Merges another contact into this one and deletes it. Every phone
    /// number, email address, postal address and group of either is kept;
    /// a field both fill keeps this contact's value unless it is named to
    /// be taken from the other.
    ///
    /// The merged contact is filled in where the action is invoked, from
    /// both contacts wherever their parts are.
    #[action(destructive, item = contacts)]
    pub fn merge_contacts(
        &mut self,
        contact: &Id,
        #[param("The contact merged into it and deleted")] source: ContactId,
        #[param(
            "The fields to take from the merged contact: First, Last, Organization, Title, \
             Website, Birthday, Notes"
        )]
        take_from_source: Vec<MergeField>,
        #[param("The two contacts as one: filled in where it is invoked")] merged: Option<Contact>,
    ) -> Result<(), Refusal> {
        let _ = take_from_source;
        let merged = merged.ok_or_else(|| Refusal::new("no contact to merge"))?;
        let survivor = self.contacts.find_mut(contact).ok_or_else(no_contact)?;
        *survivor = merged;
        self.removed.insert(source);
        Ok(())
    }
}

#[cfg(test)]
mod tests;

/// The chain the parts belong to, beside the book's own document.
#[must_use]
pub fn chain() -> ui::resource::DocumentId {
    ui::resource::DocumentId::derived("contacts", &ui::resource::DocumentId::workspace_default())
}

impl ui::framework::Chained for ContactPart {
    type Base = crate::resource::Contacts;

    fn chain() -> ui::resource::DocumentId {
        chain()
    }

    fn part_for(action: &Self::Action, parts: &[(u64, Rc<Self>)]) -> Option<Result<u64, String>> {
        Some(part_for(action, parts))
    }

    fn prepare(
        action: &mut Self::Action,
        _book: &crate::resource::Contacts,
        parts: &[(u64, Rc<Self>)],
        _invoker: &ui::tilo_actions::ActionInvoker,
    ) -> Result<(), String> {
        prepare(action, parts)
    }

    fn places(_book: &crate::resource::Contacts, parts: &[(u64, Rc<Self>)]) -> Vec<Derived<Self>> {
        places(parts)
    }

    fn search_over(
        _book: &crate::resource::Contacts,
        parts: &[(u64, Rc<Self>)],
    ) -> Vec<SearchEntry> {
        search(parts)
    }

    /// The list shows every contact, so a window holds every part; a part
    /// bounds what one change costs, not what is held.
    const HELD_WHOLE: bool = true;
}
