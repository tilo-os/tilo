//! Contacts as a [`tilo_resource::Resource`]: one action vocabulary, one
//! published tree for the address book.
//!
//! Document mutations (new contact, per-contact patch / favourite / delete,
//! the reviewed merge of a likely duplicate) live on this resource via
//! [`Resource::declarations`] / [`Resource::children`]; a book larger than the
//! published page resolves the rest by id ([`Resource::child`]). Import and
//! export are Effects with no document delta ([`Resource::local_operations`]).
//! Imported cards waiting for review are live rows of the surface: they are
//! not in the document until the person decides.

use std::collections::HashMap;
use std::rc::Rc;

use ui::framework::{LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionParam, PROMPTED_PARAM, ParamType, Resource as PublishedResource};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{
    CONTACTS_ACTION_TYPE, ContactsAction, ContactsEffect, contact_resource_id,
    create_contact_action, field_patch, open_contact_address, open_group_address, ordered_groups,
    publish_children, register, resolve_child, split_name,
};
use crate::app::ContactsApp;
use crate::doc::ContactsDoc;
use crate::model::{Birthday, Contact, ContactId, GroupEntry, Labeled, sample_contacts};

/// The address book's state: every contact, in document order.
#[derive(Clone, Debug, Default)]
pub struct ContactsState {
    pub contacts: Vec<Contact>,
    pub removed: Vec<Uuid>,
}

/// The contacts book resource.
pub struct Contacts;

impl Resource for Contacts {
    const KIND: &'static str = "contacts.book";
    const LABEL: &'static str = "Contacts";
    const ACTION_TYPE: &'static str = CONTACTS_ACTION_TYPE;

    type State = ContactsState;
    type Action = ContactsAction;

