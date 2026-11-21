//! The address book's own document, the places its contacts are published
//! under, and the app.
//!
//! The contacts are kept in numbered parts beside the book
//! ([`crate::contact_cards::ContactCards`]): a contact's fields change through the
//! field edits every path offers, and a saved form is exactly the edits
//! between the contact it opened and the contact it saves
//! ([`ContactCards::save`]), so a stale form never overwrites what someone
//! else changed meanwhile. Groups, the favorites and likely duplicates are
//! derived from every part and published by the parts' chain as places that
//! reveal them.

use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_resource;
use ui::widget::Element;
use uuid::Uuid;

use crate::app::ContactsApp;
use crate::contact_cards::{Book, ContactCards};
use crate::model::{Birthday, Contact, Labeled, sample_contacts};

pub use contacts_model::{Contacts, duplicate_place, group_place};

pub const FAVORITES_PLACE: &str = "favorites";

/// The address of the place `place` is published under: the chain's derived
/// children, beside the contacts.
pub fn place_resource_id(place: &str) -> String {
    format!(
        "{}/derived/{place}",
        <ContactCards as tilo_resource::Resource>::KIND
    )
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
    const ID: &'static str = ui::apps::CONTACTS;
    const TITLE: &'static str = "Contacts";
    const TITLE_KEY: &'static str = "contacts-app-title";
    const ICON: Option<&'static str> = Some("contacts");
    const OPENS: &'static [&'static str] = &["text/vcard", "text/x-vcard"];
    const ARGUMENTS: usize = 1;

    /// The directory of the person's identities Contacts keeps for them and
    /// every app of theirs reads.
    const PERSON_RESOURCES: &'static [ui::framework::PersonKind] =
        &[ui::framework::PersonKind::shared::<
            tilo_contact_directory::ContactDirectory,
        >()];

    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::account_editor::CATALOG)
    }

    fn import_resource(context: &ui::application::ProcessContext) -> Result<(), String> {
        crate::import_inbox::import(context)
    }

    /// The contacts, kept in parts beside the book.
    fn resources(resources: &mut ui::framework::Resources<'_>) {
        resources.chain::<ContactCards>();
    }

    /// What a capture stages of the book, provided before the window is
    /// built: the contacts a capture shows, made through the parts' own
    /// verbs as the window makes them.
    fn fixture(fixture: &mut ui::framework::Fixture<Contacts>) {
        fixture.provide(Scene::of(fixture.scenario()));
    }

    fn view(cx: &mut ViewCx<Contacts>) -> Element {
        if let Some(scene) = cx.fixture::<Scene>().map(|scene| *scene)
            && let Some(chain) = cx.chain::<ContactCards>()
        {
            scene.stage(&chain);
        }
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
}

/// The book a capture shows, staged into the parts when the window opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scene {
    /// The sample book.
    Sample,
    /// The sample book with a likely duplicate beside it.
    Duplicates,
    /// The sample book whose first two contacts share an identity and
    /// differ everywhere else.
    ReviewPair,
    /// A fresh install: no contacts.
    Empty,
}

impl Scene {
    pub(crate) fn of(scenario: Option<&str>) -> Self {
        match scenario {
            Some("empty") => Self::Empty,
            Some("duplicates") => Self::Duplicates,
            Some("duplicate-review") => Self::ReviewPair,
            _ => Self::Sample,
        }
    }

    /// The contacts the scene shows.
    pub(crate) fn contacts(self) -> Vec<Contact> {
        if self == Self::Empty {
            return Vec::new();
        }
        let mut book = sample_book();
        match self {
            Self::Duplicates => {
                if let Some(first) = book.first() {
                    let mut copy = first.clone();
                    copy.id = Uuid::from_u128(u128::from(u32::MAX));
                    copy.notes =
                        "A second import with the same identity; review before merging.".into();
                    book.push(copy);
                }
            }
            Self::ReviewPair => review_pair(&mut book),
            Self::Sample | Self::Empty => {}
        }
        book
    }

    /// Add the scene's contacts to `chain`, once.
    pub(crate) fn stage(self, chain: &ui::framework::Chain<ContactCards>) {
        if !chain.parts().is_empty() {
            return;
        }
        let book = self.contacts();
        if book.is_empty() {
            return;
        }
        if let Err(error) = chain.invoke(&ContactCards::add_all(book)) {
            tracing::warn!(%error, "a capture's contacts are not staged");
        }
    }
}

impl ContactsApp {
    /// Labels are a projection of this book's stable UUIDs. Service endpoints
    /// are independently confirmed in the shared directory the workspace keeps
    /// and never inferred from matching names, emails or telephone numbers.
    pub(crate) fn project_identities(&self, state: &Book) {
        use tilo_contact_directory::{ContactDirectory, Projected};
        let Some(directory) = &self.identity_directory else {
            return;
        };
        let current = directory.get();
        let people: Vec<Projected> = state
            .contacts
            .iter()
            .map(|contact| Projected {
                person: crate::contact_cards::key(contact.id),
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
                            || person.phones != projected.phones
                            || person.emails != projected.emails
                            || person.photo != projected.photo)
                })
            })
            .collect();
        let removed: Vec<_> = state
            .removed
            .iter()
            .map(|removed| crate::contact_cards::key(*removed))
            .filter(|id| current.people.get(id).is_none_or(|person| !person.removed))
            .collect();
        // One change for the whole book, however many people it touches.
        // The directory shares this window's store, which the pass reading the
        // book is in: the projection lands once it lets go, in the window's
        // next settle, as an edit asked for in a pass does.
        for chunk in people.chunks(tilo_contact_directory::MAX_PROJECTED) {
            directory.invoke_then(
                &ContactDirectory::PROJECT_CONTACTS.with((chunk.to_vec(), Vec::new())),
                |_| {},
            );
        }
        if !removed.is_empty() {
            directory.invoke_then(
                &ContactDirectory::PROJECT_CONTACTS.with((Vec::new(), removed)),
                |_| {},
            );
        }
    }
}

ui::reach_tests!(ContactsShell);

#[cfg(test)]
mod agent_disclosure_tests {
    use ui::framework::ResourceApp;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::ResourceSpec;

    fn read_first(kind: &str) -> ui::serde_json::Value {
        for scenario in <super::ContactsShell as ResourceApp>::scenarios() {
            let mut app = TestApp::<super::ContactsShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            app.settle();
            let resources = app.harness().resources();
            if let Some((resource, read)) = document_read(&resources, kind) {
                return app
                    .call_on(&resource, &read, ui::serde_json::json!({}))
                    .unwrap_or_else(|error| panic!("{kind} in {scenario}: {error}"))
                    .unwrap_or_else(|| panic!("the read of {kind} answers"));
            }
        }
        panic!("no scenario publishes a {kind} with a document read");
    }

    fn document_read(resources: &[ResourceSpec], kind: &str) -> Option<(String, String)> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|action| resource.kind == kind && action.contract.reads_document())
                .map(|read| (resource.id.as_str().to_owned(), read.id.clone()))
                .or_else(|| document_read(&resource.children, kind))
        })
    }

    #[test]
    fn a_possible_duplicate_is_read_through_its_document_read() {
        let answer = read_first("contacts.contacts.duplicate");
        assert!(answer["survivor"]["item"].is_string(), "{answer}");
        assert!(answer["shared"][0].is_string(), "{answer}");
    }
}
