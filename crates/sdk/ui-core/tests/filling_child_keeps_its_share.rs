//! A child that fills its parent takes exactly its share of the room along the parent's main
//! axis: a fixed strip above it keeps its own height, and the filling child holds the rest,
//! scrolling or clipping its content inside that share. It never pushes the parent past the room
//! the parent was given.

#![cfg(test)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::State;
use tilo_ui::headless::FrameHarness;
use tilo_ui::widget::arrange::{Fit, Slot, Stack};
use tilo_ui::widget::{Column, Container, Fitted, Row, Scrollable, Text};

/// A stack of a strip above `content`, `content` filled (`fill` in the language), inside a
/// container of exactly `height` standing in a column that fills the window.
fn stack_in_a_place(
    strip: Container,
    content: Box<dyn tilo_ui::widget::Widget>,
    height: f32,
) -> FrameHarness {
    let stack = Stack::stack()
        .fill_width()
        .fill_height()
        .push_slot(Slot::new(Box::new(strip)))
        .push_slot(Slot::new(Box::new(Fitted::new(content, Fit::fill(1.0)))));
    let place = Container::new()
        .with_no_padding()
        .with_id("place")
        .with_height(height)
        .with_child(Box::new(stack));
    let root = Column::new().with_no_padding().push(Box::new(place));
    let mut driver = FrameHarness::new(State::new(), Box::new(root), (400, 900));
    driver.send(&tilo_ui::synthetic_input::cursor_moved(150.0, 150.0));
    driver
}

/// 3000 px of content in a scroll view that fills its parent.
fn tall_scroll(id: &str, width: f32) -> Scrollable {
    Scrollable::new_vertical()
        .fill_parent_height()
        .with_semantic_key(format!("{id}-view"))
        .with_child(Box::new(
            Container::new()
                .with_no_padding()
                .with_size(width, 3000.0)
                .with_child(Box::new(Text::new("The whole page").with_id(id))),
        ))
}

#[test]
fn a_stack_of_a_strip_above_hugging_content_in_a_place_keeps_the_content_in_the_place() {
    let strip = Container::new()
        .with_no_padding()
        .with_id("strip")
        .with_height(40.0);
    let hugging = Container::new()
        .with_no_padding()
        .with_id("content")
        .with_child(Box::new(Text::new("One line").with_id("line")));
    let mut driver = stack_in_a_place(strip, Box::new(hugging), 520.0);
    let targets = driver.targets();
    for (name, rect) in &targets {
        println!("{name}: {rect:?}");
    }
    let place = *targets.get("place").expect("the place");
    let content = *targets.get("content").expect("the content");
    assert!(
        (place.height() - 520.0).abs() <= 1.0,
        "the place is 520: {place:?}"
    );
    assert!(
        content.y0 >= 39.0 && content.y1 <= place.y1 + 1.0,
        "the content stands in the place: {content:?} in {place:?}"
    );
    driver.relayout();
}

#[test]
fn a_stack_of_a_strip_above_a_filling_scroll_view_in_a_place_keeps_the_view_in_the_place() {
    let strip = Container::new()
        .with_no_padding()
        .with_id("strip")
        .with_height(40.0);
    let mut driver = stack_in_a_place(strip, Box::new(tall_scroll("content", 400.0)), 520.0);
    let targets = driver.targets();
    for (name, rect) in &targets {
        println!("{name}: {rect:?}");
    }
    let place = *targets.get("place").expect("the place");
    let content = *targets.get("content").expect("the content");
    assert!(
        (place.height() - 520.0).abs() <= 1.0,
        "the place is 520: {place:?}"
    );
    let before = content.y0;
    driver.scroll(0.0, -20.0);
    let after = driver.targets().get("content").expect("the content").y0;
    assert!(
        after < before - 100.0,
        "the content scrolls inside the place: {before} -> {after}"
    );
}

#[test]
fn a_filling_child_of_a_column_takes_the_room_under_its_strip() {
    let root = Column::new()
        .with_no_padding()
        .with_gap(0.0)
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_id("strip")
                .with_height(40.0),
        ))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_id("filler")
                // A `Column` places its children at the start across its width, so the test
                // names the width the filler takes; the main axis is the column's own.
                .with_width(400.0)
                .with_flex_grow()
                .with_child(Box::new(tall_scroll("content", 400.0))),
        ));
    let mut driver = FrameHarness::new(State::new(), Box::new(root), (400, 520));
    driver.send(&tilo_ui::synthetic_input::cursor_moved(150.0, 150.0));
    let targets = driver.targets();
    for (name, rect) in &targets {
        println!("{name}: {rect:?}");
    }
    let strip = *targets.get("strip").expect("the strip");
    let filler = *targets.get("filler").expect("the filling child");
    let content = *targets.get("content").expect("the content");
    assert!(
        (strip.height() - 40.0).abs() <= 1.0,
        "the strip is 40: {strip:?}"
    );
    assert!(
        (filler.height() - 480.0).abs() <= 1.0,
        "the filling child holds the room under the strip: {filler:?}"
    );
    assert!(
        (filler.y0 - 40.0).abs() <= 1.0,
        "the filling child starts under the strip: {filler:?}"
    );
    // The content is longer than the share and scrolls inside it.
    assert!(
        content.height() > 2000.0,
        "the content is its own height: {content:?}"
    );
    let before = content.y0;
    driver.scroll(0.0, -20.0);
    let after = driver.targets().get("content").expect("the content").y0;
    assert!(
        after < before - 100.0,
        "the content scrolls inside the share: {before} -> {after}"
    );
}

#[test]
fn a_filling_child_of_a_row_takes_the_room_beside_its_strip() {
    let root = Row::new()
        .no_wrap()
        .with_padding(0.0, 0.0)
        .with_gap(0.0)
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_id("strip")
                .with_width(40.0),
        ))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_id("filler")
                .with_flex_grow()
                .with_child(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_id("wide")
                        .with_size(3000.0, 100.0)
                        .with_child(Box::new(Text::new("The whole page").with_id("content"))),
                )),
        ));
    let driver = FrameHarness::new(State::new(), Box::new(root), (520, 200));
    let targets = driver.targets();
    for (name, rect) in &targets {
        println!("{name}: {rect:?}");
    }
    let strip = *targets.get("strip").expect("the strip");
    let filler = *targets.get("filler").expect("the filling child");
    let wide = *targets.get("wide").expect("the wide content");
    assert!(
        (strip.width() - 40.0).abs() <= 1.0,
        "the strip is 40: {strip:?}"
    );
    assert!(
        (filler.width() - 480.0).abs() <= 1.0,
        "the filling child holds the room beside the strip: {filler:?}"
    );
    assert!(
        (filler.x0 - 40.0).abs() <= 1.0,
        "the filling child starts beside the strip: {filler:?}"
    );
    // What is wider than the share lies inside it, cut at its edge, and never widens the row.
    assert!(
        wide.width() > 2000.0,
        "the content is its own width: {wide:?}"
    );
    assert!(
        wide.x0 >= filler.x0 - 1.0,
        "the content starts inside the share: {wide:?}, {filler:?}"
    );
    let row = *targets.get("strip").expect("the strip");
    assert!(
        row.x1 <= 41.0,
        "the row is the window's width, not its content's: {row:?}"
    );
}
