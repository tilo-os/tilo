//! Contacts owns its keyboard focus entirely. Its edit-form text fields (and
//! the list search box) are positioned with `apply_layout_external` and driven
//! by the app's own `handle_event`, so they are deliberately NOT part of the
//! `ui::App` taffy tree and never register in the App focus ring.
//!
//! This test stands up a real `ui::App` around the contacts root and pins that
//! invariant down: the App collects zero focusables, so its Tab-gating layer
//! has no target and can never steal Tab (or the arrow keys) away from the
//! contacts form. If a refactor ever leaked the form's inputs into the App
//! tree, the App would start handling Tab itself and this would fail.

#[test]
fn contacts_root_owns_focus_and_registers_none_with_the_app() {
    let state = ui::State::new();
    let mut app = ui::App::new(state, contacts::app_root());
    app.init_layout();
    assert!(
        app.focusable_order.is_empty(),
        "the contacts root manages focus itself and must register no focusables \
         in the App ring (so Tab reaches the form, not the App); found {}",
        app.focusable_order.len(),
    );
}
