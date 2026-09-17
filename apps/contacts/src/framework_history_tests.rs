include!("../../../ui/tests/support/history_adapter_conformance.rs");
fn malformed_fixture(doc: &mut AutoCommit) {
    let key = "contacts";
    let map = doc.get(ROOT, key).unwrap().unwrap().1;
    let row = doc
        .put_object(&map, "bad-identity", automerge::ObjType::Map)
        .unwrap();
    doc.put(&row, "unexpected_effect", "do not restore")
        .unwrap();
}


fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut d = AutoCommit::new();
    let mut c = crate::model::sample_contacts().remove(0);
    c.first = "Selected contact".into();
    crate::doc::ContactsDoc::upsert_contact(&mut d, &c);
    let selected = d.get_heads()[0];
    c.first = "Live contact".into();
    c.phones.clear();
    c.groups.clear();
    crate::doc::ContactsDoc::upsert_contact(&mut d, &c);
    d.put(ROOT, "view_only", "current").unwrap();
    d.get_heads();
    (d, selected, "Selected contact".into())
}
fn verify_restored(d: &AutoCommit) {
    let c = crate::doc::ContactsDoc::list_contacts(d).remove(0);
    assert_eq!(c.first, "Selected contact");
    assert!(!c.phones.is_empty());
    assert!(!c.groups.is_empty());
}

#[test]
fn shared_navigation_reaches_other_historical_entries_at_both_widths() {
    let (mut doc, _, _) = fixture();
    let mut second = crate::model::Contact::new();
    second.first = "Zeta second historical contact".into();
    crate::doc::ContactsDoc::upsert_contact(&mut doc, &second);
    let selected = doc.get_heads()[0];
    let mut p = preview(&doc, selected, PreviewSide::After);
    let first = p.accessibility().description;
    assert!(first.contains("Live contact"));
    assert!(p.navigate(PreviewNavigation::NextPage));
    let second = p.accessibility().description;
    assert!(second.contains("Zeta second historical"));
    let mut d = managed_driver(doc, selected);
    assert_page_round_trip(&mut d, &first, &second);
}

#[test]
fn restore_reconciles_all_contact_fields_membership_tombstones_and_repeated_id_order() {
    let mut doc = AutoCommit::new();
    let mut contacts = crate::model::sample_contacts();
    contacts.truncate(2);
    for c in &contacts {
        crate::doc::ContactsDoc::upsert_contact(&mut doc, c);
    }
    let selected = doc.get_heads();
    let mut expected = crate::doc::ContactsDoc::list_contacts(&doc);
    expected.sort_by_key(|c| c.id);
    crate::doc::ContactsDoc::delete_contact(&mut doc, contacts[0].id);
    let mut changed = contacts[1].clone();
    changed.first = "Changed".into();
    changed.last.clear();
    changed.org.clear();
    changed.title.clear();
    changed.website = "changed.invalid".into();
    changed.notes = "Changed notes".into();
    changed.birthday = None;
    changed.favorite = !changed.favorite;
    changed.phones.reverse();
    changed
        .phones
        .push(crate::model::Labeled::new("extra", "555"));
    changed.emails.clear();
    changed.addresses.clear();
    changed.groups.clear();
    crate::doc::ContactsDoc::upsert_contact(&mut doc, &changed);
    let mut added = crate::model::Contact::new();
    added.first = "Only later".into();
    crate::doc::ContactsDoc::upsert_contact(&mut doc, &added);
    let current = doc.get_heads();
    let prepared = prepare_content_restore(
        &doc,
        definition().scopes[0].content_schema.unwrap(),
        &current,
        &selected,
    )
    .unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    let mut actual = crate::doc::ContactsDoc::list_contacts(&doc);
    actual.sort_by_key(|c| c.id);
    assert_eq!(actual, expected);
}

#[test]
fn already_open_form_rebases_restored_fields_and_keeps_only_unsaved_edits() {
    for edited in [None, Some("Job title"), Some("First name")] {
        let (doc, selected, _) = fixture();
        let mut d = managed_driver(doc, selected);
        d.click_named("history-back-live").unwrap();
        let edit = d.a11y_node_named("Edit contact").unwrap();
        d.perform_a11y_action(edit, &ui::a11y::Action::Activate)
            .unwrap();
        if let Some(label) = edited {
            let field = d.a11y_node_named(label).unwrap();
            d.perform_a11y_action(
                field,
                &ui::a11y::Action::SetValue("Unsaved local edit".into()),
            )
            .unwrap();
        }
        d.click_named("history-open").unwrap();
        select_version(&mut d, selected);
        d.click_named("history-restore").unwrap();
        d.key("Enter");
        d.click_named("history-back-live").unwrap();
        let expected_first = if edited == Some("First name") {
            "Unsaved local edit"
        } else {
            "Selected contact"
        };
        let displayed = |label: &str| {
            d.a11y_nodes()
                .into_iter()
                .find(|(_, n)| n.label() == Some(label))
                .and_then(|(_, n)| n.value().map(str::to_owned))
                .unwrap()
        };
        assert_eq!(displayed("First name"), expected_first, "{edited:?}");
        if let Some(label) = edited {
            assert_eq!(displayed(label), "Unsaved local edit");
        }
        assert!(
            d.a11y_node_named("Phone 1").is_some(),
            "restored repeated fields enter the existing editor"
        );
        // Sync again before Save: no stale draft value may leak back to content.
        d.resize((950, 760)).unwrap();
        assert_eq!(
            crate::doc::ContactsDoc::list_contacts(&d.state_mut().doc)[0].first,
            "Selected contact"
        );
        let save = d.a11y_node_named("Save changes").unwrap();
        d.perform_a11y_action(save, &ui::a11y::Action::Activate)
            .unwrap();
        let current = crate::doc::ContactsDoc::list_contacts(&d.state_mut().doc).remove(0);
        assert_eq!(current.first, expected_first);
        if edited == Some("Job title") {
            assert_eq!(current.title, "Unsaved local edit");
        }
        assert!(!current.phones.is_empty());
    }
}

