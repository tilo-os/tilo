//! The address book, its verbs, the places derived from it, and the app.
//!
//! A contact's fields change through the field edits every path offers
//! (`Contacts::paths().contacts.item(&id).favorite.set(true)`), and a saved
//! form is exactly the edits between the contact it opened and the contact
//! it saves ([`Contacts::save`]), so a stale form never overwrites what
//! someone else changed meanwhile. The methods below are the operations
//! that are more than setting a field: adding, deleting and merging
//! contacts. Groups, the favorites and likely duplicates are derived from
//! the contacts and published as places that reveal them.

use std::collections::HashSet;
use std::rc::Rc;

use ui::framework::{LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_resource::{
    self, ActionCx, Derived, Edit, Id, Invoke, Keyed, Model, Refusal, SearchEntry, Set, resource,
};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{
    ContactsEffect, MAX_CONTEXT_DUPLICATE_RESOURCES, MergeField, duplicate_candidate,
    duplicate_candidates, reviewed_merge_contact, split_name,
};
use crate::app::ContactsApp;
use crate::model::{
    Birthday, Contact, ContactId, Labeled, normalize_text, ordered_groups, sample_contacts,
};

/// The address book.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Contacts {
    /// Contacts
    pub contacts: Keyed<Contact>,
    /// The contacts deleted from the book. A deleted contact's id never
    /// names a contact again, so a late copy of its card cannot bring it
    /// back.
    #[model(readonly)]
    pub removed: Set<ContactId>,
}

#[resource(
    kind = "contacts.book",
    label = "Contacts",
    children = Contacts::places,
    search = Contacts::search
)]
impl Contacts {
    /// New contact.
    ///
    /// A contact with a typed full name: its first word is the first name,
    /// the rest the last name.
    #[action]
    pub fn new_contact(
        &mut self,
        cx: &mut ActionCx,
        #[param("Full name")] name: String,
    ) -> Result<(), Refusal> {
        let (first, last) = split_name(&name);
        if first.is_empty() {
            return Err(Refusal::new("a new contact has a name"));
        }
        self.contacts.push(Contact {
            id: cx.uuid(),
            first,
            last,
            ..Contact::default()
        });
        self.contacts.settle();
        Ok(())
    }

    /// Add contacts.
    ///
    /// Whole cards, written in the form or imported, added as one change.
    /// Each id is new to the book: neither a contact's nor a deleted
    /// contact's.
    #[action]
    pub fn add_contacts(
        &mut self,
        #[param("The contacts")] contacts: Vec<Contact>,
    ) -> Result<(), Refusal> {
        if contacts.is_empty() {
            return Err(Refusal::new("no contacts to add"));
        }
        for contact in contacts {
            if self.removed.contains(&contact.id) || self.contact(contact.id).is_some() {
                return Err(Refusal::new(
                    "a contact with this id is in the book or was deleted",
                ));
            }
            self.contacts.push(contact);
        }
        self.contacts.settle();
        Ok(())
    }

    /// Delete contact.
    #[action(destructive, item = contacts)]
    pub fn delete_contact(&mut self, contact: &Id) -> Result<(), Refusal> {
        let removed = self
            .contacts
            .take(contact)
            .ok_or_else(|| Refusal::new("no such contact"))?;
        self.removed.insert(removed.id);
        Ok(())
    }

    /// Merge contacts.
    ///
    /// Merges another contact into this one and deletes it. Every phone
    /// number, email address, postal address and group of either is kept;
    /// a field both fill keeps this contact's value unless it is named to
    /// be taken from the other.
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
    ) -> Result<(), Refusal> {
        let survivor = self
            .contacts
            .find(contact)
            .ok_or_else(|| Refusal::new("no such contact"))?;
        if survivor.id == source {
            return Err(Refusal::new("a contact is not merged into itself"));
        }
        let other = self
            .contact(source)
            .ok_or_else(|| Refusal::new("no contact to merge"))?;
        let choices: HashSet<MergeField> = take_from_source.into_iter().collect();
        let merged = reviewed_merge_contact(survivor, other, &choices);
        if let Some(survivor) = self.contacts.find_mut(contact) {
            *survivor = merged;
        }
        self.contacts.retain(|candidate| candidate.id != source);
        self.removed.insert(source);
        Ok(())
    }
}

impl Contacts {
    /// The contact `id`.
    pub fn contact(&self, id: ContactId) -> Option<&Contact> {
        self.contacts.iter().find(|contact| contact.id == id)
    }

