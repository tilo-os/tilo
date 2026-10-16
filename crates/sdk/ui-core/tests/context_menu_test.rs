use tilo_ui::widget::context_menu::{ContextMenu, MenuItem};

#[test]
fn context_menu_shows_and_invokes_action() {
    let mut m = ContextMenu::new();
    let invoked = std::sync::Arc::new(std::sync::Mutex::new(false));
    let i = invoked.clone();
    m.push(MenuItem::action("Open", move || *i.lock().unwrap() = true));
    m.push(MenuItem::separator());
    m.push(MenuItem::action("Delete", || {}));
    assert_eq!(m.items().len(), 3);
    m.open_at([10.0, 20.0]);
    assert!(m.is_open());
    assert_eq!(m.position(), Some([10.0, 20.0]));
    m.click_item(0);
    assert!(*invoked.lock().unwrap());
    assert!(!m.is_open()); // closes after action
}