#[test]
fn restore_removing_edited_contact_closes_clean_form_but_retains_unsaved_draft() {
    for dirty in [false, true] {
        let mut doc = AutoCommit::new();
        doc.put_object(ROOT, "contacts", automerge::ObjType::Map)
            .unwrap();
        let selected = doc.get_heads()[0];
        let mut contact = crate::model::Contact::new();
        contact.first = "Later contact".into();
        crate::doc::ContactsDoc::upsert_contact(&mut doc, &contact);
        doc.get_heads();
        let mut d = managed_driver(doc, selected);
        d.click_named("history-back-live").unwrap();
        let edit = d.a11y_node_named("Edit contact").unwrap();
        d.perform_a11y_action(edit, &ui::a11y::Action::Activate)
            .unwrap();
        if dirty {
            let field = d.a11y_node_named("First name").unwrap();
            d.perform_a11y_action(
                field,
                &ui::a11y::Action::SetValue("Unsaved deleted contact".into()),
            )
            .unwrap();
        }
        d.click_named("history-open").unwrap();
        select_version(&mut d, selected);
        d.click_named("history-restore").unwrap();
        d.key("Enter");
        d.click_named("history-back-live").unwrap();
        assert!(crate::doc::ContactsDoc::list_contacts(&d.state_mut().doc).is_empty());
        if dirty {
            assert!(format!("{:?}", d.a11y_nodes()).contains("Unsaved deleted contact"));
            assert!(format!("{:?}", d.a11y_nodes()).contains("no longer exists"));
            let save = d.a11y_node_named("Save changes").unwrap();
            d.perform_a11y_action(save, &ui::a11y::Action::Activate)
                .unwrap();
            assert!(crate::doc::ContactsDoc::list_contacts(&d.state_mut().doc).is_empty());
        } else {
            assert!(
                d.a11y_node_named("First name").is_none(),
                "a removed clean form must close"
            );
        }
    }
}

#[test]
fn open_form_rebases_reordered_rows_by_identity_before_save() {
    let mut doc = AutoCommit::new();
    let mut contact = crate::model::Contact::new();
    contact.first = "Repeated rows".into();
    contact.phones = vec![
        crate::model::Labeled::new("home", "111"),
        crate::model::Labeled::new("work", "222"),
    ];
    let expected = contact.phones.clone();
    crate::doc::ContactsDoc::upsert_contact(&mut doc, &contact);
    let selected = doc.get_heads()[0];
    for row in &contact.phones {
        assert!(crate::doc::ContactsDoc::remove_labeled(
            &mut doc, contact.id, "phones", row.id
        ));
    }
    contact.phones.reverse();
    contact.phones[1].value = "999".into();
    crate::doc::ContactsDoc::upsert_contact(&mut doc, &contact);
    assert_eq!(
        crate::doc::ContactsDoc::list_contacts(&doc)[0].phones,
        contact.phones
    );
    doc.get_heads();
    let mut d = managed_driver(doc, selected);
    d.click_named("history-back-live").unwrap();
    let edit = d.a11y_node_named("Edit contact").unwrap();
    d.perform_a11y_action(edit, &ui::a11y::Action::Activate)
        .unwrap();
    let field = d.a11y_node_named("Phone 1").unwrap();
    d.perform_a11y_action(field, &ui::a11y::Action::SetValue("local 222".into()))
        .unwrap();
    d.click_named("history-open").unwrap();
    select_version(&mut d, selected);
    d.click_named("history-restore").unwrap();
    d.key("Enter");
    d.click_named("history-back-live").unwrap();
    for (name, expected) in [("Phone 1", "111"), ("Phone 2", "local 222")] {
        assert!(
            d.a11y_nodes()
                .iter()
                .any(|(_, node)| node.label() == Some(name) && node.value() == Some(expected)),
            "expected {name}={expected}; inputs: {:?}",
            d.a11y_nodes()
                .iter()
                .filter_map(|(_, node)| node
                    .value()
                    .map(|value| (node.label().map(str::to_owned), value.to_owned())))
                .collect::<Vec<_>>()
        );
    }
    let save = d.a11y_node_named("Save changes").unwrap();
    d.perform_a11y_action(save, &ui::a11y::Action::Activate)
        .unwrap();
    let saved = crate::doc::ContactsDoc::list_contacts(&d.state_mut().doc).remove(0);
    assert_eq!(saved.phones[0], expected[0]);
    assert_eq!(saved.phones[1].id, expected[1].id);
    assert_eq!(saved.phones[1].value, "local 222");
}

fn test_root() -> Box<dyn ui::widget::Widget> { crate::app_root() }
