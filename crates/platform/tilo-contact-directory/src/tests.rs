use super::ContactDirectory;
use tilo_resource::{Id, Replica};

fn first() -> Id {
    Id::from_raw("00000000-0000-4000-8000-000000000001")
}
fn second() -> Id {
    Id::from_raw("00000000-0000-4000-8000-000000000002")
}

fn fixture() -> Result<Replica<ContactDirectory>, Box<dyn std::error::Error>> {
    let mut record = Replica::<ContactDirectory>::new();
    record.apply(ContactDirectory::PROJECT_CONTACT.with((first(), "Alex".into())))?;
    record.apply(ContactDirectory::PROJECT_CONTACT.with((second(), "Alex".into())))?;
    Ok(record)
}

#[test]
fn email_hints_preserve_ambiguity_case_and_aliases_without_identity_links() {
    let mut record = fixture().expect("fixture succeeds");
    for person in [first(), second()] {
        record
            .apply(
                ContactDirectory::PROJECT_EMAILS.with((person, vec!["Alice@EXAMPLE.test".into()])),
            )
            .expect("hints project");
    }
    let state = record.state().expect("state loads");
    assert_eq!(
        state.email_candidates("Alice@example.test"),
        vec![first(), second()]
    );
    assert!(state.email_candidates("alice@example.test").is_empty());
    assert!(state.email_candidates("Alice+work@example.test").is_empty());
    assert!(
        state
            .email_candidates("Alice <Alice@example.test>")
            .is_empty()
    );
    assert!(
        state
            .people
            .iter()
            .all(|(_, person)| person.endpoints.is_empty())
    );
    assert!(state.resolve("email", "Alice@example.test").is_err());
    record
        .apply(ContactDirectory::REMOVE_CONTACT.with(first()))
        .expect("removal succeeds");
    assert!(
        record
            .apply(
                ContactDirectory::PROJECT_EMAILS
                    .with((first(), vec!["replacement@example.test".into()]))
            )
            .is_err()
    );
    assert_eq!(
        record
            .state()
            .expect("state loads")
            .email_candidates("Alice@example.test"),
        vec![second()]
    );
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

#[test]
fn duplicate_names_and_cross_service_recipients_keep_distinct_people() {
    let mut record = fixture().expect("fixture operation succeeds");
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
            first(),
            Id::from_raw("one"),
            "signal".into(),
            "+12025550123".into(),
        )))
        .expect("fixture operation succeeds");
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
            second(),
            Id::from_raw("two"),
            "sms".into(),
            "+12025550123".into(),
        )))
        .expect("fixture operation succeeds");
    let state = record.state().expect("fixture operation succeeds");
    assert_eq!(state.resolve("signal", "+12025550123"), Ok(first()));
    assert_eq!(state.resolve("sms", "+12025550123"), Ok(second()));
    assert!(state.resolve("signal", "Alex").is_err());
    assert_eq!(state.people.len(), 2);
}

#[test]
fn sequential_conflicting_confirmation_refuses_and_keeps_existing_mapping() {
    let mut record = fixture().expect("fixture operation succeeds");
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
            first(),
            Id::from_raw("one"),
            "signal".into(),
            "recipient".into(),
        )))
        .expect("fixture operation succeeds");
    assert!(
        record
            .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
                second(),
                Id::from_raw("two"),
                "signal".into(),
                "recipient".into()
            )))
            .is_err()
    );
    assert_eq!(
        record
            .state()
            .expect("fixture operation succeeds")
            .resolve("signal", "recipient"),
        Ok(first())
    );
}

#[test]
fn concurrent_conflicting_confirmations_converge_without_choosing_recipient_owner() {
    let mut left = fixture().expect("fixture operation succeeds");
    let mut right = left.fork();
    left.apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
        first(),
        Id::from_raw("one"),
        "signal".into(),
        "recipient".into(),
    )))
    .expect("fixture operation succeeds");
    right
        .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
            second(),
            Id::from_raw("two"),
            "signal".into(),
            "recipient".into(),
        )))
        .expect("fixture operation succeeds");
    left.sync_with(&mut right)
        .expect("fixture operation succeeds");
    assert_eq!(
        left.state().expect("fixture operation succeeds"),
        right.state().expect("fixture operation succeeds")
    );
    assert!(
        left.state()
            .expect("fixture operation succeeds")
            .resolve("signal", "recipient")
            .is_err()
    );
    assert!(
        right
            .state()
            .expect("fixture operation succeeds")
            .resolve("signal", "recipient")
            .is_err()
    );
}