    /// The id the contact `id` is stored under.
    pub fn key(id: ContactId) -> Id {
        Id::from_raw(id.to_string())
    }

    /// Adding `contact`, whole.
    pub fn add(contact: &Contact) -> Invoke<Self> {
        Self::add_all(vec![contact.clone()])
    }

    /// Adding `contacts`, whole, as one change.
    pub fn add_all(contacts: Vec<Contact>) -> Invoke<Self> {
        Self::ADD_CONTACTS.with(contacts)
    }

    /// Deleting the contact `id`.
    pub fn delete(id: ContactId) -> Invoke<Self> {
        Self::DELETE_CONTACT.with(Self::key(id))
    }

    /// Making the contact `id` a favorite, or no longer one.
    pub fn favorite(id: ContactId, favorite: bool) -> Edit<Self> {
        Self::paths()
            .contacts
            .item(&Self::key(id))
            .favorite
            .set(favorite)
    }

    /// The edits that change `base` into `edited`, field by field and row by
    /// row: what a form that opened on `base` saves.
    pub fn edits(base: &Contact, edited: &Contact) -> Vec<Edit<Self>> {
        Self::paths()
            .contacts
            .item_path(&Self::key(base.id))
            .edits(base, edited)
    }

    /// Saving a form that opened on `base` as `edited`: its [edits](Self::edits)
    /// as one change, or nothing when it changed nothing.
    pub fn save(base: &Contact, edited: &Contact) -> Option<Invoke<Self>> {
        let edits: Vec<Invoke<Self>> = Self::edits(base, edited)
            .iter()
            .filter_map(|edit| Invoke::edit(edit).ok())
            .collect();
        (!edits.is_empty()).then(|| Invoke::group(edits))
    }

    /// Merging `source` into `survivor`, taking the fields `take` from
    /// `source`.
    pub fn merge(
        survivor: ContactId,
        source: ContactId,
        take: impl IntoIterator<Item = MergeField>,
    ) -> Invoke<Self> {
        Self::MERGE_CONTACTS.with(contacts::MergeContacts {
            contact: Self::key(survivor),
            source,
            take_from_source: take.into_iter().collect(),
        })
    }

    /// The favorites, every group and the likely duplicates (a bounded
    /// number), as places that reveal them.
    fn places(&self) -> Vec<Derived<Self>> {
        let favorites = Derived::new("Favorites")
            .of_kind("favorites")
            .identified(FAVORITES_PLACE)
            .with_effect(&ContactsEffect::OpenFavorites);
        let groups = ordered_groups(&self.contacts).into_iter().map(|name| {
            Derived::new(format!("Group: {name}"))
                .of_kind("group")
                .identified(group_place(&name))
                .with_effect(&ContactsEffect::OpenGroup { name })
        });
        let duplicates = duplicate_candidates(&self.contacts, MAX_CONTEXT_DUPLICATE_RESOURCES)
            .into_iter()
            .filter_map(|candidate| {
                let survivor = self.contact(candidate.survivor_id)?;
                let source = self.contact(candidate.source_id)?;
                Some(
                    Derived::new(format!(
                        "Possible duplicate: {} and {}",
                        survivor.display_name(),
                        source.display_name()
                    ))
                    .of_kind("duplicate")
                    .identified(duplicate_place(survivor.id, source.id))
                    .with_effect(&ContactsEffect::OpenReview {
                        survivor: survivor.id,
                        source: source.id,
                    })
                    .with(
                        Self::MERGE_CONTACTS
                            .bind(
                                contacts::MergeContacts::paths().contact,
                                Self::key(survivor.id),
                            )
                            .and(contacts::MergeContacts::paths().source, source.id),
                    ),
                )
            });
        std::iter::once(favorites)
            .chain(groups)
            .chain(duplicates)
            .collect()
    }

    /// Every contact is found by anything it holds — name, organisation,
    /// numbers, addresses, notes, groups — and opens; every group reveals
    /// its members.
    fn search(&self) -> Vec<SearchEntry> {
        let contacts = self.contacts.iter().map(|contact| {
            SearchEntry::new(contact.display_name(), searchable_text(contact))
                .anchored(contact_resource_id(contact.id))
        });
        let groups = ordered_groups(&self.contacts).into_iter().map(|group| {
            let place = place_resource_id(&group_place(&group));
            SearchEntry::new(format!("Group: {group}"), "Filter contacts in this group")
                .opening(place, &ContactsEffect::OpenGroup { name: group })
        });
        contacts.chain(groups).collect()
    }

