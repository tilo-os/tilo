include!("../../../ui/tests/support/history_adapter_conformance.rs");
fn malformed_fixture(doc: &mut AutoCommit) {
    let themes = doc.get(ROOT, "themes").unwrap().unwrap().1;
    let row = doc.put_object(&themes, "broken", automerge::ObjType::Map).unwrap();
    doc.put(&row, "unexpected_effect", "bad").unwrap();
}
fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut doc = AutoCommit::new();
    let mut s = crate::model::seed_custom();
    s.name = "Selected theme".into();
    s.accent = [12,34,56];
    crate::doc::save_catalog(&mut doc, &ROOT, &[s.clone()]);
    let selected = doc.get_heads()[0];
    crate::doc::set_name(&mut doc, &ROOT, &s.id, "Live theme");
    crate::doc::set_token(&mut doc, &ROOT, &s.id, crate::model::TokenId::Accent, [200,100,1]);
    doc.put(ROOT, "active", "saved-applied-identity").unwrap();
    doc.put(ROOT, "view_only", "current").unwrap();
    doc.get_heads();
    (doc, selected, "Selected theme".into())
}
fn verify_restored(doc: &AutoCommit) {
    let themes = crate::doc::load_catalog(doc, &ROOT).unwrap();
    let s = themes.iter().find(|s| s.id == "sunset").unwrap();
    assert_eq!(s.name, "Selected theme");
    assert_eq!(s.accent, [12,34,56]);
    assert_eq!(crate::doc::get_str(doc, &ROOT, "active").as_deref(), Some("saved-applied-identity"));
}

#[test]
fn legacy_library_restore_and_fresh_live_sync_preserve_exact_content() {
    use ui::widget::Widget;
    let mut doc=AutoCommit::new();
    let mut one=crate::model::seed_custom();one.name="Legacy selected".into();
    let mut two=one.clone();two.id="second".into();
    crate::doc::put_text(&mut doc,&ROOT,"catalog",&serde_json::to_string(&[one,two]).unwrap());
    doc.put(ROOT,"active","saved-applied-identity").unwrap();
    let selected=doc.get_heads()[0];
    let expected=project_content(&doc,&crate::framework_history::SCHEMA).unwrap();
    crate::doc::save_catalog(&mut doc,&ROOT,&[crate::model::seed_custom()]);doc.get_heads();
    let prepared=prepare_content_restore(&doc,&crate::framework_history::SCHEMA,&doc.base_heads(),&[selected]).unwrap();
    apply_prepared_content_restore(&mut doc,&prepared).unwrap();doc.get_heads();
    for _ in 0..2 { let mut app=crate::ThemesApp::new();app.sync(&mut doc,&ROOT);doc.get_heads();assert_eq!(project_content(&doc,&crate::framework_history::SCHEMA).unwrap(),expected);doc=AutoCommit::load(&doc.save()).unwrap(); }
}

#[test]
fn valid_legacy_edit_materializes_only_addressed_record_and_invalid_edit_does_nothing() {
    use qos_actions::Action;
    let mut doc=AutoCommit::new();let one=crate::model::seed_custom();let mut two=one.clone();two.id="second".into();
    crate::doc::put_text(&mut doc,&ROOT,"catalog",&serde_json::to_string(&[one,two]).unwrap());
    let before=doc.get_heads();
    for action in [crate::actions::set_custom_theme_name_action("sunset",""),crate::actions::set_custom_theme_font_action("sunset","unknown"),crate::actions::set_custom_theme_radius_action("sunset",f32::NAN)] { action.apply(&mut doc).unwrap(); }
    assert_eq!(doc.get_heads(),before);
    let action=crate::actions::set_custom_theme_name_action("sunset","Explicit legacy edit");
    qos_actions::apply_local(&mut doc,&action,crate::actions::encode(&action)).unwrap();
    let themes=doc.get(ROOT,"themes").unwrap().unwrap().1;
    assert!(doc.get(&themes,"sunset").unwrap().is_some());assert!(doc.get(&themes,"second").unwrap().is_none());
    assert_eq!(crate::doc::load_catalog(&doc,&ROOT).unwrap().iter().find(|s|s.id=="sunset").unwrap().name,"Explicit legacy edit");
    assert_eq!(doc.get_changes(&before).len(),1);
}

fn test_root() -> Box<dyn ui::widget::Widget> { crate::app_root() }
