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

use std::rc::Rc;

use ui::framework::{LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionParam, PROMPTED_PARAM, ParamType, Resource as PublishedResource};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{
    CONTACTS_ACTION_TYPE, ContactsAction, ContactsEffect, contact_resource_id,
    create_contact_action, open_contact_address, open_group_address, ordered_groups,
    publish_children, register, resolve_child, split_name,
};
use crate::app::ContactsApp;
use crate::doc::ContactsDoc;
use crate::model::{Birthday, Contact, Labeled, sample_contacts};

/// The address book's state: every contact, in document order.
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

    fn register(registry: &mut ui::tilo_actions::ActionRegistry) {
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

    fn view(cx: &mut ViewCx<Contacts>) -> Element {
        cx.surface(ContactsApp::new())
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
    }

    fn attach(&mut self, cx: SurfaceCx<Contacts>) {
        self.attach_dispatcher(cx.dispatcher());
        self.jobs = Some(cx.jobs());
        // A vCard file the app was opened with ("open with", a dropped
        // file) is imported into the address book.
        if let Some(file) = cx.launch().file() {
            self.open_cards(String::from_utf8_lossy(file.bytes()).into_owned());
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.import_conflict_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.import_conflict_rows()
            .into_iter()
            .find(|row| row.id() == id)
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
        assert!(
            has_control(&mut edit, "Save changes"),
            "the edit form is open"
        );

        let mut duplicates = TestApp::<ContactsShell>::capture(Some("duplicates")).unwrap();
        assert_eq!(duplicates.state().contacts.len(), sample_book().len() + 1);
        let surface = duplicates.agent_surface().to_string();
        assert!(
            surface.contains("contacts.duplicate:"),
            "the staged duplicate is published for review"
        );

        let mut groups = TestApp::<ContactsShell>::capture(Some("groups")).unwrap();
        let options = groups
            .with_surface::<crate::app::ContactsApp, _>(|app| app.group_filter_options())
            .unwrap();
        assert_eq!(
            options
                .iter()
                .filter(|option| *option == "Favorites")
                .count(),
            1,
            "the group menu lists Favorites once: {options:?}"
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
        let state = ContactsState { contacts };

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
        let mut app = TestApp::<ContactsShell>::capture(None).unwrap();
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
        for target in [
            format!("contacts.select:{id}"),
            format!("contacts.detail.favorite:{id}"),
        ] {
            app.harness()
                .perform_semantic_named("contacts", Some(&target), &ui::a11y::Action::Activate)
                .unwrap();
        }
        app.settle();
        assert_ne!(favorite(&app), before, "the star reached the document");
        assert!(app.undo());
        assert_eq!(favorite(&app), before);
    }
}