    /// Whether `survivor` and `source` are still a likely duplicate.
    pub fn duplicate(&self, survivor: ContactId, source: ContactId) -> bool {
        self.contact(survivor)
            .zip(self.contact(source))
            .and_then(|(survivor, source)| duplicate_candidate(survivor, source))
            .is_some()
    }
}

/// The id of the favorites' place.
pub const FAVORITES_PLACE: &str = "favorites";

/// The id of the place of the group `name`, the same for every spelling
/// of it.
pub fn group_place(name: &str) -> String {
    format!("group:{}", normalize_text(name.trim()))
}

/// The id of the place of a likely duplicate.
pub fn duplicate_place(survivor: ContactId, source: ContactId) -> String {
    format!("duplicate:{survivor}:{source}")
}

/// The published id of a place of the book.
pub fn place_resource_id(place: &str) -> String {
    format!(
        "{}/derived/{place}",
        <Contacts as tilo_resource::Resource>::KIND
    )
}

/// The published id of the contact `id`.
pub fn contact_resource_id(id: ContactId) -> String {
    tilo_resource::items::item_resource_id::<Contacts>("contacts", &Contacts::key(id))
}

/// What a person may look a contact up by, one field per line so a query
/// never matches across two of them.
fn searchable_text(contact: &Contact) -> String {
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
    for row in contact.phones.iter().chain(contact.emails.iter()) {
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

/// The sample address book a capture shows, with stable ids so every run
/// draws the same scene.
pub(crate) fn sample_book() -> Vec<Contact> {
    let mut next = 0_u128;
    let mut id = || {
        next += 1;
        Uuid::from_u128(next)
    };
    sample_contacts()
        .into_iter()
        .map(|mut contact| {
            contact.id = id();
            for row in contact.phones.iter_mut().chain(contact.emails.iter_mut()) {
                row.id = id();
            }
            for address in &mut contact.addresses {
                address.id = id();
            }
            for group in &mut contact.groups {
                group.id = id();
            }
            contact
        })
        .collect()
}

/// The two sample contacts the duplicate review compares: every scalar field
/// differs, so the review shows its longest field chooser.
fn review_pair(book: &mut [Contact]) {
    let [survivor, source, ..] = book else {
        return;
    };
    source.first = "Augusta".into();
    source.last = "King".into();
    source.org = "Alternative organization".into();
    source.title = "Different role".into();
    source.website = "https://other.example.test".into();
    source.notes = "A deliberately longer alternate note for the review layout.".into();
    source.birthday = Some(Birthday::MonthDay { month: 12, day: 10 });
    source.emails = vec![Labeled::new("home", "ada@example.test")].into();
    survivor.org = "Primary organization".into();
    survivor.title = "Primary role".into();
    survivor.website = "https://primary.example.test".into();
    survivor.notes = "A deliberately longer primary note for the review layout.".into();
    survivor.birthday = Some(Birthday::MonthDay { month: 1, day: 1 });
    survivor.emails = vec![Labeled::new("home", "ada@example.test")].into();
}

/// Framework app: the contacts widget is the surface over [`Contacts`].
pub struct ContactsShell;

/// What the binary runs instead of opening a window.
///
/// `--probe-contact-directory-engine VALUE` (the value is ignored) runs the
/// contact directory's self-check. It needs `runtime-engine`, the release
/// build, where that code is in `libtilo_contact_directory_engine.so`; the
/// check proves Contacts loads that library and runs it.
const COMMANDS: &[ui::framework::Command] = &[ui::framework::Command {
    option: "--probe-contact-directory-engine",
    run: probe_contact_directory_engine,
}];

#[cfg(feature = "runtime-engine")]
fn probe_contact_directory_engine(_: &str) -> Result<(), String> {
    let report = tilo_contact_directory_engine::self_check()?;
    println!("contacts --probe-contact-directory-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_contact_directory_engine(_: &str) -> Result<(), String> {
    Err("the contact directory's engine library needs the `runtime-engine` feature".to_string())
}

impl ResourceApp for ContactsShell {
    type Resource = Contacts;
    const ID: &'static str = "so.tilo.contacts";
    const TITLE: &'static str = "Contacts";
    const ICON: Option<&'static str> = Some("contacts");
    const OPENS: &'static [&'static str] = &["text/vcard", "text/x-vcard"];
    const ARGUMENTS: usize = 1;

    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::account_editor::CATALOG)
    }

    fn view(cx: &mut ViewCx<Contacts>) -> Element {
        crate::account_editor::build(cx)
    }

    /// `new`, `edit`, `editfocus` and `edit-lower` show the contact form;
    /// `export`, `unsaved`, `import-conflict` and
    /// `duplicate-review` its dialogs; `groups` and `context` its menus;
    /// `duplicates` a discovered likely duplicate; `search` a filtered list;
    /// `empty` a fresh install; `loading` an import in progress; `error` the
    /// feedback of a failed import; `detail` the narrow single-pane layout on
    /// the pushed detail of a contact.
    fn scenarios() -> &'static [&'static str] {
        &[
            "new",
            "edit",
            "editfocus",
            "edit-lower",
            "export",
            "unsaved",
            "groups",
            "context",
            "import-conflict",
            "duplicate-review",
            "duplicates",
            "empty",
            "search",
            "loading",
            "error",
            "detail",
        ]
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some("detail") => (390, 844),
            _ => (1280, 800),
        }
    }

    /// The sample book, so a capture shows the address book doing real
    /// work; `empty` shows a fresh install, which has none.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Contacts>> {
        let mut book = match name {
            Some("empty") => return Vec::new(),
            _ => sample_book(),
        };
        match name {
            Some("duplicate-review") => review_pair(&mut book),
            Some("duplicates") => {
                if let Some(first) = book.first() {
                    let mut copy = first.clone();
                    copy.id = Uuid::from_u128(u128::from(u32::MAX));
                    copy.notes =
                        "A second import with the same identity; review before merging.".into();
                    book.push(copy);
                }
            }
            _ => {}
        }
        vec![Contacts::add_all(book)]
    }
}

