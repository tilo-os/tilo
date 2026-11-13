//! A `DragSource`'s press, wherever it stands: a source a person picks up from inside a
//! scrolling region lifts as one picked up anywhere else, and the drag it makes keeps the
//! pointer as it leaves what the region shows, so the drop it was let go with names where the
//! person let go. The region claims a press only for a gesture that is the region's own — its
//! bars — and never a press a drag source inside it claims.

use std::collections::HashMap;

use super::*;
use crate::Theme;
use crate::widget::scrollable::Scrollable;
use crate::widget::{DragSource, Spacer, Widget, drag_bus};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};

/// A widget laid out in its own tree, which every event is then dispatched against: the tree
/// the widget's retained nodes belong to, as a window's frame lays its root out.
struct Laid {
    widget: Box<dyn Widget>,
    taffy: TaffyTree<()>,
    node: NodeId,
}

impl Laid {
    /// Lay `widget` out in a viewport `width` by `height`.
    fn of(mut widget: impl Widget + 'static, width: f32, height: f32) -> Self {
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes = HashMap::new();
        let node = widget.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        taffy
            .compute_layout(
                node,
                Size {
                    width: AvailableSpace::Definite(width),
                    height: AvailableSpace::Definite(height),
                },
            )
            .expect("the widget solves its layout");
        widget.apply_layout(&taffy, node, Point::ZERO);
        Self {
            widget: Box::new(widget),
            taffy,
            node,
        }
    }

    fn send(&mut self, event: &winit::event::WindowEvent) {
        let mut doc = automerge::AutoCommit::new();
        crate::testing::Dispatch::new()
            .doc(&mut doc)
            .layout(&self.taffy, self.node)
            .send(self.widget.as_mut(), event);
    }

    /// The centre of the source inside the laid-out tree: a press lands on the view a person
    /// sees, not on where a page's gutter leaves it.
    fn source_centre(&self) -> Point {
        self.centre_of::<DragSource>()
    }

    /// The centre of the laid-out widget of type `T`.
    fn centre_of<T: 'static>(&self) -> Point {
        fn find<T: 'static>(widget: &dyn Widget) -> Option<Rect> {
            if let Some(any) = widget.as_any()
                && any.is::<T>()
            {
                return Some(widget.get_rect());
            }
            let mut found = None;
            widget.for_each_child(&mut |child| {
                if found.is_none() {
                    found = find::<T>(child);
                }
            });
            found
        }
        find::<T>(self.widget.as_ref())
            .unwrap_or_else(|| panic!("the tree holds no {}", std::any::type_name::<T>()))
            .center()
    }

    /// Press at `from` and carry the pointer to `to` in the steps a hand's does, without letting
    /// go: what the drag is while it travels.
    fn press_and_travel(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.send(&crate::synthetic_input::cursor_moved(from.0, from.1));
        self.send(&crate::synthetic_input::mouse_press());
        for at in [0.25, 0.5, 0.75, 1.0] {
            self.send(&crate::synthetic_input::cursor_moved(
                from.0 + (to.0 - from.0) * at,
                from.1 + (to.1 - from.1) * at,
            ));
        }
    }

    fn release(&mut self) {
        self.send(&crate::synthetic_input::mouse_release());
    }

    /// [`Self::press_and_travel`] and then let go there.
    fn drag(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.press_and_travel(from, to);
        self.release();
    }

    /// The scroll view it holds, to read where its content stands.
    fn scroll(&self) -> &Scrollable {
        self.widget
            .as_any()
            .and_then(|any| any.downcast_ref::<Scrollable>())
            .expect("a scroll view")
    }
}

/// A box that holds everything, for reading a drop that ended anywhere.
fn everywhere() -> Rect {
    Rect::new(-1_000_000.0, -1_000_000.0, 1_000_000.0, 1_000_000.0)
}

/// A source of one inert row, `height` tall, carried at `resource`.
fn source(resource: &str, height: f32) -> DragSource {
    DragSource::new(
        "source",
        Box::new(Spacer::fixed(height)),
        resource,
        "the row",
    )
}

