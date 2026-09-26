//! Keyboard preferences survive a restart through the production store.

use automerge::ROOT;
use osk::OskWidget;
use ui::tilo_actions::OperationKind;
use ui::widget::Widget;

#[test]
fn layout_and_suggestion_preferences_round_trip_through_the_store() {
    let _home = tilo_test_env::TestHome::new().expect("private home");

    {
        let (mut state, mut autosave) =
            ui::persist::try_open(osk::KEYBOARD_APP_ID).expect("open the keyboard's document");
        let mut keyboard = OskWidget::new();
        let payload = serde_json::to_vec("azerty").unwrap();
        assert!(
            keyboard
                .invoke_resource_operation(
                    OperationKind::Effect,
                    "osk.layout",
                    &payload,
                    &state.doc,
                    &ROOT,
                )
                .unwrap()
        );
        keyboard.set_suggestions_enabled(false);
        keyboard.sync(&mut state.doc, &ROOT);
        autosave.request(&mut state.doc);
        autosave.flush();
    }

    let (mut state, _autosave) =
        ui::persist::try_open(osk::KEYBOARD_APP_ID).expect("reopen the keyboard's document");
    let mut restarted = OskWidget::new();
    restarted.sync(&mut state.doc, &ROOT);
    assert_eq!(restarted.active_layout_name(), "azerty");
    assert!(!restarted.suggestions_enabled());
}
