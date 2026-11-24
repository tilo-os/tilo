//! What the keyboard does in a card grid: the arrows and the ends move the selection, Enter
//! opens the card it is on, and the edit key and Delete run the actions the app declares on it.
//! A reader is told which card the keyboard is on, and what the app made of each key.

use super::*;
use crate::change_marks::settle;

const GRID: &str = "so.tilo.lang.grid";

/// A grid of six objects, opened and at rest.
fn grid() -> Session {
    let mut session = Session::new(GRID, "ada");
    session.settle();
    session
}

/// Give the grid the keyboard, as Tab or an assistive technology does.
fn focus_grid(session: &mut Session) -> ui::accesskit::NodeId {
    let node = session
        .window
        .a11y()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::List
                && node.label().is_some_and(|label| label.trim() == "Items")
        })
        .map_or_else(|| panic!("the window shows the grid"), |(id, _)| id);
    session
        .window
        .a11y_action(node, &ui::a11y::Action::Focus);
    settle(session);
    node
}

/// The card a reader is told is selected.
fn selected(session: &mut Session) -> Option<String> {
    session
        .window
        .a11y()
        .into_iter()
        .find(|(_, node)| node.is_selected() == Some(true))
        .and_then(|(_, node)| node.label().map(str::to_owned))
}

/// Whether the window says `text` anywhere: the grid's own reports of what its keys did.
fn says(session: &mut Session, text: &str) -> bool {
    session
        .window
        .a11y()
        .into_iter()
        .any(|(_, node)| node.label().is_some_and(|label| label.trim() == text))
}

/// Press `key` and let the window settle.
fn press(session: &mut Session, key: ui::winit::keyboard::NamedKey) {
    session.window.press_key(key);
    settle(session);
}

#[test]
fn the_arrows_move_the_selection_and_enter_opens_the_card_it_is_on() {
    use ui::winit::keyboard::NamedKey;
    let mut session = grid();
    let grid = focus_grid(&mut session);
    // The grid took the keyboard and stands on its first card, which a reader is told is the
    // selected one.
    assert_eq!(selected(&mut session).as_deref(), Some("Alpha"));
    assert!(
        session
            .window
            .a11y()
            .into_iter()
            .any(|(id, _)| id == grid),
        "the grid is still the window's"
    );
    // The arrows move it a card at a time; the ends go to the first and the last.
    press(&mut session, NamedKey::ArrowRight);
    assert_eq!(selected(&mut session).as_deref(), Some("Bravo"));
    assert!(
        says(&mut session, "Selected Bravo"),
        "the app ran its select action on the card the keyboard moved to"
    );
    press(&mut session, NamedKey::End);
    assert_eq!(selected(&mut session).as_deref(), Some("Foxtrot"));
    press(&mut session, NamedKey::Home);
    assert_eq!(selected(&mut session).as_deref(), Some("Alpha"));
    press(&mut session, NamedKey::ArrowRight);
    assert_eq!(selected(&mut session).as_deref(), Some("Bravo"));
    // Enter opens the card the keyboard is on, through the app's own activate binding.
    press(&mut session, NamedKey::Enter);
    assert!(
        says(&mut session, "Opened Bravo"),
        "Enter ran the card's action on the selected item: {:?}",
        session
            .window
            .a11y()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_edit_key_and_delete_run_the_actions_the_app_declares_on_the_card() {
    use ui::winit::keyboard::NamedKey;
    let mut session = grid();
    focus_grid(&mut session);
    press(&mut session, NamedKey::ArrowRight);
    press(&mut session, NamedKey::F2);
    assert!(
        says(&mut session, "Edited Bravo"),
        "the edit key ran the app's edit action on the selected card"
    );
    press(&mut session, NamedKey::Delete);
    assert!(
        says(&mut session, "Deleted Bravo"),
        "Delete ran the app's destructive action on the selected card"
    );
    // The selection stays where the person put it through both.
    assert_eq!(selected(&mut session).as_deref(), Some("Bravo"));
}