/// A live scroll view: a viewport `width` by `height` showing content `content_h` tall under a
/// finger, holding `child`.
fn scroll_of(child: Element, width: f32, height: f32, content_h: f32) -> Scrollable {
    let mut scroll = Scrollable::new_vertical().with_child(child);
    scroll.set_content_size(width, content_h);
    scroll.set_viewport_size(width, height);
    scroll
}

#[test]
fn a_press_on_a_source_outside_a_scroll_lifts_it_past_the_threshold() {
    // The class's reference case: a source that stands on its own page lifts as soon as a
    // press travels past the threshold.
    let mut laid = Laid::of(source("notes.page:one", 40.0), 300.0, 400.0);
    let from = laid.source_centre();
    laid.press_and_travel((from.x, from.y), (from.x, from.y + 100.0));
    let in_flight = drag_bus::current();
    drag_bus::end(None);
    let in_flight = in_flight.expect("the source published what it lifted");
    assert_eq!(in_flight.source, "source");
}

#[test]
fn a_press_on_a_source_inside_a_scroll_lifts_it_and_keeps_it_past_the_viewport() {
    // The same source inside a scroll view with far more content than it shows, standing in the
    // layouts a view composes around it (a page, a stack): a press over the source is the
    // source's, and what it holds lifts as it does anywhere else.
    let mut laid = Laid::of(
        scroll_of(
            Box::new(crate::widget::arrange::Stack::stack().push_slot(
                crate::widget::arrange::Slot::new(Box::new(
                    crate::widget::arrange::Page::full().push_slot(
                        crate::widget::arrange::Slot::new(Box::new(source("notes.page:one", 40.0))),
                    ),
                )),
            )),
            300.0,
            200.0,
            4_000.0,
        ),
        300.0,
        200.0,
    );
    // From the source, out past the viewport's foot.
    let from = laid.source_centre();
    laid.press_and_travel((from.x, from.y), (from.x, 320.0));
    let in_flight = drag_bus::current();
    drag_bus::end(None);
    let in_flight = in_flight.expect("a source inside a scroll view lifts what it holds");
    assert_eq!(in_flight.source, "source");
    assert!(
        in_flight.pointer.y > 200.0,
        "the lift follows the pointer past the region's foot: {}",
        in_flight.pointer.y
    );
}

#[test]
fn a_drag_out_of_a_scroll_is_let_go_where_the_person_let_go() {
    // The drop leg: a resource picked up inside the scroll view — behind the page and stack a
    // view composes it in — is carried out of the region and let go beyond its foot, and is
    // reported dropped where the person let go, not clipped to the region's edge, which is what
    // a source that lost the pointer as it left would say.
    let mut laid = Laid::of(
        scroll_of(
            Box::new(crate::widget::arrange::Stack::stack().push_slot(
                crate::widget::arrange::Slot::new(Box::new(
                    crate::widget::arrange::Page::full().push_slot(
                        crate::widget::arrange::Slot::new(Box::new(source("notes.page:one", 40.0))),
                    ),
                )),
            )),
            300.0,
            200.0,
            4_000.0,
        ),
        300.0,
        200.0,
    );
    let from = laid.source_centre();
    laid.drag((from.x, from.y), (from.x, 320.0));
    let landed = drag_bus::take("elsewhere", everywhere());
    drag_bus::end(None);
    let landed = landed.expect("the drag that ended outside the region reports a drop");
    assert_eq!(landed.source, "source");
    assert!(
        landed.pointer.y > 200.0,
        "the drop is where the person let go, past the region's foot: {}",
        landed.pointer.y
    );
    let crate::dnd::DragPayload::Custom { blob, .. } = landed.payload else {
        panic!("a resource drag");
    };
    assert_eq!(String::from_utf8(blob).unwrap(), "notes.page:one");
}

#[test]
fn a_press_on_a_scrolls_own_bar_still_drags_the_bar() {
    // The region's own gesture is untouched: a press on its scrollbar's thumb and a drag along
    // the axis it scrolls moves the content, and lifts nothing.
    let mut laid = Laid::of(
        scroll_of(Box::new(Spacer::fixed(4_000.0)), 300.0, 200.0, 4_000.0),
        300.0,
        200.0,
    );
    let thumb = laid
        .scroll()
        .thumb_rect(crate::widget::ScrollAxis::Vertical)
        .expect("a live scrollbar has a thumb");
    let grab = thumb.center();
    let before = laid.scroll().offset_y();
    laid.drag((grab.x, grab.y), (grab.x, grab.y + 60.0));
    assert!(
        laid.scroll().offset_y() > before,
        "the thumb dragged moves the view: {before} then {}",
        laid.scroll().offset_y()
    );
    assert!(
        drag_bus::current().is_none(),
        "a bar's own drag lifts nothing"
    );
    drag_bus::end(None);
}

