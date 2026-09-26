include!("../../../crates/sdk/ui-core/tests/support/history_adapter_conformance.rs");
fn malformed_fixture(doc: &mut AutoCommit) {
    let key = "snippets";
    let map = doc.get(ROOT, key).unwrap().unwrap().1;
    let row = doc
        .put_object(&map, "bad-identity", automerge::ObjType::Map)
        .unwrap();
    doc.put(&row, "unexpected_effect", "do not restore")
        .unwrap();
}

fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut d = AutoCommit::new();
    let mut s = crate::seed::starter_library().remove(0);
    s.title = "Selected snippet".into();
    s.body = "fn selected_body() {}".into();
    s.uses = 7;
    crate::doc::SnippetsDoc::upsert(&mut d, &s);
    let selected = d.get_heads()[0];
    s.title = "Live snippet".into();
    s.body = "live_body".into();
    s.uses = 91;
    crate::doc::SnippetsDoc::upsert(&mut d, &s);
    d.put(ROOT, "view_only", "current").unwrap();
    d.get_heads();
    (d, selected, "Selected snippet".into())
}
fn verify_restored(d: &AutoCommit) {
    let s = crate::doc::SnippetsDoc::list(d).remove(0);
    assert_eq!(s.title, "Selected snippet");
    assert_eq!(s.body, "fn selected_body() {}");
    assert_eq!(s.uses, 91);
}

/// Membership restore keeps current uses, and navigation reaches historical entries at both widths.
#[test]
fn history_restore_and_navigation_preserve_library_state() {
    // Case `membership_restore_preserves_current_uses_and_omits_historical_uses_on_creation`.
    {
        let mut doc = AutoCommit::new();
        let mut original = crate::seed::starter_library().remove(0);
        original.uses = 123;
        crate::doc::SnippetsDoc::upsert(&mut doc, &original);
        let selected = doc.get_heads();
        crate::doc::SnippetsDoc::remove(&mut doc, original.id);
        let mut added = crate::model::Snippet::blank("Later");
        added.title = "Later addition".into();
        crate::doc::SnippetsDoc::upsert(&mut doc, &added);
        let heads = doc.get_heads();
        let prepared = prepare_content_restore(
            &doc,
            definition().scopes[0].content_schema.unwrap(),
            &heads,
            &selected,
        )
        .unwrap();
        apply_prepared_content_restore(&mut doc, &prepared).unwrap();
        let snippets = crate::doc::SnippetsDoc::list(&doc);
        assert_eq!(snippets.len(), 1);
        let mut expected = original.clone();
        expected.uses = 0;
        assert_eq!(snippets[0], expected);
        let map = doc.get(ROOT, "snippets").unwrap().unwrap().1;
        let row = doc.get(&map, original.id.to_string()).unwrap().unwrap().1;
        assert!(doc.get(&row, "uses").unwrap().is_none());
    }

    // Case `shared_navigation_reaches_other_historical_entries_at_both_widths`.
    {
        let (mut doc, _, _) = fixture();
        let mut second = crate::model::Snippet::blank("Another folder");
        second.title = "Zeta second historical snippet".into();
        second.body = "fn second_body() {}".into();
        crate::doc::SnippetsDoc::upsert(&mut doc, &second);
        let selected = doc.get_heads()[0];
        let mut p = preview(&doc, selected, PreviewSide::After);
        let first = p.accessibility().description;
        assert!(first.contains("live_body"));
        assert!(p.navigate(PreviewNavigation::NextPage));
        let second = p.accessibility().description;
        assert!(second.contains("Zeta second historical"));
        let mut d = managed_driver(doc, selected);
        assert_page_round_trip(&mut d, &first, &second);
    }
}

fn test_root() -> Box<dyn ui::widget::Widget> {
    crate::app_root()
}
