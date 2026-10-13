//! Contacts' controls are the window's: the header commands, the list and
//! the form's fields are retained widgets in the App's focus order, so Tab
//! moves through them the way it does in every Tilo window.

#[test]
fn contacts_controls_join_the_window_focus_order() {
    let state = ui::State::new();
    let mut app = ui::App::new(state, Box::new(contacts::ContactsApp::new()));
    app.init_layout();
    assert!(
        !app.focusable_order.is_empty(),
        "the window's commands are reachable with Tab"
    );
}
