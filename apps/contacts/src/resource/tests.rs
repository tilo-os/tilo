//! The address book through the door the host uses: its verbs, a saved
//! form's field edits, merging replicas, what it publishes, and the app.

use super::*;
use crate::model::{Address, GroupEntry, MAX_CONTACT_FIELD_BYTES};
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Effect as _, Replica};

fn book_of(contacts: &[Contact]) -> Replica<Contacts> {
    let mut book = Replica::<Contacts>::new();
    for contact in contacts {
        book.apply(Contacts::add(contact)).unwrap();
    }
    book
}

fn contact(book: &Replica<Contacts>, id: ContactId) -> Option<Contact> {
    book.state().unwrap().contact(id).cloned()
}

fn save(book: &mut Replica<Contacts>, base: &Contact, edited: &Contact) -> Result<(), String> {
    // Taking a row out is confirmed, as the host asks the person to.
    let invoke = Contacts::save(base, edited).ok_or("the form changed nothing")?;
    book.apply_confirmed(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn person(first: &str, last: &str) -> Contact {
    Contact {
        first: first.into(),
        last: last.into(),
        ..Contact::new()
    }
}

/// A typed name becomes a contact; a whole card is added under its own id,
/// once; a deleted contact's id never names a contact again.
#[test]
fn contacts_are_added_once_and_a_deleted_id_stays_deleted() {
    let mut book = Replica::<Contacts>::new();
    book.apply(Contacts::NEW_CONTACT.with("Dana Scott"))
        .unwrap();
    let state = book.state().unwrap();
    let dana = state.contacts.first().unwrap();
    assert_eq!((dana.first.as_str(), dana.last.as_str()), ("Dana", "Scott"));
    assert!(
        book.apply(Contacts::NEW_CONTACT.with("  ")).is_err(),
        "a new contact has a name"
    );

    let ada = person("Ada", "Lovelace");
    book.apply(Contacts::add(&ada)).unwrap();
    assert!(
        book.apply(Contacts::add(&ada)).is_err(),
        "an id names one contact"
    );
    book.apply_confirmed(Contacts::delete(ada.id)).unwrap();
    assert!(contact(&book, ada.id).is_none());
    assert!(book.state().unwrap().removed.contains(&ada.id));
    assert!(
        book.apply(Contacts::add(&ada)).is_err(),
        "a late copy of a deleted card does not bring it back"
    );
    assert!(
        book.apply_confirmed(Contacts::delete(ada.id)).is_err(),
        "a deleted contact is not deleted twice"
    );
}

/// A card that is too large, has a row without an id or a birthday no
/// calendar has is refused, whole and as an edit.
#[test]
fn malformed_and_oversized_contacts_are_refused() {
    let mut book = Replica::<Contacts>::new();
    let oversized = Contact {
        notes: "x".repeat(MAX_CONTACT_FIELD_BYTES + 1),
        ..person("Ada", "")
    };
    assert!(book.apply(Contacts::add(&oversized)).is_err());
    let mut unnamed_row = person("Ada", "");
    unnamed_row.phones.push(Labeled {
        id: Uuid::nil(),
        ..Labeled::new("home", "+1 555 0110")
    });
    assert!(book.apply(Contacts::add(&unnamed_row)).is_err());
    let impossible = Contact {
        birthday: Some(Birthday::Full {
            year: 2023,
            month: 2,
            day: 30,
        }),
        ..person("Ada", "")
    };
    assert!(book.apply(Contacts::add(&impossible)).is_err());

    let ada = person("Ada", "");
    book.apply(Contacts::add(&ada)).unwrap();
    let mut edited = ada.clone();
    edited.notes = "x".repeat(MAX_CONTACT_FIELD_BYTES + 1);
    assert!(save(&mut book, &ada, &edited).is_err());
    assert_eq!(contact(&book, ada.id).unwrap(), ada);
}

/// A saved form writes only what it changed: two people saving different
/// fields and rows of one contact both keep their edit, a row edited on
/// one side and removed on the other keeps its id, and the order the
/// saves arrive in does not matter.
#[test]
fn saved_forms_merge_field_by_field_and_row_by_row() {
    let mut ada = person("Ada", "Lovelace");
    let first = Labeled::new("mobile", "+1 555 0100");
    let second = Labeled::new("work", "+1 555 0100");
    ada.phones = vec![first.clone(), second].into();
    let mut a = book_of(std::slice::from_ref(&ada));
    let mut b = a.fork();

    let mut titled = ada.clone();
    titled.title = "Compiler pioneer".into();
    titled.phones[0].value = "+1 555 0199".into();
    save(&mut a, &ada, &titled).unwrap();
    let mut grouped = ada.clone();
    grouped.groups.push(GroupEntry::new("Research"));
    grouped.phones.remove(1);
    save(&mut b, &ada, &grouped).unwrap();

    a.sync_with(&mut b).unwrap();
    let merged = contact(&a, ada.id).unwrap();
    assert_eq!(merged, contact(&b, ada.id).unwrap(), "both replicas agree");
    assert_eq!(merged.title, "Compiler pioneer");
    assert!(merged.groups.iter().any(|group| group.value == "Research"));
    assert_eq!(merged.phones.len(), 1);
    assert_eq!(merged.phones[0].id, first.id);
    assert_eq!(merged.phones[0].value, "+1 555 0199");
    assert!(
        Contacts::save(&merged, &merged).is_none(),
        "a form that changed nothing saves nothing"
    );
}

/// A form saved over a row someone removed meanwhile is refused whole: no
/// field it changed is written.
#[test]
fn a_save_over_a_removed_row_is_refused_whole() {
    let ada = person("Ada", "Lovelace");
    let mut book = book_of(std::slice::from_ref(&ada));
    let mut stale = ada.clone();
    stale.addresses.push(Address {
        street: "Gone".into(),
        ..Address::default()
    });
    let mut edited = stale.clone();
    edited.first = "Augusta".into();
    edited.addresses[0].street = "12 St James's Square".into();
    assert!(save(&mut book, &stale, &edited).is_err());
    assert_eq!(contact(&book, ada.id).unwrap(), ada, "nothing is written");
}

/// Deleting a contact wins over a save of it that arrives later, in either
/// order.
#[test]
fn a_deletion_wins_over_a_concurrent_save() {
    let ada = person("Ada", "Lovelace");
    let mut a = book_of(std::slice::from_ref(&ada));
    let mut b = a.fork();
    a.apply_confirmed(Contacts::delete(ada.id)).unwrap();
    let mut noted = ada.clone();
    noted.notes = "late note".into();
    save(&mut b, &ada, &noted).unwrap();
    a.sync_with(&mut b).unwrap();
    assert!(contact(&a, ada.id).is_none());
    assert!(contact(&b, ada.id).is_none());
}

/// Merging keeps every row of both contacts, fills what the survivor lacks,
/// takes the fields named from the other, and deletes the other; a peer
/// that merges with the book agrees.
#[test]
fn merging_unions_rows_and_takes_the_chosen_fields() {
    let mut survivor = person("Ada", "Lovelace");
    survivor.id = Uuid::from_u128(1);
    survivor.emails = vec![Labeled::new("home", "ada@example.test")].into();
    survivor.title = "Mathematician".into();
    let mut source = person("Ada", "Lovelace");
    source.id = Uuid::from_u128(2);
    source.emails = vec![
        Labeled::new("home", "ADA@EXAMPLE.TEST"),
        Labeled::new("work", "unique@example.test"),
    ]
    .into();
    source.org = "Analytical Engines".into();
    source.title = "Countess".into();
    source.notes = "Kept".into();
    let mut book = book_of(&[survivor.clone(), source.clone()]);
    let mut peer = book.fork();
    assert!(book.state().unwrap().duplicate(survivor.id, source.id));

    assert!(
        book.apply_confirmed(Contacts::merge(survivor.id, survivor.id, []))
            .is_err(),
        "a contact is not merged into itself"
    );
    book.apply_confirmed(Contacts::merge(
        survivor.id,
        source.id,
        [MergeField::Organization, MergeField::Title],
    ))
    .unwrap();
    let state = book.state().unwrap();
    assert!(state.contact(source.id).is_none());
    assert!(state.removed.contains(&source.id));
    let merged = state.contact(survivor.id).unwrap();
    assert_eq!(merged.org, "Analytical Engines");
    assert_eq!(merged.title, "Countess", "a field named is taken");
    assert_eq!(merged.notes, "Kept", "an empty field fills");
    assert_eq!(
        merged
            .emails
            .iter()
            .map(|email| email.value.as_str())
            .collect::<Vec<_>>(),
        ["ada@example.test", "unique@example.test"],
        "the same address once, every other one kept"
    );

    peer.sync_with(&mut book).unwrap();
    assert_eq!(peer.state().unwrap(), book.state().unwrap());
}

/// The book publishes the favorites, every group and its likely duplicates
/// as places; each contact is an item carrying its delete and merge;
/// search finds contacts by what they hold and groups by name.
#[test]
fn the_book_publishes_places_items_and_search() {
    let mut contacts = sample_book();
    let mut copy = contacts[0].clone();
    copy.id = Uuid::from_u128(u128::MAX);
    contacts.push(copy.clone());
    let mut late = person("Late", "");
    late.groups = vec![GroupEntry::new("Late group")].into();
    contacts.push(late);
    let book = book_of(&contacts);
    let published = book.published().unwrap();
    let child = |id: &str| {
        published
            .children
            .iter()
            .find(|child| child.id.as_str() == id)
            .unwrap_or_else(|| panic!("{id} is published"))
    };

    assert!(
        published
            .actions
            .iter()
            .any(|action| action.id == "new-contact")
    );
    let favorites = child(&place_resource_id(FAVORITES_PLACE));
    assert!(
        favorites
            .actions
            .iter()
            .any(|action| action.id == "open-favorites")
    );
    let group = child(&place_resource_id(&group_place("LATE GROUP")));
    assert_eq!(group.label, "Group: Late group");
    let ada = child(&contact_resource_id(contacts[0].id));
    for id in ["delete-contact", "merge-contacts"] {
        assert!(ada.actions.iter().any(|action| action.id == id), "{id}");
    }
    let duplicate = child(&place_resource_id(&duplicate_place(
        contacts[0].id,
        copy.id,
    )));
    assert!(
        duplicate
            .actions
            .iter()
            .any(|action| action.id == "open-review")
    );
    let merge = duplicate
        .actions
        .iter()
        .find(|action| action.id == "merge-contacts")
        .expect("the duplicate offers its merge");
    assert_eq!(
        merge.contract.risk,
        ui::tilo_actions::RiskLevel::Destructive
    );
    assert_eq!(
        merge
            .params()
            .iter()
            .map(|param| param.name.clone())
            .collect::<Vec<_>>(),
        ["take_from_source"],
        "the pair is bound; only the fields to take are asked"
    );

    let state = book.state().unwrap();
    let entries = Contacts::search(&state);
    let grace = entries
        .iter()
        .find(|entry| entry.title == "Grace Hopper")
        .expect("search finds a contact");
    assert!(grace.text.contains("US Navy"), "{:?}", grace.text);
    let grace_id = contacts
        .iter()
        .find(|contact| contact.display_name() == "Grace Hopper")
        .unwrap()
        .id;
    assert_eq!(
        grace.anchor.as_deref(),
        Some(contact_resource_id(grace_id).as_str())
    );
    let work = entries
        .iter()
        .find(|entry| entry.title == "Group: Work")
        .expect("groups are found too");
    let place = place_resource_id(&group_place("Work"));
    assert_eq!(work.anchor.as_deref(), Some(place.as_str()));
    let opens = work.action.as_ref().expect("a group hit opens its place");
    assert_eq!(opens.resource, place);
    assert_eq!(
        opens.action,
        ContactsEffect::OpenGroup {
            name: "Work".into()
        }
        .id()
    );
}

/// A merge through the duplicate's place merges, and one undo restores
/// both contacts.
#[test]
fn a_published_merge_merges_and_one_undo_restores_both() {
    let mut app = TestApp::<ContactsShell>::new().unwrap();
    let mut survivor = person("Ada", "Lovelace");
    survivor.id = Uuid::from_u128(1);
    survivor.emails = vec![Labeled::new("home", "ada@example.test")].into();
    let mut source = survivor.clone();
    source.id = Uuid::from_u128(2);
    source.org = "Analytical Engines".into();
    for contact in [&survivor, &source] {
        app.invoke(Contacts::add(contact)).unwrap();
    }
    app.call_confirmed_on(
        &place_resource_id(&duplicate_place(survivor.id, source.id)),
        "merge-contacts",
        serde_json::json!({ "take_from_source": ["Organization"] }),
    )
    .unwrap();
    let merged = app.state().contacts;
    assert_eq!(merged.len(), 1, "the source is merged away");
    assert_eq!(merged[0].id, survivor.id);
    assert_eq!(merged[0].org, "Analytical Engines");
    assert!(app.undo(), "the merge is one undo step");
    assert_eq!(app.state().contacts.len(), 2);
}

/// Contacts project into the person's contact directory under their ids,
/// never with an inferred endpoint, and a deleted contact is removed there.
#[test]
fn stable_contacts_project_without_inferred_endpoints_and_deletions_disable_identity() {
    use tilo_contact_directory::ContactDirectory;
    let mut app = TestApp::<ContactsShell>::for_person("contacts-fixture-owner").unwrap();
    let directory = app.shared_person::<ContactDirectory>().unwrap();
    let mut first = person("Alex", "");
    first.phones.push(Labeled::new("mobile", "+12025550123"));
    first.emails.push(Labeled::new("work", "Alex@EXAMPLE.test"));
    let second = person("Alex", "");
    app.invoke(Contacts::add(&first)).unwrap();
    app.invoke(Contacts::add(&second)).unwrap();
    app.settle();
    let projected = directory.get();
    assert_eq!(projected.people.len(), 2);
    for contact in [&first, &second] {
        let identity = projected.people.get(&Contacts::key(contact.id)).unwrap();
        assert_eq!(identity.label, "Alex");
        assert!(
            identity.endpoints.is_empty(),
            "telephone fields require confirmation"
        );
        assert!(!identity.removed);
    }
    assert_eq!(
        projected.phone_candidates("+12025550123"),
        vec![Contacts::key(first.id)]
    );
    assert_eq!(
        projected.email_candidates("Alex@example.test"),
        vec![Contacts::key(first.id)]
    );
    assert!(projected.resolve("email", "Alex@example.test").is_err());
    assert!(projected.resolve("sms", "+12025550123").is_err());
    app.invoke_confirmed(Contacts::delete(first.id)).unwrap();
    app.settle();
    let projected = directory.get();
    assert!(projected.email_candidates("Alex@example.test").is_empty());
    assert!(
        projected
            .people
            .get(&Contacts::key(first.id))
            .unwrap()
            .removed
    );
    assert!(
        !projected
            .people
            .get(&Contacts::key(second.id))
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
        .map(Contact::display_name)
        .collect();
    names.sort();
    assert_eq!(names, ["Ada Lovelace", "Alan Turing"]);
    assert!(app.undo());
    assert!(app.state().contacts.is_empty(), "one import, one undo step");
}

/// A new window starts on an empty book; captures stage the sample book
/// through verbs and their view state through `stage`.
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
        surface.contains("contacts.book/derived/duplicate:"),
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

/// The window's own controls edit the book through its door: opening a
/// contact as an agent does shows it, its star toggled in the detail pane
/// is the book's field edit, and one undo takes it back.
#[test]
fn the_window_edits_the_book_through_its_door() {
    let mut app = TestApp::<ContactsShell>::new().unwrap();
    for invoke in ContactsShell::scenario(None) {
        app.invoke(invoke).unwrap();
    }
    let id = sample_book()[1].id;
    let favorite = |app: &TestApp<ContactsShell>| app.state().contact(id).unwrap().favorite;
    let before = favorite(&app);
    app.call(
        "open",
        serde_json::json!({ "anchor": contact_resource_id(id) }),
    )
    .unwrap();
    assert!(
        app.call(
            "open",
            serde_json::json!({ "anchor": contact_resource_id(Uuid::from_u128(4_242)) }),
        )
        .is_err(),
        "a contact that is not in the book is not opened"
    );
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

/// History says what each change to the book did, in the person's words.
#[test]
fn history_says_what_each_change_did() {
    let mut app = TestApp::<ContactsShell>::new().unwrap();
    let mut ada = person("Ada", "Lovelace");
    ada.id = Uuid::from_u128(1);
    app.invoke(Contacts::add(&ada)).unwrap();
    app.invoke(Invoke::edit(&Contacts::favorite(ada.id, true)).unwrap())
        .unwrap();
    let mut edited = ada.clone();
    edited.favorite = true;
    let mut saved = edited.clone();
    saved.title = "Countess".into();
    saved
        .phones
        .push(Labeled::new("mobile", "+44 20 7946 0101"));
    app.invoke(Contacts::save(&edited, &saved).unwrap())
        .unwrap();
    app.invoke(Contacts::NEW_CONTACT.with("Grace Hopper"))
        .unwrap();
    app.invoke_confirmed(Contacts::delete(ada.id)).unwrap();

    let source = include_str!("../../locales/en-US.ftl");
    let catalogue = tilo_history::Catalogue::new("en-US", &[source]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .map(|entry| reader.sentence(&entry))
        .collect();
    assert_eq!(
        sentences,
        [
            "Added to the address book",
            "Made Ada a favorite",
            "2 changes to Ada",
            "Added Grace Hopper",
            "Deleted Ada",
        ],
        "{sentences:#?}"
    );
}

/// Merging from the review asks the host to confirm while the review stays
/// up; confirmed, the merge lands and the review closes.
#[test]
fn merging_from_the_review_asks_the_host_and_lands() {
    let mut app = TestApp::<ContactsShell>::new().unwrap();
    let mut survivor = person("Ada", "Lovelace");
    survivor.id = Uuid::from_u128(1);
    let mut source = survivor.clone();
    source.id = Uuid::from_u128(2);
    source.org = "Analytical Engines".into();
    app.invoke(Contacts::add_all(vec![survivor.clone(), source.clone()]))
        .unwrap();
    app.effect_on(
        &place_resource_id(&duplicate_place(survivor.id, source.id)),
        &ContactsEffect::OpenReview {
            survivor: survivor.id,
            source: source.id,
        },
    )
    .unwrap();
    app.settle();
    let activate = |app: &mut TestApp<ContactsShell>, name: &str, action| {
        let node = app
            .harness()
            .a11y_node_named(name)
            .unwrap_or_else(|| panic!("{name} is shown"));
        app.harness().perform_a11y_action(node, &action).unwrap();
        app.settle();
    };
    activate(&mut app, "Merge contacts", ui::a11y::Action::Activate);
    assert_eq!(app.state().contacts.len(), 2, "nothing merges unconfirmed");
    activate(
        &mut app,
        "Confirmation",
        ui::a11y::Action::SetValue("CONFIRM".into()),
    );
    activate(&mut app, "Confirm", ui::a11y::Action::Activate);
    let merged = app.state().contacts;
    assert_eq!(merged.len(), 1, "the confirmed merge lands");
    assert_eq!(merged[0].org, "Analytical Engines");
}
