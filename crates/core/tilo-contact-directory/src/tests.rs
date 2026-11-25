//! The contact directory as Contacts, Mail and the messaging bridges use it: hints projected from
//! the address book, endpoints and accounts a person confirmed, and the routes messages take.

use super::{ContactDirectory, Projected};
use crate::routing::{SenderAccount, routes};
use tilo_resource::AppNamed;
use tilo_resource::{Id, Replica};

fn first() -> Id {
    Id::from_raw("00000000-0000-4000-8000-000000000001")
}
fn second() -> Id {
    Id::from_raw("00000000-0000-4000-8000-000000000002")
}
fn third() -> Id {
    Id::from_raw("00000000-0000-4000-8000-000000000003")
}

/// Two people of the same name.
fn fixture() -> Replica<ContactDirectory, AppNamed> {
    let mut record = Replica::<ContactDirectory>::new().in_app("so.tilo.model", "Model");
    record
        .apply(ContactDirectory::PROJECT_CONTACT.with((first(), "Alex".into())))
        .expect("projects");
    record
        .apply(ContactDirectory::PROJECT_CONTACT.with((second(), "Alex".into())))
        .expect("projects");
    record
}

/// The person confirms `recipient` on `service` as theirs, under the association `id`.
fn endpoint(
    record: &mut Replica<ContactDirectory, AppNamed>,
    person: Id,
    id: &str,
    service: &str,
    recipient: &str,
) -> Result<(), String> {
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
            person,
            Id::from_raw(id),
            service.into(),
            recipient.into(),
        )))
        .map(drop)
        .map_err(|refused| refused.to_string())
}

/// The person confirms the account `subject` at `revision` in `scope` of `service` as theirs.
fn account(
    record: &mut Replica<ContactDirectory, AppNamed>,
    person: Id,
    id: &str,
    (service, scope, subject): (&str, &str, &str),
    revision: &str,
) -> Result<(), String> {
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ACCOUNT.with((
            person,
            Id::from_raw(id),
            service.into(),
            scope.into(),
            subject.into(),
            revision.into(),
        )))
        .map(drop)
        .map_err(|refused| refused.to_string())
}

fn forget(record: &mut Replica<ContactDirectory, AppNamed>, person: Id, id: &str) {
    record
        .apply_confirmed(ContactDirectory::FORGET_ENDPOINT.with((person, Id::from_raw(id))))
        .expect("revocation succeeds");
}

const ALICE: (&str, &str, &str) = ("signal", "signal-main", "aci:alice");

fn sender(id: &str) -> SenderAccount {
    SenderAccount {
        id: Id::from_raw(id),
        service: "signal".into(),
        scope: "signal-main".into(),
        enabled: true,
    }
}

/// Mailboxes and phone numbers from the address book suggest people and link none of them: one
/// hint per person and mailbox, exact keys only, every person sharing one offered, and a hint of
/// a renamed or removed person is stale.
#[test]
fn address_book_hints_suggest_people_and_link_nobody() {
    let mut record = fixture();
    for person in [first(), second()] {
        record
            .apply(ContactDirectory::PROJECT_EMAILS.with((
                person.clone(),
                vec![
                    "Alice@EXAMPLE.test".into(),
                    "Alice@example.test".into(),
                    "bad\n@example.test".into(),
                ],
            )))
            .expect("hints project");
        record
            .apply(
                ContactDirectory::PROJECT_PHONES.with((person, vec!["+1 (202) 555-0123".into()])),
            )
            .expect("projection succeeds");
    }
    let state = record.state().expect("state loads");
    let hint = |person: Id| super::EmailHint {
        person: person.as_str().into(),
        label: "Alex".into(),
        email: "Alice@example.test".into(),
    };
    assert_eq!(
        state.email_hints(),
        vec![hint(first()), hint(second())],
        "one hint per person and mailbox, invalid ones dropped, people kept apart"
    );
    assert!(state.holds(&hint(first())));
    assert_eq!(
        state.email_candidates("Alice@example.test"),
        vec![first(), second()]
    );
    for not_the_key in [
        "alice@example.test",
        "Alice+work@example.test",
        "Alice <Alice@example.test>",
    ] {
        assert!(
            state.email_candidates(not_the_key).is_empty(),
            "{not_the_key}"
        );
    }
    assert_eq!(
        state.phone_candidates("+12025550123"),
        vec![first(), second()]
    );
    assert!(state.phone_candidates("2025550123").is_empty());
    assert!(
        state
            .people
            .iter()
            .all(|(_, person)| person.endpoints.is_empty())
    );
    assert!(state.resolve("email", "Alice@example.test").is_err());
    assert!(state.resolve("sms", "+12025550123").is_err());
    assert!(
        state
            .resolve_account("sms", "carrier", "+12025550123", "assignment-1")
            .is_err()
    );

    record
        .apply(ContactDirectory::PROJECT_CONTACT.with((first(), "Sam".into())))
        .expect("renamed");
    let state = record.state().expect("state loads");
    assert!(!state.holds(&hint(first())), "a renamed contact is stale");
    record
        .apply(ContactDirectory::REMOVE_CONTACT.with(second()))
        .expect("removed");
    assert!(
        record
            .apply(
                ContactDirectory::PROJECT_EMAILS
                    .with((second(), vec!["replacement@example.test".into()]))
            )
            .is_err(),
        "a removed person takes no hints"
    );
    let state = record.state().expect("state loads");
    assert!(!state.holds(&hint(second())), "a removed contact is stale");
    assert_eq!(state.email_hints().len(), 1);
    assert_eq!(state.email_candidates("Alice@example.test"), vec![first()]);
}

