use accesskit::Role as AkRole;
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
use tilo_ui_base::a11y::Action;
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
/// A transcript's way back is a control, not a picture of one: a reader, an agent and a keyboard
/// find it by the words it says, where the pill is drawn, and taking it goes to the
/// first row the person has not seen.
#[test]
fn the_way_back_is_a_control_a_reader_or_an_agent_can_find_and_take() {
    use accesskit::Role as AkRole;
    use tilo_ui::widget::ListRow;
    use tilo_ui::widget::conversation::Jump;
    use tilo_ui_base::a11y::Action;

    fn transcript(unread: &str) -> VirtualList<VecListModel<u32, u32>> {
        let mut list = VirtualList::new(
            VecListModel::new((0..300_u32).collect(), |item| *item).expect("the rows"),
            40.0,
            |_, _, key| Box::new(ListRow::new(key.to_string(), format!("Item {key}"))) as Element,
        )
        .with_viewport_height(400.0)
        .following_end()
        .unread_from(Some(unread.to_owned()));
        list.set_key_presenting(|id: &str| id.parse::<u32>().ok());
        list
    }
    let words = Jump::Unread.label();

    // A screen reader reads the way back where the pill stands, and takes it.
    let mut reading = Driver::for_root(
        tilo_ui::State::new(),
        transcript("250"),
        (320, 568),
        Theme::dark(),
    );
    reading.relayout();
    let (pill, node) = named(&reading.a11y_nodes(), AkRole::Button, &words);
    let bounds = node.bounds().expect("the pill stands where it is drawn");
    assert!(
        bounds.x0 >= 0.0 && bounds.x1 <= 320.0 && bounds.y0 > 0.0 && bounds.y1 <= 568.0,
        "the pill stands inside a window 320 px wide: {bounds:?}"
    );
    assert!(
        bounds.x1 - bounds.x0 > 80.0,
        "its words are whole at 320 px: {bounds:?}"
    );
    reading.relayout();
    assert_eq!(
        reading.a11y_node_named(&words),
        Some(pill),
        "the pill is one control across frames, not a new one each time"
    );
    reading
        .perform_a11y_action(pill, &Action::Activate)
        .expect("an agent presses the way back");
    reading.relayout();
    assert!(
        reading.a11y_node_named(&words).is_none(),
        "taken, the way back offers nothing more"
    );
    let (_, row) = named(&reading.a11y_nodes(), AkRole::ListItem, "Item 250");
    let shown = row.bounds().expect("the first unseen row is drawn");
    assert!(
        shown.y0 >= 0.0 && shown.y1 <= 568.0,
        "the way back went to the first unseen row: {shown:?}"
    );

    // The keyboard reaches it the same way: focus, then Space.
    let mut typing = Driver::for_root(
        tilo_ui::State::new(),
        transcript("250"),
        (320, 568),
        Theme::dark(),
    );
    typing.relayout();
    let (pill, _) = named(&typing.a11y_nodes(), AkRole::Button, &words);
    typing
        .perform_a11y_action(pill, &Action::Focus)
        .expect("the way back takes focus");
    typing.key("Space");
    typing.relayout();
    let (_, row) = named(&typing.a11y_nodes(), AkRole::ListItem, "Item 250");
    assert!(
        row.bounds().is_some_and(|at| at.y0 >= 0.0),
        "Space on the focused way back goes to the first unseen row"
    );

    // The node stands exactly where the pointer's press takes the way back.
    let mut pointing = Driver::for_root(
        tilo_ui::State::new(),
        transcript("250"),
        (320, 568),
        Theme::dark(),
    );
    pointing.relayout();
    let (_, node) = named(&pointing.a11y_nodes(), AkRole::Button, &words);
    let bounds = node.bounds().expect("the pill stands where it is drawn");
    pointing.click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    pointing.relayout();
    let (_, row) = named(&pointing.a11y_nodes(), AkRole::ListItem, "Item 250");
    assert!(
        row.bounds().is_some_and(|at| at.y0 >= 0.0),
        "the node is where the pill is pressed"
    );
}

#[test]
fn a_closed_rows_checkbox_is_one_control_under_one_row() {
    let toggled = Rc::new(Cell::new(None::<bool>));
    let heard = Rc::clone(&toggled);
    let model = VecListModel::new(vec![7_u64], |key| *key).unwrap();
    let list = VirtualList::new(model, 36.0, move |_, _, key| {
        Box::new(
            ListRow::new(key.to_string(), "Buy oat milk").checkbox(false, {
                let heard = Rc::clone(&heard);
                move |on| heard.set(Some(on))
            }),
        ) as Element
    })
    .with_viewport_height(200.0);
    let mut harness = Driver::for_root(tilo_ui::State::new(), list, (300, 400), Theme::dark());

    let nodes = harness.a11y_nodes();
    let (row_id, row) = named(&nodes, AkRole::ListItem, "Buy oat milk");
    let controls = inside(&nodes, row_id);
    assert!(
        controls
            .iter()
            .all(|(_, node)| !matches!(node.role(), AkRole::ListItem | AkRole::TreeItem)),
        "the row is one node, not a row inside a row"
    );
    let (checkbox, box_node) = controls
        .iter()
        .find(|(_, node)| node.role() == AkRole::CheckBox)
        .cloned()
        .expect("the row hands over its box");
    assert_eq!(box_node.toggled(), Some(accesskit::Toggled::False));
    assert!(row.bounds().is_some());

    harness
        .perform_a11y_action(checkbox, &Action::Activate)
        .expect("the box is operated");
    assert_eq!(toggled.get(), Some(true), "the tick reaches the row's box");
}

#[test]
fn a_closed_rows_own_controls_are_where_the_row_is() {
    use tilo_ui::widget::{Checkbox, Column, Text};

    let done = tilo_ui_base::LocalState::new(false);
    let model = VecListModel::new(vec![7_u64], |key| *key).unwrap();
    let state = done.clone();
    let list = VirtualList::new(model, 40.0, move |_, _, _| {
        Box::new(
            Column::new()
                .push(Box::new(Text::new("Buy oat milk")))
                .push(Box::new(
                    Checkbox::from_state(&state).with_accessible_name("Done"),
                )),
        ) as Element
    })
    .with_viewport_height(200.0);
    let mut harness = Driver::for_root(tilo_ui::State::new(), list, (300, 400), Theme::dark());

    let nodes = harness.a11y_nodes();
    let (row_id, row) = named(&nodes, AkRole::ListItem, "Buy oat milk");
    let row_bounds = row.bounds().expect("the row stands where it is drawn");
    let controls = inside(&nodes, row_id);
    let (checkbox, _) = named(&controls, AkRole::CheckBox, "Done");
    let bounds = exported(&nodes, checkbox)
        .bounds()
        .expect("the box stands where it is drawn");
    assert!(
        within(row_bounds, bounds),
        "the box stands inside its row: {bounds:?} not in {row_bounds:?}"
    );

    harness
        .perform_a11y_action(checkbox, &Action::Activate)
        .expect("the box is operated");
    assert!(done.with(|checked| *checked), "the tick reaches the box");
}