#[test]
fn deletion_blocks_delayed_projection_and_recipient_routing() {
    let mut record = fixture().expect("fixture operation succeeds");
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
            first(),
            Id::from_raw("one"),
            "signal".into(),
            "recipient".into(),
        )))
        .expect("fixture operation succeeds");
    record
        .apply(ContactDirectory::REMOVE_CONTACT.with(first()))
        .expect("fixture operation succeeds");
    record
        .apply(ContactDirectory::PROJECT_CONTACT.with((first(), "Alex new name".into())))
        .expect("fixture operation succeeds");
    assert!(
        record
            .state()
            .expect("fixture operation succeeds")
            .resolve("signal", "recipient")
            .is_err()
    );
    assert!(
        record
            .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
                first(),
                Id::from_raw("fresh"),
                "signal".into(),
                "fresh-recipient".into()
            )))
            .is_err()
    );
}

#[test]
fn revoked_association_ids_cannot_be_reused_or_retargeted() {
    let mut record = fixture().expect("fixture operation succeeds");
    let endpoint = Id::from_raw("old-association");
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
            first(),
            endpoint.clone(),
            "sms".into(),
            "+12025550123".into(),
        )))
        .expect("confirmation succeeds");
    record
        .apply_confirmed(ContactDirectory::FORGET_ENDPOINT.with((first(), endpoint.clone())))
        .expect("revocation succeeds");
    assert!(
        record
            .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
                first(),
                endpoint.clone(),
                "sms".into(),
                "+12025550123".into()
            )))
            .is_err()
    );
    assert!(
        record
            .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
                second(),
                endpoint.clone(),
                "sms".into(),
                "+12025550123".into()
            )))
            .is_err()
    );
    let state = record.state().expect("state loads");
    let historical = state
        .people
        .get(&first())
        .expect("person remains")
        .endpoints
        .get(&endpoint)
        .expect("association remains");
    assert!(historical.removed);
    assert_eq!(historical.recipient, "+12025550123");
    assert!(state.resolve("sms", "+12025550123").is_err());
}

#[test]
fn identity_revision_and_provider_scope_require_exact_confirmation() {
    let mut record = fixture().expect("fixture operation succeeds");
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ACCOUNT.with((
            first(),
            Id::from_raw("signal-key"),
            "signal".into(),
            "signal-main".into(),
            "aci:alice".into(),
            "key-1".into(),
        )))
        .expect("confirmation succeeds");
    let state = record.state().expect("state loads");
    assert_eq!(
        state.resolve_account("signal", "signal-main", "aci:alice", "key-1"),
        Ok(first())
    );
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
        state.resolve("signal", "aci:alice").is_err(),
        "compatibility resolver must not bypass identity revision"
    );
    assert!(
        record
            .apply_confirmed(ContactDirectory::CONFIRM_ACCOUNT.with((
                first(),
                Id::from_raw("new-key"),
                "signal".into(),
                "signal-main".into(),
                "aci:alice".into(),
                "key-2".into()
            )))
            .is_err(),
        "identity changes require revocation"
    );
    record
        .apply_confirmed(
            ContactDirectory::FORGET_ENDPOINT.with((first(), Id::from_raw("signal-key"))),
        )
        .expect("revocation succeeds");
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ACCOUNT.with((
            second(),
            Id::from_raw("new-key"),
            "signal".into(),
            "signal-main".into(),
            "aci:alice".into(),
            "key-2".into(),
        )))
        .expect("reassignment succeeds after review");
    let state = record.state().expect("state loads");
    assert_eq!(
        state.resolve_account("signal", "signal-main", "aci:alice", "key-2"),
        Ok(second())
    );
    assert!(
        state
            .resolve_account("signal", "signal-main", "aci:alice", "key-1")
            .is_err()
    );
}