#[test]
fn email_hint_keys_reject_header_injection_and_do_not_guess_unsupported_mailboxes() {
    use super::canonical_email_hint;
    assert_eq!(
        canonical_email_hint(" Alice+work@EXAMPLE.TEST "),
        Some("Alice+work@example.test".into())
    );
    for value in [
        "a..b@example.test",
        ".a@example.test",
        "a@example..test",
        "a@-example.test",
        "a@example.test,b@example.test",
        "a@example.test\r\nBcc: b@example.test",
        "\"a b\"@example.test",
        "ál@example.test",
    ] {
        assert!(canonical_email_hint(value).is_none(), "{value:?}");
    }
}

/// A confirmed endpoint routes to its person on its service alone; a second person claiming it
/// is refused; a revoked or deleted person's association id stays reserved, and a removed person
/// neither comes back through a late projection nor confirms anything new.
#[test]
fn confirmed_endpoints_route_to_one_person_and_their_ids_stay_reserved() {
    let mut record = fixture();
    endpoint(&mut record, first(), "one", "signal", "+12025550123").expect("confirms");
    endpoint(&mut record, second(), "two", "sms", "+12025550123").expect("confirms");
    endpoint(&mut record, first(), "taken", "signal", "recipient").expect("confirms");
    assert!(
        endpoint(&mut record, second(), "theirs", "signal", "recipient").is_err(),
        "a recipient confirmed as one person's is not another's"
    );
    let state = record.state().expect("state loads");
    assert_eq!(state.resolve("signal", "+12025550123"), Ok(first()));
    assert_eq!(state.resolve("sms", "+12025550123"), Ok(second()));
    assert_eq!(state.resolve("signal", "recipient"), Ok(first()));
    assert!(state.resolve("signal", "Alex").is_err());
    assert_eq!(state.people.len(), 2);

    forget(&mut record, second(), "two");
    for person in [second(), first()] {
        assert!(
            endpoint(&mut record, person, "two", "sms", "+12025550123").is_err(),
            "a revoked association is neither reused nor retargeted"
        );
    }
    let state = record.state().expect("state loads");
    let historical = state
        .people
        .get(&second())
        .expect("person remains")
        .endpoints
        .get(&Id::from_raw("two"))
        .expect("association remains");
    assert!(historical.removed);
    assert_eq!(historical.recipient, "+12025550123");
    assert!(state.resolve("sms", "+12025550123").is_err());

    record
        .apply(ContactDirectory::REMOVE_CONTACT.with(first()))
        .expect("deletion succeeds");
    record
        .apply(ContactDirectory::PROJECT_CONTACT.with((first(), "Alex new name".into())))
        .expect("a late projection is taken");
    assert!(
        record
            .state()
            .expect("state loads")
            .resolve("signal", "recipient")
            .is_err()
    );
    assert!(endpoint(&mut record, first(), "fresh", "signal", "fresh-recipient").is_err());
    assert!(
        endpoint(&mut record, second(), "one", "signal", "+12025550123").is_err(),
        "a deleted person's association ids stay reserved"
    );
}

