//! What the display-less driver keeps and says of a tree a host poses, compares and renders as
//! elements: the damage parts its frames record, the composition it shows, the keyboard its fields
//! draw carets under, the tooltip it painted, and the owner models a worker woke.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use crate::headless::Driver;
use crate::state::State;
use tilo_ui_base::theme::Theme;
use crate::widget::{Column, TextInput};

const VIEW: (u32, u32) = (400, 240);

fn a_text_field() -> Driver {
    Driver::for_root(
        State::new(),
        Column::new().push(Box::new(TextInput::local().with_id("field"))),
        VIEW,
        Theme::dark(),
    )
}

/// The composed widget named `widget`, depth first.
fn find<'a>(
    node: &'a crate::composition::Composed,
    widget: &str,
) -> Option<&'a crate::composition::Composed> {
    if node.widget == widget {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, widget))
}

#[test]
fn the_composition_is_the_laid_out_widgets() {
    let driver = a_text_field();
    let composition = driver.composition();
    assert_eq!(composition.viewport, [VIEW.0, VIEW.1]);
    let field = find(&composition.root, "TextInput").expect("the field is composed");
    assert!(field.path.contains("field") || field.rect[2] > 0.0);
    assert!(field.rect[2] > 0.0 && field.rect[3] > 0.0, "{field:?}");
}

#[test]
fn a_frame_records_its_parts_only_when_they_are_tracked() {
    let mut driver = a_text_field();
    driver.relayout();
    assert!(driver.scene_parts().is_none(), "nothing was tracked");
    driver.track_scene_parts(true);
    driver.relayout();
    assert!(
        driver.scene_parts().is_some(),
        "a tracked frame reports its parts"
    );
    driver.track_scene_parts(false);
    driver.relayout();
    assert!(driver.scene_parts().is_none(), "tracking stopped");
}

#[test]
fn a_frame_with_nothing_blurred_records_no_backdrops() {
    let mut driver = a_text_field();
    driver.relayout();
    assert!(driver.backdrops().is_empty());
    assert!(driver.lifted().is_empty());
}

#[test]
fn the_keyboard_is_held_and_given_up_once() {
    let mut driver = a_text_field();
    assert!(driver.keyboard_held());
    assert!(
        driver.set_keyboard_held(false),
        "the change is owed a frame"
    );
    assert!(!driver.keyboard_held());
    assert!(!driver.set_keyboard_held(false), "nothing changed");
    assert!(driver.set_keyboard_held(true));
    assert!(driver.keyboard_held());
}

#[test]
fn a_requested_layout_is_drawn_by_the_next_frame() {
    let mut driver = a_text_field();
    driver.request_layout();
    assert!(driver.poll_frame().unwrap(), "the requested frame ran");
}

#[test]
fn an_accessibility_node_is_read_by_its_id() {
    let driver = a_text_field();
    let (id, exported) = driver
        .a11y_nodes()
        .into_iter()
        .next()
        .expect("the field exports a node");
    let node = driver
        .a11y_node(id)
        .expect("the exported node is read back");
    assert_eq!(node.role(), exported.role());
}

#[test]
fn a_change_made_outside_the_app_shows_in_the_frame_its_wake_brings() {
    fn shown(driver: &Driver) -> Option<String> {
        driver
            .a11y_nodes()
            .into_iter()
            .find_map(|(_, node)| node.value().map(str::to_owned))
    }
    let text = tilo_ui_base::LocalState::new("one".to_owned());
    let mut driver = Driver::for_root(
        State::new(),
        TextInput::from_state(&text),
        VIEW,
        Theme::dark(),
    );
    assert_eq!(shown(&driver).as_deref(), Some("one"));
    text.set("two".to_owned());
    assert!(
        !driver.frame_owed(),
        "a write nobody woke the app for owes no frame"
    );
    driver.wake_handle().wake();
    assert!(
        driver.frame_owed(),
        "the app's wake owes the frame that takes the change up"
    );
    assert!(driver.poll_frame().unwrap());
    assert_eq!(
        shown(&driver).as_deref(),
        Some("two"),
        "the frame took up what the field reads"
    );
}

#[test]
fn an_owner_wake_queue_attaches_once() {
    let mut driver = a_text_field();
    let queue = crate::owner_wake::OwnerWakeQueue::default();
    driver
        .attach_owner_wake_queue(queue.clone())
        .expect("the first queue attaches");
    assert!(!driver.has_pending_owner_models());
    assert!(
        driver
            .attach_owner_wake_queue(crate::owner_wake::OwnerWakeQueue::default())
            .is_err(),
        "the registration identity cannot be replaced"
    );
}
