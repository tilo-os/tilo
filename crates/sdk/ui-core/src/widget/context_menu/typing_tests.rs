//! A context menu takes typing: a query narrows its rows with the shared
//! ranking, Enter runs the best match, and a sentence none answers is asked.

use std::cell::RefCell;
use std::rc::Rc;

use winit::keyboard::NamedKey;

use super::*;
use crate::synthetic_input::{char_key, named_key};
use crate::testing::dispatch;

fn type_into(menu: &mut ContextMenu, text: &str) {
    for ch in text.chars() {
        dispatch(menu, &char_key(&ch.to_string()));
    }
}

/// The commands of a file's menu, as many as a menu needs to take typing.
const COMMANDS: [&str; 13] = [
    "Open",
    "Rename",
    "Copy link",
    "Move to Trash",
    "Duplicate",
    "Pin",
    "Share",
    "Archive",
    "Compress",
    "Get info",
    "Move to folder",
    "Send to Ana",
    "New folder",
];

/// A menu the context-menu gesture opened, whose rows record what ran.
fn context_menu(ran: &Rc<RefCell<Vec<&'static str>>>) -> ContextMenu {
    let mut menu = ContextMenu::new();
    for label in COMMANDS {
        let ran = Rc::clone(ran);
        menu.push(MenuItem::action(label, move || {
            ran.borrow_mut().push(label)
        }));
        if label == "Rename" {
            menu.push(MenuItem::separator());
        }
    }
    note_input(&crate::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Right,
    ));
    menu
}

#[test]
fn typing_narrows_the_rows_best_first_and_enter_runs_the_best_match() {
    crate::shared_selection::set_send_host_available(false);
    let ran = Rc::new(RefCell::new(Vec::new()));
    let mut menu = context_menu(&ran);
    menu.open_anchored(
        Rect::new(40.0, 40.0, 40.0, 40.0),
        Rect::new(0.0, 0.0, 800.0, 600.0),
    );
    assert!(menu.takes_typing());
    assert_eq!(
        menu.shown_rows(),
        &(0..=COMMANDS.len()).collect::<Vec<_>>(),
        "nothing typed: every row"
    );
    let top = menu.get_rect().y0;
    type_into(&mut menu, "li");
    assert_eq!(menu.query(), "li");
    // "Copy link": a word starts with it. "Move to Trash" does not match.
    assert_eq!(menu.shown_rows(), &[3]);
    dispatch(&mut menu, &named_key(NamedKey::Backspace));
    type_into(&mut menu, "n");
    assert_eq!(menu.query(), "ln");
    assert!(menu.shown_rows().is_empty(), "nothing answers \"ln\"");
    menu.set_query("link");
    assert_eq!(menu.shown_rows(), &[3]);
    assert_eq!(menu.hovered(), Some(3), "the best match is the keyboard's");
    assert_eq!(
        menu.get_rect().y0,
        top,
        "the field stays where it opened while the rows narrow"
    );
    assert!(
        menu.item_rect(1).is_none(),
        "a row the query hides is not hit"
    );
    menu.set_query("o");
    assert_eq!(menu.shown_rows().first(), Some(&0), "Open starts with it");
    let shown = menu.shown_rows().to_vec();
    dispatch(&mut menu, &named_key(NamedKey::ArrowDown));
    let next = *shown.get(1).unwrap_or(&shown[0]);
    assert_eq!(menu.hovered(), Some(next), "arrows walk the rows as shown");
    menu.set_query("link");
    dispatch(&mut menu, &named_key(NamedKey::Enter));
    assert_eq!(&*ran.borrow(), &["Copy link"]);
    assert_eq!(menu.take_selection(), Some(3), "the owner's own row index");
    assert!(!menu.is_open());
}

