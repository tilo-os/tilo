//! Driven scrolling test for the settings content pane.
//!
//! A category whose section cards overflow a short window must scroll. This
//! builds the *real* composed Settings window tree, lays it out at a short
//! viewport, then drives the genuine winit wheel + keyboard path and asserts
//! the content pane's `Scrollable` actually moved its child. Geometry is read
//! only through the public `Widget` trait (`name`, `get_rect`, `children`) so
//! the assertions ride the production layout.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use settings::categories::Category;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{Modifiers, WindowEvent};

struct Harness {
    app: crate::support::Window,
}

impl Harness {
    fn new(category: Category, w: f32, h: f32) -> Self {
        Self {
            app: crate::support::window_on(category, (w, h)),
        }
    }

    fn relayout(&mut self) {
        self.app.harness().relayout();
    }

    fn dispatch(&mut self, event: &WindowEvent) -> bool {
        self.app
            .harness()
            .dispatch_to_root_with(event, &Modifiers::default())
    }

    /// The content pane's scroll ([`content_scroll`]).
    fn content_scroll(&mut self) -> (f64, f64, f64) {
        content_scroll(self.app.harness().root_widget())
    }

    /// Open `category`'s pane, as its sidebar row does.
    fn open(&mut self, category: Category) {
        self.app
            .effect(&settings::effects::open_pane(category))
            .expect("the pane opens");
    }

    fn wheel(&mut self, notches_down: f32) {
        let ev = ui::synthetic_input::mouse_wheel(0.0, -notches_down);
        self.dispatch(&ev);
    }
}

/// The content pane's `Scrollable` is the one furthest to the right (the
/// sidebar's own `Scrollable` hugs the left edge). Returns `(offset, viewport_h,
/// content_h)` where `offset` is how far it has shifted its child up.
fn content_scroll(root: &dyn Widget) -> (f64, f64, f64) {
    fn collect<'a>(w: &'a dyn Widget, out: &mut Vec<&'a dyn Widget>) {
        if w.name() == "Scrollable" {
            out.push(w);
        }
        for c in w.children() {
            collect(c, out);
        }
    }
    let mut scrolls = Vec::new();
    collect(root, &mut scrolls);
    let content = scrolls
        .into_iter()
        .max_by(|a, b| a.get_rect().x0.partial_cmp(&b.get_rect().x0).unwrap())
        .expect("a content Scrollable exists in the settings tree");
    let child = *content
        .children()
        .first()
        .expect("the Scrollable wraps its content column");
    let vp = content.get_rect();
    (
        vp.y0 - child.get_rect().y0,
        vp.height(),
        child.get_rect().height(),
    )
}

#[test]
fn content_pane_scrolls_with_the_mouse_wheel() {
    // A tall category on a short, wide (docked-sidebar) window. Power has a
    // fixed four-card structure (battery / profile / idle / behaviour) that
    // reliably overflows a short viewport by far more than a single wheel
    // step, so the staged wheel→fling→clamp assertions below don't ride on
    // one pane's exact pixel height (or on how many accounts a given box has).
    let mut h = Harness::new(Category::Power, 1100.0, 340.0);

    let (offset0, viewport_h, content_h) = h.content_scroll();
    assert!(
        content_h > viewport_h + 40.0,
        "precondition: content ({content_h}) must overflow the viewport ({viewport_h})",
    );
    assert!(offset0.abs() < 0.5, "starts unscrolled, got {offset0}");

    // Wheel down: the content pane's Scrollable shifts its child up.
    let content_point = Point::new(800.0, 170.0);
    let cursor = ui::synthetic_input::cursor_moved(content_point.x, content_point.y);
    h.dispatch(&cursor);
    h.wheel(3.0);
    h.relayout();
    let (one, _, _) = h.content_scroll();
    assert!(
        one > 0.5,
        "a wheel-down scrolls the settings content; offset stayed {one}"
    );

    // Fling to the bottom: scrolls further, then clamps — a second identical
    // fling must NOT move it any further (it's parked at the content edge).
    h.wheel(1000.0);
    h.relayout();
    let (bottom, _, _) = h.content_scroll();
    assert!(bottom > one, "flinging scrolls further ({bottom} vs {one})");

    h.wheel(1000.0);
    h.relayout();
    let (still, _, _) = h.content_scroll();
    assert!(
        (still - bottom).abs() < 0.5,
        "scroll clamps at the content edge — a second fling doesn't move it ({still} vs {bottom})",
    );
}

/// Settings rebuilds its content scroller whenever a pane's structure
/// changes: a control applied, or the pane opened again from the sidebar.
/// Each rebuild must keep the pane where the person scrolled it, not jump it
/// back to the top.
#[test]
fn a_rebuilt_pane_keeps_its_scroll_position() {
    let mut h = Harness::new(Category::Power, 1100.0, 340.0);
    let content_point = Point::new(800.0, 170.0);
    h.dispatch(&ui::synthetic_input::cursor_moved(
        content_point.x,
        content_point.y,
    ));
    h.wheel(3.0);
    h.relayout();
    let (scrolled, _, _) = h.content_scroll();
    assert!(
        scrolled > 0.5,
        "precondition: the pane scrolled, offset {scrolled}"
    );

    let rebuild = |h: &mut Harness| {
        h.app
            .harness()
            .relayout_themed(ui::headless::viewport((1100.0, 340.0)), Theme::dark())
            .expect("settings frame lays out");
    };
    h.open(Category::Network);
    rebuild(&mut h);
    let (other, _, _) = h.content_scroll();
    assert!(
        other.abs() < 0.5,
        "another pane keeps its own position, got {other}"
    );

    h.open(Category::Power);
    rebuild(&mut h);
    let (back, _, _) = h.content_scroll();
    assert!(
        (back - scrolled).abs() < 0.5,
        "the rebuilt pane returns where it was scrolled ({back} vs {scrolled})"
    );
}
