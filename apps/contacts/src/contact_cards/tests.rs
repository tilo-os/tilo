//! The address book's contacts through the parts the app keeps them in: the
//! verbs, a saved form's field edits, merging replicas, what the parts
//! publish, and the app.

use std::rc::Rc;

use ui::framework::testing::TestApp;
use ui::tilo_resource::{Effect as _, Invoke, Replica};

use crate::actions::{ContactsEffect, MergeField};
use crate::contact_cards::{Book, ContactCards, contact_cards};
use crate::model::{
    Address, Birthday, Contact, ContactId, GroupEntry, Labeled, MAX_CONTACT_FIELD_BYTES,
};
use crate::resource::{
    ContactsShell, FAVORITES_PLACE, duplicate_place, group_place, place_resource_id, sample_book,
};
use uuid::Uuid;

fn part_of(contacts: &[Contact]) -> Replica<ContactCards> {
    let mut part = Replica::<ContactCards>::new();
    part.apply(ContactCards::add_all(contacts.to_vec()))
        .unwrap();
    part
}

/// The parts the book's replica holds, oldest first.
fn parts(book: &Replica<ContactCards>) -> Vec<(u64, Rc<ContactCards>)> {
    vec![(0, Rc::new(book.state().unwrap()))]
}

fn contact(book: &Replica<ContactCards>, id: ContactId) -> Option<Contact> {
    book.state().unwrap().contact(id).cloned()
}

/// Apply `invoke` to the part it changes, read against the book and the
/// parts held where it is invoked, as a window carries it out.
fn apply(
    book: &mut Replica<ContactCards>,
    invoke: impl Into<Invoke<ContactCards>>,
) -> Result<u64, String> {
    let mut action = invoke.into().build(1_000)?;
    let held = parts(book);
    crate::contact_cards::prepare(std::slice::from_mut(&mut action), &held)?;
    let index = ui::framework::placed::<ContactCards>(&action, &held);
    book.apply_action(&action)
        .map_err(|error| error.to_string())?;
    Ok(index)
}