/// An account is confirmed at one identity revision in one provider scope and routes only so,
/// from every sender account that can reach it.
#[test]
fn confirmed_accounts_route_by_exact_identity_and_scope() {
    let mut record = fixture();
    account(&mut record, first(), "signal-key", ALICE, "key-1").expect("confirms");
    account(
        &mut record,
        first(),
        "whatsapp",
        ("whatsapp", "business-a", "US.alice"),
        "key-1",
    )
    .expect("confirms");
    account(
        &mut record,
        first(),
        "sms",
        ("sms", "sms-carrier", "aci:alice"),
        "key-1",
    )
    .expect("confirms");
    let state = record.state().expect("state loads");
    for (service, scope, subject) in [
        ALICE,
        ("whatsapp", "business-a", "US.alice"),
        ("sms", "sms-carrier", "aci:alice"),
    ] {
        assert_eq!(
            state.resolve_account(service, scope, subject, "key-1"),
            Ok(first()),
            "{service}: one person on several services, none combined"
        );
    }
    assert!(
        state
            .resolve_account("signal", "signal-main", "aci:alice", "key-2")
            .is_err()
    );
    assert!(
        state
            .resolve_account("signal", "signal-other", "aci:alice", "key-1")
            .is_err()
    );
    assert!(
        state
            .resolve_account("whatsapp", "business-b", "US.alice", "key-1")
            .is_err()
    );
    assert!(
        state.resolve("signal", "aci:alice").is_err(),
        "compatibility resolver must not bypass identity revision"
    );

    let mut senders = vec![sender("personal"), sender("work")];
    let candidates = routes(&state, &first(), "signal", &senders);
    assert_eq!(candidates.len(), 2);
    let selected = candidates.first().expect("two routes");
    assert_eq!(selected.person, first());
    assert_eq!(selected.association.as_str(), "signal-key");
    assert_eq!(selected.revision, "key-1");
    for sender in &mut senders {
        sender.enabled = false;
    }
    assert!(routes(&state, &first(), "signal", &senders).is_empty());
    assert!(routes(&state, &first(), "whatsapp", &senders).is_empty());
}

/// A new identity for a confirmed account needs revocation or a reviewed replacement: the
/// replacement keeps who the old one was and is all or nothing, and after revocation the account
/// may be another person's.
#[test]
fn a_confirmed_account_takes_a_new_identity_only_by_review_or_revocation() {
    let mut record = fixture();
    account(&mut record, first(), "signal-key", ALICE, "key-1").expect("confirms");
    assert!(
        account(&mut record, first(), "new-key", ALICE, "key-2").is_err(),
        "identity changes require revocation"
    );
    let replace = |revision: &str| {
        ContactDirectory::REPLACE_ACCOUNT.with((
            first(),
            Id::from_raw("signal-key"),
            Id::from_raw("new-reference"),
            "signal".into(),
            "signal-main".into(),
            "aci:alice".into(),
            revision.into(),
        ))
    };
    let before = record.state().expect("state loads");
    assert!(record.apply_confirmed(replace("")).is_err());
    assert_eq!(
        record.state().expect("state loads"),
        before,
        "refused replacement must not revoke the old route"
    );
    record
        .apply_confirmed(replace("key-2"))
        .expect("reviewed replacement succeeds");
    let state = record.state().expect("state loads");
    assert_eq!(
        state.resolve_account("signal", "signal-main", "aci:alice", "key-2"),
        Ok(first())
    );
    assert!(
        state
            .resolve_account("signal", "signal-main", "aci:alice", "key-1")
            .is_err()
    );
    let historical = state
        .people
        .get(&first())
        .expect("person remains")
        .endpoints
        .get(&Id::from_raw("signal-key"))
        .expect("old association remains");
    assert!(historical.removed);
    assert_eq!(historical.revision, "key-1");

    forget(&mut record, first(), "new-reference");
    account(&mut record, second(), "reassigned", ALICE, "key-3")
        .expect("reassignment succeeds after review");
    let state = record.state().expect("state loads");
    assert_eq!(
        state.resolve_account("signal", "signal-main", "aci:alice", "key-3"),
        Ok(second())
    );
    assert!(
        state
            .resolve_account("signal", "signal-main", "aci:alice", "key-2")
            .is_err()
    );
}

