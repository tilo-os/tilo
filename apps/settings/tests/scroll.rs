//! Driven scrolling test for the settings content pane.
//!
//! A category whose section cards overflow a short window must scroll. This
//! builds the *real* composed `SettingsApp` tree, lays it out at a short
//! viewport, then drives the genuine winit wheel + keyboard path and asserts
//! the content pane's `Scrollable` actually moved its child. Geometry is read
//! only through the public `Widget` trait (`name`, `get_rect`, `children`) so
//! the assertions ride the production layout.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use settings::categories::Category;
use settings::settings_app::SettingsApp;
use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{Modifiers, MouseScrollDelta, TouchPhase, WindowEvent};

struct Harness {
    app: Framed<SettingsApp>,
    /// Dropped after the app whose account stores it holds.
    _config: crate::support::ScratchConfig,
}

impl Harness {
    fn new(category: Category, w: f32, h: f32) -> Self {
        let config = crate::support::ScratchConfig::new();
        let app = config.app().with_category(category).with_width_hint(w);
        Self {
            app: Framed::new(
                ui::State::new(),
                app,
                ui::headless::viewport((w, h)),
                Theme::dark(),
            ),
            _config: config,
        }
    }

    fn relayout(&mut self) {
        Framed::harness_mut(&mut self.app).relayout();
    }

    fn dispatch(&mut self, event: &WindowEvent) -> bool {
        Framed::harness_mut(&mut self.app).dispatch_to_root_with(event, &Modifiers::default())
    }

    fn wheel(&mut self, notches_down: f32) {
        let ev = WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(0.0, -notches_down),
            phase: TouchPhase::Moved,
        };
        self.dispatch(&ev);
    }
}

/// The content pane's `Scrollable` is the one furthest to the right (the
/// sidebar's own `Scrollable` hugs the left edge). Returns `(offset, viewport_h,
/// content_h)` where `offset` is how far it has shifted its child up.
fn content_scroll(app: &SettingsApp) -> (f64, f64, f64) {
    fn collect<'a>(w: &'a dyn Widget, out: &mut Vec<&'a dyn Widget>) {
        if w.name() == "Scrollable" {
            out.push(w);
        }
        for c in w.children() {
            collect(c, out);
        }
    }
    let mut scrolls = Vec::new();
    collect(app, &mut scrolls);
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

    let (offset0, viewport_h, content_h) = content_scroll(&h.app);
    assert!(
        content_h > viewport_h + 40.0,
        "precondition: content ({content_h}) must overflow the viewport ({viewport_h})",
    );
    assert!(offset0.abs() < 0.5, "starts unscrolled, got {offset0}");

    // Wheel down: the content pane's Scrollable shifts its child up.
    let content_point = Point::new(800.0, 170.0);
    let cursor = WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(content_point.x, content_point.y),
    };
    h.dispatch(&cursor);
    h.wheel(3.0);
    h.relayout();
    let (one, _, _) = content_scroll(&h.app);
    assert!(
        one > 0.5,
        "a wheel-down scrolls the settings content; offset stayed {one}"
    );

    // Fling to the bottom: scrolls further, then clamps — a second identical
    // fling must NOT move it any further (it's parked at the content edge).
    h.wheel(1000.0);
    h.relayout();
    let (bottom, _, _) = content_scroll(&h.app);
    assert!(bottom > one, "flinging scrolls further ({bottom} vs {one})");

    h.wheel(1000.0);
    h.relayout();
    let (still, _, _) = content_scroll(&h.app);
    assert!(
        (still - bottom).abs() < 0.5,
        "scroll clamps at the content edge — a second fling doesn't move it ({still} vs {bottom})",
    );
}
