//! A finger on a text field inside a form, through the window's touch
//! emulation: what a tap, a swipe and a drag across the text each do.
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
use tilo_ui::widget::internals::text_input::*;
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

/// A form taller than its window: a field 100 px down, then the rest.
fn touch_form(field: TextInput) -> tilo_ui::app::driver::Driver {
    use tilo_ui::widget::{Column, Container, Scrollable, Text};
    let form = Scrollable::new_vertical()
        .with_height(300.0)
        .with_child(Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(0.0)
                .with_intrinsic_height()
                .push(Box::new(
                    Container::new().with_no_padding().with_size(100.0, 100.0),
                ))
                .push(Box::new(field.with_id("name")))
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_size(100.0, 2_000.0)
                        .with_child(Box::new(Text::new("Rest of the form").with_id("rest"))),
                )),
        ));
    let mut frame = tilo_ui::app::driver::Driver::new_with_theme(
        tilo_ui::State::new(),
        Box::new(form),
        (400, 300),
        Theme::dark().with_reduce_motion(true),
    );
    frame.set_interaction_environment(tilo_ui_base::InteractionEnvironment::TOUCH);
    frame
}

#[test]
fn a_finger_scrolls_a_form_up_or_down_and_selects_across_a_field() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let name = tilo_ui_base::LocalState::new("Ada Lovelace".to_owned());
    let mut frame = touch_form(TextInput::from_state(&name));
    let field = frame.targets()["name"];
    let rest = frame.targets()["rest"].y0;
    let start = field.center();

    frame.swipe_at((start.x, start.y), (start.x, start.y - 90.0));
    let moved = rest - frame.targets()["rest"].y0;
    assert!(
        (moved - 90.0).abs() < 1.0,
        "a swipe up that begins on a field moves the form with the finger: {moved}"
    );
    assert!(
        frame.ime_window().is_none(),
        "the field a swipe passes over takes no focus, so no keyboard comes up"
    );
    assert_eq!(
        frame.with_widget_mut::<TextInput, _>(|field| field.selection_range_public()),
        Some(None),
        "and selects nothing"
    );

    let field = frame.targets()["name"];
    let rest = frame.targets()["rest"].y0;
    let from = (field.x0 + 4.0, field.center().y);
    frame.swipe_at(from, (from.0 + 80.0, from.1 - 6.0));
    assert!(
        frame
            .with_widget_mut::<TextInput, _>(|field| field.selection_range_public())
            .flatten()
            .is_some_and(|(start, end)| start != end),
        "a drag across the text selects it"
    );
    assert!(
        frame.ime_window().is_some(),
        "the field it selects in has focus"
    );
    assert_eq!(
        frame.targets()["rest"].y0,
        rest,
        "the form stays still under a selection, whichever way the finger wavers"
    );
}

/// Where byte `at` of the field's text sits, in the window.
fn x_of(frame: &mut tilo_ui::app::driver::Driver, at: usize) -> f64 {
    frame
        .with_widget_mut::<TextInput, _>(|field| {
            field.test_rect().x0 + 10.0 + field.char_x_positions()[at]
        })
        .expect("the field")
}

#[test]
fn a_finger_selects_what_it_crosses_and_lets_go_where_it_lifts() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let name = tilo_ui_base::LocalState::new("Ada Lovelace".to_owned());
    let mut frame = touch_form(TextInput::from_state(&name));
    let y = frame.targets()["name"].center().y;

    let (from, to) = (x_of(&mut frame, 1), x_of(&mut frame, 5));
    frame.swipe_at((from, y), (to, y));

    assert_eq!(
        frame.with_widget_mut::<TextInput, _>(|field| field.selection_range_public()),
        Some(Some((1, 5))),
        "the selection runs from where the finger came down to where it lifted"
    );
    assert_eq!(
        frame.with_widget_mut::<TextInput, _>(|field| field.test_is_dragging()),
        Some(false),
        "the lift ends the selection gesture"
    );
}

#[test]
fn a_second_finger_ends_a_selection_without_taking_it_over() {
    use winit::event::TouchPhase;
    let _settled = tilo_ui_base::motion::testing::settled();
    let name = tilo_ui_base::LocalState::new("Ada Lovelace".to_owned());
    let mut frame = touch_form(TextInput::from_state(&name));
    let y = frame.targets()["name"].center().y;
    let (from, to) = (x_of(&mut frame, 1), x_of(&mut frame, 5));

    frame.touch(0, TouchPhase::Started, from, y);
    frame.touch(0, TouchPhase::Moved, from.midpoint(to), y);
    frame.touch(0, TouchPhase::Moved, to, y);
    let selected = frame
        .with_widget_mut::<TextInput, _>(|field| field.selection_range_public())
        .flatten();
    assert_eq!(selected, Some((1, 5)), "the first finger selects");

    let second = x_of(&mut frame, 9);
    frame.touch(1, TouchPhase::Started, second, y);
    assert_eq!(
        frame.with_widget_mut::<TextInput, _>(|field| field.test_is_dragging()),
        Some(false),
        "a second finger ends the selection where the first one is"
    );
    frame.touch(1, TouchPhase::Ended, second, y);
    frame.touch(0, TouchPhase::Cancelled, to, y);

    assert_eq!(
        frame
            .with_widget_mut::<TextInput, _>(|field| field.selection_range_public())
            .flatten(),
        selected,
        "neither finger moves the selection or the caret after that"
    );
    assert_eq!(
        frame.with_widget_mut::<TextInput, _>(|field| field.value().to_owned()),
        Some("Ada Lovelace".to_owned()),
        "and the text is as it was"
    );
}

#[test]
fn a_tap_on_a_fields_clear_button_clears_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let name = tilo_ui_base::LocalState::new("Ada Lovelace".to_owned());
    let mut frame = touch_form(TextInput::from_state(&name).with_clear_button());
    let clear = frame
        .with_widget_mut::<TextInput, _>(|field| {
            Widget::synthetic_accessibility_rect(field, &[0]).expect("the clear button is shown")
        })
        .expect("the field");

    frame.tap_at(clear.center().x, clear.center().y);

    assert_eq!(
        frame.with_widget_mut::<TextInput, _>(|field| field.value().to_owned()),
        Some(String::new()),
        "a finger presses the clear button as a click does"
    );
}

#[test]
fn a_tap_on_a_secret_fields_show_control_shows_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let secret = tilo_ui_base::LocalState::new("hunter2".to_owned());
    let mut frame = touch_form(TextInput::from_state(&secret).with_secret(true));
    let reveal = frame
        .with_widget_mut::<TextInput, _>(|field| field.reveal_control_rect())
        .flatten()
        .expect("the show control is shown");

    frame.tap_at(reveal.center().x, reveal.center().y);

    assert_eq!(
        frame.with_widget_mut::<TextInput, _>(|field| field.is_revealed()),
        Some(true),
        "a finger shows the password as a click does"
    );
    assert!(
        frame.ime_window().is_none(),
        "the field keeps out of focus, so no keyboard comes up"
    );

    frame.tap_at(reveal.center().x, reveal.center().y);
    assert_eq!(
        frame.with_widget_mut::<TextInput, _>(|field| field.is_revealed()),
        Some(false),
        "a second tap hides it again"
    );
}
