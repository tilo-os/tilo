#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = ui::prelude::HeadlessApp::<generated_app::App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
