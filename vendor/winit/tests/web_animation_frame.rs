//! The web event loop requests every dirty canvas after each window event.
//! Replacing pending callbacks lets the first canvas starve its siblings.
#[path = "../src/platform_impl/web/web_sys/animation_frame_request.rs"]
mod animation_frame_request;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

#[derive(Default)]
struct Frames {
    next: Cell<i32>,
    callbacks: RefCell<BTreeMap<i32, &'static str>>,
}
impl Frames {
    fn request(&self, canvas: &'static str, pending: &Cell<Option<i32>>) {
        animation_frame_request::request(pending, || {
            let id = self.next.get() + 1;
            self.next.set(id);
            self.callbacks.borrow_mut().insert(id, canvas);
            id
        });
    }
}

#[test]
fn repeated_redraw_demand_preserves_the_pending_callback() {
    let frames = Frames::default();
    let pending = Cell::new(None);
    frames.request("alpha", &pending);
    let first = pending.get();
    frames.request("alpha", &pending);
    assert_eq!(
        pending.get(),
        first,
        "redraw demand must not replace an already scheduled frame"
    );
    assert_eq!(frames.next.get(), 1);
}

#[test]
fn first_canvas_redraw_cannot_cancel_a_sibling_in_the_same_browser_frame() {
    let frames = Frames::default();
    let alpha = Cell::new(None);
    let beta = Cell::new(None);
    frames.request("alpha", &alpha);
    frames.request("beta", &beta);
    // Browsers snapshot the eligible callbacks at the beginning of a frame.
    // Cancellation still removes a callback before its turn in that snapshot.
    let ready: Vec<_> = frames.callbacks.borrow().keys().copied().collect();
    let mut presented = Vec::new();
    for id in ready {
        let callback = frames.callbacks.borrow_mut().remove(&id);
        if let Some(canvas) = callback {
            if canvas == "alpha" {
                alpha.set(None);
            } else {
                beta.set(None);
            }
            presented.push(canvas);
            // Runtime.about_to_wait asks both pending/animating windows again.
            frames.request("alpha", &alpha);
            frames.request("beta", &beta);
        }
    }
    assert_eq!(presented, ["alpha", "beta"]);
}

#[test]
fn a_completed_or_cancelled_callback_can_be_requested_again() {
    let frames = Frames::default();
    let pending = Cell::new(None);
    frames.request("alpha", &pending);
    let first = pending.take().unwrap();
    frames.callbacks.borrow_mut().remove(&first);
    frames.request("alpha", &pending);
    assert_eq!(pending.get(), Some(2));
    assert_eq!(frames.callbacks.borrow().len(), 1);
}