    fn register(registry: &mut ui::tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<ContactsState, ModelError> {
        Ok(ContactsState {
            contacts: ContactsDoc::list_contacts(doc),
            removed: ContactsDoc::removed_contacts(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        let optional =
            |name, description| ActionParam::optional(name, ParamType::Text, description);
        vec![
            ActionDecl::new(
                "new-contact",
                "New contact",
                vec![
                    ActionParam::required(PROMPTED_PARAM, ParamType::Text, "Full name"),
                    optional("org", "Organisation or company"),
                    optional("title", "Job title or role"),
                    optional("email", "Email address"),
                    optional("phone", "Phone number"),
                    optional("notes", "Notes"),
                    optional("group", "Group to add the contact to"),
                ],
                |args, _| {
                    let text = |name| args.text(name).map(str::trim).unwrap_or_default();
                    let (first, last) = split_name(text(PROMPTED_PARAM));
                    let mut contact = Contact::new();
                    contact.first = first;
                    contact.last = last;
                    contact.org = text("org").to_string();
                    contact.title = text("title").to_string();
                    contact.notes = args.text("notes").unwrap_or_default().to_string();
                    if !text("email").is_empty() {
                        contact.emails.push(Labeled::new("work", text("email")));
                    }
                    if !text("phone").is_empty() {
                        contact.phones.push(Labeled::new("mobile", text("phone")));
                    }
                    if !text("group").is_empty() {
                        contact.groups.push(GroupEntry::new(text("group")));
                    }
                    Ok(create_contact_action(&contact))
                },
            )
            .describe("Create a contact; answers its id.")
            .answering(),
        ]
    }

    /// `update-contact` names its contact by id, so it is bound to the
    /// first email and phone row of each, which `email` and `phone` edit.
    fn bound_declarations(state: &ContactsState) -> Vec<ActionDecl<Self>> {
        let primaries: HashMap<ContactId, Contact> = state
            .contacts
            .iter()
            .map(|contact| {
                let mut primary = Contact {
                    id: contact.id,
                    ..Contact::default()
                };
                primary.emails.extend(contact.emails.first().cloned());
                primary.phones.extend(contact.phones.first().cloned());
                (contact.id, primary)
            })
            .collect();
        let optional =
            |name, description| ActionParam::optional(name, ParamType::Text, description);
        vec![
            ActionDecl::new(
                "update-contact",
                "Update contact",
                vec![
                    ActionParam::required("id", ParamType::Text, "The contact's id"),
                    optional("name", "Full name"),
                    optional("org", "Organisation or company"),
                    optional("title", "Job title or role"),
                    optional("email", "Primary email address (\"\" removes it)"),
                    optional("phone", "Primary phone number (\"\" removes it)"),
                    optional("notes", "Notes, replacing the old ones"),
                    optional("website", "Website"),
                    optional("birthday", "Birthday, YYYY-MM-DD or --MM-DD"),
                ],
                move |args, _| {
                    let mut fields = args.to_json();
                    let id: ContactId = fields
                        .remove("id")
                        .and_then(|id| id.as_str().and_then(|id| id.trim().parse().ok()))
                        .ok_or("id is a contact id, as list-contacts answers it")?;
                    let contact = primaries.get(&id).ok_or("no contact has this id")?;
                    let patch = field_patch(contact, fields)?;
                    if patch.is_empty() {
                        return Err("name at least one field to set".into());
                    }
                    Ok(ContactsAction::PatchContact {
                        id: id.to_string(),
                        patch: Box::new(patch),
                    })
                },
            )
            .describe("Set fields of the contact with this id; changes only the fields given."),
        ]
    }

    /// `new-contact` answers the contact it made.
    fn answer(
        _doc: &Doc,
        action: &str,
        changes: &[ui::automerge::Patch],
    ) -> Option<serde_json::Value> {
        if action != "new-contact" {
            return None;
        }
        let made = changes.iter().find_map(|patch| {
            let [(_, ui::automerge::Prop::Map(map))] = patch.path.as_slice() else {
                return None;
            };
            match &patch.action {
                ui::automerge::PatchAction::PutMap { key, .. } if map == "contacts" => {
                    key.parse::<ContactId>().ok()
                }
                _ => None,
            }
        })?;
        Some(serde_json::json!({
            "id": made.to_string(),
            "resource": contact_resource_id(made),
        }))
    }

    fn children(state: &ContactsState) -> Vec<PublishedResource> {
        publish_children(&state.contacts)
    }

    fn child(state: &ContactsState, id: &str) -> Option<PublishedResource> {
        resolve_child(&state.contacts, id)
    }

    /// Every contact is found by anything it holds — name, organisation,
    /// numbers, addresses, notes, groups — and opens; every group reveals
    /// its members.
    fn search(state: &ContactsState) -> Vec<SearchEntry> {
        let contacts = state.contacts.iter().map(|contact| {
            SearchEntry::new(contact.display_name(), searchable_text(contact))
                .anchored(contact_resource_id(contact.id))
                .invoking(open_contact_address(contact.id))
        });
        let groups = ordered_groups(&state.contacts).into_iter().map(|group| {
            SearchEntry::new(format!("Group: {group}"), "Filter contacts in this group")
                .invoking(open_group_address(&group))
        });
        contacts.chain(groups).collect()
    }
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
    source.emails = vec![Labeled::new("home", "ada@example.test")];
    survivor.org = "Primary organization".into();
    survivor.title = "Primary role".into();
    survivor.website = "https://primary.example.test".into();
    survivor.notes = "A deliberately longer primary note for the review layout.".into();
    survivor.birthday = Some(Birthday::MonthDay { month: 1, day: 1 });
    survivor.emails = vec![Labeled::new("home", "ada@example.test")];
}

/// Framework app: the contacts widget is the surface over [`Contacts`].
pub struct ContactsShell;

impl ResourceApp for ContactsShell {
    type Resource = Contacts;
    const ID: &'static str = "so.tilo.contacts";
    const TITLE: &'static str = "Contacts";
    const ICON: Option<&'static str> = Some("contacts");
    const OPENS: &'static [&'static str] = &["text/vcard", "text/x-vcard"];
    const ARGUMENTS: usize = 1;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::account_editor::CATALOG)
    }

    fn view(cx: &mut ViewCx<Contacts>) -> Element {
        crate::account_editor::build(cx)
    }

    /// `new`, `edit`, `editfocus` and `edit-lower` show the contact form;
    /// `export`, `unsaved`, `confirm`, `import-conflict` and
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
            "confirm",
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
        book.iter()
            .filter_map(|contact| Invoke::apply(&create_contact_action(contact)).ok())
            .collect()
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

    fn present(&mut self, state: &Rc<ContactsState>) {
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
    fn project_identities(&self, state: &ContactsState) {
        use tilo_contact_directory::ContactDirectory;
        use ui::tilo_resource::Id;
        let Some(directory) = &self.identity_directory else {
            return;
        };
        let current = directory.get();
        for contact in &state.contacts {
            let id = Id::from_raw(contact.id.to_string());
            let label = contact.display_name();
            if current
                .people
                .get(&id)
                .is_none_or(|person| !person.removed && person.label != label)
            {
                let _ =
                    directory.invoke(&ContactDirectory::PROJECT_CONTACT.with((id.clone(), label)));
            }
            let phones = contact
                .phones
                .iter()
                .map(|phone| phone.value.clone())
                .collect::<Vec<_>>();
            if current
                .people
                .get(&id)
                .is_none_or(|person| !person.removed && person.phones.0 != phones)
            {
                let _ =
                    directory.invoke(&ContactDirectory::PROJECT_PHONES.with((id.clone(), phones)));
            }
            let emails = contact
                .emails
                .iter()
                .map(|email| email.value.clone())
                .collect::<Vec<_>>();
            if current
                .people
                .get(&id)
                .is_none_or(|person| !person.removed && person.emails.0 != emails)
            {
                let _ = directory.invoke(&ContactDirectory::PROJECT_EMAILS.with((id, emails)));
            }
        }
        for removed in &state.removed {
            let id = Id::from_raw(removed.to_string());
            if current.people.get(&id).is_none_or(|person| !person.removed) {
                let _ = directory.invoke(&ContactDirectory::REMOVE_CONTACT.with(id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{
        FAVORITES_RESOURCE_ID, MAX_CONTEXT_CONTACT_RESOURCES, duplicate_resource_id,
        group_resource_id,
    };
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::OperationKind;

    #[test]
    fn stable_contacts_project_without_inferred_endpoints_and_deletions_disable_identity() {
        use tilo_contact_directory::ContactDirectory;
        use ui::tilo_resource::Id;
        let mut app = TestApp::<ContactsShell>::for_person("contacts-fixture-owner").unwrap();
        let directory = app.shared_person::<ContactDirectory>().unwrap();
        let mut first = Contact::new();
        first.first = "Alex".into();
        first.phones.push(Labeled::new("mobile", "+12025550123"));
        first.emails.push(Labeled::new("work", "Alex@EXAMPLE.test"));
        let mut second = Contact::new();
        second.first = "Alex".into();
        app.invoke(Invoke::apply(&create_contact_action(&first)).unwrap())
            .unwrap();
        app.invoke(Invoke::apply(&create_contact_action(&second)).unwrap())
            .unwrap();
        app.settle();
        let projected = directory.get();
        assert_eq!(projected.people.len(), 2);
        for contact in [&first, &second] {
            let identity = projected
                .people
                .get(&Id::from_raw(contact.id.to_string()))
                .unwrap();
            assert_eq!(identity.label, "Alex");
            assert!(
                identity.endpoints.is_empty(),
                "telephone fields require confirmation"
            );
            assert!(!identity.removed);
        }
        assert_eq!(
            projected.phone_candidates("+12025550123"),
            vec![Id::from_raw(first.id.to_string())]
        );
        assert_eq!(
            projected.email_candidates("Alex@example.test"),
            vec![Id::from_raw(first.id.to_string())]
        );
        assert!(projected.resolve("email", "Alex@example.test").is_err());
        assert!(projected.resolve("sms", "+12025550123").is_err());
        app.invoke_confirmed(
            Invoke::apply(&ContactsAction::DeleteContact {
                id: first.id.to_string(),
            })
            .unwrap(),
        )
        .unwrap();
        app.settle();
        let projected = directory.get();
        assert!(projected.email_candidates("Alex@example.test").is_empty());
        assert!(
            projected
                .people
                .get(&Id::from_raw(first.id.to_string()))
                .unwrap()
                .removed
        );
        assert!(
            !projected
                .people
                .get(&Id::from_raw(second.id.to_string()))
                .unwrap()
                .removed
        );
    }

    fn has_control(app: &mut TestApp<ContactsShell>, label: &str) -> bool {
        app.harness()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.label() == Some(label))
    }

    fn find<'a>(resource: &'a PublishedResource, id: &str) -> Option<&'a PublishedResource> {
        resource
            .children
            .iter()
            .find(|child| child.id.as_str() == id)
    }

    /// A vCard file the app is opened with is imported into the book, as
    /// an import the person picked: created through the door, undone in one
    /// step.
    #[test]
    fn a_vcard_file_the_app_is_opened_with_is_imported() {
        let cards = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\nEND:VCARD\r\n\
                     BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Alan Turing\r\nN:Turing;Alan;;;\r\nEND:VCARD\r\n";
        let (file, _receipt) =
            ui::application::OpenFile::new("friends.vcf", "text/vcard", cards.as_bytes().to_vec())
                .unwrap();
        let mut app = TestApp::<ContactsShell>::handed(file).unwrap();
        app.harness().relayout();
        let mut names: Vec<String> = app
            .state()
            .contacts
            .iter()
            .map(crate::model::Contact::display_name)
            .collect();
        names.sort();
        assert_eq!(names, ["Ada Lovelace", "Alan Turing"]);
        assert!(app.undo());
        assert!(app.state().contacts.is_empty(), "one import, one undo step");
    }

    /// A new window starts on an empty book; captures stage the sample book
    /// through actions and their view state through `stage`.
    #[test]
    fn captures_stage_the_sample_book_and_a_new_window_is_empty() {
        let live = TestApp::<ContactsShell>::new().unwrap();
        assert!(live.state().contacts.is_empty(), "nothing is fabricated");

        let empty = TestApp::<ContactsShell>::capture(Some("empty")).unwrap();
        assert!(empty.state().contacts.is_empty());

        let mut edit = TestApp::<ContactsShell>::capture(Some("edit")).unwrap();
        assert_eq!(edit.state().contacts.len(), sample_book().len());
        assert!(has_control(&mut edit, "Save"), "the edit form is open");

        let mut duplicates = TestApp::<ContactsShell>::capture(Some("duplicates")).unwrap();
        assert_eq!(duplicates.state().contacts.len(), sample_book().len() + 1);
        let surface = duplicates.agent_surface().to_string();
        assert!(
            surface.contains("contacts.duplicate:"),
            "the staged duplicate is published for review"
        );

        let mut groups = TestApp::<ContactsShell>::capture(Some("groups")).unwrap();
        let options: Vec<_> = groups
            .harness()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.role() == ui::accesskit::Role::TreeItem)
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect();
        assert_eq!(
            options
                .iter()
                .filter(|option| *option == "Favorites")
                .count(),
            1,
            "the shared sidebar lists Favorites once: {options:?}"
        );

        let mut detail = TestApp::<ContactsShell>::capture(Some("detail")).unwrap();
        assert_eq!(detail.harness().inspection().viewport, [390, 844]);
    }

    /// The book publishes its actions, favorites, groups, contacts and
    /// likely duplicates through the one published tree; children beyond
    /// the page resolve by id, and a stale id resolves nothing.
    #[test]
    fn the_published_tree_pages_children_and_resolves_the_rest() {
        let mut contacts = sample_book();
        let mut copy = contacts[0].clone();
        copy.id = Uuid::from_u128(u128::MAX);
        contacts.push(copy.clone());
        for n in 0..MAX_CONTEXT_CONTACT_RESOURCES {
            let mut extra = Contact::new();
            extra.id = Uuid::from_u128(1_000 + n as u128);
            extra.first = format!("Extra {n}");
            contacts.push(extra);
        }
        let mut late = Contact::new();
        late.id = Uuid::from_u128(9_999);
        late.first = "Late".into();
        late.groups = vec![crate::model::GroupEntry::new("Late group")];
        contacts.push(late.clone());
        let state = ContactsState {
            contacts,
            ..ContactsState::default()
        };

        let book = ui::tilo_resource::publish::<Contacts>(Contacts::KIND, "Contacts", &state);
        assert!(
            book.actions.iter().any(|action| action.id == "new-contact"),
            "new-contact is on the book"
        );
        assert!(find(&book, FAVORITES_RESOURCE_ID).is_some());
        let ada = find(&book, &contact_resource_id(state.contacts[0].id)).unwrap();
        for id in [
            "open",
            "edit",
            "add-to-group",
            "favorite",
            "export-contact",
            "delete",
            "update-fields",
        ] {
            assert!(ada.actions.iter().any(|action| action.id == id), "{id}");
        }
        let duplicate_id = duplicate_resource_id(state.contacts[0].id, copy.id);
        let duplicate = find(&book, &duplicate_id).expect("the duplicate is on the page");
        let merge = duplicate
            .actions
            .iter()
            .find(|action| action.id == "merge")
            .expect("the duplicate offers its merge");
        assert_eq!(merge.kind, OperationKind::Document);
        assert_eq!(
            merge.contract.risk,
            ui::tilo_actions::RiskLevel::Destructive
        );
        assert!(
            duplicate
                .actions
                .iter()
                .any(|action| action.id == "open-review")
        );

        let late_id = contact_resource_id(late.id);
        assert!(find(&book, &late_id).is_none(), "the page is bounded");
        let resolved = Contacts::child(&state, &late_id).expect("a late contact resolves");
        assert_eq!(resolved.kind, "contacts.contact");
        let group = Contacts::child(&state, &group_resource_id("late group"))
            .expect("a group resolves by its canonical key");
        assert_eq!(group.kind, "contacts.group");
        assert!(Contacts::child(&state, &duplicate_id).is_some());
        assert!(
            Contacts::child(&state, &contact_resource_id(Uuid::from_u128(4))).is_none(),
            "a stale id never redirects to another contact"
        );
    }

    /// The merge a duplicate publishes builds the reviewed merge: the source
    /// is gone, the survivor takes the chosen fields, and one undo restores
    /// both.
    #[test]
    fn a_published_merge_merges_and_one_undo_restores_both() {
        let mut app = TestApp::<ContactsShell>::new().unwrap();
        let mut survivor = Contact::new();
        survivor.id = Uuid::from_u128(1);
        survivor.first = "Ada".into();
        survivor.last = "Lovelace".into();
        survivor.emails = vec![Labeled::new("home", "ada@example.test")];
        let mut source = survivor.clone();
        source.id = Uuid::from_u128(2);
        source.org = "Analytical Engines".into();
        for contact in [&survivor, &source] {
            app.invoke(Invoke::apply(&create_contact_action(contact)).unwrap())
                .unwrap();
        }
        let state = app.state();
        let duplicate = Contacts::child(&state, &duplicate_resource_id(survivor.id, source.id))
            .expect("a likely duplicate");
        let merge = duplicate
            .actions
            .iter()
            .find(|action| action.id == "merge")
            .unwrap();
        let mut args = serde_json::Map::new();
        args.insert(
            "take_from_source".into(),
            serde_json::json!(["Organization"]),
        );
        let encoded = merge.build_json(&args).unwrap();
        let action: ContactsAction = serde_json::from_slice(&encoded.payload).unwrap();
        app.invoke_confirmed(Invoke::apply(&action).unwrap())
            .unwrap();

        let merged = app.state().contacts;
        assert_eq!(merged.len(), 1, "the source is merged away");
        assert_eq!(merged[0].id, survivor.id);
        assert_eq!(merged[0].org, "Analytical Engines");
        assert!(app.undo(), "the merge is one undo step");
        assert_eq!(app.state().contacts.len(), 2);
    }

    /// The window's own controls edit the book through its door: a star
    /// toggled in the detail pane is the book's action, and one undo takes
    /// it back.
    #[test]
    fn the_window_edits_the_book_through_its_door() {
        let mut app = TestApp::<ContactsShell>::new().unwrap();
        for invoke in ContactsShell::scenario(None) {
            app.invoke(invoke).unwrap();
        }
        let id = sample_book()[1].id;
        let favorite = |app: &TestApp<ContactsShell>| {
            app.state()
                .contacts
                .iter()
                .find(|contact| contact.id == id)
                .unwrap()
                .favorite
        };
        let before = favorite(&app);
        app.effect_on(
            &crate::actions::contact_resource_id(id),
            &ContactsEffect::Open { contact: id },
        )
        .unwrap();
        let favorite_control = app
            .harness()
            .a11y_node_named("Favorite")
            .expect("the shared Favorite control is present");
        app.harness()
            .perform_a11y_action(favorite_control, &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
        assert_ne!(favorite(&app), before, "the star reached the document");
        assert!(app.undo());
        assert_eq!(favorite(&app), before);
    }
}
