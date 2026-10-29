use std::collections::HashMap;

use taffy::TaffyTree;
use vello::kurbo::Point;

use super::*;
use crate::widget::Button;

fn laid_out(bar: &mut HoverToolbar) -> (TaffyTree<()>, taffy::NodeId) {
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes = HashMap::new();
    let node = Widget::build_layout(bar, &mut taffy, &Theme::dark(), &mut nodes);
    taffy
        .compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(600.0),
                height: taffy::AvailableSpace::Definite(200.0),
            },
        )
        .expect("it lays out");
    Widget::apply_layout(bar, &taffy, node, Point::ZERO);
    (taffy, node)
}

fn drawn(bar: &mut HoverToolbar) {
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    Widget::draw(
        bar,
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        &PresenceCtx::default(),
    );
    Widget::mark_drawn(bar);
}

/// A message under its toolbar, laid out.
struct Laid {
    bar: HoverToolbar,
    taffy: TaffyTree<()>,
    node: taffy::NodeId,
}

impl Laid {
    fn send(&mut self, event: &WindowEvent) {
        crate::testing::Dispatch::new()
            .layout(&self.taffy, self.node)
            .send(&mut self.bar, event);
    }
}

fn message() -> Laid {
    let row = Box::new(
        crate::widget::Container::new()
            .with_no_padding()
            .with_width(560.0)
            .with_child(Box::new(crate::widget::Spacer::fixed(64.0))),
    );
    let mut bar = HoverToolbar::new(
        row,
        vec![
            Box::new(Button::icon_only(
                "react",
                crate::icons::Icon::ThumbsUp,
                "React",
            )),
            Box::new(Button::icon_only(
                "reply",
                crate::icons::Icon::Reply,
                "Reply",
            )),
        ],
    );
    let (taffy, node) = laid_out(&mut bar);
    drawn(&mut bar);
    Laid { bar, taffy, node }
}

#[test]
fn the_toolbar_shows_while_the_row_is_hovered() {
    let _settled = crate::motion::testing::settled();
    let mut laid = message();
    assert!(!laid.bar.is_shown(), "hidden until the pointer comes");
    let row = Widget::get_rect(&laid.bar);
    laid.send(&crate::synthetic_input::cursor_moved(
        row.center().x,
        row.center().y,
    ));
    assert!(laid.bar.is_shown());
    laid.send(&crate::synthetic_input::cursor_moved(
        row.center().x,
        row.y1 + 40.0,
    ));
    assert!(!laid.bar.is_shown(), "and goes when it leaves");
}

#[test]
fn hovering_damages_only_the_toolbar_not_the_row() {
    let _settled = crate::motion::testing::settled();
    let mut laid = message();
    let row = Widget::get_rect(&laid.bar);
    laid.send(&crate::synthetic_input::cursor_moved(
        row.center().x,
        row.center().y,
    ));
    let mut damage = Vec::new();
    Widget::collect_damage(&laid.bar, &mut damage);
    assert_eq!(damage.len(), 1, "one region: {damage:?}");
    let region = damage[0];
    assert!(
        region.area() < row.area() / 4.0,
        "the toolbar's box ({region:?}), never the row's ({row:?})"
    );
    drawn(&mut laid.bar);
    let mut after = Vec::new();
    Widget::collect_damage(&laid.bar, &mut after);
    assert!(after.is_empty(), "drawn, nothing is left to repaint");
    // Moving within the row changes nothing.
    laid.send(&crate::synthetic_input::cursor_moved(
        row.center().x + 10.0,
        row.center().y,
    ));
    let mut still = Vec::new();
    Widget::collect_damage(&laid.bar, &mut still);
    assert!(still.is_empty(), "a move within the row repaints nothing");
}

#[test]
fn focus_in_the_toolbar_shows_it_for_the_keyboard() {
    let _settled = crate::motion::testing::settled();
    let mut laid = message();
    let mut focused = false;
    for child in Widget::children_mut(&mut laid.bar) {
        focus_first(child, &mut focused);
    }
    assert!(focused, "the toolbar's buttons are in the focus order");
    drawn(&mut laid.bar);
    assert!(laid.bar.is_shown(), "focus in it shows it");
}

fn focus_first(widget: &mut dyn Widget, done: &mut bool) {
    if *done {
        return;
    }
    if widget.is_focusable() && widget.name() == "Button" {
        widget.set_focus(true);
        *done = true;
        return;
    }
    for child in widget.children_mut() {
        focus_first(child, done);
    }
}