#[test]
fn shared_phone_hints_suggest_all_people_without_linking_anyone() {
    let mut record = fixture().expect("fixture operation succeeds");
    for person in [first(), second()] {
        record
            .apply(
                ContactDirectory::PROJECT_PHONES.with((person, vec!["+1 (202) 555-0123".into()])),
            )
            .expect("projection succeeds");
    }
    let state = record.state().expect("state loads");
    assert_eq!(
        state.phone_candidates("+12025550123"),
        vec![first(), second()]
    );
    assert!(state.phone_candidates("2025550123").is_empty());
    assert!(state.resolve("sms", "+12025550123").is_err());
    assert!(
        state
            .resolve_account("sms", "carrier", "+12025550123", "assignment-1")
            .is_err()
    );
}

#[test]
fn multiple_sender_accounts_require_a_choice_and_preserve_provenance() {
    use crate::routing::{AccountChoice, SenderAccount, routes, select_route};
    let mut record = fixture().expect("fixture operation succeeds");
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ACCOUNT.with((
            first(),
            Id::from_raw("signal-key"),
            "signal".into(),
            "signal-main".into(),
            "aci:alice".into(),
            "key-1".into(),
        )))
        .expect("confirmation succeeds");
    let sender = |id: &str| SenderAccount {
        id: Id::from_raw(id),
        service: "signal".into(),
        scope: "signal-main".into(),
        enabled: true,
    };
    let state = record.state().expect("state loads");
    let mut senders = vec![sender("personal"), sender("work")];
    let candidates = routes(&state, &first(), "signal", &senders);
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        select_route(&candidates, None),
        AccountChoice::SelectionRequired(candidates.clone())
    );
    let selected = candidates.first().expect("two routes");
    assert_eq!(
        select_route(&candidates, Some(selected)),
        AccountChoice::Ready(selected.clone())
    );
    assert_eq!(selected.person, first());
    assert_eq!(selected.association.as_str(), "signal-key");
    assert_eq!(selected.revision, "key-1");
    for sender in &mut senders {
        sender.enabled = false;
    }
    assert_eq!(
        select_route(
            &routes(&state, &first(), "signal", &senders),
            Some(selected)
        ),
        AccountChoice::Unavailable
    );
    assert!(routes(&state, &first(), "whatsapp", &senders).is_empty());
}

#[test]
fn same_person_can_map_to_tilo_and_external_services_without_combining_accounts() {
    let mut record = fixture().expect("fixture operation succeeds");
    for (service, scope, subject) in [
        ("tilo", "tilo-users", "stable-user-id"),
        ("signal", "signal-main", "aci:alice"),
        ("whatsapp", "business-a", "US.alice"),
    ] {
        record
            .apply_confirmed(ContactDirectory::CONFIRM_ACCOUNT.with((
                first(),
                Id::from_raw(service),
                service.into(),
                scope.into(),
                subject.into(),
                "identity-1".into(),
            )))
            .expect("confirmation succeeds");
    }
    let state = record.state().expect("state loads");
    assert_eq!(
        state.resolve_account("tilo", "tilo-users", "stable-user-id", "identity-1"),
        Ok(first())
    );
    assert_eq!(
        state.resolve_account("signal", "signal-main", "aci:alice", "identity-1"),
        Ok(first())
    );
    assert_eq!(
        state.resolve_account("whatsapp", "business-a", "US.alice", "identity-1"),
        Ok(first())
    );
    assert!(
        state
            .resolve_account("whatsapp", "business-b", "US.alice", "identity-1")
            .is_err()
    );
}

#[test]
fn association_ids_remain_reserved_after_person_deletion() {
    let mut record = fixture().expect("fixture operation succeeds");
    let endpoint = Id::from_raw("historical");
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
            first(),
            endpoint.clone(),
            "sms".into(),
            "+12025550123".into(),
        )))
        .expect("confirmation succeeds");
    record
        .apply(ContactDirectory::REMOVE_CONTACT.with(first()))
        .expect("deletion succeeds");
    assert!(
        record
            .apply_confirmed(ContactDirectory::CONFIRM_ENDPOINT.with((
                second(),
                endpoint,
                "sms".into(),
                "+12025550123".into()
            )))
            .is_err()
    );
}

