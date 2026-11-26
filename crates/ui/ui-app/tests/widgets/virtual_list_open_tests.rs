use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::internals::virtual_list::test_support::*;
use tilo_ui::widget::internals::virtual_list::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;
#[test]
fn an_item_open_in_its_row_hands_a_reader_the_fields_that_edit_it() {
    use accesskit::{Action as AkAction, Role as AkRole};
    use tilo_ui_base::a11y::Action;

    let saved = Rc::new(Cell::new(0));
    let done = tilo_ui_base::LocalState::new(false);
    let mut harness = Driver::for_root(
        tilo_ui::State::new(),
        documented_list(Rc::clone(&saved), done.clone()),
        (400, 600),
        tilo_ui_base::theme::Theme::dark(),
    );
    harness.root_mut::<VirtualList<Opens>>().set_open(Some(0));
    harness.relayout();

    let nodes = harness.a11y_nodes();
    let (row_id, row) = named(&nodes, AkRole::ListItem, "Ring the plumber");
    let row_bounds = row.bounds().expect("the row stands where it is drawn");
    let controls = inside(&nodes, row_id);
    assert_eq!(
        controls
            .iter()
            .filter(|(_, node)| matches!(node.role(), AkRole::ListItem | AkRole::TreeItem))
            .count(),
        0,
        "the open row is one row, not one inside another"
    );
    let (_, title) = named(&controls, AkRole::TextInput, "Title");
    assert_eq!(
        title.value(),
        Some("Ring the plumber"),
        "the field reads the title it edits"
    );
    let (_, page) = named(&controls, AkRole::MultilineTextInput, "Notes");
    assert_eq!(page.value(), Some("The tap drips"));
    let (checkbox, box_node) = named(&controls, AkRole::CheckBox, "Done");
    assert_eq!(box_node.toggled(), Some(accesskit::Toggled::False));
    assert!(
        box_node.supports_action(AkAction::Click),
        "the box can be ticked from a reader"
    );
    named(&controls, AkRole::Button, "Save");
    for (_, control) in &controls {
        let bounds = control
            .bounds()
            .unwrap_or_else(|| panic!("{:?} is drawn nowhere", control.role()));
        assert!(
            within(row_bounds, bounds),
            "{:?} stands outside its row: {bounds:?} not in {row_bounds:?}",
            control.role()
        );
    }

    let stops: Vec<(tilo_ui_base::a11y::Role, Option<String>)> = harness
        .tab_through()
        .into_iter()
        .filter(|(role, _)| {
            matches!(
                role,
                tilo_ui_base::a11y::Role::TextInput
                    | tilo_ui_base::a11y::Role::MultilineTextInput
                    | tilo_ui_base::a11y::Role::Checkbox
                    | tilo_ui_base::a11y::Role::Button
            )
        })
        .collect();
    let named_stops: Vec<Option<&str>> = stops
        .iter()
        .map(|(_, name)| name.as_deref())
        .filter(|name| matches!(*name, Some("Title" | "Notes" | "Done" | "Save")))
        .collect();
    assert_eq!(
        named_stops,
        vec![Some("Title"), Some("Notes"), Some("Done"), Some("Save")],
        "the keyboard reaches what the row holds, in reading order"
    );

    harness
        .perform_a11y_action(checkbox, &Action::Activate)
        .expect("the box is operated");
    let nodes = harness.a11y_nodes();
    let (row_id, _) = named(&nodes, AkRole::ListItem, "Ring the plumber");
    let controls = inside(&nodes, row_id);
    assert_eq!(
        named(&controls, AkRole::CheckBox, "Done").1.toggled(),
        Some(accesskit::Toggled::True),
        "the tick reaches the box"
    );
    assert!(done.with(|checked| *checked), "the box's value is ticked");

    let (button, _) = named(&nodes, AkRole::Button, "Save");
    harness
        .perform_a11y_action(button, &Action::Activate)
        .expect("the button is pressed");
    assert_eq!(saved.get(), 1, "the press reaches the button");

    let nodes = harness.a11y_nodes();
    let (page, _) = named(&nodes, AkRole::MultilineTextInput, "Notes");
    harness
        .perform_a11y_action(page, &Action::SetValue("The tap runs".into()))
        .expect("the page is written in");
    let nodes = harness.a11y_nodes();
    assert_eq!(
        named(&nodes, AkRole::MultilineTextInput, "Notes").1.value(),
        Some("The tap runs"),
        "what the reader writes stands in the page"
    );
}