/// A press that is not over the source is not the source's. The desk lays the windows, with
/// their handles, under a bar of its own, and a press on the bar reaches the window's handle
/// first: the handle took every such press, lifted a row the pointer was nowhere near, and held
/// the pointer, so the release that would have clicked the bar's button went to it instead and
/// the button never saw the end of its press.
#[test]
fn a_press_outside_a_source_is_not_the_sources_and_leaves_the_release_to_what_was_pressed() {
    let clicks = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let counted = std::rc::Rc::clone(&clicks);
    let button = crate::widget::Button::new("bar", "Bar").on_click(move || {
        counted.set(counted.get().saturating_add(1));
    });
    // The button stands in the column's top slot, the source under it: the stack asks its
    // children from the last, so the source hears the press first and the button hears it
    // after, as the desk's window handles and its bar do.
    let mut laid = Laid::of(
        crate::widget::arrange::Stack::stack()
            .gap(crate::tokens::Rhythm::None)
            .push_slot(crate::widget::arrange::Slot::new(Box::new(button)))
            .push_slot(crate::widget::arrange::Slot::new(Box::new(source(
                "notes.page:one",
                40.0,
            )))),
        300.0,
        300.0,
    );
    // The press lands on the button, under the source.
    let button_at = laid.centre_of::<crate::widget::Button>();
    laid.send(&crate::synthetic_input::cursor_moved(
        button_at.x,
        button_at.y,
    ));
    laid.send(&crate::synthetic_input::mouse_press());
    assert!(
        drag_bus::current().is_none(),
        "a press outside the source lifts nothing"
    );
    laid.release();
    assert!(
        drag_bus::current().is_none(),
        "letting go outside the source ends nothing on the bus"
    );
    assert_eq!(
        clicks.get(),
        1,
        "the release stays with the button the press was on"
    );
    drag_bus::end(None);
}

/// A source at the top of a column, a target under it: a press on the source carried down onto
/// the target and let go there reports the drop the source carried, by the resource's address.
/// The two are the pair a view composes — a place's handle over a composer, a chip over a
/// canvas — and what the target reports is what any runtime's drop target takes, so this is the
/// path a drop crosses from one view to another on.
#[test]
fn a_drop_onto_a_target_reports_the_resource_the_source_carried() {
    let queue = crate::Intents::default();
    let dropped =
        crate::component::Event::View(crate::component::Intent::new(&queue, "target", "drop"));
    let target = crate::widget::DropTarget::new(
        "target",
        Box::new(Spacer::fixed(120.0)) as Element,
        dropped,
    );
    let source = DragSource::new(
        "source",
        Box::new(Spacer::fixed(40.0)) as Element,
        "notes.pages:page_1",
        "The page",
    );
    let mut laid = Laid::of(
        crate::widget::arrange::Stack::stack()
            .gap(crate::tokens::Rhythm::None)
            .push_slot(crate::widget::arrange::Slot::new(Box::new(source)))
            .push_slot(crate::widget::arrange::Slot::new(Box::new(target))),
        300.0,
        300.0,
    );
    // The source stands in the column's top 40px; the target from 40 to 160.
    let from = laid.source_centre();
    laid.drag((from.x, from.y), (from.x, 120.0));
    // The target takes a drop in its `update`, run by the frame after the release: the drop it
    // was let go with is fired then, carrying the resource the source carried.
    let doc = automerge::AutoCommit::new();
    laid.widget.update(&doc, &automerge::ROOT);
    let fired = queue.take();
    drag_bus::end(None);
    assert_eq!(
        fired,
        vec![crate::component::Fired {
            key: "target".to_owned(),
            event: "drop".to_owned(),
            value: Some(crate::component::Value::Resource(
                "notes.pages:page_1".to_owned()
            )),
        }],
        "the drop carries the resource the drag let go with, by its address"
    );
}
