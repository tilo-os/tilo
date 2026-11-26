//! An empty state's action runs once from the pointer and from a reader,
//! through the same published action.

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
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

#[test]
fn an_empty_states_action_is_the_same_path_for_pointer_and_a_reader() {
    use automerge::ReadDoc as _;
    use std::cell::Cell;
    let invoked = Rc::new(Cell::new(0));
    let counted = invoked.clone();
    let empty = EmptyState::new("Nothing here")
        .with_action("Open")
        .with_action_id("empty.open")
        .on_open(move || counted.set(counted.get() + 1));
    let mut harness = Driver::for_root(tilo_ui::State::new(), empty, (600, 400), Theme::dark());
    let action = harness.root::<EmptyState>().action_rect();
    assert!(action.area() > 0.0, "the recovery action has a target");
    harness.click_at(action.center().x, action.center().y);
    assert_eq!(
        invoked.get(),
        1,
        "pointer activation invokes the published action"
    );
    harness
        .perform_named("empty.open", &tilo_ui_base::a11y::Action::Activate)
        .unwrap();
    assert_eq!(
        invoked.get(),
        2,
        "accessibility invokes the same published action"
    );
    assert!(
        harness
            .state()
            .doc
            .get(automerge::ROOT, "empty.open")
            .unwrap()
            .is_none(),
        "a view action never writes a click pulse into the resource"
    );
}

#[test]
fn a_handler_publishes_the_action_as_a_certified_bound_button() {
    let opened = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = opened.clone();
    let root = EmptyState::new("No places yet")
        .with_action("Add a place")
        .with_action_id("weather.empty.add")
        .on_open(move || seen.set(seen.get() + 1));
    let mut driver = Driver::new(tilo_ui::State::new(), Box::new(root), (480, 360));
    let audit = driver.native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
    driver
        .perform_named("weather.empty.add", &tilo_ui_base::a11y::Action::Activate)
        .expect("the action is addressable by its id");
    assert_eq!(opened.get(), 1, "the view handler ran once");
}