fn save(book: &mut Replica<ContactCards>, base: &Contact, edited: &Contact) -> Result<(), String> {
    // Taking a row out is confirmed, as the window asks the person to.
    let invoke = ContactCards::save(base, edited).ok_or("the form changed nothing")?;
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

fn name(text: &str) -> ui::tilo_resource::NonBlank {
    text.parse().unwrap()
}

/// The book an app's parts add up to.
fn held(app: &TestApp<ContactsShell>) -> Book {
    Book::of(&app.chain_parts::<ContactCards>())
}

tilo_test_env::scenario! {
    fn the_address_book_resource_adds_saves_merges_and_publishes_its_contacts;

    phase contacts_are_added_once_and_a_deleted_id_stays_deleted;
    {
        let mut part = Replica::<ContactCards>::new();
        part.apply(ContactCards::NEW_CONTACT.with(name("Dana Scott")))
            .unwrap();
        let state = part.state().unwrap();
        let dana = state.contacts.first().unwrap();
        assert_eq!((dana.first.as_str(), dana.last.as_str()), ("Dana", "Scott"));
        assert!(
            serde_json::from_value::<contact_cards::NewContact>(serde_json::json!({"name": "  "})).is_err(),
            "a new contact has a name, refused where the arguments are read"
        );

        let ada = person("Ada", "Lovelace");
        part.apply(ContactCards::add(&ada)).unwrap();
        assert!(
            apply(&mut part, ContactCards::add(&ada)).is_err(),
            "an id names one contact"
        );
        apply(&mut part, ContactCards::delete(ada.id)).unwrap();
        assert!(contact(&part, ada.id).is_none());
        assert!(part.state().unwrap().removed.contains(&ada.id));
        assert!(
            apply(&mut part, ContactCards::add(&ada)).is_err(),
            "a late copy of a deleted card does not bring it back"
        );
        assert!(
            apply(&mut part, ContactCards::delete(ada.id)).is_err(),
            "a deleted contact is not deleted twice"
        );
    }

    phase saved_forms_merge_field_by_field_and_row_by_row;
    {
        let mut ada = person("Ada", "Lovelace");
        let first = Labeled::new("mobile", "+1 555 0100");
        let second = Labeled::new("work", "+1 555 0100");
        ada.phones = vec![first.clone(), second].into();
        let mut a = part_of(std::slice::from_ref(&ada));
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
            ContactCards::save(&merged, &merged).is_none(),
            "a form that changed nothing saves nothing"
        );
    }

    phase a_save_over_a_removed_row_is_refused_whole;
    {
        let ada = person("Ada", "Lovelace");
        let mut book = part_of(std::slice::from_ref(&ada));
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

    phase a_deletion_wins_over_a_concurrent_save;
    {
        let ada = person("Ada", "Lovelace");
        let mut a = part_of(std::slice::from_ref(&ada));
        let mut b = a.fork();
        apply(&mut a, ContactCards::delete(ada.id)).unwrap();
        let mut noted = ada.clone();
        noted.notes = "late note".into();
        save(&mut b, &ada, &noted).unwrap();
        a.sync_with(&mut b).unwrap();
        assert!(contact(&a, ada.id).is_none());
        assert!(contact(&b, ada.id).is_none());
    }

    phase merging_unions_rows_and_takes_the_chosen_fields;
    {
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
        let mut book = part_of(&[survivor.clone(), source.clone()]);
        let mut peer = book.fork();
        assert!(crate::contact_cards::duplicate(
            &parts(&book),
            survivor.id,
            source.id
        ));

        assert!(
            apply(&mut book, ContactCards::merge(survivor.id, survivor.id, [])).is_err(),
            "a contact is not merged into itself"
        );
        apply(
            &mut book,
            ContactCards::merge(
                survivor.id,
                source.id,
                [MergeField::Organization, MergeField::Title],
            ),
        )
        .unwrap();
        let state = book.state().unwrap();
        assert!(state.contact(source.id).is_none() || state.removed.contains(&source.id));
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

    phase regression_merge_preserves_distinct_dial_strings;
    {
        let mut survivor = person("Ada", "Lovelace");
        survivor.id = Uuid::from_u128(1);
        survivor.phones = vec![
            Labeled::new("work", "123"),
            Labeled::new("work", "1-800-FLOWERS"),
            Labeled::new("home", "+1 (555) 0100"),
        ]
        .into();
        let mut source = person("Ada", "Lovelace");
        source.id = Uuid::from_u128(2);
        source.phones = vec![
            Labeled::new("work", "*123#"),
            Labeled::new("work", "1-800-PIZZA"),
            Labeled::new("home", "15550100"),
        ]
        .into();
        let mut book = part_of(&[survivor.clone(), source.clone()]);
        apply(
            &mut book,
            ContactCards::merge(survivor.id, source.id, []),
        )
        .unwrap();
        let merged = contact(&book, survivor.id).unwrap();
        assert_eq!(
            merged
                .phones
                .iter()
                .map(|row| row.value.as_str())
                .collect::<Vec<_>>(),
            [
                "123",
                "1-800-FLOWERS",
                "+1 (555) 0100",
                "*123#",
                "1-800-PIZZA"
            ],
            "only formatting-equivalent numbers are deduplicated"
        );
    }

    phase the_book_publishes_places_items_and_search;
    {
        let mut contacts = sample_book();
        let mut copy = contacts[0].clone();
        copy.id = Uuid::from_u128(u128::MAX);
        contacts.push(copy.clone());
        let mut late = person("Late", "");
        late.groups = vec![GroupEntry::new("Late group")].into();
        contacts.push(late);
        let book = part_of(&contacts);
        let held = parts(&book);
        let places = ui::tilo_resource::derived::published::<ContactCards>(
            crate::contact_cards::places(&held),
        )
        .to_resources();
        let item = ui::tilo_resource::publish::<ContactCards>(
            &ui::resource::chain_resource_id(&crate::contact_cards::chain()),
            "Contacts",
            &book.state().unwrap(),
        );
        let child = |id: &str| {
            places
                .iter()
                .chain(item.children.iter())
                .find(|child| child.id.as_str() == id)
                .unwrap_or_else(|| panic!("{id} is published"))
        };

        let favorites = child(&place_resource_id(FAVORITES_PLACE));
        assert!(
            favorites
                .actions
                .iter()
                .any(|action| action.id == "open-favorites")
        );
        let group = child(&place_resource_id(&group_place("LATE GROUP")));
        assert_eq!(group.label, "Group: Late group");
        let ada = child(&crate::contact_cards::contact_resource_id(contacts[0].id));
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
                .filter(|name| name != "merged")
                .collect::<Vec<_>>(),
            ["take_from_source"],
            "the pair is bound; only the fields to take are asked"
        );

        let entries = ContactCards::search(&book.state().unwrap());
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
            Some(crate::contact_cards::contact_resource_id(grace_id).as_str())
        );
        let entries = crate::contact_cards::search(&held);
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
}

#[test]
fn malformed_and_oversized_contacts_are_refused() {
    let mut book = Replica::<ContactCards>::new();
    let oversized = Contact {
        notes: "x".repeat(MAX_CONTACT_FIELD_BYTES + 1),
        ..person("Ada", "")
    };
    assert!(book.apply(ContactCards::add(&oversized)).is_err());
    let mut unnamed_row = person("Ada", "");
    unnamed_row.phones.push(Labeled {
        id: Uuid::nil(),
        ..Labeled::new("home", "+1 555 0110")
    });
    assert!(book.apply(ContactCards::add(&unnamed_row)).is_err());
    let impossible = Contact {
        birthday: Some(Birthday::Full {
            year: 2023,
            month: 2,
            day: 30,
        }),
        ..person("Ada", "")
    };
    assert!(book.apply(ContactCards::add(&impossible)).is_err());

    let ada = person("Ada", "");
    book.apply(ContactCards::add(&ada)).unwrap();
    let mut edited = ada.clone();
    edited.notes = "x".repeat(MAX_CONTACT_FIELD_BYTES + 1);
    assert!(save(&mut book, &ada, &edited).is_err());
    assert_eq!(contact(&book, ada.id).unwrap(), ada);
}

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
        app.chain_invoke(&ContactCards::add(contact)).unwrap();
    }
    app.call_confirmed_on(
        &place_resource_id(&duplicate_place(survivor.id, source.id)),
        "merge-contacts",
        serde_json::json!({ "take_from_source": ["Organization"] }),
    )
    .unwrap();
    let merged = held(&app).contacts;
    assert_eq!(merged.len(), 1, "the source is merged away");
    assert_eq!(merged[0].id, survivor.id);
    assert_eq!(merged[0].org, "Analytical Engines");
    assert!(app.undo(), "the merge is one undo step");
    assert_eq!(held(&app).contacts.len(), 2);
}

