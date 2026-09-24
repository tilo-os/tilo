include!("../../../crates/sdk/ui-core/tests/support/history_adapter_conformance.rs");
fn malformed_fixture(doc: &mut AutoCommit) {
    let key = "history";
    let map = doc.get(ROOT, key).unwrap().unwrap().1;
    let row = doc
        .put_object(&map, "bad-identity", automerge::ObjType::Map)
        .unwrap();
    doc.put(&row, "unexpected_effect", "do not restore")
        .unwrap();
}

fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut d = AutoCommit::new();
    let mut m = crate::model::CalcModel::seeded();
    m.history[0].expr = "selected_expression".into();
    crate::doc::store(&mut d, &m);
    let selected = d.get_heads()[0];
    m.history[0].expr = "live_expression".into();
    m.funcs[0].expr = "x^7".into();
    crate::doc::store(&mut d, &m);
    d.put(ROOT, "view_only", "current").unwrap();
    d.get_heads();
    (d, selected, "selected_expression".into())
}
fn verify_restored(d: &AutoCommit) {
    let m = crate::doc::load(d).unwrap();
    assert!(m.history.iter().any(|h| h.expr == "selected_expression"));
    assert_ne!(m.funcs[0].expr, "x^7");
    assert_eq!(
        d.get(ROOT, "calculator_resources_v2")
            .unwrap()
            .unwrap()
            .0
            .as_bool(),
        Some(true)
    );
}

#[test]
fn legacy_list_versions_are_readable_but_restore_refusal_is_nonmutating() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "mode", "graph2d").unwrap();
    for (key, expr) in [("history", "7*6"), ("funcs", "sin(x)"), ("surfaces", "x*y")] {
        let list = doc.put_object(ROOT, key, automerge::ObjType::List).unwrap();
        let row = doc
            .insert_object(&list, 0, automerge::ObjType::Map)
            .unwrap();
        doc.put(&row, "expr", expr).unwrap();
        if key == "history" {
            doc.put(&row, "result", "42").unwrap();
        } else {
            doc.put(&row, "visible", true).unwrap();
        }
    }
    let legacy = doc.get_heads()[0];
    // Existing live migration is the only migration. Historical projection must
    // never call store/migrate or manufacture a normalized historical version.
    let mut m = crate::model::CalcModel::default();
    crate::doc::store(&mut doc, &m);
    let canonical = doc.get_heads()[0];
    m.history.push(crate::model::HistoryEntry {
        expr: "live".into(),
        result: "1".into(),
    });
    crate::doc::store(&mut doc, &m);
    let heads = doc.get_heads();
    let mut p = preview(&doc, legacy, PreviewSide::After);
    assert!(p.accessibility().description.contains("7*6 = 42"));
    assert!(
        p.accessibility()
            .description
            .contains("Legacy list-format version: preview only")
    );
    let mut tr = ui::TextRenderer::new();
    for (page, expr) in [("2d:0", "sin(x)"), ("3d:0", "x*y")] {
        assert!(p.navigate(PreviewNavigation::SelectPage(page.into())));
        assert!(p.accessibility().description.contains(expr));
        for width in [360., 1000.] {
            p.layout(
                vello::kurbo::Rect::new(0., 0., width, 600.),
                &mut tr,
                &ui::Theme::dark(),
            )
            .unwrap();
            p.draw(
                &mut vello::Scene::new(),
                vello::kurbo::Affine::IDENTITY,
                &mut tr,
                &ui::Theme::dark(),
            );
        }
    }
    let schema = definition().scopes[0].content_schema.unwrap();
    let before = project_content(&doc, schema).unwrap();
    let error = prepare_content_restore(&doc, schema, &heads, &[legacy])
        .unwrap_err()
        .to_string();
    assert!(error.contains("history"), "{error}");
    assert_eq!(doc.base_heads(), heads);
    assert_eq!(doc.pending_ops(), 0);
    assert_eq!(project_content(&doc, schema).unwrap(), before);
    assert_eq!(
        doc.get(ROOT, "calculator_resources_v2")
            .unwrap()
            .unwrap()
            .0
            .as_bool(),
        Some(true)
    );
    let mut d = managed_driver(doc, legacy);
    let undo = d.state_mut().undo_status(None);
    let heads = d.state_mut().doc.base_heads();
    d.click_named("history-restore").unwrap();
    d.key("Enter");
    assert_eq!(d.state_mut().doc.base_heads(), heads);
    assert_eq!(d.state_mut().doc.pending_ops(), 0);
    assert_eq!(d.state_mut().undo_status(None), undo);
    d.key("Escape");
    select_version(&mut d, canonical);
    d.click_named("history-restore").unwrap();
    d.key("Enter");
    let restored = crate::doc::load(&d.state_mut().doc).unwrap();
    assert_eq!(restored.history[0].expr, "7*6");
    assert_eq!(restored.history[0].result, "42");
    assert_eq!(restored.funcs[0].expr, "sin(x)");
    assert_eq!(restored.surfaces[0].expr, "x*y");
}

#[test]
fn incomplete_schema_marker_never_turns_recorded_maps_into_empty_previews() {
    let (mut doc, _, _) = fixture();
    doc.delete(ROOT, "calculator_resources_v2").unwrap();
    let selected = doc.get_heads()[0];
    let p = preview(&doc, selected, PreviewSide::After);
    assert!(
        p.accessibility().description.contains("live_expression"),
        "{:?}",
        p.accessibility()
    );
}

fn test_root() -> Box<dyn ui::widget::Widget> {
    crate::app_root()
}