/// Two devices confirm the same recipient, and the same account, for different people at once:
/// both converge on one state that routes the conflicting ones to nobody.
#[test]
fn conflicting_confirmations_on_two_devices_converge_and_route_to_nobody() {
    let mut left = fixture();
    let mut right = left.fork();
    endpoint(&mut left, first(), "one", "signal", "recipient").expect("confirms");
    endpoint(&mut right, second(), "two", "signal", "recipient").expect("confirms");
    account(&mut left, first(), "first-link", ALICE, "key-1").expect("confirms");
    account(&mut right, second(), "second-link", ALICE, "key-2").expect("confirms");
    left.sync_with(&mut right).expect("replicas sync");
    let state = left.state().expect("state loads");
    assert_eq!(state, right.state().expect("state loads"));
    assert!(state.resolve("signal", "recipient").is_err());
    for revision in ["key-1", "key-2"] {
        assert!(
            state
                .resolve_account("signal", "signal-main", "aci:alice", revision)
                .is_err()
        );
    }
    let senders = [sender("own")];
    assert!(routes(&state, &first(), "signal", &senders).is_empty());
    assert!(routes(&state, &second(), "signal", &senders).is_empty());
}

#[test]
fn legacy_person_records_load_without_email_hints_or_identity_changes() {
    use tilo_resource::{Doc, Fields};
    // A person from before mail hints, their phone hints one JSON string.
    let mut doc = Doc::new();
    tilo_resource::evolution::fixture(
        &mut doc,
        &tilo_resource::__private::ROOT,
        &tilo_resource::__private::serde_json::json!({
            "label": "Alex",
            "removed": false,
            "phones": r#"["+12025550123"]"#,
            "endpoints": {},
        }),
    )
    .expect("legacy record writes");
    super::upgrade_person(&mut doc, &tilo_resource::__private::ROOT).expect("migrates");
    let person = super::Person::read_fields(&doc, &tilo_resource::__private::ROOT)
        .expect("the migrated record reads");
    assert_eq!(person.label, "Alex");
    assert_eq!(person.phones, ["+12025550123"]);
    assert!(person.emails.is_empty());
    assert!(person.endpoints.is_empty());
}

/// One projection of many people is what projecting each of them alone
/// is: labels and hints land, a deleted person stays deleted, and the
/// removed are tombstoned.
#[test]
fn one_projection_of_many_people_is_each_projected_alone() {
    let mut batched = fixture();
    let mut single = fixture();
    for record in [&mut batched, &mut single] {
        record
            .apply(ContactDirectory::REMOVE_CONTACT.with(second()))
            .expect("removing succeeds");
    }
    let people = [first(), second(), third()].map(|person| Projected {
        label: format!("Person {person}"),
        phones: vec!["+12025550123".into()],
        emails: vec![format!("{person}@example.test")],
        photo: String::new(),
        photo_size: 0,
        person,
    });
    batched
        .apply(ContactDirectory::PROJECT_CONTACTS.with((people.to_vec(), vec![first()])))
        .expect("projecting succeeds");
    for projected in &people {
        let person = projected.person.clone();
        single
            .apply(
                ContactDirectory::PROJECT_CONTACT.with((person.clone(), projected.label.clone())),
            )
            .expect("projecting succeeds");
        if person != second() {
            single
                .apply(
                    ContactDirectory::PROJECT_PHONES
                        .with((person.clone(), projected.phones.clone())),
                )
                .expect("projecting succeeds");
            single
                .apply(ContactDirectory::PROJECT_EMAILS.with((person, projected.emails.clone())))
                .expect("projecting succeeds");
        }
    }
    single
        .apply(ContactDirectory::REMOVE_CONTACT.with(first()))
        .expect("removing succeeds");
    let batched = batched.state().expect("readable");
    assert_eq!(batched, single.state().expect("readable"));
    assert!(batched.people.get(&first()).expect("projected").removed);
    assert!(batched.people.get(&second()).expect("projected").removed);
    assert_eq!(
        batched.phone_candidates("+12025550123"),
        vec![third()],
        "only a person still in the book is a candidate"
    );

    let mut record = fixture();
    let invalid = Projected {
        person: Id::from_raw("not-a-uuid"),
        ..Projected::default()
    };
    assert!(
        record
            .apply(ContactDirectory::PROJECT_CONTACTS.with((vec![invalid], Vec::new())))
            .is_err(),
        "a person is named by a Contacts UUID"
    );
}

