//! A `DropTarget`'s spring: a drag that rests on the target says so once, after the moment it
//! has rested long enough, so a view can open what the target stands for — a folder's row opens
//! its folder under the hand carrying a file.

use std::collections::HashMap;

use super::*;
use crate::time::Duration;
use crate::time::Instant;
use crate::widget::drag_bus;
use taffy::{AvailableSpace, Size, TaffyTree};
use tilo_ui_base::Theme;
use tilo_ui_base::component::Event;
use tilo_ui_base::component::{Fired, Intent};
use tilo_ui_base::widget::Spacer;
use tilo_ui_base::widget::Widget;
use vello::kurbo::{Point, Rect};

/// A widget laid out at 200 by 120, read through the `Widget` trait alone: a target is a
/// decorator too, and the trait's own deadline and update are the ones a window calls.
struct Laid {
    widget: Box<dyn Widget>,
}

impl Laid {
    fn of(mut widget: impl Widget + 'static) -> Self {
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes = HashMap::new();
        let node = widget.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        taffy
            .compute_layout(
                node,
                Size {
                    width: AvailableSpace::Definite(200.0),
                    height: AvailableSpace::Definite(120.0),
                },
            )
            .expect("the widget solves its layout");
        widget.apply_layout(&taffy, node, Point::ZERO);
        Self {
            widget: Box::new(widget),
        }
    }

    fn update(&mut self) -> bool {
        self.widget.update(&(), &())
    }

    fn deadline(&self) -> Option<Instant> {
        self.widget.next_wake_deadline()
    }

    fn serve(&mut self, now: Instant) -> bool {
        self.widget.serve_deadline(now)
    }
}

/// A target whose spring is queued on `queue`.
fn target(queue: &tilo_ui_base::Intents<Fired>) -> DropTarget {
    DropTarget::new(
        "folder",
        Box::new(Spacer::token(tilo_ui_base::tokens::SpacingStep::S0)),
        Event::None,
    )
    .with_files(Event::None)
    .with_spring(Event::View(Intent::new(queue, "folder", "hover")))
}

fn carried() {
    drag_bus::carry(
        "elsewhere".to_owned(),
        vec![7],
        "notes.md".to_owned(),
        Point::new(20.0, 20.0),
        Rect::new(0.0, 0.0, 200.0, 120.0),
    );
}

#[test]
fn a_drag_that_rests_on_a_target_says_so_once() {
    let queue = tilo_ui_base::Intents::<Fired>::default();
    let mut laid = Laid::of(target(&queue));
    carried();
    assert!(laid.update(), "the drag over it changes it");
    let rested = laid
        .deadline()
        .expect("a drag over the target asks for the moment it has rested");
    assert!(
        queue.take().is_empty(),
        "the drag has not rested yet when it comes over"
    );
    // Before the moment, nothing is said.
    assert!(!laid.serve(rested - Duration::from_millis(1)));
    assert!(queue.take().is_empty(), "a drag only passing says nothing");
    // At it, the target says so once.
    assert!(laid.serve(rested));
    let said = queue.take();
    assert_eq!(said.len(), 1, "one spring for one drag");
    assert_eq!(said[0].key, "folder");
    assert_eq!(said[0].event, "hover");
    // Still held, it does not say it again.
    assert!(laid.deadline().is_none());
    drag_bus::end(None);
    laid.update();
    assert!(laid.deadline().is_none());
}

#[test]
fn a_drag_that_goes_before_resting_says_nothing() {
    let queue = tilo_ui_base::Intents::<Fired>::default();
    let mut laid = Laid::of(target(&queue));
    carried();
    assert!(laid.update());
    let rested = laid.deadline().expect("it rests on the target");
    // The drag ends before the moment comes.
    drag_bus::end(None);
    assert!(laid.update());
    assert!(laid.deadline().is_none(), "the drag took it");
    // A pass at the old moment says nothing: the drag is no longer here.
    laid.serve(rested);
    assert!(queue.take().is_empty(), "a drag gone says nothing");
}
