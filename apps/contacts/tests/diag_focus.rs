//! DIAGNOSTIC (temporary): determine, empirically, whether the `ui::App`
//! keyboard-gating layer intercepts Tab for the contacts root widget, by
//! standing up a real `ui::App` around `contacts::app_root()`.

use ui::widget::Widget;

#[test]
fn diag_contacts_focusable_order_len() {
    let state = ui::State::new();
    let mut app = ui::App::new(state, contacts::app_root());
    app.init_layout();
    eprintln!(
        "DIAG contacts focusable_order.len() = {}",
        app.focusable_order.len()
    );
    // Not an assertion we care about passing/failing — printed for inspection.
    assert!(app.focusable_order.len() <= 1_000_000);
}