#[test]
fn stable_contacts_project_without_inferred_endpoints_and_deletions_disable_identity() {
    use tilo_contact_directory::ContactDirectory;
    let mut app = TestApp::<ContactsShell>::for_person("contacts-fixture-owner").unwrap();
    let directory = app.shared_person::<ContactDirectory>().unwrap();
    let mut first = person("Alex", "");
    first.phones.push(Labeled::new("mobile", "+12025550123"));
    first.emails.push(Labeled::new("work", "Alex@EXAMPLE.test"));
    let second = person("Alex", "");
    app.chain_invoke(&ContactCards::add(&first)).unwrap();
    app.chain_invoke(&ContactCards::add(&second)).unwrap();
    app.settle();
    let projected = directory.get();
    assert_eq!(projected.people.len(), 2);
    for contact in [&first, &second] {
        let identity = projected
            .people
            .get(&crate::contact_cards::key(contact.id))
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
        vec![crate::contact_cards::key(first.id)]
    );
    assert_eq!(
        projected.email_candidates("Alex@example.test"),
        vec![crate::contact_cards::key(first.id)]
    );
    assert!(projected.resolve("email", "Alex@example.test").is_err());
    assert!(projected.resolve("sms", "+12025550123").is_err());
    app.chain_invoke(&ContactCards::delete(first.id)).unwrap();
    app.settle();
    let projected = directory.get();
    assert!(projected.email_candidates("Alex@example.test").is_empty());
    assert!(
        projected
            .people
            .get(&crate::contact_cards::key(first.id))
            .unwrap()
            .removed
    );
    assert!(
        !projected
            .people
            .get(&crate::contact_cards::key(second.id))
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

#[test]
fn a_vcard_file_the_app_is_opened_with_is_imported() {
    let cards = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\nEND:VCARD\r\n\
                 BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Alan Turing\r\nN:Turing;Alan;;;\r\nEND:VCARD\r\n";
    let (file, _receipt) =
        ui::application::OpenFile::new("friends.vcf", "text/vcard", cards.as_bytes().to_vec())
            .unwrap();
    let mut app = TestApp::<ContactsShell>::handed(file).unwrap();
    app.harness().relayout();
    let mut names: Vec<String> = held(&app)
        .contacts
        .iter()
        .map(Contact::display_name)
        .collect();
    names.sort();
    assert_eq!(names, ["Ada Lovelace", "Alan Turing"]);
    assert!(app.undo());
    assert!(held(&app).contacts.is_empty(), "one import, one undo step");
}

#[test]
fn captures_stage_the_sample_book_and_a_new_window_is_empty() {
    let live = TestApp::<ContactsShell>::new().unwrap();
    assert!(held(&live).contacts.is_empty(), "nothing is fabricated");

    let empty = TestApp::<ContactsShell>::capture(Some("empty")).unwrap();
    assert!(held(&empty).contacts.is_empty());

    let mut edit = TestApp::<ContactsShell>::capture(Some("edit")).unwrap();
    assert_eq!(held(&edit).contacts.len(), sample_book().len());
    assert!(
        has_control(&mut edit, "First name"),
        "the edit form is open"
    );
    assert!(
        ui::window_toolbar::declared()
            .is_some_and(|toolbar| toolbar.actions.iter().any(|action| action.name == "Save")),
        "the window's bar offers Save"
    );

    let mut duplicates = TestApp::<ContactsShell>::capture(Some("duplicates")).unwrap();
    assert_eq!(held(&duplicates).contacts.len(), sample_book().len() + 1);
    let surface = duplicates.agent_surface().to_string();
    assert!(
        surface.contains("contacts.contacts/derived/duplicate:"),
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

#[test]
fn the_window_edits_the_book_through_its_api() {
    let mut app = TestApp::<ContactsShell>::new().unwrap();
    app.chain_invoke(&ContactCards::add_all(sample_book()))
        .unwrap();
    let id = sample_book()[1].id;
    let favorite = |app: &TestApp<ContactsShell>| held(app).contact(id).unwrap().favorite;
    let before = favorite(&app);
    app.settle();
    app.call(
        "open",
        serde_json::json!({ "anchor": crate::contact_cards::contact_resource_id(id) }),
    )
    .unwrap();
    assert!(
        app.call(
            "open",
            serde_json::json!({ "anchor": crate::contact_cards::contact_resource_id(Uuid::from_u128(4_242)) }),
        )
        .is_err(),
        "a contact that is not in the book is not opened"
    );
    app.settle();
    assert!(
        ui::window_toolbar::declared()
            .is_some_and(|toolbar| toolbar.actions.iter().any(|action| action.id == "favorite")),
        "the window's bar offers Favorite"
    );
    ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action {
        id: "favorite".into(),
    });
    app.settle();
    assert_ne!(favorite(&app), before, "the star reached the document");
    assert!(app.undo());
    assert_eq!(favorite(&app), before);
}

#[test]
fn history_says_what_each_change_did() {
    let mut app = TestApp::<ContactsShell>::new().unwrap();
    let mut ada = person("Ada", "Lovelace");
    ada.id = Uuid::from_u128(1);
    app.chain_invoke(&ContactCards::add(&ada)).unwrap();
    app.chain_invoke(&Invoke::edit(&ContactCards::favorite(ada.id, true)).unwrap())
        .unwrap();
    let mut edited = ada.clone();
    edited.favorite = true;
    let mut saved = edited.clone();
    saved.title = "Countess".into();
    saved
        .phones
        .push(Labeled::new("mobile", "+44 20 7946 0101"));
    app.chain_invoke(&ContactCards::save(&edited, &saved).unwrap())
        .unwrap();
    app.chain_invoke(&ContactCards::NEW_CONTACT.with(name("Grace Hopper")))
        .unwrap();
    app.chain_invoke(&ContactCards::delete(ada.id)).unwrap();

    let sentences = app.sentences(app.chain_history::<ContactCards>().iter());
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

#[test]
fn merging_from_the_review_lands_at_once_and_undo_takes_it_back() {
    let mut app = TestApp::<ContactsShell>::new().unwrap();
    let mut survivor = person("Ada", "Lovelace");
    survivor.id = Uuid::from_u128(1);
    let mut source = survivor.clone();
    source.id = Uuid::from_u128(2);
    source.org = "Analytical Engines".into();
    app.chain_invoke(&ContactCards::add_all(vec![
        survivor.clone(),
        source.clone(),
    ]))
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
    assert!(app.asking().is_none(), "an undoable merge asks nothing");
    let merged = held(&app).contacts;
    assert_eq!(merged.len(), 1, "the merge lands at once");
    assert_eq!(merged[0].org, "Analytical Engines");
    assert!(app.undo(), "the merge is one undo step");
    assert_eq!(held(&app).contacts.len(), 2, "Undo brings the copy back");
}

/// The book's own record answers a read with no window open: what it holds
/// itself, which is nothing, since the contacts are read from their parts.
#[test]
fn the_books_own_record_answers_a_read() {
    const { assert!(<crate::resource::Contacts as ui::tilo_resource::Resource>::READABLE) };
    let mut app = TestApp::<ContactsShell>::new().unwrap();
    let read = app.call("read", serde_json::json!({})).unwrap().unwrap();
    assert!(read.is_object(), "the book reads as what it holds: {read}");
}

#[test]
fn export_vcard_content_keeps_every_card_or_one_by_name_as_content() {
    let mut app = TestApp::<ContactsShell>::new().unwrap();
    let (ada, alan) = (person("Ada", "Lovelace"), person("Alan", "Turing"));
    app.chain_invoke(&ContactCards::add_all(vec![ada, alan.clone()]))
        .unwrap();
    let text = |app: &TestApp<ContactsShell>, answer: &serde_json::Value| {
        let hash = ui::tilo_actions::content_hash(answer["content"].as_str().unwrap()).unwrap();
        String::from_utf8(app.content().read(hash).unwrap()).unwrap()
    };
    let all = app
        .call("export-vcard-content", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(all["contacts"], 2);
    let cards = text(&app, &all);
    assert_eq!(cards.matches("BEGIN:VCARD").count(), 2);
    assert!(cards.contains("VERSION:4.0"));
    let one = app
        .call(
            "export-vcard-content",
            serde_json::json!({ "name": "ada lovelace" }),
        )
        .unwrap()
        .unwrap();
    assert_eq!(one["names"], serde_json::json!(["Ada Lovelace"]));
    let card = text(&app, &one);
    assert!(
        card.contains("Lovelace") && !card.contains("Turing"),
        "{card}"
    );
    let by_id = app
        .call(
            "export-vcard-content",
            serde_json::json!({ "contact": alan.id.to_string() }),
        )
        .unwrap()
        .unwrap();
    assert_eq!(by_id["names"], serde_json::json!(["Alan Turing"]));
    assert!(
        app.call(
            "export-vcard-content",
            serde_json::json!({ "name": "Grace" })
        )
        .is_err(),
        "a name no card carries is refused"
    );
}

#[test]
fn the_window_bars_sidebar_button_flips_the_sidebar() {
    ui::framework::testing::check_window_sidebar_button(|| {
        ui::framework::testing::TestApp::<ContactsShell>::capture(None)
    })
    .unwrap_or_else(|error| panic!("{error}"));
}

/// What one more change costs a book does not grow with the contacts it
/// already has: the part it writes holds at most a part's worth, so writing
/// into a book ten times the size costs the same bytes into a part the same
/// size.
#[test]
#[ignore = "writes ten thousand contacts, minutes in a debug build; run it with --ignored in a release build"]
fn a_change_costs_the_same_in_a_book_ten_times_the_size() {
    use ui::framework::PART_ITEMS as PART_CONTACTS;

    /// Fill the book up to `count` contacts, a part at a time: written
    /// straight into the parts, as a sync brings them.
    fn fill(parts: &mut Vec<Replica<ContactCards>>, count: usize) {
        let mut made: usize = parts
            .iter()
            .map(|part| part.state().unwrap().contacts.len())
            .sum();
        while made < count {
            let index = made / PART_CONTACTS;
            while parts.len() <= index {
                parts.push(Replica::<ContactCards>::new());
            }
            let end = count.min((index + 1) * PART_CONTACTS);
            let cards: Vec<Contact> = (made..end)
                .map(|at| Contact {
                    id: Uuid::from_u128(u128::try_from(at).unwrap() + 1),
                    first: format!("Person {at}"),
                    last: "of the address book".into(),
                    org: "A company's directory".into(),
                    emails: vec![Labeled::new("work", &format!("person{at}@example.test"))].into(),
                    notes: "Reach them about the project; they answer in the morning.".into(),
                    ..Contact::default()
                })
                .collect();
            parts[index]
                .apply(ContactCards::add_all(cards))
                .expect("a part takes its contacts");
            made = end;
        }
    }

    /// The bytes one favourite toggle on the first contact writes, the size
    /// of the part it is written to, and the time it took.
    fn toggle(parts: &mut [Replica<ContactCards>]) -> (usize, usize, std::time::Duration) {
        let first = parts[0].state().unwrap().contacts.first().unwrap().id;
        let favorite = !parts[0].state().unwrap().contact(first).unwrap().favorite;
        let began = std::time::Instant::now();
        parts[0]
            .apply(Invoke::edit(&ContactCards::favorite(first, favorite)).unwrap())
            .expect("the star applies");
        let took = began.elapsed();
        let part = &mut parts[0];
        let change = part
            .doc_mut()
            .get_last_local_change()
            .map(|change| change.raw_bytes().len())
            .unwrap();
        (change, part.save().len(), took)
    }

    let small = PART_CONTACTS * 2;
    let mut parts = Vec::new();
    fill(&mut parts, small);
    let (change_small, record_small, took_small) = toggle(&mut parts);
    fill(&mut parts, small * 10);
    let (change_large, record_large, took_large) = toggle(&mut parts);
    eprintln!(
        "a star at {small} contacts: {change_small} bytes into a {record_small}-byte part \
         in {took_small:?}; at {} contacts: {change_large} bytes into a {record_large}-byte part \
         in {took_large:?}",
        small * 10
    );
    assert_eq!(parts.len(), 20);
    assert!(
        change_large <= change_small + 16,
        "a change is the same size: {change_small} then {change_large} bytes"
    );
    assert!(
        record_large <= record_small + record_small / 10,
        "the resource a change is written to is no larger: {record_small} then {record_large} bytes"
    );
}
