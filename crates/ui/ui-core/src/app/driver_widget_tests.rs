//! What a widget's tests read of the app through the driver: the page its frame composes, the
//! controls and accessibility names it exposes, the frames its layout skipped, the work a retained
//! frame still owes and input handed to the root under a modifier state of its own.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use std::cell::Cell;
use std::rc::Rc;

use winit::event::{ElementState, Modifiers};
use winit::keyboard::{Key, ModifiersState};

use crate::app::driver::Driver;
use crate::headless::ControlInput;
use crate::platform::{Platform, PlatformConventions};
use crate::state::State;
use crate::widget::{Button, Column, Labelled, NavShell, Text, TextInput, Widget};

/// A shell with a sidebar and content, as an app shows one.
fn shell() -> NavShell {
    NavShell::new()
        .sidebar(Box::new(Text::new("Mailboxes")))
        .overview(Box::new(Text::new("Overview")))
        .content(Box::new(Text::new("Design review")))
        .sidebar_header("Mailboxes", Vec::new())
        .overview_header("Inbox", Vec::new())
        .content_header("Design review", Vec::new())
}

#[test]
fn the_page_composes_the_app_the_frame_laid_out() {
    let driver = Driver::for_root(
        State::new(),
        Text::new("Hello"),
        (400, 300),
        crate::Theme::dark(),
    );
    let composition = driver.composition();
    assert_eq!(composition.viewport, [400, 300]);
    assert_eq!(composition.root.widget, "Text");
    let [x, y, width, height] = composition.root.rect;
    assert_eq!(
        (x, y),
        (0.0, 0.0),
        "the root stands at the surface's origin"
    );
    assert!(
        width > 0.0 && height > 0.0,
        "the root's text covers an area: {width} by {height}"
    );
}

#[test]
fn an_exported_node_is_named_by_the_label_that_names_its_control() {
    let field = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Text::new("Display name").naming_its_control()))
        .push(Box::new(Labelled::new(
            "Display name",
            Box::new(TextInput::local().with_id("display-name")),
        )));
    let driver = Driver::new(State::new(), Box::new(field), (400, 200));
    let named = driver.a11y_nodes_named("Display name");
    assert_eq!(named.len(), 1, "the label names the field once: {named:?}");
    let node = driver.a11y_node(named[0]).expect("the exported node");
    assert_eq!(node.role(), crate::accesskit::Role::TextInput);
}

#[test]
fn a_control_operates_as_a_person_would() {
    let pressed = Rc::new(Cell::new(0));
    let button = Button::new("save", "Save").on_click({
        let pressed = Rc::clone(&pressed);
        move || pressed.set(pressed.get() + 1)
    });
    let mut driver = Driver::for_root(State::new(), button, (200, 80), crate::Theme::dark());
    let control = driver
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some("Save"))
        .expect("the button offers a control");
    driver
        .operate_control(&control.id, &ControlInput::Press)
        .expect("the press lands");
    assert_eq!(pressed.get(), 1);
}

#[test]
fn a_frame_that_settled_skips_nothing_and_measures_nothing_again() {
    let mut driver = Driver::for_root(
        State::new(),
        Text::new("Hello"),
        (400, 300),
        crate::Theme::dark(),
    );
    assert_eq!(driver.take_skip_report(), None, "the layout settles");
    assert!(
        driver.layout_measure_requesters().is_empty(),
        "nothing asks for a measure after the frame that laid it out"
    );
}

#[test]
fn the_root_takes_a_key_under_the_modifier_state_it_is_given() {
    let mut driver = Driver::for_root(State::new(), shell(), (1280, 800), crate::Theme::dark());
    driver
        .root_mut::<NavShell>()
        .set_platform_conventions(PlatformConventions::new(Platform::Linux));
    driver.relayout();
    let key = crate::synthetic_input::key_input(&Key::Character("L".into()), ElementState::Pressed);
    let modifiers = Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT);
    assert!(driver.dispatch_to_root_with(&key, &modifiers));
    assert!(!driver.root::<NavShell>().is_sidebar_open());
    assert!(driver.dispatch_to_root_with(&key, &modifiers));
    assert!(driver.root::<NavShell>().is_sidebar_open());
}
