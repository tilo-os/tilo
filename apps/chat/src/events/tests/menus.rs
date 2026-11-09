use super::*;

/// The menu row says which it will do: "Delete message…" while it asks
/// first, "Delete message" while Shift is held and it goes at once.
#[test]
fn the_menus_delete_row_reads_as_it_will_act() {
    let (mut app, mut doc) = app_with_doc();
    let id = send_own(&mut app, &mut doc, "mine");
    app.open_message_menu(id, Point::new(120.0, 120.0));
    let row = |app: &ChatApp| {
        let menu = app.menu.as_ref().expect("the menu is open");
        let index = menu
            .items
            .iter()
            .position(|action| *action == MenuAction::DeleteMessage)
            .expect("Delete is offered");
        menu.widget.items()[index]
            .shown_label()
            .map(std::borrow::Cow::into_owned)
    };
    ui::widget::note_shift(false);
    assert_eq!(row(&app).as_deref(), Some("Delete message\u{2026}"));
    ui::widget::note_shift(true);
    assert_eq!(row(&app).as_deref(), Some("Delete message"));
    ui::widget::note_shift(false);
}

/// The row that sends what the menu took hands the typing to the box in its
/// place: Chat keeps the menu while that box stands, so it paints and hears
/// the keyboard.
#[test]
fn sending_a_typed_sentence_leaves_the_box_with_chat() {
    ui::shared_selection::set_send_host_available(true);
    let (mut app, mut doc) = app_with_doc();
    let id = send_own(&mut app, &mut doc, "check the plan");
    ui::widget::context_menu::note_input(&mouse(ElementState::Pressed, MouseButton::Right));
    app.open_message_menu(id, Point::new(120.0, 120.0));
    type_str(&mut app, &mut doc, "please");
    assert_eq!(
        app.menu.as_ref().map(|menu| menu.widget.query()),
        Some("please"),
        "the menu takes the typing"
    );
    feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
    let menu = app
        .menu
        .as_ref()
        .expect("Chat keeps the menu while its box stands");
    assert!(!menu.widget.is_open(), "the rows went with the menu");
    assert!(menu.widget.send_to_alone_open(), "the box took their place");
    assert_eq!(menu.widget.send_to().text(), "please");
    ui::shared_selection::set_send_host_available(false);
}
