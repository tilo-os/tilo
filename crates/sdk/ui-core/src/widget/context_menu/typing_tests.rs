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

/// A menu the context-menu gesture opened, whose rows record what ran.
fn context_menu(ran: &Rc<RefCell<Vec<&'static str>>>) -> ContextMenu {
    let mut menu = ContextMenu::new();
    for label in ["Open", "Rename", "Copy link", "Move to Trash"] {
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
    crate::shared_selection::set_agent_host_available(false);
    let ran = Rc::new(RefCell::new(Vec::new()));
    let mut menu = context_menu(&ran);
    menu.open_anchored(
        Rect::new(40.0, 40.0, 40.0, 40.0),
        Rect::new(0.0, 0.0, 800.0, 600.0),
    );
    assert!(menu.takes_typing());
    assert_eq!(
        menu.shown_rows(),
        &[0, 1, 2, 3, 4],
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
fn a_sentence_no_row_answers_is_asked_and_never_reported_as_a_row() {
    crate::shared_selection::set_agent_host_available(true);
    let _ = crate::shared_selection::take_agent_requests();
    let ran = Rc::new(RefCell::new(Vec::new()));
    let mut menu = context_menu(&ran);
    let item = crate::shared_selection::SharedSelection::of_text("Report.pdf")
        .at(tilo_actions::SelectionPlace::item("file-9"));
    menu.set_about(Some(item.clone()));
    menu.open_at([120.0, 80.0]);
    crate::shared_selection::set_agent_host_available(false);
    assert_eq!(
        menu.shown_rows(),
        &[0, 1, 2, 3, 4],
        "no row to ask until something is typed"
    );
    type_into(&mut menu, "rename");
    assert_eq!(
        menu.shown_rows(),
        &[1, 5],
        "a match, then asking the sentence"
    );
    assert_eq!(menu.hovered(), Some(1), "Enter would rename");
    menu.set_query("summarise this for Ana");
    assert_eq!(menu.shown_rows(), &[5], "only asking");
    assert_eq!(menu.items()[5].label(), Some("Ask: summarise this for Ana"));
    dispatch(&mut menu, &named_key(NamedKey::Enter));
    assert!(ran.borrow().is_empty(), "no row ran");
    assert_eq!(menu.take_selection(), None);
    assert!(!menu.is_open());
    let asked = crate::shared_selection::take_agent_requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].about, Some(item));
    assert_eq!(asked[0].ask.as_deref(), Some("summarise this for Ana"));
    assert_eq!(asked[0].at, Some(Point::new(120.0, 80.0)));
}

#[test]
fn an_owner_that_answers_typing_takes_the_sentence_itself() {
    crate::shared_selection::set_agent_host_available(false);
    // Opened with the keyboard, not the context-menu gesture: an owner that
    // answers typing still gets a field.
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
fn a_menu_a_button_opened_takes_no_typing() {
    crate::shared_selection::set_agent_host_available(true);
    note_input(&crate::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Left,
    ));
    let mut menu = ContextMenu::with_items(vec![MenuItem::action("Sort by name", || {})]);
    menu.open_at([10.0, 10.0]);
    crate::shared_selection::set_agent_host_available(false);
    assert!(!menu.takes_typing());
    assert_eq!(menu.items().len(), 1, "and adds no row");
    assert!(
        !dispatch(&mut menu, &char_key("s")),
        "a typed key is not its"
    );
}