/// Contacts' pictures reach the directory, and a mailbox shows the one
/// person it belongs to; a shared mailbox, or a removed person, shows none.
#[test]
fn a_projected_picture_shows_for_the_one_person_with_that_mailbox() {
    use super::MAX_PHOTO_BYTES;
    let mut record = fixture();
    let person = |id: Id, email: &str, photo: &str, size: u64| Projected {
        label: format!("Person {id}"),
        phones: Vec::new(),
        emails: vec![email.into()],
        photo: photo.into(),
        photo_size: size,
        person: id,
    };
    record
        .apply(ContactDirectory::PROJECT_CONTACTS.with((
            vec![
                person(first(), "ada@example.test", "00c0ffee", 3),
                person(second(), "team@example.test", "00beef", 4),
            ],
            Vec::new(),
        )))
        .expect("projecting succeeds");
    let state = record.state().expect("readable");
    assert_eq!(
        state.photo_for_email("ada@Example.TEST"),
        Some(("00c0ffee", 3))
    );
    assert_eq!(state.photo_for_email("nobody@example.test"), None);

    record
        .apply(ContactDirectory::PROJECT_CONTACTS.with((
            vec![person(third(), "team@example.test", "00cafe", 5)],
            Vec::new(),
        )))
        .expect("projecting succeeds");
    let state = record.state().expect("readable");
    assert_eq!(state.photo_for_email("team@example.test"), None);

    record
        .apply(ContactDirectory::REMOVE_CONTACT.with(first()))
        .expect("removing succeeds");
    assert_eq!(
        record
            .state()
            .expect("readable")
            .photo_for_email("ada@example.test"),
        None
    );

    let oversized = person(second(), "team@example.test", "00dead", MAX_PHOTO_BYTES + 1);
    assert!(
        record
            .apply(ContactDirectory::PROJECT_CONTACTS.with((vec![oversized], Vec::new())))
            .is_err()
    );
}

/// The surface this crate ships for `tilolang-tilo` to publish as Contacts'
/// `directory` root: the type and the verbs, written from the model itself, so a
/// field or a verb that changes here changes there. `UPDATE_SNAPSHOTS=1` writes it.
#[test]
fn the_directory_surface_file_is_what_this_crate_publishes() {
    let surface = tilo_resource::surface::surface_of::<ContactDirectory>();
    let text = tilo_surface_compile::canonical(&serde_json::json!({ "resources": [surface] }));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("surface.json");
    let committed = std::fs::read_to_string(&path).ok();
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
        std::fs::write(&path, &text).expect("the surface file is written");
        return;
    }
    assert_eq!(
        committed.as_deref(),
        Some(text.as_str()),
        "crates/core/tilo-contact-directory/surface.json is not what this crate publishes; \
         regenerate it with UPDATE_SNAPSHOTS=1"
    );
}

/// A view of another language reads the directory as a list of its people,
/// each a value of the surface's `Person`: the collection's entry ids are the
/// record's, and a reading names no item by id.
#[test]
fn a_reading_of_the_directory_is_a_list_of_its_people() {
    let mut record = fixture();
    record
        .apply(ContactDirectory::CONFIRM_ENDPOINT.with((
            first(),
            Id::from_raw("00000000-0000-4000-8000-000000000009"),
            "signal".into(),
            "+12025550123".into(),
        )))
        .expect("the endpoint confirms");
    let state = record.state().expect("readable");
    let reading = tilo_resource::read::reading::<ContactDirectory>(&state);
    let people = reading["people"].as_array().expect("a list of people");
    assert_eq!(people.len(), 2);
    let [ada, _] = people.as_slice() else {
        panic!("two people: {reading}");
    };
    assert_eq!(ada["label"], "Alex");
    assert_eq!(ada["removed"], false);
    assert_eq!(ada["endpoints"][0]["service"], "signal");
    assert_eq!(ada["endpoints"][0]["recipient"], "+12025550123");
    assert!(
        ada.get("id").is_none(),
        "a view names no person by an id the surface does not declare: {ada}"
    );
}
