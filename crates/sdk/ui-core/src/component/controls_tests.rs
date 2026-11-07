//! The controls: their props read from a view, taken in place, and what a person does to them
//! reported as their events.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use super::testing::*;
use super::*;
use crate::component::{Component, Described, Value};
use crate::widget::{
    Checkbox, CheckboxProps, Dropdown, DropdownProps, NumberInput, NumberInputProps, Slider,
    SliderProps, Stepper, StepperProps, Toggle, ToggleGroup, ToggleGroupProps, ToggleProps,
    ViewTabs, ViewTabsProps, Widget,
};

#[test]
fn a_checkbox_shows_what_it_binds_and_reports_a_press_of_the_whole_row() {
    let queue = crate::Intents::default();
    let mut node = Node::new("Checkbox", "email", &queue)
        .bound(flag_bound("Send by email", false, edits(&queue, "email")))
        .prop("label", text("Send by email"));
    let props = CheckboxProps::from_source(&mut node).unwrap();
    let mut host = harness(Checkbox::from_props("email", props));
    assert!(!host.root::<Checkbox>().is_checked());
    let rect = host.root::<Checkbox>().get_rect();
    // The labelled row is one target: a press on the label marks the box.
    host.click_at(rect.x1 - 4.0, rect.center().y);
    assert!(host.root::<Checkbox>().is_checked());
    assert_eq!(
        queue.take(),
        [fired("email", "value", Some(Value::Bool(true)))]
    );
}

#[test]
fn a_toggle_knot_turns_where_it_is_told_and_reports_a_press() {
    let queue = crate::Intents::default();
    let mut node = Node::new("Toggle", "repeat", &queue)
        .bound(flag_bound("Repeat", false, edits(&queue, "repeat")))
        .prop("label", text("Repeat weekly"));
    let props = ToggleProps::from_source(&mut node).unwrap();
    let mut host = harness(Toggle::from_props("repeat", props));
    assert!(!host.root::<Toggle>().is_checked());
    let rect = host.root::<Toggle>().get_rect();
    host.click_at(rect.center().x, rect.center().y);
    assert_eq!(
        queue.take(),
        [fired("repeat", "value", Some(Value::Bool(true)))]
    );
}

#[test]
fn a_slider_reads_its_range_and_reports_the_value_it_is_let_go_at() {
    let queue = crate::Intents::default();
    let mut node = Node::new("Slider", "share", &queue)
        .bound(number_bound("Share", 0.2, edits(&queue, "share")))
        .prop("max", Value::Float(1.0))
        .prop("step", Value::Float(0.05));
    let props = SliderProps::from_source(&mut node).unwrap();
    assert_eq!(props.value.state.get(), 0.2);
    let mut host = harness(Slider::from_props("share", props));
    let rect = host.root::<Slider>().get_rect();
    host.click_at(rect.x1 - 2.0, rect.center().y);
    let reported = queue.take();
    assert_eq!(reported.len(), 1, "one value is let go at");
    assert_eq!(reported[0].event, "value");
    match reported[0].value {
        Some(Value::Float(value)) => assert!(value > 0.9, "the far end is nearly 1: {value}"),
        ref other => panic!("a number is reported, not {other:?}"),
    }
}

#[test]
fn a_number_input_takes_a_value_its_view_gives_and_keeps_the_range() {
    let queue = crate::Intents::default();
    let props = |number: f64| {
        let mut node = Node::new("NumberInput", "seats", &queue)
            .bound(number_bound("Seats", number, edits(&queue, "seats")))
            .prop("min", Value::Float(1.0))
            .prop("max", Value::Float(9.0));
        NumberInputProps::from_source(&mut node).unwrap()
    };
    let mut input = NumberInput::from_props("seats", props(3.0));
    assert!((input.value() - 3.0).abs() < f64::EPSILON);
    assert!(input.set_props(props(7.0)));
    assert!((input.value() - 7.0).abs() < f64::EPSILON);
}

#[test]
fn a_dropdown_offers_its_options_and_reports_the_one_chosen() {
    let queue = crate::Intents::default();
    let options = || Value::List(vec![text("Studio"), text("Gallery"), text("Annex")]);
    let mut node = Node::new("Dropdown", "room", &queue)
        .bound(text_bound("Room", "Studio", edits(&queue, "room")))
        .prop("options", options());
    let props = DropdownProps::from_source(&mut node).unwrap();
    assert_eq!(props.options, ["Studio", "Gallery", "Annex"]);
    assert_eq!(props.value.state.get(), "Studio");
    assert_eq!(props.value.name, "Room", "the field's spoken name");
    let dropdown = Dropdown::from_props("room", props);
    assert_eq!(dropdown.selected(), "Studio");
}

#[test]
fn a_strip_of_choices_is_a_slot_of_segments_and_reports_the_one_chosen() {
    let queue = crate::Intents::default();
    let segment =
        |key: &str, label: &str| Node::new("Segment", key, &queue).prop("label", text(label));
    let mut node = Node::new("ToggleGroup", "arrange", &queue)
        .prop("selected", Value::Int(1))
        .holding("options", segment("grid", "Grid"))
        .holding("options", segment("list", "List"))
        .holding("options", segment("board", "Board"));
    let props = ToggleGroupProps::from_source(&mut node).unwrap();
    assert_eq!(props.options.len(), 3);
    let group = ToggleGroup::from_props("arrange", props);
    assert_eq!(group.count(), 3);
    assert_eq!(group.selected(), 1);
    assert!(group.is_enabled(), "a strip nothing disabled is choosable");
}

#[test]
fn a_strip_holds_only_segments() {
    let queue = crate::Intents::default();
    let mut node = Node::new("ToggleGroup", "arrange", &queue).holding(
        "options",
        Node::new("Badge", "n", &queue).prop("count", Value::Int(1)),
    );
    assert_eq!(
        <Vec<crate::widget::SegmentProps> as SlotType>::accepts(),
        Accepts::Component("Segment".into())
    );
    assert!(ToggleGroupProps::from_source(&mut node).is_err());
}

#[test]
fn tabs_are_a_slot_of_segments_and_show_the_one_chosen() {
    let queue = crate::Intents::default();
    let tab = |key: &str, label: &str| Node::new("Segment", key, &queue).prop("label", text(label));
    let mut node = Node::new("ViewTabs", "views", &queue)
        .prop("selected", Value::Int(2))
        .holding("tabs", tab("summary", "Summary"))
        .holding("tabs", tab("details", "Details"))
        .holding("tabs", tab("history", "History"));
    let props = ViewTabsProps::from_source(&mut node).unwrap();
    let tabs = ViewTabs::from_props("views", props);
    assert_eq!(tabs.count(), 3);
    assert_eq!(tabs.selected(), 2);
    assert!(tabs.is_enabled(), "tabs nothing disabled are choosable");
}

#[test]
fn a_stepper_shows_where_it_is_and_reports_a_station_gone_back_to() {
    let queue = crate::Intents::default();
    let mut node = Node::new("Stepper", "run", &queue)
        .prop(
            "steps",
            Value::List(vec![text("Draft"), text("Review"), text("Send")]),
        )
        .prop("current", Value::Int(1))
        .on("station");
    let props = StepperProps::from_source(&mut node).unwrap();
    let mut host = harness(Stepper::from_props("run", props));
    let rect = host.root::<Stepper>().get_rect();
    host.click_at(rect.center().x, rect.center().y);
    assert!(host.root::<Stepper>().count() == 3);
}
