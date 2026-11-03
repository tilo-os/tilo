//! What a screen reader gets from the notebook: each note in the sidebar a
//! named item with the menu a right click opens, and each block of the open
//! note a named item a reader lands on, with a checkbox of its own for a
//! checklist block.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::framework_support::{add_note, notes as notebook, open, surface};
use notes::NotesShell;
use ui::accesskit::{Action as AkAction, NodeId, Role, Toggled};
use ui::framework::testing::TestApp;

/// The exported node `app` publishes for the control named `name` with
/// `role`, as an assistive technology finds it.
fn named(app: &mut TestApp<NotesShell>, name: &str, role: Role) -> ui::accesskit::Node {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name) && node.role() == role)
        .unwrap_or_else(|| panic!("a {role:?} named {name:?}"))
        .1
}

fn id_of(app: &mut TestApp<NotesShell>, name: &str, role: Role) -> NodeId {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name) && node.role() == role)
        .unwrap_or_else(|| panic!("a {role:?} named {name:?}"))
        .0
}

/// Every note in the sidebar is a row a reader opens and right clicks, and
/// opening it is what brings the note's blocks into the tree.
#[test]
fn every_note_row_is_a_named_item_a_reader_opens_and_menus() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = notebook();
    let plan = add_note(&mut app, "Plan", "");
    let groceries_page = add_note(&mut app, "Groceries", "- [ ] Book the room\n");
    app.settle();
    assert_eq!(
        surface(&mut app, |notes| notes.selected_for_test()),
        Some(plan),
        "the first note is the one showing"
    );

    for title in ["Plan", "Groceries"] {
        let row = named(&mut app, title, Role::TreeItem);
        assert!(
            row.supports_action(AkAction::Click),
            "{title:?} cannot be opened"
        );
        assert!(
            row.supports_action(AkAction::ShowContextMenu),
            "{title:?} offers no menu"
        );
    }

    // The menu is the pointer's right-click menu, item by item.
    let groceries = id_of(&mut app, "Groceries", Role::TreeItem);
    app.harness()
        .perform_a11y_action(groceries, &ui::a11y::Action::ShowContextMenu)
        .expect("the note's menu opens");
    app.settle();
    for item in ["New sub-note", "Rename", "Duplicate", "Delete"] {
        named(&mut app, item, Role::MenuItem);
    }

    // Activating a row opens its note, as a press on it does, and its blocks
    // are what the tree then shows.
    app.harness()
        .perform_a11y_action(groceries, &ui::a11y::Action::Activate)
        .expect("the row opens the note");
    app.settle();
    assert_eq!(
        surface(&mut app, |notes| notes.selected_for_test()),
        Some(groceries_page),
        "the notebook shows the note the reader chose"
    );
    named(&mut app, "Book the room", Role::ListItem);

    let plan_row = id_of(&mut app, "Plan", Role::TreeItem);
    app.harness()
        .perform_a11y_action(plan_row, &ui::a11y::Action::Activate)
        .expect("the row opens the note");
    app.settle();
    assert_eq!(
        surface(&mut app, |notes| notes.selected_for_test()),
        Some(plan),
        "and the reader goes back the same way"
    );
}

/// A block of the open note reads as a named item, and a checklist block's
/// box is a control of its own: ticking it is the edit a press makes.
#[test]
fn every_block_is_a_named_item_and_a_checklist_box_ticks_it() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = notebook();
    let groceries = add_note(
        &mut app,
        "Groceries",
        "Buy what the week needs\n- [ ] Book the room\n",
    );
    open(&mut app, groceries);
    app.settle();

    // Each block reads as what it says.
    named(&mut app, "Buy what the week needs", Role::ListItem);
    let milk = named(&mut app, "Book the room", Role::ListItem);

    // Its checklist box is a checked control of its own.
    let nodes = app.harness().a11y_nodes();
    let boxes: Vec<_> = milk
        .children()
        .iter()
        .filter_map(|child| {
            nodes
                .iter()
                .find(|(id, node)| id == child && node.role() == Role::CheckBox)
                .cloned()
        })
        .collect();
    assert_eq!(boxes.len(), 1, "the checklist's box is its own control");
    assert_eq!(boxes[0].1.toggled(), Some(Toggled::False));
    assert_eq!(boxes[0].1.label(), Some("Book the room"));

    let box_ = id_of(&mut app, "Book the room", Role::CheckBox);
    app.harness()
        .perform_a11y_action(box_, &ui::a11y::Action::ToggleCheck)
        .expect("the box ticks");
    app.settle();
    assert_eq!(
        named(&mut app, "Book the room", Role::CheckBox).toggled(),
        Some(Toggled::True),
        "the box reads done"
    );
    let done = app.read_document(|doc| {
        notes::read::page_stored_blocks(doc, groceries)
            .into_iter()
            .any(|block| {
                matches!(
                    block.kind,
                    ui::widget::BlockKind::Checklist { done: true, .. }
                )
            })
    });
    assert!(done, "the note keeps the tick the reader made");

    // The row opens its block: the caret lands there, as a press on it does.
    let block = id_of(&mut app, "Book the room", Role::ListItem);
    app.harness()
        .perform_a11y_action(block, &ui::a11y::Action::Activate)
        .expect("the block opens");
    app.settle();
    assert_eq!(
        surface(&mut app, |notes| notes
            .main_panel_mut()
            .editor()
            .focused_index()),
        Some(1),
        "the reader's caret is in the block it opened"
    );
}
