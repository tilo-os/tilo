/// The app satisfies the Tilo-native contract: every control that can cause
/// an effect is bound to a published action.
#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = ui::prelude::TestApp::<generated_app::App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