impl ResourceSurface<Contacts, ContactsEffect> for ContactsApp {
    fn effect(
        &mut self,
        effect: ContactsEffect,
        cx: &mut ui::framework::EffectCx<Contacts, Self>,
    ) -> ui::framework::EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn present(&mut self, state: &Rc<Contacts>) {
        self.present_book(state);
        self.project_identities(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Contacts>) {
        self.attach_dispatcher(cx.dispatcher());
        self.jobs = Some(cx.jobs());
        if cx.mode().is_live() {
            self.identity_directory =
                cx.shared_person::<tilo_contact_directory::ContactDirectory>();
        }
        // A vCard file the app was opened with ("open with", a dropped
        // file) is imported into the address book.
        if let Some(file) = cx.launch().file() {
            self.open_cards(String::from_utf8_lossy(file.bytes()).into_owned());
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_capture(scenario);
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        let mut rows = self.import_conflict_rows();
        if let Some(editor) = &self.account_editor {
            rows.extend(crate::account_editor::review_rows(editor));
        }
        rows
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.live_rows().into_iter().find(|row| row.id() == id)
    }
}

impl ContactsApp {
    /// Labels are a projection of this book's stable UUIDs. Service endpoints
    /// are independently confirmed in the shared person-owned directory and
    /// never inferred from matching names, emails or telephone numbers.
    fn project_identities(&self, state: &Contacts) {
        use tilo_contact_directory::{ContactDirectory, Projected};
        let Some(directory) = &self.identity_directory else {
            return;
        };
        let current = directory.get();
        let people: Vec<Projected> = state
            .contacts
            .iter()
            .map(|contact| Projected {
                person: Contacts::key(contact.id),
                label: contact.display_name(),
                phones: contact
                    .phones
                    .iter()
                    .map(|phone| phone.value.clone())
                    .collect(),
                emails: contact
                    .emails
                    .iter()
                    .map(|email| email.value.clone())
                    .collect(),
                photo: contact.photo.clone(),
            })
            .filter(|projected| {
                current.people.get(&projected.person).is_none_or(|person| {
                    !person.removed
                        && (person.label != projected.label
                            || person.phones.0 != projected.phones
                            || person.emails.0 != projected.emails
                            || person.photo != projected.photo)
                })
            })
            .collect();
        let removed: Vec<_> = state
            .removed
            .iter()
            .map(|removed| Contacts::key(*removed))
            .filter(|id| current.people.get(id).is_none_or(|person| !person.removed))
            .collect();
        // One change for the whole book, however many people it touches.
        for chunk in people.chunks(tilo_contact_directory::MAX_PROJECTED) {
            let _ = directory
                .invoke(&ContactDirectory::PROJECT_CONTACTS.with((chunk.to_vec(), Vec::new())));
        }
        if !removed.is_empty() {
            let _ =
                directory.invoke(&ContactDirectory::PROJECT_CONTACTS.with((Vec::new(), removed)));
        }
    }
}

#[cfg(test)]
mod tests;