#[test]
fn concurrent_revision_aware_confirmations_refuse_before_routing() {
    use crate::routing::{SenderAccount, routes};
    let mut left = fixture().expect("fixture operation succeeds");
    let mut right = left.fork();
    left.apply_confirmed(ContactDirectory::CONFIRM_ACCOUNT.with((
        first(),
        Id::from_raw("first-link"),
        "signal".into(),
        "signal-main".into(),
        "aci:alice".into(),
        "key-1".into(),
    )))
    .expect("confirmation succeeds");
    right
        .apply_confirmed(ContactDirectory::CONFIRM_ACCOUNT.with((
            second(),
            Id::from_raw("second-link"),
            "signal".into(),
            "signal-main".into(),
            "aci:alice".into(),
            "key-2".into(),
        )))
        .expect("confirmation succeeds");
    left.sync_with(&mut right).expect("replicas sync");
    let state = left.state().expect("state loads");
    assert_eq!(state, right.state().expect("state loads"));
    for revision in ["key-1", "key-2"] {
        assert!(
            state
                .resolve_account("signal", "signal-main", "aci:alice", revision)
                .is_err()
        );
    }
    let senders = [SenderAccount {
        id: Id::from_raw("own"),
        service: "signal".into(),
        scope: "signal-main".into(),
        enabled: true,
    }];
    assert!(routes(&state, &first(), "signal", &senders).is_empty());
    assert!(routes(&state, &second(), "signal", &senders).is_empty());
}

#[test]
fn reviewed_revision_replacement_is_atomic_and_keeps_old_attribution() {
    let mut record = fixture().expect("fixture operation succeeds");
    let old = Id::from_raw("old-reference");
    record
        .apply_confirmed(ContactDirectory::CONFIRM_ACCOUNT.with((
            first(),
            old.clone(),
            "signal".into(),
            "signal-main".into(),
            "aci:alice".into(),
            "key-1".into(),
        )))
        .expect("confirmation succeeds");
    let before = record.state().expect("state loads");
    assert!(
        record
            .apply_confirmed(ContactDirectory::REPLACE_ACCOUNT.with((
                first(),
                old.clone(),
                Id::from_raw("new-reference"),
                "signal".into(),
                "signal-main".into(),
                "aci:alice".into(),
                String::new()
            )))
            .is_err()
    );
    assert_eq!(
        record.state().expect("state loads"),
        before,
        "refused replacement must not revoke the old route"
    );
    record
        .apply_confirmed(ContactDirectory::REPLACE_ACCOUNT.with((
            first(),
            old.clone(),
            Id::from_raw("new-reference"),
            "signal".into(),
            "signal-main".into(),
            "aci:alice".into(),
            "key-2".into(),
        )))
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
        .get(&old)
        .expect("old association remains");
    assert!(historical.removed);
    assert_eq!(historical.revision, "key-1");
}

#[test]
fn legacy_person_records_load_without_email_hints_or_identity_changes() {
    use tilo_resource::{Collection, Doc, Fields, Json, Model};
    #[derive(Model, Default)]
    struct LegacyPerson {
        label: String,
        removed: bool,
        phones: Json<Vec<String>>,
        endpoints: Collection<super::Endpoint>,
    }
    let mut doc = Doc::new();
    let old = LegacyPerson {
        label: "Alex".into(),
        phones: Json(vec!["+12025550123".into()]),
        ..LegacyPerson::default()
    };
    old.write_fields(&mut doc, &tilo_resource::__private::ROOT)
        .expect("legacy record writes");
    let person = super::Person::read_fields(&doc, &tilo_resource::__private::ROOT)
        .expect("additive schema reads legacy record");
    assert_eq!(person.label, old.label);
    assert_eq!(person.phones, old.phones);
    assert!(person.emails.0.is_empty());
    assert!(person.endpoints.is_empty());
}

/// One projection of many people is what projecting each of them alone
/// is: labels and hints land, a deleted person stays deleted, and the
/// removed are tombstoned.
#[test]
fn one_projection_of_many_people_is_each_projected_alone() {
    use super::Projected;
    let third = Id::from_raw("00000000-0000-4000-8000-000000000003");
    let mut batched = fixture().expect("fixture succeeds");
    let mut single = fixture().expect("fixture succeeds");
    for record in [&mut batched, &mut single] {
        record
            .apply(ContactDirectory::REMOVE_CONTACT.with(second()))
            .expect("removing succeeds");
    }
    let people = [first(), second(), third.clone()].map(|person| Projected {
        label: format!("Person {person}"),
        phones: vec!["+12025550123".into()],
        emails: vec![format!("{person}@example.test")],
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
        vec![third],
        "only a person still in the book is a candidate"
    );

    let mut record = fixture().expect("fixture succeeds");
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
