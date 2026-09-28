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

/// A recorded version without the schema marker still previews its recorded
/// maps.
#[test]
fn unmarked_recorded_versions_preview_content() {
    // phase incomplete_schema_marker_never_turns_recorded_maps_into_empty_previews
    {
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
}

fn test_root() -> Box<dyn ui::widget::Widget> {
    crate::app_root()
}