#[test]
fn a_sentence_no_row_answers_is_sent_from_the_box_and_never_reported_as_a_row() {
    crate::shared_selection::set_send_host_available(true);
    let _ = crate::shared_selection::take_send_to();
    let ran = Rc::new(RefCell::new(Vec::new()));
    let mut menu = context_menu(&ran);
    let item = crate::shared_selection::SharedSelection::of_text("Report.pdf")
        .at(tilo_actions::SelectionPlace::item("file-9"));
    menu.set_about(Some(item.clone()));
    menu.open_at([120.0, 80.0]);
    assert_eq!(
        menu.shown_rows(),
        &(0..=COMMANDS.len()).collect::<Vec<_>>(),
        "no row to send with until something is typed"
    );
    type_into(&mut menu, "rename");
    let send_at = menu.items().len() - 1;
    assert_eq!(
        menu.shown_rows(),
        &[1, send_at],
        "a match, then the row that sends the sentence"
    );
    crate::shared_selection::set_send_host_available(false);
    assert_eq!(menu.hovered(), Some(1), "Enter would rename");
    menu.set_query("summarise this for Ana");
    assert_eq!(menu.shown_rows(), &[send_at], "only sending it");
    assert_eq!(
        menu.items()[send_at].label(),
        Some(crate::i18n!("menu-ask-typed", "text" => "summarise this for Ana").as_str())
    );
    dispatch(&mut menu, &named_key(NamedKey::Enter));
    assert!(ran.borrow().is_empty(), "no row ran");
    assert_eq!(menu.take_selection(), None);
    assert!(
        !menu.is_open() && !menu.is_drawn(),
        "the box takes the menu's place: one field takes the typing, never two"
    );
    assert!(
        menu.send_to_alone_open(),
        "the box stands on its own, with the sentence it sends"
    );
    assert!(
        menu.is_showing(),
        "a host keeping it while it shows keeps the box past the menu it closed"
    );
    let box_ = menu.send_to();
    assert_eq!(box_.text(), "summarise this for Ana");
    assert_eq!(box_.subjects(), std::slice::from_ref(&item));
    assert!(
        box_.is_focused(),
        "and it is where the keyboard types, with no menu field to type into"
    );
    assert!(
        box_.rect().x0 >= menu.get_rect().x0,
        "the box arrives where the menu stood, {} against {}",
        box_.rect(),
        menu.get_rect()
    );
    // What a screen reader reads is one surface too: the menu's field is gone
    // with the menu, and of the box's fields exactly one holds the typing.
    fn fields(node: &crate::a11y::AccessNode, out: &mut Vec<crate::a11y::AccessState>) {
        for child in &node.children {
            if child.role == crate::a11y::Role::TextInput && !child.state.hidden {
                out.push(child.state);
            }
            fields(child, out);
        }
    }
    let mut read = Vec::new();
    fields(
        &menu.accessibility().expect("the menu's place is read"),
        &mut read,
    );
    assert_eq!(read.len(), 2, "the box's own fields, and no menu field");
    assert_eq!(
        read.iter().filter(|state| state.focused).count(),
        1,
        "one field takes the typing"
    );
}

#[test]
fn an_owner_that_answers_typing_takes_the_sentence_itself() {
    crate::shared_selection::set_send_host_available(false);
    // Opened with the keyboard, not the context-menu gesture: an owner that
    // answers typing still gets a field, however short the menu.
    note_input(&crate::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Left,
    ));
    let mut menu = ContextMenu::with_items(vec![MenuItem::selection("Close window")]);
    menu.set_owner_asks(true);
    menu.open_anchored(
        Rect::new(10.0, 10.0, 10.0, 10.0),
        Rect::new(0.0, 0.0, 800.0, 600.0),
    );
    assert!(menu.takes_typing());
    type_into(&mut menu, "tidy these tabs");
    dispatch(&mut menu, &named_key(NamedKey::Enter));
    assert_eq!(menu.take_ask().as_deref(), Some("tidy these tabs"));
    assert_eq!(menu.take_ask(), None, "taken once");
    assert_eq!(menu.take_selection(), None);
    assert!(!menu.is_open());
}

#[test]
fn a_menu_of_a_few_commands_opens_as_a_plain_list() {
    crate::shared_selection::set_send_host_available(false);
    note_input(&crate::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Right,
    ));
    let mut menu = ContextMenu::with_items(vec![
        MenuItem::action("Complete", || {}),
        MenuItem::action("Duplicate", || {}),
        MenuItem::action("Delete", || {}),
    ]);
    menu.open_anchored(
        Rect::new(40.0, 40.0, 40.0, 40.0),
        Rect::new(0.0, 0.0, 800.0, 600.0),
    );
    assert!(
        !menu.takes_typing(),
        "three commands are read at a glance: no field over them"
    );
    assert_eq!(menu.items().len(), 3, "and the menu adds no row");
    assert_eq!(menu.shown_rows(), &[0, 1, 2], "every command is shown");
}

#[test]
fn a_menu_a_button_opened_takes_no_typing() {
    crate::shared_selection::set_send_host_available(true);
    note_input(&crate::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Left,
    ));
    let mut menu = ContextMenu::with_items(vec![MenuItem::action("Sort by name", || {})]);
    menu.open_at([10.0, 10.0]);
    crate::shared_selection::set_send_host_available(false);
    assert!(!menu.takes_typing());
    assert_eq!(menu.items().len(), 1, "and adds no row");
    assert!(
        !dispatch(&mut menu, &char_key("s")),
        "a typed key is not its"
    );
}
